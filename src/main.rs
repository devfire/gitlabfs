use clap::Parser;
use fuser::{Errno, FileAttr, FileHandle, FileType, Filesystem, INodeNo, MountOption, ReplyAttr, ReplyDirectory, ReplyEntry, Request, Generation};
use libc::ENOENT;
use log::{debug, error, info};
use std::ffi::OsStr;
use std::sync::Mutex;
use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

mod gitlab;
use crate::gitlab::GitlabClient;
mod inode;
use crate::inode::{FsNode, InodeTracker};

const TTL: Duration = Duration::from_secs(1); // 1 second

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// GitLab instance URL (e.g. https://gitlab.com)
    #[arg(short, long)]
    url: String,

    /// GitLab user or access token
    #[arg(short, long)]
    token: String,

    /// GitLab username (optional)
    #[arg(long)]
    username: Option<String>,

    /// Mount point
    #[arg(short, long)]
    mount: String,
}

struct GitlabFs {
    client: GitlabClient,
    tracker: Mutex<InodeTracker>,
    file_cache: Mutex<HashMap<INodeNo, Vec<u8>>>,
}

impl GitlabFs {
    fn build_attr(&self, ino: INodeNo, node: &FsNode) -> FileAttr {
        let (kind, size) = match node {
            FsNode::Root | FsNode::Projects | FsNode::Namespace { .. } |
            FsNode::Project { .. } | FsNode::BranchDir { .. } | FsNode::GitDir { .. } => {
                (FileType::Directory, 0)
            }
            FsNode::GitFile { project_id, branch, path } => {
                // Return cached size if available, otherwise 0 for now unless queried in getattr
                let cache = self.file_cache.lock().unwrap();
                if let Some(bytes) = cache.get(&ino) {
                    (FileType::RegularFile, bytes.len() as u64)
                } else {
                    (FileType::RegularFile, 1024 * 1024 * 1024) // 1GB dummy size so readers don't truncate early before open
                }
            }
        };

        FileAttr {
            ino,
            size,
            blocks: 0,
            atime: UNIX_EPOCH,
            mtime: UNIX_EPOCH,
            ctime: UNIX_EPOCH,
            crtime: UNIX_EPOCH,
            kind,
            perm: if kind == FileType::Directory { 0o755 } else { 0o444 },
            nlink: if kind == FileType::Directory { 2 } else { 1 },
            uid: 501,
            gid: 20,
            rdev: 0,
            blksize: 512,
            flags: 0,
        }
    }
}

impl Filesystem for GitlabFs {
    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        let name_str = name.to_string_lossy();
        debug!("lookup(parent={}, name={})", parent.0, name_str);
        
        let parent_node = {
            let tracker = self.tracker.lock().unwrap();
            match tracker.get_node(parent.0) {
                Some(node) => node.clone(),
                None => {
                    reply.error(Errno::ENOENT);
                    return;
                }
            }
        };

        // For simplicity, we can just fetch the children of `parent_node` as we do in `readdir`,
        // populate the tracker, and then check if the child exists.
        // This avoids complex individual lookups (except maybe for files).
        let child_node = match parent_node {
            FsNode::Root => {
                if name_str == "projects" { Some(FsNode::Projects) } else { None }
            }
            FsNode::Projects => {
                if let Ok(projects) = self.client.fetch_projects() {
                    projects.into_iter()
                        .filter_map(|p| p.path_with_namespace.split('/').next().map(|s| s.to_string()))
                        .find(|ns| ns == name_str.as_ref())
                        .map(|ns| FsNode::Namespace { name: ns })
                } else { None }
            }
            FsNode::Namespace { name: ns_name } => {
                if let Ok(projects) = self.client.fetch_projects() {
                    projects.into_iter()
                        .find(|p| {
                            let mut parts = p.path_with_namespace.split('/');
                            parts.next() == Some(&ns_name) && p.path == name_str.as_ref()
                        })
                        .map(|p| FsNode::Project { namespace: ns_name.clone(), name: p.path, id: p.id })
                } else { None }
            }
            FsNode::Project { namespace: _, name: _, id } => {
                if let Ok(branches) = self.client.fetch_branches(id) {
                    branches.into_iter()
                        .find(|b| b.name == name_str.as_ref())
                        .map(|b| FsNode::BranchDir { project_id: id, project_name: "".into(), branch: b.name })
                } else { None }
            }
            FsNode::BranchDir { project_id, project_name: _, branch } => {
                if let Ok(tree) = self.client.fetch_tree(project_id, "", &branch) {
                    tree.into_iter()
                        .find(|item| item.name == name_str.as_ref())
                        .map(|item| {
                            if item.item_type == "tree" {
                                FsNode::GitDir { project_id, branch: branch.clone(), path: item.path }
                            } else {
                                FsNode::GitFile { project_id, branch: branch.clone(), path: item.path }
                            }
                        })
                } else { None }
            }
            FsNode::GitDir { project_id, branch, path } => {
                if let Ok(tree) = self.client.fetch_tree(project_id, &path, &branch) {
                    tree.into_iter()
                        .find(|item| item.name == name_str.as_ref())
                        .map(|item| {
                            if item.item_type == "tree" {
                                FsNode::GitDir { project_id, branch: branch.clone(), path: item.path }
                            } else {
                                FsNode::GitFile { project_id, branch: branch.clone(), path: item.path }
                            }
                        })
                } else { None }
            }
            _ => None
        };

        if let Some(node) = child_node {
            let mut tracker = self.tracker.lock().unwrap();
            let child_ino = tracker.insert_or_get(node.clone());
            let attr = self.build_attr(INodeNo(child_ino), &node);
            reply.entry(&TTL, &attr, Generation(0));
        } else {
            reply.error(Errno::ENOENT);
        }
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        debug!("getattr(ino={})", ino.0);
        let node = {
            let tracker = self.tracker.lock().unwrap();
            tracker.get_node(ino.0).cloned()
        };
        
        if let Some(node) = node {
            let mut attr = self.build_attr(ino, &node);
            
            // For files not in cache, try to fetch real size from API to make `ls -l` accurate
            if let FsNode::GitFile { project_id, branch, path } = &node {
                let cache = self.file_cache.lock().unwrap();
                if !cache.contains_key(&ino) {
                    drop(cache);
                    if let Ok(info) = self.client.get_file_info(*project_id, path, branch) {
                        attr.size = info.size;
                    }
                }
            }
            
            reply.attr(&TTL, &attr);
        } else {
            reply.error(Errno::ENOENT);
        }
    }

    fn readdir(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        debug!("readdir(ino={}, offset={})", ino.0, offset);
        
        let node = {
            let tracker = self.tracker.lock().unwrap();
            match tracker.get_node(ino.0) {
                Some(n) => n.clone(),
                None => {
                    reply.error(Errno::ENOENT);
                    return;
                }
            }
        };

        if offset == 0 {
            let _ = reply.add(ino, 0, FileType::Directory, ".");
            let _ = reply.add(INodeNo(1), 1, FileType::Directory, "..");
            
            let mut children_added = 2;
            let mut add_child = |node: FsNode, name: &str, is_dir: bool| {
                let mut tracker = self.tracker.lock().unwrap();
                let child_ino = tracker.insert_or_get(node);
                let ftype = if is_dir { FileType::Directory } else { FileType::RegularFile };
                let _ = reply.add(INodeNo(child_ino), children_added, ftype, name);
                children_added += 1;
            };

            match node {
                FsNode::Root => {
                    add_child(FsNode::Projects, "projects", true);
                }
                FsNode::Projects => {
                    if let Ok(projects) = self.client.fetch_projects() {
                        let mut namespaces = std::collections::HashSet::new();
                        for p in projects {
                            if let Some(ns) = p.path_with_namespace.split('/').next() {
                                namespaces.insert(ns.to_string());
                            }
                        }
                        for ns in namespaces {
                            add_child(FsNode::Namespace { name: ns.clone() }, &ns, true);
                        }
                    }
                }
                FsNode::Namespace { name: ns_name } => {
                    if let Ok(projects) = self.client.fetch_projects() {
                        for p in projects {
                            let mut parts = p.path_with_namespace.split('/');
                            if parts.next() == Some(&ns_name) {
                                add_child(FsNode::Project { namespace: ns_name.clone(), name: p.path.clone(), id: p.id }, &p.path, true);
                            }
                        }
                    }
                }
                FsNode::Project { namespace: _, name: _, id } => {
                    if let Ok(branches) = self.client.fetch_branches(id) {
                        for b in branches {
                            add_child(FsNode::BranchDir { project_id: id, project_name: "".into(), branch: b.name.clone() }, &b.name, true);
                        }
                    }
                }
                FsNode::BranchDir { project_id, project_name: _, branch } => {
                    if let Ok(tree) = self.client.fetch_tree(project_id, "", &branch) {
                        for item in tree {
                            let is_dir = item.item_type == "tree";
                            let node = if is_dir {
                                FsNode::GitDir { project_id, branch: branch.clone(), path: item.path }
                            } else {
                                FsNode::GitFile { project_id, branch: branch.clone(), path: item.path }
                            };
                            add_child(node, &item.name, is_dir);
                        }
                    }
                }
                FsNode::GitDir { project_id, branch, path } => {
                    if let Ok(tree) = self.client.fetch_tree(project_id, &path, &branch) {
                        for item in tree {
                            let is_dir = item.item_type == "tree";
                            let node = if is_dir {
                                FsNode::GitDir { project_id, branch: branch.clone(), path: item.path }
                            } else {
                                FsNode::GitFile { project_id, branch: branch.clone(), path: item.path }
                            };
                            add_child(node, &item.name, is_dir);
                        }
                    }
                }
                _ => {}
            }
        }
        reply.ok();
    }

    fn open(&self, _req: &Request, ino: INodeNo, _flags: fuser::OpenFlags, reply: fuser::ReplyOpen) {
        let node = {
            let tracker = self.tracker.lock().unwrap();
            tracker.get_node(ino.0).cloned()
        };

        match node {
            Some(FsNode::GitFile { project_id, branch, path }) => {
                if let Ok(bytes) = self.client.download_file(project_id, &path, &branch) {
                    let mut cache = self.file_cache.lock().unwrap();
                    cache.insert(ino, bytes);
                    reply.opened(FileHandle(0), fuser::FopenFlags::empty());
                } else {
                    reply.error(Errno::EIO);
                }
            }
            Some(_) => {
                reply.error(Errno::EISDIR);
            }
            None => {
                reply.error(Errno::ENOENT);
            }
        }
    }

    fn read(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: fuser::OpenFlags,
        _lock_owner: Option<fuser::LockOwner>,
        reply: fuser::ReplyData,
    ) {
        let cache = self.file_cache.lock().unwrap();
        if let Some(bytes) = cache.get(&ino) {
            let start = offset as usize;
            if start >= bytes.len() {
                reply.data(&[]);
            } else {
                let end = std::cmp::min(start + size as usize, bytes.len());
                reply.data(&bytes[start..end]);
            }
        } else {
            reply.error(Errno::EBADF);
        }
    }

    fn release(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        _flags: fuser::OpenFlags,
        _lock_owner: Option<fuser::LockOwner>,
        _flush: bool,
        reply: fuser::ReplyEmpty,
    ) {
        let mut cache = self.file_cache.lock().unwrap();
        cache.remove(&ino);
        reply.ok();
    }
}

fn main() -> anyhow::Result<()> {
    env_logger::init();
    let args = Args::parse();

    let client = GitlabClient::new(args.url, args.token);
    let fs = GitlabFs {
        client,
        tracker: Mutex::new(InodeTracker::new()),
        file_cache: Mutex::new(HashMap::new()),
    };

    let mountpoint = args.mount;
    let options = vec![MountOption::RO, MountOption::FSName("gitlabfs".to_string())];
    
    let mut config = fuser::Config::default();
    config.mount_options = options;

    info!("Mounting GitlabFS at {}...", mountpoint);
    fuser::mount2(fs, mountpoint, &config)?;

    Ok(())
}
