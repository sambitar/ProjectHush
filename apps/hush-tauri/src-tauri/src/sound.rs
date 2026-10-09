//! The chat arrival sound.
//!
//! Played natively rather than in the webview: WebKitGTK's audio path depends
//! on the host's GStreamer plugin set (on this machine the PulseAudio sink
//! plugin is missing, so `new Audio(...).play()` silently fails), while `rodio`
//! talks to ALSA directly, like every other desktop app. The clip is compiled
//! into the binary, so playback works wherever the app runs.

use std::io::Cursor;
use std::sync::atomic::{AtomicU8, Ordering};

use rodio::{Decoder, OutputStream, Sink};

/// The bundled clip: 16-bit stereo PCM WAV, ~1.4 s.
static CLIP: &[u8] = include_bytes!("../../src/assets/message-notification.wav");

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
            sink.append(Decoder::new_wav(Cursor::new(CLIP))?);
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
            match Decoder::new_wav(Cursor::new(CLIP)) {
                Ok(source) => {
                    println!("decode: WAV decoded OK — playing ~1.4 s probe");
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
