//! Short notices Hello shows in a small window at the top right of the screen, like the
//! meeting offer: "Couldn't paste here", "Hello can't hear its shortcut". Any thread can
//! queue one; the main loop in `meeting_watcher` shows it and closes it after a while.

use std::sync::{Mutex, OnceLock};

use crate::desktop_ui::Glyph;

#[derive(Clone, Debug)]
pub struct Notice {
    pub icon: Glyph,
    pub title: &'static str,
    pub body: String,
}

fn pending() -> &'static Mutex<Option<Notice>> {
    static PENDING: OnceLock<Mutex<Option<Notice>>> = OnceLock::new();
    PENDING.get_or_init(|| Mutex::new(None))
}

/// Queues a notice. Only the latest one waits; the window shows one at a time.
pub fn show(icon: Glyph, title: &'static str, body: impl Into<String>) {
    let notice = Notice {
        icon,
        title,
        body: body.into(),
    };
    tracing::info!(title, body = %notice.body, "notice");
    *pending().lock().unwrap_or_else(|error| error.into_inner()) = Some(notice);
}

/// The next notice to show, if any. Called from the main loop.
pub(crate) fn take() -> Option<Notice> {
    pending()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take()
}
