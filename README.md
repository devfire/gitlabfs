# gitlabfs

Mount your GitLab projects as a read-only filesystem.

```
/projects/{namespace}/{project}/{branch}/...
```

## Requirements

- Linux with FUSE support (`libfuse3`)
- A GitLab personal access token with `read_api` + `read_repository` scopes

## Usage

```sh
export GITLAB_TOKEN=glpat-...
cargo run -- --url https://gitlab.com --mount /mnt/gitlab
```

Then browse your projects normally:

```sh
ls /mnt/gitlab/projects/
cat /mnt/gitlab/projects/mygroup/myrepo/main/README.md
```

To unmount:

```sh
fusermount -u /mnt/gitlab
```

## Notes

- HTTPS is required (token protection)
- Only projects you're a member of are shown
- Files are fetched on open and cached in memory (LRU, 32 slots); everything else is fetched live
- Write operations are not supported
