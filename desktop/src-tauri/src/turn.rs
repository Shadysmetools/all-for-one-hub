//! One conversational turn: spawn Claude, stream text into the redactor and sentence
//! chunker, feed TTS, push PCM to the webview. Cancel (barge-in / Stop) kills the child
//! process tree and the TTS request together.
//!
//! A turn that fails (is_error result, no result line, non-zero exit) never saves the
//! session id it reports. A failed `--resume` turn is reported to the user and the saved session
//! is KEPT (never silently replaced by a fresh one). One only ever runs in its OWN session; a
//! session the user asks about reaches it as a read-only extract (see `tabs::focus_context`).
//!
//! Every way a turn can end (success, error, cancel, timeout, exit without a result) emits a
//! final `Done`, so the status can never stay on "Thinking" (`end_events`). A watchdog ends a
//! turn whose claude process goes silent (no stdout line at all) for 45 s, or that runs longer
//! than 120 s (10 min for an approved tool turn). Waiting for speech to drain, and for stderr, is
//! bounded too, so `Done` follows the end of claude within seconds whatever the voice does.

use crate::chunker::Chunker;
use crate::claude::{build_args_for, card_for, failure_reason, parse_line, resolve_claude, Denial, DenialCard, StreamItem};
use crate::eleven;
use crate::latency::{Clock, Latency};
use crate::mic::MicEvent;
use crate::proc::{kill_child, track};
use crate::redact::{redact, StreamRedactor};
use crate::secret::ApiKey;
use crate::state::Shared;
use crate::tts_live;
use serde::Serialize;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tauri::ipc::{Channel, InvokeResponseBody};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{ChildStdout, Command};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Serialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TurnEvent {
    Delta { text: String },
    Cards { cards: Vec<DenialCard> },
    /// How fast this reply started speaking (sent once, with the first audio).
    Latency { latency: Latency },
    Done { interrupted: bool },
    /// A calm, non-error note (e.g. the built-in voice is in use).
    Notice { message: String },
    Error { message: String },
}

/// Where a turn's events go. A typed turn has its own per-call channel; a turn Rust starts
/// itself (after speech) reports on the long-lived mic channel, tagged with its turn number
/// so the webview can drop events of a turn it has already cancelled.
#[derive(Clone)]
pub enum EventSink {
    Chan(Channel<TurnEvent>),
    Mic { chan: Channel<MicEvent>, turn: u64 },
}

impl EventSink {
    pub fn send(&self, e: TurnEvent) {
        match self {
            EventSink::Chan(c) => drop(c.send(e)),
            EventSink::Mic { chan, turn } => drop(chan.send(MicEvent::Turn { turn: *turn, event: e })),
        }
    }
}

/// No stdout line at all from claude for this long: the process is stuck. ANY line counts
/// (tool_use, thinking, system, partial text), not only reply text.
pub const IDLE_SECS: u64 = 45;
/// A plain chat turn never runs longer than this.
pub const TURN_SECS: u64 = 120;
/// A tool turn (an approved command) may run this long while its tool works; Stop always works.
pub const TOOL_TURN_SECS: u64 = 600;
/// After claude has ended: how long to wait for queued speech, and for stderr, before moving on.
pub const TTS_DRAIN_SECS: u64 = 8;
/// Speech still queued after claude has ended: keep waiting while audio keeps arriving; stop
/// when none has arrived for this long, or after the hard limit.
pub const DRAIN_IDLE_SECS: u64 = 45; // above the 25 s a sentence may take to start
pub const DRAIN_HARD_SECS: u64 = 180;
pub const STDERR_SECS: u64 = 2;
pub const TTS_SKIP_MESSAGE: &str = "I couldn't speak that part; the text is above.";
pub const TIMEOUT_MESSAGE: &str = "That's taking too long. Try again, or ask me something simpler.";

/// How long the watchdog may still wait, given the time since the turn started and since the last
/// stdout line. Zero means the turn has timed out. A tool turn has no idle limit (its tool may be
/// silent for minutes) but a longer total. Pure, so it is unit-tested.
pub fn watchdog_remaining(elapsed: std::time::Duration, idle: std::time::Duration, tool_turn: bool) -> std::time::Duration {
    use std::time::Duration;
    let total = Duration::from_secs(if tool_turn { TOOL_TURN_SECS } else { TURN_SECS });
    let left_total = total.saturating_sub(elapsed);
    if tool_turn {
        return left_total;
    }
    left_total.min(Duration::from_secs(IDLE_SECS).saturating_sub(idle))
}

/// Wait for a task at most `limit`, then stop it. None when it timed out (or failed).
async fn bounded<T>(mut h: tauri::async_runtime::JoinHandle<T>, limit: std::time::Duration) -> Option<T> {
    match tokio::time::timeout(limit, &mut h).await {
        Ok(r) => r.ok(),
        Err(_) => {
            h.abort();
            None
        }
    }
}

/// Speech that is still being produced after claude has ended. `done` counts audio progress.
struct Drain {
    task: tauri::async_runtime::JoinHandle<()>,
    done: Arc<std::sync::atomic::AtomicUsize>,
}

/// Wait for the speech worker while it makes progress. False when it was stopped for stalling
/// (no progress within `idle`) or for passing `hard`.
async fn drain_speech(mut task: tauri::async_runtime::JoinHandle<()>, done: Arc<std::sync::atomic::AtomicUsize>, idle: std::time::Duration, hard: std::time::Duration) -> bool {
    use std::sync::atomic::Ordering;
    let (start, poll) = (std::time::Instant::now(), std::time::Duration::from_millis(50).min(idle));
    let (mut seen, mut last_progress) = (done.load(Ordering::SeqCst), start);
    loop {
        if tokio::time::timeout(poll, &mut task).await.is_ok() {
            return true;
        }
        let now = done.load(Ordering::SeqCst);
        if now != seen {
            (seen, last_progress) = (now, std::time::Instant::now());
        }
        if last_progress.elapsed() >= idle || start.elapsed() >= hard {
            task.abort();
            return false;
        }
    }
}

/// The session id to keep after a timed-out turn: the one claude announced, but only when no
/// session is saved yet (a first turn that times out must not orphan its transcript).
pub fn session_after_timeout(saved: Option<&str>, init: Option<&str>) -> Option<String> {
    if saved.is_some() {
        return None;
    }
    init.map(String::from)
}

pub struct TurnRequest {
    pub prompt: String,
    pub allowed_tool: Option<String>,
    /// When the user stopped speaking (the last voiced frame). None for a typed turn.
    pub speech_end: Option<Instant>,
}

/// A plain turn uses the warm process; an approval (it needs `--allowedTools` for one turn) and
/// a turn carrying another session's extract are one-shot processes. Pure, unit-tested.
pub fn uses_warm(allowed_tool: Option<&str>, focus_on: bool) -> bool {
    allowed_tool.is_none() && !focus_on
}

fn spawn_claude(shared: &Shared, req: &TurnRequest, resume: Option<&str>, prompt_file: Option<&std::path::Path>, lockdown: bool) -> Result<tokio::process::Child, String> {
    let bin = resolve_claude().ok_or("Could not find the claude CLI (set ONE_CLAUDE_BIN)")?;
    // Only an approved call is allowed without asking. Reads inside the working directory need no
    // rule, and a rule must never ride on the lockdown turns.
    let allowed: Vec<String> = req.allowed_tool.clone().into_iter().collect();
    let args = build_args_for(resume, &allowed, prompt_file, lockdown);
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .current_dir(shared.settings().cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    #[cfg(unix)]
    cmd.process_group(0); // own group, so the whole tree can be killed (see proc.rs)
    cmd.spawn().map_err(|_| "Could not start the claude CLI (check the working directory in settings.json)".to_string())
}

/// Keep the first 4 KB of stderr, then drain the rest so the child can never block on it.
fn capture_stderr(mut s: tokio::process::ChildStderr) -> tauri::async_runtime::JoinHandle<String> {
    tauri::async_runtime::spawn(async move {
        let mut kept = Vec::new();
        let mut head = (&mut s).take(4096);
        let _ = head.read_to_end(&mut kept).await;
        let _ = tokio::io::copy(&mut s, &mut tokio::io::sink()).await;
        String::from_utf8_lossy(&kept).into_owned()
    })
}

/// The speaker of one reply: resolves the voice, opens the live voice socket right away (in
/// parallel with Claude) and speaks every piece sent to it, in order (see tts_live.rs).
/// The first audio chunk stamps the latency clock and reports the measurement once.
#[allow(clippy::too_many_arguments)]
fn spawn_speaker(
    key: ApiKey,
    voices: (String, String),
    prompt: &str,
    audio: Channel<InvokeResponseBody>,
    cancel: CancellationToken,
    ev: EventSink,
    done: Arc<std::sync::atomic::AtomicUsize>,
    clock: Arc<Mutex<Clock>>,
    spoke: Arc<std::sync::atomic::AtomicBool>,
) -> (mpsc::UnboundedSender<String>, tauri::async_runtime::JoinHandle<()>) {
    let (tx, rx) = mpsc::unbounded_channel::<String>();
    let (main, arabic) = voices;
    let (wanted, lang) = tts_live::pick_voice(prompt, &main, &arabic);
    let handle = tauri::async_runtime::spawn(async move {
        use std::sync::atomic::{AtomicBool, Ordering};
        let told = Arc::new(AtomicBool::new(false));
        // An empty setting means "the first premade voice": resolved once and cached.
        // ElevenLabs known to be down (no credits...): no lookup, the built-in voice speaks.
        let fallback = if tts_live::eleven_down() {
            Ok(String::new())
        } else {
            match eleven::client() {
                Ok(c) => eleven::resolve_voice(&c, &key, &main).await,
                Err(e) => Err(e),
            }
        };
        let fallback = fallback.unwrap_or_else(|e| {
            eprintln!("no premium voice to speak with, using the built-in one: {e}");
            tts_live::note_lookup_failure(&e);
            String::new()
        });
        let voice = if wanted.is_empty() { fallback.clone() } else { wanted };
        let plan = tts_live::Plan { key, voice, fallback_voice: fallback, lang };
        let (c2, ev2, done2) = (cancel.clone(), ev.clone(), done.clone());
        let on_audio = move |b: Vec<u8>| {
            if c2.is_cancelled() {
                return; // a cancelled turn sends no more audio (the channel can be long-lived)
            }
            done2.fetch_add(1, Ordering::SeqCst); // progress, for the drain wait
            spoke.store(true, Ordering::SeqCst); // the mic's continue window closes now
            let measured = clock.lock().unwrap_or_else(|p| p.into_inner()).audio(Instant::now());
            if let Some(latency) = measured {
                eprintln!("voice latency: {} ms (speech-to-text {} ms, claude {} ms, voice {} ms)", latency.total_ms, latency.stt_ms, latency.claude_ms, latency.voice_ms);
                ev2.send(TurnEvent::Latency { latency });
            }
            let _ = audio.send(InvokeResponseBody::Raw(b));
        };
        let (ev3, told3) = (ev.clone(), told.clone());
        let on_notice = move |m: String| {
            if !told3.swap(true, Ordering::SeqCst) {
                ev3.send(TurnEvent::Notice { message: m });
            }
        };
        if let tts_live::Spoken::Failed(reason) = tts_live::speak(plan, rx, cancel, on_audio, on_notice).await {
            eprintln!("speech failed: {reason}");
            if !told.swap(true, Ordering::SeqCst) {
                ev.send(TurnEvent::Error { message: TTS_SKIP_MESSAGE.into() });
            }
        }
        done.fetch_add(1, Ordering::SeqCst);
    });
    (tx, handle)
}

#[derive(Default)]
struct Outcome {
    denials: Vec<Denial>,
    streamed: bool,
    result_text: String,
    got_result: bool,
    is_error: bool,
    errors: Vec<String>,
    /// Session id announced by `system/init`: held, not saved, until the turn is known good.
    init_id: Option<String>,
    /// Session id of a successful result.
    result_id: Option<String>,
    /// Everything shown to the user as the reply (already redacted).
    reply: String,
}

/// The session id to remember after a finished attempt. A failed attempt (even a fresh one)
/// saves nothing: the id it announced may never have been persisted by Claude.
fn session_to_save(out: &Outcome, failed: bool) -> Option<String> {
    if failed {
        return None;
    }
    out.result_id.clone().or_else(|| out.init_id.clone())
}

/// Redact, show and queue for speech one piece of reply text.
fn emit(text: &str, ev: &EventSink, chunker: &mut Chunker, tts: &mpsc::UnboundedSender<String>, reply: &mut String) {
    if text.is_empty() {
        return;
    }
    reply.push_str(text); // the chat history keeps what the user was shown
    ev.send(TurnEvent::Delta { text: text.to_string() });
    chunker.push(text).into_iter().for_each(|s| drop(tts.send(s)));
}

enum ReadEnd {
    Eof,
    Cancelled,
    TimedOut,
}

/// Read Claude's stdout into `out` until it ends (or, for the warm process, until this turn's
/// `result` line), `cancel` fires or the watchdog runs out.
#[allow(clippy::too_many_arguments)]
async fn read_stream(
    lines: &mut Lines<BufReader<ChildStdout>>,
    ev: &EventSink,
    tts: &mpsc::UnboundedSender<String>,
    cancel: &CancellationToken,
    out: &mut Outcome,
    tool_turn: bool,
    stop_at_result: bool,
    clock: &Mutex<Clock>,
) -> ReadEnd {
    let started = std::time::Instant::now();
    let mut last_line = started;
    let mut chunker = Chunker::new();
    let mut redactor = StreamRedactor::new(); // M3: nothing leaves Rust unredacted
    loop {
        let wait = watchdog_remaining(started.elapsed(), last_line.elapsed(), tool_turn);
        let line = tokio::select! {
            _ = cancel.cancelled() => return ReadEnd::Cancelled,
            _ = tokio::time::sleep(wait) => return ReadEnd::TimedOut,
            l = lines.next_line() => match l { Ok(Some(l)) => l, _ => break },
        };
        last_line = std::time::Instant::now(); // ANY line proves claude is alive
        match parse_line(&line) {
            StreamItem::Init { session_id } => out.init_id = Some(session_id),
            StreamItem::TextDelta(t) => {
                out.streamed = true;
                clock.lock().unwrap_or_else(|p| p.into_inner()).text(Instant::now());
                emit(&redactor.push(&t), ev, &mut chunker, tts, &mut out.reply);
            }
            // L2: a new assistant message starts on a fresh line, so sentences do not glue together.
            StreamItem::NewMessage if out.streamed => emit(&redactor.push("\n"), ev, &mut chunker, tts, &mut out.reply),
            StreamItem::Result { session_id, text, is_error, errors, denials } => {
                // CR-H1: a failed result echoes back the id it was given: never save that.
                out.result_id = if is_error { None } else { session_id };
                out.got_result = true;
                out.is_error = is_error;
                out.errors = errors;
                out.result_text = text;
                out.denials = denials;
                if stop_at_result {
                    break; // the warm process stays up for the next turn
                }
            }
            _ => {}
        }
    }
    emit(&redactor.finish(), ev, &mut chunker, tts, &mut out.reply);
    if let Some(rest) = chunker.finish() {
        let _ = tts.send(rest);
    }
    ReadEnd::Eof
}

enum Ended {
    Cancelled,
    /// The watchdog killed the turn.
    TimedOut,
    /// The attempt ran to the end; the reason is set when it failed.
    Finished(Outcome, Option<String>),
}

/// After the stream ended: the result text when nothing streamed, then the "I need your OK"
/// lines. Dropping `tts_tx` ends the speech input (the caller waits for the speech).
fn finish_speech(outcome: &mut Outcome, ev: &EventSink, tts_tx: mpsc::UnboundedSender<String>) {
    // Some turns (e.g. after an approval) produce a result without partial deltas.
    if !outcome.streamed && !outcome.result_text.is_empty() {
        let text = redact(&outcome.result_text);
        ev.send(TurnEvent::Delta { text: text.clone() });
        outcome.reply = text.clone();
        let _ = tts_tx.send(crate::chunker::speakable(&text));
    }
    for d in &outcome.denials {
        // Tool name only: arguments (which may hold secrets) are never read aloud.
        let _ = tts_tx.send(format!("I need your OK to run {}.", d.tool_name));
    }
}

/// One turn, start to finish: on the warm process (plain turns) or a one-shot process.
#[allow(clippy::too_many_arguments)]
async fn attempt(
    shared: &Shared,
    req: &TurnRequest,
    resume: Option<&str>,
    prompt_file: Option<&std::path::Path>,
    lockdown: bool,
    warm: bool,
    key: &ApiKey,
    cancel: &CancellationToken,
    ev: &EventSink,
    audio: &Channel<InvokeResponseBody>,
    drain: &mut Option<Drain>,
) -> Result<Ended, String> {
    eleven::client()?; // before any spawn: no error path may leave a running child behind
    let conv = shared.conv_gen(); // a "new conversation" during this turn: its session is not saved
    let clock = Arc::new(Mutex::new(Clock::new(req.speech_end, Instant::now())));
    let done = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let s = shared.settings();
    // The reply's language follows what the user last said (an approval turn's prompt is canned).
    let said = if req.allowed_tool.is_some() { shared.chat_history().iter().rev().find(|m| m.role == "user").map(|m| m.text.clone()).unwrap_or_default() } else { req.prompt.clone() };
    let speaker = || spawn_speaker(key.clone(), (s.voice_id.clone(), s.voice_id_ar.clone()), &said, audio.clone(), cancel.clone(), ev.clone(), done.clone(), clock.clone(), shared.turn_spoke_flag());
    let tool_turn = req.allowed_tool.is_some();
    let mut outcome = Outcome::default();
    if warm {
        // A process from the slot may have died while idle (update, auth refresh): if it cannot
        // take the message, it is replaced once, silently.
        let (mut w, mut reused) = match shared.take_warm() {
            Some(w) => (w, true),
            None => (crate::brain::spawn(shared.warm_key())?, false),
        };
        loop {
            match w.send(&req.prompt).await {
                Ok(()) => break,
                Err(_) if reused => {
                    w.kill().await;
                    w = crate::brain::spawn(shared.warm_key())?;
                    reused = false;
                }
                Err(e) => {
                    w.kill().await;
                    return Err(e);
                }
            }
        }
        let (tts_tx, tts_task) = speaker(); // opens the voice socket while Claude thinks
        match read_stream(&mut w.lines, ev, &tts_tx, cancel, &mut outcome, tool_turn, true, &clock).await {
            ReadEnd::Eof => {}
            ReadEnd::Cancelled => {
                w.kill().await; // a barge-in: the process is replaced, never left mid-answer
                tts_task.abort();
                if let Some(id) = &outcome.init_id {
                    shared.set_session_for(conv, id); // a cancel is not a failure: the talk continues
                }
                return Ok(Ended::Cancelled);
            }
            ReadEnd::TimedOut => {
                w.kill().await;
                if let Some(id) = session_after_timeout(shared.session_id().as_deref(), outcome.init_id.as_deref()) {
                    shared.set_session_for(conv, &id);
                }
                let _ = tts_tx.send(TIMEOUT_MESSAGE.to_string());
                drop(tts_tx);
                let _ = bounded(tts_task, std::time::Duration::from_secs(TTS_DRAIN_SECS)).await;
                return Ok(Ended::TimedOut);
            }
        }
        let has_output = outcome.streamed || !outcome.result_text.is_empty() || !outcome.denials.is_empty();
        // The warm process does not exit per turn: no result line means it died.
        let reason = failure_reason(outcome.is_error, &outcome.errors, outcome.got_result, outcome.got_result, None, has_output);
        if let Some(id) = session_to_save(&outcome, reason.is_some()) {
            shared.set_session_for(conv, &id);
        }
        if reason.is_none() && w.alive() {
            // Now in the session this turn saved. The generation is NOT refreshed: if a one-shot
            // turn or a new conversation happened meanwhile, put_warm refuses it.
            w.key.session = shared.session_id();
            shared.put_warm(w);
        } else {
            w.kill().await;
        }
        if reason.is_some() {
            drop(tts_tx);
            *drain = Some(Drain { task: tts_task, done });
            return Ok(Ended::Finished(outcome, reason));
        }
        finish_speech(&mut outcome, ev, tts_tx);
        *drain = Some(Drain { task: tts_task, done });
        return Ok(Ended::Finished(outcome, None));
    }
    // One-shot: the warm process holds the old conversation in memory, so it goes first.
    shared.drop_warm();
    let mut child = spawn_claude(shared, req, resume, prompt_file, lockdown)?;
    let _tracked = track(&child);
    let stderr_task = child.stderr.take().map(capture_stderr);
    if let Some(mut stdin) = child.stdin.take() {
        if stdin.write_all(req.prompt.as_bytes()).await.is_err() {
            kill_child(&mut child).await; // the whole tree, not just claude
            return Err("could not send prompt".into());
        }
        // dropping stdin closes it so claude starts
    }
    let Some(stdout) = child.stdout.take() else {
        kill_child(&mut child).await;
        return Err("could not read from claude".into());
    };
    let mut lines = BufReader::new(stdout).lines();
    let (tts_tx, tts_task) = speaker();
    match read_stream(&mut lines, ev, &tts_tx, cancel, &mut outcome, tool_turn, false, &clock).await {
        ReadEnd::Eof => {}
        ReadEnd::Cancelled => {
            kill_child(&mut child).await; // M4: the whole tree, not just claude
            tts_task.abort();
            if let Some(t) = stderr_task {
                t.abort();
            }
            // A cancel is not a failure: keep the session Claude announced so the talk continues.
            if let Some(id) = &outcome.init_id {
                shared.set_session_for(conv, id);
            }
            return Ok(Ended::Cancelled);
        }
        ReadEnd::TimedOut => {
            kill_child(&mut child).await;
            // A first turn that times out keeps the session claude announced (no orphan per retry).
            if let Some(id) = session_after_timeout(shared.session_id().as_deref(), outcome.init_id.as_deref()) {
                shared.set_session_for(conv, &id);
            }
            if let Some(t) = stderr_task {
                t.abort();
            }
            // Say it out loud too, bounded, and stop the worker if it is still busy.
            let _ = tts_tx.send(TIMEOUT_MESSAGE.to_string());
            drop(tts_tx);
            let _ = bounded(tts_task, std::time::Duration::from_secs(TTS_DRAIN_SECS)).await;
            return Ok(Ended::TimedOut);
        }
    }
    // stdout closed; a process that lingers is killed rather than waited for forever.
    let status = match tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await {
        Ok(s) => s.ok(),
        Err(_) => {
            kill_child(&mut child).await;
            None
        }
    };
    let stderr = match stderr_task {
        Some(t) => bounded(t, std::time::Duration::from_secs(STDERR_SECS)).await.unwrap_or_default(),
        None => String::new(),
    };
    let has_output = outcome.streamed || !outcome.result_text.is_empty() || !outcome.denials.is_empty();
    let reason = failure_reason(outcome.is_error, &outcome.errors, outcome.got_result, status.is_some_and(|s| s.success()), status.and_then(|s| s.code()), has_output);
    if let Some(id) = session_to_save(&outcome, reason.is_some()) {
        shared.set_session_for(conv, &id);
    }
    if reason.is_some() {
        if !stderr.trim().is_empty() {
            eprintln!("claude stderr: {}", crate::claude::redact_then_truncate(stderr.trim(), 400));
        }
        drop(tts_tx);
        *drain = Some(Drain { task: tts_task, done });
        return Ok(Ended::Finished(outcome, reason));
    }
    finish_speech(&mut outcome, ev, tts_tx);
    *drain = Some(Drain { task: tts_task, done }); // the caller reports the end first, then waits for the speech
    Ok(Ended::Finished(outcome, None))
}

pub async fn drive(shared: &Shared, req: TurnRequest, ev: EventSink, audio: Channel<InvokeResponseBody>) -> Result<(), String> {
    // L4: register the turn first and synchronously, before any I/O, so two quick calls
    // cannot reach begin_turn out of order and cancel the newer one.
    let (id, cancel) = shared.begin_turn_id();
    let res = drive_attempts(shared, &req, &cancel, &ev, &audio).await;
    cancel.cancel(); // nothing of this turn (a leftover speech worker) may keep going after it
    shared.end_turn(id); // barge-in only counts while a turn runs
    if shared.focus().is_none() {
        shared.prewarm(); // ready for the next turn (after a one-shot or a killed warm process)
    }
    res
}

async fn drive_attempts(
    shared: &Shared,
    req: &TurnRequest,
    cancel: &CancellationToken,
    ev: &EventSink,
    audio: &Channel<InvokeResponseBody>,
) -> Result<(), String> {
    let key = crate::secret::load_key()?;
    let resume = shared.session_id(); // One's own session, never another one
    // While the user asks about another session, One only answers: no write, command or network
    // tools. An approval turn is not that: it carries no extract and runs the one approved call.
    let focus_on = shared.focus().is_some() && req.allowed_tool.is_none();
    let prompt = if focus_on { focus_prompt_file(shared).await? } else { None };
    let mut drain = None;
    // The tool allowlist applies on EVERY turn while the conversation is tainted, not only on the focus turn.
    let lockdown = focus_on || shared.tainted();
    let warm = uses_warm(req.allowed_tool.as_deref(), focus_on);
    let ended = attempt(shared, req, resume.as_deref(), prompt.as_ref().map(|f| f.path()), lockdown, warm, &key, cancel, ev, audio, &mut drain).await?;
    drop(prompt); // the temp file goes as soon as claude has ended
    if focus_on {
        shared.set_tainted(); // One's replies in this turn were shaped by another session's text
    }
    let deny_only = focus_on || shared.tainted();
    let reply = match &ended {
        Ended::Finished(o, _) => o.reply.clone(),
        _ => String::new(),
    };
    let denials = match &ended {
        Ended::Finished(o, None) if !cancel.is_cancelled() => o.denials.clone(),
        _ => Vec::new(),
    };
    if !denials.is_empty() && !deny_only {
        shared.set_pending(denials.clone()); // a focus turn never creates an approvable card
    }
    let kind = match ended {
        Ended::Cancelled => EndKind::Cancelled,
        Ended::TimedOut => EndKind::TimedOut,
        Ended::Finished(_, reason) => classify(reason, cancel.is_cancelled(), resume.is_some(), cards_for(&denials, deny_only)),
    };
    // Done goes out as soon as claude has ended, so the status leaves Thinking (the audio that is
    // still playing shows as Speaking). The speech is then waited for while it makes progress.
    shared.chat_push_marked("assistant", &reply, focus_on);
    end_events(kind).into_iter().for_each(|e| ev.send(e));
    if let Some(d) = drain {
        let (idle, hard) = (std::time::Duration::from_secs(DRAIN_IDLE_SECS), std::time::Duration::from_secs(DRAIN_HARD_SECS));
        if !drain_speech(d.task, d.done, idle, hard).await {
            ev.send(TurnEvent::Error { message: TTS_SKIP_MESSAGE.into() });
        }
    }
    Ok(())
}

pub const DENY_ONLY_REASON: &str = "Approvals are off for this conversation because One read another session. Start a new conversation to allow things again.";

/// The cards for a turn's denials. `deny_only` (the session read another session's text): no
/// Allow, with the plain reason.
pub fn cards_for(denials: &[Denial], deny_only: bool) -> Vec<DenialCard> {
    denials
        .iter()
        .map(|d| {
            let mut c = card_for(d);
            if deny_only {
                c.approvable = false;
                c.reason = Some(DENY_ONLY_REASON.to_string());
            }
            c
        })
        .collect()
}

/// How a finished attempt ends for the user. A cancel (barge-in, Stop) while the reply was still
/// being spoken is `Cancelled`, so the line is marked interrupted.
pub fn classify(reason: Option<String>, cancelled: bool, resumed: bool, cards: Vec<DenialCard>) -> EndKind {
    match reason {
        Some(reason) => EndKind::Failed { reason, resumed },
        None if cancelled => EndKind::Cancelled,
        None => EndKind::Success { cards },
    }
}

/// The system prompt file for a turn that carries the extract of the session the user asked
/// about: the persona plus the extract, private to the user, deleted when dropped.
async fn focus_prompt_file(shared: &Shared) -> Result<Option<crate::claude::PromptFile>, String> {
    let Some(id) = shared.focus() else { return Ok(None) };
    let (cwd, dir) = (shared.settings().cwd, shared.data_dir());
    let file = tokio::task::spawn_blocking(move || {
        let block = crate::tabs::focus_context_for(&cwd, &id);
        crate::claude::PromptFile::create(&dir, &format!("{}\n\n{block}", crate::claude::PERSONA))
    })
    .await
    .map_err(|_| "could not prepare the session context".to_string())?;
    file.map(Some).map_err(|_| "could not prepare the session context".to_string())
}

/// How a turn ended, as far as the user is concerned.
pub enum EndKind {
    Success { cards: Vec<DenialCard> },
    Cancelled,
    TimedOut,
    Failed { reason: String, resumed: bool },
}

/// The events that close a turn. The LAST one is always `Done`, whatever the end path, so the
/// status can never be left on "Thinking". A failed resume is REPORTED and the saved session is
/// kept (the failure may be a hiccup); only the user can drop it ("Start a new conversation").
pub fn end_events(kind: EndKind) -> Vec<TurnEvent> {
    match kind {
        EndKind::Cancelled => vec![TurnEvent::Done { interrupted: true }],
        EndKind::TimedOut => vec![TurnEvent::Error { message: TIMEOUT_MESSAGE.into() }, TurnEvent::Done { interrupted: false }],
        EndKind::Failed { reason, resumed } => {
            vec![TurnEvent::Error { message: failure_message(&reason, resumed) }, TurnEvent::Done { interrupted: false }]
        }
        EndKind::Success { cards } if cards.is_empty() => vec![TurnEvent::Done { interrupted: false }],
        EndKind::Success { cards } => vec![TurnEvent::Cards { cards }, TurnEvent::Done { interrupted: false }],
    }
}

/// What the user is told when a turn fails. The "One could not reach Claude" prefix makes the
/// webview add it to the transcript.
fn failure_message(reason: &str, resumed: bool) -> String {
    if resumed {
        format!("One could not reach Claude (resuming the saved session): {reason}. Your saved session was kept. Start a new conversation if this keeps happening.")
    } else {
        format!("One could not reach Claude: {reason}")
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn out(init: Option<&str>, result: Option<&str>) -> Outcome {
        Outcome { init_id: init.map(Into::into), result_id: result.map(Into::into), ..Default::default() }
    }

    #[test]
    fn l2_a_failed_fresh_turn_saves_no_session_id() {
        // announced by init, then the turn failed: nothing may be saved
        assert_eq!(session_to_save(&out(Some("init-1"), None), true), None);
        assert_eq!(session_to_save(&out(Some("init-1"), Some("res-1")), true), None);
    }

    #[test]
    fn a_good_turn_saves_the_result_id_else_the_init_id() {
        assert_eq!(session_to_save(&out(Some("init-1"), Some("res-1")), false).as_deref(), Some("res-1"));
        assert_eq!(session_to_save(&out(Some("init-1"), None), false).as_deref(), Some("init-1"));
        assert_eq!(session_to_save(&out(None, None), false), None);
    }

    #[test]
    fn a_failed_resume_is_reported_with_the_session_kept() {
        let m = failure_message("No conversation found", true);
        assert!(m.starts_with("One could not reach Claude"), "the webview adds this to the transcript");
        assert!(m.contains("No conversation found") && m.contains("kept") && m.contains("Start a new conversation"));
        assert_eq!(failure_message("x", false), "One could not reach Claude: x");
    }

    #[test]
    fn l2_stderr_is_redacted_before_it_is_cut() {
        let secret = "k9m8n7b6v5".repeat(4); // built at runtime; 40 chars
        let stderr = format!("{} {}", "e".repeat(389), secret); // the 400 cut lands inside the secret
        let shown = crate::claude::redact_then_truncate(&stderr, 400);
        assert!(!shown.contains("k9m8n7b6"), "fragment leaked: {shown}");
    }

    #[test]
    fn watchdog_is_an_idle_clock_with_a_total_and_tool_turns_get_ten_minutes() {
        let s = Duration::from_secs;
        // a silent process dies after 45 s, from the last line (or from the start)
        assert_eq!(watchdog_remaining(s(0), s(0), false), s(45));
        assert_eq!(watchdog_remaining(s(44), s(44), false), s(1));
        assert_eq!(watchdog_remaining(s(45), s(45), false), Duration::ZERO);
        // tool_use, then 40 s of silence from the tool, then more lines: never timed out
        assert!(watchdog_remaining(s(41), s(40), false) > Duration::ZERO);
        assert!(watchdog_remaining(s(60), s(5), false) > Duration::ZERO, "a line 5 s ago resets the clock");
        // a plain chat turn keeps its 120 s total however lively it is
        assert_eq!(watchdog_remaining(s(119), s(1), false), s(1));
        assert_eq!(watchdog_remaining(s(120), s(1), false), Duration::ZERO);
        assert_eq!(watchdog_remaining(s(500), s(0), false), Duration::ZERO);
        // an approved tool turn: no idle limit while the tool works, 10 minutes in total
        assert_eq!(watchdog_remaining(s(300), s(300), true), s(300));
        assert_eq!(watchdog_remaining(s(599), s(599), true), s(1));
        assert_eq!(watchdog_remaining(s(600), s(0), true), Duration::ZERO);
    }

    #[test]
    fn plain_turns_go_to_the_warm_process_approvals_and_focus_turns_never_do() {
        assert!(uses_warm(None, false));
        assert!(!uses_warm(Some("Bash(echo hi)"), false), "an approval is one turn, exact scope: never on the long-lived process");
        assert!(!uses_warm(None, true), "a focus turn carries a per-turn extract file");
        assert!(!uses_warm(Some("Bash(echo hi)"), true));
    }

    #[test]
    fn a_stuck_task_is_stopped_at_the_limit_not_waited_for() {
        let t0 = std::time::Instant::now();
        let r = tauri::async_runtime::block_on(async {
            let h = tauri::async_runtime::spawn(async {
                tokio::time::sleep(Duration::from_secs(60)).await;
                7
            });
            bounded(h, Duration::from_millis(50)).await
        });
        assert_eq!(r, None);
        assert!(t0.elapsed() < Duration::from_secs(5));
        let ok = tauri::async_runtime::block_on(async { bounded(tauri::async_runtime::spawn(async { 7 }), Duration::from_secs(5)).await });
        assert_eq!(ok, Some(7));
    }

    #[test]
    fn a_first_turn_that_times_out_keeps_the_session_it_was_given() {
        assert_eq!(session_after_timeout(None, Some("init-1")).as_deref(), Some("init-1"));
        assert_eq!(session_after_timeout(Some("saved"), Some("init-1")), None, "never replace a saved session");
        assert_eq!(session_after_timeout(None, None), None);
    }

    #[test]
    fn a_barge_in_while_the_reply_is_being_spoken_is_reported_as_interrupted() {
        assert!(matches!(end_events(classify(None, true, false, vec![]))[0], TurnEvent::Done { interrupted: true }));
        assert!(matches!(end_events(classify(None, false, false, vec![]))[0], TurnEvent::Done { interrupted: false }));
        let failed = end_events(classify(Some("x".into()), true, false, vec![]));
        assert!(matches!(failed[0], TurnEvent::Error { .. }), "a real failure is still reported");
    }

    fn card() -> DenialCard {
        DenialCard { tool_use_id: "t".into(), tool_name: "Bash".into(), summary: "s".into(), command: None, warning: false, approvable: false, sentence: String::new(), reason: None }
    }

    #[test]
    fn every_end_path_finishes_with_done_so_the_status_leaves_thinking() {
        let kinds = [
            EndKind::Success { cards: vec![] },
            EndKind::Success { cards: vec![card()] },
            EndKind::Cancelled,
            EndKind::TimedOut,
            EndKind::Failed { reason: "claude ended without an answer".into(), resumed: false },
            EndKind::Failed { reason: "x".into(), resumed: true },
        ];
        for k in kinds {
            let events = end_events(k);
            assert!(matches!(events.last(), Some(TurnEvent::Done { .. })), "no closing Done");
            assert_eq!(events.iter().filter(|e| matches!(e, TurnEvent::Done { .. })).count(), 1);
        }
    }

    #[test]
    fn a_timeout_and_a_silent_exit_are_errors_the_user_sees_a_cancel_is_not() {
        let msg = |k| end_events(k).into_iter().find_map(|e| if let TurnEvent::Error { message } = e { Some(message) } else { None });
        assert_eq!(msg(EndKind::TimedOut).as_deref(), Some("That's taking too long. Try again, or ask me something simpler."));
        assert!(msg(EndKind::Failed { reason: "claude ended without an answer".into(), resumed: false }).unwrap().contains("ended without an answer"));
        assert_eq!(msg(EndKind::Cancelled), None);
        assert!(matches!(end_events(EndKind::Cancelled)[0], TurnEvent::Done { interrupted: true }));
    }

    #[test]
    fn cards_are_deny_only_after_a_focus_turn_with_the_plain_reason() {
        let d = Denial { tool_name: "Bash".into(), tool_use_id: "t1".into(), tool_input: serde_json::json!({"command": "echo hi"}) };
        assert!(cards_for(&[d.clone()], false)[0].approvable);
        let c = &cards_for(&[d], true)[0];
        assert!(!c.approvable);
        assert_eq!(c.reason.as_deref(), Some(DENY_ONLY_REASON));
    }

    #[test]
    fn the_drain_keeps_waiting_while_sentences_finish_and_stops_when_they_stall() {
        use std::sync::{atomic::{AtomicUsize, Ordering}, Arc};
        let ms = Duration::from_millis;
        // 6 sentences, one every 60 ms (360 ms in all), idle limit 150 ms: longer than idle in total, but progress never stalls
        let done = Arc::new(AtomicUsize::new(0));
        let d = done.clone();
        let steady = tauri::async_runtime::spawn(async move {
            for _ in 0..6 {
                tokio::time::sleep(Duration::from_millis(60)).await;
                d.fetch_add(1, Ordering::SeqCst);
            }
        });
        assert!(tauri::async_runtime::block_on(drain_speech(steady, done, ms(150), Duration::from_secs(10))), "steady progress is never cut");
        // a worker that finishes one sentence and then hangs is stopped after the idle limit
        let done = Arc::new(AtomicUsize::new(0));
        let d = done.clone();
        let stalled = tauri::async_runtime::spawn(async move {
            tokio::time::sleep(Duration::from_millis(30)).await;
            d.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_secs(60)).await;
        });
        let t0 = std::time::Instant::now();
        assert!(!tauri::async_runtime::block_on(drain_speech(stalled, done, ms(150), Duration::from_secs(10))));
        assert!(t0.elapsed() < Duration::from_secs(3));
        // the hard limit ends even a worker that keeps making progress
        let done = Arc::new(AtomicUsize::new(0));
        let d = done.clone();
        let endless = tauri::async_runtime::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(20)).await;
                d.fetch_add(1, Ordering::SeqCst);
            }
        });
        assert!(!tauri::async_runtime::block_on(drain_speech(endless, done, ms(500), ms(300))));
        assert_eq!((DRAIN_IDLE_SECS, DRAIN_HARD_SECS), (45, 180));
    }
}
