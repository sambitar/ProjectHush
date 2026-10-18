//! The chat arrival sound and the voice call cues.
//!
//! Played natively rather than in the webview: WebKitGTK's audio path depends
//! on the host's GStreamer plugin set (on this machine the PulseAudio sink
//! plugin is missing, so `new Audio(...).play()` silently fails), while `rodio`
//! talks to ALSA directly, like every other desktop app. The clips are compiled
//! into the binary, so playback works wherever the app runs.

use std::io::Cursor;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Mutex, OnceLock};

use rodio::{Decoder, OutputStream, Sink, Source};

/// The bundled chat clip: 16-bit stereo PCM WAV at 44.1 kHz, 0.35 s.
static CHAT_CLIP: &[u8] = include_bytes!("../../src/assets/message-notification.wav");

/// The ringtone: what a vault being called hears. 8.6 s, a ring-ring pattern
/// with a trailing gap, so repeating it back to back is seamless.
static RING_CLIP: &[u8] = include_bytes!("../../src/assets/ring.mp3");

/// The ringback tone: what the vault that opened the room hears while it waits
/// alone. 30 s of five identical 6 s cycles (2 s tone, 4 s silence), ending in
/// silence on a cycle boundary, so it too repeats seamlessly.
static TONE_CLIP: &[u8] = include_bytes!("../../src/assets/tone.mp3");

/// `OutputStream` cannot be re-opened cheaply and each one spawns a control
/// thread, so the first successful stream is kept for the process lifetime.
/// 0 = not tried, 1 = unavailable, 2 = ready.
static STREAM_STATE: AtomicU8 = AtomicU8::new(0);

/// Play the arrival sound. Fire-and-forget: the clip plays on its own sink in
/// the background, and a machine with no usable audio device stays silent
/// rather than surfacing an error to a chat user.
pub fn play_clip() {
    eprintln!("hush-tauri: chat sound requested");
    // `OutputStream::try_default` probes the host's audio backend; the result
    // is cached so a headless machine pays for one probe, not one per message.
    match STREAM_STATE.load(Ordering::Relaxed) {
        1 => {
            eprintln!("hush-tauri: chat sound skipped — no usable audio device");
            return;
        }
        2 => {}
        _ => {
            let usable = OutputStream::try_default().is_ok();
            STREAM_STATE.store(if usable { 2 } else { 1 }, Ordering::Relaxed);
            if !usable {
                eprintln!("hush-tauri: chat sound skipped — no usable audio device");
                return;
            }
        }
    }

    // The stream must outlive the sink, so open a fresh one per play on
    // purpose: the process may have been suspended, and a stale handle would
    // silently drop the sound. Opening costs a thread for ~1.4 s.
    std::thread::spawn(|| {
        let attempt = || -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
            let (_stream, handle) = OutputStream::try_default()?;
            let sink = Sink::try_new(&handle)?;
            sink.append(Decoder::new_wav(Cursor::new(CHAT_CLIP))?);
            sink.sleep_until_end();
            Ok(())
        };
        if let Err(e) = attempt() {
            eprintln!("hush-tauri: chat sound failed: {e}");
        }
    });
}

#[tauri::command]
pub fn play_chat_sound() {
    play_clip();
}

// --- Voice call cues ------------------------------------------------------
//
// A call is the one sound that has to keep going: a ringtone that plays once
// is a missed call, and a ringback that plays once leaves the caller unsure
// whether the call is still up. So these two loop until the room's state says
// otherwise, which makes them the app's only sounds with a lifetime to manage.
//
// The state is pushed, not polled: the shell sends what should be audible and
// the worker makes that true. Sending the same cue again is a no-op, so the
// shell can send it from its 4 s poll without restarting the clip each time.

/// What the speakers should be doing about voice right now.
#[derive(Clone, Copy, PartialEq, Eq)]
enum VoiceCue {
    /// Nothing. Also the state a failure falls back to: a stuck ringtone is
    /// worse than a missed one.
    None,
    /// This vault is being called.
    Ring,
    /// This vault opened the room and is still alone in it.
    Tone,
}

impl VoiceCue {
    /// Parse the wire name the shell sends. An unknown name is ignored rather
    /// than treated as `None`: a newer shell asking for a cue this build does
    /// not know must not silence one it is already playing.
    fn parse(name: &str) -> Option<VoiceCue> {
        match name {
            "none" => Some(VoiceCue::None),
            "ring" => Some(VoiceCue::Ring),
            "tone" => Some(VoiceCue::Tone),
            _ => None,
        }
    }

    fn clip(self) -> Option<&'static [u8]> {
        match self {
            VoiceCue::None => None,
            VoiceCue::Ring => Some(RING_CLIP),
            VoiceCue::Tone => Some(TONE_CLIP),
        }
    }
}

/// The channel to the cue worker, created on the first cue. Behind a `Mutex`
/// because a `Sender` is `Send` but not `Sync`, and a `static` must be both.
static CUE_TX: OnceLock<Mutex<Sender<VoiceCue>>> = OnceLock::new();

/// The cue worker: one thread for the process, so a cue change is a message
/// rather than a thread spawn, and stopping is immediate rather than whenever
/// the current clip happens to end.
///
/// It owns the `OutputStream` because it has to: cpal's stream is deliberately
/// neither `Send` nor `Sync`, so it cannot live in a `static` or be handed to
/// another thread. The device is opened for a cue and dropped when the cue
/// changes, so an app that is not in a call holds no audio device — and the
/// stream is never reused across a suspend, which is the failure the chat
/// sound's per-play stream exists to avoid.
fn cue_worker(rx: Receiver<VoiceCue>) {
    let mut playing: Option<(OutputStream, Sink)> = None;
    let mut current = VoiceCue::None;
    while let Ok(cue) = rx.recv() {
        if cue == current {
            continue;
        }
        current = cue;
        // Dropping the stream stops its audio; `stop` first so the sink does
        // not hold the mixer open while the stream goes.
        if let Some((_, sink)) = playing.take() {
            sink.stop();
        }
        let Some(clip) = cue.clip() else { continue };
        let Ok((stream, handle)) = OutputStream::try_default() else {
            eprintln!("hush-tauri: voice cue skipped — no usable audio device");
            continue;
        };
        let Ok(sink) = Sink::try_new(&handle) else {
            continue;
        };
        let Ok(decoder) = Decoder::new_mp3(Cursor::new(clip)) else {
            eprintln!("hush-tauri: voice cue skipped — the bundled clip did not decode");
            continue;
        };
        sink.append(decoder.repeat_infinite());
        playing = Some((stream, sink));
    }
}

/// Set what the speakers should be doing about voice: `"none"`, `"ring"`, or
/// `"tone"`. Idempotent, so the shell may send the current state every poll.
#[tauri::command]
pub fn set_voice_cue(cue: String) {
    let Some(cue) = VoiceCue::parse(&cue) else {
        return;
    };
    let tx = CUE_TX.get_or_init(|| {
        let (tx, rx) = channel();
        std::thread::spawn(move || cue_worker(rx));
        Mutex::new(tx)
    });
    let Ok(tx) = tx.lock() else { return };
    let _ = tx.send(cue);
}


/// Every bundled clip decodes, to real audio of the length it is supposed to be.
///
/// MP3 is a rodio *feature* (`mp3` → `symphonia-mp3`), and this crate takes
/// rodio's defaults, so the day that default changes the cues break. `new_mp3`
/// disappearing would be a compile error, which is the loud half — but a file
/// the decoder cannot actually read (an unusual sample rate, a truncated copy)
/// fails at runtime instead, and `set_voice_cue` has nowhere to report that but
/// stderr. A ringtone that never sounds looks exactly like a quiet room.
#[test]
fn bundled_clips_decode() {
    let clips = [
        (
            "chat wav",
            0.3,
            Decoder::new_wav(Cursor::new(CHAT_CLIP)).expect("chat wav decodes"),
        ),
        (
            "ring mp3",
            8.0,
            Decoder::new_mp3(Cursor::new(RING_CLIP)).expect("ring mp3 decodes"),
        ),
        (
            "tone mp3",
            29.0,
            Decoder::new_mp3(Cursor::new(TONE_CLIP)).expect("tone mp3 decodes"),
        ),
    ];
    for (name, least, source) in clips {
        // `total_duration` is `None` for MP3 — symphonia would have to scan the
        // whole stream to answer — so length is counted from the samples, which
        // also proves the decoder produced audio and not just a parsed header.
        let rate = source.sample_rate() as usize * source.channels() as usize;
        // The decoder yields the stream's native `i16` samples; `i32` holds the
        // absolute value without the overflow `i16::MIN.abs()` would have.
        let mut peak = 0i32;
        let mut samples = 0usize;
        for sample in source {
            peak = peak.max(i32::from(sample).abs());
            samples += 1;
        }
        let seconds = samples as f32 / rate as f32;
        assert!(
            peak > 327,
            "{name} decoded to silence (peak {peak} of 32767) — no audio came out"
        );
        assert!(
            seconds >= least,
            "{name} decoded to {seconds:.2} s, expected at least {least} s"
        );
    }
}

/// The cue names are the contract between the shell and this module: the shell
/// sends these exact strings, and a typo on either side is a cue that silently
/// never plays.
#[test]
fn cue_names_are_the_wire_contract() {
    assert!(matches!(VoiceCue::parse("none"), Some(VoiceCue::None)));
    assert!(matches!(VoiceCue::parse("ring"), Some(VoiceCue::Ring)));
    assert!(matches!(VoiceCue::parse("tone"), Some(VoiceCue::Tone)));
    // Unknown names are ignored rather than read as silence: a newer shell
    // asking for a cue this build does not know must not stop one it is
    // already playing.
    assert!(VoiceCue::parse("siren").is_none());
    assert!(VoiceCue::parse("Ring").is_none());
    assert!(VoiceCue::parse("").is_none());
}

/// Probe the host audio path the way `play_clip` uses it: open the default
/// output stream, decode the embedded WAV, and actually play it so a person at
/// the machine can confirm the speakers. Run with
/// `cargo test -p hush-tauri --lib device_probe -- --nocapture`.
#[cfg(test)]
#[test]
fn device_probe() {
    match OutputStream::try_default() {
        Ok((_stream, handle)) => {
            println!("device: default output stream opened OK");
            match Decoder::new_wav(Cursor::new(CHAT_CLIP)) {
                Ok(source) => {
                    println!("decode: WAV decoded OK — playing ~0.35 s probe");
                    let sink = Sink::try_new(&handle).expect("open sink");
                    sink.append(source);
                    sink.sleep_until_end();
                    println!("playback: finished");
                }
                Err(e) => println!("decode: FAILED: {e}"),
            }
            drop(handle);
        }
        Err(e) => println!("device: FAILED: {e}"),
    }
}
