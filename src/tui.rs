use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, List, ListItem, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};

use crate::analysis::{
    analyze, known_agents, maybe_rename_session_dir_for_title, AnalyzeOptions, AnalyzeTarget,
};
use crate::audio::{
    acquire_capture_lock, copy_take1_aliases, next_take_index, prepare_continued_take,
    read_capture_progress, resume_block_reason, write_capture_progress, AudioTrack, CaptureLock,
    CaptureProgress,
};
use crate::capture_sources::{detect_sources, SourceSummary};
use crate::mic_recorder::MicRecorder;
use crate::session::{
    append_session_marker, append_session_note, default_storage_dir, internal_dir, open_path,
    primary_document_path, read_session_consent, read_session_title, resolve_session_target,
    start_session, ConsentMode, StartOptions,
};
use crate::system_recorder::SystemRecorder;
use crate::transcription::{
    engine_status_parts, format_bytes, format_model_download_label, transcribe_with_progress,
    TrackSelection, TranscribeOptions, TranscribeTarget, TranscriptionEngine,
    TranscriptionProgress, TRANSCRIPTION_CHUNK_SECONDS,
};

const TICK_RATE: Duration = Duration::from_millis(100);
const SOURCE_REFRESH_TICKS: u64 = 50;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptureState {
    Ready,
    Recording,
    Ended,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeTarget {
    Latest,
    Named(String),
}

impl ResumeTarget {
    pub fn from_arg(value: Option<&str>) -> Self {
        match value.map(str::trim).filter(|value| !value.is_empty()) {
            None | Some("latest") => Self::Latest,
            Some(other) => Self::Named(other.to_string()),
        }
    }

    fn lookup(&self) -> &str {
        match self {
            Self::Latest => "latest",
            Self::Named(name) => name,
        }
    }
}

#[derive(Debug, Clone)]
pub struct TuiOptions {
    pub consent_noted: bool,
    pub title: String,
    pub storage_dir: Option<PathBuf>,
    pub engine: TranscriptionEngine,
    pub ffmpeg_bin: Option<PathBuf>,
    pub whisper_bin: Option<PathBuf>,
    pub model_path: Option<PathBuf>,
    pub parakeet_bin: Option<PathBuf>,
    pub parakeet_model: Option<String>,
    pub parakeet_cache_dir: Option<PathBuf>,
    pub require_parakeet: bool,
    pub chunk_seconds: u64,
    pub agent: Option<String>,
    pub auto_analyze: bool,
    pub preset: String,
    pub editor: Option<String>,
    pub resume: Option<ResumeTarget>,
}

impl Default for TuiOptions {
    fn default() -> Self {
        Self {
            consent_noted: false,
            title: "Quick Capture".to_string(),
            storage_dir: None,
            engine: TranscriptionEngine::default(),
            ffmpeg_bin: None,
            whisper_bin: None,
            model_path: None,
            parakeet_bin: None,
            parakeet_model: None,
            parakeet_cache_dir: None,
            require_parakeet: false,
            chunk_seconds: TRANSCRIPTION_CHUNK_SECONDS,
            agent: None,
            auto_analyze: true,
            preset: "general".to_string(),
            editor: None,
            resume: None,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedResume {
    path: PathBuf,
    title: String,
    consent_noted: bool,
    progress: CaptureProgress,
}

pub(crate) fn prepare_resume(
    storage_dir: &Path,
    target: &ResumeTarget,
    cli_consent_noted: bool,
) -> io::Result<PreparedResume> {
    let path = resolve_session_target(storage_dir, target.lookup())?;
    if let Some(reason) = resume_block_reason(&path) {
        return Err(io::Error::other(reason));
    }

    let mut progress = read_capture_progress(&path);
    if progress.take_count == 0 {
        let next = next_take_index(&path);
        if next > 1 {
            progress.take_count = next - 1;
            progress.completed_take = progress.take_count;
        }
    }

    let stored_consent = read_session_consent(&path);
    Ok(PreparedResume {
        path: path.clone(),
        title: read_session_title(&path).unwrap_or_else(|_| "Recall Session".to_string()),
        consent_noted: cli_consent_noted
            || stored_consent.is_some_and(|consent| !matches!(consent, ConsentMode::NotYet)),
        progress,
    })
}

#[derive(Debug, Default)]
pub struct TuiExit {
    pub session_path: Option<PathBuf>,
    pub detached_logs: Vec<(PathBuf, PathBuf)>,
}

pub fn run_with_options(options: TuiOptions) -> io::Result<TuiExit> {
    let resume = match &options.resume {
        Some(target) => {
            let storage_dir = options
                .storage_dir
                .clone()
                .unwrap_or(default_storage_dir()?);
            Some(prepare_resume(&storage_dir, target, options.consent_noted)?)
        }
        None => None,
    };
    let mut terminal = ratatui::try_init()?;
    let result = App::new(options, resume)?.run(&mut terminal);
    let restore_result = ratatui::try_restore();

    match (result, restore_result) {
        (Err(error), _) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Ok(exit), Ok(())) => Ok(exit),
    }
}

struct App {
    state: CaptureState,
    consent_noted: bool,
    tick: u64,
    started_at: Option<Instant>,
    ended_at: Option<Instant>,
    accumulated: Duration,
    take_index: u32,
    completed_take: u32,
    session_path: Option<PathBuf>,
    storage_dir: PathBuf,
    toast: String,
    markers: Vec<String>,
    live_notes: Vec<String>,
    sources: SourceSummary,
    source_receiver: Option<Receiver<SourceSummary>>,
    last_source_refresh_tick: u64,
    mic_recorder: Option<MicRecorder>,
    system_recorder: Option<SystemRecorder>,
    default_title: String,
    title: String,
    mic_level_percent: u16,
    mic_level_db: Option<f32>,
    mic_device_label: Option<String>,
    mic_device_id: Option<String>,
    mic_capture_warning: Option<String>,
    call_level_percent: u16,
    call_level_db: Option<f32>,
    system_capture_failed: bool,
    system_capture_warning: Option<String>,
    transcription_jobs: Vec<TranscriptionJob>,
    transcription_status: TranscriptionStatus,
    analysis_jobs: Vec<AnalysisJob>,
    analysis_status: AnalysisStatus,
    agent: Option<String>,
    auto_analyze: bool,
    preset: String,
    engine: TranscriptionEngine,
    ffmpeg_bin: Option<PathBuf>,
    whisper_bin: Option<PathBuf>,
    model_path: Option<PathBuf>,
    parakeet_bin: Option<PathBuf>,
    parakeet_model: Option<String>,
    parakeet_cache_dir: Option<PathBuf>,
    require_parakeet: bool,
    chunk_seconds: u64,
    note_draft: Option<String>,
    editor: Option<String>,
    resumed: bool,
    append_next: bool,
    capture_lock: Option<CaptureLock>,
    detached_logs: Vec<(PathBuf, PathBuf)>,
}

#[derive(Debug, Clone)]
struct TranscriptionStatus {
    label: String,
    percent: u16,
    transcript_path: Option<PathBuf>,
    failed: bool,
}

struct TranscriptionJob {
    session_path: PathBuf,
    generation: u32,
    receiver: Receiver<TranscriptionUiEvent>,
}

#[derive(Debug, Clone)]
enum TranscriptionUiEvent {
    Progress {
        session_path: PathBuf,
        generation: u32,
        progress: TranscriptionProgress,
    },
    Complete {
        session_path: PathBuf,
        generation: u32,
        transcript_path: PathBuf,
        published: bool,
    },
    Failed {
        session_path: PathBuf,
        generation: u32,
        message: String,
    },
}

#[derive(Debug, Clone)]
struct AnalysisStatus {
    label: String,
    percent: u16,
    result_path: Option<PathBuf>,
    failed: bool,
}

struct AnalysisJob {
    session_path: PathBuf,
    generation: u32,
    receiver: Receiver<AnalysisUiEvent>,
}

#[derive(Debug, Clone)]
enum AnalysisUiEvent {
    Complete {
        original_session_path: PathBuf,
        session_path: PathBuf,
        generation: u32,
        meeting_path: PathBuf,
        generated_title: Option<String>,
        published: bool,
    },
    Failed {
        session_path: PathBuf,
        generation: u32,
        message: String,
    },
}

impl App {
    fn new(options: TuiOptions, resume: Option<PreparedResume>) -> io::Result<Self> {
        let consent_noted = options.consent_noted;
        let mut app = Self {
            state: CaptureState::Ready,
            consent_noted,
            tick: 0,
            started_at: None,
            ended_at: None,
            accumulated: Duration::ZERO,
            take_index: 0,
            completed_take: 0,
            session_path: None,
            storage_dir: options.storage_dir.unwrap_or(default_storage_dir()?),
            toast: if consent_noted {
                "Ready with consent provided. Press Space or Enter to start.".to_string()
            } else {
                "Ready. Press c after consent, then Space or Enter to start.".to_string()
            },
            markers: Vec::new(),
            live_notes: vec![
                "Waiting for a session.".to_string(),
                "Meters become real as capture sources start.".to_string(),
            ],
            sources: detect_sources(),
            source_receiver: None,
            last_source_refresh_tick: 0,
            mic_recorder: None,
            system_recorder: None,
            default_title: options.title.clone(),
            title: options.title,
            mic_level_percent: 0,
            mic_level_db: None,
            mic_device_label: None,
            mic_device_id: None,
            mic_capture_warning: None,
            call_level_percent: 0,
            call_level_db: None,
            system_capture_failed: false,
            system_capture_warning: None,
            transcription_jobs: Vec::new(),
            transcription_status: TranscriptionStatus::idle(),
            analysis_jobs: Vec::new(),
            analysis_status: AnalysisStatus::idle(),
            agent: options.agent,
            auto_analyze: options.auto_analyze,
            preset: options.preset,
            engine: options.engine,
            ffmpeg_bin: options.ffmpeg_bin,
            whisper_bin: options.whisper_bin,
            model_path: options.model_path,
            parakeet_bin: options.parakeet_bin,
            parakeet_model: options.parakeet_model,
            parakeet_cache_dir: options.parakeet_cache_dir,
            require_parakeet: options.require_parakeet,
            chunk_seconds: options.chunk_seconds,
            note_draft: None,
            editor: options.editor,
            resumed: false,
            append_next: true,
            capture_lock: None,
            detached_logs: Vec::new(),
        };
        if let Some(resume) = resume {
            app.apply_resume(resume);
        }
        Ok(app)
    }

    fn apply_resume(&mut self, resume: PreparedResume) {
        let next_take = next_take_index(&resume.path);
        self.state = CaptureState::Ended;
        self.session_path = Some(resume.path.clone());
        self.title = resume.title;
        self.consent_noted = resume.consent_noted;
        self.take_index = resume.progress.take_count.max(1);
        self.completed_take = resume.progress.completed_take;
        self.accumulated = Duration::from_millis(resume.progress.elapsed_ms);
        self.started_at = None;
        self.ended_at = None;
        self.resumed = true;
        let id = Self::session_label(&resume.path);
        let clock = self.elapsed_label();
        self.toast = format!(
            "Resumed {id}. Space appends take {next_take}. Clock continues from {clock} (break not added)."
        );
        self.live_notes = vec![
            format!("Resumed {}", resume.path.display()),
            format!(
                "Take {} of {} completed. Consent: {}.",
                resume.progress.completed_take,
                resume
                    .progress
                    .take_count
                    .max(resume.progress.completed_take),
                if self.consent_noted {
                    "noted"
                } else {
                    "not noted"
                }
            ),
            "Space or Enter records another take in this session.".to_string(),
            "q leaves; next plain recall starts a new session.".to_string(),
        ];
    }

    fn run(&mut self, terminal: &mut DefaultTerminal) -> io::Result<TuiExit> {
        loop {
            self.process_mic_events();
            self.process_system_events();
            self.process_source_events();
            self.process_transcription_events();
            self.process_analysis_events();
            terminal.draw(|frame| self.render(frame))?;

            if event::poll(TICK_RATE)? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press && self.handle_key(key)? {
                        return Ok(TuiExit {
                            session_path: self.session_path.clone(),
                            detached_logs: self.detached_logs.clone(),
                        });
                    }
                }
            }

            self.tick();
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> io::Result<bool> {
        if self.note_draft.is_some() {
            self.handle_note_key(key);
            return Ok(false);
        }

        if matches!(key.code, KeyCode::Char('c')) && key.modifiers.contains(KeyModifiers::CONTROL) {
            return self.request_quit();
        }

        match key.code {
            KeyCode::Char('q') => {
                return self.request_quit();
            }
            KeyCode::Enter | KeyCode::Char(' ') => self.primary_recording_action()?,
            KeyCode::Char('c') => self.toggle_consent(),
            KeyCode::Char('s') => self.toggle_append_next(),
            KeyCode::Char('p') => {
                self.toast = "Pause is disabled for real recording. Press Space or Enter to end."
                    .to_string();
            }
            KeyCode::Char('e') => self.end_capture(),
            KeyCode::Char('m') => self.add_marker(),
            KeyCode::Char('n') => self.start_manual_note(),
            KeyCode::Char('r') => self.refresh_sources(),
            KeyCode::Char('a') => self.toggle_auto_analyze(),
            KeyCode::Char('A') => self.cycle_agent(),
            KeyCode::Char('o') => self.open_session_folder(),
            KeyCode::Char('O') => self.open_session_document(),
            _ => {}
        }

        Ok(false)
    }

    fn request_quit(&mut self) -> io::Result<bool> {
        if matches!(self.state, CaptureState::Recording) {
            self.end_capture();
        }
        if self.has_background_jobs() {
            match self.detach_background_jobs() {
                Ok(logs) => self.detached_logs = logs,
                Err(error) => {
                    self.toast = format!(
                        "Could not keep processing after quit: {error}. Wait for Recall to finish, or try q again."
                    );
                    return Ok(false);
                }
            }
        }
        self.stop_recorders();
        self.capture_lock = None;
        Ok(true)
    }

    fn detach_background_jobs(&mut self) -> io::Result<Vec<(PathBuf, PathBuf)>> {
        let plans = self.detach_plans();
        let exe = std::env::current_exe()?;
        let mut logs = Vec::new();
        for plan in plans {
            let log_path = spawn_detached_postprocess(&exe, self, &plan)?;
            logs.push((plan.session_path, log_path));
        }
        self.transcription_jobs.clear();
        self.analysis_jobs.clear();
        Ok(logs)
    }

    fn detach_plans(&self) -> Vec<DetachPlan> {
        detach_plans_from_jobs(
            &self.transcription_jobs,
            &self.analysis_jobs,
            self.auto_analyze,
            self.agent.is_some(),
        )
    }

    fn primary_recording_action(&mut self) -> io::Result<()> {
        match next_recording_action(self.state, self.session_path.is_some(), self.append_next) {
            RecordingAction::StartNew => self.start_capture(),
            RecordingAction::Continue => self.continue_capture(),
            RecordingAction::End => {
                self.end_capture();
                Ok(())
            }
        }
    }

    fn handle_note_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter => self.save_manual_note(),
            KeyCode::Esc => {
                self.note_draft = None;
                self.toast = "Note cancelled.".to_string();
            }
            KeyCode::Backspace => {
                if let Some(draft) = &mut self.note_draft {
                    draft.pop();
                }
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.note_draft = None;
                self.toast = "Note cancelled.".to_string();
            }
            KeyCode::Char(ch)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                if let Some(draft) = &mut self.note_draft {
                    draft.push(ch);
                }
            }
            _ => {}
        }
    }

    fn tick(&mut self) {
        self.tick = self.tick.wrapping_add(1);
        self.start_source_refresh_if_due();
    }

    fn has_background_jobs(&self) -> bool {
        !self.transcription_jobs.is_empty() || !self.analysis_jobs.is_empty()
    }

    fn is_current_session(&self, session_path: &PathBuf) -> bool {
        self.session_path.as_ref() == Some(session_path)
    }

    fn should_apply_generation(&self, session_path: &PathBuf, generation: u32) -> bool {
        self.is_current_session(session_path) && generation == self.completed_take
    }

    fn session_label(session_path: &std::path::Path) -> String {
        session_path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("previous session")
            .to_string()
    }

    fn start_capture(&mut self) -> io::Result<()> {
        if matches!(self.state, CaptureState::Recording) {
            self.toast = "Session is already active.".to_string();
            return Ok(());
        }

        let capture_title = self.default_title.clone();
        self.title = capture_title.clone();
        self.transcription_status = TranscriptionStatus::idle();
        self.analysis_status = AnalysisStatus::idle();

        let options = StartOptions {
            title: capture_title,
            consent: if self.consent_noted {
                ConsentMode::Noted
            } else {
                ConsentMode::NotYet
            },
            storage_dir: self.storage_dir.clone(),
        };
        let session = start_session(&options)?;

        self.session_path = Some(session.path.clone());
        self.started_at = Some(Instant::now());
        self.ended_at = None;
        self.accumulated = Duration::ZERO;
        self.take_index = 1;
        self.completed_take = 0;
        self.resumed = false;
        self.reset_capture_health();
        match acquire_capture_lock(&session.path) {
            Ok(lock) => self.capture_lock = Some(lock),
            Err(error) => {
                self.session_path = Some(session.path);
                self.state = CaptureState::Ended;
                self.toast = error.to_string();
                return Ok(());
            }
        }

        let (started, failures) = self.start_recorders(
            &session.path,
            &AudioTrack::Mic.segment_name(1),
            &AudioTrack::Call.segment_name(1),
        );

        if started.is_empty() {
            self.capture_lock = None;
            self.state = CaptureState::Ended;
            self.started_at = None;
            self.ended_at = Some(Instant::now());
            self.toast = format!(
                "Session created, but capture failed: {}",
                failures.join("; ")
            );
        } else {
            let _ = write_capture_progress(
                &session.path,
                CaptureProgress {
                    take_count: 1,
                    completed_take: 0,
                    elapsed_ms: 0,
                },
            );
            self.state = CaptureState::Recording;
            self.toast = format!(
                "Recording {}: {}",
                started.join(" + "),
                session.path.display()
            );
        }

        self.live_notes = vec![
            "Microphone recording writes audio/mic-001.m4a.".to_string(),
            "Mic source changes are detected while recording.".to_string(),
            "System audio capture writes audio/call-001.m4a via CoreAudio process taps."
                .to_string(),
            "Press Space or Enter to end and start transcription.".to_string(),
        ];
        for failure in failures {
            self.live_notes.push(failure);
        }

        Ok(())
    }

    fn continue_capture(&mut self) -> io::Result<()> {
        if matches!(self.state, CaptureState::Recording) {
            self.toast = "Session is already active.".to_string();
            return Ok(());
        }

        let Some(session_path) = self.session_path.clone() else {
            return self.start_capture();
        };

        let included_break = if let Some(ended_at) = self.ended_at.take() {
            self.accumulated += ended_at.elapsed();
            true
        } else {
            false
        };

        let continued = prepare_continued_take(&session_path)?;
        self.take_index = continued.take_index;
        self.reset_capture_health();
        match acquire_capture_lock(&session_path) {
            Ok(lock) => self.capture_lock = Some(lock),
            Err(error) => {
                self.state = CaptureState::Ended;
                self.ended_at = Some(Instant::now());
                self.toast = error.to_string();
                return Ok(());
            }
        }

        let (started, failures) =
            self.start_recorders(&session_path, &continued.mic_name, &continued.call_name);

        if started.is_empty() {
            self.capture_lock = None;
            self.state = CaptureState::Ended;
            self.ended_at = Some(Instant::now());
            self.toast = format!("Could not continue this session: {}", failures.join("; "));
        } else {
            let _ = write_capture_progress(
                &session_path,
                CaptureProgress {
                    take_count: continued.take_index,
                    completed_take: self.completed_take,
                    elapsed_ms: self.elapsed().as_millis() as u64,
                },
            );
            self.started_at = Some(Instant::now());
            self.state = CaptureState::Recording;
            self.reset_analysis_gauge_for_new_take();
            self.toast = if self.has_background_jobs() {
                "Continuing this session. Previous transcript still running in the background."
                    .to_string()
            } else {
                "Continuing this session. New audio will append.".to_string()
            };
            self.live_notes.push(if included_break {
                "Continuing this session (audio appends; clock includes the break).".to_string()
            } else {
                "Continuing this session (audio appends; break not added to the clock).".to_string()
            });
            self.live_notes.push(format!(
                "Take {} writes audio/{} and audio/{}.",
                continued.take_index, continued.mic_name, continued.call_name
            ));
        }
        for failure in failures {
            self.live_notes.push(failure);
        }

        Ok(())
    }

    fn reset_analysis_gauge_for_new_take(&mut self) {
        if self.analysis_jobs.is_empty() {
            self.analysis_status = AnalysisStatus::idle();
        }
    }

    fn reset_capture_health(&mut self) {
        self.system_capture_failed = false;
        self.system_capture_warning = None;
        self.mic_capture_warning = None;
        self.mic_device_label = None;
        self.mic_device_id = None;
    }

    fn start_recorders(
        &mut self,
        session_path: &Path,
        mic_name: &str,
        call_name: &str,
    ) -> (Vec<&'static str>, Vec<String>) {
        let mut started = Vec::new();
        let mut failures = Vec::new();

        match MicRecorder::start(session_path, mic_name) {
            Ok(recorder) => {
                self.mic_recorder = Some(recorder);
                started.push("mic");
            }
            Err(error) => failures.push(format!("mic failed: {error}")),
        }

        match SystemRecorder::start(session_path, call_name) {
            Ok(recorder) => {
                self.system_recorder = Some(recorder);
                started.push("system audio");
            }
            Err(error) => {
                self.system_capture_failed = true;
                failures.push(format!("system audio failed: {error}"));
            }
        }

        (started, failures)
    }

    fn toggle_consent(&mut self) {
        self.consent_noted = !self.consent_noted;
        self.toast = if self.consent_noted {
            "Consent noted for this local session.".to_string()
        } else {
            "Consent set back to not noted.".to_string()
        };
    }

    fn toggle_append_next(&mut self) {
        self.append_next = !self.append_next;
        self.toast = if self.append_next {
            "Next Space or Enter will append another take to this session.".to_string()
        } else {
            "Next Space or Enter will start a new session folder.".to_string()
        };
        if matches!(self.state, CaptureState::Ended) {
            self.replace_next_action_notes();
        }
    }

    fn replace_next_action_notes(&mut self) {
        self.live_notes.retain(|note| {
            !note.contains("Space or Enter continues this session")
                && !note.contains("Space or Enter starts a new session")
                && !note.contains("q leaves (next start is a new session)")
                && !note.contains("q leaves.")
        });
        self.live_notes.push(if self.append_next {
            "Audio finalized. Space or Enter continues this session.".to_string()
        } else {
            "Audio finalized. Space or Enter starts a new session folder.".to_string()
        });
        self.live_notes
            .push("s toggles append vs new. q leaves.".to_string());
    }

    fn end_capture(&mut self) {
        match self.state {
            CaptureState::Recording => {
                if let Some(started_at) = self.started_at.take() {
                    self.accumulated += started_at.elapsed();
                }
                self.stop_recorders();
                self.ended_at = Some(Instant::now());
                self.state = CaptureState::Ended;
                self.completed_take = self.take_index.max(1);
                self.live_notes
                    .retain(|note| !note.contains("Press Space or Enter to end"));
                self.replace_next_action_notes();
                if let Some(session_path) = &self.session_path {
                    if let Err(error) = write_capture_progress(
                        session_path,
                        CaptureProgress {
                            take_count: self.take_index.max(1),
                            completed_take: self.completed_take,
                            elapsed_ms: self.elapsed().as_millis() as u64,
                        },
                    ) {
                        self.toast = format!(
                            "Take {} ended, but Recall could not save take state: {error}",
                            self.completed_take
                        );
                        self.live_notes.push(self.toast.clone());
                        return;
                    }
                    if self.completed_take == 1 {
                        let _ = copy_take1_aliases(session_path);
                    }
                }
                self.capture_lock = None;
                self.start_transcription_job();
            }
            _ => {
                self.toast = "No active session to end.".to_string();
            }
        }
    }

    fn add_marker(&mut self) {
        if self.session_path.is_none() {
            self.toast = "Start or resume a session before adding markers.".to_string();
            return;
        }

        let marker = format!("{} marker dropped", self.elapsed_label());
        if let Some(session_path) = &self.session_path {
            if let Err(error) = append_session_marker(session_path, &self.elapsed_label()) {
                self.toast = format!("Marker created in memory, but failed to save: {error}");
                return;
            }
        }
        self.markers.push(marker.clone());
        self.live_notes.push(marker.clone());
        self.toast = format!("{marker} saved with the session");
    }

    fn start_manual_note(&mut self) {
        if self.session_path.is_none() {
            self.toast = "Start or resume a session before adding notes.".to_string();
            return;
        }

        self.note_draft = Some(String::new());
        self.toast = "Type a note, then press Enter to save or Esc to cancel.".to_string();
    }

    fn save_manual_note(&mut self) {
        let note_text = self.note_draft.take().unwrap_or_default();
        let note_text = note_text.trim();
        if note_text.is_empty() {
            self.toast = "Empty note discarded.".to_string();
            return;
        }

        let note = format!("{} {note_text}", self.elapsed_label());
        if let Some(session_path) = &self.session_path {
            if let Err(error) = append_session_note(session_path, &self.elapsed_label(), note_text)
            {
                self.toast = format!("Note created in memory, but failed to save: {error}");
                return;
            }
        }
        self.live_notes.push(note.clone());
        self.toast = format!("{note} saved with the session");
    }

    fn refresh_sources(&mut self) {
        self.start_source_refresh();
        self.toast = "Refreshing capture sources.".to_string();
    }

    fn start_source_refresh_if_due(&mut self) {
        if !matches!(self.state, CaptureState::Ready | CaptureState::Recording) {
            return;
        }

        if self.source_receiver.is_some() {
            return;
        }

        if self.tick.saturating_sub(self.last_source_refresh_tick) < SOURCE_REFRESH_TICKS {
            return;
        }

        self.start_source_refresh();
    }

    fn start_source_refresh(&mut self) {
        if self.source_receiver.is_some() {
            return;
        }

        self.last_source_refresh_tick = self.tick;
        let (sender, receiver) = mpsc::channel();
        self.source_receiver = Some(receiver);
        thread::spawn(move || {
            let _ = sender.send(detect_sources());
        });
    }

    fn process_source_events(&mut self) {
        let mut latest = None;
        if let Some(receiver) = &self.source_receiver {
            while let Ok(summary) = receiver.try_recv() {
                latest = Some(summary);
            }
        }

        if let Some(summary) = latest {
            self.sources = summary;
            self.source_receiver = None;
        }
    }

    fn toggle_auto_analyze(&mut self) {
        self.auto_analyze = !self.auto_analyze;
        self.toast = if self.auto_analyze {
            format!("Auto-analyze enabled with {}.", self.agent_label())
        } else {
            "Auto-analyze disabled.".to_string()
        };
    }

    fn cycle_agent(&mut self) {
        let agents = known_agents();
        let next = match self.agent.as_deref() {
            None => agents.first().map(|agent| (*agent).to_string()),
            Some(current) => {
                let index = agents
                    .iter()
                    .position(|agent| *agent == current)
                    .map(|index| index + 1)
                    .unwrap_or(0);
                agents.get(index).map(|agent| (*agent).to_string())
            }
        };

        self.agent = next;
        self.toast = format!("Agent set to {}.", self.agent_label());
    }

    fn open_session_document(&mut self) {
        let Some(session_path) = self.session_path.clone() else {
            self.toast = "No session yet. Start a recording first.".to_string();
            return;
        };
        let document_path = primary_document_path(&session_path);
        if !document_path.exists() {
            self.toast = format!("No meeting document in {}", session_path.display());
            return;
        }
        match open_path(&document_path, None) {
            Ok(()) => self.toast = format!("Opened {}", document_path.display()),
            Err(error) => self.toast = format!("Could not open meeting: {error}"),
        }
    }

    fn open_session_folder(&mut self) {
        let Some(session_path) = self.session_path.clone() else {
            self.toast = "No session yet. Start a recording first.".to_string();
            return;
        };
        match open_path(&session_path, self.editor.as_deref()) {
            Ok(()) => self.toast = format!("Opened {}", session_path.display()),
            Err(error) => self.toast = format!("Could not open session folder: {error}"),
        }
    }

    fn process_mic_events(&mut self) {
        let Some(recorder) = self.mic_recorder.as_mut() else {
            return;
        };
        let events = recorder.drain_events();
        let mut clear_recorder = false;

        for event in events {
            match event.event_type.as_str() {
                "recording_started" => {
                    let path = event.path.unwrap_or_else(|| "audio/mic.m4a".to_string());
                    self.mic_device_label = event.device_name.clone();
                    self.mic_device_id = event.device_id.clone();
                    self.toast = match &self.mic_device_label {
                        Some(device) => format!("Mic recording started on {device}: {path}"),
                        None => format!("Mic recording started: {path}"),
                    };
                    self.live_notes.push(self.toast.clone());
                }
                "device_changed" => {
                    self.mic_device_label = event.device_name.clone();
                    self.mic_device_id = event.device_id.clone();
                    let elapsed = event
                        .elapsed_seconds
                        .map(|value| format!("{value:.1}s"))
                        .unwrap_or_else(|| self.elapsed_label());
                    let device = self
                        .mic_device_label
                        .clone()
                        .unwrap_or_else(|| "unknown input".to_string());
                    let warning =
                        format!("Mic input changed at {elapsed}: {device}. Watch the mic meter.");
                    self.mic_capture_warning = Some(warning.clone());
                    self.toast = warning.clone();
                    self.live_notes.push(warning);
                }
                "level" => {
                    if event.device_name.is_some() {
                        self.mic_device_label = event.device_name.clone();
                    }
                    if event.device_id.is_some() {
                        self.mic_device_id = event.device_id.clone();
                    }
                    if let Some(level_db) = event.level_db {
                        self.mic_level_db = Some(level_db);
                        self.mic_level_percent = db_to_percent(level_db);
                    }
                }
                "recording_stopped" => {
                    let path = event.path.unwrap_or_else(|| "audio/mic.m4a".to_string());
                    let elapsed = event
                        .elapsed_seconds
                        .map(|value| format!(" after {value:.1}s"))
                        .unwrap_or_default();
                    self.toast = format!("Mic recording saved{elapsed}: {path}");
                    clear_recorder = true;
                    self.mic_level_percent = 0;
                    self.mic_level_db = None;
                    break;
                }
                "error" => {
                    self.toast = event
                        .message
                        .unwrap_or_else(|| "Mic recorder reported an error.".to_string());
                    self.mic_capture_warning = Some(self.toast.clone());
                    self.live_notes.push(format!("mic failed: {}", self.toast));
                    clear_recorder = true;
                    self.mic_level_percent = 0;
                    self.mic_level_db = None;
                    break;
                }
                _ => {}
            }
        }

        if clear_recorder {
            self.mic_recorder = None;
            return;
        }

        if let Some(recorder) = self.mic_recorder.as_mut() {
            match recorder.try_wait() {
                Ok(Some(status)) => {
                    let warning = format!(
                        "Mic recorder stopped unexpectedly at {} ({status}).",
                        self.elapsed_label()
                    );
                    self.mic_capture_warning = Some(warning.clone());
                    self.toast = warning.clone();
                    self.live_notes.push(warning);
                    self.mic_recorder = None;
                    self.mic_level_percent = 0;
                    self.mic_level_db = None;
                }
                Ok(None) => {}
                Err(error) => {
                    let warning = format!("Mic recorder health check failed: {error}");
                    self.mic_capture_warning = Some(warning.clone());
                    self.toast = warning.clone();
                    self.live_notes.push(warning);
                }
            }
        }
    }

    fn process_system_events(&mut self) {
        let Some(recorder) = self.system_recorder.as_mut() else {
            return;
        };
        let events = recorder.drain_events();
        let mut clear_recorder = false;

        for event in events {
            match event.event_type.as_str() {
                "recording_started" => {
                    let path = event.path.unwrap_or_else(|| "audio/call.m4a".to_string());
                    self.toast = format!("System audio recording started: {path}");
                }
                "level" => {
                    if let Some(level_db) = event.level_db {
                        self.call_level_db = Some(level_db);
                        self.call_level_percent = db_to_percent(level_db);
                    }
                }
                "recording_stopped" => {
                    let path = event.path.unwrap_or_else(|| "audio/call.m4a".to_string());
                    let elapsed = event
                        .elapsed_seconds
                        .map(|value| format!(" after {value:.1}s"))
                        .unwrap_or_default();
                    self.toast = format!("System audio saved{elapsed}: {path}");
                    clear_recorder = true;
                    self.call_level_percent = 0;
                    self.call_level_db = None;
                    break;
                }
                "error" => {
                    self.toast = event
                        .message
                        .unwrap_or_else(|| "System audio recorder reported an error.".to_string());
                    self.live_notes
                        .push(format!("system audio failed: {}", self.toast));
                    self.system_capture_failed = true;
                    self.system_capture_warning = Some(self.toast.clone());
                    clear_recorder = true;
                    self.call_level_percent = 0;
                    self.call_level_db = None;
                    break;
                }
                _ => {}
            }
        }

        if clear_recorder {
            self.system_recorder = None;
            return;
        }

        if let Some(recorder) = self.system_recorder.as_mut() {
            match recorder.try_wait() {
                Ok(Some(status)) => {
                    let warning = format!(
                        "System audio recorder stopped unexpectedly at {} ({status}).",
                        self.elapsed_label()
                    );
                    self.system_capture_failed = true;
                    self.system_capture_warning = Some(warning.clone());
                    self.toast = warning.clone();
                    self.live_notes.push(warning);
                    self.system_recorder = None;
                    self.call_level_percent = 0;
                    self.call_level_db = None;
                }
                Ok(None) => {}
                Err(error) => {
                    let warning = format!("System audio health check failed: {error}");
                    self.system_capture_warning = Some(warning.clone());
                    self.toast = warning.clone();
                    self.live_notes.push(warning);
                }
            }
        }
    }

    fn start_transcription_job(&mut self) {
        let Some(session_path) = self.session_path.clone() else {
            self.toast = "Session ended, but no session path was available.".to_string();
            return;
        };
        let generation = self.completed_take.max(1);

        let (sender, receiver) = mpsc::channel();
        self.transcription_jobs.push(TranscriptionJob {
            session_path: session_path.clone(),
            generation,
            receiver,
        });
        if self.is_current_session(&session_path) {
            self.transcription_status = TranscriptionStatus::queued();
        }
        self.toast = if generation > 1 {
            format!("Take {generation} ended. Re-transcribing all takes.")
        } else {
            "Session ended. Audio finalized; transcription queued.".to_string()
        };
        self.live_notes.push(if generation > 1 {
            format!("Take {generation} queued a full re-transcribe of all audio.")
        } else {
            "Transcription queued for this session.".to_string()
        });

        let engine = self.engine;
        let ffmpeg_bin = self.ffmpeg_bin.clone();
        let whisper_bin = self.whisper_bin.clone();
        let model_path = self.model_path.clone();
        let parakeet_bin = self.parakeet_bin.clone();
        let parakeet_model = self.parakeet_model.clone();
        let parakeet_cache_dir = self.parakeet_cache_dir.clone();
        let require_parakeet = self.require_parakeet;
        let chunk_seconds = self.chunk_seconds;

        thread::spawn(move || {
            let event_session_path = session_path.clone();
            let options = TranscribeOptions {
                target: TranscribeTarget::Session(session_path),
                track: TrackSelection::Both,
                storage_dir: None,
                engine,
                ffmpeg_bin,
                model_path,
                whisper_bin,
                parakeet_bin,
                parakeet_model,
                parakeet_cache_dir,
                chunk_seconds,
                keep_wav: false,
                require_parakeet,
                generation: Some(generation),
            };

            let progress_sender = sender.clone();
            let progress_session_path = event_session_path.clone();
            let result = transcribe_with_progress(&options, |progress| {
                let _ = progress_sender.send(TranscriptionUiEvent::Progress {
                    session_path: progress_session_path.clone(),
                    generation,
                    progress,
                });
            });

            match result {
                Ok(result) => {
                    let _ = sender.send(TranscriptionUiEvent::Complete {
                        session_path: event_session_path,
                        generation,
                        transcript_path: result.transcript_path,
                        published: result.published,
                    });
                }
                Err(error) => {
                    let _ = sender.send(TranscriptionUiEvent::Failed {
                        session_path: event_session_path,
                        generation,
                        message: error.to_string(),
                    });
                }
            }
        });
    }

    fn process_transcription_events(&mut self) {
        let mut events = Vec::new();

        for (index, job) in self.transcription_jobs.iter().enumerate() {
            while let Ok(event) = job.receiver.try_recv() {
                events.push((index, event));
            }
        }

        let mut finished_jobs = Vec::new();

        for (job_index, event) in events {
            match event {
                TranscriptionUiEvent::Progress {
                    session_path,
                    generation,
                    progress,
                } => {
                    if self.should_apply_generation(&session_path, generation) {
                        self.apply_transcription_progress(progress);
                    }
                }
                TranscriptionUiEvent::Complete {
                    session_path,
                    generation,
                    transcript_path,
                    published,
                } => {
                    let apply =
                        published && self.should_apply_generation(&session_path, generation);
                    if apply {
                        self.transcription_status.label = "Transcript ready".to_string();
                        self.transcription_status.percent = 100;
                        self.transcription_status.transcript_path = Some(transcript_path.clone());
                        self.transcription_status.failed = false;
                        self.toast = format!("Transcript ready: {}", transcript_path.display());
                        self.live_notes
                            .push(format!("Transcript ready: {}", transcript_path.display()));
                    } else if self.is_current_session(&session_path) {
                        self.live_notes.push(format!(
                            "Ignored stale take {generation} transcript after a newer take."
                        ));
                    } else {
                        self.live_notes.push(format!(
                            "Transcript ready for {}.",
                            Self::session_label(&session_path)
                        ));
                    }
                    if apply && self.auto_analyze {
                        self.start_analysis_job(session_path, generation);
                    }
                    finished_jobs.push(job_index);
                }
                TranscriptionUiEvent::Failed {
                    session_path,
                    generation,
                    message,
                } => {
                    if self.should_apply_generation(&session_path, generation) {
                        self.transcription_status.label = "Transcription failed".to_string();
                        self.transcription_status.percent = 0;
                        self.transcription_status.failed = true;
                        self.toast = format!("Transcription failed: {message}");
                    }
                    if !self.is_current_session(&session_path)
                        || self.should_apply_generation(&session_path, generation)
                    {
                        self.live_notes.push(format!(
                            "Transcription failed for {}: {message}",
                            Self::session_label(&session_path)
                        ));
                    }
                    finished_jobs.push(job_index);
                }
            }
        }

        finished_jobs.sort_unstable();
        finished_jobs.dedup();
        for index in finished_jobs.into_iter().rev() {
            self.transcription_jobs.remove(index);
        }
    }

    fn start_analysis_job(&mut self, session_path: PathBuf, generation: u32) {
        if self
            .analysis_jobs
            .iter()
            .any(|job| job.session_path == session_path && job.generation == generation)
        {
            return;
        }

        let Some(agent) = self.agent.clone() else {
            if self.should_apply_generation(&session_path, generation) {
                self.analysis_status.label = "Analysis skipped: no agent".to_string();
                self.analysis_status.percent = 0;
                self.toast = "Analysis skipped because no agent is selected.".to_string();
            }
            self.live_notes.push(format!(
                "Analysis skipped for {}: no agent selected.",
                Self::session_label(&session_path)
            ));
            return;
        };

        let preset = self.preset.clone();
        let (sender, receiver) = mpsc::channel();
        self.analysis_jobs.push(AnalysisJob {
            session_path: session_path.clone(),
            generation,
            receiver,
        });
        if self.should_apply_generation(&session_path, generation) {
            self.analysis_status = AnalysisStatus::running(&agent);
        }
        self.toast = format!("Analysis queued with {agent}.");
        self.live_notes.push(format!(
            "Analysis queued with {agent} for take {generation}."
        ));

        thread::spawn(move || {
            let original_session_path = session_path.clone();
            let options = AnalyzeOptions {
                target: AnalyzeTarget::Session(session_path),
                storage_dir: None,
                agent,
                preset,
                dry_run: false,
                generation: Some(generation),
            };
            match analyze(&options) {
                Ok(result) => {
                    let meeting_path = result.session_path.join("meeting.md");
                    let _ = sender.send(AnalysisUiEvent::Complete {
                        original_session_path,
                        session_path: result.session_path,
                        generation,
                        meeting_path,
                        generated_title: result.generated_title,
                        published: result.published,
                    });
                }
                Err(error) => {
                    let _ = sender.send(AnalysisUiEvent::Failed {
                        session_path: original_session_path,
                        generation,
                        message: error.to_string(),
                    });
                }
            }
        });
    }

    fn process_analysis_events(&mut self) {
        let mut events = Vec::new();

        for (index, job) in self.analysis_jobs.iter().enumerate() {
            while let Ok(event) = job.receiver.try_recv() {
                events.push((index, event));
            }
        }

        let mut finished_jobs = Vec::new();

        for (job_index, event) in events {
            match event {
                AnalysisUiEvent::Complete {
                    original_session_path,
                    session_path,
                    generation,
                    meeting_path,
                    generated_title,
                    published,
                } => {
                    let apply = published
                        && self.should_apply_generation(&original_session_path, generation);
                    if apply {
                        let mut session_path = session_path;
                        let mut meeting_path = meeting_path;
                        if let Some(title) = generated_title.clone() {
                            if self.take_index <= 1
                                && self.completed_take <= 1
                                && matches!(self.state, CaptureState::Ended)
                            {
                                if let Ok(renamed) =
                                    maybe_rename_session_dir_for_title(&session_path, &title)
                                {
                                    if renamed != session_path {
                                        meeting_path = renamed.join("meeting.md");
                                        session_path = renamed;
                                    }
                                }
                            }
                            self.title = title.clone();
                            self.live_notes.push(format!("Session titled: {title}"));
                        }
                        self.session_path = Some(session_path.clone());
                        if self.transcription_status.transcript_path.is_some() {
                            self.transcription_status.transcript_path =
                                Some(session_path.join("transcript.md"));
                        }
                        self.analysis_status.label = "Meeting notes ready".to_string();
                        self.analysis_status.percent = 100;
                        self.analysis_status.result_path = Some(meeting_path.clone());
                        self.analysis_status.failed = false;
                        self.toast = "Meeting notes ready.".to_string();
                        self.live_notes
                            .push(format!("Meeting ready: {}", meeting_path.display()));
                    } else if self.is_current_session(&original_session_path) {
                        self.live_notes.push(format!(
                            "Ignored stale take {generation} analysis after a newer take."
                        ));
                    } else {
                        self.live_notes.push(format!(
                            "Analysis ready for {}.",
                            Self::session_label(&session_path)
                        ));
                    }
                    finished_jobs.push(job_index);
                }
                AnalysisUiEvent::Failed {
                    session_path,
                    generation,
                    message,
                } => {
                    if self.should_apply_generation(&session_path, generation) {
                        self.analysis_status.label = "Analysis failed".to_string();
                        self.analysis_status.percent = 0;
                        self.analysis_status.failed = true;
                        self.toast = format!("Analysis failed: {message}");
                    }
                    if !self.is_current_session(&session_path)
                        || self.should_apply_generation(&session_path, generation)
                    {
                        self.live_notes.push(format!(
                            "Analysis failed for {}: {message}",
                            Self::session_label(&session_path)
                        ));
                    }
                    finished_jobs.push(job_index);
                }
            }
        }

        finished_jobs.sort_unstable();
        finished_jobs.dedup();
        for index in finished_jobs.into_iter().rev() {
            self.analysis_jobs.remove(index);
        }
    }

    fn apply_transcription_progress(&mut self, progress: TranscriptionProgress) {
        match progress {
            TranscriptionProgress::Started {
                session_path,
                engine,
                model,
                note,
            } => {
                let model_short = model.rsplit(['/', '\\']).next().unwrap_or(model.as_str());
                self.transcription_status.label =
                    format!("Transcription started ({}, {model_short})", engine.as_str());
                self.transcription_status.percent = 2;
                self.live_notes.push(format!(
                    "Transcription started ({}): {}",
                    engine.as_str(),
                    session_path.display()
                ));
                if let Some(note) = note {
                    self.live_notes.push(note);
                    self.toast = self.live_notes.last().cloned().unwrap_or_default();
                }
            }
            TranscriptionProgress::ModelDownloadStarted {
                model,
                cache_path,
                expected_bytes,
            } => {
                self.transcription_status.label = format!(
                    "Downloading Parakeet model ({model}, ~{}) — one-time",
                    format_bytes(expected_bytes)
                );
                self.transcription_status.percent = 1;
                self.transcription_status.failed = false;
                self.toast = self.transcription_status.label.clone();
                self.live_notes.push(format!(
                    "Downloading {model} (~{}) to {} — one-time, reused across sessions.",
                    format_bytes(expected_bytes),
                    cache_path.display()
                ));
            }
            TranscriptionProgress::ModelDownloadProgress {
                model,
                cache_path,
                downloaded_bytes,
                total_bytes,
                bytes_per_sec,
            } => {
                self.transcription_status.label = format_model_download_label(
                    &model,
                    downloaded_bytes,
                    total_bytes,
                    bytes_per_sec,
                );
                self.transcription_status.percent = downloaded_bytes
                    .saturating_mul(99)
                    .checked_div(total_bytes)
                    .unwrap_or(1)
                    .clamp(1, 99) as u16;
                self.toast = format!(
                    "{}  Cache: {}",
                    self.transcription_status.label,
                    cache_path.display()
                );
            }
            TranscriptionProgress::ModelDownloadFinished { model, cache_path } => {
                self.transcription_status.label =
                    format!("Parakeet model ready ({model}). Starting transcription.");
                self.transcription_status.percent = 8;
                self.toast = self.transcription_status.label.clone();
                self.live_notes
                    .push(format!("Model cached at {}", cache_path.display()));
            }
            TranscriptionProgress::TrackStarted {
                track,
                chunks,
                elapsed_secs,
            } => {
                self.transcription_status.label =
                    format!("Transcribing {track}: {chunks} chunk(s) ({elapsed_secs}s)");
                self.transcription_status.percent = 8;
            }
            TranscriptionProgress::ChunkStarted {
                track,
                index,
                total,
                elapsed_secs,
            } => {
                self.transcription_status.label =
                    format!("Transcribing {track} chunk {index}/{total} ({elapsed_secs}s)");
                let completed = index.saturating_sub(1) as f64;
                self.transcription_status.percent =
                    (8.0 + (completed / total.max(1) as f64) * 80.0).round() as u16;
            }
            TranscriptionProgress::TrackFinished {
                track,
                text_len,
                chunks,
                elapsed_secs,
            } => {
                self.transcription_status.label = format!(
                    "Finished {track}: {text_len} chars across {chunks} chunk(s) ({elapsed_secs}s)"
                );
                self.transcription_status.percent = 90;
            }
            TranscriptionProgress::Finished {
                transcript_path,
                elapsed_secs,
            } => {
                self.transcription_status.label =
                    format!("Finalizing transcript ({elapsed_secs}s)");
                self.transcription_status.percent = 99;
                self.transcription_status.transcript_path = Some(transcript_path);
            }
        }
    }

    fn stop_mic_recorder(&mut self) {
        if let Some(mut recorder) = self.mic_recorder.take() {
            match recorder.stop() {
                Ok(()) => {
                    self.mic_level_percent = 0;
                    self.mic_level_db = None;
                }
                Err(error) => {
                    self.toast = format!("Failed to stop mic recorder cleanly: {error}");
                }
            }
        }
    }

    fn stop_system_recorder(&mut self) {
        if let Some(mut recorder) = self.system_recorder.take() {
            match recorder.stop() {
                Ok(()) => {
                    self.call_level_percent = 0;
                    self.call_level_db = None;
                }
                Err(error) => {
                    self.toast = format!("Failed to stop system recorder cleanly: {error}");
                }
            }
        }
    }

    fn stop_recorders(&mut self) {
        self.stop_mic_recorder();
        self.stop_system_recorder();
    }

    fn elapsed(&self) -> Duration {
        match (self.state, self.started_at) {
            (CaptureState::Recording, Some(started_at)) => self.accumulated + started_at.elapsed(),
            _ => self.accumulated,
        }
    }

    fn elapsed_label(&self) -> String {
        let elapsed = self.elapsed().as_secs();
        let minutes = elapsed / 60;
        let seconds = elapsed % 60;
        format!("{minutes:02}:{seconds:02}")
    }

    fn status_label(&self) -> &'static str {
        match self.state {
            CaptureState::Ready => "READY",
            CaptureState::Recording => "REC",
            CaptureState::Ended => "ENDED",
        }
    }

    fn status_color(&self) -> Color {
        match self.state {
            CaptureState::Ready => Color::Cyan,
            CaptureState::Recording => Color::Red,
            CaptureState::Ended => Color::Green,
        }
    }

    fn meter_value(&self, offset: u64) -> u16 {
        match self.state {
            CaptureState::Recording => {
                let wave = ((self.tick + offset) * 17) % 64;
                26 + wave as u16
            }
            _ => 0,
        }
    }

    fn render(&self, frame: &mut Frame) {
        let area = frame.area();
        let main = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Length(7),
                Constraint::Min(5),
                Constraint::Length(6),
                Constraint::Length(3),
            ])
            .split(area);

        self.render_header(frame, main[0]);

        let top = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(48), Constraint::Percentage(52)])
            .split(main[1]);
        self.render_sources(frame, top[0]);
        self.render_signal(frame, top[1]);

        self.render_live_recall(frame, main[2]);

        let bottom = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(48), Constraint::Percentage(52)])
            .split(main[3]);
        self.render_decisions(frame, bottom[0]);
        self.render_actions(frame, bottom[1]);

        self.render_footer(frame, main[4]);
    }

    fn render_header(&self, frame: &mut Frame, area: Rect) {
        let status = format!("{} {}", self.status_label(), self.elapsed_label());
        let consent = if self.consent_noted {
            "consent: noted"
        } else {
            "consent: not noted"
        };
        let next = if self.append_next {
            "next: append"
        } else {
            "next: new"
        };
        let title = Line::from(vec![
            Span::styled(
                " Recall ",
                Style::default().fg(Color::Black).bg(Color::Cyan),
            ),
            Span::raw("  Local meeting memory"),
            Span::raw(format!("  {}  ", self.title)),
            Span::raw("  "),
            Span::styled(
                status,
                Style::default()
                    .fg(self.status_color())
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(consent, Style::default().fg(Color::Gray)),
            Span::raw("  "),
            Span::styled(next, Style::default().fg(Color::Gray)),
        ]);
        frame.render_widget(
            Paragraph::new(title).block(Block::default().borders(Borders::ALL)),
            area,
        );
    }

    fn render_sources(&self, frame: &mut Frame, area: Rect) {
        let mut items = Vec::new();

        items.push(ListItem::new(Line::from(vec![
            Span::styled("Mic: ", Style::default().fg(Color::Cyan)),
            Span::styled(self.active_mic_source_label(), self.mic_signal_color()),
        ])));
        items.push(ListItem::new(Line::from(vec![
            Span::styled("Call: ", Style::default().fg(Color::Magenta)),
            Span::styled(self.active_call_source_label(), self.call_signal_color()),
        ])));
        items.push(ListItem::new(Line::from(vec![
            Span::styled("Scan: ", Style::default().fg(Color::Gray)),
            Span::raw(if self.source_receiver.is_some() {
                "refreshing likely apps"
            } else {
                self.sources.status.as_str()
            }),
        ])));
        items.push(ListItem::new(Line::raw("")));
        items.push(ListItem::new(Line::styled(
            "Likely call apps",
            Style::default().fg(Color::Gray),
        )));

        for app in self.sources.apps.iter().take(2) {
            items.push(ListItem::new(Line::from(vec![
                Span::styled("• ", Style::default().fg(Color::Magenta)),
                Span::raw(app),
            ])));
        }

        frame.render_widget(
            List::new(items).block(Block::default().title(" Sources ").borders(Borders::ALL)),
            area,
        );
    }

    fn active_mic_source_label(&self) -> String {
        if let Some(warning) = &self.mic_capture_warning {
            return warning.clone();
        }

        if let Some(device) = &self.mic_device_label {
            if self.mic_recorder.is_some() {
                return format!("active - {device}");
            }
            return format!("last - {device}");
        }

        if self.mic_recorder.is_some() {
            "active - default input".to_string()
        } else {
            self.sources
                .microphones
                .first()
                .map(|device| format!("ready - {device}"))
                .unwrap_or_else(|| "ready - default input".to_string())
        }
    }

    fn active_call_source_label(&self) -> String {
        if let Some(warning) = &self.system_capture_warning {
            return warning.clone();
        }

        if self.system_recorder.is_some() {
            "active - system audio, all apps".to_string()
        } else if self.system_capture_failed {
            "unavailable".to_string()
        } else {
            "ready - system audio, all apps".to_string()
        }
    }

    fn render_signal(&self, frame: &mut Frame, area: Rect) {
        let block = Block::default().title(" Signal ").borders(Borders::ALL);
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let inner_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Length(1),
                Constraint::Min(1),
            ])
            .split(inner);

        frame.render_widget(
            signal_gauge("Mic", self.mic_signal_value(), self.mic_signal_color()),
            inner_chunks[0],
        );
        frame.render_widget(
            signal_gauge("Call", self.call_signal_value(), self.call_signal_color()),
            inner_chunks[1],
        );
        frame.render_widget(
            signal_gauge(
                "Text",
                self.transcription_status.percent,
                self.transcription_color(),
            ),
            inner_chunks[2],
        );
        frame.render_widget(
            signal_gauge("Noise", self.meter_value(23) / 3, Color::Green),
            inner_chunks[3],
        );
        frame.render_widget(
            signal_gauge("AI", self.analysis_status.percent, self.analysis_color()),
            inner_chunks[4],
        );
    }

    fn mic_signal_value(&self) -> u16 {
        if self.mic_recorder.is_some() {
            self.mic_level_percent
        } else {
            self.meter_value(0)
        }
    }

    fn call_signal_value(&self) -> u16 {
        if self.system_recorder.is_some() {
            self.call_level_percent
        } else if self.system_capture_failed {
            0
        } else {
            self.meter_value(11)
        }
    }

    fn mic_signal_color(&self) -> Color {
        if self.mic_capture_warning.is_some() {
            Color::Yellow
        } else if self.mic_recorder.is_some() {
            Color::Cyan
        } else {
            Color::Gray
        }
    }

    fn call_signal_color(&self) -> Color {
        if self.system_capture_failed || self.system_capture_warning.is_some() {
            Color::Red
        } else if self.system_recorder.is_some() {
            Color::Magenta
        } else {
            Color::Gray
        }
    }

    fn transcription_color(&self) -> Color {
        if self.transcription_status.failed {
            Color::Red
        } else if self.transcription_status.percent >= 100 {
            Color::Green
        } else if !self.transcription_jobs.is_empty() {
            Color::Yellow
        } else {
            Color::Gray
        }
    }

    fn analysis_color(&self) -> Color {
        if self.analysis_status.failed {
            Color::Red
        } else if self.analysis_status.percent >= 100 {
            Color::Green
        } else if !self.analysis_jobs.is_empty() {
            Color::Yellow
        } else {
            Color::Gray
        }
    }

    fn agent_label(&self) -> String {
        self.agent.clone().unwrap_or_else(|| "none".to_string())
    }

    fn engine_status_parts(&self) -> (String, String) {
        engine_status_parts(
            self.engine,
            self.parakeet_model.as_deref(),
            self.model_path.as_deref(),
        )
    }

    fn editor_label(&self) -> String {
        self.editor
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty() && *value != "open")
            .unwrap_or("Finder")
            .to_string()
    }

    fn open_shortcuts_line(&self) -> Line<'static> {
        Line::from(vec![
            Span::styled("Open: ", Style::default().fg(Color::Gray)),
            Span::styled(" o ", Style::default().fg(Color::Black).bg(Color::Blue)),
            Span::raw(format!(" folder in {}  ", self.editor_label())),
            Span::styled(" O ", Style::default().fg(Color::Black).bg(Color::Blue)),
            Span::raw(" meeting.md"),
        ])
    }

    fn render_live_recall(&self, frame: &mut Frame, area: Rect) {
        let session = self
            .session_path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "No session yet".to_string());
        let mut lines = vec![Line::from(vec![
            Span::styled("Session: ", Style::default().fg(Color::Gray)),
            Span::raw(session),
        ])];
        if self.session_path.is_some() {
            lines.push(self.open_shortcuts_line());
        }
        lines.push(self.capture_health_line());
        if self.has_background_jobs() {
            lines.push(self.background_jobs_line());
        }
        if self.take_index > 1 {
            lines.push(Line::from(vec![
                Span::styled("Take: ", Style::default().fg(Color::Gray)),
                Span::raw(self.take_index.to_string()),
            ]));
        }
        if let Some(older) = self.older_transcription_take() {
            lines.push(Line::from(vec![Span::styled(
                format!("Take {older} transcript still running"),
                Style::default().fg(Color::Yellow),
            )]));
        }
        lines.push(Line::raw(""));

        if !self.transcription_jobs.is_empty()
            || self.transcription_status.percent > 0
            || self.transcription_status.failed
        {
            lines.push(Line::from(vec![
                Span::styled("Transcript: ", Style::default().fg(Color::Gray)),
                Span::styled(
                    format!(
                        "{} ({}%)",
                        self.transcription_status.label, self.transcription_status.percent
                    ),
                    Style::default().fg(self.transcription_color()),
                ),
            ]));
            if let Some(path) = &self.transcription_status.transcript_path {
                lines.push(Line::from(vec![
                    Span::styled("Output: ", Style::default().fg(Color::Gray)),
                    Span::raw(path.display().to_string()),
                ]));
            }
            lines.push(Line::raw(""));
        }

        let (engine, model) = self.engine_status_parts();
        lines.push(Line::from(vec![
            Span::styled("Engine: ", Style::default().fg(Color::Gray)),
            Span::raw(engine),
            Span::styled("  model: ", Style::default().fg(Color::Gray)),
            Span::raw(model),
        ]));
        lines.push(Line::from(vec![
            Span::styled("Agent: ", Style::default().fg(Color::Gray)),
            Span::raw(self.agent_label()),
            Span::styled("  auto-analyze: ", Style::default().fg(Color::Gray)),
            Span::raw(if self.auto_analyze { "on" } else { "off" }),
            Span::styled("  preset: ", Style::default().fg(Color::Gray)),
            Span::raw(self.preset.clone()),
        ]));

        if !self.analysis_jobs.is_empty()
            || self.analysis_status.percent > 0
            || self.analysis_status.failed
        {
            lines.push(Line::from(vec![
                Span::styled("Analysis: ", Style::default().fg(Color::Gray)),
                Span::styled(
                    format!(
                        "{} ({}%)",
                        self.analysis_status.label, self.analysis_status.percent
                    ),
                    Style::default().fg(self.analysis_color()),
                ),
            ]));
            if let Some(path) = &self.analysis_status.result_path {
                lines.push(Line::from(vec![
                    Span::styled("Meeting: ", Style::default().fg(Color::Gray)),
                    Span::raw(path.display().to_string()),
                    Span::styled("  (O)", Style::default().fg(Color::DarkGray)),
                ]));
            }
            lines.push(Line::raw(""));
        }

        if let Some(draft) = &self.note_draft {
            lines.push(Line::from(vec![
                Span::styled("Note draft: ", Style::default().fg(Color::Blue)),
                Span::raw(draft.clone()),
                Span::styled("_", Style::default().fg(Color::Blue)),
            ]));
            lines.push(Line::raw(""));
        }

        for note in self.live_notes.iter().rev().take(6).rev() {
            lines.push(Line::from(vec![
                Span::styled("• ", Style::default().fg(Color::Cyan)),
                Span::raw(note),
            ]));
        }

        if !self.markers.is_empty() {
            lines.push(Line::raw(""));
            lines.push(Line::styled("Markers", Style::default().fg(Color::Yellow)));
            for marker in self.markers.iter().rev().take(3).rev() {
                lines.push(Line::from(vec![
                    Span::styled("◆ ", Style::default().fg(Color::Yellow)),
                    Span::raw(marker),
                ]));
            }
        }

        frame.render_widget(
            Paragraph::new(lines)
                .block(
                    Block::default()
                        .title(" Live Recall ")
                        .borders(Borders::ALL),
                )
                .wrap(Wrap { trim: true }),
            area,
        );
    }

    fn capture_health_line(&self) -> Line<'static> {
        let mic_state = if let Some(warning) = &self.mic_capture_warning {
            Span::styled(
                format!("Mic warning: {warning}"),
                Style::default().fg(Color::Yellow),
            )
        } else if self.mic_recorder.is_some() {
            let device = self
                .mic_device_label
                .clone()
                .unwrap_or_else(|| "default input".to_string());
            Span::styled(
                format!("Mic active: {device}"),
                Style::default().fg(Color::Cyan),
            )
        } else if let Some(device) = &self.mic_device_label {
            Span::styled(
                format!("Mic saved: {device}"),
                Style::default().fg(Color::Gray),
            )
        } else {
            Span::styled("Mic idle", Style::default().fg(Color::Gray))
        };

        let call_state = if let Some(warning) = &self.system_capture_warning {
            Span::styled(
                format!("Call warning: {warning}"),
                Style::default().fg(Color::Red),
            )
        } else if self.system_recorder.is_some() {
            Span::styled("Call active", Style::default().fg(Color::Magenta))
        } else if self.system_capture_failed {
            Span::styled("Call unavailable", Style::default().fg(Color::Red))
        } else {
            Span::styled("Call idle", Style::default().fg(Color::Gray))
        };

        Line::from(vec![
            Span::styled("Capture: ", Style::default().fg(Color::Gray)),
            mic_state,
            Span::raw("  |  "),
            call_state,
        ])
    }

    fn older_transcription_take(&self) -> Option<u32> {
        self.transcription_jobs
            .iter()
            .map(|job| job.generation)
            .filter(|generation| *generation < self.take_index)
            .min()
    }

    fn background_jobs_line(&self) -> Line<'static> {
        let transcript_count = self.transcription_jobs.len();
        let analysis_count = self.analysis_jobs.len();
        let label = match (transcript_count, analysis_count) {
            (0, 0) => "idle".to_string(),
            (transcripts, 0) => format!("{transcripts} transcript job(s)"),
            (0, analyses) => format!("{analyses} analysis job(s)"),
            (transcripts, analyses) => {
                format!("{transcripts} transcript job(s), {analyses} analysis job(s)")
            }
        };
        Line::from(vec![
            Span::styled("Processing: ", Style::default().fg(Color::Gray)),
            Span::styled(label, Style::default().fg(Color::Yellow)),
        ])
    }

    fn render_decisions(&self, frame: &mut Frame, area: Rect) {
        let items = match self.state {
            CaptureState::Ready => vec![
                "No session yet".to_string(),
                "Press c after consent".to_string(),
                "Press Space or Enter to start".to_string(),
            ],
            CaptureState::Recording => vec![
                format!(
                    "Microphone recording targets audio/{}",
                    AudioTrack::Mic.segment_name(self.take_index.max(1))
                ),
                format!(
                    "System audio targets audio/{}",
                    AudioTrack::Call.segment_name(self.take_index.max(1))
                ),
                "Space or Enter ends and starts transcription".to_string(),
                format!("Analysis agent: {}", self.agent_label()),
            ],
            CaptureState::Ended => vec![
                if self.resumed {
                    "Resumed previous session".to_string()
                } else {
                    "Audio finalized".to_string()
                },
                if self.append_next {
                    "Space or Enter records another take in this session".to_string()
                } else {
                    "Space or Enter starts a new session folder".to_string()
                },
                "s toggles append vs new. q leaves.".to_string(),
                self.transcription_status.label.clone(),
                self.analysis_status.label.clone(),
            ],
        };

        frame.render_widget(
            List::new(items.into_iter().map(ListItem::new).collect::<Vec<_>>())
                .block(Block::default().title(" Decisions ").borders(Borders::ALL)),
            area,
        );
    }

    fn render_actions(&self, frame: &mut Frame, area: Rect) {
        let mut items = vec![
            "Review Clean Conversation for remaining mic bleed".to_string(),
            "Use typed notes for important context during calls".to_string(),
            "Review meeting.md before relying on generated notes".to_string(),
        ];
        if let Some(path) = &self.transcription_status.transcript_path {
            items.insert(0, format!("Transcript: {}", path.display()));
        }
        frame.render_widget(
            List::new(items.into_iter().map(ListItem::new).collect::<Vec<_>>()).block(
                Block::default()
                    .title(" Action Items ")
                    .borders(Borders::ALL),
            ),
            area,
        );
    }

    fn render_footer(&self, frame: &mut Frame, area: Rect) {
        if let Some(draft) = &self.note_draft {
            let text = vec![
                Line::from(vec![
                    Span::styled(
                        " Note > ",
                        Style::default().fg(Color::Black).bg(Color::Blue),
                    ),
                    Span::raw(draft.clone()),
                    Span::styled("_", Style::default().fg(Color::Blue)),
                ]),
                Line::from(vec![
                    Span::styled(
                        " Enter ",
                        Style::default().fg(Color::Black).bg(Color::Green),
                    ),
                    Span::raw(" save  "),
                    Span::styled(" Esc ", Style::default().fg(Color::Black).bg(Color::Gray)),
                    Span::raw(" cancel  "),
                    Span::styled(
                        " Backspace ",
                        Style::default().fg(Color::Black).bg(Color::Yellow),
                    ),
                    Span::raw(" edit"),
                ]),
            ];
            frame.render_widget(
                Paragraph::new(text).block(Block::default().borders(Borders::ALL)),
                area,
            );
            return;
        }

        let help = Line::from(vec![
            Span::styled(
                " Space/Enter ",
                Style::default().fg(Color::Black).bg(Color::Green),
            ),
            Span::raw(" start/end/continue  "),
            Span::styled(" c ", Style::default().fg(Color::Black).bg(Color::Cyan)),
            Span::raw(" consent  "),
            Span::styled(" s ", Style::default().fg(Color::Black).bg(Color::Cyan)),
            Span::raw(" append/new  "),
            Span::styled(" m ", Style::default().fg(Color::Black).bg(Color::Magenta)),
            Span::raw(" marker  "),
            Span::styled(" n ", Style::default().fg(Color::Black).bg(Color::Blue)),
            Span::raw(" note  "),
            Span::styled(" r ", Style::default().fg(Color::Black).bg(Color::Cyan)),
            Span::raw(" refresh  "),
            Span::styled(" a ", Style::default().fg(Color::Black).bg(Color::Green)),
            Span::raw(" auto-ai  "),
            Span::styled(" A ", Style::default().fg(Color::Black).bg(Color::Green)),
            Span::raw(" agent  "),
            Span::styled(" o ", Style::default().fg(Color::Black).bg(Color::Blue)),
            Span::raw(" folder  "),
            Span::styled(" O ", Style::default().fg(Color::Black).bg(Color::Blue)),
            Span::raw(" meeting  "),
            Span::styled(" q ", Style::default().fg(Color::Black).bg(Color::Gray)),
            Span::raw(" quit  "),
            Span::styled(
                " Ctrl+C ",
                Style::default().fg(Color::Black).bg(Color::Gray),
            ),
            Span::raw(" quit"),
        ]);
        let text = vec![Line::raw(&self.toast), help];
        frame.render_widget(
            Paragraph::new(text).block(Block::default().borders(Borders::ALL)),
            area,
        );
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DetachPlan {
    session_path: PathBuf,
    generation: u32,
    transcribe: bool,
    analyze: bool,
}

fn detach_plans_from_jobs(
    transcription_jobs: &[TranscriptionJob],
    analysis_jobs: &[AnalysisJob],
    auto_analyze: bool,
    has_agent: bool,
) -> Vec<DetachPlan> {
    let mut plans = Vec::new();
    for job in transcription_jobs {
        let index = detach_plan_index(&mut plans, &job.session_path, job.generation);
        plans[index].transcribe = true;
        if auto_analyze && has_agent {
            plans[index].analyze = true;
        }
    }
    for job in analysis_jobs {
        let index = detach_plan_index(&mut plans, &job.session_path, job.generation);
        plans[index].analyze = true;
    }
    plans
}

fn detach_plan_index(plans: &mut Vec<DetachPlan>, path: &Path, generation: u32) -> usize {
    if let Some(index) = plans.iter().position(|plan| plan.session_path == path) {
        plans[index].generation = plans[index].generation.max(generation);
        index
    } else {
        plans.push(DetachPlan {
            session_path: path.to_path_buf(),
            generation,
            transcribe: false,
            analyze: false,
        });
        plans.len() - 1
    }
}

fn spawn_detached_postprocess(exe: &Path, app: &App, plan: &DetachPlan) -> io::Result<PathBuf> {
    let work = internal_dir(&plan.session_path).join("work");
    fs::create_dir_all(&work)?;
    let log_path = work.join("postprocess.log");
    let script_path = work.join("postprocess.sh");
    fs::write(&script_path, postprocess_script(exe, app, plan, &log_path))?;

    let mut command = Command::new("/bin/sh");
    command
        .arg(&script_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        command.process_group(0);
    }
    let _child = command.spawn()?;
    Ok(log_path)
}

fn postprocess_script(exe: &Path, app: &App, plan: &DetachPlan, log_path: &Path) -> String {
    let mut lines = vec![
        "#!/bin/sh".to_string(),
        "set -e".to_string(),
        format!(
            "exec >>{} 2>&1",
            posix_single_quote(&log_path.to_string_lossy())
        ),
        format!(
            "echo Recall postprocess started for {}",
            posix_single_quote(&plan.session_path.to_string_lossy())
        ),
    ];
    if plan.transcribe {
        lines.push(transcribe_command_line(
            exe,
            app,
            &plan.session_path,
            plan.generation,
        ));
    }
    if plan.analyze {
        if let Some(agent) = &app.agent {
            lines.push(format!(
                "{} analyze {} --agent {} --preset {} --generation {}",
                posix_single_quote(&exe.to_string_lossy()),
                posix_single_quote(&plan.session_path.to_string_lossy()),
                posix_single_quote(agent),
                posix_single_quote(&app.preset),
                plan.generation,
            ));
        }
    }
    lines.push("echo Recall postprocess finished".to_string());
    lines.join("\n") + "\n"
}

fn transcribe_command_line(exe: &Path, app: &App, session_path: &Path, generation: u32) -> String {
    let mut args = vec![
        posix_single_quote(&exe.to_string_lossy()),
        "transcribe".to_string(),
        posix_single_quote(&session_path.to_string_lossy()),
        "--engine".to_string(),
        app.engine.as_str().to_string(),
        "--chunk-seconds".to_string(),
        app.chunk_seconds.to_string(),
        "--generation".to_string(),
        generation.to_string(),
    ];
    push_quoted_path_flag(&mut args, "--ffmpeg", app.ffmpeg_bin.as_deref());
    push_quoted_path_flag(&mut args, "--whisper", app.whisper_bin.as_deref());
    push_quoted_path_flag(&mut args, "--model", app.model_path.as_deref());
    push_quoted_path_flag(&mut args, "--parakeet", app.parakeet_bin.as_deref());
    if let Some(model) = &app.parakeet_model {
        args.push("--parakeet-model".to_string());
        args.push(posix_single_quote(model));
    }
    push_quoted_path_flag(
        &mut args,
        "--parakeet-cache-dir",
        app.parakeet_cache_dir.as_deref(),
    );
    args.join(" ")
}

fn push_quoted_path_flag(args: &mut Vec<String>, flag: &str, path: Option<&Path>) {
    let Some(path) = path else {
        return;
    };
    args.push(flag.to_string());
    args.push(posix_single_quote(&path.to_string_lossy()));
}

fn posix_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

impl TranscriptionStatus {
    fn idle() -> Self {
        Self {
            label: "Transcript idle".to_string(),
            percent: 0,
            transcript_path: None,
            failed: false,
        }
    }

    fn queued() -> Self {
        Self {
            label: "Transcription queued".to_string(),
            percent: 1,
            transcript_path: None,
            failed: false,
        }
    }
}

impl AnalysisStatus {
    fn idle() -> Self {
        Self {
            label: "Analysis idle".to_string(),
            percent: 0,
            result_path: None,
            failed: false,
        }
    }

    fn running(agent: &str) -> Self {
        Self {
            label: format!("Analyzing with {agent}"),
            percent: 10,
            result_path: None,
            failed: false,
        }
    }
}

fn signal_gauge(label: &'static str, percent: u16, color: Color) -> Gauge<'static> {
    Gauge::default()
        .label(format!("{label:<5} {percent:>3}%"))
        .percent(percent.min(100))
        .gauge_style(Style::default().fg(color).add_modifier(Modifier::BOLD))
}

fn db_to_percent(level_db: f32) -> u16 {
    let normalized = ((level_db + 60.0) / 60.0).clamp(0.0, 1.0);
    (normalized * 100.0) as u16
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecordingAction {
    StartNew,
    Continue,
    End,
}

fn next_recording_action(
    state: CaptureState,
    has_session: bool,
    append_next: bool,
) -> RecordingAction {
    match state {
        CaptureState::Ready => RecordingAction::StartNew,
        CaptureState::Ended if has_session && append_next => RecordingAction::Continue,
        CaptureState::Ended => RecordingAction::StartNew,
        CaptureState::Recording => RecordingAction::End,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn test_app(current_session: PathBuf) -> App {
        App {
            state: CaptureState::Ended,
            consent_noted: true,
            tick: 0,
            started_at: None,
            ended_at: None,
            accumulated: Duration::ZERO,
            take_index: 1,
            completed_take: 1,
            session_path: Some(current_session),
            storage_dir: std::env::temp_dir(),
            toast: String::new(),
            markers: Vec::new(),
            live_notes: Vec::new(),
            sources: SourceSummary::fallback("test sources"),
            source_receiver: None,
            last_source_refresh_tick: 0,
            mic_recorder: None,
            system_recorder: None,
            default_title: "Quick Capture".to_string(),
            title: "Current Session".to_string(),
            mic_level_percent: 0,
            mic_level_db: None,
            mic_device_label: None,
            mic_device_id: None,
            mic_capture_warning: None,
            call_level_percent: 0,
            call_level_db: None,
            system_capture_failed: false,
            system_capture_warning: None,
            transcription_jobs: Vec::new(),
            transcription_status: TranscriptionStatus::idle(),
            analysis_jobs: Vec::new(),
            analysis_status: AnalysisStatus::idle(),
            agent: None,
            auto_analyze: false,
            preset: "general".to_string(),
            engine: TranscriptionEngine::default(),
            ffmpeg_bin: None,
            whisper_bin: None,
            model_path: None,
            parakeet_bin: None,
            parakeet_model: None,
            parakeet_cache_dir: None,
            require_parakeet: false,
            chunk_seconds: TRANSCRIPTION_CHUNK_SECONDS,
            note_draft: None,
            editor: None,
            resumed: false,
            append_next: true,
            capture_lock: None,
            detached_logs: Vec::new(),
        }
    }

    #[test]
    fn live_recall_engine_label_uses_configured_engine_and_short_model() {
        let mut app = test_app(PathBuf::from("/tmp/recall-current-session"));
        app.engine = TranscriptionEngine::Parakeet;
        app.parakeet_model = None;
        let (engine, model) = app.engine_status_parts();
        assert_eq!(engine, "parakeet");
        assert_eq!(model, "parakeet-tdt-0.6b-v3");

        app.engine = TranscriptionEngine::Whisper;
        app.model_path = Some(PathBuf::from(
            "/Users/me/Models/whisper.cpp/ggml-large-v3-turbo.bin",
        ));
        let (engine, model) = app.engine_status_parts();
        assert_eq!(engine, "whisper");
        assert_eq!(model, "ggml-large-v3-turbo.bin");
    }

    #[test]
    fn stale_transcription_completion_does_not_replace_current_session_status() {
        let current_session = PathBuf::from("/tmp/recall-current-session");
        let previous_session = PathBuf::from("/tmp/recall-previous-session");
        let previous_transcript = previous_session.join("transcript.md");
        let (sender, receiver) = mpsc::channel();
        let mut app = test_app(current_session.clone());
        app.transcription_jobs.push(TranscriptionJob {
            session_path: previous_session.clone(),
            generation: 1,
            receiver,
        });

        sender
            .send(TranscriptionUiEvent::Complete {
                session_path: previous_session.clone(),
                generation: 1,
                transcript_path: previous_transcript,
                published: true,
            })
            .unwrap();

        app.process_transcription_events();

        assert_eq!(app.session_path.as_ref(), Some(&current_session));
        assert_eq!(app.transcription_status.label, "Transcript idle");
        assert_eq!(app.transcription_status.percent, 0);
        assert!(app.transcription_status.transcript_path.is_none());
        assert!(app.transcription_jobs.is_empty());
        assert!(app
            .live_notes
            .iter()
            .any(|note| note.contains("Transcript ready for recall-previous-session.")));
    }

    #[test]
    fn stale_analysis_completion_does_not_replace_current_session_path_or_title() {
        let current_session = PathBuf::from("/tmp/recall-current-session");
        let previous_session = PathBuf::from("/tmp/recall-previous-session");
        let renamed_previous = PathBuf::from("/tmp/recall-renamed-previous-session");
        let (sender, receiver) = mpsc::channel();
        let mut app = test_app(current_session.clone());
        app.transcription_status.transcript_path = Some(current_session.join("transcript.md"));
        app.analysis_jobs.push(AnalysisJob {
            session_path: previous_session.clone(),
            generation: 1,
            receiver,
        });

        sender
            .send(AnalysisUiEvent::Complete {
                original_session_path: previous_session,
                meeting_path: renamed_previous.join("meeting.md"),
                session_path: renamed_previous,
                generation: 1,
                generated_title: Some("Previous Generated Title".to_string()),
                published: true,
            })
            .unwrap();

        app.process_analysis_events();

        assert_eq!(app.session_path.as_ref(), Some(&current_session));
        assert_eq!(
            app.transcription_status.transcript_path.as_ref(),
            Some(&current_session.join("transcript.md"))
        );
        assert_eq!(app.title, "Current Session");
        assert_eq!(app.analysis_status.label, "Analysis idle");
        assert!(app.analysis_jobs.is_empty());
        assert!(app
            .live_notes
            .iter()
            .any(|note| note.contains("Analysis ready for recall-renamed-previous-session.")));
    }

    #[test]
    fn continue_clears_finished_ai_gauge_but_keeps_in_flight_analysis() {
        let current_session = PathBuf::from("/tmp/recall-ai-gauge");
        let mut app = test_app(current_session.clone());
        app.analysis_status = AnalysisStatus {
            label: "Meeting notes ready".to_string(),
            percent: 100,
            result_path: Some(current_session.join("meeting.md")),
            failed: false,
        };
        app.reset_analysis_gauge_for_new_take();
        assert_eq!(app.analysis_status.percent, 0);
        assert_eq!(app.analysis_status.label, "Analysis idle");
        assert!(app.analysis_status.result_path.is_none());

        let (_sender, receiver) = mpsc::channel();
        app.analysis_status = AnalysisStatus::running("grok");
        app.analysis_jobs.push(AnalysisJob {
            session_path: current_session,
            generation: 1,
            receiver,
        });
        app.reset_analysis_gauge_for_new_take();
        assert_eq!(app.analysis_status.percent, 10);
        assert!(app.analysis_status.label.contains("Analyzing with grok"));
    }

    #[test]
    fn ended_session_continues_instead_of_starting_a_new_folder() {
        assert_eq!(
            next_recording_action(CaptureState::Ended, true, true),
            RecordingAction::Continue
        );
        assert_eq!(
            next_recording_action(CaptureState::Ready, false, true),
            RecordingAction::StartNew
        );
        assert_eq!(
            next_recording_action(CaptureState::Recording, true, true),
            RecordingAction::End
        );
    }

    #[test]
    fn quit_then_new_start_creates_a_new_session_action() {
        assert_eq!(
            next_recording_action(CaptureState::Ready, false, true),
            RecordingAction::StartNew
        );
        assert_eq!(
            next_recording_action(CaptureState::Ended, false, true),
            RecordingAction::StartNew
        );
    }

    #[test]
    fn ended_session_starts_a_new_folder_when_append_is_off() {
        assert_eq!(
            next_recording_action(CaptureState::Ended, true, false),
            RecordingAction::StartNew
        );
        let mut app = test_app(PathBuf::from("/tmp/recall-append-toggle"));
        app.state = CaptureState::Ended;
        app.replace_next_action_notes();
        app.toggle_append_next();
        assert!(!app.append_next);
        assert!(app.toast.contains("new session"));
        assert!(app
            .live_notes
            .iter()
            .any(|note| note.contains("starts a new session folder")));
        app.toggle_append_next();
        assert!(app.append_next);
        assert!(app.toast.contains("append"));
        assert!(app
            .live_notes
            .iter()
            .any(|note| note.contains("continues this session")));
    }

    #[test]
    fn late_take_one_transcript_does_not_overwrite_take_two() {
        let current_session = PathBuf::from("/tmp/recall-continued-session");
        let (sender, receiver) = mpsc::channel();
        let mut app = test_app(current_session.clone());
        app.take_index = 2;
        app.completed_take = 2;
        app.transcription_status = TranscriptionStatus::queued();
        app.auto_analyze = true;
        app.agent = Some("grok".to_string());
        app.transcription_jobs.push(TranscriptionJob {
            session_path: current_session.clone(),
            generation: 1,
            receiver,
        });

        sender
            .send(TranscriptionUiEvent::Complete {
                session_path: current_session.clone(),
                generation: 1,
                transcript_path: current_session.join("stale-transcript.md"),
                published: true,
            })
            .unwrap();

        app.process_transcription_events();

        assert_eq!(app.transcription_status.label, "Transcription queued");
        assert!(app.transcription_status.transcript_path.is_none());
        assert!(app.analysis_jobs.is_empty());
        assert!(app
            .live_notes
            .iter()
            .any(|note| note.contains("Ignored stale take 1 transcript")));
    }

    #[test]
    fn continue_is_allowed_while_take_one_jobs_are_running() {
        let current_session = PathBuf::from("/tmp/recall-continue-while-running");
        let (_sender, receiver) = mpsc::channel();
        let mut app = test_app(current_session.clone());
        app.state = CaptureState::Ended;
        app.take_index = 1;
        app.completed_take = 1;
        app.transcription_jobs.push(TranscriptionJob {
            session_path: current_session,
            generation: 1,
            receiver,
        });

        assert_eq!(
            next_recording_action(app.state, app.session_path.is_some(), app.append_next),
            RecordingAction::Continue
        );
        assert!(app.has_background_jobs());
        assert_eq!(app.older_transcription_take(), None);
        app.take_index = 2;
        assert_eq!(app.older_transcription_take(), Some(1));
    }

    #[test]
    fn late_take_one_analysis_does_not_replace_take_two_title() {
        let current_session = PathBuf::from("/tmp/recall-continued-analysis");
        let (sender, receiver) = mpsc::channel();
        let mut app = test_app(current_session.clone());
        app.take_index = 2;
        app.completed_take = 2;
        app.title = "Current Session".to_string();
        app.analysis_jobs.push(AnalysisJob {
            session_path: current_session.clone(),
            generation: 1,
            receiver,
        });

        sender
            .send(AnalysisUiEvent::Complete {
                original_session_path: current_session.clone(),
                session_path: current_session,
                generation: 1,
                meeting_path: PathBuf::from("/tmp/stale-meeting.md"),
                generated_title: Some("Stale Title".to_string()),
                published: true,
            })
            .unwrap();

        app.process_analysis_events();

        assert_eq!(app.title, "Current Session");
        assert_eq!(app.analysis_status.label, "Analysis idle");
        assert!(app.analysis_jobs.is_empty());
        assert!(app
            .live_notes
            .iter()
            .any(|note| note.contains("Ignored stale take 1 analysis")));
    }

    #[test]
    fn resume_opens_ended_session_and_space_continues_the_same_folder() {
        let storage = std::env::temp_dir().join(format!(
            "recall-tui-resume-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        let session = start_session(&StartOptions {
            title: "Design Sync".to_string(),
            consent: ConsentMode::Noted,
            storage_dir: storage.clone(),
        })
        .unwrap();
        let audio = session.path.join("audio");
        fs::create_dir_all(&audio).unwrap();
        fs::write(audio.join("mic-001.m4a"), b"mic").unwrap();
        fs::write(audio.join("call-001.m4a"), b"call").unwrap();
        write_capture_progress(
            &session.path,
            CaptureProgress {
                take_count: 1,
                completed_take: 1,
                elapsed_ms: 724_000,
            },
        )
        .unwrap();

        let prepared = prepare_resume(&storage, &ResumeTarget::Latest, false).unwrap();
        assert_eq!(prepared.path, session.path);
        assert_eq!(prepared.title, "Design Sync");
        assert!(prepared.consent_noted);

        let mut app = test_app(session.path.clone());
        app.apply_resume(prepared);
        assert_eq!(app.state, CaptureState::Ended);
        assert_eq!(app.session_path.as_ref(), Some(&session.path));
        assert_eq!(app.accumulated, Duration::from_millis(724_000));
        assert!(app.ended_at.is_none());
        assert_eq!(
            next_recording_action(app.state, app.session_path.is_some(), app.append_next),
            RecordingAction::Continue
        );
        assert!(app.toast.contains("Resumed"));
        assert!(app.toast.contains("take 2"));
        assert!(app.toast.contains("12:04"));
        assert!(app.toast.contains("break not added"));

        let continued = prepare_continued_take(&session.path).unwrap();
        assert_eq!(continued.take_index, 2);
        assert_eq!(
            fs::read_dir(&storage)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.path().is_dir())
                .count(),
            1
        );

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn resume_refuses_an_in_progress_take() {
        let storage = std::env::temp_dir().join(format!(
            "recall-tui-resume-busy-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ));
        let session = start_session(&StartOptions {
            title: "Busy".to_string(),
            consent: ConsentMode::Noted,
            storage_dir: storage.clone(),
        })
        .unwrap();
        write_capture_progress(
            &session.path,
            CaptureProgress {
                take_count: 1,
                completed_take: 0,
                elapsed_ms: 1_000,
            },
        )
        .unwrap();

        let error =
            prepare_resume(&storage, &ResumeTarget::Named(session.id.clone()), false).unwrap_err();
        assert!(error.to_string().contains("unfinished take"));

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn resumed_ended_session_allows_notes_and_does_not_add_overnight_gap() {
        let mut app = test_app(PathBuf::from("/tmp/recall-resume-notes"));
        app.apply_resume(PreparedResume {
            path: PathBuf::from("/tmp/recall-resume-notes"),
            title: "Grill supper".to_string(),
            consent_noted: true,
            progress: CaptureProgress {
                take_count: 1,
                completed_take: 1,
                elapsed_ms: 60_000,
            },
        });
        assert_eq!(app.elapsed_label(), "01:00");
        app.start_manual_note();
        assert!(app.note_draft.is_some());
        assert!(app.ended_at.is_none());
    }

    #[test]
    fn posix_single_quote_escapes_embedded_quotes() {
        assert_eq!(posix_single_quote("plain"), "'plain'");
        assert_eq!(posix_single_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn detached_transcribe_command_passes_generation() {
        let app = test_app(PathBuf::from("/tmp/recall-command-line"));
        let line =
            transcribe_command_line(Path::new("/tmp/recall"), &app, Path::new("/tmp/session"), 2);
        assert!(line.contains(" transcribe "));
        assert!(line.contains("--generation 2"));
        assert!(line.contains("'/tmp/session'"));
    }

    #[test]
    fn quit_detach_plan_reruns_transcribe_and_follow_on_analysis() {
        let session = PathBuf::from("/tmp/recall-detach-session");
        let (_sender, receiver) = mpsc::channel();
        let jobs = [TranscriptionJob {
            session_path: session.clone(),
            generation: 1,
            receiver,
        }];
        let plans = detach_plans_from_jobs(&jobs, &[], true, true);
        assert_eq!(
            plans,
            vec![DetachPlan {
                session_path: session,
                generation: 1,
                transcribe: true,
                analyze: true,
            }]
        );
    }

    #[test]
    fn quit_detach_plan_reruns_analysis_only_when_transcript_already_finished() {
        let session = PathBuf::from("/tmp/recall-detach-analysis");
        let (_sender, receiver) = mpsc::channel();
        let jobs = [AnalysisJob {
            session_path: session.clone(),
            generation: 1,
            receiver,
        }];
        let plans = detach_plans_from_jobs(&[], &jobs, true, true);
        assert_eq!(
            plans,
            vec![DetachPlan {
                session_path: session,
                generation: 1,
                transcribe: false,
                analyze: true,
            }]
        );
    }

    #[test]
    fn quit_detach_plan_keeps_the_newest_take_generation() {
        let session = PathBuf::from("/tmp/recall-detach-generations");
        let (_sender_one, receiver_one) = mpsc::channel();
        let (_sender_two, receiver_two) = mpsc::channel();
        let transcription = [TranscriptionJob {
            session_path: session.clone(),
            generation: 1,
            receiver: receiver_one,
        }];
        let analysis = [AnalysisJob {
            session_path: session.clone(),
            generation: 2,
            receiver: receiver_two,
        }];
        let plans = detach_plans_from_jobs(&transcription, &analysis, true, true);
        assert_eq!(
            plans,
            vec![DetachPlan {
                session_path: session,
                generation: 2,
                transcribe: true,
                analyze: true,
            }]
        );
    }
}
