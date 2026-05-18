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

### Setting up a remote

mbr hardcodes a short setup form for most rclone backends (local
folders, SFTP, SMB, WebDAV, S3, B2, Azure, GCS, Drive, Dropbox, OneDrive,
pCloud, and many more; see `mbr remote types`) — derived from rclone's
documentation and cross-checked against
the embedded rclone's own metadata at test time (`cargo test`), so a
renamed option upstream fails the build instead of producing a form
rclone silently ignores.

In the GUI, pick a backend and fill in the generated form; in the CLI,
`mbr remote types` lists the backends and their fields, and
`mbr remote setup <name> <type>` asks for them. Answers are handed to
rclone in one shot (`config/create` with prefilled parameters); anything
rclone still needs afterwards — typically a browser sign-in for OAuth
backends — comes back as a single question the frontends relay.

For OAuth backends (Google Drive, Dropbox, OneDrive, …) the form is
mostly optional client credentials; the sign-in itself happens in the
browser, and the auth link is also printed (CLI) or shown inline (GUI) in
case no browser opens. `mbr remote add` still accepts any pre-existing
rclone target, e.g. one configured with the `rclone` executable itself.

### Scanning remotes

Scanning a remote lists everything stored there and makes the database
match: blobs recorded there but gone lose their record, and every blob
found is recorded as stored there. Blobs the database has never heard of
(pushed from another folder, say) are added too, and given a name in the
`unnamed/` folder: `unnamed/<hash>`, a symlink like any other tracked
file, dangling until the blob is downloaded. Rename or move it like any
other file.

## CLI

```
mbr init [dir]                    attach mbr to a folder
mbr scan                          ingest new files, record removals
mbr ls                            list tracked files with metadata
mbr info <path|hash>              full detail for one file or blob
mbr remote add <name> <target>    add an existing rclone target
mbr remote types                  list the rclone backends mbr can set up
mbr remote setup <name> <type>    configure an rclone remote interactively
mbr remote ls                     list remotes
mbr remote show <name>            target and rclone settings (secrets hidden)
mbr remote edit <name> [--rename NEW] [--target T] [--set KEY=VALUE...]
                                  rename, retarget, or change rclone settings
mbr remote scan [name...]         list remotes fully, record what they hold
mbr remote rm <name>              remove a remote
mbr push <remote> [path|hash...]  upload blobs (default: all not yet there)
mbr pull <remote> [path|hash...]  download from one remote (default: all it
                                  holds that is missing here)
mbr fetch <path|hash>...          download blobs from any remote holding them
mbr check <remote> [path|hash...] ask a remote about specific blobs
                                  (without any: same as `remote scan`)
```

Every command except `init` works from anywhere inside an attached folder.
Files and blobs are addressable by path, full hash, or a unique hash
prefix (≥ 6 hex chars).

## GUI

`my-beloved-rubble` has two tabs.

**Blobs** shows the annotated file listing (green dot = present locally).
Select files with a click, Ctrl/⌘+click (toggle), Shift+click (range),
by dragging across rows, or Ctrl+A (all; Esc clears). The sidebar shows
the details of a single selected file, or a summary of several, and acts
on the selected blobs: upload them to a remote, check whether a remote
holds them, or download the ones missing locally.

**Remotes** lists the remotes with Scan (and Scan all), Download all
(everything recorded there that is missing here), Upload all, Edit and
Remove. The sidebar adds remotes — an existing rclone target, or a
guided setup: pick a backend and fill in the generated form (secrets
masked, choices as dropdowns, required fields marked); if rclone still
needs an answer — usually OAuth — the question is shown inline. Edit
renames or retargets a remote and, for backends mbr has a form for,
changes its rclone settings (secrets stay hidden; blank keeps them).

Scans and rclone operations run in the background, backed by the
repository-local `.mbr/rclone.conf`. The last opened folder is reopened
on start.

## Building

```
cargo build --release   # target/release/mbr and target/release/my-beloved-rubble
```

Rclone is embedded through `librclone`; building requires a Go toolchain and C compiler, but no separate `rclone` executable is required at runtime.
