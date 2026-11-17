//! How fast One answers: from the END OF THE USER'S SPEECH (the last voiced frame the local VAD
//! heard, not the transcript commit, so the end-of-turn wait is counted) to the FIRST AUDIO of
//! the reply handed to the speaker. Measured on every turn; the target is under 1.5 s.
//! A typed turn starts its clock when the message is sent.

use serde::Serialize;
use std::time::{Duration, Instant};

pub const TARGET_MS: u64 = 1500;

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Latency {
    /// End of speech (or send) to first audio.
    pub total_ms: u64,
    /// End of speech to the final transcript (0 for a typed turn).
    pub stt_ms: u64,
    /// Transcript to Claude's first words.
    pub claude_ms: u64,
    /// Claude's first words to the first audio.
    pub voice_ms: u64,
    pub under_target: bool,
}

/// The clock of one turn. Times are set once (the first event wins).
#[derive(Clone, Debug)]
pub struct Clock {
    pub speech_end: Instant,
    pub transcript: Instant,
    pub first_text: Option<Instant>,
    pub first_audio: Option<Instant>,
}

fn ms(d: Duration) -> u64 {
    d.as_millis().min(u64::MAX as u128) as u64
}

impl Clock {
    /// `speech_end` None = a typed turn: the clock starts at `transcript` (now).
    pub fn new(speech_end: Option<Instant>, transcript: Instant) -> Clock {
        Clock { speech_end: speech_end.filter(|s| *s <= transcript).unwrap_or(transcript), transcript, first_text: None, first_audio: None }
    }

    pub fn text(&mut self, at: Instant) {
        self.first_text.get_or_insert(at);
    }

    /// Records the first audio. Returns the measurement exactly once (on the first chunk).
    pub fn audio(&mut self, at: Instant) -> Option<Latency> {
        if self.first_audio.is_some() {
            return None;
        }
        self.first_audio = Some(at);
        let text = self.first_text.unwrap_or(at).min(at);
        let total = ms(at.saturating_duration_since(self.speech_end));
        Some(Latency {
            total_ms: total,
            stt_ms: ms(self.transcript.saturating_duration_since(self.speech_end)),
            claude_ms: ms(text.saturating_duration_since(self.transcript)),
            voice_ms: ms(at.saturating_duration_since(text)),
            under_target: total < TARGET_MS,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_breakdown_adds_up_and_starts_at_the_end_of_speech() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut c = Clock::new(Some(t0), at(450));
        c.text(at(1050));
        c.text(at(1200)); // later text does not move the first-words time
        let l = c.audio(at(1280)).unwrap();
        assert_eq!(l, Latency { total_ms: 1280, stt_ms: 450, claude_ms: 600, voice_ms: 230, under_target: true });
        assert_eq!(l.stt_ms + l.claude_ms + l.voice_ms, l.total_ms);
        assert_eq!(c.audio(at(1500)), None, "reported once per turn");
    }

    /// LIVE end-to-end: a spoken question (a 16 kHz mono WAV, path in ONE_TEST_WAV) streamed at
    /// real-time pace into live speech-to-text, the committed text into a warm Claude, Claude's
    /// first chunk into the live voice. Prints the breakdown; asserts the 1.5 s target when the
    /// voice is available (a used-up quota is reported, not hidden).
    /// Run: ONE_TEST_WAV=C:\path\q.wav cargo test live_end_to_end_latency -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn live_end_to_end_latency() {
        use crate::{brain, chunker::Chunker, claude, stt_live, tts_live, ws};
        let wav = std::fs::read(std::env::var("ONE_TEST_WAV").expect("set ONE_TEST_WAV")).unwrap();
        let pcm = &wav[wav.windows(4).position(|w| w == b"data").unwrap() + 8..];
        let samples: Vec<f32> = pcm.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect();
        // trim the file's own trailing silence so "end of speech" is the real end
        let last = samples.iter().rposition(|s| s.abs() > 0.02).unwrap_or(samples.len() - 1);
        let speech = &samples[..=last];
        let key = crate::secret::load_key().expect("ElevenLabs key");
        let dir = std::env::temp_dir().join("one-desktop-live-latency");
        std::fs::create_dir_all(&dir).unwrap();
        // warm Claude, as the app has it after unmute
        let mut w = brain::spawn(brain::WarmKey { cwd: dir.to_string_lossy().into(), session: None, lockdown: false, gen: 0 }).unwrap();
        // live STT
        let mut sock = ws::connect(&stt_live::url(), &key).await.expect("STT socket");
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let (htx, mut hrx) = tokio::sync::mpsc::unbounded_channel();
        let stt = tokio::spawn(async move { stt_live::run(&mut sock, rx, &tokio_util::sync::CancellationToken::new(), move |h| drop(htx.send((Instant::now(), h)))).await });
        for c in speech.chunks(1600) {
            if tx.send(stt_live::Cmd::Audio(stt_live::pcm16(c))).is_err() {
                while let Ok((_, h)) = hrx.try_recv() {
                    println!("speech-to-text said: {h:?}");
                }
                panic!("the speech-to-text socket closed early");
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let speech_end = Instant::now();
        // the trailing silence the mic would send; the app commits after 600 ms if the model has not
        let silence = vec![0f32; 1600];
        let mut committed = None;
        for i in 0..20 {
            if i == 6 {
                tx.send(stt_live::Cmd::Commit).unwrap();
            } else {
                tx.send(stt_live::Cmd::Audio(stt_live::pcm16(&silence))).unwrap();
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
            while let Ok((t, h)) = hrx.try_recv() {
                if let stt_live::Heard::Committed(text) = h {
                    committed.get_or_insert((t, text));
                }
            }
            if committed.is_some() {
                break;
            }
        }
        drop(tx);
        let _ = stt.await;
        let (t_commit, text) = committed.expect("a committed transcript");
        println!("heard: {text:?}");
        let mut clock = Clock::new(Some(speech_end), t_commit);
        // Claude
        w.send(&text).await.unwrap();
        let mut chunker = Chunker::new();
        let first_chunk = loop {
            let l = w.lines.next_line().await.unwrap().expect("claude line");
            if let claude::StreamItem::TextDelta(t) = claude::parse_line(&l) {
                clock.text(Instant::now());
                if let Some(c) = chunker.push(&t).into_iter().next() {
                    break c;
                }
            }
        };
        // live voice
        let voice = crate::eleven::resolve_voice(&crate::eleven::client().unwrap(), &key, "").await.unwrap();
        let (vtx, vrx) = tokio::sync::mpsc::unbounded_channel();
        vtx.send(first_chunk).unwrap();
        drop(vtx);
        let first_audio = std::sync::Mutex::new(None);
        let notice = std::sync::Mutex::new(None);
        let plan = tts_live::Plan { key, voice: voice.clone(), fallback_voice: voice, lang: None };
        let spoken = tts_live::speak(plan, vrx, tokio_util::sync::CancellationToken::new(), |_| {
            first_audio.lock().unwrap().get_or_insert(Instant::now());
        }, |m| {
            notice.lock().unwrap().get_or_insert(m);
        }).await;
        w.kill().await;
        match first_audio.into_inner().unwrap() {
            Some(t) => {
                let l = clock.audio(t).unwrap();
                println!("LATENCY total {} ms = speech-to-text {} ms + claude {} ms + voice {} ms (target {TARGET_MS} ms)", l.total_ms, l.stt_ms, l.claude_ms, l.voice_ms);
                assert!(l.under_target, "over the 1.5 s target: {l:?}");
            }
            None => {
                let at = clock.first_text.unwrap();
                println!(
                    "VOICE UNAVAILABLE ({spoken:?}, {:?}). Measured without it: speech-to-text {} ms + claude first words {} ms = {} ms",
                    notice.into_inner().unwrap(),
                    ms(t_commit - speech_end),
                    ms(at - t_commit),
                    ms(at - speech_end)
                );
            }
        }
    }

    #[test]
    fn over_the_target_is_flagged_and_a_typed_turn_starts_at_send() {
        let t0 = Instant::now();
        let mut c = Clock::new(None, t0);
        c.text(t0 + Duration::from_millis(900));
        let l = c.audio(t0 + Duration::from_millis(1600)).unwrap();
        assert_eq!((l.total_ms, l.stt_ms, l.under_target), (1600, 0, false));
        // audio without any text event (a canned line) still measures
        let mut c = Clock::new(Some(t0), t0 + Duration::from_millis(300));
        assert_eq!(c.audio(t0 + Duration::from_millis(500)).unwrap().voice_ms, 0);
        // a speech end "after" the transcript (clock skew) cannot produce nonsense
        let c = Clock::new(Some(t0 + Duration::from_secs(9)), t0);
        assert_eq!(c.speech_end, t0);
    }
}
