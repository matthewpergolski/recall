use std::io;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, List, ListItem, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};

use crate::capture_sources::{detect_sources, SourceSummary};
use crate::mic_recorder::MicRecorder;
use crate::session::{default_storage_dir, start_session, ConsentMode, StartOptions};
use crate::system_recorder::SystemRecorder;

const TICK_RATE: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CaptureState {
    Ready,
    Recording,
    Paused,
    Ended,
}

#[derive(Debug, Clone)]
pub struct TuiOptions {
    pub consent_noted: bool,
    pub title: String,
}

impl Default for TuiOptions {
    fn default() -> Self {
        Self {
            consent_noted: false,
            title: "Quick Capture".to_string(),
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
    mic_recorder: Option<MicRecorder>,
    system_recorder: Option<SystemRecorder>,
    title: String,
    mic_level_percent: u16,
    mic_level_db: Option<f32>,
    call_level_percent: u16,
    call_level_db: Option<f32>,
    system_capture_failed: bool,
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
            storage_dir: default_storage_dir()?,
            toast: if consent_noted {
                "Ready with consent provided. Press Enter to start.".to_string()
            } else {
                "Ready. Press c after consent, then Enter to start.".to_string()
            },
            markers: Vec::new(),
            live_notes: vec![
                "Waiting for a session.".to_string(),
                "Meters become real as capture sources start.".to_string(),
            ],
            sources: detect_sources(),
            mic_recorder: None,
            system_recorder: None,
            title: options.title,
            mic_level_percent: 0,
            mic_level_db: None,
            call_level_percent: 0,
            call_level_db: None,
            system_capture_failed: false,
        })
    }

    fn run(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        loop {
            self.process_mic_events();
            self.process_system_events();
            terminal.draw(|frame| self.render(frame))?;

            if event::poll(TICK_RATE)? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Press && self.handle_key(key.code)? {
                        return Ok(());
                    }
                }
            }

            self.tick();
        }
    }

    fn handle_key(&mut self, code: KeyCode) -> io::Result<bool> {
        match code {
            KeyCode::Char('q') => {
                self.stop_recorders();
                return Ok(true);
            }
            KeyCode::Enter => self.start_capture()?,
            KeyCode::Char('c') => self.toggle_consent(),
            KeyCode::Char(' ') | KeyCode::Char('p') => self.toggle_pause(),
            KeyCode::Char('e') => self.end_capture(),
            KeyCode::Char('m') => self.add_marker(),
            KeyCode::Char('n') => self.add_manual_note(),
            KeyCode::Char('r') => self.refresh_sources(),
            _ => {}
        }

        Ok(false)
    }

    fn tick(&mut self) {
        self.tick = self.tick.wrapping_add(1);
    }

    fn start_capture(&mut self) -> io::Result<()> {
        if matches!(self.state, CaptureState::Recording | CaptureState::Paused) {
            self.toast = "Session is already active.".to_string();
            return Ok(());
        }

        let options = StartOptions {
            title: self.title.clone(),
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
            "System audio capture writes audio/call.m4a via CoreAudio process taps.".to_string(),
            "Press e to end and finalize active audio files.".to_string(),
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

    fn toggle_pause(&mut self) {
        if self.mic_recorder.is_some() || self.system_recorder.is_some() {
            self.toast = "Pause is not wired for real recording yet. Press e to end.".to_string();
            return;
        }

        match self.state {
            CaptureState::Recording => {
                if let Some(started_at) = self.started_at.take() {
                    self.accumulated += started_at.elapsed();
                }
                self.state = CaptureState::Paused;
                self.toast = "Paused.".to_string();
            }
            CaptureState::Paused => {
                self.started_at = Some(Instant::now());
                self.state = CaptureState::Recording;
                self.toast = "Recording resumed.".to_string();
            }
            _ => {
                self.toast = "Start a session before pausing.".to_string();
            }
        }
    }

    fn end_capture(&mut self) {
        match self.state {
            CaptureState::Recording => {
                if let Some(started_at) = self.started_at.take() {
                    self.accumulated += started_at.elapsed();
                }
                self.stop_recorders();
                self.state = CaptureState::Ended;
                self.toast =
                    "Session ended. Audio saved under the session audio folder.".to_string();
            }
            CaptureState::Paused => {
                self.stop_recorders();
                self.state = CaptureState::Ended;
                self.toast = "Session ended from paused state.".to_string();
            }
            _ => {
                self.toast = "No active session to end.".to_string();
            }
        }
    }

    fn add_marker(&mut self) {
        if !matches!(self.state, CaptureState::Recording | CaptureState::Paused) {
            self.toast = "Start a session before adding markers.".to_string();
            return;
        }

        let marker = format!("{} marker dropped", self.elapsed_label());
        self.markers.push(marker.clone());
        self.toast = marker;
    }

    fn add_manual_note(&mut self) {
        if !matches!(self.state, CaptureState::Recording | CaptureState::Paused) {
            self.toast = "Start a session before adding notes.".to_string();
            return;
        }

        let note = format!("{} manual note placeholder", self.elapsed_label());
        self.live_notes.push(note.clone());
        self.toast = note;
    }

    fn refresh_sources(&mut self) {
        self.sources = detect_sources();
        self.toast = self.sources.status.clone();
    }

    fn process_mic_events(&mut self) {
        let Some(recorder) = self.mic_recorder.as_mut() else {
            return;
        };

        for event in recorder.drain_events() {
            match event.event_type.as_str() {
                "recording_started" => {
                    let path = event.path.unwrap_or_else(|| "audio/mic.m4a".to_string());
                    self.toast = format!("Mic recording started: {path}");
                }
                "level" => {
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
                    self.mic_recorder = None;
                    self.mic_level_percent = 0;
                    self.mic_level_db = None;
                    break;
                }
                "error" => {
                    self.toast = event
                        .message
                        .unwrap_or_else(|| "Mic recorder reported an error.".to_string());
                    self.mic_recorder = None;
                    self.mic_level_percent = 0;
                    self.mic_level_db = None;
                    break;
                }
                _ => {}
            }
        }
    }

    fn process_system_events(&mut self) {
        let Some(recorder) = self.system_recorder.as_mut() else {
            return;
        };

        for event in recorder.drain_events() {
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
                    self.system_recorder = None;
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
                    self.system_recorder = None;
                    self.call_level_percent = 0;
                    self.call_level_db = None;
                    break;
                }
                _ => {}
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
            CaptureState::Paused => "PAUSED",
            CaptureState::Ended => "ENDED",
        }
    }

    fn status_color(&self) -> Color {
        match self.state {
            CaptureState::Ready => Color::Cyan,
            CaptureState::Recording => Color::Red,
            CaptureState::Paused => Color::Yellow,
            CaptureState::Ended => Color::Green,
        }
    }

    fn meter_value(&self, offset: u64) -> u16 {
        match self.state {
            CaptureState::Recording => {
                let wave = ((self.tick + offset) * 17) % 64;
                26 + wave as u16
            }
            CaptureState::Paused => 8,
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

        for app in self.sources.apps.iter().take(2) {
            items.push(ListItem::new(Line::from(vec![
                Span::styled("App: ", Style::default().fg(Color::Magenta)),
                Span::raw(app),
            ])));
        }

        for microphone in self.sources.microphones.iter().take(2) {
            items.push(ListItem::new(Line::from(vec![
                Span::styled("Mic: ", Style::default().fg(Color::Cyan)),
                Span::raw(microphone),
            ])));
        }

        items.push(ListItem::new(format!("◌ {}", self.sources.status)));
        frame.render_widget(
            List::new(items).block(Block::default().title(" Sources ").borders(Borders::ALL)),
            area,
        );
    }

    fn render_signal(&self, frame: &mut Frame, area: Rect) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),
                Constraint::Length(2),
                Constraint::Length(2),
                Constraint::Min(1),
            ])
            .split(area);

        let block = Block::default().title(" Signal ").borders(Borders::ALL);
        let inner = block.inner(area);
        frame.render_widget(block, area);

        let inner_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(2),
                Constraint::Length(2),
                Constraint::Length(2),
                Constraint::Min(1),
            ])
            .split(inner);

        frame.render_widget(
            signal_gauge("Mic", self.mic_signal_value(), Color::Cyan),
            inner_chunks[0],
        );
        frame.render_widget(
            signal_gauge("Call", self.call_signal_value(), Color::Magenta),
            inner_chunks[1],
        );
        frame.render_widget(
            signal_gauge("Noise", self.meter_value(23) / 3, Color::Green),
            inner_chunks[2],
        );
        let _ = chunks;
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
            Line::raw(""),
        ];

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

    fn render_decisions(&self, frame: &mut Frame, area: Rect) {
        let items = match self.state {
            CaptureState::Ready => vec!["No session yet", "Press c after consent"],
            CaptureState::Recording | CaptureState::Paused => vec![
                "Microphone recording targets audio/mic.m4a",
                "System audio targets audio/call.m4a",
                "Transcript backend pending",
            ],
            CaptureState::Ended => vec!["Audio finalized", "Transcript backend pending"],
        };

        frame.render_widget(
            List::new(items.into_iter().map(ListItem::new).collect::<Vec<_>>())
                .block(Block::default().title(" Decisions ").borders(Borders::ALL)),
            area,
        );
    }

    fn render_actions(&self, frame: &mut Frame, area: Rect) {
        let items = vec![
            "Verify CoreAudio system audio on a live call",
            "Persist markers/notes into session files",
            "Add transcript pipeline after audio capture",
        ];
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
        let help = Line::from(vec![
            Span::styled(
                " Enter ",
                Style::default().fg(Color::Black).bg(Color::Green),
            ),
            Span::raw(" start  "),
            Span::styled(" c ", Style::default().fg(Color::Black).bg(Color::Cyan)),
            Span::raw(" consent  "),
            Span::styled(
                " space/p ",
                Style::default().fg(Color::Black).bg(Color::Yellow),
            ),
            Span::raw(" pause  "),
            Span::styled(" m ", Style::default().fg(Color::Black).bg(Color::Magenta)),
            Span::raw(" marker  "),
            Span::styled(" n ", Style::default().fg(Color::Black).bg(Color::Blue)),
            Span::raw(" note  "),
            Span::styled(" r ", Style::default().fg(Color::Black).bg(Color::Cyan)),
            Span::raw(" refresh  "),
            Span::styled(" e ", Style::default().fg(Color::Black).bg(Color::Red)),
            Span::raw(" end  "),
            Span::styled(" q ", Style::default().fg(Color::Black).bg(Color::Gray)),
            Span::raw(" quit"),
        ]);
        let text = vec![Line::raw(&self.toast), help];
        frame.render_widget(
            Paragraph::new(text).block(Block::default().borders(Borders::ALL)),
            area,
        );
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
