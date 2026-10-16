use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::Once;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::thread;
use std::time::Instant;

use color_eyre::eyre::{Result, WrapErr, eyre};
use transcribe_cpp::{
    Backend, ExtSlot, Model, ModelOptions, RunExtension, RunOptions, TimestampKind, Transcript,
    WhisperRunOptions, sys::TRANSCRIBE_EXT_KIND_WHISPER_RUN,
};

use crate::app_settings::{LanguageRouting, SecondLanguage};
use crate::context::ContextSnapshot;
use crate::dictation::{DictationClip, DictationProtocol, pad_for_parakeet};
use crate::dictation_processor::ProcessingObservation;
use crate::gguf_session::OfflineGgufSession;
use crate::history::{History, HistoryDraft, HistoryKind};
use crate::meeting::{self, TranscriptEntry, TranscriptPublication};
use crate::paste::{PasteMode, Paster};
use crate::suppression::InputActivity;
use crate::transcription::WarmTranscriber;
use crate::transcription_models::{
    TranscriptionModelId, TranscriptionSelection, model_path, validate,
};

pub struct Parakeet {
    session: OfflineGgufSession,
    options: RunOptions,
    name: String,
    selection: Option<TranscriptionSelection>,
    max_audio_samples: Option<usize>,
}

static TRANSCRIBE_LOGGING: Once = Once::new();
const TRANSCRIPTION_SAMPLES_PER_MS: usize = 16;
const MAX_PENDING_OUTPUTS: usize = 16;

pub enum WorkerEvent {
    ModelFailed(String),
    Completed {
        job_id: DictationJobId,
        target: TranscriptionTarget,
        result: Result<String, String>,
        processing: Option<ProcessingObservation>,
    },
    Stage {
        job_id: DictationJobId,
        stage: DictationJobStage,
    },
    Cancelled {
        job_id: DictationJobId,
    },
    Pasted {
        kind: PasteKind,
        result: Result<String, String>,
    },
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct DictationJobId(u64);

impl DictationJobId {
    pub const fn value(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DictationJobStage {
    Transcribing,
    Processing,
}

#[derive(Clone, Copy, Debug)]
pub enum PasteKind {
    LastTranscript,
    MeetingDelta,
}

#[derive(Clone, Copy, Debug)]
pub enum TranscriptionTarget {
    Paste,
    Send,
    Service,
}

struct InferenceJob {
    job_id: DictationJobId,
    control: Arc<JobControl>,
    submitted_at: Instant,
    clip: DictationClip,
    target: TranscriptionTarget,
    protocol: Option<Arc<DictationProtocol>>,
    context: ContextSnapshot,
    selection: TranscriptionSelection,
    second_language: Option<SecondLanguage>,
}

enum InferenceCommand {
    Reload(TranscriptionSelection, Option<SecondLanguage>),
    Transcribe(Box<InferenceJob>),
}

struct ProcessorJob {
    job_id: DictationJobId,
    control: Arc<JobControl>,
    target: TranscriptionTarget,
    text: String,
    context: ContextSnapshot,
    timings: JobTimings,
}

/// Pipeline timings carried from inference through output for the final log.
#[derive(Clone, Copy)]
struct JobTimings {
    total_started: Instant,
    queue_ms: u128,
    audio_ms: u64,
    prepare_ms: u128,
    inference_ms: u128,
}

struct CompletedTranscript {
    text: String,
    /// Corrected local transcript before mode processing.
    raw: String,
    application: Option<String>,
    timings: JobTimings,
    processing: Option<ProcessingObservation>,
}

enum OutputJob {
    PreparePaste,
    Completed {
        job_id: DictationJobId,
        control: Arc<JobControl>,
        target: TranscriptionTarget,
        result: Box<Result<CompletedTranscript, String>>,
    },
    Cancelled {
        job_id: DictationJobId,
    },
    Paste {
        sequence: u64,
        kind: PasteKind,
    },
}

impl OutputJob {
    fn sequence(&self) -> u64 {
        match self {
            Self::PreparePaste => u64::MAX,
            Self::Completed { job_id, .. } | Self::Cancelled { job_id } => job_id.0,
            Self::Paste { sequence, .. } => *sequence,
        }
    }
}

#[derive(Default)]
struct JobControl {
    cancelled: AtomicBool,
    output_started: Mutex<bool>,
}

impl JobControl {
    fn cancel(&self) -> bool {
        let output_started = self
            .output_started
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if *output_started {
            return false;
        }
        !self.cancelled.swap(true, Ordering::AcqRel)
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    fn begin_output(&self) -> bool {
        let mut output_started = self
            .output_started
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if self.is_cancelled() {
            return false;
        }
        *output_started = true;
        true
    }
}

#[derive(Default)]
struct OrderedOutputs {
    waiting: BTreeMap<u64, OutputJob>,
    next_sequence: u64,
}

impl OrderedOutputs {
    fn push(&mut self, job: OutputJob) -> Vec<OutputJob> {
        if job.sequence() < self.next_sequence {
            return Vec::new();
        }
        self.waiting.insert(job.sequence(), job);
        let mut ready = Vec::new();
        while let Some(job) = self.waiting.remove(&self.next_sequence) {
            ready.push(job);
            self.next_sequence += 1;
        }
        ready
    }
}

pub struct DictationWorker {
    inference_jobs: Option<SyncSender<InferenceCommand>>,
    output_jobs: Option<SyncSender<OutputJob>>,
    events: Receiver<WorkerEvent>,
    state: Arc<Mutex<WorkerState>>,
    inference_worker: Option<thread::JoinHandle<()>>,
    processor_workers: Vec<thread::JoinHandle<()>>,
    output_worker: Option<thread::JoinHandle<()>>,
}

struct WorkerState {
    next_sequence: u64,
    jobs: BTreeMap<DictationJobId, Arc<JobControl>>,
    pending_pastes: usize,
}

impl WorkerState {
    fn next_output_sequence(&self) -> Result<u64, &'static str> {
        // Count accepted work even after it leaves a channel for ordered buffering.
        if self.jobs.len() + self.pending_pastes >= MAX_PENDING_OUTPUTS {
            return Err("dictation queue is full");
        }
        Ok(self.next_sequence)
    }

    fn cancel_latest(&self) -> Option<DictationJobId> {
        self.jobs
            .iter()
            .rev()
            .find_map(|(&job_id, control)| control.cancel().then_some(job_id))
    }
}

impl DictationWorker {
    pub fn start(
        activity: InputActivity,
        transformations: Arc<crate::personal_commands::TransformationClient>,
        history: Option<History>,
    ) -> Self {
        const PROCESSOR_WORKERS: usize = 2;
        let (inference_jobs, inference_receiver) = mpsc::sync_channel::<InferenceCommand>(2);
        let (processor_jobs, processor_receiver) = mpsc::sync_channel::<ProcessorJob>(4);
        let (output_jobs, output_receiver) = mpsc::sync_channel::<OutputJob>(8);
        let (event_sender, events) = mpsc::channel();
        let state = Arc::new(Mutex::new(WorkerState {
            next_sequence: 0,
            jobs: BTreeMap::new(),
            pending_pastes: 0,
        }));

        let output_worker = thread::spawn({
            let state = state.clone();
            let events = event_sender.clone();
            move || run_output_worker(output_receiver, activity, history, &state, &events)
        });

        let processor_receiver = Arc::new(Mutex::new(processor_receiver));
        let processor_workers = (0..PROCESSOR_WORKERS)
            .map(|_| {
                let jobs = processor_receiver.clone();
                let output = output_jobs.clone();
                let events = event_sender.clone();
                let transformations = transformations.clone();
                thread::spawn(move || {
                    run_processor_worker(&jobs, &output, &events, &transformations)
                })
            })
            .collect();

        let inference_worker = thread::spawn({
            let output = output_jobs.clone();
            move || {
                run_inference_worker(inference_receiver, &processor_jobs, &output, &event_sender)
            }
        });

        Self {
            inference_jobs: Some(inference_jobs),
            output_jobs: Some(output_jobs),
            events,
            state,
            inference_worker: Some(inference_worker),
            processor_workers,
            output_worker: Some(output_worker),
        }
    }

    pub fn transcribe(
        &self,
        clip: DictationClip,
        target: TranscriptionTarget,
        protocol: Option<Arc<DictationProtocol>>,
        context: ContextSnapshot,
    ) -> Result<DictationJobId, &'static str> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let job_id = DictationJobId(state.next_output_sequence()?);
        let control = Arc::new(JobControl::default());
        let (_, selection) = crate::app_settings::transcription_selection();
        let second_language = crate::app_settings::second_language();
        self.inference_jobs
            .as_ref()
            .ok_or("dictation worker is unavailable")?
            .try_send(InferenceCommand::Transcribe(Box::new(InferenceJob {
                job_id,
                control: control.clone(),
                submitted_at: Instant::now(),
                clip,
                target,
                protocol,
                context,
                selection,
                second_language,
            })))
            .map(|()| {
                state.next_sequence += 1;
                state.jobs.insert(job_id, control);
                job_id
            })
            .map_err(queue_error)
    }

    pub fn prepare_paste(&self) {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if !state.jobs.is_empty() || state.pending_pastes > 0 {
            return;
        }
        drop(state);
        let Some(output_jobs) = &self.output_jobs else {
            return;
        };
        let _ = output_jobs.try_send(OutputJob::PreparePaste);
    }

    pub fn reload(
        &self,
        selection: TranscriptionSelection,
        second_language: Option<SecondLanguage>,
    ) -> Result<(), &'static str> {
        self.inference_jobs
            .as_ref()
            .ok_or("dictation worker is unavailable")?
            .try_send(InferenceCommand::Reload(selection, second_language))
            .map_err(queue_error)
    }

    pub fn paste_last(&self) -> Result<(), &'static str> {
        self.submit_paste(PasteKind::LastTranscript)
    }

    pub fn paste_meeting(&self) -> Result<(), &'static str> {
        self.submit_paste(PasteKind::MeetingDelta)
    }

    fn submit_paste(&self, kind: PasteKind) -> Result<(), &'static str> {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let sequence = state.next_output_sequence()?;
        self.output_jobs
            .as_ref()
            .ok_or("dictation worker is unavailable")?
            .try_send(OutputJob::Paste { sequence, kind })
            .map(|()| {
                state.next_sequence += 1;
                state.pending_pastes += 1;
            })
            .map_err(queue_error)
    }

    pub fn try_recv(&self) -> Option<WorkerEvent> {
        self.events.try_recv().ok()
    }

    pub fn is_busy(&self) -> bool {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        !state.jobs.is_empty() || state.pending_pastes > 0
    }

    pub fn pending_count(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .jobs
            .len()
    }

    pub fn cancel_latest(&self) -> Option<DictationJobId> {
        let state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        let job_id = state.cancel_latest()?;
        drop(state);
        let _ = self
            .output_jobs
            .as_ref()?
            .try_send(OutputJob::Cancelled { job_id });
        Some(job_id)
    }

    fn shutdown(&mut self) {
        self.inference_jobs.take();
        join_worker(self.inference_worker.take(), "dictation inference");
        for worker in self.processor_workers.drain(..) {
            join_worker(Some(worker), "dictation processing");
        }
        self.output_jobs.take();
        join_worker(self.output_worker.take(), "dictation output");
    }
}

impl Drop for DictationWorker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn run_output_worker(
    jobs: Receiver<OutputJob>,
    activity: InputActivity,
    history: Option<History>,
    state: &Mutex<WorkerState>,
    events: &mpsc::Sender<WorkerEvent>,
) {
    let mut paster = Paster::new(activity);
    let mut last_transcript = None;
    let mut meeting_cursor = MeetingPasteCursor::default();
    let mut ordered = OrderedOutputs::default();
    while let Ok(job) = jobs.recv() {
        if matches!(job, OutputJob::PreparePaste) {
            paster.prepare();
            continue;
        }
        for job in ordered.push(job) {
            let event = finish_output(
                job,
                &mut |text, mode, commit| paster.paste(text, mode, commit),
                &mut last_transcript,
                &mut meeting_cursor,
                history.as_ref(),
            );
            let mut state = state.lock().unwrap_or_else(|error| error.into_inner());
            match &event {
                WorkerEvent::Completed { job_id, .. } | WorkerEvent::Cancelled { job_id } => {
                    state.jobs.remove(job_id);
                }
                WorkerEvent::Pasted { .. } => {
                    state.pending_pastes = state.pending_pastes.saturating_sub(1);
                }
                WorkerEvent::ModelFailed(_) | WorkerEvent::Stage { .. } => {}
            }
            drop(state);
            if events.send(event).is_err() {
                return;
            }
        }
    }
}

fn run_processor_worker(
    jobs: &Mutex<Receiver<ProcessorJob>>,
    output: &SyncSender<OutputJob>,
    events: &mpsc::Sender<WorkerEvent>,
    transformations: &crate::personal_commands::TransformationClient,
) {
    loop {
        let job = jobs
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .recv();
        let Ok(job) = job else { break };
        if job.control.is_cancelled() {
            let _ = output.send(OutputJob::Cancelled { job_id: job.job_id });
            continue;
        }
        let mut processed = process_job_text(&job, crate::config::dictation_profiles, || {
            let _ = events.send(WorkerEvent::Stage {
                job_id: job.job_id,
                stage: DictationJobStage::Processing,
            });
        });
        if !processed.transformations.is_empty() && !job.control.is_cancelled() {
            let started = Instant::now();
            let transformed = transformations.transform(
                &processed.transformations,
                &processed.text,
                &job.context,
                &job.control.cancelled,
            );
            let observation = processed
                .observation
                .get_or_insert_with(|| ProcessingObservation {
                    profile: "Custom transformations".into(),
                    latency_ms: 0,
                    fallback: None,
                });
            observation.latency_ms = observation
                .latency_ms
                .saturating_add(started.elapsed().as_millis() as u64);
            match transformed {
                Ok(text) => processed.text = text,
                Err(error) => observation.fallback = Some(error),
            }
        }
        if job.control.is_cancelled() {
            let _ = output.send(OutputJob::Cancelled { job_id: job.job_id });
            continue;
        }
        if let Some(observation) = &processed.observation
            && let Some(error) = &observation.fallback
        {
            tracing::warn!(
                profile = observation.profile,
                %error,
                "dictation processing fell back to the previous pipeline output"
            );
        }
        if output
            .send(OutputJob::Completed {
                job_id: job.job_id,
                control: job.control,
                target: job.target,
                result: Box::new(Ok(CompletedTranscript {
                    text: processed.text,
                    raw: job.text,
                    application: job.context.application,
                    timings: job.timings,
                    processing: processed.observation,
                })),
            })
            .is_err()
        {
            break;
        }
    }
}

/// Select the mode when the worker reaches the job. The stage precedes the
/// transformations when there are any.
fn process_job_text(
    job: &ProcessorJob,
    profiles: impl FnOnce() -> crate::dictation_processor::Profiles,
    processing: impl FnOnce(),
) -> crate::dictation_processor::Processed {
    let profiles = profiles();
    if profiles.processes(&job.context) {
        processing();
    }
    // Local smart editing applies to dictation output only, never to the
    // transcription service, and runs before mode corrections.
    let text = if matches!(
        job.target,
        TranscriptionTarget::Paste | TranscriptionTarget::Send
    ) {
        crate::smart_editing::apply(&job.text, crate::app_settings::smart_editing())
    } else {
        job.text.clone()
    };
    profiles.process(&text, &job.context)
}

fn run_inference_worker(
    commands: Receiver<InferenceCommand>,
    processor_jobs: &SyncSender<ProcessorJob>,
    output: &SyncSender<OutputJob>,
    events: &mpsc::Sender<WorkerEvent>,
) {
    prioritize_inference_thread();
    let mut transcriber = match WarmTranscriber::load() {
        Ok(transcriber) => {
            tracing::info!("transcription model loaded");
            transcriber
        }
        Err(error) => {
            let _ = events.send(WorkerEvent::ModelFailed(error.to_string()));
            WarmTranscriber::default()
        }
    };
    let mut languages = SecondLanguageModels::default();
    languages.prepare(crate::app_settings::second_language().as_ref());
    while let Ok(command) = commands.recv() {
        let job = match command {
            InferenceCommand::Reload(selection, second_language) => {
                if let Err(error) = transcriber.activate(&selection) {
                    let _ = events.send(WorkerEvent::ModelFailed(error.to_string()));
                }
                languages.prepare(second_language.as_ref());
                continue;
            }
            InferenceCommand::Transcribe(job) => *job,
        };
        if job.control.is_cancelled() {
            let _ = output.send(OutputJob::Cancelled { job_id: job.job_id });
            continue;
        }
        let _ = events.send(WorkerEvent::Stage {
            job_id: job.job_id,
            stage: DictationJobStage::Transcribing,
        });
        let routable = job.protocol.is_none()
            && matches!(
                job.target,
                TranscriptionTarget::Paste | TranscriptionTarget::Send
            );
        let route = match job.second_language.as_ref().filter(|_| routable) {
            Some(second) => languages.route(second, &job.clip),
            None => LanguageRoute::Main,
        };
        let collecting = routable
            && job.second_language.is_some()
            && crate::app_settings::language_samples_enabled();
        let sample_spans: Vec<(usize, usize, bool)> = match &route {
            LanguageRoute::Mixed(_, spans) => spans
                .iter()
                .map(|span| (span.start, span.end, span.second))
                .collect(),
            LanguageRoute::Second(_) => vec![(0, job.clip.samples().len(), true)],
            LanguageRoute::Main => vec![(0, job.clip.samples().len(), false)],
        };
        let total_started = job.submitted_at;
        let queue_ms = total_started.elapsed().as_millis();
        let audio_ms = job.clip.duration_ms();
        let prepare_started = Instant::now();
        let clip_samples = job.clip.into_transcription_samples();
        crate::dictation_diagnostics::persist(&clip_samples);
        let prepare_ms = prepare_started.elapsed().as_millis();
        let kept_samples = collecting.then(|| clip_samples.clone());
        let inference_started = Instant::now();
        let result = match route {
            LanguageRoute::Mixed(second_selection, spans) => transcribe_spans(
                &mut transcriber,
                &mut languages,
                &job.selection,
                &second_selection,
                &clip_samples,
                &spans,
            ),
            route => {
                let active = match route {
                    LanguageRoute::Second(selection) => {
                        match languages.second.activate(&selection) {
                            Ok(second) => Ok(second),
                            Err(error) => {
                                tracing::warn!(%error, "second-language model unavailable; using main model");
                                transcriber.activate(&job.selection)
                            }
                        }
                    }
                    _ => transcriber.activate(&job.selection),
                };
                active.and_then(|transcriber| match job.protocol.as_deref() {
                    Some(protocol) => transcriber.transcribe_voice(clip_samples, protocol),
                    None => transcriber.transcribe(clip_samples),
                })
            }
        }
        .map(|text| {
            let corrected = if matches!(job.target, TranscriptionTarget::Service) {
                text.clone()
            } else {
                strip_transcript_protocol(&text, job.protocol.as_deref())
            };
            tracing::debug!(
                raw_transcript = text,
                corrected_transcript = corrected,
                "prepared local transcript"
            );
            corrected
        })
        .map_err(|error| error.to_string());
        if let (Some(samples), Ok(text)) = (&kept_samples, &result) {
            let ms = |sample: usize| (sample / 16) as u64;
            let record = crate::language_samples::SampleRecord {
                recorded_at_ms: crate::events::now_ms(),
                duration_ms: audio_ms,
                routing: format!(
                    "{:?}",
                    job.second_language.as_ref().map(|second| second.routing)
                ),
                pieces: languages
                    .last_detection
                    .iter()
                    .map(
                        |(start, end, language)| crate::language_samples::PieceGuess {
                            start_ms: ms(*start),
                            end_ms: ms(*end),
                            language: language.clone(),
                        },
                    )
                    .collect(),
                spans: sample_spans
                    .iter()
                    .map(|(start, end, second)| crate::language_samples::SampleSpan {
                        start_ms: ms(*start),
                        end_ms: ms(*end),
                        second_language: *second,
                    })
                    .collect(),
                text: text.clone(),
            };
            if let Err(error) = crate::language_samples::save(samples, &record) {
                tracing::warn!(%error, "could not save language sample");
            }
        }
        let timings = JobTimings {
            total_started,
            queue_ms,
            audio_ms,
            prepare_ms,
            inference_ms: inference_started.elapsed().as_millis(),
        };
        if job.control.is_cancelled() {
            let _ = output.send(OutputJob::Cancelled { job_id: job.job_id });
            continue;
        }
        match result {
            Ok(text)
                if matches!(
                    job.target,
                    TranscriptionTarget::Paste | TranscriptionTarget::Send
                ) && !text.trim().is_empty() =>
            {
                if processor_jobs
                    .send(ProcessorJob {
                        job_id: job.job_id,
                        control: job.control,
                        target: job.target,
                        text,
                        context: job.context,
                        timings,
                    })
                    .is_err()
                {
                    break;
                }
            }
            result => {
                let application = job.context.application;
                let result = result.map(|text| CompletedTranscript {
                    raw: text.clone(),
                    text,
                    application,
                    timings,
                    processing: None,
                });
                if output
                    .send(OutputJob::Completed {
                        job_id: job.job_id,
                        control: job.control,
                        target: job.target,
                        result: Box::new(result),
                    })
                    .is_err()
                {
                    break;
                }
            }
        }
    }
}

fn join_worker(worker: Option<thread::JoinHandle<()>>, name: &str) {
    if let Some(worker) = worker
        && worker.join().is_err()
    {
        tracing::error!(worker = name, "worker panicked during shutdown");
    }
}

#[cfg(target_os = "macos")]
pub(crate) fn prioritize_inference_thread() {
    const QOS_CLASS_USER_INITIATED: u32 = 0x19;
    unsafe extern "C" {
        fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: i32) -> i32;
    }
    // SAFETY: This configures only the calling worker thread using the public
    // macOS pthread QoS API. Relative priority zero is valid for this class.
    let status = unsafe { pthread_set_qos_class_self_np(QOS_CLASS_USER_INITIATED, 0) };
    if status == 0 {
        tracing::info!("dictation inference worker uses user-initiated QoS");
    } else {
        tracing::warn!(status, "could not raise dictation inference worker QoS");
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn prioritize_inference_thread() {}

fn queue_error(error: TrySendError<impl Sized>) -> &'static str {
    match error {
        TrySendError::Full(_) => "dictation queue is full",
        TrySendError::Disconnected(_) => "dictation worker is unavailable",
    }
}

fn finish_output(
    job: OutputJob,
    paste: &mut impl FnMut(&str, PasteMode, &dyn Fn() -> bool) -> Result<()>,
    last_transcript: &mut Option<String>,
    meeting_cursor: &mut MeetingPasteCursor,
    history: Option<&History>,
) -> WorkerEvent {
    match job {
        OutputJob::PreparePaste => unreachable!("paste preparation bypasses ordered output"),
        OutputJob::Completed {
            job_id,
            control,
            target,
            result,
        } if !control.is_cancelled() => {
            let result = *result;
            let processing = result
                .as_ref()
                .ok()
                .and_then(|result| result.processing.clone());
            let result = result.and_then(|completed| {
                if control.is_cancelled() {
                    return Err("dictation was cancelled".into());
                }
                let paste_started = Instant::now();
                if !completed.text.trim().is_empty() {
                    let commit = || control.begin_output();
                    match target {
                        TranscriptionTarget::Paste => {
                            paste(&completed.text, PasteMode::Continue, &commit)
                        }
                        TranscriptionTarget::Send => {
                            paste(&completed.text, PasteMode::Send, &commit)
                        }
                        TranscriptionTarget::Service => commit()
                            .then_some(())
                            .ok_or_else(|| eyre!("dictation was cancelled")),
                    }
                    .map_err(|error| error.to_string())?;
                    if matches!(
                        target,
                        TranscriptionTarget::Paste | TranscriptionTarget::Send
                    ) {
                        *last_transcript = Some(completed.text.clone());
                    }
                    if let Some(history) = history {
                        record_history(history, target, &completed);
                    }
                }
                tracing::info!(
                    audio_ms = completed.timings.audio_ms,
                    queue_ms = completed.timings.queue_ms,
                    prepare_ms = completed.timings.prepare_ms,
                    inference_ms = completed.timings.inference_ms,
                    paste_ms = paste_started.elapsed().as_millis(),
                    total_ms = completed.timings.total_started.elapsed().as_millis(),
                    "dictation pipeline completed"
                );
                Ok(completed.text)
            });
            if control.is_cancelled() {
                WorkerEvent::Cancelled { job_id }
            } else {
                WorkerEvent::Completed {
                    job_id,
                    target,
                    result,
                    processing,
                }
            }
        }
        OutputJob::Completed { job_id, .. } | OutputJob::Cancelled { job_id } => {
            WorkerEvent::Cancelled { job_id }
        }
        OutputJob::Paste { kind, .. } => {
            let result = match kind {
                PasteKind::LastTranscript => last_transcript
                    .as_deref()
                    .ok_or_else(|| "no previous transcript is available".to_string())
                    .and_then(|text| {
                        paste(text, PasteMode::Continue, &|| true)
                            .map_err(|error| error.to_string())?;
                        Ok(text.to_string())
                    }),
                PasteKind::MeetingDelta => {
                    paste_meeting_delta(paste, meeting_cursor).map_err(|error| error.to_string())
                }
            };
            WorkerEvent::Pasted { kind, result }
        }
    }
}

/// Record one successfully pasted result. History failures must never fail
/// the paste that already happened.
fn record_history(history: &History, target: TranscriptionTarget, completed: &CompletedTranscript) {
    let kind = match target {
        TranscriptionTarget::Paste => HistoryKind::Dictation,
        TranscriptionTarget::Send => HistoryKind::Send,
        TranscriptionTarget::Service => return,
    };
    let draft = HistoryDraft {
        kind,
        raw_text: completed.raw.clone(),
        final_text: completed.text.clone(),
        application: completed.application.clone(),
        processing: completed.processing.clone(),
        audio_ms: completed.timings.audio_ms,
        inference_ms: completed.timings.inference_ms as u64,
        total_ms: completed.timings.total_started.elapsed().as_millis() as u64,
    };
    if let Err(error) = history.record(draft) {
        tracing::warn!(%error, "could not record dictation history");
    }
}

fn strip_transcript_protocol(text: &str, protocol: Option<&DictationProtocol>) -> String {
    protocol.map_or_else(|| text.trim().to_string(), |protocol| protocol.strip(text))
}

#[derive(Default)]
struct MeetingPasteCursor {
    meeting_id: Option<String>,
    publication: Option<TranscriptPublication>,
    seen: HashSet<TranscriptEntry>,
}

fn paste_meeting_delta(
    paste: &mut impl FnMut(&str, PasteMode, &dyn Fn() -> bool) -> Result<()>,
    cursor: &mut MeetingPasteCursor,
) -> Result<String> {
    let meetings = meeting::list()?;
    let selected = meeting::active_or_latest(&meetings)
        .ok_or_else(|| eyre!("no meeting transcript is available"))?;
    // Live and final ASR use different segment boundaries, so switching a
    // cursor between them could repeat or skip an overlapping segment.
    let same_meeting = cursor.meeting_id.as_deref() == Some(&selected.id);
    let empty = HashSet::new();
    let (publication, seen) = if same_meeting {
        (cursor.publication, &cursor.seen)
    } else {
        (None, &empty)
    };
    let transcript = meeting::completed_transcript(&selected.id, publication)?;
    let (text, pasted_entries) = prepare_meeting_delta(&transcript.entries, seen)?;
    paste(&text, PasteMode::Standalone, &|| true)?;
    if !same_meeting {
        cursor.seen.clear();
    }
    cursor.meeting_id = Some(selected.id.clone());
    cursor.publication = Some(transcript.publication);
    cursor.seen.extend(pasted_entries);
    Ok(text)
}

fn prepare_meeting_delta(
    entries: &[TranscriptEntry],
    seen: &HashSet<TranscriptEntry>,
) -> Result<(String, Vec<TranscriptEntry>)> {
    let new_entries = entries
        .iter()
        .filter(|entry| !seen.contains(*entry))
        .cloned()
        .collect::<Vec<_>>();
    if new_entries.is_empty() {
        return Err(eyre!("no new completed meeting transcript is available"));
    }
    let text = meeting::coalesce_transcript(new_entries.iter().cloned())
        .into_iter()
        .map(|entry| format!("{}: {}", entry.source.label(), entry.text))
        .collect::<Vec<_>>()
        .join("\n\n");
    Ok((text, new_entries))
}

pub(crate) fn default_model_path() -> Result<std::path::PathBuf> {
    model_path(crate::transcription_models::definition(
        crate::transcription_models::TranscriptionModelId::default(),
    ))
}

/// Which model a dictation goes to when a second language is on.
enum LanguageRoute {
    Main,
    Second(TranscriptionSelection),
    /// Speech that switches language: each span goes to its own model.
    Mixed(TranscriptionSelection, Vec<LanguageSpan>),
}

/// A stretch of a recording, in samples, and whether it is in the second
/// language.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LanguageSpan {
    pub start: usize,
    pub end: usize,
    pub second: bool,
}

const FRAME_SAMPLES: usize = 320;
/// Pieces are cut at the quietest moment between these lengths, so a switch
/// of language mid-sentence lands in a different piece from the words before.
const PIECE_MIN_SAMPLES: usize = 16_000 * 3 / 2;
const PIECE_MAX_SAMPLES: usize = 16_000 * 3;
/// Too little speech to name its language; it borrows a neighbour's.
const PIECE_DETECTABLE_SAMPLES: usize = 16_000 * 7 / 10;

/// Splits a recording at its quietest moments into pieces of 1.5 to 3 seconds.
pub(crate) fn language_pieces(samples: &[f32]) -> Vec<(usize, usize)> {
    let frames: Vec<f32> = samples
        .chunks(FRAME_SAMPLES)
        .map(|frame| {
            (frame.iter().map(|sample| sample * sample).sum::<f32>() / frame.len() as f32).sqrt()
        })
        .collect();
    // Smooth over 60 ms so a single quiet frame inside a word is not a cut.
    let smoothed: Vec<f32> = (0..frames.len())
        .map(|index| {
            let from = index.saturating_sub(1);
            let to = (index + 2).min(frames.len());
            frames[from..to].iter().sum::<f32>() / (to - from) as f32
        })
        .collect();
    let mut pieces = Vec::new();
    let mut start = 0;
    while samples.len() - start > PIECE_MAX_SAMPLES {
        let first = (start + PIECE_MIN_SAMPLES) / FRAME_SAMPLES;
        let last =
            ((start + PIECE_MAX_SAMPLES) / FRAME_SAMPLES).min(smoothed.len().saturating_sub(1));
        let cut = (first..=last)
            .min_by(|left, right| smoothed[*left].total_cmp(&smoothed[*right]))
            .map_or(start + PIECE_MAX_SAMPLES, |frame| {
                frame * FRAME_SAMPLES + FRAME_SAMPLES / 2
            });
        pieces.push((start, cut));
        start = cut;
    }
    if start < samples.len() {
        // A short tail joins the piece before it.
        match pieces.last_mut() {
            Some(last) if samples.len() - start < PIECE_DETECTABLE_SAMPLES => {
                last.1 = samples.len()
            }
            _ => pieces.push((start, samples.len())),
        }
    }
    pieces
}

/// Joins per-piece language guesses into spans. Pieces with no guess take the
/// language of the piece before them (or after, at the start).
pub(crate) fn language_spans(
    pieces: &[(usize, usize)],
    guesses: &[Option<bool>],
    fallback: bool,
) -> Vec<LanguageSpan> {
    let mut filled: Vec<bool> = Vec::with_capacity(guesses.len());
    for (index, guess) in guesses.iter().enumerate() {
        let value = guess
            .or_else(|| filled.last().copied())
            .or_else(|| guesses[index..].iter().flatten().next().copied())
            .unwrap_or(fallback);
        filled.push(value);
    }
    let mut spans: Vec<LanguageSpan> = Vec::new();
    for ((start, end), second) in pieces.iter().zip(filled) {
        match spans.last_mut() {
            Some(last) if last.second == second => last.end = *end,
            _ => spans.push(LanguageSpan {
                start: *start,
                end: *end,
                second,
            }),
        }
    }
    spans
}

/// The second-language model and the language detector, kept warm beside
/// the main model while the second language is switched on.
#[derive(Default)]
struct SecondLanguageModels {
    second: WarmTranscriber,
    detector: Option<LanguageDetector>,
    /// The last confident choice, reused for clips too short to detect.
    last_was_second: bool,
    /// What the detector heard in each piece of the latest recording.
    last_detection: Vec<(usize, usize, Option<String>)>,
}

/// Recordings shorter than this reuse the previous language: Whisper's
/// language guess is unreliable on a word or two.
const MIN_DETECTION_MS: u64 = 900;

impl SecondLanguageModels {
    fn prepare(&mut self, second: Option<&SecondLanguage>) {
        let Some(second) = second else {
            *self = Self::default();
            return;
        };
        if let Err(error) = self.second.activate(&second.selection) {
            tracing::warn!(%error, "second-language model is not ready");
        }
        if second.routing == LanguageRouting::Auto {
            self.ensure_detector();
        } else {
            self.detector = None;
        }
    }

    fn ensure_detector(&mut self) -> Option<&mut LanguageDetector> {
        if self.detector.is_none() {
            match LanguageDetector::load() {
                Ok(detector) => self.detector = Some(detector),
                Err(error) => tracing::warn!(%error, "language detector is not ready"),
            }
        }
        self.detector.as_mut()
    }

    /// Guesses the language of each piece of a recording. None when the
    /// detector is unavailable.
    fn detect_spans(
        &mut self,
        second_language: &str,
        samples: &[f32],
        fallback: bool,
    ) -> Option<Vec<LanguageSpan>> {
        let detector = self.ensure_detector()?;
        let pieces = language_pieces(samples);
        let detected: Vec<Option<String>> = pieces
            .iter()
            .map(|(start, end)| {
                if end - start < PIECE_DETECTABLE_SAMPLES {
                    return None;
                }
                match detector.detect(&samples[*start..*end]) {
                    Ok(language) => language,
                    Err(error) => {
                        tracing::warn!(%error, "language detection failed");
                        None
                    }
                }
            })
            .collect();
        let guesses: Vec<Option<bool>> = detected
            .iter()
            .map(|language| {
                language
                    .as_deref()
                    .map(|language| belongs_to(language, second_language))
            })
            .collect();
        self.last_detection = pieces
            .iter()
            .zip(detected)
            .map(|((start, end), language)| (*start, *end, language))
            .collect();
        Some(language_spans(&pieces, &guesses, fallback))
    }

    fn route(&mut self, second: &SecondLanguage, clip: &DictationClip) -> LanguageRoute {
        let second_installed = crate::transcription_models::is_installed(
            crate::transcription_models::definition(second.selection.model),
            &second.selection.language,
        );
        if !second_installed {
            return LanguageRoute::Main;
        }
        let to_second = match second.routing {
            LanguageRouting::Main => false,
            LanguageRouting::Second => true,
            LanguageRouting::Auto if clip.duration_ms() < MIN_DETECTION_MS => self.last_was_second,
            LanguageRouting::Auto => {
                let started = Instant::now();
                let fallback = self.last_was_second;
                let Some(spans) =
                    self.detect_spans(&second.selection.language, clip.samples(), fallback)
                else {
                    return if fallback {
                        LanguageRoute::Second(second.selection.clone())
                    } else {
                        LanguageRoute::Main
                    };
                };
                tracing::info!(
                    spans = ?spans.iter().map(|span| (span.end - span.start, span.second)).collect::<Vec<_>>(),
                    detect_ms = started.elapsed().as_millis(),
                    "detected dictation languages"
                );
                if let Some(last) = spans.last() {
                    self.last_was_second = last.second;
                }
                if spans.iter().any(|span| !span.second) {
                    // Main-language spans get checked for missed phrases.
                    return LanguageRoute::Mixed(second.selection.clone(), spans);
                }
                !spans.is_empty()
            }
        };
        if to_second {
            LanguageRoute::Second(second.selection.clone())
        } else {
            LanguageRoute::Main
        }
    }
}

/// Whether macOS's English spell checker knows `word`.
pub(crate) fn is_english_word(word: &str) -> bool {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject, Bool};
    use objc2_foundation::{NSRange, NSString};
    let Some(class) = AnyClass::get(c"NSSpellChecker") else {
        return true;
    };
    let text = NSString::from_str(word);
    let language = NSString::from_str("en");
    let misspelled: NSRange = unsafe {
        let checker: *mut AnyObject = msg_send![class, sharedSpellChecker];
        if checker.is_null() {
            return true;
        }
        let mut count: isize = 0;
        msg_send![
            checker,
            checkSpellingOfString: &*text,
            startingAt: 0_isize,
            language: &*language,
            wrap: Bool::NO,
            inSpellDocumentWithTag: 0_isize,
            wordCount: &mut count
        ]
    };
    misspelled.length == 0
}

/// Transcribes each language span with its own model and joins the text.
/// Main-language spans get a second look for short second-language phrases
/// the language detector could not catch.
fn transcribe_spans(
    main: &mut WarmTranscriber,
    languages: &mut SecondLanguageModels,
    main_selection: &TranscriptionSelection,
    second_selection: &TranscriptionSelection,
    samples: &[f32],
    spans: &[LanguageSpan],
) -> Result<String> {
    let mut parts: Vec<String> = Vec::new();
    for span in spans {
        let piece = &samples[span.start.min(samples.len())..span.end.min(samples.len())];
        let text = if span.second {
            match languages.second.activate(second_selection) {
                Ok(model) => model.transcribe(piece.to_vec())?,
                Err(error) => {
                    tracing::warn!(%error, "second-language model unavailable; using main model");
                    main.activate(main_selection)?.transcribe(piece.to_vec())?
                }
            }
        } else {
            transcribe_main_span(main, languages, main_selection, second_selection, piece)?
        };
        let text = text.trim();
        if !text.is_empty() {
            parts.push(text.to_string());
        }
    }
    Ok(parts.join(" "))
}

/// Transcribes main-language audio, then re-transcribes any run of words the
/// English spell checker does not know (a Bangla phrase heard as English
/// sounds) with the second-language model.
fn transcribe_main_span(
    main: &mut WarmTranscriber,
    languages: &mut SecondLanguageModels,
    main_selection: &TranscriptionSelection,
    second_selection: &TranscriptionSelection,
    piece: &[f32],
) -> Result<String> {
    let model = main.activate(main_selection)?;
    let Some(timed) = model.transcribe_words(piece.to_vec()) else {
        return model.transcribe(piece.to_vec());
    };
    let timed = timed?;
    let flags: Vec<bool> = timed
        .words
        .iter()
        .map(|word| looks_foreign(&word.text))
        .collect();
    let runs = foreign_runs(&flags);
    if runs.is_empty() {
        return Ok(timed.text);
    }
    let piece_ms = (piece.len() * 1_000 / 16_000) as u64;
    let to_sample = |ms: u64| ((ms as usize) * 16).min(piece.len());
    let mut out: Vec<String> = Vec::new();
    let mut next = 0;
    for (first, last) in runs {
        // Word timings run late; start a little inside the previous word so
        // the phrase's first sound is not clipped.
        let start_ms = if first > 0 {
            let previous = &timed.words[first - 1];
            previous
                .end_ms
                .saturating_sub(PHRASE_LEAD_MS)
                .max(previous.start_ms)
        } else {
            0
        };
        let end_ms = timed
            .words
            .get(last + 1)
            .map_or(piece_ms, |word| word.start_ms)
            .max(start_ms);
        let audio = &piece[to_sample(start_ms)..to_sample(end_ms)];
        if audio.len() < 16_000 / 4 {
            continue;
        }
        let foreign_words = flags[first..=last].iter().filter(|flag| **flag).count();
        // Two unknown words could be names; let the detector confirm.
        let confirmed = foreign_words >= 3
            || languages.ensure_detector().is_some_and(|detector| {
                detector
                    .detect(audio)
                    .ok()
                    .flatten()
                    .is_some_and(|language| belongs_to(&language, &second_selection.language))
            });
        if !confirmed {
            continue;
        }
        let Ok(second) = languages.second.activate(second_selection) else {
            break;
        };
        let replacement = second.transcribe(audio.to_vec())?;
        tracing::info!(
            heard = timed.words[first..=last]
                .iter()
                .map(|word| word.text.as_str())
                .collect::<Vec<_>>()
                .join(" "),
            replacement,
            "re-transcribed a second-language phrase"
        );
        out.extend(
            timed.words[next..first]
                .iter()
                .map(|word| word.text.clone()),
        );
        if !replacement.trim().is_empty() {
            out.push(replacement.trim().to_string());
        }
        next = last + 1;
    }
    if next == 0 {
        return Ok(timed.text);
    }
    out.extend(timed.words[next..].iter().map(|word| word.text.clone()));
    Ok(out.join(" "))
}

const PHRASE_LEAD_MS: u64 = 240;

/// A word the English spell checker does not know. Acronyms, numbers and
/// one-letter words are never foreign.
fn looks_foreign(word: &str) -> bool {
    let clean: String = word
        .chars()
        .filter(|character| character.is_alphabetic() || *character == '\'')
        .collect();
    let letters = clean
        .chars()
        .filter(|character| character.is_alphabetic())
        .count();
    if letters < 2
        || word.chars().any(|character| character.is_ascii_digit())
        || clean.chars().all(|character| !character.is_lowercase())
        || !clean.is_ascii()
    {
        return false;
    }
    !is_english_word(&clean)
}

/// Runs of at least two unknown words, allowing up to two known words between
/// them; a run within two words of either edge reaches that edge.
pub(crate) fn foreign_runs(flags: &[bool]) -> Vec<(usize, usize)> {
    let mut runs: Vec<(usize, usize, usize)> = Vec::new();
    for (index, foreign) in flags.iter().enumerate() {
        if !foreign {
            continue;
        }
        match runs.last_mut() {
            Some((_, last, count)) if index - *last <= 3 => {
                *last = index;
                *count += 1;
            }
            _ => runs.push((index, index, 1)),
        }
    }
    runs.into_iter()
        .filter(|(_, _, count)| *count >= 2)
        .map(|(first, last, _)| {
            let first = if first <= 2 { 0 } else { first };
            let last = if last + 3 >= flags.len() {
                flags.len() - 1
            } else {
                last
            };
            (first, last)
        })
        .collect()
}

/// Whether a detected language should go to the second-language model.
/// For Bangla, Whisper labels many pieces as Hindi, Urdu or even Turkish, so
/// everything except English and the languages Whisper mistakes accented
/// English for counts as Bangla. Other second languages must match exactly.
pub(crate) fn belongs_to(detected: &str, second: &str) -> bool {
    const ENGLISH_LOOKALIKES: &[&str] = &[
        "en", "cy", "nn", "no", "da", "sv", "de", "nl", "la", "haw", "jw",
    ];
    detected == second || (second == "bn" && !ENGLISH_LOOKALIKES.contains(&detected))
}

/// Whisper base run only to read which language it hears.
pub(crate) struct LanguageDetector {
    model: Parakeet,
}

/// Ten seconds is plenty to tell languages apart and keeps detection quick.
const DETECTION_WINDOW_SAMPLES: usize = 16_000 * 10;

impl LanguageDetector {
    pub(crate) fn load() -> Result<Self> {
        let definition =
            crate::transcription_models::definition(TranscriptionModelId::WhisperBaseLanguageId);
        if !crate::transcription_models::is_installed(definition, "auto") {
            return Err(eyre!("{} is not installed", definition.name));
        }
        let detector = Self::load_from(&model_path(definition)?)?;
        tracing::info!("language detector loaded");
        Ok(detector)
    }

    fn load_from(path: &std::path::Path) -> Result<Self> {
        let mut model = Parakeet::load_from(path, true, None)?;
        model.options.timestamps = TimestampKind::None;
        model.options.language = None;
        Ok(Self { model })
    }

    pub(crate) fn detect(&mut self, samples: &[f32]) -> Result<Option<String>> {
        let mut window = samples[..samples.len().min(DETECTION_WINDOW_SAMPLES)].to_vec();
        pad_for_parakeet(&mut window);
        Ok(self.model.transcribe_segments(&window)?.language)
    }
}

impl Parakeet {
    #[cfg(test)]
    pub fn load() -> Result<Self> {
        let (_, selection) = crate::app_settings::transcription_selection();
        Self::load_selection(&selection)
    }

    pub fn load_selection(selection: &TranscriptionSelection) -> Result<Self> {
        let definition = validate(selection)?;
        if !crate::transcription_models::is_installed(definition, &selection.language) {
            return Err(eyre!("{} is not installed", definition.name));
        }
        let path = model_path(definition)?;
        let mut parakeet = Self::load_from(&path, true, Some(selection))?;
        let prewarm_started = Instant::now();
        let mut silence = Vec::new();
        pad_for_parakeet(&mut silence);
        parakeet
            .transcribe(&silence)
            .wrap_err("could not prewarm transcription model")?;
        tracing::info!(
            prewarm_ms = prewarm_started.elapsed().as_millis(),
            "prewarmed transcription model"
        );
        Ok(parakeet)
    }

    pub(crate) fn load_for_benchmark(model_path: &std::path::Path) -> Result<Self> {
        Self::load_from(model_path, false, None)
    }

    fn load_from(
        model_path: &std::path::Path,
        diagnostics: bool,
        selection: Option<&TranscriptionSelection>,
    ) -> Result<Self> {
        if diagnostics {
            TRANSCRIBE_LOGGING.call_once(transcribe_cpp::init_logging);
        } else {
            transcribe_cpp::disable_logging();
        }
        let model = Model::load_with(
            model_path,
            &ModelOptions {
                backend: Backend::Metal,
                device: None,
            },
        )
        .wrap_err_with(|| {
            format!(
                "could not load transcription model from {}",
                model_path.display()
            )
        })?;
        let device = model.device()?;
        if device.kind != "metal" {
            return Err(eyre!(
                "transcription model selected {} ({}) instead of Metal",
                device.kind,
                device.name
            ));
        }
        let variant = model.variant();
        let architecture = model.arch();
        let name = format!("transcribe-cpp-{}-{variant}", device.kind);
        let definition = selection.map(validate).transpose()?;
        tracing::info!(
            backend = device.kind,
            device = device.description,
            variant,
            "loaded transcription model"
        );
        let capabilities = model.capabilities();
        let mut language = None;
        let mut family = None;
        if let (Some(selection), Some(definition)) = (selection, definition) {
            let runtime_language = crate::gguf_session::validate_gguf_artifact(
                model_path,
                definition,
                &architecture,
                &variant,
                selection,
                capabilities.supports_language_detect,
            )?;
            if let Some(runtime_language) = &runtime_language
                && !capabilities.languages.is_empty()
                && !capabilities
                    .languages
                    .iter()
                    .any(|language| language == runtime_language)
            {
                return Err(eyre!(
                    "{} does not advertise support for {}",
                    definition.name,
                    crate::transcription_models::language_name(&selection.language)
                ));
            }
            if definition.supports_recognition_hints {
                if !model.accepts_ext(ExtSlot::Run, TRANSCRIBE_EXT_KIND_WHISPER_RUN) {
                    return Err(eyre!(
                        "{} does not accept Whisper recognition hints",
                        definition.name
                    ));
                }
                family = Some(RunExtension::Whisper(WhisperRunOptions {
                    initial_prompt: (!selection.recognition_hints.trim().is_empty())
                        .then(|| selection.recognition_hints.trim().to_string()),
                    ..Default::default()
                }));
            }
            language = runtime_language;
        }
        let session = OfflineGgufSession::new(model)?;
        Ok(Self {
            session,
            options: RunOptions {
                timestamps: match capabilities.max_timestamp_kind {
                    TimestampKind::Word | TimestampKind::Token => TimestampKind::Word,
                    TimestampKind::Auto | TimestampKind::Segment => TimestampKind::Segment,
                    TimestampKind::None => TimestampKind::None,
                },
                language,
                family,
                ..Default::default()
            },
            name,
            selection: selection.cloned(),
            max_audio_samples: crate::transcription_models::max_audio_chunk_samples(
                &architecture,
                capabilities.max_audio_ms,
            ),
        })
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn matches_selection(&self, selection: &TranscriptionSelection) -> bool {
        self.selection.as_ref() == Some(selection)
    }

    pub(crate) fn model_id(&self) -> Option<TranscriptionModelId> {
        self.selection.as_ref().map(|selection| selection.model)
    }

    /// Low-level GGUF entry point over prepared audio. `Transcriber` owns the
    /// whole-clip padding; chunks and control-trimmed reruns below only apply
    /// the minimum duration, never another model-specific trailing context.
    pub fn transcribe(&mut self, samples: &[f32]) -> Result<String> {
        let Some(max_audio_samples) = self
            .max_audio_samples
            .filter(|&max_samples| samples.len() > max_samples)
        else {
            return self.transcribe_segments(samples).map(|result| result.text);
        };
        let mut text = Vec::new();
        for chunk in samples.chunks(max_audio_samples) {
            let mut chunk = chunk.to_vec();
            pad_for_parakeet(&mut chunk);
            let result = self.transcribe_segments(&chunk)?;
            let result = result.text.trim();
            if !result.is_empty() {
                text.push(result.to_string());
            }
        }
        Ok(text.join(" "))
    }

    pub fn transcribe_voice(
        &mut self,
        samples: &[f32],
        protocol: &DictationProtocol,
    ) -> Result<String> {
        if self
            .max_audio_samples
            .is_some_and(|maximum| samples.len() > maximum)
        {
            return self.transcribe(samples);
        }
        let transcript = self.transcribe_segments(samples)?;
        let words = transcript
            .words
            .iter()
            .map(|word| word.text.clone())
            .collect::<Vec<_>>();
        let Some(control_words) = protocol.control_suffix_word_count(&words) else {
            return Ok(transcript.text);
        };
        let control = &transcript.words[transcript.words.len() - control_words];
        let cut = (control.t0_ms.max(0) as usize * TRANSCRIPTION_SAMPLES_PER_MS).min(samples.len());
        if cut == 0 || cut == samples.len() {
            return Ok(transcript.text);
        }
        let mut content = samples[..cut].to_vec();
        pad_for_parakeet(&mut content);
        tracing::debug!(
            control_start_ms = control.t0_ms,
            "retranscribing voice dictation without control audio"
        );
        self.transcribe(&content)
    }

    /// Whether `samples` are short enough to transcribe in one pass.
    pub fn fits_one_pass(&self, samples: usize) -> bool {
        self.max_audio_samples.is_none_or(|max| samples <= max)
    }

    pub fn transcribe_segments(&mut self, samples: &[f32]) -> Result<Transcript> {
        self.session
            .run(samples, &self.options)
            .wrap_err("transcription failed")
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn pieces_cut_at_the_quietest_moment_and_keep_short_tails() {
        let mut samples = vec![0.2_f32; 16_000 * 2];
        samples.extend(vec![0.0; 1_600]);
        samples.extend(vec![0.2; 16_000 * 2]);
        samples.extend(vec![0.2; 4_000]);
        let pieces = language_pieces(&samples);
        assert_eq!(pieces.len(), 2);
        let cut = pieces[0].1;
        assert!((32_000..33_600).contains(&cut), "cut at {cut}");
        assert_eq!(pieces[1].1, samples.len());
        assert_eq!(language_pieces(&vec![0.1; 16_000 * 2]), vec![(0, 32_000)]);
    }

    #[test]
    fn foreign_runs_need_two_unknown_words_and_reach_nearby_edges() {
        let f = false;
        let t = true;
        assert_eq!(foreign_runs(&[f, f, f, t, f, f, f, f]), vec![]);
        assert_eq!(
            foreign_runs(&[f, f, f, f, t, f, t, f, f, f, f]),
            vec![(4, 6)]
        );
        assert_eq!(foreign_runs(&[f, t, t, f, f, f, f, f]), vec![(0, 2)]);
        assert_eq!(foreign_runs(&[f, f, f, f, f, t, f, t, f]), vec![(5, 8)]);
    }

    #[test]
    fn english_words_and_romanized_bangla_are_told_apart() {
        for word in [
            "screens", "improved", "tomorrow", "email", "meeting", "GGUF", "a",
        ] {
            assert!(!looks_foreign(word), "{word}");
        }
        for word in ["jabuna", "shorir", "kalke", "pathab"] {
            assert!(looks_foreign(word), "{word}");
        }
    }

    #[test]
    fn spans_merge_neighbours_and_fill_unknown_pieces() {
        let pieces = [(0, 10), (10, 20), (20, 30), (30, 40)];
        let spans = language_spans(&pieces, &[Some(false), None, Some(true), Some(true)], false);
        assert_eq!(
            spans,
            vec![
                LanguageSpan {
                    start: 0,
                    end: 20,
                    second: false
                },
                LanguageSpan {
                    start: 20,
                    end: 40,
                    second: true
                },
            ]
        );
        let spans = language_spans(&pieces[..2], &[None, Some(true)], false);
        assert_eq!(
            spans,
            vec![LanguageSpan {
                start: 0,
                end: 20,
                second: true
            }]
        );
    }

    #[test]
    #[ignore = "needs HELLO_DETECT_MODELS (gguf paths) and HELLO_DETECT_CLIPS (wav paths), colon separated"]
    fn compares_language_detectors_piece_by_piece() {
        let models = std::env::var("HELLO_DETECT_MODELS").unwrap();
        let clips = std::env::var("HELLO_DETECT_CLIPS").unwrap();
        let clips: Vec<(String, Vec<f32>)> = clips
            .split(':')
            .map(|path| {
                let mut reader = hound::WavReader::open(path).unwrap();
                let samples = match reader.spec().sample_format {
                    hound::SampleFormat::Float => {
                        reader.samples::<f32>().map(Result::unwrap).collect()
                    }
                    hound::SampleFormat::Int => reader
                        .samples::<i16>()
                        .map(|sample| sample.unwrap() as f32 / 32_768.0)
                        .collect(),
                };
                let name = std::path::Path::new(path)
                    .file_stem()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned();
                (name, samples)
            })
            .collect();
        for model in models.split(':') {
            let mut detector = LanguageDetector::load_from(std::path::Path::new(model)).unwrap();
            let mut total_ms = 0;
            eprintln!("== {model}");
            for (name, samples) in &clips {
                let started = Instant::now();
                let guesses: Vec<String> = language_pieces(samples)
                    .iter()
                    .map(|(start, end)| {
                        let language = if end - start < PIECE_DETECTABLE_SAMPLES {
                            "--".to_string()
                        } else {
                            detector
                                .detect(&samples[*start..*end])
                                .unwrap()
                                .unwrap_or_else(|| "??".into())
                        };
                        format!(
                            "{:.1}-{:.1}s {language}",
                            *start as f32 / 16_000.0,
                            *end as f32 / 16_000.0
                        )
                    })
                    .collect();
                total_ms += started.elapsed().as_millis();
                eprintln!("  {name}: {}", guesses.join(" | "));
            }
            eprintln!("  detect time {total_ms} ms");
        }
    }

    #[test]
    #[ignore = "needs installed models and HELLO_MIXED_DIR with mixed-language wav clips"]
    fn transcribes_speech_that_switches_language() {
        let directory = std::env::var("HELLO_MIXED_DIR").unwrap();
        let main_selection = TranscriptionSelection::default();
        let second_selection = TranscriptionSelection {
            model: TranscriptionModelId::WhisperLargeV3TurboBengali,
            language: "bn".into(),
            recognition_hints: String::new(),
        };
        let mut main = WarmTranscriber::default();
        main.activate(&main_selection).unwrap();
        let mut languages = SecondLanguageModels::default();
        languages.prepare(Some(&crate::app_settings::SecondLanguage {
            routing: LanguageRouting::Auto,
            selection: second_selection.clone(),
        }));
        let mut names: Vec<_> = std::fs::read_dir(&directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "wav"))
            .collect();
        names.sort();
        for path in names {
            let mut reader = hound::WavReader::open(&path).unwrap();
            let samples: Vec<f32> = reader.samples::<f32>().map(Result::unwrap).collect();
            let started = Instant::now();
            let spans = languages.detect_spans("bn", &samples, false).unwrap();
            let detect_ms = started.elapsed().as_millis();
            let text = transcribe_spans(
                &mut main,
                &mut languages,
                &main_selection,
                &second_selection,
                &samples,
                &spans,
            )
            .unwrap();
            let layout: Vec<String> = spans
                .iter()
                .map(|span| {
                    format!(
                        "{}{:.1}s",
                        if span.second { "BN " } else { "EN " },
                        (span.end - span.start) as f32 / 16_000.0
                    )
                })
                .collect();
            eprintln!(
                "{}: [{}] detect {detect_ms} ms, total {} ms\n  {text}",
                path.file_stem().unwrap().to_string_lossy(),
                layout.join(", "),
                started.elapsed().as_millis()
            );
        }
    }

    use std::path::PathBuf;

    use hound::WavReader;

    use super::*;

    use crate::dictation::resample_for_parakeet;

    #[test]
    fn detected_indic_neighbours_route_to_bangla_and_english_stays_main() {
        for language in ["bn", "as", "hi", "ur", "tr"] {
            assert!(belongs_to(language, "bn"), "{language}");
        }
        for language in ["en", "cy", "de", "nn"] {
            assert!(!belongs_to(language, "bn"), "{language}");
        }
        assert!(belongs_to("fr", "fr"));
        assert!(!belongs_to("hi", "fr"));
    }
    use crate::meeting::MeetingSource;

    fn processing_job(target: TranscriptionTarget) -> ProcessorJob {
        ProcessorJob {
            job_id: DictationJobId(0),
            control: Arc::new(JobControl::default()),
            target,
            text: "raw instruction".into(),
            context: ContextSnapshot::default(),
            timings: JobTimings {
                total_started: Instant::now(),
                queue_ms: 0,
                audio_ms: 1000,
                prepare_ms: 0,
                inference_ms: 0,
            },
        }
    }

    #[test]
    fn ordinary_processing_snapshots_modes_before_stage_and_keeps_transformations() {
        use crate::dictation_processor::{Profile, Profiles};
        use std::cell::Cell;

        for target in [TranscriptionTarget::Paste, TranscriptionTarget::Send] {
            for transformations in [vec![], vec!["fixture-transform".to_string()]] {
                let loaded = Cell::new(false);
                let stage_sent = Cell::new(false);
                let processed = process_job_text(
                    &processing_job(target),
                    || {
                        assert!(!stage_sent.get());
                        loaded.set(true);
                        Profiles::new(Profile::default().transformations(transformations.clone()))
                    },
                    || {
                        assert!(loaded.get());
                        stage_sent.set(true);
                    },
                );
                assert!(loaded.get());
                assert_eq!(stage_sent.get(), !transformations.is_empty());
                assert_eq!(processed.text, "raw instruction");
                assert_eq!(processed.transformations, transformations);
            }
        }
    }

    #[test]
    #[ignore = "requires HELLO_COHERE_MODEL and HELLO_COHERE_FIXTURES synthetic audio"]
    fn cohere_long_form_keeps_all_sections_with_and_without_pauses() {
        let model_path = PathBuf::from(std::env::var_os("HELLO_COHERE_MODEL").expect("model path"));
        let fixtures =
            PathBuf::from(std::env::var_os("HELLO_COHERE_FIXTURES").expect("fixture path"));
        let selection = TranscriptionSelection {
            model: TranscriptionModelId::CohereTranscribe,
            language: "en".into(),
            recognition_hints: String::new(),
        };
        let mut model = Parakeet::load_from(&model_path, false, Some(&selection)).unwrap();
        eprintln!(
            "Cohere maximum chunk: {:?} samples",
            model.max_audio_samples
        );
        for (name, repeats) in [
            ("long-continuous", 1),
            ("long-pauses", 1),
            ("long-repeated", 3),
        ] {
            let mut reader = WavReader::open(fixtures.join(format!("{name}.wav"))).unwrap();
            assert_eq!(reader.spec().sample_rate, 16_000);
            assert_eq!(reader.spec().channels, 1);
            let samples = reader
                .samples::<i16>()
                .map(|sample| f32::from(sample.unwrap()) / 32768.0)
                .collect::<Vec<_>>();
            assert!(samples.len() > 35 * 16_000);
            let text = model.transcribe(&samples).unwrap().to_lowercase();
            for phrase in [
                "small community garden",
                "bright green door",
                "purple submarine",
            ] {
                assert_eq!(
                    text.matches(phrase).count(),
                    repeats,
                    "{name} lost {phrase:?}: {text}"
                );
            }
            assert!(
                text.split_whitespace().count() >= 160 * repeats,
                "{name}: {text}"
            );
        }
    }

    #[test]
    fn protocol_stripping_removes_control_phrases_without_a_protocol_fallback() {
        let protocol = DictationProtocol::default();
        assert_eq!(
            strip_transcript_protocol(
                "Dictate start, use open code and alpha. Dictate stop.",
                Some(&protocol),
            ),
            "Use open code and alpha."
        );
        assert_eq!(
            strip_transcript_protocol("Dictate start, alpha. Dictate stop.", None),
            "Dictate start, alpha. Dictate stop."
        );
        let custom_protocol = DictationProtocol::try_new(
            vec!["begin note".into()],
            vec!["finish note".into()],
            vec!["send note".into()],
            vec!["discard note".into()],
        )
        .unwrap();
        assert_eq!(
            strip_transcript_protocol("Begin note, alpha. Finish note.", Some(&custom_protocol)),
            "Alpha."
        );
    }

    #[test]
    fn meeting_delta_tracks_each_source_and_coalesces_turns() {
        let entry = |source, start_ms, end_ms, text: &str| TranscriptEntry {
            speaker: None,
            source,
            start_ms,
            end_ms,
            text: text.into(),
        };
        let entries = [
            entry(MeetingSource::Microphone, 0, 1_000, "First."),
            entry(MeetingSource::System, 500, 2_500, "Reply."),
            entry(MeetingSource::Microphone, 1_100, 2_000, "Second."),
        ];

        let mut seen = HashSet::new();
        let (text, pasted) = prepare_meeting_delta(&entries, &seen).unwrap();
        assert_eq!(text, "You: First.\n\nComputer: Reply.\n\nYou: Second.");
        seen.extend(pasted);
        assert!(prepare_meeting_delta(&entries, &seen).is_err());

        let appended = [
            entries[0].clone(),
            entries[1].clone(),
            entry(MeetingSource::System, 900, 1_050, "Late completion."),
            entries[2].clone(),
            entry(MeetingSource::Microphone, 2_600, 3_000, "Third."),
            entry(MeetingSource::System, 2_800, 3_200, "Follow-up."),
        ];
        let (text, pasted) = prepare_meeting_delta(&appended, &seen).unwrap();
        assert_eq!(
            text,
            "Computer: Late completion.\n\nYou: Third.\n\nComputer: Follow-up."
        );
        seen.extend(pasted);
        assert!(prepare_meeting_delta(&appended, &seen).is_err());
    }

    #[test]
    fn parallel_processing_results_are_released_in_submission_order() {
        let mut outputs = OrderedOutputs::default();
        let job = |sequence| OutputJob::Paste {
            sequence,
            kind: PasteKind::LastTranscript,
        };

        assert!(outputs.push(job(1)).is_empty());
        assert_eq!(
            outputs
                .push(job(0))
                .into_iter()
                .map(|job| job.sequence())
                .collect::<Vec<_>>(),
            [0, 1]
        );
        assert_eq!(
            outputs
                .push(job(2))
                .into_iter()
                .map(|job| job.sequence())
                .collect::<Vec<_>>(),
            [2]
        );
    }

    #[test]
    fn ordered_waiting_outputs_remain_bounded_after_channel_drain() {
        let (output_sender, output_receiver) = mpsc::sync_channel(1);
        let (_, events) = mpsc::channel();
        let worker = DictationWorker {
            inference_jobs: None,
            output_jobs: Some(output_sender),
            events,
            state: Arc::new(Mutex::new(WorkerState {
                next_sequence: 1,
                jobs: BTreeMap::from([(DictationJobId(0), Arc::new(JobControl::default()))]),
                pending_pastes: 0,
            })),
            inference_worker: None,
            processor_workers: Vec::new(),
            output_worker: None,
        };
        let mut ordered = OrderedOutputs::default();
        for _ in 1..MAX_PENDING_OUTPUTS {
            worker.paste_last().unwrap();
            assert!(ordered.push(output_receiver.try_recv().unwrap()).is_empty());
        }
        assert_eq!(worker.paste_last(), Err("dictation queue is full"));
        assert_eq!(worker.paste_meeting(), Err("dictation queue is full"));
        assert_eq!(ordered.waiting.len(), MAX_PENDING_OUTPUTS - 1);
        assert_eq!(
            worker.state.lock().unwrap().next_sequence,
            MAX_PENDING_OUTPUTS as u64
        );

        worker.state.lock().unwrap().jobs.remove(&DictationJobId(0));
        worker.paste_last().unwrap();
        assert_eq!(
            output_receiver.try_recv().unwrap().sequence(),
            MAX_PENDING_OUTPUTS as u64
        );
    }

    #[test]
    fn cancellation_replaces_waiting_output_and_ignores_late_completion() {
        let mut outputs = OrderedOutputs::default();
        let completed = |sequence| OutputJob::Completed {
            job_id: DictationJobId(sequence),
            control: Arc::new(JobControl::default()),
            target: TranscriptionTarget::Paste,
            result: Box::new(Err("late result".into())),
        };

        assert!(outputs.push(completed(1)).is_empty());
        assert!(
            outputs
                .push(OutputJob::Cancelled {
                    job_id: DictationJobId(1),
                })
                .is_empty()
        );
        let ready = outputs.push(OutputJob::Paste {
            sequence: 0,
            kind: PasteKind::LastTranscript,
        });
        assert_eq!(ready.len(), 2);
        assert!(matches!(ready[1], OutputJob::Cancelled { .. }));
        assert!(outputs.push(completed(1)).is_empty());
    }

    #[test]
    fn a_job_can_only_be_cancelled_once() {
        let control = JobControl::default();
        assert!(control.cancel());
        assert!(!control.cancel());
        assert!(control.is_cancelled());
    }

    #[test]
    fn output_commit_and_cancellation_are_mutually_exclusive() {
        let committed = JobControl::default();
        assert!(committed.begin_output());
        assert!(!committed.cancel());
        assert!(!committed.is_cancelled());

        let cancelled = JobControl::default();
        assert!(cancelled.cancel());
        assert!(!cancelled.begin_output());
    }

    #[test]
    fn output_stays_cancellable_through_preparation_but_not_after_mutation() {
        use std::cell::{Cell, RefCell};
        use std::time::{Duration, SystemTime, UNIX_EPOCH};

        use crate::history::{HistoryRetention, HistoryStore};
        use crate::paste::commit_prepared_paste;

        let completed = |text: &str| CompletedTranscript {
            text: text.into(),
            raw: text.into(),
            application: None,
            timings: JobTimings {
                total_started: Instant::now(),
                queue_ms: 0,
                audio_ms: 1_000,
                prepare_ms: 0,
                inference_ms: 0,
            },
            processing: None,
        };
        for target in [TranscriptionTarget::Paste, TranscriptionTarget::Send] {
            for cancel_during_preparation in [true, false] {
                let path = std::env::temp_dir().join(format!(
                    "hello-output-cancellation-{}-{}.json",
                    std::process::id(),
                    SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap()
                        .as_nanos()
                ));
                let history = History::new(HistoryStore::open(
                    path.clone(),
                    HistoryRetention::Forever,
                    crate::history::now_ms(),
                ));
                record_history(&history, TranscriptionTarget::Paste, &completed("previous"));
                let saved_history = std::fs::read(&path).unwrap();
                let mut last_transcript = Some("previous".into());
                let clipboard = RefCell::new("original clipboard".to_string());
                let pasted = RefCell::new(Vec::new());
                let sent = Cell::new(false);
                let control = Arc::new(JobControl::default());
                let (blocked, blocking) = mpsc::channel();
                let (resume, resumed) = mpsc::channel();
                let cancelling = control.clone();
                let cancellation = thread::spawn(move || {
                    blocking.recv_timeout(Duration::from_secs(5)).unwrap();
                    let accepted = cancelling.cancel();
                    resume.send(()).unwrap();
                    accepted
                });
                let pause = || {
                    blocked.send(()).unwrap();
                    resumed.recv_timeout(Duration::from_secs(5)).unwrap();
                };
                let event = finish_output(
                    OutputJob::Completed {
                        job_id: DictationJobId(0),
                        control: control.clone(),
                        target,
                        result: Box::new(Ok(completed("new output"))),
                    },
                    &mut |text, mode, commit| {
                        assert!(matches!(
                            (target, mode),
                            (TranscriptionTarget::Paste, PasteMode::Continue)
                                | (TranscriptionTarget::Send, PasteMode::Send)
                        ));
                        commit_prepared_paste(
                            || {
                                // Model a blocked restore lock or lazy clipboard provider.
                                if cancel_during_preparation {
                                    pause();
                                }
                                Ok(clipboard.borrow().clone())
                            },
                            commit,
                            |_previous| {
                                *clipboard.borrow_mut() = text.into();
                                if !cancel_during_preparation {
                                    pause();
                                }
                                pasted.borrow_mut().push(text.to_string());
                                Ok(())
                            },
                        )?;
                        if matches!(mode, PasteMode::Send) {
                            sent.set(true);
                        }
                        Ok(())
                    },
                    &mut last_transcript,
                    &mut MeetingPasteCursor::default(),
                    Some(&history),
                );

                assert_eq!(cancellation.join().unwrap(), cancel_during_preparation);
                assert_eq!(control.is_cancelled(), cancel_during_preparation);
                if cancel_during_preparation {
                    assert!(matches!(
                        event,
                        WorkerEvent::Cancelled {
                            job_id: DictationJobId(0)
                        }
                    ));
                    assert_eq!(*clipboard.borrow(), "original clipboard");
                    assert!(pasted.borrow().is_empty());
                    assert!(!sent.get());
                    assert_eq!(last_transcript.as_deref(), Some("previous"));
                    assert_eq!(history.search("").len(), 1);
                    assert_eq!(std::fs::read(&path).unwrap(), saved_history);
                } else {
                    assert!(matches!(
                        event,
                        WorkerEvent::Completed { result: Ok(ref text), .. } if text == "new output"
                    ));
                    assert_eq!(*clipboard.borrow(), "new output");
                    assert_eq!(*pasted.borrow(), ["new output"]);
                    assert_eq!(sent.get(), matches!(target, TranscriptionTarget::Send));
                    assert_eq!(last_transcript.as_deref(), Some("new output"));
                    let entries = history.search("");
                    assert_eq!(entries.len(), 2);
                    assert_eq!(entries[0].final_text, "new output");
                    assert_eq!(
                        entries[0].kind,
                        match target {
                            TranscriptionTarget::Paste => HistoryKind::Dictation,
                            TranscriptionTarget::Send => HistoryKind::Send,
                            TranscriptionTarget::Service => unreachable!(),
                        }
                    );
                }
                drop(history);
                std::fs::remove_file(path).unwrap();
            }
        }
    }

    #[test]
    fn repeated_cancellation_walks_back_through_pending_jobs() {
        let jobs = [0, 1, 2]
            .into_iter()
            .map(|sequence| (DictationJobId(sequence), Arc::new(JobControl::default())))
            .collect();
        let state = WorkerState {
            next_sequence: 3,
            jobs,
            pending_pastes: 0,
        };

        assert_eq!(state.cancel_latest(), Some(DictationJobId(2)));
        assert_eq!(state.cancel_latest(), Some(DictationJobId(1)));
        assert_eq!(state.cancel_latest(), Some(DictationJobId(0)));
        assert_eq!(state.cancel_latest(), None);
    }

    #[test]
    #[ignore = "requires VOICE_CONTROL_DICTATION_FIXTURE_DIR and the installed Parakeet model"]
    fn dictation_protocol_audio() {
        let directory =
            PathBuf::from(std::env::var("VOICE_CONTROL_DICTATION_FIXTURE_DIR").unwrap());
        let protocol = DictationProtocol::try_new(
            vec!["say".into()],
            vec!["say paste".into(), "say stop".into()],
            vec!["say send".into()],
            vec!["say cancel".into(), "never mind".into()],
        )
        .unwrap();
        let mut model = Parakeet::load().unwrap();

        for (name, expected) in [
            ("period-stop", "I don't understand this."),
            ("question-stop", "Is this working?"),
            ("exclamation-send", "Ship it."),
            ("comma-stop", "Meet me at five."),
        ] {
            let mut reader = WavReader::open(directory.join(format!("{name}.wav"))).unwrap();
            assert_eq!(reader.spec().sample_rate, 16_000);
            let samples = reader
                .samples::<f32>()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            let transcript = model.transcribe_segments(&samples).unwrap();
            println!("{name}: {:?}", transcript.text);
            for word in &transcript.words {
                println!("  {}..{} {:?}", word.t0_ms, word.t1_ms, word.text);
            }
            assert_ne!(transcript.timestamp_kind, TimestampKind::None);
            assert!(
                protocol.control_suffix(&transcript.text).is_some(),
                "{name} did not contain a configured control: {:?}",
                transcript.text
            );

            let clipped = model.transcribe_voice(&samples, &protocol).unwrap();
            let prepared = strip_transcript_protocol(&clipped, Some(&protocol));
            println!("  clipped: {:?} -> {:?}", clipped, prepared);
            assert_eq!(prepared, expected);
        }
    }

    #[test]
    #[ignore = "requires VOICE_CONTROL_STUTTER_FIXTURE and the installed Parakeet model"]
    fn tdt_decoder_does_not_repeat_single_letter_tokens() {
        let path = PathBuf::from(std::env::var("VOICE_CONTROL_STUTTER_FIXTURE").unwrap());
        let mut reader = WavReader::open(path).unwrap();
        let sample_rate = reader.spec().sample_rate;
        let samples = reader
            .samples::<f32>()
            .skip(sample_rate as usize * 20)
            .take(sample_rate as usize * 25)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        let samples = resample_for_parakeet(&samples, sample_rate);
        let text = Parakeet::load().unwrap().transcribe(&samples).unwrap();

        assert!(text.contains("sweatshirt"), "unexpected transcript: {text}");
        assert!(
            !text.contains("p p p p") && !text.contains("p-p-p-p"),
            "pathological token repetition: {text}"
        );
    }
}
