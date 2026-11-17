//! Streaming speech for one reply. Claude's text arrives chunk by chunk (sentences, the first one
//! cut at a clause) and goes straight into ONE ElevenLabs Text-to-Dialogue WebSocket with the
//! `eleven_v4_turbo` model, so One starts speaking ~0.2 s after the first chunk instead of
//! waiting for a whole sentence request. The socket is opened when the turn starts, in parallel
//! with Claude, so its handshake is hidden.
//!
//! Fallback: when the socket cannot be used (refused, quota, a library voice on a free plan),
//! the reply is spoken with the older per-sentence HTTP stream (`eleven_flash_v2_5`). Text that
//! was sent to the socket but never voiced is replayed, so nothing is skipped silently.

use crate::eleven;
use crate::secret::ApiKey;
use crate::ws::{self, Ws};
use base64::Engine;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// The most natural real-time model in the current ElevenLabs docs (Nov 2026): ~100 ms median,
/// 90+ languages including Arabic, served on the Text-to-Dialogue socket.
pub const LIVE_MODEL: &str = "eleven_v4_turbo";
/// Lower is livelier, higher is steadier. v4 on this socket takes only `stability` (and
/// `clone_strength`): similarity, style, speaker boost and speed do not exist for it.
pub const STABILITY: f64 = 0.4;
/// Send this when nothing else went out for a while (the server closes after 20 s idle).
const KEEP_ALIVE: std::time::Duration = std::time::Duration::from_secs(10);
/// No audio for this long after the text was flushed: the socket is stuck.
const AUDIO_STALL: std::time::Duration = std::time::Duration::from_secs(15);

pub fn url(lang: Option<&str>) -> String {
    // sync_alignment: each audio chunk says which characters it voiced, so after a failure only
    // the text not spoken yet is replayed.
    let mut u = format!("{}v1/text-to-dialogue/stream-input?model_id={LIVE_MODEL}&output_format={}&sync_alignment=true", ws::HOST, eleven::TTS_FORMAT);
    if let Some(l) = lang.filter(|l| l.chars().all(|c| c.is_ascii_lowercase()) && l.len() <= 3) {
        u.push_str("&language_code=");
        u.push_str(l);
    }
    u
}

pub fn first_message(voice: &str) -> Value {
    json!({ "voices": [voice], "voice_settings": { "stability": STABILITY } })
}

/// One piece of reply text. A trailing space keeps words from gluing across pieces.
pub fn input_message(text: &str, voice: &str, new_turn: bool) -> Value {
    json!({ "inputs": [{ "text": format!("{text} "), "voice_id": voice, "new_turn": new_turn }] })
}

#[derive(Debug, PartialEq)]
pub enum Server {
    /// Audio, and how many characters of the sent text it voiced (0 when unknown).
    Audio(Vec<u8>, usize),
    Final,
    /// `paid`: the plan does not allow this voice (try the fallback voice).
    Error { text: String, paid: bool },
    Other,
}

pub fn parse_server(v: &Value) -> Server {
    if let Some(e) = v.get("error").and_then(Value::as_str) {
        let msg = v.get("message").and_then(Value::as_str).unwrap_or(e);
        return Server::Error { text: ws::server_error(msg), paid: e == "payment_required" };
    }
    if let Some(a) = v.get("audio").and_then(Value::as_str).filter(|a| !a.is_empty()) {
        let chars = v.get("alignment").and_then(|a| a.get("chars")).and_then(Value::as_array).map_or(0, |c| c.len());
        return match base64::engine::general_purpose::STANDARD.decode(a) {
            Ok(b) => Server::Audio(b, chars),
            Err(_) => Server::Other,
        };
    }
    if v.get("is_final").and_then(Value::as_bool) == Some(true) {
        return Server::Final;
    }
    Server::Other
}

/// More Arabic letters than Latin ones: the reply will be Arabic (One answers in the language
/// it was spoken to).
pub fn is_arabic(text: &str) -> bool {
    let (mut ar, mut lat) = (0usize, 0usize);
    for c in text.chars() {
        if ('\u{0600}'..='\u{06FF}').contains(&c) || ('\u{0750}'..='\u{077F}').contains(&c) || ('\u{FB50}'..='\u{FDFF}').contains(&c) || ('\u{FE70}'..='\u{FEFF}').contains(&c) {
            if c.is_alphabetic() {
                ar += 1;
            }
        } else if c.is_ascii_alphabetic() {
            lat += 1;
        }
    }
    ar > lat
}

/// Which voice speaks a reply to `prompt`: the Arabic voice for Arabic (when one is set), else
/// the main voice. Returns (voice, language code for the model).
pub fn pick_voice(prompt: &str, main: &str, arabic: &str) -> (String, Option<&'static str>) {
    if is_arabic(prompt) {
        let v = if arabic.is_empty() { main } else { arabic };
        (v.to_string(), Some("ar"))
    } else {
        (main.to_string(), None)
    }
}

/// Drop the pieces at the front of `pending` that are fully voiced (`voiced` characters, counted
/// from the start of the first piece; each piece was sent with a trailing space).
pub fn prune_voiced(pending: &mut Vec<String>, voiced: &mut usize) {
    while let Some(first) = pending.first() {
        let n = first.chars().count() + 1;
        if *voiced < n {
            break;
        }
        *voiced -= n;
        pending.remove(0);
    }
}

/// Voices the plan refused this run (a library voice on a free plan): not tried again.
fn refused() -> std::sync::MutexGuard<'static, std::collections::HashSet<String>> {
    static R: std::sync::OnceLock<std::sync::Mutex<std::collections::HashSet<String>>> = std::sync::OnceLock::new();
    R.get_or_init(Default::default).lock().unwrap_or_else(|p| p.into_inner())
}

/// The calm, one-time notice shown when ElevenLabs has no credits / refuses the key.
pub const BUILTIN_NOTICE: &str = "Using the built-in voice \u{2013} premium voice needs credits";
/// ...and when it simply could not be reached (a network problem, not money).
pub const BUILTIN_NOTICE_OFFLINE: &str = "Using the built-in voice \u{2013} premium voice is not reachable right now";
/// After an account-level failure (no credits, key refused) ElevenLabs is not tried again for
/// this long: a free plan with 0 credits would otherwise fail, and delay, every sentence.
const DOWN_FOR: std::time::Duration = std::time::Duration::from_secs(300);

fn down_until() -> std::sync::MutexGuard<'static, Option<std::time::Instant>> {
    static D: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);
    D.lock().unwrap_or_else(|p| p.into_inner())
}

/// True while ElevenLabs is known not to work (no credits, refused key).
pub fn eleven_down() -> bool {
    matches!(*down_until(), Some(t) if std::time::Instant::now() < t)
}

/// The account cannot speak (no credits / key refused): use the built-in voice for a while.
pub fn mark_down() {
    *down_until() = Some(std::time::Instant::now() + DOWN_FOR);
}

/// A failed voice lookup: only an account-level failure starts the backoff. A network blip
/// does not; the next reply tries the premium voice again.
pub fn note_lookup_failure(reason: &str) {
    if is_account_error(reason) {
        mark_down();
    }
}

fn mark_up() {
    *down_until() = None;
    NOTICED.store(false, std::sync::atomic::Ordering::SeqCst);
}

static NOTICED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static NOTICED_TEXT_AR: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static NOTICED_TEXT_EN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn once(flag: &std::sync::atomic::AtomicBool, text: String, on_notice: &mut (impl FnMut(String) + Send)) {
    if !flag.swap(true, std::sync::atomic::Ordering::SeqCst) {
        on_notice(text);
    }
}

/// The premium voice is out and the built-in one takes over. Says WHY (credits or reach), once;
/// when this PC has no built-in voice for the language it says "text only" instead, once, and
/// reports false so nothing is spawned for the speech.
async fn start_builtin(lang: Option<&str>, account: bool, on_notice: &mut (impl FnMut(String) + Send)) -> bool {
    if crate::winvoice::voice_available(lang).await {
        let text = if account { BUILTIN_NOTICE } else { BUILTIN_NOTICE_OFFLINE };
        once(&NOTICED, text.to_string(), on_notice);
        return true;
    }
    let ar = lang == Some("ar");
    once(if ar { &NOTICED_TEXT_AR } else { &NOTICED_TEXT_EN }, no_voice_notice(ar), on_notice);
    false
}

pub fn no_voice_notice(arabic: bool) -> String {
    format!("No built-in voice for {} \u{2013} text only", if arabic { "Arabic" } else { "English" })
}

/// Plan, quota, key and payment failures (by status code or error kind, not by loose words): the
/// account cannot speak, so retrying over HTTP is pointless and the built-in voice takes over.
pub fn is_account_error(reason: &str) -> bool {
    let r = reason.to_lowercase();
    const CODES: [&str; 4] = ["401", "402", "403", "429"];
    const KINDS: [&str; 7] = ["quota_exceeded", "payment_required", "invalid_api_key", "unauthorized", "insufficient_credits", "subscription_required", "needs_paid_plan"];
    const PHRASES: [&str; 6] = ["free users cannot", "paid plan", "0 credits", "out of credits", "exceeds your quota", "key was not accepted"];
    r.split(|c: char| !c.is_ascii_alphanumeric() && c != '_').any(|t| CODES.contains(&t) || KINDS.contains(&t)) || PHRASES.iter().any(|p| r.contains(p))
}

/// What the speaker needs. `fallback_voice` is used when the plan refuses `voice`.
#[derive(Clone)]
pub struct Plan {
    pub key: ApiKey,
    pub voice: String,
    pub fallback_voice: String,
    pub lang: Option<&'static str>,
}

/// How speaking went, for the caller's "I couldn't speak that part" notice.
#[derive(Debug, PartialEq)]
pub enum Spoken {
    Done,
    Cancelled,
    /// No voice could speak it (e.g. no built-in Arabic voice): the text stays text, by design.
    TextOnly,
    /// Some or all text could not be voiced; the reason is safe to log.
    Failed(String),
}

/// Speak everything that arrives on `rx` until it closes. `on_audio` gets raw PCM16 chunks.
/// `on_notice` is called (at most once) when the live voice failed and the fallback took over.
pub async fn speak(
    plan: Plan,
    mut rx: mpsc::UnboundedReceiver<String>,
    cancel: CancellationToken,
    mut on_audio: impl FnMut(Vec<u8>) + Send,
    mut on_notice: impl FnMut(String) + Send,
) -> Spoken {
    let mut pending: Vec<String> = Vec::new(); // sent to the socket, not yet known to be voiced
    if eleven_down() {
        if !start_builtin(plan.lang, true, &mut on_notice).await {
            return Spoken::TextOnly;
        }
        return speak_local(plan.lang, Vec::new(), &mut rx, &cancel, &mut on_audio).await;
    }
    let mut voice = plan.voice.clone();
    if !plan.fallback_voice.is_empty() && refused().contains(&voice) {
        voice = plan.fallback_voice.clone(); // already refused once: no new handshake, no new notice
    }
    for attempt in 0..2 {
        let addr = url(plan.lang);
        let conn = tokio::select! {
            _ = cancel.cancelled() => return Spoken::Cancelled,
            c = ws::connect(&addr, &plan.key) => c,
        };
        let res = match conn {
            Ok(mut sock) => run_socket(&mut sock, &voice, &mut rx, &cancel, &mut on_audio, &mut pending).await,
            Err(e) => SockEnd::Failed { reason: e, paid: false, voiced: false },
        };
        match res {
            SockEnd::Done => {
                mark_up();
                return Spoken::Done;
            }
            SockEnd::Cancelled => return Spoken::Cancelled,
            // The plan does not allow this voice: try the main voice once over the socket.
            SockEnd::Failed { paid: true, voiced: false, .. } if attempt == 0 && voice != plan.fallback_voice && !plan.fallback_voice.is_empty() => {
                refused().insert(voice.clone());
                on_notice("That voice needs a paid ElevenLabs plan, so I'm using the standard voice.".into());
                voice = plan.fallback_voice.clone();
                continue;
            }
            SockEnd::Failed { reason, .. } => {
                // Whatever was sent and not voiced yet is replayed (`pending` is pruned as the
                // alignment says text was spoken; a half-spoken sentence is repeated whole).
                eprintln!("live voice failed: {reason}");
                return after_live_failure(&plan, &reason, pending, &mut rx, &cancel, &mut on_audio, &mut on_notice).await;
            }
        }
    }
    Spoken::Failed("the live voice failed".into())
}

/// The live socket failed. A plan/quota/key problem goes straight to the built-in voice; anything
/// else gets one try over HTTP first. Either way One keeps speaking.
async fn after_live_failure(
    plan: &Plan,
    reason: &str,
    pending: Vec<String>,
    rx: &mut mpsc::UnboundedReceiver<String>,
    cancel: &CancellationToken,
    on_audio: &mut (impl FnMut(Vec<u8>) + Send),
    on_notice: &mut (impl FnMut(String) + Send),
) -> Spoken {
    let mut rest = pending;
    if !is_account_error(reason) {
        let fb_voice = if plan.fallback_voice.is_empty() { plan.voice.clone() } else { plan.fallback_voice.clone() };
        match speak_http(&plan.key, &fb_voice, rest, rx, cancel, &mut *on_audio).await {
            HttpEnd::Done => return Spoken::Done,
            HttpEnd::Cancelled => return Spoken::Cancelled,
            HttpEnd::Failed { reason: r, rest: left } => {
                eprintln!("premium voice over HTTP failed too: {r}");
                rest = left;
            }
        }
    }
    let account = is_account_error(reason);
    if account {
        mark_down();
    }
    if !start_builtin(plan.lang, account, on_notice).await {
        return Spoken::TextOnly;
    }
    speak_local(plan.lang, rest, rx, cancel, on_audio).await
}

/// The free Windows voice: each piece (the replay first, then whatever still arrives) is
/// synthesised and sent on the same audio path as the premium voice.
pub async fn speak_local(
    lang: Option<&'static str>,
    replay: Vec<String>,
    rx: &mut mpsc::UnboundedReceiver<String>,
    cancel: &CancellationToken,
    on_audio: &mut (impl FnMut(Vec<u8>) + Send),
) -> Spoken {
    if !crate::winvoice::voice_available(lang).await {
        return Spoken::TextOnly; // never spawn PowerShell speech for a voice that is not installed
    }
    let mut queue: std::collections::VecDeque<String> = replay.into();
    let mut fails = 0u8;
    loop {
        let piece = match queue.pop_front() {
            Some(p) => p,
            None => match rx.recv().await {
                Some(p) => p,
                None => return if fails == 0 { Spoken::Done } else { Spoken::Failed("some pieces were not spoken".into()) },
            },
        };
        if cancel.is_cancelled() {
            return Spoken::Cancelled;
        }
        match crate::winvoice::synth(&piece, lang, cancel).await {
            Ok(pcm) => {
                fails = 0;
                for chunk in pcm.chunks(32 * 1024) {
                    if cancel.is_cancelled() {
                        return Spoken::Cancelled;
                    }
                    on_audio(chunk.to_vec());
                }
            }
            Err(_) if cancel.is_cancelled() => return Spoken::Cancelled,
            Err(e) => {
                eprintln!("built-in voice failed for one piece: {e}");
                fails += 1;
                if fails >= 2 || e.contains("no suitable voice") {
                    return Spoken::Failed(e);
                }
            }
        }
    }
}

enum SockEnd {
    Done,
    Cancelled,
    Failed { reason: String, paid: bool, voiced: bool },
}

/// Drive one socket: forward text, flush after the first piece (the server otherwise waits for
/// ~40 characters), close when the text ends, and pass audio on until the final frame.
async fn run_socket(
    sock: &mut Ws,
    voice: &str,
    rx: &mut mpsc::UnboundedReceiver<String>,
    cancel: &CancellationToken,
    on_audio: &mut (impl FnMut(Vec<u8>) + Send),
    pending: &mut Vec<String>,
) -> SockEnd {
    let fail = |reason: String, paid: bool, voiced: bool| SockEnd::Failed { reason, paid, voiced };
    if let Err(e) = ws::send_json(sock, &first_message(voice)).await {
        return fail(e, false, false);
    }
    // Text sent on an earlier socket that never got voiced goes first.
    for (i, t) in pending.iter().enumerate() {
        if ws::send_json(sock, &input_message(t, voice, false)).await.is_err() || (i == 0 && ws::send_json(sock, &json!({"flush": true})).await.is_err()) {
            return fail("the voice connection dropped".into(), false, false);
        }
    }
    let (mut text_open, mut voiced) = (true, false);
    let mut voiced_chars = 0usize; // into the first piece of `pending`
    let mut last_out = tokio::time::Instant::now();
    let mut closed_at: Option<tokio::time::Instant> = None;
    loop {
        let keep = last_out + KEEP_ALIVE;
        let stall = closed_at.map(|t| t + AUDIO_STALL);
        tokio::select! {
            _ = cancel.cancelled() => {
                let _ = sock.close(None).await;
                return SockEnd::Cancelled;
            }
            piece = rx.recv(), if text_open => {
                let msg = match piece {
                    Some(t) => {
                        let first = pending.is_empty();
                        pending.push(t.clone());
                        let r = ws::send_json(sock, &input_message(&t, voice, false)).await;
                        if r.is_ok() && first {
                            // the first words go out at once; later ones fill the server's buffer
                            ws::send_json(sock, &json!({"flush": true})).await
                        } else { r }
                    }
                    None => {
                        text_open = false;
                        closed_at = Some(tokio::time::Instant::now());
                        // close_socket flushes what is buffered, sends the last audio, then is_final
                        ws::send_json(sock, &json!({"close_socket": true})).await
                    }
                };
                if let Err(e) = msg {
                    return fail(e, false, voiced);
                }
                last_out = tokio::time::Instant::now();
            }
            _ = tokio::time::sleep_until(keep), if text_open => {
                if ws::send_json(sock, &json!({"keep_alive": true})).await.is_err() {
                    return fail("the voice connection dropped".into(), false, voiced);
                }
                last_out = tokio::time::Instant::now();
            }
            _ = sleep_opt(stall) => return fail("no audio came back".into(), false, voiced),
            m = ws::next_json(sock) => match m {
                Err(e) => return fail(e.clone(), e.to_lowercase().contains("paid") || e.to_lowercase().contains("upgrade"), voiced),
                Ok(None) => return if text_open { fail("the voice connection closed early".into(), false, voiced) } else { SockEnd::Done },
                Ok(Some(v)) => match parse_server(&v) {
                    Server::Audio(b, chars) => {
                        voiced = true;
                        voiced_chars += chars;
                        prune_voiced(pending, &mut voiced_chars);
                        if let Some(t) = closed_at.as_mut() { *t = tokio::time::Instant::now(); }
                        on_audio(b);
                    }
                    Server::Final if !text_open => return SockEnd::Done,
                    Server::Error { text, paid } => return fail(text, paid, voiced),
                    _ => {}
                },
            },
        }
    }
}

async fn sleep_opt(t: Option<tokio::time::Instant>) {
    match t {
        Some(t) => tokio::time::sleep_until(t).await,
        None => std::future::pending().await,
    }
}

enum HttpEnd {
    Done,
    Cancelled,
    /// The first failed piece and everything after it that was not voiced yet.
    Failed { reason: String, rest: Vec<String> },
}

/// The older path: one HTTP stream per piece, in order. The first failure stops it and hands the
/// unspoken text back, so the built-in voice can say it.
async fn speak_http(
    key: &ApiKey,
    voice: &str,
    replay: Vec<String>,
    rx: &mut mpsc::UnboundedReceiver<String>,
    cancel: &CancellationToken,
    on_audio: &mut (impl FnMut(Vec<u8>) + Send),
) -> HttpEnd {
    let Ok(client) = eleven::client() else { return HttpEnd::Failed { reason: "no HTTP client".into(), rest: replay } };
    let mut queue: std::collections::VecDeque<String> = replay.into();
    loop {
        let piece = match queue.pop_front() {
            Some(p) => p,
            None => match rx.recv().await {
                Some(p) => p,
                None => return HttpEnd::Done,
            },
        };
        if cancel.is_cancelled() {
            return HttpEnd::Cancelled;
        }
        if let Err(e) = eleven::tts_stream(&client, key, voice, &piece, eleven::TTS_FORMAT, cancel, &mut *on_audio).await {
            let mut rest = vec![piece];
            rest.extend(queue);
            return HttpEnd::Failed { reason: e, rest };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tests that touch the shared backoff / notice state run one at a time.
    static STATE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    fn serial() -> std::sync::MutexGuard<'static, ()> {
        STATE.lock().unwrap_or_else(|p| p.into_inner())
    }

    #[test]
    fn the_live_url_pins_model_format_and_a_safe_language_code() {
        let u = url(None);
        assert!(u.starts_with("wss://api.elevenlabs.io/v1/text-to-dialogue/stream-input?"));
        assert!(u.contains("model_id=eleven_v4_turbo") && u.contains("output_format=pcm_22050") && u.contains("sync_alignment=true"));
        assert!(!u.contains("language_code") && !u.contains("xi"));
        assert!(url(Some("ar")).ends_with("&language_code=ar"));
        assert!(!url(Some("ar&x=1")).contains("x=1"), "only a bare code is appended");
        assert!(ws::allowed_url(&url(Some("ar"))));
    }

    #[test]
    fn messages_match_the_dialogue_socket_schema() {
        let f = first_message("v1");
        assert_eq!(f["voices"], json!(["v1"]));
        assert_eq!(f["voice_settings"]["stability"], json!(0.4));
        assert!(f.get("xi_api_key").is_none(), "the key is only ever in the header");
        let m = input_message("Hello there.", "v1", false);
        assert_eq!(m["inputs"][0]["text"], "Hello there. ");
        assert_eq!(m["inputs"][0]["voice_id"], "v1");
    }

    #[test]
    fn server_frames_are_parsed() {
        let b64 = base64::engine::general_purpose::STANDARD.encode([1u8, 2, 3, 4]);
        assert_eq!(parse_server(&json!({"audio": b64})), Server::Audio(vec![1, 2, 3, 4], 0));
        assert_eq!(parse_server(&json!({"audio": b64, "alignment": {"chars": ["H", "i", " "]}})), Server::Audio(vec![1, 2, 3, 4], 3));
        assert_eq!(parse_server(&json!({"is_final": true})), Server::Final);
        assert_eq!(parse_server(&json!({"is_final_audio_for_turn": true})), Server::Other);
        let e = parse_server(&json!({"error": "payment_required", "message": "Free users cannot use library voices via the API.", "code": 1008}));
        assert!(matches!(e, Server::Error { paid: true, ref text } if text.contains("library voices")));
        assert!(matches!(parse_server(&json!({"error": "quota_exceeded", "message": "quota"})), Server::Error { paid: false, .. }));
        assert_eq!(parse_server(&json!({"audio": "%%%"})), Server::Other);
    }

    #[test]
    fn m3_only_text_not_yet_voiced_is_kept_for_a_replay() {
        let mut p: Vec<String> = vec!["Sure.".into(), "It's fine.".into(), "Anything else?".into()];
        let mut v = 0;
        prune_voiced(&mut p, &mut v);
        assert_eq!(p.len(), 3, "nothing voiced: everything is replayed");
        v = 6 + 4; // "Sure. " fully, then 4 characters into "It's fine. "
        prune_voiced(&mut p, &mut v);
        assert_eq!(p, vec!["It's fine.".to_string(), "Anything else?".into()], "the half-spoken sentence is repeated whole");
        assert_eq!(v, 4);
        v += 7; // the rest of "It's fine. "
        prune_voiced(&mut p, &mut v);
        assert_eq!(p, vec!["Anything else?".to_string()]);
        v += 100;
        prune_voiced(&mut p, &mut v);
        assert!(p.is_empty());
    }

    #[test]
    fn arabic_prompts_get_the_arabic_voice_and_code() {
        assert!(is_arabic("يا وان، ممكن تبص على السجلات؟"));
        assert!(is_arabic("شوف الـ hub logs يا وان"), "mostly Arabic with an English word");
        assert!(!is_arabic("Check the hub logs, and tell Maya"));
        assert!(!is_arabic("ok"));
        assert_eq!(pick_voice("إزيك يا وان", "en1", "ar1"), ("ar1".to_string(), Some("ar")));
        assert_eq!(pick_voice("إزيك يا وان", "en1", ""), ("en1".to_string(), Some("ar")), "no Arabic voice set: the main voice speaks Arabic");
        assert_eq!(pick_voice("How are you?", "en1", "ar1"), ("en1".to_string(), None));
    }

    /// A local fake of the dialogue socket (plain ws on 127.0.0.1): text in, audio out.
    async fn fake_server(mode: &'static str) -> (String, tokio::task::JoinHandle<Vec<Value>>) {
        use futures_util::{SinkExt, StreamExt};
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = format!("ws://{}", l.local_addr().unwrap());
        let h = tokio::spawn(async move {
            let (s, _) = l.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(s).await.unwrap();
            let mut got = Vec::new();
            while let Some(Ok(m)) = ws.next().await {
                let Ok(v) = serde_json::from_str::<Value>(m.to_text().unwrap_or("")) else { continue };
                got.push(v.clone());
                let audio = json!({"audio": base64::engine::general_purpose::STANDARD.encode([0u8; 8])}).to_string();
                if mode == "paid" && v.get("voices").is_some() {
                    let _ = ws.send(tokio_tungstenite::tungstenite::Message::text(json!({"error": "payment_required", "message": "Free users cannot use library voices via the API."}).to_string())).await;
                    break;
                }
                if v.get("flush").is_some() {
                    let _ = ws.send(tokio_tungstenite::tungstenite::Message::text(audio.clone())).await;
                }
                if v.get("close_socket").is_some() {
                    let _ = ws.send(tokio_tungstenite::tungstenite::Message::text(audio)).await;
                    let _ = ws.send(tokio_tungstenite::tungstenite::Message::text(json!({"is_final": true}).to_string())).await;
                    break;
                }
            }
            got
        });
        (addr, h)
    }

    #[tokio::test]
    async fn the_socket_flushes_the_first_piece_closes_at_the_end_and_streams_audio() {
        let (addr, server) = fake_server("ok").await;
        let (mut sock, _) = tokio_tungstenite::connect_async(addr).await.unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        tx.send("Sure, I checked,".to_string()).unwrap();
        tx.send("and everything looks fine.".to_string()).unwrap();
        drop(tx);
        let (mut audio, mut pending) = (0usize, Vec::new());
        let end = run_socket(&mut sock, "v1", &mut rx, &CancellationToken::new(), &mut |b: Vec<u8>| audio += b.len(), &mut pending).await;
        assert!(matches!(end, SockEnd::Done));
        assert_eq!(audio, 16, "audio after the first flush and at the close");
        let got = server.await.unwrap();
        assert_eq!(got[0]["voices"], json!(["v1"]));
        assert_eq!(got[1]["inputs"][0]["text"], "Sure, I checked, ");
        assert_eq!(got[2], json!({"flush": true}), "flush right after the FIRST piece only");
        assert_eq!(got[3]["inputs"][0]["text"], "and everything looks fine. ");
        assert_eq!(got.last().unwrap(), &json!({"close_socket": true}));
        assert_eq!(got.iter().filter(|v| v.get("flush").is_some()).count(), 1);
    }

    #[tokio::test]
    async fn a_refused_voice_is_reported_as_paid_before_any_audio() {
        let (addr, _server) = fake_server("paid").await;
        let (mut sock, _) = tokio_tungstenite::connect_async(addr).await.unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        tx.send("مرحبا".to_string()).unwrap();
        let mut pending = Vec::new();
        let end = run_socket(&mut sock, "lib", &mut rx, &CancellationToken::new(), &mut |_| {}, &mut pending).await;
        assert!(matches!(end, SockEnd::Failed { paid: true, voiced: false, .. }));
    }

    #[tokio::test]
    async fn cancel_stops_the_socket_at_once() {
        let (addr, _server) = fake_server("ok").await;
        let (mut sock, _) = tokio_tungstenite::connect_async(addr).await.unwrap();
        let (_tx, mut rx) = mpsc::unbounded_channel::<String>();
        let c = CancellationToken::new();
        c.cancel();
        let t0 = std::time::Instant::now();
        let end = run_socket(&mut sock, "v1", &mut rx, &c, &mut |_| {}, &mut Vec::new()).await;
        assert!(matches!(end, SockEnd::Cancelled) && t0.elapsed() < std::time::Duration::from_secs(1));
    }

    #[test]
    fn account_errors_are_matched_by_status_code_or_kind_not_loose_words() {
        for r in ["quota_exceeded", "ElevenLabs 401: the key was not accepted", "HTTP 402 payment_required", "429 too many requests", "Free users cannot use library voices via the API.", "You have 0 credits"] {
            assert!(is_account_error(r), "{r}");
        }
        for r in ["the voice connection dropped", "TTS request failed: timed out", "connection reset after 4010 ms", "rate limit of the network card", "no audio came back"] {
            assert!(!is_account_error(r), "{r}");
        }
    }

    #[test]
    fn a_network_blip_does_not_start_the_backoff_but_no_credits_does() {
        let _g = serial();
        note_lookup_failure("TTS request failed: timed out");
        assert!(!eleven_down());
        note_lookup_failure("ElevenLabs 402: payment_required");
        assert!(eleven_down());
        *down_until() = None; // leave no state behind for the other tests
    }

    #[test]
    fn the_no_voice_notice_names_the_real_cause() {
        assert_eq!(no_voice_notice(true), "No built-in voice for Arabic \u{2013} text only");
        assert!(!no_voice_notice(true).contains("credit"));
        assert!(BUILTIN_NOTICE_OFFLINE.contains("not reachable") && !BUILTIN_NOTICE_OFFLINE.contains("credit"));
    }

    /// This PC has no Arabic voice: Arabic text stays text, with ONE honest notice, and no
    /// PowerShell speech is started for it.
    #[cfg(windows)]
    #[tokio::test]
    async fn arabic_without_an_installed_voice_is_text_only_with_the_right_notice() {
        let _g = serial();
        if crate::winvoice::voice_available(Some("ar")).await {
            return; // this PC does have an Arabic voice: nothing to prove here
        }
        let plan = Plan { key: ApiKey::new("test-key-not-real"), voice: "v".into(), fallback_voice: "v".into(), lang: Some("ar") };
        let (tx, mut rx) = mpsc::unbounded_channel();
        tx.send("\u{645}\u{631}\u{62d}\u{628}\u{627}".to_string()).unwrap();
        drop(tx);
        let (mut bytes, mut notices) = (0usize, Vec::new());
        let r = after_live_failure(&plan, "quota_exceeded", vec![], &mut rx, &CancellationToken::new(), &mut |b: Vec<u8>| bytes += b.len(), &mut |m: String| notices.push(m)).await;
        assert_eq!(r, Spoken::TextOnly);
        assert_eq!(bytes, 0);
        assert_eq!(notices, vec![no_voice_notice(true)], "not the credits notice");
        let (tx, mut rx) = mpsc::unbounded_channel();
        tx.send("x".to_string()).unwrap();
        drop(tx);
        let mut again = 0;
        let r = after_live_failure(&plan, "quota_exceeded", vec![], &mut rx, &CancellationToken::new(), &mut |_| {}, &mut |_| again += 1).await;
        assert!(r == Spoken::TextOnly && again == 0, "next Arabic reply: still text only, no second notice");
        *down_until() = None;
    }

    /// The real failure path: ElevenLabs says "no credits" -> the built-in Windows voice speaks
    /// (really synthesised, not mocked), ONE calm notice goes out, and the premium voice is
    /// skipped afterwards. Synthesised audio is checked as bytes; nothing is played.
    #[cfg(windows)]
    #[tokio::test]
    async fn no_credits_falls_back_to_the_windows_voice_with_one_notice() {
        let _g = serial();
        let plan = Plan { key: ApiKey::new("test-key-not-real"), voice: "v".into(), fallback_voice: "v".into(), lang: None };
        let (tx, mut rx) = mpsc::unbounded_channel();
        tx.send("This is the second sentence.".to_string()).unwrap();
        drop(tx);
        let (mut bytes, mut notices) = (0usize, Vec::new());
        let spoken = after_live_failure(
            &plan,
            "quota_exceeded: this request exceeds your quota",
            vec!["This is the first sentence.".to_string()],
            &mut rx,
            &CancellationToken::new(),
            &mut |b: Vec<u8>| bytes += b.len(),
            &mut |m: String| notices.push(m),
        )
        .await;
        assert_eq!(spoken, Spoken::Done);
        assert!(bytes > 2 * 22050, "two sentences of real audio, got {bytes} bytes");
        assert_eq!(notices, vec![BUILTIN_NOTICE.to_string()]);
        assert!(eleven_down(), "the premium voice is skipped from now on");
        // a second reply: straight to the built-in voice, no second notice
        let (tx, rx) = mpsc::unbounded_channel();
        tx.send("And a later reply.".to_string()).unwrap();
        drop(tx);
        let (mut more, mut again) = (0usize, 0usize);
        let r = speak(plan, rx, CancellationToken::new(), |b| more += b.len(), |_| again += 1).await;
        assert_eq!(r, Spoken::Done);
        assert!(more > 22050 && again == 0);
        assert!(BUILTIN_NOTICE.contains("built-in voice") && !BUILTIN_NOTICE.to_lowercase().contains("couldn"));
    }
}
