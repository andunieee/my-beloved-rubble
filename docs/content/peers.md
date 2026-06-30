+++
title = "Peers"
weight = 6
[extra]
nav = "peers"
+++

A folder can live on several devices at once: each keeps its own copy of the folder's metadata — the names, folders and
history of the files, the remotes, the settings — and they sync it with each other over [iroh](https://iroh.computer),
directly when they are on the same network, through a relay otherwise. The files' content isn't sent between devices:
it lives on the [remotes](../remotes/), and each device downloads what it needs from them.

<p align="center">
  <img src="../screenshots/peers.png" alt="The Peers tab: two peers, a device asking to sync, and one found on the local network">
</p>

## Adding a device

Every device has an id for each folder, shown in the **Peers** tab under **This computer** (`mbr peer id`). To sync two
devices, add each one's id on the other: **Add a peer** (`mbr peer add <id> --name laptop`). Syncing starts once *both*
have added each other; until then the one that was added says *waiting for them to add this device as a peer*. A device
that isn't a peer but tries to sync shows up in the list, with **Add it**.

While the **Peers** tab is open, mbr also looks for other mbr devices on the same local network. They appear under
**Add a peer** with the name each one announces, and **Add** adds one without typing its id. This is how to bring a
folder to a new device without copying long ids around.

To bring a folder to a new device, start a new, empty mbr folder there, and add the two devices to each other. The empty
folder *joins* the other's: everything it recorded is replaced by the other device's metadata, and every file appears as
a link, dangling until its content is downloaded from a remote. A folder that has files of its own never joins another,
and two folders that both have files never merge: they are different folders.

## The master password

The password is never sent. A device that joined a folder knows the folder is encrypted, but has to be told the password:
type it in Settings (**Enter the master password**, or `mbr config password`). The remotes become usable there once it
is typed, with the same settings and sign-ins as on the other devices. Remotes of the **Local disk or directory** kind
are the exception: their path belongs to the machine that set them up, so they stay there and are never synced.

## When syncing happens

The app syncs with every peer when the folder is opened, after every scan, every minute, and on **Sync now**. From the
command line, `mbr sync` scans and syncs once; `mbr sync --watch` keeps running, accepting the peers' syncs and syncing
every minute. A folder with no peers makes no network connections unless the **Peers** tab is open, where it listens for
devices on the local network.

Changes from a peer show up in the folder: new files appear as links, removed ones go, replaced ones point at their new
content. A file changed on this device since the last scan is left alone; the next scan records it, and since it is the
newest change, it wins on every device. Two devices renaming or replacing the same file at once is resolved the same
way: the newest change wins, and the other stays in the file's history.

Removing a peer stops syncing with it; it keeps what it has.
