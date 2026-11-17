//! The Team board fills itself. Read-only, bounded, redacted sources:
//!   1. `team-asks.json` (One's data folder, or the path in settings.json `team_asks_file`):
//!      people and asks written by the coordinator, in the same format as always;
//!   2. the status notes (`status_file`, default `<cwd>/STATUS.md`): each `- **Project: summary.** ... **Next:** ... **Waiting on:** ...`
//!      bullet becomes a project card (plain `- Name: text` lines work too);
//!   3. the Claude Code sessions of the working directory: one agent card per session, by its
//!      tab title, waiting when the last message came from the assistant.
//! No file is ever required: with nothing found the board says nothing is waiting.

use crate::tabs::SessionActivity;
use crate::team::{self, safe, FileSource, Loaded, Person, Status, TeamData, TeamSource};
use std::path::PathBuf;

const MAX_STATUS_BYTES: u64 = 256 * 1024;
const MAX_STATUS_LINES: usize = 200;
const MAX_SESSIONS: usize = 8;
/// A session untouched for longer than this is not on the board.
const SESSION_FRESH_SECS: i64 = 3 * 86_400;

#[derive(Clone, Debug, Default)]
pub struct BoardPaths {
    pub asks_file: Option<PathBuf>,
    pub status_file: Option<PathBuf>,
    pub cwd: String,
    pub sessions: bool,
}

impl BoardPaths {
    #[allow(dead_code)] // used by tests that build a Team without sources
    pub fn none() -> Self {
        Self::default()
    }

    /// Empty settings values mean the defaults.
    pub fn from_config(status_file: &str, team_asks_file: &str, cwd: &str) -> Self {
        let status = if status_file.trim().is_empty() { default_status_path(cwd) } else { PathBuf::from(status_file.trim()) };
        let asks = (!team_asks_file.trim().is_empty()).then(|| PathBuf::from(team_asks_file.trim()));
        BoardPaths { asks_file: asks, status_file: Some(status), cwd: cwd.to_string(), sessions: true }
    }
}

/// Where the status notes live when settings.json has no `status_file`: `STATUS.md` in One's
/// working directory.
pub fn default_status_path(cwd: &str) -> PathBuf {
    PathBuf::from(cwd).join("STATUS.md")
}

/// The card name of a bold lead: `All for One (main focus), the shared hub` is `All for One`.
fn project_name(bold: &str) -> String {
    let lead = bold.split(':').next().unwrap_or(bold);
    let cut = lead.find([',', '(']).map_or(lead, |i| &lead[..i]);
    team::clean(cut.trim().trim_end_matches(['.', ':']), 60)
}

/// One `- **Project: summary.** text **Next:** ... **Waiting on:** ...` line, as a project card.
fn project_card(line: &str) -> Option<Person> {
    let rest = line.strip_prefix("**")?;
    let (bold, after) = rest.split_once("**")?;
    if after.trim_start().starts_with(':') {
        return None; // `**Name**: text` is a plain person line
    }
    let name = project_name(bold);
    // a sub-bullet like "**Next:** ..." is a label, not a project
    if name.is_empty() || reserved(&name) {
        return None;
    }
    let body = after.split("**Next:**").next().unwrap_or("").split("**Waiting on:**").next().unwrap_or("");
    let summary = format!("{} {}", bold.split_once(':').map_or("", |(_, s)| s.trim()), body.replace("**", ""));
    let working_on = readable(&crate::redact::redact(summary.trim()), 160).unwrap_or_default();
    let lower = body.to_lowercase();
    // the writer marks it with **Waiting on:**, or just says it in the text
    let waiting = after.split("**Waiting on:**").nth(1).is_some_and(|w| !w.trim().is_empty()) || says_waiting(&lower);
    Some(person(&name, "Project", "project", working_on, if waiting { "waiting" } else { "active" }))
}

/// The status notes as cards. Bullets of the form `- **Project: summary.** ...` become project
/// cards; a plain `- Name: what they do` becomes a person card (a name must look like a person's).
/// Text is redacted and cleaned like every board text. Bounded in size and lines.
pub fn parse_status(text: &str) -> Vec<Person> {
    let mut out: Vec<Person> = Vec::new();
    for line in text.lines().take(MAX_STATUS_LINES) {
        let line = line.trim_start();
        if !line.starts_with(['-', '*', '+']) {
            continue; // only bullets are cards (not "updated: ..." or headings)
        }
        let line = line.trim_start_matches(['-', '*', '+']).trim_start();
        let card = project_card(line).or_else(|| plain_card(line));
        if let Some(p) = card.filter(|p| !out.iter().any(|q| q.name.eq_ignore_ascii_case(&p.name))) {
            out.push(p);
        }
    }
    out
}

/// "waiting on" / "blocked" in the text, but not "not waiting on", "no longer blocked" or "unblocked".
fn says_waiting(lower: &str) -> bool {
    let hit = |w: &str| {
        lower.match_indices(w).any(|(i, _)| {
            let before = lower[..i].trim_end();
            !(before.ends_with("not") || before.ends_with("no longer") || lower[..i].ends_with("un"))
        })
    };
    hit("waiting on") || hit("blocked")
}

/// Card text that reads well: whole sentences up to `cap` characters; if even the first sentence
/// is longer, it is cut at a word and ends with "…". Never a word cut in half.
fn readable(s: &str, cap: usize) -> Option<String> {
    let full = safe(s, 4000)?;
    if full.chars().count() <= cap {
        return Some(full);
    }
    let head: String = full.chars().take(cap).collect();
    // the last sentence end inside the limit (". ", "! ", "? " or the Arabic "؟ ")
    let ends = [". ", "! ", "? ", "\u{061F} "];
    if let Some(i) = ends.iter().filter_map(|e| head.rfind(e).map(|i| i + e.len() - 1)).max() {
        if head[..i].chars().count() >= cap / 3 {
            return Some(head[..i].trim_end().to_string());
        }
    }
    let cut = head.rfind(char::is_whitespace).unwrap_or(head.len());
    Some(format!("{}\u{2026}", head[..cut].trim_end_matches(|c: char| c.is_whitespace() || c == ',' || c == ';' || c == ':')))
}

/// Labels used inside a bullet, never a card name.
fn reserved(name: &str) -> bool {
    ["next", "waiting on", "blocked", "done", "updated"].contains(&name.trim().to_lowercase().as_str())
}

fn plain_card(line: &str) -> Option<Person> {
    let line = line.replace("**", "");
    let (name, rest) = line.split_once(':')?;
    let name = name.trim();
    if rest.starts_with("//") {
        return None; // a URL, not "Name: text"
    }
    if !crate::remind::is_person_name(name) || reserved(name) {
        return None;
    }
    let working_on = readable(&crate::redact::redact(rest), 160).filter(|w| !w.is_empty())?;
    let lower = working_on.to_lowercase();
    let status = if lower.contains("waiting") || lower.contains("blocked") { "waiting" } else { "active" };
    Some(person(name, "Team", "person", working_on, status))
}

fn person(name: &str, role: &str, kind: &str, working_on: String, status: &str) -> Person {
    Person {
        name: team::clean(name, 60),
        name_ar: None,
        role: role.into(),
        role_ar: None,
        kind: kind.into(),
        working_on,
        working_on_ar: None,
        status: status.into(),
        gender: "m".into(),
        session_id: None,
    }
}

/// One agent card per fresh session.
pub fn sessions_to_people(acts: &[SessionActivity], now: i64) -> Vec<Person> {
    acts.iter()
        .filter(|a| a.live && now - a.modified <= SESSION_FRESH_SECS) // finished sessions are not on the board
        .take(MAX_SESSIONS)
        .filter_map(|a| {
            let name = safe(&a.tab_title, 60).filter(|n| !n.is_empty())?;
            let working_on = readable(&a.snippet, 90).unwrap_or_default();
            let status = if a.last_role == "assistant" { "waiting" } else { "active" };
            let mut p = person(&name, "Session", "session", working_on, status);
            p.session_id = Some(a.session_id.clone());
            Some(p)
        })
        .collect()
}

fn read_status_file(path: &PathBuf) -> Vec<Person> {
    match std::fs::metadata(path) {
        Ok(m) if m.is_file() && m.len() <= MAX_STATUS_BYTES => std::fs::read_to_string(path).map(|t| parse_status(&t)).unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn add_new(data: &mut TeamData, people: Vec<Person>) {
    for p in people {
        if !data.people.iter().any(|q| q.name.eq_ignore_ascii_case(&p.name)) {
            data.people.push(p);
        }
    }
}

pub struct AutoSource {
    default_asks: PathBuf,
    paths: BoardPaths,
}

impl AutoSource {
    pub fn new(default_asks: PathBuf, paths: BoardPaths) -> Self {
        AutoSource { default_asks, paths }
    }

    fn asks_path(&self) -> PathBuf {
        self.paths.asks_file.clone().unwrap_or_else(|| self.default_asks.clone())
    }

    /// `trusted_only`: just `team-asks.json`. The status file is written by an agent and the
    /// sessions are transcripts: neither may decide who can be @mentioned.
    fn assemble(&self, trusted_only: bool, now: i64) -> Loaded {
        let with_sessions = !trusted_only;
        // A broken team-asks.json is reported; a missing one is normal.
        let mut data = match (FileSource { path: self.asks_path() }).load() {
            Loaded { status: Status::Invalid(m), data } => return Loaded { status: Status::Invalid(m), data },
            Loaded { data, .. } => data,
        };
        if let (Some(p), false) = (&self.paths.status_file, trusted_only) {
            add_new(&mut data, read_status_file(p));
        }
        if with_sessions && self.paths.sessions {
            let acts = crate::tabs::session_activity(&self.paths.cwd, MAX_SESSIONS * 2);
            add_new(&mut data, sessions_to_people(&acts, now));
        }
        Loaded { status: Status::Ok, data }
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

impl TeamSource for AutoSource {
    fn describe(&self) -> String {
        self.asks_path().display().to_string()
    }
    fn load(&self) -> Loaded {
        self.assemble(false, now_secs())
    }
    /// No session-derived names: a transcript must not decide who can be @mentioned.
    fn load_trusted(&self) -> Loaded {
        self.assemble(true, now_secs())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn act(title: &str, role: &'static str, modified: i64) -> SessionActivity {
        SessionActivity { session_id: format!("00000000-0000-4000-8000-{:012}", modified.unsigned_abs() % 1_000_000_000_000), tab_title: title.into(), modified, last_role: role, snippet: "doing things".into(), live: true }
    }

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("one-desktop-autoboard-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn status_bullets_become_people_and_junk_lines_are_skipped() {
        let secret = "k3j4h5g6f7".repeat(4);
        let t = format!("# Status\n- Riley: labelling tickets\n* **Maya**: waiting on Sam for the number\n- Sam: token {secret}\nplain line\n- http://x.example: no\n- Riley: duplicate\n- everyone: nope\n- Jordan:\n");
        let p = parse_status(&t);
        let names: Vec<&str> = p.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names, ["Riley", "Maya", "Sam"]);
        assert_eq!(p[1].status, "waiting");
        assert_eq!(p[0].working_on, "labelling tickets");
        assert!(!p[2].working_on.contains("k3j4h5g6"), "redacted");
    }

    #[test]
    fn status_parsing_is_bounded() {
        let big: String = (0..1000).map(|i| format!("- P{i}: x\n")).collect();
        assert!(parse_status(&big).len() <= MAX_STATUS_LINES);
    }

    #[test]
    fn sessions_become_agent_cards_waiting_when_the_assistant_spoke_last_and_old_ones_are_left_out() {
        let now = 1_000_000;
        let p = sessions_to_people(&[act("hub-lane", "assistant", now - 60), act("qa-lane", "user", now - 60), act("old", "user", now - 5 * 86_400)], now);
        assert_eq!(
            p.iter().map(|x| (x.name.as_str(), x.status.as_str(), x.kind.as_str())).collect::<Vec<_>>(),
            [("hub-lane", "waiting", "session"), ("qa-lane", "active", "session")]
        );
        assert!(p.iter().all(|x| x.session_id.as_deref().is_some_and(|s| s.len() == 36)), "a session card carries its id for Ask about");
    }

    #[test]
    fn finished_sessions_are_not_cards() {
        let now = 1_000_000;
        let mut done = act("all-for-one-2", "assistant", now - 3_600);
        done.live = false;
        let p = sessions_to_people(&[done, act("hub-lane", "user", now - 60)], now);
        assert_eq!(p.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(), ["hub-lane"]);
    }

    #[test]
    fn nothing_found_is_an_ok_empty_board_not_an_error() {
        let d = dir("empty");
        let src = AutoSource::new(d.join("team-asks.json"), BoardPaths { status_file: Some(d.join("none.md")), ..Default::default() });
        let l = src.load();
        assert!(matches!(l.status, Status::Ok) && l.data.people.is_empty() && l.data.asks.is_empty());
    }

    #[test]
    fn the_file_people_win_and_status_adds_new_names_only_and_a_broken_file_is_reported() {
        let d = dir("merge");
        std::fs::write(d.join("team-asks.json"), r#"{"people":[{"name":"Riley","kind":"person","role":"QA","working_on":"from file","status":"active"}],"asks":[]}"#).unwrap();
        std::fs::write(d.join("S.md"), "- Riley: from status\n- Maya: from status\n").unwrap();
        let src = AutoSource::new(d.join("team-asks.json"), BoardPaths { status_file: Some(d.join("S.md")), ..Default::default() });
        let l = src.load();
        assert_eq!(l.data.people.iter().map(|p| (p.name.as_str(), p.working_on.as_str())).collect::<Vec<_>>(), [("Riley", "from file"), ("Maya", "from status")]);
        std::fs::write(d.join("team-asks.json"), "{ nope").unwrap();
        assert!(matches!(src.load().status, Status::Invalid(_)));
    }

    #[test]
    fn the_real_status_bullets_become_project_cards() {
        let secret = "k3j4h5g6f7".repeat(4);
        let t = format!(
            "# What Alex is working on\nupdated: today\n\n\
- **All for One (main focus), the shared middleware hub:** WhatsApp hub PRs are merged, key {secret}. **Next:** merge after OK. **Waiting on:** Sam's OK on the number.\n\n\
- **Reports:** Fixes are live in production. **Next:** review and merge.\n\
- **Helpdesk agent / support automation:** The helpdesk cycle is being automated.\n"
        );
        let p = parse_status(&t);
        assert_eq!(p.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(), ["All for One", "Reports", "Helpdesk agent / support automation"]);
        assert_eq!((p[0].status.as_str(), p[1].status.as_str()), ("waiting", "active"), "Waiting on: marks a card waiting");
        assert!(p[0].working_on.starts_with("WhatsApp hub PRs are merged"), "{}", p[0].working_on);
        assert!(!p[0].working_on.contains("k3j4h5g6") && !p[0].working_on.contains("Next") && !p[0].working_on.contains("**"));
        assert_eq!(p[0].kind, "project");
        assert_eq!(p[1].working_on, "Fixes are live in production.");
    }

    #[test]
    fn the_default_status_path_is_status_md_in_the_working_directory() {
        let cwd = dir("cwd");
        let cwd_s = cwd.to_string_lossy().into_owned();
        assert_eq!(default_status_path(&cwd_s), cwd.join("STATUS.md"));
    }

    #[test]
    fn explicit_settings_paths_are_used_as_they_are() {
        let p = BoardPaths::from_config("", "", r"C:\work");
        assert!(p.status_file.is_some() && p.asks_file.is_none());
        let q = BoardPaths::from_config(" D:/s.md ", "D:/a.json", "x");
        assert_eq!((q.status_file, q.asks_file), (Some(PathBuf::from("D:/s.md")), Some(PathBuf::from("D:/a.json"))));
    }

    #[test]
    fn card_text_is_never_cut_mid_word() {
        let long = "Phase 1 is live. Maya has ruled that replies should never open a ticket. ".repeat(4);
        let r = readable(&long, 160).unwrap();
        assert!(r.ends_with('.') && r.chars().count() <= 160, "{r}");
        let one = format!("A {}", "word ".repeat(60));
        let r = readable(&one, 90).unwrap();
        assert!(r.ends_with('\u{2026}') && !r.contains("wor\u{2026}") && r.chars().count() <= 91, "{r}");
        assert_eq!(readable("short", 90).as_deref(), Some("short"));
    }

    #[test]
    fn negated_waiting_is_not_waiting() {
        for (t, w) in [("waiting on alex", true), ("now blocked by ci", true), ("not waiting on anyone", false), ("unblocked today", false), ("no longer blocked", false)] {
            assert_eq!(says_waiting(t), w, "{t}");
        }
    }

    #[test]
    fn plain_waiting_on_in_the_text_counts_and_label_bullets_are_not_projects() {
        let t = "- **Acme CX:** Phase 1 is live. Waiting on Alex's decision. **Next:** deploy.\n- **Next:** something\n- **Reports:** fixes are live. **Next:** merge.\n";
        let p = parse_status(t);
        assert_eq!(p.iter().map(|x| (x.name.as_str(), x.status.as_str())).collect::<Vec<_>>(), [("Acme CX", "waiting"), ("Reports", "active")]);
    }

    #[test]
    fn the_trusted_view_has_team_asks_only() {
        let d = dir("trusted");
        std::fs::write(d.join("team-asks.json"), r#"{"people":[{"name":"Riley","kind":"person","role":"QA","status":"active"}],"asks":[]}"#).unwrap();
        std::fs::write(d.join("S.md"), "- Maya: from status\n- **Proj:** text\n").unwrap();
        let src = AutoSource::new(d.join("team-asks.json"), BoardPaths { status_file: Some(d.join("S.md")), ..Default::default() });
        assert_eq!(src.load_trusted().data.people.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["Riley"], "no status names for @mentions");
        assert_eq!(src.load().data.people.len(), 3);
    }
}
