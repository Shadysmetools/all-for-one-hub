//! Secret redaction for reply text (M3). Runs in Rust before text reaches the webview
//! or TTS. Masked secrets become "[hidden]" (TTS says "hidden").
//!
//! Streaming: a secret can arrive split across deltas, so `StreamRedactor` only releases
//! text up to a word boundary and holds back the last few words until more text (or the
//! end of the turn) arrives.

use regex::Regex;
use std::sync::OnceLock;

pub const MASK: &str = "[hidden]";
/// Words held back while streaming. Covers `api_key = value` (three words) plus the tail.
const HOLD_WORDS: usize = 3;

fn patterns() -> &'static Vec<Regex> {
    static P: OnceLock<Vec<Regex>> = OnceLock::new();
    P.get_or_init(|| {
        [
            // name = value pairs first, so the value is masked even when it is short
            r"(?i)\b(?:api[_-]?key|access[_-]?key|token|password|passwd|secret)\b\s*[=:]\s*\S+",
            // Authorization headers: `Bearer <token>`
            r"(?i)\bBearer\s+[A-Za-z0-9._~+/=\-]{6,}",
            // well-known token prefixes
            r"\b(?:sk[-_]|ghp_|gho_|xox[bp]-)[A-Za-z0-9_\-]{6,}",
            r"\b(?:AKIA|EAA)[A-Za-z0-9]{8,}",
            // JWT-looking
            r"\beyJ[A-Za-z0-9_\-]{5,}(?:\.[A-Za-z0-9_\-]+)*",
            // any long run: identifier-like, hex, or base64
            r"[A-Za-z0-9_\-]{32,}",
            r"[A-Za-z0-9+/]{32,}={0,2}",
        ]
        .iter()
        .map(|p| Regex::new(p).expect("static regex"))
        .collect()
    })
}

/// `scheme://user:password@host`: only the credentials are masked, the host stays readable.
fn url_credentials() -> &'static Regex {
    static P: OnceLock<Regex> = OnceLock::new();
    P.get_or_init(|| Regex::new(r"://[^/\s:@]+:[^@\s]+@").expect("static regex"))
}

pub fn redact(text: &str) -> String {
    let mut out = url_credentials().replace_all(text, "://[hidden]@").into_owned();
    for re in patterns() {
        out = re.replace_all(&out, MASK).into_owned();
    }
    out
}

#[derive(Default)]
pub struct StreamRedactor {
    buf: String,
}

impl StreamRedactor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a delta; returns text that is safe to release now (possibly empty).
    pub fn push(&mut self, delta: &str) -> String {
        self.buf.push_str(delta);
        let cut = self.safe_cut();
        if cut == 0 {
            return String::new();
        }
        let rest = self.buf.split_off(cut);
        let head = std::mem::replace(&mut self.buf, rest);
        redact(&head)
    }

    /// End of turn: release everything that is left.
    pub fn finish(&mut self) -> String {
        redact(&std::mem::take(&mut self.buf))
    }

    /// Byte index of the start of the third-last complete word (0 when there is nothing to release).
    fn safe_cut(&self) -> usize {
        // word starts: a non-space char that follows a space (or the start of the buffer)
        let mut starts: Vec<usize> = Vec::new();
        let mut prev_space = true;
        for (i, c) in self.buf.char_indices() {
            if !c.is_whitespace() && prev_space {
                starts.push(i);
            }
            prev_space = c.is_whitespace();
        }
        // the last word may still be growing: it counts as part of the hold, along with HOLD_WORDS before it
        let keep = HOLD_WORDS + 1;
        let mut cut = if starts.len() <= keep { 0 } else { starts[starts.len() - keep] };
        // Never cut through a match: move the cut back to where that match starts.
        loop {
            let moved = patterns()
                .iter()
                .flat_map(|re| re.find_iter(&self.buf))
                .find(|m| m.start() < cut && m.end() > cut)
                .map(|m| m.start());
            match moved {
                Some(s) => cut = s,
                None => return cut,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test secrets are assembled at runtime so no source line looks like a real token
    /// to secret scanners (gitleaks), while the redaction tests stay meaningful.
    fn cat(parts: &[&str]) -> String {
        parts.concat()
    }

    #[test]
    fn masks_known_token_prefixes() {
        for t in [
            cat(&["s", "k-", "abcdef1234567890"]),
            cat(&["s", "k_", "live_", "abcdef123456"]),
            cat(&["gh", "p_", "abcdefghijklmnop"]),
            cat(&["gh", "o_", "abcdefghijklmnop"]),
            cat(&["xo", "xb-", "123456789012-abcdef"]), // pii-scan: allow (test fixture)
            cat(&["xo", "xp-", "123456789012-abcdef"]), // pii-scan: allow (test fixture)
            cat(&["AK", "IA", "IOSFODNN7EXAMPLE"]),
            cat(&["EA", "A", "Gm0PX4ZCpsBAKexample"]),
        ] {
            let t = t.as_str();
            let r = redact(&format!("here it is {t} ok"));
            assert!(!r.contains(t), "leaked {t}: {r}");
            assert!(r.contains(MASK));
        }
    }

    #[test]
    fn masks_jwt_and_long_runs() {
        let jwt = cat(&["ey", "JhbGciOiJIUzI1NiJ9", ".", "ey", "JzdWIiOiIxMjM0In0", ".", "abc_DEF-123"]);
        assert_eq!(redact(&format!("jwt {jwt} end")), "jwt [hidden] end");
        let hex = "0123456789abcdef0123456789abcdef"; // pii-scan: allow (test fixture)
        assert!(!redact(hex).contains("0123456789abcdef0123"));
        let b64 = "QWxhZGRpbjpvcGVuIHNlc2FtZSBhbmQgbW9yZQ==";
        assert!(!redact(b64).contains("QWxhZGRpbjpvcGVu"));
        let short = "abcdefghijklmnopqrstuvwxyz01234"; // 31 chars: below the threshold
        assert_eq!(short.len(), 31);
        assert_eq!(redact(short), short);
    }

    #[test]
    fn masks_name_value_pairs() {
        for s in ["api_key=abc", "API-KEY: abc", "token = abc", "my password: hunter2", "secret=x"] {
            let r = redact(s);
            assert!(r.contains(MASK), "{s} -> {r}");
            assert!(!r.contains("hunter2") && !r.contains("=abc") && !r.ends_with("abc"), "{s} -> {r}");
        }
    }

    #[test]
    fn masks_bearer_tokens_and_url_credentials() {
        let tok = cat(&["abc", "123", "xyz"]); // short, so only the Bearer rule can catch it
        let out = redact(&format!("curl -H 'Authorization: Bearer {tok}' https://example.org"));
        assert!(!out.contains(&tok), "{out}");
        let url = cat(&["https://alice:", "hunter", "2@db.example.org/path"]); // pii-scan: allow
        let out = redact(&format!("connect to {url} now"));
        assert!(!out.contains("alice") && !out.contains("hunter2"), "{out}");
        assert!(out.contains("db.example.org/path"), "the host stays readable: {out}");
        assert_eq!(redact("the bearer of good news"), "the bearer of good news");
        assert_eq!(redact("see https://example.org/a:b and user@example.org"), "see https://example.org/a:b and user@example.org"); // pii-scan: allow
    }

    #[test]
    fn leaves_normal_speech_alone() {
        for s in ["The build passed and two tests were skipped.", "Open T-2041 for Riley, it is three days old.", "Il mot de passe est secret.", "C:/Users/alex/file.txt"] {
            assert_eq!(redact(s), s);
        }
    }

    #[test]
    fn stream_redactor_catches_a_token_split_across_deltas() {
        let mut r = StreamRedactor::new();
        let mut out = String::new();
        let lead = cat(&["The key is s", "k-"]);
        for d in [lead.as_str(), "abcdef", "1234567890 and then ", "more words follow here ", "so it flushes out ", "of the hold"] {
            out.push_str(&r.push(d));
        }
        out.push_str(&r.finish());
        assert!(!out.contains("abcdef1234567890"), "{out}");
        assert!(!out.contains(&cat(&["s", "k-abc"])), "{out}");
        assert!(out.contains(MASK) && out.contains("more words follow"));
    }

    #[test]
    fn stream_redactor_catches_a_spaced_pair_split_across_deltas() {
        let mut r = StreamRedactor::new();
        let mut out = String::new();
        for d in ["Note: the pass", "word", " = ", "hunter2", " is set. ", "Nothing else to say at all."] {
            out.push_str(&r.push(d));
        }
        out.push_str(&r.finish());
        assert!(!out.contains("hunter2"), "{out}");
        assert!(out.contains("Nothing else to say"));
    }

    #[test]
    fn stream_redactor_output_equals_whole_text_redaction() {
        let text = "Here is token: abc123 and a long id 0123456789abcdef0123456789abcdef plus words after it to flush."; // pii-scan: allow (test fixture)
        let mut r = StreamRedactor::new();
        let mut out = String::new();
        for ch in text.chars() {
            out.push_str(&r.push(&ch.to_string()));
        }
        out.push_str(&r.finish());
        assert_eq!(out, redact(text));
    }
}
