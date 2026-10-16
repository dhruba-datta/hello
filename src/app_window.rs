use std::cell::RefCell;
use std::path::PathBuf;
use std::process::Command as ProcessCommand;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::thread;
use std::time::{Duration, Instant};

use crate::desktop_ui::{rgb, rgba};
use gpui::{
    AnyElement, App, Bounds, Context, Div, Entity, FocusHandle, Focusable, FontWeight, IntoElement,
    KeyDownEvent, Modifiers as GpuiModifiers, ModifiersChangedEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, PathPromptOptions, Pixels, Point, Render, ScrollHandle, SharedString,
    Subscription, Timer, TitlebarOptions, Window, WindowBounds, WindowHandle, WindowOptions,
    actions, deferred, div, img, linear_color_stop, linear_gradient, prelude::*, px, relative,
    size,
};

use crate::app_settings::{
    AppSettings, ColorTheme, DictationMode, HotkeyBinding, HotkeyKey, HotkeyModifiers,
    IndicatorPosition, IndicatorStyle, LanguageRouting, MeetingAudioRetention, ModifierSide,
    RecordingAudioBehavior, SmartEditing, TextReplacement,
};
use crate::application_catalog::InstalledApplication;
use crate::commands::{CommandConfig, CommandInfo, CommandScope};
use crate::desktop_activity::DesktopActivity;
use crate::desktop_transcription_picker::{
    ModelDeletion, TranscriptionPickerDelegate, TranscriptionPickerModel,
    TranscriptionPickerProgress, TranscriptionPickerStatus, TranscriptionPickerView,
    render_transcription_picker as render_shared_transcription_picker,
    transcription_selection_is_active,
};
use crate::desktop_ui::{
    ACCENT, ACCENT_SOFT, ACCENT_SURFACE, CANVAS, CONTROL_HEIGHT, DANGER_BORDER, DANGER_SURFACE,
    FAINT, Glyph, LINE, MUTED, NEGATIVE, NavigationIcon, ON_ACCENT, PANE_CONTENT_WIDTH,
    PANE_LIST_WIDTH, PANEL_RADIUS, POPOVER, POSITIVE, POSITIVE_SURFACE, SECTION_GAP, SIDEBAR_WIDTH,
    SURFACE, SURFACE_HOVER, SURFACE_SELECTED, TEXT, TEXT_SOFT, TINT_BLUE, TINT_GRAY, TINT_GREEN,
    TINT_INDIGO, TINT_ORANGE, TINT_PINK, TINT_PURPLE, TINT_TEAL, canvas_button, chip,
    compact_button, compact_header_plus_button, compact_panel, compact_panel_header,
    compact_plus_button, disclosure_button, empty_message, error_message, glyph, glyph_badge,
    glyph_button, group_heading, header_button, hotkey_keycaps, icon_chip, list_row,
    listener_status, mix_color, navigation_item, pane_body, pane_content, pane_header,
    pane_header_with_action, pane_header_with_subtitle, pane_list, panel_title, section_label,
    segmented_control, segmented_item, settings_copy, settings_panel, settings_row,
    settings_section_label, sidebar_frame, sliding_segmented_control, sliding_segmented_item,
    tinted, toggle, tone, window_frame,
};
use crate::developer_control::DeveloperPane;
use crate::dictation_indicator::{DictationIndicatorEvent, DictationIndicatorSender, HudTuning};
use crate::events::{
    CommandOutcome, DictationPhase, EventReader, TranscriptPhase, VoiceEvent, VoiceState, now_ms,
};
use crate::history::{History, HistoryEntry, HistoryInsights, HistoryKind, HistoryRetention};
use crate::login_item::{LoginItemRequest, LoginItemResponse, LoginItemStatus, LoginItemWorker};
use crate::meeting::{self, MeetingManifest, MeetingRequest, MeetingStatus};
use crate::onboarding::{
    PermissionAction, PermissionKind, PermissionState, PermissionWarning, SetupStatus,
};
use crate::personal_commands::{HostState, StatusContext, StatusSnapshot, StatusTransformation};
use crate::recognition::CommandModelStatus;
use crate::text_input::{
    Changed as TextChanged, Dismissed as TextDismissed, Navigate as TextNavigate,
    Submitted as TextSubmitted, TextInput,
};
use crate::transcription_models::{
    ModelDefinition, ModelRuntime, TranscriptionModelId, TranscriptionSelection, definition,
    language_name, validate,
};
use crate::transcription_preparation::{PreparationStatus, TranscriptionPreparation};

const WINDOW_WIDTH: f32 = 1040.0;
const WINDOW_HEIGHT: f32 = 700.0;
const MINIMUM_WIDTH: f32 = 860.0;
const MINIMUM_HEIGHT: f32 = 560.0;
const HOTKEY_MIN_WIDTH: f32 = 148.0;
const HOTKEY_SIDE_SELECTOR_WIDTH: f32 = 116.0;
const PERMISSION_REFRESH_INTERVAL: Duration = Duration::from_secs(5);
const ACTIVITY_LIMIT: usize = 100;

fn sidebar_update_button(
    status: crate::sparkle::UpdateStatus,
    preview: bool,
) -> Option<gpui::Stateful<Div>> {
    (status == crate::sparkle::UpdateStatus::UpdateAvailable).then(|| {
        div()
            .id("check-for-updates-sidebar")
            .h(px(30.0))
            .px_3()
            .flex()
            .items_center()
            .rounded_sm()
            .text_size(px(12.0))
            .flex_none()
            .bg(rgb(ACCENT))
            .text_color(rgb(ON_ACCENT))
            .font_weight(FontWeight::SEMIBOLD)
            .cursor_pointer()
            .hover(|button| button.bg(mix_color(rgb(ACCENT), rgb(TEXT), 0.12)))
            .child("Update")
            .on_click(move |_, window, cx| {
                if !preview {
                    window.dispatch_action(Box::new(CheckForUpdates), cx);
                    window.refresh();
                }
            })
    })
}

/// General → About: check for updates by hand. Hidden when updates are off or
/// the updater could not start (for example outside the packaged app).
fn about_update_button(
    status: crate::sparkle::UpdateStatus,
    preview: bool,
) -> Option<gpui::Stateful<Div>> {
    use crate::sparkle::UpdateStatus;
    if !preview && (crate::sparkle::updates_managed() || status == UpdateStatus::Unavailable) {
        return None;
    }
    let label = match status {
        UpdateStatus::Checking => "Checking…",
        UpdateStatus::UpToDate => "Hello is up to date",
        UpdateStatus::UpdateAvailable => "Install update",
        UpdateStatus::Idle | UpdateStatus::Unavailable => "Check for Updates",
    };
    let button = compact_button(label)
        .id("check-for-updates-about")
        .bg(tone(SURFACE_HOVER))
        .cursor_pointer()
        .on_click(move |_, window, cx| {
            if !preview {
                window.dispatch_action(Box::new(CheckForUpdates), cx);
                window.refresh();
            }
        });
    Some(if status == UpdateStatus::UpdateAvailable {
        button.bg(rgb(ACCENT)).text_color(rgb(ON_ACCENT))
    } else {
        button
    })
}

/// The microphone menu is deferred above the settings panel and occludes
/// everything beneath it, so hovering or choosing a device never reaches the
/// controls under the menu. A mouse-down anywhere else dismisses it.
fn microphone_picker_menu(
    choices: Vec<Option<String>>,
    selected: Option<String>,
    error: Option<String>,
    choose: impl Fn(&Option<String>, &mut Window, &mut App) + 'static,
    dismiss: impl Fn(&MouseDownEvent, &mut Window, &mut App) + 'static,
) -> gpui::Stateful<Div> {
    let choose = Rc::new(choose);
    div()
        .id("microphone-picker")
        .debug_selector(|| "microphone-picker".into())
        .absolute()
        .top(px(CONTROL_HEIGHT + 4.0))
        .right_0()
        .w(px(220.0))
        .max_h(px(240.0))
        .p_2()
        .overflow_y_scroll()
        .rounded(px(12.0))
        .border_1()
        .border_color(rgba(0xffffff1f))
        .bg(rgb(POPOVER))
        .shadow_lg()
        .occlude()
        .on_mouse_down_out(dismiss)
        .children(choices.into_iter().enumerate().map(|(index, device)| {
            let is_selected = selected == device;
            let label = device.clone().unwrap_or_else(|| "Automatic".into());
            let choose = choose.clone();
            div()
                .id(("microphone-choice", index))
                .debug_selector(move || format!("microphone-choice-{index}"))
                .w_full()
                .h(px(34.0))
                .px_3()
                .flex()
                .items_center()
                .justify_between()
                .rounded_sm()
                .text_size(px(12.0))
                .text_color(if is_selected {
                    rgb(TEXT)
                } else {
                    rgb(TEXT_SOFT)
                })
                .when(is_selected, |row| row.bg(tone(SURFACE_SELECTED)))
                .hover(|row| row.bg(tone(SURFACE_HOVER)))
                .child(label)
                .on_click(move |_, window, cx| {
                    cx.stop_propagation();
                    choose(&device, window, cx);
                })
        }))
        .when_some(error, |picker, error| {
            picker.child(
                div()
                    .px_3()
                    .pt_2()
                    .text_size(px(11.0))
                    .text_color(rgb(NEGATIVE))
                    .child(error),
            )
        })
}

#[cfg(test)]
fn settings_pane(content: Div) -> AnyElement {
    settings_pane_titled("Settings", "settings-scroll", content)
}

fn settings_pane_titled(title: &'static str, scroll_id: &'static str, content: Div) -> AnyElement {
    div()
        .size_full()
        .flex()
        .flex_col()
        .child(pane_header(title))
        .child(
            div()
                .id(scroll_id)
                .flex_1()
                .overflow_y_scroll()
                .px_8()
                .pt_1()
                .pb_7()
                .child(
                    div().w_full().flex().justify_center().child(
                        content
                            .w_full()
                            .max_w(px(PANE_CONTENT_WIDTH))
                            .min_w_0()
                            .relative()
                            .debug_selector(|| "settings-content".into()),
                    ),
                ),
        )
        .into_any_element()
}

actions!(
    hello,
    [
        CloseWindow,
        CheckForUpdates,
        IndicatorAtBottom,
        IndicatorAtTop,
        IndicatorStyleClassic,
        IndicatorStyleDots,
        IndicatorStyleLine,
        IndicatorStyleOrb,
        IndicatorStyleWave,
        LanguageAuto,
        LanguageBangla,
        LanguageEnglish,
        PasteLastDictation,
        RetryCommands,
        ShowAppearance,
        ShowGeneral,
        ShowProfile,
        ShowSounds,
        HideApplication,
        MinimizeWindow,
        QuitApplication,
        ShowCommands,
        ShowHistory,
        ShowModes,
        ShowNotetaker,
        StartMeetingNotes,
        StopMeetingNotes,
        ToggleMeetingDetection,
        ShowSettings,
        ToggleFullscreen,
    ]
);

/// Opens the app shell, or focuses and refreshes an existing shell window.
#[allow(clippy::too_many_arguments)]
pub fn open_or_focus(
    app_window: &Rc<RefCell<Option<WindowHandle<AppWindow>>>>,
    event_path: PathBuf,
    commands: CommandConfig,
    meeting_requests: SyncSender<MeetingRequest>,
    indicator: DictationIndicatorSender,
    recognition_start: Option<SyncSender<()>>,
    history: Option<History>,
    cx: &mut App,
) -> gpui::Result<WindowHandle<AppWindow>> {
    if let Some(handle) = app_window.borrow().as_ref().copied()
        && handle
            .update(cx, |this, window, cx| {
                if this.event_reader.path() != event_path {
                    this.event_reader = EventReader::open(event_path.clone());
                }
                this.meeting_requests = meeting_requests.clone();
                this.indicator = indicator.clone();
                this.recognition_start = recognition_start.clone();
                this.history = history.clone();
                this.refresh(cx);
                window.activate_window();
            })
            .is_ok()
    {
        cx.activate(true);
        return Ok(handle);
    }

    open_new(
        app_window,
        event_path,
        commands,
        meeting_requests,
        indicator,
        recognition_start,
        history,
        None,
        cx,
    )
}

/// Commit through the live editor when present, preserving its unsaved settings.
/// Quiet startup does not require creating or focusing a Settings window.
pub fn commit_transcription_selection(
    app_window: &Rc<RefCell<Option<WindowHandle<AppWindow>>>>,
    selection: TranscriptionSelection,
    cx: &mut App,
) -> Result<AppSettings, String> {
    let handle = app_window.borrow().as_ref().copied();
    if let Some(handle) = handle
        && let Ok(result) = handle.update(cx, |this, _, cx| {
            this.apply_transcription_selection(selection.clone(), cx)?;
            cx.notify();
            Ok(this.settings.clone())
        })
    {
        return result;
    }
    let mut settings = AppSettings::load().map_err(|error| error.to_string())?;
    settings
        .save_transcription(selection)
        .map_err(|error| error.to_string())?;
    Ok(settings)
}

pub fn menu_transcription_selection(
    app_window: &Rc<RefCell<Option<WindowHandle<AppWindow>>>>,
    model: TranscriptionModelId,
    fallback: &AppSettings,
    cx: &mut App,
) -> TranscriptionSelection {
    let handle = app_window.borrow().as_ref().copied();
    handle
        .and_then(|handle| {
            handle
                .update(cx, |this, _, _| {
                    this.settings.transcription_for_model(model)
                })
                .ok()
        })
        .unwrap_or_else(|| fallback.transcription_for_model(model))
}

/// The open window's settings, the freshest copy when it exists.
pub fn current_settings(
    app_window: &Rc<RefCell<Option<WindowHandle<AppWindow>>>>,
    cx: &mut App,
) -> Option<AppSettings> {
    let handle = app_window.borrow().as_ref().copied()?;
    handle.update(cx, |this, _, _| this.settings.clone()).ok()
}

/// Applies a menu-bar settings change through the window when it exists so
/// its controls stay in sync, else straight to disk.
pub fn apply_menu_settings(
    app_window: &Rc<RefCell<Option<WindowHandle<AppWindow>>>>,
    cx: &mut App,
    update: impl Fn(&mut AppSettings),
) {
    let handle = app_window.borrow().as_ref().copied();
    if let Some(handle) = handle
        && handle
            .update(cx, |this, _, cx| {
                if this.update_settings(cx, &update) {
                    this.sync_menu_controls();
                }
            })
            .is_ok()
    {
        return;
    }
    let result = AppSettings::load().and_then(|mut settings| {
        update(&mut settings);
        settings.save()
    });
    if let Err(error) = result {
        tracing::error!(%error, "could not save menu bar setting");
    }
}

/// Menu bar language choice: switches the second language on when needed.
pub fn menu_choose_language(
    app_window: &Rc<RefCell<Option<WindowHandle<AppWindow>>>>,
    routing: LanguageRouting,
    cx: &mut App,
) {
    let handle = app_window.borrow().as_ref().copied();
    if let Some(handle) = handle
        && handle
            .update(cx, |this, _, cx| this.choose_language(routing, cx))
            .is_ok()
    {
        return;
    }
    apply_menu_settings(app_window, cx, |settings| {
        settings.multilingual.enabled = true;
        settings.multilingual.routing = routing;
    });
}

pub fn refresh_transcription_menu(
    app_window: &Rc<RefCell<Option<WindowHandle<AppWindow>>>>,
    fallback: &AppSettings,
    status: &PreparationStatus,
    cx: &mut App,
) {
    let handle = app_window.borrow().as_ref().copied();
    if handle.is_some_and(|handle| {
        handle
            .update(cx, |this, _, _| {
                crate::status_item::update_transcription(&this.settings, status);
            })
            .is_ok()
    }) {
        return;
    }
    crate::status_item::update_transcription(fallback, status);
}

pub fn open_preview(
    app_window: &Rc<RefCell<Option<WindowHandle<AppWindow>>>>,
    event_path: PathBuf,
    commands: CommandConfig,
    meeting_requests: SyncSender<MeetingRequest>,
    indicator: DictationIndicatorSender,
    preview: AppWindowPreview,
    cx: &mut App,
) -> gpui::Result<WindowHandle<AppWindow>> {
    if let Some(handle) = app_window.borrow().as_ref().copied()
        && handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
    {
        cx.activate(true);
        return Ok(handle);
    }
    open_new(
        app_window,
        event_path,
        commands,
        meeting_requests,
        indicator,
        None,
        None,
        Some(preview),
        cx,
    )
}

#[allow(clippy::too_many_arguments)]
fn open_new(
    app_window: &Rc<RefCell<Option<WindowHandle<AppWindow>>>>,
    event_path: PathBuf,
    commands: CommandConfig,
    meeting_requests: SyncSender<MeetingRequest>,
    indicator: DictationIndicatorSender,
    recognition_start: Option<SyncSender<()>>,
    history: Option<History>,
    preview: Option<AppWindowPreview>,
    cx: &mut App,
) -> gpui::Result<WindowHandle<AppWindow>> {
    let preview_mode = preview.is_some();
    let bounds = Bounds::centered(None, size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)), cx);
    let handle = cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some("Hello".into()),
                appears_transparent: true,
                ..Default::default()
            }),
            is_resizable: !preview_mode,
            is_minimizable: true,
            window_min_size: Some(size(px(MINIMUM_WIDTH), px(MINIMUM_HEIGHT))),
            tabbing_identifier: preview_mode.then(|| "hello-preview".into()),
            ..Default::default()
        },
        |window, cx| {
            cx.new(|cx| {
                AppWindow::new(
                    event_path,
                    commands,
                    meeting_requests,
                    indicator,
                    recognition_start,
                    history,
                    preview,
                    window,
                    cx,
                )
            })
        },
    )?;
    *app_window.borrow_mut() = Some(handle);
    cx.activate(true);
    Ok(handle)
}

#[derive(Clone, Copy)]
pub enum PreviewModelState {
    Actual,
    Installed,
    Missing,
    Downloading,
    Error,
}

#[derive(Clone)]
pub struct AppWindowPreview {
    pub pane: DeveloperPane,
    pub transcription_picker: Option<(String, PreviewModelState)>,
    pub onboarding: bool,
    pub confirm_model_deletion: bool,
    pub open_transformation_picker: bool,
    pub select_global_mode: bool,
    pub permissions_missing: bool,
    pub model_missing: bool,
    pub command_model_missing: bool,
    pub open_history_retention: bool,
    pub confirm_release_microphone: bool,
    pub update_available: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum Pane {
    HudLab,
    Meetings,
    Commands,
    Activity,
    History,
    Modes,
    #[default]
    Settings,
    General,
    Appearance,
    Sounds,
    Profile,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HudDemoState {
    Recording,
    Transcribing,
    Processing,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HudControl {
    LineCount,
    Curvature,
    Speed,
    Blur,
    Glow,
    Depth,
    LightAngle,
    Outline,
}

const HUD_MAX_SPEED: f32 = 24.0;

impl Pane {
    fn on_reopen(self, status: SetupStatus) -> Self {
        if crate::onboarding::permission_warnings(status).is_empty() {
            self
        } else {
            Self::Settings
        }
    }

    const ALL: [Self; 11] = [
        Self::Profile,
        Self::General,
        Self::Settings,
        Self::Appearance,
        Self::Sounds,
        Self::Modes,
        Self::Commands,
        Self::Meetings,
        Self::History,
        Self::HudLab,
        Self::Activity,
    ];

    fn all(developer_features: bool) -> Vec<Self> {
        Self::ALL
            .into_iter()
            .filter(|pane| match pane {
                Self::Profile
                | Self::General
                | Self::Settings
                | Self::Appearance
                | Self::Sounds
                | Self::Modes
                | Self::Commands
                | Self::History => true,
                Self::Meetings => crate::MEETINGS_ENABLED,
                Self::HudLab | Self::Activity => developer_features,
            })
            .collect()
    }

    fn label(self) -> &'static str {
        match self {
            Self::HudLab => "HUD Lab",
            Self::Meetings => "Notetaker",
            Self::Commands => "Commands",
            Self::Activity => "Activity",
            Self::History => "History",
            Self::Modes => "Modes",
            Self::Settings => "Dictation",
            Self::General => "General",
            Self::Appearance => "Appearance",
            Self::Sounds => "Sounds",
            Self::Profile => "Profile",
        }
    }

    /// The sidebar heading each pane is listed under.
    fn group(self) -> &'static str {
        match self {
            Self::Profile | Self::General | Self::Settings => "GENERAL",
            Self::Appearance | Self::Sounds => "PERSONALIZE",
            Self::Modes | Self::Commands => "WORKFLOWS",
            Self::Meetings | Self::History => "LIBRARY",
            Self::HudLab | Self::Activity => "DEVELOPER",
        }
    }

    fn icon(self) -> NavigationIcon {
        match self {
            Self::Settings => NavigationIcon::Settings,
            Self::General => NavigationIcon::General,
            Self::Appearance => NavigationIcon::Appearance,
            Self::Sounds => NavigationIcon::Sounds,
            Self::Profile => NavigationIcon::Profile,
            Self::Modes => NavigationIcon::Modes,
            Self::History => NavigationIcon::History,
            Self::Commands => NavigationIcon::Commands,
            Self::Meetings => NavigationIcon::Meetings,
            Self::Activity => NavigationIcon::Activity,
            Self::HudLab => NavigationIcon::HudLab,
        }
    }

    fn from_developer(pane: DeveloperPane) -> Self {
        match pane {
            DeveloperPane::Settings => Self::Settings,
            DeveloperPane::General => Self::General,
            DeveloperPane::Appearance => Self::Appearance,
            DeveloperPane::Sounds => Self::Sounds,
            DeveloperPane::Profile => Self::Profile,
            DeveloperPane::Modes => Self::Modes,
            DeveloperPane::History => Self::History,
            DeveloperPane::HudLab => Self::HudLab,
            DeveloperPane::Commands => Self::Commands,
            DeveloperPane::Meetings => Self::Meetings,
            DeveloperPane::Activity => Self::Activity,
        }
    }

    fn developer(self) -> DeveloperPane {
        match self {
            Self::Settings => DeveloperPane::Settings,
            Self::General => DeveloperPane::General,
            Self::Appearance => DeveloperPane::Appearance,
            Self::Sounds => DeveloperPane::Sounds,
            Self::Profile => DeveloperPane::Profile,
            Self::Modes => DeveloperPane::Modes,
            Self::History => DeveloperPane::History,
            Self::HudLab => DeveloperPane::HudLab,
            Self::Commands => DeveloperPane::Commands,
            Self::Meetings => DeveloperPane::Meetings,
            Self::Activity => DeveloperPane::Activity,
        }
    }
}

#[derive(Clone)]
struct CatalogCommand {
    task: String,
    scope: CommandScope,
    phrase: String,
    aliases: Vec<String>,
    description: String,
    id: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PersonalWorkspaceState {
    Missing,
    Creating,
    Ready,
    Error,
}

fn personal_workspace_state(
    config_exists: bool,
    creating: bool,
    status: &StatusSnapshot,
) -> PersonalWorkspaceState {
    if creating {
        PersonalWorkspaceState::Creating
    } else if config_exists {
        if status.host_state == HostState::Unavailable && status.active_generation.is_none() {
            PersonalWorkspaceState::Error
        } else {
            PersonalWorkspaceState::Ready
        }
    } else {
        PersonalWorkspaceState::Missing
    }
}

#[derive(Clone, PartialEq)]
struct TranscriptLine {
    speaker: meeting::MeetingSource,
    /// Which of the other people spoke (speakers.json), when known.
    speaker_id: Option<u32>,
    name: String,
    at: String,
    text: String,
    pause_before_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MeetingTab {
    Transcript,
    Overview,
    Notes,
    Summary,
}

#[derive(Clone)]
enum MeetingTranscript {
    Empty,
    Loaded(Vec<TranscriptLine>),
    Unavailable(String),
}

struct ToggleSpring {
    position: f32,
    velocity: f32,
    target: f32,
    last_frame: Instant,
}

struct ProcessingInput {
    entity: Entity<TextInput>,
    _subscriptions: Vec<Subscription>,
}

struct ModeInputs {
    name: ProcessingInput,
    browser_hosts: ProcessingInput,
    replacements: Vec<ReplacementInputs>,
}

struct ModeEditor {
    // Authoritative editor draft; AppSettings receives a projection only when
    // constructing a save candidate, never by keeping parallel indices in sync.
    settings: DictationMode,
    inputs: ModeInputs,
}

struct ModeEditors {
    default_mode: ModeEditor,
    modes: Vec<ModeEditor>,
}

impl ModeEditors {
    fn project(&self, settings: &mut AppSettings) {
        settings.dictation_processing.default_mode = self.default_mode.settings.clone();
        settings.dictation_processing.modes =
            self.modes.iter().map(|row| row.settings.clone()).collect();
    }
}

struct ReplacementInputs {
    matched_phrase: ProcessingInput,
    output: ProcessingInput,
}

#[derive(Clone)]
struct TransformationDrag {
    selection: ModeSelection,
    id: String,
    name: String,
    position: Point<Pixels>,
}

impl TransformationDrag {
    fn at(mut self, position: Point<Pixels>) -> Self {
        self.position = position;
        self
    }
}

impl Render for TransformationDrag {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .pl(self.position.x - px(90.0))
            .pt(self.position.y - px(17.0))
            .child(
                div()
                    .w(px(180.0))
                    .h(px(34.0))
                    .px_3()
                    .flex()
                    .items_center()
                    .rounded(px(6.0))
                    .border_1()
                    .border_color(rgb(0x505050))
                    .bg(tone(SURFACE_SELECTED))
                    .text_size(px(11.0))
                    .text_color(rgb(TEXT))
                    .child(self.name.clone()),
            )
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ModeSelection {
    Default,
    Custom(usize),
}

enum ApplicationCatalogState {
    Loading,
    Loaded(Vec<InstalledApplication>),
}

impl ApplicationCatalogState {
    fn application_named(&self, name: &str) -> Option<&InstalledApplication> {
        let Self::Loaded(applications) = self else {
            return None;
        };
        applications
            .iter()
            .find(|application| application.name == name)
    }
}

/// A helper model download started from the Languages settings.
struct LanguageDownload {
    model: TranscriptionModelId,
    bytes: Arc<AtomicU64>,
    done: Receiver<std::result::Result<(), String>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HotkeyKind {
    Dictation,
    PasteLast,
}

fn set_hotkey_binding(settings: &mut AppSettings, kind: HotkeyKind, binding: HotkeyBinding) {
    match kind {
        HotkeyKind::Dictation => {
            if binding.key.is_none() {
                settings.double_tap_only = false;
            }
            settings.dictation_hotkey = binding;
        }
        HotkeyKind::PasteLast => settings.paste_last_hotkey = Some(binding),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MicrophonePolicyChange {
    SetCommands(bool),
    SetReleaseWhileIdle(bool),
    KeepReadyAndEnableCommands,
    DisableCommandsAndRelease,
}

enum HotkeyCaptureState {
    Idle,
    Listening {
        kind: HotkeyKind,
        modifiers: HotkeyModifiers,
        message: Option<&'static str>,
        started_at: Instant,
    },
    Saved {
        kind: HotkeyKind,
        saved_at: Instant,
    },
}

impl HotkeyCaptureState {
    fn is_listening(&self) -> bool {
        matches!(self, Self::Listening { .. })
    }
}

impl ToggleSpring {
    fn new(enabled: bool) -> Self {
        let position = if enabled { 1.0 } else { 0.0 };
        Self::at(position)
    }

    fn at(position: f32) -> Self {
        Self {
            position,
            velocity: 0.0,
            target: position,
            last_frame: Instant::now(),
        }
    }

    fn set_enabled(&mut self, enabled: bool) {
        self.set_target(if enabled { 1.0 } else { 0.0 });
    }

    fn set_target(&mut self, target: f32) {
        if self.target == target {
            return;
        }
        self.target = target;
        self.last_frame = Instant::now();
    }

    fn advance(&mut self, elapsed: Duration) {
        /// Critically damped spring stiffness in rad/s; higher settles faster.
        const STIFFNESS: f32 = 40.0;
        let mut remaining = elapsed.as_secs_f32().min(0.1);
        while remaining > 0.0 {
            let dt = remaining.min(1.0 / 240.0);
            let acceleration = -STIFFNESS.powi(2) * (self.position - self.target)
                - 2.0 * STIFFNESS * self.velocity;
            self.velocity += acceleration * dt;
            self.position += self.velocity * dt;
            remaining -= dt;
        }
        if self.is_settled() {
            self.position = self.target;
            self.velocity = 0.0;
        }
    }

    fn render_position(&mut self, window: &mut Window) -> f32 {
        let now = Instant::now();
        self.advance(now.duration_since(self.last_frame));
        self.last_frame = now;
        if !self.is_settled() {
            window.request_animation_frame();
        }
        self.position
    }

    fn is_settled(&self) -> bool {
        (self.position - self.target).abs() < 0.001 && self.velocity.abs() < 0.01
    }

    fn enabled(&self) -> bool {
        self.target >= 0.5
    }
}

/// Root GPUI entity for the production app shell.
pub struct AppWindow {
    preview: bool,
    pane: Pane,
    event_reader: EventReader,
    activity: DesktopActivity,
    meeting_requests: SyncSender<MeetingRequest>,
    indicator: DictationIndicatorSender,
    hud_tuning: HudTuning,
    hud_demo: HudDemoState,
    hud_queued: usize,
    hud_dragging: Option<HudControl>,
    hud_copied: bool,
    recognition_start: Option<SyncSender<()>>,
    setup_status: SetupStatus,
    setup_visible: bool,
    onboarding_completed: bool,
    permission_refresh_at: Instant,
    settings: AppSettings,
    settings_load_error: Option<String>,
    #[cfg(test)]
    settings_save_override: Option<fn(&AppSettings) -> color_eyre::Result<()>>,
    microphone_devices: Vec<String>,
    microphone_picker_open: bool,
    microphone_picker_error: Option<String>,
    launch_at_login_status: Option<LoginItemStatus>,
    login_item_worker: Option<LoginItemWorker>,
    launch_at_login_error: Option<String>,
    launch_at_login_toggle: ToggleSpring,
    update_status: crate::sparkle::UpdateStatus,
    update_status_changed_at: Instant,
    commands_toggle: ToggleSpring,
    command_model_status: CommandModelStatus,
    release_microphone_toggle: ToggleSpring,
    pending_microphone_policy: Option<MicrophonePolicyChange>,
    double_tap_toggle: ToggleSpring,
    double_tap_only_visibility: ToggleSpring,
    dock_icon_toggle: ToggleSpring,
    multilingual_toggle: ToggleSpring,
    language_download: Option<LanguageDownload>,
    language_download_error: Option<String>,
    sound_volume_spring: ToggleSpring,
    recording_audio_spring: ToggleSpring,
    indicator_position_spring: ToggleSpring,
    theme_spring: ToggleSpring,
    menu_bar_icon_toggle: ToggleSpring,
    profile_name: ProcessingInput,
    appearance_clock: Instant,
    smart_editing_toggles: [ToggleSpring; 5],
    transcription_hints: ProcessingInput,
    transcription_picker_language: Option<String>,
    transcription_status: PreparationStatus,
    transcription_preparation: TranscriptionPreparation,
    transcription_preview_installed: Option<bool>,
    transcription_deletion: Option<TranscriptionModelId>,
    transcription_deletion_error: Option<String>,
    mode_editors: ModeEditors,
    selected_mode: ModeSelection,
    application_catalog: ApplicationCatalogState,
    application_catalog_receiver: Option<Receiver<Vec<InstalledApplication>>>,
    application_search: ProcessingInput,
    application_picker_open: bool,
    application_picker_error: Option<String>,
    transformation_picker_open: bool,
    application_picker_highlight: usize,
    mode_delete_armed: bool,
    mode_context_menu: Option<Point<Pixels>>,
    settings_save_generation: u64,
    settings_dirty: bool,
    hotkey_capture: HotkeyCaptureState,
    hotkey_capture_animation: ToggleSpring,
    hotkey_width_spring: ToggleSpring,
    hotkey_side_animations: [ToggleSpring; 2],
    hotkey_side_selection_springs: [ToggleSpring; 2],
    window_focus: FocusHandle,
    hotkey_focus: FocusHandle,
    _hotkey_blur_subscription: Subscription,
    _activation_subscription: Subscription,
    _appearance_subscription: Subscription,
    meetings: Vec<MeetingManifest>,
    meetings_error: Option<String>,
    meeting_runtime_error: Option<String>,
    meeting_refresh_started_ms: Option<u64>,
    meeting_stop_pending: bool,
    selected_meeting: Option<String>,
    meeting_transcript: MeetingTranscript,
    meeting_transcript_id: Option<String>,
    meeting_transcript_reader: meeting::TranscriptReader,
    meeting_transcript_scroll: ScrollHandle,
    meeting_tab: MeetingTab,
    meeting_notes: ProcessingInput,
    meeting_notes_id: Option<String>,
    meeting_retention_open: bool,
    meeting_detect_toggle: ToggleSpring,
    meeting_calendar_toggle: ToggleSpring,
    language_samples_toggle: ToggleSpring,
    meeting_copied: bool,
    /// The meeting whose delete button was pressed once (asks to confirm).
    meeting_delete_armed: Option<String>,
    /// How long you spoke in the selected meeting.
    meeting_you_talk_ms: u64,
    /// The speaker being named in the Overview tab.
    meeting_speaker_editing: Option<u32>,
    meeting_speaker_input: ProcessingInput,
    speaker_download: Option<(Arc<AtomicU64>, Receiver<Result<(), String>>)>,
    speaker_download_error: Option<String>,
    screen_capture_allowed: Option<bool>,
    compiled_commands: Vec<CatalogCommand>,
    commands: Vec<CatalogCommand>,
    personal_commands_status: StatusSnapshot,
    personal_workspace: PathBuf,
    personal_workspace_receiver: Option<Receiver<Result<PathBuf, String>>>,
    personal_workspace_error: Option<String>,
    personal_commands_prompt_copied: bool,
    command_search: ProcessingInput,
    selected_command: Option<String>,
    events: Vec<VoiceEvent>,
    current_context: (Option<String>, Option<String>),
    selected_event: Option<usize>,
    history: Option<History>,
    history_search: ProcessingInput,
    history_entries: Vec<HistoryEntry>,
    history_insights: HistoryInsights,
    selected_history: Option<u64>,
    history_error: Option<String>,
    history_clear_armed: bool,
    history_retention_open: bool,
    history_copied: Option<u64>,
}

impl AppWindow {
    #[allow(clippy::too_many_arguments)]
    fn new(
        event_path: PathBuf,
        commands: CommandConfig,
        meeting_requests: SyncSender<MeetingRequest>,
        indicator: DictationIndicatorSender,
        recognition_start: Option<SyncSender<()>>,
        history: Option<History>,
        preview: Option<AppWindowPreview>,
        native_window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        cx.spawn(async move |window, cx| {
            loop {
                Timer::after(Duration::from_millis(500)).await;
                if window
                    .update(cx, |window, cx| {
                        let application_catalog_changed = window.poll_application_catalog();
                        let transcription_changed = window.poll_transcription_download()
                            | window.poll_language_download()
                            | window.poll_speaker_download();
                        let setup_changed = window.poll_setup(false);
                        let command_model_status = if window.preview {
                            window.command_model_status
                        } else {
                            crate::recognition::command_model_status()
                        };
                        let command_model_changed =
                            window.command_model_status != command_model_status;
                        window.command_model_status = command_model_status;
                        let login_item_changed = window.poll_login_item();
                        let personal_commands_changed = window.poll_personal_commands();
                        let personal_workspace_changed = window.poll_personal_workspace();
                        let history_changed = window.poll_history(cx);
                        let update_status = if window.preview {
                            window.update_status
                        } else {
                            crate::sparkle::status()
                        };
                        let update_status_changed = window.update_status != update_status;
                        if update_status_changed {
                            window.update_status = update_status;
                            window.update_status_changed_at = Instant::now();
                        }
                        let update_confirmation_expired = window.update_status
                            == crate::sparkle::UpdateStatus::UpToDate
                            && window.update_status_changed_at.elapsed() >= Duration::from_secs(4);
                        if update_confirmation_expired {
                            window.update_status = crate::sparkle::UpdateStatus::Idle;
                            crate::sparkle::clear_confirmation();
                        }
                        if crate::MEETINGS_ENABLED
                            && (window.meeting_refresh_started_ms.is_some()
                                || meeting::active(&window.meetings).is_some())
                        {
                            window.reload_meetings();
                            cx.notify();
                        }
                        if application_catalog_changed
                            || transcription_changed
                            || setup_changed
                            || command_model_changed
                            || login_item_changed
                            || personal_commands_changed
                            || personal_workspace_changed
                            || history_changed
                            || update_status_changed
                            || update_confirmation_expired
                        {
                            cx.notify();
                        }
                    })
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        let preview_mode = preview.is_some();
        let preview_choices = crate::transcription_models::choices_for_runtime;
        let (settings, settings_load_error) = if let Some(preview) = &preview {
            let mut settings = AppSettings {
                commands_enabled: preview.command_model_missing
                    || preview.confirm_release_microphone,
                ..AppSettings::default()
            };
            if let Some((language, _)) = &preview.transcription_picker
                && let Some(choice) = preview_choices(language).first()
            {
                settings.transcription.model = choice.model.id;
                settings.transcription.language = language.clone();
            }
            if matches!(preview.pane, DeveloperPane::Modes) {
                let mode = DictationMode {
                    name: "Work notes".into(),
                    applications: vec!["Zed".into()],
                    browser_hosts: vec!["github.com".into()],
                    replacements: vec![TextReplacement {
                        matched_phrase: "git hub".into(),
                        output: "GitHub".into(),
                    }],
                    transformations: vec!["smart-punctuation".into(), "normalize-links".into()],
                    ..DictationMode::default()
                };
                settings.dictation_processing.modes.push(mode);
            }
            (settings, None)
        } else {
            match AppSettings::load() {
                Ok(settings) => (settings, None),
                Err(error) => {
                    tracing::error!(%error, "could not load app settings");
                    (
                        AppSettings::default(),
                        Some(format!("Could not load app settings: {error:#}")),
                    )
                }
            }
        };
        let transcription_hints =
            Self::transcription_hints_input(&settings.transcription.recognition_hints, cx);
        let profile_name = Self::profile_name_input(&settings.profile_name, cx);
        let mode_editors = Self::mode_editors(&settings, cx);
        let application_catalog = if preview_mode {
            ApplicationCatalogState::Loaded(Vec::new())
        } else {
            ApplicationCatalogState::Loading
        };
        let application_search = Self::application_search_input(cx);
        if !preview_mode {
            crate::app_settings::set_dock_icon_visible(true);
        }
        let window_focus = cx.focus_handle();
        window_focus.focus(native_window);
        let hotkey_focus = cx.focus_handle();
        let hotkey_blur_subscription = cx.on_blur(&hotkey_focus, native_window, |this, _, cx| {
            this.cancel_hotkey_capture(cx)
        });
        let activation_subscription =
            cx.observe_window_activation(native_window, |this, window, cx| {
                if window.is_window_active() {
                    this.permission_refresh_at = Instant::now();
                    if this.poll_setup(true) {
                        cx.notify();
                    }
                }
            });
        // Auto follows the Mac, so redraw when it switches between light and dark.
        let appearance_subscription =
            cx.observe_window_appearance(native_window, |_, _, cx| cx.notify());
        let compiled_commands = commands
            .catalog()
            .into_iter()
            .map(|command| CatalogCommand {
                task: command_task(&command),
                scope: command.scope,
                phrase: command.phrase,
                aliases: command.aliases,
                description: command.description,
                id: command.id,
            })
            .collect::<Vec<_>>();
        let personal_workspace = if preview_mode {
            PathBuf::from("~/.config/hello")
        } else {
            crate::app_paths::personal_commands_workspace()
                .unwrap_or_else(|_| PathBuf::from("~/.config/hello"))
        };
        let mut personal_commands_status = if preview_mode {
            StatusSnapshot {
                transformations: vec![
                    StatusTransformation {
                        id: "smart-punctuation".into(),
                        name: "Smart punctuation".into(),
                        description: None,
                    },
                    StatusTransformation {
                        id: "normalize-links".into(),
                        name: "Normalize links".into(),
                        description: None,
                    },
                ],
                ..StatusSnapshot::default()
            }
        } else {
            crate::personal_commands::load_status().unwrap_or_else(|error| StatusSnapshot {
                host_state: HostState::Unavailable,
                last_reload_error: Some(error.to_string()),
                ..StatusSnapshot::default()
            })
        };
        crate::personal_commands::include_builtin_transformations(&mut personal_commands_status);
        let commands = merged_catalog(&compiled_commands, &personal_commands_status);
        let preview_picker = preview
            .as_ref()
            .and_then(|preview| preview.transcription_picker.as_ref());
        let preview_model_missing = preview
            .as_ref()
            .is_some_and(|preview| preview.model_missing);
        let preview_model_state = preview_picker.map(|(_, state)| *state);
        let preview_downloading = preview_picker.and_then(|(language, state)| {
            matches!(state, PreviewModelState::Downloading)
                .then(|| preview_choices(language)[0].model.id)
        });
        let selected_model = validate(&settings.transcription).ok();
        let setup_status = if preview.as_ref().is_some_and(|preview| preview.onboarding) {
            SetupStatus {
                microphone: PermissionState::NeedsRequest,
                input_monitoring: PermissionState::NeedsRequest,
                accessibility: PermissionState::NeedsRequest,
                transcription_model: false,
            }
        } else if preview
            .as_ref()
            .is_some_and(|preview| preview.permissions_missing)
        {
            SetupStatus {
                microphone: PermissionState::NeedsRequest,
                input_monitoring: PermissionState::NeedsSettings,
                accessibility: PermissionState::NeedsSettings,
                transcription_model: !preview_model_missing,
            }
        } else if preview_mode {
            SetupStatus {
                microphone: PermissionState::Ready,
                input_monitoring: PermissionState::Ready,
                accessibility: PermissionState::Ready,
                transcription_model: !preview_model_missing,
            }
        } else {
            crate::onboarding::status_with_transcription_model(selected_model.is_some_and(
                |model| {
                    crate::transcription_models::is_installed(
                        model,
                        &settings.transcription.language,
                    )
                },
            ))
        };
        let onboarding_completed = preview_mode || crate::onboarding::completion_recorded();
        let setup_visible = preview.as_ref().is_some_and(|preview| preview.onboarding)
            || !onboarding_completed && !setup_status.ready();
        let microphone_devices = if preview_mode {
            vec!["Built-in Microphone".into()]
        } else {
            crate::audio::input_device_names().unwrap_or_else(|error| {
                tracing::warn!(%error, "could not list microphones for settings");
                Vec::new()
            })
        };
        let launch_at_login_status = preview_mode.then_some(LoginItemStatus::Disabled);
        let (login_item_worker, launch_at_login_error) = if preview_mode {
            (None, None)
        } else {
            match LoginItemWorker::new() {
                Ok(worker) => (Some(worker), None),
                Err(error) => (None, Some(error)),
            }
        };
        let history = if preview_mode {
            preview
                .as_ref()
                .is_some_and(|preview| matches!(preview.pane, DeveloperPane::History))
                .then(preview_history)
                .flatten()
        } else {
            history
        };
        let history_search = Self::history_search_input(cx);
        let meeting_notes = Self::meeting_notes_input(cx);
        let meeting_speaker_input = Self::processing_input("Their name", "", cx);
        let detect_meetings = settings.notetaker.detect_meetings;
        let use_calendar = settings.notetaker.use_calendar;
        let collect_samples = settings.multilingual.collect_samples;
        let command_search = Self::command_search_input(cx);
        let mut window = Self {
            preview: preview_mode,
            pane: preview.as_ref().map_or(Pane::default(), |preview| {
                Pane::from_developer(preview.pane)
            }),
            event_reader: EventReader::open(event_path),
            activity: DesktopActivity::default(),
            meeting_requests,
            indicator,
            hud_tuning: HudTuning::default(),
            hud_demo: HudDemoState::Transcribing,
            hud_queued: 1,
            hud_dragging: None,
            hud_copied: false,
            recognition_start,
            setup_status,
            setup_visible,
            onboarding_completed,
            permission_refresh_at: Instant::now() + PERMISSION_REFRESH_INTERVAL,
            microphone_devices,
            microphone_picker_open: false,
            microphone_picker_error: None,
            launch_at_login_status,
            login_item_worker,
            launch_at_login_error,
            launch_at_login_toggle: ToggleSpring::new(
                launch_at_login_status == Some(LoginItemStatus::Enabled),
            ),
            update_status: if let Some(preview) = &preview {
                if preview.update_available {
                    crate::sparkle::UpdateStatus::UpdateAvailable
                } else {
                    crate::sparkle::UpdateStatus::Idle
                }
            } else {
                crate::sparkle::status()
            },
            update_status_changed_at: Instant::now(),
            commands_toggle: ToggleSpring::new(settings.commands_enabled),
            command_model_status: if preview
                .as_ref()
                .is_some_and(|preview| preview.command_model_missing)
            {
                CommandModelStatus::Failed
            } else if preview_mode {
                CommandModelStatus::Disabled
            } else {
                crate::recognition::command_model_status()
            },
            release_microphone_toggle: ToggleSpring::new(settings.release_microphone_while_idle),
            pending_microphone_policy: preview
                .as_ref()
                .filter(|preview| preview.confirm_release_microphone)
                .map(|_| MicrophonePolicyChange::DisableCommandsAndRelease),
            double_tap_toggle: ToggleSpring::new(settings.double_tap_lock),
            double_tap_only_visibility: ToggleSpring::new(
                settings.double_tap_lock && settings.dictation_hotkey.key.is_some(),
            ),
            dock_icon_toggle: ToggleSpring::new(settings.show_dock_icon),
            multilingual_toggle: ToggleSpring::new(settings.multilingual.enabled),
            language_download: None,
            language_download_error: None,
            sound_volume_spring: ToggleSpring::at(sound_volume_index(&settings) as f32),
            recording_audio_spring: ToggleSpring::at(recording_audio_index(
                settings.recording_audio_behavior,
            ) as f32),
            indicator_position_spring: ToggleSpring::at(indicator_position_index(
                IndicatorPosition::resolve(settings.indicator_position, settings.indicator_style),
            ) as f32),
            theme_spring: ToggleSpring::at(theme_index(settings.theme) as f32),
            menu_bar_icon_toggle: ToggleSpring::new(settings.show_menu_bar_icon),
            profile_name,
            appearance_clock: Instant::now(),
            smart_editing_toggles: smart_editing_flags(settings.smart_editing)
                .map(ToggleSpring::new),
            transcription_hints,
            transcription_picker_language: preview_picker.map(|(language, _)| language.clone()),
            transcription_status: PreparationStatus {
                model: preview_downloading,
                downloaded_bytes: preview_downloading
                    .and_then(|model| definition(model).download_bytes())
                    .map_or(0, |bytes| bytes * 37 / 100),
                error: matches!(preview_model_state, Some(PreviewModelState::Error))
                    .then(|| "Preview download failed while verifying the model checksum.".into()),
                ..Default::default()
            },
            transcription_preparation: if preview.is_some() {
                TranscriptionPreparation::default()
            } else {
                cx.default_global::<TranscriptionPreparation>().clone()
            },
            transcription_preview_installed: match preview_model_state {
                Some(PreviewModelState::Installed) => Some(true),
                Some(
                    PreviewModelState::Missing
                    | PreviewModelState::Downloading
                    | PreviewModelState::Error,
                ) => Some(false),
                Some(PreviewModelState::Actual) | None => preview_model_missing.then_some(false),
            },
            transcription_deletion: preview_picker
                .filter(|_| {
                    preview
                        .as_ref()
                        .is_some_and(|preview| preview.confirm_model_deletion)
                })
                .and_then(|(language, _)| {
                    crate::transcription_models::choices_for_runtime(language)
                        .into_iter()
                        .map(|choice| choice.model)
                        .find(|model| {
                            matches!(model.runtime, ModelRuntime::Gguf(_))
                                && model.id != settings.transcription.model
                        })
                        .map(|model| model.id)
                }),
            transcription_deletion_error: None,
            mode_editors,
            selected_mode: if preview
                .as_ref()
                .is_some_and(|preview| preview.select_global_mode)
            {
                ModeSelection::Default
            } else if preview
                .as_ref()
                .is_some_and(|preview| matches!(preview.pane, DeveloperPane::Modes))
            {
                ModeSelection::Custom(0)
            } else {
                ModeSelection::Default
            },
            application_catalog,
            application_catalog_receiver: None,
            application_search,
            application_picker_open: false,
            application_picker_error: None,
            transformation_picker_open: preview
                .as_ref()
                .is_some_and(|preview| preview.open_transformation_picker),
            application_picker_highlight: 0,
            mode_delete_armed: false,
            mode_context_menu: None,
            settings_save_generation: 0,
            settings_dirty: false,
            hotkey_capture: HotkeyCaptureState::Idle,
            hotkey_capture_animation: ToggleSpring::new(false),
            hotkey_width_spring: ToggleSpring::at(HOTKEY_MIN_WIDTH),
            hotkey_side_animations: [HotkeyKind::Dictation, HotkeyKind::PasteLast].map(|kind| {
                ToggleSpring::new(
                    hotkey_binding(&settings, kind)
                        .and_then(standalone_modifier_side)
                        .is_some(),
                )
            }),
            hotkey_side_selection_springs: [HotkeyKind::Dictation, HotkeyKind::PasteLast].map(
                |kind| {
                    ToggleSpring::at(hotkey_side_index(
                        hotkey_binding(&settings, kind)
                            .and_then(standalone_modifier_side)
                            .unwrap_or(ModifierSide::Either),
                    ) as f32)
                },
            ),
            window_focus,
            hotkey_focus,
            _hotkey_blur_subscription: hotkey_blur_subscription,
            _activation_subscription: activation_subscription,
            _appearance_subscription: appearance_subscription,
            settings,
            settings_load_error,
            #[cfg(test)]
            settings_save_override: None,
            meetings: Vec::new(),
            meetings_error: None,
            meeting_runtime_error: None,
            meeting_refresh_started_ms: None,
            meeting_stop_pending: false,
            selected_meeting: None,
            meeting_transcript: MeetingTranscript::Empty,
            meeting_transcript_id: None,
            meeting_transcript_reader: meeting::TranscriptReader::default(),
            meeting_transcript_scroll: ScrollHandle::new(),
            compiled_commands,
            commands,
            personal_commands_status,
            personal_workspace,
            personal_workspace_receiver: None,
            personal_workspace_error: None,
            personal_commands_prompt_copied: false,
            command_search,
            selected_command: None,
            events: Vec::new(),
            current_context: (None, None),
            selected_event: None,
            history,
            history_search,
            history_entries: Vec::new(),
            history_insights: HistoryInsights::default(),
            selected_history: None,
            history_error: None,
            history_clear_armed: false,
            history_retention_open: preview
                .as_ref()
                .is_some_and(|preview| preview.open_history_retention),
            history_copied: None,
            meeting_tab: MeetingTab::Transcript,
            meeting_notes,
            meeting_notes_id: None,
            meeting_retention_open: false,
            meeting_detect_toggle: ToggleSpring::new(detect_meetings),
            meeting_calendar_toggle: ToggleSpring::new(use_calendar),
            language_samples_toggle: ToggleSpring::new(collect_samples),
            meeting_copied: false,
            meeting_delete_armed: None,
            meeting_you_talk_ms: 0,
            meeting_speaker_editing: None,
            meeting_speaker_input,
            speaker_download: None,
            speaker_download_error: None,
            screen_capture_allowed: None,
        };
        if !window.preview {
            if crate::MEETINGS_ENABLED {
                window.reload_meetings();
                if meeting::active(&window.meetings).is_some() {
                    window.pane = Pane::Meetings;
                }
            }
            window.reload_events();
        }
        window.reload_history(cx);
        if window.preview {
            window.selected_history = window.history_entries.first().map(|entry| entry.id);
        }
        if matches!(window.pane, Pane::Modes | Pane::History) {
            window.ensure_application_catalog_load();
        }
        if window.pane == Pane::HudLab {
            window.apply_hud_lab();
        }
        window
    }

    /// Refreshes disk-backed meeting and activity data while retaining valid
    /// selections. This is also called whenever an existing shell is reopened.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if !self.preview {
            if crate::MEETINGS_ENABLED {
                self.reload_meetings();
            }
            self.reload_events();
        }
        self.reload_history(cx);
        if matches!(self.pane, Pane::Modes | Pane::History) {
            self.ensure_application_catalog_load();
        }
        self.permission_refresh_at = Instant::now();
        self.poll_setup(true);
        let pane = self.pane.on_reopen(self.setup_status);
        if pane != self.pane {
            self.select_pane(pane, cx);
        }
        cx.notify();
    }

    fn select_pane(&mut self, pane: Pane, cx: &mut Context<Self>) {
        self.pane = pane;
        self.mode_context_menu = None;
        self.history_retention_open = false;
        self.pending_microphone_policy = None;
        match pane {
            Pane::HudLab => self.apply_hud_lab(),
            Pane::Meetings => {
                self.screen_capture_allowed = None;
                self.meeting_copied = false;
                self.meeting_retention_open = false;
                self.reload_meetings();
            }
            Pane::Commands | Pane::Activity => self.reload_events(),
            Pane::History => {
                self.reload_history(cx);
                self.ensure_application_catalog_load();
            }
            Pane::Modes => self.ensure_application_catalog_load(),
            Pane::Settings => self.permission_refresh_at = Instant::now(),
            Pane::Appearance => self.appearance_clock = Instant::now(),
            Pane::General | Pane::Sounds => {}
            Pane::Profile => self.reload_history(cx),
        }
        cx.notify();
    }

    pub(crate) fn show_settings(&mut self, cx: &mut Context<Self>) {
        self.select_pane(Pane::Settings, cx);
    }

    pub(crate) fn show_notetaker(&mut self, meeting: Option<String>, cx: &mut Context<Self>) {
        self.select_pane(Pane::Meetings, cx);
        if meeting.is_some() {
            self.selected_meeting = meeting;
            self.meeting_tab = MeetingTab::Transcript;
            self.load_selected_meeting();
        }
        cx.notify();
    }

    /// Re-aligns animated switches after settings changed outside the window.
    fn sync_menu_controls(&mut self) {
        self.multilingual_toggle
            .set_enabled(self.settings.multilingual.enabled);
        self.meeting_detect_toggle
            .set_enabled(self.settings.notetaker.detect_meetings);
        self.meeting_calendar_toggle
            .set_enabled(self.settings.notetaker.use_calendar);
        self.language_samples_toggle
            .set_enabled(self.settings.multilingual.collect_samples);
    }

    pub(crate) fn show_transcription_models(&mut self, cx: &mut Context<Self>) {
        self.show_settings(cx);
        self.transcription_picker_language = Some(self.settings.transcription.language.clone());
        self.poll_transcription_download();
        cx.notify();
    }

    pub(crate) fn developer_select_pane(&mut self, pane: DeveloperPane, cx: &mut Context<Self>) {
        self.select_pane(Pane::from_developer(pane), cx);
    }

    pub(crate) fn developer_pane(&self) -> DeveloperPane {
        self.pane.developer()
    }

    pub(crate) fn developer_set_commands_enabled(
        &mut self,
        enabled: bool,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        self.update_microphone_policy(MicrophonePolicyChange::SetCommands(enabled), cx)
            .map_err(|error| error.to_string())
    }

    fn update_microphone_policy(
        &mut self,
        change: MicrophonePolicyChange,
        cx: &mut Context<Self>,
    ) -> color_eyre::Result<()> {
        let mut candidate = self.settings_candidate();
        match change {
            MicrophonePolicyChange::SetCommands(enabled) => {
                candidate.set_commands_enabled(enabled)?
            }
            MicrophonePolicyChange::SetReleaseWhileIdle(enabled) => {
                candidate.set_release_microphone_while_idle(enabled)?
            }
            MicrophonePolicyChange::KeepReadyAndEnableCommands => {
                candidate.keep_microphone_ready_and_enable_commands()
            }
            MicrophonePolicyChange::DisableCommandsAndRelease => {
                candidate.disable_commands_and_release_microphone()
            }
        }
        let result = self.commit_settings(candidate);
        cx.notify();
        result?;
        self.commands_toggle
            .set_enabled(self.settings.commands_enabled);
        self.release_microphone_toggle
            .set_enabled(self.settings.release_microphone_while_idle);
        self.pending_microphone_policy = None;
        cx.notify();
        Ok(())
    }

    fn poll_application_catalog(&mut self) -> bool {
        let Some(receiver) = &self.application_catalog_receiver else {
            return false;
        };
        let Ok(result) = receiver.try_recv() else {
            return false;
        };
        let mut applications = result;
        if let ApplicationCatalogState::Loaded(existing) = &self.application_catalog {
            for application in existing.iter().cloned() {
                crate::application_catalog::insert(&mut applications, application);
            }
        }
        self.application_catalog = ApplicationCatalogState::Loaded(applications);
        self.application_catalog_receiver = None;
        true
    }

    fn ensure_application_catalog_load(&mut self) {
        if !application_catalog_load_needed(
            self.preview,
            &self.application_catalog,
            self.application_catalog_receiver.is_some(),
        ) {
            return;
        }
        let (sender, receiver) = sync_channel(1);
        thread::spawn(move || {
            let _ = sender.send(crate::application_catalog::discover());
        });
        self.application_catalog_receiver = Some(receiver);
    }

    fn poll_transcription_download(&mut self) -> bool {
        if self.preview {
            return false;
        }
        let status = self.transcription_preparation.status();
        let changed = self.transcription_status != status;
        self.transcription_status = status;
        changed || self.transcription_status.model.is_some()
    }

    fn poll_setup(&mut self, force: bool) -> bool {
        if self.preview
            || !force
                && (Instant::now() < self.permission_refresh_at
                    || !self.setup_visible
                        && self.recognition_start.is_none()
                        && self.pane != Pane::Settings
                        && self.setup_status.transcription_model)
        {
            return false;
        }
        self.permission_refresh_at = Instant::now() + PERMISSION_REFRESH_INTERVAL;
        let mut changed = false;
        let transcription_model = validate(&self.settings.transcription).is_ok_and(|model| {
            crate::transcription_models::is_installed(model, &self.settings.transcription.language)
        });
        let status = crate::onboarding::status_with_transcription_model(transcription_model);
        if status != self.setup_status {
            self.setup_status = status;
            changed = true;
        }
        if self.setup_status.ready() {
            if !self.onboarding_completed {
                if let Err(error) = crate::onboarding::record_completion() {
                    tracing::warn!(%error, "could not record onboarding completion");
                }
                self.onboarding_completed = true;
            }
            if let Some(start) = self.recognition_start.take() {
                let _ = start.try_send(());
            }
            if self.setup_visible {
                self.setup_visible = false;
                changed = true;
            }
        }
        changed
    }

    fn poll_login_item(&mut self) -> bool {
        let Some(worker) = &mut self.login_item_worker else {
            return false;
        };
        let response = worker.poll();
        let changed = match response {
            Ok(Some(response)) => self.apply_login_item_response(response),
            Ok(None) => false,
            Err(error) => {
                self.login_item_worker_failed(error);
                return true;
            }
        };
        let request_changed = self.request_login_item(LoginItemRequest::Status);
        changed || request_changed
    }

    fn apply_login_item_response(&mut self, response: LoginItemResponse) -> bool {
        let before = (
            self.launch_at_login_status,
            self.launch_at_login_toggle.enabled(),
        );
        let error_changed = match response.result {
            Ok(status) => {
                let clear_error = self.launch_at_login_status.is_none()
                    || matches!(response.request, LoginItemRequest::SetEnabled(_))
                    || self.launch_at_login_status != Some(status)
                        && status == LoginItemStatus::Enabled;
                self.launch_at_login_status = Some(status);
                clear_error && self.launch_at_login_error.take().is_some()
            }
            Err(error) => {
                if let Some(status) = error.status {
                    self.launch_at_login_status = Some(status);
                }
                let changed = self.launch_at_login_error.as_deref() != Some(error.message.as_str());
                self.launch_at_login_error = Some(error.message);
                changed
            }
        };
        let enabled = self
            .login_item_worker
            .as_ref()
            .and_then(LoginItemWorker::desired_enabled)
            .unwrap_or(self.launch_at_login_status == Some(LoginItemStatus::Enabled));
        self.launch_at_login_toggle.set_enabled(enabled);
        error_changed
            || before
                != (
                    self.launch_at_login_status,
                    self.launch_at_login_toggle.enabled(),
                )
    }

    fn request_login_item(&mut self, request: LoginItemRequest) -> bool {
        let Some(worker) = &mut self.login_item_worker else {
            return false;
        };
        if let Err(error) = worker.request(request) {
            self.login_item_worker_failed(error);
            return true;
        }
        false
    }

    fn login_item_worker_failed(&mut self, error: String) {
        self.login_item_worker = None;
        self.launch_at_login_error = Some(error);
        self.launch_at_login_toggle
            .set_enabled(self.launch_at_login_status == Some(LoginItemStatus::Enabled));
    }

    fn poll_personal_commands(&mut self) -> bool {
        if self.preview {
            return false;
        }
        let Ok(mut status) = crate::personal_commands::load_status() else {
            return false;
        };
        crate::personal_commands::include_builtin_transformations(&mut status);
        if status == self.personal_commands_status {
            return false;
        }
        self.personal_commands_status = status;
        self.commands = merged_catalog(&self.compiled_commands, &self.personal_commands_status);
        if self
            .selected_command
            .as_ref()
            .is_some_and(|id| !self.commands.iter().any(|command| &command.id == id))
        {
            self.selected_command = None;
        }
        true
    }

    fn poll_personal_workspace(&mut self) -> bool {
        let Some(receiver) = &self.personal_workspace_receiver else {
            return false;
        };
        let Ok(result) = receiver.try_recv() else {
            return false;
        };
        self.personal_workspace_receiver = None;
        match result {
            Ok(workspace) => {
                self.personal_workspace = workspace;
                self.personal_workspace_error = None;
            }
            Err(error) => self.personal_workspace_error = Some(error),
        }
        true
    }

    fn provision_personal_workspace(&mut self) {
        if self.preview || self.personal_workspace_receiver.is_some() {
            return;
        }
        let (sender, receiver) = sync_channel(1);
        thread::spawn(move || {
            let result =
                crate::personal_commands::initialize_workspace().map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
        self.personal_workspace_error = None;
        self.personal_workspace_receiver = Some(receiver);
    }

    fn apply_transcription_selection(
        &mut self,
        selection: TranscriptionSelection,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        validate(&selection).map_err(|error| error.to_string())?;
        let mut candidate = self.settings_candidate();
        candidate.remember_transcription(selection);
        self.commit_settings(candidate)
            .map_err(|error| error.to_string())?;
        if self.preview {
            self.transcription_preview_installed = Some(true);
            self.setup_status.transcription_model = true;
        }
        self.transcription_hints.entity.update(cx, |input, cx| {
            input.set_text(&self.settings.transcription.recognition_hints, cx);
        });
        self.permission_refresh_at = Instant::now();
        self.transcription_picker_language = None;
        self.clear_transcription_error();
        Ok(())
    }

    fn choose_transcription_model(
        &mut self,
        model: &'static ModelDefinition,
        language: String,
        cx: &mut Context<Self>,
    ) {
        self.cancel_transcription_download();
        let selection = TranscriptionSelection {
            model: model.id,
            language,
            recognition_hints: if model.supports_recognition_hints {
                self.transcription_hints.entity.read(cx).text().to_string()
            } else {
                String::new()
            },
        };
        self.clear_transcription_error();
        if self.preview {
            self.apply_transcription_selection(selection, cx)
                .expect("preview selections do not write settings");
            return;
        }
        if transcription_selection_is_active(
            &self.settings.transcription,
            model,
            &selection.language,
            self.transcription_model_installed(model, &selection.language),
        ) {
            self.transcription_picker_language = None;
            return;
        }
        self.transcription_preparation.start(selection, false);
        self.poll_transcription_download();
    }

    fn cancel_transcription_download(&mut self) {
        self.transcription_preparation.cancel();
        self.transcription_status = self.transcription_preparation.status();
    }

    fn clear_transcription_error(&mut self) {
        self.transcription_preparation.set_error(None);
        self.transcription_status = self.transcription_preparation.status();
        self.transcription_deletion = None;
        self.transcription_deletion_error = None;
    }

    /// Downloaded local models other than the dictation model can be deleted,
    /// whatever language the picker shows, unless they are being prepared.
    fn transcription_model_deletion(
        &self,
        model: &ModelDefinition,
        installed: bool,
    ) -> ModelDeletion {
        if !installed
            || !matches!(model.runtime, ModelRuntime::Gguf(_))
            || self.settings.transcription.model == model.id
            || self.transcription_status.model == Some(model.id)
        {
            ModelDeletion::Unavailable
        } else if self.transcription_deletion == Some(model.id) {
            ModelDeletion::Confirming
        } else {
            ModelDeletion::Offered
        }
    }

    fn delete_transcription_model(&mut self, model: &'static ModelDefinition) {
        self.transcription_deletion_error = None;
        let language = self
            .transcription_picker_language
            .clone()
            .unwrap_or_default();
        let installed = self.transcription_model_installed(model, &language);
        match self.transcription_model_deletion(model, installed) {
            ModelDeletion::Unavailable => self.transcription_deletion = None,
            ModelDeletion::Offered => self.transcription_deletion = Some(model.id),
            ModelDeletion::Confirming => {
                self.transcription_deletion = None;
                if self.preview {
                    return;
                }
                match crate::transcription_models::remove_installed(model) {
                    Ok(()) => tracing::info!(model = ?model.id, "deleted transcription model"),
                    Err(error) => {
                        tracing::warn!(model = ?model.id, %error, "could not delete transcription model");
                        self.transcription_deletion_error = Some(error.to_string());
                    }
                }
            }
        }
    }

    fn transcription_model_installed(&self, model: &ModelDefinition, language: &str) -> bool {
        self.transcription_preview_installed
            .unwrap_or_else(|| crate::transcription_models::is_installed(model, language))
    }

    pub fn meeting_starting(&mut self, cx: &mut Context<Self>) {
        self.meeting_runtime_error = None;
        self.meeting_refresh_started_ms = Some(now_ms());
        self.meeting_stop_pending = false;
        self.pane = Pane::Meetings;
        self.reload_meetings();
        cx.notify();
    }

    pub fn meeting_started(&mut self, cx: &mut Context<Self>) {
        self.meeting_refresh_started_ms = None;
        self.reload_meetings();
        cx.notify();
    }

    pub fn meeting_updated(&mut self, cx: &mut Context<Self>) {
        self.meeting_stop_pending = true;
        self.reload_meetings();
        cx.notify();
    }

    pub fn meeting_finished(&mut self, cx: &mut Context<Self>) {
        self.meeting_refresh_started_ms = None;
        self.meeting_stop_pending = false;
        self.reload_meetings();
        cx.notify();
    }

    pub fn meeting_failed(&mut self, title: String, error: String, cx: &mut Context<Self>) {
        self.meeting_runtime_error = Some(format!("{title}: {error}"));
        self.meeting_refresh_started_ms = None;
        self.meeting_stop_pending = false;
        self.pane = Pane::Meetings;
        self.reload_meetings();
        if let Some(failed) = self
            .meetings
            .iter()
            .find(|meeting| meeting.status == MeetingStatus::Failed && meeting.title == title)
        {
            self.selected_meeting = Some(failed.id.clone());
            self.load_selected_meeting();
        }
        cx.notify();
    }

    fn reload_meetings(&mut self) {
        match meeting::list() {
            Ok(meetings) => {
                let previous_active = meeting::active(&self.meetings).map(|meeting| &meeting.id);
                let next_active = meeting::active(&meetings).map(|meeting| meeting.id.clone());
                if next_active.as_deref() != previous_active.map(String::as_str)
                    && next_active.is_some()
                {
                    self.selected_meeting = next_active;
                }
                if meeting::active(&meetings).is_none()
                    && (previous_active.is_some()
                        || self.meeting_refresh_started_ms.is_some_and(|started| {
                            meetings.iter().any(|meeting| {
                                meeting.created_at_ms >= started
                                    && matches!(
                                        meeting.status,
                                        MeetingStatus::Complete
                                            | MeetingStatus::Interrupted
                                            | MeetingStatus::Failed
                                    )
                            })
                        }))
                {
                    self.meeting_refresh_started_ms = None;
                }
                self.meetings = meetings;
                if meeting::active(&self.meetings)
                    .is_none_or(|meeting| meeting.status == MeetingStatus::Transcribing)
                {
                    self.meeting_stop_pending = false;
                }
                self.meetings_error = None;
                if self
                    .selected_meeting
                    .as_ref()
                    .is_some_and(|selected| !self.meetings.iter().any(|item| &item.id == selected))
                {
                    self.selected_meeting = None;
                    self.meeting_transcript = MeetingTranscript::Empty;
                } else if self.selected_meeting.is_some() {
                    self.load_selected_meeting();
                }
            }
            Err(error) => {
                tracing::error!(%error, "could not list meetings");
                self.meetings_error = Some(error.to_string());
            }
        }
    }

    fn load_selected_meeting(&mut self) {
        let Some(id) = self.selected_meeting.clone() else {
            self.meeting_transcript = MeetingTranscript::Empty;
            self.meeting_transcript_id = None;
            return;
        };
        let selection_changed = self.meeting_transcript_id.as_deref() != Some(&id);
        if selection_changed {
            self.meeting_transcript_id = Some(id.clone());
            self.meeting_transcript_scroll = ScrollHandle::new();
        }
        let previous_lines = match &self.meeting_transcript {
            MeetingTranscript::Loaded(lines) if !selection_changed => Some(lines.clone()),
            MeetingTranscript::Empty
            | MeetingTranscript::Loaded(_)
            | MeetingTranscript::Unavailable(_) => None,
        };
        let was_following = previous_lines.as_ref().is_none_or(|lines| {
            lines.is_empty()
                || self.meeting_transcript_scroll.max_offset().height
                    + self.meeting_transcript_scroll.offset().y
                    <= px(48.0)
        });
        let selected = self.meetings.iter().find(|meeting| meeting.id == id);
        let speakers = selected.and_then(|meeting| meeting.speakers.clone());
        let you = selected
            .and_then(|meeting| meeting.details.as_ref())
            .and_then(|details| details.you.clone())
            .or_else(|| Some(profile_display_name(&self.settings)));
        self.meeting_transcript = match self.meeting_transcript_reader.read(&id) {
            Ok(entries) => MeetingTranscript::Loaded({
                self.meeting_you_talk_ms = entries
                    .iter()
                    .filter(|entry| entry.source == meeting::MeetingSource::Microphone)
                    .map(|entry| entry.end_ms.saturating_sub(entry.start_ms))
                    .sum();
                meeting::coalesce_transcript(entries)
                    .into_iter()
                    .scan(None, |previous_end, entry| {
                        let pause_before_ms = previous_end
                            .map(|end| entry.start_ms.saturating_sub(end))
                            .filter(|pause| *pause >= 8_000);
                        *previous_end =
                            Some(previous_end.map_or(entry.end_ms, |end| end.max(entry.end_ms)));
                        Some(TranscriptLine {
                            speaker: entry.source,
                            speaker_id: entry.speaker,
                            name: meeting::speaker_name(&entry, speakers.as_ref(), you.as_deref()),
                            at: meeting::format_duration(entry.start_ms),
                            text: entry.text,
                            pause_before_ms,
                        })
                    })
                    .collect()
            }),
            Err(error) => MeetingTranscript::Unavailable(error.to_string()),
        };
        if !selection_changed
            && matches!(
                &self.meeting_transcript,
                MeetingTranscript::Loaded(lines) if was_following && previous_lines.as_ref() != Some(lines)
            )
        {
            self.meeting_transcript_scroll.scroll_to_bottom();
        }
    }

    fn reload_events(&mut self) {
        if !matches!(self.pane, Pane::Commands | Pane::Activity) {
            return;
        }
        let selected = self
            .selected_event
            .and_then(|index| self.events.get(index))
            .cloned();
        self.activity.refresh(&mut self.event_reader);
        if self.activity.error.is_some() {
            return;
        }
        self.events = self.event_reader.recent(ACTIVITY_LIMIT);
        self.current_context = activity_context(&self.event_reader);
        self.selected_event =
            selected.and_then(|selected| self.events.iter().position(|event| event == &selected));
    }

    fn history_search_input(cx: &mut Context<Self>) -> ProcessingInput {
        let entity = cx.new(|cx| TextInput::picker(cx, "Search history", ""));
        let changed = cx.subscribe(&entity, |this, _, _: &TextChanged, cx| {
            this.reload_history(cx);
            cx.notify();
        });
        ProcessingInput {
            entity,
            _subscriptions: vec![changed],
        }
    }

    /// Refresh the bounded history snapshot for the current search query.
    fn reload_history(&mut self, cx: &App) {
        let Some(history) = &self.history else {
            self.history_entries.clear();
            self.history_insights = HistoryInsights::default();
            self.selected_history = None;
            return;
        };
        let query = self.history_search.entity.read(cx).text().to_string();
        self.history_entries = history.search(&query);
        self.history_insights = history.insights();
        if self
            .selected_history
            .is_some_and(|id| !self.history_entries.iter().any(|entry| entry.id == id))
        {
            self.selected_history = None;
        }
    }

    /// Refresh the visible history while the pane is open so entries recorded
    /// by the recognition side appear without reopening the pane.
    fn poll_history(&mut self, cx: &App) -> bool {
        if self.preview || self.pane != Pane::History || self.history.is_none() {
            return false;
        }
        let previous = std::mem::take(&mut self.history_entries);
        self.reload_history(cx);
        self.history_entries != previous
    }

    fn set_history_retention(&mut self, retention: HistoryRetention, cx: &mut Context<Self>) {
        if self.settings.history_retention == retention {
            return;
        }
        if !self.update_settings(cx, |settings| settings.history_retention = retention) {
            return;
        }
        if let Some(history) = &self.history
            && let Err(error) = history.set_retention(retention)
        {
            self.history_error = Some(error.to_string());
        }
        self.reload_history(cx);
        cx.notify();
    }

    fn copy_history_entry(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(entry) = self.history_entries.iter().find(|entry| entry.id == id) else {
            return;
        };
        match arboard::Clipboard::new()
            .and_then(|mut clipboard| clipboard.set_text(entry.final_text.clone()))
        {
            Ok(()) => {
                self.history_copied = Some(id);
                self.history_error = None;
            }
            Err(error) => self.history_error = Some(error.to_string()),
        }
        cx.notify();
    }

    fn delete_history_entry(&mut self, id: u64, cx: &mut Context<Self>) {
        if let Some(history) = &self.history {
            if let Err(error) = history.delete(id) {
                self.history_error = Some(error.to_string());
            }
            self.reload_history(cx);
        }
        cx.notify();
    }

    fn clear_history(&mut self, cx: &mut Context<Self>) {
        if !self.history_clear_armed {
            self.history_clear_armed = true;
            cx.notify();
            return;
        }
        self.history_clear_armed = false;
        if let Some(history) = &self.history {
            if let Err(error) = history.clear() {
                self.history_error = Some(error.to_string());
            }
            self.reload_history(cx);
        }
        cx.notify();
    }

    fn render_history(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let retention = self.settings.history_retention;
        let search = div().w(px(220.0)).child(self.history_search.entity.clone());
        let retention_control = glyph_button(Glyph::Clock, format!("Keep {}", retention.label()))
            .id("history-retention")
            .on_click(cx.listener(|this, _, _, cx| {
                this.history_retention_open = true;
                cx.notify();
            }));
        let retention_control = div().relative().child(retention_control).when(
            self.history_retention_open,
            |control| {
                control.child(deferred(
                    div()
                        .id("history-retention-menu")
                        .absolute()
                        .top(px(36.0))
                        .right_0()
                        .w(px(160.0))
                        .p_1()
                        .rounded(px(12.0))
                        .border_1()
                        .border_color(rgba(0xffffff1f))
                        .bg(rgb(POPOVER))
                        .shadow_lg()
                        .occlude()
                        .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                            this.history_retention_open = false;
                            cx.notify();
                        }))
                        .children(HistoryRetention::ALL.into_iter().enumerate().map(
                            |(index, choice)| {
                                compact_button(choice.label())
                                    .id(("history-retention-choice", index))
                                    .w_full()
                                    .when(choice == retention, |item| {
                                        item.bg(tone(SURFACE_SELECTED))
                                    })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.history_retention_open = false;
                                        this.set_history_retention(choice, cx);
                                        cx.notify();
                                    }))
                            },
                        )),
                ))
            },
        );
        let clear = glyph_button(
            Glyph::Trash,
            if self.history_clear_armed {
                "Really clear all?"
            } else {
                "Clear all"
            },
        )
        .id("history-clear")
        .when(self.history_clear_armed, |button| {
            button
                .border_color(rgb(DANGER_BORDER))
                .bg(rgb(DANGER_SURFACE))
                .text_color(rgb(NEGATIVE))
        })
        .on_click(cx.listener(|this, _, _, cx| this.clear_history(cx)))
        .into_any_element();
        let header_action = div()
            .flex()
            .items_center()
            .gap_2()
            .child(search)
            .child(retention_control)
            .child(clear)
            .into_any_element();
        let effective_selected = self.effective_history_selection();
        let mut rows: Vec<AnyElement> = Vec::new();
        let mut current_day: Option<String> = None;
        for (index, entry) in self.history_entries.iter().enumerate() {
            let day = history_day_label(entry.timestamp_ms);
            if current_day.as_deref() != Some(day.as_str()) {
                let count = self
                    .history_entries
                    .iter()
                    .filter(|other| history_day_label(other.timestamp_ms) == day)
                    .count();
                rows.push(group_heading(day.clone(), Some(count)).into_any_element());
                current_day = Some(day);
            }
            let id = entry.id;
            let selected = effective_selected == Some(id);
            let preview = entry.final_text.replace('\n', " ");
            let application = entry.application.clone();
            let app_icon = application.as_deref().map(|name| {
                application_icon(
                    self.application_catalog.application_named(name),
                    Some(name),
                    14.0,
                )
            });
            let (kind_glyph, kind_tint) = history_kind_style(entry.kind);
            let mut meta = clock_time(entry.timestamp_ms);
            if let Some(application) = &application {
                meta.push_str(" · ");
                meta.push_str(application);
            }
            rows.push(
                list_row(selected)
                    .id(("history-entry", index))
                    .flex_row()
                    .items_start()
                    .gap(px(10.0))
                    .py(px(10.0))
                    .child(
                        div()
                            .pt(px(1.0))
                            .child(glyph_badge(kind_glyph, 26.0, kind_tint)),
                    )
                    .child(
                        div()
                            .w(px(HISTORY_ROW_TEXT_WIDTH))
                            .flex()
                            .flex_col()
                            .gap(px(5.0))
                            .child(
                                div()
                                    .w(px(HISTORY_ROW_TEXT_WIDTH))
                                    .text_size(px(12.5))
                                    .line_height(px(17.0))
                                    .text_color(rgb(if selected { TEXT } else { TEXT_SOFT }))
                                    .line_clamp(2)
                                    .text_ellipsis()
                                    .child(preview),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(5.0))
                                    .when_some(app_icon, |meta_row, icon| meta_row.child(icon))
                                    .child(
                                        div()
                                            .text_size(px(10.5))
                                            .text_color(rgb(if selected { MUTED } else { FAINT }))
                                            .child(meta),
                                    ),
                            ),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected_history = Some(id);
                        this.history_clear_armed = false;
                        cx.notify();
                    }))
                    .into_any_element(),
            );
        }
        let retention_off = retention.is_off();
        let insights = render_history_insights(&self.history_insights);
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(pane_header_with_subtitle(
                "History",
                "Everything you dictated, kept privately on this Mac.",
                Some(header_action),
            ))
            .child(
                pane_body().px_5().pb_5().pt_1().child(
                    pane_content().h_full().gap_4().child(insights).child(
                        div()
                            .flex_1()
                            .min_h(px(0.0))
                            .flex()
                            .flex_row()
                            .gap_4()
                            .child(pane_list_card(
                                "history-list",
                                if retention_off {
                                    Some("History is off. New dictations are not retained.")
                                } else if self.history_entries.is_empty()
                                    && self.history_error.is_none()
                                {
                                    Some("No dictations retained yet.")
                                } else {
                                    None
                                },
                                "History could not be loaded.",
                                self.history_error.clone(),
                                rows,
                            ))
                            .child(
                                compact_panel()
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .h_full()
                                    .flex()
                                    .flex_col()
                                    .child(self.render_history_detail(cx)),
                            ),
                    ),
                ),
            )
            .into_any_element()
    }

    /// The entry the detail shows: the chosen one, else the newest.
    fn effective_history_selection(&self) -> Option<u64> {
        self.selected_history
            .filter(|id| self.history_entries.iter().any(|entry| entry.id == *id))
            .or_else(|| self.history_entries.first().map(|entry| entry.id))
    }

    fn render_history_detail(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(entry) = self
            .effective_history_selection()
            .and_then(|id| self.history_entries.iter().find(|entry| entry.id == id))
        else {
            return detail_placeholder("Your dictations will appear here.");
        };
        let id = entry.id;
        let copied = self.history_copied == Some(id);
        let show_raw = entry.raw_text.trim() != entry.final_text.trim();
        let (kind_glyph, kind_tint) = history_kind_style(entry.kind);
        let words = entry.final_text.split_whitespace().count();
        let when = format!(
            "{} at {}",
            history_day_label(entry.timestamp_ms),
            clock_time(entry.timestamp_ms)
        );
        let app_chip = entry.application.clone().map(|name| {
            icon_chip(
                application_icon(
                    self.application_catalog.application_named(&name),
                    Some(&name),
                    15.0,
                ),
                name.clone(),
            )
        });
        let mut stats: Vec<(Glyph, String)> = vec![(
            Glyph::Text,
            format!("{words} word{}", if words == 1 { "" } else { "s" }),
        )];
        if entry.audio_ms > 0 {
            stats.push((Glyph::Mic, format_duration_ms(entry.audio_ms)));
        }
        if entry.inference_ms > 0 {
            stats.push((
                Glyph::Bolt,
                format!("Transcribed in {}", format_duration_ms(entry.inference_ms)),
            ));
        }
        if let Some(processing) = &entry.processing {
            let mut summary = format!(
                "{} · {}",
                processing.profile,
                format_duration_ms(processing.latency_ms)
            );
            if let Some(fallback) = &processing.fallback {
                summary.push_str(&format!(" · fell back: {fallback}"));
            }
            stats.push((Glyph::Wand, summary));
        }
        let detail_button = |icon: Glyph, label: &'static str, id_suffix: &'static str| {
            glyph_button(icon, label).id(SharedString::from(format!("history-action-{id_suffix}")))
        };
        let text_card = |label: &'static str, text: String, primary: bool| {
            div()
                .w_full()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .text_size(px(11.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(MUTED))
                        .child(label),
                )
                .child(
                    div()
                        .w_full()
                        .px_4()
                        .py(px(14.0))
                        .rounded(px(12.0))
                        .border_1()
                        .border_color(rgb(0x303037))
                        .bg(tone(CANVAS))
                        .text_size(px(if primary { 14.0 } else { 12.5 }))
                        .line_height(px(if primary { 22.0 } else { 19.0 }))
                        .text_color(rgb(if primary { TEXT } else { MUTED }))
                        .child(text),
                )
        };
        div()
            .id("history-detail")
            .flex_1()
            .min_w(px(0.0))
            .h_full()
            .overflow_y_scroll()
            .px_6()
            .py_5()
            .child(
                div()
                    .w_full()
                    .flex()
                    .flex_col()
                    .gap_5()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .child(glyph_badge(kind_glyph, 38.0, kind_tint))
                                    .child(
                                        div()
                                            .flex()
                                            .flex_col()
                                            .gap(px(2.0))
                                            .child(
                                                div()
                                                    .text_size(px(16.0))
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .child(entry.kind.label()),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(11.5))
                                                    .text_color(rgb(MUTED))
                                                    .child(when),
                                            ),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        detail_button(
                                            Glyph::Copy,
                                            if copied { "Copied" } else { "Copy" },
                                            "copy",
                                        )
                                        .when(copied, |button| {
                                            button
                                                .border_color(rgb(0x2a5a3a))
                                                .text_color(rgb(POSITIVE))
                                        })
                                        .on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                this.copy_history_entry(id, cx)
                                            }),
                                        ),
                                    )
                                    .child(
                                        detail_button(Glyph::Trash, "Delete", "delete")
                                            .hover(|button| {
                                                button
                                                    .bg(rgb(DANGER_SURFACE))
                                                    .text_color(rgb(NEGATIVE))
                                            })
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.delete_history_entry(id, cx)
                                            })),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_2()
                            .when_some(app_chip, |chips, chip| chips.child(chip))
                            .children(stats.into_iter().map(|(icon, label)| {
                                icon_chip(glyph(icon, 12.0, 0x98989d), label)
                            })),
                    )
                    .child(text_card("Final text", entry.final_text.clone(), true))
                    .when(show_raw, |detail| {
                        detail.child(text_card("What you said", entry.raw_text.clone(), false))
                    }),
            )
            .into_any_element()
    }

    fn render_navigation(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let mut groups: Vec<(&'static str, Vec<AnyElement>)> = Vec::new();
        for (index, pane) in Pane::all(crate::DEVELOPER_FEATURES_ENABLED)
            .into_iter()
            .enumerate()
        {
            // The profile has its own card at the bottom of the sidebar.
            if pane == Pane::Profile {
                continue;
            }
            let selected = self.pane == pane;
            let item = navigation_item(pane.icon(), selected)
                .id(("app-nav", index))
                .child(pane.label())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.select_pane(pane, cx);
                }))
                .into_any_element();
            match groups.last_mut() {
                Some((group, items)) if *group == pane.group() => items.push(item),
                _ => groups.push((pane.group(), vec![item])),
            }
        }
        let name = profile_display_name(&self.settings);
        let profile_selected = self.pane == Pane::Profile;
        let profile_card = div()
            .id("sidebar-profile")
            .flex_none()
            .mt_3()
            .p(px(8.0))
            .flex()
            .items_center()
            .gap_2p5()
            .rounded(px(8.0))
            .border_1()
            .border_color(if profile_selected {
                rgba(0xffffff2e)
            } else {
                tone(LINE)
            })
            .bg(tone(if profile_selected {
                SURFACE_SELECTED
            } else {
                SURFACE
            }))
            .when(!profile_selected, |card| {
                card.hover(|card| card.bg(tone(SURFACE_HOVER)))
            })
            .child(profile_avatar(&name, 30.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .truncate()
                            .text_size(px(13.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(TEXT))
                            .child(name),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(rgba(crate::desktop_ui::SIDEBAR_SECONDARY))
                            .child("View profile"),
                    ),
            )
            .on_click(cx.listener(|this, _, _, cx| this.select_pane(Pane::Profile, cx)));

        sidebar_frame()
            .w(px(SIDEBAR_WIDTH))
            .px(px(10.0))
            .pt(px(44.0))
            .pb_4()
            .flex()
            .flex_col()
            .children(groups.into_iter().map(|(group, items)| {
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.0))
                    .child(
                        div()
                            .px_3()
                            .pt(px(14.0))
                            .pb(px(4.0))
                            .text_size(px(11.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgba(crate::desktop_ui::SIDEBAR_SECONDARY))
                            .child(group),
                    )
                    .children(items)
            }))
            .child(div().flex_1())
            // The version lives on the Profile page; the sidebar only flags an update.
            .children(
                sidebar_update_button(self.update_status, self.preview)
                    .map(|button| button.mt_3().justify_center()),
            )
            .child(profile_card)
            .into_any_element()
    }

    fn render_appearance(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        // The style previews are live miniatures, so keep frames coming while visible.
        window.request_animation_frame();
        let time = self.appearance_clock.elapsed().as_secs_f32();
        let position = IndicatorPosition::resolve(
            self.settings.indicator_position,
            self.settings.indicator_style,
        );
        let position_spring = self.indicator_position_spring.render_position(window);
        let position_widths = [64.0, 76.0];
        let position_control = sliding_segmented_control(position_spring, &position_widths)
            .children(
                IndicatorPosition::ALL
                    .into_iter()
                    .enumerate()
                    .map(|(index, option)| {
                        sliding_segmented_item(position_widths[index], position == option)
                            .id(("indicator-position", index))
                            .child(option.label())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.set_indicator_position(option, cx)
                            }))
                    }),
            );
        let theme = self.settings.theme;
        let theme_spring = self.theme_spring.render_position(window);
        let theme_widths = [64.0, 64.0, 64.0];
        let theme_control = sliding_segmented_control(theme_spring, &theme_widths).children(
            ColorTheme::ALL
                .into_iter()
                .enumerate()
                .map(|(index, option)| {
                    sliding_segmented_item(theme_widths[index], theme == option)
                        .id(("color-theme", index))
                        .child(option.label())
                        .on_click(cx.listener(move |this, _, _, cx| this.set_theme(option, cx)))
                }),
        );
        let cards = IndicatorStyle::ALL
            .into_iter()
            .enumerate()
            .map(|(index, style)| {
                let selected = self.settings.indicator_style == style;
                div()
                    .id(("indicator-style-card", index))
                    .flex_1()
                    .min_w(px(0.0))
                    .flex()
                    .flex_col()
                    .rounded(px(PANEL_RADIUS))
                    .border_1()
                    .border_color(if selected {
                        rgb(ACCENT_SOFT)
                    } else {
                        tone(LINE)
                    })
                    .bg(tone(if selected { ACCENT_SURFACE } else { SURFACE }))
                    .overflow_hidden()
                    .when(!selected, |card| {
                        card.hover(|card| card.border_color(rgb(0x3a3a3a)))
                    })
                    .child(
                        div()
                            .h(px(92.0))
                            .flex()
                            .items_center()
                            .justify_center()
                            .bg(if selected {
                                linear_gradient(
                                    180.0,
                                    linear_color_stop(rgb(0x1b2c47), 0.0),
                                    linear_color_stop(rgb(0x0e1522), 1.0),
                                )
                            } else {
                                linear_gradient(
                                    180.0,
                                    linear_color_stop(rgb(0x1d1d21), 0.0),
                                    linear_color_stop(rgb(0x111113), 1.0),
                                )
                            })
                            .border_b_1()
                            .border_color(if selected {
                                rgba(0x64b0ff40)
                            } else {
                                tone(LINE)
                            })
                            .child(indicator_preview(style, time)),
                    )
                    .child(
                        div()
                            .px_3()
                            .py_2p5()
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .child(
                                        div()
                                            .text_size(px(13.0))
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .child(style.label()),
                                    )
                                    .when(selected, |row| {
                                        row.child(
                                            div().size(px(8.0)).rounded_full().bg(rgb(ACCENT_SOFT)),
                                        )
                                    }),
                            )
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(rgb(MUTED))
                                    .child(style.description()),
                            ),
                    )
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.set_indicator_style(style, cx)),
                    )
            });
        settings_pane_titled(
            "Appearance",
            "appearance-scroll",
            div()
                .child(
                    settings_panel().child(
                        settings_row(
                            "Theme",
                            match theme {
                                ColorTheme::Auto => "Matches your Mac's light or dark setting",
                                ColorTheme::Light => "Light windows",
                                ColorTheme::Dark => "Dark windows",
                            },
                            theme_control,
                        )
                        .border_b_0()
                        .id("color-theme-setting"),
                    ),
                )
                .child(settings_section_label("RECORDING INDICATOR"))
                .child(div().flex().flex_row().gap_3().children(cards))
                .child(
                    div().mt_3().child(
                        settings_panel().child(
                            settings_row(
                                "Position",
                                match position {
                                    IndicatorPosition::Top => "Near the top of the screen, by the menu bar",
                                    IndicatorPosition::Bottom => "Just above the Dock",
                                },
                                position_control,
                            )
                            .border_b_0()
                            .id("indicator-position-setting"),
                        ),
                    ),
                )
                .child(
                    div()
                        .pt_3()
                        .px_1()
                        .text_size(px(11.0))
                        .text_color(rgb(FAINT))
                        .child("The indicator appears on whichever display your pointer is on. You can also change it from the Dictation menu."),
                ),
        )
    }

    fn render_profile(&mut self) -> AnyElement {
        let name = profile_display_name(&self.settings);
        let insights = &self.history_insights;
        let summary = if insights.dictations == 0 {
            "Your first dictation will show up here".to_string()
        } else {
            format!(
                "{} dictations · {} words",
                format_count(insights.dictations),
                format_count(insights.words)
            )
        };
        let today = crate::history::local_day(now_ms());
        let streak = insights.streak_days(today);
        let profile_since = match insights.first_at_ms {
            Some(first) => format!(
                "Dictating since {} · {} active {} · everything stays on this Mac",
                profile_date(first),
                insights.daily_words.len(),
                if insights.daily_words.len() == 1 {
                    "day"
                } else {
                    "days"
                }
            ),
            None => "Local profile · everything stays on this Mac".to_string(),
        };
        let hero = compact_panel()
            .p_5()
            .flex()
            .items_center()
            .gap_4()
            .bg(linear_gradient(
                110.0,
                linear_color_stop(tone(ACCENT_SURFACE), 0.0),
                linear_color_stop(tone(SURFACE), 1.0),
            ))
            .child(profile_avatar(&name, 64.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .truncate()
                            .text_size(px(22.0))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(rgb(TEXT))
                            .child(name),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(rgb(TEXT_SOFT))
                            .child(summary),
                    )
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(rgb(MUTED))
                            .child(profile_since),
                    ),
            )
            .when(streak > 0, |hero| {
                hero.child(
                    div()
                        .flex_none()
                        .flex()
                        .flex_col()
                        .items_end()
                        .gap(px(2.0))
                        .child(
                            div()
                                .text_size(px(26.0))
                                .font_weight(FontWeight::BOLD)
                                .text_color(rgb(TEXT))
                                .child(streak.to_string()),
                        )
                        .child(div().text_size(px(11.0)).text_color(rgb(MUTED)).child(
                            if streak == 1 {
                                "day streak"
                            } else {
                                "day streak"
                            },
                        )),
                )
            });
        let meeting_count = self.meetings.len();
        let meeting_ms: u64 = self.meetings.iter().map(meeting_duration).sum();
        let voices = crate::speakers::load_voices().len();
        let (languages, languages_label) = if self.settings.multilingual.enabled {
            ("EN + BN".to_string(), "English and Bangla")
        } else {
            (
                language_name(&self.settings.transcription.language).to_string(),
                "Dictation language",
            )
        };
        settings_pane_titled(
            "Profile",
            "profile-scroll",
            div()
                .pt_2()
                .child(hero)
                .child(settings_section_label("DETAILS"))
                .child(
                    settings_panel().child(
                        settings_row(
                            "Display name",
                            "Shown in the sidebar. Leave empty to use your Mac account name",
                            div()
                                .w(px(260.0))
                                .flex_none()
                                .child(self.profile_name.entity.clone()),
                        )
                        .border_b_0(),
                    ),
                )
                .child(settings_section_label("ABOUT HELLO"))
                .child(
                    settings_panel().child(
                        settings_row(
                            "Version",
                            format!("Hello {APP_VERSION}. Checks for updates once a day"),
                            div()
                                .flex_none()
                                .children(about_update_button(self.update_status, self.preview)),
                        )
                        .border_b_0(),
                    ),
                )
                .child(settings_section_label("YOUR DICTATION"))
                .child(render_history_insights(insights))
                .child(div().h(px(12.0)))
                .child(render_profile_tiles(&[
                    (
                        Glyph::Bolt,
                        TINT_PINK,
                        "Today",
                        format!("{} words", format_count(insights.words_on(today))),
                    ),
                    (
                        Glyph::Layers,
                        TINT_TEAL,
                        "Last 7 days",
                        format!(
                            "{} words",
                            format_count((0..7).map(|back| insights.words_on(today - back)).sum())
                        ),
                    ),
                    (
                        Glyph::Text,
                        TINT_INDIGO,
                        "Per dictation",
                        format!(
                            "{} words",
                            format_count(insights.words / insights.dictations.max(1))
                        ),
                    ),
                    (
                        Glyph::Pencil,
                        TINT_ORANGE,
                        "Longest",
                        format!("{} words", format_count(insights.longest_words)),
                    ),
                ]))
                .child(settings_section_label("LAST 14 DAYS"))
                .child(render_daily_words(insights, today))
                .child(settings_section_label("WHERE YOU DICTATE"))
                .child(render_top_apps(insights))
                .child(settings_section_label("MEETINGS & LANGUAGES"))
                .child(render_profile_tiles(&[
                    (
                        Glyph::People,
                        TINT_PURPLE,
                        "Meeting notes",
                        format_count(meeting_count),
                    ),
                    (
                        Glyph::Clock,
                        TINT_GREEN,
                        "In meetings",
                        meeting_length_label(meeting_ms),
                    ),
                    (Glyph::Mic, TINT_BLUE, "Voices known", format_count(voices)),
                    (Glyph::Globe, TINT_ORANGE, languages_label, languages),
                ]))
                .child(settings_section_label("MILESTONES"))
                .child(render_milestones(insights.words)),
        )
    }

    fn render_hotkey_control(
        &mut self,
        kind: HotkeyKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let binding_keycaps = hotkey_binding(&self.settings, kind)
            .map_or_else(|| vec!["Off".into()], HotkeyBinding::keycaps);
        let idle_width = hotkey_idle_width(binding_keycaps.len());
        if matches!(
            self.hotkey_capture,
            HotkeyCaptureState::Saved { saved_at, .. }
                if saved_at.elapsed() >= Duration::from_millis(700)
        ) {
            self.hotkey_capture = HotkeyCaptureState::Idle;
            self.hotkey_capture_animation.set_enabled(false);
            self.hotkey_width_spring.set_target(idle_width);
        } else if matches!(self.hotkey_capture, HotkeyCaptureState::Idle) {
            self.hotkey_width_spring.set_target(idle_width);
        }
        let intensity = self.hotkey_capture_animation.render_position(window);
        let animated_control_width = self.hotkey_width_spring.render_position(window);
        let this_capture = matches!(
            self.hotkey_capture,
            HotkeyCaptureState::Listening { kind: active, .. }
                | HotkeyCaptureState::Saved { kind: active, .. }
                if active == kind
        );
        let control_width = hotkey_control_width(this_capture, idle_width, animated_control_width);
        let capture_active = !matches!(self.hotkey_capture, HotkeyCaptureState::Idle);
        let control_intensity = hotkey_control_intensity(this_capture, intensity);
        let capture_color = if matches!(
            self.hotkey_capture,
            HotkeyCaptureState::Saved { kind: active, .. } if active == kind
        ) {
            rgb(POSITIVE_SURFACE)
        } else {
            rgb(DANGER_SURFACE)
        };
        let pulse = match &self.hotkey_capture {
            HotkeyCaptureState::Listening {
                kind: active,
                started_at,
                ..
            } if *active == kind => {
                window.request_animation_frame();
                (started_at.elapsed().as_secs_f32() * 4.5).sin() * 0.5 + 0.5
            }
            HotkeyCaptureState::Saved { kind: active, .. } if *active == kind => {
                window.request_animation_frame();
                1.0
            }
            HotkeyCaptureState::Idle
            | HotkeyCaptureState::Listening { .. }
            | HotkeyCaptureState::Saved { .. } => 0.0,
        };
        let content = match &self.hotkey_capture {
            HotkeyCaptureState::Listening {
                kind: active,
                modifiers,
                message,
                ..
            } if *active == kind => {
                let label = message.unwrap_or(if modifiers.is_empty() {
                    "Press shortcut"
                } else {
                    "Release to save or add key"
                });
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .size(px(5.0 + pulse * 3.0))
                            .ml_2()
                            .rounded_full()
                            .bg(rgb(NEGATIVE))
                            .opacity(0.6 + pulse * 0.4),
                    )
                    .when(!modifiers.is_empty() && message.is_none(), |content| {
                        content.child(hotkey_keycaps(
                            HotkeyBinding {
                                modifiers: *modifiers,
                                key: None,
                            }
                            .keycaps(),
                            0.85,
                        ))
                    })
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(rgb(TEXT_SOFT))
                            .child(label),
                    )
                    .child(
                        div()
                            .id(match kind {
                                HotkeyKind::Dictation => "cancel-dictation-hotkey-capture",
                                HotkeyKind::PasteLast => "cancel-paste-last-hotkey-capture",
                            })
                            .h(px(26.0))
                            .ml_1()
                            .px_2()
                            .flex()
                            .items_center()
                            .rounded(px(4.0))
                            .text_size(px(11.0))
                            .text_color(rgb(MUTED))
                            .hover(|button| {
                                button.bg(tone(SURFACE_HOVER)).text_color(rgb(TEXT_SOFT))
                            })
                            .child("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| {
                                cx.stop_propagation();
                                this.cancel_hotkey_capture(cx);
                            })),
                    )
                    .into_any_element()
            }
            HotkeyCaptureState::Saved { kind: active, .. } if *active == kind => div()
                .flex()
                .items_center()
                .gap_2()
                .child(
                    div()
                        .text_size(px(10.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(rgb(TEXT_SOFT))
                        .child("Saved"),
                )
                .child(hotkey_keycaps(binding_keycaps.clone(), 1.0))
                .into_any_element(),
            HotkeyCaptureState::Idle
            | HotkeyCaptureState::Listening { .. }
            | HotkeyCaptureState::Saved { .. } => div()
                .flex()
                .items_center()
                .gap_2()
                .child(hotkey_keycaps(binding_keycaps, 1.0))
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(rgb(TEXT_SOFT))
                        .child("Change shortcut"),
                )
                .into_any_element(),
        };
        div()
            .id(match kind {
                HotkeyKind::Dictation => "dictation-hotkey-control",
                HotkeyKind::PasteLast => "paste-last-hotkey-control",
            })
            .track_focus(&self.hotkey_focus)
            .w(px(control_width))
            .min_w(px(HOTKEY_MIN_WIDTH))
            .h(px(32.0))
            .px(px(4.0))
            .flex()
            .items_center()
            .overflow_hidden()
            .rounded(px(6.0))
            .border_1()
            .border_color(mix_color(
                tone(LINE),
                rgb(TEXT_SOFT),
                control_intensity * 0.55,
            ))
            .bg(mix_color(
                tone(CANVAS),
                capture_color,
                control_intensity * (0.55 + pulse * 0.15),
            ))
            .when(!capture_active, |control| {
                control.hover(|control| control.bg(tone(SURFACE_HOVER)))
            })
            .child(content)
            .on_click(cx.listener(move |this, _, window, cx| {
                if this.hotkey_capture.is_listening() && this_capture {
                    this.cancel_hotkey_capture(cx);
                } else {
                    this.begin_hotkey_capture(kind, window, cx);
                }
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                this.capture_hotkey_key(event, cx);
            }))
            .on_modifiers_changed(cx.listener(|this, event: &ModifiersChangedEvent, _, cx| {
                this.capture_hotkey_modifiers(event, cx);
            }))
            .into_any_element()
    }

    fn render_hotkey_setting_control(
        &mut self,
        kind: HotkeyKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let hotkey = self.render_hotkey_control(kind, window, cx);
        let side = hotkey_binding(&self.settings, kind).and_then(standalone_modifier_side);
        let side_animation = &mut self.hotkey_side_animations[hotkey_kind_index(kind)];
        side_animation.set_enabled(side.is_some());
        let side_position = side_animation.render_position(window).clamp(0.0, 1.0);
        let selected = side.unwrap_or(ModifierSide::Either);
        let side_widths = [34.0, 44.0, 34.0];
        let side_selection_spring =
            &mut self.hotkey_side_selection_springs[hotkey_kind_index(kind)];
        side_selection_spring.set_target(hotkey_side_index(selected) as f32);
        let selection_position = side_selection_spring.render_position(window);
        let side_selector = div()
            .w(px(HOTKEY_SIDE_SELECTOR_WIDTH * side_position))
            .mr(px(8.0 * side_position))
            .flex_none()
            .overflow_hidden()
            .opacity(side_position)
            .child(
                sliding_segmented_control(selection_position, &side_widths)
                    .w(px(HOTKEY_SIDE_SELECTOR_WIDTH))
                    .children(
                        [
                            ("Left", ModifierSide::Left),
                            ("Either", ModifierSide::Either),
                            ("Right", ModifierSide::Right),
                        ]
                        .into_iter()
                        .enumerate()
                        .map(|(index, (label, side))| {
                            let candidate = hotkey_side_binding(&self.settings, kind, side);
                            sliding_segmented_item(side_widths[index], selected == side)
                                .id(("hotkey-side", hotkey_kind_index(kind) * 3 + index))
                                .text_size(px(10.0))
                                .when(candidate.is_none(), |item| item.opacity(0.35))
                                .child(label)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let Some(candidate) =
                                        hotkey_side_binding(&this.settings, kind, side)
                                    else {
                                        return;
                                    };
                                    if !this.update_settings(cx, |settings| {
                                        set_hotkey_binding(settings, kind, candidate);
                                    }) {
                                        return;
                                    }
                                    this.hotkey_side_selection_springs[hotkey_kind_index(kind)]
                                        .set_target(index as f32);
                                }))
                        }),
                    ),
            );
        div()
            .flex()
            .items_center()
            .child(side_selector)
            .child(hotkey)
            .into_any_element()
    }

    fn begin_hotkey_capture(
        &mut self,
        kind: HotkeyKind,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.preview {
            crate::app_settings::set_hotkey_capture_active(true);
        }
        self.hotkey_width_spring.set_target(hotkey_capture_width(0));
        self.hotkey_capture = HotkeyCaptureState::Listening {
            kind,
            modifiers: HotkeyModifiers::default(),
            message: None,
            started_at: Instant::now(),
        };
        self.hotkey_capture_animation.set_enabled(true);
        self.hotkey_focus.focus(window);
        cx.notify();
    }

    fn cancel_hotkey_capture(&mut self, cx: &mut Context<Self>) {
        if !matches!(self.hotkey_capture, HotkeyCaptureState::Idle) {
            if !self.preview {
                crate::app_settings::set_hotkey_capture_active(false);
            }
            self.hotkey_capture = HotkeyCaptureState::Idle;
            self.hotkey_capture_animation.set_enabled(false);
            self.hotkey_width_spring.set_target(HOTKEY_MIN_WIDTH);
            cx.notify();
        }
    }

    fn capture_hotkey_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        if !self.hotkey_capture.is_listening() {
            return;
        }
        cx.stop_propagation();
        if event.is_held {
            return;
        }
        if event.keystroke.key == "escape" {
            self.cancel_hotkey_capture(cx);
            return;
        }
        let modifiers = hotkey_modifiers(event.keystroke.modifiers).without_side_constraints();
        let key = match hotkey_key(&event.keystroke.key) {
            Ok(key) => key,
            Err(message) => {
                self.set_hotkey_capture_message(message, cx);
                return;
            }
        };
        if modifiers.is_empty() && !is_function_key(&key.label) {
            self.set_hotkey_capture_message("Add a modifier", cx);
            return;
        }
        let binding = HotkeyBinding {
            modifiers,
            key: Some(key),
        };
        self.hotkey_width_spring
            .set_target(hotkey_capture_width(binding.keycaps().len()));
        self.save_hotkey_binding(binding, cx);
    }

    fn capture_hotkey_modifiers(&mut self, event: &ModifiersChangedEvent, cx: &mut Context<Self>) {
        let current = hotkey_modifiers(event.modifiers);
        if !current.is_empty() {
            self.hotkey_width_spring.set_target(hotkey_capture_width(
                HotkeyBinding {
                    modifiers: current,
                    key: None,
                }
                .keycaps()
                .len(),
            ));
        }
        let released = {
            let HotkeyCaptureState::Listening {
                modifiers, message, ..
            } = &mut self.hotkey_capture
            else {
                return;
            };
            if !current.is_empty() {
                if current.count() >= modifiers.count() {
                    *modifiers = current;
                    *message = None;
                }
                cx.notify();
                None
            } else if message.is_some() {
                *modifiers = HotkeyModifiers::default();
                *message = None;
                cx.notify();
                None
            } else {
                (!modifiers.is_empty()).then_some(*modifiers)
            }
        };
        if let Some(modifiers) = released {
            let modifiers = if modifiers.count() == 1 {
                modifiers
            } else {
                modifiers.without_side_constraints()
            };
            self.save_hotkey_binding(
                HotkeyBinding {
                    modifiers,
                    key: None,
                },
                cx,
            );
        }
    }

    fn set_hotkey_capture_message(&mut self, next_message: &'static str, cx: &mut Context<Self>) {
        if let HotkeyCaptureState::Listening { message, .. } = &mut self.hotkey_capture {
            *message = Some(next_message);
            cx.notify();
        }
    }

    fn save_hotkey_binding(&mut self, binding: HotkeyBinding, cx: &mut Context<Self>) {
        if binding.is_empty() {
            self.set_hotkey_capture_message("Press a shortcut", cx);
            return;
        }
        let kind = match self.hotkey_capture {
            HotkeyCaptureState::Listening { kind, .. } => kind,
            HotkeyCaptureState::Idle | HotkeyCaptureState::Saved { .. } => return,
        };
        if hotkey_binding_conflicts(&self.settings, kind, &binding) {
            self.set_hotkey_capture_message("Already in use", cx);
            return;
        }
        let keycap_count = binding.keycaps().len();
        if !self.update_settings(cx, |settings| set_hotkey_binding(settings, kind, binding)) {
            self.set_hotkey_capture_message("Could not save shortcut. Try again.", cx);
            return;
        }
        self.hotkey_width_spring
            .set_target(hotkey_saved_width(keycap_count));
        if !self.preview {
            crate::app_settings::set_hotkey_capture_active(false);
        }
        self.hotkey_capture = HotkeyCaptureState::Saved {
            kind,
            saved_at: Instant::now(),
        };
        cx.notify();
    }

    fn render_mode_replacements(
        &mut self,
        selection: ModeSelection,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let replacement_count = self.mode_inputs_for(selection).replacements.len();
        let replacement_rows = self
            .mode_inputs_for(selection)
            .replacements
            .iter()
            .enumerate()
            .map(|(index, inputs)| {
                div()
                    .id(("text-replacement", index))
                    .w_full()
                    .px_3()
                    .py_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .when(index + 1 < replacement_count, |row| {
                        row.border_b_1().border_color(tone(LINE))
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .child(inputs.matched_phrase.entity.clone()),
                    )
                    .child(
                        div()
                            .px_1()
                            .text_size(px(11.0))
                            .text_color(rgb(FAINT))
                            .child("→"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .child(inputs.output.entity.clone()),
                    )
                    .child(
                        compact_button("×")
                            .id(("remove-text-replacement", index))
                            .size(px(30.0))
                            .px_0()
                            .justify_center()
                            .flex_none()
                            .text_size(px(14.0))
                            .text_color(rgb(MUTED))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if this.update_mode_settings(selection, cx, |mode| {
                                    mode.replacements.remove(index);
                                }) {
                                    this.mode_inputs_mut(selection).replacements.remove(index);
                                }
                            })),
                    )
                    .into_any_element()
            })
            .collect::<Vec<_>>();

        let add = compact_header_plus_button()
            .id("add-mode-replacement")
            .on_click(cx.listener(move |this, _, window, cx| {
                let replacement = TextReplacement::default();
                if this.update_mode_settings(selection, cx, |mode| {
                    mode.replacements.push(replacement.clone());
                }) {
                    let inputs = Self::replacement_inputs(&replacement, cx);
                    let focus = inputs.matched_phrase.entity.focus_handle(cx);
                    this.mode_inputs_mut(selection).replacements.push(inputs);
                    focus.focus(window);
                }
            }))
            .into_any_element();

        compact_panel()
            .child(compact_panel_header(
                panel_title(
                    Glyph::Replace,
                    TINT_TEAL,
                    "Corrections",
                    "Swap words the model often gets wrong.",
                ),
                Some(add),
            ))
            .when(replacement_count == 0, |panel| {
                panel.child(
                    div()
                        .px_4()
                        .py_3()
                        .border_t_1()
                        .border_color(tone(LINE))
                        .text_size(px(11.5))
                        .text_color(rgb(FAINT))
                        .child("No corrections yet. Press + to add one."),
                )
            })
            .children(replacement_rows)
            .into_any_element()
    }

    fn transcription_picker_view(&mut self, selected_language: &str) -> TranscriptionPickerView {
        let models = crate::transcription_models::choices_for_runtime(selected_language)
            .into_iter()
            .map(|choice| {
                let model = choice.model;
                let installed = self.transcription_model_installed(model, selected_language);
                let downloading = self.transcription_status.model == Some(model.id);
                let active = transcription_selection_is_active(
                    &self.settings.transcription,
                    model,
                    selected_language,
                    installed,
                );
                let progress = if downloading {
                    model.download_bytes().map_or(0.0, |bytes| {
                        (self.transcription_status.downloaded_bytes as f32 / bytes as f32)
                            .clamp(0.0, 1.0)
                    })
                } else {
                    0.0
                };
                let status = if downloading {
                    if installed {
                        let elapsed = self
                            .transcription_status
                            .started
                            .map_or(0, |started| started.elapsed().as_secs());
                        let progress = self
                            .transcription_status
                            .started
                            .map_or(0.0, |started| (started.elapsed().as_secs_f32() % 1.6) / 1.6);
                        TranscriptionPickerStatus::Preparing {
                            label: format!("Loading model · {elapsed}s"),
                            progress: Some(TranscriptionPickerProgress::Loading(progress)),
                        }
                    } else {
                        TranscriptionPickerStatus::Preparing {
                            label: format!("Downloading {:.0}%", progress * 100.0),
                            progress: matches!(model.runtime, ModelRuntime::Gguf(_))
                                .then_some(TranscriptionPickerProgress::Downloading(progress)),
                        }
                    }
                } else if active {
                    TranscriptionPickerStatus::Active
                } else {
                    TranscriptionPickerStatus::Available {
                        installed,
                        deletion: self.transcription_model_deletion(model, installed),
                    }
                };
                TranscriptionPickerModel { choice, status }
            })
            .collect();
        TranscriptionPickerView {
            error: self.transcription_status.error.clone(),
            deletion_error: self.transcription_deletion_error.clone(),
            language: selected_language.to_string(),
            models,
        }
    }

    fn render_transcription_picker(
        &mut self,
        selected_language: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        render_shared_transcription_picker(self.transcription_picker_view(selected_language), cx)
    }

    fn dismiss_transcription_picker(&mut self, cx: &mut Context<Self>) {
        self.transcription_picker_language = None;
        self.clear_transcription_error();
        cx.notify();
    }

    fn render_microphone_picker(&self, cx: &mut Context<Self>) -> AnyElement {
        let choices = std::iter::once(None)
            .chain(self.microphone_devices.iter().cloned().map(Some))
            .collect::<Vec<_>>();
        microphone_picker_menu(
            choices,
            self.settings.microphone.clone(),
            self.microphone_picker_error.clone(),
            cx.listener(|this, device: &Option<String>, _, cx| {
                if this.update_settings(cx, |settings| settings.microphone = device.clone()) {
                    this.microphone_picker_open = false;
                    this.microphone_picker_error = None;
                }
            }),
            cx.listener(|this, _, _, cx| {
                this.microphone_picker_open = false;
                this.microphone_picker_error = None;
                cx.notify();
            }),
        )
        .into_any_element()
    }

    fn set_launch_at_login(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.preview {
            self.launch_at_login_status = Some(if enabled {
                LoginItemStatus::Enabled
            } else {
                LoginItemStatus::Disabled
            });
        } else if self.launch_at_login_status.is_none() || self.login_item_worker.is_none() {
            return;
        }
        self.launch_at_login_toggle.set_enabled(enabled);
        self.request_login_item(LoginItemRequest::SetEnabled(enabled));
        cx.notify();
    }

    fn render_permission_warnings(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let warnings = crate::onboarding::permission_warnings(self.setup_status);
        if self.setup_visible || warnings.is_empty() {
            return None;
        }
        let warning_count = warnings.len();
        Some(
            div()
                .child(settings_section_label("PERMISSIONS NEEDED"))
                .child(
                    settings_panel().children(warnings.into_iter().enumerate().map(
                        |(index, warning)| {
                            let (name, description) = permission_warning_copy(warning.kind);
                            let action = header_button(permission_action_label(warning.action))
                                .id(permission_warning_id(warning.kind))
                                .bg(tone(CANVAS))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.perform_permission_action(warning);
                                    cx.notify();
                                }));
                            settings_row(name, description, action)
                                .when(index + 1 == warning_count, |row| row.border_b_0())
                        },
                    )),
                )
                .into_any_element(),
        )
    }

    fn render_model_notice(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if self.setup_visible {
            return None;
        }
        let dictation_notice =
            dictation_model_notice(self.setup_status, self.transcription_status.model.is_some());
        let (title, description) = dictation_notice.or_else(|| {
            self.settings
                .commands_enabled
                .then(|| command_model_notice(&self.command_model_status))
                .flatten()
        })?;
        let action = if dictation_notice.is_some() {
            compact_button("Choose model")
                .id("choose-required-model")
                .bg(rgb(ACCENT))
                .text_color(rgb(ON_ACCENT))
                .font_weight(FontWeight::MEDIUM)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.transcription_picker_language =
                        Some(this.settings.transcription.language.clone());
                    cx.notify();
                }))
                .into_any_element()
        } else {
            div()
                .flex()
                .items_center()
                .gap_2()
                .when(
                    matches!(self.command_model_status, CommandModelStatus::Failed),
                    |controls| {
                        controls.child(
                            header_button("Retry")
                                .id("retry-command-model")
                                .bg(tone(CANVAS))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if !this.preview {
                                        cx.dispatch_action(&RetryCommands);
                                    }
                                })),
                        )
                    },
                )
                .child(
                    header_button("Turn off commands")
                        .id("disable-unavailable-commands")
                        .bg(tone(CANVAS))
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Err(error) = this.developer_set_commands_enabled(false, cx) {
                                tracing::warn!(%error, "could not disable voice commands");
                            }
                        })),
                )
                .into_any_element()
        };
        Some(
            div()
                .id("dictation-model-notice")
                .w_full()
                .flex()
                .justify_center()
                .flex_shrink_0()
                .px_8()
                .pt_6()
                .child(
                    div()
                        .w_full()
                        .max_w(px(PANE_CONTENT_WIDTH))
                        .px_4()
                        .py_3p5()
                        .flex()
                        .items_center()
                        .gap_4()
                        .rounded(px(PANEL_RADIUS))
                        .border_1()
                        .border_color(tone(LINE))
                        .bg(tone(SURFACE))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .border_l_2()
                                .border_color(rgb(ACCENT))
                                .pl_3()
                                .child(settings_copy(title, description)),
                        )
                        .child(div().flex_shrink_0().child(action)),
                )
                .into_any_element(),
        )
    }

    fn perform_permission_action(&mut self, warning: PermissionWarning) {
        match warning.action {
            PermissionAction::RequestMicrophone => crate::onboarding::request_microphone(),
            PermissionAction::OpenMicrophoneSettings => {
                crate::onboarding::open_permission_settings("microphone");
            }
            PermissionAction::OpenInputMonitoringSettings => {
                crate::permission_guide::show_input_monitoring();
            }
            PermissionAction::OpenAccessibilitySettings => {
                crate::permission_guide::show_accessibility();
            }
        }
        self.permission_refresh_at = Instant::now() + PERMISSION_REFRESH_INTERVAL;
    }

    fn render_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let languages = self.render_languages(window, cx);
        let permission_warnings = self.render_permission_warnings(cx);
        let hotkey_control = self.render_hotkey_setting_control(HotkeyKind::Dictation, window, cx);
        let paste_last_control = self.render_hotkey_control(HotkeyKind::PasteLast, window, cx);
        let transcription_model = definition(self.settings.transcription.model);
        let transcription_label = format!(
            "{} · {}",
            language_name(&self.settings.transcription.language),
            transcription_model.name
        );
        let transcription_control = disclosure_button(transcription_label)
            .id("transcription-model-setting")
            .on_click(cx.listener(|this, _, _, cx| {
                this.transcription_picker_language =
                    Some(this.settings.transcription.language.clone());
                this.clear_transcription_error();
                cx.notify();
            }));
        let double_tap_position = self.double_tap_toggle.render_position(window);
        let release_microphone_position = self.release_microphone_toggle.render_position(window);
        self.double_tap_only_visibility.set_enabled(
            self.settings.double_tap_lock && self.settings.dictation_hotkey.key.is_some(),
        );
        let double_tap_only_visibility = self
            .double_tap_only_visibility
            .render_position(window)
            .clamp(0.0, 1.0);
        let microphone_label = self
            .settings
            .microphone
            .clone()
            .unwrap_or_else(|| "Automatic".into());
        let microphone_picker = self
            .microphone_picker_open
            .then(|| self.render_microphone_picker(cx));
        let recording_audio_position = self.recording_audio_spring.render_position(window);
        let audio_widths = [50.0, 90.0, 80.0];
        let audio_behavior = sliding_segmented_control(recording_audio_position, &audio_widths)
            .children(RecordingAudioBehavior::ALL.into_iter().enumerate().map(
                |(index, behavior)| {
                    let selected = self.settings.recording_audio_behavior == behavior;
                    sliding_segmented_item(audio_widths[index], selected)
                        .id(("recording-audio-behavior", index))
                        .child(behavior.label())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if this.settings.recording_audio_behavior == behavior {
                                return;
                            }
                            if this.update_settings(cx, |settings| {
                                settings.recording_audio_behavior = behavior
                            }) {
                                this.recording_audio_spring.set_target(index as f32);
                            }
                        }))
                },
            ));
        let smart_editing_positions: Vec<f32> = self
            .smart_editing_toggles
            .iter_mut()
            .map(|toggle| toggle.render_position(window))
            .collect();
        let smart_editing_rows =
            SMART_EDITING_ROWS
                .iter()
                .enumerate()
                .map(|(index, (title, detail))| {
                    let row = settings_row(title, *detail, toggle(smart_editing_positions[index]));
                    let row = if index + 1 == SMART_EDITING_ROWS.len() {
                        row.border_b_0()
                    } else {
                        row
                    };
                    row.id(("smart-editing-setting", index))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let mut flags = smart_editing_flags(this.settings.smart_editing);
                            flags[index] = !flags[index];
                            if this.update_settings(cx, |settings| {
                                settings.smart_editing = smart_editing_from_flags(flags)
                            }) {
                                this.smart_editing_toggles[index].set_enabled(flags[index]);
                            }
                        }))
                });
        let microphone_mode = sliding_segmented_control(release_microphone_position, &[114.0; 2])
            .id("microphone-mode")
            .children(
                [("Keep ready (fast)", false), ("Release when idle", true)]
                    .into_iter()
                    .enumerate()
                    .map(|(index, (label, release))| {
                        sliding_segmented_item(
                            114.0,
                            self.settings.release_microphone_while_idle == release,
                        )
                        .id(("microphone-mode", index))
                        .child(label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if release && !this.settings.release_microphone_while_idle {
                                this.pending_microphone_policy =
                                    Some(MicrophonePolicyChange::DisableCommandsAndRelease);
                                cx.notify();
                                return;
                            }
                            this.pending_microphone_policy = None;
                            if this.settings.release_microphone_while_idle != release
                                && let Err(error) = this.update_microphone_policy(
                                    MicrophonePolicyChange::SetReleaseWhileIdle(release),
                                    cx,
                                )
                            {
                                tracing::error!(%error, "could not update microphone policy");
                            }
                            cx.notify();
                        }))
                    }),
            );
        let microphone_confirmation = (self.pending_microphone_policy
            == Some(MicrophonePolicyChange::DisableCommandsAndRelease))
        .then(|| {
            div()
                .px_4()
                .py_3()
                .flex()
                .items_center()
                .justify_between()
                .gap_4()
                .border_b_1()
                .border_color(tone(LINE))
                .bg(tone(CANVAS))
                .child(
                    div().flex_1().min_w_0().child(settings_copy(
                        "Release the microphone when idle?",
                        if self.settings.commands_enabled {
                            "Opens the microphone on shortcut press without pre-roll and turns Commands off."
                        } else {
                            "Opens the microphone on shortcut press without pre-roll. Commands remain off."
                        },
                    )),
                )
                .child(
                    div()
                        .flex_none()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            header_button("Cancel")
                                .id("cancel-release-microphone")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.pending_microphone_policy = None;
                                    cx.notify();
                                })),
                        )
                        .child(
                            header_button("Release microphone")
                                .id("confirm-release-microphone")
                                .bg(tone(SURFACE_SELECTED))
                                .text_color(rgb(TEXT))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    if let Err(error) = this.update_microphone_policy(
                                        MicrophonePolicyChange::DisableCommandsAndRelease,
                                        cx,
                                    ) {
                                        tracing::error!(%error, "could not update microphone policy");
                                    }
                                })),
                        ),
                )
        });
        settings_pane_titled(
            "Dictation",
            "settings-scroll",
            div()
                                    .children(permission_warnings)
                                    .child(settings_section_label("SPEECH"))
                                    .child(
                                        settings_panel()
                                    .child(settings_row(
                                        "Local transcription",
                                        "Language and on-device speech model",
                                        transcription_control,
                                    ))
                                    .child(settings_row(
                                        "Microphone",
                                        "Input device for dictation and commands",
                                        div()
                                            .relative()
                                            .flex_none()
                                            .child(
                                                disclosure_button(microphone_label)
                                                    .id("microphone-setting")
                                                    .on_click(cx.listener(|this, _, _, cx| {
                                                        this.toggle_microphone_picker(cx)
                                                    })),
                                            )
                                            .children(microphone_picker.map(deferred)),
                                    ))
                                    .when(
                                        transcription_model.supports_recognition_hints,
                                        |settings| {
                                            settings.child(settings_row(
                                                "Recognition hints",
                                                "Names and terms to prime the speech model",
                                                div()
                                                    .w(px(320.0))
                                                    .flex_none()
                                                    .child(
                                                        self.transcription_hints.entity.clone(),
                                                    ),
                                            ))
                                        },
                                    )
                            )
                            .child(settings_section_label("LANGUAGES"))
                            .child(languages)
                            .child(settings_section_label("SHORTCUTS"))
                            .child(
                                settings_panel()
                                    .child(
                                        settings_row(
                                            "Dictation shortcut",
                                            "Hold to dictate, release to transcribe",
                                            hotkey_control,
                                        )
                                        .id("dictation-hotkey-setting"),
                                    )
                                    .child(
                                        settings_row(
                                            "Double-tap to lock",
                                            "Double-tap the shortcut for hands-free dictation",
                                            toggle(double_tap_position),
                                        )
                                        .id("double-tap-setting")
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            let enabled = !this.settings.double_tap_lock;
                                            this.set_double_tap_lock(enabled, cx);
                                        })),
                                    )
                                    .child(
                                        div()
                                            .h(px(72.0 * double_tap_only_visibility))
                                            .overflow_hidden()
                                            .opacity(double_tap_only_visibility)
                                            .child(
                                                settings_row(
                                                    "Double-tap only",
                                                    "Wait for two complete shortcut taps before recording",
                                                    toggle(if self.settings.double_tap_only { 1.0 } else { 0.0 }),
                                                )
                                                .id("double-tap-only-setting")
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    let enabled = !this.settings.double_tap_only;
                                                    this.set_double_tap_only(enabled, cx);
                                                })),
                                            ),
                                    )
                                    .child(
                                        settings_row(
                                            "Paste last dictation",
                                            "Re-paste the most recent dictation",
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap_2()
                                                .child(paste_last_control)
                                                .when(self.settings.paste_last_hotkey.is_some(), |row| {
                                                    row.child(
                                                        canvas_button()
                                                            .id("disable-paste-last-hotkey")
                                                            .h(px(32.0))
                                                            .px_2p5()
                                                            .text_size(px(11.0))
                                                            .text_color(rgb(MUTED))
                                                            .child("Clear")
                                                            .on_click(cx.listener(|this, _, _, cx| {
                                                                this.update_settings(cx, |settings| settings.paste_last_hotkey = None);
                                                            })),
                                                    )
                                                }),
                                        )
                                        .border_b_0()
                                        .id("paste-last-hotkey-setting"),
                                    )
                            )
                            .child(settings_section_label("BEHAVIOR"))
                            .child(
                                settings_panel()
                                    .child(
                                        settings_row(
                                            "While dictating",
                                            "Mute or pause other audio during dictation",
                                            audio_behavior,
                                        )
                                        .id("recording-audio-setting"),
                                    )
                                    .child(settings_row(
                                        "Microphone mode",
                                        if self.settings.release_microphone_while_idle {
                                            "Opens only while holding the shortcut. Disables pre-roll and commands."
                                        } else {
                                            "Keeps the microphone open for instant start and pre-roll."
                                        },
                                        microphone_mode,
                                    )
                                    .border_b_0())
                                    .children(microphone_confirmation)
                            )
                            .child(settings_section_label("SMART EDITING"))
                            .child(settings_panel().children(smart_editing_rows)),
        )
    }

    fn render_general(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let launch_at_login_position = self.launch_at_login_toggle.render_position(window);
        let launch_at_login_control = if self.launch_at_login_status.is_none() {
            div()
                .text_color(rgb(MUTED))
                .child(if self.login_item_worker.is_some() {
                    "Checking…"
                } else {
                    "Unavailable"
                })
                .into_any_element()
        } else if self.launch_at_login_status == Some(LoginItemStatus::RequiresApproval) {
            compact_button("Open Settings")
                .id("launch-at-login-approval")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.request_login_item(LoginItemRequest::OpenSettings);
                    cx.notify();
                }))
                .into_any_element()
        } else {
            toggle(launch_at_login_position)
        };
        let menu_bar_position = self.menu_bar_icon_toggle.render_position(window);
        let dock_position = self.dock_icon_toggle.render_position(window);
        let both_hidden = !self.settings.show_menu_bar_icon && !self.settings.show_dock_icon;
        settings_pane_titled(
            "General",
            "general-scroll",
            div()
                .child(settings_section_label("STARTUP"))
                .child(
                    settings_panel()
                                    .child(
                                        settings_row(
                                            "Launch at login",
                                            "Start Hello when you log in",
                                            launch_at_login_control,
                                        )
                                        .id("launch-at-login-setting")
                                        .when(
                                            matches!(self.launch_at_login_status, Some(LoginItemStatus::Enabled | LoginItemStatus::Disabled))
                                                && (self.preview || self.login_item_worker.is_some()),
                                            |row| {
                                                row.on_click(cx.listener(
                                                    |this, _, _, cx| {
                                                        let enabled = !this.launch_at_login_toggle.enabled();
                                                        this.set_launch_at_login(enabled, cx);
                                                    },
                                                ))
                                            },
                                        ),
                                    )
                                    .when_some(
                                        self.launch_at_login_error.clone(),
                                        |settings, error| {
                                            settings.child(
                                                div()
                                                    .px_4()
                                                    .py_2()
                                                    .text_size(px(11.0))
                                                    .text_color(rgb(NEGATIVE))
                                                    .child(error),
                                            )
                                        },
                                    )
                )
                .child(settings_section_label("MENU BAR & DOCK"))
                .child(
                    settings_panel()
                        .child(
                                                        settings_row(
                                "Show menu bar icon",
                                "Quick access to paste, models, and settings",
                                toggle(menu_bar_position),
                            )
                            .id("menu-bar-icon-setting")
                            .on_click(cx.listener(|this, _, _, cx| {
                                let visible = !this.settings.show_menu_bar_icon;
                                this.set_menu_bar_icon(visible, cx);
                            })),
                        )
                        .child(
                            settings_row(
                                "Show Dock icon",
                                                                "Show Hello in the Dock when the window is closed",
                                toggle(dock_position),
                            )
                            .when(!both_hidden, |row| row.border_b_0())
                            .id("dock-icon-setting")
                            .on_click(cx.listener(|this, _, _, cx| {
                                if !this.update_settings(cx, |settings| {
                                    settings.show_dock_icon = !settings.show_dock_icon
                                }) {
                                    return;
                                }
                                this.dock_icon_toggle
                                    .set_enabled(this.settings.show_dock_icon);
                            })),
                        )
                        .when(both_hidden, |panel| {
                            panel.child(
                                div()
                                    .px_4()
                                    .py_3()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .text_size(px(12.0))
                                    .text_color(rgb(TEXT_SOFT))
                                    .bg(rgb(0x1f2a3d))
                                    .child("Hello runs quietly in the background. To open this window, launch Hello again from Applications or Spotlight."),
                            )
                        }),
                )
                .child(settings_section_label("ABOUT"))
                .child(
                    settings_panel().child(
                        settings_row(
                            "Hello",
                            "Free, private dictation and meeting notes. Everything stays on your Mac.",
                            div()
                                .flex()
                                .items_center()
                                .gap_3()
                                .child(
                                    div()
                                        .text_size(px(12.0))
                                        .text_color(rgb(MUTED))
                                        .child(format!("Version {APP_VERSION}")),
                                )
                                .children(about_update_button(self.update_status, self.preview)),
                        )
                        .border_b_0(),
                    ),
                ),
        )
    }

    fn render_sounds(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let sound_volume_position = self.sound_volume_spring.render_position(window);
        let selected_volume = sound_volume_index(&self.settings);
        let sound_volume = sliding_segmented_control(sound_volume_position, &[34.0; 5]).children(
            crate::desktop_ui::SOUND_VOLUME_STEPS
                .into_iter()
                .enumerate()
                .map(|(index, (label, volume))| {
                    sliding_segmented_item(34.0, selected_volume == index)
                        .id(("sound-volume", index))
                        .text_size(px(10.0))
                        .child(label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if !this.update_settings(cx, |settings| {
                                settings.sound_effects = volume > 0.0;
                                if volume > 0.0 {
                                    settings.sound_effect_volume = volume;
                                }
                            }) {
                                return;
                            }
                            this.sound_volume_spring.set_target(index as f32);
                            if volume > 0.0 && !this.preview {
                                crate::feedback::play(crate::feedback::Tone::DictationStart);
                            }
                        }))
                }),
        );
        let sounds_on = self.settings.sound_effects;
        let sound_chips = crate::feedback::SoundTheme::ALL
            .into_iter()
            .enumerate()
            .map(|(index, theme)| {
                let selected = self.settings.sound_theme == theme;
                div()
                    .id(("sound-theme", index))
                    .h(px(34.0))
                    .px(px(16.0))
                    .flex()
                    .items_center()
                    .gap_2()
                    .rounded_full()
                    .border_1()
                    .text_size(px(12.5))
                    .font_weight(FontWeight::MEDIUM)
                    .when(selected, |chip| {
                        chip.bg(rgb(ACCENT))
                            .border_color(rgb(ACCENT))
                            .text_color(rgb(0xffffff))
                    })
                    .when(!selected, |chip| {
                        chip.bg(rgb(0x2a2a2e))
                            .border_color(rgb(0x38383d))
                            .text_color(rgb(TEXT_SOFT))
                            .hover(|chip| chip.bg(rgb(0x333338)))
                    })
                    .child(theme.label())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let changed = this.settings.sound_theme != theme;
                        if (!changed
                            || this.update_settings(cx, |settings| settings.sound_theme = theme))
                            && !this.preview
                        {
                            crate::feedback::preview();
                        }
                    }))
            });
        settings_pane_titled(
            "Sounds",
            "sounds-scroll",
            div()
                .child(settings_section_label("VOLUME"))
                .child(
                    settings_panel().child(
                        settings_row(
                            "Sound volume",
                            "Start, processing, cancel, and error tones",
                            sound_volume,
                        )
                        .border_b_0()
                        .id("sound-effects-setting"),
                    ),
                )
                .child(settings_section_label("SOUND THEME"))
                .child(
                    settings_panel().child(
                        div()
                            .px_4()
                            .py_4()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(settings_copy(
                                "Dictation sounds",
                                if sounds_on {
                                    "Played when you start, finish, or cancel. Click one to hear it."
                                } else {
                                    "Sounds are off. Raise the volume above to hear them."
                                },
                            ))
                            .child(div().flex().flex_wrap().gap_2().children(sound_chips)),
                    ),
                ),
        )
    }

    fn toggle_microphone_picker(&mut self, cx: &mut Context<Self>) {
        self.microphone_picker_open = !self.microphone_picker_open;
        if self.microphone_picker_open {
            match crate::audio::input_device_names() {
                Ok(devices) => {
                    self.microphone_devices = devices;
                    self.microphone_picker_error = None;
                }
                Err(error) => self.microphone_picker_error = Some(error.to_string()),
            }
        }
        cx.notify();
    }

    fn render_setup(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let status = self.setup_status;
        let microphone = match status.microphone {
            PermissionState::Ready => setup_ready_badge(),
            PermissionState::NeedsRequest => header_button("Allow")
                .id("setup-microphone")
                .bg(tone(CANVAS))
                .on_click(cx.listener(|_this, _, _, cx| {
                    crate::onboarding::request_microphone();
                    cx.notify();
                }))
                .into_any_element(),
            PermissionState::NeedsSettings => header_button("Open Settings")
                .id("setup-microphone")
                .bg(tone(CANVAS))
                .on_click(cx.listener(|_this, _, _, cx| {
                    crate::onboarding::open_permission_settings("microphone");
                    cx.notify();
                }))
                .into_any_element(),
        };
        let mut permission_items: Vec<(&'static str, &'static str, AnyElement)> = Vec::new();
        if status.microphone != PermissionState::Ready {
            permission_items.push((
                "Microphone",
                if self.settings.commands_enabled {
                    "Hear commands and dictation."
                } else {
                    "Record dictation while you hold the shortcut."
                },
                microphone,
            ));
        }
        if status.input_monitoring != PermissionState::Ready {
            let input_monitoring = header_button("Grant Access")
                .id("setup-input-monitoring")
                .bg(tone(CANVAS))
                .on_click(cx.listener(|_this, _, _, cx| {
                    crate::permission_guide::show_input_monitoring();
                    cx.notify();
                }))
                .into_any_element();
            permission_items.push((
                "Input Monitoring",
                "Recognize the dictation shortcut anywhere.",
                input_monitoring,
            ));
        }
        if status.accessibility != PermissionState::Ready {
            let accessibility = header_button("Grant Access")
                .id("setup-accessibility")
                .bg(tone(CANVAS))
                .on_click(cx.listener(|_this, _, _, cx| {
                    crate::permission_guide::show_accessibility();
                    cx.notify();
                }))
                .into_any_element();
            permission_items.push((
                "Accessibility",
                if self.settings.commands_enabled {
                    "Paste text and run keyboard commands."
                } else {
                    "Paste completed dictation into the foreground app."
                },
                accessibility,
            ));
        }
        let permission_count = permission_items.len();
        let permission_rows = permission_items
            .into_iter()
            .enumerate()
            .map(|(index, (title, description, control))| {
                settings_row(title, description, control)
                    .when(index + 1 == permission_count, |row| row.border_b_0())
            })
            .collect::<Vec<_>>();
        let models_ready = status.transcription_model;
        let model_notice =
            dictation_model_notice(status, self.transcription_status.model.is_some());
        let models = if models_ready {
            setup_ready_badge()
        } else {
            compact_button("Choose model")
                .id("setup-models")
                .bg(rgb(ACCENT))
                .text_color(rgb(ON_ACCENT))
                .font_weight(FontWeight::SEMIBOLD)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.transcription_picker_language =
                        Some(this.settings.transcription.language.clone());
                    this.clear_transcription_error();
                    cx.notify();
                }))
                .into_any_element()
        };

        div()
            .id("setup-backdrop")
            .occlude()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(rgba(0x000000dd))
            .child(
                div()
                    .id("setup")
                    .w(px(640.0))
                    .p_6()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .rounded(px(PANEL_RADIUS + 2.0))
                    .border_1()
                    .border_color(tone(LINE))
                    .bg(rgb(crate::desktop_ui::SHEET))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(px(18.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Set up Hello"),
                            )
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .text_color(rgb(MUTED))
                                    .child("Grant permissions and choose a local model to start dictating."),
                            ),
                    )
                    .when(!permission_rows.is_empty(), |setup| {
                        setup.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .child(setup_group_label("PERMISSIONS NEEDED"))
                                .child(settings_panel().children(permission_rows)),
                        )
                    })
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(setup_group_label("LOCAL MODELS"))
                            .child(
                                settings_panel().child(
                                    settings_row(
                                        if let Some((title, _)) = model_notice {
                                            title
                                        } else {
                                            "Local dictation model"
                                        },
                                        if let Some((_, description)) = model_notice {
                                            description
                                        } else {
                                            "The selected model transcribes speech entirely on this Mac."
                                        },
                                        models,
                                    )
                                    .border_b_0(),
                                ),
                            ),
                    )
                    .when(crate::MEETINGS_ENABLED, |setup| {
                        setup.child(
                            div()
                                .text_size(px(10.0))
                                .line_height(px(16.0))
                                .text_color(rgb(FAINT))
                                .child("Meeting recording requests Screen & System Audio Recording separately when you use it."),
                        )
                    }),
            )
            .into_any_element()
    }

    fn save_settings(&mut self, cx: &mut Context<Self>) {
        // Modes and text fields are explicit drafts; failed writes remain dirty
        // for retry or the close-time flush.
        self.settings_dirty = true;
        if let Err(error) = self.persist_settings() {
            tracing::error!(%error, "could not save app settings");
        }
        cx.notify();
    }

    fn persist_settings(&mut self) -> color_eyre::Result<()> {
        self.commit_settings(self.settings_candidate())
    }

    fn settings_candidate(&self) -> AppSettings {
        let mut candidate = self.settings.clone();
        self.mode_editors.project(&mut candidate);
        candidate
    }

    /// Saves before assigning so a failed save leaves the live settings
    /// untouched; the generation bump supersedes any pending debounced save.
    fn commit_settings(&mut self, candidate: AppSettings) -> color_eyre::Result<()> {
        #[cfg(test)]
        if let Some(save) = self.settings_save_override {
            return self.commit_settings_with(candidate, save);
        }
        let preview = self.preview;
        self.commit_settings_with(
            candidate,
            |candidate| {
                if preview { Ok(()) } else { candidate.save() }
            },
        )
    }

    fn commit_settings_with(
        &mut self,
        candidate: AppSettings,
        save: impl FnOnce(&AppSettings) -> color_eyre::Result<()>,
    ) -> color_eyre::Result<()> {
        if let Err(error) = self.settings.commit_with(candidate, save) {
            self.settings_load_error = Some(error.to_string());
            return Err(error);
        }
        self.settings_save_generation = self.settings_save_generation.wrapping_add(1);
        self.settings_dirty = false;
        self.settings_load_error = None;
        Ok(())
    }

    fn update_settings(
        &mut self,
        cx: &mut Context<Self>,
        update: impl FnOnce(&mut AppSettings),
    ) -> bool {
        let mut candidate = self.settings_candidate();
        update(&mut candidate);
        let saved = self.commit_settings(candidate).is_ok();
        cx.notify();
        saved
    }

    fn set_double_tap_lock(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.update_settings(cx, |settings| {
            settings.double_tap_lock = enabled;
            if !enabled {
                settings.double_tap_only = false;
            }
        }) {
            self.double_tap_toggle.set_enabled(enabled);
        }
    }

    fn set_double_tap_only(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.update_settings(cx, |settings| {
            settings.double_tap_only =
                enabled && settings.double_tap_lock && settings.dictation_hotkey.key.is_some();
        });
    }

    fn processing_input(
        placeholder: &'static str,
        initial: &str,
        cx: &mut Context<Self>,
    ) -> ProcessingInput {
        let entity = cx.new(|cx| TextInput::new(cx, placeholder, initial));
        Self::synchronized_processing_input(entity, cx)
    }

    fn replacement_inputs(
        replacement: &TextReplacement,
        cx: &mut Context<Self>,
    ) -> ReplacementInputs {
        ReplacementInputs {
            matched_phrase: Self::processing_input(
                "Spoken phrase (e.g. open code)",
                &replacement.matched_phrase,
                cx,
            ),
            output: Self::processing_input("Replacement (e.g. GitHub)", &replacement.output, cx),
        }
    }

    fn profile_name_input(initial: &str, cx: &mut Context<Self>) -> ProcessingInput {
        let entity = cx.new(|cx| TextInput::new(cx, "Your name", initial));
        let subscription = cx.subscribe(&entity, |this, _, _: &TextChanged, cx| {
            this.settings.profile_name =
                this.profile_name.entity.read(cx).text().trim().to_string();
            this.schedule_settings_save(cx);
            cx.notify();
        });
        ProcessingInput {
            entity,
            _subscriptions: vec![subscription],
        }
    }

    fn set_indicator_style(&mut self, style: IndicatorStyle, cx: &mut Context<Self>) {
        if self.settings.indicator_style == style {
            return;
        }
        if self.update_settings(cx, |settings| settings.indicator_style = style) {
            let position = IndicatorPosition::resolve(self.settings.indicator_position, style);
            self.indicator_position_spring
                .set_target(indicator_position_index(position) as f32);
        }
    }

    fn set_theme(&mut self, theme: ColorTheme, cx: &mut Context<Self>) {
        if self.settings.theme == theme {
            return;
        }
        if self.update_settings(cx, |settings| settings.theme = theme) {
            self.theme_spring.set_target(theme_index(theme) as f32);
        }
    }

    fn set_indicator_position(&mut self, position: IndicatorPosition, cx: &mut Context<Self>) {
        if self.settings.indicator_position == Some(position) {
            return;
        }
        if self.update_settings(cx, |settings| settings.indicator_position = Some(position)) {
            self.indicator_position_spring
                .set_target(indicator_position_index(position) as f32);
        }
    }

    fn set_multilingual(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if !self.update_settings(cx, |settings| settings.multilingual.enabled = enabled) {
            return;
        }
        self.multilingual_toggle.set_enabled(enabled);
        if enabled && self.settings.multilingual.routing == LanguageRouting::Auto && !self.preview {
            self.download_language_model(TranscriptionModelId::WhisperBaseLanguageId);
        }
    }

    /// Menu choice: switches the second language on if needed, then routes.
    fn choose_language(&mut self, routing: LanguageRouting, cx: &mut Context<Self>) {
        if !self.settings.multilingual.enabled {
            self.set_multilingual(true, cx);
        }
        self.set_language_routing(routing, cx);
        cx.notify();
    }

    fn set_language_routing(&mut self, routing: LanguageRouting, cx: &mut Context<Self>) {
        if self.update_settings(cx, |settings| settings.multilingual.routing = routing)
            && routing == LanguageRouting::Auto
            && !self.preview
        {
            self.download_language_model(TranscriptionModelId::WhisperBaseLanguageId);
        }
    }

    /// Fetch a helper model in the background; the poll loop picks up the
    /// result and the worker loads it on its next reload.
    fn download_language_model(&mut self, id: TranscriptionModelId) {
        let definition = crate::transcription_models::definition(id);
        if self.language_download.is_some()
            || !definition.hosted()
            || crate::transcription_models::is_installed(definition, "auto")
        {
            return;
        }
        let bytes = Arc::new(AtomicU64::new(0));
        let progress = bytes.clone();
        let (sender, receiver) = sync_channel(1);
        thread::spawn(move || {
            let canceled = AtomicBool::new(false);
            let result = crate::transcription_models::download_with_progress(
                definition, &canceled, &progress,
            )
            .map(drop)
            .map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
        self.language_download_error = None;
        self.language_download = Some(LanguageDownload {
            model: id,
            bytes,
            done: receiver,
        });
    }

    fn poll_language_download(&mut self) -> bool {
        let Some(download) = &self.language_download else {
            return false;
        };
        match download.done.try_recv() {
            Ok(result) => {
                self.language_download = None;
                if let Err(error) = result {
                    self.language_download_error = Some(error);
                }
                // Bump the runtime selection so the worker loads the new model.
                crate::app_settings::refresh_second_language();
                true
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => true,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.language_download = None;
                true
            }
        }
    }

    fn render_languages(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let multilingual = self.settings.multilingual.clone();
        let enabled_position = self.multilingual_toggle.render_position(window);
        let second_name =
            crate::transcription_models::language_name(&multilingual.second.language).to_string();
        let main_name =
            crate::transcription_models::language_name(&self.settings.transcription.language)
                .to_string();
        let routing_index = LanguageRouting::ALL
            .iter()
            .position(|routing| *routing == multilingual.routing)
            .unwrap_or(0);
        let routing_widths = [96.0, 78.0, 78.0];
        let routing_labels = [
            "Auto-detect".to_string(),
            main_name.clone(),
            second_name.clone(),
        ];
        let routing_control = sliding_segmented_control(routing_index as f32, &routing_widths)
            .children(
                LanguageRouting::ALL
                    .into_iter()
                    .enumerate()
                    .map(|(index, routing)| {
                        sliding_segmented_item(
                            routing_widths[index],
                            routing == multilingual.routing,
                        )
                        .id(("language-routing", index))
                        .child(routing_labels[index].clone())
                        .on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.set_language_routing(routing, cx)
                            }),
                        )
                    }),
            );
        let model_status = |id: TranscriptionModelId, this: &Self| -> AnyElement {
            let definition = crate::transcription_models::definition(id);
            let installed = crate::transcription_models::is_installed(definition, "auto");
            let downloading = this
                .language_download
                .as_ref()
                .filter(|download| download.model == id);
            if installed {
                icon_chip(glyph(Glyph::Bolt, 12.0, 0x30d158), "Ready")
                    .text_color(rgb(POSITIVE))
                    .into_any_element()
            } else if let Some(download) = downloading {
                let total = definition.download_bytes().unwrap_or(1).max(1);
                let percent = download.bytes.load(Ordering::Relaxed) * 100 / total;
                chip(format!("Downloading {percent}%")).into_any_element()
            } else if definition.hosted() {
                glyph_button(Glyph::Plus, format!("Download {}", definition.size_label()))
                    .id(("language-model-download", id as usize))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.download_language_model(id);
                        cx.notify();
                    }))
                    .into_any_element()
            } else {
                chip("Not installed")
                    .text_color(rgb(MUTED))
                    .into_any_element()
            }
        };
        let second_status = model_status(multilingual.second.model, self);
        let detector_status = model_status(TranscriptionModelId::WhisperBaseLanguageId, self);
        let samples_position = self.language_samples_toggle.render_position(window);
        let samples_on = multilingual.collect_samples;
        let samples_saved = crate::language_samples::count();
        let second_definition = crate::transcription_models::definition(multilingual.second.model);
        settings_panel()
            .child(
                settings_row(
                    "Bangla + English",
                    "Speak either language. Hello keeps both models ready and picks the right one.",
                    toggle(enabled_position),
                )
                .when(!multilingual.enabled, |row| row.border_b_0())
                .id("multilingual-setting")
                .on_click(cx.listener(|this, _, _, cx| {
                    let enabled = !this.settings.multilingual.enabled;
                    this.set_multilingual(enabled, cx);
                })),
            )
            .when(multilingual.enabled, |panel| {
                panel
                    .child(settings_row(
                        "Language",
                        "Auto-detect listens to each recording first",
                        routing_control,
                    ))
                    .child(settings_row(
                        "Bangla model",
                        format!(
                            "{} · {}",
                            second_definition.name,
                            second_definition.size_label()
                        ),
                        second_status,
                    ))
                    .child(settings_row(
                        "Language detector",
                        "Whisper base · 85 MB · only used by Auto-detect",
                        detector_status,
                    ))
                    .child(
                        settings_row(
                            "Help tune Bangla",
                            if samples_on {
                                format!(
                                    "Saving your dictations on this Mac · {samples_saved} of {} saved",
                                    crate::language_samples::MAX_SAMPLES
                                )
                            } else if samples_saved > 0 {
                                format!("Off · {samples_saved} recordings kept on this Mac")
                            } else {
                                "Save your next dictations on this Mac so language switching can \
                                 be tuned to your voice"
                                    .to_string()
                            },
                            div()
                                .flex()
                                .items_center()
                                .gap_2()
                                .when(samples_saved > 0, |controls| {
                                    controls.child(
                                        compact_button("Show")
                                            .id("language-samples-show")
                                            .on_click(cx.listener(|_, _, _, cx| {
                                                cx.stop_propagation();
                                                if let Ok(directory) =
                                                    crate::language_samples::directory()
                                                {
                                                    open_path(&directory);
                                                }
                                            })),
                                    )
                                })
                                .child(toggle(samples_position)),
                        )
                        .border_b_0()
                        .id("language-samples-setting")
                        .on_click(cx.listener(|this, _, _, cx| {
                            let enabled = !this.settings.multilingual.collect_samples;
                            if this.update_settings(cx, |settings| {
                                settings.multilingual.collect_samples = enabled
                            }) {
                                this.language_samples_toggle.set_enabled(enabled);
                            }
                        })),
                    )
                    .when_some(self.language_download_error.clone(), |panel, error| {
                        panel.child(
                            div()
                                .px_4()
                                .pb_3()
                                .text_size(px(11.0))
                                .text_color(rgb(NEGATIVE))
                                .child(error),
                        )
                    })
            })
            .into_any_element()
    }

    fn set_menu_bar_icon(&mut self, visible: bool, cx: &mut Context<Self>) {
        if !self.update_settings(cx, |settings| settings.show_menu_bar_icon = visible) {
            return;
        }
        self.menu_bar_icon_toggle.set_enabled(visible);
        if !self.preview {
            crate::status_item::set_visible(visible);
        }
    }

    fn transcription_hints_input(initial: &str, cx: &mut Context<Self>) -> ProcessingInput {
        let entity = cx.new(|cx| {
            TextInput::multiline(
                cx,
                "Names and terms Whisper should expect, e.g. GitHub, Dhaka...",
                initial,
            )
        });
        let subscription = cx.subscribe(&entity, |this, _, _: &TextChanged, cx| {
            this.settings.transcription.recognition_hints =
                this.transcription_hints.entity.read(cx).text().to_string();
            this.schedule_settings_save(cx);
        });
        ProcessingInput {
            entity,
            _subscriptions: vec![subscription],
        }
    }

    fn synchronized_processing_input(
        entity: Entity<TextInput>,
        cx: &mut Context<Self>,
    ) -> ProcessingInput {
        let subscription = cx.subscribe(&entity, |this, _, _: &TextChanged, cx| {
            this.sync_processing_settings(cx)
        });
        ProcessingInput {
            entity,
            _subscriptions: vec![subscription],
        }
    }

    fn application_search_input(cx: &mut Context<Self>) -> ProcessingInput {
        let entity = cx.new(|cx| TextInput::picker(cx, "Search applications", ""));
        let changed = cx.subscribe(&entity, |this, _, _: &TextChanged, cx| {
            this.application_picker_highlight = 0;
            cx.notify();
        });
        let navigate = cx.subscribe(&entity, |this, _, event: &TextNavigate, cx| {
            let count = this.application_choice_names(cx).len();
            if count > 0 {
                this.application_picker_highlight =
                    advance_highlight(this.application_picker_highlight, event.0, count);
                cx.notify();
            }
        });
        let submitted = cx.subscribe(&entity, |this, _, _: &TextSubmitted, cx| {
            if let Some(name) = this
                .application_choice_names(cx)
                .get(this.application_picker_highlight)
                .cloned()
            {
                this.add_mode_application(this.selected_mode, name, cx);
            }
        });
        let dismissed = cx.subscribe(&entity, |this, _, _: &TextDismissed, cx| {
            this.close_application_picker(cx);
            cx.notify();
        });
        ProcessingInput {
            entity,
            _subscriptions: vec![changed, navigate, submitted, dismissed],
        }
    }

    fn mode_inputs(mode: &DictationMode, cx: &mut Context<Self>) -> ModeInputs {
        ModeInputs {
            name: Self::processing_input("Mode name", &mode.name, cx),
            browser_hosts: Self::processing_input(
                "github.com, x.com",
                &mode.browser_hosts.join(", "),
                cx,
            ),
            replacements: mode
                .replacements
                .iter()
                .map(|replacement| Self::replacement_inputs(replacement, cx))
                .collect(),
        }
    }

    fn mode_editor(mode: &DictationMode, cx: &mut Context<Self>) -> ModeEditor {
        ModeEditor {
            settings: mode.clone(),
            inputs: Self::mode_inputs(mode, cx),
        }
    }

    fn mode_editors(settings: &AppSettings, cx: &mut Context<Self>) -> ModeEditors {
        let processing = &settings.dictation_processing;
        ModeEditors {
            default_mode: Self::mode_editor(&processing.default_mode, cx),
            modes: processing
                .modes
                .iter()
                .map(|mode| Self::mode_editor(mode, cx))
                .collect(),
        }
    }

    fn sync_processing_settings(&mut self, cx: &mut Context<Self>) {
        let row = &mut self.mode_editors.default_mode;
        apply_mode_inputs(&row.inputs, &mut row.settings, true, cx);
        for row in &mut self.mode_editors.modes {
            apply_mode_inputs(&row.inputs, &mut row.settings, false, cx);
        }
        self.schedule_settings_save(cx);
    }

    fn update_mode_settings(
        &mut self,
        selection: ModeSelection,
        cx: &mut Context<Self>,
        update: impl FnOnce(&mut DictationMode),
    ) -> bool {
        let mut candidate = self.settings_candidate();
        let mode = mode_settings_mut(&mut candidate, selection);
        update(mode);
        let mode = mode.clone();
        let saved = self.commit_settings(candidate).is_ok();
        if saved {
            self.mode_editor_mut(selection).settings = mode;
        }
        cx.notify();
        saved
    }

    fn add_mode(&mut self, cx: &mut Context<Self>) -> Option<ModeSelection> {
        let mut mode = self.mode_editors.default_mode.settings.clone();
        mode.name = format!("Mode {}", self.mode_editors.modes.len() + 1);
        mode.applications.clear();
        mode.browser_hosts.clear();
        let mut candidate = self.settings_candidate();
        candidate.dictation_processing.modes.push(mode.clone());
        let saved = self.commit_settings(candidate).is_ok();
        cx.notify();
        if !saved {
            return None;
        }
        let selection = ModeSelection::Custom(self.mode_editors.modes.len());
        self.mode_editors.modes.push(Self::mode_editor(&mode, cx));
        Some(selection)
    }

    fn delete_mode(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.mode_editors.modes.len() {
            return;
        }
        let mut candidate = self.settings_candidate();
        candidate.dictation_processing.modes.remove(index);
        if self.commit_settings(candidate).is_ok() {
            self.mode_editors.modes.remove(index);
            self.selected_mode = ModeSelection::Default;
            self.mode_delete_armed = false;
            self.application_picker_open = false;
            self.transformation_picker_open = false;
        }
        cx.notify();
    }

    fn schedule_settings_save(&mut self, cx: &mut Context<Self>) {
        self.settings_dirty = true;
        self.settings_save_generation = self.settings_save_generation.wrapping_add(1);
        let generation = self.settings_save_generation;
        let delay = cx.background_executor().timer(Duration::from_millis(250));
        cx.spawn(async move |window, cx| {
            delay.await;
            let _ = window.update(cx, |window, cx| {
                if window.settings_save_generation == generation {
                    window.save_settings(cx);
                }
            });
        })
        .detach();
    }

    fn render_modes(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let compact = window.viewport_size().width < px(980.0);
        let mode_list_width = if compact { 220.0 } else { 250.0 };
        let add = glyph_button(Glyph::Plus, "New mode")
            .id("add-dictation-mode")
            .on_click(cx.listener(|this, _, window, cx| {
                if let Some(selection) = this.add_mode(cx) {
                    this.select_mode(selection, window, cx);
                }
            }));
        let default_selected = self.selected_mode == ModeSelection::Default;
        let custom_count = self.mode_editors.modes.len();
        let mut rows = vec![
            group_heading("Default", None).into_any_element(),
            mode_row(
                Glyph::Globe,
                TINT_BLUE,
                "Global",
                "Used in every app",
                default_selected,
                &[],
                &[],
                &self.application_catalog,
            )
            .id("default-dictation-mode")
            .on_click(cx.listener(|this, _, window, cx| {
                this.select_mode(ModeSelection::Default, window, cx);
            }))
            .into_any_element(),
        ];
        rows.extend(
            self.mode_editors
                .modes
                .iter()
                .enumerate()
                .map(|(index, row)| {
                    let name = input_text(&row.inputs.name, cx);
                    let mode = &row.settings;
                    let selected = self.selected_mode == ModeSelection::Custom(index);
                    mode_row(
                        Glyph::Layers,
                        mode_tint(index),
                        name,
                        "Not linked to any app yet",
                        selected,
                        &mode.applications,
                        &mode.browser_hosts,
                        &self.application_catalog,
                    )
                    .id(("dictation-mode", index))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            this.select_mode(ModeSelection::Custom(index), window, cx);
                            this.mode_context_menu = Some(event.position);
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_mode(ModeSelection::Custom(index), window, cx);
                    }))
                    .into_any_element()
                })
                .collect::<Vec<_>>(),
        );
        rows.insert(
            2,
            group_heading("Your modes", Some(custom_count)).into_any_element(),
        );
        if custom_count == 0 {
            rows.push(
                div()
                    .px_2()
                    .py_2()
                    .text_size(px(11.0))
                    .line_height(px(16.0))
                    .text_color(rgb(FAINT))
                    .child("Add a mode to write differently in specific apps or websites.")
                    .into_any_element(),
            );
        }
        let detail = self.render_mode_detail(cx);
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(pane_header_with_subtitle(
                "Modes",
                "Shape how your words are written, app by app.",
                Some(add.into_any_element()),
            ))
            .child(
                pane_body().px_5().pb_5().pt_1().child(
                    pane_content()
                        .flex_row()
                        .gap_4()
                        .child(
                            div()
                                .id("modes-list")
                                .w(px(mode_list_width))
                                .h_full()
                                .flex_none()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .child(
                                    compact_panel()
                                        .id("mode-list-card")
                                        .overflow_y_scroll()
                                        .child(
                                            div().p_2().flex().flex_col().gap_1().children(rows),
                                        ),
                                ),
                        )
                        .child(detail),
                ),
            )
            .into_any_element()
    }

    fn render_mode_context_menu(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let position = self.mode_context_menu?;
        Some(
            div()
                .absolute()
                .left(position.x)
                .top(position.y)
                .w(px(148.0))
                .p_1()
                .rounded(px(12.0))
                .border_1()
                .border_color(rgba(0xffffff1f))
                .bg(rgb(POPOVER))
                .child(
                    div()
                        .id("context-delete-mode")
                        .h(px(30.0))
                        .px_2()
                        .flex()
                        .items_center()
                        .rounded(px(5.0))
                        .text_size(px(11.0))
                        .text_color(rgb(NEGATIVE))
                        .hover(|item| item.bg(tone(SURFACE_HOVER)))
                        .child("Delete mode…")
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                this.mode_delete_armed = true;
                                this.mode_context_menu = None;
                                cx.stop_propagation();
                                cx.notify();
                            }),
                        ),
                )
                .into_any_element(),
        )
    }

    fn render_mode_detail(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let selection = self.selected_mode;
        let is_default = selection == ModeSelection::Default;
        let (name, browser_hosts) = {
            let inputs = self.selected_mode_inputs_mut();
            (
                inputs.name.entity.clone(),
                inputs.browser_hosts.entity.clone(),
            )
        };
        let corrections = self.render_mode_replacements(selection, cx);
        let transformations = self.render_mode_transformations(selection, cx);
        let application_picker =
            (!is_default).then(|| self.render_application_picker(selection, cx));
        let basics = if is_default {
            compact_panel()
                .child(
                    div()
                        .px_4()
                        .py_4()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(glyph_badge(Glyph::Globe, 42.0, TINT_BLUE))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(2.0))
                                .child(
                                    div()
                                        .text_size(px(16.0))
                                        .font_weight(FontWeight::BOLD)
                                        .child("Global"),
                                )
                                .child(div().text_size(px(11.5)).text_color(rgb(MUTED)).child(
                                    "Used in every app unless a more specific mode \
                                             matches.",
                                )),
                        ),
                )
                .into_any_element()
        } else {
            compact_panel()
                .child(
                    div()
                        .px_4()
                        .py_4()
                        .flex()
                        .items_center()
                        .gap_3()
                        .border_b_1()
                        .border_color(tone(LINE))
                        .child(glyph_badge(
                            Glyph::Layers,
                            42.0,
                            match selection {
                                ModeSelection::Custom(index) => mode_tint(index),
                                ModeSelection::Default => TINT_BLUE,
                            },
                        ))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .flex()
                                .flex_col()
                                .gap(px(4.0))
                                .child(
                                    div()
                                        .text_size(px(11.0))
                                        .text_color(rgb(MUTED))
                                        .child("Mode name"),
                                )
                                .child(div().max_w(px(320.0)).child(name)),
                        ),
                )
                .when_some(application_picker, |panel, picker| {
                    panel.child(div().border_b_1().border_color(tone(LINE)).child(picker))
                })
                .child(
                    div()
                        .min_h(px(64.0))
                        .px_3()
                        .py_2()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_4()
                        .child(settings_copy(
                            "Web pages",
                            "Switch to this mode when one of these sites is open in Brave",
                        ))
                        .child(div().max_w(px(300.0)).flex_1().child(browser_hosts)),
                )
                .into_any_element()
        };

        let remove = (!is_default).then(|| {
            let action =
                if self.mode_delete_armed {
                    div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .child(compact_button("Cancel").id("cancel-remove-mode").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.mode_delete_armed = false;
                                cx.notify();
                            }),
                        ))
                        .child(
                            compact_button("Confirm delete")
                                .id("confirm-remove-mode")
                                .border_1()
                                .border_color(rgb(DANGER_BORDER))
                                .bg(rgb(DANGER_SURFACE))
                                .text_color(rgb(NEGATIVE))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    let ModeSelection::Custom(index) = this.selected_mode else {
                                        return;
                                    };
                                    this.delete_mode(index, cx);
                                })),
                        )
                        .into_any_element()
                } else {
                    compact_button("Delete mode")
                        .id("remove-selected-mode")
                        .border_1()
                        .border_color(rgb(DANGER_BORDER))
                        .bg(rgb(DANGER_SURFACE))
                        .text_color(rgb(NEGATIVE))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.mode_delete_armed = true;
                            cx.notify();
                        }))
                        .into_any_element()
                };
            div()
                .pt_2()
                .flex()
                .items_center()
                .justify_end()
                .child(action)
                .into_any_element()
        });

        div()
            .id("mode-detail")
            .flex_1()
            .min_w(px(0.0))
            .h_full()
            .overflow_y_scroll()
            .child(
                div()
                    .w_full()
                    .max_w(px(700.0))
                    .flex()
                    .flex_col()
                    .gap(px(SECTION_GAP))
                    .pb_5()
                    .child(basics)
                    .child(div().h(px(4.0)))
                    .child(group_heading("Text processing", None).px_1())
                    .child(corrections)
                    .child(transformations)
                    .when_some(remove, |detail, remove| {
                        detail.child(div().pt_1().child(remove))
                    }),
            )
            .into_any_element()
    }

    fn render_mode_transformations(
        &self,
        selection: ModeSelection,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = self.mode_settings(selection).transformations.clone();
        let catalog = &self.personal_commands_status.transformations;
        let selected_rows = selected
            .iter()
            .enumerate()
            .map(|(target_index, id)| {
                let name = catalog
                    .iter()
                    .find(|transformation| &transformation.id == id)
                    .map_or_else(|| id.clone(), |transformation| transformation.name.clone());
                let drag = TransformationDrag {
                    selection,
                    id: id.clone(),
                    name: name.clone(),
                    position: Point::default(),
                };
                let dragged_id = id.clone();
                let drop_id = id.clone();
                div()
                    .id(("mode-transformation", target_index))
                    .h(px(36.0))
                    .px_2()
                    .flex()
                    .items_center()
                    .gap_2()
                    .when(target_index + 1 < selected.len(), |row| {
                        row.border_b_1().border_color(tone(LINE))
                    })
                    .can_drop(move |value, _, _| {
                        value
                            .downcast_ref::<TransformationDrag>()
                            .is_some_and(|drag| drag.selection == selection && drag.id != drop_id)
                    })
                    .on_drop(cx.listener(move |this, drag: &TransformationDrag, _, cx| {
                        if drag.selection != selection {
                            return;
                        }
                        this.update_mode_settings(selection, cx, |mode| {
                            reorder_transformation(
                                &mut mode.transformations,
                                &drag.id,
                                target_index,
                            );
                        });
                    }))
                    .child(
                        div()
                            .id(("drag-mode-transformation", target_index))
                            .w(px(20.0))
                            .h_full()
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .text_size(px(12.0))
                            .text_color(rgb(FAINT))
                            .child("⠿")
                            .on_drag(drag, |drag, position, _, cx| {
                                cx.new(|_| drag.clone().at(position))
                            }),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .overflow_x_hidden()
                            .text_size(px(11.0))
                            .text_color(rgb(TEXT_SOFT))
                            .truncate()
                            .child(name),
                    )
                    .child(
                        compact_button("×")
                            .id(("remove-mode-transformation", target_index))
                            .size(px(28.0))
                            .px_0()
                            .justify_center()
                            .text_size(px(14.0))
                            .text_color(rgb(MUTED))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.update_mode_settings(selection, cx, |mode| {
                                    mode.transformations
                                        .retain(|candidate| candidate != &dragged_id);
                                });
                            })),
                    )
                    .into_any_element()
            })
            .collect::<Vec<_>>();

        let available_rows = if self.transformation_picker_open {
            catalog
                .iter()
                .enumerate()
                .filter(|(_, transformation)| !selected.contains(&transformation.id))
                .map(|(index, transformation)| {
                    let id = transformation.id.clone();
                    let name = transformation.name.clone();
                    let description = transformation.description.clone();
                    div()
                        .id(("available-mode-transformation", index))
                        .min_h(px(40.0))
                        .px_3()
                        .py_2()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .border_t_1()
                        .border_color(tone(LINE))
                        .hover(|row| row.bg(tone(SURFACE_HOVER)))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .child(
                                    div()
                                        .text_size(px(11.0))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(rgb(TEXT_SOFT))
                                        .child(name),
                                )
                                .when_some(description, |copy, description| {
                                    copy.child(
                                        div()
                                            .pt_1()
                                            .text_size(px(10.0))
                                            .text_color(rgb(FAINT))
                                            .child(description),
                                    )
                                }),
                        )
                        .child(
                            canvas_button()
                                .h(px(24.0))
                                .px_2()
                                .text_size(px(10.0))
                                .child("Add"),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.update_mode_settings(selection, cx, |mode| {
                                mode.transformations.push(id.clone());
                            });
                        }))
                        .into_any_element()
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let available_empty = available_rows.is_empty();
        let catalog_empty = catalog.is_empty();
        let add = (!catalog_empty).then(|| {
            let button = if self.transformation_picker_open {
                compact_button("Done")
                    .h(px(28.0))
                    .mr(px(-7.0))
                    .text_size(px(10.0))
            } else {
                compact_header_plus_button()
            };
            button
                .id("toggle-transformation-picker")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.transformation_picker_open = !this.transformation_picker_open;
                    cx.notify();
                }))
                .into_any_element()
        });

        compact_panel()
            .child(
                compact_panel_header(
                    panel_title(
                        Glyph::Sparkles,
                        TINT_ORANGE,
                        "Transformations",
                        "Run your own text transformations, in order.",
                    ),
                    add,
                )
                .when(
                    selected.is_empty() && !self.transformation_picker_open,
                    |header| header.border_b_0(),
                ),
            )
            .children(selected_rows)
            .when(self.transformation_picker_open && !catalog_empty, |panel| {
                panel
                    .when(available_empty, |list| {
                        list.child(
                            div()
                                .border_t_1()
                                .border_color(tone(LINE))
                                .px_3()
                                .py_3()
                                .text_size(px(10.0))
                                .text_color(rgb(MUTED))
                                .child("Every registered transformation is already included."),
                        )
                    })
                    .children(available_rows)
            })
            .into_any_element()
    }

    fn render_application_picker(
        &self,
        selection: ModeSelection,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selected = self.mode_settings(selection).applications.clone();
        let selected_rows = selected.iter().enumerate().map(|(index, name)| {
            let application = self.application_named(name);
            let name = name.clone();
            div()
                .id(("selected-application", index))
                .h(px(30.0))
                .pl_1()
                .pr_1()
                .flex()
                .items_center()
                .gap_1()
                .rounded_sm()
                .bg(tone(SURFACE_SELECTED))
                .border_1()
                .border_color(tone(LINE))
                .child(application_icon(application, Some(name.as_str()), 20.0))
                .child(
                    div()
                        .text_size(px(10.0))
                        .text_color(rgb(TEXT_SOFT))
                        .child(name.clone()),
                )
                .child(
                    div()
                        .id(("remove-selected-application", index))
                        .size(px(18.0))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded_sm()
                        .text_size(px(14.0))
                        .text_color(rgb(FAINT))
                        .hover(|button| button.bg(tone(SURFACE_HOVER)).text_color(rgb(TEXT_SOFT)))
                        .child("×")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.remove_mode_application(selection, &name, cx);
                        })),
                )
        });
        let query = self
            .application_search
            .entity
            .read(cx)
            .text()
            .trim()
            .to_lowercase();
        let available = if self.application_picker_open {
            self.application_choices(selection, cx)
        } else {
            Vec::new()
        };
        let no_matches = available.is_empty();
        let available_rows =
            available
                .into_iter()
                .cloned()
                .enumerate()
                .map(|(index, application)| {
                    let name = application.name.clone();
                    let location = application
                        .path
                        .parent()
                        .map_or_else(String::new, |path| path.display().to_string());
                    div()
                        .id(("available-application", index))
                        .w_full()
                        .px_2()
                        .py_2()
                        .flex()
                        .items_center()
                        .gap_3()
                        .border_b_1()
                        .border_color(tone(LINE))
                        .when(index == self.application_picker_highlight, |row| {
                            row.bg(tone(SURFACE_SELECTED))
                        })
                        .hover(|row| row.bg(tone(SURFACE_HOVER)))
                        .child(application_icon(Some(&application), Some(&name), 30.0))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.0))
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .text_size(px(12.0))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(rgb(TEXT_SOFT))
                                        .child(name.clone()),
                                )
                                .child(
                                    div()
                                        .text_size(px(10.0))
                                        .text_color(rgb(FAINT))
                                        .overflow_hidden()
                                        .child(location),
                                ),
                        )
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                this.add_mode_application(selection, name.clone(), cx);
                            }),
                        )
                });
        let picker_status = match &self.application_catalog {
            ApplicationCatalogState::Loading => Some("Finding applications on this Mac...".into()),
            ApplicationCatalogState::Loaded(applications) if applications.is_empty() => {
                Some("No applications found.".into())
            }
            ApplicationCatalogState::Loaded(_) if no_matches && !query.is_empty() => {
                Some(format!("No applications match \"{query}\"."))
            }
            _ => None,
        };
        let tray = self.application_picker_open.then(|| {
            div()
                .mt_2()
                .p_2()
                .rounded_sm()
                .border_1()
                .border_color(tone(LINE))
                .bg(tone(SURFACE))
                .child(self.application_search.entity.clone())
                .when_some(picker_status, |tray, status| {
                    tray.child(
                        div()
                            .px_2()
                            .py_3()
                            .text_size(px(11.0))
                            .text_color(rgb(MUTED))
                            .child(status),
                    )
                })
                .child(
                    div()
                        .id("application-picker-results")
                        .max_h(px(300.0))
                        .overflow_y_scroll()
                        .children(available_rows),
                )
                .child(
                    div()
                        .pt_2()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .id("choose-application-bundle")
                                .h(px(28.0))
                                .px_2()
                                .flex()
                                .items_center()
                                .rounded_sm()
                                .text_size(px(11.0))
                                .text_color(rgb(TEXT_SOFT))
                                .hover(|button| button.bg(tone(SURFACE_HOVER)))
                                .child("Choose from Finder...")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.choose_application(selection, cx);
                                })),
                        )
                        .child(
                            div()
                                .id("close-application-picker")
                                .h(px(28.0))
                                .px_2()
                                .flex()
                                .items_center()
                                .rounded_sm()
                                .text_size(px(11.0))
                                .text_color(rgb(MUTED))
                                .hover(|button| button.bg(tone(SURFACE_HOVER)))
                                .child("Done")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.close_application_picker(cx);
                                    window.blur();
                                    cx.notify();
                                })),
                        ),
                )
        });
        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .min_h(px(64.0))
                    .px_3()
                    .py_2()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_4()
                    .child(settings_copy(
                        "Applications",
                        "Switch when an app is frontmost",
                    ))
                    .child(
                        div()
                            .max_w(px(300.0))
                            .min_h(px(CONTROL_HEIGHT))
                            .flex_1()
                            .p_1()
                            .flex()
                            .flex_wrap()
                            .items_center()
                            .justify_end()
                            .gap_1()
                            .children(selected_rows)
                            .child(
                                compact_plus_button()
                                    .id("open-application-picker")
                                    .size(px(30.0))
                                    .border_1()
                                    .border_color(tone(LINE))
                                    .bg(tone(CANVAS))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.application_picker_highlight = 0;
                                        this.application_picker_open = true;
                                        this.application_picker_error = None;
                                        this.ensure_application_catalog_load();
                                        this.application_search
                                            .entity
                                            .update(cx, |input, cx| input.set_text("", cx));
                                        this.application_search
                                            .entity
                                            .focus_handle(cx)
                                            .focus(window);
                                        cx.notify();
                                    })),
                            ),
                    ),
            )
            .when_some(self.application_picker_error.clone(), |picker, error| {
                picker.child(
                    div()
                        .text_size(px(10.0))
                        .text_color(rgb(NEGATIVE))
                        .child(error),
                )
            })
            .when_some(tray, |picker, tray| picker.child(tray))
            .into_any_element()
    }

    fn application_named(&self, name: &str) -> Option<&InstalledApplication> {
        self.application_catalog.application_named(name)
    }

    fn application_choice_names(&self, cx: &App) -> Vec<String> {
        self.application_choices(self.selected_mode, cx)
            .into_iter()
            .map(|application| application.name.clone())
            .collect()
    }

    fn application_choices<'a>(
        &'a self,
        selection: ModeSelection,
        cx: &App,
    ) -> Vec<&'a InstalledApplication> {
        let ApplicationCatalogState::Loaded(applications) = &self.application_catalog else {
            return Vec::new();
        };
        let selected = &self.mode_settings(selection).applications;
        let query = self
            .application_search
            .entity
            .read(cx)
            .text()
            .trim()
            .to_lowercase();
        applications
            .iter()
            .filter(|application| {
                !selected.contains(&application.name)
                    && (query.is_empty()
                        || application.name.to_lowercase().contains(&query)
                        || application
                            .bundle_id
                            .as_deref()
                            .is_some_and(|bundle| bundle.to_lowercase().contains(&query)))
            })
            .take(10)
            .collect()
    }

    fn add_mode_application(
        &mut self,
        selection: ModeSelection,
        name: String,
        cx: &mut Context<Self>,
    ) {
        if let Some(conflict) = self
            .mode_editors
            .modes
            .iter()
            .enumerate()
            .find(|(index, row)| {
                selection != ModeSelection::Custom(*index)
                    && row.settings.applications.contains(&name)
            })
            .map(|(_, row)| row.settings.name.clone())
        {
            self.application_picker_error = Some(format!(
                "{name} already activates {conflict}. Remove it there before reassigning it."
            ));
            cx.notify();
            return;
        }
        if !self.mode_settings(selection).applications.contains(&name)
            && self.update_mode_settings(selection, cx, |mode| {
                mode.applications.push(name);
                mode.applications.sort_by_key(|name| name.to_lowercase());
            })
        {
            self.application_picker_highlight = 0;
            self.application_search
                .entity
                .update(cx, |input, cx| input.set_text("", cx));
            self.application_picker_error = None;
        }
    }

    fn remove_mode_application(
        &mut self,
        selection: ModeSelection,
        name: &str,
        cx: &mut Context<Self>,
    ) {
        self.update_mode_settings(selection, cx, |mode| {
            mode.applications.retain(|application| application != name);
        });
    }

    fn choose_application(&mut self, selection: ModeSelection, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Add Application".into()),
        });
        cx.spawn(async move |window, cx| {
            let path = match receiver
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result.map_err(|error| error.to_string()))
            {
                Ok(paths) => paths.and_then(|paths| paths.into_iter().next()),
                Err(error) => {
                    let _ = window.update(cx, |window, cx| {
                        window.application_picker_error = Some(error);
                        cx.notify();
                    });
                    return;
                }
            };
            let Some(path) = path else {
                return;
            };
            let result = cx
                .background_executor()
                .spawn(async move { crate::application_catalog::metadata(&path) })
                .await;
            let _ = window.update(cx, |window, cx| match result {
                Ok(application) => {
                    let name = application.name.clone();
                    match &mut window.application_catalog {
                        ApplicationCatalogState::Loading => {
                            window.application_catalog =
                                ApplicationCatalogState::Loaded(vec![application]);
                        }
                        ApplicationCatalogState::Loaded(applications) => {
                            crate::application_catalog::insert(applications, application);
                        }
                    }
                    window.application_picker_error = None;
                    window.add_mode_application(selection, name, cx);
                }
                Err(error) => {
                    window.application_picker_error = Some(error.to_string());
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn selected_mode_inputs_mut(&mut self) -> &mut ModeInputs {
        self.mode_inputs_mut(self.selected_mode)
    }

    fn select_mode(
        &mut self,
        selection: ModeSelection,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selected_mode = selection;
        self.close_application_picker(cx);
        self.application_picker_error = None;
        self.transformation_picker_open = false;
        self.mode_delete_armed = false;
        self.mode_context_menu = None;
        window.blur();
        cx.notify();
    }

    fn close_application_picker(&mut self, cx: &mut Context<Self>) {
        self.application_picker_open = false;
        self.application_picker_highlight = 0;
        self.application_search
            .entity
            .update(cx, |input, cx| input.set_text("", cx));
    }

    fn mode_inputs_mut(&mut self, selection: ModeSelection) -> &mut ModeInputs {
        &mut self.mode_editor_mut(selection).inputs
    }

    fn mode_inputs_for(&self, selection: ModeSelection) -> &ModeInputs {
        &self.mode_editor_for(selection).inputs
    }

    fn mode_settings(&self, selection: ModeSelection) -> &DictationMode {
        &self.mode_editor_for(selection).settings
    }

    fn mode_editor_for(&self, selection: ModeSelection) -> &ModeEditor {
        match selection {
            ModeSelection::Default => &self.mode_editors.default_mode,
            ModeSelection::Custom(index) => &self.mode_editors.modes[index],
        }
    }

    fn mode_editor_mut(&mut self, selection: ModeSelection) -> &mut ModeEditor {
        match selection {
            ModeSelection::Default => &mut self.mode_editors.default_mode,
            ModeSelection::Custom(index) => &mut self.mode_editors.modes[index],
        }
    }

    fn meeting_notes_input(cx: &mut Context<Self>) -> ProcessingInput {
        let entity = cx.new(|cx| {
            TextInput::multiline(
                cx,
                "Jot down decisions, follow-ups, anything you want to remember…",
                "",
            )
        });
        let changed = cx.subscribe(&entity, |this, _, _: &TextChanged, cx| {
            let Some(id) = this.meeting_notes_id.clone() else {
                return;
            };
            let text = this.meeting_notes.entity.read(cx).text().to_string();
            if let Err(error) = meeting::write_notes(&id, &text) {
                tracing::warn!(%error, meeting = id, "could not save meeting notes");
            }
        });
        ProcessingInput {
            entity,
            _subscriptions: vec![changed],
        }
    }

    /// Point the notes editor at the selected meeting.
    fn sync_meeting_notes(&mut self, cx: &mut Context<Self>) {
        if self.meeting_notes_id == self.selected_meeting {
            return;
        }
        self.meeting_notes_id = None;
        let text = self
            .selected_meeting
            .as_deref()
            .map(meeting::read_notes)
            .unwrap_or_default();
        self.meeting_notes
            .entity
            .update(cx, |input, cx| input.set_text(text, cx));
        self.meeting_notes_id = self.selected_meeting.clone();
    }

    fn set_meeting_detection(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if self.update_settings(cx, |settings| settings.notetaker.detect_meetings = enabled) {
            self.meeting_detect_toggle.set_enabled(enabled);
        }
    }

    fn set_meeting_calendar(&mut self, enabled: bool, cx: &mut Context<Self>) {
        if enabled && crate::calendar::access() == crate::calendar::CalendarAccess::NotAsked {
            crate::calendar::request_access(|_| {});
        }
        if self.update_settings(cx, |settings| settings.notetaker.use_calendar = enabled) {
            self.meeting_calendar_toggle.set_enabled(enabled);
        }
    }

    fn download_speaker_model(&mut self) {
        if self.speaker_download.is_some() || crate::speakers::is_installed() {
            return;
        }
        let bytes = Arc::new(AtomicU64::new(0));
        let progress = bytes.clone();
        let (sender, receiver) = sync_channel(1);
        thread::spawn(move || {
            let result = crate::speakers::download(&progress).map_err(|error| error.to_string());
            let _ = sender.send(result);
        });
        self.speaker_download_error = None;
        self.speaker_download = Some((bytes, receiver));
    }

    fn poll_speaker_download(&mut self) -> bool {
        let Some((_, done)) = &self.speaker_download else {
            return false;
        };
        match done.try_recv() {
            Ok(result) => {
                self.speaker_download = None;
                self.speaker_download_error = result.err();
                true
            }
            Err(std::sync::mpsc::TryRecvError::Empty) => true,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.speaker_download = None;
                true
            }
        }
    }

    fn edit_speaker(&mut self, speaker: u32, current: Option<String>, cx: &mut Context<Self>) {
        self.meeting_speaker_editing = Some(speaker);
        self.meeting_speaker_input.entity.update(cx, |input, cx| {
            input.set_text(current.unwrap_or_default(), cx)
        });
        cx.notify();
    }

    fn save_speaker_name(&mut self, name: Option<String>, cx: &mut Context<Self>) {
        let (Some(id), Some(speaker)) =
            (self.selected_meeting.clone(), self.meeting_speaker_editing)
        else {
            return;
        };
        let name = name.unwrap_or_else(|| {
            self.meeting_speaker_input
                .entity
                .read(cx)
                .text()
                .trim()
                .to_string()
        });
        let saved = meeting::root()
            .and_then(|root| crate::speakers::name_speaker(&root.join(&id), speaker, &name));
        if let Err(error) = saved {
            tracing::warn!(%error, meeting = id, "could not name meeting speaker");
        }
        self.meeting_speaker_editing = None;
        self.reload_meetings();
        self.meeting_transcript_id = None;
        self.load_selected_meeting();
        cx.notify();
    }

    fn delete_meeting(&mut self, id: String, cx: &mut Context<Self>) {
        self.meeting_delete_armed = None;
        match meeting::trash(&id) {
            Ok(()) => {
                if self.selected_meeting.as_deref() == Some(id.as_str()) {
                    self.selected_meeting = None;
                    self.meeting_notes_id = None;
                }
                self.reload_meetings();
                if self.selected_meeting.is_none() {
                    self.selected_meeting = self.meetings.first().map(|meeting| meeting.id.clone());
                }
                self.load_selected_meeting();
            }
            Err(error) => self.meeting_runtime_error = Some(error.to_string()),
        }
        cx.notify();
    }

    fn set_meeting_retention(&mut self, retention: MeetingAudioRetention, cx: &mut Context<Self>) {
        if self.update_settings(cx, |settings| {
            settings.notetaker.audio_retention = retention
        }) && let Err(error) = meeting::prune_audio(retention)
        {
            tracing::warn!(%error, "could not apply meeting audio retention");
        }
    }

    fn start_meeting_notes(&mut self, cx: &mut Context<Self>) {
        self.meeting_refresh_started_ms = Some(now_ms());
        if let Err(error) = self.meeting_requests.try_send(MeetingRequest::Start) {
            self.meeting_refresh_started_ms = None;
            log_meeting_request_error(error);
        }
        cx.notify();
    }

    fn render_meetings(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        if self.screen_capture_allowed.is_none() && !self.preview {
            self.screen_capture_allowed = Some(screen_capture_allowed());
        }
        self.sync_meeting_notes(cx);
        let active_status = meeting::active(&self.meetings).map(|meeting| meeting.status);
        let busy_chip = |label: &'static str| {
            icon_chip(glyph(Glyph::Clock, 12.0, 0x98989d), label)
                .h(px(30.0))
                .into_any_element()
        };
        let action = match active_status {
            _ if self.meeting_stop_pending => busy_chip("Saving notes…"),
            Some(MeetingStatus::Transcribing) => busy_chip("Saving notes…"),
            Some(MeetingStatus::Starting) => busy_chip("Starting…"),
            Some(MeetingStatus::Recording) => glyph_button(Glyph::Power, "Stop & save")
                .id("stop-meeting")
                .border_color(rgb(DANGER_BORDER))
                .bg(rgb(DANGER_SURFACE))
                .text_color(rgb(NEGATIVE))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.meeting_stop_pending = true;
                    if let Err(error) = this.meeting_requests.try_send(MeetingRequest::Stop) {
                        this.meeting_stop_pending = false;
                        log_meeting_request_error(error);
                    }
                    cx.notify();
                }))
                .into_any_element(),
            _ if self.meeting_refresh_started_ms.is_some() => busy_chip("Starting…"),
            _ => glyph_button(Glyph::Mic, "Take notes")
                .id("start-meeting")
                .border_color(rgb(ACCENT))
                .bg(rgb(ACCENT))
                .text_color(rgb(0xffffff))
                .hover(|button| button.bg(rgb(0x2b95ff)).text_color(rgb(0xffffff)))
                .on_click(cx.listener(|this, _, _, cx| this.start_meeting_notes(cx)))
                .into_any_element(),
        };

        let mut rows: Vec<AnyElement> = Vec::new();
        let mut current_day: Option<String> = None;
        for (index, meeting) in self.meetings.iter().enumerate() {
            let day = history_day_label(meeting.created_at_ms);
            if current_day.as_deref() != Some(day.as_str()) {
                rows.push(group_heading(day.clone(), None).into_any_element());
                current_day = Some(day);
            }
            let id = meeting.id.clone();
            let selected = self.selected_meeting.as_deref() == Some(id.as_str());
            let live = meeting.status.is_active();
            let (icon, tint) = match meeting.status {
                MeetingStatus::Starting | MeetingStatus::Recording => (Glyph::Mic, TINT_PINK),
                MeetingStatus::Transcribing => (Glyph::Clock, TINT_ORANGE),
                MeetingStatus::Failed | MeetingStatus::Interrupted => (Glyph::Text, TINT_GRAY),
                MeetingStatus::Complete => (Glyph::Text, TINT_INDIGO),
            };
            let mut meta = format!(
                "{} · {}",
                clock_time(meeting.created_at_ms),
                meeting_length_label(meeting_duration(meeting))
            );
            if let Some(platform) = meeting
                .details
                .as_ref()
                .and_then(|details| details.platform.as_deref())
            {
                meta.push_str(" · ");
                meta.push_str(platform);
            }
            if meeting.status != MeetingStatus::Complete {
                meta.push_str(" · ");
                meta.push_str(meeting_status(meeting.status));
            }
            rows.push(
                list_row(selected)
                    .id(("meeting", index))
                    .flex_row()
                    .items_center()
                    .gap(px(10.0))
                    .child(glyph_badge(icon, 30.0, tint))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .flex()
                            .flex_col()
                            .gap(px(3.0))
                            .child(
                                div()
                                    .w_full()
                                    .overflow_x_hidden()
                                    .truncate()
                                    .text_size(px(12.5))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(TEXT))
                                    .child(meeting.title.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(10.5))
                                    .text_color(rgb(if live {
                                        NEGATIVE
                                    } else if selected {
                                        MUTED
                                    } else {
                                        FAINT
                                    }))
                                    .child(meta),
                            ),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected_meeting = Some(id.clone());
                        this.meeting_tab = MeetingTab::Transcript;
                        this.load_selected_meeting();
                        cx.notify();
                    }))
                    .into_any_element(),
            );
        }

        let detect_position = self.meeting_detect_toggle.render_position(window);
        let calendar_position = self.meeting_calendar_toggle.render_position(window);
        let calendar_denied = self.settings.notetaker.use_calendar
            && crate::calendar::access() == crate::calendar::CalendarAccess::Denied;
        let speaker_control = if crate::speakers::is_installed() {
            icon_chip(glyph(Glyph::Sparkles, 12.0, 0x30d158), "Ready").into_any_element()
        } else if let Some((bytes, _)) = &self.speaker_download {
            let percent = bytes.load(std::sync::atomic::Ordering::Relaxed) as f64
                / crate::speakers::MODEL_BYTES as f64
                * 100.0;
            chip(format!("{percent:.0}%")).into_any_element()
        } else {
            compact_button("Download 28 MB")
                .id("speaker-model-download")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.download_speaker_model();
                    cx.notify();
                }))
                .into_any_element()
        };
        let retention = self.settings.notetaker.audio_retention;
        let retention_control = div()
            .relative()
            .child(
                chip(format!("{} ▾", retention.label()))
                    .id("meeting-retention")
                    .cursor_pointer()
                    .hover(|chip| chip.bg(rgb(0x34343a)))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.meeting_retention_open = true;
                        cx.notify();
                    })),
            )
            .when(self.meeting_retention_open, |control| {
                control.child(deferred(
                    div()
                        .id("meeting-retention-menu")
                        .absolute()
                        .top(px(30.0))
                        .right_0()
                        .w(px(150.0))
                        .p_1()
                        .rounded(px(12.0))
                        .border_1()
                        .border_color(rgba(0xffffff1f))
                        .bg(rgb(POPOVER))
                        .shadow_lg()
                        .occlude()
                        .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                            this.meeting_retention_open = false;
                            cx.notify();
                        }))
                        .children(MeetingAudioRetention::ALL.into_iter().enumerate().map(
                            |(index, choice)| {
                                compact_button(choice.label())
                                    .id(("meeting-retention-choice", index))
                                    .w_full()
                                    .when(choice == retention, |item| {
                                        item.bg(tone(SURFACE_SELECTED))
                                    })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        cx.stop_propagation();
                                        this.meeting_retention_open = false;
                                        this.set_meeting_retention(choice, cx);
                                        cx.notify();
                                    }))
                            },
                        )),
                ))
            });
        let option_row = |title: &'static str, detail: &'static str| {
            div()
                .flex_1()
                .min_w(px(0.0))
                .flex()
                .flex_col()
                .gap(px(2.0))
                .child(
                    div()
                        .text_size(px(12.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(title),
                )
                .child(
                    div()
                        .text_size(px(10.5))
                        .text_color(rgb(MUTED))
                        .child(detail),
                )
        };
        let options = compact_panel()
            .flex_none()
            .child(
                div()
                    .id("meeting-detect-setting")
                    .px_3()
                    .py(px(10.0))
                    .flex()
                    .items_center()
                    .gap_3()
                    .border_b_1()
                    .border_color(tone(LINE))
                    .child(option_row(
                        "Detect meetings",
                        "Ask to take notes when a call starts",
                    ))
                    .child(toggle(detect_position))
                    .on_click(cx.listener(|this, _, _, cx| {
                        let enabled = !this.settings.notetaker.detect_meetings;
                        this.set_meeting_detection(enabled, cx);
                    })),
            )
            .child(
                div()
                    .px_3()
                    .py(px(10.0))
                    .flex()
                    .items_center()
                    .gap_3()
                    .border_b_1()
                    .border_color(tone(LINE))
                    .child(option_row(
                        "Keep audio",
                        "Transcripts and notes are always kept",
                    ))
                    .child(retention_control),
            )
            .child(
                div()
                    .id("meeting-calendar-setting")
                    .px_3()
                    .py(px(10.0))
                    .flex()
                    .items_center()
                    .gap_3()
                    .border_b_1()
                    .border_color(tone(LINE))
                    .child(option_row(
                        "Use Calendar",
                        if calendar_denied {
                            "Allow Hello in Privacy & Security › Calendars"
                        } else {
                            "Titles and attendees from your events"
                        },
                    ))
                    .child(toggle(calendar_position))
                    .on_click(cx.listener(|this, _, _, cx| {
                        let enabled = !this.settings.notetaker.use_calendar;
                        this.set_meeting_calendar(enabled, cx);
                    })),
            )
            .child(
                div()
                    .px_3()
                    .py(px(10.0))
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(option_row(
                        "Tell speakers apart",
                        if self.speaker_download_error.is_some() {
                            "Download failed. Try again."
                        } else {
                            "Open-source voice model, runs on this Mac"
                        },
                    ))
                    .child(speaker_control),
            );
        let list = div()
            .id("meetings-list")
            .flex_1()
            .min_h(px(0.0))
            .overflow_y_scroll()
            .rounded(px(PANEL_RADIUS))
            .border_1()
            .border_color(tone(LINE))
            .bg(tone(SURFACE))
            .when(
                self.meetings.is_empty() && self.meetings_error.is_none(),
                |list| {
                    list.child(
                        div()
                            .p_5()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap_2()
                            .child(glyph_badge(Glyph::Text, 36.0, TINT_INDIGO))
                            .child(
                                div()
                                    .text_size(px(12.5))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("No meeting notes yet"),
                            )
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .line_height(px(16.0))
                                    .text_color(rgb(MUTED))
                                    .child(
                                        "Join a call and Hello will offer to take notes, or press \
                                     Take notes.",
                                    ),
                            ),
                    )
                },
            )
            .when_some(self.meetings_error.clone(), |list, error| {
                list.child(error_message("Meeting notes could not be loaded.", error))
            })
            .child(
                div()
                    .w(px(PANE_LIST_WIDTH - 2.0))
                    .p_2()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .children(rows),
            );

        let permission_banner = (self.screen_capture_allowed == Some(false)).then(|| {
            compact_panel()
                .flex_none()
                .px_4()
                .py_3()
                .flex()
                .items_center()
                .gap_3()
                .bg(tinted(TINT_ORANGE, 0.08))
                .border_color(tinted(TINT_ORANGE, 0.3))
                .child(glyph_badge(Glyph::Speed, 30.0, TINT_ORANGE))
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(
                            div()
                                .text_size(px(12.5))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("Allow Hello to hear your calls"),
                        )
                        .child(div().text_size(px(11.0)).text_color(rgb(MUTED)).child(
                            "Turn on Hello under Screen & System Audio Recording so it \
                                     can capture the other people in a call.",
                        )),
                )
                .child(
                    header_button("Open Settings")
                        .id("meeting-permission")
                        .on_click(cx.listener(|this, _, _, cx| {
                            request_screen_capture_access();
                            this.screen_capture_allowed = None;
                            cx.notify();
                        })),
                )
        });

        div()
            .size_full()
            .flex()
            .flex_col()
            .child(pane_header_with_subtitle(
                "Notetaker",
                "Meeting notes, transcribed privately on this Mac.",
                Some(action),
            ))
            .child(
                pane_body().px_5().pb_5().pt_1().child(
                    pane_content()
                        .h_full()
                        .gap_3()
                        .when_some(permission_banner, |column, banner| column.child(banner))
                        .when_some(self.meeting_runtime_error.clone(), |column, error| {
                            column.child(
                                compact_panel()
                                    .flex_none()
                                    .px_4()
                                    .py_3()
                                    .text_size(px(11.5))
                                    .text_color(rgb(NEGATIVE))
                                    .child(error),
                            )
                        })
                        .child(
                            div()
                                .id("meetings-workspace")
                                .flex_1()
                                .min_h(px(0.0))
                                .flex()
                                .flex_row()
                                .gap_4()
                                .child(
                                    div()
                                        .w(px(PANE_LIST_WIDTH))
                                        .h_full()
                                        .flex_none()
                                        .flex()
                                        .flex_col()
                                        .gap_3()
                                        .child(options)
                                        .child(list),
                                )
                                .child(
                                    compact_panel()
                                        .flex_1()
                                        .min_w(px(0.0))
                                        .h_full()
                                        .flex()
                                        .flex_col()
                                        .child(self.render_meeting_detail(cx)),
                                ),
                        ),
                ),
            )
            .into_any_element()
    }

    fn render_meeting_detail(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(meeting) = self
            .selected_meeting
            .as_deref()
            .and_then(|id| self.meetings.iter().find(|meeting| meeting.id == id))
            .cloned()
        else {
            return detail_placeholder("Select a meeting note.");
        };
        let id = meeting.id.clone();
        let recording = meeting.status == MeetingStatus::Recording && !self.meeting_stop_pending;
        let finalizing = meeting.status == MeetingStatus::Transcribing
            || (meeting.status == MeetingStatus::Recording && self.meeting_stop_pending);
        let reveal_id = id.clone();
        let reveal = glyph_button(Glyph::Apps, "Files")
            .id("reveal-meeting-files")
            .on_click(cx.listener(move |_, _, _, _| match meeting::root() {
                Ok(root) => open_path(&root.join(&reveal_id)),
                Err(error) => tracing::error!(%error, "could not locate meeting files"),
            }));
        let copy_id = id.clone();
        let copy = glyph_button(
            Glyph::Copy,
            if self.meeting_copied {
                "Copied"
            } else {
                "Copy"
            },
        )
        .id("copy-meeting-transcript")
        .on_click(cx.listener(move |this, _, _, cx| {
            let copied = meeting::show(&copy_id).and_then(|markdown| {
                arboard::Clipboard::new()
                    .and_then(|mut clipboard| clipboard.set_text(markdown))
                    .map_err(Into::into)
            });
            match copied {
                Ok(()) => this.meeting_copied = true,
                Err(error) => tracing::warn!(%error, "could not copy meeting transcript"),
            }
            cx.notify();
        }));
        let armed = self.meeting_delete_armed.as_deref() == Some(id.as_str());
        let delete_id = id.clone();
        let arm_id = id.clone();
        let delete = if meeting.status.is_active() {
            None
        } else if armed {
            Some(
                div()
                    .flex()
                    .gap_2()
                    .child(
                        compact_button("Cancel")
                            .id("cancel-delete-meeting")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.meeting_delete_armed = None;
                                cx.notify();
                            })),
                    )
                    .child(
                        glyph_button(Glyph::Trash, "Move to Trash")
                            .id("confirm-delete-meeting")
                            .border_color(rgb(DANGER_BORDER))
                            .bg(rgb(DANGER_SURFACE))
                            .text_color(rgb(NEGATIVE))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.delete_meeting(delete_id.clone(), cx);
                            })),
                    )
                    .into_any_element(),
            )
        } else {
            Some(
                glyph_button(Glyph::Trash, "Delete")
                    .id("delete-meeting")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.meeting_delete_armed = Some(arm_id.clone());
                        cx.notify();
                    }))
                    .into_any_element(),
            )
        };
        let details = meeting.details.clone().unwrap_or_default();
        let app_label = details
            .app
            .clone()
            .map(|app| match &details.browser_profile {
                Some(profile) => format!("{app} · {profile} profile"),
                None => app,
            });
        let when = format!(
            "{} · {}",
            history_day_label(meeting.created_at_ms),
            clock_time(meeting.created_at_ms)
        );
        let tab = |label: &'static str, which: MeetingTab, current: MeetingTab| {
            let selected = which == current;
            div()
                .id(SharedString::from(format!("meeting-tab-{label}")))
                .h(px(38.0))
                .flex()
                .flex_col()
                .justify_between()
                .cursor_pointer()
                .child(
                    div()
                        .pt(px(10.0))
                        .text_size(px(12.5))
                        .font_weight(if selected {
                            FontWeight::SEMIBOLD
                        } else {
                            FontWeight::NORMAL
                        })
                        .text_color(rgb(if selected { TEXT } else { MUTED }))
                        .child(label),
                )
                .child(
                    div()
                        .h(px(2.0))
                        .rounded_full()
                        .when(selected, |bar| bar.bg(rgb(ACCENT))),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.meeting_tab = which;
                    cx.notify();
                }))
        };
        let current = self.meeting_tab;
        let body = match current {
            MeetingTab::Transcript => self.render_meeting_transcript(&meeting),
            MeetingTab::Overview => self.render_meeting_overview(&meeting, cx),
            MeetingTab::Notes => div()
                .flex_1()
                .min_h(px(0.0))
                .p_5()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .flex_1()
                        .min_h(px(240.0))
                        .child(self.meeting_notes.entity.clone()),
                )
                .child(
                    div()
                        .text_size(px(10.5))
                        .text_color(rgb(FAINT))
                        .child("Saved automatically. Your notes stay on this Mac."),
                )
                .into_any_element(),
            MeetingTab::Summary => div()
                .flex_1()
                .p_6()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_3()
                .child(glyph_badge(Glyph::Sparkles, 44.0, TINT_PURPLE))
                .child(
                    div()
                        .text_size(px(14.0))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("AI summaries are coming next"),
                )
                .child(
                    div()
                        .max_w(px(380.0))
                        .text_size(px(11.5))
                        .line_height(px(18.0))
                        .text_color(rgb(MUTED))
                        .child(
                            "Hello will summarize meetings and answer questions about them \
                             with an open-source model running on this Mac. Nothing is sent \
                             to the cloud.",
                        ),
                )
                .into_any_element(),
        };

        div()
            .flex_1()
            .min_w(px(0.0))
            .h_full()
            .overflow_hidden()
            .flex()
            .flex_col()
            .child(
                div()
                    .px_6()
                    .pt_5()
                    .pb_3()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .items_start()
                            .justify_between()
                            .gap_3()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .text_size(px(20.0))
                                    .font_weight(FontWeight::BOLD)
                                    .child(meeting.title.clone()),
                            )
                            .child(
                                div()
                                    .flex()
                                    .gap_2()
                                    .when(!armed, |buttons| buttons.child(copy).child(reveal))
                                    .when_some(delete, |buttons, delete| buttons.child(delete)),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .gap_2()
                            .when_some(details.platform.clone(), |chips, platform| {
                                chips
                                    .child(icon_chip(glyph(Glyph::Globe, 12.0, 0x98989d), platform))
                            })
                            .when_some(app_label, |chips, app| {
                                chips.child(icon_chip(glyph(Glyph::Apps, 12.0, 0x98989d), app))
                            })
                            .when_some(details.account.clone(), |chips, account| {
                                chips
                                    .child(icon_chip(glyph(Glyph::People, 12.0, 0x98989d), account))
                            })
                            .child(icon_chip(glyph(Glyph::Clock, 12.0, 0x98989d), when))
                            .child(icon_chip(
                                glyph(Glyph::Mic, 12.0, 0x98989d),
                                meeting_length_label(meeting_duration(&meeting)),
                            ))
                            .when(meeting.audio_removed, |chips| {
                                chips.child(chip("Audio removed").text_color(rgb(FAINT)))
                            })
                            .when(
                                matches!(
                                    meeting.status,
                                    MeetingStatus::Failed | MeetingStatus::Interrupted
                                ),
                                |chips| {
                                    chips.child(
                                        chip(meeting_status(meeting.status))
                                            .text_color(rgb(NEGATIVE)),
                                    )
                                },
                            ),
                    )
                    .when(recording, |header| {
                        header.child(
                            div()
                                .px_3()
                                .py_2()
                                .flex()
                                .items_center()
                                .gap_2()
                                .rounded(px(10.0))
                                .bg(rgb(DANGER_SURFACE))
                                .text_size(px(11.5))
                                .text_color(rgb(TEXT_SOFT))
                                .child(div().size(px(8.0)).rounded_full().bg(rgb(NEGATIVE)))
                                .child("Recording. Hello stops by itself when the call ends."),
                        )
                    })
                    .when(finalizing, |header| {
                        header.child(
                            div()
                                .px_3()
                                .py_2()
                                .rounded(px(10.0))
                                .bg(tinted(TINT_ORANGE, 0.12))
                                .text_size(px(11.5))
                                .text_color(rgb(TEXT_SOFT))
                                .child("Writing the full transcript on this Mac…"),
                        )
                    })
                    .when_some(meeting.error.clone(), |header, error| {
                        header.child(
                            div()
                                .text_size(px(11.0))
                                .line_height(px(17.0))
                                .text_color(rgb(NEGATIVE))
                                .child(error),
                        )
                    }),
            )
            .child(
                div()
                    .px_6()
                    .flex()
                    .gap_5()
                    .border_b_1()
                    .border_color(tone(LINE))
                    .child(tab("Transcript", MeetingTab::Transcript, current))
                    .child(tab("Overview", MeetingTab::Overview, current))
                    .child(tab("My notes", MeetingTab::Notes, current))
                    .child(tab("Summary", MeetingTab::Summary, current)),
            )
            .child(body)
            .into_any_element()
    }

    fn render_meeting_overview(&self, meeting: &MeetingManifest, cx: &Context<Self>) -> AnyElement {
        let details = meeting.details.clone().unwrap_or_default();
        let section = |title: &'static str| {
            div()
                .pt_2()
                .pb_1()
                .text_size(px(11.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(MUTED))
                .child(title)
        };
        let row = |label: &'static str, value: String| {
            div()
                .px_4()
                .py(px(9.0))
                .flex()
                .items_start()
                .gap_4()
                .border_b_1()
                .border_color(tone(LINE))
                .child(
                    div()
                        .w(px(120.0))
                        .flex_none()
                        .text_size(px(11.5))
                        .text_color(rgb(MUTED))
                        .child(label),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .text_size(px(12.0))
                        .line_height(px(17.0))
                        .text_color(rgb(TEXT))
                        .child(value),
                )
        };
        let card = || {
            div()
                .rounded(px(12.0))
                .border_1()
                .border_color(tone(LINE))
                .bg(tone(CANVAS))
                .overflow_hidden()
                .flex()
                .flex_col()
        };

        let mut place: Vec<AnyElement> = Vec::new();
        let mut add = |label: &'static str, value: Option<String>| {
            if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
                place.push(row(label, value).into_any_element());
            }
        };
        add("Platform", details.platform.clone());
        add("App", details.app.clone());
        add("Browser profile", details.browser_profile.clone());
        add("Signed in as", details.account.clone());
        add("Meeting code", details.meeting_code.clone());
        add("Call window", details.window_title.clone());
        add(
            "Started",
            Some(format!(
                "{}, {}",
                history_day_label(meeting.created_at_ms),
                clock_time(meeting.created_at_ms)
            )),
        );
        add(
            "Length",
            Some(meeting_length_label(meeting_duration(meeting))),
        );
        let link = details.url.clone().map(|url| {
            let open = url.clone();
            div()
                .id("meeting-link")
                .px_4()
                .py(px(9.0))
                .flex()
                .items_start()
                .gap_4()
                .child(
                    div()
                        .w(px(120.0))
                        .flex_none()
                        .text_size(px(11.5))
                        .text_color(rgb(MUTED))
                        .child("Link"),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .overflow_x_hidden()
                        .truncate()
                        .text_size(px(12.0))
                        .text_color(rgb(ACCENT_SOFT))
                        .cursor_pointer()
                        .child(url),
                )
                .on_click(move |_, _, _| open_path(std::path::Path::new(&open)))
        });

        let calendar = match &details.calendar {
            Some(event) => {
                let mut rows: Vec<AnyElement> = vec![
                    row("Event", event.title.clone()).into_any_element(),
                    row(
                        "Scheduled",
                        format!(
                            "{} – {}",
                            clock_time(event.starts_at_ms),
                            clock_time(event.ends_at_ms)
                        ),
                    )
                    .into_any_element(),
                ];
                if let Some(name) = &event.calendar {
                    rows.push(row("Calendar", name.clone()).into_any_element());
                }
                if let Some(organizer) = &event.organizer {
                    rows.push(row("Organizer", organizer.label()).into_any_element());
                }
                if let Some(location) = &event.location {
                    rows.push(row("Location", location.clone()).into_any_element());
                }
                if !event.attendees.is_empty() {
                    let invited = event
                        .attendees
                        .iter()
                        .map(|person| match &person.response {
                            Some(response) => format!("{} ({response})", person.label()),
                            None => person.label(),
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    rows.push(
                        row("Invited", format!("{} · {invited}", event.attendees.len()))
                            .into_any_element(),
                    );
                }
                card().children(rows).into_any_element()
            }
            None => card()
                .px_4()
                .py_3()
                .text_size(px(11.5))
                .line_height(px(17.0))
                .text_color(rgb(MUTED))
                .child(if self.settings.notetaker.use_calendar {
                    "No calendar event matched this meeting."
                } else {
                    "Turn on Use Calendar (left) to add the event's title, organizer and \
                     invitees to future meetings."
                })
                .into_any_element(),
        };

        // People: you, then everyone Hello told apart by voice.
        let speakers = meeting.speakers.clone().unwrap_or_default();
        let others_heard = matches!(
            &self.meeting_transcript,
            MeetingTranscript::Loaded(lines)
                if lines.iter().any(|line| line.speaker == meeting::MeetingSource::System)
        );
        let others_ms: u64 = speakers
            .speakers
            .iter()
            .map(|speaker| speaker.talk_ms)
            .sum();
        let total_ms = (others_ms + self.meeting_you_talk_ms).max(1);
        let timed = others_ms + self.meeting_you_talk_ms > 0;
        let share = |milliseconds: u64| {
            if timed {
                format!(
                    "Spoke {} · {}% of the talking",
                    meeting_length_label(milliseconds),
                    milliseconds * 100 / total_ms
                )
            } else {
                "Talk time not recorded".to_string()
            }
        };
        let person = |icon: Glyph, tint: u32, name: String, detail: String| {
            div()
                .flex_1()
                .min_w(px(0.0))
                .flex()
                .items_center()
                .gap_3()
                .child(glyph_badge(icon, 30.0, tint))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.0))
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(
                            div()
                                .text_size(px(12.5))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(name),
                        )
                        .child(
                            div()
                                .text_size(px(10.5))
                                .text_color(rgb(MUTED))
                                .child(detail),
                        ),
                )
        };
        let you = details
            .you
            .clone()
            .unwrap_or_else(|| profile_display_name(&self.settings));
        let mut people: Vec<AnyElement> = vec![
            div()
                .px_4()
                .py(px(10.0))
                .flex()
                .border_b_1()
                .border_color(tone(LINE))
                .child(person(
                    Glyph::Mic,
                    TINT_BLUE,
                    format!("{you} (you)"),
                    share(self.meeting_you_talk_ms),
                ))
                .into_any_element(),
        ];
        let suggestions: Vec<String> = details
            .calendar
            .as_ref()
            .map(|event| {
                event
                    .attendees
                    .iter()
                    .filter(|person| !person.is_you)
                    .filter_map(|person| person.name.clone())
                    .filter(|name| {
                        !speakers
                            .speakers
                            .iter()
                            .any(|speaker| speaker.name.as_deref() == Some(name.as_str()))
                    })
                    .take(6)
                    .collect()
            })
            .unwrap_or_default();
        for speaker in &speakers.speakers {
            let (tint, _) = speaker_tint(Some(speaker.id));
            let id = speaker.id;
            let current = speaker.name.clone();
            let editing = self.meeting_speaker_editing == Some(id);
            let mut detail = share(speaker.talk_ms);
            if speaker.recognized {
                detail.push_str(" · recognized by voice");
            }
            let mut entry = div()
                .px_4()
                .py(px(10.0))
                .flex()
                .flex_col()
                .gap_2()
                .border_b_1()
                .border_color(tone(LINE))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(person(Glyph::People, tint, speaker.label(), detail))
                        .when(!editing, |line| {
                            line.child(
                                compact_button(if current.is_some() { "Rename" } else { "Name" })
                                    .id(("name-speaker", id as usize))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.edit_speaker(id, current.clone(), cx);
                                    })),
                            )
                        }),
                );
            if editing {
                entry = entry
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .child(self.meeting_speaker_input.entity.clone()),
                            )
                            .child(compact_button("Cancel").id("cancel-speaker-name").on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.meeting_speaker_editing = None;
                                    cx.notify();
                                }),
                            ))
                            .child(
                                compact_button("Save")
                                    .id("save-speaker-name")
                                    .bg(rgb(ACCENT))
                                    .text_color(rgb(0xffffff))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.save_speaker_name(None, cx);
                                    })),
                            ),
                    )
                    .when(!suggestions.is_empty(), |entry| {
                        entry.child(div().flex().flex_wrap().gap_2().children(
                            suggestions.iter().enumerate().map(|(index, name)| {
                                let chosen = name.clone();
                                chip(name.clone())
                                    .id(("speaker-suggestion", index))
                                    .cursor_pointer()
                                    .hover(|chip| chip.bg(rgb(0x34343a)))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.save_speaker_name(Some(chosen.clone()), cx);
                                    }))
                            }),
                        ))
                    })
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(rgb(FAINT))
                            .child("Hello remembers this voice and names it in later meetings."),
                    );
            }
            people.push(entry.into_any_element());
        }
        if speakers.speakers.is_empty() {
            let note =
                if meeting.status.is_active() || meeting.status == MeetingStatus::Transcribing {
                    "The other people are told apart by voice once the meeting is saved."
                } else if others_heard {
                    "This meeting was saved before Hello could tell voices apart, so everyone \
                 else is shown as Others. New meetings list each person."
                } else if !crate::speakers::is_installed() {
                    "Download the voice model under Tell speakers apart (left). Later meetings \
                 will list each person separately."
                } else {
                    "Nobody else was heard in this meeting."
                };
            people.push(
                div()
                    .px_4()
                    .py_3()
                    .text_size(px(11.5))
                    .line_height(px(17.0))
                    .text_color(rgb(MUTED))
                    .child(note)
                    .into_any_element(),
            );
        }

        div()
            .id("meeting-overview")
            .flex_1()
            .min_h(px(0.0))
            .overflow_y_scroll()
            .px_6()
            .py_4()
            .flex()
            .flex_col()
            .gap_2()
            .child(section("WHERE"))
            .child(
                card()
                    .children(place)
                    .when_some(link, |card, link| card.child(link)),
            )
            .child(section("PEOPLE"))
            .child(card().children(people))
            .child(section("CALENDAR EVENT"))
            .child(calendar)
            .into_any_element()
    }

    fn render_meeting_transcript(&self, meeting: &MeetingManifest) -> AnyElement {
        let content = match &self.meeting_transcript {
            MeetingTranscript::Empty => empty_message("No transcript yet."),
            MeetingTranscript::Loaded(lines) if lines.is_empty() => {
                empty_message(match meeting.status {
                    MeetingStatus::Starting => "Preparing audio capture…",
                    MeetingStatus::Recording if meeting.live_transcription_error.is_some() => {
                        "Live preview is off. The full transcript appears when the meeting ends."
                    }
                    MeetingStatus::Recording if self.meeting_stop_pending => {
                        "Writing the full transcript…"
                    }
                    MeetingStatus::Recording => "Listening…",
                    MeetingStatus::Transcribing => "Writing the full transcript…",
                    MeetingStatus::Complete => "Nobody spoke in this meeting.",
                    MeetingStatus::Interrupted | MeetingStatus::Failed => {
                        "No transcript could be recovered."
                    }
                })
            }
            MeetingTranscript::Loaded(lines) => div()
                .flex()
                .flex_col()
                .gap_3()
                .children(lines.iter().enumerate().map(|(index, line)| {
                    let you = line.speaker == meeting::MeetingSource::Microphone;
                    let (icon, tint, name_color) = if you {
                        (Glyph::Mic, TINT_BLUE, ACCENT_SOFT)
                    } else {
                        let (tint, color) = speaker_tint(line.speaker_id);
                        (Glyph::People, tint, color)
                    };
                    let name = line.name.clone();
                    div()
                        .id(("transcript-line", index))
                        .flex()
                        .flex_col()
                        .when_some(line.pause_before_ms, |row, pause| {
                            row.child(
                                div()
                                    .pb_3()
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .text_size(px(10.0))
                                    .text_color(rgb(FAINT))
                                    .child(div().h(px(1.0)).flex_1().bg(tone(LINE)))
                                    .child(format!("{} pause", meeting_length_label(pause)))
                                    .child(div().h(px(1.0)).flex_1().bg(tone(LINE))),
                            )
                        })
                        .child(
                            div()
                                .flex()
                                .items_start()
                                .gap_3()
                                .child(glyph_badge(icon, 28.0, tint))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w(px(0.0))
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .gap_2()
                                                .child(
                                                    div()
                                                        .text_size(px(11.5))
                                                        .font_weight(FontWeight::SEMIBOLD)
                                                        .text_color(rgb(name_color))
                                                        .child(name),
                                                )
                                                .child(
                                                    div()
                                                        .text_size(px(10.5))
                                                        .text_color(rgb(FAINT))
                                                        .child(line.at.clone()),
                                                ),
                                        )
                                        .child(
                                            div()
                                                .px_3()
                                                .py(px(9.0))
                                                .rounded(px(12.0))
                                                .bg(if you {
                                                    tinted(TINT_BLUE, 0.12)
                                                } else {
                                                    tone(CANVAS)
                                                })
                                                .border_1()
                                                .border_color(rgb(0x303037))
                                                .text_size(px(13.0))
                                                .line_height(px(20.0))
                                                .text_color(rgb(TEXT))
                                                .child(line.text.clone()),
                                        ),
                                ),
                        )
                }))
                .into_any_element(),
            MeetingTranscript::Unavailable(error) => {
                error_message("Transcript unavailable.", error.clone())
            }
        };
        div()
            .id("meeting-transcript")
            .flex_1()
            .min_h(px(0.0))
            .overflow_y_scroll()
            .track_scroll(&self.meeting_transcript_scroll)
            .px_6()
            .py_5()
            .child(content)
            .into_any_element()
    }

    fn render_commands(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let config_path = self.personal_workspace.join("hello.config.ts");
        let workspace_state = personal_workspace_state(
            !self.preview && config_path.is_file(),
            self.personal_workspace_receiver.is_some(),
            &self.personal_commands_status,
        );
        let last_error = self
            .personal_workspace_error
            .clone()
            .or_else(|| self.personal_commands_status.last_reload_error.clone());
        let open_config = config_path.clone();
        let config_control = match workspace_state {
            PersonalWorkspaceState::Missing => glyph_button(Glyph::Pencil, "Create config")
                .id("personal-commands-create")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.provision_personal_workspace();
                    cx.notify();
                }))
                .into_any_element(),
            PersonalWorkspaceState::Creating => div()
                .h(px(30.0))
                .px_3()
                .flex()
                .items_center()
                .text_size(px(12.0))
                .text_color(rgb(MUTED))
                .child("Creating Config...")
                .into_any_element(),
            PersonalWorkspaceState::Ready | PersonalWorkspaceState::Error => {
                glyph_button(Glyph::Pencil, "Edit config")
                    .id("personal-commands-open-config")
                    .on_click(move |_, _, _| open_path(&open_config))
                    .into_any_element()
            }
        };
        let copy_prompt = matches!(
            workspace_state,
            PersonalWorkspaceState::Ready | PersonalWorkspaceState::Error
        )
        .then(|| {
            let copied = self.personal_commands_prompt_copied;
            let workspace = self.personal_workspace.display();
            let config = config_path.display();
            let prompt = format!(
                "Help me configure my Hello custom voice commands.\n\n\
                 Workspace: {workspace}\n\
                 Config: {config}\n\n\
                 Read AGENTS.md and .agents/skills/personal-commands/SKILL.md in the workspace \
                 before editing. Ask me what commands or dictation phrases I want, update \
                 hello.config.ts, and run `bun run check` from the workspace when finished. You \
                 may add npm packages when a command needs them."
            );
            glyph_button(
                Glyph::Copy,
                if copied {
                    "Copied"
                } else {
                    "Copy agent prompt"
                },
            )
            .id("personal-commands-copy-prompt")
            .on_click(cx.listener(move |this, _, _, cx| {
                match arboard::Clipboard::new()
                    .and_then(|mut clipboard| clipboard.set_text(prompt.clone()))
                {
                    Ok(()) => {
                        this.personal_commands_prompt_copied = true;
                        cx.notify();
                    }
                    Err(error) => {
                        tracing::warn!(%error, "could not copy personal command agent prompt")
                    }
                }
            }))
        });
        let commands_position = self.commands_toggle.render_position(window);
        let commands_toggle_control = if self.pending_microphone_policy
            == Some(MicrophonePolicyChange::KeepReadyAndEnableCommands)
        {
            compact_button("Keep Microphone Ready & Enable Commands")
                .id("confirm-enable-commands")
                .on_click(cx.listener(|this, _, _, cx| {
                    cx.stop_propagation();
                    if let Err(error) = this.update_microphone_policy(
                        MicrophonePolicyChange::KeepReadyAndEnableCommands,
                        cx,
                    ) {
                        tracing::error!(%error, "could not update microphone policy");
                    }
                }))
                .into_any_element()
        } else {
            div()
                .flex()
                .items_center()
                .gap_3()
                .child(
                    div()
                        .text_size(px(12.0))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(rgb(TEXT_SOFT))
                        .child("Voice commands"),
                )
                .child(toggle(commands_position))
                .into_any_element()
        };
        let commands_control = div()
            .flex()
            .items_center()
            .gap_3()
            .child(config_control)
            .when_some(copy_prompt, |controls, prompt| controls.child(prompt))
            .child(div().w(px(1.0)).h(px(16.0)).bg(tone(LINE)))
            .child(
                div()
                    .id("commands-enabled")
                    .flex()
                    .items_center()
                    .child(commands_toggle_control)
                    .on_click(cx.listener(|this, _, _, cx| {
                        let enabled = !this.settings.commands_enabled;
                        if enabled && this.settings.release_microphone_while_idle {
                            this.pending_microphone_policy =
                                Some(MicrophonePolicyChange::KeepReadyAndEnableCommands);
                            cx.notify();
                            return;
                        }
                        if let Err(error) = this.developer_set_commands_enabled(enabled, cx) {
                            tracing::error!(%error, "could not save app settings");
                        }
                    })),
            )
            .into_any_element();
        let context_label = self.current_context_label();
        let query = self
            .command_search
            .entity
            .read(cx)
            .text()
            .trim()
            .to_lowercase();
        let visible: Vec<usize> = self
            .commands
            .iter()
            .enumerate()
            .filter_map(|(index, command)| Self::command_matches(command, &query).then_some(index))
            .collect();
        let mut tasks = Vec::new();
        for index in &visible {
            let task = self.commands[*index].task.clone();
            if !tasks.contains(&task) {
                tasks.push(task);
            }
        }
        let effective_selected = self
            .selected_command
            .clone()
            .filter(|id| visible.iter().any(|index| self.commands[*index].id == *id))
            .or_else(|| {
                visible
                    .first()
                    .map(|index| self.commands[*index].id.clone())
            });
        let mut groups: Vec<AnyElement> = Vec::new();
        for task in tasks {
            let members: Vec<usize> = visible
                .iter()
                .copied()
                .filter(|index| self.commands[*index].task == task)
                .collect();
            groups.push(group_heading(task.clone(), Some(members.len())).into_any_element());
            let (task_glyph, task_tint) = command_group_style(&task);
            groups.extend(members.into_iter().map(|index| {
                let command = self.commands[index].clone();
                let command_id = command.id.clone();
                let selected = effective_selected.as_deref() == Some(command_id.as_str());
                list_row(selected)
                    .id(("command", index))
                    .flex_row()
                    .items_center()
                    .gap(px(10.0))
                    .child(glyph_badge(task_glyph, 26.0, task_tint))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .child(
                                div()
                                    .w_full()
                                    .overflow_x_hidden()
                                    .text_size(px(12.5))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(rgb(TEXT))
                                    .truncate()
                                    .child(command.phrase),
                            )
                            .child(
                                div()
                                    .w_full()
                                    .overflow_x_hidden()
                                    .text_size(px(10.5))
                                    .text_color(rgb(if selected { MUTED } else { FAINT }))
                                    .truncate()
                                    .child(command.description),
                            ),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected_command = Some(command_id.clone());
                        cx.notify();
                    }))
                    .into_any_element()
            }));
        }

        let no_commands = groups.is_empty();
        let commands_list_width = PANE_LIST_WIDTH;
        let commands_off = !self.settings.commands_enabled;
        let off_banner = commands_off.then(|| {
            compact_panel()
                .flex_none()
                .px_4()
                .py_3()
                .flex()
                .items_center()
                .gap_3()
                .bg(tinted(TINT_ORANGE, 0.08))
                .border_color(tinted(TINT_ORANGE, 0.3))
                .child(glyph_badge(Glyph::Power, 30.0, TINT_ORANGE))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(
                            div()
                                .text_size(px(12.5))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child("Voice commands are off"),
                        )
                        .child(div().text_size(px(11.0)).text_color(rgb(MUTED)).child(
                            "Switch them on at the top right to run these by voice. \
                                     Dictation works either way.",
                        )),
                )
        });
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(pane_header_with_subtitle(
                "Commands",
                "Phrases you can say to control your Mac hands-free.",
                Some(commands_control),
            ))
            .child(
                pane_body().px_5().pb_5().pt_1().child(
                    pane_content()
                        .gap_3()
                        .when_some(last_error, |column, error| {
                            column.child(
                                compact_panel().flex_none().child(error_message(
                                    "Personal commands are not loading.",
                                    error,
                                )),
                            )
                        })
                        .when_some(off_banner, |column, banner| column.child(banner))
                        .child(
                            div()
                                .flex_1()
                                .min_h(px(0.0))
                                .flex()
                                .gap_4()
                                .child(
                                    div()
                                        .w(px(commands_list_width))
                                        .h_full()
                                        .flex_none()
                                        .flex()
                                        .flex_col()
                                        .gap_2()
                                        .child(
                                            div()
                                                .flex_none()
                                                .child(self.command_search.entity.clone()),
                                        )
                                        .child(
                                            compact_panel()
                                                .id("commands-list")
                                                .flex_1()
                                                .min_h(px(0.0))
                                                .overflow_y_scroll()
                                                .when(no_commands, |card| {
                                                    card.child(empty_message(if query.is_empty() {
                                                        "No commands yet."
                                                    } else {
                                                        "No matching commands."
                                                    }))
                                                })
                                                .child(
                                                    div()
                                                        .w(px(commands_list_width - 2.0))
                                                        .p_2()
                                                        .flex()
                                                        .flex_col()
                                                        .gap_1()
                                                        .children(groups),
                                                ),
                                        ),
                                )
                                .child(
                                    compact_panel()
                                        .id("command-detail")
                                        .flex_1()
                                        .min_w(px(0.0))
                                        .h_full()
                                        .overflow_y_scroll()
                                        .child(self.render_command_detail(
                                            effective_selected.as_deref(),
                                            &context_label,
                                        )),
                                ),
                        ),
                ),
            )
            .into_any_element()
    }

    fn render_command_detail(&self, selected: Option<&str>, context_label: &str) -> AnyElement {
        let Some(command) =
            selected.and_then(|id| self.commands.iter().find(|command| command.id == id))
        else {
            return detail_placeholder("Select a command.");
        };
        let (task_glyph, task_tint) = command_group_style(&command.task);
        let heading = |label: &'static str| {
            div()
                .text_size(px(11.5))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(MUTED))
                .child(label)
        };
        let aliases = command.aliases.clone();
        div()
            .p_6()
            .flex()
            .flex_col()
            .gap_5()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(glyph_badge(task_glyph, 44.0, task_tint))
                    .child(
                        div()
                            .min_w(px(0.0))
                            .flex()
                            .flex_col()
                            .gap(px(3.0))
                            .child(
                                div()
                                    .text_size(px(18.0))
                                    .font_weight(FontWeight::BOLD)
                                    .child(command.phrase.clone()),
                            )
                            .child(
                                div()
                                    .text_size(px(12.0))
                                    .text_color(rgb(MUTED))
                                    .child(command.description.clone()),
                            ),
                    ),
            )
            .child(
                div().flex().flex_col().gap_2().child(heading("Say")).child(
                    div()
                        .w_full()
                        .px_4()
                        .py(px(14.0))
                        .flex()
                        .items_center()
                        .gap_3()
                        .rounded(px(12.0))
                        .border_1()
                        .border_color(tinted(task_tint, 0.3))
                        .bg(tone(CANVAS))
                        .child(glyph(Glyph::Mic, 15.0, ACCENT_SOFT))
                        .child(
                            div()
                                .text_size(px(15.0))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(rgb(TEXT))
                                .child(format!("\u{201c}{}\u{201d}", command.phrase)),
                        ),
                ),
            )
            .when(!aliases.is_empty(), |detail| {
                detail.child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(heading("Or say"))
                        .child(
                            div().flex().flex_wrap().gap_2().children(
                                aliases
                                    .into_iter()
                                    .map(|alias| chip(format!("\u{201c}{alias}\u{201d}"))),
                            ),
                        ),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(detail_row("Group", command.task.clone()))
                    .child(detail_row("Works in", command_scope(&command.scope)))
                    .child(detail_row("Current app", context_label.to_string())),
            )
            .into_any_element()
    }

    fn command_search_input(cx: &mut Context<Self>) -> ProcessingInput {
        let entity = cx.new(|cx| TextInput::picker(cx, "Search commands", ""));
        let changed = cx.subscribe(&entity, |_, _, _: &TextChanged, cx| {
            cx.notify();
        });
        ProcessingInput {
            entity,
            _subscriptions: vec![changed],
        }
    }

    /// Case-insensitive match over everything a person might remember about a
    /// command: its phrase, aliases, description, and group.
    fn command_matches(command: &CatalogCommand, query: &str) -> bool {
        if query.is_empty() {
            return true;
        }
        let scope = match &command.scope {
            CommandScope::Sleeping | CommandScope::Global => "global",
            CommandScope::Application(name) => name,
            CommandScope::Browser(host) => host,
        };
        command.phrase.to_lowercase().contains(query)
            || command.description.to_lowercase().contains(query)
            || command.task.to_lowercase().contains(query)
            || scope.to_lowercase().contains(query)
            || command
                .aliases
                .iter()
                .any(|alias| alias.to_lowercase().contains(query))
    }

    fn current_context(&self) -> (Option<String>, Option<String>) {
        self.current_context.clone()
    }

    fn current_context_label(&self) -> String {
        let (application, browser_host) = self.current_context();
        match (application, browser_host) {
            (Some(application), Some(host)) => format!("{application} / {host}"),
            (Some(application), None) => application,
            _ => "Unavailable".into(),
        }
    }

    fn render_activity(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let refresh = header_button("Refresh")
            .id("refresh-activity")
            .on_click(cx.listener(|this, _, _, cx| {
                this.reload_events();
                cx.notify();
            }))
            .into_any_element();
        let header_action = div()
            .flex()
            .items_center()
            .gap_4()
            .when_some(self.activity.state_label(), |actions, status| {
                let active = self.activity.state != Some(VoiceState::Stopping);
                actions.child(listener_status(
                    status,
                    self.activity.device.clone().unwrap_or_default(),
                    active,
                ))
            })
            .child(refresh)
            .into_any_element();
        let rows: Vec<AnyElement> = self
            .events
            .iter()
            .enumerate()
            .map(|(index, event)| {
                let selected = self.selected_event == Some(index);
                let summary = event_summary(event);
                let age = event_age(event.timestamp_ms());
                let boundary = matches!(event, VoiceEvent::SessionStarted { .. });
                timestamped_list_row(
                    ("activity-event", index),
                    selected,
                    age,
                    div()
                        .flex_1()
                        .text_size(if boundary { px(11.0) } else { px(12.0) })
                        .font_weight(if boundary {
                            FontWeight::SEMIBOLD
                        } else {
                            FontWeight::NORMAL
                        })
                        .text_color(if boundary { rgb(FAINT) } else { rgb(TEXT_SOFT) })
                        .line_height(px(18.0))
                        .child(summary),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.selected_event = Some(index);
                    cx.notify();
                }))
                .into_any_element()
            })
            .collect();

        div()
            .size_full()
            .flex()
            .flex_col()
            .child(pane_header_with_action("Activity", Some(header_action)))
            .child(
                pane_body().p_5().child(
                    pane_content()
                        .flex_row()
                        .gap_5()
                        .child(pane_list_card(
                            "activity-list",
                            (self.events.is_empty() && self.activity.error.is_none())
                                .then_some("No activity yet."),
                            "Activity could not be loaded.",
                            self.activity.error.clone(),
                            rows,
                        ))
                        .child(
                            compact_panel()
                                .flex_1()
                                .min_w(px(0.0))
                                .h_full()
                                .flex()
                                .flex_col()
                                .child(self.render_event_detail()),
                        ),
                ),
            )
            .into_any_element()
    }

    fn render_event_detail(&self) -> AnyElement {
        let Some(event) = self.selected_event.and_then(|index| self.events.get(index)) else {
            return detail_placeholder("Select an activity row.");
        };
        let raw = serde_json::to_string(event).unwrap_or_else(|error| error.to_string());
        div()
            .id("activity-detail")
            .flex_1()
            .min_w(px(0.0))
            .h_full()
            .overflow_y_scroll()
            .px_6()
            .py_6()
            .child(
                div()
                    .text_size(px(18.0))
                    .font_weight(FontWeight::SEMIBOLD)
                    .line_height(px(24.0))
                    .child(event_summary(event)),
            )
            .child(
                div()
                    .pt_6()
                    .child(detail_row("Event", event.kind()))
                    .child(detail_row(
                        "Timestamp",
                        format!("{} ms", event.timestamp_ms()),
                    )),
            )
            .child(
                div()
                    .mt_6()
                    .pt_5()
                    .border_t_1()
                    .border_color(tone(LINE))
                    .child(section_label("Raw event"))
                    .child(
                        div()
                            .pt_3()
                            .text_size(px(11.0))
                            .line_height(px(18.0))
                            .text_color(rgb(MUTED))
                            .child(raw),
                    ),
            )
            .into_any_element()
    }
    fn apply_hud_lab(&self) {
        self.indicator
            .send(DictationIndicatorEvent::Configure(self.hud_tuning));
        self.indicator.send(DictationIndicatorEvent::Reset);
        match self.hud_demo {
            HudDemoState::Recording => {
                for job_id in 0..self.hud_queued as u64 {
                    self.indicator
                        .send(DictationIndicatorEvent::Submitted { job_id });
                    self.indicator
                        .send(DictationIndicatorEvent::Transcribing { job_id });
                }
                self.indicator.send(DictationIndicatorEvent::Started);
                self.indicator.send(DictationIndicatorEvent::Meter {
                    average: 0.08,
                    peak: 0.42,
                });
            }
            HudDemoState::Transcribing | HudDemoState::Processing => {
                self.indicator
                    .send(DictationIndicatorEvent::Submitted { job_id: 0 });
                self.indicator.send(match self.hud_demo {
                    HudDemoState::Transcribing => {
                        DictationIndicatorEvent::Transcribing { job_id: 0 }
                    }
                    HudDemoState::Processing => DictationIndicatorEvent::Processing { job_id: 0 },
                    HudDemoState::Recording => unreachable!(),
                });
                for job_id in 1..=self.hud_queued as u64 {
                    self.indicator
                        .send(DictationIndicatorEvent::Submitted { job_id });
                    self.indicator
                        .send(DictationIndicatorEvent::Transcribing { job_id });
                }
            }
        }
    }

    fn hud_control_value(&self, control: HudControl) -> f32 {
        match control {
            HudControl::LineCount => (self.hud_tuning.line_count - 1.0) / 5.0,
            HudControl::Curvature => self.hud_tuning.curvature,
            HudControl::Speed => self.hud_tuning.speed / HUD_MAX_SPEED,
            HudControl::Blur => 1.0 - self.hud_tuning.sharpness,
            HudControl::Glow => self.hud_tuning.glow,
            HudControl::Depth => self.hud_tuning.depth,
            HudControl::LightAngle => self.hud_tuning.light_angle,
            HudControl::Outline => self.hud_tuning.outline,
        }
        .clamp(0.0, 1.0)
    }

    fn set_hud_control(&mut self, control: HudControl, value: f32, cx: &mut Context<Self>) {
        let value = value.clamp(0.0, 1.0);
        match control {
            HudControl::LineCount => self.hud_tuning.line_count = (1.0 + value * 5.0).round(),
            HudControl::Curvature => self.hud_tuning.curvature = value,
            HudControl::Speed => self.hud_tuning.speed = value * HUD_MAX_SPEED,
            HudControl::Blur => self.hud_tuning.sharpness = 1.0 - value,
            HudControl::Glow => self.hud_tuning.glow = value,
            HudControl::Depth => self.hud_tuning.depth = value,
            HudControl::LightAngle => self.hud_tuning.light_angle = value,
            HudControl::Outline => self.hud_tuning.outline = value,
        }
        self.hud_copied = false;
        self.indicator
            .send(DictationIndicatorEvent::Configure(self.hud_tuning));
        cx.notify();
    }

    fn update_hud_from_pointer(
        &mut self,
        control: HudControl,
        pointer_x: f32,
        window: &Window,
        cx: &mut Context<Self>,
    ) {
        const SLIDER_WIDTH: f32 = 360.0;
        let available = (f32::from(window.viewport_size().width) - SIDEBAR_WIDTH - 64.0).max(0.0);
        let content_width = available.min(PANE_CONTENT_WIDTH);
        let slider_left = SIDEBAR_WIDTH + 32.0 + (available - content_width) / 2.0;
        self.set_hud_control(control, (pointer_x - slider_left) / SLIDER_WIDTH, cx);
    }

    fn select_hud_style(&mut self, style: usize, cx: &mut Context<Self>) {
        self.hud_tuning = match style {
            0 => HudTuning {
                style: 0.0,
                line_count: 1.0,
                curvature: 0.5,
                speed: 0.7,
                sharpness: 0.42,
                glow: 0.4,
                depth: 0.75,
                light_angle: 0.35,
                outline: 0.25,
            },
            1 => HudTuning::default(),
            _ => HudTuning {
                style: 2.0,
                line_count: 3.0,
                curvature: 0.7,
                speed: 0.9,
                sharpness: 0.64,
                glow: 0.28,
                depth: 0.74,
                light_angle: 0.35,
                outline: 0.3,
            },
        };
        self.hud_copied = false;
        self.apply_hud_lab();
        cx.notify();
    }

    fn hud_preset(&self) -> String {
        format!(
            "hud-v1 style={} lines={:.0} curvature={:.2} speed={:.2} blur={:.2} glow={:.2} depth={:.2} light={:.2} outline={:.2}",
            self.hud_tuning.style as usize,
            self.hud_tuning.line_count,
            self.hud_tuning.curvature,
            self.hud_tuning.speed,
            1.0 - self.hud_tuning.sharpness,
            self.hud_tuning.glow,
            self.hud_tuning.depth,
            self.hud_tuning.light_angle,
            self.hud_tuning.outline,
        )
    }

    fn render_hud_slider(
        &self,
        control: HudControl,
        label: &'static str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let value = self.hud_control_value(control);
        let display = match control {
            HudControl::LineCount => format!("{:.0}", self.hud_tuning.line_count),
            HudControl::Speed => format!("{:.2}x", self.hud_tuning.speed),
            HudControl::LightAngle => format!("{:.0}°", self.hud_tuning.light_angle * 360.0),
            _ => format!("{:.2}", value),
        };
        div()
            .w(px(360.0))
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .flex()
                    .justify_between()
                    .text_size(px(12.0))
                    .child(label)
                    .child(div().text_color(rgb(MUTED)).child(display)),
            )
            .child(
                div()
                    .id(("hud-slider", control as usize))
                    .relative()
                    .w_full()
                    .h(px(18.0))
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .right_0()
                            .top(px(7.0))
                            .h(px(4.0))
                            .rounded_full()
                            .bg(rgb(0x303030)),
                    )
                    .child(
                        div()
                            .absolute()
                            .left_0()
                            .top(px(7.0))
                            .w(relative(value))
                            .h(px(4.0))
                            .rounded_full()
                            .bg(rgb(ACCENT)),
                    )
                    .child(
                        div()
                            .absolute()
                            .left(relative(value))
                            .top(px(2.0))
                            .ml(px(-7.0))
                            .size(px(14.0))
                            .rounded_full()
                            .border_2()
                            .border_color(rgb(TEXT))
                            .bg(tone(SURFACE_SELECTED)),
                    )
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            this.hud_dragging = Some(control);
                            this.update_hud_from_pointer(
                                control,
                                f32::from(event.position.x),
                                window,
                                cx,
                            );
                        }),
                    )
                    .on_mouse_move(
                        cx.listener(move |this, event: &MouseMoveEvent, window, cx| {
                            if event.dragging() && this.hud_dragging == Some(control) {
                                this.update_hud_from_pointer(
                                    control,
                                    f32::from(event.position.x),
                                    window,
                                    cx,
                                );
                            }
                        }),
                    ),
            )
            .into_any_element()
    }

    fn render_hud_lab(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let styles = [
            (
                "A",
                "Moving light",
                "A line-free area light travels around the orb.",
            ),
            (
                "B",
                "Meridians",
                "Highlights wrap around the sphere instead of crossing it.",
            ),
            (
                "C",
                "Ribbons",
                "Curved bands retained for comparison and tuning.",
            ),
        ];
        let style_cards =
            styles
                .into_iter()
                .enumerate()
                .map(|(index, (key, name, description))| {
                    let selected = self.hud_tuning.style as usize == index;
                    div()
                        .id(("hud-style", index))
                        .w(px(190.0))
                        .p_3()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .rounded(px(PANEL_RADIUS))
                        .border_1()
                        .border_color(if selected { rgb(ACCENT) } else { tone(LINE) })
                        .bg(tone(if selected { ACCENT_SURFACE } else { SURFACE }))
                        .hover(|card| card.bg(tone(SURFACE_HOVER)))
                        .child(
                            div()
                                .text_size(px(10.0))
                                .text_color(rgb(ACCENT_SOFT))
                                .child(key),
                        )
                        .child(
                            div()
                                .text_size(px(12.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(name),
                        )
                        .child(
                            div()
                                .text_size(px(10.0))
                                .text_color(rgb(MUTED))
                                .child(description),
                        )
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.select_hud_style(index, cx)),
                        )
                });
        let state_button = |state, label: &'static str, index: usize| {
            segmented_item(self.hud_demo == state)
                .id(("hud-state", index))
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.hud_demo = state;
                    this.apply_hud_lab();
                    cx.notify();
                }))
        };
        let queue_buttons = (0..=3).map(|count| {
            segmented_item(self.hud_queued == count)
                .id(("hud-queue", count))
                .child(count.to_string())
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.hud_queued = count;
                    this.apply_hud_lab();
                    cx.notify();
                }))
        });
        let preset = self.hud_preset();
        let copied = self.hud_copied;

        div()
            .size_full()
            .flex()
            .flex_col()
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| {
                    this.hud_dragging = None;
                }),
            )
            .child(pane_header_with_action(
                "HUD Lab",
                Some(
                    header_button(if copied { "Copied" } else { "Copy preset" })
                        .id("copy-hud-preset")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Ok(mut clipboard) = arboard::Clipboard::new() {
                                let _ = clipboard.set_text(this.hud_preset());
                                this.hud_copied = true;
                                cx.notify();
                            }
                        }))
                        .into_any_element(),
                ),
            ))
            .child(
                pane_body().child(
                    div()
                        .id("hud-lab-scroll")
                        .flex_1()
                        .overflow_y_scroll()
                        .px_8()
                        .py_6()
                        .child(
                            div().w_full().flex().justify_center().child(
                                pane_content()
                                    .gap_6()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_4()
                                            .child(segmented_control().children([
                                                state_button(
                                                    HudDemoState::Recording,
                                                    "Recording",
                                                    0,
                                                ),
                                                state_button(
                                                    HudDemoState::Transcribing,
                                                    "Transcribing",
                                                    1,
                                                ),
                                                state_button(
                                                    HudDemoState::Processing,
                                                    "Processing",
                                                    2,
                                                ),
                                            ]))
                                            .child(
                                                div()
                                                    .flex()
                                                    .items_center()
                                                    .gap_2()
                                                    .child(
                                                        div()
                                                            .text_size(px(11.0))
                                                            .text_color(rgb(MUTED))
                                                            .child("Queued"),
                                                    )
                                                    .child(
                                                        segmented_control().children(queue_buttons),
                                                    ),
                                            ),
                                    )
                                    .child(div().flex().gap_3().children(style_cards))
                                    .child(div().flex().child(
                                        div().flex().flex_col().gap_4().children([
                                            self.render_hud_slider(
                                                HudControl::LineCount,
                                                "Line count",
                                                cx,
                                            ),
                                            self.render_hud_slider(
                                                HudControl::Curvature,
                                                "Curvature",
                                                cx,
                                            ),
                                            self.render_hud_slider(HudControl::Speed, "Speed", cx),
                                            self.render_hud_slider(
                                                HudControl::Blur,
                                                "Highlight blur",
                                                cx,
                                            ),
                                            self.render_hud_slider(
                                                HudControl::Glow,
                                                "Line glow",
                                                cx,
                                            ),
                                            self.render_hud_slider(
                                                HudControl::Depth,
                                                "Sphere depth",
                                                cx,
                                            ),
                                            self.render_hud_slider(
                                                HudControl::LightAngle,
                                                "Light angle",
                                                cx,
                                            ),
                                            self.render_hud_slider(
                                                HudControl::Outline,
                                                "Sphere outline",
                                                cx,
                                            ),
                                        ]),
                                    ))
                                    .child(
                                        div()
                                            .p_3()
                                            .rounded(px(PANEL_RADIUS))
                                            .border_1()
                                            .border_color(tone(LINE))
                                            .bg(tone(SURFACE))
                                            .font_family("SF Mono")
                                            .text_size(px(10.0))
                                            .text_color(rgb(TEXT_SOFT))
                                            .child(preset),
                                    ),
                            ),
                        ),
                ),
            )
            .into_any_element()
    }
}

fn application_catalog_load_needed(
    preview: bool,
    catalog: &ApplicationCatalogState,
    receiver_pending: bool,
) -> bool {
    !preview && matches!(catalog, ApplicationCatalogState::Loading) && !receiver_pending
}

impl Drop for AppWindow {
    fn drop(&mut self) {
        if !self.preview
            && self.settings_dirty
            && let Err(error) = self.settings_candidate().save()
        {
            tracing::error!(%error, "could not flush app settings while closing window");
        }
        if !self.preview {
            crate::app_settings::set_hotkey_capture_active(false);
            crate::app_settings::set_dock_icon_visible(crate::app_settings::dock_icon_visible(
                self.settings.show_dock_icon,
                crate::status_item::installed(),
            ));
        }
    }
}

impl TranscriptionPickerDelegate for AppWindow {
    fn cancel_transcription_preparation(&mut self) {
        self.cancel_transcription_download();
    }

    fn choose_transcription_model(
        &mut self,
        model: TranscriptionModelId,
        language: String,
        cx: &mut Context<Self>,
    ) {
        AppWindow::choose_transcription_model(self, definition(model), language, cx);
    }

    fn delete_transcription_model(&mut self, model: TranscriptionModelId, _cx: &mut Context<Self>) {
        AppWindow::delete_transcription_model(self, definition(model));
    }

    fn dismiss_transcription_picker(&mut self, cx: &mut Context<Self>) {
        AppWindow::dismiss_transcription_picker(self, cx);
    }

    fn select_transcription_language(&mut self, language: String, cx: &mut Context<Self>) {
        if self.transcription_picker_language.as_deref() != Some(&language) {
            self.cancel_transcription_download();
            self.transcription_picker_language = Some(language);
            self.clear_transcription_error();
            cx.notify();
        }
    }
}

impl Render for AppWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::desktop_ui::apply_theme(self.settings.theme, window.appearance());
        let model_notice = self.render_model_notice(cx);
        let content = match self.pane {
            Pane::HudLab => self.render_hud_lab(cx),
            Pane::Meetings => self.render_meetings(window, cx),
            Pane::Commands => self.render_commands(window, cx),
            Pane::Activity => self.render_activity(cx),
            Pane::History => self.render_history(cx),
            Pane::Modes => self.render_modes(window, cx),
            Pane::Settings => self.render_settings(window, cx),
            Pane::General => self.render_general(window, cx),
            Pane::Appearance => self.render_appearance(window, cx),
            Pane::Sounds => self.render_sounds(window, cx),
            Pane::Profile => self.render_profile(),
        };
        let transcription_picker = self
            .transcription_picker_language
            .clone()
            .map(|language| self.render_transcription_picker(&language, cx));
        let setup = self.setup_visible.then(|| self.render_setup(cx));
        let mode_context_menu = (self.pane == Pane::Modes)
            .then(|| self.render_mode_context_menu(cx))
            .flatten();
        window_frame()
            .track_focus(&self.window_focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key != "escape" {
                    return;
                }
                if this.history_retention_open {
                    this.history_retention_open = false;
                    cx.stop_propagation();
                    cx.notify();
                } else if this.mode_context_menu.take().is_some()
                    || this.pending_microphone_policy.take().is_some()
                {
                    cx.stop_propagation();
                    cx.notify();
                } else if this.transcription_picker_language.is_some() {
                    cx.stop_propagation();
                    this.dismiss_transcription_picker(cx);
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.mode_context_menu.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            .on_action(|_: &CloseWindow, window, _| window.remove_window())
            .on_action(|_: &MinimizeWindow, window, _| window.minimize_window())
            .on_action(|_: &ToggleFullscreen, window, _| window.toggle_fullscreen())
            .on_action(cx.listener(|this, _: &ShowSettings, window, cx| {
                this.select_pane(Pane::Settings, cx);
                window.activate_window();
            }))
            .on_action(cx.listener(|this, _: &ShowModes, window, cx| {
                this.select_pane(Pane::Modes, cx);
                window.activate_window();
            }))
            .on_action(cx.listener(|this, _: &ShowCommands, window, cx| {
                this.select_pane(Pane::Commands, cx);
                window.activate_window();
            }))
            .on_action(cx.listener(|this, _: &StartMeetingNotes, window, cx| {
                this.select_pane(Pane::Meetings, cx);
                if meeting::active(&this.meetings).is_none() {
                    this.start_meeting_notes(cx);
                }
                window.activate_window();
            }))
            .on_action(cx.listener(|this, _: &StopMeetingNotes, _, cx| {
                if meeting::active(&this.meetings)
                    .is_some_and(|meeting| meeting.status == MeetingStatus::Recording)
                {
                    this.meeting_stop_pending = true;
                    if let Err(error) = this.meeting_requests.try_send(MeetingRequest::Stop) {
                        this.meeting_stop_pending = false;
                        log_meeting_request_error(error);
                    }
                    cx.notify();
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleMeetingDetection, _, cx| {
                let enabled = !this.settings.notetaker.detect_meetings;
                this.set_meeting_detection(enabled, cx);
            }))
            .on_action(cx.listener(|this, _: &ShowNotetaker, window, cx| {
                this.select_pane(Pane::Meetings, cx);
                window.activate_window();
            }))
            .on_action(cx.listener(|this, _: &ShowHistory, window, cx| {
                this.select_pane(Pane::History, cx);
                window.activate_window();
            }))
            .on_action(cx.listener(|this, _: &ShowAppearance, window, cx| {
                this.select_pane(Pane::Appearance, cx);
                window.activate_window();
            }))
            .on_action(cx.listener(|this, _: &ShowGeneral, window, cx| {
                this.select_pane(Pane::General, cx);
                window.activate_window();
            }))
            .on_action(cx.listener(|this, _: &ShowSounds, window, cx| {
                this.select_pane(Pane::Sounds, cx);
                window.activate_window();
            }))
            .on_action(cx.listener(|this, _: &ShowProfile, window, cx| {
                this.select_pane(Pane::Profile, cx);
                window.activate_window();
            }))
            .on_action(cx.listener(|this, _: &IndicatorStyleClassic, _, cx| {
                this.set_indicator_style(IndicatorStyle::Classic, cx)
            }))
            .on_action(cx.listener(|this, _: &IndicatorStyleWave, _, cx| {
                this.set_indicator_style(IndicatorStyle::Wave, cx)
            }))
            .on_action(cx.listener(|this, _: &IndicatorStyleOrb, _, cx| {
                this.set_indicator_style(IndicatorStyle::Orb, cx)
            }))
            .on_action(cx.listener(|this, _: &IndicatorStyleLine, _, cx| {
                this.set_indicator_style(IndicatorStyle::Line, cx)
            }))
            .on_action(cx.listener(|this, _: &IndicatorStyleDots, _, cx| {
                this.set_indicator_style(IndicatorStyle::Dots, cx)
            }))
            .on_action(cx.listener(|this, _: &IndicatorAtTop, _, cx| {
                this.set_indicator_position(IndicatorPosition::Top, cx)
            }))
            .on_action(cx.listener(|this, _: &IndicatorAtBottom, _, cx| {
                this.set_indicator_position(IndicatorPosition::Bottom, cx)
            }))
            .on_action(cx.listener(|this, _: &LanguageAuto, _, cx| {
                this.choose_language(LanguageRouting::Auto, cx)
            }))
            .on_action(cx.listener(|this, _: &LanguageEnglish, _, cx| {
                this.choose_language(LanguageRouting::Main, cx)
            }))
            .on_action(cx.listener(|this, _: &LanguageBangla, _, cx| {
                this.choose_language(LanguageRouting::Second, cx)
            }))
            .child(self.render_navigation(cx))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .overflow_hidden()
                    // Content stays solid; only the navigation layer is glass.
                    .bg(tone(crate::desktop_ui::CONTENT))
                    .children(model_notice)
                    .when_some(self.settings_load_error.clone(), |column, error| {
                        column.child(
                            div()
                                .px_5()
                                .py_2()
                                .debug_selector(|| "settings-error".into())
                                .child(error_message("Settings error:", error)),
                        )
                    })
                    .child(div().flex_1().min_h_0().child(content)),
            )
            .children(setup)
            .children(transcription_picker)
            .children(mode_context_menu)
    }
}

/// Deterministic retained-history fixtures for the History pane preview.
fn preview_history() -> Option<History> {
    use crate::history::{HistoryDraft, HistoryProcessing, HistoryStore, now_ms};
    let directory =
        std::env::temp_dir().join(format!("hello-history-preview-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir_all(&directory).ok()?;
    let mut store = HistoryStore::open(
        directory.join("history.json"),
        HistoryRetention::Week,
        now_ms(),
    );
    let now = now_ms();
    let fixtures = [
        (
            26 * 60 * 60 * 1_000,
            HistoryKind::Dictation,
            "so um the parser needs a retry when the socket drops",
            "The parser needs a retry when the socket drops.",
            Some("Zed"),
            Some(("Work notes", 940)),
        ),
        (
            3 * 60 * 60 * 1_000,
            HistoryKind::Send,
            "sounds good shipping it after lunch",
            "Sounds good — shipping it after lunch.",
            Some("Slack"),
            Some(("Messages", 610)),
        ),
        (
            4 * 60 * 1_000,
            HistoryKind::Dictation,
            "so um remember to publish the appcast after validating the artifact and uh \
             double check that the release notes mention the new capture placeholders \
             before anyone downloads it",
            "Remember to publish the appcast after validating the artifact, and double-check \
             that the release notes mention the new capture placeholders before anyone \
             downloads it.",
            Some("Messages"),
            None,
        ),
    ];
    for (age_ms, kind, raw, final_text, application, processing) in fixtures {
        let _ = store.record(
            HistoryDraft {
                kind,
                raw_text: raw.into(),
                final_text: final_text.into(),
                application: application.map(Into::into),
                processing: processing.map(|(profile, latency_ms)| HistoryProcessing {
                    profile: profile.into(),
                    latency_ms,
                    fallback: None,
                }),
                audio_ms: 2_800,
                inference_ms: 96,
                total_ms: 3_400,
            },
            now.saturating_sub(age_ms),
        );
    }
    Some(History::new(store))
}

fn hotkey_idle_width(keycap_count: usize) -> f32 {
    (112.0 + hotkey_keycaps_width(keycap_count)).max(HOTKEY_MIN_WIDTH)
}

const fn hotkey_kind_index(kind: HotkeyKind) -> usize {
    match kind {
        HotkeyKind::Dictation => 0,
        HotkeyKind::PasteLast => 1,
    }
}

const fn hotkey_side_index(side: ModifierSide) -> usize {
    match side {
        ModifierSide::Left => 0,
        ModifierSide::Either => 1,
        ModifierSide::Right => 2,
    }
}

fn standalone_modifier_side(binding: &HotkeyBinding) -> Option<ModifierSide> {
    if binding.key.is_some() || binding.modifiers.count() != 1 {
        return None;
    }
    binding
        .modifiers
        .control
        .or(binding.modifiers.option)
        .or(binding.modifiers.shift)
        .or(binding.modifiers.command)
}

fn set_standalone_modifier_side(binding: &mut HotkeyBinding, side: ModifierSide) {
    if standalone_modifier_side(binding).is_none() {
        return;
    }
    if binding.modifiers.control.is_some() {
        binding.modifiers.control = Some(side);
    } else if binding.modifiers.option.is_some() {
        binding.modifiers.option = Some(side);
    } else if binding.modifiers.shift.is_some() {
        binding.modifiers.shift = Some(side);
    } else if binding.modifiers.command.is_some() {
        binding.modifiers.command = Some(side);
    }
}

fn hotkey_binding_conflicts(
    settings: &AppSettings,
    kind: HotkeyKind,
    binding: &HotkeyBinding,
) -> bool {
    let mut others = match kind {
        HotkeyKind::Dictation => Vec::new(),
        HotkeyKind::PasteLast => vec![settings.dictation_hotkey.clone()],
    };
    if kind != HotkeyKind::PasteLast
        && let Some(paste) = &settings.paste_last_hotkey
    {
        others.push(paste.clone());
    }
    if crate::DEVELOPER_FEATURES_ENABLED {
        others.push(HotkeyBinding::paste_meeting_default());
    }
    crate::app_settings::hotkey_conflicts(binding, others)
}

fn hotkey_binding(settings: &AppSettings, kind: HotkeyKind) -> Option<&HotkeyBinding> {
    match kind {
        HotkeyKind::Dictation => Some(&settings.dictation_hotkey),
        HotkeyKind::PasteLast => settings.paste_last_hotkey.as_ref(),
    }
}

fn hotkey_side_binding(
    settings: &AppSettings,
    kind: HotkeyKind,
    side: ModifierSide,
) -> Option<HotkeyBinding> {
    let mut binding = hotkey_binding(settings, kind)?.clone();
    standalone_modifier_side(&binding)?;
    set_standalone_modifier_side(&mut binding, side);
    (!hotkey_binding_conflicts(settings, kind, &binding)).then_some(binding)
}

fn hotkey_control_width(active: bool, idle_width: f32, animated_width: f32) -> f32 {
    if active { animated_width } else { idle_width }
}

fn hotkey_control_intensity(active: bool, intensity: f32) -> f32 {
    if active { intensity } else { 0.0 }
}

fn hotkey_capture_width(keycap_count: usize) -> f32 {
    if keycap_count == 0 {
        180.0
    } else {
        236.0 + hotkey_keycaps_width(keycap_count)
    }
}

fn hotkey_saved_width(keycap_count: usize) -> f32 {
    (53.0 + hotkey_keycaps_width(keycap_count)).max(HOTKEY_MIN_WIDTH)
}

fn hotkey_keycaps_width(keycap_count: usize) -> f32 {
    if keycap_count == 0 {
        0.0
    } else {
        34.0 * keycap_count as f32 + 3.0 * keycap_count.saturating_sub(1) as f32
    }
}

fn recording_audio_index(behavior: RecordingAudioBehavior) -> usize {
    match behavior {
        RecordingAudioBehavior::Mute => 0,
        RecordingAudioBehavior::PauseMedia => 1,
        RecordingAudioBehavior::DoNothing => 2,
    }
}

const SMART_EDITING_ROWS: [(&str, &str); 5] = [
    ("Remove filler words", "Drop um, uh, and erm from dictation"),
    (
        "Scratch that",
        "Say \u{201c}scratch that\u{201d} to delete the sentence before it",
    ),
    (
        "Spoken line breaks",
        "Say \u{201c}new line\u{201d} or \u{201c}new paragraph\u{201d}",
    ),
    (
        "Numbered lists",
        "Say \u{201c}number one\u{2026} number two\u{2026}\u{201d} to make a list",
    ),
    (
        "End with a full stop",
        "Add a full stop when you stop mid-sentence",
    ),
];

fn smart_editing_flags(options: SmartEditing) -> [bool; 5] {
    [
        options.remove_fillers,
        options.scratch_that,
        options.spoken_line_breaks,
        options.numbered_lists,
        options.end_with_period,
    ]
}

fn smart_editing_from_flags(flags: [bool; 5]) -> SmartEditing {
    SmartEditing {
        remove_fillers: flags[0],
        scratch_that: flags[1],
        spoken_line_breaks: flags[2],
        numbered_lists: flags[3],
        end_with_period: flags[4],
    }
}

/// A row of usage tiles above the History list, computed from retained entries.
fn render_history_insights(insights: &HistoryInsights) -> AnyElement {
    let speed = insights
        .words_per_minute()
        .map_or_else(|| "\u{2014}".to_string(), |wpm| format!("{wpm:.0} wpm"));
    let saved = insights.minutes_saved();
    let saved = if saved >= 60.0 {
        format!("{:.1} h", saved / 60.0)
    } else {
        format!("{saved:.0} min")
    };
    let tiles = [
        (
            Glyph::Text,
            TINT_BLUE,
            "Words",
            format_count(insights.words),
        ),
        (Glyph::Speed, TINT_PURPLE, "Speaking speed", speed),
        (Glyph::Clock, TINT_GREEN, "Time saved", saved),
        (
            Glyph::Apps,
            TINT_ORANGE,
            "Top app",
            insights
                .top_application
                .clone()
                .unwrap_or_else(|| "\u{2014}".to_string()),
        ),
    ];
    div()
        .flex_none()
        .flex()
        .flex_row()
        .gap_3()
        .children(tiles.into_iter().map(|(icon, tint, label, value)| {
            compact_panel()
                .flex_1()
                .min_w(px(0.0))
                .px(px(14.0))
                .py(px(12.0))
                .flex()
                .items_center()
                .gap_3()
                .child(glyph_badge(icon, 34.0, tint))
                .child(
                    div()
                        .min_w(px(0.0))
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(
                            div()
                                .overflow_x_hidden()
                                .truncate()
                                .text_size(px(17.0))
                                .font_weight(FontWeight::BOLD)
                                .text_color(rgb(TEXT))
                                .child(value),
                        )
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(rgb(MUTED))
                                .child(label),
                        ),
                )
        }))
        .into_any_element()
}

/// Width of the text column in a history row: the list column minus its
/// padding, the row padding and the leading badge.
const HISTORY_ROW_TEXT_WIDTH: f32 = PANE_LIST_WIDTH - 2.0 - 16.0 - 24.0 - 26.0 - 10.0;

fn history_kind_style(kind: HistoryKind) -> (Glyph, u32) {
    match kind {
        HistoryKind::Dictation => (Glyph::Mic, TINT_BLUE),
        HistoryKind::Send => (Glyph::Bolt, TINT_GREEN),
    }
}

fn format_duration_ms(ms: u64) -> String {
    if ms >= 1_000 {
        format!("{:.1} s", ms as f64 / 1_000.0)
    } else {
        format!("{ms} ms")
    }
}

/// The local calendar fields for a millisecond timestamp.
fn local_time(timestamp_ms: u64) -> Option<libc::tm> {
    let seconds = (timestamp_ms / 1_000) as libc::time_t;
    // SAFETY: localtime_r only writes into the zeroed struct we own.
    let mut fields: libc::tm = unsafe { std::mem::zeroed() };
    let result = unsafe { libc::localtime_r(&seconds, &mut fields) };
    (!result.is_null()).then_some(fields)
}

fn local_day_number(fields: &libc::tm, timestamp_ms: u64) -> i64 {
    (timestamp_ms / 1_000) as i64 + fields.tm_gmtoff as i64
}

/// "Today", "Yesterday", a weekday within the last week, else a date.
fn history_day_label(timestamp_ms: u64) -> String {
    const WEEKDAYS: [&str; 7] = [
        "Sunday",
        "Monday",
        "Tuesday",
        "Wednesday",
        "Thursday",
        "Friday",
        "Saturday",
    ];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let now = now_ms();
    let (Some(entry), Some(today)) = (local_time(timestamp_ms), local_time(now)) else {
        return "Earlier".into();
    };
    let days_ago = local_day_number(&today, now).div_euclid(86_400)
        - local_day_number(&entry, timestamp_ms).div_euclid(86_400);
    match days_ago {
        ..=0 => "Today".into(),
        1 => "Yesterday".into(),
        2..=6 => WEEKDAYS[entry.tm_wday.rem_euclid(7) as usize].into(),
        _ => {
            let month = MONTHS[entry.tm_mon.rem_euclid(12) as usize];
            if entry.tm_year == today.tm_year {
                format!("{month} {}", entry.tm_mday)
            } else {
                format!("{month} {}, {}", entry.tm_mday, entry.tm_year + 1900)
            }
        }
    }
}

/// Local clock time such as "3:42 PM".
fn clock_time(timestamp_ms: u64) -> String {
    let Some(fields) = local_time(timestamp_ms) else {
        return event_age(timestamp_ms);
    };
    let hour = fields.tm_hour.rem_euclid(24);
    let display = match hour % 12 {
        0 => 12,
        other => other,
    };
    format!(
        "{display}:{:02} {}",
        fields.tm_min,
        if hour < 12 { "AM" } else { "PM" }
    )
}

fn format_count(count: usize) -> String {
    let digits = count.to_string();
    let mut grouped = String::new();
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(digit);
    }
    grouped
}

/// Hello's own release number. The engine underneath keeps its upstream
/// version (CARGO_PKG_VERSION) for local API and SDK compatibility.
pub(crate) const APP_VERSION: &str = "1.0.3";

fn theme_index(theme: ColorTheme) -> usize {
    ColorTheme::ALL
        .iter()
        .position(|option| *option == theme)
        .unwrap_or(0)
}

fn indicator_position_index(position: IndicatorPosition) -> usize {
    match position {
        IndicatorPosition::Top => 0,
        IndicatorPosition::Bottom => 1,
    }
}

/// The profile name, falling back to the macOS account's full name.
fn profile_display_name(settings: &AppSettings) -> String {
    let chosen = settings.profile_name.trim();
    if !chosen.is_empty() {
        return chosen.to_string();
    }
    #[cfg(target_os = "macos")]
    {
        let full_name = objc2_foundation::NSFullUserName().to_string();
        if !full_name.trim().is_empty() {
            return full_name.trim().to_string();
        }
    }
    "You".to_string()
}

fn profile_initials(name: &str) -> String {
    let initials: String = name
        .split_whitespace()
        .filter_map(|word| word.chars().find(|c| c.is_alphanumeric()))
        .take(2)
        .flat_map(char::to_uppercase)
        .collect();
    if initials.is_empty() {
        "?".to_string()
    } else {
        initials
    }
}

fn profile_avatar(name: &str, diameter: f32) -> AnyElement {
    div()
        .size(px(diameter))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(linear_gradient(
            135.0,
            linear_color_stop(rgb(0x5b6cff), 0.0),
            linear_color_stop(rgb(0xb44cff), 1.0),
        ))
        .text_size(px(diameter * 0.38))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(0xffffff))
        .child(profile_initials(name))
        .into_any_element()
}

/// Simulated speech loudness that drives the Appearance previews.
fn preview_level(time: f32) -> f32 {
    (0.55 + 0.45 * (time * 2.1).sin() * (time * 0.7 + 1.0).sin()).clamp(0.0, 1.0)
}

/// The Appearance previews are drawn larger than the real indicator so the
/// shapes read clearly inside the cards.
const PREVIEW_SCALE: f32 = 1.4;

fn preview_px(value: f32) -> Pixels {
    px(value * PREVIEW_SCALE)
}

fn preview_pill(width: f32, height: f32) -> Div {
    div()
        .w(preview_px(width))
        .h(preview_px(height))
        .flex_none()
        .relative()
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(gpui::rgb(0x09090a))
        .border_1()
        .border_color(gpui::rgba(0xffffff2e))
        .shadow(vec![gpui::BoxShadow {
            color: gpui::hsla(0.0, 0.0, 0.0, 0.55),
            offset: gpui::point(px(0.0), px(4.0)),
            blur_radius: px(12.0),
            spread_radius: px(0.0),
        }])
}

/// A live miniature of each recording indicator style for the Appearance page.
/// Drawn in the real indicator's colors, which don't change with the theme.
fn indicator_preview(style: IndicatorStyle, time: f32) -> AnyElement {
    let level = preview_level(time);
    match style {
        IndicatorStyle::Classic => div()
            .p(preview_px(3.0))
            .rounded_full()
            .bg(gpui::rgba(0xff2d3a2e))
            .opacity(0.75 + 0.25 * (time * 3.0).sin())
            .child(
                div()
                    .w(preview_px(56.0))
                    .h(preview_px(16.0))
                    .rounded_full()
                    .bg(linear_gradient(
                        90.0,
                        linear_color_stop(gpui::rgb(0xff1f2f), 0.0),
                        linear_color_stop(gpui::rgb(0xff6a3d), 1.0),
                    )),
            )
            .into_any_element(),
        IndicatorStyle::Wave => preview_pill(68.0, 28.0)
            .gap(preview_px(2.6))
            .children((0..9).map(|index| {
                let slot = index as f32 - 4.0;
                let envelope = 1.0 - slot.abs() / 4.0 * 0.55;
                let jitter = 0.6 + 0.4 * (time * 14.0 + slot * 1.7).sin();
                let amount = (level * envelope * jitter).max(0.1);
                div()
                    .w(preview_px(2.4))
                    .h(preview_px(2.4 + 17.6 * amount))
                    .rounded_full()
                    .bg(gpui::rgb(0xffffff))
            }))
            .into_any_element(),
        IndicatorStyle::Orb => {
            let radius = 5.0 + 0.6 * (time * 3.0).sin() + 5.5 * level;
            preview_pill(30.0, 30.0)
                .child(
                    div()
                        .size(preview_px(radius * 2.0))
                        .rounded_full()
                        .bg(linear_gradient(
                            (time * 60.0) % 360.0,
                            linear_color_stop(gpui::rgb(0x528fff), 0.0),
                            linear_color_stop(gpui::rgb(0xd070ff), 1.0),
                        )),
                )
                .into_any_element()
        }
        IndicatorStyle::Line => {
            let amplitude = 7.0 * level.max(0.06);
            preview_pill(68.0, 24.0)
                .child(
                    gpui::canvas(
                        |_, _, _| {},
                        move |bounds, _, window, _| {
                            const HALF_SPAN: f32 = 25.0;
                            const STEPS: usize = 64;
                            let center = bounds.center();
                            let mut path = gpui::PathBuilder::stroke(preview_px(1.6));
                            for index in 0..=STEPS {
                                let x =
                                    -HALF_SPAN + index as f32 * (2.0 * HALF_SPAN / STEPS as f32);
                                let taper = 1.0 - (x / HALF_SPAN).powi(2);
                                let wave = (x * 0.42 - time * 9.0).sin() * 0.6
                                    + (x * 0.23 + time * 5.3).sin() * 0.3
                                    + (x * 0.71 - time * 13.0).sin() * 0.1;
                                let point = gpui::point(
                                    center.x + preview_px(x),
                                    center.y - preview_px(amplitude * taper * wave),
                                );
                                if index == 0 {
                                    path.move_to(point);
                                } else {
                                    path.line_to(point);
                                }
                            }
                            if let Ok(path) = path.build() {
                                window.paint_path(path, gpui::rgb(0xffffff));
                            }
                        },
                    )
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full(),
                )
                .into_any_element()
        }
        IndicatorStyle::Dots => preview_pill(52.0, 22.0)
            .gap(preview_px(3.0))
            .children((0..5).map(|index| {
                let slot = index as f32 - 2.0;
                let envelope = 1.0 - slot.abs() / 2.0 * 0.4;
                let jitter = 0.55 + 0.45 * (time * 12.0 + slot * 2.1).sin();
                let hop = 5.0 * level * envelope * jitter;
                div()
                    .relative()
                    .top(preview_px(-hop))
                    .size(preview_px(4.0))
                    .rounded_full()
                    .bg(gpui::rgb(0xffffff))
            }))
            .into_any_element(),
    }
}

/// "Oct 15" (this year) or "Oct 15, 2025".
fn profile_date(timestamp_ms: u64) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let (Some(date), Some(now)) = (local_time(timestamp_ms), local_time(now_ms())) else {
        return "earlier".into();
    };
    let month = MONTHS[date.tm_mon.rem_euclid(12) as usize];
    if date.tm_year == now.tm_year {
        format!("{month} {}", date.tm_mday)
    } else {
        format!("{month} {}, {}", date.tm_mday, date.tm_year + 1900)
    }
}

fn render_profile_tiles(tiles: &[(Glyph, u32, &'static str, String)]) -> AnyElement {
    div()
        .flex_none()
        .flex()
        .flex_row()
        .gap_3()
        .children(tiles.iter().cloned().map(|(icon, tint, label, value)| {
            compact_panel()
                .flex_1()
                .min_w(px(0.0))
                .px(px(14.0))
                .py(px(12.0))
                .flex()
                .items_center()
                .gap_3()
                .child(glyph_badge(icon, 34.0, tint))
                .child(
                    div()
                        .min_w(px(0.0))
                        .flex()
                        .flex_col()
                        .gap(px(2.0))
                        .child(
                            div()
                                .overflow_x_hidden()
                                .truncate()
                                .text_size(px(17.0))
                                .font_weight(FontWeight::BOLD)
                                .text_color(rgb(TEXT))
                                .child(value),
                        )
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(rgb(MUTED))
                                .child(label),
                        ),
                )
        }))
        .into_any_element()
}

/// Words dictated on each of the last 14 days, as bars.
fn render_daily_words(insights: &HistoryInsights, today: i64) -> AnyElement {
    const WEEKDAYS: [&str; 7] = ["Th", "Fr", "Sa", "Su", "Mo", "Tu", "We"];
    let days: Vec<(i64, usize)> = (0..14)
        .rev()
        .map(|back| (today - back, insights.words_on(today - back)))
        .collect();
    let most = days
        .iter()
        .map(|(_, words)| *words)
        .max()
        .unwrap_or(0)
        .max(1);
    compact_panel()
        .px_4()
        .pt_4()
        .pb_3()
        .flex()
        .items_end()
        .gap_2()
        .children(days.into_iter().map(|(day, words)| {
            let height = if words == 0 {
                3.0
            } else {
                6.0 + 74.0 * words as f32 / most as f32
            };
            div()
                .flex_1()
                .min_w(px(0.0))
                .flex()
                .flex_col()
                .items_center()
                .gap_1()
                .child(
                    div()
                        .text_size(px(9.5))
                        .text_color(rgb(if words == 0 { FAINT } else { MUTED }))
                        .child(if words == 0 {
                            String::new()
                        } else {
                            format_count(words)
                        }),
                )
                .child(
                    div()
                        .w_full()
                        .max_w(px(28.0))
                        .h(px(height))
                        .rounded(px(4.0))
                        .bg(if day == today {
                            rgb(ACCENT)
                        } else if words == 0 {
                            rgb(0x2a2a2e)
                        } else {
                            tinted(TINT_BLUE, 0.55)
                        }),
                )
                .child(
                    div()
                        .text_size(px(10.0))
                        .text_color(rgb(if day == today { TEXT } else { FAINT }))
                        .child(WEEKDAYS[day.rem_euclid(7) as usize]),
                )
        }))
        .into_any_element()
}

/// The apps you dictate into most, with each one's share.
fn render_top_apps(insights: &HistoryInsights) -> AnyElement {
    let total = insights.dictations.max(1);
    let rows: Vec<AnyElement> = insights
        .applications
        .iter()
        .take(5)
        .enumerate()
        .map(|(index, (name, count))| {
            let share = *count as f32 / total as f32;
            div()
                .px_4()
                .py(px(10.0))
                .flex()
                .items_center()
                .gap_3()
                .when(index > 0, |row| row.border_t_1().border_color(tone(LINE)))
                .child(
                    div()
                        .w(px(150.0))
                        .flex_none()
                        .overflow_x_hidden()
                        .truncate()
                        .text_size(px(12.5))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(name.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .h(px(6.0))
                        .rounded_full()
                        .bg(rgb(0x2a2a2e))
                        .child(
                            div()
                                .h_full()
                                .w(relative(share))
                                .rounded_full()
                                .bg(tinted(TINT_ORANGE, 0.8)),
                        ),
                )
                .child(
                    div()
                        .w(px(110.0))
                        .flex_none()
                        .text_size(px(11.0))
                        .text_color(rgb(MUTED))
                        .child(format!(
                            "{} {} · {:.0}%",
                            format_count(*count),
                            if *count == 1 {
                                "dictation"
                            } else {
                                "dictations"
                            },
                            share * 100.0
                        )),
                )
                .into_any_element()
        })
        .collect();
    if rows.is_empty() {
        return compact_panel()
            .px_4()
            .py_3()
            .text_size(px(11.5))
            .text_color(rgb(MUTED))
            .child("Apps you dictate into will appear here.")
            .into_any_element();
    }
    compact_panel().children(rows).into_any_element()
}

const WORD_MILESTONES: [(usize, &str); 4] = [
    (1_000, "First thousand"),
    (10_000, "Ten thousand"),
    (50_000, "Fifty thousand"),
    (100_000, "Hundred thousand"),
];

fn render_milestones(words: usize) -> AnyElement {
    div()
        .flex()
        .flex_row()
        .gap_3()
        .children(WORD_MILESTONES.into_iter().map(|(goal, title)| {
            let progress = (words as f32 / goal as f32).clamp(0.0, 1.0);
            let reached = words >= goal;
            compact_panel()
                .flex_1()
                .min_w(px(0.0))
                .px_3()
                .py_3()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .child(
                            div()
                                .text_size(px(12.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(title),
                        )
                        .child(
                            div()
                                .text_size(px(10.0))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(rgb(if reached { POSITIVE } else { FAINT }))
                                .child(if reached {
                                    "UNLOCKED".to_string()
                                } else {
                                    format!("{:.0}%", progress * 100.0)
                                }),
                        ),
                )
                .child(
                    div()
                        .h(px(4.0))
                        .w_full()
                        .rounded_full()
                        .bg(rgb(0x2a2a2a))
                        .child(
                            div()
                                .h_full()
                                .w(relative(progress))
                                .rounded_full()
                                .bg(rgb(if reached { POSITIVE } else { ACCENT })),
                        ),
                )
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(rgb(MUTED))
                        .child(format!("{} words", format_count(goal))),
                )
        }))
        .into_any_element()
}

fn sound_volume_index(settings: &AppSettings) -> usize {
    if !settings.sound_effects {
        return 0;
    }
    crate::desktop_ui::SOUND_VOLUME_STEPS[1..]
        .iter()
        .enumerate()
        .min_by(|(_, (_, left)), (_, (_, right))| {
            (settings.sound_effect_volume - *left)
                .abs()
                .total_cmp(&(settings.sound_effect_volume - *right).abs())
        })
        .map_or(0, |(index, _)| index + 1)
}

fn advance_highlight(current: usize, direction: i32, count: usize) -> usize {
    if direction < 0 {
        current.saturating_sub(1)
    } else {
        (current + 1).min(count - 1)
    }
}

fn mode_row(
    icon: Glyph,
    tint: u32,
    title: impl Into<String>,
    subtitle: impl Into<String>,
    selected: bool,
    applications: &[String],
    browser_hosts: &[String],
    catalog: &ApplicationCatalogState,
) -> Div {
    let title = title.into();
    let subtitle = subtitle.into();
    let has_activations = !applications.is_empty() || !browser_hosts.is_empty();
    let text = div()
        .flex_1()
        .min_w(px(0.0))
        .flex()
        .flex_col()
        .gap(px(4.0))
        .child(
            div()
                .w_full()
                .overflow_x_hidden()
                .text_size(px(12.5))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(TEXT))
                .truncate()
                .child(title),
        )
        .when(has_activations, |column| {
            column.child(mode_activation_icons(applications, browser_hosts, catalog))
        })
        .when(!has_activations, |column| {
            column.child(
                div()
                    .w_full()
                    .overflow_x_hidden()
                    .text_size(px(10.5))
                    .text_color(rgb(if selected { MUTED } else { FAINT }))
                    .truncate()
                    .child(subtitle),
            )
        });
    list_row(selected)
        .min_h(px(54.0))
        .flex_row()
        .items_center()
        .gap(px(10.0))
        .child(glyph_badge(icon, 30.0, tint))
        .child(text)
}

/// Each custom mode keeps a stable colour by position.
fn mode_tint(index: usize) -> u32 {
    const TINTS: [u32; 6] = [
        TINT_PURPLE,
        TINT_ORANGE,
        TINT_GREEN,
        TINT_PINK,
        TINT_TEAL,
        TINT_INDIGO,
    ];
    TINTS[index % TINTS.len()]
}

fn mode_activation_icons(
    applications: &[String],
    browser_hosts: &[String],
    catalog: &ApplicationCatalogState,
) -> AnyElement {
    let total = applications.len() + browser_hosts.len();
    let visible = total.min(5);
    let mut icons = Vec::with_capacity(visible + usize::from(total > visible));
    for name in applications.iter().take(visible) {
        let application = catalog.application_named(name);
        icons.push(application_icon(application, Some(name), 18.0));
    }
    for host in browser_hosts
        .iter()
        .take(visible.saturating_sub(icons.len()))
    {
        let initial = host
            .chars()
            .find(|character| character.is_alphanumeric())
            .map(|character| character.to_uppercase().to_string())
            .unwrap_or_else(|| "?".into());
        icons.push(
            div()
                .size(px(18.0))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(4.0))
                .border_1()
                .border_color(rgb(0x3f3f3f))
                .bg(rgb(0x282828))
                .text_size(px(8.0))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(rgb(TEXT_SOFT))
                .child(initial)
                .into_any_element(),
        );
    }
    if total > visible {
        icons.push(
            div()
                .h(px(18.0))
                .px_1()
                .flex()
                .items_center()
                .rounded(px(4.0))
                .bg(rgb(0x303030))
                .text_size(px(8.0))
                .text_color(rgb(MUTED))
                .child(format!("+{}", total - visible))
                .into_any_element(),
        );
    }
    div()
        .h(px(18.0))
        .flex()
        .items_center()
        .gap_1()
        .children(icons)
        .into_any_element()
}

fn application_icon(
    application: Option<&InstalledApplication>,
    fallback_name: Option<&str>,
    size: f32,
) -> AnyElement {
    if let Some(icon) = application.and_then(|application| application.icon.as_ref()) {
        return img(icon.clone())
            .size(px(size))
            .rounded(px(size * 0.22))
            .into_any_element();
    }
    let initial = fallback_name
        .and_then(|name| name.chars().find(|character| character.is_alphanumeric()))
        .map(|character| character.to_uppercase().to_string())
        .unwrap_or_else(|| "?".into());
    div()
        .size(px(size))
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(size * 0.22))
        .bg(rgb(0x343434))
        .text_size(px(size * 0.42))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(TEXT_SOFT))
        .child(initial)
        .into_any_element()
}

fn mode_settings_mut(settings: &mut AppSettings, selection: ModeSelection) -> &mut DictationMode {
    match selection {
        ModeSelection::Default => &mut settings.dictation_processing.default_mode,
        ModeSelection::Custom(index) => &mut settings.dictation_processing.modes[index],
    }
}

fn input_text(input: &ProcessingInput, cx: &App) -> String {
    input.entity.read(cx).text().trim().to_string()
}

fn browser_hosts(value: &str) -> Vec<String> {
    let mut hosts: Vec<_> = value
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .filter_map(|value| {
            let candidate = if value.contains("://") {
                value.to_owned()
            } else {
                format!("https://{value}")
            };
            url::Url::parse(&candidate)
                .ok()?
                .host_str()
                .map(str::to_lowercase)
        })
        .collect();
    hosts.sort();
    hosts.dedup();
    hosts
}

fn reorder_transformation(values: &mut Vec<String>, id: &str, target_index: usize) -> bool {
    let Some(from) = values.iter().position(|candidate| candidate == id) else {
        return false;
    };
    if from == target_index {
        return false;
    }
    let value = values.remove(from);
    let target = target_index.min(values.len());
    values.insert(target, value);
    true
}

fn apply_mode_inputs(inputs: &ModeInputs, mode: &mut DictationMode, is_default: bool, cx: &App) {
    mode.name = if is_default {
        "Global".into()
    } else {
        input_text(&inputs.name, cx)
    };
    mode.browser_hosts = if is_default {
        Vec::new()
    } else {
        browser_hosts(&input_text(&inputs.browser_hosts, cx))
    };
    mode.replacements = inputs
        .replacements
        .iter()
        .map(|inputs| TextReplacement {
            matched_phrase: inputs.matched_phrase.entity.read(cx).text().to_string(),
            output: inputs.output.entity.read(cx).text().to_string(),
        })
        .collect();
}

fn dictation_model_notice(
    status: SetupStatus,
    preparing: bool,
) -> Option<(&'static str, &'static str)> {
    if status.transcription_model {
        return None;
    }
    Some(if preparing {
        (
            "Preparing speech model",
            "Dictation will be available once your local speech model is ready.",
        )
    } else {
        (
            "Download a speech model to start dictating",
            "The dictation shortcut needs a local speech model. Choose one to download first.",
        )
    })
}

fn command_model_notice(status: &CommandModelStatus) -> Option<(&'static str, &'static str)> {
    match status {
        CommandModelStatus::Disabled | CommandModelStatus::Ready => None,
        CommandModelStatus::Loading => Some((
            "Preparing voice commands",
            "The command model is loading. Hotkey dictation does not require it.",
        )),
        CommandModelStatus::Failed => Some((
            "Voice commands are unavailable",
            "The command model could not load. Retry or turn off commands; hotkey dictation is unaffected.",
        )),
    }
}

fn setup_group_label(label: &'static str) -> Div {
    div()
        .px_1()
        .text_size(px(10.0))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(MUTED))
        .child(label)
}

fn setup_ready_badge() -> AnyElement {
    div()
        .h(px(28.0))
        .px_3()
        .flex()
        .items_center()
        .rounded_sm()
        .bg(rgb(POSITIVE_SURFACE))
        .text_size(px(11.0))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(rgb(POSITIVE))
        .child("Ready")
        .into_any_element()
}

const fn permission_warning_copy(kind: PermissionKind) -> (&'static str, &'static str) {
    match kind {
        PermissionKind::Microphone => (
            "Microphone access is off",
            "Hello cannot record dictation until microphone access is restored.",
        ),
        PermissionKind::InputMonitoring => (
            "Input Monitoring is off",
            "Hello cannot recognize the dictation shortcut in other apps.",
        ),
        PermissionKind::Accessibility => (
            "Accessibility is off",
            "Hello cannot paste completed dictation into the foreground app.",
        ),
    }
}

const fn permission_action_label(action: PermissionAction) -> &'static str {
    match action {
        PermissionAction::RequestMicrophone => "Allow",
        PermissionAction::OpenMicrophoneSettings => "Open Settings",
        PermissionAction::OpenInputMonitoringSettings
        | PermissionAction::OpenAccessibilitySettings => "Grant Access",
    }
}

const fn permission_warning_id(kind: PermissionKind) -> &'static str {
    match kind {
        PermissionKind::Microphone => "permission-warning-microphone",
        PermissionKind::InputMonitoring => "permission-warning-input-monitoring",
        PermissionKind::Accessibility => "permission-warning-accessibility",
    }
}

fn hotkey_modifiers(modifiers: GpuiModifiers) -> HotkeyModifiers {
    hotkey_modifiers_with_flags(modifiers, crate::suppression::physical_modifier_flags())
}

fn hotkey_modifiers_with_flags(modifiers: GpuiModifiers, flags: u64) -> HotkeyModifiers {
    let physical = crate::app_settings::modifiers_from_flags(flags);
    if !physical.is_empty() {
        return physical;
    }
    HotkeyModifiers {
        control: modifiers.control.then_some(Default::default()),
        option: modifiers.alt.then_some(Default::default()),
        shift: modifiers.shift.then_some(Default::default()),
        command: modifiers.platform.then_some(Default::default()),
        function: modifiers.function,
    }
}

fn hotkey_key(key: &str) -> std::result::Result<HotkeyKey, &'static str> {
    let special = match key {
        "space" => Some((49, "Space")),
        "tab" => Some((48, "Tab")),
        "enter" => Some((36, "Return")),
        "backspace" => Some((51, "Delete")),
        "up" => Some((126, "Up")),
        "down" => Some((125, "Down")),
        "left" => Some((123, "Left")),
        "right" => Some((124, "Right")),
        "pageup" => Some((116, "Page Up")),
        "pagedown" => Some((121, "Page Down")),
        "home" => Some((115, "Home")),
        "end" => Some((119, "End")),
        "delete" => Some((117, "Forward Delete")),
        "insert" => Some((114, "Help")),
        "f1" => Some((122, "F1")),
        "f2" => Some((120, "F2")),
        "f3" => Some((99, "F3")),
        "f4" => Some((118, "F4")),
        "f5" => Some((96, "F5")),
        "f6" => Some((97, "F6")),
        "f7" => Some((98, "F7")),
        "f8" => Some((100, "F8")),
        "f9" => Some((101, "F9")),
        "f10" => Some((109, "F10")),
        "f11" => Some((103, "F11")),
        "f12" => Some((111, "F12")),
        "f13" => Some((105, "F13")),
        "f14" => Some((107, "F14")),
        "f15" => Some((113, "F15")),
        "f16" => Some((106, "F16")),
        "f17" => Some((64, "F17")),
        "f18" => Some((79, "F18")),
        "f19" => Some((80, "F19")),
        "f20" => Some((90, "F20")),
        _ => None,
    };
    if let Some((code, label)) = special {
        return Ok(HotkeyKey {
            code,
            label: label.into(),
        });
    }
    let mut characters = key.chars();
    let Some(character) = characters.next() else {
        return Err("Unsupported key");
    };
    if characters.next().is_some() {
        return Err("Unsupported key");
    }
    let code = crate::keyboard::key_code_for(character).map_err(|_| "Unsupported key")?;
    Ok(HotkeyKey {
        code,
        label: character.to_uppercase().collect(),
    })
}

fn is_function_key(label: &str) -> bool {
    label
        .strip_prefix('F')
        .and_then(|number| number.parse::<u8>().ok())
        .is_some_and(|number| (1..=20).contains(&number))
}

fn detail_placeholder(message: &'static str) -> AnyElement {
    div()
        .flex_1()
        .h_full()
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(12.0))
        .text_color(rgb(FAINT))
        .child(message)
        .into_any_element()
}

fn detail_row(label: &'static str, value: impl Into<String>) -> AnyElement {
    div()
        .py_3()
        .flex()
        .items_start()
        .gap_4()
        .border_b_1()
        .border_color(tone(LINE))
        .child(
            div()
                .w(px(92.0))
                .flex_none()
                .text_size(px(11.0))
                .text_color(rgb(FAINT))
                .child(label),
        )
        .child(
            div()
                .flex_1()
                .text_size(px(12.0))
                .line_height(px(18.0))
                .text_color(rgb(TEXT_SOFT))
                .child(value.into()),
        )
        .into_any_element()
}

/// Badge glyph and tint for a command group, by what the group does.
fn command_group_style(task: &str) -> (Glyph, u32) {
    let task = task.to_lowercase();
    if task.contains("voice") {
        (Glyph::Sparkles, TINT_PURPLE)
    } else if task.contains("keyboard") || task.contains("key") {
        (Glyph::Keyboard, TINT_BLUE)
    } else if task.contains("dictation") || task.contains("text") {
        (Glyph::Mic, TINT_GREEN)
    } else if task.contains("browser") || task.contains("web") || task.contains("tab") {
        (Glyph::Globe, TINT_TEAL)
    } else if task.contains("window") || task.contains("app") {
        (Glyph::Apps, TINT_ORANGE)
    } else if task.contains("mode") {
        (Glyph::Layers, TINT_INDIGO)
    } else if task.contains("media") || task.contains("music") {
        (Glyph::Speed, TINT_PINK)
    } else {
        (Glyph::Cursor, TINT_GRAY)
    }
}

fn command_task(command: &CommandInfo) -> String {
    if let Some(group) = &command.group {
        return group.clone();
    }
    if command.id.starts_with("mode.") {
        "Voice control".into()
    } else if command.id.starts_with("dictation.") {
        "Dictation".into()
    } else if command.id.starts_with("meeting.") {
        "Meetings".into()
    } else {
        match command.description.as_str() {
            "Open application" => "Open applications",
            "Open website" => "Open websites",
            "Navigate active tab" => "Browser navigation",
            "Use keyboard shortcut" => "Keyboard",
            "Use application shortcut" | "Open application destination" => "Application navigation",
            _ => "Other",
        }
        .into()
    }
}

fn merged_catalog(compiled: &[CatalogCommand], personal: &StatusSnapshot) -> Vec<CatalogCommand> {
    let mut commands = compiled.to_vec();
    commands.extend(personal.catalog.iter().filter_map(|command| {
        let (phrase, aliases) = command.phrases.split_first()?;
        Some(CatalogCommand {
            task: command.group.clone(),
            scope: match &command.context {
                StatusContext::Global => CommandScope::Global,
                StatusContext::Application { application } => {
                    CommandScope::Application(application.clone())
                }
                StatusContext::BrowserHost { browser_host } => {
                    CommandScope::Browser(browser_host.clone())
                }
            },
            phrase: phrase.clone(),
            aliases: aliases.to_vec(),
            description: command.description.clone(),
            id: command.id.clone(),
        })
    }));
    commands
}

fn open_path(path: &std::path::Path) {
    if let Err(error) = ProcessCommand::new("/usr/bin/open").arg(path).spawn() {
        tracing::error!(%error, path = %path.display(), "could not open personal command path");
    }
}

fn command_scope(scope: &CommandScope) -> String {
    match scope {
        CommandScope::Sleeping | CommandScope::Global => "Global".into(),
        CommandScope::Application(application) => application.clone(),
        CommandScope::Browser(host) => host.clone(),
    }
}

/// The badge tint and name colour for one of the other people in a meeting.
fn speaker_tint(speaker: Option<u32>) -> (u32, u32) {
    const PALETTE: [(u32, u32); 6] = [
        (TINT_PURPLE, 0xd7a6f5),
        (TINT_ORANGE, 0xffc069),
        (TINT_GREEN, 0x7ee2a0),
        (TINT_PINK, 0xff8fa8),
        (TINT_TEAL, 0x8fe3f0),
        (TINT_INDIGO, 0xa9a8ff),
    ];
    match speaker {
        Some(id) if id > 0 => PALETTE[(id as usize - 1) % PALETTE.len()],
        _ => PALETTE[0],
    }
}

fn meeting_status(status: MeetingStatus) -> &'static str {
    match status {
        MeetingStatus::Starting => "Starting",
        MeetingStatus::Recording => "Recording",
        MeetingStatus::Transcribing => "Finalizing",
        MeetingStatus::Complete => "Complete",
        MeetingStatus::Interrupted => "Interrupted",
        MeetingStatus::Failed => "Failed",
    }
}

fn pane_list_card(
    id: &'static str,
    empty: Option<&'static str>,
    error_title: &'static str,
    error: Option<String>,
    rows: Vec<AnyElement>,
) -> gpui::Stateful<Div> {
    pane_list(id, empty, error_title, error)
        .rounded(px(PANEL_RADIUS))
        .border_1()
        .border_color(tone(LINE))
        .bg(tone(SURFACE))
        .overflow_x_hidden()
        .when(!rows.is_empty(), |list| {
            list.child(
                div()
                    .w(px(PANE_LIST_WIDTH - 2.0))
                    .p_2()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .children(rows),
            )
        })
}

fn timestamped_list_row(
    id: (&'static str, usize),
    selected: bool,
    age: String,
    body: Div,
) -> gpui::Stateful<Div> {
    list_row(selected)
        .id(id)
        .flex_row()
        .items_start()
        .justify_between()
        .gap_3()
        .child(body.min_w(px(0.0)))
        .child(
            div()
                .flex_none()
                .text_size(px(10.0))
                .text_color(rgb(if selected { TEXT_SOFT } else { FAINT }))
                .child(age),
        )
}

/// "52 min", "1 h 05 min" or "40 s" for a meeting or pause length.
fn meeting_length_label(milliseconds: u64) -> String {
    let seconds = milliseconds / 1_000;
    match seconds {
        0..=59 => format!("{seconds} s"),
        60..=3_599 => format!("{} min", seconds / 60),
        _ => format!("{} h {:02} min", seconds / 3_600, seconds % 3_600 / 60),
    }
}

#[cfg(target_os = "macos")]
#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}

/// Whether macOS lets Hello record system audio for meeting notes.
fn screen_capture_allowed() -> bool {
    // SAFETY: a side-effect-free permission query.
    unsafe { CGPreflightScreenCaptureAccess() }
}

/// Adds Hello to the Screen & System Audio Recording list and opens it.
fn request_screen_capture_access() {
    // SAFETY: shows the system prompt at most once; returns current access.
    let _ = unsafe { CGRequestScreenCaptureAccess() };
    if let Err(error) = ProcessCommand::new("/usr/bin/open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")
        .spawn()
    {
        tracing::error!(%error, "could not open Screen Recording settings");
    }
}

fn log_meeting_request_error(error: TrySendError<MeetingRequest>) {
    match error {
        TrySendError::Full(_) => tracing::warn!("meeting controller is busy"),
        TrySendError::Disconnected(_) => tracing::error!("meeting controller is unavailable"),
    }
}

fn meeting_duration(meeting: &MeetingManifest) -> u64 {
    meeting.duration_ms.unwrap_or_else(|| {
        if matches!(
            meeting.status,
            MeetingStatus::Starting | MeetingStatus::Recording
        ) {
            now_ms().saturating_sub(meeting.created_at_ms)
        } else {
            meeting
                .ended_at_ms
                .map_or(0, |ended| ended.saturating_sub(meeting.created_at_ms))
        }
    })
}

fn activity_context(reader: &EventReader) -> (Option<String>, Option<String>) {
    reader
        .current_context()
        .map(|(application, browser_url)| {
            (
                application.clone(),
                browser_url
                    .as_deref()
                    .and_then(|url| url::Url::parse(url).ok())
                    .and_then(|url| url.host_str().map(str::to_owned)),
            )
        })
        .unwrap_or_default()
}

fn event_age(timestamp_ms: u64) -> String {
    let seconds = now_ms().saturating_sub(timestamp_ms) / 1_000;
    match seconds {
        0..=59 => format!("{seconds}s ago"),
        60..=3_599 => format!("{}m ago", seconds / 60),
        3_600..=86_399 => format!("{}h ago", seconds / 3_600),
        _ => format!("{}d ago", seconds / 86_400),
    }
}

fn event_summary(event: &VoiceEvent) -> String {
    match event {
        VoiceEvent::SessionStarted { .. } => "Hello session started".into(),
        VoiceEvent::State { state, device, .. } => match state {
            VoiceState::Sleeping => "Voice commands went to sleep".into(),
            VoiceState::Listening => format!("Voice commands resumed on {device}"),
            VoiceState::Dictating => format!("Dictation started on {device}"),
            VoiceState::Transcribing => "Dictation is being transcribed".into(),
            VoiceState::Stopping => "Hello is stopping".into(),
        },
        VoiceEvent::Transcript {
            phase,
            latency_ms,
            text,
            ..
        } => match phase {
            TranscriptPhase::Started => "Speech recognition started".into(),
            TranscriptPhase::Updated => format!("Heard \"{text}\" so far"),
            TranscriptPhase::Completed => format!("Heard \"{text}\" in {latency_ms} ms"),
        },
        VoiceEvent::Command {
            heard,
            command,
            outcome,
            ..
        } => {
            let command = command.as_deref().unwrap_or("an unmatched command");
            match outcome {
                CommandOutcome::Ignored => format!("Ignored \"{heard}\""),
                CommandOutcome::Woke => format!("\"{heard}\" woke Hello"),
                CommandOutcome::Slept => format!("\"{heard}\" put voice commands to sleep"),
                CommandOutcome::Submitted => format!("Sent {command} for \"{heard}\""),
                CommandOutcome::Executed => format!("Ran {command} for \"{heard}\""),
                CommandOutcome::Failed(error) => format!("Could not run {command}: {error}"),
            }
        }
        VoiceEvent::Dictation {
            phase,
            text,
            processing,
            ..
        } => {
            let description = match phase {
                DictationPhase::Started => "Dictation started".into(),
                DictationPhase::Discarded => "A brief dictation was discarded".into(),
                DictationPhase::Cancelled => "Dictation was cancelled".into(),
                DictationPhase::Transcribing => "Dictation is being transcribed".into(),
                DictationPhase::Pasted => text_or("Dictation was pasted", text),
                DictationPhase::Edited => text_or("Selected text was edited", text),
                DictationPhase::Logged => text_or("Journal entry was saved", text),
                DictationPhase::Repasted => text_or("The last dictation was pasted again", text),
                DictationPhase::MeetingPasted => text_or("New meeting transcript was pasted", text),
                DictationPhase::Failed(error) => format!("Dictation failed: {error}"),
            };
            processing
                .as_ref()
                .map_or(description.clone(), |processing| {
                    processing.fallback.as_ref().map_or_else(
                        || {
                            format!(
                                "{description} · {} profile · {} ms",
                                processing.profile, processing.latency_ms
                            )
                        },
                        |error| {
                            format!(
                                "{description} · {} profile fell back to raw after {} ms: {}",
                                processing.profile, processing.latency_ms, error
                            )
                        },
                    )
                })
        }
        VoiceEvent::Context {
            application,
            browser_url,
            ..
        } => match (application, browser_url) {
            (Some(application), Some(url)) => format!("Context changed to {application} / {url}"),
            (Some(application), None) => format!("Context changed to {application}"),
            _ => "Foreground context became unavailable".into(),
        },
        VoiceEvent::ApiServerStarted { port, .. } => {
            format!("Local API started on port {port}")
        }
        VoiceEvent::ApiServerStopped { .. } => "Local API stopped".into(),
        VoiceEvent::ApiAuthFailed { method, path, .. } => {
            format!("Rejected unauthenticated {method} {path}")
        }
    }
}

fn text_or(prefix: &str, text: &str) -> String {
    if text.is_empty() {
        prefix.into()
    } else {
        format!("{prefix}: \"{text}\"")
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::personal_commands::StatusExecution;

    fn editor_fixture(window: &mut Window, cx: &mut Context<AppWindow>) -> AppWindow {
        AppWindow::new(
            PathBuf::from("unused-preview-events.ndjson"),
            crate::config::voice_control(),
            sync_channel(1).0,
            crate::dictation_indicator::channel().0,
            None,
            None,
            Some(AppWindowPreview {
                pane: DeveloperPane::Modes,
                transcription_picker: None,
                onboarding: false,
                confirm_model_deletion: false,
                open_transformation_picker: false,
                select_global_mode: true,
                permissions_missing: false,
                model_missing: false,
                command_model_missing: false,
                open_history_retention: false,
                confirm_release_microphone: false,
                update_available: false,
            }),
            window,
            cx,
        )
    }

    #[gpui::test]
    fn login_item_response_reconciles_optimistic_toggle_and_unknown_state(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = cx.add_window_view(editor_fixture);
        cx.update(|_, cx| {
            view.update(cx, |view, _| {
                view.launch_at_login_status = None;
                assert!(view.apply_login_item_response(LoginItemResponse {
                    request: LoginItemRequest::Status,
                    result: Ok(LoginItemStatus::Disabled),
                }));
                assert_eq!(view.launch_at_login_status, Some(LoginItemStatus::Disabled));
                view.launch_at_login_toggle.set_enabled(true);
                view.launch_at_login_error = Some("Registration failed".into());
                // Even with unchanged confirmed status and error, rollback needs a redraw.
                assert!(view.apply_login_item_response(LoginItemResponse {
                    request: LoginItemRequest::SetEnabled(true),
                    result: Err(crate::login_item::LoginItemFailure {
                        status: Some(LoginItemStatus::Disabled),
                        message: "Registration failed".into(),
                    }),
                }));
                assert!(!view.launch_at_login_toggle.enabled());
                assert!(!view.apply_login_item_response(LoginItemResponse {
                    request: LoginItemRequest::SetEnabled(true),
                    result: Err(crate::login_item::LoginItemFailure {
                        status: Some(LoginItemStatus::Disabled),
                        message: "Registration failed".into(),
                    }),
                }));
                // Clearing only the error still needs a redraw.
                assert!(view.apply_login_item_response(LoginItemResponse {
                    request: LoginItemRequest::SetEnabled(false),
                    result: Ok(LoginItemStatus::Disabled),
                }));
                assert!(view.launch_at_login_error.is_none());
                assert!(view.apply_login_item_response(LoginItemResponse {
                    request: LoginItemRequest::SetEnabled(true),
                    result: Ok(LoginItemStatus::RequiresApproval),
                }));
                assert_eq!(
                    view.launch_at_login_status,
                    Some(LoginItemStatus::RequiresApproval)
                );
                assert!(view.launch_at_login_error.is_none());
                assert!(view.apply_login_item_response(LoginItemResponse {
                    request: LoginItemRequest::Status,
                    result: Ok(LoginItemStatus::Enabled),
                }));
                assert!(view.launch_at_login_toggle.enabled());
                assert!(!view.apply_login_item_response(LoginItemResponse {
                    request: LoginItemRequest::Status,
                    result: Ok(LoginItemStatus::Enabled),
                }));
            });
        });
    }

    fn reject_settings_save(_: &AppSettings) -> color_eyre::Result<()> {
        Err(color_eyre::eyre::eyre!("fixture persistence failure"))
    }

    #[gpui::test]
    fn model_deletion_confirms_first_and_spares_the_dictation_model(cx: &mut gpui::TestAppContext) {
        let (view, cx) = cx.add_window_view(editor_fixture);
        cx.update(|_, cx| {
            view.update(cx, |view, _| {
                view.transcription_preview_installed = Some(true);
                view.transcription_picker_language = Some("en".into());
                let current = definition(view.settings.transcription.model);
                let other = crate::transcription_models::choices_for_runtime("en")
                    .into_iter()
                    .map(|choice| choice.model)
                    .find(|model| {
                        matches!(model.runtime, ModelRuntime::Gguf(_)) && model.id != current.id
                    })
                    .unwrap();

                assert_eq!(
                    view.transcription_model_deletion(current, true),
                    ModelDeletion::Unavailable
                );
                assert_eq!(
                    view.transcription_model_deletion(other, false),
                    ModelDeletion::Unavailable
                );
                assert_eq!(
                    view.transcription_model_deletion(other, true),
                    ModelDeletion::Offered
                );

                view.delete_transcription_model(other);
                assert_eq!(
                    view.transcription_model_deletion(other, true),
                    ModelDeletion::Confirming
                );
                view.clear_transcription_error();
                assert_eq!(
                    view.transcription_model_deletion(other, true),
                    ModelDeletion::Offered
                );

                view.delete_transcription_model(other);
                view.delete_transcription_model(other);
                assert_eq!(view.transcription_deletion, None);
                assert_eq!(view.transcription_deletion_error, None);

                view.transcription_status.model = Some(other.id);
                assert_eq!(
                    view.transcription_model_deletion(other, true),
                    ModelDeletion::Unavailable
                );
            });
        });
    }

    #[gpui::test]
    fn activity_log_is_only_read_when_an_observing_pane_needs_it(cx: &mut gpui::TestAppContext) {
        let path = std::env::temp_dir().join(format!(
            "hello-lazy-activity-{}-{}.ndjson",
            std::process::id(),
            now_ms()
        ));
        std::fs::write(&path, b"{\"kind\":\"context\",\"timestamp_ms\":1,\"application\":\"Fixture\",\"browser_url\":null}\n").unwrap();
        let (view, cx) = cx.add_window_view(editor_fixture);
        cx.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.event_reader = EventReader::open(&path);
                view.pane = Pane::Settings;
                view.reload_events();
                assert!(view.event_reader.events().is_empty());
                view.select_pane(Pane::Commands, cx);
                assert_eq!(view.current_context.0.as_deref(), Some("Fixture"));

                std::fs::write(
                    &path,
                    b"{\"kind\":\"session_started\",\"timestamp_ms\":2}\n",
                )
                .unwrap();
                view.select_pane(Pane::Settings, cx);
                view.reload_events();
                assert_eq!(view.current_context.0.as_deref(), Some("Fixture"));
                view.select_pane(Pane::Activity, cx);
                assert_eq!(view.current_context, (None, None));
                assert_eq!(view.activity.session_started_at, Some(2));
            });
        });
        std::fs::remove_file(path).unwrap();
    }

    #[gpui::test]
    fn failed_shortcut_save_stays_in_capture_and_preserves_pending_edits(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = cx.add_window_view(editor_fixture);
        cx.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.settings.transcription.recognition_hints = "Unsaved vocabulary".into();
                view.schedule_settings_save(cx);
                let generation = view.settings_save_generation;
                let before = serde_json::to_value(&view.settings).unwrap();
                view.settings_save_override = Some(reject_settings_save);
                view.hotkey_capture = HotkeyCaptureState::Listening {
                    kind: HotkeyKind::Dictation,
                    modifiers: HotkeyModifiers::default(),
                    message: None,
                    started_at: Instant::now(),
                };
                let binding = HotkeyBinding {
                    modifiers: HotkeyModifiers {
                        control: Some(ModifierSide::Either),
                        ..Default::default()
                    },
                    key: None,
                };
                view.save_hotkey_binding(binding.clone(), cx);
                assert!(view.hotkey_capture.is_listening());
                assert_eq!(serde_json::to_value(&view.settings).unwrap(), before);
                assert!(view.settings_dirty);
                assert_eq!(view.settings_save_generation, generation);
                assert_eq!(
                    view.settings_load_error.as_deref(),
                    Some("fixture persistence failure")
                );

                // An immediate retry commits the unrelated draft as well, and only
                // then ends capture and invalidates the earlier debounce.
                view.settings_save_override = None;
                view.save_hotkey_binding(binding.clone(), cx);
                assert!(matches!(
                    view.hotkey_capture,
                    HotkeyCaptureState::Saved { .. }
                ));
                assert_eq!(view.settings.dictation_hotkey, binding);
                assert_eq!(
                    view.settings.transcription.recognition_hints,
                    "Unsaved vocabulary"
                );
                assert!(!view.settings_dirty);
                assert_ne!(view.settings_save_generation, generation);
                assert!(view.settings_load_error.is_none());
            })
        });
    }

    #[gpui::test]
    fn failed_retention_save_keeps_previous_choice_and_reports_the_error(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = cx.add_window_view(editor_fixture);
        cx.update(|_, cx| {
            view.update(cx, |view, cx| {
                view.settings_save_override = Some(reject_settings_save);
                view.set_history_retention(HistoryRetention::Off, cx);
                assert_eq!(view.settings.history_retention, HistoryRetention::Week);
                assert_eq!(
                    view.settings_load_error.as_deref(),
                    Some("fixture persistence failure")
                );
            })
        });
        cx.run_until_parked();
        assert!(cx.debug_bounds("settings-error").is_some());
    }

    #[gpui::test]
    fn mode_rows_preserve_edits_across_add_delete_and_supersede_old_debounces(
        cx: &mut gpui::TestAppContext,
    ) {
        let (view, cx) = cx.add_window_view(editor_fixture);
        cx.update(|_, cx| {
            view.update(cx, |view, cx| {
                let name = view
                    .mode_inputs_for(ModeSelection::Custom(0))
                    .name
                    .entity
                    .clone();
                name.update(cx, |input, cx| input.set_text("Pending mode name", cx));
                view.sync_processing_settings(cx);
                assert!(view.settings_dirty);
                let pending_generation = view.settings_save_generation;

                view.settings_save_override = Some(reject_settings_save);
                assert!(view.add_mode(cx).is_none());
                view.delete_mode(0, cx);
                assert_eq!(view.mode_editors.modes.len(), 1);
                assert_eq!(
                    view.settings_candidate().dictation_processing.modes[0].name,
                    "Pending mode name"
                );
                assert_eq!(
                    view.settings.dictation_processing.modes[0].name,
                    "Work notes"
                );
                assert_eq!(view.settings_save_generation, pending_generation);

                view.settings_save_override = None;
                assert!(view.add_mode(cx) == Some(ModeSelection::Custom(1)));
                assert_eq!(view.mode_editors.modes.len(), 2);
                assert_eq!(
                    view.settings.dictation_processing.modes[0].name,
                    "Pending mode name"
                );
                let second_name = view
                    .mode_inputs_for(ModeSelection::Custom(1))
                    .name
                    .entity
                    .clone();
                second_name.update(cx, |input, cx| input.set_text("Survivor", cx));
                view.sync_processing_settings(cx);
                view.delete_mode(0, cx);
                assert_eq!(view.mode_editors.modes.len(), 1);
                assert_eq!(
                    view.mode_settings(ModeSelection::Custom(0)).name,
                    "Survivor"
                );
                assert_eq!(view.settings.dictation_processing.modes[0].name, "Survivor");
                assert!(!view.settings_dirty);

                // If either obsolete debounce writes, the injected failure becomes
                // visible and makes the assertions below fail.
                view.settings_save_override = Some(reject_settings_save);
            })
        });
        cx.run_until_parked();
        cx.background_executor
            .advance_clock(Duration::from_millis(300));
        cx.run_until_parked();
        cx.update(|_, cx| {
            view.update(cx, |view, cx| {
                assert!(view.settings_load_error.is_none());
                assert!(!view.settings_dirty);
                let name = view
                    .mode_inputs_for(ModeSelection::Custom(0))
                    .name
                    .entity
                    .clone();
                name.update(cx, |input, cx| input.set_text("Later draft", cx));
                view.sync_processing_settings(cx);
            })
        });
        cx.run_until_parked();
        cx.background_executor
            .advance_clock(Duration::from_millis(300));
        cx.run_until_parked();
        cx.update(|_, cx| {
            view.update(cx, |view, _| {
                assert!(view.settings_dirty);
                assert_eq!(
                    view.settings_load_error.as_deref(),
                    Some("fixture persistence failure")
                );
                assert_eq!(
                    view.settings_candidate().dictation_processing.modes[0].name,
                    "Later draft"
                );
                assert_eq!(view.settings.dictation_processing.modes[0].name, "Survivor");
            })
        });
    }

    #[gpui::test]
    fn settings_content_stays_inside_the_pane_at_supported_widths(cx: &mut gpui::TestAppContext) {
        struct SettingsLayout;

        impl Render for SettingsLayout {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                window_frame()
                    .child(sidebar_frame().w(px(SIDEBAR_WIDTH)))
                    .child(
                        div().flex_1().min_w_0().h_full().child(settings_pane(
                            div()
                                .child(settings_panel().child(settings_row(
                                    "Microphone",
                                    "Automatically chooses the preferred available input",
                                    disclosure_button("Automatic"),
                                )))
                                .child(settings_copy(
                                    "Release the microphone when idle?",
                                    "Hello will open the microphone when you press the shortcut. This adds a start-up delay and removes pre-roll, so the beginning of speech may be missed if you speak right away.",
                                )),
                        )),
                    )
            }
        }

        let (_, cx) = cx.add_window_view(|_, _| SettingsLayout);
        for width in [MINIMUM_WIDTH, WINDOW_WIDTH, 1400.0] {
            cx.simulate_resize(size(px(width), px(MINIMUM_HEIGHT)));
            cx.update(|window, _| window.refresh());
            cx.run_until_parked();
            let column = cx
                .debug_bounds("settings-content")
                .expect("content was rendered");
            let sidebar = SIDEBAR_WIDTH;
            let available = width - sidebar - 64.0;
            let expected_width = available.min(PANE_CONTENT_WIDTH);
            assert_eq!(column.size.width, px(expected_width));
            assert_eq!(
                column.origin.x,
                px(sidebar + 32.0 + (available - expected_width) / 2.0)
            );
        }
    }

    #[test]
    fn command_model_failure_has_recovery_without_blocking_dictation() {
        assert!(command_model_notice(&CommandModelStatus::Ready).is_none());
        assert!(command_model_notice(&CommandModelStatus::Disabled).is_none());
        assert_eq!(
            command_model_notice(&CommandModelStatus::Loading)
                .unwrap()
                .0,
            "Preparing voice commands"
        );
        let (_, description) = command_model_notice(&CommandModelStatus::Failed).unwrap();
        assert!(description.contains("Retry"));
        assert!(description.contains("dictation is unaffected"));
    }

    #[test]
    fn opening_the_app_prioritizes_settings_and_missing_permissions() {
        assert_eq!(Pane::default(), Pane::Settings);
        let ready = SetupStatus {
            microphone: PermissionState::Ready,
            input_monitoring: PermissionState::Ready,
            accessibility: PermissionState::Ready,
            transcription_model: true,
        };
        assert_eq!(Pane::Modes.on_reopen(ready), Pane::Modes);
        for status in [
            SetupStatus {
                microphone: PermissionState::NeedsRequest,
                ..ready
            },
            SetupStatus {
                input_monitoring: PermissionState::NeedsSettings,
                ..ready
            },
            SetupStatus {
                accessibility: PermissionState::NeedsSettings,
                ..ready
            },
        ] {
            assert_eq!(Pane::Modes.on_reopen(status), Pane::Settings);
            assert_eq!(Pane::Settings.on_reopen(status), Pane::Settings);
        }
        assert_eq!(
            Pane::from_developer(
                serde_json::from_str::<DeveloperPane>(r#""replacements""#).unwrap()
            ),
            Pane::Modes
        );
    }

    #[test]
    fn missing_dictation_model_has_a_notice_even_with_all_permissions_granted() {
        let mut status = SetupStatus {
            microphone: PermissionState::Ready,
            input_monitoring: PermissionState::Ready,
            accessibility: PermissionState::Ready,
            transcription_model: false,
        };
        let (title, description) = dictation_model_notice(status, false).unwrap();
        assert_eq!(title, "Download a speech model to start dictating");
        assert!(description.contains("dictation shortcut"));
        assert!(!status.ready());

        assert_eq!(
            dictation_model_notice(status, true).unwrap().0,
            "Preparing speech model"
        );
        // A failed or cancelled preparation must expose the download action again.
        assert_eq!(dictation_model_notice(status, false).unwrap().0, title);

        status.transcription_model = true;
        assert!(dictation_model_notice(status, false).is_none());
        assert!(dictation_model_notice(status, true).is_none());

        // Permission failures have their own setup UI.
        status.microphone = PermissionState::NeedsRequest;
        assert!(dictation_model_notice(status, false).is_none());
    }

    #[test]
    fn sidebar_update_button_only_appears_for_an_available_update() {
        use crate::sparkle::UpdateStatus;

        for (status, visible) in [
            (UpdateStatus::Unavailable, false),
            (UpdateStatus::Idle, false),
            (UpdateStatus::Checking, false),
            (UpdateStatus::UpToDate, false),
            (UpdateStatus::UpdateAvailable, true),
        ] {
            assert_eq!(sidebar_update_button(status, false).is_some(), visible);
        }
    }

    #[gpui::test]
    fn sidebar_update_click_dispatches_only_outside_preview(cx: &mut gpui::TestAppContext) {
        struct UpdateButton {
            preview: bool,
        }

        impl Render for UpdateButton {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                sidebar_update_button(crate::sparkle::UpdateStatus::UpdateAvailable, self.preview)
                    .unwrap()
                    .debug_selector(|| "sidebar-update".into())
            }
        }

        let calls = Rc::new(std::cell::Cell::new(0));
        cx.update(|cx| {
            let calls = calls.clone();
            cx.on_action(move |_: &CheckForUpdates, _| calls.set(calls.get() + 1));
        });

        for preview in [true, false] {
            let (_, cx) = cx.add_window_view(|_, _| UpdateButton { preview });
            cx.update(|window, cx| {
                // Test windows do not activate automatically; an inactive window masks the bug.
                window.activate_window();
                assert!(cx.active_window() == Some(window.window_handle()));
            });
            cx.run_until_parked();
            let button = cx.debug_bounds("sidebar-update").unwrap();
            cx.simulate_click(button.center(), GpuiModifiers::default());
            assert_eq!(calls.get(), usize::from(!preview), "preview={preview}");
        }
    }

    #[gpui::test]
    fn microphone_picker_occludes_the_controls_beneath_it(cx: &mut gpui::TestAppContext) {
        #[derive(Default)]
        struct Observed {
            beneath_clicks: usize,
            beneath_hovered: bool,
            chosen: Option<Option<String>>,
            dismissed: usize,
        }

        struct PickerOverControl {
            observed: Rc<RefCell<Observed>>,
        }

        impl Render for PickerOverControl {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                let observed = self.observed.clone();
                let hovered = self.observed.clone();
                let chosen = self.observed.clone();
                let dismissed = self.observed.clone();
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .relative()
                            .w(px(220.0))
                            .child(disclosure_button("Automatic").id("anchor"))
                            .child(deferred(microphone_picker_menu(
                                vec![None, Some("Studio Microphone".into())],
                                None,
                                None,
                                move |device, _, _| {
                                    chosen.borrow_mut().chosen = Some(device.clone());
                                },
                                move |_, _, _| dismissed.borrow_mut().dismissed += 1,
                            )))
                            .child(
                                div()
                                    .id("beneath")
                                    .debug_selector(|| "beneath".into())
                                    .w(px(220.0))
                                    .h(px(120.0))
                                    .on_hover(move |is_hovered, _, _| {
                                        if *is_hovered {
                                            hovered.borrow_mut().beneath_hovered = true;
                                        }
                                    })
                                    .on_click(move |_, _, _| {
                                        observed.borrow_mut().beneath_clicks += 1;
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .id("outside")
                            .debug_selector(|| "outside".into())
                            .w(px(220.0))
                            .h(px(200.0)),
                    )
            }
        }

        let observed = Rc::new(RefCell::new(Observed::default()));
        let (_, cx) = cx.add_window_view(|_, _| PickerOverControl {
            observed: observed.clone(),
        });
        cx.update(|window, cx| {
            window.activate_window();
            assert!(cx.active_window() == Some(window.window_handle()));
        });
        cx.run_until_parked();
        let beneath = cx.debug_bounds("beneath").unwrap();
        let menu = cx.debug_bounds("microphone-picker").unwrap();
        let second_choice = cx.debug_bounds("microphone-choice-1").unwrap();
        assert!(
            menu.contains(&second_choice.center()) && beneath.contains(&second_choice.center()),
            "fixture must place a menu choice over the control beneath"
        );

        cx.simulate_mouse_move(second_choice.center(), None, GpuiModifiers::default());
        cx.run_until_parked();
        assert!(
            !observed.borrow().beneath_hovered,
            "hover leaked beneath the menu"
        );

        cx.simulate_click(second_choice.center(), GpuiModifiers::default());
        cx.run_until_parked();
        {
            let observed = observed.borrow();
            assert_eq!(observed.beneath_clicks, 0, "click leaked beneath the menu");
            assert_eq!(observed.chosen, Some(Some("Studio Microphone".into())));
            assert_eq!(
                observed.dismissed, 0,
                "choosing must not also count as dismissal"
            );
        }

        let outside = cx.debug_bounds("outside").unwrap();
        cx.simulate_click(outside.center(), GpuiModifiers::default());
        cx.run_until_parked();
        assert_eq!(observed.borrow().dismissed, 1);
    }

    #[test]
    fn inactive_hotkey_controls_keep_their_idle_width() {
        assert_eq!(hotkey_control_width(false, 146.0, 270.0), 146.0);
        assert_eq!(hotkey_control_width(true, 146.0, 270.0), 270.0);
        assert_eq!(hotkey_control_intensity(false, 0.8), 0.0);
        assert_eq!(hotkey_control_intensity(true, 0.8), 0.8);
    }

    #[test]
    fn shortcut_side_changes_reject_overlap_without_changing_the_saved_binding() {
        let mut settings = AppSettings::default();
        settings.dictation_hotkey.modifiers.option = Some(ModifierSide::Left);
        settings.paste_last_hotkey = Some(HotkeyBinding {
            modifiers: HotkeyModifiers {
                option: Some(ModifierSide::Right),
                ..Default::default()
            },
            key: None,
        });
        let original = settings.dictation_hotkey.clone();

        assert!(
            hotkey_side_binding(&settings, HotkeyKind::Dictation, ModifierSide::Right).is_none()
        );
        assert!(
            hotkey_side_binding(&settings, HotkeyKind::Dictation, ModifierSide::Either).is_none()
        );
        assert_eq!(settings.dictation_hotkey, original);
        assert_eq!(
            hotkey_side_binding(&settings, HotkeyKind::Dictation, ModifierSide::Left),
            Some(original)
        );

        settings.paste_last_hotkey = None;
        let candidate =
            hotkey_side_binding(&settings, HotkeyKind::Dictation, ModifierSide::Right).unwrap();
        assert_eq!(
            standalone_modifier_side(&candidate),
            Some(ModifierSide::Right)
        );
    }

    #[test]
    fn side_selection_only_changes_a_standalone_modifier() {
        let mut standalone = HotkeyBinding {
            modifiers: HotkeyModifiers {
                option: Some(ModifierSide::Left),
                ..Default::default()
            },
            key: None,
        };
        set_standalone_modifier_side(&mut standalone, ModifierSide::Either);
        assert_eq!(
            standalone_modifier_side(&standalone),
            Some(ModifierSide::Either)
        );

        let mut chord = HotkeyBinding {
            modifiers: HotkeyModifiers {
                option: Some(ModifierSide::Left),
                ..Default::default()
            },
            key: Some(HotkeyKey {
                code: 0,
                label: "A".into(),
            }),
        };
        set_standalone_modifier_side(&mut chord, ModifierSide::Right);
        assert_eq!(chord.modifiers.option, Some(ModifierSide::Left));
    }

    #[test]
    fn production_navigation_keeps_commands_available_as_an_opt_in() {
        assert_eq!(
            Pane::all(false),
            vec![
                Pane::Profile,
                Pane::General,
                Pane::Settings,
                Pane::Appearance,
                Pane::Sounds,
                Pane::Modes,
                Pane::Commands,
                Pane::Meetings,
                Pane::History,
            ]
        );
    }

    #[test]
    fn developer_navigation_appends_developer_panes_in_order() {
        assert_eq!(
            Pane::all(true),
            vec![
                Pane::Profile,
                Pane::General,
                Pane::Settings,
                Pane::Appearance,
                Pane::Sounds,
                Pane::Modes,
                Pane::Commands,
                Pane::Meetings,
                Pane::History,
                Pane::HudLab,
                Pane::Activity,
            ]
        );
    }

    #[test]
    fn application_catalog_loads_only_when_explicitly_requested() {
        let loading = ApplicationCatalogState::Loading;
        let loaded = ApplicationCatalogState::Loaded(Vec::new());

        assert!(application_catalog_load_needed(false, &loading, false));
        assert!(!application_catalog_load_needed(false, &loading, true));
        assert!(!application_catalog_load_needed(false, &loaded, false));
        assert!(!application_catalog_load_needed(true, &loading, false));
    }

    #[test]
    fn hotkey_capture_preserves_the_function_modifier() {
        let modifiers = hotkey_modifiers_with_flags(
            GpuiModifiers {
                function: true,
                ..Default::default()
            },
            0,
        );

        assert!(modifiers.function);
        assert_eq!(modifiers.count(), 1);
    }

    #[test]
    fn toggle_spring_is_frame_rate_independent_and_settles() {
        let simulate = |frame_rate: u32| {
            let mut spring = ToggleSpring::new(false);
            spring.set_enabled(true);
            for _ in 0..frame_rate {
                spring.advance(Duration::from_secs_f32(1.0 / frame_rate as f32));
            }
            spring
        };

        let at_60_hz = simulate(60);
        let at_120_hz = simulate(120);
        assert!(at_60_hz.is_settled());
        assert!(at_120_hz.is_settled());
        assert!((at_60_hz.position - at_120_hz.position).abs() < 0.001);
        assert_eq!(at_60_hz.position, 1.0);
    }

    #[test]
    fn recent_activity_keeps_context_older_than_the_visible_window() {
        let path = std::env::temp_dir().join(format!(
            "voice-control-app-window-{}-{}.ndjson",
            std::process::id(),
            crate::events::now_ms()
        ));
        let mut lines = vec![
            serde_json::to_string(&VoiceEvent::Context {
                timestamp_ms: 1,
                application: Some("Brave Browser".into()),
                browser_url: Some("https://meet.google.com/example".into()),
            })
            .unwrap(),
        ];
        lines.extend((0..=ACTIVITY_LIMIT).map(|timestamp_ms| {
            serde_json::to_string(&VoiceEvent::State {
                timestamp_ms: timestamp_ms as u64 + 2,
                state: VoiceState::Listening,
                device: "Test microphone".into(),
            })
            .unwrap()
        }));
        fs::write(&path, lines.join("\n")).unwrap();

        let mut reader = EventReader::open(&path);
        reader.refresh().unwrap();

        assert_eq!(reader.recent(ACTIVITY_LIMIT).len(), ACTIVITY_LIMIT);
        assert_eq!(
            activity_context(&reader),
            (Some("Brave Browser".into()), Some("meet.google.com".into()))
        );
        fs::remove_file(path).unwrap();
    }

    #[test]
    fn browser_hosts_are_canonicalized_and_deduplicated() {
        assert_eq!(
            browser_hosts(" https://www.Reddit.com/r/rust, , reddit.com, github.com/path"),
            ["github.com", "reddit.com", "www.reddit.com"]
        );
    }

    #[test]
    fn transformations_reorder_in_both_directions() {
        let mut transformations = vec!["one".into(), "two".into(), "three".into()];

        assert!(reorder_transformation(&mut transformations, "one", 2));
        assert_eq!(transformations, ["two", "three", "one"]);
        assert!(reorder_transformation(&mut transformations, "one", 0));
        assert_eq!(transformations, ["one", "two", "three"]);
    }

    #[test]
    fn transformation_reorder_ignores_same_position_and_missing_ids() {
        let mut transformations = vec!["one".into(), "two".into()];

        assert!(!reorder_transformation(&mut transformations, "one", 0));
        assert!(!reorder_transformation(&mut transformations, "missing", 1));
        assert_eq!(transformations, ["one", "two"]);
    }

    #[test]
    fn picker_arrows_stop_at_the_ends() {
        assert_eq!(advance_highlight(0, -1, 4), 0);
        assert_eq!(advance_highlight(2, 1, 4), 3);
        assert_eq!(advance_highlight(3, 1, 4), 3);
    }

    #[test]
    fn selected_transcription_model_is_not_active_until_installed() {
        let selection = TranscriptionSelection::default();
        let model = definition(selection.model);

        assert!(!transcription_selection_is_active(
            &selection,
            model,
            &selection.language,
            false
        ));
        assert!(transcription_selection_is_active(
            &selection,
            model,
            &selection.language,
            true
        ));
    }

    #[test]
    fn custom_catalog_preserves_groups() {
        let compiled = vec![CatalogCommand {
            task: "Built in".into(),
            scope: CommandScope::Global,
            phrase: "built in".into(),
            aliases: Vec::new(),
            description: "Compiled command".into(),
            id: "compiled".into(),
        }];
        let status = StatusSnapshot {
            catalog: vec![
                crate::personal_commands::StatusCommand {
                    id: "default".into(),
                    phrases: vec!["default custom".into()],
                    group: "Other".into(),
                    description: "Custom command".into(),
                    context: StatusContext::Global,
                    execution: StatusExecution::Handler,
                },
                crate::personal_commands::StatusCommand {
                    id: "work".into(),
                    phrases: vec!["do work".into()],
                    group: "Work".into(),
                    description: "Do work".into(),
                    context: StatusContext::Global,
                    execution: StatusExecution::Handler,
                },
            ],
            ..StatusSnapshot::default()
        };

        let merged = merged_catalog(&compiled, &status);
        assert_eq!(merged[0].task, "Built in");
        assert_eq!(merged[1].task, "Other");
        assert_eq!(merged[2].task, "Work");
    }

    #[test]
    fn personal_workspace_states_cover_setup_and_runtime_failures() {
        let mut status = StatusSnapshot::default();
        assert_eq!(
            personal_workspace_state(false, false, &status),
            PersonalWorkspaceState::Missing
        );
        assert_eq!(
            personal_workspace_state(false, true, &status),
            PersonalWorkspaceState::Creating
        );
        assert_eq!(
            personal_workspace_state(true, false, &status),
            PersonalWorkspaceState::Ready
        );
        status.host_state = HostState::Unavailable;
        assert_eq!(
            personal_workspace_state(true, false, &status),
            PersonalWorkspaceState::Error
        );
        status.active_generation = Some(3);
        assert_eq!(
            personal_workspace_state(true, false, &status),
            PersonalWorkspaceState::Ready
        );
    }
}
