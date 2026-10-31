# Roadmap

| Phase | Scope | State |
| --- | --- | --- |
| 0 | Integration audit | Done: [AUDIT.md](AUDIT.md) |
| 1 | Service, `Share1`, client crate, CLI | Done: daemon with D-Bus activation and idle exit; Share, ShareTo, destinations, jobs, cancel, preferences; `nyx-share` CLI |
| 2 | Share Sheet | Done and checked: preview with thumbnails, sections, pinned and hidden destinations, progress, completion, errors, Retry, Show in folder, light and dark. Written but not yet exercised by hand: the context menu (pin, hide, move) and arrow-key movement between sections |
| — | Clipboard and Save to folder | Done and checked end to end |
| — | `nyx share` in the umbrella CLI, suite.toml entry | Done (nyx umbrella) |
| — | Nyx Files: Share… and Send to → Share | Done through the Files provider protocol (`nyx-share files-provider`); protocol checked end to end, the menu itself to be looked at in Files. A flat Send to → G16 Laptop needs a Files change |
| — | The Share app: nearby devices, transfers, settings | Done: compact window in place of the LocalSend app; checked against a second device over the network |
| 3 | LocalSend nearby sharing, receiving with confirmation | Done ([LOCALSEND.md](LOCALSEND.md)); checked with the official apps in both directions (Linux desktop, Android over Tailscale) |
| 4 | My Computers over LAN and Tailscale | Done: saved computers (from a nearby device or by address) identified by their LocalSend key, LAN and Tailscale routes with LAN first, online status, trusted computers received from without asking, a receive folder per computer; in the Share app, the sheet's Devices section and `nyx-share --device`. Checked between two services on private buses. Not yet looked at: the settings group with computers in it, and Send to in Nyx Files |
| 5 | NAS destinations | Done through GVfs (SMB, SFTP, NFS, local): [NAS.md](NAS.md). Checked against Samba and OpenSSH servers; NFS untried |
| 6 | Flux phones | Planned: [FLUX.md](FLUX.md) |
| 7 | Mail, Cloud, Pulse, Bluetooth, QR code, Notes, Assistant; external providers | Planned: [PROVIDERS.md](PROVIDERS.md) |
| 8 | Settings → Sharing | Planned; talks only to `Share1` |
| 9 | Capture, Photos, PDF, Notes, Maps, Mail, Cloud clients | Planned |
| 10 | Third-party docs, provider manifest, portal investigation | API docs and client done; a freedesktop sharing portal is a possible upstream proposal |

Also planned: folder sharing, and the Share app icon in nyx-icons.
