//! Done / Remind for the Team board.
//!
//! - Done is local only (persisted in `team-state.json`); nothing goes to the hub.
//! - Remind posts to the team chat through a user-supplied script (settings.json `remind_script`),
//!   spawned directly (never a shell). With no script configured, Remind says so.
//!   The text is built here from the stored item; the webview only sends an item id.
//! - Manual reminds: 1 per item per hour. Automatic (OFF unless settings.json has
//!   `"auto_remind": true`): ONE per item, at 3 days, recorded before it is sent so a restart
//!   can never repeat it; only for asks created after auto-remind was first enabled; at most
//!   3 per hourly run. An automatic post is NEVER retried.
//! The poster is a trait so tests never run a script.

use crate::team::{self, Ask, Loaded, Person, Status, TeamSource};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const MSG_CAP: usize = 300;
/// Most automatic reminders sent in one hourly run (anti-flood).
pub const AUTO_MAX_PER_RUN: usize = 3;
/// Most automatic reminders per calendar day, whatever the hourly runs do.
pub const AUTO_MAX_PER_DAY: usize = 10;
pub const MANUAL_COOLDOWN_SECS: i64 = 3600;
const POST_TIMEOUT: Duration = Duration::from_secs(150);

pub const DEFAULT_PYTHON: &str = "python";
pub const DEFAULT_SCRIPT: &str = "";

// ---- message ------------------------------------------------------------------

/// "@<Name> reminder: <who> needs <whom> for <what> (open <N> days)."
/// Control chars and newlines are stripped and the length is capped at 300.
pub fn build_message(ask: &Ask, now: i64) -> String {
    let days = team::age_secs(ask.created_ts, now) / 86_400;
    let raw = format!("@{} reminder: {} needs {} for {} (open {} days).", ask.whom, ask.who, ask.whom, ask.what, days);
    team::clean(&raw, MSG_CAP)
}

// ---- poster -------------------------------------------------------------------

#[derive(Debug, PartialEq)]
pub enum PostError {
    /// Definitely NOT sent: the remind script printed `REFUSED:` before it queued anything
    /// (or the script could not even start). The user may click Remind again.
    Failed(String),
    /// Maybe sent. The script queues the message BEFORE it triggers the workflow, so any
    /// other failure (non-zero exit, timeout, lost process) may already have queued it.
    /// Never retried automatically.
    Unknown(String),
}

/// Decide the outcome of a finished script run from its exit status and stderr.
pub fn classify_exit(success: bool, stderr: &str) -> Result<(), PostError> {
    if success {
        return Ok(());
    }
    let note = team::clean(stderr, 160);
    if note.starts_with("REFUSED:") {
        Err(PostError::Failed(note))
    } else {
        Err(PostError::Unknown("The remind script did not confirm; the reminder may have been sent".into()))
    }
}

pub trait Poster: Send + Sync {
    fn post(&self, msg: &str) -> Result<(), PostError>;
}

/// Keep an `@mention` only when it names a known team member (SEC L3); drop the `@` of every
/// other one (`@everyone`, `@here`, a name the file invented), so nothing pings outsiders.
pub fn strip_unknown_mentions(text: &str, known: &[String]) -> String {
    let mut out = String::with_capacity(text.len());
    for (i, c) in text.char_indices() {
        if c == '@' && !names_a_member(&text[i + 1..], known) {
            continue;
        }
        out.push(c);
    }
    out
}

/// Whether `rest` starts with a known name that ends at a word boundary.
fn names_a_member(rest: &str, known: &[String]) -> bool {
    known.iter().filter(|n| !n.trim().is_empty()).any(|n| {
        let Some(head) = rest.get(..n.len()) else { return false };
        head.eq_ignore_ascii_case(n) && !rest[n.len()..].chars().next().is_some_and(|c| c.is_alphanumeric() || c == '_')
    })
}

/// Wraps a poster so every message passes the mention allowlist first.
struct MentionFilter<'a> {
    inner: &'a dyn Poster,
    known: &'a [String],
}

impl Poster for MentionFilter<'_> {
    fn post(&self, msg: &str) -> Result<(), PostError> {
        self.inner.post(&strip_unknown_mentions(msg, self.known))
    }
}

/// Handles that notify a whole room. Never mentionable, whatever the people list says: the
/// list lives in a file that is not trusted.
const BROADCAST: [&str; 9] = ["everyone", "everybody", "here", "channel", "all", "team", "anyone", "room", "group"];

/// A normal person name: 1 to 40 characters, letters, digits, spaces, dots, dashes and
/// underscores only, with at least one letter, and not a broadcast handle.
pub fn is_person_name(n: &str) -> bool {
    let n = n.trim();
    n.chars().count() <= 40
        && n.chars().any(char::is_alphabetic)
        && n.chars().all(|c| c.is_alphanumeric() || matches!(c, ' ' | '.' | '-' | '_'))
        && !n.split_whitespace().any(|w| BROADCAST.contains(&w.to_lowercase().as_str()))
}

fn known_names(people: &[Person]) -> Vec<String> {
    people
        .iter()
        .flat_map(|p| std::iter::once(p.name.clone()).chain(p.name_ar.clone()))
        .filter(|n| is_person_name(n))
        .collect()
}

pub struct ScriptPoster {
    pub python: String,
    pub script: String,
}

impl Poster for ScriptPoster {
    fn post(&self, msg: &str) -> Result<(), PostError> {
        if self.python.trim().is_empty() || self.script.trim().is_empty() {
            return Err(PostError::Failed("No remind script is configured (settings.json remind_script)".into()));
        }
        let mut cmd = Command::new(&self.python);
        // Fixed argv, no shell: the message is a single argument and never parsed.
        cmd.args(["-I", "-X", "utf8", &self.script, "one-desktop", msg])
            .stdin(Stdio::null())
            .stdout(Stdio::null()) // never read, so it must not be a pipe that can fill up
            .stderr(Stdio::piped());
        #[cfg(windows)]
        std::os::windows::process::CommandExt::creation_flags(&mut cmd, 0x0800_0000); // CREATE_NO_WINDOW
        let mut child = cmd.spawn().map_err(|_| PostError::Failed("could not start the remind script".into()))?;
        // Drain stderr on its own thread (first 2 KB kept) so the script can never block on it.
        let reader = child.stderr.take().map(|mut s| {
            std::thread::spawn(move || {
                let mut kept = String::new();
                let _ = s.by_ref().take(2048).read_to_string(&mut kept);
                let _ = std::io::copy(&mut s, &mut std::io::sink());
                kept
            })
        });
        let start = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if start.elapsed() > POST_TIMEOUT => {
                    let _ = child.kill();
                    let _ = child.wait(); // reap it
                    return Err(PostError::Unknown("The remind script did not answer in time".into()));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(100)),
                Err(_) => return Err(PostError::Unknown("lost track of the remind script".into())),
            }
        };
        let stderr = reader.and_then(|r| r.join().ok()).unwrap_or_default();
        classify_exit(status.success(), &stderr)
    }
}

// ---- persistent state -----------------------------------------------------------

#[derive(Serialize, Deserialize, Default, Clone)]
struct Persisted {
    #[serde(default)]
    done: HashMap<String, i64>,
    #[serde(default)]
    manual: HashMap<String, i64>,
    #[serde(default)]
    auto: HashMap<String, i64>,
    /// When automatic reminders were first switched on (CR-M4): older asks are never eligible.
    #[serde(default)]
    auto_enabled_at: Option<i64>,
    /// Daily cap on automatic posts (SEC L3): the day (days since the epoch) and how many went out.
    #[serde(default)]
    auto_day: i64,
    #[serde(default)]
    auto_today: usize,
}

pub struct Store {
    path: PathBuf,
    inner: Mutex<Persisted>,
}

impl Store {
    pub fn load(path: PathBuf) -> Self {
        let p = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        Store { path, inner: Mutex::new(p) }
    }

    fn save(&self, p: &Persisted) -> Result<(), String> {
        let tmp = self.path.with_extension("json.tmp");
        let json = serde_json::to_string_pretty(p).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, json).and_then(|_| std::fs::rename(&tmp, &self.path)).map_err(|_| "could not save the board state".to_string())
    }

    /// Mutate, persist, and roll the change back if it could not be saved (fail closed).
    fn change<T>(&self, f: impl FnOnce(&mut Persisted) -> Result<T, String>) -> Result<T, String> {
        let mut g = self.inner.lock().map_err(|_| "state is locked")?;
        let before = g.clone();
        let out = f(&mut g)?;
        if let Err(e) = self.save(&g) {
            *g = before;
            return Err(e);
        }
        Ok(out)
    }

    fn read<T>(&self, f: impl FnOnce(&Persisted) -> T) -> T {
        f(&self.inner.lock().unwrap_or_else(|e| e.into_inner()))
    }

    pub fn is_done(&self, id: &str) -> bool {
        self.read(|p| p.done.contains_key(id))
    }

    pub fn set_done(&self, id: &str, done: bool, now: i64) -> Result<(), String> {
        self.change(|p| {
            if done {
                p.done.insert(id.to_string(), now);
            } else {
                p.done.remove(id);
            }
            Ok(())
        })
    }

    /// Claim the hourly manual slot. Returns the previous timestamp (to roll back).
    pub fn claim_manual(&self, id: &str, now: i64) -> Result<Option<i64>, String> {
        self.change(|p| {
            let prev = p.manual.get(id).copied();
            if prev.map(|t| now - t < MANUAL_COOLDOWN_SECS).unwrap_or(false) {
                return Err("Already reminded in the last hour".into());
            }
            p.manual.insert(id.to_string(), now);
            Ok(prev)
        })
    }

    fn release_manual(&self, id: &str, prev: Option<i64>) {
        let _ = self.change(|p| {
            match prev {
                Some(t) => p.manual.insert(id.to_string(), t),
                None => p.manual.remove(id),
            };
            Ok(())
        });
    }

    /// Record the one automatic reminder. False when it was already sent, when today's cap
    /// (`AUTO_MAX_PER_DAY`, counted per calendar day and persisted) is used up, or when the
    /// record could not be saved: then nothing is sent and the id stays unclaimed.
    fn claim_auto(&self, id: &str, now: i64) -> bool {
        self.change(|p| {
            if p.auto.contains_key(id) {
                return Err(String::new());
            }
            let day = now.div_euclid(86_400);
            if p.auto_day != day {
                p.auto_day = day;
                p.auto_today = 0;
            }
            if p.auto_today >= AUTO_MAX_PER_DAY {
                return Err(String::new());
            }
            p.auto_today += 1;
            p.auto.insert(id.to_string(), now);
            Ok(())
        })
        .is_ok()
    }

    /// Automatic reminders are off: forget when they were first enabled, so switching them on
    /// later never makes the whole gap eligible.
    pub fn clear_auto_enabled(&self) -> Result<(), String> {
        self.change(|p| {
            p.auto_enabled_at = None;
            Ok(())
        })
    }

    /// The first time automatic reminders were enabled (stored on first call).
    fn auto_enabled_since(&self, now: i64) -> Result<i64, String> {
        self.change(|p| Ok(*p.auto_enabled_at.get_or_insert(now)))
    }

    fn flags(&self, id: &str, now: i64) -> (bool, bool) {
        self.read(|p| (p.manual.get(id).map(|t| now - t < MANUAL_COOLDOWN_SECS).unwrap_or(false), p.auto.contains_key(id)))
    }
}

pub fn manual_remind(store: &Store, poster: &dyn Poster, ask: &Ask, now: i64) -> Result<(), String> {
    if store.is_done(&ask.id) {
        return Err("This ask is already done".into());
    }
    let msg = build_message(ask, now);
    let prev = store.claim_manual(&ask.id, now)?;
    match poster.post(&msg) {
        Ok(()) => Ok(()),
        Err(PostError::Failed(e)) => {
            store.release_manual(&ask.id, prev);
            Err(e)
        }
        Err(PostError::Unknown(e)) => Err(e),
    }
}

/// One automatic reminder per item, at 3 days. Only asks created after `since` (when auto
/// mode was first enabled) are eligible, and at most `max` are posted per call. The claim is
/// recorded before the post and kept whatever happens: an automatic post is never retried.
/// Returns the ids that were sent.
pub fn auto_remind(store: &Store, poster: &dyn Poster, asks: &[Ask], now: i64, since: i64, max: usize) -> Vec<String> {
    let mut sent = Vec::new();
    let mut tried = 0;
    for a in asks {
        if tried >= max {
            break;
        }
        let eligible = a.created_ts > since && !store.is_done(&a.id) && team::is_long(team::age_secs(a.created_ts, now));
        if !eligible || !store.claim_auto(&a.id, now) {
            continue;
        }
        tried += 1;
        if poster.post(&build_message(a, now)).is_ok() {
            sent.push(a.id.clone());
        }
    }
    sent
}

// ---- the board, as the webview sees it ----------------------------------------------

#[derive(Serialize)]
pub struct AskView {
    #[serde(flatten)]
    pub ask: Ask,
    pub age_hours: i64,
    pub amber: bool,
    pub long: bool,
    pub reminded_recently: bool,
    pub auto_reminded: bool,
}

#[derive(Serialize)]
pub struct TeamView {
    pub me: String,
    pub status: &'static str, // ok | missing | error
    pub path: String,
    pub message: String,
    pub people: Vec<Person>,
    pub asks: Vec<AskView>,
    pub dropped: usize,
    /// The hub's side of "who needs me" and whether it could be reached (filled by `team_load`).
    pub hub: crate::hub_lifecycle::HubBrief,
    /// Nothing on you from the team AND from the hub, and the hub answered (filled by `team_load`).
    pub caught_up: bool,
}

pub struct Team {
    source: Box<dyn TeamSource>,
    store: Store,
    poster: Box<dyn Poster>,
    me: String,
    auto_remind: bool,
}

impl Team {
    pub fn new(dir: PathBuf, me: String, python: String, script: String, auto_remind: bool, paths: crate::autoboard::BoardPaths) -> Self {
        let store = Store::load(dir.join("team-state.json"));
        if !auto_remind {
            if let Err(e) = store.clear_auto_enabled() {
                eprintln!("could not reset the auto-remind start: {e}");
            }
        }
        Team {
            source: Box::new(crate::autoboard::AutoSource::new(dir.join("team-asks.json"), paths)),
            store,
            poster: Box::new(ScriptPoster { python, script }),
            me,
            auto_remind,
        }
    }

    #[cfg(test)]
    pub fn with_parts(source: Box<dyn TeamSource>, store: Store, poster: Box<dyn Poster>, me: String, auto_remind: bool) -> Self {
        Team { source, store, poster, me, auto_remind }
    }

    pub fn view(&self, now: i64) -> TeamView {
        let Loaded { status, data } = self.source.load();
        let (status, message) = match status {
            Status::Ok => ("ok", String::new()),
            Status::Missing => ("missing", String::new()),
            Status::Invalid(m) => ("error", m),
        };
        let asks = data
            .asks
            .into_iter()
            .filter(|a| !self.store.is_done(&a.id))
            .map(|ask| {
                let age = team::age_secs(ask.created_ts, now);
                let (reminded_recently, auto_reminded) = self.store.flags(&ask.id, now);
                AskView { age_hours: age / 3600, amber: team::is_amber(age), long: team::is_long(age), reminded_recently, auto_reminded, ask }
            })
            .collect();
        TeamView { me: self.me.clone(), status, path: self.source.describe(), message, people: data.people, asks, dropped: data.dropped, hub: Default::default(), caught_up: false }
    }

    pub fn set_done(&self, id: &str, done: bool, now: i64) -> Result<(), String> {
        self.store.set_done(id, done, now)
    }

    pub fn remind(&self, id: &str, now: i64) -> Result<(), String> {
        let data = self.source.load_trusted().data;
        let known = known_names(&data.people);
        let ask = data.asks.into_iter().find(|a| a.id == id).ok_or("That ask is no longer on the board")?;
        manual_remind(&self.store, &MentionFilter { inner: self.poster.as_ref(), known: &known }, &ask, now)
    }

    pub fn auto_tick(&self, now: i64) -> Vec<String> {
        if !self.auto_remind {
            return Vec::new(); // off by default: needs "auto_remind": true in settings.json
        }
        let Ok(since) = self.store.auto_enabled_since(now) else { return Vec::new() };
        let data = self.source.load_trusted().data;
        let known = known_names(&data.people);
        auto_remind(&self.store, &MentionFilter { inner: self.poster.as_ref(), known: &known }, &data.asks, now, since, AUTO_MAX_PER_RUN)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    const DAY: i64 = 86_400;
    const T0: i64 = 1_794_571_200;

    #[derive(Clone, Default)]
    struct Fake {
        sent: Arc<Mutex<Vec<String>>>,
        fail: Arc<Mutex<Option<PostError>>>,
    }
    impl Poster for Fake {
        fn post(&self, msg: &str) -> Result<(), PostError> {
            if let Some(e) = self.fail.lock().unwrap().take() {
                return Err(e);
            }
            self.sent.lock().unwrap().push(msg.to_string());
            Ok(())
        }
    }

    fn ask(id: &str, age_days: i64) -> Ask {
        Ask {
            id: id.into(),
            who: "Hub".into(),
            whom: "Alex".into(),
            what: "an OK on T-2041".into(),
            what_ar: None,
            kind: "approval".into(),
            ticket_id: None,
            created_at: String::new(),
            created_ts: T0 - age_days * DAY,
        }
    }

    fn store(name: &str) -> (Store, PathBuf) {
        let p = std::env::temp_dir().join(format!("one-desktop-remind-{name}-{}.json", std::process::id()));
        let _ = std::fs::remove_file(&p);
        (Store::load(p.clone()), p)
    }

    #[test]
    fn sec_l3_automatic_posts_have_a_persisted_daily_cap() {
        let (s, p) = store("daycap");
        let f = Fake::default();
        let asks: Vec<Ask> = (0..15).map(|i| ask(&format!("a{i}"), 5)).collect();
        let day1: usize = (0..4).map(|h| auto_remind(&s, &f, &asks, T0 + h * 3600, 0, 3).len()).sum();
        assert_eq!(day1, AUTO_MAX_PER_DAY, "3+3+3+1, then the cap");
        assert!(auto_remind(&s, &f, &asks, T0 + 5 * 3600, 0, 3).is_empty());
        // the cap survives a restart on the same day
        let restarted = Store::load(p);
        assert!(auto_remind(&restarted, &f, &asks, T0 + 6 * 3600, 0, 3).is_empty());
        // next day it resets; ids skipped by the cap were never claimed, so they go out now
        assert_eq!(auto_remind(&restarted, &f, &asks, T0 + DAY, 0, 3).len(), 3);
        assert_eq!(f.sent.lock().unwrap().len(), AUTO_MAX_PER_DAY + 3);
    }

    #[test]
    fn sec_l3_only_known_team_members_can_be_mentioned() {
        let known = vec!["Alex".to_string(), "Maya".to_string()];
        let s = |t: &str| strip_unknown_mentions(t, &known);
        assert_eq!(s("@Alex reminder: @everyone @here @Maya please"), "@Alex reminder: everyone here @Maya please");
        assert_eq!(s("@alex lower-case still matches"), "@alex lower-case still matches");
        assert_eq!(s("@Alexx is not Alex"), "Alexx is not Alex", "must end at a word boundary");
        assert_eq!(s("mail a@b.c"), "mail ab.c", "a bare @ is dropped");
        assert_eq!(strip_unknown_mentions("@Alex hi", &[]), "Alex hi", "no known members: nothing pings");
    }

    fn person(name: &str) -> Person {
        Person {
            name: name.into(),
            name_ar: None,
            role: "r".into(),
            role_ar: None,
            kind: "person".into(),
            working_on: String::new(),
            working_on_ar: None,
            status: "active".into(),
            gender: "m".into(),
            session_id: None,
        }
    }

    #[test]
    fn sec_l3_broadcast_handles_are_never_mentionable_even_if_the_file_lists_them() {
        for evil in ["everyone", "Everyone", "here", "channel", "all", "team", "anyone", "all staff", "here!", "<b>x</b>", "a@b", ""] {
            assert!(!is_person_name(evil), "{evil:?}");
        }
        for ok in ["Alex", "Maya", "Riley Ali", "Mary-Jane", "مايا", "Dr. Sam", "agent_7"] {
            assert!(is_person_name(ok), "{ok:?}");
        }
        assert!(!is_person_name(&"x".repeat(41)));
        // a crafted people list gets @everyone back out of the allowlist
        let known = known_names(&[person("Alex"), person("everyone"), person("here"), person("channel")]);
        assert_eq!(known, vec!["Alex".to_string()]);
        assert_eq!(strip_unknown_mentions("@Alex @everyone @here @channel @all", &known), "@Alex everyone here channel all");
    }

    #[test]
    fn sec_l3_switching_auto_remind_off_resets_its_start_time() {
        let dir = std::env::temp_dir().join(format!("one-desktop-remind-reset-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("team-state.json"), r#"{"auto_enabled_at": 1000}"#).unwrap();
        // on at start: the old start time is kept
        let on = Store::load(dir.join("team-state.json"));
        assert_eq!(on.auto_enabled_since(5000).unwrap(), 1000);
        // off at start: it is forgotten, so re-enabling later starts from THEN
        let _off = Team::new(dir.clone(), "Alex".into(), "python".into(), "x.py".into(), false, crate::autoboard::BoardPaths::none());
        let again = Store::load(dir.join("team-state.json"));
        assert_eq!(again.auto_enabled_since(9000).unwrap(), 9000);
    }

    #[test]
    fn sec_l3_the_poster_only_ever_sees_filtered_text() {
        let f = Fake::default();
        let known = vec!["Alex".to_string()];
        let filtered = MentionFilter { inner: &f, known: &known };
        let mut a = ask("a", 1);
        a.what = "@everyone sign off".into();
        a.whom = "Mallory".into(); // not on the team
        manual_remind(&store("mention").0, &filtered, &a, T0).unwrap();
        let sent = f.sent.lock().unwrap();
        assert!(!sent[0].contains('@'), "{}", sent[0]);
        assert!(sent[0].contains("Mallory"));
    }

    #[test]
    fn message_has_the_agreed_shape() {
        assert_eq!(build_message(&ask("a", 4), T0), "@Alex reminder: Hub needs Alex for an OK on T-2041 (open 4 days).");
    }

    #[test]
    fn message_is_sanitised_and_capped() {
        let mut a = ask("a", 1);
        a.what = format!("line one\nline two\r\n\u{0}\u{7}tab\there {}", "x".repeat(600));
        let m = build_message(&a, T0);
        assert!(!m.chars().any(|c| c.is_control()), "no control chars or newlines");
        assert!(m.chars().count() <= MSG_CAP);
        assert!(m.starts_with("@Alex reminder: Hub needs Alex for line one line two tab here"));
        a.whom = "Al\u{202E}ex".into(); // bidi override is dropped
        assert!(build_message(&a, T0).starts_with("@Alex "));
    }

    #[test]
    fn manual_remind_is_limited_to_one_per_item_per_hour() {
        let (s, p) = store("rate");
        let f = Fake::default();
        let a = ask("a", 1);
        manual_remind(&s, &f, &a, T0).unwrap();
        assert!(manual_remind(&s, &f, &a, T0 + 3599).is_err());
        manual_remind(&s, &f, &ask("b", 1), T0 + 10).unwrap(); // another item is unaffected
        manual_remind(&s, &f, &a, T0 + 3600).unwrap();
        assert_eq!(f.sent.lock().unwrap().len(), 3);
        // the limit survives a restart
        let s2 = Store::load(p);
        assert!(manual_remind(&s2, &f, &a, T0 + 3601).is_err());
    }

    #[test]
    fn failed_manual_post_does_not_use_up_the_hour() {
        let (s, _) = store("fail");
        let f = Fake::default();
        *f.fail.lock().unwrap() = Some(PostError::Failed("REFUSED: x".into()));
        assert!(manual_remind(&s, &f, &ask("a", 1), T0).is_err());
        manual_remind(&s, &f, &ask("a", 1), T0 + 1).unwrap();
        // an unknown outcome (timeout) keeps the slot, so it cannot double-post
        *f.fail.lock().unwrap() = Some(PostError::Unknown("timeout".into()));
        assert!(manual_remind(&s, &f, &ask("b", 1), T0).is_err());
        assert!(manual_remind(&s, &f, &ask("b", 1), T0 + 1).is_err());
    }

    #[test]
    fn done_items_are_not_reminded() {
        let (s, _) = store("done");
        let f = Fake::default();
        s.set_done("a", true, T0).unwrap();
        assert!(manual_remind(&s, &f, &ask("a", 5), T0).is_err());
        assert!(auto_remind(&s, &f, &[ask("a", 5)], T0, 0, 3).is_empty());
        assert!(f.sent.lock().unwrap().is_empty());
    }

    #[test]
    fn auto_remind_fires_once_at_three_days_even_across_restarts() {
        let (s, p) = store("auto");
        let f = Fake::default();
        let asks = vec![ask("young", 2), ask("old", 3)];
        assert_eq!(auto_remind(&s, &f, &asks, T0, 0, 3), vec!["old".to_string()]);
        assert!(auto_remind(&s, &f, &asks, T0 + 3600, 0, 3).is_empty()); // hourly check: no repeat
        let restarted = Store::load(p); // app restart
        assert!(auto_remind(&restarted, &f, &asks, T0 + 7200, 0, 3).is_empty());
        assert_eq!(f.sent.lock().unwrap().len(), 1);
        // the young one crosses 3 days later and is reminded exactly once
        assert_eq!(auto_remind(&restarted, &f, &asks, T0 + DAY, 0, 3), vec!["young".to_string()]);
        assert_eq!(f.sent.lock().unwrap().len(), 2);
    }

    #[test]
    fn sec_r2_m1_an_automatic_post_is_never_retried_whatever_the_failure() {
        let f = Fake::default();
        let asks = vec![ask("old", 4)];
        for (i, err) in [PostError::Failed("REFUSED: x".into()), PostError::Unknown("timeout".into())].into_iter().enumerate() {
            let (s, _) = store(&format!("noretry{i}"));
            *f.fail.lock().unwrap() = Some(err);
            assert!(auto_remind(&s, &f, &asks, T0, 0, 3).is_empty());
            assert!(auto_remind(&s, &f, &asks, T0 + 3600, 0, 3).is_empty(), "no retry on the next run");
            assert!(auto_remind(&s, &f, &asks, T0 + 7200, 0, 3).is_empty());
        }
        assert!(f.sent.lock().unwrap().is_empty());
    }

    #[test]
    fn sec_r2_m1_only_refused_counts_as_not_sent() {
        assert_eq!(classify_exit(true, ""), Ok(()));
        assert!(matches!(classify_exit(false, "REFUSED: message looks like it contains a phone"), Err(PostError::Failed(_))));
        for other in ["", "Traceback (most recent call last): ...", "queued (trigger failed; will post on next trigger)", "refused: lowercase"] {
            assert!(matches!(classify_exit(false, other), Err(PostError::Unknown(_))), "{other:?} must count as maybe sent");
        }
    }

    #[test]
    fn cr_m4_only_asks_created_after_enabling_are_eligible() {
        let (s, _) = store("enabled-since");
        let f = Fake::default();
        // enabled at T0: an ask that is already 5 days old was created before that
        let asks = vec![ask("backlog", 5), ask("later", -1)]; // created one day after enabling
        let since = s.auto_enabled_since(T0).unwrap();
        assert_eq!(since, T0);
        assert!(auto_remind(&s, &f, &asks, T0, since, 3).is_empty());
        // 4 days on, "later" has crossed 3 days and was created after enabling
        assert_eq!(auto_remind(&s, &f, &asks, T0 + 4 * DAY, since, 3), vec!["later".to_string()]);
        // the enabling time is stored once, not moved by a later call
        assert_eq!(s.auto_enabled_since(T0 + 9 * DAY).unwrap(), T0);
    }

    #[test]
    fn cr_m4_at_most_three_per_run() {
        let (s, _) = store("cap");
        let f = Fake::default();
        let asks: Vec<Ask> = (0..5).map(|i| ask(&format!("a{i}"), 4)).collect();
        assert_eq!(auto_remind(&s, &f, &asks, T0, 0, AUTO_MAX_PER_RUN).len(), 3);
        assert_eq!(auto_remind(&s, &f, &asks, T0 + 3600, 0, AUTO_MAX_PER_RUN).len(), 2); // the rest next hour
        assert_eq!(f.sent.lock().unwrap().len(), 5);
    }

    #[test]
    fn cr_m4_auto_remind_is_off_by_default_and_posts_nothing() {
        struct Src;
        impl TeamSource for Src {
            fn load(&self) -> Loaded {
                Loaded { status: Status::Ok, data: team::TeamData { people: vec![], asks: vec![ask("old", 9)], dropped: 0 } }
            }
            fn describe(&self) -> String {
                "fake".into()
            }
        }
        let (s, _) = store("off");
        let f = Fake::default();
        let t = Team::with_parts(Box::new(Src), s, Box::new(f.clone()), "Alex".into(), false);
        assert!(t.auto_tick(T0).is_empty());
        assert!(f.sent.lock().unwrap().is_empty());
        // switched on: the 9-day-old ask predates enabling, so it is still not sent
        let (s2, _) = store("on");
        let t2 = Team::with_parts(Box::new(Src), s2, Box::new(f.clone()), "Alex".into(), true);
        assert!(t2.auto_tick(T0).is_empty());
        assert!(f.sent.lock().unwrap().is_empty());
    }

    #[test]
    fn unsaveable_state_sends_nothing() {
        // A directory where the state file should be: saving fails, so no claim, no post.
        let dir = std::env::temp_dir().join(format!("one-desktop-remind-dir-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("state.json")).unwrap();
        let s = Store::load(dir.join("state.json"));
        let f = Fake::default();
        assert!(auto_remind(&s, &f, &[ask("old", 4)], T0, 0, 3).is_empty());
        assert!(manual_remind(&s, &f, &ask("old", 4), T0).is_err());
        assert!(f.sent.lock().unwrap().is_empty());
    }

    #[test]
    fn view_marks_amber_and_hides_done() {
        struct Src;
        impl TeamSource for Src {
            fn load(&self) -> Loaded {
                let asks = vec![ask("fresh", 1), ask("amber", 2), ask("gone", 5)];
                Loaded { status: Status::Ok, data: team::TeamData { people: vec![], asks, dropped: 0 } }
            }
            fn describe(&self) -> String {
                "fake".into()
            }
        }
        let (s, _) = store("view");
        s.set_done("gone", true, T0).unwrap();
        let t = Team::with_parts(Box::new(Src), s, Box::new(Fake::default()), "Alex".into(), false);
        let v = t.view(T0);
        let ids: Vec<_> = v.asks.iter().map(|a| (a.ask.id.as_str(), a.amber, a.long)).collect();
        assert_eq!(ids, vec![("fresh", false, false), ("amber", true, false)]);
        assert!(t.remind("nope", T0).is_err());
    }
}
