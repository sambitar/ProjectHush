# Torrents (TorBox client) — rail pane

**Status:** implemented 2026-10-10, not yet exercised against a real TorBox account (no key
available in this environment). Backend unit tests pass; UI typechecks and builds.

A **Torrents** rail pane that behaves like a torrent client but does the actual torrenting in
the cloud via **TorBox**. The user pastes a magnet / `.torrent` URL (or drops a `.torrent`
file); TorBox fetches it, and when it is ready the daemon downloads the files into the vault.

**Relationship to other docs:** follows the daemon-only-network and vault-blob posture of
[`hush-articles.md`](hush-articles.md) and [`hush-reads.md`](hush-reads.md). Unlike Reads, the
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
| Large files | Streamed both ways in the ingest leg: HTTP Range into a temp file, then 8 MiB chunked AEAD into the vault. See the OPEN note about the open/save leg below. |
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

A locked vault answers **401** on every state route, matching the Reads contract.

---

## Known limitations (honest)

1. **Open / Save as… on very large files buffers.** The ingest leg is streamed, but the
   Library's Open and Save as… reuse the app's existing `/api/blob/{id}` route, which
   base64-encodes the whole file in one JSON body. A multi-GB file materialises in the daemon
   and the webview. Closing this needs a streaming blob route plus a Tauri command that writes
   a temp file without the webview holding the bytes. Logged OPEN in `DEEPSEEKPROBLEMS.MD`.
2. **Delete is per torrent, not per file.** The daemon's Library delete removes a whole entry
   and its blobs; there is no single-file delete.
3. **"Reveal in vault" is represented by the Vault pane's Torrents tile**, because the vault
   is a flat blob store with no per-file browser to reveal into. The Library itself is the
   vault view for torrent files.
4. **Not verified against a live TorBox account.** Endpoint shapes follow the official docs
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
