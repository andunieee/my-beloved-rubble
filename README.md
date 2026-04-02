# my beloved rubble

Content-addressed folders in the spirit of git-annex, with rclone remotes.
One core library, two frontends: the `mbr` CLI and the `my-beloved-rubble`
Slint GUI.

## The model

Attach mbr to any folder (`mbr init`, or "Open Folder" in the GUI). From
then on, whenever the folder is inspected (a scan), every regular file in
it is moved into a hidden content-addressed store and replaced by a
symlink, so it still opens like a normal file:

```
folder/
  .mbr/
    blobs/fa/2e/fa2e38…(full sha256)   the actual content
    mbr.db                            sqlite metadata
    rclone.conf                       repository-local rclone config
  fruits.png -> .mbr/blobs/fa/2e/fa2e38…
```

`.mbr/mbr.db` tracks:

- **paths** — every name the folder ever held: which blob it pointed to,
  when it was added, when it was removed. Replacing a file's content
  closes the old row and opens a new one, so previous versions stay
  addressable as long as their blobs are stored somewhere.
- **blobs** — hash → size.
- **remotes** — named rclone targets (e.g. `backup` → `s3:bucket/mbr`).
- **blob_remotes** — which blobs are known to live on which remotes.

For each file the UIs show: size, hash, whether the content is present
locally (the filesystem is the source of truth for that), which remotes
store it, other names — current or past — pointing at the same blob, and
previous versions of the name with where those old blobs still live.

## Remotes

Blobs are pushed to rclone remotes named purely by hash, in the same
sharded `fa/2e/fa2e38…` layout, with no metadata. Encryption is rclone's
job: point mbr at an rclone `crypt` remote if you want it. Each attached
folder owns its rclone configuration at `.mbr/rclone.conf`.

## CLI

```
mbr init [dir]                    attach mbr to a folder
mbr scan                          ingest new files, record removals
mbr ls                            list tracked files with metadata
mbr info <path|hash>              full detail for one file or blob
mbr remote add <name> <target>    add an existing rclone target
mbr remote setup <name> <type>    configure an rclone remote interactively
mbr remote rm <name>              remove a remote
mbr remote ls                     list remotes
mbr push <remote> [path|hash...]  push blobs (default: all not yet there)
mbr fetch <path|hash>...          download blobs missing locally
mbr check <remote>                verify reachability and blob presence
```

Every command except `init` works from anywhere inside an attached folder.
Files and blobs are addressable by path, full hash, or a unique hash
prefix (≥ 6 hex chars).

## GUI

`my-beloved-rubble` shows the annotated file listing (green dot = present
locally), a detail pane for the selected file, and remote management with
Check / Push all / per-file Push and Fetch buttons, plus Add Existing
Remote and an interactive Configure Remote flow (backend questions answered
one at a time, backed by the repository-local `.mbr/rclone.conf`). rclone
operations run in the background. The last opened folder is reopened on start.

## Building

```
cargo build --release   # target/release/mbr and target/release/my-beloved-rubble
```

Rclone is embedded through `librclone`; building requires a Go toolchain and C compiler, but no separate `rclone` executable is required at runtime.
