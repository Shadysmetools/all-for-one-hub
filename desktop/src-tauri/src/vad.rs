//! Voice activity detection, utterance segmentation, barge-in detection and WAV encoding.
//! This all lives in Rust: the webview only captures raw 16 kHz mono PCM (so the browser's
//! echo cancellation stays in the loop) and streams it here. Pure and fully unit tested.

pub const RATE: usize = 16_000;
/// One AudioWorklet quantum: 128 samples at 16 kHz.
pub const FRAME: usize = 128;
const FRAME_MS: usize = 8;
const PREROLL_FRAMES: usize = 38; // ~300 ms
const END_SILENCE_MS: usize = 600;
const MIN_SPEECH_MS: usize = 300;
const MAX_UTTERANCE_MS: usize = 30_000;

#[derive(Debug, PartialEq)]
pub enum VadEvent {
    /// 0..1, for the orb.
    Level(f32),
    SpeechStart,
    /// Speech started while barge mode is on (One is talking or thinking).
    BargeIn,
    /// Speech started but ended too short to be an utterance (a noise burst).
    SpeechDropped,
    /// A finished utterance as a 16 kHz mono PCM16 WAV.
    Utterance(Vec<u8>),
}

pub struct Vad {
    muted: bool,
    barge: bool,
    floor: f32,
    carry: Vec<f32>,
    preroll: Vec<Vec<f32>>,
    utt: Vec<f32>,
    utt_frames: usize,
    in_speech: bool,
    loud_run: usize,
    silence_ms: usize,
    speech_ms: usize,
    /// Samples of `utt` already handed to the live speech-to-text stream.
    streamed: usize,
    /// The end of the last utterance that was not streamed yet (sent before its commit).
    stream_tail: Vec<f32>,
    /// Trailing silence of the last finished utterance (its speech ended this long before).
    last_trailing_ms: usize,
}

impl Default for Vad {
    fn default() -> Self {
        Vad {
            muted: false,
            barge: false,
            floor: 0.005,
            carry: Vec::new(),
            preroll: Vec::new(),
            utt: Vec::new(),
            utt_frames: 0,
            in_speech: false,
            loud_run: 0,
            silence_ms: 0,
            speech_ms: 0,
            streamed: 0,
            stream_tail: Vec::new(),
            last_trailing_ms: 0,
        }
    }
}

fn rms(f: &[f32]) -> f32 {
    if f.is_empty() {
        return 0.0;
    }
    (f.iter().map(|x| x * x).sum::<f32>() / f.len() as f32).sqrt()
}

impl Vad {
    #[cfg(test)]
    pub fn new() -> Self {
        Self::default()
    }

    /// Muting drops everything in flight, including the pre-roll and any half-heard speech.
    pub fn set_muted(&mut self, m: bool) {
        self.muted = m;
        if m {
            self.reset();
            self.preroll.clear();
            self.carry.clear();
            self.stream_tail.clear();
        }
    }

    /// Stop pressed: drop half-heard speech, pre-roll and carry, but stay in the same mute state.
    pub fn discard(&mut self) {
        self.reset();
        self.preroll.clear();
        self.carry.clear();
        self.stream_tail.clear();
    }

    /// Audio of the current utterance (pre-roll included) not handed out yet, plus the unsent
    /// end of an utterance that just finished. Empty while nobody speaks: silence is never streamed.
    pub fn take_stream(&mut self) -> Vec<f32> {
        let mut out = std::mem::take(&mut self.stream_tail);
        if self.in_speech {
            out.extend_from_slice(&self.utt[self.streamed.min(self.utt.len())..]);
            self.streamed = self.utt.len();
        }
        out
    }

    pub fn in_speech(&self) -> bool {
        self.in_speech
    }

    /// Quiet time at the end of the speech in progress.
    pub fn silence_ms(&self) -> usize {
        self.silence_ms
    }

    /// How long before the end of the last utterance the user actually stopped speaking.
    pub fn last_trailing_ms(&self) -> usize {
        self.last_trailing_ms
    }

    #[cfg(test)]
    pub fn is_muted(&self) -> bool {
        self.muted
    }

    pub fn set_barge(&mut self, on: bool) {
        self.barge = on;
    }

    fn reset(&mut self) {
        self.in_speech = false;
        self.loud_run = 0;
        self.utt.clear();
        self.utt_frames = 0;
        self.silence_ms = 0;
        self.speech_ms = 0;
        self.streamed = 0;
    }

    /// Feed any number of samples; they are cut into 128-sample frames (a partial frame is
    /// carried to the next call). Returns the events, in order.
    pub fn feed(&mut self, samples: &[f32]) -> Vec<VadEvent> {
        let mut events = Vec::new();
        if self.muted {
            return events;
        }
        self.carry.extend_from_slice(samples);
        let whole = self.carry.len() / FRAME * FRAME;
        let frames: Vec<f32> = self.carry.drain(..whole).collect();
        for f in frames.chunks_exact(FRAME) {
            self.frame(f, &mut events);
        }
        events
    }

    fn frame(&mut self, f: &[f32], out: &mut Vec<VadEvent>) {
        let e = rms(f);
        out.push(VadEvent::Level((e * 8.0).min(1.0)));
        if !self.in_speech && e < self.floor * 2.0 {
            self.floor = self.floor * 0.98 + e * 0.02;
        }
        let mult = if self.barge { 2.5 } else { 1.0 };
        let loud = e > (0.012f32).max(self.floor * 3.0) * mult;
        let need_run = if self.barge { 19 } else { 8 }; // ~150 ms vs ~60 ms of sustained energy
        if self.in_speech {
            self.speech_frame(f, loud, out);
        } else {
            self.idle_frame(f, loud, need_run, out);
        }
    }

    fn idle_frame(&mut self, f: &[f32], loud: bool, need_run: usize, out: &mut Vec<VadEvent>) {
        self.preroll.push(f.to_vec());
        if self.preroll.len() > PREROLL_FRAMES {
            self.preroll.remove(0);
        }
        self.loud_run = if loud { self.loud_run + 1 } else { 0 };
        if self.loud_run < need_run {
            return;
        }
        if self.barge {
            out.push(VadEvent::BargeIn);
        }
        out.push(VadEvent::SpeechStart);
        self.in_speech = true;
        self.utt = self.preroll.drain(..).flatten().collect();
        self.utt_frames = self.utt.len() / FRAME;
        self.silence_ms = 0;
        self.speech_ms = self.loud_run * FRAME_MS;
    }

    fn speech_frame(&mut self, f: &[f32], loud: bool, out: &mut Vec<VadEvent>) {
        self.utt.extend_from_slice(f);
        self.utt_frames += 1;
        if loud {
            self.silence_ms = 0;
            self.speech_ms += FRAME_MS;
        } else {
            self.silence_ms += FRAME_MS;
        }
        let too_long = self.utt_frames * FRAME_MS > MAX_UTTERANCE_MS;
        if self.silence_ms < END_SILENCE_MS && !too_long {
            return;
        }
        self.end_utterance(out);
    }

    fn end_utterance(&mut self, out: &mut Vec<VadEvent>) {
        let ok = self.speech_ms >= MIN_SPEECH_MS;
        self.stream_tail = self.utt[self.streamed.min(self.utt.len())..].to_vec();
        self.last_trailing_ms = self.silence_ms;
        let samples = std::mem::take(&mut self.utt);
        self.reset();
        out.push(if ok { VadEvent::Utterance(encode_wav(&samples)) } else { VadEvent::SpeechDropped });
    }

    /// End any speech in progress right now (push-to-talk released).
    pub fn flush(&mut self) -> Vec<VadEvent> {
        let mut out = Vec::new();
        if self.in_speech {
            self.end_utterance(&mut out);
        }
        out
    }
}

/// 16 kHz mono PCM16 WAV.
pub fn encode_wav(samples: &[f32]) -> Vec<u8> {
    let n = samples.len() as u32;
    let mut w = Vec::with_capacity(44 + samples.len() * 2);
    w.extend(b"RIFF");
    w.extend((36 + n * 2).to_le_bytes());
    w.extend(b"WAVEfmt ");
    w.extend(16u32.to_le_bytes());
    w.extend(1u16.to_le_bytes()); // PCM
    w.extend(1u16.to_le_bytes()); // mono
    w.extend((RATE as u32).to_le_bytes());
    w.extend((RATE as u32 * 2).to_le_bytes());
    w.extend(2u16.to_le_bytes());
    w.extend(16u16.to_le_bytes());
    w.extend(b"data");
    w.extend((n * 2).to_le_bytes());
    for s in samples {
        let c = s.clamp(-1.0, 1.0);
        let v = if c < 0.0 { c * 32768.0 } else { c * 32767.0 };
        w.extend((v as i16).to_le_bytes());
    }
    w
}

/// Raw little-endian f32 bytes (what the webview sends) to samples. Odd tails are dropped.
pub fn decode_f32le(bytes: &[u8]) -> Vec<f32> {
    bytes.chunks_exact(4).map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(frames: usize, amp: f32) -> Vec<f32> {
        (0..frames * FRAME).map(|i| if i % 2 == 0 { amp } else { -amp }).collect()
    }
    #[test]
    fn flush_ends_speech_in_progress_and_is_a_no_op_when_idle() {
        let mut v = Vad::new();
        assert!(v.flush().is_empty());
        v.feed(&tone(80, 0.3)); // still talking: no silence yet, so no utterance so far
        let ev = v.flush();
        assert_eq!(count(&ev, |e| matches!(e, VadEvent::Utterance(_))), 1);
        assert!(v.flush().is_empty(), "nothing left");
    }

    fn count(ev: &[VadEvent], f: impl Fn(&VadEvent) -> bool) -> usize {
        ev.iter().filter(|e| f(e)).count()
    }
    fn utterances(ev: &[VadEvent]) -> Vec<&Vec<u8>> {
        ev.iter().filter_map(|e| if let VadEvent::Utterance(w) = e { Some(w) } else { None }).collect()
    }

    /// 100 frames of speech (800 ms), then 80 frames (640 ms) of silence.
    fn speak_then_pause(v: &mut Vad) -> Vec<VadEvent> {
        let mut ev = v.feed(&tone(100, 0.2));
        ev.extend(v.feed(&tone(80, 0.0)));
        ev
    }

    #[test]
    fn only_speech_is_handed_to_the_live_stream_preroll_and_tail_included_in_order() {
        let mut v = Vad::new();
        v.feed(&tone(40, 0.0));
        assert!(v.take_stream().is_empty(), "silence before speech is never streamed");
        v.feed(&tone(100, 0.2));
        let first = v.take_stream();
        assert!(first.len() >= 100 * FRAME, "the speech so far, with its pre-roll");
        assert!(v.take_stream().is_empty(), "nothing is handed out twice");
        let ev = v.feed(&tone(80, 0.0)); // the silence that ends it
        assert_eq!(utterances(&ev).len(), 1);
        let tail = v.take_stream();
        assert!(!tail.is_empty(), "the unsent end goes out before the commit");
        // everything streamed is exactly the utterance that was cut
        let wav = utterances(&ev)[0];
        assert_eq!((first.len() + tail.len()) * 2 + 44, wav.len());
        assert!(v.last_trailing_ms() >= 600);
        assert!(v.take_stream().is_empty());
        v.feed(&tone(40, 0.0));
        assert!(v.take_stream().is_empty(), "silence after speech is never streamed");
        // mute and Stop drop an unsent tail
        v.feed(&tone(100, 0.2));
        v.feed(&tone(80, 0.0));
        v.discard();
        assert!(v.take_stream().is_empty());
    }

    #[test]
    fn silence_makes_levels_but_no_speech() {
        let mut v = Vad::new();
        let ev = v.feed(&tone(50, 0.0));
        assert_eq!(count(&ev, |e| matches!(e, VadEvent::Level(_))), 50);
        assert_eq!(count(&ev, |e| !matches!(e, VadEvent::Level(_))), 0);
    }

    #[test]
    fn speech_then_600ms_of_silence_is_one_utterance() {
        let mut v = Vad::new();
        let ev = speak_then_pause(&mut v);
        assert_eq!(count(&ev, |e| matches!(e, VadEvent::SpeechStart)), 1);
        assert_eq!(count(&ev, |e| matches!(e, VadEvent::BargeIn)), 0);
        let u = utterances(&ev);
        assert_eq!(u.len(), 1);
        assert_eq!(&u[0][0..4], b"RIFF");
        assert_eq!(&u[0][36..40], b"data");
        // pre-roll + speech + the 600 ms of trailing silence: more than the speech alone
        assert!((u[0].len() - 44) / 2 > 100 * FRAME);
    }

    #[test]
    fn a_short_blip_is_not_an_utterance() {
        let mut v = Vad::new();
        let mut ev = v.feed(&tone(10, 0.2)); // 80 ms: starts "speech" (8 frames) but is under 300 ms
        ev.extend(v.feed(&tone(100, 0.0)));
        assert_eq!(count(&ev, |e| matches!(e, VadEvent::SpeechStart)), 1);
        assert!(utterances(&ev).is_empty());
        // and the detector is ready for the next real utterance
        assert_eq!(utterances(&speak_then_pause(&mut v)).len(), 1);
    }

    #[test]
    fn speech_needs_about_60ms_of_sustained_energy() {
        let mut v = Vad::new();
        assert_eq!(count(&v.feed(&tone(7, 0.2)), |e| matches!(e, VadEvent::SpeechStart)), 0);
        let mut v = Vad::new();
        assert_eq!(count(&v.feed(&tone(8, 0.2)), |e| matches!(e, VadEvent::SpeechStart)), 1);
    }

    #[test]
    fn barge_mode_is_stricter_and_reports_barge_in() {
        let mut v = Vad::new();
        v.set_barge(true);
        // quiet speech (0.02) is below the raised threshold (2.5x), so no barge-in
        assert_eq!(count(&v.feed(&tone(40, 0.02)), |e| matches!(e, VadEvent::BargeIn)), 0);
        // 18 loud frames is not enough, 19 is
        let mut v = Vad::new();
        v.set_barge(true);
        assert_eq!(count(&v.feed(&tone(18, 0.2)), |e| matches!(e, VadEvent::BargeIn)), 0);
        let ev = v.feed(&tone(1, 0.2));
        assert_eq!(count(&ev, |e| matches!(e, VadEvent::BargeIn)), 1);
        assert_eq!(count(&ev, |e| matches!(e, VadEvent::SpeechStart)), 1);
    }

    #[test]
    fn muted_ignores_audio_and_mute_drops_half_heard_speech() {
        let mut v = Vad::new();
        v.feed(&tone(30, 0.2)); // mid-sentence
        v.set_muted(true);
        assert!(v.feed(&tone(200, 0.2)).is_empty());
        assert!(v.is_muted());
        v.set_muted(false);
        // the earlier speech is gone: silence now ends nothing
        assert!(utterances(&v.feed(&tone(100, 0.0))).is_empty());
        assert_eq!(utterances(&speak_then_pause(&mut v)).len(), 1);
    }

    #[test]
    fn partial_frames_are_carried_between_calls() {
        let mut v = Vad::new();
        let all = tone(100, 0.2);
        let mut ev = Vec::new();
        for chunk in all.chunks(100) {
            ev.extend(v.feed(chunk)); // 100 samples: never a whole frame boundary
        }
        assert_eq!(count(&ev, |e| matches!(e, VadEvent::Level(_))), 100);
    }

    #[test]
    fn a_very_long_utterance_is_cut_at_30_seconds() {
        let mut v = Vad::new();
        let ev = v.feed(&tone(4000, 0.2)); // 32 s of speech
        assert_eq!(utterances(&ev).len(), 1);
    }

    #[test]
    fn wav_encoding_is_correct_and_clamped() {
        let w = encode_wav(&[0.0, 1.0, -1.0, 2.0, -2.0]);
        assert_eq!(w.len(), 44 + 10);
        assert_eq!(u32::from_le_bytes(w[4..8].try_into().unwrap()), 36 + 10);
        assert_eq!(u32::from_le_bytes(w[24..28].try_into().unwrap()), 16_000);
        let s = |i: usize| i16::from_le_bytes([w[44 + i * 2], w[45 + i * 2]]);
        assert_eq!((s(0), s(1), s(2), s(3), s(4)), (0, 32767, -32768, 32767, -32768));
    }

    #[test]
    fn decode_f32le_round_trips_and_drops_a_torn_tail() {
        let mut bytes: Vec<u8> = [0.5f32, -0.25].iter().flat_map(|f| f.to_le_bytes()).collect();
        bytes.push(9);
        assert_eq!(decode_f32le(&bytes), vec![0.5, -0.25]);
    }
}
