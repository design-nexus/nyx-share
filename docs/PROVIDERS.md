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

## Planned

SMS through Flux, Nyx Mail, Nyx Cloud, Nyx Pulse, Bluetooth,
QR code, Nyx Notes and Nyx Assistant; see [ROADMAP.md](ROADMAP.md).

## External providers (design, not implemented)

Third-party providers will never load into the daemon. They will run as separate
processes described by a manifest (`~/.local/share/nyx-share/providers/<id>.toml`
with `exec`, `kinds`, `max_items` and the permissions it needs) and speak
newline-delimited JSON over stdin/stdout, like Nyx Files' providers. The service
will pass only the items of a job the user chose for that provider, as
descriptors, and refuse requests outside the declared permissions. The manifest
format will be published here before the first external provider ships.
