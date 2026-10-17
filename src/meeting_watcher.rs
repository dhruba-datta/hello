use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::desktop_ui::rgb;
use color_eyre::eyre::{Result, eyre};
use gpui::{
    App, Application, Bounds, Context, IntoElement, KeyBinding, Menu, MenuItem, Render, Rgba,
    SystemMenuType, Timer, Window, WindowBackgroundAppearance, WindowBounds, WindowHandle,
    WindowKind, WindowOptions, div, point, prelude::*, px, size,
};

use crate::app_settings::AppSettings;
use crate::app_window::AppWindow;
use crate::context::ContextSnapshot;
use crate::dictation_indicator::DictationIndicatorEvent;
use crate::dictation_indicator::{self, DictationIndicatorUi};
use crate::meeting::{self, MeetingRequest};
use crate::meeting_detection::{
    MeetingCandidate, MeetingDetector, MeetingSource, candidate_from_microphone,
};
use crate::microphone_activity::active_microphone_applications;
use crate::transcription_preparation::TranscriptionPreparation;
use crate::{config, recognition};

/// How long a call app must be off the microphone before notes stop.
const CALL_ENDED_AFTER: Duration = Duration::from_secs(20);
const WINDOW_WIDTH: f32 = 344.0;
const WINDOW_HEIGHT: f32 = 150.0;
const WINDOW_MARGIN: f32 = 18.0;
const NOTICE_HEIGHT: f32 = 98.0;
/// How long a notice stays on screen.
const NOTICE_DURATION: Duration = Duration::from_secs(8);
/// Secure input held this long is no longer someone typing a password: say so once.
const SECURE_INPUT_PATIENCE: Duration = Duration::from_secs(15);
/// The pause after a dictation ends before Hello checks for stuck modifier keys.
const STUCK_KEY_CHECK_DELAY: Duration = Duration::from_millis(400);

enum ControllerCommand {
    Record(MeetingCandidate),
    Dismiss,
    Stop,
}

enum ControllerEvent {
    Offer(MeetingCandidate),
    Starting,
    RecordingStarted,
    Transcribing,
    Finished,
    Failed { title: String, error: String },
}

struct ActiveRecording {
    title: String,
    /// The call app that triggered the offer; manual recordings have none.
    app: Option<&'static str>,
    /// When the call app was last seen holding the microphone.
    app_seen_at: Instant,
    stop: Arc<AtomicBool>,
    started: Receiver<()>,
    reported_started: bool,
    worker: JoinHandle<Result<meeting::MeetingManifest>>,
}

struct MeetingUi {
    commands: SyncSender<ControllerCommand>,
    app_window: Rc<RefCell<Option<WindowHandle<AppWindow>>>>,
    event_path: PathBuf,
    meeting_requests: SyncSender<MeetingRequest>,
    indicator: crate::dictation_indicator::DictationIndicatorSender,
    recognition_start: Option<SyncSender<()>>,
    history: Option<crate::history::History>,
    recognition_controls: SyncSender<crate::recognition::RecognitionControl>,
    status_actions: Option<Receiver<crate::status_item::StatusItemAction>>,
    transcription_preparation: TranscriptionPreparation,
    model_menu_settings: AppSettings,
}

impl MeetingUi {
    fn open_or_focus(&self, cx: &mut App) -> gpui::Result<WindowHandle<AppWindow>> {
        crate::app_window::open_or_focus(
            &self.app_window,
            self.event_path.clone(),
            config::voice_control(),
            self.meeting_requests.clone(),
            self.indicator.clone(),
            self.recognition_start.clone(),
            self.history.clone(),
            cx,
        )
    }

    /// Replaces a pending offer with the app window and applies a meeting
    /// state change to it. A failed open is logged; the update is still
    /// attempted against any already-open window.
    fn show_meeting_window(
        &self,
        offer: Option<WindowHandle<MeetingOffer>>,
        open_failure: &'static str,
        cx: &mut App,
        update: impl FnOnce(&mut AppWindow, &mut Context<AppWindow>),
    ) {
        close_window(offer, cx);
        if let Err(error) = self.open_or_focus(cx) {
            tracing::error!(%error, "{open_failure}");
        }
        update_app_window(&self.app_window, cx, update);
    }
}

struct RuntimeWorkers {
    local_api: Option<crate::local_api::LocalApi>,
    recognition: Option<thread::JoinHandle<()>>,
    controller: Option<thread::JoinHandle<()>>,
}

impl RuntimeWorkers {
    fn join(mut self) -> Result<()> {
        if let Some(mut local_api) = self.local_api.take() {
            local_api.shutdown();
        }
        if let Some(worker) = self.recognition {
            worker
                .join()
                .map_err(|_| eyre!("recognition worker panicked during shutdown"))?;
        }
        if let Some(controller) = self.controller {
            controller
                .join()
                .map_err(|_| eyre!("meeting controller panicked during shutdown"))?;
        }
        Ok(())
    }

    fn join_and_log(self) {
        if let Err(error) = self.join() {
            tracing::error!(%error, "runtime worker shutdown failed");
        }
    }
}

pub struct ListenerConfig {
    pub project_root: PathBuf,
    pub event_path: PathBuf,
    pub device: Option<String>,
}

/// What the desktop process hosts for its lifetime.
pub enum Launch {
    /// The production app with local dictation.
    App(ListenerConfig),
    /// The dictation HUD driven by a synthetic capture loop, without recognition.
    DictationHudPreview,
    /// Developer meeting detection; `offer_preview` shows an offer immediately.
    #[cfg(debug_assertions)]
    MeetingWatch { offer_preview: bool },
    /// One isolated, deterministic UI preview without app services.
    Shell(crate::app_window::AppWindowPreview),
}

fn should_open_app_on_launch(
    show_dock_icon: bool,
    setup_ready: bool,
    onboarding_completed: bool,
    status_item_available: bool,
) -> bool {
    show_dock_icon || !setup_ready || !onboarding_completed || !status_item_available
}

pub fn run(shutdown: &'static AtomicBool, launch: Launch) -> Result<()> {
    if objc2::MainThreadMarker::new().is_none() {
        return Err(eyre!("desktop startup requires the main thread"));
    }
    let (listener, dictation_preview, preview, shell_preview) = match launch {
        Launch::App(listener) => (Some(listener), false, false, None),
        Launch::DictationHudPreview => (None, true, false, None),
        #[cfg(debug_assertions)]
        Launch::MeetingWatch { offer_preview } => (None, false, offer_preview, None),
        Launch::Shell(shell_preview) => (None, false, false, Some(shell_preview)),
    };
    shutdown.store(false, Ordering::Relaxed);
    crate::keyboard::initialize_layout()?;
    let settings = if shell_preview.is_some() {
        AppSettings::default()
    } else {
        AppSettings::load()?
    };
    if shell_preview.is_none() {
        crate::dictionary::load_at_startup();
    }
    let show_dock_icon = settings.show_dock_icon;
    let show_menu_bar_icon = settings.show_menu_bar_icon;
    let setup_ready = crate::onboarding::status(&settings.transcription).ready();
    let onboarding_completed = shell_preview.is_some() || crate::onboarding::completion_recorded();
    let history = if listener.is_some() {
        match crate::history::History::open_default(settings.history_retention) {
            Ok(history) => Some(history),
            Err(error) => {
                tracing::warn!(%error, "dictation history is unavailable");
                None
            }
        }
    } else {
        None
    };
    let app_event_path = listener
        .as_ref()
        .map(|listener| listener.event_path.clone());
    let event_log = app_event_path
        .as_ref()
        .map(|path| crate::events::EventLog::create(path))
        .transpose()?;
    let (developer_sender, developer_receiver) = mpsc::sync_channel(8);
    let (recognition_control_sender, recognition_control_receiver) = mpsc::sync_channel(8);
    let local_api = event_log
        .as_ref()
        .map(|events| {
            crate::local_api::LocalApi::start(
                events.clone(),
                crate::local_api::LocalApiOptions {
                    developer_control: crate::DEVELOPER_FEATURES_ENABLED
                        .then(|| developer_sender.clone()),
                    dictation_control: Some(recognition_control_sender.clone()),
                },
            )
        })
        .transpose()?;
    let meeting_project_root = listener
        .as_ref()
        .map(|listener| listener.project_root.clone())
        .unwrap_or(std::env::current_dir()?);
    let shell_event_path = app_event_path
        .clone()
        .unwrap_or_else(|| meeting_project_root.join("logs/live.ndjson"));
    let (event_sender, event_receiver) = mpsc::sync_channel(32);
    let (command_sender, command_receiver) = mpsc::sync_channel(8);
    let (meeting_request_sender, meeting_request_receiver) = mpsc::sync_channel(8);
    let (indicator_sender, indicator_receiver) = dictation_indicator::channel();
    if dictation_preview {
        let preview_sender = indicator_sender.clone();
        thread::spawn(move || {
            loop {
                preview_sender.send(DictationIndicatorEvent::Started);
                thread::sleep(Duration::from_millis(450));
                let started = Instant::now();
                while started.elapsed() < Duration::from_secs(5) {
                    let wave = (started.elapsed().as_secs_f32() * 5.0).sin() * 0.5 + 0.5;
                    preview_sender.send(DictationIndicatorEvent::Meter {
                        average: 0.025 + wave * 0.09,
                        peak: 0.15 + wave * 0.55,
                    });
                    thread::sleep(Duration::from_millis(20));
                }
                preview_sender.send(DictationIndicatorEvent::Submitted { job_id: 0 });
                preview_sender.send(DictationIndicatorEvent::Transcribing { job_id: 0 });
                thread::sleep(Duration::from_millis(700));
                preview_sender.send(DictationIndicatorEvent::Started);
                thread::sleep(Duration::from_millis(500));
                preview_sender.send(DictationIndicatorEvent::Processing { job_id: 0 });
                thread::sleep(Duration::from_millis(800));
                preview_sender.send(DictationIndicatorEvent::Submitted { job_id: 1 });
                preview_sender.send(DictationIndicatorEvent::Transcribing { job_id: 1 });
                thread::sleep(Duration::from_millis(700));
                preview_sender.send(DictationIndicatorEvent::JobCompleted { job_id: 0 });
                thread::sleep(Duration::from_millis(700));
                preview_sender.send(DictationIndicatorEvent::Processing { job_id: 1 });
                thread::sleep(Duration::from_secs(1));
                preview_sender.send(DictationIndicatorEvent::JobCompleted { job_id: 1 });
                thread::sleep(Duration::from_secs(1));
            }
        });
    }
    let controller_worker =
        (!shell_preview.is_some() && !dictation_preview && crate::MEETINGS_ENABLED).then(|| {
            let controller_events = event_sender.clone();
            thread::spawn(move || {
                if preview {
                    let _ = controller_events.send(ControllerEvent::Offer(MeetingCandidate {
                        key: "preview".into(),
                        title: "Design sync".into(),
                        source: MeetingSource::Zoom,
                        app: None,
                    }));
                }
                if let Err(error) = controller_loop(
                    shutdown,
                    controller_events.clone(),
                    command_receiver,
                    meeting_request_receiver,
                    meeting_project_root,
                ) {
                    tracing::error!(%error, "meeting controller stopped");
                    let _ = controller_events.send(ControllerEvent::Failed {
                        title: "Meeting detection stopped".into(),
                        error: error.to_string(),
                    });
                }
            })
        });

    let recognition_meeting_requests = meeting_request_sender.clone();
    let (recognition_start_sender, recognition_start_receiver) = if listener.is_some() {
        let (sender, receiver) = mpsc::sync_channel(1);
        (Some(sender), Some(receiver))
    } else {
        (None, None)
    };
    let recognition_worker = listener.map(|listener| {
        let recognition_events = event_log.clone().expect("listener event log must exist");
        let failure_indicator = indicator_sender.clone();
        let recognition_indicator = indicator_sender.clone();
        let meeting_requests =
            crate::MEETINGS_ENABLED.then(|| recognition_meeting_requests.clone());
        let recognition_history = history.clone();
        let start = recognition_start_receiver.expect("listener start receiver must exist");
        thread::spawn(move || {
            if !setup_ready {
                loop {
                    if shutdown.load(Ordering::Relaxed) {
                        return;
                    }
                    match start.recv_timeout(Duration::from_millis(100)) {
                        Ok(()) => break,
                        Err(RecvTimeoutError::Timeout) => continue,
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
            }
            if let Err(error) = recognition::listen(
                &listener.project_root,
                recognition_events,
                listener.device.as_deref(),
                config::voice_control(),
                shutdown,
                Some(recognition_indicator),
                meeting_requests,
                recognition_history,
                Some(recognition_control_receiver),
            ) {
                tracing::error!(%error, "desktop recognition stopped");
                failure_indicator.send(crate::dictation_indicator::DictationIndicatorEvent::Failed);
            }
        })
    });
    let runtime_workers = Rc::new(RefCell::new(Some(RuntimeWorkers {
        local_api,
        recognition: recognition_worker,
        controller: controller_worker,
    })));

    let app_window = Rc::new(RefCell::new(None::<WindowHandle<AppWindow>>));
    let reopen_window = app_window.clone();
    let reopen_event_path = shell_event_path.clone();
    let reopen_meeting_requests = meeting_request_sender.clone();
    let reopen_recognition_start = recognition_start_sender.clone();
    let reopen_preview = shell_preview.clone();
    let reopen_history = history.clone();
    let reopen_indicator = indicator_sender.clone();
    let indicator_enabled = shell_preview.as_ref().is_none_or(|preview| {
        matches!(
            preview.pane,
            crate::developer_control::DeveloperPane::HudLab
        )
    });
    let application = Application::new();
    application.on_reopen(move |cx| {
        let result = match reopen_preview.clone() {
            Some(preview) => crate::app_window::open_preview(
                &reopen_window,
                reopen_event_path.clone(),
                config::voice_control(),
                reopen_meeting_requests.clone(),
                reopen_indicator.clone(),
                preview,
                cx,
            ),
            None => crate::app_window::open_or_focus(
                &reopen_window,
                reopen_event_path.clone(),
                config::voice_control(),
                reopen_meeting_requests.clone(),
                reopen_indicator.clone(),
                reopen_recognition_start.clone(),
                reopen_history.clone(),
                cx,
            ),
        };
        if let Err(error) = result {
            tracing::error!(%error, "could not open Hello");
        }
    });
    let application_runtime_workers = runtime_workers.clone();
    application.run(move |cx| {
        let transcription_preparation = cx.default_global::<TranscriptionPreparation>().clone();
        let close_app_window = app_window.clone();
        cx.bind_keys([
            KeyBinding::new("cmd-w", crate::app_window::CloseWindow, None),
            KeyBinding::new("cmd-q", crate::app_window::QuitApplication, None),
            KeyBinding::new("cmd-h", crate::app_window::HideApplication, None),
            KeyBinding::new("cmd-m", crate::app_window::MinimizeWindow, None),
            KeyBinding::new("ctrl-cmd-f", crate::app_window::ToggleFullscreen, None),
            KeyBinding::new("cmd-,", crate::app_window::ShowGeneral, None),
            KeyBinding::new("cmd-0", crate::app_window::ShowProfile, None),
            KeyBinding::new("cmd-1", crate::app_window::ShowGeneral, None),
            KeyBinding::new("cmd-2", crate::app_window::ShowSettings, None),
            KeyBinding::new("cmd-3", crate::app_window::ShowAppearance, None),
            KeyBinding::new("cmd-4", crate::app_window::ShowSounds, None),
            KeyBinding::new("cmd-5", crate::app_window::ShowModes, None),
            KeyBinding::new("cmd-6", crate::app_window::ShowCommands, None),
            KeyBinding::new("cmd-7", crate::app_window::ShowDictionary, None),
            KeyBinding::new("cmd-8", crate::app_window::ShowNotetaker, None),
            KeyBinding::new("cmd-9", crate::app_window::ShowHistory, None),
        ]);
        cx.bind_keys(crate::text_input::key_bindings());
        cx.on_action(move |_: &crate::app_window::CloseWindow, cx| {
            close_window(close_app_window.borrow_mut().take(), cx)
        });
        cx.on_action(|_: &crate::app_window::QuitApplication, cx| cx.quit());
        cx.on_action(|_: &crate::app_window::CheckForUpdates, _| {
            crate::sparkle::check_for_updates()
        });
        let retry_commands = recognition_control_sender.clone();
        cx.on_action(move |_: &crate::app_window::RetryCommands, _| {
            if let Err(error) =
                retry_commands.try_send(crate::recognition::RecognitionControl::RetryCommands)
            {
                tracing::warn!(%error, "could not request command model retry");
            }
        });
        cx.on_action(|_: &crate::app_window::HideApplication, _| {
            crate::app_settings::hide_application()
        });
        let paste_last = recognition_control_sender.clone();
        cx.on_action(move |_: &crate::app_window::PasteLastDictation, _| {
            if let Err(error) =
                paste_last.try_send(crate::recognition::RecognitionControl::PasteLast)
            {
                tracing::warn!(%error, "could not request paste of last dictation");
            }
        });
        let mut application_items = Vec::new();
        if !crate::sparkle::updates_managed() {
            application_items.extend([
                MenuItem::action("Check for Updates…", crate::app_window::CheckForUpdates),
                MenuItem::separator(),
            ]);
        }
        application_items.extend([
            MenuItem::action("Settings…", crate::app_window::ShowGeneral),
            MenuItem::os_submenu("Services", SystemMenuType::Services),
            MenuItem::separator(),
            MenuItem::action("Hide Hello", crate::app_window::HideApplication),
            MenuItem::separator(),
            MenuItem::action("Quit Hello", crate::app_window::QuitApplication),
        ]);
        cx.set_menus(vec![
            Menu {
                name: "Hello".into(),
                items: application_items,
            },
            Menu {
                name: "File".into(),
                items: vec![MenuItem::action(
                    "Close Window",
                    crate::app_window::CloseWindow,
                )],
            },
            Menu {
                name: "View".into(),
                items: vec![
                    MenuItem::action("Profile", crate::app_window::ShowProfile),
                    MenuItem::separator(),
                    MenuItem::action("General", crate::app_window::ShowGeneral),
                    MenuItem::action("Dictation", crate::app_window::ShowSettings),
                    MenuItem::separator(),
                    MenuItem::action("Appearance", crate::app_window::ShowAppearance),
                    MenuItem::action("Sounds", crate::app_window::ShowSounds),
                    MenuItem::separator(),
                    MenuItem::action("Modes", crate::app_window::ShowModes),
                    MenuItem::action("Commands", crate::app_window::ShowCommands),
                    MenuItem::action("Dictionary", crate::app_window::ShowDictionary),
                    MenuItem::separator(),
                    MenuItem::action("Notetaker", crate::app_window::ShowNotetaker),
                    MenuItem::action("History", crate::app_window::ShowHistory),
                ],
            },
            Menu {
                name: "Dictation".into(),
                items: vec![
                    MenuItem::action(
                        "Paste Last Dictation",
                        crate::app_window::PasteLastDictation,
                    ),
                    MenuItem::separator(),
                    MenuItem::submenu(Menu {
                        name: "Indicator Style".into(),
                        items: vec![
                            MenuItem::action("Wave", crate::app_window::IndicatorStyleWave),
                            MenuItem::action("Orb", crate::app_window::IndicatorStyleOrb),
                            MenuItem::action("Line", crate::app_window::IndicatorStyleLine),
                            MenuItem::action("Dots", crate::app_window::IndicatorStyleDots),
                            MenuItem::action("Pulse", crate::app_window::IndicatorStylePulse),
                            MenuItem::action(
                                "Equalizer",
                                crate::app_window::IndicatorStyleEqualizer,
                            ),
                            MenuItem::action("Spectrum", crate::app_window::IndicatorStyleSpectrum),
                            MenuItem::action("Twin", crate::app_window::IndicatorStyleTwin),
                            MenuItem::action("Meter", crate::app_window::IndicatorStyleMeter),
                            MenuItem::action("Pixels", crate::app_window::IndicatorStylePixels),
                            MenuItem::action("Ripple", crate::app_window::IndicatorStyleRipple),
                            MenuItem::action("Halo", crate::app_window::IndicatorStyleHalo),
                            MenuItem::action("Blob", crate::app_window::IndicatorStyleBlob),
                            MenuItem::action("Comet", crate::app_window::IndicatorStyleComet),
                            MenuItem::action("Dot", crate::app_window::IndicatorStyleDot),
                            MenuItem::action("Aurora", crate::app_window::IndicatorStyleAurora),
                            MenuItem::action("Classic", crate::app_window::IndicatorStyleClassic),
                        ],
                    }),
                    MenuItem::submenu(Menu {
                        name: "Indicator Position".into(),
                        items: vec![
                            MenuItem::action("Top", crate::app_window::IndicatorAtTop),
                            MenuItem::action("Bottom", crate::app_window::IndicatorAtBottom),
                            MenuItem::separator(),
                            MenuItem::action("Left", crate::app_window::IndicatorAlignLeft),
                            MenuItem::action("Center", crate::app_window::IndicatorAlignCenter),
                            MenuItem::action("Right", crate::app_window::IndicatorAlignRight),
                        ],
                    }),
                    MenuItem::separator(),
                    MenuItem::submenu(Menu {
                        name: "Language".into(),
                        items: vec![
                            MenuItem::action("Auto-detect", crate::app_window::LanguageAuto),
                            MenuItem::action("English", crate::app_window::LanguageEnglish),
                            MenuItem::action("Bangla", crate::app_window::LanguageBangla),
                        ],
                    }),
                ],
            },
            Menu {
                name: "Notetaker".into(),
                items: vec![
                    MenuItem::action("Take Meeting Notes", crate::app_window::StartMeetingNotes),
                    MenuItem::action("Stop & Save Notes", crate::app_window::StopMeetingNotes),
                    MenuItem::separator(),
                    MenuItem::action("Open Notetaker", crate::app_window::ShowNotetaker),
                    MenuItem::separator(),
                    MenuItem::action(
                        "Detect Meetings On/Off",
                        crate::app_window::ToggleMeetingDetection,
                    ),
                ],
            },
            Menu {
                name: "Window".into(),
                items: vec![
                    MenuItem::action("Minimize", crate::app_window::MinimizeWindow),
                    MenuItem::action("Enter Full Screen", crate::app_window::ToggleFullscreen),
                ],
            },
        ]);
        let updated = shell_preview.is_none()
            && crate::sparkle::launched_after_update(crate::app_window::APP_VERSION);
        if updated {
            crate::sparkle::refresh_icon_registration();
        }
        if shell_preview.is_none() {
            crate::sparkle::start();
        }
        let status_actions = if shell_preview.is_none() {
            match crate::status_item::install() {
                Ok(actions) => Some(actions),
                Err(error) => {
                    tracing::error!(%error, "could not install Hello status item");
                    None
                }
            }
        } else {
            None
        };
        crate::desktop_ui::apply_theme(settings.theme, cx.window_appearance());
        crate::status_item::update_transcription(&settings, &transcription_preparation.status());
        crate::status_item::set_visible(show_menu_bar_icon);
        crate::app_settings::set_dock_icon_visible(crate::app_settings::dock_icon_visible(
            show_dock_icon,
            status_actions.is_some(),
        ));
        // With both icons hidden, a manual launch shows the window so Hello is
        // findable; a launch at login stays quiet. Opening Hello again while it
        // runs always brings the window back.
        let quiet_entry_available = status_actions.is_some()
            && (show_menu_bar_icon || crate::login_item::launches_at_login());
        // After an update Sparkle relaunches Hello; show the window so the
        // user sees it came back.
        let show_app_on_launch = updated
            || should_open_app_on_launch(
                show_dock_icon,
                setup_ready,
                onboarding_completed,
                quiet_entry_available,
            );
        let open_result = match shell_preview.clone() {
            Some(preview) => Some(crate::app_window::open_preview(
                &app_window,
                shell_event_path.clone(),
                config::voice_control(),
                meeting_request_sender.clone(),
                indicator_sender.clone(),
                preview,
                cx,
            )),
            None if show_app_on_launch => app_event_path.map(|event_path| {
                crate::app_window::open_or_focus(
                    &app_window,
                    event_path,
                    config::voice_control(),
                    meeting_request_sender.clone(),
                    indicator_sender.clone(),
                    recognition_start_sender.clone(),
                    history.clone(),
                    cx,
                )
            }),
            None => None,
        };
        if let Some(Err(error)) = open_result {
            tracing::error!(%error, "could not open Hello");
        }
        let quit_shutdown = shutdown;
        let quit_workers = application_runtime_workers.clone();
        let quit_preparation = transcription_preparation.clone();
        cx.on_app_quit(move |_| {
            quit_preparation.cancel();
            quit_shutdown.store(true, Ordering::Relaxed);
            if let Some(workers) = quit_workers.borrow_mut().take() {
                workers.join_and_log();
            }
            async {}
        })
        .detach();

        let ui_history = history.clone();
        cx.spawn(async move |cx| {
            drive_ui(
                event_receiver,
                indicator_receiver,
                developer_receiver,
                MeetingUi {
                    commands: command_sender,
                    app_window: app_window.clone(),
                    event_path: shell_event_path.clone(),
                    meeting_requests: meeting_request_sender.clone(),
                    indicator: indicator_sender.clone(),
                    recognition_start: recognition_start_sender.clone(),
                    history: ui_history.clone(),
                    recognition_controls: recognition_control_sender.clone(),
                    status_actions,
                    transcription_preparation,
                    model_menu_settings: settings,
                },
                shutdown,
                indicator_enabled,
                cx,
            )
            .await;
        })
        .detach();
    });
    shutdown.store(true, Ordering::Relaxed);
    if let Some(workers) = runtime_workers.borrow_mut().take() {
        workers.join()?
    }
    Ok(())
}

async fn drive_ui(
    events: Receiver<ControllerEvent>,
    indicator_events: Receiver<crate::dictation_indicator::DictationIndicatorEvent>,
    developer_calls: Receiver<crate::developer_control::DeveloperCall>,
    mut ui: MeetingUi,
    shutdown: &AtomicBool,
    indicator_enabled: bool,
    cx: &mut gpui::AsyncApp,
) {
    let mut offer_window: Option<WindowHandle<MeetingOffer>> = None;
    let mut notice_window: Option<(WindowHandle<NoticeWindow>, Instant)> = None;
    let mut secure_input_since: Option<Instant> = None;
    let mut secure_input_reported = false;
    let mut secure_input_check_at = Instant::now();
    let mut stuck_key_check_at: Option<Instant> = None;
    let mut dictation_indicator = indicator_enabled.then(DictationIndicatorUi::new);
    let mut model_menu_refresh_at = Instant::now();
    let mut extras_refresh_at = Instant::now();
    let mut microphones_refresh_at = Instant::now();
    let mut microphones: Vec<String> = Vec::new();

    loop {
        if shutdown.load(Ordering::Relaxed) {
            ui.transcription_preparation.cancel();
            let _ = cx.update(|cx| cx.quit());
            return;
        }
        while let Ok(event) = indicator_events.try_recv() {
            if matches!(
                event,
                DictationIndicatorEvent::Discarded
                    | DictationIndicatorEvent::Cancelled
                    | DictationIndicatorEvent::Failed
                    | DictationIndicatorEvent::JobCompleted { .. }
                    | DictationIndicatorEvent::JobCancelled { .. }
                    | DictationIndicatorEvent::JobFailed { .. }
            ) {
                stuck_key_check_at = Some(Instant::now() + STUCK_KEY_CHECK_DELAY);
            }
            if let Some(dictation_indicator) = &mut dictation_indicator
                && let Err(error) = cx.update(|cx| dictation_indicator.handle(event, cx))
            {
                tracing::error!(%error, "could not update dictation indicator");
                return;
            }
        }
        if stuck_key_check_at.is_some_and(|at| Instant::now() >= at) {
            stuck_key_check_at = None;
            match crate::keyboard::release_stuck_modifiers() {
                Ok(released) if !released.is_empty() => {
                    tracing::info!(?released, "released stuck modifier keys");
                }
                Ok(_) => {}
                Err(error) => tracing::warn!(%error, "could not check for stuck keys"),
            }
        }
        if let Some(notice) = crate::notice::take() {
            let _ = cx.update(|cx| {
                close_window(notice_window.take().map(|(window, _)| window), cx);
                notice_window = open_notice(notice, cx)
                    .ok()
                    .map(|window| (window, Instant::now()));
            });
        }
        if notice_window
            .as_ref()
            .is_some_and(|(_, shown)| shown.elapsed() >= NOTICE_DURATION)
        {
            let _ = cx.update(|cx| {
                close_window(notice_window.take().map(|(window, _)| window), cx);
            });
        }
        // Only the real app watches secure input; previews have no status item.
        if ui.status_actions.is_some() && Instant::now() >= secure_input_check_at {
            secure_input_check_at = Instant::now() + Duration::from_secs(1);
            if crate::suppression::secure_input_enabled() {
                let since = *secure_input_since.get_or_insert_with(Instant::now);
                if !secure_input_reported && since.elapsed() >= SECURE_INPUT_PATIENCE {
                    secure_input_reported = true;
                    crate::notice::show(
                        crate::desktop_ui::Glyph::Keyboard,
                        "Hello can't hear its shortcut",
                        "A password field or secure keyboard entry is active in another app. \
                         Hello's shortcut works again when that app lets go of the keyboard.",
                    );
                }
            } else {
                secure_input_since = None;
                secure_input_reported = false;
            }
        }
        if let Some(dictation_indicator) = &mut dictation_indicator {
            let _ = cx.update(|cx| dictation_indicator.follow_pointer(cx));
        }
        while let Ok(call) = developer_calls.try_recv() {
            let reply = cx
                .update(|cx| apply_developer_command(call.command, &ui, cx))
                .unwrap_or_else(|error| {
                    crate::developer_control::DeveloperReply::error(
                        "developer-ui-failed",
                        error.to_string(),
                    )
                });
            let _ = call.reply.send(reply);
        }
        while let Some(action) = ui
            .status_actions
            .as_ref()
            .and_then(|actions| actions.try_recv().ok())
        {
            let result = cx.update(|cx| match action {
                crate::status_item::StatusItemAction::ToggleMeetingNotes => {
                    let recording = meeting::list().ok().is_some_and(|meetings| {
                        meeting::active(&meetings).is_some_and(|active| {
                            active.status == meeting::MeetingStatus::Recording
                        })
                    });
                    let request = if recording {
                        MeetingRequest::Stop
                    } else {
                        MeetingRequest::Start
                    };
                    if let Err(error) = ui.meeting_requests.try_send(request) {
                        tracing::warn!(%error, "could not send meeting request");
                    }
                    extras_refresh_at = Instant::now();
                }
                crate::status_item::StatusItemAction::OpenNotetaker
                | crate::status_item::StatusItemAction::OpenNote(_)
                | crate::status_item::StatusItemAction::OpenShortcuts => match ui.open_or_focus(cx)
                {
                    Ok(handle) => {
                        let _ = handle.update(cx, |window, _, cx| match action {
                            crate::status_item::StatusItemAction::OpenNote(tag) => {
                                window.show_notetaker(crate::status_item::note_id(tag), cx)
                            }
                            crate::status_item::StatusItemAction::OpenShortcuts => {
                                window.show_settings(cx)
                            }
                            _ => window.show_notetaker(None, cx),
                        });
                    }
                    Err(error) => tracing::error!(%error, "could not open Hello from status item"),
                },
                crate::status_item::StatusItemAction::SetLanguage(tag) => {
                    let routing = match tag {
                        0 => crate::app_settings::LanguageRouting::Auto,
                        1 => crate::app_settings::LanguageRouting::Main,
                        _ => crate::app_settings::LanguageRouting::Second,
                    };
                    crate::app_window::menu_choose_language(&ui.app_window, routing, cx);
                    extras_refresh_at = Instant::now();
                }
                crate::status_item::StatusItemAction::SetMicrophone(tag) => {
                    if let Some(choice) = crate::status_item::microphone_choice(tag) {
                        crate::app_window::apply_menu_settings(&ui.app_window, cx, |settings| {
                            settings.microphone = choice.clone()
                        });
                    }
                    extras_refresh_at = Instant::now();
                }
                crate::status_item::StatusItemAction::OpenSettings
                | crate::status_item::StatusItemAction::OpenModels => match ui.open_or_focus(cx) {
                    Ok(handle) => {
                        let _ = handle.update(cx, |window, _, cx| {
                            if action == crate::status_item::StatusItemAction::OpenModels {
                                window.show_transcription_models(cx);
                            } else {
                                window.show_settings(cx);
                            }
                        });
                    }
                    Err(error) => {
                        tracing::error!(%error, "could not open Hello from status item");
                    }
                },
                crate::status_item::StatusItemAction::SelectModel(model) => {
                    let (_, current) = crate::app_settings::transcription_selection();
                    ui.model_menu_settings.transcription = current;
                    let selection = crate::app_window::menu_transcription_selection(
                        &ui.app_window,
                        model,
                        &ui.model_menu_settings,
                        cx,
                    );
                    ui.transcription_preparation.start(selection, true);
                    model_menu_refresh_at = Instant::now();
                }
                crate::status_item::StatusItemAction::CancelModelPreparation => {
                    ui.transcription_preparation.cancel();
                    model_menu_refresh_at = Instant::now();
                }
                crate::status_item::StatusItemAction::PasteLast => {
                    let _ = ui
                        .recognition_controls
                        .try_send(crate::recognition::RecognitionControl::PasteLast);
                }
                crate::status_item::StatusItemAction::CheckForUpdates => {
                    crate::sparkle::check_for_updates();
                }
                crate::status_item::StatusItemAction::Quit => {
                    ui.transcription_preparation.cancel();
                    cx.quit();
                }
            });
            if let Err(error) = result {
                tracing::error!(%error, "could not handle Hello status item action");
                return;
            }
        }
        // Delivered menu cancellations/new choices win over an uncommitted completion.
        if let Some(result) = ui.transcription_preparation.poll() {
            let result = result.and_then(|selection| {
                cx.update(|cx| {
                    crate::app_window::commit_transcription_selection(&ui.app_window, selection, cx)
                })
                .map_err(|error| error.to_string())?
            });
            match result {
                Ok(settings) => {
                    ui.model_menu_settings = settings;
                    ui.transcription_preparation.set_error(None);
                }
                Err(error) => {
                    tracing::warn!(%error, "could not select transcription model");
                    ui.transcription_preparation.set_error(Some(error));
                }
            }
            model_menu_refresh_at = Instant::now();
        }
        if ui.status_actions.is_some() && Instant::now() >= model_menu_refresh_at {
            let status = ui.transcription_preparation.status();
            let _ = cx.update(|cx| {
                crate::app_window::refresh_transcription_menu(
                    &ui.app_window,
                    &ui.model_menu_settings,
                    &status,
                    cx,
                );
            });
            model_menu_refresh_at = Instant::now() + Duration::from_secs(1);
        }
        if ui.status_actions.is_some() && Instant::now() >= extras_refresh_at {
            if Instant::now() >= microphones_refresh_at {
                microphones = crate::audio::input_device_names().unwrap_or_default();
                microphones_refresh_at = Instant::now() + Duration::from_secs(10);
            }
            let _ = cx.update(|cx| {
                let settings = crate::app_window::current_settings(&ui.app_window, cx)
                    .or_else(|| AppSettings::load().ok())
                    .unwrap_or_default();
                crate::status_item::update_extras(menu_extras(&settings, microphones.clone()));
            });
            extras_refresh_at = Instant::now() + Duration::from_secs(2);
        }
        while let Ok(event) = events.try_recv() {
            let result = cx.update(|cx| match event {
                ControllerEvent::Offer(candidate) => {
                    close_window(offer_window.take(), cx);
                    offer_window = open_offer(candidate, ui.commands.clone(), cx).ok();
                }
                ControllerEvent::Starting => ui.show_meeting_window(
                    offer_window.take(),
                    "could not open Hello for starting meeting",
                    cx,
                    |window, cx| window.meeting_starting(cx),
                ),
                ControllerEvent::RecordingStarted => ui.show_meeting_window(
                    offer_window.take(),
                    "could not open Hello for active meeting",
                    cx,
                    |window, cx| window.meeting_started(cx),
                ),
                ControllerEvent::Transcribing => {
                    update_app_window(&ui.app_window, cx, |window, cx| {
                        window.meeting_updated(cx);
                    });
                }
                ControllerEvent::Finished => {
                    update_app_window(&ui.app_window, cx, |window, cx| {
                        window.meeting_finished(cx);
                    });
                }
                ControllerEvent::Failed { title, error } => ui.show_meeting_window(
                    offer_window.take(),
                    "could not open Hello for meeting failure",
                    cx,
                    |window, cx| window.meeting_failed(title, error, cx),
                ),
            });
            if let Err(error) = result {
                tracing::error!(%error, "could not update meeting UI");
                return;
            }
        }
        Timer::after(Duration::from_millis(16)).await;
    }
}

fn apply_developer_command(
    command: crate::developer_control::DeveloperCommand,
    ui: &MeetingUi,
    cx: &mut App,
) -> crate::developer_control::DeveloperReply {
    use crate::developer_control::{DeveloperCommand, DeveloperHudState, DeveloperReply};

    match command {
        DeveloperCommand::Status => {
            let pane = ui.app_window.borrow().as_ref().copied().and_then(|handle| {
                handle
                    .update(cx, |window, _, _| window.developer_pane())
                    .ok()
            });
            DeveloperReply::State {
                window_open: pane.is_some(),
                pane,
                commands_enabled: crate::app_settings::commands_enabled(),
            }
        }
        DeveloperCommand::Hud { state } => {
            ui.indicator.send(DictationIndicatorEvent::Reset);
            match state {
                DeveloperHudState::Reset => {}
                DeveloperHudState::Recording => {
                    ui.indicator.send(DictationIndicatorEvent::Started);
                    ui.indicator.send(DictationIndicatorEvent::Meter {
                        average: 0.08,
                        peak: 0.42,
                    });
                }
                DeveloperHudState::Transcribing => {
                    ui.indicator
                        .send(DictationIndicatorEvent::Submitted { job_id: 0 });
                    ui.indicator
                        .send(DictationIndicatorEvent::Transcribing { job_id: 0 });
                }
                DeveloperHudState::Processing => {
                    ui.indicator
                        .send(DictationIndicatorEvent::Submitted { job_id: 0 });
                    ui.indicator
                        .send(DictationIndicatorEvent::Processing { job_id: 0 });
                }
            }
            DeveloperReply::Ok
        }
        DeveloperCommand::ShowPane { pane } => {
            let handle = match ui.open_or_focus(cx) {
                Ok(handle) => handle,
                Err(error) => {
                    return DeveloperReply::error("open-window-failed", error.to_string());
                }
            };
            match handle.update(cx, |window, _, cx| window.developer_select_pane(pane, cx)) {
                Ok(()) => DeveloperReply::Ok,
                Err(error) => DeveloperReply::error("show-pane-failed", error.to_string()),
            }
        }
        DeveloperCommand::SetCommandsEnabled { enabled } => {
            let handle = ui.app_window.borrow().as_ref().copied();
            if let Some(handle) = handle
                && let Ok(result) = handle.update(cx, |window, _, cx| {
                    window.developer_set_commands_enabled(enabled, cx)
                })
            {
                return match result {
                    Ok(()) => DeveloperReply::Ok,
                    Err(error) => DeveloperReply::error("settings-save-failed", error),
                };
            }
            let mut settings = match AppSettings::load() {
                Ok(settings) => settings,
                Err(error) => {
                    return DeveloperReply::error("settings-load-failed", error.to_string());
                }
            };
            if let Err(error) = settings.set_commands_enabled(enabled) {
                return DeveloperReply::error("settings-conflict", error.to_string());
            }
            match settings.save() {
                Ok(()) => DeveloperReply::Ok,
                Err(error) => DeveloperReply::error("settings-save-failed", error.to_string()),
            }
        }
    }
}

fn controller_loop(
    shutdown: &AtomicBool,
    events: SyncSender<ControllerEvent>,
    commands: Receiver<ControllerCommand>,
    meeting_requests: Receiver<MeetingRequest>,
    project_root: PathBuf,
) -> Result<()> {
    let mut detector = MeetingDetector::default();
    let mut recording: Option<ActiveRecording> = None;
    let mut observed_microphone_apps = Vec::new();
    let mut prune_at = Instant::now();
    tracing::info!("GPUI meeting watcher started");

    while !shutdown.load(Ordering::Relaxed) {
        if recording.is_none() && Instant::now() >= prune_at {
            match meeting::prune_audio(crate::app_settings::meeting_audio_retention()) {
                Ok(0) => {}
                Ok(removed) => tracing::info!(removed, "removed expired meeting audio"),
                Err(error) => tracing::warn!(%error, "could not prune meeting audio"),
            }
            prune_at = Instant::now() + Duration::from_secs(60 * 60);
        }
        let requested_commands = meeting_requests
            .try_iter()
            .take(8)
            .map(|request| match request {
                MeetingRequest::Start => ControllerCommand::Record(MeetingCandidate {
                    key: "manual".into(),
                    title: "Meeting".into(),
                    source: MeetingSource::Manual,
                    app: None,
                }),
                MeetingRequest::Stop => ControllerCommand::Stop,
            });
        for command in commands.try_iter().take(8).chain(requested_commands) {
            match command {
                ControllerCommand::Record(candidate) if recording.is_none() => {
                    let stop = Arc::new(AtomicBool::new(false));
                    let worker_stop = stop.clone();
                    let (started_sender, started) = mpsc::sync_channel(1);
                    let title = candidate.title.clone();
                    let worker_title = title.clone();
                    let worker_project_root = project_root.clone();
                    let hint = meeting::DetailsHint {
                        app: candidate.app,
                        platform: match candidate.source {
                            MeetingSource::Manual | MeetingSource::BrowserCall => None,
                            source => Some(source.label().to_string()),
                        },
                    };
                    let worker = thread::spawn(move || {
                        meeting::record(
                            Some(worker_title),
                            hint,
                            &worker_stop,
                            &worker_project_root,
                            Some(started_sender),
                        )
                    });
                    let _ = events.send(ControllerEvent::Starting);
                    recording = Some(ActiveRecording {
                        app: candidate.app,
                        app_seen_at: Instant::now(),
                        title,
                        stop,
                        started,
                        reported_started: false,
                        worker,
                    });
                }
                ControllerCommand::Stop => {
                    if let Some(active) = &recording
                        && !active.stop.swap(true, Ordering::Relaxed)
                    {
                        let _ = events.send(ControllerEvent::Transcribing);
                    }
                }
                ControllerCommand::Dismiss | ControllerCommand::Record(_) => {}
            }
        }

        if let Some(active) = &mut recording
            && !active.reported_started
            && active.started.try_recv().is_ok()
        {
            active.reported_started = true;
            if !active.stop.load(Ordering::Relaxed) {
                let _ = events.send(ControllerEvent::RecordingStarted);
            }
        }

        if recording
            .as_ref()
            .is_some_and(|active| active.worker.is_finished())
        {
            let active = recording
                .take()
                .ok_or_else(|| eyre!("recording disappeared"))?;
            let event = match active.worker.join() {
                Ok(Ok(_manifest)) => ControllerEvent::Finished,
                Ok(Err(error)) => ControllerEvent::Failed {
                    title: active.title,
                    error: error.to_string(),
                },
                Err(_) => ControllerEvent::Failed {
                    title: active.title,
                    error: "meeting recorder panicked".into(),
                },
            };
            let _ = events.send(event);
        }

        // Stop on our own once the call app has let go of the microphone for
        // a while; a brief release (switching devices, a muted rejoin) is ignored.
        if let Some(active) = &mut recording
            && let Some(app) = active.app
            && active.reported_started
            && !active.stop.load(Ordering::Relaxed)
        {
            let still_in_call = active_microphone_applications()?
                .iter()
                .any(|application| application.canonical_bundle_id() == Some(app));
            if still_in_call {
                active.app_seen_at = Instant::now();
            } else if active.app_seen_at.elapsed() >= CALL_ENDED_AFTER {
                tracing::info!(app, "call ended; finishing meeting notes");
                active.stop.store(true, Ordering::Relaxed);
                let _ = events.send(ControllerEvent::Transcribing);
            }
        }

        if recording.is_none() && !crate::app_settings::meeting_detection_enabled() {
            detector.observe(None);
        } else if recording.is_none() {
            let microphone_apps = active_microphone_applications()?;
            let observed = microphone_apps
                .iter()
                .map(|application| (application.pid, application.bundle_id.clone()))
                .collect::<Vec<_>>();
            if observed != observed_microphone_apps {
                let applications = microphone_apps
                    .iter()
                    .map(|application| {
                        format!(
                            "{} ({}, pid {})",
                            application.name, application.bundle_id, application.pid
                        )
                    })
                    .collect::<Vec<_>>();
                tracing::info!(?applications, "microphone application activity changed");
                observed_microphone_apps = observed;
            }
            let application = microphone_apps
                .into_iter()
                .filter(|application| application.is_supported_meeting_app())
                .min_by_key(|application| application.detection_priority());
            let context = if application
                .as_ref()
                .is_some_and(|application| application.is_browser())
            {
                ContextSnapshot::capture().unwrap_or_default()
            } else {
                ContextSnapshot::default()
            };
            let candidate = application
                .and_then(|application| candidate_from_microphone(&application, &context));
            if let Some(candidate) = detector.observe(candidate) {
                tracing::info!(
                    source = candidate.source.label(),
                    title = candidate.title,
                    "meeting recording offer triggered"
                );
                let _ = events.send(ControllerEvent::Offer(candidate));
            }
        }
        thread::sleep(Duration::from_millis(500));
    }

    if let Some(active) = recording {
        active.stop.store(true, Ordering::Relaxed);
        active
            .worker
            .join()
            .map_err(|_| eyre!("meeting recorder panicked"))??;
    }
    Ok(())
}

/// What the menu bar's Notetaker, Languages and Microphone menus show.
fn menu_extras(settings: &AppSettings, microphones: Vec<String>) -> crate::status_item::MenuExtras {
    let meetings = meeting::list().unwrap_or_default();
    let active = meeting::active(&meetings).map(|meeting| meeting.status);
    crate::status_item::MenuExtras {
        recording: active == Some(meeting::MeetingStatus::Recording),
        busy: matches!(
            active,
            Some(meeting::MeetingStatus::Starting | meeting::MeetingStatus::Transcribing)
        ),
        notes: meetings
            .iter()
            .filter(|meeting| !meeting.status.is_active())
            .take(8)
            .map(|meeting| (meeting.id.clone(), meeting.title.clone()))
            .collect(),
        routing: settings
            .multilingual
            .enabled
            .then_some(settings.multilingual.routing),
        main_language: crate::transcription_models::language_name(&settings.transcription.language)
            .to_string(),
        second_language: crate::transcription_models::language_name(
            &settings.multilingual.second.language,
        )
        .to_string(),
        microphones,
        microphone: settings.microphone.clone(),
    }
}

fn popup_options(cx: &App, height: f32) -> WindowOptions {
    let window_size = size(px(WINDOW_WIDTH), px(height));
    let (display_id, bounds) = cx.primary_display().map_or_else(
        || (None, Bounds::centered(None, window_size, cx)),
        |display| {
            let origin = display.bounds().top_right()
                - point(px(WINDOW_WIDTH + WINDOW_MARGIN), px(-WINDOW_MARGIN));
            (
                Some(display.id()),
                Bounds {
                    origin,
                    size: window_size,
                },
            )
        },
    );
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        display_id,
        titlebar: None,
        window_background: WindowBackgroundAppearance::Transparent,
        focus: false,
        show: true,
        kind: WindowKind::PopUp,
        is_movable: false,
        is_resizable: false,
        is_minimizable: false,
        ..Default::default()
    }
}

fn open_offer(
    candidate: MeetingCandidate,
    commands: SyncSender<ControllerCommand>,
    cx: &mut App,
) -> gpui::Result<WindowHandle<MeetingOffer>> {
    cx.open_window(popup_options(cx, WINDOW_HEIGHT), |_, cx| {
        cx.new(|_| MeetingOffer {
            candidate,
            commands,
        })
    })
}

fn open_notice(
    notice: crate::notice::Notice,
    cx: &mut App,
) -> gpui::Result<WindowHandle<NoticeWindow>> {
    cx.open_window(popup_options(cx, NOTICE_HEIGHT), |_, cx| {
        cx.new(|_| NoticeWindow { notice })
    })
}

/// A short message at the top right of the screen; see `notice.rs`.
struct NoticeWindow {
    notice: crate::notice::Notice,
}

impl Render for NoticeWindow {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        use crate::desktop_ui::{TINT_ORANGE, glyph_badge};
        crate::desktop_ui::apply_theme(crate::app_settings::color_theme(), window.appearance());
        div()
            .flex()
            .flex_col()
            .size_full()
            .gap_3()
            .p(px(14.0))
            .bg(rgb(0x1f1f22))
            .border_1()
            .border_color(rgb(0x34343b))
            .rounded(px(16.0))
            .shadow_lg()
            .child(
                div()
                    .flex()
                    .items_start()
                    .gap_3()
                    .child(glyph_badge(self.notice.icon, 36.0, TINT_ORANGE))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .flex()
                            .flex_col()
                            .gap(px(3.0))
                            .child(
                                div()
                                    .text_size(px(13.5))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgb(0xf5f5f7))
                                    .child(self.notice.title),
                            )
                            .child(
                                div()
                                    .text_size(px(11.5))
                                    .line_height(px(16.0))
                                    .text_color(rgb(0x98989d))
                                    .child(self.notice.body.clone()),
                            ),
                    )
                    .child(secondary_button("dismiss-notice", "OK", |window| {
                        window.remove_window();
                    })),
            )
    }
}

fn close_window<T: Render + 'static>(window: Option<WindowHandle<T>>, cx: &mut App) {
    if let Some(window) = window {
        window
            .update(cx, |_, window, _| window.remove_window())
            .ok();
    }
}

fn update_app_window(
    app_window: &Rc<RefCell<Option<WindowHandle<AppWindow>>>>,
    cx: &mut App,
    update: impl FnOnce(&mut AppWindow, &mut Context<AppWindow>),
) {
    if let Some(window) = app_window.borrow().as_ref().copied() {
        window.update(cx, |window, _, cx| update(window, cx)).ok();
    }
}

struct MeetingOffer {
    candidate: MeetingCandidate,
    commands: SyncSender<ControllerCommand>,
}

impl Render for MeetingOffer {
    fn render(&mut self, window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        use crate::desktop_ui::{Glyph, TINT_PINK, glyph_badge};
        crate::desktop_ui::apply_theme(crate::app_settings::color_theme(), window.appearance());
        let dismiss = self.commands.clone();
        let record = self.commands.clone();
        let candidate = self.candidate.clone();
        let is_preview = candidate.key == "preview";
        div()
            .flex()
            .flex_col()
            .size_full()
            .gap_3()
            .p(px(14.0))
            .bg(rgb(0x1f1f22))
            .border_1()
            .border_color(rgb(0x34343b))
            .rounded(px(16.0))
            .shadow_lg()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(glyph_badge(Glyph::Mic, 36.0, TINT_PINK))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .flex()
                            .flex_col()
                            .gap(px(2.0))
                            .child(
                                div()
                                    .text_size(px(10.5))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgb(0x98989d))
                                    .child(format!(
                                        "Meeting detected · {}",
                                        self.candidate.source.label()
                                    )),
                            )
                            .child(
                                div()
                                    .overflow_x_hidden()
                                    .truncate()
                                    .text_size(px(15.0))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .text_color(rgb(0xf5f5f7))
                                    .child(self.candidate.title.clone()),
                            ),
                    ),
            )
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(0x98989d))
                    .child("Take private notes on this Mac. Let others know you're recording."),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .items_end()
                    .justify_end()
                    .gap_2()
                    .child(secondary_button("not-now", "Not now", move |window| {
                        if dismiss.try_send(ControllerCommand::Dismiss).is_ok() {
                            window.remove_window();
                        }
                    }))
                    .child(primary_button("record", "Take notes", move |window| {
                        let command = if is_preview {
                            ControllerCommand::Dismiss
                        } else {
                            ControllerCommand::Record(candidate.clone())
                        };
                        if record.try_send(command).is_ok() {
                            window.remove_window();
                        }
                    })),
            )
    }
}

fn primary_button(
    id: &'static str,
    label: &'static str,
    on_click: impl Fn(&mut Window) + 'static,
) -> impl IntoElement {
    button(id, label, rgb(0x0a84ff), rgb(0xffffff), on_click)
}

fn secondary_button(
    id: &'static str,
    label: &'static str,
    on_click: impl Fn(&mut Window) + 'static,
) -> impl IntoElement {
    button(id, label, rgb(0x2c2c31), rgb(0xd1d1d6), on_click)
}

fn button(
    id: &'static str,
    label: &'static str,
    background: Rgba,
    foreground: Rgba,
    on_click: impl Fn(&mut Window) + 'static,
) -> impl IntoElement {
    div()
        .id(id)
        .h(px(30.0))
        .px(px(14.0))
        .flex()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(background)
        .text_size(px(12.0))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(foreground)
        .hover(|style| style.opacity(0.88))
        .active(|style| style.opacity(0.8))
        .child(label)
        .on_click(move |_, window, _| on_click(window))
}

#[cfg(debug_assertions)]
pub fn probe() -> Result<()> {
    for application in active_microphone_applications()? {
        println!(
            "{}\t{}\t{}\t{}",
            application.pid,
            if application.is_supported_meeting_app() {
                "meeting"
            } else {
                "other"
            },
            application.bundle_id,
            application.name
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dockless_startup_stays_quiet_only_when_setup_and_menu_bar_are_ready() {
        for setup_ready in [false, true] {
            for onboarding_completed in [false, true] {
                for status_item_available in [false, true] {
                    assert_eq!(
                        should_open_app_on_launch(
                            false,
                            setup_ready,
                            onboarding_completed,
                            status_item_available,
                        ),
                        !(setup_ready && onboarding_completed && status_item_available),
                        "setup_ready={setup_ready}, onboarding_completed={onboarding_completed}, status_item_available={status_item_available}",
                    );
                }
            }
        }
    }

    #[test]
    fn dock_visible_startup_always_opens_the_app() {
        for setup_ready in [false, true] {
            for onboarding_completed in [false, true] {
                for status_item_available in [false, true] {
                    assert!(should_open_app_on_launch(
                        true,
                        setup_ready,
                        onboarding_completed,
                        status_item_available,
                    ));
                }
            }
        }
    }
}
