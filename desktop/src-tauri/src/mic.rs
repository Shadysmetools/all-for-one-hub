//! Microphone pipeline, Rust side. The webview captures raw PCM (so the browser's echo
//! cancellation stays in the loop) and streams it here; Rust runs the VAD, decides when an
//! utterance starts and ends, detects barge-in, runs speech-to-text itself, merges what was
//! said, and STARTS THE TURN itself. The webview only renders the events.
//!
//! Speech-to-text is LIVE (stt_live.rs): while the VAD hears speech, the audio streams to
//! ElevenLabs and partial text comes back to show as the user talks. The utterance's end commits
//! it (or the model's turn prediction commits sooner and the utterance is cut early). Nothing is
//! streamed while the user is silent or the mic is off. When the live socket is down, the
//! finished utterance goes through the older one-shot transcription instead.

use crate::eleven;
use crate::secret;
use crate::state::Shared;
use crate::stt_live::{self, Heard};
use crate::turn::{self, EventSink, TurnEvent, TurnRequest};
use crate::vad::{decode_f32le, Vad, VadEvent};
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tauri::ipc::{Channel, InvokeBody, InvokeResponseBody, Request};
use tauri::{AppHandle, Manager, State};

const MAX_FRAME_BYTES: usize = 256 * 1024;
const MAX_WAV_BYTES: usize = 25 * 1024 * 1024;
/// ElevenLabs speech-to-text calls allowed per minute (SEC L4): over this, utterances are dropped.
pub const STT_PER_MINUTE: usize = 20;

#[derive(Serialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum MicEvent {
    /// Mic level 0..1 for the orb.
    Level { level: f32 },
    /// The user spoke over One: the turn was cancelled in Rust; stop playback.
    BargeIn,
    /// Speech-to-text is running (drives the "Thinking" state).
    Transcribing { on: bool },
    /// What the user said. Rust has already started turn `turn` with it.
    Transcript { text: String, turn: u64 },
    /// What the user is saying right now (live, changes as they talk). Shown only, never stored.
    Partial { text: String },
    /// An event of a turn Rust started (tagged, so a cancelled turn's late events can be dropped).
    Turn { turn: u64, event: TurnEvent },
    /// The mic was switched off automatically (a call was detected). Unmute stays manual.
    Paused { reason: String },
    /// An unmute (the hotkey) was refused because a call is on; "Unmute anyway" is offered.
    Blocked { reason: String },
    Error { message: String },
}

// ---- pure state (no Tauri) ----------------------------------------------------------

/// Handle for one utterance that is being transcribed.
#[derive(Debug, Clone, Copy)]
pub struct Ticket {
    epoch: u64,
    seq: u64,
}

/// Keeps the transcripts of utterances in order and turns them into ONE turn. Text is held
/// while anything is still being transcribed or the user is still speaking, so "Check the hub
/// logs ... and tell Maya" becomes one request. `reset` (Stop, mute, a real barge-in) drops
/// everything pending and makes every outstanding ticket stale.
#[derive(Default)]
pub struct Merger {
    epoch: u64,
    seq: u64,
    inflight: usize,
    speaking: bool,
    done: Vec<(u64, String)>,
}

impl Merger {
    pub fn reset(&mut self) {
        self.epoch += 1;
        self.inflight = 0;
        self.speaking = false;
        self.done.clear();
    }

    pub fn speech_started(&mut self) {
        self.speaking = true;
    }

    /// Speech ended without a usable utterance (too short). May release held text.
    pub fn speech_dropped(&mut self) -> Option<String> {
        self.speaking = false;
        self.flush()
    }

    /// An utterance ended and goes to speech-to-text.
    pub fn begin(&mut self) -> Ticket {
        self.speaking = false;
        self.seq += 1;
        self.inflight += 1;
        Ticket { epoch: self.epoch, seq: self.seq }
    }

    /// Transcription finished (`None` = failed or empty). Returns the merged text to start a
    /// turn with when nothing else is pending; a stale ticket returns None.
    pub fn complete(&mut self, t: Ticket, text: Option<String>) -> Option<String> {
        if t.epoch != self.epoch {
            return None;
        }
        self.inflight = self.inflight.saturating_sub(1);
        if let Some(x) = text {
            self.done.push((t.seq, x));
        }
        self.flush()
    }

    pub fn busy(&self) -> bool {
        self.inflight > 0
    }

    /// The user went on talking right after a turn started: put what they said back in front, so
    /// it is joined with the rest of the sentence.
    pub fn carry(&mut self, text: String) {
        self.done.push((0, text)); // sequence 0 sorts before every utterance
    }

    fn flush(&mut self) -> Option<String> {
        if self.inflight > 0 || self.speaking || self.done.is_empty() {
            return None;
        }
        self.done.sort_by_key(|(s, _)| *s);
        let merged = self.done.drain(..).map(|(_, t)| t).collect::<Vec<_>>().join(" ");
        Some(merged)
    }
}

/// Barge-in only counts while a turn is running or One's audio is playing. Anything else
/// (for example the user still talking while the last utterance is being transcribed) is
/// ordinary speech and must not throw anything away.
pub fn barge_counts(turn_running: bool, audio_playing: bool) -> bool {
    turn_running || audio_playing
}

/// Sliding one-minute window over speech-to-text calls.
#[derive(Default)]
pub struct RateCap {
    stamps: VecDeque<Instant>,
}

impl RateCap {
    pub fn allow(&mut self, now: Instant, max: usize) -> bool {
        while self.stamps.front().is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(60)) {
            self.stamps.pop_front();
        }
        if self.stamps.len() >= max {
            return false;
        }
        self.stamps.push_back(now);
        true
    }
}

/// Live speech-to-text state for the speech in progress.
#[derive(Default)]
struct LiveCtl {
    conn: Option<stt_live::Live>,
    next_id: u64,
    /// The current speech is being streamed.
    sending: bool,
    /// Audio went out since the last commit.
    uncommitted: bool,
    /// The model's latest "the user has finished" estimate (from the partial text).
    last_prob: Option<f64>,
    /// One entry per Commit sent, oldest first. The socket commits only when asked (manual), so
    /// every committed text answers exactly the oldest open Commit.
    commits: VecDeque<CommitFor>,
    /// Utterances that ended and whose commit is on the way (several when the user says a short
    /// sentence before the first one's text is back), oldest first.
    waiting: Vec<Waiting>,
    /// After a failure the live socket is not retried for a while (the batch path is used).
    retry_after: Option<Instant>,
}

#[derive(Debug, PartialEq)]
enum CommitFor {
    /// The utterance with this ticket sequence number.
    Utterance(u64),
    /// A noise burst: its text is thrown away.
    Discard,
}

struct Waiting {
    ticket: Ticket,
    wav: Vec<u8>,
    speech_end: Instant,
    seq: u64,
}

/// How long the commit of an ended utterance may take before the batch path takes over.
pub const COMMIT_WAIT: Duration = Duration::from_millis(1000);
/// After the live socket failed, use the batch path for this long.
pub const LIVE_RETRY: Duration = Duration::from_secs(60);
/// Right after a turn starts from speech, the user may just have paused mid-sentence. For this
/// long the mic keeps its normal (not barge-in) sensitivity, and speech that resumes is joined to
/// what was said instead of starting a separate turn.
pub const CONTINUE_WINDOW: Duration = Duration::from_millis(700);

// ---- shared mic state -----------------------------------------------------------------

/// What the orb and the badge show. Derived in one place (here), never guessed by the webview.
#[derive(Serialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// Mic off: nothing is listened to.
    Muted,
    Listening,
    Thinking,
    Speaking,
}

/// Speaking beats thinking beats listening; with nothing happening and the mic off it is Muted.
/// "Thinking" needs real work: a turn running or speech being transcribed.
pub fn derive_status(live: bool, turn_running: bool, transcribing: bool, playing: bool) -> Status {
    if playing {
        Status::Speaking
    } else if turn_running || transcribing {
        Status::Thinking
    } else if live {
        Status::Listening
    } else {
        Status::Muted
    }
}

#[derive(Serialize, Clone, Copy)]
pub struct MicStatus {
    pub status: Status,
    /// Audio is being listened to right now (the red dot).
    pub live: bool,
    pub muted: bool,
    pub push_to_talk: bool,
}

pub struct MicState {
    vad: Mutex<Vad>,
    chan: Mutex<Option<Channel<MicEvent>>>,
    audio: Mutex<Option<Channel<InvokeResponseBody>>>,
    merger: Mutex<Merger>,
    cap: Mutex<RateCap>,
    /// The webview says One's audio is playing (it owns the player).
    playing: AtomicBool,
    turn_seq: AtomicU64,
    /// The mic is OFF at every launch and this is never persisted as "on".
    muted: AtomicBool,
    /// Bumped by every `set_muted`. An unmute that scanned for a call captures it first and
    /// applies only if it is unchanged: a Mute that arrived meanwhile always wins.
    mute_gen: AtomicU64,
    /// An unmute is checking for a call right now (a toggle then means "mute").
    unmuting: AtomicBool,
    /// Serialises "change the mute state" so a mute can never slip between the generation
    /// check and the store of an unmute.
    mute_lock: Mutex<()>,
    /// Optional push-to-talk (settings.json `push_to_talk`, off by default): while on, an
    /// unmuted mic only listens while the key is held.
    ptt_mode: AtomicBool,
    ptt_held: AtomicBool,
    live: Mutex<LiveCtl>,
    /// Live speech-to-text sessions started per minute (same cap as the one-shot calls).
    live_cap: Mutex<RateCap>,
    /// One batch of mic frames at a time, in order (the webview does not wait between calls).
    frames: Mutex<()>,
    /// The running turn has sent audio (Rust knows before the webview reports playback).
    turn_spoke: AtomicBool,
    /// The last turn started from speech: when, and what was said (see CONTINUE_WINDOW).
    recent: Mutex<Option<(Instant, String)>>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl Default for MicState {
    fn default() -> Self {
        Self::new()
    }
}

impl MicState {
    /// Starts MUTED: the VAD drops every sample until the user unmutes.
    pub fn new() -> Self {
        let mut vad = Vad::default();
        vad.set_muted(true);
        MicState {
            vad: Mutex::new(vad),
            chan: Mutex::new(None),
            audio: Mutex::new(None),
            merger: Mutex::new(Merger::default()),
            cap: Mutex::new(RateCap::default()),
            playing: AtomicBool::new(false),
            turn_seq: AtomicU64::new(0),
            muted: AtomicBool::new(true),
            mute_gen: AtomicU64::new(0),
            unmuting: AtomicBool::new(false),
            mute_lock: Mutex::new(()),
            ptt_mode: AtomicBool::new(false),
            ptt_held: AtomicBool::new(false),
            live: Mutex::new(LiveCtl::default()),
            live_cap: Mutex::new(RateCap::default()),
            frames: Mutex::new(()),
            turn_spoke: AtomicBool::new(false),
            recent: Mutex::new(None),
        }
    }

    /// Whether audio is being listened to right now.
    pub fn live(&self) -> bool {
        !self.muted.load(Ordering::SeqCst) && (!self.ptt_mode.load(Ordering::SeqCst) || self.ptt_held.load(Ordering::SeqCst))
    }

    pub fn is_muted(&self) -> bool {
        self.muted.load(Ordering::SeqCst)
    }

    pub fn status(&self, turn_running: bool) -> MicStatus {
        let transcribing = lock(&self.merger).busy();
        let status = derive_status(self.live(), turn_running, transcribing, self.playing.load(Ordering::SeqCst));
        MicStatus { status, live: self.live(), muted: self.is_muted(), push_to_talk: self.ptt_mode.load(Ordering::SeqCst) }
    }

    pub fn set_push_to_talk(&self, on: bool) {
        self.ptt_mode.store(on, Ordering::SeqCst);
        self.refresh_vad();
    }

    fn refresh_vad(&self) {
        lock(&self.vad).set_muted(!self.live());
    }

    /// Push-to-talk key down or up. Releasing ends the speech in progress (it is NOT dropped),
    /// so the events of that final utterance are returned for the caller to handle.
    pub fn set_ptt(&self, down: bool) -> Vec<VadEvent> {
        if !self.ptt_mode.load(Ordering::SeqCst) {
            return Vec::new();
        }
        self.ptt_held.store(down, Ordering::SeqCst);
        if down {
            self.refresh_vad();
            return Vec::new();
        }
        // The VAD is switched off by `ptt_done` AFTER the caller has handled these events, so the
        // end of the speech still reaches the live socket.
        lock(&self.vad).flush()
    }

    /// After a push-to-talk release was handled: stop listening.
    pub fn ptt_done(&self) {
        self.refresh_vad();
    }

    /// Stop, mute or barge-in: drop pending speech so nothing fires afterwards. The live socket
    /// is closed too, so a late commit of the old speech can never join a new utterance.
    pub fn invalidate(&self) {
        lock(&self.merger).reset();
        self.close_live();
    }

    /// Close the live speech-to-text socket and forget its pending text.
    pub fn close_live(&self) {
        let had_waiting = {
            let mut l = lock(&self.live);
            l.conn = None; // dropping the sender ends the socket task, which closes the socket
            l.sending = false;
            l.uncommitted = false;
            l.last_prob = None;
            l.commits.clear();
            !std::mem::take(&mut l.waiting).is_empty()
        };
        if had_waiting && !lock(&self.merger).busy() {
            self.send(MicEvent::Transcribing { on: false }); // the status must not stay on Thinking
        }
    }

    /// Whether the live socket is streaming the speech in progress.
    #[cfg(test)]
    fn live_sending(&self) -> bool {
        lock(&self.live).sending
    }

    /// Stop: drop pending speech everywhere, including what the VAD is still hearing, so an
    /// utterance cannot start a turn after Stop. The mute state is kept.
    pub fn stop_pending(&self) {
        lock(&self.vad).discard();
        self.invalidate();
    }

    fn send(&self, e: MicEvent) {
        if let Some(c) = lock(&self.chan).as_ref() {
            let _ = c.send(e);
        }
    }

    pub fn set_muted(&self, m: bool) {
        let _g = lock(&self.mute_lock);
        self.mute_gen.fetch_add(1, Ordering::SeqCst);
        if m {
            self.unmuting.store(false, Ordering::SeqCst); // a pending unmute is cancelled too
        }
        self.apply_muted(m);
    }

    fn apply_muted(&self, m: bool) {
        self.muted.store(m, Ordering::SeqCst);
        self.refresh_vad();
        if m {
            self.invalidate();
        }
    }

    /// The generation to hand to `try_unmute`, and the mark that an unmute is being checked.
    pub fn begin_unmute(&self) -> u64 {
        let _g = lock(&self.mute_lock);
        self.unmuting.store(true, Ordering::SeqCst);
        self.mute_gen.load(Ordering::SeqCst)
    }

    /// The unmute was refused or failed: nothing to apply.
    pub fn end_unmute(&self) {
        self.unmuting.store(false, Ordering::SeqCst);
    }

    /// Finish an unmute that started at generation `g`: it applies only if no mute or unmute
    /// happened since (the call check takes tens of milliseconds). False = it lost.
    pub fn try_unmute(&self, g: u64) -> bool {
        let _guard = lock(&self.mute_lock);
        self.unmuting.store(false, Ordering::SeqCst);
        if self.mute_gen.load(Ordering::SeqCst) != g {
            return false;
        }
        self.mute_gen.fetch_add(1, Ordering::SeqCst);
        self.apply_muted(false);
        true
    }

    /// What the mute hotkey should do, from Rust's own state: while an unmute is still being
    /// checked, or when the mic is live, it mutes.
    pub fn toggle_wants_mute(&self) -> bool {
        self.unmuting.load(Ordering::SeqCst) || !self.is_muted()
    }

    #[cfg(test)]
    pub fn mute_gen(&self) -> u64 {
        self.mute_gen.load(Ordering::SeqCst)
    }

    /// The webview reports whether One's audio is playing.
    pub fn set_playing(&self, on: bool) {
        self.playing.store(on, Ordering::SeqCst);
    }

    /// Right after a turn started from speech, before One says anything.
    fn in_continue_window(&self, now: Instant) -> bool {
        !self.playing.load(Ordering::SeqCst) && !self.turn_spoke.load(Ordering::SeqCst) && lock(&self.recent).as_ref().is_some_and(|(t, _)| now.saturating_duration_since(*t) < CONTINUE_WINDOW)
    }

    /// Speech started. If it is the same sentence going on (inside the continue window while the
    /// turn it started still runs), returns what was said before, and forgets it.
    pub fn take_continuation(&self, now: Instant, turn_running: bool) -> Option<String> {
        if !turn_running || !self.in_continue_window(now) {
            return None;
        }
        lock(&self.recent).take().map(|(_, t)| t)
    }

    /// A barge-in was heard. Returns true (and drops pending speech) only when it counts.
    pub fn on_barge(&self, turn_running: bool) -> bool {
        if !barge_counts(turn_running, self.playing.load(Ordering::SeqCst)) {
            return false;
        }
        self.invalidate();
        true
    }

    /// Feed samples. Barge mode (stricter VAD) is on only while a turn runs or audio plays.
    /// Returns events with all but the last level removed (one orb update per batch).
    pub fn feed(&self, samples: &[f32], turn_running: bool) -> Vec<VadEvent> {
        let barge = barge_counts(turn_running, self.playing.load(Ordering::SeqCst)) && !self.in_continue_window(Instant::now());
        let events = {
            let mut vad = lock(&self.vad);
            vad.set_barge(barge);
            vad.feed(samples)
        };
        let last_level = events.iter().rposition(|e| matches!(e, VadEvent::Level(_)));
        events.into_iter().enumerate().filter(|(i, e)| !matches!(e, VadEvent::Level(_)) || Some(*i) == last_level).map(|(_, e)| e).collect()
    }
}

// ---- commands -------------------------------------------------------------------------

/// Register the channels the mic events and the spoken audio go to (called once at start-up).
#[tauri::command]
pub fn mic_start(mic: State<'_, Arc<MicState>>, on_mic: Channel<MicEvent>, on_audio: Channel<InvokeResponseBody>) {
    *lock(&mic.chan) = Some(on_mic);
    *lock(&mic.audio) = Some(on_audio);
}

/// How a mute or unmute request ended.
#[derive(Serialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum MuteOutcome {
    Muted,
    Unmuted,
    /// A call is on: the mic stays off until the user forces it.
    Blocked,
    /// A mute (or another change) arrived while the call check was running: the mute wins.
    Superseded,
}

/// Mute (always works) or unmute. Unmuting first looks for a call: if one is on, the mic STAYS
/// OFF (`Blocked`) until the user asks again with `force` ("Unmute anyway"). The check takes
/// tens of milliseconds, so the unmute applies only if nothing changed the mute state meanwhile.
pub async fn request_mute(app: &AppHandle, muted: bool, force: bool) -> Result<MuteOutcome, String> {
    let mic = app.state::<Arc<MicState>>();
    if muted {
        mic.set_muted(true);
        app.state::<Arc<crate::call::CallWatch>>().clear_overrides();
        app.state::<Shared>().cancel_turn();
        return Ok(MuteOutcome::Muted);
    }
    let g = mic.begin_unmute();
    if app.state::<Shared>().settings().auto_pause {
        // Muted right now, so One's own WebView is not holding the mic.
        let now = match tokio::task::spawn_blocking(|| crate::call::signals(false)).await {
            Ok(n) => n,
            Err(_) => {
                mic.end_unmute();
                return Err("could not check for a call".into());
            }
        };
        if !app.state::<Arc<crate::call::CallWatch>>().check_unmute(&now, force) {
            mic.end_unmute();
            return Ok(MuteOutcome::Blocked);
        }
    }
    if !mic.try_unmute(g) {
        return Ok(MuteOutcome::Superseded);
    }
    // The user is about to talk: have a warm Claude ready (an idle process uses no tokens).
    let shared = app.state::<Shared>();
    if shared.focus().is_none() {
        shared.prewarm();
    }
    Ok(MuteOutcome::Unmuted)
}

#[tauri::command]
pub async fn mic_set_muted(app: AppHandle, muted: bool, force: Option<bool>) -> Result<MuteOutcome, String> {
    request_mute(&app, muted, force.unwrap_or(false)).await
}

/// The global mute hotkey. It decides from Rust's own state, never from the webview's copy, so
/// a double press is "on, then off" and ends muted.
pub fn toggle_hotkey(app: &AppHandle) {
    let want_mute = app.state::<Arc<MicState>>().toggle_wants_mute();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        match request_mute(&app, want_mute, false).await {
            Ok(MuteOutcome::Blocked) => app.state::<Arc<MicState>>().send(MicEvent::Blocked { reason: "Call detected: mic stays off".into() }),
            Ok(_) => {}
            Err(message) => app.state::<Arc<MicState>>().send(MicEvent::Error { message }),
        }
    });
}

/// Where the mic is right now (the webview asks once at start; it starts MUTED).
#[tauri::command]
pub fn mic_state(mic: State<'_, Arc<MicState>>, shared: State<'_, Shared>) -> MicStatus {
    mic.status(shared.turn_running())
}

/// Push-to-talk key down/up (only has an effect when `push_to_talk` is on in settings.json).
#[tauri::command]
pub fn mic_set_ptt(app: AppHandle, down: bool) {
    ptt(&app, down);
}

pub fn ptt(app: &AppHandle, down: bool) {
    let mic = app.state::<Arc<MicState>>();
    let _one = lock(&mic.frames); // in order with the mic frames
    for ev in mic.set_ptt(down) {
        handle_vad_event(app, mic.inner(), &app.state::<Shared>(), ev);
    }
    if !down {
        push_stream(&mic);
        mic.ptt_done();
    }
}

/// Switch the mic off because a call was detected, and tell the webview why.
pub fn pause_for_call(app: &AppHandle, reason: &str) {
    let mic = app.state::<Arc<MicState>>();
    if mic.is_muted() {
        return;
    }
    mic.set_muted(true);
    app.state::<Arc<crate::call::CallWatch>>().clear_overrides();
    app.state::<Shared>().cancel_turn();
    mic.send(MicEvent::Paused { reason: reason.to_string() });
}

/// `on` = One's audio is playing. (Whether a turn runs is known to Rust itself.)
#[tauri::command]
pub fn mic_set_barge(mic: State<'_, Arc<MicState>>, on: bool) {
    mic.set_playing(on);
}

/// Body is raw little-endian f32 samples (16 kHz mono) straight from the AudioWorklet.
#[tauri::command]
pub async fn mic_frames(app: AppHandle, mic: State<'_, Arc<MicState>>, shared: State<'_, Shared>, request: Request<'_>) -> Result<(), String> {
    let InvokeBody::Raw(bytes) = request.body() else { return Err("expected raw audio".into()) };
    if bytes.len() > MAX_FRAME_BYTES {
        return Err("audio chunk too large".into());
    }
    // One batch at a time: the VAD and the live stream must see the audio in order.
    let _one = lock(&mic.frames);
    mic.turn_spoke.store(shared.turn_spoke(), Ordering::SeqCst);
    for ev in mic.feed(&decode_f32le(bytes), shared.turn_running()) {
        handle_vad_event(&app, mic.inner(), &shared, ev);
    }
    push_stream(&mic);
    // The model is sure the user has finished and they have paused: end the utterance now
    // instead of waiting out the whole end-of-speech silence.
    let early = {
        let mut v = lock(&mic.vad);
        let l = lock(&mic.live);
        if l.sending && v.in_speech() && stt_live::early_end(v.silence_ms(), l.last_prob) {
            drop(l);
            v.flush()
        } else {
            Vec::new()
        }
    };
    for ev in early {
        handle_vad_event(&app, mic.inner(), &shared, ev);
    }
    Ok(())
}

/// Hand the speech heard since the last call to the live socket (only while it streams). Taken
/// and sent under the VAD lock, so two callers can never reorder audio.
fn push_stream(mic: &MicState) {
    let mut v = lock(&mic.vad);
    let samples = v.take_stream();
    if samples.is_empty() {
        return;
    }
    let mut l = lock(&mic.live);
    if let (true, Some(c)) = (l.sending, l.conn.as_ref()) {
        if c.send(stt_live::Cmd::Audio(stt_live::pcm16(&samples))) {
            l.uncommitted = true;
        }
    }
}

/// Remove and return the waiting utterance with this sequence number.
fn take_waiting(waiting: &mut Vec<Waiting>, seq: u64) -> Option<Waiting> {
    let i = waiting.iter().position(|w| w.seq == seq)?;
    Some(waiting.remove(i))
}

/// Send a Commit for whatever was streamed since the last one. False when there was nothing to
/// commit or the socket is gone.
fn commit(l: &mut LiveCtl, what: CommitFor) -> bool {
    match (l.uncommitted, l.conn.as_ref()) {
        (true, Some(c)) if c.send(stt_live::Cmd::Commit) => {
            l.uncommitted = false;
            l.last_prob = None;
            l.commits.push_back(what);
            true
        }
        _ => false,
    }
}

/// Speech started: stream it live, opening the socket if needed (unless the live path is
/// resting after a failure, the per-minute cap is reached, or there is no key).
fn live_begin(app: &AppHandle, mic: &Arc<MicState>) {
    let now = Instant::now();
    let need_conn = {
        let l = lock(&mic.live);
        if l.retry_after.is_some_and(|t| now < t) {
            return;
        }
        l.conn.is_none()
    };
    // The key store is read outside the lock (it can be slow).
    let key = if need_conn {
        if !lock(&mic.live_cap).allow(now, STT_PER_MINUTE) {
            return;
        }
        match secret::load_key() {
            Ok(k) => Some(k),
            Err(_) => return,
        }
    } else {
        None
    };
    let mut l = lock(&mic.live);
    if mic.is_muted() {
        return; // muted meanwhile: nothing is streamed
    }
    if l.conn.is_none() {
        let Some(key) = key else { return };
        l.next_id += 1;
        l.commits.clear();
        let (app2, mic2) = (app.clone(), mic.clone());
        l.conn = Some(stt_live::start(key, l.next_id, move |id, h| on_heard(&app2, &mic2, id, h)));
    }
    l.last_prob = None;
    l.sending = true;
}

/// An event from the live socket `id`.
fn on_heard(app: &AppHandle, mic: &Arc<MicState>, id: u64, h: Heard) {
    let mut l = lock(&mic.live);
    if l.conn.as_ref().map(|c| c.id) != Some(id) {
        return; // an old socket (closed by mute, Stop or barge-in): ignore it
    }
    match h {
        Heard::Partial { text, turn_prob } => {
            let live = l.sending;
            // A late partial of an utterance that already ended must not end the next one early.
            if live && turn_prob.is_some() {
                l.last_prob = turn_prob;
            }
            drop(l);
            if live && !text.is_empty() {
                mic.send(MicEvent::Partial { text });
            }
        }
        Heard::Committed(text) => {
            // It answers the oldest open Commit.
            let Some(CommitFor::Utterance(seq)) = l.commits.pop_front() else { return };
            // (Not found: that utterance already went the batch way.)
            if let Some(w) = take_waiting(&mut l.waiting, seq) {
                drop(l);
                finish_utterance(app, mic, w.ticket, Some(text), w.speech_end);
            }
        }
        Heard::Error(reason) => {
            let waiting = std::mem::take(&mut l.waiting);
            l.conn = None;
            l.sending = false;
            l.uncommitted = false;
            l.commits.clear();
            if !reason.is_empty() {
                eprintln!("live speech-to-text failed, using one-shot transcription: {reason}");
                l.retry_after = Some(Instant::now() + LIVE_RETRY);
            }
            drop(l);
            for w in waiting {
                batch_transcribe(app, mic, w.ticket, w.wav, w.speech_end);
            }
        }
        Heard::Other => {}
    }
}

/// The text of a finished utterance is known: merge it and start the turn when nothing else is
/// pending (a stale ticket is dropped by the merger).
fn finish_utterance(app: &AppHandle, mic: &Arc<MicState>, ticket: Ticket, text: Option<String>, speech_end: Instant) {
    let text = text.map(|t| crate::transcript::strip_tags(&t)).filter(|t| !t.is_empty());
    let merged = lock(&mic.merger).complete(ticket, text);
    if !lock(&mic.merger).busy() {
        mic.send(MicEvent::Transcribing { on: false });
    }
    if let Some(text) = merged {
        start_turn(app, mic, text, Some(speech_end));
    }
}

fn handle_vad_event(app: &AppHandle, mic: &Arc<MicState>, shared: &Shared, ev: VadEvent) {
    match ev {
        VadEvent::Level(level) => mic.send(MicEvent::Level { level }),
        VadEvent::SpeechStart => {
            // The same sentence going on after a short pause: stop the turn it started (nothing
            // was said yet) and join the two halves.
            if let Some(before) = mic.take_continuation(Instant::now(), shared.turn_running()) {
                shared.cancel_turn();
                mic.send(MicEvent::BargeIn);
                lock(&mic.merger).carry(before);
            }
            lock(&mic.merger).speech_started();
            live_begin(app, mic);
        }
        VadEvent::SpeechDropped => {
            // A noise burst: what was streamed of it is committed and its text thrown away, so it
            // never leaks into the next utterance.
            push_stream(mic);
            {
                let mut l = lock(&mic.live);
                if l.sending {
                    commit(&mut l, CommitFor::Discard);
                }
                l.sending = false;
            }
            let released = lock(&mic.merger).speech_dropped();
            if let Some(text) = released {
                start_turn(app, mic, text, None);
            }
        }
        VadEvent::BargeIn => {
            if mic.on_barge(shared.turn_running()) {
                shared.cancel_turn();
                mic.send(MicEvent::BargeIn);
            }
        }
        VadEvent::Utterance(wav) => begin_utterance(app, mic, wav),
    }
}

fn begin_utterance(app: &AppHandle, mic: &Arc<MicState>, wav: Vec<u8>) {
    // The user stopped speaking this long ago (the VAD waits out a short silence first).
    let now = Instant::now();
    let trailing = Duration::from_millis(lock(&mic.vad).last_trailing_ms() as u64);
    let speech_end = now.checked_sub(trailing).unwrap_or(now);
    let ticket = lock(&mic.merger).begin();
    mic.send(MicEvent::Transcribing { on: true });
    push_stream(mic); // the end of the speech goes out before the commit
    let seq = ticket.seq;
    let committed = {
        let mut l = lock(&mic.live);
        let streamed = std::mem::replace(&mut l.sending, false);
        let ok = streamed && commit(&mut l, CommitFor::Utterance(seq));
        if ok {
            l.waiting.push(Waiting { ticket, wav, speech_end, seq });
            None
        } else {
            Some(wav) // not streamed, or the socket is gone: the batch path
        }
    };
    let Some(wav) = committed else {
        // The commit normally comes back in ~150 ms; if not, the whole recording goes the batch
        // way (never only the part committed so far).
        let (app2, mic2) = (app.clone(), mic.clone());
        tauri::async_runtime::spawn(async move {
            tokio::time::sleep(COMMIT_WAIT).await;
            let late = {
                let mut l = lock(&mic2.live);
                if l.waiting.iter().any(|w| w.seq == seq) {
                    // A socket that cannot commit in time is not trusted: everything still waiting
                    // on it goes the batch way, whole.
                    l.conn = None;
                    l.sending = false;
                    l.uncommitted = false;
                    l.commits.clear();
                    std::mem::take(&mut l.waiting)
                } else {
                    Vec::new()
                }
            };
            if !late.is_empty() {
                eprintln!("live speech-to-text commit was late, using one-shot transcription");
            }
            for w in late {
                batch_transcribe(&app2, &mic2, w.ticket, w.wav, w.speech_end);
            }
        });
        return;
    };
    batch_transcribe(app, mic, ticket, wav, speech_end);
}

/// One-shot transcription of a finished utterance (the fallback path), within the per-minute cap.
fn batch_transcribe(app: &AppHandle, mic: &Arc<MicState>, ticket: Ticket, wav: Vec<u8>, speech_end: Instant) {
    if !lock(&mic.cap).allow(Instant::now(), STT_PER_MINUTE) {
        mic.send(MicEvent::Error { message: "Too many speech-to-text requests this minute; that speech was dropped.".into() });
        finish_utterance(app, mic, ticket, None, speech_end); // releases text held for it
        return;
    }
    let (app, m) = (app.clone(), mic.clone());
    tauri::async_runtime::spawn(async move { transcribe_and_send(app, m, ticket, wav, speech_end).await });
}

async fn transcribe_and_send(app: AppHandle, mic: Arc<MicState>, ticket: Ticket, wav: Vec<u8>, speech_end: Instant) {
    mic.send(MicEvent::Transcribing { on: true });
    let res = if wav.len() < 44 || wav.len() > MAX_WAV_BYTES {
        Ok(String::new())
    } else {
        match (secret::load_key(), eleven::client()) {
            (Ok(key), Ok(client)) => eleven::transcribe(&client, &key, wav).await,
            (Err(e), _) | (_, Err(e)) => Err(e),
        }
    };
    let text = match res {
        // Sound tags ("[door closing]") are not speech: a part that is only tags counts as empty.
        Ok(t) => Some(t),
        Err(message) => {
            mic.send(MicEvent::Error { message });
            None
        }
    };
    // Muted, stopped or interrupted meanwhile: the ticket is stale and nothing starts.
    finish_utterance(&app, &mic, ticket, text, speech_end);
}

/// Start one turn with what the user said. A running turn is cancelled by `drive` itself.
fn start_turn(app: &AppHandle, mic: &Arc<MicState>, text: String, speech_end: Option<Instant>) {
    // Optional wake word: only speech addressed to "One" becomes a turn; the word is removed.
    let Some(text) = crate::wake::filter(&text, app.state::<Shared>().settings().wake_word) else { return };
    // Junk turns (under 2 words, bar a few real one-word replies) never reach Claude.
    if !crate::transcript::worth_a_turn(&text) {
        return;
    }
    let (Some(chan), Some(audio)) = (lock(&mic.chan).clone(), lock(&mic.audio).clone()) else { return };
    let turn = mic.turn_seq.fetch_add(1, Ordering::SeqCst) + 1;
    if speech_end.is_some() {
        *lock(&mic.recent) = Some((Instant::now(), text.clone())); // see CONTINUE_WINDOW
    }
    mic.send(MicEvent::Transcript { text: text.clone(), turn });
    app.state::<Shared>().chat_push("user", &text);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        let shared = app.state::<Shared>();
        shared.clear_pending();
        let sink = EventSink::Mic { chan, turn };
        let res = turn::drive(&shared, TurnRequest { prompt: text, allowed_tool: None, speech_end }, sink.clone(), audio).await;
        if let Err(message) = res {
            sink.send(TurnEvent::Error { message });
            sink.send(TurnEvent::Done { interrupted: false });
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(frames: usize, amp: f32) -> Vec<f32> {
        (0..frames * 128).map(|i| if i % 2 == 0 { amp } else { -amp }).collect()
    }

    fn s(x: &str) -> Option<String> {
        Some(x.to_string())
    }

    #[test]
    fn feed_keeps_one_level_per_batch() {
        let m = unmuted();
        let ev = m.feed(&tone(4, 0.0), false);
        assert_eq!(ev.len(), 1);
        assert!(matches!(ev[0], VadEvent::Level(_)));
    }

    fn unmuted() -> MicState {
        let m = MicState::new();
        m.set_muted(false);
        m
    }

    #[test]
    fn a_fresh_mic_is_muted_and_listens_to_nothing() {
        // QA incident: the app must never start live. No audio is processed until the user unmutes.
        let m = MicState::new();
        assert!(m.is_muted() && !m.live());
        assert!(m.feed(&tone(200, 0.3), false).is_empty(), "loud speech while muted produces no events");
        assert!(m.feed(&tone(200, 0.3), true).is_empty());
        m.set_muted(false);
        assert!(m.live());
        assert!(!m.feed(&tone(4, 0.0), false).is_empty(), "unmuted: levels flow");
        m.set_muted(true);
        assert!(m.feed(&tone(200, 0.3), false).is_empty());
    }

    #[test]
    fn r2_m1_a_mute_during_the_call_scan_ends_muted() {
        let m = MicState::new();
        let g = m.begin_unmute(); // the unmute starts scanning for a call...
        m.set_muted(true); // ...and the user presses Mute meanwhile
        assert!(!m.try_unmute(g), "the stale unmute must lose");
        assert!(m.is_muted() && !m.live());
        assert!(m.feed(&tone(100, 0.3), false).is_empty(), "no audio is processed");
        // an unmute with nothing in between still works
        let g = m.begin_unmute();
        assert!(m.try_unmute(g) && m.live());
    }

    #[test]
    fn r2_m1_two_quick_unmutes_cannot_both_apply_and_a_double_hotkey_ends_muted() {
        let m = MicState::new();
        // press 1: muted, nothing pending: unmute
        assert!(!m.toggle_wants_mute());
        let g = m.begin_unmute();
        // press 2 while the first is still scanning: from Rust's own state it must be a MUTE
        assert!(m.toggle_wants_mute());
        m.set_muted(true);
        assert!(!m.try_unmute(g));
        assert!(m.is_muted());
        // the pending mark is cleared by the mute: the next press unmutes again
        assert!(!m.toggle_wants_mute());
        // live mic: a press mutes
        let g = m.begin_unmute();
        assert!(m.try_unmute(g));
        assert!(m.toggle_wants_mute());
        // two unmutes racing: only the first one applies
        m.set_muted(true);
        let (a, b) = (m.begin_unmute(), m.begin_unmute());
        assert!(m.try_unmute(a));
        assert!(!m.try_unmute(b));
        m.end_unmute();
        assert!(m.mute_gen() > 0);
    }

    #[test]
    fn stop_also_drops_the_utterance_still_inside_the_vad() {
        let m = unmuted();
        let loud = m.feed(&tone(60, 0.3), false); // mid-sentence
        assert!(!loud.iter().any(|e| matches!(e, VadEvent::Utterance(_))));
        m.stop_pending();
        let after = m.feed(&tone(120, 0.0), false); // the silence that would have ended it
        assert!(!after.iter().any(|e| matches!(e, VadEvent::Utterance(_) | VadEvent::SpeechDropped)), "Stop must not leave an utterance behind");
        assert!(m.live(), "Stop does not change the mute state");
        // without Stop the same input DOES produce an utterance
        let m2 = unmuted();
        m2.feed(&tone(60, 0.3), false);
        assert!(m2.feed(&tone(120, 0.0), false).iter().any(|e| matches!(e, VadEvent::Utterance(_))));
    }

    #[test]
    fn d_the_initial_status_is_muted_not_thinking() {
        let m = MicState::new();
        let s = m.status(false);
        assert_eq!(s.status, Status::Muted);
        assert!(s.muted && !s.live && !s.push_to_talk);
        m.set_muted(false);
        assert_eq!(m.status(false).status, Status::Listening);
        assert_eq!(m.status(true).status, Status::Thinking);
        // being transcribed is thinking too; speaking beats both
        let t = lock(&m.merger).begin();
        assert_eq!(m.status(false).status, Status::Thinking);
        m.set_playing(true);
        assert_eq!(m.status(false).status, Status::Speaking);
        let _ = t;
    }

    #[test]
    fn derive_status_priorities() {
        assert_eq!(derive_status(false, false, false, false), Status::Muted);
        assert_eq!(derive_status(true, false, false, false), Status::Listening);
        assert_eq!(derive_status(false, true, false, false), Status::Thinking, "an answer in progress shows even with the mic off");
        assert_eq!(derive_status(true, true, true, true), Status::Speaking);
    }

    #[test]
    fn push_to_talk_is_off_by_default_and_listens_only_while_held() {
        let m = unmuted();
        assert!(m.set_ptt(true).is_empty() && m.live(), "ptt mode off: ignored, hands-free stays on");
        m.set_push_to_talk(true);
        assert!(!m.live(), "ptt mode on: not listening until the key is held");
        assert!(m.feed(&tone(100, 0.3), false).is_empty());
        m.set_ptt(true);
        assert!(m.live());
        m.feed(&tone(80, 0.3), false);
        let ev = m.set_ptt(false);
        assert!(ev.iter().any(|e| matches!(e, VadEvent::Utterance(_))), "release ends the utterance instead of dropping it");
        assert!(!m.live());
        // a muted mic ignores the key entirely
        m.set_muted(true);
        m.set_ptt(true);
        assert!(!m.live());
    }

    #[test]
    fn barge_in_is_reported_only_while_a_turn_runs_or_audio_plays() {
        let loud = tone(40, 0.3);
        let has = |m: &MicState, running| m.feed(&loud, running).iter().any(|e| *e == VadEvent::BargeIn);
        assert!(!has(&unmuted(), false), "idle: ordinary speech");
        assert!(has(&unmuted(), true), "a turn is running");
        let m = unmuted();
        m.set_playing(true);
        assert!(has(&m, false), "audio is playing");
    }

    #[test]
    fn m1_speech_while_only_transcribing_is_not_a_barge_in_and_keeps_both_texts_in_order() {
        let m = MicState::new();
        // utterance 1 goes to STT; no turn runs and nothing plays
        let t1 = lock(&m.merger).begin();
        // the user starts talking again: not a barge-in, nothing is invalidated
        assert!(!m.on_barge(false));
        lock(&m.merger).speech_started();
        // utterance 1's text arrives while the user is still speaking: held, no turn yet
        assert_eq!(lock(&m.merger).complete(t1, s("Check the hub logs")), None);
        // utterance 2 ends and is transcribed
        let t2 = lock(&m.merger).begin();
        assert_eq!(lock(&m.merger).complete(t2, s("and tell Maya")), Some("Check the hub logs and tell Maya".into()));
        assert_eq!(lock(&m.merger).complete(t2, None), None, "released once");
    }

    #[test]
    fn m1_results_arriving_out_of_order_are_joined_in_speaking_order() {
        let mut g = Merger::default();
        let (t1, t2) = (g.begin(), g.begin());
        assert_eq!(g.complete(t2, s("second")), None);
        assert_eq!(g.complete(t1, s("first")), Some("first second".into()));
    }

    #[test]
    fn m1_a_failed_or_empty_part_does_not_block_the_rest() {
        let mut g = Merger::default();
        let (t1, t2) = (g.begin(), g.begin());
        assert_eq!(g.complete(t1, None), None);
        assert_eq!(g.complete(t2, s("hello")), Some("hello".into()));
        // a too-short noise burst releases held text
        let t3 = g.begin();
        g.speech_started();
        assert_eq!(g.complete(t3, s("held")), None);
        assert_eq!(g.speech_dropped(), Some("held".into()));
    }

    #[test]
    fn m1_barge_in_while_a_turn_runs_invalidates_pending_speech() {
        let m = MicState::new();
        let t = lock(&m.merger).begin();
        assert!(m.on_barge(true), "a turn is running: it counts");
        assert_eq!(lock(&m.merger).complete(t, s("old")), None, "the pre-barge transcript is dropped");
        let m = MicState::new();
        m.set_playing(true);
        assert!(m.on_barge(false), "audio is playing: it counts");
    }

    #[test]
    fn stop_and_mute_drop_a_pending_transcript() {
        // Stop (cancel_turn calls invalidate)
        let m = MicState::new();
        let t = lock(&m.merger).begin();
        m.invalidate();
        assert_eq!(lock(&m.merger).complete(t, s("late")), None);
        assert!(!lock(&m.merger).busy());
        // text already held when Stop arrives is dropped too
        let (a, b) = {
            let mut g = lock(&m.merger);
            (g.begin(), g.begin())
        };
        assert_eq!(lock(&m.merger).complete(a, s("held")), None);
        m.invalidate();
        assert_eq!(lock(&m.merger).complete(b, s("after")), None);
        // mute
        let t = lock(&m.merger).begin();
        m.set_muted(true);
        assert_eq!(lock(&m.merger).complete(t, s("late")), None);
        m.set_muted(false);
        assert_eq!(lock(&m.merger).complete(t, s("still late")), None, "mute bumped the epoch for good");
        let t = lock(&m.merger).begin();
        assert_eq!(lock(&m.merger).complete(t, s("fresh")), Some("fresh".into()));
    }

    #[test]
    fn mute_stop_and_barge_in_close_the_live_stream_and_drop_its_text() {
        for how in ["mute", "stop", "barge"] {
            let m = unmuted();
            {
                let mut l = lock(&m.live);
                l.sending = true;
                l.uncommitted = true;
                l.commits.push_back(CommitFor::Utterance(1));
                l.waiting.push(Waiting { ticket: lock(&m.merger).begin(), wav: vec![], speech_end: Instant::now(), seq: 1 });
            }
            match how {
                "mute" => m.set_muted(true),
                "stop" => m.stop_pending(),
                _ => assert!(m.on_barge(true)),
            }
            let l = lock(&m.live);
            assert!(l.conn.is_none() && !l.sending && l.commits.is_empty() && l.waiting.is_empty() && !l.uncommitted, "{how}");
            drop(l);
            assert!(!m.live_sending());
        }
    }

    #[test]
    fn a_muted_mic_hands_nothing_to_the_live_stream() {
        let m = MicState::new();
        m.feed(&tone(100, 0.3), false);
        assert!(lock(&m.vad).take_stream().is_empty(), "muted: no audio is even buffered for streaming");
        let m = unmuted();
        m.feed(&tone(100, 0.3), false);
        assert!(!lock(&m.vad).take_stream().is_empty());
    }

    #[test]
    fn commits_are_answered_in_order_and_a_noise_commit_is_thrown_away() {
        let mut l = LiveCtl::default();
        assert!(!commit(&mut l, CommitFor::Discard), "no socket: nothing to commit");
        l.uncommitted = true;
        assert!(!commit(&mut l, CommitFor::Utterance(1)), "still no socket");
        assert!(l.commits.is_empty() && l.uncommitted);
    }

    #[test]
    fn m6_speech_resuming_right_after_a_turn_started_continues_the_same_sentence() {
        let m = unmuted();
        let t0 = Instant::now();
        *lock(&m.recent) = Some((t0, "Check the hub logs".into()));
        assert_eq!(m.take_continuation(t0 + Duration::from_millis(300), false), None, "the turn already ended");
        assert!(m.in_continue_window(t0 + Duration::from_millis(300)));
        // while the window is open the mic keeps normal sensitivity: a quiet "and tell Maya" is heard
        assert!(!m.feed(&tone(1, 0.0), true).is_empty());
        assert_eq!(m.take_continuation(t0 + Duration::from_millis(300), true).as_deref(), Some("Check the hub logs"));
        assert_eq!(m.take_continuation(t0 + Duration::from_millis(310), true), None, "used once");
        *lock(&m.recent) = Some((t0, "x".into()));
        assert_eq!(m.take_continuation(t0 + CONTINUE_WINDOW, true), None, "after the window it is a new thought (or a barge-in)");
        m.set_playing(true);
        *lock(&m.recent) = Some((Instant::now(), "x".into()));
        assert!(!m.in_continue_window(Instant::now()), "once One speaks, talking over it is a barge-in");
        // the merger puts the first half back in front of the rest
        let mut g = Merger::default();
        g.carry("Check the hub logs".into());
        let t = g.begin();
        assert_eq!(g.complete(t, s("and tell Maya")), Some("Check the hub logs and tell Maya".into()));
    }

    #[test]
    fn r2_h1_two_utterances_waiting_for_their_commits_both_get_their_own_text() {
        let m = unmuted();
        let (t1, t2) = {
            let mut g = lock(&m.merger);
            (g.begin(), g.begin())
        };
        {
            let mut l = lock(&m.live);
            l.waiting.push(Waiting { ticket: t1, wav: vec![], speech_end: Instant::now(), seq: t1.seq });
            l.waiting.push(Waiting { ticket: t2, wav: vec![], speech_end: Instant::now(), seq: t2.seq });
        }
        // the commits are answered in order: each finds its own utterance
        let w1 = take_waiting(&mut lock(&m.live).waiting, t1.seq).expect("first still waiting");
        assert_eq!(lock(&m.merger).complete(w1.ticket, s("Check the hub")), None, "held: the second is still out");
        let w2 = take_waiting(&mut lock(&m.live).waiting, t2.seq).expect("second not overwritten");
        assert_eq!(lock(&m.merger).complete(w2.ticket, s("and tell Maya")), Some("Check the hub and tell Maya".into()));
        assert!(take_waiting(&mut lock(&m.live).waiting, t1.seq).is_none());
    }

    #[test]
    fn r2_m1_the_continue_window_closes_once_the_reply_has_audio() {
        let m = unmuted();
        *lock(&m.recent) = Some((Instant::now(), "x".into()));
        assert!(m.in_continue_window(Instant::now()));
        m.turn_spoke.store(true, Ordering::SeqCst);
        assert!(!m.in_continue_window(Instant::now()), "One's own voice must not be taken for the user going on");
    }

    #[test]
    fn m4_closing_the_live_socket_with_a_commit_pending_clears_thinking() {
        let m = unmuted();
        let t = lock(&m.merger).begin();
        lock(&m.live).waiting.push(Waiting { ticket: t, wav: vec![], speech_end: Instant::now(), seq: t.seq });
        m.stop_pending();
        assert!(!lock(&m.merger).busy() && lock(&m.live).waiting.is_empty());
        assert_eq!(m.status(false).status, Status::Listening, "not stuck on Thinking");
    }

    #[test]
    fn stt_calls_are_capped_per_minute() {
        let mut c = RateCap::default();
        let t0 = Instant::now();
        for i in 0..STT_PER_MINUTE {
            assert!(c.allow(t0 + Duration::from_millis(i as u64), STT_PER_MINUTE));
        }
        assert!(!c.allow(t0 + Duration::from_secs(30), STT_PER_MINUTE), "over the cap");
        assert!(c.allow(t0 + Duration::from_secs(61), STT_PER_MINUTE), "the window slid");
    }
}
