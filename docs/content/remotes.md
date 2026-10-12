+++
title = "Remotes"
weight = 3
[extra]
nav = "remotes"
+++

A remote is a place your files are copied to: a folder on an external disk, a server over SFTP or SMB, a cloud storage
account. The app talks to all of them through [rclone](https://rclone.org), which is built in, so nothing else needs
to be installed.

Files are stored on a remote under their content hash only, without names. The names live in the folder's database,
which has its own [backup](../metadata/). To keep what is stored on a remote private from whoever runs it, point the app
at an rclone [crypt](https://rclone.org/crypt/) remote.

## Adding an existing target

If you already know the rclone location, give it a name and type it in, for example `/media/disk/rubble` for a folder on
an external disk, or `backup:bucket/rubble` for a remote already configured with the `rclone` program.

## Setting up a new remote

Give the remote a name, pick the kind of storage, and fill in the form. Fields marked **required** must be filled;
the others can usually be left alone. The **?** next to each field opens rclone's documentation for it.

Services that sign in through the browser (Google Drive, Dropbox, OneDrive, …) open a browser window once you save.
If one doesn't open, the link is shown in the app. If rclone needs anything else, it asks in a box below the form.

Each folder keeps its own rclone settings in `.mbr/rclone.conf`, separate from any other rclone configuration on the
computer.

## How many copies

**Keep N copies of every file** sets how many copies each file should have, counting this computer and every remote.
The default is 2. Files with fewer are shown in red, and **Free up space** won't remove this computer's copy unless that
many remotes hold the file.

Three copies, one of them in another building, is a good goal for things you can't replace.

## Scanning

**Scan** lists everything stored on a remote and updates the records to match: files recorded there but gone lose
their record, and every file found there is recorded. Files the folder has never seen, for example uploaded from
another folder, show up under `unnamed/`, where you can rename or move them like any other file. **Scan all** does
this for every remote.

## Editing a remote

**Edit** changes a remote's name, its target, and, for the kinds of storage the app has a form for, its rclone
settings. Changing the target doesn't move anything: scan the remote afterwards so the records match what is at the
new location. Passwords and keys already set are not shown; leave those fields blank to keep them.

**Remove** forgets a remote and its records. Nothing stored on it is deleted.
