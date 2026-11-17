//! Chat history, kept in Rust: the last 50 messages of the thread, voice and typed together.
//! Text is redacted before it is stored, capped in length, and saved atomically to
//! `history.json` in One's data folder so the thread survives a restart.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const MAX_MESSAGES: usize = 50;
pub const MAX_CHARS: usize = 6000;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ChatMsg {
    /// "user" or "assistant".
    pub role: String,
    pub text: String,
    /// Written while the user was asking about another session: dropped when that focus clears.
    #[serde(default)]
    pub focus: bool,
}

pub struct History {
    path: Option<PathBuf>,
    msgs: Vec<ChatMsg>,
}

impl History {
    /// Load `dir/history.json`. A missing or damaged file is an empty history, and only
    /// well-formed, bounded entries are kept.
    pub fn load(dir: Option<PathBuf>) -> Self {
        let path = dir.map(|d| d.join("history.json"));
        let loaded: Vec<ChatMsg> = path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default();
        let mut msgs: Vec<ChatMsg> = loaded.into_iter().filter(|m| m.role == "user" || m.role == "assistant").map(clip).collect();
        let extra = msgs.len().saturating_sub(MAX_MESSAGES);
        msgs.drain(..extra);
        History { path, msgs }
    }

    pub fn all(&self) -> Vec<ChatMsg> {
        self.msgs.clone()
    }

    /// Add a message (redacted, capped). Empty text is ignored. The oldest fall off past 50.
    #[cfg(test)]
    pub fn push(&mut self, role: &str, text: &str) {
        self.push_with(role, text, false);
    }

    /// Like `push`; `focus` marks a message of a turn that carried another session's extract.
    pub fn push_with(&mut self, role: &str, text: &str, focus: bool) {
        let role = if role == "user" { "user" } else { "assistant" };
        let text = crate::redact::redact(text);
        if text.trim().is_empty() {
            return;
        }
        self.msgs.push(clip(ChatMsg { role: role.into(), text, focus }));
        let extra = self.msgs.len().saturating_sub(MAX_MESSAGES);
        self.msgs.drain(..extra);
        self.save();
    }

    /// The focus was cleared: what was said while another session was in view goes.
    pub fn drop_focus_turns(&mut self) {
        let before = self.msgs.len();
        self.msgs.retain(|m| !m.focus);
        if self.msgs.len() != before {
            self.save();
        }
    }

    pub fn clear(&mut self) {
        self.msgs.clear();
        if let Some(p) = &self.path {
            let _ = std::fs::remove_file(p);
        }
    }

    fn save(&self) {
        let Some(p) = &self.path else { return };
        let Ok(json) = serde_json::to_string(&self.msgs) else { return };
        let tmp = p.with_extension("json.tmp");
        let write = || -> std::io::Result<()> {
            use std::io::Write;
            let mut opts = std::fs::OpenOptions::new();
            opts.write(true).create(true).truncate(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600); // private to the user
            opts.open(&tmp)?.write_all(json.as_bytes())?;
            std::fs::rename(&tmp, p)
        };
        if write().is_err() {
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

fn clip(mut m: ChatMsg) -> ChatMsg {
    if m.text.chars().count() > MAX_CHARS {
        m.text = m.text.chars().take(MAX_CHARS).collect();
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("one-desktop-history-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn keeps_the_last_50_messages_in_order() {
        let mut h = History::load(None);
        for i in 0..80 {
            h.push(if i % 2 == 0 { "user" } else { "assistant" }, &format!("m{i}"));
        }
        let all = h.all();
        assert_eq!(all.len(), MAX_MESSAGES);
        assert_eq!((all[0].text.as_str(), all[49].text.as_str()), ("m30", "m79"));
        assert_eq!(all[0].role, "user");
    }

    #[test]
    fn text_is_redacted_capped_and_empty_text_is_ignored() {
        let mut h = History::load(None);
        let secret = "p9o8i7u6y5".repeat(4);
        h.push("user", &format!("my key is {secret}"));
        h.push("assistant", &"w ".repeat(MAX_CHARS));
        h.push("user", "   ");
        let all = h.all();
        assert_eq!(all.len(), 2);
        assert!(!all[0].text.contains("p9o8i7u6"));
        assert!(all[1].text.chars().count() <= MAX_CHARS);
    }

    #[test]
    fn survives_a_restart_and_clear_removes_the_file() {
        let d = dir("persist");
        let mut h = History::load(Some(d.clone()));
        h.push("user", "hello");
        h.push("assistant", "hi there");
        let again = History::load(Some(d.clone()));
        assert_eq!(again.all(), h.all());
        assert!(!d.join("history.json.tmp").exists());
        h.clear();
        assert!(History::load(Some(d)).all().is_empty());
    }

    #[test]
    fn a_damaged_or_hand_edited_file_is_safe() {
        let d = dir("damaged");
        std::fs::write(d.join("history.json"), "{ not json").unwrap();
        assert!(History::load(Some(d.clone())).all().is_empty());
        let many: Vec<serde_json::Value> = (0..70).map(|i| serde_json::json!({"role": if i == 3 { "system" } else { "user" }, "text": format!("t{i}")})).collect();
        std::fs::write(d.join("history.json"), serde_json::to_string(&many).unwrap()).unwrap();
        let h = History::load(Some(d));
        assert_eq!(h.all().len(), MAX_MESSAGES);
        assert!(h.all().iter().all(|m| m.role == "user"), "an unknown role is dropped");
    }

    #[test]
    fn messages_written_while_another_session_was_in_view_are_dropped_when_the_focus_clears() {
        let d = dir("focus");
        let mut h = History::load(Some(d.clone()));
        h.push_with("user", "plain question", false);
        h.push_with("user", "about the other session", true);
        h.push_with("assistant", "it says X", true);
        h.push_with("assistant", "plain answer", false);
        h.drop_focus_turns();
        assert_eq!(h.all().iter().map(|m| m.text.as_str()).collect::<Vec<_>>(), ["plain question", "plain answer"]);
        assert_eq!(History::load(Some(d)).all().len(), 2, "also on disk");
    }
}
