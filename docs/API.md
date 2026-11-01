# Share1 D-Bus API

Bus name `io.github.design_nexus.nyx.Share1`, object `/io/github/design_nexus/nyx/Share1`,
interface `io.github.design_nexus.nyx.Share1`, on the session bus. The service
starts on the first call. Structured values are JSON strings; ignore fields you
don't know. Compatible additions (methods, JSON fields, destinations) keep the
version; anything else gets `Share2`.

## Methods

| Method | Signature | Returns |
| --- | --- | --- |
| `Share` | `(s items, ah fds, s options) → s` | Request id. Opens the Share Sheet. |
| `ShareTo` | `(s destination, s items, ah fds, s options) → s` | Job id. Sends without the sheet. |
| `ListDestinations` | `(as kinds, u count) → s` | JSON array of destinations that accept `count` items of `kinds`; every destination when `kinds` is empty. Hidden ones are included and marked. |
| `GetCapabilities` | `() → s` | JSON: `version`, `providers` (`id`, `name`, `kinds`, `available`, `status`, `enabled`), `max_items`, `max_text_bytes`. |
| `ListJobs` | `() → s` | JSON array of jobs, newest first: running ones and the last 50 finished ones (kept in `$XDG_STATE_HOME/nyx-share/history.json`). |
| `GetJob` | `(s id) → s` | JSON job. |
| `CancelJob` | `(s id) → ()` | |
| `SetDestinationPreferences` | `(s update) → ()` | `{"order":[…],"pinned":[…],"hidden":[…],"disabled":[provider ids],"default":"id","history":50}`; each field present replaces the saved one (`"default":""` clears it; `history` is how many finished transfers are kept, at most 500). Destinations of disabled providers aren't listed or accepted. |
| `GetPreferences` | `() → s` | The same fields as JSON, with the history limit in effect. |
| `RetryJob` | `(s id) → s` | Run a failed job again as a new job (its id); possible while `retryable` is true (30 minutes, the last 20 failures). Not for sandboxed callers. |
| `ClearHistory` | `() → ()` | Forget finished transfers. Not for sandboxed callers. |
| `ListRecipients` | `(s destination) → s` | Suggested recipients for a destination with `needs_recipient`, most recent first: `[{name,address}]`. Not for sandboxed callers. |
| `ListDevices` | `() → s` | Linked devices as JSON: `{"devices":[{id,name,routes}],"suggestions":[{name,routes}],"routes":[{id,name,provider,status}]}`. `routes` lists every linkable destination present now (saved computers, Flux, Bluetooth, LocalSend); `suggestions` groups unlinked ones with the same name, for the user to confirm. |
| `SaveDevice` | `(s device) → s` | Link two to six routes into one device (`{id,name,routes}`, empty `id` adds one); returns the id. A route belongs to one device, so it leaves any other. |
| `RemoveDevice` | `(s id) → ()` | Unlink a device; its routes are listed on their own again. |
| `Scan` | `() → ()` | Ask nearby devices to answer now. |
| `Hold` | `() → ()` | Keep the service, and Nearby visibility, running while the caller stays on the bus. |
| `GetNearby` | `() → s` | JSON: `enabled`, `visibility` (`running` or `off`), `alias`, `fingerprint`, `receive_folder`, `problem`, `tailscale`, `tailscale_state`. |
| `SetNearby` | `(s update) → ()` | `{"visibility":…,"alias":…,"receive_folder":…,"tailscale":…}`; fields present are saved and applied at once. |
| `ListComputers` | `() → s` | JSON array of saved computers (fields as in [LOCALSEND.md](LOCALSEND.md#my-computers)), each with `presence`: `lan`, `tailscale`, `offline` or `unknown`. |
| `AddComputer` | `(s from) → s` | Save a nearby device (`localsend:<key>`) or the device answering at `host[:port]`, with the key it presents. Returns the computer as JSON. A key that is already saved is refused. Needs nearby sharing on. |
| `SaveComputer` | `(s computer) → ()` | Change a saved computer (name, routes, `prefer`, `trusted`, `receive_folder`); its key can't change. |
| `RemoveComputer` | `(s id) → ()` | |
| `ListNas` | `() → s` | JSON array of NAS folders (fields as in [NAS.md](NAS.md#settings)), each with `has_password`. |
| `SaveNas` | `(s folder) → s` | Add (empty `id`) or change a NAS folder; returns its id. An optional `password` field is stored in the Secret Service (`application=nyx-share`, `nas=<id>`; empty clears it), never in `nas.toml`, and never listed. |
| `RemoveNas` | `(s id) → ()` | Also removes its saved password. |
| `TestNas` | `(s id) → s` | Connect and check the folder can be written to; a sentence, or `Failed` with the reason. |

Property `Version` (`u`, read-only) is `1`. Signals: `JobChanged(s id, s job)`,
`DestinationsChanged()` and `NearbyChanged()`.

## Items

`items` is a JSON array. `fds` carries one descriptor per file item, referenced
by index:

```json
[
  {"kind": "text", "text": "Hello"},
  {"kind": "url", "text": "https://example.com"},
  {"kind": "image", "fd": 0, "name": "photo.jpg", "mime": "image/jpeg"},
  {"kind": "file", "fd": 1, "name": "report.pdf"}
]
```

- `kind`: `text`, `url`, `image` or `file`.
- Text and links carry `text` (at most 1 MiB). Links must use `http`, `https`,
  `mailto`, `tel`, `sms`, `geo`, `magnet`, `ftp`, `sftp`, `smb` or `nfs`.
- Images and files carry `fd`: a regular file or a folder opened
  **read-only**, not `O_PATH`. Every descriptor sent must be used exactly once. `name` and `mime`
  are optional; names are reduced to a safe file name before use.
- Unsandboxed callers may send `path` (absolute) instead of `fd`. Sandboxed
  callers (Flatpak, Snap, or a caller whose process can't be identified) must
  send descriptors.
- A folder is shared as its files and subfolders, at most 16 deep. Hidden
  entries, links and special files are left out, and links are never followed.
  Each file's `ItemInfo` gets `folder`, where it sits from the shared folder's
  own name down (`"Trip/day 1"`). Save to folder, NAS, Cloud, saved computers
  and LocalSend devices rebuild the layout as a new folder ("Trip", or
  "Trip (2)" if that's taken; a NAS set to skip files already there goes into
  the existing one). Flux, Bluetooth, Mail and the clipboard get the files on
  their own. A folder with no files is refused.
- At most 200 items, and 1000 files once folders are opened up.

## Options

`{"app_name": "Photos", "folder": "/home/me/Backups", "notify": true}`, all optional.

- `app_name`: shown in the sheet ("from Photos").
- `folder`: the target for `save` with `ShareTo`. Refused from sandboxed callers,
  who use `Share` so the user picks the folder.
- `subfolder`: for a NAS folder, a folder under it (`Reports/2026`), made if
  missing. No `.` or `..`.
- `notify`: notify when a `ShareTo` job finishes (default `true`).
- `notify_after`: with `notify`, only if the job took at least this many
  seconds; for callers that show a quick result themselves.
- `recipient`: for a destination with `needs_recipient` (a text message), the
  phone number: digits, an optional leading `+`, and spaces, dashes, dots or
  parentheses. Refused from sandboxed callers, who use `Share` so the user
  picks.

## Destinations and jobs

```json
{"id": "save", "provider": "folder", "name": "Save to folder", "icon": "folder-download-symbolic",
 "tint": "graphite", "section": "apps", "kinds": ["text", "image", "file"], "needs_folder": true,
 "available": true, "pinned": false, "hidden": false}
```

`section` is `devices`, `nearby` or `apps`; `max_items` limits a share (the
clipboard takes 1); `status` says how a destination is reached or why it isn't
available. `needs_folder` and `needs_recipient` mean the Share Sheet asks for a
folder or a phone number before sending; `default` marks the one Settings made
the default.

```json
{"id": "…", "destination": "save", "destination_name": "Save to folder", "state": "completed",
 "items": 2, "bytes_done": 2048, "bytes_total": 2048, "message": "Saved 2 items to Backups",
 "outputs": ["/home/me/Backups/a.pdf", "/home/me/Backups/b (2).pdf"], "created": 1793400000, "updated": 1793400001}
```

`state` is `queued`, `running`, `completed`, `failed` or `cancelled` (with
`retryable` on a failed job that `RetryJob` can run again); `message`
is a sentence for people; `outputs` lists what was created. While a job runs,
`step` may say what it is doing before any bytes move ("Connecting to NAS
Documents…"); it is cleared once bytes flow. `direction` is
`send`, or `receive` for files arriving from a nearby device (then
`destination_name` is the sender).

## Errors

| D-Bus error | Meaning |
| --- | --- |
| `org.freedesktop.DBus.Error.InvalidArgs` | Bad JSON, items, descriptors or options; the destination can't take these items. |
| `org.freedesktop.DBus.Error.AccessDenied` | Not allowed for this caller (another user, a path or folder from a sandboxed app, a wrong sheet token). |
| `org.freedesktop.DBus.Error.UnknownObject` | No such destination or job. |
| `org.freedesktop.DBus.Error.Failed` | The destination is unavailable, or the sheet couldn't be opened. |

The message is always a sentence that can be shown to the user.

## Examples

```sh
busctl --user call io.github.design_nexus.nyx.Share1 /io/github/design_nexus/nyx/Share1 \
  io.github.design_nexus.nyx.Share1 ShareTo ssahs clipboard '[{"kind":"text","text":"Hello"}]' 0 '{}'
```

Rust, with `nyx-share-client` (blocking; call from a worker thread):

```rust
let client = nyx_share_client::Client::connect()?;
let mut content = nyx_share_client::Content::new();
content.url("https://example.com");
let job = client.share_to("clipboard", &content, &Default::default())?;
let done = client.wait(&job, |_| {})?;
```

`Content::file(path)` opens a file or a folder read-only; `Content::bytes(kind, name, mime,
bytes)` shares something that exists only in memory (an edited picture, an
exported page) through a private file removed at once. `Client::events()` yields
`JobChanged` and `DestinationsChanged` as they happen.

## Compatibility check

```sh
cargo build -p nyx-share-daemon
dbus-run-session -- cargo test -p nyx-share-client --test service -- --ignored --test-threads=1
```

## Private: ShareSheet1

Used only by `nyx-share-sheet`; not stable. `GetRequest(s request, s token) → s`,
`OpenItem(s, s, u index) → h` (images only, read-only), `Choose(s, s, s destination,
s options) → s job`, `Dismiss(s, s)`. For offers from nearby devices:
`GetIncoming(s session, s token) → s`, `AnswerIncoming(s, s, b accept)` and
`DismissIncoming(s, s)` (declines an unanswered offer). The id and token reach
the sheet in `NYX_SHARE_REQUEST` or `NYX_SHARE_INCOMING`, and `NYX_SHARE_TOKEN`.
For checks, `NYX_SHARE_CHECK_CHOOSE` (`save:/path;clipboard;nas:<id>>Sub/Folder`)
makes the sheet choose destinations as if clicked, `NYX_SHARE_CHECK_STEPS`
(`focus:save;up;menu:clipboard;pin:save`) focuses tiles, moves like the arrow
keys, opens a tile's menu and runs its actions, one step every 1.5 s, logging
the focus and the order of the tiles, and `NYX_SHARE_CHECK_ANSWER`
(`accept` or `decline`) answers an offer after two seconds. In the Share window,
`NYX_SHARE_CHECK_ADD` (`/a:/b`) fills the tray, `NYX_SHARE_CHECK_PAGE`
(`transfers`, `settings` or `add-nas`) opens a page or the Add a NAS folder
dialog, `NYX_SHARE_CHECK_NAS` (`name|address|tailscale|user|password`) fills
that dialog and adds, and `NYX_SHARE_CHECK_SEND=1` sends to the first device
that appears.

## Linked devices

A linked device replaces its routes in `ListDestinations` with one destination,
`device:<id>` (provider `devices`): it takes what any route takes, and its status
names the routes that are up ("Flux · Bluetooth"). A share to it tries the routes
that take the items, reachable ones first in the user's order; when one fails the
job starts over with the next (its `step` says "Trying Bluetooth…"). A cancel stops
it. The links are kept in `$XDG_CONFIG_HOME/nyx-share/devices.toml`.
