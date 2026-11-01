# Adding Share to an app

Any Linux app can offer "Share…" through Nyx Share, from any language and any
toolkit, by calling one D-Bus method. The reference is [API.md](API.md); this
page is the short way in.

## Which call

- **`Share`**: opens the Share Sheet for your items; the user picks where they
  go. Use this for a Share button. It returns at once; the transfer, if any, runs
  in the service and survives your app closing.
- **`ShareTo`**: sends straight to a destination you name (from
  `ListDestinations`), for a "Send to" menu or automation. It returns a job id;
  follow it with `GetJob` or the `JobChanged` signal.

Pass files as **read-only descriptors**, not paths: open each file and put the
descriptor in the message (`ah`), referring to it by index in the items. A
sandboxed app can do this too. Texts and links go in the items themselves.

```json
[{"kind": "file", "fd": 0, "name": "report.pdf"}, {"kind": "url", "text": "https://example.com"}]
```

Set `app_name` in the options so the sheet says where the items came from. The
service's error messages are sentences you can show as they are.

## Examples

All of these are run against the real service by `tests/examples.sh`:

| Example | Language | Shows |
| --- | --- | --- |
| [`share-sheet.sh`](../examples/integration/share-sheet.sh) | shell, busctl | The Share Sheet for files, passed as descriptors. |
| [`send-and-wait.sh`](../examples/integration/send-and-wait.sh) | shell, busctl | Listing destinations, `ShareTo`, waiting for the job. |
| [`share.c`](../examples/integration/share.c) | C, GDBus | The Share Sheet for files with a `GUnixFDList`, and showing the service's refusal. |
| `nyx-share-client` | Rust | `Client::connect()?.share(&content, &options)`; see [API.md](API.md#examples). |

The Nyx apps are larger examples: Capture (an edited picture from memory with
`Content::bytes`), Photos (the selection), PDF (a document with unsaved changes
as a copy), Notes (a note's current text), Maps (a link) and Mail (an
attachment from memory).

## From a sandbox

A Flatpak app needs `--talk-name=io.github.design_nexus.nyx.Share1`. The service
then treats it as sandboxed: descriptors only, and no Save to folder target of
its own. See [PORTAL.md](PORTAL.md) for portals.

## Adding a destination

Destinations come from providers. Third-party providers are separate programs
run in a sandbox, described by a manifest; see
[PROVIDERS.md](PROVIDERS.md#external-providers) and the
[word-count example](../examples/providers/word-count).

## Compatibility

`Share1` is versioned by its name and the `Version` property (`1`): methods,
JSON fields and destinations may be added; anything else would be `Share2`.
Ignore JSON fields you don't know. The checks a compatible service must pass:

```sh
cargo build -p nyx-share-daemon
dbus-run-session -- cargo test -p nyx-share-client --test service -- --ignored --test-threads=1
dbus-run-session -- tests/examples.sh
cargo test -p nyx-share-external --test sandbox      # the provider sandbox
```
