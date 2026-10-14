//! Conclave voice's audio glue (Task 23 Phase 2): the Opus codec FFI the
//! engine spike (`crates/voice-spike`) proved, the device-rate → wire-rate
//! resampler, and the playout jitter buffer.
//!
//! The wire contract is fixed: **48 kHz mono s16le, 20 ms (960-sample) frames,
//! one Opus packet per frame** — what the spike carried losslessly over the
//! daemon's own `webrtc` stack (docs/tasks/conclave-voice.md §2 #4). Devices
//! offer whatever they offer (44.1/48 kHz, mono/stereo, f32); [`resample`]
//! and [`to_i16`] bring a capture callback's buffers to the wire contract, and
//! [`JitterBuffer`] holds decoded frames for a playout callback that runs on
//! the device's clock, not the network's.

use std::ffi::c_int;

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

/// A mono 48 kHz Opus encoder — the capture half of the voice plane's glue.
pub struct Encoder {
    ptr: *mut ffi::OpusEncoder,
}

// libopus encoders are not thread-safe, but an encoder is owned by the one
// capture task that creates it and never shared.
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

/// A capture device's interleaved f32 buffer, brought to the wire contract:
/// mixed to mono and linearly resampled to 48 kHz.
///
/// Linear interpolation is below voice-difference audibility for the common
/// 44.1→48 kHz hop, and it is stateless — a dropped callback cannot desync
/// it. Channel count `channels` interleaves; rate 48 kHz is a passthrough
/// (mix only).
pub fn resample(input: &[f32], in_rate: u32, channels: u16) -> Vec<f32> {
    if input.is_empty() || channels == 0 {
        return Vec::new();
    }
    let channels = channels as usize;

    // Mix to mono first: one sample per input frame.
    let mono: Vec<f32> = input
        .chunks(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect();

    if in_rate == SAMPLE_RATE {
        return mono;
    }

    // Linear interpolation across the mono stream. Output sample i sits at
    // input position i * (in_rate / 48k); the last input sample is never
    // extrapolated past.
    let in_len = mono.len();
    let out_len = (in_len as u64 * SAMPLE_RATE as u64 / in_rate as u64) as usize;
    let step = in_rate as f64 / SAMPLE_RATE as f64;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let pos = i as f64 * step;
        let idx = pos as usize;
        if idx >= in_len {
            break;
        }
        if idx + 1 >= in_len {
            // The last input sample has no right neighbour to interpolate
            // with; at output rates above the input's this lands at most once
            // per call, and holding the sample beats dropping the frame edge.
            out.push(mono[idx]);
            break;
        }
        let frac = (pos - idx as f64) as f32;
        out.push(mono[idx] * (1.0 - frac) + mono[idx + 1] * frac);
    }
    out
}

/// f32 samples in [-1, 1] to i16 PCM, clamped — the form Opus encodes.
pub fn to_i16(samples: &[f32]) -> Vec<i16> {
    samples
        .iter()
        .map(|s| {
            let scaled = s * 32_767.0;
            scaled.clamp(i16::MIN as f32, i16::MAX as f32).round() as i16
        })
        .collect()
}

/// Holds decoded 20 ms frames so playout can run on the device's clock.
///
/// The network delivers in bursts; the sound device consumes one frame every
/// 20 ms, sharp. The buffer primes to [`target`](JitterBuffer::target) frames
/// before the first pop (a small, deliberate latency that absorbs a jittery
/// start), then serves in order; a frame pushed into a full buffer drops the
/// oldest, so sustained excess delays the newest audio least.
pub struct JitterBuffer {
    frames: std::collections::VecDeque<Vec<i16>>,
    target: usize,
    max: usize,
    primed: bool,
}

impl JitterBuffer {
    /// A buffer that primes to `target` frames and holds at most `max`.
    pub fn new(target: usize, max: usize) -> Self {
        Self {
            frames: std::collections::VecDeque::new(),
            target: target.max(1),
            max: max.max(1),
            primed: false,
        }
    }

    /// Queue one decoded frame.
    pub fn push(&mut self, frame: Vec<i16>) {
        if self.frames.len() >= self.max {
            self.frames.pop_front();
        }
        self.frames.push_back(frame);
    }

    /// The next frame once the buffer is primed, else `None` (the caller
    /// plays silence). A drained buffer re-primes — after an underrun the
    /// playout waits for depth again rather than stuttering through dregs.
    pub fn pop(&mut self) -> Option<Vec<i16>> {
        if !self.primed {
            if self.frames.len() < self.target {
                return None;
            }
            self.primed = true;
        }
        let frame = self.frames.pop_front();
        if self.frames.is_empty() {
            self.primed = false;
        }
        frame
    }

    /// How many frames are queued (for the plane's stats and the dogfood).
    pub fn depth(&self) -> usize {
        self.frames.len()
    }
}

/// Fill `pcm` with a 440 Hz sine, phase-continuous across calls when the
/// caller advances `phase` — the test-tone capture source.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Sine helper for resampler tests: `n` samples of `hz` at `rate`.
    fn sine(hz: f64, rate: u32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (0.2 * (2.0 * std::f64::consts::PI * hz * i as f64 / rate as f64).sin()) as f32)
            .collect()
    }

    #[test]
    fn opus_round_trips_a_tone() {
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
        // within 4.5 dB of the input's level, and at the right pitch.
        let loud = rms(&decoded);
        let input = rms(&pcm);
        assert!(
            loud >= 0.6 * input,
            "decoded tone lost too much level: rms {loud} vs input {input}"
        );
    }

    #[test]
    fn resampler_hits_48k() {
        // One second of 44.1 kHz stereo interleaved.
        let left = sine(440.0, 44_100, 44_100);
        let mut interleaved = Vec::with_capacity(2 * left.len());
        for s in &left {
            interleaved.push(*s);
            interleaved.push(*s); // stereo with identical channels
        }
        let out = resample(&interleaved, 44_100, 2);
        assert_eq!(out.len(), 48_000, "one second in, 48 kHz out");
        assert!(out.iter().all(|s| s.is_finite()), "no NaN/inf");

        // Loudness survives the hop within a decibel-scale tolerance.
        let rms_of = |v: &[f32]| {
            (v.iter().map(|s| s * s).sum::<f32>() / v.len() as f32).sqrt()
        };
        let (in_rms, out_rms) = (rms_of(&left), rms_of(&out));
        assert!(
            out_rms >= 0.9 * in_rms,
            "resampling lost level: {out_rms} vs {in_rms}"
        );

        // 48 kHz input is a passthrough (mix to mono only).
        let at_48 = sine(440.0, 48_000, 960);
        let same = resample(&at_48, 48_000, 1);
        assert_eq!(same, at_48);
    }

    #[test]
    fn jitter_buffer_smoothes_bursts() {
        let frame_of = |v: i16| vec![v; FRAME_SAMPLES];

        // Primes to the target: below it, pop is silence-during-warmup.
        let mut jb = JitterBuffer::new(2, 8);
        jb.push(frame_of(1));
        assert_eq!(jb.pop(), None, "unprimed buffer holds frames back");
        jb.push(frame_of(2));
        assert_eq!(jb.pop(), Some(frame_of(1)), "primed: frames in order");
        assert_eq!(jb.pop(), Some(frame_of(2)), "the primer keeps one spare");
        assert_eq!(jb.pop(), None, "drained buffer re-primes");

        // Overfull drops the oldest: sustained bursts stay bounded.
        let mut jb = JitterBuffer::new(1, 3);
        for v in 1..=5 {
            jb.push(frame_of(v));
        }
        assert_eq!(jb.depth(), 3, "the cap holds");
        assert_eq!(jb.pop(), Some(frame_of(3)), "oldest dropped, newest kept");
    }
}
