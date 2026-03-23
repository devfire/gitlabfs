The caching in this repository is managed comprehensively across a few levels to optimize requests to the GitLab API and balance memory usage since the filesystem could be browsing massive directory trees.

Here is a breakdown of where caching occurs in [GitlabFs](cci:2://file:///home/ig/Documents/rust/gitlabfs/src/gitlabfs.rs:13:0-19:1):

### 1. File Contents Cache (`file_cache`)
The raw bytes of file contents are cached in memory using an **LRU (Least Recently Used) Cache** bounded to 32 entries (set in [src/main.rs](cci:7://file:///home/ig/Documents/rust/gitlabfs/src/main.rs:0:0-0:0)). You can see it in [gitlabfs.rs](cci:7://file:///home/ig/Documents/rust/gitlabfs/src/gitlabfs.rs:0:0-0:0) here:

```rust
pub(crate) file_cache: Mutex<LruCache<INodeNo, Vec<u8>>>,
```

- When you access a file, [open()](cci:1://file:///home/ig/Documents/rust/gitlabfs/src/gitlabfs.rs:427:4-456:5) fetches the raw file blob directly from the GitLab API and saves the bytes into `file_cache`.
- On [read()](cci:1://file:///home/ig/Documents/rust/gitlabfs/src/gitlabfs.rs:458:4-486:5), the bytes are streamed safely from the cache via `.get_mut()`.
- Upon FUSE calling [release()](cci:1://file:///home/ig/Documents/rust/gitlabfs/src/gitlabfs.rs:488:4-501:5) (when an application closes the file descriptor), the buffer is explicitly wiped cleanly from the cache via `cache.pop(&ino)`.
- **Note:** Evicting on [release](cci:1://file:///home/ig/Documents/rust/gitlabfs/src/gitlabfs.rs:488:4-501:5) essentially acts as an Active File Buffer protecting memory from unbounded OOM issues.

### 2. Inode Structure Caching ([InodeTracker](cci:2://file:///home/ig/Documents/rust/gitlabfs/src/inode.rs:13:0-18:1))
Because FUSE operates entirely with integers representing elements (`inodes`) but GitLab's tree hierarchy relies on full strings representing `branch/path.rs`, we cache the actual filesystem structure in [src/inode.rs](cci:7://file:///home/ig/Documents/rust/gitlabfs/src/inode.rs:0:0-0:0):

```rust
ino_to_node: HashMap<u64, FsNode>,
node_to_ino: HashMap<FsNode, u64>,
```

- Every time [readdir](cci:1://file:///home/ig/Documents/rust/gitlabfs/src/gitlabfs.rs:255:4-425:5) or [lookup](cci:1://file:///home/ig/Documents/rust/gitlabfs/src/gitlabfs.rs:74:4-217:5) parses a project, directory, or file name, it caches a static `FsNode` mapping bidirectionally mapped to a unique `ino` increment.
- **Reference Counting**: `tracker.inc_lookup(child_ino)` is invoked mapping to the Kernel's native VFS cache. Entries correctly remain in memory until the FUSE API kernel triggers a [forget()](cci:1://file:///home/ig/Documents/rust/gitlabfs/src/inode.rs:61:4-72:5) event. Our manual callback drops the memory hash.

### 3. File Size Optimization
In [getattr()](cci:1://file:///home/ig/Documents/rust/gitlabfs/src/gitlabfs.rs:219:4-253:5), checking a file's size often requires executing an expensive metadata call per file to properly display attributes like `ls -l`. We first leverage the `file_cache` via `<LruCache>.peek()`—if the file bytes physically exist locally, we just use its `.len()`. Only on a cache miss will it ask the client:

```rust
// gitlabfs.rs:242
let cache = self.file_cache.lock().unwrap_or_else(|e| e.into_inner());
if !cache.contains(&ino) {
    drop(cache);
    if let Ok(info) = self.client.get_file_info(*project_id, path, branch) {
        ...
```

So generally, "caching" functions in three layers:
1. `HashMap` for hierarchical tree nodes ([InodeTracker](cci:2://file:///home/ig/Documents/rust/gitlabfs/src/inode.rs:13:0-18:1)).
2. API caching for file `HEAD` size optimization ([build_attr](cci:1://file:///home/ig/Documents/rust/gitlabfs/src/gitlabfs.rs:26:4-70:5) / [getattr](cci:1://file:///home/ig/Documents/rust/gitlabfs/src/gitlabfs.rs:219:4-253:5)).
3. Managed memory Blob loading via `LRU` tracking `Vec<u8>` arrays during active streams.