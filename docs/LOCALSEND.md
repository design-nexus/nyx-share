# Nearby sharing (LocalSend)

Nyx Share speaks the official LocalSend protocol (v2.2, as LocalSend 1.18 uses
it), so it sends to and receives from the LocalSend apps on phones, tablets and
computers. The code is `crates/nyx-share-localsend`; nothing here is a protocol
of our own.

## What it does

- **Discovery.** While the service runs, it joins multicast 224.0.0.167:53317
  on every IPv4 interface and announces itself (when it starts, and whenever a
  Share Sheet opens). Devices that answer, or that register with it over HTTPS,
  appear under **Nearby** in the Share Sheet. A device not heard from for 15
  minutes is dropped.
- **Sending.** `prepare-upload`, then one streamed `upload` per accepted file,
  over TLS pinned to the receiver's announced fingerprint. Text and links go as
  LocalSend text messages (a `text/plain` file whose preview is the text), which
  the other side shows without saving. Cancelling closes the connection and
  sends `cancel`.
- **Receiving.** An HTTPS server on TCP 53317 (`register`, `info`,
  `prepare-upload`, `upload`, `cancel`). Every offer opens the Share Sheet with
  the sender's name, address, device key and files; nothing is saved unless you
  accept within two minutes. Accepted files go to the receive folder, through
  a hidden partial file renamed into place without replacing anything (a taken
  name becomes `name (2).ext`), with the sender's SHA-256 checked when it sends
  one. Text messages are shown with a Copy button. Received files are never
  opened automatically. A notification reports the result if the sheet was
  closed.

## Identity and trust

The device key is an RSA-2048 self-signed certificate (`CN=LocalSend User`,
like LocalSend's own), made once and kept in
`$XDG_DATA_HOME/nyx-share/localsend/` (0600). Its fingerprint is the uppercase
hex SHA-256 of the certificate.

- Sending pins the receiver: the TLS handshake fails unless the receiver holds
  the certificate whose fingerprint was announced. A hostname or IP address is
  never trusted on its own.
- Senders present their certificate (mutual TLS). An offer whose claimed
  fingerprint matches the presented certificate is shown with its device key;
  otherwise the sheet says the device is unverified.
- There is no automatic acceptance in this release. Trusted devices with
  unattended receiving come with My Computers (Phase 4) and will require a
  pinned fingerprint.

## Settings

`$XDG_CONFIG_HOME/nyx-share/nearby.toml`, changed from the Share app's settings
or with `SetNearby`; a new name or visibility applies at once:

```toml
visibility = "running"          # discoverable and receiving while the service runs; "off" disables Nearby
alias = "Hades"                 # the name others see; the host name by default
receive_folder = "/home/me/Downloads"   # the XDG Downloads folder by default
```

The service runs while you share, while the Share window is open, and for a
minute after, so this computer is visible to others only then. Keeping it
receiving all the time is a later option.

## Living beside the official app

- Discovery shares UDP port 53317 with other programs (`SO_REUSEPORT`).
- If TCP 53317 is taken (by the LocalSend app, say), the receiver takes another
  free port and announces that one; senders follow the announced port.

## Not supported yet

PIN-protected receivers (reported as such), peers with encryption turned off
(listed as unavailable rather than sent to in the clear), the legacy HTTP
subnet scan, the browser download API, IPv6, folders (a sender's subfolders
are flattened into the receive folder), and partial acceptance of an offer.

## Checks

- `cargo test -p nyx-share-localsend`: fingerprint format against LocalSend's
  own test vector, DTOs, the HTTP layer (chunked bodies, limits), and a
  loopback suite over mutual TLS: accept and save (including a `../` name),
  decline, text message, and refusal of a receiver with another certificate.
- Daemon to daemon on one machine over real multicast and TCP (two private
  buses): discovery both ways, port fallback, files byte-identical, text
  message, decline, cancellation on both sides with no partial file left, and
  the Share Sheet prompt.
- Interoperability with the official LocalSend 1.18.2 app: pending (needs the
  app opened on the desktop).
