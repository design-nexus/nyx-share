# NAS destinations

A NAS folder is a destination that copies into a folder on a NAS (or any
server) running nothing but its usual file service. The code is
`crates/nyx-share-nas`.

## How it works

Share reaches the folder through GVfs, the same layer Nyx Files uses, so SMB,
SFTP and NFS need no extra setup and share Files' connections:

| Address | Backend | Notes |
| --- | --- | --- |
| `smb://host/share/folder` | `gvfsd-smb` | User, domain and password as saved; guest shares connect anonymously. |
| `sftp://user@host[:port]/path` | `gvfsd-sftp` (ssh) | The host key must already be known (see below). |
| `nfs://host/export/path` | `gvfsd-nfs` | Not yet tried against a real NFS server. |
| `file:///path` | local | A mounted share, or any local folder. |

Sending to a NAS folder:

1. Mounts the share if it isn't, answering the password prompt from the saved
   credentials. A folder that doesn't exist yet is mounted through its nearest
   existing parent and then created.
2. Tries the main address, then the Tailscale address if one is set.
3. Writes each item to a hidden partial file (`.name.<tag>.nyx-share-partial`),
   then moves it to the first free name without replacing anything: a taken
   name becomes `name (2).ext`, or, with **Skip files already there**, the item
   is skipped. Source files are only read; nothing is moved or deleted.
4. Reports progress per megabyte; cancelling (also while mounting) stops the
   job and removes the partial file. Failures say what happened and how many
   items were already copied; Retry in the sheet starts the job again.

The sheet's context menu on a NAS folder has **Send to a subfolder…**, which
sends into a folder under the NAS folder (made if missing) without changing the
saved folder. `ShareTo` takes the same as `{"subfolder": "Reports/2026"}`.

## Settings

`$XDG_CONFIG_HOME/nyx-share/nas.toml`, changed from the Share app's settings
(**NAS folders**), the `nyx-share nas` commands or `SaveNas`:

```toml
[[nas]]
id = "2e580355-9134-444a-85fd-a75c8e24f82d"
name = "NAS Documents"
uri = "smb://nas.local/Documents/Inbox"
tailscale_uri = "smb://nas.tailnet.ts.net/Documents/Inbox"   # optional
username = "ken"                                              # optional
domain = "WORKGROUP"                                          # optional
conflict = "keep-both"                                        # or "skip"
```

Several entries may point at different folders of one NAS.

## Credentials

Passwords are never written to `nas.toml`. They are kept in the Secret Service
(GNOME Keyring or KeePassXC) through `secret-tool`, with the attributes
`application=nyx-share` and `nas=<id>`, and passed to `secret-tool` on stdin.
Removing a NAS folder removes its password. A wrong password fails with "refused
the saved user name or password" instead of asking again.

Share answers only password prompts. Any other question (an unknown SSH host
key, mostly) is refused, and the job fails with a sentence saying so: connect
to the server once in Files, accept the key there, and Share can use it. Note
that GVfs's ssh skips host-key checks for `localhost` addresses.

## Checked

Against Samba (a guest share and a password-protected share) and OpenSSH SFTP
servers in containers: copying files, keep-both naming, Skip mode,
subfolders, mounting from cold and after an unmount, a wrong password, the
Tailscale address as fallback, cancelling during the mount and mid-file (no
partial file left), byte-identical copies, an unknown SSH host key refused
without changing `known_hosts`, and adding a NAS folder from the Share app.

## Known limits

- Mounting an SMB share can take 25 seconds or more on some servers, before
  the first byte is written; the sheet, the Share app and the CLI say
  "Connecting to …" meanwhile.
- **Test** writes and deletes a hidden `.nyx-share-check-…` file; a NAS with a
  recycle bin keeps it there.
- NFS is untried.
- Folders can't be shared yet (like every destination).
