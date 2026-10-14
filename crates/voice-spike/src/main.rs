//! The engine spike runner (Task 23 §2 #4, the one decision made with
//! evidence).
//!
//! `voice-spike` runs the loopback proof: two peer connections on the daemon's
//! own `webrtc` stack carry three seconds of Opus audio, and the run prints
//! the evidence — packets delivered, sample fidelity, arrival cadence, and
//! encode/decode CPU per frame. Nonzero exit when the audio did not survive.
//!
//! `voice-spike --capture [seconds]` records the default input device through
//! `cpal`/ALSA to a WAV — the capture-backend probe (§2 #5).

use std::time::{Duration, Instant};

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("--capture") {
        return capture(
            args.get(2)
                .and_then(|s| s.parse().ok())
                .unwrap_or(2)
                .max(1),
        );
    }

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let report = runtime.block_on(voice_spike::engine_proof())?;
    println!("{}", report.text);
    if !report.verdict_ok {
        std::process::exit(1);
    }
    Ok(())
}

/// The §2 #5 probe: record the default input device, write a WAV, report.
fn capture(seconds: u64) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc as StdArc, Mutex as StdMutex};

    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or("no default input device on this host")?;
    let config = device.default_input_config()?;
    println!(
        "input device: {} — {} Hz, {:?}, {} channels",
        device.name()?,
        config.sample_rate().0,
        config.sample_format(),
        config.channels(),
    );

    let spec = hound::WavSpec {
        channels: config.channels(),
        sample_rate: config.sample_rate().0,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let writer = StdArc::new(StdMutex::new(hound::WavWriter::create("spike-capture.wav", spec)?));
    let written = StdArc::new(AtomicUsize::new(0));

    let writer_in = StdArc::clone(&writer);
    let written_in = StdArc::clone(&written);
    let stream = device.build_input_stream(
        &config.into(),
        move |data: &[i16], _| {
            if let Ok(mut w) = writer_in.lock() {
                for s in data {
                    let _ = w.write_sample(*s);
                }
                written_in.fetch_add(data.len(), Ordering::SeqCst);
            }
        },
        |err| eprintln!("capture error: {err}"),
        None,
    )?;
    stream.play()?;
    std::thread::sleep(Duration::from_secs(seconds));
    drop(stream);
    // `finalize` consumes the writer, and the stream's closure clone is gone
    // with the stream, so the Arc unwraps.
    let writer = StdArc::try_unwrap(writer)
        .map_err(|_| "the capture stream still holds the writer")?;
    writer.into_inner().expect("writer unlock").finalize()?;
    println!(
        "wrote spike-capture.wav: {} samples over {seconds}s",
        written.load(Ordering::SeqCst)
    );
    Ok(())
}

// Keep `Instant` used: the capture probe's cadence prints would take it, but
// the runner above measures inside the lib.
#[allow(unused)]
fn _instant_used(t: Instant) -> Duration {
    t.elapsed()
}
