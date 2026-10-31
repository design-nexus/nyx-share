# NAS destinations (Phase 5, not implemented)

"Send to NAS" copies into a configured folder on a NAS that runs nothing but its
usual file service.

## What exists in the suite

- **nyx-transferd** (`Transfer1`, nyx-transfer): rclone-based, background jobs in
  SQLite, retries, cancellation, notifications, passwords in Secret Service.
  Native profiles: SFTP, FTP, FTPS, WebDAV. SMB works through an imported
  rclone remote. A plain copy runs `rclone copyto --immutable`, so it fails on
  an existing name, and one job copies one file.
- **Nyx Files** reaches SMB and SFTP through GVfs mounts.
- **NFS** has no backend; only a local mount is usable.

## Plan

Reuse nyx-transferd rather than build a second file-transfer stack. That needs
three compatible additions to `Transfer1`, proposed to nyx-transfer:

1. `ListProfiles()`: non-secret profile metadata.
2. An enqueue variant with a conflict policy that keeps both (rename), never
   replacing.
3. Batch ids, so a multi-file share is one job to watch.

A NAS destination in Share then stores a display name, a Transfer profile, a
default folder, optional LAN and Tailscale endpoints, pinning and conflict
behaviour; several destinations may point at different folders of one NAS. The
sheet lets you pick another folder for one share without changing the default.
Credentials stay in nyx-transfer's Secret Service entries; Share stores none.
NFS destinations use a local mount point and the Save to folder writer.

Open question for Ken: SMB as a native nyx-transfer profile type (rclone's `smb`
backend) or GVfs through Files.
