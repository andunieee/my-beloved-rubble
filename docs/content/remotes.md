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
which has its own [backup](../metadata/).

Every remote is encrypted with the folder's [master password](../settings/), so one must be set before adding a
remote. The app wraps the storage in an rclone [crypt](https://rclone.org/crypt/) remote: whoever runs it sees only
scrambled file names and content.

Each folder stores in its own directory on a remote, `mbr-` followed by a random code (for example
`mbr-3f9a0c1b2e4d`), so several folders can share one storage account. [Metadata backups](../metadata/) go next to
that directory.

## Setting up a new remote

Give the remote a name, pick the kind of storage, and fill in the form. Fields marked **required** must be filled;
the others can usually be left alone. The **?** next to each field opens rclone's documentation for it. Storage that
holds buckets or shares asks which one to use; a local disk asks for the folder.

**Don't encrypt** stores files as they are, readable by anyone with access to the remote. It is meant for disks you
keep yourself, and is off unless you tick it.

Services that sign in through the browser (Google Drive, Dropbox, OneDrive, …) open a browser window once you save.
If one doesn't open, the link is shown in the app. If rclone needs anything else, it asks in a box below the form.

Each folder keeps its own rclone settings in `.mbr/rclone.conf`, separate from any other rclone configuration on the
computer. The remotes there have names starting with `_root-mbr-` (the storage itself), `_plain-mbr-` (the folder's
directory in it), `_crypt-mbr-` (the encryption) or `_dangerouslyplain-mbr-`; the app only ever shows the name you
chose.

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

**Edit** changes a remote's name and, for the kinds of storage the app has a form for, its settings (an expired
key, a new server address, …). Passwords and keys already set are not shown; leave those fields blank to keep them.

**Remove** forgets a remote, its records and its settings. Nothing stored on it is deleted.
