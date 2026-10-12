# my beloved rubble

Content-addressed folders in the spirit of git-annex, with rclone remotes.
One core library, two frontends: the `mbr` CLI and the `my-beloved-rubble`
Slint GUI.

The user guide lives in `docs/` (a [zola](https://www.getzola.org) site,
`zola --root docs serve` to preview) and is published to
<https://andunieee.github.io/my-beloved-rubble/> on every push to master.
The GUI's "?" buttons link into it.

## The model

Attach mbr to any folder (`mbr init`, or "Open Folder" in the GUI). From
then on, whenever the folder is inspected (a scan), every regular file in
it is moved into a hidden content-addressed store and replaced by a
symlink, so it still opens like a normal file. Stored blobs are read-only:
a program that writes into a file in place gets an error instead of
silently changing content stored under its old hash (saving a new file
over the name works, and is recorded as a new version):

```
folder/
  .mbr/
    blobs/fa/2e/fa2e38…(full sha256)   the actual content
    mbr.db                            sqlite metadata
    rclone.conf                       repository-local rclone config
    corrupt/                          blobs that failed verification
    tmp/                              scratch space
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
- **metadata_remotes** — remotes chosen to hold metadata backups.
- **settings** — folder settings, such as the minimum number of copies.

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
case no browser opens.

### Encryption and layout

A master password must be set (`mbr config password`, or the Settings
tab) before any remote can be added: every remote is encrypted with it.
A remote named `drive` becomes two rclone sections, which are never shown:

```
[_plain-mbr-drive]       the backend itself
[_autocrypt-mbr-drive]   type = crypt, password = the master password
                         remote = _plain-mbr-drive:<bucket/>mbr-<folder id>
```

and the mbr remote `drive` targets `_autocrypt-mbr-drive:`, so blobs,
their names and the metadata backups are all encrypted. `mbr remote setup
--unencrypted` (or "Don't encrypt" in the GUI) instead makes the single
section `_dangerouslyplain-mbr-drive`, storing in the clear. Wrapper
backends (alias, chunker, crypt, union, …) are not offered.

The folder id is 6 random bytes in hex, made once and kept in the
database, so each folder stores in its own `mbr-<id>` directory and several
folders can share one storage account. The only location asked is the
bucket, container or share (or, for `local`, the folder).

Since remotes are encrypted with it, the master password can't change
once one exists. It is saved, with a check, in the per-machine
`state.json` (mode 0600), so it is asked for once.

### Scanning remotes

Scanning a remote lists everything stored there and makes the database
match: blobs recorded there but gone lose their record, and every blob
found is recorded as stored there. Blobs the database has never heard of
(pushed from another folder, say) are added too, and given a name in the
`unnamed/` folder: `unnamed/<hash>`, a symlink like any other tracked
file, dangling until the blob is downloaded. Rename or move it like any
other file.

## Keeping files safe

Each file has *copies*: the local blob, if present, plus every remote that
stores it. The folder keeps a minimum number of copies (default 2; `mbr
config min-copies N`, or the Remotes tab); files below it are flagged as
at risk.

- **Freeing space** (`mbr drop`, "Free up space"): removes local copies,
  but only of blobs that at least *min-copies* remotes are confirmed to
  hold right now. Each remote is asked directly rather than trusting the
  records, and its size must match. The file stays listed (dangling) and
  can be downloaded again.
- **Verifying** (`mbr fsck`, "Verify"): re-hashes local blobs; one that no
  longer matches is moved to `.mbr/corrupt/` so it counts as missing and
  can be downloaded again. `--remote` (or "Verify on remote") downloads a
  remote's copies to check them; a corrupt copy is deleted there, so
  pushing again replaces it.
- **Old versions and deleted files** (`mbr gc`, "Clean up old versions"):
  blobs no current file points at. `gc` drops their local copies under the
  same minimum-copies rule; `gc --purge [--older-than DAYS]` deletes them
  everywhere, remotes included, after confirmation.

## Metadata backups

Remotes store blobs by hash only: the names, folders and history of the
files live in `.mbr/mbr.db`. If the folder is lost, its remotes alone would
come back as a heap of nameless files. So the database is backed up
separately. It is not content-addressed like a blob, but encrypted with
the folder's **master password** and uploaded to every remote chosen for
it, replacing the previous backup:

```
<target>/mbr-metadata/mbr.db.age        the database
<target>/mbr-metadata/rclone.conf.age   the rclone config, so every remote
                                        can be reached from a new computer
```

These are [age](https://age-encryption.org) files encrypted to the password
(scrypt), so they can also be opened without mbr: `age -d mbr.db.age >
mbr.db`. The password can't be recovered: without it the backups are
unreadable.

To restore, attach a new, empty folder, set the same master password, set
up one of the old remotes there, and restore from it (`mbr metadata
restore <remote>`, or the Settings tab). The new folder has a new id, so
its remote points next to the old folder's `mbr-<id>` directory: restoring
looks in the sibling `mbr-<id>` directories too, and the new folder then
takes on the old id, moving its remotes over. Every file reappears as a
dangling symlink, the other remotes come back with their settings, and the
content can then be downloaded.

Restoring into a folder already in use merges the backup in: whatever only
the backup knows (files, older versions, remotes and what they store,
settings) is added, and where the two disagree the folder wins. The database
as it was before is kept in `.mbr/mbr.db.before-restore`.

### Local state

When each remote last received a backup is per-machine state, never synced:
`<config dir>/mbr/state.json` (e.g. `~/.config/mbr/state.json`), loaded on
every start. It also holds the last opened folder and, per folder, an
Argon2 hash of the master password, so a mistyped password is caught
before anything is encrypted with it. A remote whose last backup predates
a database change shows as out of date.

## CLI

```
mbr init [dir]                    attach mbr to a folder
mbr scan                          ingest new files, record removals
mbr ls                            list tracked files with metadata
mbr info <path|hash>              full detail for one file or blob
mbr remote types                  list the rclone backends mbr can set up
mbr remote setup <name> <type> [--unencrypted]
                                  set up an (encrypted) remote interactively
mbr remote ls                     list remotes
mbr remote show <name>            a remote's settings (secrets hidden)
mbr remote edit <name> [--rename NEW] [--set KEY=VALUE...]
                                  rename, or change its backend settings
mbr remote scan [name...]         list remotes fully, record what they hold
mbr remote rm <name>              remove a remote and its rclone settings
mbr push <remote> [path|hash...]  upload blobs (default: all not yet there)
mbr pull <remote> [path|hash...]  download from one remote (default: all it
                                  holds that is missing here)
mbr fetch <path|hash>...          download blobs from any remote holding them
mbr check <remote> [path|hash...] ask a remote about specific blobs
                                  (without any: same as `remote scan`)
mbr fsck [--remote R] [path|hash...]
                                  verify blobs against their hashes
mbr drop (--all | path|hash...)   remove local copies enough remotes hold
mbr gc [--purge [--older-than DAYS] [--yes]]
                                  drop (or delete everywhere) old versions
                                  and deleted files
mbr config min-copies [N]         show or set the copies to keep
mbr config password               set the master password (before any remote)
mbr metadata status               where metadata is backed up, and when
mbr metadata add|rm <remote>...   choose the remotes for metadata backups
mbr metadata backup               encrypt and upload the metadata
mbr metadata restore <remote>     merge the backup on a remote into this folder
```

Every command except `init` works from anywhere inside an attached folder.
Files and blobs are addressable by path, full hash, or a unique hash
prefix (≥ 6 hex chars).

## GUI

`my-beloved-rubble` has three tabs.

**Blobs** shows the annotated file listing (green dot = present locally).
Select files with a click, Ctrl/⌘+click (toggle), Shift+click (range),
by dragging across rows, or Ctrl+A (all; Esc clears). The sidebar shows
the details of a single selected file, or a summary of several, and acts
on the selected blobs: upload them to a remote, check whether a remote
holds them, download the ones missing locally, free up space, or verify
them locally or on a remote. Files with fewer copies than the minimum are
shown in red. With nothing selected, the sidebar can verify every local
copy and clean up old versions.

**Remotes** sets the minimum number of copies, and lists the remotes with
Scan (and Scan all), Download all
(everything recorded there that is missing here), Upload all, Edit and
Remove. Once a master password is set, the sidebar sets up remotes: pick
a backend and fill in the generated form (choices as dropdowns, required
fields marked, each field's "?" opening its rclone documentation), with
"Don't encrypt" unticked by default; if rclone still needs an answer —
usually OAuth — the question is shown inline. Edit renames a remote and,
for backends mbr has a form for, changes its settings (secrets stay
hidden; blank keeps them).

**Settings** holds the master password (set once, then saved on this
computer) and chooses which remotes receive metadata backups, showing
when each was last backed up and whether that is up to date. The sidebar
sets, unlocks or changes the password and backs up. It also restores a backup from a remote: first
thing in an empty folder, merged in otherwise.

Scans and rclone operations run in the background, backed by the
repository-local `.mbr/rclone.conf`. The last opened folder is reopened
on start.

## Building

```
cargo build --release   # target/release/mbr and target/release/my-beloved-rubble
```

Rclone is embedded through `librclone`; building requires a Go toolchain and C compiler, but no separate `rclone` executable is required at runtime.
