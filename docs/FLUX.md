# Phones through Flux

Phones and tablets paired in [Flux](https://github.com/bjarneo/flux) appear in
Share under My devices. Flux stays an external program: Share doesn't pair,
discover or talk to devices itself, and doesn't parse `flux-cli` output. The code
is `crates/nyx-share-flux`.

## What Share uses

`fluxd`, Flux's user service, answers newline-delimited JSON on
`$XDG_RUNTIME_DIR/flux/fluxd.sock` (0600), or `$FLUX_SOCKET`, with ids to match
replies (`{"id":1,"method":"share.url","params":{"device":"…","url":"…"}}`).
Checked against Flux 0.19.1:

| Method | For |
| --- | --- |
| `state` | Devices (`id`, `name`, `type`, `paired`, `online`, `plugins`) and transfers (`id`, `state`, `done`, `error`). Share reads only these two lists. |
| `share.url` | `http`/`https` links. |
| `share.text` | Text, and links with other schemes. At most 256 KiB. |
| `share.files` | Files and images, as absolute paths. Flux copies each into its outbox before it answers, and returns transfer ids. |
| `transfer.cancel` | Cancelling a job. |
| `sms.send` | A text message through the phone, to one number. Each device's `conversations` (`name`, `address`, `addresses`) in `state` give the suggestions. |

Errors come back with a code and a sentence (`offline`, `not_paired`,
`too_large`, …); Share shows the sentence.

## Behaviour

- A device is listed when it is paired and has the **share** plugin on; its
  status says "Flux", or "Offline · Flux sends files when it's back". Nothing
  is listed while fluxd isn't running. The list is cached for three seconds.
- Text and links go at once and need the device online.
- Files are staged under their own names in a private folder
  (`$XDG_CACHE_HOME/nyx-share/flux/<random>/`, 0700): a hard link to the open
  file where the filesystem allows, otherwise a copy (after checking free
  space). Flux copies them into its outbox, and the folder is removed when the
  job ends.
- Progress follows Flux's transfers. Cancelling cancels them in Flux too. A
  failed transfer fails the job with Flux's reason.
- Files for an offline device wait in Flux's outbox; the job then completes with
  "Waiting in Flux: 1 file goes when Pandora is back" instead of keeping Share
  running.
- At most 100 items per share, Flux's outbox limit.
- A shared folder's files go one by one, under their own names ("a (2).jpg"
  when two subfolders hold an a.jpg).
- **Text message** (`flux-sms:<device>`, under Apps & services): for a paired
  phone with the **sms** plugin, while it is connected. It takes text and links
  (up to 5, joined by new lines, at most 2000 characters). The Share Sheet asks
  for the number and suggests recent one-to-one conversations
  (`ListRecipients`, which sandboxed apps can't call). The job completes when
  the phone has the request ("Pandora is texting Kari"); Flux for Android
  texts one number at a time.

## Not done

- A phone reachable through both Flux and LocalSend shows twice until you link
  them in Settings → Sharing → Linked devices. Names are only a suggestion.

## Checked

With a fake fluxd in the crate's tests (listing, links, text, files with name
clashes, an offline phone, a failed transfer, cancelling a running transfer,
the copy fallback, text messages with suggestions and an offline phone; in the
Share Sheet on a test display, picking a recent conversation), and with Flux 0.19.1 and a paired Android phone: Flux took
a link and a text and reported the file delivered, and the staging folder was
gone afterwards.
