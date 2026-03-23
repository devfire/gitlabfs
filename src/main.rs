use anyhow::Context;
use clap::Parser;

use log::info;
use lru::LruCache;

use std::num::NonZeroUsize;
use std::sync::Mutex;
use std::time::Duration;

mod gitlab;
mod gitlabfs;
use crate::gitlab::GitlabClient;
use crate::gitlabfs::GitlabFs;
use fuser::MountOption;
mod inode;
use crate::inode::InodeTracker;

const TTL: Duration = Duration::from_secs(1); // 1 second

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// GitLab instance URL (e.g. https://gitlab.com)
    #[arg(short, long)]
    url: String,

    /// GitLab username (optional)
    #[arg(long)]
    username: Option<String>,

    /// Mount point
    #[arg(short, long)]
    mount: String,
}

fn main() -> anyhow::Result<()> {
    env_logger::init();
    let args = Args::parse();

    let token = std::env::var("GITLAB_TOKEN").context("GITLAB_TOKEN not found")?;

    let client = GitlabClient::new(args.url, token)?;

    let fs = GitlabFs {
        client,
        tracker: Mutex::new(InodeTracker::new()),
        file_cache: Mutex::new(LruCache::new(NonZeroUsize::new(32).unwrap())),
        uid: unsafe { libc::getuid() },
        gid: unsafe { libc::getgid() },
    };

    let mountpoint = args.mount;

    // Validate mountpoint exists before attempting to mount
    if !std::path::Path::new(&mountpoint).exists() {
        anyhow::bail!("Mount point '{}' does not exist", mountpoint);
    }

    let options = vec![MountOption::RO, MountOption::FSName("gitlabfs".to_string())];

    let mut config = fuser::Config::default();
    config.mount_options = options;

    info!("Mounting GitlabFS at {}...", mountpoint);
    eprintln!("GitlabFS mounted at '{}'. Press Ctrl+C to unmount.", mountpoint);
    fuser::mount2(fs, &mountpoint, &config)
        .with_context(|| format!(
            "Failed to mount at '{}'. Make sure 'fusermount'/'fusermount3' is installed \
             (e.g. `sudo apt install fuse` or `sudo pacman -S fuse2`) and the mountpoint is a directory",
            mountpoint
        ))?;

    Ok(())
}
