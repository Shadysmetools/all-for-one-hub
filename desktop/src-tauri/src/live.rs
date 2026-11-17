//! Which Claude Code sessions are LIVE, and which are finished.
//!
//! A transcript file never says "this session ended", and `~/.claude/sessions/<pid>.json`
//! files outlive their process, so neither the file's existence nor "newest in the list" means a
//! session is running. The evidence, read-only:
//! - the transcript's last write, and the registry's `updatedAt` (recent = live);
//! - the job's `~/.claude/jobs/<jobId>/state.json`: `done`, `stopped` or `failed` = finished,
//!   whatever the clock says (unless the transcript was written clearly after it, i.e. resumed).
//!
//! A busy session can write nothing to the main transcript for a long while (a long tool run,
//! subagents), so these also count as life: the registry saying `busy`, a `working` job whose
//! state file moved, and a recent subagent transcript (`<sid>/subagents/*.jsonl`).
//!
//! `blocked` alone is NOT live: most blocked jobs are days old.

use std::collections::HashMap;
use std::path::Path;

/// A session with no activity for longer than this is not live.
pub const LIVE_SECS: i64 = 30 * 60;
/// A transcript written this much after its job ended means the session was resumed.
const RESUME_SLACK: i64 = 300;
/// A registry entry that says `busy` counts for this long after its last update (a dead process
/// leaves `busy` behind forever, so it cannot count for ever).
const BUSY_MAX_SECS: i64 = 4 * 3600;
/// Timestamps further in the future than this are clock garbage and ignored.
const FUTURE_SLACK: i64 = 86_400;
const MAX_STATE_BYTES: u64 = 256 * 1024;
const MAX_SUBAGENT_FILES: usize = 200;

/// What the registry file `~/.claude/sessions/<pid>.json` says about a session.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Registry {
    /// Seconds since the Unix epoch.
    pub updated: Option<i64>,
    /// The session says it is busy right now.
    pub busy: bool,
    pub job_id: Option<String>,
}

/// A job's `state.json`: its state word and when the file last changed.
#[derive(Clone, Debug, PartialEq)]
pub struct JobState {
    pub state: String,
    pub changed: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Liveness {
    pub live: bool,
    /// Last sign of life (live) or when it finished.
    pub at: i64,
}

fn finished_state(s: &str) -> bool {
    matches!(s, "done" | "stopped" | "failed")
}

/// A timestamp that cannot be right (far in the future) counts as "no signal"; a slightly
/// future one (clock skew) counts as now.
fn sane(now: i64, t: i64) -> i64 {
    if t > now + FUTURE_SLACK {
        0
    } else {
        t.min(now)
    }
}

/// The pure rule. `transcript`: the transcript's last write; `subagent`: the newest write under
/// `<sid>/subagents` (0 = none).
pub fn liveness(now: i64, transcript: i64, subagent: i64, reg: Option<&Registry>, job: Option<&JobState>) -> Liveness {
    let (transcript, subagent) = (sane(now, transcript), sane(now, subagent));
    let reg_updated = reg.and_then(|r| r.updated).map(|t| sane(now, t)).unwrap_or(0);
    let working = job.filter(|j| j.state == "working").map(|j| sane(now, j.changed)).unwrap_or(0);
    let seen = transcript.max(subagent).max(reg_updated).max(working);
    if let Some(j) = job.filter(|j| finished_state(&j.state)) {
        if transcript.max(subagent) <= j.changed + RESUME_SLACK {
            return Liveness { live: false, at: j.changed.min(now) };
        }
    }
    let busy = reg.is_some_and(|r| r.busy) && now - reg_updated <= BUSY_MAX_SECS;
    Liveness { live: busy || now - seen <= LIVE_SECS, at: if busy { seen.max(reg_updated) } else { seen } }
}

/// "just now", "5 min ago", "3 h ago", "2 days ago".
pub fn ago(now: i64, then: i64) -> String {
    let d = (now - then).max(0);
    match d {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", d / 60),
        3600..=86_399 => format!("{} h ago", d / 3600),
        _ => {
            let n = d / 86_400;
            format!("{n} day{} ago", if n == 1 { "" } else { "s" })
        }
    }
}

/// "live now" or "finished 3 h ago".
pub fn status_text(now: i64, l: &Liveness) -> String {
    if l.live {
        "live now".into()
    } else {
        format!("finished {}", ago(now, l.at))
    }
}

/// sessionId -> registry data, from `~/.claude/sessions/*.json` (a missing directory is empty).
pub fn read_registry(dir: &Path) -> HashMap<String, Registry> {
    let mut out = HashMap::new();
    let Ok(files) = std::fs::read_dir(dir) else { return out };
    for f in files.flatten().filter(|f| f.path().extension().is_some_and(|e| e == "json")) {
        if f.metadata().map(|m| m.len() > MAX_STATE_BYTES).unwrap_or(true) {
            continue;
        }
        let Some(v) = std::fs::read(f.path()).ok().and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok()) else { continue };
        let Some(sid) = v["sessionId"].as_str().filter(|s| crate::tabs::valid_session_id(s)) else { continue };
        let updated = v["updatedAt"].as_i64().into_iter().chain(v["statusUpdatedAt"].as_i64()).max().map(|ms| ms / 1000);
        let busy = v["status"].as_str() == Some("busy");
        let job_id = v["jobId"].as_str().filter(|j| !j.is_empty() && j.len() <= 64 && j.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')).map(String::from);
        // two files can name one session: keep the most recent
        let entry = out.entry(sid.to_string()).or_insert_with(Registry::default);
        if updated >= entry.updated {
            *entry = Registry { updated, busy, job_id: job_id.or_else(|| entry.job_id.clone()) };
        }
    }
    out
}

/// `~/.claude/jobs/<job_id>/state.json`. The id was checked to be plain characters.
pub fn read_job(jobs_dir: &Path, job_id: &str) -> Option<JobState> {
    let path = jobs_dir.join(job_id).join("state.json");
    let meta = std::fs::metadata(&path).ok()?;
    if meta.len() > MAX_STATE_BYTES {
        return None;
    }
    let v: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).ok()?).ok()?;
    let changed = meta.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_secs() as i64;
    Some(JobState { state: v["state"].as_str()?.to_string(), changed })
}

/// The newest write under `<projects>/<project>/<sid>/subagents/*.jsonl` (0 = none). Bounded.
pub fn subagent_mtime(projects: &Path, project: &str, sid: &str) -> i64 {
    let Ok(rd) = std::fs::read_dir(projects.join(project).join(sid).join("subagents")) else { return 0 };
    rd.flatten()
        .take(MAX_SUBAGENT_FILES)
        .filter(|f| f.path().extension().is_some_and(|e| e == "jsonl"))
        .filter_map(|f| f.metadata().ok()?.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok().map(|d| d.as_secs() as i64))
        .max()
        .unwrap_or(0)
}

/// Liveness of one session from the real `~/.claude` folders.
pub fn for_session(base: &Path, project: &str, session_id: &str, transcript: i64, now: i64) -> Liveness {
    let reg = read_registry(&base.join("sessions"));
    let job = reg.get(session_id).and_then(|r| r.job_id.as_deref()).and_then(|j| read_job(&base.join("jobs"), j));
    liveness(now, transcript, subagent_mtime(&base.join("projects"), project, session_id), reg.get(session_id), job.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 2_000_000_000;
    fn job(state: &str, ago_s: i64) -> JobState {
        JobState { state: state.into(), changed: NOW - ago_s }
    }

    fn reg(updated: i64, busy: bool, job: Option<&str>) -> Registry {
        Registry { updated: Some(updated), busy, job_id: job.map(String::from) }
    }
    fn live(transcript: i64, sub: i64, r: Option<&Registry>, j: Option<&JobState>) -> Liveness {
        liveness(NOW, transcript, sub, r, j)
    }

    #[test]
    fn a_recent_transcript_is_live() {
        let l = live(NOW - 120, 0, None, None);
        assert!(l.live && l.at == NOW - 120);
    }

    #[test]
    fn a_day_old_transcript_is_finished() {
        let l = live(NOW - 86_400, 0, None, None);
        assert!(!l.live);
        assert_eq!(status_text(NOW, &l), "finished 1 day ago");
    }

    #[test]
    fn a_done_job_is_finished_even_if_the_transcript_is_fresh_ish() {
        // all-for-one-2: job done, registry says idle, transcript last written when the job ended
        let r = reg(NOW - 100, false, Some("45f42afe"));
        let l = live(NOW - 100, 0, Some(&r), Some(&job("done", 100)));
        assert!(!l.live, "a finished job is not live just because it is recent");
        assert_eq!(status_text(NOW, &l), "finished 1 min ago");
    }

    #[test]
    fn a_resumed_session_is_live_again() {
        assert!(live(NOW - 30, 0, None, Some(&job("done", 7200))).live);
    }

    #[test]
    fn blocked_alone_is_not_live_and_an_old_working_job_is_not_live() {
        assert!(!live(NOW - 40_000, 0, None, Some(&job("working", 40_000))).live);
        assert!(!live(NOW - 3 * 86_400, 0, None, Some(&job("blocked", 3 * 86_400))).live);
    }

    #[test]
    fn the_registry_update_counts_as_a_sign_of_life() {
        assert!(live(NOW - 86_400, 0, Some(&reg(NOW - 30, false, None)), None).live);
    }

    #[test]
    fn a_busy_session_that_wrote_nothing_for_an_hour_is_still_live() {
        // busy in the registry (updated 50 min ago), silent transcript
        let r = reg(NOW - 3000, true, None);
        assert!(live(NOW - 3600, 0, Some(&r), None).live, "registry busy");
        // a working job whose state file moved 5 min ago
        assert!(live(NOW - 3600, 0, None, Some(&job("working", 300))).live, "working job moved");
        // a subagent wrote 2 min ago
        assert!(live(NOW - 3600, NOW - 120, None, None).live, "subagent transcript");
        // but busy left behind by a dead process 2 days ago is not live
        assert!(!live(NOW - 2 * 86_400, 0, Some(&reg(NOW - 2 * 86_400, true, None)), None).live);
        // and a done job is not rescued by a stale busy flag
        let r = reg(NOW - 600, true, Some("j"));
        assert!(!live(NOW - 700, 0, Some(&r), Some(&job("done", 650))).live);
    }

    #[test]
    fn clock_skew_never_gives_a_negative_age_or_a_forever_live_session() {
        // 30 s in the future (skew): live, "just now", never negative
        let l = live(NOW + 30, 0, None, None);
        assert!(l.live && l.at <= NOW);
        assert_eq!(status_text(NOW, &l), "live now");
        assert_eq!(ago(NOW, NOW + 500), "just now");
        // a timestamp years in the future is garbage, not "live for ever"
        assert!(!live(NOW + 5 * 365 * 86_400, 0, None, None).live);
        // a finished job whose file is in the future reads "just now", not negative
        let l = live(NOW - 5000, 0, None, Some(&job("done", -50)));
        assert!(!l.live && status_text(NOW, &l) == "finished just now");
    }

    #[test]
    fn the_newest_subagent_file_is_found_and_the_rest_ignored() {
        let d = std::env::temp_dir().join(format!("one-sub-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        let sid = "45f42afe-e89a-4ec2-87ff-592852ee9e3e";
        std::fs::create_dir_all(d.join("proj").join(sid).join("subagents")).unwrap();
        assert_eq!(subagent_mtime(&d, "proj", sid), 0, "none yet");
        std::fs::write(d.join("proj").join(sid).join("subagents").join("a.jsonl"), "x").unwrap();
        std::fs::write(d.join("proj").join(sid).join("subagents").join("b.txt"), "x").unwrap();
        assert!(subagent_mtime(&d, "proj", sid) > 1_700_000_000);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn ago_reads_naturally() {
        assert_eq!(ago(NOW, NOW - 10), "just now");
        assert_eq!(ago(NOW, NOW - 300), "5 min ago");
        assert_eq!(ago(NOW, NOW - 7300), "2 h ago");
        assert_eq!(ago(NOW, NOW - 3 * 86_400), "3 days ago");
    }

    #[test]
    fn registry_and_job_files_are_read_and_bad_ones_skipped() {
        let d = std::env::temp_dir().join(format!("one-live-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("sessions")).unwrap();
        std::fs::create_dir_all(d.join("jobs/abc12345")).unwrap();
        let sid = "45f42afe-e89a-4ec2-87ff-592852ee9e3e";
        std::fs::write(d.join("sessions/1.json"), format!(r#"{{"sessionId":"{sid}","jobId":"abc12345","updatedAt":{ts},"status":"idle"}}"#, ts = 1_794_757_573_318u64)).unwrap();
        std::fs::write(d.join("sessions/2.json"), r#"{"sessionId":"../../x","jobId":"../.."}"#).unwrap();
        std::fs::write(d.join("sessions/3.json"), "not json").unwrap();
        std::fs::write(d.join("jobs/abc12345/state.json"), r#"{"state":"done","detail":"x"}"#).unwrap();
        let reg = read_registry(&d.join("sessions"));
        assert_eq!(reg.len(), 1);
        assert_eq!(reg[sid].updated, Some(1_794_757_573));
        let now = 1_794_757_573 + 86_400 * 2;
        let l = for_session(&d, "proj", sid, 1_794_757_500, now);
        assert!(!l.live);
        let _ = std::fs::remove_dir_all(&d);
    }
}
