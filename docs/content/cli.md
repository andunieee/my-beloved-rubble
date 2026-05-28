+++
title = "Command line"
weight = 6
[extra]
nav = "command line"
+++

Everything the app does is also available from the `mbr` command. Every command except `init` works from anywhere
inside an attached folder. Files can be named by path, by full hash, or by the first few characters of their hash (at
least 6).

```
mbr init [dir]                    attach mbr to a folder
mbr scan                          take in new files, record removals
mbr ls                            list tracked files
mbr info <path|hash>              full details of one file
mbr remote types                  list the kinds of storage mbr can set up
mbr remote setup <name> <type> [--unencrypted]
                                  set up a new remote, asking for each field
mbr remote ls                     list remotes
mbr remote show <name>            a remote's settings (secrets hidden)
mbr remote edit <name> [--rename NEW] [--set KEY=VALUE...]
                                  rename or change settings
mbr remote scan [name...]         list remotes fully, record what they hold
mbr remote rm <name>              forget a remote
mbr push <remote> [path|hash...]  upload (default: everything not there yet)
mbr pull <remote> [path|hash...]  download from one remote (default: all it
                                  holds that is missing here)
mbr fetch <path|hash>...          download from any remote that has them
mbr check <remote> [path|hash...] ask a remote about specific files
mbr fsck [--remote R] [path|hash...]
                                  check files for damage
mbr drop (--all | path|hash...)   free up space: remove local copies that
                                  enough remotes hold
mbr gc [--purge [--older-than DAYS] [--yes]]
                                  free up (or delete everywhere) old versions
                                  and deleted files
mbr config min-copies [N]         show or set how many copies to keep
mbr config password               set the master password (before any remote)
mbr metadata status               where metadata is backed up, and when
mbr metadata add|rm <remote>...   choose the remotes for metadata backups
mbr metadata backup               encrypt and upload the metadata
mbr metadata restore <remote>     merge the backup on a remote into this folder
```

`mbr gc --purge` deletes old versions and deleted files everywhere, remotes included, after asking you to confirm.
