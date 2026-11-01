# Nyx Share and XDG desktop portals

Checked against xdg-desktop-portal 1.22.1 (with the GTK and Hyprland backends).

## What exists

There is no sharing portal. The portals nearest to sharing do other jobs, and
Nyx Share implements none of them, so it can't conflict with them:

| Portal | What it does | Relation to Share |
| --- | --- | --- |
| `Email` (`ComposeEmail`) | Opens the mail client with addresses, a subject, a body and attachments. | Share's Mail destination talks to Nyx Mail directly; Share doesn't implement the Email backend. |
| `OpenURI` (`OpenURI`, `OpenFile`, `OpenDirectory`) | Opens a link or file in the default app. | Unrelated: Share never opens received files on its own. |
| `FileTransfer` | Hands files between sandboxed apps (drag and drop, copy and paste). | Unrelated. |
| `FileChooser` | The file dialog. | Share's Save to folder uses it through GTK (in Nyx, Nyx Files' picker). |
| `Documents` | Exports files into a sandbox. | A portal-mediated Share would use it to hand files to sandboxed providers. |

## Sandboxed apps today

A Flatpak app can call Share directly if it is allowed to talk to the name:
`--talk-name=io.github.design_nexus.nyx.Share1`. The service treats every
Flatpak or Snap caller (and any caller it can't identify) as sandboxed:

- files must come as descriptors (which a sandboxed app can open);
- it can't name a folder for Save to folder; the user picks it in the sheet.

That works without any portal, but it is Nyx-specific: the app must know the
name, and the user grants it per app.

## A sharing portal, as a possible upstream proposal

A freedesktop sharing portal would make "Share…" work the same in every app and
on every desktop, with Nyx Share as one backend among others. A sketch, not an
implementation:

- **Front end** `org.freedesktop.portal.Share`:
  `Share(s parent_window, a{sv} options) → o handle`, with `items` as
  `aa{sv}` (kind, name, mime, text, or a file as a handle into the message's
  FD list), plus `title` and `activation_token`. It returns a Request; the
  response says whether the user shared and to what kind of destination (never
  the destination itself, which is private).
- **Back end** `org.freedesktop.impl.portal.Share`: the same call, handled by
  the desktop's share UI. Nyx Share would implement it by opening its Share
  Sheet for the items, which it already does for `Share`.
- **Permissions**: none to grant. The user chooses each time, as with the file
  chooser.

This would be proposed to the xdg-desktop-portal project, discussed there, and
designed with other desktops; it isn't a prerequisite for Nyx Share, which
keeps its own `Share1` interface either way. Until then Share registers no
portal backend and changes no portal configuration.
