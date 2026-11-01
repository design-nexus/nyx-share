# nyx-share

The command-line client. It talks to the service over D-Bus like any app: files
are opened read-only and passed as descriptors. The service starts on demand.

## Share

```sh
nyx-share FILE...                       # open the Share Sheet for files
nyx-share --text "Hello" --url https://example.com
nyx-share --to clipboard --url https://example.com
nyx-share --to save --folder ~/Backups report.pdf
nyx-share --to qr --url https://example.com
nyx-share --device "G16 Laptop" report.pdf      # a saved computer or nearby device, by name
nyx-share --nas "NAS Documents" --subfolder Reports/2026 report.pdf
```

| Option | Meaning |
| --- | --- |
| `--text TEXT` | Share a text; may be repeated. |
| `--url URL` | Share a link; may be repeated. |
| `--to DESTINATION` | Send straight to a destination id (see `nyx-share destinations`), without the sheet. |
| `--device NAME` | Send to a saved computer or nearby device, by name. |
| `--nas NAME` | Send to a NAS folder, by name. |
| `--subfolder PATH` | With `--nas` (or a cloud account): into this folder under it, made if missing. |
| `--recipient NUMBER` | With a text message destination (`flux-sms:…`): the phone number to text. |
| `--folder DIR` | The folder for `--to save`. |
| `--no-wait` | Print the job id and return instead of waiting. |

Without `--to`, `--device` or `--nas`, the Share Sheet opens and the command
returns. With them it waits, shows progress on a terminal, then prints the
result and any outputs (saved paths, NAS addresses).

## Destinations and jobs

```sh
nyx-share destinations [--json]   # every destination, with what it takes
nyx-share jobs [--json]           # running and recent transfers
nyx-share cancel JOB
nyx-share version                 # the CLI's version and the service's API version
```

## My computers

```sh
nyx-share computers                       # saved computers, with routes and trust
nyx-share computers add 100.101.102.103   # by address (IP or name, :port if not 53317)
nyx-share computers add "Pixel"           # or a device nearby right now
nyx-share computers trust "G16 Laptop"    # receive from it without asking
nyx-share computers untrust "G16 Laptop"
nyx-share computers remove "G16 Laptop"
```

## NAS folders

```sh
nyx-share nas
echo "$PASSWORD" | nyx-share nas add "NAS Documents" smb://nas.local/Documents/Inbox \
    --user ken [--domain WORKGROUP] [--tailscale smb://nas.tailnet.ts.net/Documents/Inbox] \
    [--skip-existing] --password-stdin
nyx-share nas test "NAS Documents"
nyx-share nas remove "NAS Documents"
```

The password is read from stdin (never an argument, which other users could
see) and kept in the keyring. See [NAS.md](NAS.md).

## Nyx Files

`nyx-share files-provider` is the Nyx Files provider (JSON lines on
stdin/stdout); the installer registers it. It isn't meant to be run by hand.

## Exit status

`0` on success; `1` when a send fails or is cancelled, or the service refuses
(the reason goes to stderr as a sentence); `2` for a usage error.
