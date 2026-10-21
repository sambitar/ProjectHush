# Torrents (TorBox client) — rail pane

**Status:** implemented 2026-10-10, not yet exercised against a real TorBox account (no key
available in this environment). Backend unit tests pass; UI typechecks and builds.

A **Torrents** rail pane that behaves like a torrent client but does the actual torrenting in
the cloud via **TorBox**. The user pastes a magnet / `.torrent` URL (or drops a `.torrent`
file); TorBox fetches it, and when it is ready the daemon downloads the files into the vault.

**Relationship to other docs:** follows the daemon-only-network and vault-blob posture of
[`atproto.md`](atproto.md) and [`hush-reads.md`](hush-reads.md). Unlike Reads, the
data here is **private** (the API key and the file names are user secrets), so all state is
vault-encrypted.

---

## Decisions

| Decision | Choice |
|----------|--------|
| Torrent engine | **TorBox debrid** (cloud). The only setup is pasting an API key in Settings. |
| Network path | **Direct HTTPS from the daemon**, never the webview. `requestdl` puts the key in a query string, so the daemon fetches the CDN URL itself. |
| API key storage | Vault-encrypted sidecar `torrents-state.enc`, beside `reads-state.enc` / `drop-keys.enc`. Never logged, never returned to the UI, never in a URL shown to the user. |
| Downloaded files | **Real vault entries** — chunked XChaCha20-Poly1305 blobs under `blobs/<hex>/chunk-N`. Visible to the rest of the vault. |
| Folder organisation | The vault is a flat content-addressed blob store with no tree, so the tree lives in the sidecar: each torrent keeps its files' in-torrent `path` and the Library reconstructs folders from it. One folder per torrent. |
| Large files | Streamed both ways. Ingest: HTTP Range into a temp file, then 8 MiB chunked AEAD into the vault. Egress: `POST /api/blob/{id}/materialize` decrypts chunk by chunk to a temp file the shell opens/saves, so the bytes never enter the webview. |
| Resume | HTTP Range from the temp file's current length; a complete-but-unstored temp file is stored directly instead of re-fetched. |
| Concurrency | Bounded per torrent (default 2, configurable 1–8 in Settings). |
| Polling | Daemon ticks every 4 s while anything is active, 30 s when idle. The UI polls 3 s while active, 8 s when the pane is open and idle, and stops when the pane is closed and nothing is active. |

---

## Architecture

```
apps/hush-tauri (Torrents rail pane, Settings → TorBox)
    │  /api/torrents/*   (same transport as every other pane: UDS or loopback HTTP)
    ▼
hush-daemon
    ├─ torbox.rs          typed TorBox client (user/me, createtorrent, mylist,
    │                     checkcached, requestdl, controltorrent); error classes;
    │                     429 backoff; Range download into a sink
    ├─ torrents_store.rs  durable shape: api key + queue + library; encode/decode
    ├─ torrents.rs        the pipeline: state machine, polling, bounded concurrent
    │                     downloads, progress, library filing
    └─ account_worker.rs  owns the Vault: state get/put, streamed blob store/delete
                         │
                         ▼
        api.torbox.app (direct HTTPS, bearer key) + the CDN download URL
```

- The webview never talks to TorBox or the CDN. The key never leaves the daemon.
- `hush-core` gained chunked blob storage (`put_blob_stream`, `read_blob_stream`,
  `delete_blob`, `has_blob`); `get_blob` now reads either format, so existing single-file
  blobs are untouched.

### Pipeline

```
add ─▶ Queued ─▶ Sending ─▶ Caching ─▶ Ready ─▶ Downloading ─▶ Completed
                  (createtorrent)  (poll mylist)  (requestdl + Range)
```

- A cached hash is detected up front (`checkcached`) so the UI can say "cached", but the
  torrent is still created to obtain an id for `requestdl`.
- On completion the item moves into the Library and its files are filed under the torrent's
  name; the queue no longer holds it.
- Errors are classified: a bad key / plan limit parks the item in `Error` (user action:
  Retry); a network blip or 429 backs off and retries on its own (5 s → 15 min cap).

---

## API (all under `/api/torrents`)

| Route | Body | Returns |
|-------|------|---------|
| `GET /state` | — | `PublicState` (no API key, no `source_data`) |
| `POST /test` | `{key}` | `UserInfo` (plan, label, email) |
| `POST /key` | `{key}` | `PublicState` (empty key clears) |
| `POST /refresh` | — | `PublicState` |
| `POST /add` | `{source}` | `PublicState` (magnet or `.torrent` URL) |
| `POST /add-file` | `{name, data_base64}` | `PublicState` |
| `POST /pause` \| `/resume` \| `/retry` \| `/remove` | `{id}` | `PublicState` |
| `POST /concurrency` | `{max}` | `PublicState` |
| `DELETE /library/{id}` | — | `PublicState` (deletes the vault blobs too) |

Opening a Library file is not under `/api/torrents`: `POST /api/blob/{id}/materialize`
(`{name}` → `{path}`) decrypts the blob to a temp file the shell opens or saves, so the bytes
never enter the webview. See the Open/Save note below.

A locked vault answers **401** on every state route, matching the Reads contract.

---

## Sharing with a trustee (2026-10-21)

A trustee can offer one torrent instead of sending a multi-gigabyte file. The offer is a pointer:
name, info hash and size cross the sideband; the recipient's own TorBox account fetches the content
and stores it as ordinary vault blobs.

- **Permission:** `torrents` and `peer_torrents` on the contact, both deny by default, announced by
  `TorrentConsent`. Both sides must allow it; the trust menu has the toggle and a badge.
- **Entry points:** `/torrent` in the 1:1 composer, **Share a torrent** in the trust menu, and the
  composer's `+` menu when both sides allow it.
- **Source:** the picker lists Active & Queue and Library entries with a hash. The daemon resolves
  the name/hash/size itself; the UI never supplies a magnet.
- **Accept:** the recipient's daemon builds `magnet:?xt=urn:btih:<hash>&dn=<name>` and queues a
  normal item. A missing TorBox key leaves the offer pending; the modal says so.
- **Limit:** library entries retain the hash, not the original magnet or tracker list. A
  private-tracker torrent may not resolve from a hash-only magnet. The UI states this.

Routes: `POST /api/torrents/share`, `POST /api/torrents/offers/{id}/accept`,
`POST /api/torrents/offers/{id}/decline`; offers are returned in `GET /api/torrents/state`.

## Known limitations (honest)

1. **Open / Save as… streams, with a plaintext temp file.** Both legs are now memory-bounded:
   `POST /api/blob/{id}/materialize` decrypts chunk by chunk (via `Vault::read_blob_stream`)
   into a uniquely named file under the system temp dir (`/tmp/hush-open`; never the runtime
   dir, which is RAM-backed), and `hush_open_blob` / `hush_save_blob` open or copy that path.
   The webview never holds the bytes and the base64 `Array.from` path is gone for blobs. Two
   costs remain: the file is plaintext on disk while it exists (0600, in the 0700 temp dir,
   swept after an hour), and the daemon is a single-threaded actor, so a very large materialize
   blocks other API calls for its duration — the same as the old read did.
2. **Opening needs a handler that can read the temp dir.** The shell hands the materialized
   path to the OS default handler. A sandboxed handler (Flatpak/Snap) with no filesystem access
   cannot see `/tmp/hush-open` and opens nothing — this is what made every torrent file "show
   fine but not open" when the video default was the Stremio Flatpak, whose permissions grant no
   filesystem at all. Making this robust needs the XDG document portal; for now the handler must
   be a native app (e.g. VLC).
3. **Delete is per torrent, not per file.** The daemon's Library delete removes a whole entry
   and its blobs; there is no single-file delete.
4. **"Reveal in vault" is represented by the Vault pane's Torrents tile**, because the vault
   is a flat blob store with no per-file browser to reveal into. The Library itself is the
   vault view for torrent files.
5. **Not verified against a live TorBox account.** Endpoint shapes follow the official docs
   and SDKs; the client's decoding is unit-tested against documented payloads, but no real key
   was available.

---

## Manual test plan (with a real TorBox key)

1. Settings → TorBox: paste the key, **Test connection** (expect plan + email), **Save**.
2. Rail → Torrents: the empty state disappears; the Add bar shows.
3. Paste a well-seeded magnet → it should appear in Active & Queue and walk
   `Sending → Caching → Ready → Downloading → Completed`, then move to Library.
4. Drop a `.torrent` file on the Add bar → same pipeline.
5. Library: open a small file (plays/saves), Save as…, search, sort, switch list/grid,
   descend into a subfolder via the breadcrumb, delete a torrent (confirm).
6. Lock and unlock: the queue and library persist; an in-flight item resumes.
7. Kill the daemon mid-download and restart: the download resumes (Range) rather than
   restarting.
8. Vault pane: the Torrents tile shows the count and total size and jumps to the pane.

---

## Next move (planned, not built)

**Export files out of the app.** The Open path hands a decrypted temp file to the OS handler,
which fails for any sandboxed handler (see limitation 2). An explicit **Export** is the durable
answer for every handler that has ordinary filesystem access.

- The primitive already exists: `Save as…` → `hush_save_blob` decrypts the blob and copies it to
  a user-chosen path, so the file lands outside the vault and opens with anything.
- To make it first-class: rename the action to **Export…**, default the dialog to `~/Downloads`
  (`rfd::FileDialog::set_directory`), add **Export all files** for a whole torrent (preserving the
  folder tree), and surface it on the files rail, chat, mail and conclave attachments too.
- It does not rescue a handler with *zero* filesystem access (e.g. the Stremio Flatpak); only the
  XDG document portal could. Export and a native handler are complementary.
- Exported files are plaintext outside the vault — label the action so it is not mistaken for a
  move.
