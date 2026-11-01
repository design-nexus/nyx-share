# Security

## Threat model

The service runs as the user and serves that user's session bus. Other
processes of the same user are trusted as much as the session bus trusts them;
the goals are to keep **sandboxed apps** inside their sandbox, to never write
outside a folder the user chose, never lose or replace data, and never act on a
remote device without explicit trust (from Phase 3).

## What the service enforces

- **Callers.** Each `Share`/`ShareTo` caller is identified with
  `GetConnectionCredentials`. Another user's call is refused. A caller is
  treated as sandboxed if its process has `/.flatpak-info`, runs in a Flatpak
  or Snap cgroup, or can't be identified at all.
- **Sandboxed callers** must pass files as descriptors (no paths) and can't
  choose a Save to folder target; they use `Share`, where the user picks.
- **Descriptors** must be regular files or folders opened read-only and not
  `O_PATH`; sockets, pipes and devices are refused. A folder is read through
  its descriptor: each entry is opened relative to its folder's descriptor with
  `O_NOFOLLOW`, links and special files are skipped, and the walk stops at 16
  levels and 1000 files. So a link inside a shared folder can't reach anything
  the caller couldn't read. Folders written for it are new ones the service
  made, so nothing is written through an existing link either. Each sent descriptor must be
  used by exactly one item.
- **Content limits.** At most 200 items, 1 MiB of text per item, links limited
  to a list of schemes (no `file:`, `data:` or `javascript:`), content types
  checked for shape.
- **File names** from callers are reduced to one safe name: no directories, no
  `..`, no control characters, no leading dots or dashes, at most 255 bytes.
- **Writing.** Save to folder writes to a new hidden partial file (`O_EXCL`),
  syncs it, then renames it into place without replacing anything; a taken name
  becomes `name (2).ext`. It checks free space first and removes partial files
  on failure or cancellation. Source files are only read.
- **The Share Sheet** can only reach a request with its 128-bit random token,
  passed in its environment (readable by the same user, as everything on the
  session bus is). Requests expire after 30 minutes or when the sheet closes.
- **Notifications and logs** never contain file contents.

## Known limits

- Same-user processes can call the service like any app; this is inherent to a
  session service.
- Nearby sharing (LocalSend) is described in [LOCALSEND.md](LOCALSEND.md). In
  short: an incoming offer needs your acceptance unless it comes from a saved
  computer you marked trusted and the sender proved that computer's key (a
  name or address is never enough); senders pin the receiver's certificate; senders' certificates are
  checked against what they claim; received names are sanitized and never
  replace a file; offers are refused when the receive folder lacks the space
  (plus a 512 MB reserve) or carry more than 1,000 files; uploads are only taken
  from the address and with the per-file tokens of the accepted session;
  received files are never opened automatically. The receiver listens only
  while the service runs, unless `visibility = "off"` turns it off.
- NAS folders ([NAS.md](NAS.md)) keep passwords in the Secret Service, never in
  `nas.toml`, and hand them to `secret-tool` on stdin, not in arguments. Share
  answers only password prompts; an unknown SSH host key or any other question
  is refused. Writes go to a partial file and never replace an existing one;
  sources are only read. A subfolder may not contain `.` or `..`, and each of
  its parts is reduced to a safe name.
- Phones through Flux ([FLUX.md](FLUX.md)): Share talks only to fluxd's own
  socket (0600) and never pairs devices. Files are staged in a private folder
  (0700) as hard links or copies, never moved, and the folder is removed when
  the job ends. Of fluxd's state Share reads only the device and transfer
  lists, never the clipboard or notifications it also holds.
- Nyx apps (Notes, Mail, Pulse) are reached through their own control tools,
  only for the destination you chose: Notes shows a note for you to accept,
  Mail opens a draft you send yourself, Pulse sends the message you shared to
  the chat you picked. Files for Mail and Bluetooth are staged in a private
  folder (0700) under their names, never moved, and removed afterwards.
- QR codes are made locally and kept in the private runtime folder for an hour.
- External providers never run inside the service. Each send runs the
  provider's program in a bubblewrap sandbox with fresh namespaces, read-only
  `/usr` and `/etc`, no home folder, no session bus or display, a cleared
  environment, its own data folder, and no network unless its manifest asks
  (the sheet then says so). It gets only that send's items, files as read-only
  descriptors. Checked by a probe in `crates/nyx-share-external/tests/sandbox.rs`.

## Reporting

Open a private security advisory on the GitHub repository.
