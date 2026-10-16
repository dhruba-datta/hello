use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use serde::{Deserialize, Serialize};
use url::Url;

use crate::context::ContextSnapshot;
use crate::microphone_activity::ActiveMicrophoneApplication;

const STABLE_OBSERVATIONS: u8 = 4;
const ABSENT_OBSERVATIONS_TO_RESET: u8 = 10;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MeetingCandidate {
    pub key: String,
    pub title: String,
    pub source: MeetingSource,
    /// The call app holding the microphone, used to stop when the call ends.
    pub app: Option<&'static str>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MeetingSource {
    Manual,
    GoogleMeet,
    Zoom,
    Teams,
    SlackHuddle,
    FaceTime,
    WhatsApp,
    Discord,
    Webex,
    Skype,
    Telegram,
    Signal,
    BrowserCall,
}

impl MeetingSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Manual => "Meeting",
            Self::GoogleMeet => "Google Meet",
            Self::Zoom => "Zoom",
            Self::Teams => "Microsoft Teams",
            Self::SlackHuddle => "Slack Huddle",
            Self::FaceTime => "FaceTime",
            Self::WhatsApp => "WhatsApp",
            Self::Discord => "Discord",
            Self::Webex => "Webex",
            Self::Skype => "Skype",
            Self::Telegram => "Telegram",
            Self::Signal => "Signal",
            Self::BrowserCall => "Browser call",
        }
    }
}

#[derive(Default)]
pub struct MeetingDetector {
    pending: Option<(String, u8)>,
    offered: Option<String>,
    absent_observations: u8,
}

impl MeetingDetector {
    pub fn observe(&mut self, candidate: Option<MeetingCandidate>) -> Option<MeetingCandidate> {
        let Some(candidate) = candidate else {
            self.pending = None;
            self.absent_observations = self.absent_observations.saturating_add(1);
            if self.absent_observations >= ABSENT_OBSERVATIONS_TO_RESET {
                self.offered = None;
            }
            return None;
        };
        self.absent_observations = 0;
        if self.offered.as_deref() == Some(&candidate.key) {
            return None;
        }
        let observations = match &mut self.pending {
            Some((key, count)) if key == &candidate.key => {
                *count = count.saturating_add(1);
                *count
            }
            _ => {
                self.pending = Some((candidate.key.clone(), 1));
                1
            }
        };
        if observations < STABLE_OBSERVATIONS {
            return None;
        }
        self.pending = None;
        self.offered = Some(candidate.key.clone());
        Some(candidate)
    }
}

pub fn candidate_from_context(context: &ContextSnapshot) -> Option<MeetingCandidate> {
    let application = context.application.as_deref()?;
    let application_lower = application.to_ascii_lowercase();
    let title = context.window_title.as_deref().unwrap_or("").trim();
    let title_lower = title.to_ascii_lowercase();

    if application_lower.contains("brave")
        && let Some(url) = &context.browser_url
        && is_google_meet_call(url)
    {
        return Some(candidate(
            MeetingSource::GoogleMeet,
            url.as_str(),
            clean_title(title, &[" - Google Meet", " | Google Meet"]),
        ));
    }
    // Other browsers expose no URL here; their tab title names the call.
    // A known URL that is not a call (a Meet landing page) settles it.
    if is_browser_name(&application_lower) && context.browser_url.is_none() {
        let meet_call = title_lower.starts_with("meet - ")
            || title_lower.starts_with("meet – ")
            || title_lower.ends_with(" - google meet")
            || title_lower.ends_with(" | google meet");
        if meet_call {
            return Some(candidate(
                MeetingSource::GoogleMeet,
                title,
                clean_title(title, &[" - Google Meet", " | Google Meet"]),
            ));
        }
        if title_lower.contains("microsoft teams") {
            return Some(candidate(MeetingSource::Teams, title, title));
        }
        if title_lower.contains("zoom") {
            return Some(candidate(MeetingSource::Zoom, title, title));
        }
        if title_lower.contains("whatsapp") {
            return Some(candidate(MeetingSource::WhatsApp, title, "WhatsApp call"));
        }
        if title_lower.contains("discord") {
            return Some(candidate(MeetingSource::Discord, title, "Discord call"));
        }
    }
    if application_lower.contains("zoom")
        && (title_lower.contains("meeting") || title_lower.contains("webinar"))
    {
        return Some(candidate(MeetingSource::Zoom, title, title));
    }
    if application_lower.contains("teams")
        && (title_lower.contains("meeting") || title_lower.contains("call"))
    {
        return Some(candidate(MeetingSource::Teams, title, title));
    }
    if application_lower == "slack" && title_lower.contains("huddle") {
        return Some(candidate(MeetingSource::SlackHuddle, title, title));
    }
    if application_lower == "facetime" && !title.is_empty() && title_lower != "facetime" {
        return Some(candidate(MeetingSource::FaceTime, title, title));
    }
    None
}

pub fn candidate_from_microphone(
    application: &ActiveMicrophoneApplication,
    context: &ContextSnapshot,
) -> Option<MeetingCandidate> {
    let canonical_bundle = application.canonical_bundle_id()?;
    candidate_for_app(application, canonical_bundle, context).map(|mut candidate| {
        candidate.app = Some(canonical_bundle);
        candidate
    })
}

fn candidate_for_app(
    application: &ActiveMicrophoneApplication,
    canonical_bundle: &'static str,
    context: &ContextSnapshot,
) -> Option<MeetingCandidate> {
    if application.is_browser() {
        if context
            .application
            .as_deref()
            .is_some_and(|foreground| foreground.contains(&application.name))
            && let Some(candidate) = candidate_from_context(context)
        {
            return Some(candidate);
        }
        return Some(candidate(
            MeetingSource::BrowserCall,
            canonical_bundle,
            "Browser call",
        ));
    }
    let (source, title) = match canonical_bundle {
        "us.zoom.xos" => (MeetingSource::Zoom, "Zoom meeting"),
        "com.microsoft.teams2" => (MeetingSource::Teams, "Microsoft Teams meeting"),
        "com.tinyspeck.slackmacgap" => (MeetingSource::SlackHuddle, "Slack huddle"),
        "com.apple.FaceTime" => (MeetingSource::FaceTime, "FaceTime call"),
        "com.microsoft.teams" => (MeetingSource::Teams, "Microsoft Teams meeting"),
        "net.whatsapp.WhatsApp" => (MeetingSource::WhatsApp, "WhatsApp call"),
        "com.hnc.Discord" => (MeetingSource::Discord, "Discord call"),
        "Cisco-Systems.Spark" => (MeetingSource::Webex, "Webex meeting"),
        "com.skype.skype" => (MeetingSource::Skype, "Skype call"),
        "ru.keepcoder.Telegram" => (MeetingSource::Telegram, "Telegram call"),
        "org.whispersystems.signal-desktop" => (MeetingSource::Signal, "Signal call"),
        _ => return None,
    };
    Some(candidate(source, canonical_bundle, title))
}

fn candidate(source: MeetingSource, identity: &str, title: &str) -> MeetingCandidate {
    MeetingCandidate {
        app: None,
        key: opaque_key(source, identity),
        title: if title.is_empty() {
            source.label().into()
        } else {
            title.into()
        },
        source,
    }
}

fn opaque_key(source: MeetingSource, identity: &str) -> String {
    let mut hasher = DefaultHasher::new();
    source.hash(&mut hasher);
    identity.hash(&mut hasher);
    format!("meeting-{:016x}", hasher.finish())
}

fn is_browser_name(application: &str) -> bool {
    [
        "chrome", "safari", "arc", "brave", "edge", "firefox", "opera", "vivaldi",
    ]
    .iter()
    .any(|name| application.contains(name))
}

fn is_google_meet_call(url: &Url) -> bool {
    if url.host_str() != Some("meet.google.com") {
        return false;
    }
    let path = url.path().trim_matches('/');
    !path.is_empty() && path != "landing" && path != "new"
}

fn clean_title<'a>(title: &'a str, suffixes: &[&str]) -> &'a str {
    suffixes
        .iter()
        .find_map(|suffix| title.strip_suffix(suffix))
        .unwrap_or(title)
        .trim()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(application: &str, url: Option<&str>, title: &str) -> ContextSnapshot {
        ContextSnapshot {
            application: Some(application.into()),
            browser_url: url.map(|url| Url::parse(url).unwrap()),
            window_title: Some(title.into()),
            input_revision: None,
        }
    }

    #[test]
    fn recognizes_calls_without_treating_landing_pages_as_meetings() {
        assert!(
            candidate_from_context(&context(
                "Brave Browser",
                Some("https://meet.google.com/abc-defg-hij"),
                "Design sync - Google Meet"
            ))
            .is_some()
        );
        assert!(
            candidate_from_context(&context(
                "Brave Browser",
                Some("https://meet.google.com/landing"),
                "Google Meet"
            ))
            .is_none()
        );
        assert!(candidate_from_context(&context("zoom.us", None, "Zoom Workplace")).is_none());
        assert!(candidate_from_context(&context("zoom.us", None, "Zoom Meeting")).is_some());
    }

    #[test]
    fn requires_stability_and_offers_once_per_session() {
        let candidate = candidate_from_context(&context(
            "Brave Browser",
            Some("https://meet.google.com/abc-defg-hij"),
            "Design sync - Google Meet",
        ));
        let mut detector = MeetingDetector::default();
        assert!(detector.observe(candidate.clone()).is_none());
        assert!(detector.observe(candidate.clone()).is_none());
        assert!(detector.observe(candidate.clone()).is_none());
        assert!(detector.observe(candidate.clone()).is_some());
        assert!(detector.observe(candidate.clone()).is_none());
        for _ in 0..ABSENT_OBSERVATIONS_TO_RESET {
            assert!(detector.observe(None).is_none());
        }
        assert!(detector.observe(candidate.clone()).is_none());
        assert!(detector.observe(candidate.clone()).is_none());
        assert!(detector.observe(candidate.clone()).is_none());
        assert!(detector.observe(candidate).is_some());
    }
}
