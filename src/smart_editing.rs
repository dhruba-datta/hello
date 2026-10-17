//! Local, deterministic cleanup of a dictated transcript before mode processing:
//! filler-word removal, "scratch that", spoken line breaks, numbered lists, a closing
//! full stop, and Bangla punctuation and digits.
//! Everything here is offline rule-based text processing; no model or network.

use std::sync::OnceLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct SmartEditing {
    /// Drop "um", "uh", "erm" and similar hesitation sounds.
    pub remove_fillers: bool,
    /// "scratch that" deletes the sentence spoken just before it.
    pub scratch_that: bool,
    /// "new line" and "new paragraph" insert line breaks.
    pub spoken_line_breaks: bool,
    /// "number one … number two …" becomes a numbered list.
    pub numbered_lists: bool,
    /// A dictation that stops mid-sentence ends with a full stop ("।" after Bangla).
    pub end_with_period: bool,
    /// A full stop after Bangla text becomes "।" wherever the model wrote one.
    pub bangla_punctuation: bool,
    /// How digits are written in Bangla sentences.
    pub bangla_digits: BanglaDigits,
}

impl Default for SmartEditing {
    fn default() -> Self {
        Self {
            remove_fillers: true,
            scratch_that: true,
            spoken_line_breaks: true,
            numbered_lists: true,
            end_with_period: true,
            bangla_punctuation: true,
            bangla_digits: BanglaDigits::Keep,
        }
    }
}

impl SmartEditing {
    #[cfg(test)]
    pub const fn disabled() -> Self {
        Self {
            remove_fillers: false,
            scratch_that: false,
            spoken_line_breaks: false,
            numbered_lists: false,
            end_with_period: false,
            bangla_punctuation: false,
            bangla_digits: BanglaDigits::Keep,
        }
    }

    pub const fn is_enabled(self) -> bool {
        self.remove_fillers
            || self.scratch_that
            || self.spoken_line_breaks
            || self.numbered_lists
            || self.end_with_period
            || self.bangla_punctuation
            || !matches!(self.bangla_digits, BanglaDigits::Keep)
    }
}

/// Digits in sentences that contain Bangla.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BanglaDigits {
    /// As the model wrote them.
    #[default]
    Keep,
    /// Bangla digits: ১২৩.
    Bangla,
    /// Latin digits: 123.
    Latin,
}

impl BanglaDigits {
    pub const ALL: [Self; 3] = [Self::Keep, Self::Bangla, Self::Latin];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Keep => "Keep",
            Self::Bangla => "\u{09e7}\u{09e8}\u{09e9}",
            Self::Latin => "123",
        }
    }

    pub const fn encoded(self) -> u8 {
        match self {
            Self::Keep => 0,
            Self::Bangla => 1,
            Self::Latin => 2,
        }
    }

    pub const fn decode(value: u8) -> Self {
        match value {
            1 => Self::Bangla,
            2 => Self::Latin,
            _ => Self::Keep,
        }
    }
}

pub fn apply(text: &str, options: SmartEditing) -> String {
    if !options.is_enabled() || text.trim().is_empty() {
        return text.to_string();
    }
    let mut text = text.to_string();
    if options.scratch_that {
        text = apply_scratch_that(&text);
    }
    if options.remove_fillers {
        text = remove_fillers(&text);
    }
    if options.spoken_line_breaks {
        text = apply_line_breaks(&text);
    }
    if options.numbered_lists {
        text = apply_numbered_list(&text);
    }
    let text = tidy(&text);
    let text = if options.end_with_period {
        end_sentence(&text)
    } else {
        text
    };
    let text = if options.bangla_punctuation {
        bangla_full_stops(&text)
    } else {
        text
    };
    convert_digits(&text, options.bangla_digits)
}

fn is_bangla_digit(character: char) -> bool {
    ('\u{09e6}'..='\u{09ef}').contains(&character)
}

fn is_digit(character: char) -> bool {
    character.is_ascii_digit() || is_bangla_digit(character)
}

/// A full stop after Bangla becomes the dari, except inside a number like ৩.৫.
fn bangla_full_stops(text: &str) -> String {
    let characters: Vec<char> = text.chars().collect();
    let mut output = String::with_capacity(text.len());
    for (index, &character) in characters.iter().enumerate() {
        if character == '.' {
            let previous = characters[..index].iter().rev().find(|previous| {
                !previous.is_whitespace()
                    && !matches!(previous, '"' | '\'' | ')' | '\u{201d}' | '\u{2019}')
            });
            let decimal = characters[..index]
                .last()
                .is_some_and(|last| is_digit(*last))
                && characters
                    .get(index + 1)
                    .is_some_and(|next| is_digit(*next));
            if previous.is_some_and(|previous| is_bangla(*previous)) && !decimal {
                output.push('\u{0964}');
                continue;
            }
        }
        output.push(character);
    }
    output
}

/// Rewrites digits in sentences that contain Bangla letters. Latin digits are also used
/// in sentences without Bangla, so only Bangla → Latin applies to the whole text.
fn convert_digits(text: &str, choice: BanglaDigits) -> String {
    match choice {
        BanglaDigits::Keep => text.to_string(),
        BanglaDigits::Latin => text
            .chars()
            .map(|character| {
                if is_bangla_digit(character) {
                    char::from(b'0' + (character as u32 - 0x09e6) as u8)
                } else {
                    character
                }
            })
            .collect(),
        BanglaDigits::Bangla => {
            let mut output = String::with_capacity(text.len() * 2);
            let mut sentence = String::new();
            let flush = |sentence: &mut String, output: &mut String| {
                let bangla = sentence
                    .chars()
                    .any(|character| is_bangla(character) && !is_bangla_digit(character));
                for character in sentence.chars() {
                    if bangla && character.is_ascii_digit() {
                        output.push(
                            char::from_u32(0x09e6 + u32::from(character as u8 - b'0'))
                                .unwrap_or(character),
                        );
                    } else {
                        output.push(character);
                    }
                }
                sentence.clear();
            };
            for character in text.chars() {
                sentence.push(character);
                if is_sentence_end(character) || character == '\u{0964}' {
                    flush(&mut sentence, &mut output);
                }
            }
            flush(&mut sentence, &mut output);
            output
        }
    }
}

/// Bangla letters, vowel signs and digits (U+0980–U+09FF).
fn is_bangla(character: char) -> bool {
    ('\u{0980}'..='\u{09FF}').contains(&character)
}

/// Adds a full stop when the text ends in a word, so the next dictation starts a new
/// sentence. Bangla takes the dari ("।"). Text ending in punctuation is left alone.
fn end_sentence(text: &str) -> String {
    let Some(last) = text.chars().next_back() else {
        return text.to_string();
    };
    if is_bangla(last) {
        format!("{text}\u{0964}")
    } else if last.is_alphanumeric() {
        format!("{text}.")
    } else {
        text.to_string()
    }
}

fn is_sentence_end(character: char) -> bool {
    matches!(character, '.' | '!' | '?' | '\n')
}

fn capitalize_first(text: &str) -> String {
    let mut characters = text.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().chain(characters).collect(),
        None => String::new(),
    }
}

/// Uppercases the first letter of `rest` when it begins a new sentence.
fn join_sentence(kept: &str, rest: &str) -> String {
    let rest = rest.trim_start();
    let starts_sentence = kept.trim_end().chars().last().is_none_or(is_sentence_end);
    let rest = if starts_sentence {
        capitalize_first(rest)
    } else {
        rest.to_string()
    };
    match (kept.trim_end().is_empty(), rest.is_empty()) {
        (true, _) => rest,
        (false, true) => kept.trim_end().to_string(),
        (false, false) if kept.ends_with('\n') => format!("{kept}{rest}"),
        (false, false) => format!("{} {rest}", kept.trim_end()),
    }
}

fn scratch_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| Regex::new(r"(?i)\b(?:no,?\s+)?scratch\s+that\b[.,!?;:]*").unwrap())
}

/// Removes each "scratch that" together with the sentence spoken right before it.
fn apply_scratch_that(text: &str) -> String {
    let mut text = text.to_string();
    while let Some(found) = scratch_pattern().find(&text) {
        let prefix =
            text[..found.start()].trim_end_matches(|c: char| c.is_whitespace() || c == ',');
        let body = prefix
            .strip_suffix(|c: char| matches!(c, '.' | '!' | '?'))
            .unwrap_or(prefix);
        let cut = body.rfind(is_sentence_end).map_or(0, |index| index + 1);
        let kept = &text[..cut];
        let rest = text[found.end()..].to_string();
        text = join_sentence(kept, &rest);
    }
    text
}

fn is_filler(word: &str) -> bool {
    let word = word.to_lowercase();
    let only = |allowed: &str| word.chars().all(|c| allowed.contains(c));
    match word.as_str() {
        "er" | "erm" | "uhm" | "umm" | "uhh" | "hmm" | "hm" | "mm" | "mmm" => true,
        _ => {
            (word.starts_with("um") && word.len() <= 5 && only("um"))
                || (word.starts_with("uh") && word.len() <= 5 && only("uh"))
                || (word.starts_with("hmm") && only("hm"))
        }
    }
}

/// Drops hesitation words, moving sentence-ending punctuation to the previous
/// word and re-capitalizing a sentence whose first word was a filler.
fn remove_fillers(text: &str) -> String {
    text.split('\n')
        .map(remove_fillers_in_line)
        .collect::<Vec<_>>()
        .join("\n")
}

fn remove_fillers_in_line(line: &str) -> String {
    let mut kept: Vec<String> = Vec::new();
    let mut capitalize_next = false;
    for token in line.split_whitespace() {
        let core = token.trim_end_matches(|c: char| c.is_ascii_punctuation() || c == '…');
        let trailing = &token[core.len()..];
        if !core.is_empty() && is_filler(core) {
            let sentence_end = trailing.chars().find(|c| matches!(c, '.' | '!' | '?'));
            let at_sentence_start = kept
                .last()
                .is_none_or(|previous| previous.ends_with(['.', '!', '?']));
            if let Some(end) = sentence_end {
                match kept.last_mut() {
                    Some(previous) if !previous.ends_with(['.', '!', '?']) => {
                        previous.truncate(previous.trim_end_matches([',', ';', ':']).len());
                        previous.push(end);
                        capitalize_next = true;
                    }
                    _ => capitalize_next |= at_sentence_start,
                }
            } else if at_sentence_start && core.chars().next().is_some_and(char::is_uppercase) {
                capitalize_next = true;
            }
            continue;
        }
        if capitalize_next {
            kept.push(capitalize_first(token));
            capitalize_next = false;
        } else {
            kept.push(token.to_string());
        }
    }
    kept.join(" ")
}

fn line_break_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(r"(?i)[ \t]*,?[ \t]*\bnew[ \t]+(line|paragraph)\b[.,!?;:]*[ \t]*").unwrap()
    })
}

fn apply_line_breaks(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut last = 0;
    let mut capitalize_next = false;
    for captures in line_break_pattern().captures_iter(text) {
        let whole = captures.get(0).unwrap();
        let before = &text[last..whole.start()];
        output.push_str(&if capitalize_next {
            capitalize_first(before)
        } else {
            before.to_string()
        });
        let paragraph = captures[1].eq_ignore_ascii_case("paragraph");
        output.push_str(if paragraph { "\n\n" } else { "\n" });
        capitalize_next = true;
        last = whole.end();
    }
    let rest = &text[last..];
    output.push_str(&if capitalize_next {
        capitalize_first(rest)
    } else {
        rest.to_string()
    });
    output
}

fn list_marker_pattern() -> &'static Regex {
    static PATTERN: OnceLock<Regex> = OnceLock::new();
    PATTERN.get_or_init(|| {
        Regex::new(
            r"(?i)\bnumber\s+(one|two|three|four|five|six|seven|eight|nine|ten|[1-9]|10)\b[.,:;]?\s*",
        )
        .unwrap()
    })
}

fn marker_value(word: &str) -> Option<usize> {
    let words = [
        "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
    ];
    word.parse::<usize>().ok().or_else(|| {
        words
            .iter()
            .position(|candidate| candidate.eq_ignore_ascii_case(word))
            .map(|index| index + 1)
    })
}

fn clean_item(item: &str) -> String {
    let mut item = item.trim().trim_end_matches([',', ';']).trim_end();
    for joiner in [" and then", " and", " then"] {
        if item.len() > joiner.len() && item.to_lowercase().ends_with(joiner) {
            item = item[..item.len() - joiner.len()]
                .trim_end_matches([',', ';'])
                .trim_end();
        }
    }
    capitalize_first(item.trim_end_matches('.'))
}

/// Formats "number one … number two …" as a numbered list. Requires at least two
/// markers counting up from one, so an isolated "number one" is left untouched.
fn apply_numbered_list(text: &str) -> String {
    let markers: Vec<_> = list_marker_pattern().captures_iter(text).collect();
    if markers.len() < 2 {
        return text.to_string();
    }
    let sequential = markers
        .iter()
        .enumerate()
        .all(|(index, captures)| marker_value(&captures[1]) == Some(index + 1));
    if !sequential {
        return text.to_string();
    }
    let intro = text[..markers[0].get(0).unwrap().start()].trim();
    let mut items = Vec::new();
    for (index, captures) in markers.iter().enumerate() {
        let start = captures.get(0).unwrap().end();
        let end = markers
            .get(index + 1)
            .map_or(text.len(), |next| next.get(0).unwrap().start());
        items.push(&text[start..end]);
    }
    // Text after the last item's first sentence end is an outro, not part of the item.
    let mut outro = "";
    if let Some(last) = items.last_mut()
        && let Some(position) = last.find(['.', '!', '?'])
        && !last[position + 1..].trim().is_empty()
    {
        outro = last[position + 1..].trim();
        *last = &last[..position];
    }
    let mut output = String::new();
    if !intro.is_empty() {
        let intro = intro.trim_end_matches([',', ';']);
        output.push_str(intro);
        if !intro.ends_with([':', '.', '!', '?']) {
            output.push(':');
        }
        output.push('\n');
    }
    let list = items
        .iter()
        .enumerate()
        .map(|(index, item)| format!("{}. {}", index + 1, clean_item(item)))
        .collect::<Vec<_>>()
        .join("\n");
    output.push_str(&list);
    if !outro.is_empty() {
        output.push('\n');
        output.push_str(&capitalize_first(outro));
    }
    output
}

/// Collapses doubled spaces and stray spaces before punctuation, line by line.
fn tidy(text: &str) -> String {
    text.split('\n')
        .map(|line| {
            let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
            let mut tidied = String::with_capacity(line.len());
            for character in line.chars() {
                if matches!(character, ',' | '.' | '!' | '?' | ';' | ':') && tidied.ends_with(' ') {
                    tidied.pop();
                }
                if character == ',' && tidied.ends_with(',') {
                    continue;
                }
                tidied.push(character);
            }
            tidied
        })
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every rule except the closing full stop, which has its own tests.
    fn all(text: &str) -> String {
        apply(
            text,
            SmartEditing {
                end_with_period: false,
                ..SmartEditing::default()
            },
        )
    }

    #[test]
    fn ends_an_unfinished_sentence_with_a_full_stop() {
        let options = SmartEditing::default();
        assert_eq!(apply("I will call you", options), "I will call you.");
        assert_eq!(apply("Are you coming?", options), "Are you coming?");
        assert_eq!(apply("Done.", options), "Done.");
        assert_eq!(apply("um", options), "");
        assert_eq!(apply("call me at 5", options), "call me at 5.");
        assert_eq!(apply("আমি আসছি", options), "আমি আসছি।");
        assert_eq!(apply("আমি আসছি।", options), "আমি আসছি।");
        assert_eq!(apply("কালকে meeting", options), "কালকে meeting.");
        let only_period = SmartEditing {
            end_with_period: true,
            ..SmartEditing::disabled()
        };
        assert_eq!(apply("see you soon ", only_period), "see you soon.");
    }

    #[test]
    fn bangla_full_stops_become_the_dari() {
        let options = SmartEditing {
            bangla_punctuation: true,
            ..SmartEditing::disabled()
        };
        assert_eq!(apply("আমি যাব. তুমি আসবে.", options), "আমি যাব। তুমি আসবে।");
        assert_eq!(apply("দাম ৩.৫ টাকা.", options), "দাম ৩.৫ টাকা।");
        assert_eq!(
            apply("See you at 5. ঠিক আছে.", options),
            "See you at 5. ঠিক আছে।"
        );
        assert_eq!(apply("Done.", options), "Done.");
    }

    #[test]
    fn digits_follow_the_bangla_digits_choice() {
        let bangla = SmartEditing {
            bangla_digits: BanglaDigits::Bangla,
            ..SmartEditing::disabled()
        };
        assert_eq!(
            apply("আমি 25 টা কিনব। Call me at 5.", bangla),
            "আমি ২৫ টা কিনব। Call me at 5."
        );
        let latin = SmartEditing {
            bangla_digits: BanglaDigits::Latin,
            ..SmartEditing::disabled()
        };
        assert_eq!(apply("আমি ২৫ টা কিনব।", latin), "আমি 25 টা কিনব।");
        assert_eq!(apply("আমি ২৫ টা", SmartEditing::disabled()), "আমি ২৫ টা");
    }

    #[test]
    fn disabled_options_leave_text_untouched() {
        let text = "Um, so new line scratch that number one a number two b";
        assert_eq!(apply(text, SmartEditing::disabled()), text);
    }

    #[test]
    fn removes_fillers_and_recapitalizes() {
        assert_eq!(all("Um, I think we should go."), "I think we should go.");
        assert_eq!(
            all("I think, uh, we should, umm, go now."),
            "I think, we should, go now."
        );
        assert_eq!(all("We are done, uh. Thanks."), "We are done. Thanks.");
        assert_eq!(all("Hmm. Okay then."), "Okay then.");
        assert_eq!(all("Uh."), "");
    }

    #[test]
    fn keeps_real_words_that_look_like_fillers() {
        let text = "The umbrella is under the hummingbird feeder, err on the side of caution.";
        assert_eq!(all(text), text);
        assert_eq!(all("Ah, I see. Mhm."), "Ah, I see. Mhm.");
    }

    #[test]
    fn scratch_that_removes_the_previous_sentence() {
        assert_eq!(
            all("Let's meet at five. Scratch that. Let's meet at six."),
            "Let's meet at six."
        );
        assert_eq!(
            all("Hello team. Let's meet at five scratch that let's meet at six."),
            "Hello team. Let's meet at six."
        );
        assert_eq!(all("Scratch that. Start over."), "Start over.");
        assert_eq!(all("Send the report. No, scratch that."), "");
    }

    #[test]
    fn scratch_requires_the_full_phrase() {
        let text = "Don't scratch the paint. That cat will scratch.";
        assert_eq!(all(text), text);
    }

    #[test]
    fn spoken_line_breaks() {
        assert_eq!(
            all("Hi Sam, new line thanks for the update."),
            "Hi Sam\nThanks for the update."
        );
        assert_eq!(
            all("First point. New paragraph. Second point."),
            "First point.\n\nSecond point."
        );
    }

    #[test]
    fn numbered_lists() {
        assert_eq!(
            all("My list number one buy milk, number two call mom and number three book flights."),
            "My list:\n1. Buy milk\n2. Call mom\n3. Book flights"
        );
        assert_eq!(
            all("Number 1, eggs. Number 2, bread. That's all."),
            "1. Eggs\n2. Bread\nThat's all."
        );
    }

    #[test]
    fn isolated_or_unordered_numbers_are_not_lists() {
        let single = "This is our number one priority.";
        assert_eq!(all(single), single);
        let unordered = "Number two came before number one.";
        assert_eq!(all(unordered), unordered);
    }

    #[test]
    fn tidies_spacing() {
        assert_eq!(tidy("Hello ,  world ."), "Hello, world.");
    }

    #[test]
    fn settings_round_trip_and_default_on() {
        let decoded: SmartEditing = serde_json::from_str("{}").unwrap();
        assert_eq!(decoded, SmartEditing::default());
        assert!(decoded.is_enabled());
        let off: SmartEditing =
            serde_json::from_str(r#"{"remove_fillers":false,"scratch_that":false,"spoken_line_breaks":false,"numbered_lists":false,"end_with_period":false,"bangla_punctuation":false}"#)
                .unwrap();
        assert!(!off.is_enabled());
        // Settings written before 1.0.5 get the Bangla full stop on and digits kept.
        let old: SmartEditing = serde_json::from_str(r#"{"remove_fillers":false}"#).unwrap();
        assert!(old.bangla_punctuation);
        assert_eq!(old.bangla_digits, BanglaDigits::Keep);
    }
}
