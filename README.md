# my beloved rubble

Content-addressed folders in the spirit of git-annex, with rclone remotes.

`mbr` comes with a GUI written in [Slint](https://slint.dev) and a CLI. It is licensed under [AGPL](LICENSE).

<p align="center">
  <img src="docs/static/screenshots/blobs.png" alt="The Blobs tab: files, the remotes holding them, and old versions">
</p>

<p align="center">
  <img src="docs/static/screenshots/remotes.png" alt="The Remotes tab, with three remotes and the form for a new one">
</p>

## Installation

Grab the files from the [latest release](https://github.com/andunieee/my-beloved-rubble/releases/latest), or use the commands below, which all download the latest one. Every desktop release has both `mbr` (the CLI) and `my-beloved-rubble` (the GUI).

<details>
<summary>Linux</summary>

x86_64 and arm64, glibc 2.35+ (Ubuntu 22.04, Debian 12, Fedora 36 or newer). The GUI also needs fontconfig and freetype, and X11 or Wayland, which any desktop already has.

**One-liner**, extracts both binaries into `~/.local/bin`:
```
mkdir -p ~/.local/bin && curl -fL "$(curl -s https://api.github.com/repos/andunieee/my-beloved-rubble/releases/latest | grep -o "https://[^\"]*$(uname -m)-unknown-linux-gnu\.tar\.gz")" | tar -xz -C ~/.local/bin --strip-components=1 --wildcards '*/mbr' '*/my-beloved-rubble'
```

**Manually**: download the `x86_64-unknown-linux-gnu` (or `aarch64-unknown-linux-gnu`) `.tar.gz`, extract it and put `mbr` and `my-beloved-rubble` somewhere in your `PATH`.

**With cargo**, builds from source into `~/.cargo/bin` (see [Building from source](#building-from-source) for the dependencies):
```
cargo install --locked --git https://github.com/andunieee/my-beloved-rubble mybelovedrubble
```

**From a clone**, same as cargo, but also adds the GUI to the application menu:
```
git clone https://github.com/andunieee/my-beloved-rubble && cd my-beloved-rubble && just desktop-install
```
</details>

<details>
<summary>Windows</summary>

x86_64, Windows 10 or newer. Before using mbr, enable **Developer Mode** (Settings → System → For developers): tracked files are stored as symlinks, which normal users can only create with that setting.

**Manually**: download the `x86_64-windows-setup` `.exe` from the [releases page](https://github.com/andunieee/my-beloved-rubble/releases) and run it. It installs for your user (no administrator needed) and adds a Start menu entry and an uninstaller. `mbr.exe` sits next to the GUI, in `%LOCALAPPDATA%\Programs\My Beloved Rubble`.

**One-liner** (PowerShell), does the same: downloads the installer and runs it.
```
$u=(irm https://api.github.com/repos/andunieee/my-beloved-rubble/releases/latest).assets.browser_download_url -match 'x86_64-windows-setup\.exe$'; iwr $u -OutFile $env:TEMP\mbr-setup.exe; Start-Process $env:TEMP\mbr-setup.exe
```

**Silently** (PowerShell), the same without the installer's windows, for scripts:
```
$u=(irm https://api.github.com/repos/andunieee/my-beloved-rubble/releases/latest).assets.browser_download_url -match 'x86_64-windows-setup\.exe$'; iwr $u -OutFile $env:TEMP\mbr-setup.exe; Start-Process -Wait $env:TEMP\mbr-setup.exe '/VERYSILENT /SUPPRESSMSGBOXES /NORESTART'
```
</details>

<details>
<summary>macOS</summary>

Apple Silicon and Intel, macOS 11 or newer. The app is ad-hoc signed but not notarized.

**One-liner** (Terminal), puts `My Beloved Rubble.app` in `/Applications` and `mbr` in `/usr/local/bin` (asks for your password for the latter):
```
a=$([ "$(uname -m)" = arm64 ] && echo aarch64 || echo x86_64); d=$(mktemp -d) && curl -fL "$(curl -s https://api.github.com/repos/andunieee/my-beloved-rubble/releases/latest | grep -o "https://[^\"]*$a-apple-darwin\.tar\.gz")" | tar -xz -C "$d" --strip-components=1 && rm -rf "/Applications/My Beloved Rubble.app" && mv "$d/My Beloved Rubble.app" /Applications/ && sudo mkdir -p /usr/local/bin && sudo install -m755 "$d/mbr" /usr/local/bin/mbr
```

**Manually**: download the `aarch64-apple-darwin` (Apple Silicon) or `x86_64-apple-darwin` (Intel) `.tar.gz`, double-click it in Finder to extract it, and drag `My Beloved Rubble.app` to `Applications`. On first launch right-click it and choose Open. If macOS still refuses to open it, run:
```
xattr -dr com.apple.quarantine "/Applications/My Beloved Rubble.app"
```

**With cargo**, builds from source into `~/.cargo/bin` (see [Building from source](#building-from-source)):
```
cargo install --locked --git https://github.com/andunieee/my-beloved-rubble mybelovedrubble
```
</details>

<details>
<summary>Android</summary>

arm64 phones and tablets, Android 8.0 or newer. The APK isn't on any store, so Android asks you to allow installing apps from the browser (or file manager) you open it with.

**On the device**: open the [latest release](https://github.com/andunieee/my-beloved-rubble/releases/latest) in the browser, download the `android-arm64` `.apk` and open it.

**With [Obtainium](https://obtainium.imranr.dev)**, which installs it from the GitHub releases and keeps it updated: add an app with the URL `https://github.com/andunieee/my-beloved-rubble`.

**From a computer**, with adb: downloads the APK and installs it on every device connected through adb (with USB debugging on):
```
curl -fL "$(curl -s https://api.github.com/repos/andunieee/my-beloved-rubble/releases/latest | grep -o 'https://[^"]*android-arm64\.apk')" -o /tmp/mbr.apk && adb devices | awk 'NR > 1 && $2 == "device" { print $1 }' | xargs -I{} adb -s {} install -r /tmp/mbr.apk
```

**From source**, builds the APK and installs it on every device connected through adb (see [Building from source](#building-from-source)):
```
git clone https://github.com/andunieee/my-beloved-rubble && cd my-beloved-rubble && just android-install
```
An APK built this way is signed with your own key, so it can't replace an install of the release APK: uninstall that first.
</details>

## The model

Attach mbr to any folder (`mbr init`, or "Open Folder" in the GUI). From then on, whenever the folder is inspected (a scan), every regular file in it is moved into a hidden content-addressed store and replaced by a symlink, so it still opens like a normal file. Stored blobs are read-only: a program that writes into a file in place gets an error instead of silently changing content stored under its old hash (saving a new file over the name works, and is recorded as a new version):

```
folder/
  .mbr/
    blobs/fa/2e/fa2e38…(full sha256)   the actual content
    db/                               metadata (an iroh-docs replica)
    rclone.conf                       repository-local rclone config
    local.json                        per-machine state, never synced
                                      (password in base64, peers, device key)
    corrupt/                          blobs that failed verification
    tmp/                              scratch space
  fruits.png -> .mbr/blobs/fa/2e/fa2e38…
```

`.mbr/db` tracks:

- **paths** — every name the folder ever held: which blob it pointed to, when it was added, when it was removed. Replacing a file's content closes the old row and opens a new one, so previous versions stay addressable as long as their blobs are stored somewhere.
- **blobs** — hash → size.
- **remotes** — named rclone targets (e.g. `backup` → `s3:bucket/mbr`).
- **blob_remotes** — which blobs are known to live on which remotes.
- **metadata_remotes** — remotes chosen to hold metadata backups.
- **settings** — folder settings, such as the minimum number of copies, and the master password's check.

For each file the UIs show: size, hash, whether the content is present locally (the filesystem is the source of truth for that), which remotes store it, other names — current or past — pointing at the same blob, and previous versions of the name with where those old blobs still live.

## Remotes

Blobs are pushed to rclone remotes named purely by hash, in the same sharded `fa/2e/fa2e38…` layout, with no metadata. Every remote is automatically wrapped in an rclone `crypt` remote keyed by the folder's master password. Each attached folder owns its rclone configuration at `.mbr/rclone.conf`.

### Setting up a remote

mbr hardcodes a short setup form for most rclone backends (local folders, SFTP, SMB, WebDAV, R2, S3, B2, Azure, GCS, Drive, Dropbox, OneDrive, pCloud, and many more; see `mbr remote types`) — derived from rclone's documentation and cross-checked against the embedded rclone's own metadata at test time (`cargo test`), so a renamed option upstream fails the build instead of producing a form rclone silently ignores.

In the GUI, pick a backend and fill in the generated form; in the CLI, `mbr remote types` lists the backends and their fields, and `mbr remote setup <name> <type>` asks for them. Answers are handed to rclone in one shot (`config/create` with prefilled parameters); anything rclone still needs afterwards — typically a browser sign-in for OAuth backends — comes back as a single question the frontends relay.

For OAuth backends (Google Drive, Dropbox, OneDrive, …) the form is empty, or nearly (a GCS project number, a Zoho region): the sign-in happens in the browser, and the auth link is also printed (CLI) or shown inline (GUI) in case no browser opens. The GUI answers rclone's "use a web browser?" question yes by itself, since it runs on the user's desktop; the CLI asks it, as it may run on a machine without one. Fields rclone says to leave blank (OAuth client ids, service accounts, …) are left off the forms too; any rclone option, on the form or not, can be given with `mbr remote setup <name> <type> --set KEY=VALUE` (repeatable; form fields given this way aren't asked) or changed later with `mbr remote edit <name> --set KEY=VALUE`.

### Encryption and layout

A master password must be set (`mbr config password`, or the Settings tab) before any remote can be added: every remote is encrypted with it. A remote named `drive` becomes three rclone sections, which are never shown:

```
[_root-mbr-drive]    the backend itself, at its root
[_plain-mbr-drive]   type = alias
                     remote = _root-mbr-drive:<bucket/>mbr-<folder id>
[_crypt-mbr-drive]   type = crypt, password = the master password
                     remote = _plain-mbr-drive:
```

and the mbr remote `drive` targets `_crypt-mbr-drive:`, so blobs and their names are encrypted.

The folder id is 6 random bytes in hex, made once and kept in the database, so each folder stores in its own `mbr-<id>` directory and several folders can share one storage account. Metadata backups go on `_root-mbr-drive`, next to `mbr-<id>`.

The master password is set once, before the first remote is added, and can't change. Its check (Argon2) is part of the metadata, so every device syncing the folder can verify the password typed there. The password itself is saved in the folder's `.mbr/local.json` (mode 0600), which is never synced or backed up, in base64 so it doesn't show at a glance (an encoding, not encryption; `.mbr/rclone.conf` also holds it obscured, rclone's reversible encoding).

### Scanning remotes

Scanning a remote lists everything stored there and makes the database match: blobs recorded there but gone lose their record, and every blob found is recorded as stored there. Blobs the database has never heard of (pushed from another folder, say) are added too, and given a name in the `unnamed/` folder: `unnamed/<hash>`, a symlink like any other tracked file, dangling until the blob is downloaded. Rename or move it like any other file.

## Keeping files safe

Each file has *copies*: the local blob, if present, plus every remote that stores it. The folder keeps a minimum number of copies (default 2; `mbr config min-copies N`, or the Remotes tab); files below it are flagged as at risk.

- **Freeing space** (`mbr drop`, "Free up space"): removes local copies, but only of blobs that at least *min-copies* remotes are confirmed to hold right now. Each remote is asked directly rather than trusting the records, and its size must match. The file stays listed (dangling) and can be downloaded again.
- **Verifying** (`mbr fsck`, "Verify"): re-hashes local blobs; one that no longer matches is moved to `.mbr/corrupt/` so it counts as missing and can be downloaded again. `--remote` (or "Verify on remote") downloads a remote's copies to check them; a corrupt copy is deleted there, so pushing again replaces it.
- **Old versions and deleted files** (`mbr gc`, "Clean up old versions"): blobs no current file points at. `gc` drops their local copies under the same minimum-copies rule; `gc --purge [--older-than DAYS]` deletes them everywhere, remotes included, after confirmation.

## Metadata backups

Remotes store blobs by hash only: the names, folders and history of the files live in `.mbr/db`. If the folder is lost, its remotes alone would come back as a heap of nameless files. So the database is backed up separately. It is not content-addressed like a blob, but encrypted with the folder's **master password** and uploaded to every remote chosen for it, replacing the previous backup:

```
<bucket or folder>/mbr-metadata-<backup id>.tar.gz.encrypted
```

holding `mbr-metadata.json`, the whole database including the remote settings, in a tar.gz. The backup id is 8 bytes of an Argon2id hash of the master password, in hex: the backup is found again knowing only the password and where the remote is, and the name makes the password no easier to guess than the content does. It is an [age](https://age-encryption.org) file encrypted to the password (scrypt), so it can also be opened without mbr: `age -d mbr-metadata-…. tar.gz.encrypted | tar xz`. The password can't be recovered: without it the backups are unreadable. Folders sharing a remote and a master password share the backup name too, so give them different passwords.

A backup is restored into a folder that isn't an mbr folder yet, instead of `mbr init` (`mbr restore`, or the choice the GUI offers when opening such a folder), from any of:

- a remote: the master password and the remote's settings, set up in a throwaway rclone config, then the backup made with that password is downloaded from where they point;
- an encrypted backup file and the master password.

The backup's database and rclone config become the folder's, so its remotes, folder id and all come back as they were. Every file reappears as a dangling symlink (a file already in the way is kept, and the next scan records it as a new version), and the content can then be downloaded. There is no merging into a folder in use.

## Syncing between devices

A folder can live on several devices that sync its metadata over [iroh](https://iroh.computer) (the Peers tab, or `mbr peer` and `mbr sync`). Each device has an id per folder (an iroh endpoint key kept in `local.json`); two devices sync once each has added the other's id. A new device starts an empty folder, which *joins* the other's: it is handed the iroh-docs namespace secret and adopts it. Two folders that both hold files never merge.

A sync is iroh-docs set reconciliation of the entries, then an exchange of the few values entries point at (settings, remotes); the newest entry of each key wins, whichever device wrote it. Synced changes become links in the folder (dangling until downloaded from a remote), except for files changed locally since the last scan, which the next scan records as the newest change. Blob content never travels between devices, and neither does the master password: each device types it, checked against the check in the metadata.

## Future plans

The goal is to have a bunch more options to make it easier to deal with large numbers of files and automate their backups, checking and custom rules.

Android support would build on syncing: phones would sync the metadata, then type the password and gain access to all the files, downloading them directly from the remotes, and add new files by reusing the rclone configurations of the other machines to upload them.

`mbr` is designed for power-users, but eventually a "simple mode" app can be developed that allows a non-tech family member, for example, to just share a bunch of pictures and they get all uploaded and synced.

## Releases

Pushing a `v*` tag (`just release` tags the version in `Cargo.toml`) builds and publishes a GitHub release with `.tar.gz` archives for Linux and macOS (x86_64 and arm64), a Windows x86_64 installer, and an Android arm64 APK. Each archive holds both binaries, `mbr` and `my-beloved-rubble` (and a `My Beloved Rubble.app` bundle on macOS); the release also carries a `SHA256SUMS` file. Running the `release` workflow by hand builds the same artifacts without publishing a release.

The Android APK is signed with a throwaway key unless the repository has `ANDROID_KEYSTORE_BASE64` (a base64-encoded keystore) and `ANDROID_KEYSTORE_PASSWORD` secrets, in which case that key is used so new APKs can upgrade earlier installs.

## Building from source

### Dependencies

* Rust (stable)
* Go: rclone is embedded through `librclone`, which builds it with `go build`; no separate `rclone` executable is needed at runtime
* A C compiler and libclang (for bindgen)
* On Linux, `pkg-config` and the fontconfig, freetype, X11, xkbcommon, OpenGL and Wayland development packages, e.g. on Debian/Ubuntu:
  ```
  sudo apt install pkg-config clang libclang-dev libfontconfig1-dev libfreetype6-dev libx11-dev libxkbcommon-dev libgl1-mesa-dev libwayland-dev
  ```
* On Windows, the `x86_64-pc-windows-gnu` Rust target and MinGW's GCC, since Go's C archive is MinGW-flavoured (see `.github/workflows/release.yml`)
* For Android, the Android SDK and NDK (`ANDROID_HOME`, `ANDROID_NDK_ROOT`), a JDK, the `aarch64-linux-android` Rust target and `cargo install cargo-apk`
* [just](https://just.systems), for the recipes below (optional, they are short)

### Running

```
just gui           # the GUI
just cli --help    # the CLI
cargo build --release   # target/release/mbr and target/release/my-beloved-rubble
```

### Installing

```
just install          # both of the below
just desktop-install  # mbr and my-beloved-rubble into ~/.cargo/bin, and the application menu on Linux
just android-install  # builds the APK and installs it on every device connected through adb
```

`just android-install` signs the APK with the keystore in `CARGO_APK_RELEASE_KEYSTORE` (password in `CARGO_APK_RELEASE_KEYSTORE_PASSWORD`) if set, or else with `~/.android/debug.keystore`, which it creates if missing.

### Screenshots

The screenshots in `docs/static/screenshots` are rendered from the real UI over made-up data:

```
just screenshots
```
