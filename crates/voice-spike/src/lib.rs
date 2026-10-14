//! Spike support for the voice-engine decision (Task 23 §2 #4): the Opus FFI
//! the daemon would own if `webrtc-rs` wins, the test tone, and the metric
//! that decides whether audio survived the transport.
//!
//! Throwaway evidence, not product code. The FFI surface is deliberately the
//! minimum: create, encode/decode, destroy — nothing else is needed to answer
//! "is the bespoke glue acceptable".

use std::ffi::c_int;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const SAMPLE_RATE: u32 = 48_000;
/// 20 ms frames — the ptime the wire docs pin.
pub const FRAME_MS: usize = 20;
pub const FRAME_SAMPLES: usize = SAMPLE_RATE as usize * FRAME_MS / 1000;
/// Generous ceiling for one 20 ms Opus frame at VoIP bitrates.
pub const MAX_PACKET: usize = 400;
/// libopus `OPUS_APPLICATION_VOIP`.
const OPUS_APPLICATION_VOIP: c_int = 2048;
const OPUS_OK: c_int = 0;

#[allow(non_camel_case_types)]
mod ffi {
    use std::ffi::c_int;

    // Opaque handles; the layouts live inside libopus.
    #[repr(C)]
    pub struct OpusEncoder {
        _private: [u8; 0],
    }
    #[repr(C)]
    pub struct OpusDecoder {
        _private: [u8; 0],
    }

    extern "C" {
        pub fn opus_encoder_create(
            sample_rate: c_int,
            channels: c_int,
            application: c_int,
            error: *mut c_int,
        ) -> *mut OpusEncoder;
        pub fn opus_encoder_destroy(enc: *mut OpusEncoder);
        pub fn opus_encode(
            enc: *mut OpusEncoder,
            pcm: *const i16,
            frame_size: c_int,
            data: *mut u8,
            max_data_bytes: c_int,
        ) -> c_int;

        pub fn opus_decoder_create(
            sample_rate: c_int,
            channels: c_int,
            error: *mut c_int,
        ) -> *mut OpusDecoder;
        pub fn opus_decoder_destroy(dec: *mut OpusDecoder);
        pub fn opus_decode(
            dec: *mut OpusDecoder,
            data: *const u8,
            len: c_int,
            pcm: *mut i16,
            frame_size: c_int,
            decode_fec: c_int,
        ) -> c_int;
    }
}

/// A mono 48 kHz Opus encoder. The bindings link the vendored libopus built
/// by `build.rs`; the surface is the encoder half of the glue the daemon
/// would own.
pub struct Encoder {
    ptr: *mut ffi::OpusEncoder,
}

// The raw handle is moved across tasks in the spike; libopus encoders are not
// themselves thread-safe but the spike uses one per task, never shared.
unsafe impl Send for Encoder {}

impl Encoder {
    pub fn new() -> Option<Self> {
        let mut err: c_int = 0;
        let ptr = unsafe {
            ffi::opus_encoder_create(
                SAMPLE_RATE as c_int,
                1,
                OPUS_APPLICATION_VOIP,
                &mut err,
            )
        };
        (err == OPUS_OK && !ptr.is_null()).then(|| Self { ptr })
    }

    /// One 20 ms frame in, one Opus packet out.
    pub fn encode(&mut self, pcm: &[i16; FRAME_SAMPLES]) -> Vec<u8> {
        let mut out = vec![0u8; MAX_PACKET];
        let n = unsafe {
            ffi::opus_encode(
                self.ptr,
                pcm.as_ptr(),
                FRAME_SAMPLES as c_int,
                out.as_mut_ptr(),
                MAX_PACKET as c_int,
            )
        };
        assert!(n > 0, "opus_encode failed: {n}");
        out.truncate(n as usize);
        out
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe { ffi::opus_encoder_destroy(self.ptr) }
    }
}

/// A mono 48 kHz Opus decoder — the playout half of the glue.
pub struct Decoder {
    ptr: *mut ffi::OpusDecoder,
}

unsafe impl Send for Decoder {}

impl Decoder {
    pub fn new() -> Option<Self> {
        let mut err: c_int = 0;
        let ptr = unsafe { ffi::opus_decoder_create(SAMPLE_RATE as c_int, 1, &mut err) };
        (err == OPUS_OK && !ptr.is_null()).then(|| Self { ptr })
    }

    /// One Opus packet in, one 20 ms frame of PCM out.
    pub fn decode(&mut self, packet: &[u8]) -> Vec<i16> {
        let mut out = vec![0i16; FRAME_SAMPLES];
        let n = unsafe {
            ffi::opus_decode(
                self.ptr,
                packet.as_ptr(),
                packet.len() as c_int,
                out.as_mut_ptr(),
                FRAME_SAMPLES as c_int,
                0,
            )
        };
        assert!(n > 0, "opus_decode failed: {n}");
        out
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        unsafe { ffi::opus_decoder_destroy(self.ptr) }
    }
}

/// Fill `pcm` with a 440 Hz sine, phase-continuous across calls when the
/// caller advances `phase`.
pub fn fill_tone(pcm: &mut [i16], phase_deg: &mut f64) {
    const HZ: f64 = 440.0;
    const AMP: f64 = 12_000.0; // comfortably below clip, above Opus noise floor
    for sample in pcm.iter_mut() {
        *sample = (AMP * (*phase_deg).to_radians().sin()) as i16;
        *phase_deg += 360.0 * HZ as f64 / SAMPLE_RATE as f64;
    }
}

/// Root mean square, as a loudness figure immune to sign.
pub fn rms(samples: &[i16]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let energy: f64 = samples.iter().map(|s| (*s as f64) * (*s as f64)).sum();
    (energy / samples.len() as f64).sqrt()
}

/// A crude dominant-pitch estimate: the zero-crossing rate mapped through the
/// sampling theorem. Good enough to tell 440 Hz speech-band tone apart from
/// silence or a codec noise floor.
pub fn dominant_hz(samples: &[i16]) -> f64 {
    if samples.len() < 2 {
        return 0.0;
    }
    let crossings = samples
        .windows(2)
        .filter(|w| w[0] < 0 && w[1] >= 0)
        .count();
    // One crossing per half-period in a sine; rising crossings only, so the
    // period is crossings-per-sample times two samples.
    crossings as f64 * SAMPLE_RATE as f64 / samples.len() as f64
}

/// The spike's verdict, printed and asserted on.
pub struct Report {
    pub text: String,
    pub verdict_ok: bool,
}

const PT_OPUS: u8 = 111; // the default registry's Opus payload type
const SSRC: u32 = 1001;
const TONE_FRAMES: usize = 150; // 3 s of audio at 20 ms

/// The engine proof (Task 23 §2 #4): two peer connections on the daemon's own
/// `webrtc` stack exchange a 440 Hz tone over loopback as Opus-carrying RTP,
/// and the run reports delivery, fidelity, cadence, and CPU. The verdict is
/// true only when every frame came back sample-exact against a codec-only
/// reference — libopus is deterministic, so a lossless path returns exactly
/// those bytes.
pub async fn engine_proof() -> Result<Report, Box<dyn std::error::Error + Send + Sync>> {
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
        RTCPeerConnectionState,
    };

    struct Connected {
        tx: tokio::sync::watch::Sender<bool>,
        gathered_tx: tokio::sync::watch::Sender<bool>,
        gathered_rx: tokio::sync::watch::Receiver<bool>,
    }
    impl Connected {
        fn channel() -> (Arc<Self>, tokio::sync::watch::Receiver<bool>) {
            let (tx, rx) = tokio::sync::watch::channel(false);
            let (gathered_tx, gathered_rx) = tokio::sync::watch::channel(false);
            (
                Arc::new(Self {
                    tx,
                    gathered_tx,
                    gathered_rx,
                }),
                rx,
            )
        }
        fn gathered(&self) {
            let _ = self.gathered_tx.send(true);
        }
        async fn gathered_rx(&self) {
            let mut watch = self.gathered_rx.clone();
            let _ = watch.changed().await;
        }
    }

    struct TxHandler(Arc<Connected>);
    #[async_trait::async_trait]
    impl PeerConnectionEventHandler for TxHandler {
        async fn on_ice_gathering_state_change(
            &self,
            state: webrtc::peer_connection::RTCIceGatheringState,
        ) {
            if state == webrtc::peer_connection::RTCIceGatheringState::Complete {
                self.0.gathered();
            }
        }
        async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
            if state == RTCPeerConnectionState::Connected {
                let _ = self.0.tx.send(true);
            }
        }
    }

    let decoded = Arc::new(tokio::sync::Mutex::new(Vec::<i16>::new()));
    let packets = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let arrivals = Arc::new(tokio::sync::Mutex::new(Vec::<Instant>::new()));

    struct RxHandler {
        packets: Arc<std::sync::atomic::AtomicUsize>,
        decoded: Arc<tokio::sync::Mutex<Vec<i16>>>,
        arrivals: Arc<tokio::sync::Mutex<Vec<Instant>>>,
        gathered: Arc<Connected>,
    }
    #[async_trait::async_trait]
    impl PeerConnectionEventHandler for RxHandler {
        async fn on_ice_gathering_state_change(
            &self,
            state: webrtc::peer_connection::RTCIceGatheringState,
        ) {
            if state == webrtc::peer_connection::RTCIceGatheringState::Complete {
                self.gathered.gathered();
            }
        }
        async fn on_track(&self, track: Arc<dyn TrackRemote>) {
            let packets = Arc::clone(&self.packets);
            let decoded = Arc::clone(&self.decoded);
            let arrivals = Arc::clone(&self.arrivals);
            tokio::spawn(async move {
                let mut dec = Decoder::new().expect("decoder");
                while let Some(event) = track.poll().await {
                    match event {
                        TrackRemoteEvent::OnRtpPacket(pkt) => {
                            packets.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            arrivals.lock().await.push(Instant::now());
                            let frame = dec.decode(&pkt.payload);
                            decoded.lock().await.extend_from_slice(&frame);
                        }
                        TrackRemoteEvent::OnEnded => break,
                        _ => {}
                    }
                }
            });
        }
    }

    // The default registry's Opus codec — the one the media engine negotiates
    // (opus/48000/2, PT 111).
    let opus_codec = RTCRtpCodec {
        mime_type: "audio/opus".to_owned(),
        clock_rate: 48_000,
        channels: 2,
        sdp_fmtp_line: "minptime=10;useinbandfec=1".to_owned(),
        rtcp_feedback: vec![],
    };

    // Sender: the track exists before the offer, so the SDP carries the m-line.
    let (tx_connected, mut tx_connected_rx) = Connected::channel();
    let mut media_engine = MediaEngine::default();
    media_engine.register_default_codecs()?;
    let tx_pc = PeerConnectionBuilder::new()
        .with_handler(Arc::new(TxHandler(Arc::clone(&tx_connected))))
        .with_media_engine(media_engine)
        .with_udp_addrs(vec!["0.0.0.0:0".to_string()])
        .build()
        .await?;

    let track = Arc::new(TrackLocalStaticSample::new(
        Instant::now(),
        MediaStreamTrack::new(
            "spike-stream".to_owned(),
            "spike-track".to_owned(),
            "voice-spike".to_owned(),
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
    )?);
    let track_for_send = Arc::clone(&track);
    tx_pc
        .add_track(track as Arc<dyn TrackLocal>)
        .await?;

    // Non-trickle, the paper_data pattern: set local, wait for gathering to
    // complete, and only then hand the full description across.
    let offer = tx_pc.create_offer(None).await?;
    tx_pc
        .set_local_description(offer)
        .await
        .map_err(|e| format!("tx set_local: {e}"))?;
    tokio::time::timeout(Duration::from_secs(10), tx_connected.gathered_rx())
        .await
        .map_err(|_| "gathering the offer's ICE candidates timed out")?;

    // Receiver half — the same default codec registry, so the answer can
    // match the offer's opus m-line.
    let (rx_connected, _) = Connected::channel();
    let mut rx_media_engine = MediaEngine::default();
    rx_media_engine.register_default_codecs()?;
    let rx_pc = PeerConnectionBuilder::new()
        .with_handler(Arc::new(RxHandler {
            packets: Arc::clone(&packets),
            decoded: Arc::clone(&decoded),
            arrivals: Arc::clone(&arrivals),
            gathered: Arc::clone(&rx_connected),
        }))
        .with_media_engine(rx_media_engine)
        .with_udp_addrs(vec!["0.0.0.0:0".to_string()])
        .build()
        .await?;
    rx_pc
        .set_remote_description(tx_pc.local_description().await.unwrap())
        .await
        .map_err(|e| format!("rx set_remote(offer): {e}"))?;
    let answer = rx_pc.create_answer(None).await?;
    rx_pc
        .set_local_description(answer)
        .await
        .map_err(|e| format!("rx set_local: {e}"))?;
    tokio::time::timeout(Duration::from_secs(10), rx_connected.gathered_rx())
        .await
        .map_err(|_| "gathering the answer's ICE candidates timed out")?;
    let answer_desc = rx_pc
        .local_description()
        .await
        .ok_or("no local answer after gathering")?;
    tx_pc
        .set_remote_description(answer_desc)
        .await
        .map_err(|e| format!("tx set_remote(answer): {e}"))?;

    // ICE on loopback connects in well under a second; a deadline keeps a
    // stuck handshake from hanging the spike.
    tokio::time::timeout(Duration::from_secs(10), tx_connected_rx.changed())
        .await
        .map_err(|_| "the peer connections never reached Connected")?
        .map_err(|_| "the connected watcher was dropped")?;
    // Settle the channel: the first sample_writer writes race the ICE pair
    // opening otherwise, and the spike should measure the steady state.
    tokio::time::sleep(Duration::from_millis(150)).await;

    // Stream the tone at real-time cadence: 20 ms of audio every 20 ms.
    let mut enc = Encoder::new().ok_or("opus encoder")?;
    let mut phase = 0.0;
    let mut pcm = [0i16; FRAME_SAMPLES];
    let mut encode_micros_total = 0u128;
    for _ in 0..TONE_FRAMES {
        fill_tone(&mut pcm, &mut phase);
        let enc_start = Instant::now();
        let packet = enc.encode(&pcm);
        encode_micros_total += enc_start.elapsed().as_micros();
        track_for_send
            .sample_writer(SSRC, PT_OPUS)
            .write_sample(&Sample {
                data: packet.into(),
                timestamp: Instant::now(),
                duration: Duration::from_millis(FRAME_MS as u64),
                packet_timestamp: 0,
                prev_dropped_packets: 0,
                prev_padding_packets: 0,
            })
            .await?;
        tokio::time::sleep(Duration::from_millis(FRAME_MS as u64)).await;
    }

    // Let the tail drain, then measure.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let _ = tx_pc.close().await;
    let _ = rx_pc.close().await;

    let received = packets.load(std::sync::atomic::Ordering::SeqCst);
    let decoded_now = decoded.lock().await.clone();
    let arrivals_now = arrivals.lock().await.clone();

    // The reference: the same frames encoded and decoded without a transport.
    let mut ref_enc = Encoder::new().ok_or("opus encoder")?;
    let mut ref_dec = Decoder::new().ok_or("opus decoder")?;
    let mut ref_phase = 0.0;
    let mut reference = Vec::with_capacity(TONE_FRAMES * FRAME_SAMPLES);
    let mut ref_pcm = [0i16; FRAME_SAMPLES];
    let mut ref_decode_micros_total = 0u128;
    for _ in 0..TONE_FRAMES {
        fill_tone(&mut ref_pcm, &mut ref_phase);
        let packet = ref_enc.encode(&ref_pcm);
        let dec_start = Instant::now();
        let frame = ref_dec.decode(&packet);
        ref_decode_micros_total += dec_start.elapsed().as_micros();
        reference.extend_from_slice(&frame);
    }

    let cadence = if arrivals_now.len() > 2 {
        let deltas: Vec<u128> = arrivals_now
            .windows(2)
            .map(|w| (w[1] - w[0]).as_micros())
            .collect();
        let mean = deltas.iter().sum::<u128>() / deltas.len() as u128;
        let worst = deltas.iter().map(|d| d.abs_diff(mean)).max().unwrap_or(0);
        format!(
            "{mean}µs mean, ±{worst}µs worst over {} arrivals",
            deltas.len()
        )
    } else {
        "no arrivals".to_string()
    };

    let (skips, clean) = matches_reference(&decoded_now, &reference);
    let level = rms(&decoded_now);
    let verdict_ok = clean && received + skips >= TONE_FRAMES && skips <= TONE_FRAMES / 10;
    let verdict = if clean && skips == 0 {
        "the daemon's own stack carries opus audio losslessly on loopback"
    } else if clean {
        "the transport survived with whole-frame loss only — audio is exact between the skips"
    } else {
        "the transport or the codec glue did not survive — see the counts above"
    };

    Ok(Report {
        text: format!(
            "voice-spike engine proof\n\
             frames sent: {TONE_FRAMES} ({} ms of audio)\n\
             rtp packets received: {received} ({:.1}% of sent)\n\
             decoded samples: {} (reference {})\n\
             fidelity: {clean} with {skips} whole-frame skips\n\
             decoded rms: {level:.0}\n\
             arrival cadence: {cadence}\n\
             encode cpu: {} µs/frame, decode cpu: {} µs/frame ({} µs of audio per frame)\n\
             verdict: {verdict}",
            TONE_FRAMES * FRAME_MS,
            100.0 * received as f64 / TONE_FRAMES as f64,
            decoded_now.len(),
            reference.len(),
            encode_micros_total / TONE_FRAMES as u128,
            ref_decode_micros_total / TONE_FRAMES as u128,
            FRAME_MS * 1000,
        ),
        verdict_ok,
    })
}

/// Whether the audio survived: the received stream is the reference stream
/// with whole 20 ms frames deleted — the libopus output is deterministic, so
/// every frame that arrives unlost must match the reference sample-for-sample
/// at the right offset. Returns the frames the reference has that the
/// received stream does not (the loss count) and whether no mismatch needed
/// anything but whole-frame skips.
pub fn matches_reference(received: &[i16], reference: &[i16]) -> (usize, bool) {
    let mut skips = 0usize;
    let mut r = 0usize; // into received
    let mut f = 0usize; // into reference
    while r < received.len() {
        if f + FRAME_SAMPLES > reference.len() {
            // Received audio beyond the reference: an unexplained mismatch.
            return (skips, false);
        }
        let recv_frame = &received[r..r + FRAME_SAMPLES];
        let ref_frame = &reference[f..f + FRAME_SAMPLES];
        if recv_frame == ref_frame {
            r += FRAME_SAMPLES;
            f += FRAME_SAMPLES;
        } else {
            // Whole-frame loss: advance the reference past one frame.
            skips += 1;
            f += FRAME_SAMPLES;
            // A single lost frame may straddle a decode frame boundary only
            // by packet loss of whole packets, so one skip is exact.
        }
    }
    (skips, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_vendored_opus_encodes_and_decodes_a_tone() {
        let mut enc = Encoder::new().expect("encoder");
        let mut dec = Decoder::new().expect("decoder");
        let mut phase = 0.0;
        let mut pcm = [0i16; FRAME_SAMPLES];
        fill_tone(&mut pcm, &mut phase);

        let packet = enc.encode(&pcm);
        assert!(!packet.is_empty(), "an encoded frame is never empty");
        assert!(packet.len() <= MAX_PACKET);

        let decoded = dec.decode(&packet);
        assert_eq!(decoded.len(), FRAME_SAMPLES);
        // Opus is lossy but a clean 440 Hz tone must survive recognisably:
        // within 4.5 dB of the input's level, whatever the encoder's default
        // bitrate does to it.
        let loud = rms(&decoded);
        let input = rms(&pcm);
        assert!(
            loud >= 0.6 * input,
            "decoded tone lost too much level: rms {loud} vs input {input}"
        );
        let hz = dominant_hz(&decoded);
        assert!(
            (400.0..=480.0).contains(&hz),
            "decoded tone drifted off pitch: {hz} Hz"
        );
    }

    #[test]
    fn a_round_trip_matches_itself_sample_for_sample() {
        let mut enc = Encoder::new().expect("encoder");
        let mut dec = Decoder::new().expect("decoder");
        let mut phase = 0.0;
        let mut pcm = [0i16; FRAME_SAMPLES];
        fill_tone(&mut pcm, &mut phase);
        let packet = enc.encode(&pcm);
        let decoded = dec.decode(&packet);
        // The metric's matched-pair case: encode/decode the same frame again
        // and the two must agree.
        let mut enc2 = Encoder::new().expect("encoder");
        let mut dec2 = Decoder::new().expect("decoder");
        let mut phase2 = 0.0;
        let mut pcm2 = [0i16; FRAME_SAMPLES];
        fill_tone(&mut pcm2, &mut phase2);
        let reference = dec2.decode(&enc2.encode(&pcm2));
        assert_eq!(matches_reference(&decoded, &reference), (0, true));
        // And a corrupted buffer must not match — clean is false whatever the
        // skip count did on the way there.
        let (_, clean) = matches_reference(&pcm, &reference);
        assert!(!clean);
        // A lost frame is a whole-frame skip, and only that: three
        // phase-continuous frames, middle one gone.
        let mut three_frames = Vec::new();
        let mut lossy_phase = 0.0;
        for _ in 0..3 {
            let mut f = [0i16; FRAME_SAMPLES];
            fill_tone(&mut f, &mut lossy_phase);
            three_frames.extend_from_slice(&f);
        }
        let mut with_loss = three_frames.clone();
        with_loss.drain(FRAME_SAMPLES..2 * FRAME_SAMPLES);
        assert_eq!(matches_reference(&with_loss, &three_frames), (1, true));
        // Without the loss it is exact.
        assert_eq!(matches_reference(&three_frames, &three_frames), (0, true));
    }
}
