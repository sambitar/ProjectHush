# Conclave Voice Phase 2 — Real Audio Implementation Plan

**Goal:** Two joined trustees hold a real two-way Opus voice conversation inside a conclave,
replacing Phase 1's stub SDP with the daemon's own `webrtc` stack carrying live audio.

**Architecture:** One `VoicePlane` per live session (the `paper_planes` pattern), one peer
connection per pair of joined trustees, DTLS-SRTP, non-trickle ICE (candidates gathered into the
SDP, the Paper convention). The joiner's `VoiceOffer` *is* its join announcement (Phase 1
convention preserved); the peer answers with real SDP; `VoiceAnswer` completes the pair. Capture
and playout live in the daemon; the webview stays off the media path.

**Tech Stack:** `webrtc` 0.21.1 (already shipped), `rtc-media` samples, Opus 1.4 (vendored C, the
spike's proven `cc` build), `cpal`/ALSA for capture and playout (PipeWire-host via the ALSA
emulation, spike-proven on this machine), hand-rolled linear resampler and jitter buffer.

**Spec:** `docs/tasks/conclave-voice.md` §8 Phase 2 (approved 2026-10-13), §2 #4–5 (engine +
capture decisions), `crates/voice-spike` (the promoted evidence).

## Global Constraints

- Media is **never written to disk** — no temp files, no recording (spec §2 #14).
- Voice state is **memory-only**; a lock/restart is a leave and tears every peer connection down
  (spec §2 #13, the `paper_planes_shutdown` hook).
- One WebRTC stack, one config surface: the plane reads the same `stun_url` config the Paper
  reads; no new TURN fields (spec §10).
- Mute stops capture **before** the flag fans out (spec §6 `VoiceMute` note).
- The capture backend is **cpal/ALSA on this machine** — `libpipewire-0.3-dev` cannot be installed
  here (no sudo); `pipewire-rs` remains the distro-readiness probe (logged OPEN in
  `DEEPSEEKPROBLEMS.MD`, spec §2 #5 keeps it first for the shipped unit).
- Phase 2 is **two trustees**; the N-way mesh (per-pair offers need a `to` field on
  `VoiceOffer`/`VoiceAnswer`) is Phase 3 and must not be built here (YAGNI).
- The wire format does not change in Phase 2 — `VoiceOffer { session_id, sdp }` now carries a real
  gathered SDP where Phase 1 carried `VOICE_FAKE_SDP`.

## Task 1: `crates/hush-audio` — Opus FFI, resampler, jitter buffer

**Files:**
- Create: `crates/hush-audio/Cargo.toml`, `crates/hush-audio/build.rs`, `crates/hush-audio/src/lib.rs`,
  `crates/hush-audio/vendor/README.md`
- Modify: `crates/hush-daemon/Cargo.toml` (depend on `hush-audio`)

**Interfaces:**
- Produces: `OpusEncoder::new()` / `encode(&[i16]) -> Vec<u8>`, `OpusDecoder::new()` /
  `decode(&[u8]) -> Vec<i16>` (48 kHz mono, 20 ms / 960-sample frames — the spike's proven FFI),
  `resample_to_48k(input: &[f32], in_rate: u32, channels: u16) -> Vec<f32>` (linear),
  `JitterBuffer::new(target_frames, max_frames)` / `push(frame: Vec<i16>)` / `pop() -> Option<Vec<i16>>`.
- Consumes: vendored Opus 1.4 (same pinned hash as `crates/voice-spike/vendor/README.md`).

- [x] **Step 1: Write failing tests** — `opus round_trips_a_tone` (encode→decode of a 440 Hz frame
      stays within a small RMS error of the input, per the spike's metric), `resampler_hits_48k`
      (44.1 kHz sine in → 48 kHz out, length ratio 48/44.1, no NaN, RMS preserved ±10 %),
      `jitter_buffer_smoothes_bursts` (burst of 5 pushed → pop returns silence-pad then frames in
      order; overfull drops oldest; underrun returns `None`).
- [x] **Step 2: Run — expect FAIL** — `cargo test -p hush-audio` (crate does not exist yet).
- [x] **Step 3: Implement** — `build.rs` compiles the vendored Opus with `cc` exactly like
      `crates/voice-spike/build.rs` (including `VAR_ARRAYS`); `vendor/README.md` carries the pinned
      fetch command; the FFI is the spike's `lib.rs` block lifted verbatim; the resampler and
      jitter buffer are new, small, and documented in the module header.
- [x] **Step 4: Run — expect PASS** — `cargo test -p hush-audio`.
- [x] **Step 5: Daemon depends on it** — add to `crates/hush-daemon/Cargo.toml`;
      `cargo build -p hush-daemon` green.

## Task 2: `voice_data.rs` — the voice plane (peer connections + audio legs)

**Files:**
- Create: `crates/hush-daemon/src/voice_data.rs`
- Modify: `crates/hush-daemon/src/main.rs` (declare the module; registry field comes in Task 4)
- Test: `voice_data.rs` in-file `#[cfg(test)]` (the `paper_data.rs` test shape)

**Interfaces:**
- Consumes: `webrtc` PC API as in `paper_data.rs` (`PeerConnectionBuilder`, gathered-description
  helper, event handler), `hush-audio::{OpusEncoder, OpusDecoder, JitterBuffer}`,
  `rtc-media::Sample`, `webrtc::track::track_local::TrackLocalStaticSample`.
- Produces: `VoicePlane::new(stun: Option<String>)`, `offer(peer) -> Result<String, String>` (PC +
  send track + gathered offer SDP, serialized like the Paper's), `answer(peer, offer_sdp) ->
  Result<String, String>`, `accept_answer(peer, answer_sdp)`, `set_muted(bool)`, `forget(peer)`,
  `shutdown()`, `stats() -> VoiceStats { sent_frames, decoded_frames, played_frames, peers_live }`.
- Media internals: capture ticker (20 ms) encodes one 960-sample frame and writes it to every live
  link's send track; `on_track` pumps `TrackRemoteEvent::OnRtpPacket` into the peer's decoder and
  jitter buffer; a playout drain pulls frames and counts them (`played_frames`). **In Task 2 the
  capture source is a synthetic 440 Hz tone and playout is a counter** — no device required; the
  real device streams are Task 3.

- [x] **Step 1: Write the failing test** — `two_planes_carry_audio_on_loopback`: A offers B, B
      answers, A accepts; within ~5 s B's `decoded_frames` exceeds 10 and A's `sent_frames`
      exceeds 10; `shutdown()` on both. Modeled on `a_trustee_dials_the_hub_and_an_update_crosses`.
- [x] **Step 2: Run — expect FAIL** — `cargo test -p hush-daemon voice_data` (module missing).
- [x] **Step 3: Implement** — the plane per the interfaces above; `MediaEngine::default()` with
      `register_default_codecs()` on **both** offerer and answerer (the spike's
      `ErrSessionDescriptionMissingIceUfrag` lesson); await `RTCIceGatheringState::Complete` before
      reading `local_description()`; links keyed by peer account; mute gates the encode step.
- [x] **Step 4: Run — expect PASS** — `cargo test -p hush-daemon voice_data`.
- [x] **Step 5: No-disk assertion in test** — assert the test writes no files (it must not need
      any temp path).

## Task 3: Real capture and playout (cpal) + tone override

**Files:**
- Modify: `crates/hush-daemon/src/voice_data.rs`, `crates/hush-daemon/Cargo.toml` (`cpal = "0.15"`)
- Test: `voice_data.rs` in-file

**Interfaces:**
- Consumes: `cpal::traits::{DeviceTrait, HostTrait, StreamTrait}` (spike-proven import set),
  `hush_audio::resample_to_48k`.
- Produces: `AudioTap::open()` — starts the default input stream (any rate/channels accepted;
  converted to 48 kHz mono f32 → i16 frames) and the default output stream (48 kHz stereo/mixed
  from per-peer jitter buffers, silence on underrun). Env `HUSH_VOICE_TEST_TONE=1` replaces the
  input with the synthetic tone (the automated dogfood uses this; no mic needed headless).

- [x] **Step 1: Write the failing test** — `a_real_device_round_trips_when_present`: guarded by
      `std::env::var("HUSH_VOICE_TEST_TONE")` — with the tone override, two planes on loopback
      still cross audio exactly as in Task 2 (proves the tap does not break the path); plus a
      device-present smoke test that opens the tap, records `input_frames_seen > 0` within 500 ms,
      and is `#[ignore]`d when no default device exists.
- [x] **Step 2: Run — expect FAIL** — `cargo test -p hush-daemon voice_data` (no tap yet).
- [x] **Step 3: Implement** — the capture thread feeds a bounded queue (drop-oldest); the playout
      callback mixes one frame per peer per tick; device failure is **degrade, not crash** — a
      plane whose device cannot open logs once and keeps the signaling plane alive (the Paper's
      failure-is-expected posture).
- [x] **Step 4: Run — expect PASS** — `cargo test -p hush-daemon voice_data`; then a manual
      `HUSH_VOICE_TEST_TONE=1` run of the two-plane test with a real device check via
      `cargo run -p voice-spike -- --capture 2` left to the dogfood (Task 5).

## Task 4: Wire the real SDP through the daemon

**Files:**
- Modify: `crates/hush-daemon/src/main.rs` — `App` gains `voice_planes: tokio::sync::Mutex<HashMap<String, Arc<voice_data::VoicePlane>>>`
  keyed by session id; `voice_join` builds and fans a **real** offer; `voice_apply_inbound`
  becomes async and answers a `VoiceOffer` with a **real** gathered answer via the plane, applies
  `VoiceAnswer` via `accept_answer`; `voice_leave` / ended rooms / `ConclaveEnd` / the lock hook
  (line ~461, beside `paper_planes_shutdown`) drop the session's plane; delete `VOICE_FAKE_SDP`.
- Modify: `crates/hush-daemon/src/account_worker.rs` — only if the fan-out call sites need the
  async answer (they do not own media; expect no change).
- Test: `crates/hush-daemon` existing `voice_api_tests` + new wiring tests

**Interfaces:**
- Consumes: `VoicePlane` (Task 2/3), the stun value the Paper passes to `DataPlane::new` (same
  `ClientConfig::stun_url`), the existing `VoiceManager` state machine (unchanged).
- Produces: unchanged HTTP surface — `/api/voice/*` keeps its routes and shapes; only the SDP
  bytes inside payloads become real.

- [x] **Step 1: Update the failing tests** — `voice_api_tests` assertions change from the stub
      string to: an offer/answer SDP starts with `v=0`, carries `m=audio`, `ice-ufrag`, and an
      `a=fingerprint` line; a locked vault is still 401; a restart still leaves (no plane for the
      session id). New test: two daemons (or two managers + two planes in-process, the
      `voice-dogfood` shape) complete a join handshake and both planes report `peers_live == 1`.
- [x] **Step 2: Run — expect FAIL** — `cargo test -p hush-daemon voice_api` (stub assertions gone).
- [x] **Step 3: Implement** — the join path: manager join → plane for the session (create if new)
      → `plane.offer(peer)` per existing member (two trustees: one) → fan-out `VoiceOffer` with
      the real SDP. The inbound path (inside the existing tokio-mutex block, which may be held
      across the awaited gather): manager join → `plane.answer(from, offer)` → fan-out
      `VoiceAnswer`. `VoiceAnswer` inbound: roster join (unchanged) + `accept_answer`. Leave/end/
      lock: `voice_plane_reset`/`voice_planes_shutdown` mirroring the paper functions at
      `main.rs:5798–5814`.
- [x] **Step 4: Run — expect PASS** — `cargo test -p hush-daemon`.
- [x] **Step 5: Workspace** — `cargo test --workspace` green (the UI and core are untouched).

## Task 5: Verify end-to-end (dogfood + no-plaintext)

**Files:**
- Modify: `scripts/voice-dogfood.sh` — both daemons run with `HUSH_VOICE_TEST_TONE=1`; after the
  existing signaling assertions, the script asserts both daemon logs show `sent_frames` and
  `decoded_frames` climbing, then `played_frames` on the answering side; keeps the Phase 1
  terminal-state assertions; scans `instances/relay/relay.db` with `strings` for the test tone's
  absence-of-plaintext claim and asserts no media file appeared in either vault.

- [x] **Step 1: Extend the script** as above; keep every Phase 1 assertion passing.
- [x] **Step 2: Run it** — `./scripts/voice-dogfood.sh` exits 0 with the new assertions.
- [x] **Step 3: Full suite** — `cargo test --workspace` and the UI suite green.
- [x] **Step 4: Real-mic instructions** — document in the task spec's Phase 2 section:
      `./scripts/run-two-ui.sh` with real devices (no tone env) — join on both UIs, speak, hear.

## Task 6: Docs and honest bookkeeping

**Files:**
- Modify: `docs/tasks/conclave-voice.md` — tick the two Phase 2 boxes with dates and the dogfood
  evidence; record the `pipewire-rs` probe outcome (cannot be exercised here — no dev headers, no
  sudo; cpal/ALSA shipped; pipewire-rs stays first for the distro unit) and the Phase 3 wire note
  (per-pair offers need a `to` field).
- Modify: `DEEPSEEKPROBLEMS.MD` — OPEN entry for the un-runnable pipewire-rs probe; FIXED entries
  for anything the build shakes out.

- [x] **Step 1:** Update both files; no new spec forks — anything surprising goes to the log first.
      **Done 2026-10-13** — spec §8 Phase 2 boxes ticked with dogfood evidence; problems log has
      the pipewire-rs `OPEN` entry plus the `VoiceAnswer`/`sed` fixes.
