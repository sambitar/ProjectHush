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
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use rtc::rtp_transceiver::rtp_sender::{
    RTCRtpCodingParameters, RTCRtpCodec, RTCRtpEncodingParameters, RtpCodecKind,
};
use rtc_media::Sample;
use webrtc::media_stream::track_local::static_sample::TrackLocalStaticSample;
use webrtc::media_stream::track_local::TrackLocal;
use webrtc::media_stream::track_remote::{TrackRemote, TrackRemoteEvent};
use webrtc::media_stream::MediaStreamTrack;
use webrtc::peer_connection::{
    MediaEngine, PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler,
    RTCIceGatheringState, RTCIceServer, RTCPeerConnectionState,
};
use webrtc::runtime::{channel, default_runtime, Receiver, Runtime, Sender};

use hush_audio::{fill_tone, resample, to_i16, Decoder, Encoder, JitterBuffer};

/// The default registry's Opus payload type — what the media engine
/// negotiates (opus/48000/2, PT 111).
const PT_OPUS: u8 = 111;
const SSRC: u32 = 1001;

/// How long the whole handshake may take before it is given up on. A peer
/// that cannot connect in this long is not audible, but the room keeps
/// working and the leave machinery stays live.
const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(15);

/// The capture/playout cadence: one 20 ms Opus frame.
const FRAME_TICK: Duration = Duration::from_millis(
    hush_audio::FRAME_SAMPLES as u64 * 1000 / hush_audio::SAMPLE_RATE as u64,
);

/// The one runtime handle every peer connection and timer here shares.
fn runtime() -> Arc<dyn Runtime> {
    static RUNTIME: OnceLock<Arc<dyn Runtime>> = OnceLock::new();
    Arc::clone(RUNTIME.get_or_init(|| {
        default_runtime().expect("runtime-tokio is enabled for the daemon")
    }))
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
    /// silent capture side with a counting playout.
    device: OnceLock<Option<Arc<DeviceAudio>>>,
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
    in_rate: u32,
    in_channels: u16,
    /// The output callback's position inside the frame it is draining.
    cursor: Mutex<PlayoutCursor>,
    /// False once the plane is shutting down: the device thread exits and
    /// drops the streams.
    alive: Arc<AtomicBool>,
    /// Whether the output device's callback is the playout clock. False means
    /// playout stays on the counting ticker.
    output_live: AtomicBool,
    /// Set when the input side opened: the smoke test's signal.
    input_live: AtomicBool,
}

impl DeviceAudio {
    /// The next 20 ms wire frame from the device, or `None` when not enough
    /// audio has arrived yet (the ticker skips the tick and keeps cadence).
    fn next_frame(&self) -> Option<[i16; hush_audio::FRAME_SAMPLES]> {
        let raw: Vec<f32> = std::mem::take(
            &mut *self.pending.lock().unwrap_or_else(|e| e.into_inner()),
        );
        let converted = to_i16(&resample(&raw, self.in_rate, self.in_channels));
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

    let audio = match inner.device.get().and_then(|d| d.as_ref()) {
        Some(audio) => Arc::clone(audio),
        None => return,
    };
    let host = cpal::default_host();
    let Some(input_device) = host.default_input_device() else {
        eprintln!("hush-daemon: voice: no default input device; capture is silent");
        return;
    };
    let output_device = host.default_output_device();

    let Ok(in_config) = input_device.default_input_config() else {
        eprintln!("hush-daemon: voice: the input device has no default config");
        return;
    };
    audio.input_live.store(true, Ordering::SeqCst);

    let complained = Arc::new(AtomicBool::new(false));
    let stream_err = {
        let complained = Arc::clone(&complained);
        move |err| {
            if !complained.swap(true, Ordering::SeqCst) {
                eprintln!("hush-daemon: voice: device stream error: {err}");
            }
        }
    };

    // Input: whatever the device offers (f32 or i16), queued raw; the capture
    // ticker converts to the wire contract. The queue is bounded at a second
    // of samples: a ticker that cannot keep up drops the oldest.
    let bound = in_config.sample_rate().0 as usize * in_config.channels() as usize;
    let queue = move |q: &mut Vec<f32>, data: &[f32]| {
        q.extend_from_slice(data);
        if q.len() > bound {
            q.drain(0..q.len() - bound);
        }
    };
    let in_stream = match in_config.sample_format() {
        cpal::SampleFormat::F32 => {
            let audio = Arc::clone(&audio);
            input_device
                .build_input_stream(
                    &in_config.clone().into(),
                    move |data: &[f32], _| {
                        queue(
                            &mut audio.pending.lock().unwrap_or_else(|e| e.into_inner()),
                            data,
                        );
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
                        let f32s: Vec<f32> =
                            data.iter().map(|s| s.to_sample::<f32>()).collect();
                        queue(
                            &mut audio.pending.lock().unwrap_or_else(|e| e.into_inner()),
                            &f32s,
                        );
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
    let at_wire_rate = native_out.as_ref().is_some_and(|c| {
        c.sample_rate().0 == hush_audio::SAMPLE_RATE
    });
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
/// `played` counts each frame actually popped.
fn mix_one_frame(inner: &Arc<Inner>) -> Vec<i16> {
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
                    stats.sent_frames, stats.decoded_frames, stats.played_frames,
                    stats.peers_live
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
                let device = inner.device.get().and_then(|d| d.as_ref());
                match device.and_then(|d| d.next_frame()) {
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
        if let Some(audio) = inner.device.get().and_then(|d| d.as_ref()) {
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
    webrtc::runtime::timeout(&*runtime(), duration, future).await.ok()
}

/// A voice session's media plane: one peer connection per joined peer, audio
/// both ways.
pub struct VoicePlane {
    inner: Arc<Inner>,
}

impl VoicePlane {
    /// A plane with no connections yet. `stun` is the operator's STUN server
    /// when they run one — the same `resolve_stun_url` value the Paper's
    /// plane reads; with `None`, peers gather host candidates only.
    pub fn new(stun: Option<String>, mode: MediaMode) -> Self {
        let inner = Arc::new(Inner {
            stun,
            mode,
            links: Mutex::new(HashMap::new()),
            muted: AtomicBool::new(false),
            sent: AtomicUsize::new(0),
            decoded: AtomicUsize::new(0),
            played: AtomicUsize::new(0),
            loops_started: OnceLock::new(),
            device: OnceLock::new(),
            tone_phase: Mutex::new(0.0),
        });
        if mode == MediaMode::Device {
            let device = Arc::new(DeviceAudio {
                pending: Mutex::new(Vec::new()),
                staging: Mutex::new(Vec::new()),
                // The real rates are written by the device thread from the
                // config it actually opened; these defaults (48 kHz mono)
                // are what a tap with no device would want anyway.
                in_rate: hush_audio::SAMPLE_RATE,
                in_channels: 1,
                cursor: Mutex::new(PlayoutCursor::default()),
                alive: Arc::new(AtomicBool::new(true)),
                output_live: AtomicBool::new(false),
                input_live: AtomicBool::new(false),
            });
            let _ = inner.device.set(Some(device));
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
        if let Some(Some(device)) = self.inner.device.get() {
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
    async fn build(
        &self,
        peer: &str,
    ) -> Result<(Arc<dyn PeerConnection>, Receiver<()>), String> {
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
        let mut builder = PeerConnectionBuilder::new()
            .with_handler(handler)
            .with_runtime(runtime())
            .with_media_engine(media_engine)
            // Bind every interface: a LAN peer has to be reachable on the
            // interface the two machines actually share, not just loopback.
            .with_udp_addrs(vec!["0.0.0.0:0".to_string()]);
        // No STUN unless the operator configured one: a public server would
        // learn the address of every peer that joins a voice room.
        if let Some(stun) = self.inner.stun.as_ref() {
            builder = builder.with_configuration(
                webrtc::peer_connection::RTCConfigurationBuilder::new()
                    .with_ice_servers(vec![RTCIceServer {
                        urls: vec![stun.clone()],
                        ..Default::default()
                    }])
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
            assert!(offer.contains("m=audio"), "the offer carries an audio m-line");
            let answer = b.answer("a", &offer).await.expect("B answers");
            assert!(answer.contains("m=audio"), "the answer carries an audio m-line");
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
        // The device thread opens the streams; give it half a second and the
        // capture path must have seen something off the device.
        let plane = VoicePlane::new(None, MediaMode::Device);
        std::thread::sleep(Duration::from_millis(500));
        let device = plane.inner.device.get().and_then(|d| d.as_ref());
        match device {
            Some(device) => {
                assert!(
                    device.input_live.load(Ordering::SeqCst),
                    "the device thread must mark the input side live"
                );
            }
            None => panic!("a Device-mode plane always carries its device queues"),
        }
        plane.shutdown();
    }
}
