# Providers

A provider offers destinations and sends a job's items to one of them. Built-in
providers implement `nyx_share_providers::Provider`:

```rust
pub trait Provider: Send + Sync {
    fn id(&self) -> &'static str;
    fn name(&self) -> &'static str;
    fn kinds(&self) -> Vec<ContentKind>;
    fn destinations(&self) -> Vec<Destination>;   // with availability
    fn send(&self, destination: &str, task: &Task<'_>) -> Result<Outcome, SendError>;
}
```

`send` runs on a worker thread. It reads items through `Task::items` (text, or a
file read with positional reads), reports bytes through `Task::progress` (and, before any bytes move, what it is
doing through `Task::step`, such as connecting),
checks `Task::cancelled()` between chunks, and returns a sentence for the user
plus any outputs. A destination declares the kinds it takes, an optional
`max_items` and `needs_folder`; the service enforces those before `send` runs.

## Built in

| Provider | Destination | Kinds | Behaviour |
| --- | --- | --- | --- |
| `clipboard` | `clipboard` ("Copy") | text, url, image; 1 item | `wl-copy --type <mime>` with the data on stdin. Unavailable without Wayland or wl-clipboard. |
| `folder` | `save` ("Save to folder") | text, image, file | Checks free space, writes each item to a hidden partial file, fsyncs, then renames without replacing (`renameat2(RENAME_NOREPLACE)`, falling back to a hard link, then to check-and-rename on filesystems with neither). Text is saved as `Shared text.txt`. Partial files are removed on failure or cancellation. |

| `localsend` | `localsend:<key>` (each nearby device) | text, url, image, file | LocalSend v2 upload to a device found on the network or tailnet, pinned to its key; see [LOCALSEND.md](LOCALSEND.md). Saved computers are left to `computers`. |
| `computers` | `computer:<id>` (each saved computer) | text, url, image, file | Always listed, with LAN, Tailscale or Offline as status. Sending re-checks the routes in order and pins the saved key. |
| `flux` | `flux:<id>` (each device paired in Flux) | text, url, image, file; 100 | Through fluxd's socket: `share.url`, `share.text`, `share.files` with staged files, progress from its transfers; see [FLUX.md](FLUX.md). |
| `nas` | `nas:<id>` (each NAS folder) | text, image, file | GVfs copy with partial files and no replacing; `subfolder` option; see [NAS.md](NAS.md). |
| `qr` | `qr` ("QR code") | text, url; 1 item | Makes a QR code here (UTF-8 marked with ECI when needed, at most 1,200 bytes) as a PNG in `$XDG_RUNTIME_DIR/nyx-share/qr`, removed after an hour. The Share Sheet shows it with Copy image and Save image. |
| `notes` | `notes` ("Notes") | text, url; 1 item | Nyx Notes' `notes.vault_create`, or `notes.clip` for a web link: Notes shows the new note for you to accept, and the job reports what you decided. Starts Notes if needed; needs a vault open and "proposed edits" allowed in Notes. |
| `mail` | `mail` ("Mail") | text, url, image, file; 20 | Nyx Mail's `mail.draft`: a new message with the texts and links as its body and the files attached (staged in `$XDG_CACHE_HOME/nyx-share/mail`, read by Mail, then removed; 25 MB each). Mail opens it for you to address and send. |
| `cloud` | `cloud:<id>` (each Nyx Cloud account with storage on) | text, image, file | Asks Nyx Cloud (`Cloud1`) to mount the account if it isn't, then copies into its folder like Save to folder; rclone uploads in the background. `subfolder` option. |
| `pulse` | `pulse:<chat>` (your six most recent Telegram chats) | text, url; 1 item | Nyx Pulse's `pulse.messages.send`, which sends at once. Listed only while Pulse is open; chats without a name are left out. |
| `bluetooth` | `bluetooth:<address>` (each paired device with Object Push) | text, image, file | BlueZ's obexd (`bluez-obex`): an OBEX Object Push session, one `SendFile` per item (staged; texts as `.txt`), progress from the transfer's signals, Cancel. The device asks its user to accept. Unavailable without obexd. |
| `external` | `ext:<id>` (each installed extension) | as its manifest says | A separate program in a sandbox; see below. |

Notes, Mail and Pulse are reached through the control interface every Nyx app
serves for Nyx Assistant (`io.github.design_nexus.nyx.Control`,
`Call(name, arguments)`), as the user's explicit choice in the sheet.

## Not done

SMS through Flux, and Nyx Assistant: Assistant has no way yet to receive shared
content into a conversation.

## External providers

Third-party providers never load into the service. Each is a folder,
`~/.local/share/nyx-share/providers/<id>/`, with a manifest and a program:

```toml
id = "word-count"            # the folder's name: lowercase letters, digits, dashes
name = "Word count"
exec = "word-count"          # an executable inside the folder
kinds = ["text", "url", "file"]   # text, url, image, file
max_items = 20               # optional
icon = "accessories-text-editor-symbolic"   # optional, a symbolic icon name
[permissions]
network = false              # the default
```

A manifest with unknown keys, a program outside its folder or unknown kinds is
skipped (the service logs why). Each send runs the program under bubblewrap
(`bwrap`), which must be installed:

- new user, PID, IPC, UTS, cgroup and network namespaces; the network is shared
  only with `network = true`, and the sheet then says "uses the network";
- `/usr` and `/etc` read-only, a fresh `/tmp`, `/run`, `/proc` and `/dev`, the
  provider's folder read-only at `/provider`, and its own writable folder
  (`~/.local/share/nyx-share/provider-data/<id>`) at `/data`, which is `HOME`;
- nothing else of the user's: no home folder, no session bus, no display, a
  cleared environment;
- the items of that one send, and nothing more: texts in the request, files as
  open read-only descriptors (3, 4, …).

The program reads one line on stdin and answers in lines on stdout:

```json
{"version": 1, "method": "send", "items": [
  {"kind": "text", "name": "Shared text", "text": "Hello"},
  {"kind": "file", "name": "report.pdf", "mime": "application/pdf", "size": 2048, "fd": 3}
]}
```

```json
{"step": "Uploading"}
{"progress": 2048}
{"done": {"message": "Uploaded report.pdf"}}
```

or `{"error": "a sentence for the user"}`. Messages are cut to 300 characters.
Cancelling the job kills the sandbox. `examples/providers/word-count` is a
complete example; `crates/nyx-share-external/tests/sandbox.rs` runs a probe in
the real sandbox and checks what it can and can't reach.
