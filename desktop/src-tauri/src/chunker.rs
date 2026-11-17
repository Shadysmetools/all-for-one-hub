//! Sentence chunker: turns streamed text deltas into speakable chunks.
//! The first chunk may break at a clause (about 6+ words) to cut time-to-first-audio.
//! All markdown is removed before speech: emphasis, headings, list markers, links (the link text
//! is read, the address is not) and code blocks (replaced by one short sentence).

use regex::Regex;
use std::sync::OnceLock;

/// Said once, in place of a whole code block.
pub const CODE_NOTE: &str = "I've put the code on screen.";

#[derive(Default)]
struct MdState {
    in_code: bool,
    said_code: bool,
}

#[derive(Default)]
pub struct Chunker {
    buf: String,
    emitted: usize,
    md: MdState,
}

const MIN_CHARS: usize = 12;

fn is_terminator(c: char) -> bool {
    matches!(c, '.' | '!' | '?' | '\u{061F}' | '\u{2026}' | '\n')
}

fn is_clause_break(c: char) -> bool {
    matches!(c, ',' | ';' | ':' | '\u{060C}' | '\u{061B}')
}

fn re(cell: &'static OnceLock<Regex>, pat: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pat).expect("static pattern"))
}

/// One line without its markdown: heading hashes, quote marks, list markers, links, emphasis.
fn strip_line(line: &str) -> String {
    static LINK: OnceLock<Regex> = OnceLock::new();
    static LEAD: OnceLock<Regex> = OnceLock::new();
    static UNDERSCORE: OnceLock<Regex> = OnceLock::new();
    let line = re(&LEAD, r"^\s*(?:#{1,6}\s+|>+\s?|[-*+]\s+|\d+[.)]\s+)").replace(line, "");
    let line = re(&LINK, r"!?\[([^\]]*)\]\([^)]*\)").replace_all(&line, "$1");
    // _emphasis_ goes, but snake_case keeps its word
    let line = re(&UNDERSCORE, r"(^|\W)_+|_+(\W|$)").replace_all(&line, "$1$2");
    line.chars().filter(|c| !matches!(c, '*' | '#' | '`' | '~' | '|')).collect()
}

/// Speakable text of `s`; a code fence flips `st`, so a block that spans several chunks is
/// dropped as a whole and announced once.
fn strip_md(s: &str, st: &mut MdState) -> String {
    let mut out = String::new();
    for (i, seg) in s.split("```").enumerate() {
        if i > 0 {
            st.in_code = !st.in_code;
            if st.in_code && !st.said_code {
                st.said_code = true;
                out.push_str(&format!("\n{CODE_NOTE}\n"));
            }
        }
        if !st.in_code {
            out.push_str(seg);
        }
    }
    let lines: Vec<String> = out.lines().map(|l| strip_line(l).split_whitespace().collect::<Vec<_>>().join(" ")).filter(|l| !l.is_empty()).collect();
    lines.join(" ")
}

/// Strip markdown that sounds wrong when spoken (a whole text at once).
pub fn speakable(s: &str) -> String {
    strip_md(s, &mut MdState::default())
}

impl Chunker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a delta; returns zero or more completed chunks.
    pub fn push(&mut self, delta: &str) -> Vec<String> {
        self.buf.push_str(delta);
        let mut out = Vec::new();
        while let Some(end) = self.find_break() {
            let rest = self.buf.split_off(end);
            let chunk = std::mem::replace(&mut self.buf, rest);
            let s = strip_md(&chunk, &mut self.md);
            if !s.is_empty() {
                self.emitted += 1;
                out.push(s);
            }
        }
        out
    }

    /// Flush whatever is left (end of the reply).
    pub fn finish(&mut self) -> Option<String> {
        let rest = std::mem::take(&mut self.buf);
        let s = strip_md(&rest, &mut self.md);
        (!s.is_empty()).then_some(s)
    }

    /// Byte index just past a break, only when the next char is known (whitespace),
    /// so "3.5" and "e.g." fragments are not split mid-token.
    fn find_break(&self) -> Option<usize> {
        let chars: Vec<(usize, char)> = self.buf.char_indices().collect();
        for (i, &(idx, c)) in chars.iter().enumerate() {
            let Some(&(_, next)) = chars.get(i + 1) else { break };
            let end = idx + c.len_utf8();
            if !next.is_whitespace() || end < MIN_CHARS {
                continue;
            }
            let words = self.buf[..end].split_whitespace().count();
            if is_terminator(c) || (self.emitted == 0 && is_clause_break(c) && words >= 6) {
                return Some(end);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_sentences_across_deltas() {
        let mut c = Chunker::new();
        let mut all = Vec::new();
        for d in ["Sure thing", ". I checked the ", "queue. There are ", "3.5 items left", "."] {
            all.extend(c.push(d));
        }
        all.extend(c.finish());
        assert_eq!(all, vec!["Sure thing. I checked the queue.".to_string(), "There are 3.5 items left.".to_string()]);
    }

    #[test]
    fn first_chunk_breaks_at_clause() {
        let mut c = Chunker::new();
        let out = c.push("Well, the three tickets waiting for you are urgent, and more. ");
        assert!(!out.is_empty());
        assert_eq!(out[0], "Well, the three tickets waiting for you are urgent,");
    }

    #[test]
    fn strips_markdown_and_handles_arabic_stop() {
        assert_eq!(speakable("**Done** `ok`"), "Done ok");
        let mut c = Chunker::new();
        let out = c.push("\u{0647}\u{0644} \u{0627}\u{0646}\u{062A} \u{0628}\u{062E}\u{064A}\u{0631}\u{061F} \u{0646}\u{0639}\u{0645}");
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn all_markdown_is_removed_before_speech() {
        assert_eq!(speakable("## Status\nThe **hub** is `green`."), "Status The hub is green.");
        assert_eq!(speakable("- first item\n* second item\n+ third\n1. one\n2) two"), "first item second item third one two");
        assert_eq!(speakable("See [the board](https://x.example/a.b) and ![logo](i.png) now"), "See the board and logo now");
        assert_eq!(speakable("> quoted _text_ and snake_case ~~gone~~"), "quoted text and snake_case gone");
        assert_eq!(speakable("a | b |"), "a b");
        assert_eq!(speakable("It costs 3.5 million, 1.5x more"), "It costs 3.5 million, 1.5x more");
        assert!(!speakable("**bold** # x `y` [t](u)").contains(['*', '`', '[', ']', '(']));
    }

    #[test]
    fn a_code_block_becomes_one_short_sentence_even_when_streamed_in_pieces() {
        assert_eq!(speakable("Run this:\n```bash\nrm -rf x\nls\n```\nThen wait."), format!("Run this: {CODE_NOTE} Then wait."));
        let mut c = Chunker::new();
        let mut all = Vec::new();
        for d in ["Here is the fix for you.\n```rust\nfn main() {", "}\nlet x = 1;\n```\nThat is all there is to it.", " Bye now friend."] {
            all.extend(c.push(d));
        }
        all.extend(c.finish());
        let spoken = all.join(" ");
        assert!(spoken.contains("Here is the fix") && spoken.contains("That is all") && spoken.contains("Bye now"));
        assert_eq!(spoken.matches(CODE_NOTE).count(), 1);
        assert!(!spoken.contains("fn main") && !spoken.contains("let x"), "code is never read aloud: {spoken}");
    }
}
