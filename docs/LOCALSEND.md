# LocalSend (Phase 3, not implemented)

Nearby sharing will speak the official LocalSend protocol, not a protocol of our
own. This is the plan; nothing below exists yet.

## Protocol facts (v2.2, checked against localsend/protocol)

- Discovery: a JSON announcement to multicast 224.0.0.167, UDP port 53317
  (`alias`, `version`, `deviceModel`, `deviceType`, `fingerprint`, `port`,
  `protocol`, `download`, `announce`). Peers answer with `POST
  /api/localsend/v2/register` to the announcer, or a multicast reply. When
  multicast fails, the legacy mode posts `register` to every local address.
- Transfer: the receiver runs the HTTP(S) server. The sender calls
  `POST /api/localsend/v2/prepare-upload` with file metadata (optional `?pin=`);
  the receiver accepts all, some or none (204, 401, 403, 409, 429) and returns a
  session id and per-file tokens; files go up with
  `POST /api/localsend/v2/upload?sessionId&fileId&token`, in parallel; `422` on a
  SHA-256 mismatch; `POST /api/localsend/v2/cancel?sessionId` cancels.
- HTTPS uses a self-signed certificate; the fingerprint is the SHA-256 of that
  certificate. In HTTP mode it is a random string.

## Design

- A `nyx-share-localsend` crate: discovery, client and receiver, with rustls and
  a persistent device certificate in `$XDG_DATA_HOME/nyx-share/`.
- Device identity is the certificate fingerprint, pinned on first trust; a
  hostname or IP address is never proof. Trusted devices are listed explicitly.
- Incoming transfers ask for confirmation by default, show sender, file names
  and sizes, and write through the same partial-file, no-overwrite path as Save
  to folder into a configurable receive folder, with size and free-space limits.
  Received files are never opened automatically.
- Port 53317 is shared with the official LocalSend app: if it is bound, Nyx
  Share won't receive and will say why; sending still works.
- Tailscale: multicast doesn't cross a tailnet, so known tailnet devices are
  probed directly (`/api/localsend/v2/info`, then `register`) at their
  Tailscale IP or MagicDNS name, LAN first with Tailscale as fallback.
- Interoperability is checked against the official app (1.18.2 is installed
  here) in both directions before the provider is marked working.
