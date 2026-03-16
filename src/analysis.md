Here is a thorough analysis across **security** and **Rust best-practices** dimensions, organized by severity.

--- 

## 🔴 Critical — Security

### 1. Token exposed as `pub` field — [`GitlabClient`](src/gitlab.rs:6)
The `token` field is `pub`, meaning any code holding a reference to the struct can read the secret. Use `pub(crate)` at most, or keep it private.

### 2. Token passed as CLI argument — [`Args.token`](src/main.rs:25)
CLI arguments are visible to every process via `/proc/<pid>/cmdline`, `ps aux`, and shell history. The token should come from an environment variable (`GITLAB_TOKEN`) or a file path, never a named CLI flag.

### 3. No HTTPS enforcement — [`GitlabClient::request()`](src/gitlab.rs:42)
```rust
let full_url = format!("{}/api/v4{}", self.url, path);
```
If the user passes `http://...`, the `PRIVATE-TOKEN` header is sent in cleartext. Validate that `self.url` starts with `https://` in [`GitlabClient::new()`](src/gitlab.rs:38).

### 4. FUSE `_req` uid/pid ignored — all handlers in [`main.rs`](src/main.rs:80)
Every FUSE callback receives a `_req: &Request` with the caller's `uid`/`pid`. All handlers ignore it (prefixed `_`). This means **any local user** on the machine can `ls`/`cat` through the mount point and read repository files authenticated with your token. Compare `req.uid()` to the mounting user's uid.

---

## 🟠 High — Memory / Correctness

### 5. Unbounded file cache — [`GitlabFs.file_cache`](src/main.rs:39)
Every `open()` loads the entire file into RAM and holds it until `release()`. Opening many large files causes unbounded memory growth. Add a max size cap or LRU eviction.

### 6. 1 GiB dummy file size — [`build_attr()`](src/main.rs:55)
```rust
(FileType::RegularFile, 1024 * 1024 * 1024) // 1GB dummy size
```
Tools like `cp` and `mmap`-based readers pre-allocate based on `stat.st_size`. Reporting 1 GiB for every uncached file causes OOM in callers. Use `0` as the unknown-size sentinel (standard FUSE practice); `getattr` already fetches the real size on lines 178–185.

### 7. `read_to_end` without size limit — [`download_file()`](src/gitlab.rs:93)
```rust
resp.into_reader().read_to_end(&mut bytes)?;
```
No upper bound on response size — a server returning a huge stream causes OOM. Use `.take(MAX_FILE_BYTES)` before `read_to_end`.

### 8. Lossy `offset as usize` cast — [`read()`](src/main.rs:332)
```rust
let start = offset as usize;
```
On 32-bit targets `u64 → usize` truncates silently. Use `usize::try_from(offset).unwrap_or(usize::MAX)`.

---

## 🟡 Medium — Robustness

### 9. `.unwrap()` on every Mutex lock — e.g. [`build_attr()`](src/main.rs:51), [`readdir()`](src/main.rs:221)
If any thread panics while holding a lock, all subsequent `.unwrap()` calls on other threads panic (poisoned mutex), crashing the FUSE daemon. Use `.unwrap_or_else(|e| e.into_inner())` or reply with `Errno::EIO`.

### 10. Silent truncation at 100 items — [`fetch_projects()`](src/gitlab.rs:49), [`fetch_branches()`](src/gitlab.rs:54), [`fetch_tree()`](src/gitlab.rs:61)
GitLab's API paginates via `X-Next-Page`. Any namespace with >100 projects, branches, or tree entries is silently truncated. Implement pagination.

---

## 🔵 Low — Rust Best Practices

### 11. Hardcoded macOS-style uid/gid — [`build_attr()`](src/main.rs:71)
```rust
uid: 501,
gid: 20,
```
501/20 are macOS defaults; on Linux these map to the wrong or non-existent users. Use `unsafe { libc::getuid() }` / `libc::getgid()` at startup.

### 12. `pub` fields break encapsulation — [`gitlab.rs:7-8`](src/gitlab.rs:7)
Beyond the security concern, exposing internal fields via `pub` prevents future refactoring without breaking callers. Make them private.

### 13. Placeholder `project_name: ""` — [`main.rs:125`](src/main.rs:125), [`main.rs:258`](src/main.rs:258)
`BranchDir.project_name` is always constructed as `""` and always matched as `_`. Either remove the field from the enum variant or populate it.

### 14. Unused dependencies — [`Cargo.toml:11`](Cargo.toml:11)
`libc` and `serde_json` are listed but not directly used in source (`ureq` handles JSON via its own feature). Remove them to reduce attack surface and compile time. Confirm with `cargo +nightly udeps`.

### 15. Token not zeroed on drop — [`gitlab.rs:8`](src/gitlab.rs:8)
The token `String` sits in heap memory until the allocator reuses the page. Wrap it in `secrecy::SecretString` (the `secrecy` crate) to guarantee zeroing on drop.

### 16. `readdir` silently ignores non-zero offsets — [`readdir()`](src/main.rs:215)
```rust
if offset == 0 { /* populate */ }
reply.ok(); // always OK even if offset > 0
```
This is safe only if the kernel never re-requests with a non-zero offset. For large directories (hitting the 100-item truncation), the kernel may paginate and receive empty responses it misinterprets.

---

## Summary Table

| # | File | Issue | Severity |
|---|------|-------|----------|
| 1 | [`gitlab.rs:7`](src/gitlab.rs:7) | Token in `pub` field | 🔴 Critical |
| 2 | [`main.rs:25`](src/main.rs:25) | Token via CLI arg (ps/history exposure) | 🔴 Critical |
| 3 | [`gitlab.rs:42`](src/gitlab.rs:42) | No HTTPS enforcement | 🔴 Critical |
| 4 | all handlers | FUSE `_req` uid ignored — any local user can read | 🔴 Critical |
| 5 | [`main.rs:39`](src/main.rs:39) | Unbounded file cache | 🟠 High |
| 6 | [`main.rs:55`](src/main.rs:55) | 1 GiB dummy size causes caller OOM | 🟠 High |
| 7 | [`gitlab.rs:93`](src/gitlab.rs:93) | `read_to_end` without size cap | 🟠 High |
| 8 | [`main.rs:332`](src/main.rs:332) | Lossy `offset as usize` cast | 🟠 High |
| 9 | multiple | `.unwrap()` on Mutex — panics on poisoning | 🟡 Medium |
| 10 | [`gitlab.rs:49`](src/gitlab.rs:49) | No pagination (silent 100-item truncation) | 🟡 Medium |
| 11 | [`main.rs:71`](src/main.rs:71) | Hardcoded uid=501/gid=20 | 🔵 Low |
| 12 | [`gitlab.rs:7`](src/gitlab.rs:7) | `pub` fields break encapsulation | 🔵 Low |
| 13 | [`main.rs:125`](src/main.rs:125) | Placeholder `project_name: ""` in inode | 🔵 Low |
| 14 | [`Cargo.toml:11`](Cargo.toml:11) | Unused `libc`/`serde_json` dependencies | 🔵 Low |
| 15 | [`gitlab.rs:8`](src/gitlab.rs:8) | Token not zeroed on drop | 🔵 Low |
| 16 | [`main.rs:215`](src/main.rs:215) | `readdir` silently ignores non-zero offsets | 🔵 Low |