+++
title = "Settings"
weight = 4
[extra]
nav = "settings"
+++

The **Settings** tab chooses which remotes receive [metadata backups](../metadata/) and shows their backup status.

## The master password

Choose master password when creating a folder or restoring from unencrypted database files, before adding any remote.
Everything sent to a remote is encrypted with it: your files, their names, and the metadata backups. Whoever runs a
remote sees only scrambled names and content.

**Write it down somewhere safe.** Nobody can recover it: without it, nothing stored on your remotes can be read.

The password is saved on this computer (in the app's settings, readable only by you). It cannot be changed once set.
Settings reports **master password is locked**. Use **Test** to check that you remember it.

When restoring an encrypted backup, its password is saved automatically.
