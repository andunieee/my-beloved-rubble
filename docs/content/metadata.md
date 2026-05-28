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

Tick the remotes that should hold a backup, then click **Back up now**. The app encrypts the database and this
folder's rclone settings (so the remotes can be reached again from a new computer) and uploads them to each ticked
remote, into an `mbr-metadata` folder, replacing the previous backup there.

Back up again after adding or changing files: the tab shows which backups are out of date.

The backups are ordinary [age](https://age-encryption.org) files encrypted with the password, so they can also be
opened without the app: `age -d mbr.db.age > mbr.db`.

## Restoring a lost folder

1. Open a new, empty folder in the app.
2. In the **Settings** tab, set the same master password as the old folder.
3. In the **Remotes** tab, set up one of the remotes the old folder backed up to, with the same kind of storage and
   the same bucket or folder.
4. In the **Settings** tab, pick that remote under **Restore from a backup** and click **Restore**.

The new folder finds the old one's directory on the remote and takes it over, so the remotes set up here reach what
the old folder stored.

Every file reappears with its name, history and records of where it is stored; the other remotes come back too, with
their settings. The files' content is still on the remotes: download it from the **Remotes** tab with **Download
all**, or only the files you need.

## Merging in a backup

A folder already in use can take in a backup too, for instance after its database was damaged and rebuilt by a scan,
or to bring back the history of its files. Under **Merge in a backup**, pick the remote and click **Restore**; once
password field can stay empty if the backup uses the folder's master password.

Merging only ever adds what the backup knows and the folder doesn't:

- files the folder lost track of reappear, ready to download if their content isn't here;
- older versions and deleted files join the history;
- remotes, records of what each remote stores, and settings come back.

Where the two disagree, the folder wins. A file the folder knows changed or was deleted since the backup stays as it is, and
the backup's version only joins its history. A remote with the same name but a different target keeps this folder's
target.

The database as it was before is kept in `.mbr/mbr.db.before-restore`: to undo a merge, put it back as `.mbr/mbr.db`
while the app is closed.

## Changing the password

Unlock with the current password, then enter the new one twice. Backups made before keep the old password until they
are replaced, so back up again right after.
