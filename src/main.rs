use clap::Parser;

use log::{debug, info};
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
    let token = match std::env::var("GITLAB_TOKEN") {
        Ok(t) => t,
        Err(_) => {
            eprintln!("Error: GITLAB_TOKEN environment variable is required but not set.");
            eprintln!("Please set it using: export GITLAB_TOKEN='your_personal_access_token'");
            std::process::exit(1);
        }
    };

    let client = GitlabClient::new(args.url, token)?;
    let fs = GitlabFs {
        client,
        tracker: Mutex::new(InodeTracker::new()),
        file_cache: Mutex::new(LruCache::new(NonZeroUsize::new(32).unwrap())),
        uid: unsafe { libc::getuid() },
        gid: unsafe { libc::getgid() },
    };

    let mountpoint = args.mount;
    let options = vec![MountOption::RO, MountOption::FSName("gitlabfs".to_string())];

    let mut config = fuser::Config::default();
    config.mount_options = options;

    info!("Mounting GitlabFS at {}...", mountpoint);
    fuser::mount2(fs, mountpoint, &config)?;

    Ok(())
}
