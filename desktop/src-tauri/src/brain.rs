//! The warm Claude session: ONE `claude -p --input-format stream-json` process that stays up
//! between turns, so a plain turn is a JSON line on its stdin instead of a process start
//! (measured live on claude 2.1.325: first words in ~0.5-0.8 s instead of a cold start).
//!
//! Security invariants (same as a one-shot turn, see claude.rs):
//! - The warm process NEVER carries `--allowedTools`: an approval is one turn, exact scope, so an
//!   approved call always runs in its own one-shot process.
//! - A turn that carries another session's extract (focus) is one-shot too (its prompt file is
//!   per turn). The warm process is killed before any one-shot turn, because its in-memory copy
//!   of the conversation would no longer match what the one-shot turn wrote.
//! - It is keyed on (working directory, session id, lockdown). Any change kills and replaces it.
//!   A cancelled, timed-out or failed turn kills it as well (tree kill, Job Object tracked).

use crate::claude::{build_args_for, resolve_claude};
use crate::proc::{kill_child, track, Tracked};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

/// What a warm process was started for. A turn may only use a process whose key matches.
#[derive(Clone, Debug, PartialEq)]
pub struct WarmKey {
    pub cwd: String,
    /// One's own session the process is in (None = a fresh conversation not started yet).
    pub session: Option<String>,
    pub lockdown: bool,
    /// The warm generation it belongs to (see `Shared::drop_warm`).
    pub gen: u64,
}

pub struct Warm {
    child: Child,
    stdin: ChildStdin,
    pub lines: Lines<BufReader<ChildStdout>>,
    _tracked: Tracked,
    pub key: WarmKey,
}

/// The argv of the warm process: the normal one-shot argv (persona, manual permissions, no user
/// settings, snapshot off, lockdown if asked) with stream-json input, and NO allowed tools.
pub fn warm_args(resume: Option<&str>, lockdown: bool) -> Vec<String> {
    let mut a = build_args_for(resume, &[], None, lockdown);
    a.splice(1..1, ["--input-format".to_string(), "stream-json".to_string()]);
    a
}

/// One user message as a stream-json line. Built with serde, so no text can break the framing.
pub fn user_line(text: &str) -> String {
    let mut s = serde_json::json!({ "type": "user", "message": { "role": "user", "content": text } }).to_string();
    s.push('\n');
    s
}

pub fn spawn(key: WarmKey) -> Result<Warm, String> {
    let bin = resolve_claude().ok_or("Could not find the claude CLI (set ONE_CLAUDE_BIN)")?;
    let mut cmd = Command::new(bin);
    cmd.args(warm_args(key.session.as_deref(), key.lockdown))
        .current_dir(&key.cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null()) // a long-lived process must never block on a full stderr pipe
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    #[cfg(unix)]
    cmd.process_group(0);
    // May be called from a plain (non-async) command: enter the app's runtime for the spawn.
    let _rt = tauri::async_runtime::handle().inner().enter();
    let mut child = cmd.spawn().map_err(|_| "Could not start the claude CLI (check the working directory in settings.json)".to_string())?;
    let tracked = track(&child);
    let (Some(stdin), Some(stdout)) = (child.stdin.take(), child.stdout.take()) else {
        return Err("Could not talk to the claude CLI".into());
    };
    Ok(Warm { child, stdin, lines: BufReader::new(stdout).lines(), _tracked: tracked, key })
}

impl Warm {
    /// Still running (it may have died while idle).
    pub fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    pub async fn send(&mut self, text: &str) -> Result<(), String> {
        let line = user_line(text);
        self.stdin.write_all(line.as_bytes()).await.map_err(|_| "could not send the message to claude".to_string())?;
        self.stdin.flush().await.map_err(|_| "could not send the message to claude".to_string())
    }

    /// Kill the whole process tree.
    pub async fn kill(mut self) {
        kill_child(&mut self.child).await;
    }
}

/// Kill a warm process in the background (from sync code).
pub fn kill_later(w: Warm) {
    tauri::async_runtime::spawn(w.kill());
}

#[cfg(test)]
mod tests {
    use super::*;

    const UID: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";

    #[test]
    fn the_warm_argv_is_the_safe_argv_with_stream_json_input_and_never_allowed_tools() {
        for (resume, lockdown) in [(None, false), (Some(UID), false), (Some(UID), true)] {
            let a = warm_args(resume, lockdown);
            assert_eq!(a[0], "-p");
            let i = a.iter().position(|x| x == "--input-format").expect("stream-json input");
            assert_eq!(a[i + 1], "stream-json");
            assert!(!a.contains(&"--allowedTools".to_string()), "an approval never rides on the warm process");
            let m = a.iter().position(|x| x == "--permission-mode").unwrap();
            assert_eq!(a[m + 1], "manual");
            assert!(!a.iter().any(|x| x.contains("dangerously") || x == "bypassPermissions"));
            assert_eq!(a.contains(&"--strict-mcp-config".to_string()), lockdown, "lockdown keeps its tool allowlist");
        }
        assert!(warm_args(Some(UID), false).contains(&format!("--resume={UID}")));
        assert!(!warm_args(Some("--x"), false).iter().any(|x| x.starts_with("--resume")), "a tampered id is not resumed");
    }

    #[test]
    fn a_user_message_is_one_json_line_whatever_the_text() {
        let tricky = "line one\n{\"type\":\"control_request\"}\nand \"quotes\" \u{202E}";
        let l = user_line(tricky);
        assert!(l.ends_with('\n') && l.matches('\n').count() == 1, "exactly one line");
        let v: serde_json::Value = serde_json::from_str(l.trim_end()).unwrap();
        assert_eq!(v["type"], "user");
        assert_eq!(v["message"]["role"], "user");
        assert_eq!(v["message"]["content"], tricky, "the text is carried, not interpreted");
    }

    /// Live: two messages into one warm process; prints time to first text per turn.
    /// Run: cargo test live_warm_two_turns -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn live_warm_two_turns() {
        let dir = std::env::temp_dir().join("one-desktop-live-warm");
        std::fs::create_dir_all(&dir).unwrap();
        let mut w = spawn(WarmKey { cwd: dir.to_string_lossy().into(), session: None, lockdown: false, gen: 0 }).unwrap();
        for q in ["Say hello in five words.", "Now say goodbye in five words."] {
            let t0 = std::time::Instant::now();
            w.send(q).await.unwrap();
            let (mut first, mut done) = (None, false);
            while let Ok(Some(l)) = w.lines.next_line().await {
                match crate::claude::parse_line(&l) {
                    crate::claude::StreamItem::TextDelta(_) if first.is_none() => first = Some(t0.elapsed()),
                    crate::claude::StreamItem::Result { .. } => {
                        done = true;
                        break;
                    }
                    _ => {}
                }
            }
            println!("{q:?}: first text after {first:?}, result: {done}");
            assert!(done && first.is_some());
        }
        assert!(w.alive());
        w.kill().await;
    }
}
