//! Hello Type: suggestions while typing, in any app, learned from the owner's own writing.
//!
//! The keyboard tap (`suppression.rs`) reports key presses here. After a short pause the
//! worker reads the focused text field through Accessibility, learns the words that were
//! completed since the last look, and asks the model (`typing_model.rs`) for what comes
//! next. The main loop shows the answer in a small overlay at the caret
//! (`typing_overlay.rs`); Tab, swallowed by the tap while a suggestion shows, types it.
//!
//! Nothing here leaves the Mac. Password fields and secure input are never read, and
//! Hello's own window is skipped.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use color_eyre::eyre::{Result, eyre};
use serde::{Deserialize, Serialize};

use crate::accessibility::{self, ScreenRect};
use crate::history::History;
use crate::typing_model::{self, SENTENCE_START, Suggestion, TypingModel};

const TAB_KEY_CODE: u16 = 48;
const ESCAPE_KEY_CODE: u16 = 53;
const COMMAND_FLAG: u64 = 1 << 20;
const CONTROL_FLAG: u64 = 1 << 18;
const OPTION_FLAG: u64 = 1 << 19;
/// How long after the last key press the field is read.
const LOOK_DELAY: Duration = Duration::from_millis(160);
/// Characters read before the caret.
const TEXT_LIMIT: usize = 1500;
/// The end of the learned text kept to find where new text starts next time.
const TAIL_CHARS: usize = 80;
const SAVE_INTERVAL: Duration = Duration::from_secs(20);
const HISTORY_INTERVAL: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct TypingSettings {
    /// Suggest while typing (and learn, unless `learn_from_typing` is off).
    pub enabled: bool,
    /// Learn from what the owner types; off means dictations only.
    pub learn_from_typing: bool,
}

impl Default for TypingSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            learn_from_typing: true,
        }
    }
}

/// What the Typing page shows.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct TypingStats {
    pub words: usize,
    pub sentences: u64,
    pub accepted: u64,
    pub last_learned_ms: u64,
}

static ENABLED: AtomicBool = AtomicBool::new(false);
static LEARN: AtomicBool = AtomicBool::new(true);
/// A suggestion is on screen, so Tab belongs to Hello.
static SHOWING: AtomicBool = AtomicBool::new(false);
/// The key-up of a swallowed Tab must be swallowed too.
static SWALLOW_TAB_UP: AtomicBool = AtomicBool::new(false);
/// Logged once, so the log shows whether key events reach Hello Type at all.
static SEEN_KEY: AtomicBool = AtomicBool::new(false);
static KEYS: OnceLock<SyncSender<Key>> = OnceLock::new();
static STATS: Mutex<TypingStats> = Mutex::new(TypingStats {
    words: 0,
    sentences: 0,
    accepted: 0,
    last_learned_ms: 0,
});

pub fn apply_settings(settings: &TypingSettings) {
    ENABLED.store(settings.enabled, Ordering::Relaxed);
    LEARN.store(settings.learn_from_typing, Ordering::Relaxed);
    if !settings.enabled {
        SHOWING.store(false, Ordering::Relaxed);
    }
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

pub fn stats() -> TypingStats {
    *STATS.lock().unwrap_or_else(|error| error.into_inner())
}

/// Asks the worker to forget everything it learned (the file goes to the Trash).
pub fn forget() {
    let _ = KEYS.get().map(|keys| keys.try_send(Key::Forget));
}

pub enum KeyDecision {
    Pass,
    Swallow,
}

enum Key {
    Press { code: u16, flags: u64 },
    Accept,
    Forget,
}

/// Called on the event-tap thread for every physical key event. Never blocks.
pub fn observe_key(code: u16, down: bool, flags: u64) -> KeyDecision {
    if !enabled() {
        return KeyDecision::Pass;
    }
    let plain = flags & (COMMAND_FLAG | CONTROL_FLAG | OPTION_FLAG) == 0;
    if code == TAB_KEY_CODE && plain {
        if down && SHOWING.load(Ordering::Acquire) {
            SWALLOW_TAB_UP.store(true, Ordering::Release);
            send(Key::Accept);
            return KeyDecision::Swallow;
        }
        if !down && SWALLOW_TAB_UP.swap(false, Ordering::AcqRel) {
            return KeyDecision::Swallow;
        }
    }
    if down {
        if !SEEN_KEY.swap(true, Ordering::Relaxed) {
            tracing::info!("Hello Type sees key presses");
        }
        send(Key::Press { code, flags });
    }
    KeyDecision::Pass
}

fn send(key: Key) {
    if let Some(keys) = KEYS.get() {
        let _ = keys.try_send(key);
    }
}

/// What the main loop draws.
#[derive(Clone, Debug, PartialEq)]
pub enum OverlayCommand {
    Show {
        text: String,
        caret: Option<ScreenRect>,
    },
    Hide,
}

/// Starts the worker thread; the receiver carries overlay commands for the main loop.
pub fn start(history: Option<History>) -> Receiver<OverlayCommand> {
    let (keys, key_receiver) = mpsc::sync_channel(256);
    let _ = KEYS.set(keys);
    let (overlay, overlay_receiver) = mpsc::sync_channel(16);
    std::thread::Builder::new()
        .name("hello-type".into())
        .spawn(move || Worker::new(history, overlay).run(key_receiver))
        .expect("could not start the Hello Type worker");
    overlay_receiver
}

fn model_path() -> Result<PathBuf> {
    Ok(crate::app_paths::support_dir()?.join("typing-model.json"))
}

/// The text field being typed in, and how much of it has been learned.
struct Session {
    pid: i32,
    element: usize,
    /// The end of the text learned so far; new text starts after it.
    learned_tail: String,
    /// The last learned tokens, the context for the next ones.
    context: Vec<String>,
}

struct Current {
    insertion: String,
}

struct Worker {
    model: TypingModel,
    path: Option<PathBuf>,
    history: Option<History>,
    overlay: SyncSender<OverlayCommand>,
    session: Option<Session>,
    current: Option<Current>,
    look_at: Option<Instant>,
    last_save: Instant,
    last_history: Option<Instant>,
}

impl Worker {
    fn new(history: Option<History>, overlay: SyncSender<OverlayCommand>) -> Self {
        let path = match model_path() {
            Ok(path) => Some(path),
            Err(error) => {
                tracing::warn!(%error, "Hello Type has nowhere to keep its model");
                None
            }
        };
        let model = path
            .as_ref()
            .map(|path| match TypingModel::load(path) {
                Ok(model) => model,
                Err(error) => {
                    tracing::warn!(%error, "could not load the typing model; starting fresh");
                    TypingModel::default()
                }
            })
            .unwrap_or_default();
        let worker = Self {
            model,
            path,
            history,
            overlay,
            session: None,
            current: None,
            look_at: None,
            last_save: Instant::now(),
            last_history: None,
        };
        worker.publish_stats();
        worker
    }

    fn run(mut self, keys: Receiver<Key>) {
        loop {
            match keys.recv_timeout(Duration::from_millis(40)) {
                Ok(Key::Press { code, flags }) => self.key_pressed(code, flags),
                Ok(Key::Accept) => self.accept(),
                Ok(Key::Forget) => self.forget(),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            if self.look_at.is_some_and(|at| Instant::now() >= at) {
                self.look_at = None;
                if enabled() {
                    self.look();
                }
            }
            if !enabled() && self.current.is_some() {
                self.hide();
            }
            if self
                .last_history
                .is_none_or(|at| at.elapsed() >= HISTORY_INTERVAL)
            {
                self.last_history = Some(Instant::now());
                self.learn_history();
            }
            if self.model.is_dirty() && self.last_save.elapsed() >= SAVE_INTERVAL {
                self.save();
            }
        }
        self.save();
    }

    fn key_pressed(&mut self, code: u16, flags: u64) {
        self.hide();
        let shortcut = flags & (COMMAND_FLAG | CONTROL_FLAG) != 0;
        if shortcut || code == ESCAPE_KEY_CODE {
            self.look_at = None;
        } else {
            self.look_at = Some(Instant::now() + LOOK_DELAY);
        }
    }

    fn accept(&mut self) {
        let Some(current) = self.current.take() else {
            SHOWING.store(false, Ordering::Release);
            return;
        };
        SHOWING.store(false, Ordering::Release);
        let _ = self.overlay.try_send(OverlayCommand::Hide);
        if let Err(error) = crate::keyboard::type_text(&current.insertion) {
            tracing::warn!(%error, "could not type the accepted suggestion");
            return;
        }
        tracing::debug!(
            chars = current.insertion.chars().count(),
            "Hello Type suggestion accepted"
        );
        self.model.note_accepted();
        self.publish_stats();
        // The typed words are learned from the field on the next look, like any typing.
        self.look_at = None;
    }

    fn hide(&mut self) {
        if self.current.take().is_some() || SHOWING.load(Ordering::Acquire) {
            SHOWING.store(false, Ordering::Release);
            let _ = self.overlay.try_send(OverlayCommand::Hide);
        }
    }

    /// Reads the focused field: learns what was completed, then suggests what comes next.
    fn look(&mut self) {
        if crate::suppression::secure_input_enabled() {
            self.session = None;
            return;
        }
        let Some(focused) = accessibility::focused_text(TEXT_LIMIT) else {
            if self.session.take().is_some() {
                tracing::debug!("Hello Type: the focused element exposes no text");
            }
            return;
        };
        if focused.secure || focused.pid == std::process::id() as i32 {
            self.session = None;
            return;
        }
        let text = focused.text_before_caret.as_str();
        let complete_end = complete_end(text);
        let complete = &text[..complete_end];
        let switched = self
            .session
            .as_ref()
            .is_none_or(|session| session.pid != focused.pid || session.element != focused.element);
        if switched {
            // Text that was already there is not learned; only what is typed from now on.
            self.session = Some(Session {
                pid: focused.pid,
                element: focused.element,
                learned_tail: tail(complete),
                context: context_tokens(complete),
            });
        } else if LEARN.load(Ordering::Relaxed) {
            self.learn_new_text(complete);
        } else if let Some(session) = &mut self.session {
            session.learned_tail = tail(complete);
            session.context = context_tokens(complete);
        }
        self.suggest(text, focused.caret);
    }

    fn learn_new_text(&mut self, complete: &str) {
        let Some(session) = &mut self.session else {
            return;
        };
        let new_part = if session.learned_tail.is_empty() {
            Some(complete)
        } else {
            complete
                .rfind(&session.learned_tail)
                .map(|position| &complete[position + session.learned_tail.len()..])
        };
        let Some(new_part) = new_part else {
            // The text changed under the learned part (deleted or moved); start over
            // from here without counting anything twice.
            session.learned_tail = tail(complete);
            session.context = context_tokens(complete);
            return;
        };
        if new_part.trim().is_empty() {
            return;
        }
        let mut tokens = typing_model::tokens(new_part);
        if !session.context.is_empty()
            && tokens.first().is_some_and(|first| first == SENTENCE_START)
        {
            tokens.remove(0);
        }
        if tokens.is_empty() {
            return;
        }
        self.model
            .learn_tokens(&session.context, &tokens, crate::events::now_ms());
        session.context.extend(tokens);
        let keep = session.context.len().saturating_sub(2);
        session.context.drain(..keep);
        session.learned_tail = tail(complete);
        self.publish_stats();
    }

    fn suggest(&mut self, text: &str, caret: Option<ScreenRect>) {
        let (context, prefix) = typing_model::split_at_caret(text);
        let Some(suggestion) = self.model.predict(&context, &prefix) else {
            self.hide();
            return;
        };
        let insertion = insertion_text(text, &suggestion);
        if insertion.trim().is_empty() {
            self.hide();
            return;
        }
        // Only the size is logged: the words are the owner's own typing.
        tracing::debug!(
            words = insertion.split_whitespace().count(),
            has_caret = caret.is_some(),
            "Hello Type suggestion"
        );
        self.current = Some(Current {
            insertion: insertion.clone(),
        });
        SHOWING.store(true, Ordering::Release);
        let _ = self.overlay.try_send(OverlayCommand::Show {
            text: insertion,
            caret,
        });
    }

    /// Dictations are the owner's words too; new ones are learned as they arrive.
    fn learn_history(&mut self) {
        let Some(history) = &self.history else {
            return;
        };
        let watermark = self.model.history_watermark_ms;
        let mut newest = watermark;
        let entries = history.search("");
        for entry in entries.iter().rev() {
            if entry.timestamp_ms <= watermark {
                continue;
            }
            self.model.learn_text(&entry.final_text, entry.timestamp_ms);
            newest = newest.max(entry.timestamp_ms);
        }
        if newest > watermark {
            self.model.history_watermark_ms = newest;
            self.publish_stats();
        }
    }

    fn forget(&mut self) {
        self.hide();
        self.session = None;
        if let Some(path) = &self.path
            && path.exists()
            && let Err(error) = trash_file(path)
        {
            tracing::warn!(%error, "could not move the typing model to the Trash");
        }
        self.model = TypingModel::default();
        // Only what is written from now on is learned, dictations included.
        self.model.history_watermark_ms = crate::events::now_ms();
        self.save();
        self.publish_stats();
        tracing::info!("Hello Type forgot everything it learned");
    }

    fn save(&mut self) {
        self.last_save = Instant::now();
        if let Some(path) = &self.path
            && let Err(error) = self.model.save(path)
        {
            tracing::warn!(%error, "could not save the typing model");
        }
    }

    fn publish_stats(&self) {
        *STATS.lock().unwrap_or_else(|error| error.into_inner()) = TypingStats {
            words: self.model.word_count(),
            sentences: self.model.sentences,
            accepted: self.model.accepted,
            last_learned_ms: self.model.last_learned_ms,
        };
    }
}

/// Where the complete words end: a word still being typed at the caret is left out.
fn complete_end(text: &str) -> usize {
    let last_is_word = text
        .chars()
        .next_back()
        .is_some_and(|character| character.is_alphanumeric() || character == '\'');
    if !last_is_word {
        return text.len();
    }
    text.char_indices()
        .rev()
        .find(|(_, character)| !(character.is_alphanumeric() || *character == '\''))
        .map_or(0, |(index, character)| index + character.len_utf8())
}

fn tail(text: &str) -> String {
    let count = text.chars().count();
    text.chars()
        .skip(count.saturating_sub(TAIL_CHARS))
        .collect()
}

fn context_tokens(text: &str) -> Vec<String> {
    let mut tokens = typing_model::tokens(text);
    let keep = tokens.len().saturating_sub(2);
    tokens.drain(..keep);
    tokens
}

/// The characters to type for a suggestion: a space first when the caret follows a word
/// or punctuation, nothing extra while a started word is being completed.
fn insertion_text(text_before_caret: &str, suggestion: &Suggestion) -> String {
    let insertion = suggestion.insertion();
    let continuing_word = suggestion.typed_prefix_chars > 0;
    let needs_space = !continuing_word
        && text_before_caret
            .chars()
            .next_back()
            .is_some_and(|character| {
                !character.is_whitespace()
                    && !matches!(character, '(' | '[' | '{' | '"' | '\u{201c}')
            });
    if needs_space {
        format!(" {insertion}")
    } else {
        insertion
    }
}

/// Moves a file to the Trash (Hello never deletes the owner's data outright).
fn trash_file(path: &std::path::Path) -> Result<()> {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject, Bool};
    use objc2_foundation::NSString;
    let path = NSString::from_str(&path.to_string_lossy());
    let manager_class =
        AnyClass::get(c"NSFileManager").ok_or_else(|| eyre!("NSFileManager is unavailable"))?;
    let url_class = AnyClass::get(c"NSURL").ok_or_else(|| eyre!("NSURL is unavailable"))?;
    let moved: Bool = unsafe {
        let manager: *mut AnyObject = msg_send![manager_class, defaultManager];
        let url: *mut AnyObject = msg_send![url_class, fileURLWithPath: &*path];
        msg_send![
            manager,
            trashItemAtURL: url,
            resultingItemURL: std::ptr::null_mut::<*mut AnyObject>(),
            error: std::ptr::null_mut::<*mut AnyObject>()
        ]
    };
    moved
        .as_bool()
        .then_some(())
        .ok_or_else(|| eyre!("the file could not be moved to the Trash"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_words_stop_before_the_one_being_typed() {
        assert_eq!(complete_end("see you so"), 8);
        assert_eq!(complete_end("see you "), 8);
        assert_eq!(complete_end("see you."), 8);
        assert_eq!(complete_end("আমি আজ"), "আমি ".len());
        assert_eq!(complete_end("word"), 0);
    }

    #[test]
    fn insertion_adds_a_space_only_between_words() {
        let next = Suggestion {
            words: vec!["the".into(), "report".into()],
            typed_prefix_chars: 0,
        };
        assert_eq!(insertion_text("I will send", &next), " the report");
        assert_eq!(insertion_text("I will send ", &next), "the report");
        assert_eq!(insertion_text("Done.", &next), " the report");
        let completion = Suggestion {
            words: vec!["regards".into()],
            typed_prefix_chars: 3,
        };
        assert_eq!(insertion_text("Best reg", &completion), "ards");
    }

    #[test]
    fn context_keeps_the_last_two_tokens() {
        assert_eq!(context_tokens("one two three "), vec!["two", "three"]);
        assert_eq!(context_tokens(""), vec![SENTENCE_START]);
        assert_eq!(context_tokens("Done. "), vec!["Done", SENTENCE_START]);
    }
}
