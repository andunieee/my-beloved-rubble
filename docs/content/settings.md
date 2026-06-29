+++
title = "Settings"
weight = 4
[extra]
nav = "settings"
+++

The **Settings** tab chooses which remotes receive [metadata backups](../metadata/) and shows their backup status.

## The master password

Choose a master password before adding any remote, in the **Settings** tab. A restored folder gets the password from
its backup automatically. Everything sent to a remote is encrypted with it: your files, their names, and the
metadata backups. Whoever runs a remote sees only scrambled names and content.

**Write it down somewhere safe.** Nobody can recover it: without it, nothing stored on your remotes can be read.

The password is saved in the folder's `.mbr/local.json`, readable only by you. That file is not part of the database,
is never synced or backed up, and goes away with the folder. The password cannot be changed once set. Settings reports
**master password is locked**. Use **Test** to check that you remember it.

When restoring an encrypted backup, its password is saved automatically.
