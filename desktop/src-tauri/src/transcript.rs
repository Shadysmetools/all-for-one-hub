//! Junk-turn filter. Speech-to-text also reports sounds as tags: `[door closing]`, `(laughs)`,
//! `[music]`. They must never start a turn (the incident: a lone "[door closing]" was sent to
//! Claude as if the user had said it).
//!
//! The filter drops only what is surely not speech: a part that is only sound tags or music
//! notes, and a turn that is only known fillers ("uh", "hmm", ...). Every real word, including
//! one-word answers ("hub", "two", "status", "نعم"), starts a turn.
//!
//! - `strip_tags` removes sound tags (letters and spaces only, so "(555)" and "(b)" stay) and music
//!   notes from one transcribed part.
//! - `worth_a_turn` is checked on the merged text, after Arabic normalisation.

/// Longest bracketed tag that is treated as a sound label, in characters.
const MAX_TAG: usize = 40;
/// A tag needs at least this many letters ("(b)" is an option, "(laughs)" is a sound).
const MIN_TAG_LETTERS: usize = 3;

/// Sounds that are not words. A turn made only of these is dropped (the Arabic ones are
/// written in their normalised form, see `normalize`).
const FILLERS: [&str; 19] = [
    "uh", "uhh", "um", "umm", "er", "erm", "hmm", "hm", "hmmm", "mm", "mmm", "ah", "eh", "huh", "امم", "اممم", "ام", "ااه", "همم",
];

/// Lowercase, strip Arabic diacritics and tatweel, fold the hamza forms of alef, alef maqsura
/// and teh marbuta (أ/إ/آ→ا, ى→ي, ة→ه).
pub fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| !matches!(*c, '\u{064B}'..='\u{065F}' | '\u{0670}' | '\u{0640}'))
        .map(|c| match c {
            'أ' | 'إ' | 'آ' => 'ا',
            'ى' => 'ي',
            'ة' => 'ه',
            c => c,
        })
        .flat_map(char::to_lowercase)
        .collect()
}

fn is_sound_tag(inner: &[char]) -> bool {
    inner.iter().filter(|c| c.is_alphabetic()).count() >= MIN_TAG_LETTERS && inner.iter().all(|c| c.is_alphabetic() || matches!(c, ' ' | '-' | '\'' | '_'))
}

/// Remove sound tags like `[door closing]`, `(laughs)`, `{noise}` and music notes, then tidy the
/// whitespace. A bracket that holds digits or punctuation, or is very long, is real text.
pub fn strip_tags(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let close = match chars[i] {
            '[' => Some(']'),
            '(' => Some(')'),
            '{' => Some('}'),
            _ => None,
        };
        let end = close.and_then(|c| chars[i + 1..].iter().take(MAX_TAG + 1).position(|x| *x == c));
        match end {
            Some(n) if is_sound_tag(&chars[i + 1..i + 1 + n]) => i += n + 2, // skip the whole tag
            _ => {
                if !matches!(chars[i], '\u{266A}' | '\u{266B}' | '\u{266C}') {
                    out.push(chars[i]);
                }
                i += 1;
            }
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn words(text: &str) -> Vec<String> {
    normalize(text)
        .split_whitespace()
        .map(|w| w.chars().filter(|c| c.is_alphanumeric()).collect::<String>())
        .filter(|w| !w.is_empty())
        .collect()
}

/// Whether merged, tag-free text deserves a turn: it has at least one word that is not a filler.
pub fn worth_a_turn(text: &str) -> bool {
    words(text).iter().any(|w| !FILLERS.contains(&w.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_part_that_is_only_sound_tags_becomes_nothing() {
        for junk in ["[door closing]", "(laughs)", "[music]", "[silence]", "  [Music]  (applause) ", "\u{266A}", "[door closing] [music]", "{noise}"] {
            assert_eq!(strip_tags(junk), "", "{junk:?}");
            assert!(!worth_a_turn(&strip_tags(junk)));
        }
    }

    #[test]
    fn tags_are_stripped_from_mixed_text_and_real_brackets_stay() {
        assert_eq!(strip_tags("check the hub [door closing] logs"), "check the hub logs");
        assert_eq!(strip_tags("what is (laughs) the status"), "what is the status");
        assert_eq!(strip_tags("\u{266A} hello there \u{266A}"), "hello there");
        assert_eq!(strip_tags("call (555) 1234"), "call (555) 1234", "digits are not a sound tag");
        assert_eq!(strip_tags("pick option (b) please"), "pick option (b) please", "one letter is not a sound tag");
        assert_eq!(strip_tags("a [b"), "a [b");
        let long = format!("[{}]", "word ".repeat(20));
        assert!(strip_tags(&long).starts_with("[word"));
    }

    #[test]
    fn real_one_word_answers_are_kept_and_only_fillers_and_nothing_are_dropped() {
        for ok in ["hub", "two", "Maya", "tomorrow", "help", "status", "hello", "yes", "No.", "Stop!", "نعم", "لا", "لأ", "اه", "آه", "أيوه", "ايوة", "خلاص", "شكرا", "okay", "the"] {
            assert!(worth_a_turn(ok), "{ok:?} is real speech");
        }
        for junk in ["", " ", "...", "?", "hmm", "Uh", "um", "uh um hmm", "Mmm.", "اممم", "أمم"] {
            assert!(!worth_a_turn(junk), "{junk:?} is not");
        }
        assert!(worth_a_turn("uh the hub"), "a filler next to a real word is fine");
    }

    #[test]
    fn arabic_is_normalised_before_matching() {
        assert_eq!(normalize("أَيْوَه"), "ايوه");
        assert_eq!(normalize("إلى"), "الي");
        assert_eq!(normalize("شكرًا"), "شكرا");
        assert_eq!(normalize("مدرسـة"), "مدرسه", "tatweel and teh marbuta");
        assert_eq!(strip_tags("ايه (ضحك) الاخبار"), "ايه الاخبار");
        assert!(worth_a_turn("ايه الاخبار"));
    }

    #[test]
    fn the_wake_word_leaves_a_real_command_through() {
        let after_wake = crate::wake::filter("One, status", true).unwrap();
        assert!(worth_a_turn(&after_wake), "'One, status' keeps 'status'");
    }
}
