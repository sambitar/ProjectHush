//! Conclave voice — the media plane (Task 23 Phase 2).
//!
//! One [`VoicePlane`] per live voice session, one peer connection per pair of
//! joined trustees: capture → Opus encode → RTP/DTLS-SRTP straight to the
//! peer, and the mirror image inbound. This is the engine the spike
//! (`crates/voice-spike`) proved on the daemon's own `webrtc` stack — same
//! codec registry, same 20 ms frame cadence, same non-trickle handshake as
//! [`crate::paper_data`] (both descriptions carry their gathered ICE, so the
//! exchange is one round trip).
//!
//! The control payloads (`VoiceOffer`/`VoiceAnswer`) ride the conclave fan-out
//! exactly as Phase 1 built them; only the SDP bytes stopped being fake. The
//! plane knows nothing about conclave authority — the `VoiceManager` in
//! `hush-core` owns the room's rules; this module only moves audio for peers
//! the caller tells it to dial or answer.
//!
//! **Failure is degraded, not fatal**, the paper's posture: a device that
//! cannot be opened means silence on one side while the signaling plane and
//! the peer connections keep working — mute state, joins and leaves stay
//! truthful even when this machine has nothing to say into.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use rtc::rtp_transceiver::rtp_sender::{
    RTCRtpCodec, RTCRtpCodingParameters, RTCRtpEncodingParameters, RtpCodecKind,
};
use rtc_media::Sample;
use webrtc::media_stream::track_local::static_sample::TrackLocalStaticSample;
use webrtc::media_stream::track_local::TrackLocal;
use webrtc::media_stream::track_remote::{TrackRemote, TrackRemoteEvent};
use webrtc::media_stream::MediaStreamTrack;
use webrtc::peer_connection::{
    MediaEngine, PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler,
    RTCIceGatheringState, RTCIceServer, RTCPeerConnectionState, SettingEngineBuilder,
};
use webrtc::runtime::{channel, default_runtime, Receiver, Runtime, Sender};

use hush_audio::{fill_tone, resample, to_i16, Decoder, Encoder, JitterBuffer};
use hush_core::config::TurnConfig;
use serde::Serialize;

/// The default registry's Opus payload type — what the media engine
/// negotiates (opus/48000/2, PT 111).
const PT_OPUS: u8 = 111;
const SSRC: u32 = 1001;

/// How long the whole handshake may take before it is given up on. A peer
/// that cannot connect in this long is not audible, but the room keeps
/// working and the leave machinery stays live.
const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(15);

/// The capture/playout cadence: one 20 ms Opus frame.
const FRAME_TICK: Duration =
    Duration::from_millis(hush_audio::FRAME_SAMPLES as u64 * 1000 / hush_audio::SAMPLE_RATE as u64);

/// The one runtime handle every peer connection and timer here shares.
fn runtime() -> Arc<dyn Runtime> {
    static RUNTIME: OnceLock<Arc<dyn Runtime>> = OnceLock::new();
    Arc::clone(
        RUNTIME.get_or_init(|| default_runtime().expect("runtime-tokio is enabled for the daemon")),
    )
}

/// Where captured audio comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaMode {
    /// The default input device via cpal/ALSA, played back on the default
    /// output device. A device that cannot be opened degrades to silence
    /// (logged once) — the plane still negotiates.
    Device,
    /// Synthetic 440 Hz capture and counted playout. The dogfood's mode
    /// (`HUSH_VOICE_TEST_TONE=1`): the two-daemon verify needs no microphone,
    /// and "audio crossed" is a counter the script can assert on.
    Tone,
}

/// What the plane has done, for the dogfood and the log.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VoiceStats {
    /// Encoded frames written to at least one live peer connection.
    pub sent_frames: usize,
    /// Opus packets decoded from any peer.
    pub decoded_frames: usize,
    /// Frames the playout clock consumed (device callback or, in tone mode,
    /// the counting ticker).
    pub played_frames: usize,
    /// Peer connections currently in the `Connected` state.
    pub peers_live: usize,
}

/// One peer's connection: the outbound track and the inbound jitter buffer.
struct Link {
    connection: Option<Arc<dyn PeerConnection>>,
    track: Option<Arc<TrackLocalStaticSample>>,
    /// Decoded inbound frames, shared between the `on_track` pump (producer)
    /// and the playout clock (consumer).
    jitter: Arc<Mutex<JitterBuffer>>,
    /// The connection reached `Connected`; audio may be written to it.
    live: bool,
}

struct Inner {
    stun: Option<String>,
    turn: Option<TurnConfig>,
    mode: MediaMode,
    links: Mutex<HashMap<String, Link>>,
    muted: AtomicBool,
    sent: AtomicUsize,
    decoded: AtomicUsize,
    played: AtomicUsize,
    /// The capture and playout loops are started once, on the first link.
    loops_started: OnceLock<()>,
    /// The device queues in `Device` mode, shared between the device thread
    /// (which owns the `cpal` streams — they are `!Send`, so the streams live
    /// only on that thread), the capture ticker, and the output callback.
    /// `None` means the devices would not open and the plane degrades to a
    /// silent capture side with a counting playout. Swapped whole by
    /// `reopen_devices` — the old thread exits on its own `alive` flag.
    device: Mutex<Option<Arc<DeviceAudio>>>,
    /// Which devices the thread should prefer. Replaced by a selection change.
    selector: Mutex<DeviceSelector>,
    /// Until when a speaker test tone outranks the room mix.
    test_tone_until: Mutex<Option<Instant>>,
    /// Tone phase, so a plane that dials twice keeps a continuous tone.
    tone_phase: Mutex<f64>,
}

impl Inner {
    fn stats(&self) -> VoiceStats {
        VoiceStats {
            sent_frames: self.sent.load(Ordering::SeqCst),
            decoded_frames: self.decoded.load(Ordering::SeqCst),
            played_frames: self.played.load(Ordering::SeqCst),
            peers_live: self.lock().values().filter(|l| l.live).count(),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Link>> {
        self.links.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The current device queues, when a device thread has them.
    fn device(&self) -> Option<Arc<DeviceAudio>> {
        self.device
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    fn begin(&self, peer: &str) {
        self.lock().insert(
            peer.to_string(),
            Link {
                connection: None,
                track: None,
                jitter: Arc::new(Mutex::new(JitterBuffer::new(2, 8))),
                live: false,
            },
        );
    }

    fn set_connection(&self, peer: &str, connection: Arc<dyn PeerConnection>) {
        if let Some(link) = self.lock().get_mut(peer) {
            link.connection = Some(connection);
        }
    }

    fn set_track(&self, peer: &str, track: Arc<TrackLocalStaticSample>) {
        if let Some(link) = self.lock().get_mut(peer) {
            link.track = Some(track);
        }
    }

    fn set_live(&self, peer: &str, live: bool) {
        if let Some(link) = self.lock().get_mut(peer) {
            link.live = live;
        }
    }

    fn start_loops(self: &Arc<Self>) {
        self.loops_started.get_or_init(|| {
            let inner = Arc::clone(self);
            runtime().spawn(Box::pin(async move {
                capture_loop(inner).await;
            }));
            // In device mode the output device's own callback is the primary
            // playout clock; the counting ticker runs only while the output
            // device is not draining (no device, or it refused to open).
            let inner = Arc::clone(self);
            runtime().spawn(Box::pin(async move {
                playout_loop(inner).await;
            }));
        });
    }
}

/// The device queues shared between the dedicated device thread (which owns
/// the `cpal` streams), the capture ticker, and the output callback.
struct DeviceAudio {
    /// Raw interleaved samples off the input callback, oldest first.
    pending: Mutex<Vec<f32>>,
    /// 48 kHz mono i16 samples converted but not yet a full 20 ms frame.
    staging: Mutex<Vec<i16>>,
    /// The rate and channel count of the input config actually opened,
    /// written by the device thread. (These were once plain fields that the
    /// comment claimed the thread wrote and it never did — a 44.1 kHz mic
    /// was resampled as if it ran at 48 kHz and came out garbled.)
    in_rate: AtomicU32,
    in_channels: AtomicU16,
    /// The output callback's position inside the frame it is draining.
    cursor: Mutex<PlayoutCursor>,
    /// False once the plane is shutting down (or being replaced by a
    /// reopen): the device thread exits and drops the streams.
    alive: Arc<AtomicBool>,
    /// Whether the output device's callback is the playout clock. False means
    /// playout stays on the counting ticker.
    output_live: AtomicBool,
    /// Set when the input side opened: the smoke test's signal.
    input_live: AtomicBool,
    /// The device names actually picked by the device thread — `None` until
    /// it has made its pick. What the health endpoint reports so a requested
    /// name that fell back to the default is visible.
    in_name: Mutex<Option<String>>,
    out_name: Mutex<Option<String>>,
    /// The last stream error text, for the health panel. A degraded device
    /// must be able to say why, not just show silent LEDs.
    last_error: Mutex<Option<String>>,
    /// Peak |sample| of the most recent input callback, as `f32` bits. The
    /// Settings meter's source — live even while muted, because muted means
    /// "not sent", not "not hearing".
    input_level: AtomicU32,
}

impl DeviceAudio {
    fn new() -> Self {
        Self {
            pending: Mutex::new(Vec::new()),
            staging: Mutex::new(Vec::new()),
            // Defaults for a tap whose thread has not opened yet; the real
            // rates are written from the config actually opened.
            in_rate: AtomicU32::new(hush_audio::SAMPLE_RATE as u32),
            in_channels: AtomicU16::new(1),
            cursor: Mutex::new(PlayoutCursor::default()),
            alive: Arc::new(AtomicBool::new(true)),
            output_live: AtomicBool::new(false),
            input_live: AtomicBool::new(false),
            in_name: Mutex::new(None),
            out_name: Mutex::new(None),
            last_error: Mutex::new(None),
            input_level: AtomicU32::new(0),
        }
    }

    /// The next 20 ms wire frame from the device, or `None` when not enough
    /// audio has arrived yet (the ticker skips the tick and keeps cadence).
    fn next_frame(&self) -> Option<[i16; hush_audio::FRAME_SAMPLES]> {
        let raw: Vec<f32> =
            std::mem::take(&mut *self.pending.lock().unwrap_or_else(|e| e.into_inner()));
        let converted = to_i16(&resample(
            &raw,
            self.in_rate.load(Ordering::SeqCst),
            self.in_channels.load(Ordering::SeqCst),
        ));
        let mut staging = self.staging.lock().unwrap_or_else(|e| e.into_inner());
        staging.extend_from_slice(&converted);
        if staging.len() < hush_audio::FRAME_SAMPLES {
            return None;
        }
        let frame: Vec<i16> = staging.drain(..hush_audio::FRAME_SAMPLES).collect();
        let mut out = [0i16; hush_audio::FRAME_SAMPLES];
        out.copy_from_slice(&frame);
        Some(out)
    }
}

/// Spawn the thread that owns the device streams. `cpal::Stream` is `!Send`,
/// so the streams live only here: the thread opens them, runs until
/// `alive` goes false, and drops them on its way out.
fn spawn_device_thread(inner: Arc<Inner>) {
    let result = std::thread::Builder::new()
        .name("voice-device".into())
        .spawn(move || device_thread(inner));
    if let Err(e) = result {
        eprintln!("hush-daemon: voice: could not spawn the device thread: {e}");
    }
}

fn device_thread(inner: Arc<Inner>) {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    use cpal::Sample as _;

    let audio = match inner.device() {
        Some(audio) => audio,
        None => return,
    };
    let host = cpal::default_host();
    let selector = inner
        .selector
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();

    // Pick by name when a device is selected, falling back to the OS default
    // when the name is absent or the system no longer has it. What actually
    // opened is recorded on the queues, so the health endpoint can compare
    // requested against resolved.
    let pick = |want: Option<&str>, output: bool| -> Option<cpal::Device> {
        let by_name = |d: &cpal::Device| {
            d.name()
                .ok()
                .is_some_and(|n| want.is_some_and(|w| n.trim().eq_ignore_ascii_case(w.trim())))
        };
        if let Some(want) = want {
            let found = if output {
                host.output_devices()
                    .ok()
                    .and_then(|mut ds| ds.find(by_name))
            } else {
                host.input_devices()
                    .ok()
                    .and_then(|mut ds| ds.find(by_name))
            };
            if found.is_some() {
                return found;
            }
            eprintln!(
                "hush-daemon: voice: the selected {} device {want:?} is not \
                 available; using the default",
                if output { "output" } else { "input" }
            );
        }
        if output {
            host.default_output_device()
        } else {
            host.default_input_device()
        }
    };

    let input_device = pick(selector.input.as_deref(), false);
    let output_device = pick(selector.output.as_deref(), true);
    *audio.in_name.lock().unwrap_or_else(|e| e.into_inner()) =
        input_device.as_ref().and_then(|d| d.name().ok());
    *audio.out_name.lock().unwrap_or_else(|e| e.into_inner()) =
        output_device.as_ref().and_then(|d| d.name().ok());
    let Some(input_device) = input_device else {
        eprintln!("hush-daemon: voice: no default input device; capture is silent");
        return;
    };

    let Ok(in_config) = input_device.default_input_config() else {
        eprintln!("hush-daemon: voice: the input device has no default config");
        return;
    };
    // The rates the conversion actually must assume — written now that the
    // config is in hand (this write used to be promised and never made).
    audio
        .in_rate
        .store(in_config.sample_rate().0, Ordering::SeqCst);
    audio
        .in_channels
        .store(in_config.channels(), Ordering::SeqCst);
    audio.input_live.store(true, Ordering::SeqCst);

    let complained = Arc::new(AtomicBool::new(false));
    let stream_err = {
        let complained = Arc::clone(&complained);
        let audio = Arc::clone(&audio);
        move |err: cpal::StreamError| {
            *audio.last_error.lock().unwrap_or_else(|e| e.into_inner()) = Some(err.to_string());
            if !complained.swap(true, Ordering::SeqCst) {
                eprintln!("hush-daemon: voice: device stream error: {err}");
            }
        }
    };

    // Input: whatever the device offers (f32 or i16), queued raw; the capture
    // ticker converts to the wire contract. The queue is bounded at a second
    // of samples: a ticker that cannot keep up drops the oldest. The peak of
    // every callback is kept for the Settings meter.
    let bound = in_config.sample_rate().0 as usize * in_config.channels() as usize;
    let queue = move |audio: &Arc<DeviceAudio>, data: &[f32]| {
        let peak = data.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        audio.input_level.store(peak.to_bits(), Ordering::SeqCst);
        let mut q = audio.pending.lock().unwrap_or_else(|e| e.into_inner());
        q.extend_from_slice(data);
        if q.len() > bound {
            let cut = q.len() - bound;
            q.drain(0..cut);
        }
    };
    let in_stream = match in_config.sample_format() {
        cpal::SampleFormat::F32 => {
            let audio = Arc::clone(&audio);
            input_device
                .build_input_stream(
                    &in_config.clone().into(),
                    move |data: &[f32], _| {
                        queue(&audio, data);
                    },
                    stream_err.clone(),
                    None,
                )
                .ok()
        }
        cpal::SampleFormat::I16 => {
            let audio = Arc::clone(&audio);
            input_device
                .build_input_stream(
                    &in_config.clone().into(),
                    move |data: &[i16], _| {
                        let f32s: Vec<f32> = data.iter().map(|s| s.to_sample::<f32>()).collect();
                        queue(&audio, &f32s);
                    },
                    stream_err.clone(),
                    None,
                )
                .ok()
        }
        other => {
            eprintln!(
                "hush-daemon: voice: unsupported input sample format {other:?}; \
                 capture is silent"
            );
            None
        }
    };
    let Some(_in_stream) = in_stream else {
        eprintln!("hush-daemon: voice: the input stream would not open; capture is silent");
        return;
    };
    let _ = input_device.name();

    // Output: the device's own clock pulling the mix out of the jitter
    // buffers. Preferred at the device's default config when it already is
    // the wire rate; a device reporting another rate (HDA analog commonly
    // defaults to 44.1 kHz) is retried with 48 kHz requested explicitly —
    // the audio server resamples, the mix is already 48 kHz. Only when both
    // opens fail does playout fall back to the counting ticker.
    let build_out_stream = |config: cpal::SupportedStreamConfig| -> Option<cpal::Stream> {
        let out_channels = config.channels() as usize;
        let device_audio = Arc::clone(&audio);
        let inner = Arc::clone(&inner);
        output_device
            .as_ref()?
            .build_output_stream(
                &config.into(),
                move |data: &mut [f32], _| {
                    let mut cursor = device_audio
                        .cursor
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    for chunk in data.chunks_mut(out_channels) {
                        if cursor.pos >= cursor.frame.len() {
                            cursor.frame = mix_one_frame(&inner);
                            cursor.pos = 0;
                        }
                        let sample = cursor
                            .frame
                            .get(cursor.pos)
                            .map(|s| *s as f32 / 32_768.0)
                            .unwrap_or(0.0);
                        cursor.pos += 1;
                        for out in chunk {
                            *out = sample;
                        }
                    }
                },
                stream_err.clone(),
                None,
            )
            .ok()
    };
    let native_out = output_device
        .as_ref()
        .and_then(|d| d.default_output_config().ok());
    let at_wire_rate = native_out
        .as_ref()
        .is_some_and(|c| c.sample_rate().0 == hush_audio::SAMPLE_RATE);
    let out_config = match &native_out {
        Some(config) if at_wire_rate => Some(config.clone()),
        Some(config) => {
            eprintln!(
                "hush-daemon: voice: output default is {} Hz; requesting the \
                 wire rate {} Hz so playout is audible",
                config.sample_rate().0,
                hush_audio::SAMPLE_RATE
            );
            let forced = cpal::SupportedStreamConfig::new(
                config.channels(),
                cpal::SampleRate(hush_audio::SAMPLE_RATE),
                config.buffer_size().clone(),
                config.sample_format(),
            );
            Some(forced)
        }
        None => None,
    };
    let out_stream: Option<cpal::Stream> = match out_config {
        Some(config) => build_out_stream(config),
        None => {
            eprintln!(
                "hush-daemon: voice: no usable output device; playout is \
                 counted, not audible"
            );
            None
        }
    };
    match out_stream {
        Some(stream) => {
            audio.output_live.store(true, Ordering::SeqCst);
            let _ = stream.play();
            // Held by the match arm's binding for the thread's lifetime.
            std::mem::forget(stream);
        }
        None => {}
    }

    // The input stream is held here, on the thread that created it, for as
    // long as the plane lives.
    let _ = _in_stream.play();
    while audio.alive.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The output callback's position inside the frame it is draining.
#[derive(Default)]
struct PlayoutCursor {
    frame: Vec<i16>,
    pos: usize,
}

/// One mixed output frame across all peers' jitter buffers: sum, clamp.
/// `played` counts each frame actually popped. A running speaker test
/// outranks the mix: the test tone must be audible alone, and one test
/// window (~2 s) of a room not being on the speaker is the honest cost.
fn mix_one_frame(inner: &Arc<Inner>) -> Vec<i16> {
    if let Some(until) = inner
        .test_tone_until
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
    {
        if Instant::now() < *until {
            let mut pcm = vec![0i16; hush_audio::FRAME_SAMPLES];
            let mut phase = inner.tone_phase.lock().unwrap_or_else(|e| e.into_inner());
            fill_tone(&mut pcm, &mut phase);
            return pcm;
        }
    }
    let jitters: Vec<Arc<Mutex<JitterBuffer>>> = inner
        .lock()
        .values()
        .map(|link| Arc::clone(&link.jitter))
        .collect();
    let mut mixed = vec![0i64; hush_audio::FRAME_SAMPLES];
    let mut any = false;
    for jb in jitters {
        if let Some(frame) = jb.lock().unwrap_or_else(|e| e.into_inner()).pop() {
            inner.played.fetch_add(1, Ordering::SeqCst);
            any = true;
            for (dst, src) in mixed.iter_mut().zip(frame.iter()) {
                *dst += *src as i64;
            }
        }
    }
    if !any {
        return Vec::new();
    }
    mixed
        .iter()
        .map(|s| (*s).clamp(i16::MIN as i64, i16::MAX as i64) as i16)
        .collect()
}

/// The capture/encode/write ticker: every 20 ms, one frame — tone or device —
/// encoded once and written to every live peer's track.
async fn capture_loop(inner: Arc<Inner>) {
    let mut enc = match Encoder::new() {
        Some(enc) => enc,
        None => {
            eprintln!("hush-daemon: voice: no Opus encoder; capture is silent");
            return;
        }
    };
    // A periodic one-line stats report while the plane has connections —
    // what the dogfood asserts on, since audio itself leaves no trace.
    let mut since_report = 0u32;
    const FRAMES_PER_REPORT: u32 = 100; // 2 s of audio
    loop {
        runtime().sleep(FRAME_TICK).await;
        since_report += 1;
        if since_report >= FRAMES_PER_REPORT {
            since_report = 0;
            let stats = inner.stats();
            if stats.peers_live > 0 {
                eprintln!(
                    "hush-daemon: voice stats sent={} decoded={} played={} live={}",
                    stats.sent_frames, stats.decoded_frames, stats.played_frames, stats.peers_live
                );
            }
        }
        if inner.muted.load(Ordering::SeqCst) {
            // Muted: capture stops. The flag rides the control plane; nothing
            // is encoded or written while it is set.
            continue;
        }
        let frame = match inner.mode {
            MediaMode::Tone => {
                let mut pcm = [0i16; hush_audio::FRAME_SAMPLES];
                let mut phase = inner.tone_phase.lock().unwrap_or_else(|e| e.into_inner());
                fill_tone(&mut pcm, &mut phase);
                pcm
            }
            MediaMode::Device => {
                let device = inner.device();
                match device.as_ref().and_then(|d| d.next_frame()) {
                    Some(frame) => frame,
                    // Not enough device audio yet: skip the tick rather than
                    // skew the frames.
                    None => continue,
                }
            }
        };
        let packet = enc.encode(&frame);
        // Snapshot the live tracks under a brief lock and write outside it —
        // a std mutex never crosses an await here (the paper's rule).
        let live: Vec<(String, Arc<TrackLocalStaticSample>)> = inner
            .lock()
            .iter()
            .filter(|(_, link)| link.live)
            .filter_map(|(peer, link)| link.track.clone().map(|t| (peer.clone(), t)))
            .collect();
        for (peer, track) in live {
            let writer = track.sample_writer(SSRC, PT_OPUS);
            let sample = Sample {
                data: packet.clone().into(),
                timestamp: Instant::now(),
                duration: FRAME_TICK,
                packet_timestamp: 0,
                prev_dropped_packets: 0,
                prev_padding_packets: 0,
            };
            if writer.write_sample(&sample).await.is_ok() {
                inner.sent.fetch_add(1, Ordering::SeqCst);
            } else {
                eprintln!("hush-daemon: voice: a sample write to {peer} failed");
            }
        }
    }
}

/// The playout ticker (tone mode, or a plane whose output device did not
/// open): the counting stand-in for the device clock — one frame per peer per
/// tick, consumed and counted. While the device callback drains (`output_live`),
/// it stays out of the way.
async fn playout_loop(inner: Arc<Inner>) {
    loop {
        runtime().sleep(FRAME_TICK).await;
        if let Some(audio) = inner.device() {
            if audio.output_live.load(Ordering::SeqCst) {
                continue;
            }
        }
        for link in inner.lock().values() {
            if link
                .jitter
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .pop()
                .is_some()
            {
                inner.played.fetch_add(1, Ordering::SeqCst);
            }
        }
    }
}

/// Peer-connection events.
struct Handler {
    peer: String,
    inner: Arc<Inner>,
    gathered: Sender<()>,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete {
            let _ = self.gathered.try_send(());
        }
    }

    /// The peer's audio arrives here: decode each RTP payload and queue it
    /// for playout. The pump runs for the connection's lifetime.
    async fn on_track(&self, track: Arc<dyn TrackRemote>) {
        let inner = Arc::clone(&self.inner);
        let peer = self.peer.clone();
        runtime().spawn(Box::pin(async move {
            let mut dec = match Decoder::new() {
                Some(dec) => dec,
                None => {
                    eprintln!("hush-daemon: voice: no Opus decoder; inbound audio dropped");
                    return;
                }
            };
            while let Some(event) = track.poll().await {
                match event {
                    TrackRemoteEvent::OnRtpPacket(pkt) => {
                        let frame = dec.decode(&pkt.payload);
                        if let Some(link) = inner.lock().get(&peer) {
                            link.jitter
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .push(frame);
                        }
                        inner.decoded.fetch_add(1, Ordering::SeqCst);
                    }
                    TrackRemoteEvent::OnEnded => break,
                    _ => {}
                }
            }
        }));
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        match state {
            RTCPeerConnectionState::Connected => self.inner.set_live(&self.peer, true),
            RTCPeerConnectionState::Failed
            | RTCPeerConnectionState::Disconnected
            | RTCPeerConnectionState::Closed => self.inner.set_live(&self.peer, false),
            _ => {}
        }
    }
}

/// Await `future`, giving up after `duration` instead of hanging the caller.
async fn within<T>(duration: Duration, future: impl Future<Output = T>) -> Option<T> {
    webrtc::runtime::timeout(&*runtime(), duration, future)
        .await
        .ok()
}

/// A voice session's media plane: one peer connection per joined peer, audio
/// both ways.
pub struct VoicePlane {
    inner: Arc<Inner>,
}

/// Which devices a plane's device thread should prefer, by name as cpal
/// reports it. `None` means the OS default. A name the system no longer has
/// falls back to the default — what actually opened is recorded on the
/// device, so the health endpoint can show the mismatch.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DeviceSelector {
    pub input: Option<String>,
    pub output: Option<String>,
}

/// One device side as the health panel reports it.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct DeviceHealth {
    /// The device the thread picked, by name. `None` until picked (or when
    /// the system has none).
    pub input_name: Option<String>,
    pub output_name: Option<String>,
    pub input_live: bool,
    pub output_live: bool,
    /// The input config actually opened — what the conversion assumes.
    pub in_rate: u32,
    pub in_channels: u16,
    /// Peak |sample| of the latest input callback (0.0..1.0).
    pub input_level: f32,
    /// The last device stream error, when there was one.
    pub last_error: Option<String>,
}

/// The device names this machine offers cpal, split by direction.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct DeviceList {
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
}

/// Enumerate the system's audio devices. A device may serve either direction
/// or both; direction is what its default config says. Names are deduped in
/// enumeration order (ALSA and friends can list the same card twice).
pub fn list_devices() -> DeviceList {
    use cpal::traits::{DeviceTrait, HostTrait};
    let host = cpal::default_host();
    let mut list = DeviceList::default();
    let Ok(devices) = host.devices() else {
        return list;
    };
    for d in devices {
        if let Ok(name) = d.name() {
            if d.default_input_config().is_ok() && !list.inputs.contains(&name) {
                list.inputs.push(name.clone());
            }
            if d.default_output_config().is_ok() && !list.outputs.contains(&name) {
                list.outputs.push(name);
            }
        }
    }
    list
}

impl VoicePlane {
    /// A plane with no connections yet. `stun` is the operator's STUN server
    /// when they run one — the same `resolve_stun_url` value the Paper's
    /// plane reads; with `None`, peers gather host candidates only.
    ///
    /// The daemon's own planes always carry a selector (`with_selector`);
    /// this default-selector form is the tests' convenience.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn new(stun: Option<String>, mode: MediaMode) -> Self {
        Self::with_selector(stun, None, mode, DeviceSelector::default())
    }

    /// A plane with a device preference — what the daemon builds from the
    /// voice device names on file in the config.
    pub fn with_selector(
        stun: Option<String>,
        turn: Option<TurnConfig>,
        mode: MediaMode,
        selector: DeviceSelector,
    ) -> Self {
        let inner = Arc::new(Inner {
            stun,
            turn,
            mode,
            links: Mutex::new(HashMap::new()),
            muted: AtomicBool::new(false),
            sent: AtomicUsize::new(0),
            decoded: AtomicUsize::new(0),
            played: AtomicUsize::new(0),
            loops_started: OnceLock::new(),
            device: Mutex::new(None),
            selector: Mutex::new(selector),
            test_tone_until: Mutex::new(None),
            tone_phase: Mutex::new(0.0),
        });
        if mode == MediaMode::Device {
            *inner.device.lock().unwrap_or_else(|e| e.into_inner()) =
                Some(Arc::new(DeviceAudio::new()));
            spawn_device_thread(Arc::clone(&inner));
        }
        Self { inner }
    }

    /// Local mute: capture stops before the flag fans out (the caller sends
    /// `VoiceMute`; this only silences the wire).
    pub fn set_muted(&self, muted: bool) {
        self.inner.muted.store(muted, Ordering::SeqCst);
    }

    /// Counters and liveness for the dogfood and the log.
    pub fn stats(&self) -> VoiceStats {
        self.inner.stats()
    }

    /// Play a test tone on the output device for `ms`, so Settings can prove
    /// the speaker works without a live call. Returns whether the tone will
    /// actually be audible — a plane whose output device did not open cannot
    /// play anything, and that is the answer the panel needs, not a lie.
    /// While the tone plays it outranks the room mix.
    pub fn play_test_tone(&self, ms: u64) -> bool {
        let audible = self
            .inner
            .device()
            .is_some_and(|d| d.output_live.load(Ordering::SeqCst));
        if audible {
            *self
                .inner
                .test_tone_until
                .lock()
                .unwrap_or_else(|e| e.into_inner()) =
                Some(Instant::now() + Duration::from_millis(ms));
        }
        audible
    }

    /// What the device thread actually opened, when this plane runs on
    /// devices. `None` in tone mode (there are no devices to report).
    pub fn device_health(&self) -> Option<DeviceHealth> {
        let audio = self.inner.device()?;
        // Guards are dropped into locals before the struct is built: a
        // MutexGuard must not outlive its statement.
        let in_name = audio
            .in_name
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let out_name = audio
            .out_name
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let last_error = audio
            .last_error
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        Some(DeviceHealth {
            input_name: in_name,
            output_name: out_name,
            input_live: audio.input_live.load(Ordering::SeqCst),
            output_live: audio.output_live.load(Ordering::SeqCst),
            in_rate: audio.in_rate.load(Ordering::SeqCst),
            in_channels: audio.in_channels.load(Ordering::SeqCst),
            input_level: f32::from_bits(audio.input_level.load(Ordering::SeqCst)),
            last_error,
        })
    }

    /// Swap the device thread for one honoring `selector` — what a selection
    /// change calls on live planes and on the probe. The old thread exits on
    /// its own `alive` flag within a tick; a dial or handshake running
    /// through the swap is untouched (only the queues move).
    pub fn reopen_devices(&self, selector: DeviceSelector) {
        if self.inner.mode != MediaMode::Device {
            return;
        }
        *self
            .inner
            .selector
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = selector;
        if let Some(old) = self
            .inner
            .device
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            old.alive.store(false, Ordering::SeqCst);
        }
        *self.inner.device.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(Arc::new(DeviceAudio::new()));
        spawn_device_thread(Arc::clone(&self.inner));
    }

    /// Dial `peer`: build a connection with an outbound track, gather ICE,
    /// and return the offer for the control plane to send.
    pub async fn offer(&self, peer: &str) -> Result<String, String> {
        self.inner.forget(peer);
        self.inner.begin(peer);
        self.inner.start_loops();
        let (connection, mut gathered) = self.build(peer).await?;
        self.inner.set_connection(peer, Arc::clone(&connection));

        let track = self.add_send_track(&connection).await?;
        self.inner.set_track(peer, track);

        let offer = connection
            .create_offer(None)
            .await
            .map_err(|e| format!("could not build an offer: {e}"))?;
        connection
            .set_local_description(offer)
            .await
            .map_err(|e| format!("could not accept our own offer: {e}"))?;
        let description = gathered_description(&connection, &mut gathered).await?;
        serde_json::to_string(&description).map_err(|e| format!("offer is unencodable: {e}"))
    }

    /// Answer `peer`'s offer, returning the answer for the control plane.
    /// The answerer carries an outbound track too — both halves of the pair
    /// send.
    pub async fn answer(&self, peer: &str, offer: &str) -> Result<String, String> {
        let description: webrtc::peer_connection::RTCSessionDescription =
            serde_json::from_str(offer).map_err(|e| format!("the offer is unreadable: {e}"))?;
        self.inner.forget(peer);
        self.inner.begin(peer);
        self.inner.start_loops();
        let (connection, mut gathered) = self.build(peer).await?;
        self.inner.set_connection(peer, Arc::clone(&connection));

        let track = self.add_send_track(&connection).await?;
        self.inner.set_track(peer, track);

        connection
            .set_remote_description(description)
            .await
            .map_err(|e| format!("the offer was refused: {e}"))?;
        let answer = connection
            .create_answer(None)
            .await
            .map_err(|e| format!("could not build an answer: {e}"))?;
        connection
            .set_local_description(answer)
            .await
            .map_err(|e| format!("could not accept our own answer: {e}"))?;
        let description = gathered_description(&connection, &mut gathered).await?;
        serde_json::to_string(&description).map_err(|e| format!("answer is unencodable: {e}"))
    }

    /// Finish the handshake with `peer`'s answer.
    pub async fn accept_answer(&self, peer: &str, answer: &str) -> Result<(), String> {
        let description: webrtc::peer_connection::RTCSessionDescription =
            serde_json::from_str(answer).map_err(|e| format!("the answer is unreadable: {e}"))?;
        let connection = self
            .inner
            .lock()
            .get(peer)
            .and_then(|l| l.connection.clone())
            .ok_or_else(|| format!("no outstanding offer for {peer}"))?;
        connection
            .set_remote_description(description)
            .await
            .map_err(|e| format!("the answer was refused: {e}"))
    }

    /// Drop `peer`: close the connection and forget the link.
    pub fn forget(&self, peer: &str) {
        self.inner.forget(peer);
    }

    /// Close every connection. The plane is unusable afterwards.
    pub fn shutdown(&self) {
        // The device thread exits first, so no callback races a link being
        // torn down mid-mix.
        if let Some(device) = self.inner.device() {
            device.alive.store(false, Ordering::SeqCst);
        }
        let peers: Vec<String> = self.inner.lock().keys().cloned().collect();
        for peer in peers {
            self.inner.forget(&peer);
        }
    }

    /// The outbound audio track for one connection: an Opus-carrying static
    /// sample track, added before the offer/answer so the SDP carries the
    /// m-line (the spike's shape).
    async fn add_send_track(
        &self,
        connection: &Arc<dyn PeerConnection>,
    ) -> Result<Arc<TrackLocalStaticSample>, String> {
        let opus_codec = RTCRtpCodec {
            mime_type: "audio/opus".to_owned(),
            clock_rate: 48_000,
            channels: 2,
            sdp_fmtp_line: "minptime=10;useinbandfec=1".to_owned(),
            rtcp_feedback: vec![],
        };
        let track = Arc::new(
            TrackLocalStaticSample::new(
                Instant::now(),
                MediaStreamTrack::new(
                    "voice-stream".to_owned(),
                    "voice-track".to_owned(),
                    "hush-daemon".to_owned(),
                    RtpCodecKind::Audio,
                    vec![RTCRtpEncodingParameters {
                        rtp_coding_parameters: RTCRtpCodingParameters {
                            ssrc: Some(SSRC),
                            ..Default::default()
                        },
                        codec: opus_codec,
                        ..Default::default()
                    }],
                ),
            )
            .map_err(|e| format!("could not build the send track: {e}"))?,
        );
        connection
            .add_track(track.clone() as Arc<dyn TrackLocal>)
            .await
            .map_err(|e| format!("could not add the send track: {e}"))?;
        Ok(track)
    }

    /// Build one peer connection with the Opus media engine (the spike's
    /// lesson: the answerer needs the registered codecs too, or the answer
    /// comes out unusable) and the receiver its gathering signals arrive on.
    async fn build(&self, peer: &str) -> Result<(Arc<dyn PeerConnection>, Receiver<()>), String> {
        let (gathered_tx, gathered_rx) = channel::<()>(1);
        let handler = Arc::new(Handler {
            peer: peer.to_string(),
            inner: Arc::clone(&self.inner),
            gathered: gathered_tx,
        });
        let mut media_engine = MediaEngine::default();
        media_engine
            .register_default_codecs()
            .map_err(|e| format!("no codec registry: {e}"))?;
        // Chromium (the phone's and a browser's WebView) wants the DTLS
        // fingerprint beside the ICE credentials in the media section, not at
        // the session level. A session-level fingerprint is legal but some
        // Chromium builds refuse to wire the DTLS transport without a
        // media-level copy, which is why the desktop-as-answerer call stuck on
        // "Connecting" and never went live.
        let setting_engine = SettingEngineBuilder::new()
            .with_sdp_media_level_fingerprints(true)
            .build();
        let mut builder = PeerConnectionBuilder::new()
            .with_handler(handler)
            .with_runtime(runtime())
            .with_media_engine(media_engine)
            .with_setting_engine(setting_engine)
            // Bind every interface: a LAN peer has to be reachable on the
            // interface the two machines actually share, not just loopback.
            .with_udp_addrs(vec!["0.0.0.0:0".to_string()]);
        // STUN discovers the peer's address; TURN relays media for the peers
        // that cannot connect peer-to-peer (symmetric NAT, client-isolating
        // firewalls). Both default to the operator's own coturn; a plane with
        // neither gathers host candidates only, which is the loopback/LAN test
        // posture.
        let mut ice_servers: Vec<RTCIceServer> = Vec::new();
        if let Some(stun) = self.inner.stun.as_ref() {
            ice_servers.push(RTCIceServer {
                urls: vec![stun.clone()],
                ..Default::default()
            });
        }
        if let Some(turn) = self.inner.turn.as_ref() {
            ice_servers.push(RTCIceServer {
                urls: turn.urls.clone(),
                username: turn.username.clone(),
                credential: turn.credential.clone(),
                ..Default::default()
            });
        }
        if !ice_servers.is_empty() {
            builder = builder.with_configuration(
                webrtc::peer_connection::RTCConfigurationBuilder::new()
                    .with_ice_servers(ice_servers)
                    .build(),
            );
        }
        let connection = builder
            .build()
            .await
            .map_err(|e| format!("could not open a peer connection: {e}"))?;
        Ok((Arc::new(connection), gathered_rx))
    }
}

impl Inner {
    /// Drop a peer: close its connection and forget it. The head of a fresh
    /// handshake too, so a renegotiation does not leave the old link behind.
    fn forget(&self, peer: &str) {
        let link = self.lock().remove(peer);
        if let Some(link) = link {
            if let Some(connection) = link.connection {
                runtime().spawn(Box::pin(async move {
                    let _ = connection.close().await;
                }));
            }
        }
    }
}

/// Wait for gathering to finish and return the description carrying the
/// candidates, so the handshake is one round trip.
async fn gathered_description(
    connection: &Arc<dyn PeerConnection>,
    gathered: &mut Receiver<()>,
) -> Result<webrtc::peer_connection::RTCSessionDescription, String> {
    if within(HANDSHAKE_DEADLINE, gathered.recv()).await.is_none() {
        return Err("gathering ICE candidates timed out".to_string());
    }
    connection
        .local_description()
        .await
        .ok_or_else(|| "no local description after gathering".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;

    /// Drive a future to completion on the data-plane runtime (the
    /// `paper_data` tests' helper).
    fn block_on<F: Future>(future: F) -> F::Output {
        let mut out: Option<F::Output> = None;
        {
            let slot = &mut out;
            runtime().block_on(Box::pin(async move {
                *slot = Some(future.await);
            }));
        }
        out.expect("the future ran to completion")
    }

    /// Wait until `predicate` holds over the plane's stats.
    async fn wait_until(plane: &VoicePlane, predicate: impl Fn(&VoiceStats) -> bool) -> bool {
        for _ in 0..200 {
            if predicate(&plane.stats()) {
                return true;
            }
            runtime().sleep(Duration::from_millis(25)).await;
        }
        false
    }

    /// The point of the plane: two planes exchange real Opus audio on
    /// loopback — A's tone arrives decoded at B, B's at A, and the playout
    /// clock consumes it.
    #[test]
    fn two_planes_carry_audio_on_loopback() {
        block_on(async {
            let a = VoicePlane::new(None, MediaMode::Tone);
            let b = VoicePlane::new(None, MediaMode::Tone);

            let offer = a.offer("b").await.expect("A offers");
            assert!(
                offer.contains("m=audio"),
                "the offer carries an audio m-line"
            );
            let answer = b.answer("a", &offer).await.expect("B answers");
            assert!(
                answer.contains("m=audio"),
                "the answer carries an audio m-line"
            );
            a.accept_answer("b", &answer)
                .await
                .expect("A takes the answer");

            assert!(
                wait_until(&a, |s| s.peers_live == 1).await,
                "A's connection must reach Connected"
            );
            assert!(
                wait_until(&b, |s| s.peers_live == 1).await,
                "B's connection must reach Connected"
            );

            // Both directions cross: each side decodes the other's tone and
            // its playout clock consumes the frames.
            assert!(
                wait_until(&a, |s| s.decoded_frames > 10 && s.played_frames > 10).await,
                "A must receive and play B's audio: {:?}",
                a.stats()
            );
            assert!(
                wait_until(&b, |s| s.decoded_frames > 10 && s.played_frames > 10).await,
                "B must receive and play A's audio: {:?}",
                b.stats()
            );

            a.shutdown();
            b.shutdown();
        });
    }

    /// The phone's and a browser's WebView (Chromium) will not wire the DTLS
    /// transport unless the `a=fingerprint` line sits beside the ICE
    /// credentials in the media section. A session-level fingerprint parses but
    /// leaves the desktop-as-answerer call stuck on "Connecting" — the bug this
    /// asserts against. Both directions share [`build`], so assert both.
    #[test]
    fn fingerprints_are_media_level_for_chromium() {
        fn sdp_of(serialized: &str) -> String {
            serde_json::from_str::<serde_json::Value>(serialized)
                .ok()
                .and_then(|v| v.get("sdp").and_then(|s| s.as_str()).map(str::to_string))
                .expect("the description carries its SDP")
        }

        block_on(async {
            let a = VoicePlane::new(None, MediaMode::Tone);
            let b = VoicePlane::new(None, MediaMode::Tone);
            let offer = a.offer("b").await.expect("A offers");
            let answer = b.answer("a", &offer).await.expect("B answers");

            for (label, sdp) in [("offer", sdp_of(&offer)), ("answer", sdp_of(&answer))] {
                let audio = sdp
                    .find("m=audio")
                    .unwrap_or_else(|| panic!("{label} carries an audio m-line"));
                let fingerprint = sdp
                    .find("a=fingerprint")
                    .unwrap_or_else(|| panic!("{label} carries a fingerprint"));
                assert!(
                    fingerprint > audio,
                    "{label} must put a=fingerprint in the media section (after m=audio), \
                     not at the session level"
                );
            }

            a.shutdown();
            b.shutdown();
        });
    }

    /// Mute stops the wire audio: after the flag is set, no new frames are
    /// sent, though the peer connection itself stays up.
    #[test]
    fn muting_stops_the_sent_audio() {
        block_on(async {
            let a = VoicePlane::new(None, MediaMode::Tone);
            let b = VoicePlane::new(None, MediaMode::Tone);
            let offer = a.offer("b").await.unwrap();
            let answer = b.answer("a", &offer).await.unwrap();
            a.accept_answer("b", &answer).await.unwrap();
            assert!(wait_until(&a, |s| s.peers_live == 1).await);
            assert!(wait_until(&a, |s| s.sent_frames > 5).await);

            a.set_muted(true);
            let at_mute = a.stats().sent_frames;
            runtime().sleep(Duration::from_millis(300)).await;
            let after = a.stats().sent_frames;
            assert!(
                after - at_mute <= 2,
                "a muted plane sends at most a tick's worth of stragglers: \
                 {at_mute} then {after}"
            );

            a.shutdown();
            b.shutdown();
        });
    }

    /// A real device, when this machine has one, feeds the tap: the capture
    /// path sees samples arrive. Skipped where there is no default input
    /// device, so headless environments stay green.
    #[test]
    fn a_real_device_feeds_the_capture_tap() {
        use cpal::traits::HostTrait;
        let host = cpal::default_host();
        if host.default_input_device().is_none() {
            eprintln!("no default input device; skipping the device tap check");
            return;
        }
        // The device thread opens the streams; poll until the capture side is
        // live. A fixed sleep races a busy machine — the two loopback tests
        // run in parallel and can stretch the open (and the 44.1→48 kHz
        // re-request) well past half a second.
        let plane = VoicePlane::new(None, MediaMode::Device);
        let deadline = Instant::now() + Duration::from_secs(10);
        while plane.device_health().is_none_or(|h| !h.input_live) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
        let health = plane
            .device_health()
            .expect("a Device-mode plane always carries its device queues");
        assert!(
            health.input_live,
            "the device thread must mark the input side live: {health:?}"
        );
        // The rates the thread opened must be the rates the conversion now
        // assumes — the never-written-rates bug this replaces.
        assert!(
            health.in_rate == hush_audio::SAMPLE_RATE || health.in_rate == 44_100,
            "the opened input rate should be the device's real one: {health:?}"
        );
        // The meter plumbing: a finite peak in range, whatever the room is
        // doing (true silence may legitimately read 0.0).
        assert!(
            (0.0..=1.0).contains(&health.input_level),
            "the input meter must be a 0..1 peak: {health:?}"
        );
        plane.shutdown();
    }

    /// A selected device name the system does not have falls back to the
    /// default, and the health report shows both facts — the panel can say
    /// "you asked for X, the daemon opened Y". Skipped without devices.
    #[test]
    fn an_unknown_device_name_falls_back_and_is_visible() {
        use cpal::traits::HostTrait;
        let host = cpal::default_host();
        if host.default_input_device().is_none() {
            eprintln!("no default input device; skipping the fallback check");
            return;
        }
        let plane = VoicePlane::with_selector(
            None,
            None,
            MediaMode::Device,
            DeviceSelector {
                input: Some("not a real microphone".into()),
                output: None,
            },
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        while plane.device_health().is_none_or(|h| !h.input_live) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
        let health = plane.device_health().expect("device queues exist");
        assert!(
            health.input_live,
            "the fallback must still open: {health:?}"
        );
        assert_ne!(
            health.input_name.as_deref(),
            Some("not a real microphone"),
            "the resolved name must be the real device, not the missing one: {health:?}"
        );
        plane.shutdown();
    }

    /// A reopen honors a new selection: the old thread's queues are replaced,
    /// and the health report shows the newly resolved names. Skipped without
    /// devices (the reopen cannot get a device open without one).
    #[test]
    fn a_reopen_picks_up_the_new_selection() {
        use cpal::traits::HostTrait;
        let host = cpal::default_host();
        if host.default_input_device().is_none() {
            eprintln!("no default input device; skipping the reopen check");
            return;
        }
        let plane = VoicePlane::new(None, MediaMode::Device);
        let deadline = Instant::now() + Duration::from_secs(10);
        while plane.device_health().is_none_or(|h| !h.input_live) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
        let before = plane
            .device_health()
            .expect("the first open must have queues")
            .input_name
            .clone();
        plane.reopen_devices(DeviceSelector {
            input: Some(before.clone().unwrap_or_else(|| "unused".into())),
            output: None,
        });
        let deadline = Instant::now() + Duration::from_secs(10);
        while plane.device_health().is_none_or(|h| !h.input_live) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(25));
        }
        let health = plane.device_health().expect("the reopen must have queues");
        assert!(
            health.input_live,
            "the reopened thread must reach the input side: {health:?}"
        );
        assert_eq!(
            health.input_name, before,
            "re-selecting the device that was already open must reopen it: {health:?}"
        );
        plane.shutdown();
    }

    /// The speaker test overrides the mix: while the test window is open the
    /// mixed frame is the tone (never empty, whatever the room is doing),
    /// and after it the mix resumes — empty when no peer is sending.
    #[test]
    fn the_test_tone_overrides_the_mix() {
        block_on(async {
            let a = VoicePlane::new(None, MediaMode::Tone);
            // No audible output path in tone mode: the honest answer is no.
            assert!(!a.play_test_tone(50));
            // The override itself is device-independent; drive it directly.
            *a.inner
                .test_tone_until
                .lock()
                .unwrap_or_else(|e| e.into_inner()) =
                Some(Instant::now() + Duration::from_millis(100));
            let frame = mix_one_frame(&a.inner);
            assert!(
                frame.iter().any(|s| *s != 0),
                "a running test tone must produce a non-silent frame"
            );
            runtime().sleep(Duration::from_millis(150)).await;
            let frame = mix_one_frame(&a.inner);
            assert!(
                frame.is_empty(),
                "after the test window the mix resumes (and a quiet room mixes to silence)"
            );
            a.shutdown();
        });
    }
}
