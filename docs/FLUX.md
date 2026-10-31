# Flux (Phase 6, not implemented)

Phones come through [Flux](https://github.com/bjarneo/flux), used as an external
program; Nyx Share doesn't fork it or reimplement its device protocol.

## What Flux offers (audited: 0.18.0 installed, v0.7.1 source)

- `fluxd`, a user service, owns devices and networking. Its only IPC is
  newline-delimited JSON on `$XDG_RUNTIME_DIR/flux/fluxd.sock` (0600), with ids
  to match replies: `{"id":1,"method":"share.url","params":{"device":"Pixel","url":"…"}}`.
- Methods used by Share: `state` (devices with `id`, `name`, `type`, `paired`,
  `online`, `plugins`, `lastSeen`; transfers with progress), `subscribe` (state
  events), `share.files` (absolute paths; returns transfer ids), `share.url`,
  `share.text`, `clipboard.send`, `sms.send` (one recipient; needs the phone's
  Text messages plugin), `transfer.cancel`.
- Errors come back as codes: `offline`, `not_paired`, `not_supported`,
  `unsupported`, `no_device`, `ambiguous`, `bad_params`, …
- Pairing and discovery need the LAN. Off the LAN, Flux dials up to five
  "extra addresses", such as a Tailscale name.
- There is no D-Bus interface, and `flux-cli` prints JSON only for `status --json`.

## Design

- A provider that talks to the socket directly (structured errors), not by
  parsing `flux-cli` output; it is unavailable, with a reason, if the socket is
  missing.
- Paired devices appear under My devices; capabilities follow each device's
  `plugins` (no SMS destination unless the `sms` plugin is on).
- `share.files` takes paths, so the provider hands Flux a private copy of each
  item (from the descriptor) and removes it when the transfer finishes.
- SMS goes through `sms.send` only. RCS or Google Messages features are not
  assumed, and private Google APIs are out of scope.
- A device reachable through both Flux and LocalSend is one entry with both
  capabilities, matched by explicit user pairing, never by name.
