# Roadmap

| Phase | Scope | State |
| --- | --- | --- |
| 0 | Integration audit | Done: [AUDIT.md](AUDIT.md) |
| 1 | Service, `Share1`, client crate, CLI | Done: daemon with D-Bus activation and idle exit; Share, ShareTo, destinations, jobs, cancel, preferences; `nyx-share` CLI |
| 2 | Share Sheet | Done and checked: preview with thumbnails, sections, pinned and hidden destinations, progress, completion, errors, Retry, Show in folder, light and dark. Written but not yet exercised by hand: the context menu (pin, hide, move) and arrow-key movement between sections |
| — | Clipboard and Save to folder | Done and checked end to end |
| — | `nyx share` in the umbrella CLI, suite.toml entry | Next (umbrella change) |
| — | Nyx Files: Share… and Send to | Next: a Files provider manifest first; a flat Send to needs a Files change |
| 3 | LocalSend nearby sharing, receiving with confirmation | Planned: [LOCALSEND.md](LOCALSEND.md) |
| 4 | My Computers over LAN and Tailscale | Planned (builds on Phase 3) |
| 5 | NAS destinations | Planned: [NAS.md](NAS.md); needs nyx-transfer additions |
| 6 | Flux phones | Planned: [FLUX.md](FLUX.md) |
| 7 | Mail, Cloud, Pulse, Bluetooth, QR code, Notes, Assistant; external providers | Planned: [PROVIDERS.md](PROVIDERS.md) |
| 8 | Settings → Sharing | Planned; talks only to `Share1` |
| 9 | Capture, Photos, PDF, Notes, Maps, Mail, Cloud clients | Planned |
| 10 | Third-party docs, provider manifest, portal investigation | API docs and client done; a freedesktop sharing portal is a possible upstream proposal |

Also planned: persistent transfer history (jobs are kept in memory until the
service exits), folder sharing, and the Share app icon in nyx-icons.
