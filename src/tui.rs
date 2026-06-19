use std::io;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, List, ListItem, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};

use crate::analysis::{analyze, known_agents, AnalyzeOptions, AnalyzeTarget};
use crate::capture_sources::{detect_sources, SourceSummary};
use crate::mic_recorder::MicRecorder;
use crate::session::{
    append_session_marker, append_session_note, default_storage_dir, start_session, ConsentMode,
    StartOptions,
};
use crate::system_recorder::SystemRecorder;
use crate::transcription::{
    transcribe_with_progress, TrackSelection, TranscribeOptions, TranscribeTarget,
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

#[derive(Debug, Clone)]
pub struct TuiOptions {
    pub consent_noted: bool,
    pub title: String,
    pub storage_dir: Option<PathBuf>,
    pub ffmpeg_bin: Option<PathBuf>,
    pub whisper_bin: Option<PathBuf>,
    pub model_path: Option<PathBuf>,
    pub chunk_seconds: u64,
    pub agent: Option<String>,
    pub auto_analyze: bool,
    pub preset: String,
}

impl Default for TuiOptions {
    fn default() -> Self {
        Self {
            consent_noted: false,
            title: "Quick Capture".to_string(),
            storage_dir: None,
            ffmpeg_bin: None,
            whisper_bin: None,
            model_path: None,
            chunk_seconds: TRANSCRIPTION_CHUNK_SECONDS,
            agent: None,
            auto_analyze: true,
            preset: "general".to_string(),
        }
    }
}

pub fn run_with_options(options: TuiOptions) -> io::Result<()> {
    let mut terminal = ratatui::try_init()?;
    let result = App::new(options)?.run(&mut terminal);
    let restore_result = ratatui::try_restore();

    match (result, restore_result) {
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) => Err(error),
        (Ok(()), Ok(())) => Ok(()),
    }
}

struct App {
    state: CaptureState,
    consent_noted: bool,
    tick: u64,
    started_at: Option<Instant>,
    accumulated: Duration,
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
    ffmpeg_bin: Option<PathBuf>,
    whisper_bin: Option<PathBuf>,
    model_path: Option<PathBuf>,
    chunk_seconds: u64,
    note_draft: Option<String>,
}

#[derive(Debug, Clone)]
struct TranscriptionStatus {
    label: String,
    percent: u16,
    transcript_path: Option<PathBuf>,
    failed: bool,
}

struct TranscriptionJob {
    receiver: Receiver<TranscriptionUiEvent>,
}

#[derive(Debug, Clone)]
enum TranscriptionUiEvent {
    Progress {
        session_path: PathBuf,
        progress: TranscriptionProgress,
    },
    Complete {
        session_path: PathBuf,
        transcript_path: PathBuf,
    },
    Failed {
        session_path: PathBuf,
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
    receiver: Receiver<AnalysisUiEvent>,
}

#[derive(Debug, Clone)]
enum AnalysisUiEvent {
    Complete {
        original_session_path: PathBuf,
        session_path: PathBuf,
        result_path: Option<PathBuf>,
        written_files: usize,
        generated_title: Option<String>,
    },
    Failed {
        session_path: PathBuf,
        message: String,
    },
}

impl App {
    fn new(options: TuiOptions) -> io::Result<Self> {
        let consent_noted = options.consent_noted;
        Ok(Self {
            state: CaptureState::Ready,
            consent_noted,
            tick: 0,
            started_at: None,
            accumulated: Duration::ZERO,
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
            ffmpeg_bin: options.ffmpeg_bin,
            whisper_bin: options.whisper_bin,
            model_path: options.model_path,
            chunk_seconds: options.chunk_seconds,
            note_draft: None,
        })
    }

    fn run(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
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
                        return Ok(());
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
            if self.has_background_jobs() {
                self.toast =
                    "Background work is running; wait for Recall to finish before quitting."
                        .to_string();
                return Ok(false);
            }
            self.stop_recorders();
            return Ok(true);
        }

        match key.code {
            KeyCode::Char('q') => {
                if self.has_background_jobs() {
                    self.toast =
                        "Background work is running; wait for Recall to finish before quitting."
                            .to_string();
                    return Ok(false);
                }
                self.stop_recorders();
                return Ok(true);
            }
            KeyCode::Enter | KeyCode::Char(' ') => self.primary_recording_action()?,
            KeyCode::Char('c') => self.toggle_consent(),
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
            _ => {}
        }

        Ok(false)
    }

    fn primary_recording_action(&mut self) -> io::Result<()> {
        match self.state {
            CaptureState::Ready | CaptureState::Ended => self.start_capture(),
            CaptureState::Recording => {
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
        self.accumulated = Duration::ZERO;
        self.system_capture_failed = false;
        self.system_capture_warning = None;
        self.mic_capture_warning = None;
        self.mic_device_label = None;
        self.mic_device_id = None;

        let mut started = Vec::new();
        let mut failures = Vec::new();

        match MicRecorder::start(&session.path) {
            Ok(recorder) => {
                self.mic_recorder = Some(recorder);
                started.push("mic");
            }
            Err(error) => failures.push(format!("mic failed: {error}")),
        }

        match SystemRecorder::start(&session.path) {
            Ok(recorder) => {
                self.system_recorder = Some(recorder);
                started.push("system audio");
            }
            Err(error) => {
                self.system_capture_failed = true;
                failures.push(format!("system audio failed: {error}"));
            }
        }

        if started.is_empty() {
            self.state = CaptureState::Ended;
            self.started_at = None;
            self.toast = format!(
                "Session created, but capture failed: {}",
                failures.join("; ")
            );
        } else {
            self.state = CaptureState::Recording;
            self.toast = format!(
                "Recording {}: {}",
                started.join(" + "),
                session.path.display()
            );
        }

        self.live_notes = vec![
            "Microphone recording writes audio/mic.m4a.".to_string(),
            "Mic source changes are detected while recording.".to_string(),
            "System audio capture writes audio/call.m4a via CoreAudio process taps.".to_string(),
            "Press Space or Enter to end and start transcription.".to_string(),
        ];
        for failure in failures {
            self.live_notes.push(failure);
        }

        Ok(())
    }

    fn toggle_consent(&mut self) {
        self.consent_noted = !self.consent_noted;
        self.toast = if self.consent_noted {
            "Consent noted for this local session.".to_string()
        } else {
            "Consent set back to not noted.".to_string()
        };
    }

    fn end_capture(&mut self) {
        match self.state {
            CaptureState::Recording => {
                if let Some(started_at) = self.started_at.take() {
                    self.accumulated += started_at.elapsed();
                }
                self.stop_recorders();
                self.state = CaptureState::Ended;
                self.start_transcription_job();
            }
            _ => {
                self.toast = "No active session to end.".to_string();
            }
        }
    }

    fn add_marker(&mut self) {
        if !matches!(self.state, CaptureState::Recording) {
            self.toast = "Start a session before adding markers.".to_string();
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
        self.toast = format!("{marker} and saved to markers.md");
    }

    fn start_manual_note(&mut self) {
        if !matches!(self.state, CaptureState::Recording) {
            self.toast = "Start a session before adding notes.".to_string();
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
        self.toast = format!("{note} saved to notes.md");
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

        let (sender, receiver) = mpsc::channel();
        self.transcription_jobs.push(TranscriptionJob { receiver });
        if self.is_current_session(&session_path) {
            self.transcription_status = TranscriptionStatus::queued();
        }
        self.toast = "Session ended. Audio finalized; transcription queued.".to_string();
        self.live_notes
            .push("Transcription queued for this session.".to_string());

        let ffmpeg_bin = self.ffmpeg_bin.clone();
        let whisper_bin = self.whisper_bin.clone();
        let model_path = self.model_path.clone();
        let chunk_seconds = self.chunk_seconds;

        thread::spawn(move || {
            let event_session_path = session_path.clone();
            let options = TranscribeOptions {
                target: TranscribeTarget::Session(session_path),
                track: TrackSelection::Both,
                storage_dir: None,
                ffmpeg_bin,
                model_path,
                whisper_bin,
                chunk_seconds,
                keep_wav: false,
            };

            let progress_sender = sender.clone();
            let progress_session_path = event_session_path.clone();
            let result = transcribe_with_progress(&options, |progress| {
                let _ = progress_sender.send(TranscriptionUiEvent::Progress {
                    session_path: progress_session_path.clone(),
                    progress,
                });
            });

            match result {
                Ok(result) => {
                    let _ = sender.send(TranscriptionUiEvent::Complete {
                        session_path: event_session_path,
                        transcript_path: result.transcript_path,
                    });
                }
                Err(error) => {
                    let _ = sender.send(TranscriptionUiEvent::Failed {
                        session_path: event_session_path,
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
                    progress,
                } => {
                    if self.is_current_session(&session_path) {
                        self.apply_transcription_progress(progress);
                    }
                }
                TranscriptionUiEvent::Complete {
                    session_path,
                    transcript_path,
                } => {
                    if self.is_current_session(&session_path) {
                        self.transcription_status.label = "Transcript ready".to_string();
                        self.transcription_status.percent = 100;
                        self.transcription_status.transcript_path = Some(transcript_path.clone());
                        self.transcription_status.failed = false;
                        self.toast = format!("Transcript ready: {}", transcript_path.display());
                        self.live_notes
                            .push(format!("Transcript ready: {}", transcript_path.display()));
                    } else {
                        self.live_notes.push(format!(
                            "Transcript ready for {}.",
                            Self::session_label(&session_path)
                        ));
                    }
                    if self.auto_analyze {
                        self.start_analysis_job(session_path);
                    }
                    finished_jobs.push(job_index);
                }
                TranscriptionUiEvent::Failed {
                    session_path,
                    message,
                } => {
                    if self.is_current_session(&session_path) {
                        self.transcription_status.label = "Transcription failed".to_string();
                        self.transcription_status.percent = 0;
                        self.transcription_status.failed = true;
                        self.toast = format!("Transcription failed: {message}");
                    }
                    self.live_notes.push(format!(
                        "Transcription failed for {}: {message}",
                        Self::session_label(&session_path)
                    ));
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

    fn start_analysis_job(&mut self, session_path: PathBuf) {
        if self
            .analysis_jobs
            .iter()
            .any(|job| job.session_path == session_path)
        {
            return;
        }

        let Some(agent) = self.agent.clone() else {
            if self.is_current_session(&session_path) {
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
            receiver,
        });
        if self.is_current_session(&session_path) {
            self.analysis_status = AnalysisStatus::running(&agent);
        }
        self.toast = format!("Analysis queued with {agent}.");
        self.live_notes
            .push(format!("Analysis queued with {agent}."));

        thread::spawn(move || {
            let original_session_path = session_path.clone();
            let options = AnalyzeOptions {
                target: AnalyzeTarget::Session(session_path),
                storage_dir: None,
                agent,
                preset,
                dry_run: false,
            };
            match analyze(&options) {
                Ok(result) => {
                    let _ = sender.send(AnalysisUiEvent::Complete {
                        original_session_path,
                        session_path: result.session_path,
                        result_path: result.result_path,
                        written_files: result.written_files.len(),
                        generated_title: result.generated_title,
                    });
                }
                Err(error) => {
                    let _ = sender.send(AnalysisUiEvent::Failed {
                        session_path: original_session_path,
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
                    result_path,
                    written_files,
                    generated_title,
                } => {
                    let is_current = self.is_current_session(&original_session_path);
                    if is_current {
                        self.session_path = Some(session_path.clone());
                        if self.transcription_status.transcript_path.is_some() {
                            self.transcription_status.transcript_path =
                                Some(session_path.join("transcript.md"));
                        }
                        self.analysis_status.label =
                            format!("Analysis ready ({written_files} files)");
                        self.analysis_status.percent = 100;
                        self.analysis_status.result_path = result_path.clone();
                        self.analysis_status.failed = false;
                        self.toast = "Analysis ready: summary/actions updated.".to_string();
                        if let Some(title) = generated_title {
                            self.title = title.clone();
                            self.live_notes.push(format!("Session titled: {title}"));
                        }
                    }
                    if let Some(path) = result_path {
                        if is_current {
                            self.live_notes
                                .push(format!("Analysis JSON ready: {}", path.display()));
                        } else {
                            self.live_notes.push(format!(
                                "Analysis ready for {}.",
                                Self::session_label(&session_path)
                            ));
                        }
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
                    message,
                } => {
                    if self.is_current_session(&session_path) {
                        self.analysis_status.label = "Analysis failed".to_string();
                        self.analysis_status.percent = 0;
                        self.analysis_status.failed = true;
                        self.toast = format!("Analysis failed: {message}");
                    }
                    self.live_notes.push(format!(
                        "Analysis failed for {}: {message}",
                        Self::session_label(&session_path)
                    ));
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
            TranscriptionProgress::Started { session_path } => {
                self.transcription_status.label = "Transcription started".to_string();
                self.transcription_status.percent = 2;
                self.live_notes
                    .push(format!("Transcription started: {}", session_path.display()));
            }
            TranscriptionProgress::TrackStarted { track, chunks } => {
                self.transcription_status.label =
                    format!("Transcribing {track}: {chunks} chunk(s)");
                self.transcription_status.percent = 5;
            }
            TranscriptionProgress::ChunkStarted {
                track,
                index,
                total,
            } => {
                self.transcription_status.label =
                    format!("Transcribing {track} chunk {index}/{total}");
                self.transcription_status.percent =
                    ((index as f64 / total.max(1) as f64) * 100.0).round() as u16;
            }
            TranscriptionProgress::TrackFinished {
                track,
                text_len,
                chunks,
            } => {
                self.transcription_status.label =
                    format!("Finished {track}: {text_len} chars across {chunks} chunk(s)");
                self.transcription_status.percent = 100;
            }
            TranscriptionProgress::Finished { transcript_path } => {
                self.transcription_status.label = "Finalizing transcript".to_string();
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

    fn render_live_recall(&self, frame: &mut Frame, area: Rect) {
        let session = self
            .session_path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "No session yet".to_string());
        let mut lines = vec![
            Line::from(vec![
                Span::styled("Session: ", Style::default().fg(Color::Gray)),
                Span::raw(session),
            ]),
            self.capture_health_line(),
        ];
        if self.has_background_jobs() {
            lines.push(self.background_jobs_line());
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
                    Span::styled("Analysis JSON: ", Style::default().fg(Color::Gray)),
                    Span::raw(path.display().to_string()),
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
                "Microphone recording targets audio/mic.m4a".to_string(),
                "System audio targets audio/call.m4a".to_string(),
                "Space or Enter ends and starts transcription".to_string(),
                format!("Analysis agent: {}", self.agent_label()),
            ],
            CaptureState::Ended => vec![
                "Audio finalized".to_string(),
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
            "Review generated summary/actions before relying on them".to_string(),
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
            Span::raw(" start/end  "),
            Span::styled(" c ", Style::default().fg(Color::Black).bg(Color::Cyan)),
            Span::raw(" consent  "),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app(current_session: PathBuf) -> App {
        App {
            state: CaptureState::Ended,
            consent_noted: true,
            tick: 0,
            started_at: None,
            accumulated: Duration::ZERO,
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
            ffmpeg_bin: None,
            whisper_bin: None,
            model_path: None,
            chunk_seconds: TRANSCRIPTION_CHUNK_SECONDS,
            note_draft: None,
        }
    }

    #[test]
    fn stale_transcription_completion_does_not_replace_current_session_status() {
        let current_session = PathBuf::from("/tmp/recall-current-session");
        let previous_session = PathBuf::from("/tmp/recall-previous-session");
        let previous_transcript = previous_session.join("transcript.md");
        let (sender, receiver) = mpsc::channel();
        let mut app = test_app(current_session.clone());
        app.transcription_jobs.push(TranscriptionJob { receiver });

        sender
            .send(TranscriptionUiEvent::Complete {
                session_path: previous_session.clone(),
                transcript_path: previous_transcript,
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
            receiver,
        });

        sender
            .send(AnalysisUiEvent::Complete {
                original_session_path: previous_session,
                session_path: renamed_previous,
                result_path: None,
                written_files: 7,
                generated_title: Some("Previous Generated Title".to_string()),
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
}
