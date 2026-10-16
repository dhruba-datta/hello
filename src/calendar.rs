//! Reads the calendar event behind a meeting through EventKit, so meeting notes
//! carry the event's title, organizer and attendees. Nothing leaves this Mac.

use std::ffi::{CStr, c_char};

use objc2::rc::autoreleasepool;
use objc2::runtime::{AnyClass, AnyObject, Bool};
use objc2::{msg_send, rc::Retained};
use serde::{Deserialize, Serialize};

#[link(name = "EventKit", kind = "framework")]
unsafe extern "C" {
    static EKErrorDomain: *const AnyObject;
}

const ENTITY_EVENT: usize = 0;
/// Events this far either side of now still count as "this meeting".
const MATCH_WINDOW_MS: u64 = 15 * 60 * 1_000;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct CalendarEvent {
    pub title: String,
    #[serde(default)]
    pub calendar: Option<String>,
    pub starts_at_ms: u64,
    pub ends_at_ms: u64,
    #[serde(default)]
    pub location: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub organizer: Option<Person>,
    #[serde(default)]
    pub attendees: Vec<Person>,
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct Person {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub is_you: bool,
    #[serde(default)]
    pub response: Option<String>,
}

impl Person {
    pub fn label(&self) -> String {
        self.name
            .clone()
            .filter(|name| !name.trim().is_empty())
            .or_else(|| self.email.clone())
            .unwrap_or_else(|| "Guest".into())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CalendarAccess {
    NotAsked,
    Denied,
    Granted,
}

pub fn access() -> CalendarAccess {
    let Some(class) = AnyClass::get(c"EKEventStore") else {
        return CalendarAccess::Denied;
    };
    // Keeps the EventKit framework linked.
    let _ = unsafe { EKErrorDomain };
    let status: isize = unsafe { msg_send![class, authorizationStatusForEntityType: ENTITY_EVENT] };
    match status {
        0 => CalendarAccess::NotAsked,
        3 => CalendarAccess::Granted,
        _ => CalendarAccess::Denied,
    }
}

/// Shows the macOS calendar permission prompt; `done` runs with the answer.
pub fn request_access(done: impl Fn(bool) + Send + Sync + 'static) {
    let Some(class) = AnyClass::get(c"EKEventStore") else {
        done(false);
        return;
    };
    let store: Retained<AnyObject> = unsafe { msg_send![class, new] };
    let handler = block2::RcBlock::new(move |granted: Bool, _error: *mut AnyObject| {
        tracing::info!(
            granted = granted.as_bool(),
            "calendar permission request finished"
        );
        done(granted.as_bool());
    });
    unsafe {
        let _: () = msg_send![&*store, requestFullAccessToEventsWithCompletion: &*handler];
    }
    // The store must outlive the asynchronous request.
    std::mem::forget(store);
}

/// The calendar event happening around `now_ms`, preferring one whose link or
/// location mentions any of `hints` (a meeting code, a host, a window title).
pub fn current_event(now_ms: u64, hints: &[String]) -> Option<CalendarEvent> {
    if access() != CalendarAccess::Granted {
        return None;
    }
    let events = autoreleasepool(|_| {
        events_between(
            now_ms.saturating_sub(12 * 60 * 60 * 1_000),
            now_ms + 12 * 60 * 60 * 1_000,
        )
    });
    choose_event(events, now_ms, hints)
}

fn choose_event(
    events: Vec<CalendarEvent>,
    now_ms: u64,
    hints: &[String],
) -> Option<CalendarEvent> {
    let hints: Vec<String> = hints
        .iter()
        .map(|hint| hint.trim().to_lowercase())
        .filter(|hint| hint.len() >= 4)
        .collect();
    events
        .into_iter()
        .filter(|event| {
            event.starts_at_ms <= now_ms + MATCH_WINDOW_MS
                && event.ends_at_ms + MATCH_WINDOW_MS >= now_ms
        })
        .map(|event| {
            let haystack = format!(
                "{} {} {}",
                event.title.to_lowercase(),
                event.url.as_deref().unwrap_or("").to_lowercase(),
                event.location.as_deref().unwrap_or("").to_lowercase()
            );
            let linked = hints.iter().any(|hint| haystack.contains(hint.as_str()));
            let ongoing = event.starts_at_ms <= now_ms && now_ms <= event.ends_at_ms;
            let distance = event.starts_at_ms.abs_diff(now_ms);
            let flags = u64::from(linked) * 4
                + u64::from(ongoing) * 2
                + u64::from(!event.attendees.is_empty());
            let score = flags * 1_000_000_000 + (999_999_999 - distance.min(999_999_999));
            (score, event)
        })
        .max_by_key(|(score, _)| *score)
        .map(|(_, event)| event)
}

fn events_between(start_ms: u64, end_ms: u64) -> Vec<CalendarEvent> {
    let Some(class) = AnyClass::get(c"EKEventStore") else {
        return Vec::new();
    };
    let Some(date_class) = AnyClass::get(c"NSDate") else {
        return Vec::new();
    };
    unsafe {
        let store: Retained<AnyObject> = msg_send![class, new];
        let start: *mut AnyObject =
            msg_send![date_class, dateWithTimeIntervalSince1970: start_ms as f64 / 1_000.0];
        let end: *mut AnyObject =
            msg_send![date_class, dateWithTimeIntervalSince1970: end_ms as f64 / 1_000.0];
        let predicate: *mut AnyObject = msg_send![
            &*store,
            predicateForEventsWithStartDate: start,
            endDate: end,
            calendars: std::ptr::null_mut::<AnyObject>()
        ];
        if predicate.is_null() {
            return Vec::new();
        }
        let events: *mut AnyObject = msg_send![&*store, eventsMatchingPredicate: predicate];
        array(events)
            .into_iter()
            .filter_map(|event| read_event(event))
            .collect()
    }
}

unsafe fn read_event(event: *mut AnyObject) -> Option<CalendarEvent> {
    unsafe {
        let all_day: Bool = msg_send![event, isAllDay];
        if all_day.as_bool() {
            return None;
        }
        let title = string(msg_send![event, title]).unwrap_or_else(|| "Untitled event".into());
        let calendar: *mut AnyObject = msg_send![event, calendar];
        let calendar = (!calendar.is_null())
            .then(|| string(msg_send![calendar, title]))
            .flatten();
        let url: *mut AnyObject = msg_send![event, URL];
        let url = (!url.is_null())
            .then(|| string(msg_send![url, absoluteString]))
            .flatten();
        let organizer: *mut AnyObject = msg_send![event, organizer];
        let organizer = (!organizer.is_null()).then(|| person(organizer));
        let attendees: *mut AnyObject = msg_send![event, attendees];
        Some(CalendarEvent {
            title,
            calendar,
            starts_at_ms: date_ms(msg_send![event, startDate]),
            ends_at_ms: date_ms(msg_send![event, endDate]),
            location: string(msg_send![event, location]).filter(|text| !text.is_empty()),
            url: url
                .or_else(|| string(msg_send![event, notes]).and_then(|notes| meeting_link(&notes))),
            organizer,
            attendees: array(attendees)
                .into_iter()
                .map(|item| person(item))
                .collect(),
        })
    }
}

unsafe fn person(participant: *mut AnyObject) -> Person {
    unsafe {
        let url: *mut AnyObject = msg_send![participant, URL];
        let email = (!url.is_null())
            .then(|| string(msg_send![url, absoluteString]))
            .flatten()
            .map(|address| address.trim_start_matches("mailto:").to_string())
            .filter(|address| address.contains('@'));
        let is_you: Bool = msg_send![participant, isCurrentUser];
        let status: isize = msg_send![participant, participantStatus];
        Person {
            name: string(msg_send![participant, name]).filter(|name| !name.is_empty()),
            email,
            is_you: is_you.as_bool(),
            response: match status {
                2 => Some("Accepted".into()),
                3 => Some("Declined".into()),
                4 => Some("Maybe".into()),
                _ => None,
            },
        }
    }
}

unsafe fn array(value: *mut AnyObject) -> Vec<*mut AnyObject> {
    if value.is_null() {
        return Vec::new();
    }
    unsafe {
        let count: usize = msg_send![value, count];
        (0..count)
            .map(|index| msg_send![value, objectAtIndex: index])
            .collect()
    }
}

unsafe fn string(value: *mut AnyObject) -> Option<String> {
    if value.is_null() {
        return None;
    }
    let bytes: *const c_char = unsafe { msg_send![value, UTF8String] };
    (!bytes.is_null()).then(|| {
        unsafe { CStr::from_ptr(bytes) }
            .to_string_lossy()
            .into_owned()
    })
}

unsafe fn date_ms(value: *mut AnyObject) -> u64 {
    if value.is_null() {
        return 0;
    }
    let seconds: f64 = unsafe { msg_send![value, timeIntervalSince1970] };
    (seconds.max(0.0) * 1_000.0) as u64
}

/// The first video-call link in free text such as an invitation's notes.
pub fn meeting_link(text: &str) -> Option<String> {
    text.split(|character: char| {
        character.is_whitespace() || matches!(character, '<' | '>' | '"' | '(' | ')')
    })
    .find(|word| {
        word.starts_with("https://")
            && [
                "meet.google.com/",
                "zoom.us/",
                "teams.microsoft.com/",
                "teams.live.com/",
                "webex.com/",
            ]
            .iter()
            .any(|host| word.contains(host))
    })
    .map(|word| word.trim_end_matches(['.', ',', ';']).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(title: &str, starts: u64, ends: u64, url: Option<&str>) -> CalendarEvent {
        CalendarEvent {
            title: title.into(),
            starts_at_ms: starts,
            ends_at_ms: ends,
            url: url.map(Into::into),
            ..CalendarEvent::default()
        }
    }

    #[test]
    fn prefers_the_event_linked_to_the_call() {
        let now = 10 * 60 * 60 * 1_000;
        let chosen = choose_event(
            vec![
                event("Standup", now - 60_000, now + 60_000, None),
                event(
                    "Design review",
                    now - 120_000,
                    now + 600_000,
                    Some("https://meet.google.com/abc-defg-hij"),
                ),
            ],
            now,
            &["abc-defg-hij".into()],
        );
        assert_eq!(
            chosen.map(|event| event.title),
            Some("Design review".into())
        );
    }

    #[test]
    fn ignores_events_far_from_now() {
        let now = 10 * 60 * 60 * 1_000;
        let chosen = choose_event(vec![event("Yesterday", 0, 60_000, None)], now, &[]);
        assert!(chosen.is_none());
    }

    #[test]
    fn finds_call_links_in_invitation_notes() {
        assert_eq!(
            meeting_link("Join: <https://us02web.zoom.us/j/123456?pwd=x>.").as_deref(),
            Some("https://us02web.zoom.us/j/123456?pwd=x")
        );
        assert_eq!(meeting_link("no link here"), None);
    }
}
