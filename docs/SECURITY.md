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
- **Descriptors** must be regular files opened read-only and not `O_PATH`;
  folders, sockets, pipes and devices are refused. Each sent descriptor must be
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
- Received files and remote devices don't exist yet. Before Phase 3 ships:
  incoming-transfer confirmation by default, certificate-pinned device
  identity (never a hostname or IP alone), explicit trust for unattended
  transfers, per-transfer size limits and path-traversal tests for received
  names. Received files are never opened automatically.
- External providers are not implemented; they will run out of process with
  enforced permissions (see [PROVIDERS.md](PROVIDERS.md)).

## Reporting

Open a private security advisory on the GitHub repository.
