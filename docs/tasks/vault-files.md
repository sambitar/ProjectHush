# Your files — the vault's own encrypted file library

**Status:** implemented 2026-10-23.
**Surface:** Vault → Your files (a bento tile, then a full-bleed sub-pane).

## What it is

A personal file store inside the vault. A user adds files from the computer
they are running on, and they are encrypted with the vault's master key. Nothing
is uploaded, and nothing leaves the device.

This is deliberately separate from **Exchanged files**, which browses the
attachments sent to or received from a contact. The two tiles sit beside each
other in the Vault bento because they answer different questions: what came from
a person, and what did I put here myself.

## Storage

- **Index:** `vault-files.enc`, an encrypted sidecar written through
  `Vault::write_encrypted_file`. Each entry is
  `{ id, name, size, blob_id, added_at }`.
- **Bytes:** the existing content-addressed blob store (`blobs/<hex>`), the same
  one chat, mail and torrents use. The desktop import path streams through
  `Vault::put_blob_stream`, so a multi-GB file is never held in memory or sent
  through the webview.
- **Backup:** the sidecar and the blobs are ordinary vault files, so the existing
  backup tar carries them with no extra work.

## API

| Route | Body | Result |
|---|---|---|
| `GET /api/vault/files` | — | `{ files: [...] }` |
| `POST /api/vault/files/import` | `{ path }` | `{ file, state }` |
| `POST /api/vault/files/upload` | `{ name, data_base64 }` | `{ file, state }` |
| `DELETE /api/vault/files/{id}` | — | the new state |

All four round-trip through `AccountWorker`, so a locked vault answers 401 and
the master key stays in the daemon.

- `import` is the desktop path: the Tauri picker returns paths, and the daemon
  opens and encrypts each one. It inherits `/api/send-file`'s existing trust
  boundary (a local client may name a local path).
- `upload` is the browser-build fallback and is capped at 24 MiB before base64
  expansion. Its route raises Axum's body limit so the cap, not the framework,
  is what refuses a file.
- Open and Save as… reuse `POST /api/blob/{id}/materialize` and the existing
  `hush_open_blob` / `hush_save_blob` shell commands, so the bytes stay out of
  the webview on desktop.

## Deleting

Removing an entry deletes the blob only when nothing else points at it. Blobs
are content-addressed, so an imported file and a chat attachment can be the same
bytes. The delete path checks:

1. the remaining entries in `vault-files.enc`;
2. `Account::blob_referenced`, which scans chat, mail, drops, drafts and conclave
   files;
3. `TorrentsManager::blob_referenced`, which scans the queue and the library.

If any check says yes, the entry goes and the bytes stay. A locked worker or a
failed check is treated as "still referenced": leaving bytes is safe, deleting
a live attachment is not.

The same rule applies to the Torrents Library: deleting a torrent entry now asks
`blob_still_used` first, which checks the remaining torrents, this sidecar, and
the account records. Without that, deleting a torrented copy of an imported file
would have left the import listed but unopenable.

## Known limits

- Flat list, newest first. No folders, search, or drag-and-drop yet.
- Desktop imports have no size cap. Browser uploads are capped at 24 MiB.
- Importing the same bytes twice creates two entries (two names), and they share
  one blob.
- The file name is a display name: the daemon strips path components and control
  characters before storing it.
