//! Live speech-to-text over the ElevenLabs realtime socket (`scribe_v3_live`). Audio is sent
//! ONLY while the local VAD hears speech (from speech start with its pre-roll, through the
//! trailing silence, to the commit). Nothing is sent while the user is silent, muted or away.
//!
//! The server sends partial text as the user speaks (shown live, never stored). The app commits
//! (`commit_strategy=manual`) when its VAD ends the utterance, so every committed text answers
//! exactly one Commit. Fast end of turn: when the partials' `turn_probability` says the user has
//! finished and they have paused briefly (`early_end`), the app ends the utterance at once.

use crate::secret::ApiKey;
use crate::ws::{self, Ws};
use base64::Engine;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub const MODEL: &str = "scribe_v3_live";
/// The socket is closed when no audio was sent for this long (the next speech reopens it).
pub const IDLE_CLOSE: std::time::Duration = std::time::Duration::from_secs(20);
/// A turn may end early when the model is at least this sure the user has finished...
pub const EARLY_TURN_PROB: f64 = 0.8;
/// ...and the user has been quiet at least this long.
pub const EARLY_SILENCE_MS: usize = 200;

pub fn url() -> String {
    format!(
        "{}v1/speech-to-text/realtime?model_id={MODEL}&audio_format=pcm_16000&commit_strategy=manual&multilang_mode=true",
        ws::HOST
    )
}

/// What the server told us.
#[derive(Debug, PartialEq)]
pub enum Heard {
    Partial { text: String, turn_prob: Option<f64> },
    Committed(String),
    Error(String),
    Other,
}

pub fn parse(v: &Value) -> Heard {
    let text = || v.get("text").and_then(Value::as_str).unwrap_or("").trim().to_string();
    match v.get("message_type").and_then(Value::as_str) {
        Some("partial_transcript") => Heard::Partial { text: text(), turn_prob: v.get("turn_probability").and_then(Value::as_f64) },
        Some("committed_transcript") => Heard::Committed(text()),
        Some("session_started") | Some("committed_transcript_with_timestamps") | Some("committed_transcript_entities") => Heard::Other,
        Some(t) if t.contains("error") || t == "rate_limited" || t == "quota_exceeded" || t == "commit_throttled" || t.starts_with("auth") => {
            Heard::Error(ws::server_error(v.get("error").or(v.get("message")).and_then(Value::as_str).unwrap_or(t)))
        }
        _ => Heard::Other,
    }
}

/// 16 kHz f32 samples to the PCM16LE the socket takes.
pub fn pcm16(samples: &[f32]) -> Vec<u8> {
    samples.iter().flat_map(|s| ((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes()).collect()
}

pub fn audio_message(pcm: &[u8], commit: bool) -> Value {
    let mut m = json!({
        "message_type": "input_audio_chunk",
        "audio_base_64": base64::engine::general_purpose::STANDARD.encode(pcm),
        "sample_rate": 16000,
    });
    if commit {
        m["commit"] = json!(true);
    }
    m
}

/// End the utterance now instead of waiting for the full end-of-speech silence: the model is
/// confident the user has finished and they have been quiet briefly. Pure, unit-tested.
pub fn early_end(silence_ms: usize, turn_prob: Option<f64>) -> bool {
    silence_ms >= EARLY_SILENCE_MS && turn_prob.is_some_and(|p| p >= EARLY_TURN_PROB)
}

pub enum Cmd {
    Audio(Vec<u8>),
    Commit,
}

/// A running socket. Dropping it (or `close`) ends the session.
pub struct Live {
    tx: mpsc::UnboundedSender<Cmd>,
    pub id: u64,
    /// Fired when the owner drops this socket (mute, Stop, call pause, barge-in): the task stops
    /// at once and audio still queued is thrown away, never sent.
    stop: CancellationToken,
}

impl Drop for Live {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

impl Live {
    /// False when the socket task has ended (send failed).
    pub fn send(&self, c: Cmd) -> bool {
        self.tx.send(c).is_ok()
    }
}

/// Open a socket and run it in the background. `on_heard` gets every server event plus a final
/// `Heard::Error` when the socket fails; `on_end` runs when the task stops for any reason.
pub fn start(key: ApiKey, id: u64, on_heard: impl Fn(u64, Heard) + Send + Sync + 'static) -> Live {
    let (tx, rx) = mpsc::unbounded_channel();
    let stop = CancellationToken::new();
    let task_stop = stop.clone();
    tauri::async_runtime::spawn(async move {
        let addr = url();
        let conn = tokio::select! {
            biased;
            _ = task_stop.cancelled() => return, // dropped while still connecting: nothing is sent
            c = ws::connect(&addr, &key) => c,
        };
        match conn {
            Ok(mut sock) => run(&mut sock, rx, &task_stop, |h| on_heard(id, h)).await,
            Err(e) => on_heard(id, Heard::Error(e)),
        }
        on_heard(id, Heard::Error(String::new())); // closed: the owner drops this socket
    });
    Live { tx, id, stop }
}

pub async fn run(sock: &mut Ws, mut rx: mpsc::UnboundedReceiver<Cmd>, stop: &CancellationToken, on_heard: impl Fn(Heard)) {
    loop {
        tokio::select! {
            biased;
            // First, always: once the owner let go, queued audio is dropped, not sent.
            _ = stop.cancelled() => { let _ = sock.close(None).await; return; }
            c = tokio::time::timeout(IDLE_CLOSE, rx.recv()) => {
                let msg = match c {
                    Err(_) | Ok(None) => { let _ = sock.close(None).await; return; }
                    Ok(Some(Cmd::Audio(pcm))) => audio_message(&pcm, false),
                    Ok(Some(Cmd::Commit)) => audio_message(&[], true),
                };
                if ws::send_json(sock, &msg).await.is_err() {
                    on_heard(Heard::Error("the speech connection dropped".into()));
                    return;
                }
            }
            m = ws::next_json(sock) => match m {
                Ok(Some(v)) => {
                    let h = parse(&v);
                    let stop = matches!(h, Heard::Error(_));
                    on_heard(h);
                    if stop { let _ = sock.close(None).await; return; }
                }
                Ok(None) => return,
                Err(e) => { on_heard(Heard::Error(e)); return; }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_url_asks_for_the_live_model_turn_prediction_and_code_switching() {
        let u = url();
        assert!(ws::allowed_url(&u));
        for p in ["model_id=scribe_v3_live", "audio_format=pcm_16000", "commit_strategy=manual", "multilang_mode=true"] {
            assert!(u.contains(p), "{p}");
        }
        assert!(!u.contains("token") && !u.contains("xi"), "the key is in the header only");
    }

    #[test]
    fn server_messages_parse() {
        assert_eq!(
            parse(&json!({"message_type": "partial_transcript", "text": " check the hub", "turn_probability": 0.42})),
            Heard::Partial { text: "check the hub".into(), turn_prob: Some(0.42) }
        );
        assert_eq!(parse(&json!({"message_type": "committed_transcript", "text": "Check the hub logs."})), Heard::Committed("Check the hub logs.".into()));
        assert_eq!(parse(&json!({"message_type": "committed_transcript_with_timestamps", "text": "x"})), Heard::Other, "the same text again; only the plain commit counts");
        assert!(matches!(parse(&json!({"message_type": "quota_exceeded", "error": "over quota"})), Heard::Error(e) if e == "over quota"));
        assert!(matches!(parse(&json!({"message_type": "auth_error", "error": "bad key"})), Heard::Error(_)));
        assert_eq!(parse(&json!({"message_type": "session_started"})), Heard::Other);
    }

    #[test]
    fn audio_goes_out_as_pcm16_base64_and_a_commit_is_an_empty_chunk() {
        let p = pcm16(&[0.0, 1.0, -1.0, 2.0]);
        assert_eq!(p, vec![0, 0, 0xff, 0x7f, 0x01, 0x80, 0xff, 0x7f]);
        let m = audio_message(&p, false);
        assert_eq!(m["message_type"], "input_audio_chunk");
        assert_eq!(m["sample_rate"], 16000);
        assert!(m.get("commit").is_none());
        let c = audio_message(&[], true);
        assert_eq!(c["commit"], true);
        assert_eq!(c["audio_base_64"], "");
    }

    #[test]
    fn a_turn_ends_early_only_when_the_model_is_sure_and_the_user_paused() {
        assert!(early_end(250, Some(0.9)));
        assert!(!early_end(100, Some(0.99)), "a breath between words is not the end");
        assert!(!early_end(500, Some(0.5)), "a long pause mid-thought is not the end either");
        assert!(!early_end(500, None));
    }

    /// SEC-H1: after Mute/Stop/pause the owner drops the socket; audio still in the queue must
    /// never reach the server, even when it was queued before the drop.
    #[tokio::test]
    async fn sec_h1_queued_audio_is_never_sent_after_the_socket_is_dropped() {
        use futures_util::StreamExt;
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = format!("ws://{}", l.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (s, _) = l.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(s).await.unwrap();
            let mut audio = 0;
            while let Some(Ok(m)) = ws.next().await {
                if m.to_text().unwrap_or("").contains("input_audio_chunk") {
                    audio += 1;
                }
            }
            audio
        });
        let (mut sock, _) = tokio_tungstenite::connect_async(addr).await.unwrap();
        let (tx, rx) = mpsc::unbounded_channel();
        for _ in 0..50 {
            tx.send(Cmd::Audio(pcm16(&[0.2; 1600]))).unwrap(); // queued while "connecting"
        }
        let stop = CancellationToken::new();
        stop.cancel(); // the user muted before the socket got to send anything
        run(&mut sock, rx, &stop, |_| {}).await;
        drop(sock);
        assert_eq!(server.await.unwrap(), 0, "no audio may leave after mute");
    }

    #[tokio::test]
    async fn the_socket_task_sends_audio_and_reports_partials_and_commits() {
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::tungstenite::Message;
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = format!("ws://{}", l.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let (s, _) = l.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(s).await.unwrap();
            let mut n = 0;
            while let Some(Ok(m)) = ws.next().await {
                let v: Value = serde_json::from_str(m.to_text().unwrap_or("{}")).unwrap_or_default();
                n += 1;
                if v.get("commit").is_some() {
                    ws.send(Message::text(json!({"message_type": "committed_transcript", "text": "hello there"}).to_string())).await.unwrap();
                } else {
                    ws.send(Message::text(json!({"message_type": "partial_transcript", "text": "hello", "turn_probability": 0.3}).to_string())).await.unwrap();
                }
                if n == 2 {
                    break;
                }
            }
        });
        let (mut sock, _) = tokio_tungstenite::connect_async(addr).await.unwrap();
        let (tx, rx) = mpsc::unbounded_channel();
        tx.send(Cmd::Audio(pcm16(&[0.1; 160]))).unwrap();
        tx.send(Cmd::Commit).unwrap();
        let heard = std::sync::Mutex::new(Vec::new());
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), run(&mut sock, rx, &CancellationToken::new(), |h| heard.lock().unwrap().push(h))).await;
        server.await.unwrap();
        let h = heard.into_inner().unwrap();
        assert!(h.contains(&Heard::Partial { text: "hello".into(), turn_prob: Some(0.3) }));
        assert!(h.contains(&Heard::Committed("hello there".into())));
    }
}
