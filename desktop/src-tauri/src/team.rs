//! Team board data: the read-only source (a local JSON file in Phase 1), privacy
//! validation, and the ageing rules. No network, no process spawning in here.
//!
//! Source adapters (only `FileSource` exists today). Stubs for later, all read-only:
//!   - hub API / MCP: tickets, owners, approvals (Phase 2, once the hub is live)
//!   - team chat: posts with blockers tagged @Name
//!   - afo CLI: health and sessions (agent cards)
//!   - STATUS.md
//! Each adapter implements `TeamSource` and returns the same validated `Loaded`.

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::OnceLock;

pub const AMBER_SECS: i64 = 2 * 86_400;
pub const LONG_SECS: i64 = 3 * 86_400;
pub const WHAT_CAP: usize = 120;
const MAX_FILE_BYTES: u64 = 1_048_576;
const MAX_ENTRIES: usize = 500;

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Person {
    pub name: String,
    pub name_ar: Option<String>,
    pub role: String,
    pub role_ar: Option<String>,
    pub kind: String, // person | agent
    pub working_on: String,
    pub working_on_ar: Option<String>,
    pub status: String, // active | waiting
    pub gender: String, // m | f (Arabic verb forms only)
    /// Session cards only: the Claude session id, so "Ask about" goes through the focus path by id
    /// (never by matching text, never by putting that session's text in a message).
    #[serde(skip_serializing_if = "Option::is_none")] // pii-scan: allow (attribute name, not a key)
    pub session_id: Option<String>,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Ask {
    pub id: String,
    pub who: String,  // needs
    pub whom: String, // is needed
    pub what: String,
    pub what_ar: Option<String>,
    pub kind: String, // ticket | approval | access
    pub ticket_id: Option<String>,
    pub created_at: String,
    pub created_ts: i64,
}

#[derive(Default, Debug)]
pub struct TeamData {
    pub people: Vec<Person>,
    pub asks: Vec<Ask>,
    pub dropped: usize,
}

#[derive(Debug)]
pub enum Status {
    Ok,
    Missing,
    Invalid(String),
}

pub struct Loaded {
    pub status: Status,
    pub data: TeamData,
}

pub trait TeamSource: Send + Sync {
    fn load(&self) -> Loaded;
    /// The data that may be trusted to name people for @mentions (nothing taken from session
    /// transcripts). Sources without untrusted parts just load.
    fn load_trusted(&self) -> Loaded {
        self.load()
    }
    fn describe(&self) -> String;
}

pub struct FileSource {
    pub path: PathBuf,
}

impl TeamSource for FileSource {
    fn describe(&self) -> String {
        self.path.display().to_string()
    }

    fn load(&self) -> Loaded {
        let bad = |m: &str| Loaded { status: Status::Invalid(m.into()), data: TeamData::default() };
        match std::fs::metadata(&self.path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Loaded { status: Status::Missing, data: TeamData::default() }
            }
            Err(_) => return bad("could not read the team file"),
            Ok(m) if m.len() > MAX_FILE_BYTES => return bad("the team file is too big"),
            Ok(_) => {}
        }
        match std::fs::read_to_string(&self.path) {
            Ok(text) => parse_team(&text),
            Err(_) => bad("could not read the team file"),
        }
    }
}

// ---- privacy ----------------------------------------------------------------

fn private_patterns() -> &'static Vec<Regex> {
    static P: OnceLock<Vec<Regex>> = OnceLock::new();
    P.get_or_init(|| {
        // Same patterns as the remind script's privacy check (no lookaround in the Rust regex engine).
        [
            r"\b\d{1,3}(\.\d{1,3}){3}\b",                      // IPv4
            r"(sk|ghp|gho|xox[bp]|AKIA|EAA)[A-Za-z0-9_\-]{10,}", // tokens
            r"\+?\d[\d ()-]{9,}\d",                             // phone number
            r"[\w.+-]+@[\w-]+\.[\w.]+",                         // email (any domain)
        ]
        .iter()
        .map(|p| Regex::new(p).expect("static regex"))
        .collect()
    })
}

/// True when the text looks like a phone number, email, IP or token.
pub fn looks_private(s: &str) -> bool {
    private_patterns().iter().any(|r| r.is_match(s))
}

fn is_bidi(c: char) -> bool {
    matches!(c, '\u{200E}' | '\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')
}

/// Control characters and newlines become spaces, bidi overrides are dropped,
/// whitespace is collapsed, and the result is cut to `cap` characters.
pub fn clean(s: &str, cap: usize) -> String {
    let mut out = String::new();
    let mut last_space = true;
    for c in s.chars().filter(|c| !is_bidi(*c)) {
        if c.is_control() || c.is_whitespace() {
            if !last_space {
                out.push(' ');
                last_space = true;
            }
        } else {
            out.push(c);
            last_space = false;
        }
    }
    let cut: String = out.trim_end().chars().take(cap).collect();
    cut.trim_end().to_string()
}

/// Cleaned, capped text, or None when it looks private.
pub fn safe(s: &str, cap: usize) -> Option<String> {
    let c = clean(s, cap);
    if looks_private(&c) {
        None
    } else {
        Some(c)
    }
}

fn safe_opt(s: &Option<String>, cap: usize) -> Option<String> {
    s.as_deref().and_then(|t| safe(t, cap)).filter(|t| !t.is_empty())
}

// ---- time -------------------------------------------------------------------

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn num(s: &str) -> Option<i64> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Parse `YYYY-MM-DD[THH:MM[:SS[.fff]]][Z|+HH:MM|-HH:MM]` to Unix seconds. A timestamp with
/// no offset is read as UTC (documented in the README). Impossible dates (Feb 31) are rejected.
pub fn parse_iso(s: &str) -> Option<i64> {
    let s = s.trim();
    if !s.is_ascii() || s.len() < 10 {
        return None;
    }
    let (y, m, d) = (num(&s[0..4])?, num(&s[5..7])?, num(&s[8..10])?);
    if &s[4..5] != "-" || &s[7..8] != "-" || !(1..=12).contains(&m) || d < 1 || d > days_in_month(y, m) {
        return None;
    }
    let mut secs = days_from_civil(y, m, d) * 86_400;
    let rest = &s[10..];
    if rest.is_empty() {
        return Some(secs);
    }
    let rest = rest.strip_prefix('T').or_else(|| rest.strip_prefix(' '))?;
    if rest.len() < 5 || &rest[2..3] != ":" {
        return None;
    }
    let (hh, mm) = (num(&rest[0..2])?, num(&rest[3..5])?);
    if hh > 23 || mm > 59 {
        return None;
    }
    secs += hh * 3600 + mm * 60;
    let mut tail = &rest[5..];
    if let Some(t) = tail.strip_prefix(':') {
        secs += num(t.get(0..2)?)?.min(59);
        tail = &t[2..];
    }
    if let Some(t) = tail.strip_prefix('.') {
        tail = t.trim_start_matches(|c: char| c.is_ascii_digit());
    }
    Some(secs - tz_offset(tail)?)
}

fn tz_offset(tail: &str) -> Option<i64> {
    if tail.is_empty() || tail == "Z" {
        return Some(0);
    }
    let sign = match tail.as_bytes()[0] {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let t = tail[1..].replace(':', "");
    if t.len() != 4 {
        return None;
    }
    Some(sign * (num(&t[0..2])? * 3600 + num(&t[2..4])? * 60))
}

// ---- ageing -----------------------------------------------------------------

pub fn age_secs(created_ts: i64, now: i64) -> i64 {
    (now - created_ts).max(0)
}

/// Amber from 2 days.
pub fn is_amber(age: i64) -> bool {
    age >= AMBER_SECS
}

/// "Waiting long" tag, and the one automatic reminder, from 3 days.
pub fn is_long(age: i64) -> bool {
    age >= LONG_SECS
}

// ---- parsing and validation ---------------------------------------------------

#[derive(Deserialize)]
#[serde(untagged)]
enum IdIn {
    S(String),
    N(i64),
}

#[derive(Deserialize)]
struct RawAsk {
    id: IdIn,
    who: String,
    whom: String,
    what: String,
    kind: String,
    created_at: String,
    ticket_id: Option<String>,
    what_ar: Option<String>,
}

#[derive(Deserialize)]
struct RawPerson {
    name: String,
    name_ar: Option<String>,
    role: Option<String>,
    role_ar: Option<String>,
    kind: String,
    working_on: Option<String>,
    working_on_ar: Option<String>,
    status: Option<String>,
    gender: Option<String>,
}

fn valid_id(s: &str) -> bool {
    !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
}

pub fn validate_ask(raw: Value) -> Option<Ask> {
    let r: RawAsk = serde_json::from_value(raw).ok()?;
    let id = match r.id {
        IdIn::S(s) => s,
        IdIn::N(n) => n.to_string(),
    };
    let kind = r.kind.trim().to_lowercase();
    if !valid_id(&id) || !["ticket", "approval", "access"].contains(&kind.as_str()) {
        return None;
    }
    let ticket_id = match &r.ticket_id {
        Some(t) => Some(safe(t, 40).filter(|t| !t.is_empty())?),
        None => None,
    };
    let (who, whom, what) = (safe(&r.who, 60)?, safe(&r.whom, 60)?, safe(&r.what, WHAT_CAP)?);
    if who.is_empty() || whom.is_empty() || what.is_empty() {
        return None;
    }
    // A bad Arabic title rejects the item too: never show half-checked text.
    let what_ar = match &r.what_ar {
        Some(t) => Some(safe(t, WHAT_CAP)?).filter(|t| !t.is_empty()),
        None => None,
    };
    let created_ts = parse_iso(&r.created_at)?;
    Some(Ask { id, who, whom, what, what_ar, kind, ticket_id, created_at: clean(&r.created_at, 40), created_ts })
}

pub fn validate_person(raw: Value) -> Option<Person> {
    let r: RawPerson = serde_json::from_value(raw).ok()?;
    let kind = r.kind.trim().to_lowercase();
    if kind != "person" && kind != "agent" {
        return None;
    }
    let name = safe(&r.name, 60).filter(|n| !n.is_empty())?;
    // Free text that looks private is blanked, the person stays.
    let text = |t: &Option<String>, cap| safe(t.as_deref().unwrap_or(""), cap).unwrap_or_default();
    let status = if r.status.as_deref().map(|s| s.trim().eq_ignore_ascii_case("waiting")).unwrap_or(false) {
        "waiting"
    } else {
        "active"
    };
    let gender = if r.gender.as_deref() == Some("f") { "f" } else { "m" };
    Some(Person {
        name,
        name_ar: safe_opt(&r.name_ar, 60),
        role: text(&r.role, 80),
        role_ar: safe_opt(&r.role_ar, 80),
        kind,
        working_on: text(&r.working_on, 160),
        working_on_ar: safe_opt(&r.working_on_ar, 160),
        status: status.into(),
        gender: gender.into(),
        session_id: None,
    })
}

fn entries(v: &Value, key: &str) -> Vec<Value> {
    v.get(key).and_then(Value::as_array).map(|a| a.iter().take(MAX_ENTRIES).cloned().collect()).unwrap_or_default()
}

pub fn parse_team(text: &str) -> Loaded {
    let v: Value = match serde_json::from_str(text) {
        Ok(v) => v,
        Err(e) => return Loaded { status: Status::Invalid(format!("not valid JSON ({e})")), data: TeamData::default() },
    };
    let mut data = TeamData::default();
    for raw in entries(&v, "people") {
        match validate_person(raw) {
            Some(p) if !data.people.iter().any(|q| q.name.eq_ignore_ascii_case(&p.name)) => data.people.push(p),
            _ => data.dropped += 1,
        }
    }
    for raw in entries(&v, "asks") {
        match validate_ask(raw) {
            Some(a) if !data.asks.iter().any(|q| q.id == a.id) => data.asks.push(a),
            _ => data.dropped += 1,
        }
    }
    Loaded { status: Status::Ok, data }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ask(what: &str) -> Value {
        json!({"id":"a1","who":"Riley","whom":"Alex","what":what,"kind":"ticket","created_at":"2026-11-10T09:00:00Z"})
    }

    #[test]
    fn privacy_rejects_phone_email_ip_token() {
        for bad in [
            "call +1 555 0100 123 today", // pii-scan: allow (test fixture)
            "15550100123",
            "write to client@example.com",
            "host 10.0.0.12 is down", // pii-scan: allow (test fixture)
            "key ghp_abcdefghijklmnop1234", // pii-scan: allow (test fixture)
        ] {
            assert!(validate_ask(ask(bad)).is_none(), "should reject: {bad}");
        }
    }

    #[test]
    fn privacy_keeps_ticket_ids_and_short_titles() {
        for ok in ["T-2041 \"Invoice export fails\"", "60 labelled tickets", "which email clients see (info@)", "T-2044 and T-2046"] {
            assert!(validate_ask(ask(ok)).is_some(), "should keep: {ok}");
        }
    }

    #[test]
    fn privacy_applies_to_every_text_field() {
        let mut a = ask("fine");
        a["who"] = json!("a@b.co"); // pii-scan: allow (test fixture)
        assert!(validate_ask(a).is_none());
        let mut a = ask("fine");
        a["what_ar"] = json!("اتصل على 0100 123 4567");
        assert!(validate_ask(a).is_none());
        let mut a = ask("fine");
        a["ticket_id"] = json!("+15550100123"); // pii-scan: allow (test fixture)
        assert!(validate_ask(a).is_none());
    }

    #[test]
    fn what_is_capped_and_cleaned() {
        let long = "x".repeat(500);
        assert_eq!(validate_ask(ask(&long)).unwrap().what.chars().count(), WHAT_CAP);
        assert_eq!(validate_ask(ask("a\n\tb\u{7}c  d")).unwrap().what, "a b c d");
    }

    #[test]
    fn bad_kind_id_or_date_is_rejected() {
        let mut a = ask("x");
        a["kind"] = json!("rumour");
        assert!(validate_ask(a).is_none());
        let mut a = ask("x");
        a["id"] = json!("bad id!");
        assert!(validate_ask(a).is_none());
        let mut a = ask("x");
        a["created_at"] = json!("yesterday");
        assert!(validate_ask(a).is_none());
        let mut a = ask("x");
        a["id"] = json!(7);
        assert_eq!(validate_ask(a).unwrap().id, "7");
    }

    #[test]
    fn person_text_is_blanked_not_dropped() {
        let p = validate_person(json!({"name":"Riley","kind":"person","working_on":"ring +1 555 0100 123"})).unwrap(); // pii-scan: allow (test fixture)
        assert_eq!(p.working_on, "");
        assert!(validate_person(json!({"name":"x@y.com","kind":"person"})).is_none()); // pii-scan: allow (test fixture)
        assert!(validate_person(json!({"name":"Bot","kind":"robot"})).is_none());
    }

    #[test]
    fn one_bad_item_does_not_sink_the_file() {
        let text = json!({"people":[],"asks":[ask("ok"), {"nonsense":1}, ask("ok again")]}).to_string();
        let l = parse_team(&text);
        assert_eq!(l.data.asks.len(), 1); // duplicate id a1 dropped too
        assert_eq!(l.data.dropped, 2);
        assert!(matches!(parse_team("{").status, Status::Invalid(_)));
    }

    #[test]
    fn iso_parsing() {
        assert_eq!(parse_iso("1970-01-02T00:00:00Z"), Some(86_400));
        assert_eq!(parse_iso("1970-01-01T02:00:00+02:00"), Some(0));
        assert_eq!(parse_iso("1970-01-01T00:00:00.123Z"), Some(0));
        assert_eq!(parse_iso("1970-01-01"), Some(0));
        assert_eq!(parse_iso("2026-11-13T12:00:00Z"), Some(1_794_571_200));
        assert_eq!(parse_iso("2026-13-01"), None);
        assert_eq!(parse_iso("2026-02-31"), None);
        assert_eq!(parse_iso("2026-02-29"), None);
        assert_eq!(parse_iso("2028-02-29T00:00:00Z"), Some(1_835_395_200));
        assert_eq!(parse_iso("nope"), None);
    }

    #[test]
    fn amber_at_two_days_long_at_three() {
        let day = 86_400;
        assert!(!is_amber(2 * day - 1));
        assert!(is_amber(2 * day));
        assert!(!is_long(3 * day - 1));
        assert!(is_long(3 * day));
        assert_eq!(age_secs(100, 50), 0); // future dates never go negative
    }

    #[test]
    fn shipped_example_file_passes_validation_untouched() {
        let l = parse_team(include_str!("../../team-asks.example.json"));
        assert!(matches!(l.status, Status::Ok));
        assert_eq!((l.data.people.len(), l.data.asks.len(), l.data.dropped), (9, 11, 0));
    }

    #[test]
    fn missing_file_is_missing_not_an_error() {
        let src = FileSource { path: std::env::temp_dir().join("one-desktop-no-such-team-asks.json") };
        assert!(matches!(src.load().status, Status::Missing));
    }
}
