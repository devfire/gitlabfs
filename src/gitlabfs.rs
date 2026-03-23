use fuser::{
    Errno, FileAttr, FileHandle, FileType, Filesystem, Generation, INodeNo, ReplyAttr,
    ReplyDirectory, ReplyEntry, Request,
};
use log::{debug, warn};
use lru::LruCache;
use std::ffi::OsStr;
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

use crate::TTL;
use crate::gitlab::GitlabClient;
use crate::inode::{FsNode, InodeTracker};
pub struct GitlabFs {
    pub(crate) client: GitlabClient,
    pub(crate) tracker: Mutex<InodeTracker>,
    pub(crate) file_cache: Mutex<LruCache<INodeNo, Vec<u8>>>,
    pub(crate) uid: u32,
    pub(crate) gid: u32,
}

impl GitlabFs {
    fn check_access(&self, req: &Request) -> bool {
        req.uid() == self.uid || req.uid() == 0
    }

    fn build_attr(&self, ino: INodeNo, node: &FsNode) -> FileAttr {
        let (kind, size) = match node {
            FsNode::Root
            | FsNode::Projects
            | FsNode::Namespace { .. }
            | FsNode::Project { .. }
            | FsNode::BranchDir { .. }
            | FsNode::GitDir { .. } => (FileType::Directory, 0),
            FsNode::GitFile {
                project_id: _,
                branch: _,
                path: _,
            } => {
                // Return cached size if available, otherwise 0
                let cache = self.file_cache.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(bytes) = cache.peek(&ino) {
                    (FileType::RegularFile, bytes.len() as u64)
                } else {
                    (FileType::RegularFile, 0)
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
            perm: if kind == FileType::Directory {
                0o755
            } else {
                0o444
            },
            nlink: if kind == FileType::Directory { 2 } else { 1 },
            uid: self.uid,
            gid: self.gid,
            rdev: 0,
            blksize: 512,
            flags: 0,
        }
    }
}

impl Filesystem for GitlabFs {
    fn lookup(&self, req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        if !self.check_access(req) {
            reply.error(Errno::EACCES);
            return;
        }

        let name_str = name.to_string_lossy();
        debug!("lookup(parent={}, name={})", parent.0, name_str);

        let parent_node = {
            let tracker = self.tracker.lock().unwrap_or_else(|e| e.into_inner());
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
                if name_str == "projects" {
                    Some(FsNode::Projects)
                } else {
                    None
                }
            }
            FsNode::Projects => {
                if let Ok(projects) = self.client.fetch_projects() {
                    projects
                        .into_iter()
                        .filter_map(|p| {
                            p.path_with_namespace
                                .split('/')
                                .next()
                                .map(|s| s.to_string())
                        })
                        .find(|ns| ns == name_str.as_ref())
                        .map(|ns| FsNode::Namespace { name: ns })
                } else {
                    None
                }
            }
            FsNode::Namespace { name: ns_name } => {
                if let Ok(projects) = self.client.fetch_projects() {
                    projects
                        .into_iter()
                        .find(|p| {
                            let mut parts = p.path_with_namespace.split('/');
                            parts.next() == Some(&ns_name) && p.path == name_str.as_ref()
                        })
                        .map(|p| FsNode::Project {
                            namespace: ns_name.clone(),
                            name: p.path,
                            id: p.id,
                        })
                } else {
                    None
                }
            }
            FsNode::Project {
                namespace: _,
                name: _,
                id,
            } => {
                if let Ok(branches) = self.client.fetch_branches(id) {
                    branches
                        .into_iter()
                        .find(|b| b.name == name_str.as_ref())
                        .map(|b| FsNode::BranchDir {
                            project_id: id,
                            branch: b.name,
                        })
                } else {
                    None
                }
            }
            FsNode::BranchDir { project_id, branch } => {
                if let Ok(tree) = self.client.fetch_tree(project_id, "", &branch) {
                    tree.into_iter()
                        .find(|item| item.name == name_str.as_ref())
                        .map(|item| {
                            if item.item_type == "tree" {
                                FsNode::GitDir {
                                    project_id,
                                    branch: branch.clone(),
                                    path: item.path,
                                }
                            } else {
                                FsNode::GitFile {
                                    project_id,
                                    branch: branch.clone(),
                                    path: item.path,
                                }
                            }
                        })
                } else {
                    None
                }
            }
            FsNode::GitDir {
                project_id,
                branch,
                path,
            } => {
                if let Ok(tree) = self.client.fetch_tree(project_id, &path, &branch) {
                    tree.into_iter()
                        .find(|item| item.name == name_str.as_ref())
                        .map(|item| {
                            if item.item_type == "tree" {
                                FsNode::GitDir {
                                    project_id,
                                    branch: branch.clone(),
                                    path: item.path,
                                }
                            } else {
                                FsNode::GitFile {
                                    project_id,
                                    branch: branch.clone(),
                                    path: item.path,
                                }
                            }
                        })
                } else {
                    None
                }
            }
            _ => None,
        };

        if let Some(node) = child_node {
            let mut tracker = self.tracker.lock().unwrap_or_else(|e| e.into_inner());
            let child_ino = tracker.insert_or_get(node.clone());
            tracker.inc_lookup(child_ino);
            let attr = self.build_attr(INodeNo(child_ino), &node);
            reply.entry(&TTL, &attr, Generation(0));
        } else {
            reply.error(Errno::ENOENT);
        }
    }

    fn getattr(&self, req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        if !self.check_access(req) {
            reply.error(Errno::EACCES);
            return;
        }

        debug!("getattr(ino={})", ino.0);
        let node = {
            let tracker = self.tracker.lock().unwrap_or_else(|e| e.into_inner());
            tracker.get_node(ino.0).cloned()
        };

        match node {
            Some(n) => {
                let mut attr = self.build_attr(ino, &n);
                // For files not in cache, try to fetch real size from API to make `ls -l` accurate
                if let FsNode::GitFile {
                    project_id,
                    branch,
                    path,
                } = &n
                {
                    let cache = self.file_cache.lock().unwrap_or_else(|e| e.into_inner());
                    if !cache.contains(&ino) {
                        drop(cache);
                        if let Ok(info) = self.client.get_file_info(*project_id, path, branch) {
                            attr.size = info.size;
                        }
                    }
                }
                reply.attr(&TTL, &attr);
            }
            None => reply.error(Errno::ENOENT),
        }
    }

    fn readdir(
        &self,
        req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        if !self.check_access(req) {
            reply.error(Errno::EACCES);
            return;
        }

        debug!("readdir(ino={}, offset={})", ino.0, offset);

        let node = {
            let tracker = self.tracker.lock().unwrap_or_else(|e| e.into_inner());
            match tracker.get_node(ino.0) {
                Some(n) => n.clone(),
                None => {
                    reply.error(Errno::ENOENT);
                    return;
                }
            }
        };

        let mut entries = Vec::new();
        entries.push((ino, FileType::Directory, ".".to_string()));
        entries.push((INodeNo(1), FileType::Directory, "..".to_string()));

        let mut tracker = self.tracker.lock().unwrap_or_else(|e| e.into_inner());

        match node {
            FsNode::Root => {
                let child_node = FsNode::Projects;
                let child_ino = tracker.insert_or_get(child_node);
                entries.push((
                    INodeNo(child_ino),
                    FileType::Directory,
                    "projects".to_string(),
                ));
            }
            FsNode::Projects => {
                match self.client.fetch_projects() {
                    Ok(projects) => {
                        let mut namespaces = std::collections::HashSet::new();
                        for p in projects {
                            if let Some(ns) = p.path_with_namespace.split('/').next() {
                                namespaces.insert(ns.to_string());
                            }
                        }
                        for ns in namespaces {
                            let child_node = FsNode::Namespace { name: ns.clone() };
                            let child_ino = tracker.insert_or_get(child_node);
                            entries.push((INodeNo(child_ino), FileType::Directory, ns));
                        }
                    }
                    Err(e) => warn!("readdir(Projects): fetch_projects failed: {}", e),
                }
            }
            FsNode::Namespace { name: ns_name } => {
                match self.client.fetch_projects() {
                    Ok(projects) => {
                        for p in projects {
                            let mut parts = p.path_with_namespace.split('/');
                            if parts.next() == Some(&ns_name) {
                                let child_node = FsNode::Project {
                                    namespace: ns_name.clone(),
                                    name: p.path.clone(),
                                    id: p.id,
                                };
                                let child_ino = tracker.insert_or_get(child_node);
                                entries.push((INodeNo(child_ino), FileType::Directory, p.path));
                            }
                        }
                    }
                    Err(e) => warn!("readdir(Namespace '{}'): fetch_projects failed: {}", ns_name, e),
                }
            }
            FsNode::Project {
                namespace: _,
                name: _,
                id,
            } => {
                match self.client.fetch_branches(id) {
                    Ok(branches) => {
                        for b in branches {
                            let child_node = FsNode::BranchDir {
                                project_id: id,
                                branch: b.name.clone(),
                            };
                            let child_ino = tracker.insert_or_get(child_node);
                            entries.push((INodeNo(child_ino), FileType::Directory, b.name));
                        }
                    }
                    Err(e) => warn!("readdir(Project id={}): fetch_branches failed: {}", id, e),
                }
            }
            FsNode::BranchDir { project_id, branch } => {
                match self.client.fetch_tree(project_id, "", &branch) {
                    Ok(tree) => {
                        for item in tree {
                            let is_dir = item.item_type == "tree";
                            let node = if is_dir {
                                FsNode::GitDir {
                                    project_id,
                                    branch: branch.clone(),
                                    path: item.path.clone(),
                                }
                            } else {
                                FsNode::GitFile {
                                    project_id,
                                    branch: branch.clone(),
                                    path: item.path.clone(),
                                }
                            };
                            let child_ino = tracker.insert_or_get(node);
                            let ftype = if is_dir { FileType::Directory } else { FileType::RegularFile };
                            entries.push((INodeNo(child_ino), ftype, item.name));
                        }
                    }
                    Err(e) => warn!("readdir(BranchDir project={} branch='{}'): fetch_tree failed: {}", project_id, branch, e),
                }
            }
            FsNode::GitDir {
                project_id,
                branch,
                path,
            } => {
                match self.client.fetch_tree(project_id, &path, &branch) {
                    Ok(tree) => {
                        for item in tree {
                            let is_dir = item.item_type == "tree";
                            let node = if is_dir {
                                FsNode::GitDir {
                                    project_id,
                                    branch: branch.clone(),
                                    path: item.path.clone(),
                                }
                            } else {
                                FsNode::GitFile {
                                    project_id,
                                    branch: branch.clone(),
                                    path: item.path.clone(),
                                }
                            };
                            let child_ino = tracker.insert_or_get(node);
                            let ftype = if is_dir { FileType::Directory } else { FileType::RegularFile };
                            entries.push((INodeNo(child_ino), ftype, item.name));
                        }
                    }
                    Err(e) => warn!("readdir(GitDir project={} branch='{}' path='{}'): fetch_tree failed: {}", project_id, branch, path, e),
                }
            }
            _ => {}
        }

        drop(tracker);

        // Iterate over collected entries, respecting the offset
        for (i, (child_ino, ftype, name)) in entries
            .into_iter()
            .enumerate()
            .skip(usize::try_from(offset).unwrap_or(usize::MAX))
        {
            if reply.add(child_ino, (i + 1) as _, ftype, &name) {
                break;
            }
        }
        reply.ok();
    }

    fn open(&self, req: &Request, ino: INodeNo, _flags: fuser::OpenFlags, reply: fuser::ReplyOpen) {
        if !self.check_access(req) {
            reply.error(Errno::EACCES);
            return;
        }

        let node = {
            let tracker = self.tracker.lock().unwrap_or_else(|e| e.into_inner());
            tracker.get_node(ino.0).cloned()
        };

        match node {
            Some(FsNode::GitFile {
                project_id,
                branch,
                path,
            }) => match self.client.download_file(project_id, &path, &branch) {
                Ok(bytes) => {
                    let mut cache = self.file_cache.lock().unwrap_or_else(|e| e.into_inner());
                    let _ = cache.put(ino, bytes);
                    reply.opened(FileHandle(0), fuser::FopenFlags::empty());
                }
                Err(_) => reply.error(Errno::EIO),
            },
            Some(_) => {
                reply.error(Errno::EISDIR);
            }
            None => reply.error(Errno::ENOENT),
        }
    }

    fn read(
        &self,
        req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        size: u32,
        _flags: fuser::OpenFlags,
        _lock_owner: Option<fuser::LockOwner>,
        reply: fuser::ReplyData,
    ) {
        if !self.check_access(req) {
            reply.error(Errno::EACCES);
            return;
        }

        let mut cache = self.file_cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(bytes) = cache.get_mut(&ino) {
            let start = usize::try_from(offset).unwrap_or(usize::MAX);
            if start >= bytes.len() {
                reply.data(&[]);
            } else {
                let end = std::cmp::min(start.saturating_add(size as usize), bytes.len());
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
        let mut cache = self.file_cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.pop(&ino);
        reply.ok();
    }

    fn forget(&self, _req: &Request, ino: INodeNo, nlookup: u64) {
        let mut tracker = self.tracker.lock().unwrap_or_else(|e| e.into_inner());
        tracker.forget(ino.0, nlookup);
    }
}