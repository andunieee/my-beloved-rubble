+++
title = "Settings"
weight = 4
[extra]
nav = "settings"
+++

The **Settings** tab holds the folder's master password and where its [metadata backups](../metadata/) go.

## The master password

Set a master password before adding any remote. Everything sent to a remote is encrypted with it: your files, their
names, and the metadata backups. Whoever runs a remote sees only scrambled names and content.

**Write it down somewhere safe.** Nobody can recover it: without it, nothing stored on your remotes can be read.

The password is saved on this computer (in the app's settings, readable only by you), so it is asked for only once.
A folder whose password was set on another computer, or before passwords were saved, asks for it once under
**Unlock**.

## Changing the password

The password can be changed only while no remote is encrypted with it, since those remotes couldn't read what they
already store. Metadata backups made before a change keep the old password, and a restore finds them only with it, so
back up again right after.
