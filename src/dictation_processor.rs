use crate::context::{ContextSelector, ContextSnapshot};
use crate::text_replacements::ReplacementSet;

/// Local text rules for one dictation mode: its corrections, then its
/// transformations, which run in order.
#[derive(Clone, Default)]
pub struct Profile {
    replacements: ReplacementSet,
    transformations: Vec<String>,
}

impl Profile {
    pub(crate) fn replacements(mut self, replacements: ReplacementSet) -> Self {
        self.replacements = replacements;
        self
    }

    pub fn transformations(mut self, transformations: Vec<String>) -> Self {
        self.transformations = transformations;
        self
    }
}

#[derive(Clone)]
struct ContextualProfile {
    selector: ContextSelector,
    profile: Profile,
}

#[derive(Clone)]
pub struct Profiles {
    default: Profile,
    contextual: Vec<ContextualProfile>,
}

impl Profiles {
    pub fn new(default: Profile) -> Self {
        Self {
            default,
            contextual: Vec::new(),
        }
    }

    pub fn application(self, application: impl Into<String>, profile: Profile) -> Self {
        self.push(ContextSelector::application(application.into()), profile)
    }

    pub fn browser_host(self, host: impl Into<String>, profile: Profile) -> Self {
        self.push(ContextSelector::browser_host(host.into()), profile)
    }

    fn push(mut self, selector: ContextSelector, profile: Profile) -> Self {
        if self
            .contextual
            .iter()
            .any(|existing| existing.selector == selector)
        {
            tracing::warn!(?selector, "ignored duplicate dictation processor selector");
            return self;
        }
        self.contextual
            .push(ContextualProfile { selector, profile });
        self
    }

    fn select<'a>(&'a self, context: &ContextSnapshot) -> &'a Profile {
        let selected = self
            .contextual
            .iter()
            .find(|candidate| {
                candidate.selector.is_browser() && candidate.selector.matches(context)
            })
            .or_else(|| {
                self.contextual.iter().find(|candidate| {
                    !candidate.selector.is_browser() && candidate.selector.matches(context)
                })
            });
        selected.map_or(&self.default, |selected| &selected.profile)
    }

    pub fn processes(&self, context: &ContextSnapshot) -> bool {
        !self.select(context).transformations.is_empty()
    }

    pub fn process(&self, transcript: &str, context: &ContextSnapshot) -> Processed {
        let profile = self.select(context);
        Processed {
            text: profile.replacements.replace(transcript),
            observation: None,
            transformations: profile.transformations.clone(),
        }
    }
}

/// Timing and fallback details for a local processing step, kept in history
/// and the activity log.
pub type ProcessingObservation = crate::events::DictationProcessing;

pub struct Processed {
    pub text: String,
    pub observation: Option<ProcessingObservation>,
    pub transformations: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use url::Url;

    fn context(application: &str, url: Option<&str>) -> ContextSnapshot {
        ContextSnapshot {
            application: Some(application.into()),
            browser_url: url.map(|url| Url::parse(url).unwrap()),
            window_title: None,
            input_revision: None,
        }
    }

    fn profiles() -> Profiles {
        Profiles::new(Profile::default().transformations(vec!["default".into()]))
            .application(
                "Slack",
                Profile::default().transformations(vec!["slack".into()]),
            )
            .browser_host(
                "x.com",
                Profile::default().transformations(vec!["x".into()]),
            )
    }

    #[test]
    fn browser_host_beats_application_and_application_beats_default() {
        let profiles = profiles();
        assert_eq!(
            profiles
                .select(&context("Brave Browser", Some("https://x.com/home")))
                .transformations,
            ["x"]
        );
        assert_eq!(
            profiles.select(&context("Slack", None)).transformations,
            ["slack"]
        );
        assert_eq!(
            profiles.select(&context("Zed", None)).transformations,
            ["default"]
        );
    }

    #[test]
    fn mode_corrections_run_before_registered_transformations() {
        let profile = Profile::default()
            .replacements(ReplacementSet::new(&[
                crate::app_settings::TextReplacement {
                    matched_phrase: "git hub".into(),
                    output: "GitHub".into(),
                },
            ]))
            .transformations(vec!["lowercase".into()]);
        let profiles = Profiles::new(profile);

        let processed = profiles.process("Use git hub.", &context("Zed", None));

        assert_eq!(processed.text, "Use GitHub.");
        assert_eq!(processed.transformations, ["lowercase"]);
        assert!(processed.observation.is_none());
    }

    #[test]
    fn processing_is_needed_only_when_a_mode_has_transformations() {
        let profiles = Profiles::new(Profile::default()).application(
            "Slack",
            Profile::default().transformations(vec!["lowercase".into()]),
        );

        assert!(!profiles.processes(&context("Zed", None)));
        assert!(profiles.processes(&context("Slack", None)));
    }
}
