+++
title = "Files"
weight = 2
[extra]
nav = "files"
+++

The **Blobs** tab lists every file in the folder. A green dot means the file's content is on this computer; an empty
dot means it is stored only elsewhere and has to be downloaded before it can be opened. Next to it are the remotes
holding a copy, shown in red when there are fewer copies than the [minimum](../remotes/#how-many-copies).

## Selecting files

- **Click** a file to select it and see its details: hash, size, when it was added, where it is stored, other names it
  goes by and its previous versions.
- **Ctrl+click** (**⌘+click** on a Mac) adds a file to the selection, or removes it.
- **Shift+click** selects every file between the last one clicked and this one.
- **Drag** across rows to select them.
- **Ctrl+A** selects everything; **Esc** clears the selection.

## Working with selected files

Pick a remote, then:

- **Upload** sends the selected files that are on this computer to the remote.
- **Check remote** asks the remote whether it really has each file, and corrects the records if not.
- **Download** fetches the selected files that are missing here, trying the chosen remote first and then any other
  remote that has them.
- **Free up space** removes this computer's copy of the selected files, to make room. It only does so for files that
  enough remotes (the [minimum](../remotes/#how-many-copies)) are confirmed to hold right now: each remote is asked
  directly, not just the records. The files stay listed and can be downloaded again.
- **Verify** reads this computer's copies again and checks they still match what was stored. A damaged copy is set
  aside in `.mbr/corrupt/`, so it can be downloaded again from a remote.
- **Verify on remote** downloads the remote's copies to check them the same way. A damaged copy is deleted from the
  remote, so uploading again replaces it.

## Keeping files safe

With nothing selected, the sidebar shows whether every file has enough copies, and offers two actions on the whole
folder:

- **Verify all local copies** checks every file stored on this computer for damage, as **Verify** does above. Disks
  do fail slowly; it is worth doing now and then.
- **Clean up old versions** frees the space taken by previous versions of files and by deleted files, under the same
  rule as **Free up space**: only what enough remotes hold is removed from here. To delete old versions everywhere,
  remotes included, use [`mbr gc --purge`](../cli/).
