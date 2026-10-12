+++
title = "Getting started"
weight = 1
[extra]
nav = "getting started"
+++

## Attaching a folder

Click **Open Folder…** and pick the folder you want to keep safe, or an empty one to start from. The app creates a
hidden `.mbr` folder inside it, where it keeps everything it needs, and reopens the same folder the next time it
starts.

## Adding files

Put files into the folder however you like: copy them in, save them from another program, move whole folders of
photos. Then click **Rescan**. Every new file is stored in `.mbr/blobs/`, named after its content, and replaced by a
link to it, so it still opens like a normal file.

From then on the app knows each file's size, when it was added and where copies of it are kept. Renaming, moving and
deleting files works as usual; the next scan records what changed.

## What changes in your folder

Files you add become read-only. A program that tries to change a file in place gets an error, so a stored file can
never silently change under you. Saving a new file over the old name still works: the app records it as a new version
and keeps the previous one, which you can find in the file's details.

Nothing leaves your computer until you add a [remote](../remotes/) and upload to it.
