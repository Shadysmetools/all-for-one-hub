//! Shared app state: settings, persisted Claude session id, pending approvals,
//! and the cancel token of the running turn.

use crate::claude::Denial;
use crate::eleven::DEFAULT_VOICE;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use tokio_util::sync::CancellationToken;

/// One's default working directory: the home folder (set `cwd` in settings.json to change it).
pub fn default_cwd() -> String {
    crate::secret::home_dir().map(|h| h.to_string_lossy().into_owned()).unwrap_or_else(|| ".".into())
}

/// Everything in settings.json. `cwd` and the Team keys are hand-edited and file-only (M1).
/// Every field has a default, so a partial hand-edited file keeps what the user did write.
#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Settings {
    pub cwd: String,
    pub voice_id: String,
    /// The voice for Arabic replies. Empty = the main voice speaks Arabic too.
    pub voice_id_ar: String,
    /// "main" (the normal window, default) or "mini" (the compact orb). Remembered between runs.
    pub window_mode: String,
    /// Hand-edited, off by default: the unmuted mic only listens while the key is held.
    pub push_to_talk: bool,
    /// Hand-edited, off by default: only speech addressed to "One" becomes a turn.
    pub wake_word: bool,
    /// Hand-edited, on by default: mute when a call is detected.
    pub auto_pause: bool,
    /// The full window's logical size as the user left it (none on first launch).
    pub main_size: Option<[f64; 2]>,
}

/// A voice id as ElevenLabs makes them: empty (the default) or up to 64 ASCII letters and digits.
pub fn valid_voice_id(v: &str) -> bool {
    v.len() <= 64 && v.chars().all(|c| c.is_ascii_alphanumeric())
}

/// The full window's size on first launch, and the smallest it may be.
pub const FIRST_LAUNCH_SIZE: [f64; 2] = [1200.0, 800.0];
pub const MIN_WINDOW: [f64; 2] = [760.0, 560.0];

/// Keep a window size on screen: at most 90% of the monitor (logical size, if known), and not
/// below the minimum unless the screen itself is smaller than that.
pub fn clamp_window(size: [f64; 2], screen: Option<[f64; 2]>) -> [f64; 2] {
    let Some(screen) = screen else { return size };
    let one = |v: f64, max: f64, min: f64| v.min(max * 0.9).max(min.min(max * 0.9)).round();
    [one(size[0], screen[0], MIN_WINDOW[0]), one(size[1], screen[1], MIN_WINDOW[1])]
}

/// The ONLY settings the webview may read or write: UI preferences. No working directory,
/// no binary or script paths. Unknown fields are rejected, so a forged `cwd` fails loudly.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct UiPrefs {
    pub voice_id: String,
    /// The voice for Arabic replies (empty = the main voice).
    #[serde(default)]
    pub voice_id_ar: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            cwd: default_cwd(),
            voice_id: DEFAULT_VOICE.into(),
            voice_id_ar: String::new(),
            window_mode: "main".into(),
            push_to_talk: false,
            wake_word: false,
            auto_pause: true,
            main_size: None,
        }
    }
}

/// Hand-edited keys in settings.json that the webview can NOT change: `save_settings`
/// only accepts `Settings`, and rewrites these from Rust's copy. Read at start-up.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct TeamConfig {
    /// Who "I" am on the Team board (matched to a name in team-asks.json).
    pub me: String,
    pub remind_python: String,
    pub remind_script: String,
    /// Automatic 3-day reminders. OFF unless the file says `"auto_remind": true`.
    pub auto_remind: bool,
    /// Where the coordinator writes its status notes (see `autoboard.rs`). Empty = `STATUS.md` in the working directory.
    pub status_file: String,
    /// Where the coordinator writes team asks (the team-asks.json format). Empty = One's data folder.
    pub team_asks_file: String,
    /// The session "Chat with One" and the overall "Ask about" talk to by default (matched by tab
    /// title or name). When it is not live, the newest live `all-for-one-<n>` session is used.
    pub coordinator_session: String,
}

impl Default for TeamConfig {
    fn default() -> Self {
        TeamConfig {
            me: "Alex".into(),
            remind_python: crate::remind::DEFAULT_PYTHON.into(),
            remind_script: crate::remind::DEFAULT_SCRIPT.into(),
            auto_remind: false,
            status_file: String::new(),
            team_asks_file: String::new(),
            coordinator_session: crate::route::DEFAULT_COORDINATOR.into(),
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    #[default]
    En,
    Ar,
}
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    Light,
    #[default]
    Dark,
}
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Mine,
    #[default]
    Team,
}
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Default)]
#[serde(rename_all = "lowercase")]
pub enum View {
    #[default]
    Cards,
    Graph,
}

/// Team screen preferences. They live in settings.json under `team_prefs`; the webview keeps
/// no persisted state of its own. Unknown values are rejected, so nothing odd gets stored.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct TeamPrefs {
    pub lang: Lang,
    pub theme: Theme,
    pub scope: Scope,
    pub view: View,
    pub helpers: bool,
    /// Which cards are expanded, by name.
    pub open: std::collections::BTreeMap<String, bool>,
}

impl TeamPrefs {
    pub fn validate(&self) -> Result<(), String> {
        if self.open.len() > 100 || self.open.keys().any(|k| k.chars().count() > 80) {
            return Err("too many open cards".into());
        }
        Ok(())
    }
}

/// One voice test at a time, with a cooldown after each (it costs speech quota).
pub struct VoiceGate {
    busy: bool,
    last: Option<std::time::Instant>,
}

impl VoiceGate {
    pub const fn new() -> Self {
        VoiceGate { busy: false, last: None }
    }

    pub fn try_start(&mut self, now: std::time::Instant, cooldown: std::time::Duration) -> Result<(), String> {
        if self.busy {
            return Err("A voice test is already playing.".into());
        }
        if self.last.is_some_and(|l| now.duration_since(l) < cooldown) {
            return Err("Please wait a few seconds before testing the voice again.".into());
        }
        self.busy = true;
        Ok(())
    }

    pub fn finish(&mut self, now: std::time::Instant) {
        self.busy = false;
        self.last = Some(now);
    }
}

pub struct Shared {
    dir: PathBuf,
    team_cfg: TeamConfig,
    /// settings.json exists but could not be parsed: it is never rewritten (CR-M3).
    broken: bool,
    team_prefs: Mutex<TeamPrefs>,
    settings: Mutex<Settings>,
    /// One's OWN Claude session. Never another session's id.
    session: Mutex<Option<String>>,
    /// A turn in this session carried another session's extract: approval cards stay Deny-only
    /// until "Start a new conversation" (One's own replies in that turn are in its history).
    tainted: Mutex<bool>,
    /// The session the user asked One to look at ("Ask about a session"). In memory only; One
    /// still runs in its own session and only gets a read-only extract of this one's tail.
    focus: Mutex<Option<String>>,
    pending: Mutex<HashMap<String, Denial>>,
    history: Mutex<crate::history::History>,
    /// The running turn: (id, token). Cleared by `end_turn` when the turn finishes.
    current: Mutex<Option<(u64, CancellationToken)>>,
    turn_seq: AtomicU64,
    /// The warm Claude process for plain turns (see brain.rs). Taken out while a turn uses it.
    warm: Mutex<Option<crate::brain::Warm>>,
    /// Bumped whenever a warm process may hold a stale copy of the conversation: before every
    /// one-shot turn (it writes to the session behind the warm process's back) and on "Start a
    /// new conversation". Part of the warm key, so a process from an older generation is never
    /// used or put back.
    warm_gen: AtomicU64,
    /// Bumped by "Start a new conversation": a turn that started before it never saves its
    /// session id afterwards (the old conversation must not come back).
    conv_gen: AtomicU64,
    /// The running turn has sent its first audio (reset when a turn begins).
    turn_spoke: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

/// A crash can leave a `focus-*.txt` prompt file (it holds another session's text): delete them.
fn remove_leftover_prompt_files(dir: &PathBuf) {
    let Ok(files) = std::fs::read_dir(dir) else { return };
    for f in files.flatten() {
        let name = f.file_name().to_string_lossy().into_owned();
        if name.starts_with("focus-") && name.ends_with(".txt") {
            let _ = std::fs::remove_file(f.path());
        }
    }
}

fn read_json<T: for<'a> Deserialize<'a>>(p: &PathBuf) -> Option<T> {
    serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()
}

/// (settings, broken). A missing file is fine (defaults); an unparsable one is "broken".
fn read_settings(path: &PathBuf) -> (Settings, bool) {
    match std::fs::read_to_string(path) {
        Err(_) => (Settings::default(), false),
        Ok(t) => match serde_json::from_str::<Settings>(&t) {
            Ok(s) => (s, false),
            Err(_) => {
                eprintln!("settings.json could not be read; using defaults and NOT rewriting it");
                (Settings::default(), true)
            }
        },
    }
}

impl Shared {
    pub fn load(dir: PathBuf) -> Self {
        let _ = std::fs::create_dir_all(&dir);
        let (settings, broken) = read_settings(&dir.join("settings.json"));
        // A saved session belongs to one working directory: a different (or unknown) one drops it.
        // The id must be a strict UUID: session.json is a file anyone with the user's rights can edit.
        let saved = read_json::<serde_json::Value>(&dir.join("session.json")).filter(|v| v["cwd"].as_str() == Some(settings.cwd.as_str()));
        // Only a session One itself created (`own: true`) is kept. An older file (a picked session
        // or a `fork` marker) may name someone else's transcript: it is ignored and One starts fresh.
        let session = saved
            .as_ref()
            .filter(|v| v["own"].as_bool() == Some(true) && v["fork"].as_bool() != Some(true))
            .and_then(|v| v["session_id"].as_str())
            .filter(|id| crate::tabs::valid_session_id(id))
            .map(String::from);
        // A session file without the field predates the taint mark (it may have carried an extract): tainted.
        let tainted = session.is_some() && saved.as_ref().and_then(|v| v["tainted"].as_bool()).unwrap_or(true);
        remove_leftover_prompt_files(&dir);
        let team_cfg = read_json::<TeamConfig>(&dir.join("settings.json")).unwrap_or_default();
        let team_prefs = read_json::<serde_json::Value>(&dir.join("settings.json"))
            .and_then(|v| serde_json::from_value::<TeamPrefs>(v["team_prefs"].clone()).ok())
            .filter(|p| p.validate().is_ok())
            .unwrap_or_default();
        let mut history = crate::history::History::load(Some(dir.clone()));
        // the focus is never restored at start, so text from a focus left open at exit goes now
        history.drop_focus_turns();
        crate::tabs::set_own(session.as_deref());
        Shared {
            dir,
            team_cfg,
            broken,
            team_prefs: Mutex::new(team_prefs),
            settings: Mutex::new(settings),
            session: Mutex::new(session),
            tainted: Mutex::new(tainted),
            focus: Mutex::new(None),
            history: Mutex::new(history),
            pending: Mutex::new(HashMap::new()),
            current: Mutex::new(None),
            warm: Mutex::new(None),
            warm_gen: AtomicU64::new(0),
            conv_gen: AtomicU64::new(0),
            turn_spoke: Default::default(),
            turn_seq: AtomicU64::new(0),
        }
    }

    /// One's data folder (settings, session, temporary prompt files).
    pub fn data_dir(&self) -> PathBuf {
        self.dir.clone()
    }

    pub fn team_config(&self) -> TeamConfig {
        self.team_cfg.clone()
    }

    pub fn settings(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }

    pub fn ui_prefs(&self) -> UiPrefs {
        let s = self.settings();
        UiPrefs { voice_id: s.voice_id, voice_id_ar: s.voice_id_ar }
    }

    /// Saves UI prefs only. The working directory and the Team keys are carried over from
    /// what was read at start-up; the webview cannot change them.
    pub fn save_prefs(&self, p: UiPrefs) -> Result<(), String> {
        if self.broken {
            return Err("settings.json could not be read; fix it by hand (it was left untouched)".into());
        }
        for v in [&p.voice_id, &p.voice_id_ar] {
            if !valid_voice_id(v) {
                return Err("not a voice id".into());
            }
        }
        let s = Settings { voice_id: p.voice_id, voice_id_ar: p.voice_id_ar, ..self.settings() };
        self.write_key("voice_id", serde_json::Value::String(s.voice_id.clone()))?;
        self.write_key("voice_id_ar", serde_json::Value::String(s.voice_id_ar.clone()))?;
        *self.settings.lock().unwrap() = s;
        Ok(())
    }

    /// Update ONE key inside the existing settings.json, so every hand-edited key survives.
    /// A file that stops parsing while the app runs is REFUSED and left untouched (the safer
    /// choice: a backup copy would still lose the user's live edits). A missing file is created.
    /// The write goes to a temp file in the same directory, then a rename, so a crash can
    /// never leave a truncated settings.json.
    fn write_key(&self, key: &str, value: serde_json::Value) -> Result<(), String> {
        let path = self.dir.join("settings.json");
        let mut doc = match std::fs::read_to_string(&path) {
            Ok(t) => match serde_json::from_str::<serde_json::Value>(&t) {
                Ok(v) if v.is_object() => v,
                _ => return Err("settings.json could not be read; fix it by hand (it was left untouched)".into()),
            },
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::to_value(self.settings()).unwrap_or_default(),
            Err(_) => return Err("could not read settings.json".into()),
        };
        doc[key] = value;
        let json = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
        let tmp = self.dir.join("settings.json.tmp");
        std::fs::write(&tmp, json).and_then(|_| std::fs::rename(&tmp, &path)).map_err(|_| {
            let _ = std::fs::remove_file(&tmp);
            "could not save settings".to_string()
        })
    }

    /// Remember the window mode ("main" or "mini"). Anything else is refused.
    pub fn save_window_mode(&self, mode: &str) -> Result<(), String> {
        if mode != "main" && mode != "mini" {
            return Err("unknown mode".into());
        }
        if self.broken {
            return Err("settings.json could not be read; fix it by hand (it was left untouched)".into());
        }
        self.write_key("window_mode", serde_json::Value::String(mode.to_string()))?;
        self.settings.lock().unwrap().window_mode = mode.to_string();
        Ok(())
    }

    /// Remember the full window's size. Implausible sizes are refused.
    pub fn save_main_size(&self, size: [f64; 2]) -> Result<(), String> {
        if self.broken {
            return Err("settings.json could not be read; fix it by hand (it was left untouched)".into());
        }
        if !size.iter().all(|v| v.is_finite() && (200.0..=20_000.0).contains(v)) {
            return Err("implausible window size".into());
        }
        let size = [size[0].round(), size[1].round()];
        self.write_key("main_size", serde_json::json!(size))?;
        self.settings.lock().unwrap().main_size = Some(size);
        Ok(())
    }

    pub fn team_prefs(&self) -> TeamPrefs {
        self.team_prefs.lock().unwrap().clone()
    }

    pub fn save_team_prefs(&self, p: TeamPrefs) -> Result<(), String> {
        if self.broken {
            return Err("settings.json could not be read; fix it by hand (it was left untouched)".into());
        }
        p.validate()?;
        self.write_key("team_prefs", serde_json::to_value(&p).map_err(|e| e.to_string())?)?;
        *self.team_prefs.lock().unwrap() = p;
        Ok(())
    }

    pub fn session_id(&self) -> Option<String> {
        self.session.lock().unwrap().clone()
    }

    /// Which conversation is current ("Start a new conversation" moves it on).
    pub fn conv_gen(&self) -> u64 {
        self.conv_gen.load(Ordering::SeqCst)
    }

    /// Remember the session a turn ended in, unless the user started a new conversation since the
    /// turn began (`conv` = `conv_gen()` at its start).
    pub fn set_session_for(&self, conv: u64, id: &str) {
        if self.conv_gen() != conv {
            return;
        }
        self.set_session(id);
    }

    /// Remember the session One itself is in (its own id after a turn).
    pub fn set_session(&self, id: &str) {
        if !crate::tabs::valid_session_id(id) {
            eprintln!("ignoring a session id that is not a UUID");
            return;
        }
        *self.session.lock().unwrap() = Some(id.to_string());
        crate::tabs::set_own(Some(id));
        let cwd = self.settings().cwd;
        self.write_session_file(id, &cwd);
    }

    fn write_session_file(&self, id: &str, cwd: &str) {
        let tainted = *self.tainted.lock().unwrap();
        let _ = std::fs::write(self.dir.join("session.json"), serde_json::json!({ "session_id": id, "cwd": cwd, "own": true, "tainted": tainted }).to_string());
    }

    pub fn tainted(&self) -> bool {
        *self.tainted.lock().unwrap()
    }

    /// Remember that a turn of this session carried another session's extract.
    pub fn set_tainted(&self) {
        *self.tainted.lock().unwrap() = true;
        let id = self.session.lock().unwrap().clone();
        if let Some(id) = id {
            self.write_session_file(&id, &self.settings().cwd);
        }
    }

    /// The session the user asked about, if any.
    pub fn focus(&self) -> Option<String> {
        self.focus.lock().unwrap().clone()
    }

    /// "Ask about a session": only a strict UUID is accepted. `None` clears it (One's own session).
    pub fn set_focus(&self, id: Option<&str>) -> Result<(), String> {
        if id.is_some_and(|id| !crate::tabs::valid_session_id(id)) {
            return Err("not a session id".into());
        }
        *self.focus.lock().unwrap() = id.map(String::from);
        if id.is_none() {
            self.history.lock().unwrap().drop_focus_turns();
        }
        Ok(())
    }

    /// Another session's text may be in play (the focus is set, or an earlier turn carried it):
    /// replies must not offer live links.
    pub fn locked(&self) -> bool {
        self.focus.lock().unwrap().is_some() || *self.tainted.lock().unwrap()
    }

    /// Forget the saved session. Only the user asks for this ("Start a new conversation"):
    /// a failed resume is reported, never silently turned into a fresh session.
    pub fn clear_session(&self) {
        *self.session.lock().unwrap() = None;
        crate::tabs::set_own(None);
        *self.focus.lock().unwrap() = None;
        *self.tainted.lock().unwrap() = false;
        self.history.lock().unwrap().clear(); // a new conversation starts an empty thread
        let _ = std::fs::remove_file(self.dir.join("session.json"));
        self.conv_gen.fetch_add(1, Ordering::SeqCst);
        self.drop_warm(); // its in-memory conversation is the old one
    }

    /// The chat thread (last 50 messages).
    pub fn chat_history(&self) -> Vec<crate::history::ChatMsg> {
        self.history.lock().unwrap().all()
    }

    pub fn chat_push(&self, role: &str, text: &str) {
        // a message written while another session is in view is marked, and goes when the focus clears
        let focus = self.focus.lock().unwrap().is_some();
        self.chat_push_marked(role, text, focus);
    }

    /// Like `chat_push`, with the mark decided by the caller: a reply is marked by the turn that
    /// produced it (the focus may have been cleared while it streamed), and then dropped at once
    /// if that focus is already gone.
    pub fn chat_push_marked(&self, role: &str, text: &str, focus: bool) {
        let mut h = self.history.lock().unwrap();
        h.push_with(role, text, focus);
        if focus && self.focus.lock().unwrap().is_none() {
            h.drop_focus_turns();
        }
    }

    /// Replace pending approvals (one set per finished turn).
    pub fn set_pending(&self, d: Vec<Denial>) {
        let mut p = self.pending.lock().unwrap();
        p.clear();
        p.extend(d.into_iter().map(|x| (x.tool_use_id.clone(), x)));
    }

    pub fn clear_pending(&self) {
        self.pending.lock().unwrap().clear();
    }

    /// One-shot: an approval can be consumed exactly once.
    pub fn take_pending(&self, id: &str) -> Option<Denial> {
        self.pending.lock().unwrap().remove(id)
    }

    /// Set by the speaker on a turn's first audio chunk.
    pub fn turn_spoke_flag(&self) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        self.turn_spoke.clone()
    }

    pub fn turn_spoke(&self) -> bool {
        self.turn_spoke.load(Ordering::SeqCst)
    }

    /// Cancel any running turn and register a fresh token (with its id).
    pub fn begin_turn_id(&self) -> (u64, CancellationToken) {
        self.turn_spoke.store(false, Ordering::SeqCst);
        let mut cur = self.current.lock().unwrap();
        if let Some((_, old)) = cur.take() {
            old.cancel();
        }
        let id = self.turn_seq.fetch_add(1, Ordering::SeqCst) + 1;
        let t = CancellationToken::new();
        *cur = Some((id, t.clone()));
        (id, t)
    }

    #[cfg(test)]
    pub fn begin_turn(&self) -> CancellationToken {
        self.begin_turn_id().1
    }

    /// Turn `id` is over. A newer turn that replaced it is left alone.
    pub fn end_turn(&self, id: u64) {
        let mut cur = self.current.lock().unwrap();
        if cur.as_ref().is_some_and(|(cur_id, _)| *cur_id == id) {
            *cur = None;
        }
    }

    /// Whether a turn is running right now (barge-in only counts while one is).
    pub fn turn_running(&self) -> bool {
        self.current.lock().unwrap().is_some()
    }

    pub fn cancel_turn(&self) {
        if let Some((_, t)) = self.current.lock().unwrap().take() {
            t.cancel();
        }
    }

    /// What a warm process must have been started for to serve the next plain turn.
    pub fn warm_key(&self) -> crate::brain::WarmKey {
        crate::brain::WarmKey { cwd: self.settings().cwd, session: self.session_id(), lockdown: self.tainted(), gen: self.warm_gen.load(Ordering::SeqCst) }
    }

    /// The warm process, if one is running for exactly the current key. A process for another
    /// key or an older generation (or a dead one) is killed.
    pub fn take_warm(&self) -> Option<crate::brain::Warm> {
        let key = self.warm_key();
        let mut w = self.warm.lock().unwrap().take()?;
        if w.key == key && w.alive() {
            return Some(w);
        }
        crate::brain::kill_later(w);
        None
    }

    /// Put a healthy warm process back after a good turn, if it still matches the current key
    /// (same generation: no one-shot turn or new conversation happened meanwhile). Else it dies.
    pub fn put_warm(&self, w: crate::brain::Warm) {
        if w.key != self.warm_key() {
            crate::brain::kill_later(w);
            return;
        }
        if let Some(old) = self.warm.lock().unwrap().replace(w) {
            crate::brain::kill_later(old);
        }
    }

    /// Kill the warm process and move to a new generation, so a process being spawned or used
    /// right now can never be put back (before a one-shot turn, a new conversation).
    pub fn drop_warm(&self) {
        self.warm_gen.fetch_add(1, Ordering::SeqCst);
        if let Some(w) = self.warm.lock().unwrap().take() {
            crate::brain::kill_later(w);
        }
    }

    #[cfg(test)]
    pub fn has_warm(&self) -> bool {
        self.warm.lock().unwrap().is_some()
    }

    /// Start a warm process now for the next plain turn, unless one is ready. Cheap: an idle
    /// claude process uses no tokens until a message is written to it. Never while a turn runs
    /// (a one-shot turn may still be writing to the session). The spawn happens outside the lock.
    pub fn prewarm(&self) {
        // The key first: a one-shot turn starting right after this moves the generation on, so the
        // process spawned below is refused by put_warm.
        let key = self.warm_key();
        if self.turn_running() {
            return;
        }
        {
            let mut slot = self.warm.lock().unwrap();
            if let Some(w) = slot.as_mut() {
                if w.key == key && w.alive() {
                    return;
                }
            }
            if let Some(old) = slot.take() {
                crate::brain::kill_later(old);
            }
        }
        match crate::brain::spawn(key) {
            // put_warm re-checks the key: a turn or a new conversation may have started meanwhile
            Ok(w) => self.put_warm(w),
            Err(e) => eprintln!("could not start the warm session: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn h1_a_one_shot_turn_or_a_new_conversation_makes_every_older_warm_process_unusable() {
        let s = Shared::load(std::env::temp_dir().join("one-desktop-test-warm-gen"));
        let k1 = s.warm_key();
        assert_eq!(s.warm_key(), k1, "stable while nothing happens");
        s.drop_warm(); // a one-shot (approval or focus) turn starts
        let k2 = s.warm_key();
        assert_ne!(k2, k1, "a process spawned before the one-shot turn can never be used or put back");
        let conv = s.conv_gen();
        s.clear_session();
        assert_ne!(s.warm_key(), k2, "a new conversation retires the warm process too");
        assert_ne!(s.conv_gen(), conv);
        // a turn that began before "new conversation" does not bring the old session back
        s.set_session_for(conv, "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa");
        assert_eq!(s.session_id(), None);
        s.set_session_for(s.conv_gen(), "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa");
        assert!(s.session_id().is_some());
        s.clear_session();
    }

    #[test]
    fn h1_no_warm_process_is_started_while_a_turn_runs() {
        let s = Shared::load(std::env::temp_dir().join("one-desktop-test-warm-busy"));
        let _t = s.begin_turn();
        s.prewarm(); // e.g. the user unmutes during an approval turn
        assert!(!s.has_warm(), "prewarm must wait for the turn to end");
    }

    #[test]
    fn sec_l2_only_real_voice_ids_can_be_saved() {
        let s = Shared::load(std::env::temp_dir().join("one-desktop-test-voiceid"));
        for bad in ["a/b", "x\"y", "../v", &"a".repeat(65), "v v"] {
            assert!(s.save_prefs(UiPrefs { voice_id: bad.to_string(), voice_id_ar: String::new() }).is_err(), "{bad}");
            assert!(s.save_prefs(UiPrefs { voice_id: String::new(), voice_id_ar: bad.to_string() }).is_err(), "{bad}");
        }
        assert!(valid_voice_id("") && valid_voice_id("cjVigY5qzO86Huf0OWal"));
    }

    #[test]
    fn approvals_are_single_use() {
        let s = Shared::load(std::env::temp_dir().join("one-desktop-test-state"));
        s.set_pending(vec![Denial { tool_name: "Bash".into(), tool_use_id: "t1".into(), tool_input: json!({"command":"echo hello"}) }]);
        assert!(s.take_pending("t1").is_some());
        assert!(s.take_pending("t1").is_none());
        assert!(s.take_pending("forged").is_none());
    }

    #[test]
    fn save_settings_keeps_hand_edited_team_keys() {
        let dir = std::env::temp_dir().join(format!("one-desktop-test-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("settings.json"),
            r#"{"cwd":"C:/x","voice_id":"v","me":"Riley","remind_python":"py","remind_script":"S.py"}"#,
        )
        .unwrap();
        let s = Shared::load(dir.clone());
        assert_eq!(s.team_config().me, "Riley");
        s.save_prefs(UiPrefs { voice_id: "v2".into(), voice_id_ar: String::new() }).unwrap();
        let again = Shared::load(dir);
        assert_eq!(again.team_config(), s.team_config());
        assert_eq!(again.team_config().remind_script, "S.py");
        assert_eq!(again.settings().voice_id, "v2");
    }

    #[test]
    fn m1_webview_payload_cannot_change_cwd_or_paths() {
        // a forged payload is rejected at the IPC boundary
        for forged in [
            r#"{"voice_id":"v","cwd":"C:/evil"}"#,
            r#"{"voice_id":"v","remind_script":"C:/evil.py"}"#,
            r#"{"voice_id":"v","remind_python":"evil.exe"}"#,
        ] {
            assert!(serde_json::from_str::<UiPrefs>(forged).is_err(), "{forged}");
        }
        // and saving prefs never touches the file-only values
        let dir = std::env::temp_dir().join(format!("one-desktop-test-m1-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("settings.json"),
            r#"{"cwd":"C:/real","voice_id":"v","me":"Riley","remind_python":"py","remind_script":"S.py"}"#,
        )
        .unwrap();
        let s = Shared::load(dir.clone());
        s.save_prefs(serde_json::from_str(r#"{"voice_id":"v9"}"#).unwrap()).unwrap();
        let again = Shared::load(dir);
        assert_eq!(again.settings().cwd, "C:/real");
        assert_eq!(again.settings().voice_id, "v9");
        assert_eq!(again.team_config().remind_script, "S.py");
        assert_eq!(again.team_config().remind_python, "py");
    }

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("one-desktop-test-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn cr_m3_partial_settings_keep_the_users_cwd_and_survive_a_save() {
        let dir = temp("m3");
        std::fs::write(dir.join("settings.json"), r#"{"cwd":"D:/proj","me":"Riley","auto_remind":true}"#).unwrap();
        let s = Shared::load(dir.clone());
        assert_eq!(s.settings().cwd, "D:/proj", "no voice_id must not reset cwd");
        assert_eq!(s.settings().voice_id, "");
        assert!(s.team_config().auto_remind);
        // boot's voice pick must not overwrite the file-only keys
        s.save_prefs(UiPrefs { voice_id: "v1".into(), voice_id_ar: String::new() }).unwrap();
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join("settings.json")).unwrap()).unwrap();
        assert_eq!((v["cwd"].as_str(), v["me"].as_str(), v["auto_remind"].as_bool(), v["voice_id"].as_str()), (Some("D:/proj"), Some("Riley"), Some(true), Some("v1")));
    }

    #[test]
    fn r2_team_prefs_round_trip_and_keep_other_keys() {
        let dir = temp("r2");
        std::fs::write(dir.join("settings.json"), r#"{"cwd":"D:/proj","voice_id":"v","me":"Riley"}"#).unwrap();
        let s = Shared::load(dir.clone());
        assert_eq!(s.team_prefs(), TeamPrefs::default());
        let mut p = TeamPrefs { lang: Lang::Ar, theme: Theme::Light, scope: Scope::Mine, view: View::Graph, helpers: true, ..Default::default() };
        p.open.insert("Riley".into(), true);
        s.save_team_prefs(p.clone()).unwrap();
        let again = Shared::load(dir.clone());
        assert_eq!(again.team_prefs(), p);
        assert_eq!((again.settings().cwd.as_str(), again.team_config().me.as_str()), ("D:/proj", "Riley"));
    }

    #[test]
    fn r2_team_prefs_reject_bad_input() {
        for bad in [r#"{"lang":"fr"}"#, r#"{"theme":"neon"}"#, r#"{"cwd":"C:/evil"}"#, r#"{"helpers":"yes"}"#] {
            assert!(serde_json::from_str::<TeamPrefs>(bad).is_err(), "{bad}");
        }
        let mut p = TeamPrefs::default();
        for i in 0..101 {
            p.open.insert(format!("n{i}"), true);
        }
        assert!(p.validate().is_err());
        let dir = temp("r2-bad");
        let s = Shared::load(dir);
        assert!(s.save_team_prefs(p).is_err());
        // a partial object keeps defaults for the rest
        let q: TeamPrefs = serde_json::from_str(r#"{"lang":"ar"}"#).unwrap();
        assert_eq!((q.lang, q.theme, q.scope), (Lang::Ar, Theme::Dark, Scope::Team));
    }

    #[test]
    fn cr_m3_unparsable_settings_are_never_rewritten() {
        let dir = temp("m3-broken");
        std::fs::write(dir.join("settings.json"), "{ this is not json").unwrap();
        let s = Shared::load(dir.clone());
        assert!(s.save_prefs(UiPrefs { voice_id: "v".into(), voice_id_ar: String::new() }).is_err());
        assert_eq!(std::fs::read_to_string(dir.join("settings.json")).unwrap(), "{ this is not json");
    }

    const UID: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

    #[test]
    fn the_window_is_1200x800_on_first_launch_clamped_to_the_screen_then_remembered() {
        // first launch on a big screen: 1200x800
        assert_eq!(clamp_window(FIRST_LAUNCH_SIZE, Some([2560.0, 1440.0])), [1200.0, 800.0]);
        // a 1366x768 laptop at 125% scaling is about 1093x614 logical: at most 90% of it
        assert_eq!(clamp_window(FIRST_LAUNCH_SIZE, Some([1093.0, 614.0])), [984.0, 553.0]);
        // a tiny screen is never given a window larger than the screen allows
        let [w, h] = clamp_window(FIRST_LAUNCH_SIZE, Some([800.0, 600.0]));
        assert!(w <= 720.0 && h <= 540.0);
        // unknown monitor: unchanged
        assert_eq!(clamp_window([900.0, 700.0], None), [900.0, 700.0]);
        // a saved size is restored (and still clamped)
        let dir = temp("size");
        let s = Shared::load(dir.clone());
        assert_eq!(s.settings().main_size, None);
        s.save_main_size([1000.4, 700.0]).unwrap();
        assert_eq!(Shared::load(dir).settings().main_size, Some([1000.0, 700.0]));
        assert!(s.save_main_size([f64::NAN, 700.0]).is_err());
        assert!(s.save_main_size([10.0, 700.0]).is_err());
    }

    #[test]
    fn one_never_resumes_a_foreign_or_forked_session_from_an_old_session_file() {
        let dir = temp("own-only");
        std::fs::write(dir.join("settings.json"), r#"{"cwd":"C:/a","voice_id":"v"}"#).unwrap();
        let write = |v: serde_json::Value| std::fs::write(dir.join("session.json"), v.to_string()).unwrap();
        // old PR #4 shapes: a picked (forked) session, and a bare id with no marker
        write(json!({"session_id": UID, "cwd": "C:/a", "fork": true}));
        assert_eq!(Shared::load(dir.clone()).session_id(), None, "fork:true is ignored");
        write(json!({"session_id": UID, "cwd": "C:/a"}));
        assert_eq!(Shared::load(dir.clone()).session_id(), None, "a foreign id is ignored");
        write(json!({"session_id": UID, "cwd": "C:/a", "own": true, "fork": true}));
        assert_eq!(Shared::load(dir.clone()).session_id(), None);
        // One's own id survives a restart
        let s = Shared::load(dir.clone());
        s.set_session(UID);
        assert_eq!(Shared::load(dir).session_id().as_deref(), Some(UID));
    }

    #[test]
    fn the_focus_is_validated_in_memory_only_and_cleared_by_none_or_a_new_conversation() {
        let s = Shared::load(temp("focus"));
        assert_eq!(s.focus(), None);
        assert!(s.set_focus(Some("--dangerously-skip-permissions")).is_err());
        s.set_focus(Some(UID)).unwrap();
        assert_eq!(s.focus().as_deref(), Some(UID));
        assert_eq!(s.session_id(), None, "asking about a session never becomes One's session");
        s.set_focus(None).unwrap();
        assert_eq!(s.focus(), None);
        s.set_focus(Some(UID)).unwrap();
        s.clear_session();
        assert_eq!(s.focus(), None);
    }

    #[test]
    fn a_tampered_session_file_cannot_smuggle_a_flag() {
        let dir = temp("tamper");
        std::fs::write(dir.join("settings.json"), r#"{"cwd":"C:/a","voice_id":"v"}"#).unwrap();
        for bad in ["--dangerously-skip-permissions", "-p", "abc-123", "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa --x"] {
            std::fs::write(dir.join("session.json"), serde_json::json!({"session_id": bad, "cwd": "C:/a"}).to_string()).unwrap();
            assert_eq!(Shared::load(dir.clone()).session_id(), None, "{bad}");
        }
        let s = Shared::load(dir);
        s.set_session("not-a-uuid"); // ignored, never stored
        assert_eq!(s.session_id(), None);
    }

    #[test]
    fn cr_h1_session_is_tied_to_its_cwd_and_can_be_cleared() {
        let dir = temp("h1-session");
        std::fs::write(dir.join("settings.json"), r#"{"cwd":"C:/a","voice_id":"v"}"#).unwrap();
        let s = Shared::load(dir.clone());
        s.set_session(UID);
        assert_eq!(Shared::load(dir.clone()).session_id().as_deref(), Some(UID));
        // the user edits cwd by hand: the old session no longer applies
        std::fs::write(dir.join("settings.json"), r#"{"cwd":"C:/b","voice_id":"v"}"#).unwrap();
        assert_eq!(Shared::load(dir.clone()).session_id(), None);
        // an old-format session file (no cwd) is dropped too
        std::fs::write(dir.join("session.json"), r#"{"session_id":"old"}"#).unwrap();
        assert_eq!(Shared::load(dir.clone()).session_id(), None);
        s.clear_session();
        assert!(!dir.join("session.json").exists() && s.session_id().is_none());
    }

    #[test]
    fn begin_turn_cancels_previous() {
        let s = Shared::load(std::env::temp_dir().join("one-desktop-test-state"));
        let a = s.begin_turn();
        let _b = s.begin_turn();
        assert!(a.is_cancelled());
    }

    #[test]
    fn turn_running_follows_begin_and_end_and_ignores_a_stale_end() {
        let s = Shared::load(temp("turn-running"));
        assert!(!s.turn_running());
        let (a, _) = s.begin_turn_id();
        assert!(s.turn_running());
        let (b, _) = s.begin_turn_id(); // replaces a
        s.end_turn(a); // the old turn finishing must not clear the new one
        assert!(s.turn_running());
        s.end_turn(b);
        assert!(!s.turn_running());
        let _ = s.begin_turn_id();
        s.cancel_turn();
        assert!(!s.turn_running());
    }

    #[test]
    fn l3_a_settings_file_broken_while_running_is_refused_not_overwritten() {
        let dir = temp("l3");
        std::fs::write(dir.join("settings.json"), r#"{"cwd":"D:/p","voice_id":"v","me":"Riley"}"#).unwrap();
        let s = Shared::load(dir.clone());
        // the user saves a syntax error mid-edit
        std::fs::write(dir.join("settings.json"), r#"{"cwd":"D:/p","me":"Riley""#).unwrap();
        assert!(s.save_prefs(UiPrefs { voice_id: "v2".into(), voice_id_ar: String::new() }).is_err());
        assert!(s.save_team_prefs(TeamPrefs::default()).is_err());
        assert_eq!(std::fs::read_to_string(dir.join("settings.json")).unwrap(), r#"{"cwd":"D:/p","me":"Riley""#);
        // a non-object document is refused too
        std::fs::write(dir.join("settings.json"), "[1,2]").unwrap();
        assert!(s.save_prefs(UiPrefs { voice_id: "v3".into(), voice_id_ar: String::new() }).is_err());
    }

    #[test]
    fn l3_settings_write_is_atomic_and_leaves_no_temp_file() {
        let dir = temp("l3-atomic");
        std::fs::write(dir.join("settings.json"), r#"{"cwd":"D:/p","voice_id":"v","me":"Riley"}"#).unwrap();
        let s = Shared::load(dir.clone());
        s.save_prefs(UiPrefs { voice_id: "v2".into(), voice_id_ar: String::new() }).unwrap();
        assert!(!dir.join("settings.json.tmp").exists());
        let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join("settings.json")).unwrap()).unwrap();
        assert_eq!((v["voice_id"].as_str(), v["me"].as_str()), (Some("v2"), Some("Riley")));
        // a missing file is created (first run)
        let fresh = temp("l3-fresh");
        Shared::load(fresh.clone()).save_prefs(UiPrefs { voice_id: "x".into(), voice_id_ar: String::new() }).unwrap();
        assert!(fresh.join("settings.json").exists());
    }

    #[test]
    fn window_mode_defaults_to_main_is_remembered_and_validated() {
        let dir = temp("mode");
        std::fs::write(dir.join("settings.json"), r#"{"cwd":"D:/p","voice_id":"v","me":"Riley"}"#).unwrap();
        let s = Shared::load(dir.clone());
        let d = s.settings();
        assert_eq!((d.window_mode.as_str(), d.push_to_talk, d.wake_word, d.auto_pause), ("main", false, false, true));
        s.save_window_mode("mini").unwrap();
        assert_eq!(Shared::load(dir.clone()).settings().window_mode, "mini");
        assert!(s.save_window_mode("fullscreen").is_err());
        // a voice save keeps the mode and the hand-edited keys
        s.save_prefs(UiPrefs { voice_id: "v2".into(), voice_id_ar: String::new() }).unwrap();
        let again = Shared::load(dir);
        assert_eq!((again.settings().window_mode.as_str(), again.team_config().me.as_str()), ("mini", "Riley"));
    }

    #[test]
    fn l4_prefs_are_capped_at_100_keys_exactly() {
        let mut p = TeamPrefs::default();
        for i in 0..100 {
            p.open.insert(format!("n{i}"), true);
        }
        assert!(p.validate().is_ok());
        p.open.insert("one-too-many".into(), true);
        assert!(p.validate().is_err());
    }

    #[test]
    fn the_voice_test_runs_one_at_a_time_with_a_cooldown() {
        use std::time::{Duration, Instant};
        let mut g = VoiceGate::new();
        let t0 = Instant::now();
        let cd = Duration::from_secs(5);
        assert!(g.try_start(t0, cd).is_ok());
        assert!(g.try_start(t0, cd).unwrap_err().contains("already"), "not while one is playing");
        g.finish(t0 + Duration::from_secs(2));
        assert!(g.try_start(t0 + Duration::from_secs(3), cd).unwrap_err().contains("wait"), "cooldown after the end");
        assert!(g.try_start(t0 + Duration::from_secs(8), cd).is_ok());
    }

    #[test]
    fn leftover_focus_prompt_files_are_deleted_at_start_up() {
        let dir = temp("leftover");
        std::fs::write(dir.join("focus-00ff00ff00ff00ff.txt"), "another session's text").unwrap();
        std::fs::write(dir.join("keep.txt"), "x").unwrap();
        let _ = Shared::load(dir.clone());
        assert!(!dir.join("focus-00ff00ff00ff00ff.txt").exists());
        assert!(dir.join("keep.txt").exists());
    }

    #[test]
    fn start_up_focuses_nothing_even_when_a_default_session_is_live() {
        let dir = temp("nofocus");
        std::fs::write(dir.join("settings.json"), r#"{"cwd":"C:/a","voice_id":"v"}"#).unwrap();
        let s = Shared::load(dir);
        // the default route is only a suggestion: resolving it touches no state
        let live = crate::tabs::SessionTab { tab_title: "all-for-one-6".into(), session_id: UID.into(), project: "p".into(), modified: 0, name: None, live: true, status: String::new() };
        let r = crate::route::resolve(&[live], crate::route::DEFAULT_COORDINATOR, None);
        assert_eq!(r.session_id.as_deref(), Some(UID));
        assert_eq!(s.focus(), None, "no session is focused at start");
        assert!(!s.tainted() && !s.locked(), "a plain chat stays One's own conversation, with its tools");
    }

    #[test]
    fn a_session_that_carried_another_sessions_extract_stays_tainted_until_a_new_conversation() {
        let dir = temp("taint");
        std::fs::write(dir.join("settings.json"), r#"{"cwd":"C:/a","voice_id":"v"}"#).unwrap();
        let s = Shared::load(dir.clone());
        s.set_session(UID);
        assert!(!s.tainted());
        s.set_tainted();
        assert!(s.tainted());
        s.set_session(UID); // the next turn re-saves the id: the mark stays
        assert!(Shared::load(dir.clone()).tainted(), "survives a restart");
        s.clear_session();
        assert!(!s.tainted() && !Shared::load(dir.clone()).tainted());
        // a session.json written before the field existed loads as tainted
        std::fs::write(dir.join("session.json"), json!({"session_id": UID, "cwd": "C:/a", "own": true}).to_string()).unwrap();
        assert!(Shared::load(dir).tainted());
    }

    #[test]
    fn the_chat_thread_is_kept_in_rust_survives_a_restart_and_a_new_conversation_clears_it() {
        let dir = temp("chat");
        let s = Shared::load(dir.clone());
        s.chat_push("user", "hello");
        s.chat_push("assistant", "hi, how can I help?");
        assert_eq!(Shared::load(dir.clone()).chat_history().len(), 2);
        s.clear_session();
        assert!(s.chat_history().is_empty() && Shared::load(dir).chat_history().is_empty());
    }

    #[test]
    fn a_focus_reply_that_lands_after_the_focus_cleared_is_not_kept_nor_after_a_restart() {
        let dir = temp("chat-focus");
        let s = Shared::load(dir.clone());
        s.chat_push("user", "plain");
        // the reply of a focus turn arrives after the user switched back to One
        s.chat_push_marked("assistant", "text from the other session", true);
        assert_eq!(s.chat_history().iter().map(|m| m.text.as_str()).collect::<Vec<_>>(), ["plain"]);
        // a focus left open at exit: its marked text is gone at the next start
        let mut h = crate::history::History::load(Some(dir.clone()));
        h.push_with("assistant", "left open", true);
        assert_eq!(Shared::load(dir).chat_history().len(), 1);
    }
}
