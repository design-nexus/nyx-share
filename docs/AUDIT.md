# Phase 0: integration audit

Checked on 2026-10-30 against the local checkouts in `~/Projects` and the
installed tools. "Exists" means the code or binary was read or run, not that a
plan mentions it.

## Suite conventions

- **Repositories.** Every app is its own repository; the `nyx` umbrella records
  pins in `suite.toml` and holds no app source. Shared crates are `nyx-core`
  (pinned at `52cba5e`) and `nyx-design` (`nyx-theme`, `nyx-ui`, `nyx-icons`).
  New GitHub repositories need Ken's approval (given for this one).
- **Names.** App IDs are `io.github.design_nexus.nyx.<Name>`, binaries
  `nyx-<slug>`, settings `~/.config/nyx-<slug>/settings.toml`. Test copies take
  an app-ID override; this repository uses `NYX_SHARE_APP_ID`.
- **Services.** Existing D-Bus services use `io.github.design_nexus.nyx.<Name>1`:
  `Cloud1` (nyx-cloud) and `Transfer1` (nyx-transfer). Both are zbus daemons,
  D-Bus-activated into a `Type=dbus` systemd user unit, returning JSON strings
  with a payload-free `Changed` signal. The handoff's `org.nyx.Share1` was
  replaced by `io.github.design_nexus.nyx.Share1` (Nyx doesn't own `org.nyx`).
- **Design 2.0.** `nyx-design/STYLE.md` is canonical. Apps call
  `nyx_theme::use_design_2()` then `nyx_theme::install`. nyx-ui provides
  `chrome::app_header`, `widgets::{dialog, banner, empty_state, icon_button, …}`,
  `keys::install` and `settings_card`; it has no popover or toast helper.
- **Design pin.** `suite.toml` records `9465abd`, but most apps pin the newer
  `5c41729` (Settings pins `34e9ace`). Nyx Share pins `5c41729`.

## Components that exist and what Share reuses

| Component | What it offers | Use in Share |
| --- | --- | --- |
| nyx-files | GTK 4 file manager (fork of strata). A **Send to…** submenu (removable drives, recent folders) and a file-provider protocol (`~/.config/nyx-files/providers/<id>/provider.json`, NDJSON over stdin/stdout) with `send-to` and `send-to-entry` placements and `"feedback":"none"`. FileChooser portal backend. SMB/SFTP through GVfs. zbus with the **async-io** runtime. | First client, through the provider protocol (no Files code change). A flat "Send to → G16 Laptop" needs a Files change because `send-to` nests under the provider's name. The client crate must not force tokio. |
| nyx-transfer | `nyx-transferd` (`Transfer1`): rclone subprocess, SQLite job queue, retries, cancel, notifications; SFTP, FTP, FTPS, WebDAV natively; SMB and cloud through an imported rclone remote; secrets in Secret Service. | Best fit for NAS sending. Needs: a profile list method, a keep-both conflict policy (plain copies run `--immutable` and fail on an existing name), and batch grouping. |
| nyx-cloud | `Cloud1`: rclone FUSE mounts of cloud accounts; a blocking Rust client pattern. | Model for the client crate; "Nyx Drive" in the handoff is this (there is no nyx-drive). |
| nyx-settings | Static section list in `apps/settings/src/sections/mod.rs`; native pages talk to services over D-Bus off the main thread and listen for signals (see `sections/cloud.rs`). Parses `tailscale status` in `sections/network.rs`. | Phase 8 Sharing page, talking only to `Share1`. |
| nyx-core | `app`, `paths`, `prefs`, `cmd` (`background`, `show_in_folder`, `atomic_write`), `control`. No notification or secret-storage crate. | The sheet uses it; the daemon doesn't (its `gtk` default feature would link GTK into the daemon). |
| nyx-capture, nyx-photos, nyx-mail, nyx-notes, nyx-pulse, nyx-pdf, nyx-maps | Export/copy features exist; none has a share action. nyx-pulse is the Telegram client; nyx-mail can attach files. | Phase 9 clients and Phase 7 destinations. |
| `nyx` CLI (`nyx/bin/nyx`) | A fixed bash `case`; no subcommand plugins. | `nyx share` needs an explicit `share) exec nyx-share "$@"` line; a generic `nyx-<name>` fallback would launch apps (`nyx photos`). |

Not present: **Nyx Drive**, **Nyx Browser**, a shared notification crate, a
shared secret-storage crate. Notifications are raw `org.freedesktop.Notifications`
calls (as in nyx-transferd); secrets use the `secret-service` crate or `secret-tool`.

## External protocols and tools

- **LocalSend** (official app 1.18.2 installed). Protocol v2.2: multicast
  announcement on 224.0.0.167:53317 and HTTPS REST on port 53317
  (`/api/localsend/v2/register`, `prepare-upload`, `upload`, `cancel`, `info`).
  The fingerprint is the SHA-256 of the device's self-signed certificate.
  Running a Nyx receiver beside the official app contends for port 53317.
  See [LOCALSEND.md](LOCALSEND.md).
- **Flux** (`omarchy-flux` 0.18.0 installed; the local clone is v0.7.1). The only
  IPC is newline-delimited JSON on `$XDG_RUNTIME_DIR/flux/fluxd.sock`: `state`,
  `subscribe`, `share.files`, `share.url`, `share.text`, `clipboard.send`,
  `sms.send` (one recipient). No D-Bus interface; `flux-cli` prints JSON only for
  `status --json`. Pairing is LAN-only; Tailscale works through "extra
  addresses". Flux isn't KDE Connect compatible. See [FLUX.md](FLUX.md).
- **Tailscale** 1.104.2 installed. Nothing in the suite talks to it except the
  Settings network page.
- **NFS**: no backend anywhere in the suite; only a local mount would work.
  See [NAS.md](NAS.md).

## Conflicts reported to Ken

1. D-Bus name: suite form adopted (`io.github.design_nexus.nyx.Share1`).
2. Nyx Drive and Nyx Browser don't exist.
3. `nyx share` needs an umbrella change (explicit case), not done in this release.
4. zbus runtime: nyx-files uses async-io; the client crate uses zbus defaults.
5. Files' provider protocol nests Send to destinations under the provider name.
6. The nyx-design pin differs between `suite.toml` and the apps.
7. LocalSend port 53317 is shared with the official app.
8. NFS has no backend; SMB only through rclone remotes or GVfs.
