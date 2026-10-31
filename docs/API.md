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
| `GetCapabilities` | `() → s` | JSON: `version`, `providers` (`id`, `name`, `kinds`, `available`, `status`), `max_items`, `max_text_bytes`. |
| `ListJobs` | `() → s` | JSON array of jobs since the service started (newest first, the last 50 finished ones). |
| `GetJob` | `(s id) → s` | JSON job. |
| `CancelJob` | `(s id) → ()` | |
| `SetDestinationPreferences` | `(s update) → ()` | `{"order":[…],"pinned":[…],"hidden":[…]}`; each list present replaces the saved one. |

Property `Version` (`u`, read-only) is `1`. Signals: `JobChanged(s id, s job)`
and `DestinationsChanged()`.

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
- Images and files carry `fd`: a regular file opened **read-only**, not
  `O_PATH`. Every descriptor sent must be used exactly once. `name` and `mime`
  are optional; names are reduced to a safe file name before use.
- Unsandboxed callers may send `path` (absolute) instead of `fd`. Sandboxed
  callers (Flatpak, Snap, or a caller whose process can't be identified) must
  send descriptors.
- At most 200 items. Folders aren't accepted yet.

## Options

`{"app_name": "Photos", "folder": "/home/me/Backups", "notify": true}`, all optional.

- `app_name`: shown in the sheet ("from Photos").
- `folder`: the target for `save` with `ShareTo`. Refused from sandboxed callers,
  who use `Share` so the user picks the folder.
- `notify`: notify when a `ShareTo` job finishes (default `true`).

## Destinations and jobs

```json
{"id": "save", "provider": "folder", "name": "Save to folder", "icon": "folder-download-symbolic",
 "tint": "graphite", "section": "apps", "kinds": ["text", "image", "file"], "needs_folder": true,
 "available": true, "pinned": false, "hidden": false}
```

`section` is `devices`, `nearby` or `apps`; `max_items` limits a share (the
clipboard takes 1); `status` says how a destination is reached or why it isn't
available.

```json
{"id": "…", "destination": "save", "destination_name": "Save to folder", "state": "completed",
 "items": 2, "bytes_done": 2048, "bytes_total": 2048, "message": "Saved 2 items to Backups",
 "outputs": ["/home/me/Backups/a.pdf", "/home/me/Backups/b (2).pdf"], "created": 1793400000, "updated": 1793400001}
```

`state` is `queued`, `running`, `completed`, `failed` or `cancelled`; `message`
is a sentence for people; `outputs` lists what was created. `direction` is
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

`Client::events()` yields `JobChanged` and `DestinationsChanged` as they happen.

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
For checks, `NYX_SHARE_CHECK_CHOOSE` (`save:/path;clipboard`) makes the sheet
choose destinations as if clicked, and `NYX_SHARE_CHECK_ANSWER` (`accept` or
`decline`) answers an offer after two seconds.
