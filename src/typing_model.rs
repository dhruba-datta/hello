//! The word model behind Hello Type: which word tends to follow which, learned only from
//! the owner's own writing (what they type, their dictations and their dictionary). Plain
//! counts, no download, and nothing leaves `typing-model.json`.
//!
//! Suggestions are deliberately conservative: a word is offered only when it has followed
//! the same two (or one) words often enough, so a fresh install stays silent until Hello
//! has seen how the owner writes.

use std::collections::HashMap;
use std::path::Path;

use color_eyre::eyre::{Result, WrapErr};
use serde::{Deserialize, Serialize};

/// The token that stands for the start of a sentence.
pub const SENTENCE_START: &str = "<s>";
/// Tokens longer than this are not words anyone types twice (links, keys, hashes).
const MAX_WORD_CHARS: usize = 30;
/// Suggest at most this many words in one go.
const MAX_SUGGESTED_WORDS: usize = 3;
/// Above this many distinct contexts the rarest ones are dropped.
const MAX_CONTEXTS: usize = 150_000;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct TypingModel {
    /// "first second" (lower case) → next word as typed → times seen.
    trigrams: HashMap<String, HashMap<String, u32>>,
    /// "previous" (lower case) → next word as typed → times seen.
    bigrams: HashMap<String, HashMap<String, u32>>,
    /// word as typed → times seen.
    unigrams: HashMap<String, u32>,
    /// Sentences learned so far, for the Typing page.
    pub sentences: u64,
    /// Suggestions accepted with Tab, for the Typing page.
    pub accepted: u64,
    /// Dictations up to this time have been learned.
    pub history_watermark_ms: u64,
    /// Unix milliseconds of the last thing learned.
    pub last_learned_ms: u64,
    #[serde(skip)]
    dirty: bool,
}

/// What Hello proposes to insert at the caret.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    /// The words, the first one complete even when the owner has typed part of it.
    pub words: Vec<String>,
    /// How many characters of the first word the owner has already typed.
    pub typed_prefix_chars: usize,
}

impl Suggestion {
    /// The text to type, after the characters already at the caret.
    pub fn insertion(&self) -> String {
        let mut text = self
            .words
            .first()
            .map(|word| {
                word.chars()
                    .skip(self.typed_prefix_chars)
                    .collect::<String>()
            })
            .unwrap_or_default();
        for word in self.words.iter().skip(1) {
            text.push(' ');
            text.push_str(word);
        }
        text
    }
}

impl TypingModel {
    pub fn load(path: &Path) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let contents = std::fs::read_to_string(path)
            .wrap_err_with(|| format!("could not read {}", path.display()))?;
        serde_json::from_str(&contents)
            .wrap_err_with(|| format!("could not parse {}", path.display()))
    }

    pub fn save(&mut self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .wrap_err_with(|| format!("could not create {}", parent.display()))?;
        }
        let contents = serde_json::to_string(self)?;
        std::fs::write(path, contents)
            .wrap_err_with(|| format!("could not write {}", path.display()))?;
        self.dirty = false;
        Ok(())
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Distinct words seen so far.
    pub fn word_count(&self) -> usize {
        self.unigrams.len()
    }

    pub fn note_accepted(&mut self) {
        self.accepted += 1;
        self.dirty = true;
    }

    /// Learns a whole text: sentences, in order.
    pub fn learn_text(&mut self, text: &str, now_ms: u64) {
        let mut tokens = tokens(text);
        if tokens.last().is_some_and(|last| last != SENTENCE_START) {
            tokens.push(SENTENCE_START.to_string());
        }
        self.learn_tokens(&[], &tokens, now_ms);
    }

    /// Learns `new` tokens that continue `previous` ones (the end of what was learned before
    /// in the same text field), so words that cross a snapshot boundary still count.
    pub fn learn_tokens(&mut self, previous: &[String], new: &[String], now_ms: u64) {
        if new.iter().all(|token| token == SENTENCE_START) {
            return;
        }
        let mut context: Vec<String> = previous.iter().rev().take(2).rev().cloned().collect();
        if context.is_empty() {
            context.push(SENTENCE_START.to_string());
        }
        for token in new {
            if token == SENTENCE_START {
                if context.last().is_some_and(|last| last != SENTENCE_START) {
                    self.sentences += 1;
                }
            } else {
                if let Some(previous) = context.last() {
                    bump(
                        self.bigrams.entry(previous.to_lowercase()).or_default(),
                        token,
                    );
                }
                if context.len() >= 2 {
                    let key = format!(
                        "{} {}",
                        context[context.len() - 2].to_lowercase(),
                        context[context.len() - 1].to_lowercase()
                    );
                    bump(self.trigrams.entry(key).or_default(), token);
                }
                *self.unigrams.entry(token.clone()).or_insert(0) += 1;
            }
            context.push(token.clone());
            if context.len() > 2 {
                context.remove(0);
            }
        }
        self.last_learned_ms = now_ms;
        self.dirty = true;
        if self.trigrams.len() + self.bigrams.len() > MAX_CONTEXTS {
            self.prune();
        }
    }

    /// Drops contexts seen only once; they never produce a suggestion anyway.
    fn prune(&mut self) {
        for map in [&mut self.trigrams, &mut self.bigrams] {
            map.retain(|_, nexts| {
                nexts.retain(|_, count| *count > 1);
                !nexts.is_empty()
            });
        }
    }

    /// The words to offer after `context` (the last tokens typed, oldest first, with
    /// `SENTENCE_START` where a sentence began) when the owner has typed `prefix` of the
    /// next word so far. `None` when nothing is likely enough.
    pub fn predict(&self, context: &[String], prefix: &str) -> Option<Suggestion> {
        let first = self.next_word(context, prefix)?;
        let typed_prefix_chars = prefix.chars().count();
        let mut words = vec![first];
        let mut extended: Vec<String> = context.to_vec();
        while words.len() < MAX_SUGGESTED_WORDS {
            extended.push(words[words.len() - 1].clone());
            let Some(next) = self.continuation(&extended) else {
                break;
            };
            words.push(next);
        }
        Some(Suggestion {
            words,
            typed_prefix_chars,
        })
    }

    fn next_word(&self, context: &[String], prefix: &str) -> Option<String> {
        let has_prefix = !prefix.is_empty();
        let lower_prefix = prefix.to_lowercase();
        let matches = |word: &str| word.to_lowercase().starts_with(&lower_prefix);
        // With part of the word typed, the share is measured among the words that fit it.
        if let Some(key) = trigram_key(context)
            && let Some(nexts) = self.trigrams.get(&key)
            && let Some(choice) = best(nexts, &matches, 2, if has_prefix { 0.5 } else { 0.3 })
        {
            return Some(choice);
        }
        if let Some(previous) = context.last()
            && let Some(nexts) = self.bigrams.get(&previous.to_lowercase())
            && let Some(choice) = best(nexts, &matches, 3, if has_prefix { 0.5 } else { 0.4 })
        {
            return Some(choice);
        }
        // Finishing a word the owner started needs no context, only a word seen often.
        if lower_prefix.chars().count() >= 3
            && let Some(choice) = best(&self.unigrams, &matches, 3, 0.5)
            && choice.chars().count() >= lower_prefix.chars().count() + 2
        {
            return Some(choice);
        }
        None
    }

    /// A further word, only when it almost always follows.
    fn continuation(&self, context: &[String]) -> Option<String> {
        let key = trigram_key(context)?;
        let nexts = self.trigrams.get(&key)?;
        best(nexts, &|_| true, 3, 0.6)
    }
}

fn bump(nexts: &mut HashMap<String, u32>, token: &str) {
    *nexts.entry(token.to_string()).or_insert(0) += 1;
}

fn trigram_key(context: &[String]) -> Option<String> {
    if context.len() < 2 {
        return None;
    }
    Some(format!(
        "{} {}",
        context[context.len() - 2].to_lowercase(),
        context[context.len() - 1].to_lowercase()
    ))
}

/// The most frequent word that `matches`, when it was seen at least `min_count` times and
/// makes up at least `min_share` of the matching words seen in this context.
fn best(
    nexts: &HashMap<String, u32>,
    matches: &dyn Fn(&str) -> bool,
    min_count: u32,
    min_share: f32,
) -> Option<String> {
    let candidates: Vec<(&String, &u32)> = nexts.iter().filter(|(word, _)| matches(word)).collect();
    let total: u32 = candidates.iter().map(|(_, count)| **count).sum();
    if total == 0 {
        return None;
    }
    let (word, count) = candidates
        .into_iter()
        .max_by(|left, right| left.1.cmp(right.1).then_with(|| right.0.cmp(left.0)))?;
    let share = *count as f32 / total as f32;
    (*count >= min_count && share >= min_share).then(|| word.clone())
}

/// Splits text into words, with `SENTENCE_START` before the first word and after every
/// sentence end (a full stop, "!", "?", "।" or a line break). Punctuation around words is
/// dropped; apostrophes and hyphens inside stay.
pub fn tokens(text: &str) -> Vec<String> {
    let mut tokens = vec![SENTENCE_START.to_string()];
    let lines: Vec<&str> = text.split('\n').collect();
    for (index, line) in lines.iter().enumerate() {
        for piece in line.split(char::is_whitespace) {
            if piece.is_empty() {
                continue;
            }
            let ends_sentence = piece
                .trim_end_matches(['"', '\'', ')', ']', '\u{201d}', '\u{2019}'])
                .ends_with(['.', '!', '?', '\u{0964}']);
            if let Some(word) = clean_word(piece) {
                tokens.push(word);
            }
            if ends_sentence && tokens.last().is_some_and(|last| last != SENTENCE_START) {
                tokens.push(SENTENCE_START.to_string());
            }
        }
        if index + 1 < lines.len() && tokens.last().is_some_and(|last| last != SENTENCE_START) {
            tokens.push(SENTENCE_START.to_string());
        }
    }
    tokens
}

/// The word inside a typed piece, or `None` when it is not a word worth learning.
pub fn clean_word(piece: &str) -> Option<String> {
    let word = piece
        .trim_matches(|character: char| !character.is_alphanumeric())
        .to_string();
    learnable(&word).then_some(word)
}

/// Words only: letters (with digits allowed inside, like "mp3"), not too long, and not
/// something that looks like a number, code, link or address.
fn learnable(word: &str) -> bool {
    let length = word.chars().count();
    if length == 0 || length > MAX_WORD_CHARS {
        return false;
    }
    if word.contains(['@', '/', '\\', ':', '=']) {
        return false;
    }
    let letters = word
        .chars()
        .filter(|character| character.is_alphabetic())
        .count();
    let digits = word
        .chars()
        .filter(|character| character.is_numeric())
        .count();
    letters > 0 && digits <= 2 && letters * 2 >= length
}

/// The complete tokens at the end of a text field plus the partly typed word, split at
/// the caret: `(tokens, prefix)`. The prefix is empty when the caret follows a space or
/// punctuation.
pub fn split_at_caret(text_before_caret: &str) -> (Vec<String>, String) {
    let ends_with_break = text_before_caret
        .chars()
        .next_back()
        .is_none_or(|character| !(character.is_alphanumeric() || character == '\''));
    if ends_with_break {
        return (tokens(text_before_caret), String::new());
    }
    let split = text_before_caret
        .char_indices()
        .rev()
        .find(|(_, character)| !(character.is_alphanumeric() || *character == '\''))
        .map_or(0, |(index, character)| index + character.len_utf8());
    let prefix = text_before_caret[split..].to_string();
    (tokens(&text_before_caret[..split]), prefix)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn learned(lines: &[&str]) -> TypingModel {
        let mut model = TypingModel::default();
        for line in lines {
            model.learn_text(line, 1);
        }
        model
    }

    fn context(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| word.to_string()).collect()
    }

    #[test]
    fn tokens_mark_sentences_and_drop_punctuation() {
        assert_eq!(
            tokens("Hello team. Let's meet at six!"),
            vec![
                "<s>", "Hello", "team", "<s>", "Let's", "meet", "at", "six", "<s>"
            ]
        );
        assert_eq!(
            tokens("আমি আসছি। তুমি?"),
            vec!["<s>", "আমি", "আসছি", "<s>", "তুমি", "<s>"]
        );
        assert_eq!(
            tokens("first line\nsecond"),
            vec!["<s>", "first", "line", "<s>", "second"]
        );
    }

    #[test]
    fn secrets_links_and_numbers_are_not_words() {
        assert_eq!(clean_word("hello,"), Some("hello".into()));
        assert_eq!(clean_word("4111111111111111"), None);
        assert_eq!(clean_word("dhruba@example.com"), None);
        assert_eq!(clean_word("https://hello.minutecampus.com"), None);
        assert_eq!(clean_word("sk-abcdefghijklmnopqrstuvwxyz0123456789"), None);
        assert_eq!(clean_word("mp3"), Some("mp3".into()));
    }

    #[test]
    fn a_fresh_model_suggests_nothing() {
        let model = TypingModel::default();
        assert_eq!(model.predict(&context(&["<s>", "I"]), ""), None);
        let once = learned(&["I will send the report"]);
        assert_eq!(once.predict(&context(&["I", "will"]), ""), None);
    }

    #[test]
    fn repeated_phrases_are_suggested_and_chained() {
        let model = learned(&[
            "I will send the report tomorrow.",
            "I will send the report tonight.",
            "I will send the invoice today.",
            "Please send the report now.",
        ]);
        let suggestion = model.predict(&context(&["<s>", "I", "will"]), "").unwrap();
        assert_eq!(suggestion.words, vec!["send", "the", "report"]);
        assert_eq!(suggestion.insertion(), "send the report");
        // "report" follows "send the" in 3 of 4 cases: offered after "the".
        assert_eq!(
            model.predict(&context(&["send", "the"]), "").unwrap().words,
            vec!["report"]
        );
    }

    #[test]
    fn a_started_word_is_completed() {
        let model = learned(&[
            "Best regards, Dhruba",
            "Best regards, Dhruba",
            "Best regards, Dhruba",
        ]);
        let (tokens, prefix) = split_at_caret("Best reg");
        assert_eq!(prefix, "reg");
        let suggestion = model.predict(&tokens, &prefix).unwrap();
        assert_eq!(suggestion.words[0], "regards");
        assert_eq!(suggestion.insertion(), "ards Dhruba");
        // A word seen often is completed even without context, after three letters.
        let (tokens, prefix) = split_at_caret("yours Dhr");
        assert_eq!(
            model.predict(&tokens, &prefix).unwrap().words,
            vec!["Dhruba"]
        );
        assert_eq!(model.predict(&tokens, "Dh"), None);
    }

    #[test]
    fn bangla_and_mixed_text_work() {
        let model = learned(&[
            "আমি আজ অফিসে যাব।",
            "আমি আজ অফিসে যাব।",
            "আমি আজ বাসায় থাকব।",
            "আমি আজ meeting এ আছি।",
        ]);
        let suggestion = model.predict(&context(&["<s>", "আমি"]), "").unwrap();
        assert_eq!(suggestion.words[0], "আজ");
        let (tokens, prefix) = split_at_caret("আমি আজ অফি");
        assert_eq!(prefix, "অফি");
        // "যাব" followed twice only: not confident enough to chain.
        assert_eq!(
            model.predict(&tokens, &prefix).unwrap().words,
            vec!["অফিসে"]
        );
    }

    #[test]
    fn learning_continues_across_snapshots() {
        let mut model = TypingModel::default();
        for _ in 0..3 {
            model.learn_tokens(
                &context(&["<s>", "see"]),
                &context(&["you", "soon", "<s>"]),
                1,
            );
        }
        assert_eq!(
            model.predict(&context(&["<s>", "see"]), "").unwrap().words,
            vec!["you", "soon"]
        );
        assert_eq!(model.sentences, 3);
    }

    #[test]
    fn saves_and_loads() {
        let directory = std::env::temp_dir().join(format!("hello-typing-{}", std::process::id()));
        let path = directory.join("typing-model.json");
        let mut model = learned(&["save me please", "save me please"]);
        assert!(model.is_dirty());
        model.save(&path).unwrap();
        assert!(!model.is_dirty());
        let loaded = TypingModel::load(&path).unwrap();
        assert_eq!(loaded.word_count(), 3);
        assert_eq!(loaded.sentences, 2);
        let _ = std::fs::remove_dir_all(directory);
    }
}
