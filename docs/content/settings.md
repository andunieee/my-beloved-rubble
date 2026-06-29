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

The password cannot be changed once set. Use **Test** to check that you remember it. The folder's metadata keeps a
*check* of the password (a slow Argon2 hash, not the password), so every device [syncing the folder](../peers/) can tell
whether the password typed there is the right one.

The password is saved on this computer, in the folder's `.mbr/local.json`, readable only by you. That file is not part
of the database, is never synced or backed up, and goes away with the folder. The password is written there in base64,
so it doesn't show at a glance; that is an encoding, not encryption: anyone who can read the folder's `.mbr` directory
can recover it (`.mbr/rclone.conf` also holds it, obscured the way rclone stores secrets).

On a device that [joined the folder](../peers/), the password was set elsewhere: type it once in Settings (**Enter the
master password**, or `mbr config password`), and it is saved there too.

When restoring an encrypted backup, its password is saved automatically.
