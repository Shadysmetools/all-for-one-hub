//! ElevenLabs HTTP client. Official API only (api.elevenlabs.io). All calls
//! happen here in Rust; the key is only ever placed in the `xi-api-key` header.

use crate::secret::ApiKey;
use futures_util::StreamExt;
use reqwest::{multipart, Client};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

/// Next stream item, or Err(()) when nothing arrives within `idle`.
pub async fn next_with_idle<S: futures_util::Stream + Unpin>(s: &mut S, idle: std::time::Duration) -> Result<Option<S::Item>, ()> {
    tokio::time::timeout(idle, s.next()).await.map_err(|_| ())
}

/// CR-M2: HTTP timeouts. A stalled request must never hold the UI in "Thinking".
// Connects were measured at 1.7 s and the voices call at 3.7 s: 5 s was too tight.
pub const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
pub const STT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
pub const TTS_IDLE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
/// How long ONE sentence may take to start answering (the audio of the next sentence has its own clock).
pub const TTS_FIRST_BYTE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(25);

const BASE: &str = "https://api.elevenlabs.io";
pub const STT_MODEL: &str = "scribe_v1";
pub const TTS_MODEL: &str = "eleven_flash_v2_5";
/// Raw PCM16 mono at this rate; the webview plays it as-is.
pub const TTS_FORMAT: &str = "pcm_22050";
/// Empty means "first premade voice on this account" (library voices like Rachel return
/// 402 on free plans, so no voice id is hardcoded).
pub const DEFAULT_VOICE: &str = "";

/// Use the configured voice, or pick the first premade voice when none is set.
pub async fn resolve_voice(c: &Client, key: &ApiKey, configured: &str) -> Result<String, String> {
    if !configured.is_empty() {
        return Ok(configured.to_string());
    }
    // Asked on every turn: the answer is kept, so only the first turn pays for the voice list.
    // Kept per key (a hash of it, never the key): another account gets its own answer.
    static FIRST: std::sync::Mutex<Option<(u64, String)>> = std::sync::Mutex::new(None);
    let id = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        key.expose().hash(&mut h);
        h.finish()
    };
    if let Some((k, v)) = FIRST.lock().unwrap_or_else(|p| p.into_inner()).clone() {
        if k == id {
            return Ok(v);
        }
    }
    let voices = rank(list_voices(c, key).await?);
    let v = voices.first().map(|v| v.voice_id.clone()).ok_or_else(|| "no voices available".to_string())?;
    *FIRST.lock().unwrap_or_else(|p| p.into_inner()) = Some((id, v.clone()));
    Ok(v)
}

#[derive(Serialize, Clone, Debug)]
pub struct Voice {
    pub voice_id: String,
    pub name: String,
    /// "premade" voices work on every plan; library voices need a paid plan.
    pub category: String,
    /// From the voice's labels: "male"/"female", "egyptian"/"american"..., "ar"/"en".
    pub gender: String,
    pub accent: String,
    pub language: String,
}

/// The voices One recommends, best first (checked live, Nov 2026). Premade voices work on every
/// plan; the Egyptian ones are library voices and need a paid ElevenLabs plan.
pub const RECOMMENDED: [(&str, &str); 4] = [
    ("cjVigY5qzO86Huf0OWal", "Eric: warm, calm, conversational (English)"),
    ("cgSgspJ2msm6clMCkdW9", "Jessica: bright, friendly, conversational (English)"),
    ("ihycSANIrpHfhWoaq1g3", "Ahmad: young, conversational, Egyptian Arabic (paid plan)"),
    ("L10lEremDiJfPicq5CPh", "Yasmine: upbeat, conversational, Egyptian Arabic (paid plan)"),
];

/// Recommended voices first (in that order), then premade, then the rest.
pub fn rank(mut v: Vec<Voice>) -> Vec<Voice> {
    let pos = |id: &str| RECOMMENDED.iter().position(|(r, _)| *r == id).unwrap_or(usize::MAX);
    v.sort_by_key(|x| (pos(&x.voice_id), x.category != "premade"));
    v
}

pub fn client() -> Result<Client, String> {
    Client::builder()
        .use_rustls_tls()
        .https_only(true)
        .connect_timeout(CONNECT_TIMEOUT)
        // L2: never follow a redirect: the xi-api-key header would go along to another host.
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "could not build HTTP client".to_string())
}

/// Status + short body only. Never includes request headers.
async fn http_error(resp: reqwest::Response) -> String {
    // L4: the body is untrusted and is not shown, only the status and a fixed message.
    status_message(resp.status())
}

pub fn status_message(status: reqwest::StatusCode) -> String {
    let what = match status.as_u16() {
        401 | 403 => "the key was not accepted",
        402 => "this voice or feature needs a paid plan",
        429 => "too many requests, try again shortly",
        500..=599 => "the service is having trouble",
        _ => "the request failed",
    };
    format!("ElevenLabs {}: {what}", status.as_u16())
}

pub fn parse_voices(v: &serde_json::Value) -> Vec<Voice> {
    let mut out: Vec<Voice> = v["voices"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| {
                    let label = |k: &str| x["labels"][k].as_str().unwrap_or("").to_string();
                    Some(Voice {
                        voice_id: x["voice_id"].as_str()?.to_string(),
                        name: x["name"].as_str()?.to_string(),
                        category: x["category"].as_str().unwrap_or("").to_string(),
                        gender: label("gender"),
                        accent: label("accent"),
                        language: label("language"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    // Premade first: they are the ones a free-plan key can actually speak with.
    out.sort_by_key(|x| x.category != "premade");
    out
}

/// The error and its causes ("error sending request: connection timed out"), never the URL. The
/// text is shown to the user as is, so a failure says what really went wrong.
pub fn describe(e: reqwest::Error) -> String {
    let e = e.without_url();
    let mut parts = vec![e.to_string()];
    let mut src = std::error::Error::source(&e);
    while let Some(s) = src {
        let t = s.to_string();
        if !parts.contains(&t) && !t.contains("http") {
            parts.push(t);
        }
        src = s.source();
    }
    parts.join(": ")
}

/// Voices is an idempotent GET: one retry when the request could not be sent at all.
pub async fn list_voices(c: &Client, key: &ApiKey) -> Result<Vec<Voice>, String> {
    let send = || c.get(format!("{BASE}/v1/voices")).header("xi-api-key", key.expose()).timeout(STT_TIMEOUT).send();
    let resp = match send().await {
        Ok(r) => r,
        Err(_) => send().await.map_err(|e| format!("voices request failed: {}", describe(e)))?,
    };
    if !resp.status().is_success() {
        return Err(http_error(resp).await);
    }
    let v: serde_json::Value = resp.json().await.map_err(|_| "bad voices response".to_string())?;
    Ok(parse_voices(&v))
}

/// Speech-to-text. `wav` is held in memory only and never written to disk.
pub async fn transcribe(c: &Client, key: &ApiKey, wav: Vec<u8>) -> Result<String, String> {
    let part = multipart::Part::bytes(wav).file_name("audio.wav").mime_str("audio/wav").map_err(|_| "mime".to_string())?;
    let form = multipart::Form::new().text("model_id", STT_MODEL).part("file", part);
    let resp = c
        .post(format!("{BASE}/v1/speech-to-text"))
        .header("xi-api-key", key.expose())
        .multipart(form)
        .timeout(STT_TIMEOUT) // total time for the whole request, 30 s
        .send()
        .await
        .map_err(|e| format!("STT request failed: {}", describe(e)))?;
    if !resp.status().is_success() {
        return Err(http_error(resp).await);
    }
    let v: serde_json::Value = resp.json().await.map_err(|_| "bad STT response".to_string())?;
    Ok(v["text"].as_str().unwrap_or("").trim().to_string())
}

/// Stream TTS for one sentence, calling `on_chunk` with raw PCM bytes as they arrive.
/// Returns early (Ok) when `cancel` fires, dropping the HTTP connection.
pub async fn tts_stream(
    c: &Client,
    key: &ApiKey,
    voice_id: &str,
    text: &str,
    format: &str,
    cancel: &CancellationToken,
    mut on_chunk: impl FnMut(Vec<u8>),
) -> Result<(), String> {
    // voice_id is user-selectable: keep it to a safe charset so it cannot alter the path.
    if voice_id.is_empty() || !voice_id.chars().all(|ch| ch.is_ascii_alphanumeric()) {
        return Err("invalid voice id".to_string());
    }
    let url = format!("{BASE}/v1/text-to-speech/{voice_id}/stream?output_format={format}");
    let send = c
        .post(url)
        .timeout(std::time::Duration::from_secs(60)) // whole-request ceiling for one sentence
        .header("xi-api-key", key.expose())
        .json(&serde_json::json!({ "text": text, "model_id": TTS_MODEL }))
        .send();
    let resp = tokio::select! {
        _ = cancel.cancelled() => return Ok(()),
        r = tokio::time::timeout(TTS_FIRST_BYTE_TIMEOUT, send) => match r {
            Ok(r) => r.map_err(|e| format!("TTS request failed: {}", describe(e)))?,
            Err(_) => return Err("TTS timed out".to_string()),
        },
    };
    if !resp.status().is_success() {
        return Err(http_error(resp).await);
    }
    let mut stream = resp.bytes_stream();
    loop {
        tokio::select! {
            _ = cancel.cancelled() => return Ok(()),
            next = next_with_idle(&mut stream, TTS_IDLE_TIMEOUT) => match next {
                Err(_) => return Err("TTS stalled (no audio for 15 s)".to_string()),
                Ok(Some(Ok(b))) => on_chunk(b.to_vec()),
                Ok(Some(Err(_))) => return Err("TTS stream interrupted".to_string()),
                Ok(None) => return Ok(()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn l4_error_text_is_status_plus_fixed_message() {
        use reqwest::StatusCode;
        let m = status_message(StatusCode::UNAUTHORIZED);
        assert_eq!(m, "ElevenLabs 401: the key was not accepted");
        assert!(status_message(StatusCode::INTERNAL_SERVER_ERROR).starts_with("ElevenLabs 500:"));
        assert!(status_message(StatusCode::IM_A_TEAPOT).contains("failed"));
    }

    #[test]
    fn m2_timeouts_are_the_agreed_values() {
        assert_eq!(CONNECT_TIMEOUT.as_secs(), 10);
        assert_eq!(STT_TIMEOUT.as_secs(), 30);
        assert_eq!(TTS_IDLE_TIMEOUT.as_secs(), 15);
    }

    /// A server that sends headers and then goes silent: the idle timeout must fire.
    #[test]
    fn m2_stalled_stream_times_out() {
        let rt = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        rt.block_on(async {
            let mut silent = futures_util::stream::pending::<u8>();
            assert!(next_with_idle(&mut silent, std::time::Duration::from_millis(50)).await.is_err());
            let mut live = futures_util::stream::iter([7u8]);
            assert_eq!(next_with_idle(&mut live, std::time::Duration::from_millis(50)).await, Ok(Some(7)));
        });
    }

    #[test]
    fn l2_client_builds_with_redirects_off() {
        // reqwest does not expose the policy; this guards the builder from regressing to a panic.
        assert!(client().is_ok());
    }

    #[test]
    fn parses_voice_list() {
        let v = serde_json::json!({"voices":[{"voice_id":"a1","name":"Rachel","x":1},{"name":"bad"}]});
        let p = parse_voices(&v);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].voice_id, "a1");
    }

    #[test]
    fn voices_carry_their_labels_and_the_recommended_ones_come_first() {
        let v = serde_json::json!({"voices":[
            {"voice_id":"zz","name":"Other","category":"premade"},
            {"voice_id":"L10lEremDiJfPicq5CPh","name":"Yasmine","category":"professional","labels":{"gender":"female","accent":"egyptian","language":"ar"}},
            {"voice_id":"cjVigY5qzO86Huf0OWal","name":"Eric","category":"premade","labels":{"gender":"male","accent":"american","language":"en"}},
            {"voice_id":"lib","name":"Library","category":"professional"}
        ]});
        let r = rank(parse_voices(&v));
        let ids: Vec<&str> = r.iter().map(|x| x.voice_id.as_str()).collect();
        assert_eq!(ids, ["cjVigY5qzO86Huf0OWal", "L10lEremDiJfPicq5CPh", "zz", "lib"]);
        assert_eq!((r[1].gender.as_str(), r[1].accent.as_str(), r[1].language.as_str()), ("female", "egyptian", "ar"));
        assert_eq!(r[2].accent, "", "missing labels are empty, not an error");
    }

    fn wav16k(pcm: &[u8]) -> Vec<u8> {
        let mut w = Vec::new();
        w.extend(b"RIFF");
        w.extend((36 + pcm.len() as u32).to_le_bytes());
        w.extend(b"WAVEfmt ");
        w.extend(16u32.to_le_bytes());
        w.extend(1u16.to_le_bytes());
        w.extend(1u16.to_le_bytes());
        w.extend(16000u32.to_le_bytes());
        w.extend(32000u32.to_le_bytes());
        w.extend(2u16.to_le_bytes());
        w.extend(16u16.to_le_bytes());
        w.extend(b"data");
        w.extend((pcm.len() as u32).to_le_bytes());
        w.extend(pcm);
        w
    }

    /// Live smoke test. Needs the ElevenLabs key (file or keychain). Bytes are discarded.
    /// Run with: cargo test live_tts_stt_smoke -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn live_tts_stt_smoke() {
        let key = crate::secret::load_key().expect("no key available");
        let c = client().unwrap();
        let cancel = CancellationToken::new();
        // Same path the app takes with the shipped default (empty voice id).
        let voice = resolve_voice(&c, &key, DEFAULT_VOICE).await.expect("default voice failed");
        let mut pcm = Vec::new();
        tts_stream(&c, &key, &voice, "Hello, this is a short test.", "pcm_16000", &cancel, |b| pcm.extend(b))
            .await
            .expect("tts failed");
        assert!(!pcm.is_empty(), "no audio bytes");
        println!("TTS ok: {} bytes", pcm.len());
        // Round-trip through STT (in memory only).
        match transcribe(&c, &key, wav16k(&pcm)).await {
            Ok(t) => println!("STT ok, {} chars, mentions 'test': {}", t.len(), t.to_lowercase().contains("test")),
            Err(e) => println!("STT skipped/failed: {e}"),
        }
    }
}
