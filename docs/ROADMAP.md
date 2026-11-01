# Roadmap

| Phase | Scope | State |
| --- | --- | --- |
| 0 | Integration audit | Done: [AUDIT.md](AUDIT.md) |
| 1 | Service, `Share1`, client crate, CLI | Done: daemon with D-Bus activation and idle exit; Share, ShareTo, destinations, jobs, cancel, preferences; `nyx-share` CLI |
| 2 | Share Sheet | Done and checked: preview with thumbnails, sections, pinned and hidden destinations, progress, completion, errors, Retry, Show in folder, light and dark. The context menu (pin, hide, move, only the moves that apply) and moving between sections with the arrow keys checked through the sheet's check hooks |
| — | Clipboard and Save to folder | Done and checked end to end |
| — | `nyx share` in the umbrella CLI, suite.toml entry | Done (nyx umbrella) |
| — | Nyx Files: Share… and Send to → Share | Done through the Files provider protocol (`nyx-share files-provider`); protocol checked end to end, the menu itself to be looked at in Files. A flat Send to → G16 Laptop needs a Files change |
| — | The Share app: nearby devices, transfers, settings | Done: compact window in place of the LocalSend app; checked against a second device over the network |
| 3 | LocalSend nearby sharing, receiving with confirmation | Done ([LOCALSEND.md](LOCALSEND.md)); checked with the official apps in both directions (Linux desktop, Android over Tailscale) |
| 4 | My Computers over LAN and Tailscale | Done: saved computers (from a nearby device or by address) identified by their LocalSend key, LAN and Tailscale routes with LAN first, online status, trusted computers received from without asking, a receive folder per computer; in the Share app, the sheet's Devices section and `nyx-share --device`. Checked between two services on private buses. The settings group checked; Nyx Files gets saved computers and NAS folders in Send to → Share (checked through the provider protocol; the Files menu itself not yet seen) |
| 5 | NAS destinations | Done through GVfs (SMB, SFTP, NFS, local): [NAS.md](NAS.md). Checked against Samba and OpenSSH servers; NFS unverified (libnfs failed against NFS-Ganesha) |
| 6 | Flux phones | Done: paired devices with the share plugin under My devices; links, text and files through fluxd, with progress, cancel and the offline outbox ([FLUX.md](FLUX.md)). Checked with a fake fluxd and with Flux 0.19.1 and an Android phone. Not done: SMS, merging a phone seen through both Flux and LocalSend |
| 7 | Mail, Cloud, Pulse, Bluetooth, QR code, Notes, Assistant; external providers | Done except Assistant ([PROVIDERS.md](PROVIDERS.md)). Checked: QR code on screen and decoded; Notes and Mail with test copies (the review and the draft with its attachment open; accepting in Notes not clicked); Cloud against a stand-in service; Pulse listing your chats (sending not tried, by your choice); Bluetooth to an Android phone; external providers in the real sandbox. Assistant has no way to receive content yet; SMS not done |
| 8 | Settings → Sharing | Done in nyx-settings: nearby sharing, saved computers and trust, NAS folders (passwords through `SaveNas`), and the Share Sheet's destinations (pin, hide, order), talking only to `Share1` through `nyx-share-client`. Checked on screen against a test service; the buttons and dialogs themselves not yet clicked through |
| 9 | Capture, Photos, PDF, Notes, Maps, Mail, Cloud clients | Planned |
| 10 | Third-party docs, provider manifest, portal investigation | API docs and client done; a freedesktop sharing portal is a possible upstream proposal |

Also planned: folder sharing, and the Share app icon in nyx-icons.
