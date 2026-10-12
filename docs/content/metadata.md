+++
title = "Metadata backup"
weight = 5
[extra]
nav = "metadata backup"
+++

Remotes store your files by their content only. The names, folders and history of your files live in the folder's
database, `.mbr/mbr.db`. If the folder is lost along with the computer, its remotes would come back as a heap of
nameless files. So the database is backed up separately, encrypted with the [master password](../settings/), to the
remotes you choose in the **Settings** tab.

Each remote shows when it was last backed up, and whether that backup is out of date because files changed since.

## Backing up

Tick the remotes that should hold a backup, then click **Back up now**. The app packs the database together with this
folder's rclone settings (so the remotes can be reached again from a new computer), encrypts them, and uploads the
result to each ticked remote, replacing the previous backup there.

The backup sits at the top of the remote, in the bucket or folder you named when setting it up, next to the folder's
own `mbr-…` directory. It is called `mbr-metadata-` followed by a code made from the master password (for example
`mbr-metadata-9c1e0b7a42d3f581.tar.gz.encrypted`), so it can be found again knowing only the password and where the
remote is.

Back up again after adding or changing files: the tab shows which backups are out of date.

The backup is an ordinary [age](https://age-encryption.org) file encrypted with the password, holding a tar.gz, so it
can also be opened without the app: `age -d mbr-metadata-….tar.gz.encrypted | tar xz` gives back `mbr.db` and
`rclone.conf`.

## Restoring a lost folder

Open a new folder in the app (an empty one, or one holding what you could save). Since it isn't an mbr folder yet, the
app asks whether to start a new one there or to restore a lost one, and restores from either:

- **a remote**: enter the master password and fill in the remote's settings as when it was set up: the same kind of
  storage, account and bucket or folder. The app looks for the backup made with that password there.
- **a backup file**: a `mbr-metadata-….tar.gz.encrypted` file you downloaded, and the master password.
- **database files**: a copy of the old folder's `.mbr/mbr.db` and `.mbr/rclone.conf`, as they are. Afterwards, set
  the old folder's master password in the **Settings** tab.

Every file reappears with its name, history and records of where it is stored, and the remotes come back with their
settings. The files' content is still on the remotes: download it from the **Remotes** tab with **Download all**, or
only the files you need. A file already in the folder under a restored name is kept, and the next scan records it as a
new version.

A backup is only ever restored into a folder that isn't an mbr folder yet, never merged into one in use.

## Changing the password

Unlock with the current password, then enter the new one twice. The backup's name comes from the password, so the
next backup is stored under a new name; the old one, still under the old password, stays until you delete it.
