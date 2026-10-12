+++
title = "Metadata backup"
weight = 4
[extra]
nav = "metadata backup"
+++

Remotes store your files by their content only. The names, folders and history of your files live in the folder's
database, `.mbr/mbr.db`. If the folder is lost along with the computer, its remotes would come back as a heap of
nameless files. So the database is backed up separately, encrypted with a master password, to the remotes you choose
in the **Metadata backup** tab.

Each remote shows when it was last backed up, and whether that backup is out of date because files changed since.

## The master password

Before the first backup, choose a master password. The database is encrypted with it before it leaves the computer,
so whoever runs a remote can't read the names of your files.

**Write it down somewhere safe.** Nobody can recover it: without it, the backups can't be read.

## Unlocking

To back up, enter the master password once. The app keeps it in memory until you click **Lock**, open another folder
or quit, and never writes it to disk; it only stores a check to catch typos.

## Backing up

Tick the remotes that should hold a backup, then click **Back up now**. The app encrypts the database and this
folder's rclone settings (so the remotes can be reached again from a new computer) and uploads them to each ticked
remote, into an `mbr-metadata` folder, replacing the previous backup there.

Back up again after adding or changing files: the tab shows which backups are out of date.

The backups are ordinary [age](https://age-encryption.org) files encrypted with the password, so they can also be
opened without the app: `age -d mbr.db.age > mbr.db`.

## Restoring a lost folder

1. Open a new, empty folder in the app.
2. In the **Remotes** tab, set up one of the remotes the old folder backed up to.
3. In the **Metadata backup** tab, pick that remote under **Restore from a backup**, enter the master password and
   click **Restore**.

Every file reappears with its name, history and records of where it is stored; the other remotes come back too, with
their settings. The files' content is still on the remotes: download it from the **Remotes** tab with **Download
all**, or only the files you need.

## Merging in a backup

A folder already in use can take in a backup too, for instance after its database was damaged and rebuilt by a scan,
or to bring back the history of its files. Under **Merge in a backup**, pick the remote and click **Restore**; once
unlocked, the password field can stay empty if the backup uses the current password.

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
