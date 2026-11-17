//! Claude Code brain: argv builder, stream-json parser, approval scoping.
//! Permission policy: `--permission-mode manual` is ALWAYS passed. Nothing here
//! ever bypasses permissions; approvals are one-turn, exact-scope `--allowedTools`.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub const PERSONA: &str = "You are One, the voice of the team's AI assistant. Your replies are spoken aloud. \
Talk like a helpful colleague, in plain words. Answer first. \
Spoken replies: two to five short sentences. Typed chat replies may be a little longer. \
Never mention PR numbers, file paths, commit ids or session jargon unless the user asks. \
End with one clear next step or question when something is needed from the user. \
Reply in the language the user spoke (English or Egyptian Arabic). \
Never read secrets, tokens, API keys or passwords aloud; say that you are withholding them. \
Summarise long output instead of reading it out. Light formatting is fine in chat (bold, short lists, short code blocks); it is spoken without the symbols. \
If an action needs permission, say what you need and wait. \
The team is not technical. When asked what's new or for the latest updates, answer in plain, short English: \
what changed, what's next, what's blocked, and who needs to do what. No jargon, no PR numbers or code terms \
unless asked. Refer to sessions by their tab title. SPOKEN STYLE (your words are read aloud by a natural voice): talk the way a friendly colleague talks. Short sentences. Contractions (I'm, it's, you'll). Start with the answer, not a preamble. A light filler now and then is fine (\"okay, so\", \"right\"), never more than one per reply. No markdown, lists, headings or tables in a spoken reply: say \"first ... then ...\" instead. Numbers the way people say them (\"about two thousand\", \"half past three\"), never ids or long digits. No stage directions, sound effects or text in brackets. In Arabic, speak natural Egyptian colloquial (عامية مصرية), not formal Arabic.";

/// Tools that write, run commands or reach the network. While the user is asking about another
/// session, One only answers: none of these run (and no MCP server is loaded).
/// The only tools available while the user asks about another session.
pub const FOCUS_TOOLS: &str = "Read,Glob,Grep";
pub const LOCKDOWN_TOOLS: [&str; 11] =
    ["Bash", "PowerShell", "Edit", "Write", "MultiEdit", "NotebookEdit", "WebFetch", "WebSearch", "Task", "Agent", "Skill"];

/// Build the `claude` argv. The prompt is NOT in argv: it goes over stdin so it
/// can never be parsed as a flag and does not show in process listings.
///
/// A resume id must be a strict UUID and is passed as ONE argument, `--resume=<uuid>`, so a
/// tampered value can never be read as a separate flag. Anything else is not resumed at all.
/// `resume` is always One's OWN session: One never resumes or forks another session.
///
/// `system_file` (the persona plus the extract of the session the user asked about, see
/// `tabs::focus_context`) goes in with `--append-system-prompt-file`, so the other session's text
/// never appears in argv or process-creation logs. `lockdown` (set while that extract is in use)
/// denies the write, command and network tools and loads no MCP servers.
pub fn build_args_for(resume: Option<&str>, allowed: &[String], system_file: Option<&Path>, lockdown: bool) -> Vec<String> {
    let mut a: Vec<String> = [
        "-p",
        "--output-format",
        "stream-json",
        "--verbose",
        "--include-partial-messages",
        "--permission-mode",
        "manual",
        "--permission-prompts",
        "none",
        // M2: do not load the user's hooks, plugins or global settings into One's turns.
        "--setting-sources",
        "project,local",
        // The CLI otherwise records the first turn's system prompt and replays it on every later
        // turn and resume, so a session extract would stick (or later ones be ignored). Checked
        // live on claude 2.1.325: `off` also overrides a prompt that was already recorded.
        "--system-prompt-snapshot",
        "off",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    match system_file {
        Some(f) => {
            a.push("--append-system-prompt-file".into());
            a.push(f.to_string_lossy().into_owned());
        }
        None => {
            a.push("--append-system-prompt".into());
            a.push(PERSONA.to_string());
        }
    }
    if let Some(id) = resume.filter(|id| crate::tabs::valid_session_id(id)) {
        a.push(format!("--resume={id}"));
    }
    if !allowed.is_empty() {
        a.push("--allowedTools".into());
        a.extend(allowed.iter().cloned());
    }
    if lockdown {
        // An allowlist: only these three exist for the model (checked live on claude 2.1.325), so a
        // tool this list never heard of (SendMessage, ScheduleWakeup, ...) is off too. The blocklist
        // stays as a second line.
        a.push("--tools".into());
        a.push(FOCUS_TOOLS.into());
        a.push("--strict-mcp-config".into());
        a.push("--disallowedTools".into());
        a.extend(LOCKDOWN_TOOLS.iter().map(|t| t.to_string()));
    }
    a
}

/// Tests: the plain argv (persona inline, no lockdown). The third argument is unused.
#[cfg(test)]
pub fn build_args(resume: Option<&str>, allowed_tool: Option<&str>, _unused: Option<&str>) -> Vec<String> {
    let allowed: Vec<String> = allowed_tool.map(String::from).into_iter().collect();
    build_args_for(resume, &allowed, None, false)
}

/// The system-prompt file for one turn: private to the user, removed when dropped (also on
/// cancel, timeout and error paths).
pub struct PromptFile(PathBuf);

impl PromptFile {
    pub fn create(dir: &Path, content: &str) -> std::io::Result<PromptFile> {
        use std::io::Write;
        std::fs::create_dir_all(dir)?;
        let path = dir.join(format!("focus-{}.txt", crate::tabs::new_nonce()));
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        // Windows: the app data folder is under the user's profile; its inherited ACL is user-only.
        let mut f = opts.open(&path)?;
        let file = PromptFile(path);
        f.write_all(content.as_bytes())?;
        Ok(file)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for PromptFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// An explicit ONE_CLAUDE_BIN must be an absolute path to an existing file, and on Windows an
/// `.exe` (a `.cmd` would go through cmd.exe, which re-parses its arguments).
pub fn valid_override(p: &Path, windows: bool) -> bool {
    p.is_absolute() && p.is_file() && (!windows || p.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe")))
}

/// Find a real `claude` binary without going through cmd.exe (which would re-parse user text).
/// Override with ONE_CLAUDE_BIN (see `valid_override`; an invalid value is ignored). On Windows
/// only `claude.exe` counts: an extensionless `claude` there is an npm `#!/bin/sh` shim that
/// cannot be spawned, so it is skipped. Relative PATH entries are skipped. `~/.local/bin` is last.
pub fn resolve_claude() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("ONE_CLAUDE_BIN").map(PathBuf::from) {
        if valid_override(&p, cfg!(windows)) {
            return Some(p);
        }
        eprintln!("ONE_CLAUDE_BIN is ignored: it must be an absolute path to an existing claude.exe");
    }
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
        dirs.push(PathBuf::from(home).join(".local").join("bin"));
    }
    find_claude(&dirs, cfg!(windows))
}

/// First real claude binary in `dirs` (see `resolve_claude`). Relative entries are skipped.
pub fn find_claude(dirs: &[PathBuf], windows: bool) -> Option<PathBuf> {
    let name = if windows { "claude.exe" } else { "claude" };
    for dir in dirs.iter().filter(|d| d.is_absolute()) {
        let p = dir.join(name);
        if p.is_file() {
            return Some(p);
        }
        // npm global shim: <dir>/claude.cmd -> <dir>/node_modules/@anthropic-ai/claude-code/bin/claude.exe
        let npm = dir.join("node_modules/@anthropic-ai/claude-code/bin/claude.exe");
        if windows && dir.join("claude.cmd").is_file() && npm.is_file() {
            return Some(npm);
        }
    }
    None
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Denial {
    pub tool_name: String,
    pub tool_use_id: String,
    pub tool_input: Value,
}

/// Card data sent to the UI. For Bash, `command` is the FULL command, exactly what would run.
#[derive(Debug, Clone, Serialize)]
pub struct DenialCard {
    pub tool_use_id: String,
    pub tool_name: String,
    pub summary: String,
    pub command: Option<String>,
    /// The command contains shell operators (`; & | > < $( \``): show a red warning.
    pub warning: bool,
    pub approvable: bool,
    /// The request as one plain sentence ("One wants to search for ..."). Plain text.
    pub sentence: String,
    /// Why there is no Approve button.
    pub reason: Option<String>,
}

/// Longest command that can be approved (H1).
pub const MAX_APPROVE_CMD: usize = 500;

fn is_hidden_format_char(c: char) -> bool {
    matches!(c,
        '\u{00AD}' | '\u{061C}' | '\u{180E}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}'
        | '\u{2060}'..='\u{206F}' | '\u{FEFF}' | '\u{FFF9}'..='\u{FFFB}')
}

/// H1: an approvable command must be plain printable ASCII, so what the card shows is
/// byte for byte what runs. Anything else gets a Deny-only card with the reason.
pub fn approval_check(cmd: &str) -> Result<(), String> {
    if cmd.is_empty() {
        return Err("The command is empty".into());
    }
    if cmd.chars().count() > MAX_APPROVE_CMD {
        return Err(format!("The command is longer than {MAX_APPROVE_CMD} characters"));
    }
    for c in cmd.chars() {
        if c == ' ' {
            continue;
        }
        if c.is_control() {
            return Err("The command contains control characters (tab, newline or similar)".into());
        }
        if is_hidden_format_char(c) {
            return Err("The command contains hidden or direction-changing characters".into());
        }
        if !c.is_ascii() {
            return Err("The command contains non-ASCII characters".into());
        }
    }
    Ok(())
}

/// True when the command contains `; & | > < $(` or a backtick.
pub fn has_shell_operators(cmd: &str) -> bool {
    cmd.contains([';', '&', '|', '>', '<', '`']) || cmd.contains("$(")
}

#[derive(Debug, PartialEq)]
pub enum StreamItem {
    Init { session_id: String },
    TextDelta(String),
    Result { session_id: Option<String>, text: String, is_error: bool, errors: Vec<String>, denials: Vec<Denial> },
    /// A new assistant message began (separates sentences of different messages).
    NewMessage,
    Other,
}

/// Parse one stdout line of `--output-format stream-json`.
pub fn parse_line(line: &str) -> StreamItem {
    let Ok(v) = serde_json::from_str::<Value>(line) else { return StreamItem::Other };
    match v.get("type").and_then(Value::as_str) {
        Some("system") if v.get("subtype").and_then(Value::as_str) == Some("init") => {
            match v.get("session_id").and_then(Value::as_str) {
                Some(s) => StreamItem::Init { session_id: s.to_string() },
                None => StreamItem::Other,
            }
        }
        Some("stream_event") => parse_stream_event(&v),
        Some("result") => StreamItem::Result {
            session_id: v.get("session_id").and_then(Value::as_str).map(String::from),
            text: v.get("result").and_then(Value::as_str).unwrap_or("").to_string(),
            is_error: v.get("is_error").and_then(Value::as_bool).unwrap_or(false),
            errors: v
                .get("errors")
                .and_then(Value::as_array)
                .map(|a| a.iter().map(|e| e.as_str().map(String::from).unwrap_or_else(|| e.to_string())).collect())
                .unwrap_or_default(),
            denials: v
                .get("permission_denials")
                .and_then(|d| serde_json::from_value(d.clone()).ok())
                .unwrap_or_default(),
        },
        _ => StreamItem::Other,
    }
}

fn parse_stream_event(v: &Value) -> StreamItem {
    // Ignore sub-agent chatter: only the top-level conversation is spoken.
    if v.get("parent_tool_use_id").is_some_and(|p| !p.is_null()) {
        return StreamItem::Other;
    }
    let ev = &v["event"];
    if ev["type"] == "message_start" {
        return StreamItem::NewMessage;
    }
    if ev["type"] == "content_block_delta" && ev["delta"]["type"] == "text_delta" {
        if let Some(t) = ev["delta"]["text"].as_str() {
            return StreamItem::TextDelta(t.to_string());
        }
    }
    StreamItem::Other
}

fn str_field<'a>(input: &'a Value, key: &str) -> Option<&'a str> {
    input.get(key).and_then(Value::as_str)
}

/// Exact-scope `--allowedTools` entry for one denial, or an error if it cannot be
/// scoped without widening. Fails closed: unknown tools are not approvable.
pub fn scope_for(tool: &str, input: &Value) -> Result<String, String> {
    // MVP: Bash only. Path-scoped rules (Write/Edit) were probed on Windows and drive-letter
    // paths did not match any rule syntax we tried, so they stay unapprovable (fail closed).
    let (arg, kind) = match tool {
        "Bash" => (str_field(input, "command"), "command"),
        _ => return Err(format!("{tool} cannot be approved from here")),
    };
    let arg = arg.ok_or_else(|| format!("{tool} has no {kind} to scope"))?;
    approval_check(arg)?;
    // These characters are wildcards/syntax in permission rules and would widen or break the scope.
    if arg.is_empty() || arg.contains(['*', '(', ')', '\n', '\r']) {
        return Err(format!("{tool} {kind} cannot be scoped exactly"));
    }
    Ok(format!("{tool}({arg})"))
}

/// Why a finished claude run counts as failed, or None when it worked (CR-H1).
/// Failed: an `is_error` result, no result line at all, or a non-zero exit with no output.
/// The text is short, redacted and safe to show in the UI.
pub fn failure_reason(is_error: bool, errors: &[String], got_result: bool, exit_ok: bool, exit_code: Option<i32>, has_output: bool) -> Option<String> {
    let raw = if is_error {
        errors.iter().find(|e| !e.trim().is_empty()).cloned().unwrap_or_else(|| "Claude reported an error".to_string())
    } else if !got_result {
        match (exit_ok, exit_code) {
            (false, Some(c)) => format!("claude stopped with exit code {c}"),
            _ => "claude ended without an answer".to_string(),
        }
    } else if !exit_ok && !has_output {
        format!("claude stopped with exit code {}", exit_code.map_or("?".to_string(), |c| c.to_string()))
    } else {
        return None;
    };
    Some(redact_then_truncate(raw.trim(), 160))
}

/// Redact FIRST, then cut: a secret that straddles the cut must not leave a fragment
/// (a 40-char token cut to 30 would fall under the redactor's length threshold).
pub fn redact_then_truncate(s: &str, n: usize) -> String {
    truncate(&crate::redact::redact(s), n)
}

pub fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}...", s.chars().take(n).collect::<String>())
    }
}

pub fn card_for(d: &Denial) -> DenialCard {
    let summary = ["command", "file_path", "url", "pattern"]
        .iter()
        .find_map(|k| str_field(&d.tool_input, k))
        .map(|s| truncate(s, 160))
        .unwrap_or_else(|| "(no details)".to_string());
    let command = (d.tool_name == "Bash").then(|| str_field(&d.tool_input, "command").map(String::from)).flatten();
    let scope = scope_for(&d.tool_name, &d.tool_input);
    DenialCard {
        tool_use_id: d.tool_use_id.clone(),
        tool_name: d.tool_name.clone(),
        summary: command.clone().unwrap_or(summary),
        warning: command.as_deref().is_some_and(has_shell_operators),
        command,
        approvable: scope.is_ok(),
        sentence: plain_words(&d.tool_name, &d.tool_input),
        reason: scope.err().map(|e| if d.tool_name == "Bash" { e } else { "One can't be given this permission from here yet. Deny it.".to_string() }),
    }
}

fn shown(v: &Value, key: &str, n: usize) -> Option<String> {
    let raw = str_field(v, key)?;
    // bidi and zero-width characters could make a path look like something else: drop them; a quote
    // in the text must not fake the sentence structure
    let clean: String = redact_then_truncate(raw, n).chars().filter(|c| !is_hidden_format_char(*c)).map(|c| if c.is_control() { ' ' } else if c == '"' { '\'' } else { c }).collect();
    Some(clean.split_whitespace().collect::<Vec<_>>().join(" ")).filter(|s| !s.is_empty())
}

/// One tool request as a sentence anybody can read: "One wants to search for "T-2041" in docs.
/// Allow?". Text from the request is redacted, cleaned and cut; it is shown as text only.
pub fn plain_words(tool: &str, input: &Value) -> String {
    let at = |key: &str| shown(input, key, 80);
    let what = match tool {
        "Grep" => match (at("pattern"), at("path")) {
            (Some(p), Some(d)) => format!("search for \"{p}\" in {d}"),
            (Some(p), None) => format!("search for \"{p}\""),
            _ => "search through files".to_string(),
        },
        "Glob" => at("pattern").map_or("look for files".to_string(), |p| format!("look for files matching {p}")),
        "Read" => at("file_path").map_or("read a file".to_string(), |p| format!("read the file {p}")),
        "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => at("file_path").map_or("change a file".to_string(), |p| format!("change the file {p}")),
        "Bash" => shown(input, "command", 120).map_or("run a command".to_string(), |c| format!("run this command: {c}")),
        "WebFetch" => at("url").map_or("open a web page".to_string(), |u| format!("open the web page {u}")),
        "WebSearch" => at("query").map_or("search the web".to_string(), |q| format!("search the web for \"{q}\"")),
        other => format!("use the {} tool", truncate(&other.chars().filter(|c| c.is_alphanumeric() || *c == '_').collect::<String>(), 40)),
    };
    format!("One wants to {what}. Allow?")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn l2_a_secret_straddling_the_cut_leaves_no_fragment() {
        // Built at runtime so no secret-looking literal sits in the repo.
        let secret = "a1b2c3d4e5".repeat(4); // 40 chars: the cut at 160 would leave 30 (< the 32 threshold)
        let text = format!("{} {}", "w".repeat(129), secret);
        let out = redact_then_truncate(&text, 160);
        assert!(!out.contains("a1b2c3d4"), "a fragment of the secret survived: {out}");
        // the old order (truncate, then redact) leaks, which is the bug this guards
        let old = crate::redact::redact(&truncate(&text, 160));
        assert!(old.contains("a1b2c3d4"), "the fixture no longer straddles the threshold");
        // failure_reason uses the safe order for the text that reaches the UI
        let reason = failure_reason(true, &[text], true, true, Some(0), false).unwrap();
        assert!(!reason.contains("a1b2c3d4"));
    }

    #[test]
    fn persona_with_the_plain_updates_rule_is_passed_via_append_system_prompt() {
        for args in [build_args(None, None, None), build_args(Some(UID), Some("Bash(echo hello)"), None)] {
            let i = args.iter().position(|a| a == "--append-system-prompt").expect("flag present");
            let text = &args[i + 1];
            assert_eq!(text, PERSONA);
            assert!(text.contains("The team is not technical"));
            assert!(text.contains("Refer to sessions by their tab title"));
            for rule in ["SPOKEN STYLE", "Short sentences", "Contractions", "No markdown", "Numbers the way people say them", "Egyptian colloquial"] {
                assert!(text.contains(rule), "spoken-style rule missing: {rule}");
            }
        }
    }

    const UID: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    const FORBIDDEN: [&str; 5] = ["--dangerously-skip-permissions", "bypassPermissions", "acceptEdits", "auto", "dontAsk"];

    #[test]
    fn argv_never_contains_bypass_flags() {
        for args in [
            build_args(None, None, None),
            build_args(Some(UID), None, None),
            build_args(Some(UID), Some("Bash(echo hello)"), None),
        ] {
            for a in &args {
                assert!(!FORBIDDEN.contains(&a.as_str()), "forbidden arg {a}");
                assert!(!a.contains("dangerously"), "forbidden arg {a}");
            }
            let i = args.iter().position(|a| a == "--permission-mode").unwrap();
            assert_eq!(args[i + 1], "manual");
        }
    }

    #[test]
    fn argv_does_not_load_user_settings() {
        for args in [build_args(None, None, None), build_args(Some(UID), Some("Bash(echo hi)"), None)] {
            let i = args.iter().position(|a| a == "--setting-sources").expect("flag present");
            let sources: Vec<&str> = args[i + 1].split(',').collect();
            assert_eq!(sources, ["project", "local"]);
            assert!(!sources.contains(&"user"));
        }
    }

    #[test]
    fn argv_resume_and_allowed_tools() {
        let a = build_args(Some(UID), Some("Bash(echo hello)"), None);
        assert!(a.contains(&format!("--resume={UID}")), "one single argument");
        assert!(!a.contains(&"--resume".to_string()) && !a.contains(&"--fork-session".to_string()));
        let t = a.iter().position(|x| x == "--allowedTools").unwrap();
        assert_eq!(a[t + 1], "Bash(echo hello)");
        assert!(!build_args(None, None, None).contains(&"--allowedTools".to_string()));
    }

    #[test]
    fn the_focus_goes_in_a_private_file_not_argv_and_the_file_is_deleted_after_the_turn() {
        let dir = std::env::temp_dir().join(format!("one-desktop-promptfile-{}", std::process::id()));
        let f = PromptFile::create(&dir, "persona and extract").unwrap();
        let path = f.path().to_path_buf();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "persona and extract");
        let a = build_args_for(Some(UID), &[], Some(&path), true);
        assert!(!a.contains(&"--fork-session".to_string()) && !a.contains(&"--append-system-prompt".to_string()), "the extract never rides inline in argv");
        let i = a.iter().position(|x| x == "--append-system-prompt-file").unwrap();
        assert_eq!(a[i + 1], path.to_string_lossy());
        assert_eq!(a.iter().filter(|x| x.starts_with("--resume")).count(), 1);
        drop(f);
        assert!(!path.exists(), "removed when the turn ends");
    }

    #[test]
    fn every_turn_turns_the_system_prompt_snapshot_off() {
        for a in [build_args_for(None, &[], None, false), build_args_for(Some(UID), &["Bash(echo hi)".to_string()], None, false), build_args_for(Some(UID), &[], Some(Path::new("x.txt")), true)] {
            let i = a.iter().position(|x| x == "--system-prompt-snapshot").expect("flag present on every turn");
            assert_eq!(a[i + 1], "off");
        }
    }

    #[test]
    fn while_a_session_is_asked_about_write_command_and_network_tools_and_mcp_are_off() {
        let on = build_args_for(None, &[], None, true);
        let i = on.iter().position(|x| x == "--disallowedTools").unwrap();
        for t in ["Bash", "PowerShell", "Edit", "Write", "WebFetch", "WebSearch", "NotebookEdit", "MultiEdit", "Task", "Agent", "Skill"] {
            assert!(on[i + 1..].contains(&t.to_string()), "{t} must be denied");
        }
        assert!(on.contains(&"--strict-mcp-config".to_string()), "no MCP servers are loaded");
        let t = on.iter().position(|x| x == "--tools").expect("allowlist present");
        assert_eq!(on[t + 1], "Read,Glob,Grep", "only read-only tools exist on a focus turn");
        let off = build_args_for(None, &[], None, false);
        assert!(!off.contains(&"--tools".to_string()) && !off.contains(&"--disallowedTools".to_string()) && !off.contains(&"--strict-mcp-config".to_string()));
        assert_eq!(off[off.iter().position(|x| x == "--append-system-prompt").unwrap() + 1], PERSONA);
    }

    /// A directory removed when dropped, also when a test assertion panics.
    struct TempDir(PathBuf);
    impl TempDir {
        fn new(base: &Path, name: &str) -> TempDir {
            let d = base.join(format!("{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&d);
            std::fs::create_dir_all(&d).unwrap();
            TempDir(d)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn the_claude_override_must_be_an_absolute_existing_exe_and_relative_path_entries_are_skipped() {
        let d = TempDir::new(&std::env::temp_dir(), "one-desktop-override");
        let (exe, cmd) = (d.0.join("claude.exe"), d.0.join("claude.cmd"));
        std::fs::write(&exe, "MZ").unwrap();
        std::fs::write(&cmd, "@echo off").unwrap();
        assert!(valid_override(&exe, true));
        assert!(!valid_override(&cmd, true), ".cmd goes through cmd.exe");
        assert!(valid_override(&cmd, false), "elsewhere the extension does not matter");
        assert!(!valid_override(Path::new("claude.exe"), true), "relative");
        assert!(!valid_override(&d.0.join("missing.exe"), true));
        assert_eq!(find_claude(&[PathBuf::new(), d.0.clone()], true), Some(exe));
        // a RELATIVE entry that really holds a claude.exe is still skipped (it would resolve against the app's cwd)
        let here = TempDir::new(&std::env::current_dir().unwrap(), "one-test-rel");
        std::fs::write(here.0.join("claude.exe"), "MZ").unwrap();
        let rel = PathBuf::from(here.0.file_name().unwrap());
        assert!(rel.is_relative() && rel.join("claude.exe").is_file());
        assert_eq!(find_claude(&[rel], true), None);
    }

    #[test]
    fn a_tampered_session_id_can_never_become_a_flag() {
        for bad in ["--dangerously-skip-permissions", "-p", "", "abc-123", "../x", "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa --x", "--------------------------------------"] {
            let a = build_args(Some(bad), None, None);
            assert!(!a.iter().any(|x| x.starts_with("--resume") || x == "--fork-session" || x.contains("dangerously")), "{bad:?} leaked into argv: {a:?}");
        }
    }

    #[test]
    fn an_extensionless_shim_on_windows_is_skipped_for_the_real_exe_later_on_the_path() {
        let root = std::env::temp_dir().join(format!("one-desktop-resolve-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (npm, local) = (root.join("npm"), root.join("local"));
        std::fs::create_dir_all(&npm).unwrap();
        std::fs::create_dir_all(&local).unwrap();
        std::fs::write(npm.join("claude"), "#!/bin/sh\nexec node x \"$@\"\n").unwrap(); // the npm shim
        std::fs::write(local.join("claude.exe"), "MZ").unwrap();
        let dirs = [npm.clone(), local.clone()];
        assert_eq!(find_claude(&dirs, true), Some(local.join("claude.exe")), "the shim must not win on Windows");
        // only a shim: nothing usable on Windows
        assert_eq!(find_claude(&[npm.clone()], true), None);
        // elsewhere the extensionless file IS the binary
        assert_eq!(find_claude(&dirs, false), Some(npm.join("claude")));
        assert_eq!(find_claude(&[], true), None);
    }

    #[test]
    fn parses_stream() {
        let init = r#"{"type":"system","subtype":"init","session_id":"s1","permissionMode":"default"}"#;
        assert_eq!(parse_line(init), StreamItem::Init { session_id: "s1".into() });
        let delta = r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"Hi"}}}"#;
        assert_eq!(parse_line(delta), StreamItem::TextDelta("Hi".into()));
        let sub = r#"{"type":"stream_event","parent_tool_use_id":"x","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"no"}}}"#;
        assert_eq!(parse_line(sub), StreamItem::Other);
        assert_eq!(parse_line("garbage"), StreamItem::Other);
    }

    #[test]
    fn cr_h1_parses_an_is_error_result_with_errors_and_the_echoed_session_id() {
        // the exact shape `claude --resume <unknown id>` prints (checked live in the code review)
        let r = r#"{"type":"result","subtype":"error_during_execution","is_error":true,"session_id":"bad-id","errors":["No conversation found with session ID: bad-id"]}"#;
        match parse_line(r) {
            StreamItem::Result { session_id, text, is_error, errors, denials } => {
                assert!(is_error);
                assert_eq!(session_id.as_deref(), Some("bad-id"));
                assert_eq!(text, "");
                assert_eq!(errors, vec!["No conversation found with session ID: bad-id".to_string()]);
                assert!(denials.is_empty());
            }
            other => panic!("unexpected {other:?}"),
        }
        // object-shaped errors do not break the parse
        assert!(matches!(parse_line(r#"{"type":"result","is_error":true,"errors":[{"code":1}]}"#), StreamItem::Result { .. }));
    }

    #[test]
    fn cr_h1_failure_reasons() {
        let e = vec!["No conversation found".to_string()];
        assert_eq!(failure_reason(true, &e, true, false, Some(1), false).as_deref(), Some("No conversation found"));
        assert!(failure_reason(true, &[], true, true, Some(0), false).unwrap().contains("error"));
        assert!(failure_reason(false, &[], false, false, Some(2), false).unwrap().contains("exit code 2"));
        assert!(failure_reason(false, &[], false, true, Some(0), false).unwrap().contains("without an answer"));
        assert!(failure_reason(false, &[], true, false, Some(1), false).unwrap().contains("exit code 1"));
        // a good result, or a non-zero exit after real output, is not a failure
        assert_eq!(failure_reason(false, &[], true, true, Some(0), true), None);
        assert_eq!(failure_reason(false, &[], true, false, Some(1), true), None);
        // reasons are short and redacted
        let long = format!("token = abc {}", "x".repeat(500));
        let r = failure_reason(true, &[long], true, false, Some(1), false).unwrap();
        assert!(r.chars().count() <= 163 && !r.contains("abc"));
    }

    #[test]
    fn l2_message_start_is_its_own_item() {
        let s = r#"{"type":"stream_event","event":{"type":"message_start","message":{}}}"#;
        assert_eq!(parse_line(s), StreamItem::NewMessage);
        let sub = r#"{"type":"stream_event","parent_tool_use_id":"x","event":{"type":"message_start"}}"#;
        assert_eq!(parse_line(sub), StreamItem::Other);
    }

    #[test]
    fn parses_result_denials() {
        let r = r#"{"type":"result","session_id":"s2","result":"ok","is_error":false,
          "permission_denials":[{"tool_name":"Bash","tool_use_id":"t1","tool_input":{"command":"echo hello"}}]}"#;
        match parse_line(r) {
            StreamItem::Result { session_id, denials, .. } => {
                assert_eq!(session_id.as_deref(), Some("s2"));
                assert_eq!(denials.len(), 1);
                assert_eq!(scope_for(&denials[0].tool_name, &denials[0].tool_input).unwrap(), "Bash(echo hello)");
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn scope_fails_closed() {
        assert!(scope_for("Bash", &json!({"command":"rm -rf *"})).is_err());
        assert!(scope_for("Bash", &json!({"command":"echo (x)"})).is_err());
        assert!(scope_for("Bash", &json!({})).is_err());
        assert!(scope_for("mcp__x__y", &json!({})).is_err());
        assert!(scope_for("Write", &json!({"file_path":"C:/a/b.txt"})).is_err());
        assert_eq!(scope_for("Bash", &json!({"command":"echo hello","description":"x"})).unwrap(), "Bash(echo hello)");
    }

    fn bash(cmd: &str) -> Denial {
        Denial { tool_name: "Bash".into(), tool_use_id: "t".into(), tool_input: json!({ "command": cmd }) }
    }

    #[test]
    fn card_shows_the_full_command_untruncated() {
        let cmd = format!("echo {}", "x".repeat(400));
        let c = card_for(&bash(&cmd));
        assert_eq!(c.command.as_deref(), Some(cmd.as_str()));
        assert_eq!(c.summary, cmd);
        assert!(c.approvable && c.reason.is_none() && !c.warning);
        // exactly what is approved is exactly what is shown
        assert_eq!(scope_for("Bash", &bash(&cmd).tool_input).unwrap(), format!("Bash({cmd})"));
    }

    #[test]
    fn h1_rejects_control_chars_including_tab() {
        for cmd in ["echo a\tb", "echo a\u{1b}[31m", "echo a\u{7f}b", "echo a\u{0}b", "echo a\r"] {
            let c = card_for(&bash(cmd));
            assert!(!c.approvable, "{cmd:?}");
            assert!(c.reason.is_some());
            assert!(scope_for("Bash", &bash(cmd).tool_input).is_err());
        }
    }

    #[test]
    fn h1_rejects_hidden_format_and_bidi_chars() {
        for ch in ['\u{200B}', '\u{200E}', '\u{200F}', '\u{202A}', '\u{202E}', '\u{2066}', '\u{2069}', '\u{FEFF}', '\u{00AD}'] {
            let cmd = format!("echo a{ch}b");
            let c = card_for(&bash(&cmd));
            assert!(!c.approvable, "U+{:04X}", ch as u32);
            assert!(c.reason.unwrap().contains("hidden"));
        }
    }

    #[test]
    fn h1_rejects_any_non_ascii() {
        for cmd in ["echo caf\u{e9}", "echo \u{627}\u{644}", "echo \u{1F600}"] {
            let c = card_for(&bash(cmd));
            assert!(!c.approvable, "{cmd}");
            assert!(c.reason.unwrap().contains("non-ASCII"));
        }
    }

    #[test]
    fn h1_rejects_over_500_chars_and_allows_exactly_500() {
        let ok = format!("echo {}", "x".repeat(MAX_APPROVE_CMD - 5));
        assert_eq!(ok.chars().count(), 500);
        assert!(card_for(&bash(&ok)).approvable);
        let long = format!("{ok}x");
        let c = card_for(&bash(&long));
        assert!(!c.approvable);
        assert_eq!(c.command.as_deref(), Some(long.as_str())); // still shown in full
        assert!(c.reason.unwrap().contains("500"));
    }

    #[test]
    fn h1_shell_operators_warn_but_plain_ascii_stays_approvable() {
        for cmd in ["echo a; echo b", "echo a & echo b", "echo a | more", "echo a > f.txt", "cat < f.txt", "echo `x`"] {
            let c = card_for(&bash(cmd));
            assert!(c.warning, "{cmd}");
            assert!(c.approvable, "{cmd}: warning only, approval stays allowed");
        }
        let c = card_for(&bash("echo $(whoami)")); // `(` is also unscopable
        assert!(c.warning && !c.approvable);
        assert!(!card_for(&bash("echo hello world")).warning);
    }

    /// Live: a real `claude -p` turn with the real argv gets a Bash command denied; the card
    /// must carry the full command, and the warning flag follows the operators.
    /// Run: cargo test live_denied_bash_card -- --ignored --nocapture
    #[test]
    #[ignore]
    fn live_denied_bash_card() {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let dir = std::env::temp_dir().join("one-desktop-live-h1");
        std::fs::create_dir_all(&dir).unwrap();
        // not read-only (echo alone is auto-allowed), so Claude Code denies it in manual mode
        let cmd_text = "ping -n 1 127.0.0.1 && echo one-live-check-plain-ascii";
        let mut child = Command::new(resolve_claude().expect("claude on PATH"))
            .args(build_args(None, None, None))
            .current_dir(&dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let prompt = format!("Use the Bash tool to run exactly this command, nothing else: {cmd_text}");
        child.stdin.take().unwrap().write_all(prompt.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        let denials = text
            .lines()
            .find_map(|l| match parse_line(l) {
                StreamItem::Result { denials, .. } => Some(denials),
                _ => None,
            })
            .expect("a result line");
        println!("denials: {}; stdout tail: {}", denials.len(), text.lines().last().unwrap_or("").chars().take(600).collect::<String>());
        assert_eq!(denials.len(), 1, "one denied Bash call expected");
        let card = card_for(&denials[0]);
        println!("card: command={:?} warning={} approvable={} reason={:?}", card.command, card.warning, card.approvable, card.reason);
        assert_eq!(card.command.as_deref(), Some(cmd_text));
        assert!(card.warning, "&& is a shell operator");
        assert!(card.approvable, "plain ASCII under 500 chars stays approvable");
    }

    #[test]
    fn non_bash_cards_are_deny_only_with_a_reason() {
        let d = Denial { tool_name: "Write".into(), tool_use_id: "t".into(), tool_input: json!({"file_path":"C:/a.txt"}) };
        let c = card_for(&d);
        assert!(!c.approvable && c.command.is_none() && c.reason.is_some());
    }

    #[test]
    fn requests_are_written_as_plain_sentences() {
        assert_eq!(plain_words("Grep", &json!({"pattern": "T-2041", "path": "hub"})), "One wants to search for \"T-2041\" in hub. Allow?");
        assert_eq!(plain_words("Grep", &json!({"pattern": "T-2041"})), "One wants to search for \"T-2041\". Allow?");
        assert_eq!(plain_words("Glob", &json!({"pattern": "**/*.md"})), "One wants to look for files matching **/*.md. Allow?");
        assert_eq!(plain_words("Read", &json!({"file_path": "a/b.txt"})), "One wants to read the file a/b.txt. Allow?");
        assert_eq!(plain_words("Write", &json!({"file_path": "x.rs"})), "One wants to change the file x.rs. Allow?");
        assert_eq!(plain_words("Bash", &json!({"command": "echo hi"})), "One wants to run this command: echo hi. Allow?");
        assert_eq!(plain_words("WebFetch", &json!({"url": "https://a.example/x"})), "One wants to open the web page https://a.example/x. Allow?");
        assert_eq!(plain_words("Weird-Tool!", &json!({})), "One wants to use the WeirdTool tool. Allow?");
        assert_eq!(plain_words("Grep", &json!({})), "One wants to search through files. Allow?");
    }

    #[test]
    fn plain_sentences_are_redacted_cleaned_and_cut() {
        let secret = "m3n4b5v6c7".repeat(4);
        let s = plain_words("Bash", &json!({"command": format!("curl x\n\u{202E}{secret} {}", "a".repeat(400))}));
        assert!(!s.contains("m3n4b5v6") && !s.contains('\n') && !s.contains('\u{202E}'), "{s}");
        let spoof = plain_words("Read", &json!({"file_path": "a\u{202E}txt.exe\u{200B}"}));
        assert!(!spoof.contains('\u{202E}') && !spoof.contains('\u{200B}'), "bidi and zero-width characters are removed");
        assert_eq!(plain_words("Grep", &json!({"pattern": "x\" in C:/y"})), "One wants to search for \"x' in C:/y\". Allow?", "a quote cannot fake the sentence");
        assert!(s.chars().count() < 220);
        let card = card_for(&Denial { tool_name: "Grep".into(), tool_use_id: "t".into(), tool_input: json!({"pattern": "T-2041"}) });
        assert!(card.sentence.contains("T-2041") && !card.approvable && card.reason.is_some(), "Deny and a reason are shown for what cannot be allowed");
    }

    #[test]
    fn no_read_rule_is_pre_allowed_on_any_turn() {
        // reads inside the working directory need no approval anyway; nothing is added for them,
        // least of all on the focus and tainted (lockdown) turns
        for lockdown in [false, true] {
            let a = build_args_for(None, &[], None, lockdown);
            assert!(!a.iter().any(|x| x.contains("Read(")), "no Read( rule in {a:?}");
            assert!(!a.contains(&"--allowedTools".to_string()));
        }
        let a = build_args_for(None, &["Bash(echo hi)".to_string()], None, false);
        let i = a.iter().position(|x| x == "--allowedTools").unwrap();
        assert_eq!(a[i + 1..], ["Bash(echo hi)".to_string()]);
    }
}
