//! The personal dictionary: names and words Hello should write the owner's way, and
//! snippets, short phrases that paste a saved text.
//!
//! Entries live in `dictionary.json` next to the settings. Models that take recognition
//! hints hear the entries before every dictation; every model gets a fix-up pass on the
//! text afterwards: "heard as" corrections, the dictionary's own spelling and capitals for
//! words that came out close, and snippet expansion.

use std::sync::{Arc, RwLock};

use color_eyre::eyre::{Result, WrapErr};
use serde::{Deserialize, Serialize};

use crate::app_settings::TextReplacement;
use crate::text_replacements::ReplacementSet;

/// Whisper-family prompts are short, so the hints stop here.
const HINT_LIMIT: usize = 600;
/// A word this short is left alone by the fuzzy pass; too many words are one letter apart.
const FUZZY_MIN_CHARS: usize = 4;

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct DictionaryEntry {
    /// The word or phrase, written the way it should come out. For a snippet, the
    /// phrase that triggers it.
    pub phrase: String,
    /// What the model tends to write instead, separated by commas. Optional.
    pub heard_as: String,
    /// Saying the phrase pastes `body` instead of the phrase.
    pub snippet: bool,
    pub body: String,
    pub starred: bool,
    /// Unix milliseconds when the entry was added.
    pub added_ms: u64,
}

impl DictionaryEntry {
    pub fn new(phrase: &str) -> Self {
        Self {
            phrase: phrase.trim().to_string(),
            added_ms: now_ms(),
            ..Default::default()
        }
    }

    pub fn heard_as_list(&self) -> Vec<String> {
        self.heard_as
            .split([',', ';', '\n'])
            .map(str::trim)
            .filter(|variant| !variant.is_empty())
            .map(str::to_string)
            .collect()
    }

    fn is_valid(&self) -> bool {
        !self.phrase.trim().is_empty() && (!self.snippet || !self.body.trim().is_empty())
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
pub struct Dictionary {
    pub entries: Vec<DictionaryEntry>,
}

impl Dictionary {
    pub fn load() -> Result<Self> {
        let path = path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let contents = std::fs::read_to_string(&path)
            .wrap_err_with(|| format!("could not read {}", path.display()))?;
        serde_json::from_str(&contents)
            .wrap_err_with(|| format!("could not parse {}", path.display()))
    }

    /// Writes the file and makes the entries active for the next dictation.
    pub fn save(&self) -> Result<()> {
        let path = path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .wrap_err_with(|| format!("could not create {}", parent.display()))?;
        }
        let contents = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, contents)
            .wrap_err_with(|| format!("could not write {}", path.display()))?;
        install(self);
        Ok(())
    }

    /// Starred entries first, then newest first: the order the Dictionary page shows.
    pub fn sorted(&self) -> Vec<(usize, &DictionaryEntry)> {
        let mut entries: Vec<_> = self.entries.iter().enumerate().collect();
        entries.sort_by(|(_, left), (_, right)| {
            right
                .starred
                .cmp(&left.starred)
                .then_with(|| right.added_ms.cmp(&left.added_ms))
        });
        entries
    }
}

fn path() -> Result<std::path::PathBuf> {
    Ok(crate::app_paths::support_dir()?.join("dictionary.json"))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

/// The entries prepared for the recognition thread.
#[derive(Default)]
struct Compiled {
    hints: String,
    /// "Heard as" variants and the dictionary's own casing, as whole-phrase rules.
    replacements: ReplacementSet,
    /// Single words the fuzzy pass may snap close misspellings to.
    words: Vec<String>,
    /// Snippet triggers, normalised, with their bodies.
    snippets: Vec<(String, String)>,
}

fn compiled() -> &'static RwLock<Arc<Compiled>> {
    static COMPILED: std::sync::OnceLock<RwLock<Arc<Compiled>>> = std::sync::OnceLock::new();
    COMPILED.get_or_init(|| RwLock::new(Arc::new(Compiled::default())))
}

fn current() -> Arc<Compiled> {
    compiled()
        .read()
        .unwrap_or_else(|error| error.into_inner())
        .clone()
}

/// Loads the dictionary at startup; a broken file leaves the dictionary empty.
pub fn load_at_startup() {
    match Dictionary::load() {
        Ok(dictionary) => install(&dictionary),
        Err(error) => tracing::warn!(%error, "could not load the personal dictionary"),
    }
}

fn install(dictionary: &Dictionary) {
    let prepared = Arc::new(compile(dictionary));
    *compiled()
        .write()
        .unwrap_or_else(|error| error.into_inner()) = prepared;
}

fn compile(dictionary: &Dictionary) -> Compiled {
    let entries: Vec<_> = dictionary
        .sorted()
        .into_iter()
        .map(|(_, entry)| entry)
        .filter(|entry| entry.is_valid())
        .collect();
    let mut hints = String::new();
    for entry in &entries {
        let phrase = entry.phrase.trim();
        let addition = phrase.len() + 2;
        if !hints.is_empty() && hints.len() + addition > HINT_LIMIT {
            break;
        }
        if !hints.is_empty() {
            hints.push_str(", ");
        }
        hints.push_str(phrase);
    }
    let mut rules = Vec::new();
    for entry in &entries {
        let phrase = entry.phrase.trim().to_string();
        for heard in entry.heard_as_list() {
            rules.push(TextReplacement {
                matched_phrase: heard,
                output: phrase.clone(),
            });
        }
        // Matching is case-insensitive, so this rule only fixes the capitals.
        if !entry.snippet {
            rules.push(TextReplacement {
                matched_phrase: phrase.clone(),
                output: phrase,
            });
        }
    }
    let words = entries
        .iter()
        .filter(|entry| !entry.snippet)
        .map(|entry| entry.phrase.trim())
        .filter(|phrase| {
            !phrase.contains(char::is_whitespace)
                && phrase.chars().count() >= FUZZY_MIN_CHARS
                && phrase.chars().all(|character| character.is_alphabetic())
        })
        .map(str::to_string)
        .collect();
    let snippets = entries
        .iter()
        .filter(|entry| entry.snippet)
        .map(|entry| (normalize(&entry.phrase), entry.body.trim().to_string()))
        .filter(|(trigger, _)| !trigger.is_empty())
        .collect();
    Compiled {
        hints,
        replacements: ReplacementSet::new(&rules),
        words,
        snippets,
    }
}

/// The dictionary as a recognition hint, combined with the hints from Settings.
pub fn hints_with(settings_hints: &str) -> Option<String> {
    let dictionary = current();
    let settings_hints = settings_hints.trim();
    let combined = match (settings_hints.is_empty(), dictionary.hints.is_empty()) {
        (true, true) => return None,
        (false, true) => settings_hints.to_string(),
        (true, false) => dictionary.hints.clone(),
        (false, false) => format!("{settings_hints}, {}", dictionary.hints),
    };
    Some(combined)
}

/// Applies the dictionary to a recognised text. Snippets only expand when `snippets` is
/// set, which is the case for text that goes to the caret.
pub fn apply(text: &str, snippets: bool) -> String {
    apply_with(&current(), text, snippets)
}

fn apply_with(compiled: &Compiled, text: &str, snippets: bool) -> String {
    if text.trim().is_empty() {
        return text.to_string();
    }
    let corrected = compiled.replacements.replace(text);
    let corrected = fuzzy_fix(&corrected, &compiled.words);
    if snippets && let Some(expanded) = expand_snippets(&corrected, &compiled.snippets) {
        return expanded;
    }
    corrected
}

/// Replaces words that are one or two letters away from a dictionary word.
fn fuzzy_fix(text: &str, words: &[String]) -> String {
    if words.is_empty() {
        return text.to_string();
    }
    let mut output = String::with_capacity(text.len());
    let mut token = String::new();
    let flush = |token: &mut String, output: &mut String| {
        if !token.is_empty() {
            output.push_str(&snap(token, words));
            token.clear();
        }
    };
    for character in text.chars() {
        if character.is_alphabetic() || character == '\'' {
            token.push(character);
        } else {
            flush(&mut token, &mut output);
            output.push(character);
        }
    }
    flush(&mut token, &mut output);
    output
}

fn snap(token: &str, words: &[String]) -> String {
    let length = token.chars().count();
    if length < FUZZY_MIN_CHARS {
        return token.to_string();
    }
    let allowed = if length >= 8 { 2 } else { 1 };
    let lowered = token.to_lowercase();
    let mut best: Option<(usize, &String)> = None;
    for word in words {
        let candidate = word.to_lowercase();
        if candidate == lowered {
            return token.to_string();
        }
        // Only words of a similar length can be close.
        if word.chars().count().abs_diff(length) > allowed {
            continue;
        }
        let distance = edit_distance(&lowered, &candidate);
        if distance <= allowed && best.is_none_or(|(best_distance, _)| distance < best_distance) {
            best = Some((distance, word));
        }
    }
    match best {
        Some((_, word)) => keep_initial_capital(token, word),
        None => token.to_string(),
    }
}

/// A capitalised token at a sentence start keeps its capital after the swap, unless the
/// dictionary word has its own capitals.
fn keep_initial_capital(token: &str, word: &str) -> String {
    let dictionary_has_capitals = word.chars().any(char::is_uppercase);
    let token_capitalised = token.chars().next().is_some_and(char::is_uppercase);
    if dictionary_has_capitals || !token_capitalised {
        return word.to_string();
    }
    let mut characters = word.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().chain(characters).collect(),
        None => String::new(),
    }
}

fn edit_distance(left: &str, right: &str) -> usize {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut current = vec![0; right.len() + 1];
    for (row, &left_character) in left.iter().enumerate() {
        current[0] = row + 1;
        for (column, &right_character) in right.iter().enumerate() {
            let substitution = previous[column] + usize::from(left_character != right_character);
            current[column + 1] = substitution
                .min(previous[column + 1] + 1)
                .min(current[column] + 1);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

/// Lower case, letters and digits only, single spaces: how triggers are compared.
fn normalize(text: &str) -> String {
    text.to_lowercase()
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The whole text, or each sentence of it, that is a snippet trigger becomes the snippet.
fn expand_snippets(text: &str, snippets: &[(String, String)]) -> Option<String> {
    if snippets.is_empty() {
        return None;
    }
    let lookup = |candidate: &str| {
        let normalized = normalize(candidate);
        snippets
            .iter()
            .find(|(trigger, _)| *trigger == normalized)
            .map(|(_, body)| body.clone())
    };
    if let Some(body) = lookup(text) {
        return Some(body);
    }
    let mut output = String::new();
    let mut replaced = false;
    let mut sentence = String::new();
    for character in text.chars() {
        sentence.push(character);
        if matches!(character, '.' | '!' | '?' | '\u{0964}' | '\n') {
            output.push_str(&expand_sentence(&sentence, &lookup, &mut replaced));
            sentence.clear();
        }
    }
    output.push_str(&expand_sentence(&sentence, &lookup, &mut replaced));
    replaced.then_some(output)
}

fn expand_sentence(
    sentence: &str,
    lookup: &impl Fn(&str) -> Option<String>,
    replaced: &mut bool,
) -> String {
    let Some(body) = lookup(sentence) else {
        return sentence.to_string();
    };
    *replaced = true;
    let leading: String = sentence
        .chars()
        .take_while(|character| character.is_whitespace())
        .collect();
    let trailing: String = sentence
        .chars()
        .rev()
        .take_while(|character| *character == '\n')
        .collect();
    format!("{leading}{body}{trailing}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(phrase: &str) -> DictionaryEntry {
        DictionaryEntry::new(phrase)
    }

    fn dictionary(entries: Vec<DictionaryEntry>) -> Compiled {
        compile(&Dictionary { entries })
    }

    #[test]
    fn heard_as_variants_and_capitals_are_fixed() {
        let mut dhruba = entry("Dhruba");
        dhruba.heard_as = "Druba, Drooba".into();
        let compiled = dictionary(vec![dhruba, entry("GitHub")]);
        assert_eq!(
            apply_with(&compiled, "tell druba to open github today", true),
            "tell Dhruba to open GitHub today"
        );
    }

    #[test]
    fn close_misspellings_snap_to_dictionary_words() {
        let compiled = dictionary(vec![entry("Optify"), entry("Minutecampus")]);
        assert_eq!(
            apply_with(&compiled, "Optifi and minute campus meet", true),
            "Optify and minute campus meet"
        );
        assert_eq!(
            apply_with(&compiled, "minutecampos is live.", true),
            "Minutecampus is live."
        );
        // Short common words stay.
        assert_eq!(apply_with(&compiled, "opt in now", true), "opt in now");
    }

    #[test]
    fn snippets_expand_whole_dictations_and_sentences() {
        let mut signature = entry("my signature");
        signature.snippet = true;
        signature.body = "Best,\nDhruba".into();
        let compiled = dictionary(vec![signature]);
        assert_eq!(
            apply_with(&compiled, "My signature.", true),
            "Best,\nDhruba"
        );
        assert_eq!(
            apply_with(&compiled, "Thanks for the call. My signature.", true),
            "Thanks for the call. Best,\nDhruba"
        );
        assert_eq!(
            apply_with(&compiled, "My signature.", false),
            "My signature."
        );
    }

    #[test]
    fn hints_combine_settings_and_dictionary() {
        let compiled = dictionary(vec![entry("Optify"), entry("Dhruba")]);
        assert_eq!(compiled.hints, "Optify, Dhruba");
        let mut starred = entry("Later");
        starred.starred = true;
        let compiled = dictionary(vec![entry("First"), starred]);
        assert_eq!(compiled.hints, "Later, First");
    }

    #[test]
    fn invalid_entries_are_skipped() {
        let mut blank_snippet = entry("sign off");
        blank_snippet.snippet = true;
        let compiled = dictionary(vec![entry("  "), blank_snippet]);
        assert!(compiled.hints.is_empty());
        assert!(compiled.snippets.is_empty());
    }
}
