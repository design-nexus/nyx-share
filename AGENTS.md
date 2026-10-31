# Share

Use pinned public Nyx crates and canonical nyx-design STYLE.md. Preserve the
application ID `io.github.design_nexus.nyx.Share`, the bus name
`io.github.design_nexus.nyx.Share1`, the binaries `nyx-share`, `nyx-share-daemon`
and `nyx-share-sheet`, and the display name `Share`. `Share1` is public: change it
only compatibly (new methods, new JSON fields); anything else needs `Share2`.
`ShareSheet1` is private to the sheet. Keep `nyx-share-client` free of GTK and
tokio. Run only targeted checks; the D-Bus check runs on a private bus
(see docs/API.md). Install with `./install.sh` without launching anything.
Don't claim a destination works until a transfer through it has been checked.
