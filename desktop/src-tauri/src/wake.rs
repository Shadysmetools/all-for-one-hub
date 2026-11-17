//! Optional wake word (settings.json `wake_word`, off by default): when on, only speech
//! addressed to "One" becomes a turn. Everything else is dropped, and the word is removed.
//!
//! "One" must be the FIRST word, or come right after one greeting (hey, hi, ok, يا). Anywhere
//! else it is an ordinary word ("No one told me", "Which one is blocked?"). The Arabic spelling
//! "ون" counts under the same rule; "وان" is not accepted because it is also the common
//! spelling of "and that / and if".

const WORDS: [&str; 2] = ["one", "ون"];
const GREETINGS: [&str; 6] = ["hey", "hi", "hello", "ok", "okay", "يا"];

fn bare(token: &str) -> String {
    token.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase()
}

/// `enabled` off: the text is returned as is. On: `None` unless the wake word is the first word
/// (after an optional greeting); the greeting and the word are removed. An empty remainder
/// (just "One") is dropped too: there is nothing to answer.
pub fn filter(text: &str, enabled: bool) -> Option<String> {
    if !enabled {
        return Some(text.to_string());
    }
    let words: Vec<&str> = text.split_whitespace().collect();
    let at = match words.first() {
        Some(w) if GREETINGS.contains(&bare(w).as_str()) => 1,
        _ => 0,
    };
    if !words.get(at).is_some_and(|w| WORDS.contains(&bare(w).as_str())) {
        return None;
    }
    let out = words[at + 1..].join(" ");
    // "One, check" leaves no leading comma or colon.
    let out = out.trim_start_matches(|c: char| c == ',' || c == '،' || c == ':' || c.is_whitespace());
    (!out.trim().is_empty()).then(|| out.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn off_means_everything_passes_unchanged() {
        assert_eq!(filter("check the logs", false).as_deref(), Some("check the logs"));
    }

    #[test]
    fn on_drops_speech_not_addressed_to_one() {
        assert_eq!(filter("check the hub logs please", true), None);
        assert_eq!(filter("someone else said that", true), None, "'someone' is not the word 'one'");
        assert_eq!(filter("No one told me", true), None, "'one' not first");
        assert_eq!(filter("Which one is blocked?", true), None);
        assert_eq!(filter("check the logs, One", true), None, "at the end is not addressing");
        assert_eq!(filter("hey you, one more thing", true), None, "only one greeting, directly before the word");
    }

    #[test]
    fn on_needs_one_first_and_strips_it() {
        assert_eq!(filter("One, check the hub logs", true).as_deref(), Some("check the hub logs"));
        assert_eq!(filter("one check the hub logs", true).as_deref(), Some("check the hub logs"));
        assert_eq!(filter("Hey One check the hub logs", true).as_deref(), Some("check the hub logs"));
        assert_eq!(filter("ok, one: where are we", true).as_deref(), Some("where are we"));
    }

    #[test]
    fn arabic_wake_word_is_only_accepted_first() {
        assert_eq!(filter("ون، شوف اللوجز", true).as_deref(), Some("شوف اللوجز"));
        assert_eq!(filter("يا ون شوف اللوجز", true).as_deref(), Some("شوف اللوجز"));
        assert_eq!(filter("شوف ون اللوجز", true), None, "not first");
        assert_eq!(filter("وان شوف اللوجز", true), None, "too ambiguous: dropped from the list");
        assert_eq!(filter("يا وان شوف اللوجز", true), None);
    }

    #[test]
    fn only_the_wake_word_is_nothing_to_answer() {
        assert_eq!(filter("One", true), None);
        assert_eq!(filter("One!", true), None);
        assert_eq!(filter("hey one", true), None);
    }
}
