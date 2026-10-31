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
| 3 | LocalSend nearby sharing, receiving with confirmation | Done and checked against itself over the network ([LOCALSEND.md](LOCALSEND.md)); interoperability with the official app pending |
| 4 | My Computers over LAN and Tailscale | Planned (builds on Phase 3) |
| 5 | NAS destinations | Planned: [NAS.md](NAS.md); needs nyx-transfer additions |
| 6 | Flux phones | Planned: [FLUX.md](FLUX.md) |
| 7 | Mail, Cloud, Pulse, Bluetooth, QR code, Notes, Assistant; external providers | Planned: [PROVIDERS.md](PROVIDERS.md) |
| 8 | Settings → Sharing | Planned; talks only to `Share1` |
| 9 | Capture, Photos, PDF, Notes, Maps, Mail, Cloud clients | Planned |
| 10 | Third-party docs, provider manifest, portal investigation | API docs and client done; a freedesktop sharing portal is a possible upstream proposal |

Also planned: folder sharing, and the Share app icon in nyx-icons.
