//! Works out where a meeting is happening: the call platform, the app or
//! browser (and browser profile and account), the call's title, link and code,
//! and the calendar event behind it. Everything is read locally.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::calendar::{self, CalendarEvent};
use crate::microphone_activity::{self, ActiveMicrophoneApplication};

pub const DETAILS_FILE: &str = "details.json";

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(default)]
pub struct MeetingDetails {
    /// The best name for the meeting (calendar event, then call title).
    pub title: Option<String>,
    pub platform: Option<String>,
    pub app: Option<String>,
    pub bundle_id: Option<String>,
    pub browser_profile: Option<String>,
    pub account: Option<String>,
    pub window_title: Option<String>,
    pub meeting_code: Option<String>,
    pub url: Option<String>,
    pub calendar: Option<CalendarEvent>,
    /// The name used for "You" in the transcript.
    pub you: Option<String>,
    pub updated_at_ms: u64,
}

impl MeetingDetails {
    /// Keeps what an earlier look found when a later one cannot see it.
    pub fn merge(&mut self, newer: MeetingDetails) {
        fn keep<T>(slot: &mut Option<T>, value: Option<T>) {
            if value.is_some() {
                *slot = value;
            }
        }
        // A real call title beats a generic one found earlier.
        if newer.title.is_some()
            && (self.title.is_none() || newer.calendar.is_some() || newer.title_is_specific())
        {
            self.title = newer.title;
        }
        keep(&mut self.platform, newer.platform);
        keep(&mut self.app, newer.app);
        keep(&mut self.bundle_id, newer.bundle_id);
        keep(&mut self.browser_profile, newer.browser_profile);
        keep(&mut self.account, newer.account);
        keep(&mut self.window_title, newer.window_title);
        keep(&mut self.meeting_code, newer.meeting_code);
        keep(&mut self.url, newer.url);
        keep(&mut self.calendar, newer.calendar);
        keep(&mut self.you, newer.you);
        self.updated_at_ms = newer.updated_at_ms.max(self.updated_at_ms);
    }

    fn title_is_specific(&self) -> bool {
        self.title.as_deref().is_some_and(|title| {
            self.platform
                .as_deref()
                .is_none_or(|platform| !title.starts_with(platform))
        })
    }

    pub fn read(directory: &Path) -> Option<Self> {
        std::fs::read(directory.join(DETAILS_FILE))
            .ok()
            .and_then(|data| serde_json::from_slice(&data).ok())
    }

    pub fn write(&self, directory: &Path) -> std::io::Result<()> {
        let temporary = directory.join("details.json.tmp");
        std::fs::write(&temporary, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(temporary, directory.join(DETAILS_FILE))
    }
}

/// Looks at the call that is using the microphone now. `app` narrows the look
/// to one call app (a canonical bundle id); `platform` is the detector's guess.
pub fn gather(app: Option<&str>, platform: Option<&str>, read_tabs: bool) -> MeetingDetails {
    let now = now_ms();
    let mut details = MeetingDetails {
        platform: platform.map(Into::into),
        you: Some(crate::app_settings::profile_name())
            .filter(|name| !name.trim().is_empty())
            .or_else(|| Some(objc2_foundation::NSFullUserName().to_string()))
            .filter(|name| !name.trim().is_empty()),
        updated_at_ms: now,
        ..MeetingDetails::default()
    };
    let call_app = microphone_activity::active_microphone_applications()
        .unwrap_or_default()
        .into_iter()
        .filter(|candidate| candidate.is_supported_meeting_app())
        .filter(|candidate| app.is_none_or(|app| candidate.canonical_bundle_id() == Some(app)))
        .min_by_key(ActiveMicrophoneApplication::detection_priority);
    if let Some(call_app) = &call_app {
        describe_app(call_app, read_tabs, &mut details);
    }
    if crate::app_settings::meeting_calendar_enabled() {
        let hints: Vec<String> = [
            details.meeting_code.clone(),
            details.url.clone(),
            details.title.clone(),
        ]
        .into_iter()
        .flatten()
        .collect();
        details.calendar = calendar::current_event(now, &hints);
    }
    if let Some(event) = &details.calendar {
        details.title = Some(event.title.clone());
        if details.url.is_none() {
            details.url = event.url.clone();
        }
    }
    if details.title.is_none() {
        details.title = details.platform.as_deref().map(|platform| match platform {
            "Zoom" | "Webex" | "Microsoft Teams" | "Google Meet" => format!("{platform} meeting"),
            "Slack Huddle" => platform.to_string(),
            other => format!("{other} call"),
        });
    }
    details
}

fn describe_app(
    call_app: &ActiveMicrophoneApplication,
    read_tabs: bool,
    details: &mut MeetingDetails,
) {
    let Some(bundle) = call_app.canonical_bundle_id() else {
        return;
    };
    let (pid, name) = main_application(bundle).unwrap_or((call_app.pid, call_app.name.clone()));
    details.app = Some(name.clone());
    details.bundle_id = Some(bundle.to_string());
    if call_app.is_browser() {
        let profiles = browser_profiles(bundle);
        let windows = crate::accessibility::window_labels(pid);
        let tabs = if read_tabs {
            call_tabs(bundle)
        } else {
            Vec::new()
        };
        let window = windows
            .iter()
            .find(|window| platform_from_text(&window.title).is_some());
        if let Some((url, title)) = tabs.first() {
            details.url = Some(url.clone());
            details.platform = platform_from_text(url).or(details.platform.take());
            details.meeting_code = meeting_code(url);
            apply_call_title(details, title, &profiles, &name);
        }
        if let Some(window) = window {
            details.window_title = Some(window.title.clone());
            if details.platform.is_none() {
                details.platform = platform_from_text(&window.title);
            }
            if details.meeting_code.is_none() {
                details.meeting_code = meeting_code(&window.title);
            }
            if details.title.is_none() {
                apply_call_title(details, &window.title, &profiles, &name);
            }
        }
        let profile = window
            .and_then(|window| match_profile(&profiles, &window.title, &window.controls))
            .or_else(|| {
                windows
                    .first()
                    .and_then(|window| match_profile(&profiles, &window.title, &window.controls))
            })
            .or_else(|| (profiles.len() == 1).then(|| profiles[0].clone()))
            .or_else(|| profiles.iter().find(|profile| profile.last_used).cloned());
        if let Some(profile) = profile {
            details.account = profile.email.clone().or(profile.full_name.clone());
            details.browser_profile = Some(profile.name);
        }
    } else {
        if details.platform.is_none() {
            details.platform = platform_for_bundle(bundle).map(Into::into);
        }
        let titles = crate::accessibility::window_titles(pid);
        let window = titles
            .iter()
            .find(|title| is_call_window(bundle, title))
            .or_else(|| titles.first());
        if let Some(window) = window {
            details.window_title = Some(window.clone());
            details.meeting_code = meeting_code(window);
            apply_call_title(details, window, &[], &name);
        }
    }
}

fn apply_call_title(
    details: &mut MeetingDetails,
    raw: &str,
    profiles: &[BrowserProfile],
    app: &str,
) {
    if let Some(title) = call_title(raw, profiles, app) {
        details.title = Some(title);
    }
}

/// The meaningful part of a call window or tab title, without the browser,
/// profile, recording notices or a bare meeting code.
pub fn call_title(raw: &str, profiles: &[BrowserProfile], app: &str) -> Option<String> {
    let noise = [
        "google chrome",
        "brave",
        "microsoft edge",
        "arc",
        "vivaldi",
        "opera",
        "safari",
        "camera and microphone recording",
        "camera recording",
        "microphone recording",
        "audio playing",
        "recording",
        "google meet",
        "meet",
        "microsoft teams",
        "teams",
        "zoom",
        "zoom meeting",
        "zoom workplace",
        "webex",
    ];
    let app = app.to_lowercase();
    let segments: Vec<String> = raw
        .split(" | ")
        .flat_map(|part| part.split(" - "))
        .flat_map(|part| part.split(" – "))
        .flat_map(|part| part.split(" — "))
        .map(|part| part.trim().to_string())
        .filter(|part| !part.is_empty())
        .filter(|part| {
            let lower = part.to_lowercase();
            !noise.contains(&lower.as_str())
                && lower != app
                && !lower.starts_with("high memory usage")
                && !profiles
                    .iter()
                    .any(|profile| profile.name.eq_ignore_ascii_case(part))
                && meeting_code(part).as_deref() != Some(part.as_str())
                && !lower
                    .chars()
                    .all(|character| character.is_ascii_digit() || character == ' ')
        })
        .collect();
    segments
        .into_iter()
        .next()
        .filter(|title| title.chars().count() >= 2)
}

fn is_call_window(bundle: &str, title: &str) -> bool {
    let lower = title.to_lowercase();
    match bundle {
        "us.zoom.xos" => lower.contains("meeting") || lower.contains("webinar"),
        "com.microsoft.teams2" | "com.microsoft.teams" => {
            lower.contains("meeting") || lower.contains("call") || lower.contains("|")
        }
        "com.tinyspeck.slackmacgap" => lower.contains("huddle"),
        _ => !lower.is_empty(),
    }
}

fn platform_for_bundle(bundle: &str) -> Option<&'static str> {
    Some(match bundle {
        "us.zoom.xos" => "Zoom",
        "com.microsoft.teams2" | "com.microsoft.teams" => "Microsoft Teams",
        "com.tinyspeck.slackmacgap" => "Slack Huddle",
        "com.apple.FaceTime" => "FaceTime",
        "net.whatsapp.WhatsApp" => "WhatsApp",
        "com.hnc.Discord" => "Discord",
        "Cisco-Systems.Spark" => "Webex",
        "com.skype.skype" => "Skype",
        "ru.keepcoder.Telegram" => "Telegram",
        "org.whispersystems.signal-desktop" => "Signal",
        _ => return None,
    })
}

/// The call platform named by a URL or title.
pub fn platform_from_text(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    let platform = if lower.contains("meet.google.com")
        || lower.contains("google meet")
        || lower.starts_with("meet - ")
        || lower.starts_with("meet – ")
    {
        "Google Meet"
    } else if lower.contains("zoom.us")
        || lower.contains("zoom meeting")
        || lower.contains("zoom webinar")
    {
        "Zoom"
    } else if lower.contains("teams.microsoft.com")
        || lower.contains("teams.live.com")
        || lower.contains("microsoft teams")
    {
        "Microsoft Teams"
    } else if lower.contains("app.slack.com/huddle") || lower.contains("huddle") {
        "Slack Huddle"
    } else if lower.contains("web.whatsapp.com") || lower.contains("whatsapp") {
        "WhatsApp"
    } else if lower.contains("discord.com") || lower.contains("discord") {
        "Discord"
    } else if lower.contains("webex.com") || lower.contains("webex") {
        "Webex"
    } else {
        return None;
    };
    Some(platform.into())
}

/// A Google Meet code (abc-defg-hij) or a Zoom meeting number.
pub fn meeting_code(text: &str) -> Option<String> {
    let meet = regex::Regex::new(r"\b([a-z]{3}-[a-z]{4}-[a-z]{3})\b").ok()?;
    if let Some(found) = meet.captures(text) {
        return Some(found[1].to_string());
    }
    let zoom = regex::Regex::new(r"zoom\.us/(?:j|wc|w|s)/(\d{9,11})").ok()?;
    zoom.captures(text).map(|found| found[1].to_string())
}

/// The browser's own app (not a helper process): its pid and name.
fn main_application(bundle: &str) -> Option<(i32, String)> {
    use objc2_app_kit::NSRunningApplication;
    use objc2_foundation::NSString;
    let applications =
        NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(bundle));
    let application = applications.iter().next()?;
    let name = application
        .localizedName()
        .map(|name| name.to_string())
        .unwrap_or_else(|| bundle.to_string());
    Some((application.processIdentifier(), name))
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct BrowserProfile {
    pub name: String,
    pub email: Option<String>,
    pub full_name: Option<String>,
    pub last_used: bool,
}

fn local_state_path(bundle: &str) -> Option<PathBuf> {
    let support = dirs::data_dir()?;
    let relative = match bundle {
        "com.google.Chrome" => "Google/Chrome",
        "com.brave.Browser" => "BraveSoftware/Brave-Browser",
        "com.microsoft.edgemac" => "Microsoft Edge",
        "company.thebrowser.Browser" => "Arc/User Data",
        "com.vivaldi.Vivaldi" => "Vivaldi",
        "com.operasoftware.Opera" => "com.operasoftware.Opera",
        _ => return None,
    };
    Some(support.join(relative).join("Local State"))
}

/// The profiles of a Chromium browser, from its Local State file.
pub fn browser_profiles(bundle: &str) -> Vec<BrowserProfile> {
    let Some(data) = local_state_path(bundle).and_then(|path| std::fs::read(path).ok()) else {
        return Vec::new();
    };
    parse_profiles(&data)
}

fn parse_profiles(data: &[u8]) -> Vec<BrowserProfile> {
    let Ok(state) = serde_json::from_slice::<serde_json::Value>(data) else {
        return Vec::new();
    };
    let last_used = state["profile"]["last_used"].as_str().unwrap_or("Default");
    let Some(cache) = state["profile"]["info_cache"].as_object() else {
        return Vec::new();
    };
    let text = |value: &serde_json::Value| {
        value
            .as_str()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(Into::into)
    };
    cache
        .iter()
        .map(|(directory, info)| BrowserProfile {
            name: text(&info["name"]).unwrap_or_else(|| directory.clone()),
            email: text(&info["user_name"]),
            full_name: text(&info["gaia_name"]),
            last_used: directory == last_used,
        })
        .collect()
}

fn match_profile(
    profiles: &[BrowserProfile],
    title: &str,
    controls: &[String],
) -> Option<BrowserProfile> {
    let title = title.to_lowercase();
    profiles
        .iter()
        .find(|profile| {
            let name = profile.name.to_lowercase();
            let email = profile.email.as_deref().map(str::to_lowercase);
            let full = profile.full_name.as_deref().map(str::to_lowercase);
            title.ends_with(&format!(" - {name}"))
                || controls.iter().any(|control| {
                    let control = control.to_lowercase();
                    control == name
                        || control.starts_with(&format!("{name} "))
                        || control.contains(&format!("({name})"))
                        || email
                            .as_deref()
                            .is_some_and(|email| control.contains(email))
                        || full.as_deref().is_some_and(|full| {
                            control == full || control.starts_with(&format!("{full} "))
                        })
                })
        })
        .cloned()
}

/// Call tabs open in a browser, read through AppleScript: (url, title) pairs.
fn call_tabs(bundle: &str) -> Vec<(String, String)> {
    let title_property = if bundle == "com.apple.Safari" {
        "name"
    } else {
        "title"
    };
    let script = format!(
        r#"tell application id "{bundle}"
  set found to ""
  repeat with w in windows
    repeat with t in tabs of w
      set u to URL of t
      if u contains "meet.google.com/" or u contains "zoom.us/" or u contains "teams.microsoft.com" or u contains "teams.live.com" or u contains "app.slack.com/huddle" or u contains "web.whatsapp.com" or u contains "discord.com/channels" or u contains "webex.com/" then
        set found to found & u & tab & ({title_property} of t) & linefeed
      end if
    end repeat
  end repeat
  return found
end tell"#
    );
    let Ok(mut child) = Command::new("/usr/bin/osascript")
        .args(["-e", &script])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return Vec::new();
    };
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < Duration::from_secs(3) => {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Vec::new();
            }
        }
    }
    let Ok(output) = child.wait_with_output() else {
        return Vec::new();
    };
    let mut tabs: Vec<(String, String)> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let (url, title) = line.split_once('\t')?;
            Some((url.trim().to_string(), title.trim().to_string()))
        })
        // A Meet landing page is not a call; a call URL carries its code.
        .filter(|(url, _)| !url.contains("meet.google.com/") || meeting_code(url).is_some())
        .collect();
    tabs.sort_by_key(|(url, _)| u8::from(meeting_code(url).is_none()));
    tabs
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_titles_from_meet_tabs() {
        let profiles = vec![BrowserProfile {
            name: "Work".into(),
            ..BrowserProfile::default()
        }];
        assert_eq!(
            call_title(
                "Meet - Weekly sync - Camera and microphone recording - Google Chrome - Work",
                &profiles,
                "Google Chrome"
            )
            .as_deref(),
            Some("Weekly sync")
        );
        assert_eq!(
            call_title(
                "Meet - abc-defg-hij - Google Chrome",
                &profiles,
                "Google Chrome"
            ),
            None
        );
        assert_eq!(
            call_title("Q3 planning | Microsoft Teams", &[], "Microsoft Teams").as_deref(),
            Some("Q3 planning")
        );
        assert_eq!(call_title("Zoom Meeting", &[], "zoom.us"), None);
    }

    #[test]
    fn finds_meeting_codes() {
        assert_eq!(
            meeting_code("https://meet.google.com/abc-defg-hij?authuser=1").as_deref(),
            Some("abc-defg-hij")
        );
        assert_eq!(
            meeting_code("https://us02web.zoom.us/j/81234567890?pwd=x").as_deref(),
            Some("81234567890")
        );
        assert_eq!(meeting_code("Weekly sync"), None);
    }

    #[test]
    fn names_platforms() {
        assert_eq!(
            platform_from_text("https://meet.google.com/abc-defg-hij").as_deref(),
            Some("Google Meet")
        );
        assert_eq!(
            platform_from_text("Meet - Weekly sync").as_deref(),
            Some("Google Meet")
        );
        assert_eq!(platform_from_text("Inbox - Gmail"), None);
    }

    #[test]
    fn reads_chromium_profiles_and_matches_the_toolbar() {
        let state = br#"{"profile":{"last_used":"Profile 1","info_cache":{
            "Default":{"name":"Personal","user_name":"me@gmail.com","gaia_name":"Dhruba Datta"},
            "Profile 1":{"name":"Work","user_name":"dhruba@company.com","gaia_name":"Dhruba Datta"}}}}"#;
        let profiles = parse_profiles(state);
        assert_eq!(profiles.len(), 2);
        let work = match_profile(&profiles, "Meet - Weekly sync", &["Work".into()]).unwrap();
        assert_eq!(work.email.as_deref(), Some("dhruba@company.com"));
        assert!(
            profiles
                .iter()
                .any(|profile| profile.last_used && profile.name == "Work")
        );
    }

    #[test]
    fn merge_keeps_earlier_findings() {
        let mut details = MeetingDetails {
            platform: Some("Google Meet".into()),
            title: Some("Weekly sync".into()),
            account: Some("me@gmail.com".into()),
            ..MeetingDetails::default()
        };
        details.merge(MeetingDetails {
            platform: Some("Google Meet".into()),
            title: Some("Google Meet meeting".into()),
            ..MeetingDetails::default()
        });
        assert_eq!(details.title.as_deref(), Some("Weekly sync"));
        assert_eq!(details.account.as_deref(), Some("me@gmail.com"));
    }
}
