# Assets

`message-notification.wav` — chat message arrival sound.

Source: freesound.org sound 740421, uploaded by "anthonyrox"
(`https://freesound.org/s/740421/`). Its licence page lists the terms it is
shared under; keep this note beside the file when redistributing.

`ring.mp3` — voice call ringtone, heard by the vault being called. 8.65 s,
44.1 kHz, 256 kb/s: two ring bursts with a 1.2 s gap and a 1.5 s gap after
the second, so looping it back to back is seamless.

`tone.mp3` — voice call ringback, heard by the vault that opened the room
while it waits alone. 30.05 s, 24 kHz, 160 kb/s: five identical 6 s cycles
(2 s tone, 4 s silence), ending in silence on a cycle boundary, so it too
loops seamlessly.

Both were supplied by the operator from `~/Downloads` and their provenance
and licence are unrecorded. **They are redistributed with the app as-is and
must be replaced or cleared before any public release** — unlike
`message-notification.wav` above, there is no source or licence note for
them. The loop points and cycle lengths are asserted in
`src-tauri/src/sound.rs` (`bundled_clips_decode`), so a replacement has to
decode and be at least as long, but nothing checks its licence.
