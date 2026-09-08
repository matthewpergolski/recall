use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

use crossterm::event::{
    self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, KeyboardEnhancementFlags, MouseButton,
    MouseEvent, MouseEventKind, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, List, ListItem, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::analysis::{
    analyze, known_agents, maybe_rename_session_dir_for_title, AnalyzeOptions, AnalyzeTarget,
};
use crate::audio::{
    acquire_capture_lock, copy_take1_aliases, next_take_index, prepare_continued_take,
    read_capture_progress, resume_block_reason, write_capture_progress, AudioTrack, CaptureLock,
    CaptureProgress,
};
use crate::capture_sources::{
    detect_sources, read_clipboard_text, write_clipboard_image, ClipboardImageOutcome,
    ClipboardTextOutcome, SourceSummary,
};
use crate::mic_recorder::{clear_mute_mic, set_mute_mic, MicRecorder};
use crate::session::{
    append_session_marker, append_session_note, append_session_note_with_images,
    copy_image_into_session, default_storage_dir, discard_unused_note_images,
    format_session_note_bullet, internal_dir, next_note_image_relative_path, open_path,
    pasted_image_path, primary_document_path, read_session_consent, read_session_title,
    remove_empty_images_dir, resolve_session_target, start_session, ConsentMode, StartOptions,
};
use crate::system_recorder::SystemRecorder;
use crate::transcription::{
    engine_status_parts, format_bytes, format_model_download_label, transcribe_with_progress,
    TrackSelection, TranscribeOptions, TranscribeTarget, TranscriptionEngine,
    TranscriptionProgress, TRANSCRIPTION_CHUNK_SECONDS,
};

const TICK_RATE: Duration = Duration::from_millis(100);
const SOURCE_REFRESH_TICKS: u64 = 50;
const MAX_NOTE_PASTE_CHARS: usize = 4096;

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
    // Negotiate keys in-app (Ghostty/kitty protocol) so Shift+Enter and Cmd+V
    // work without a separate /terminal-setup step. Ignore terminals that refuse.
    let _ = execute!(
        io::stdout(),
        EnableBracketedPaste,
        PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES
        ),
    );
    let result = App::new(options, resume)?.run(&mut terminal);
    let _ = execute!(
        io::stdout(),
        PopKeyboardEnhancementFlags,
        DisableMouseCapture,
        DisableBracketedPaste
    );
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
    mic_muted: bool,
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
    note_draft: Option<NoteDraft>,
    note_footer: Rect,
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

const NOTE_LABEL: &str = " Note >";
const NOTE_GAP: &str = " ";
const NOTE_CONT: &str = "        ";
const NOTE_PREVIEW_LINES: usize = 3;

fn note_prefix_width() -> u16 {
    (UnicodeWidthStr::width(NOTE_LABEL) + UnicodeWidthStr::width(NOTE_GAP)) as u16
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum NotePiece {
    Text(String),
    Image(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NoteDraft {
    pieces: Vec<NotePiece>,
    caret_piece: usize,
    /// Char offset in a text piece, or `0` before / `1` after an image chip.
    caret_offset: usize,
}

impl Default for NoteDraft {
    fn default() -> Self {
        Self {
            pieces: vec![NotePiece::Text(String::new())],
            caret_piece: 0,
            caret_offset: 0,
        }
    }
}

#[derive(Debug, Clone)]
struct NoteRun {
    piece: usize,
    start_offset: usize,
    text: String,
    image: bool,
    chip_index: usize,
}

impl NoteDraft {
    fn char_len(text: &str) -> usize {
        text.chars().count()
    }

    fn display_width(text: &str) -> usize {
        UnicodeWidthStr::width(text)
    }

    fn char_index_at_cells(text: &str, target_cells: usize) -> usize {
        let mut cells = 0;
        let mut index = 0;
        for ch in text.chars() {
            let width = UnicodeWidthChar::width(ch).unwrap_or(0);
            if cells + width > target_cells {
                return index;
            }
            cells += width;
            index += 1;
            if cells == target_cells {
                return index;
            }
        }
        index
    }

    fn chip_label(index: usize) -> String {
        format!("[Image #{}]", index + 1)
    }

    fn caption(&self) -> String {
        let mut out = String::new();
        for piece in &self.pieces {
            if let NotePiece::Text(text) = piece {
                out.push_str(text);
            }
        }
        out
    }

    fn images(&self) -> Vec<String> {
        self.pieces
            .iter()
            .filter_map(|piece| match piece {
                NotePiece::Image(path) => Some(path.clone()),
                NotePiece::Text(_) => None,
            })
            .collect()
    }

    fn is_empty(&self) -> bool {
        self.caption().trim().is_empty() && self.images().is_empty()
    }

    fn clamp_caret(&mut self) {
        if self.pieces.is_empty() {
            self.pieces.push(NotePiece::Text(String::new()));
        }
        self.caret_piece = self.caret_piece.min(self.pieces.len() - 1);
        match &self.pieces[self.caret_piece] {
            NotePiece::Text(text) => {
                self.caret_offset = self.caret_offset.min(Self::char_len(text));
            }
            NotePiece::Image(_) => {
                self.caret_offset = self.caret_offset.min(1);
            }
        }
    }

    fn merge_text(&mut self) {
        let mut merged: Vec<NotePiece> = Vec::new();
        let (caret_piece, caret_offset) = (self.caret_piece, self.caret_offset);
        let mut new_caret_piece = 0;
        let mut new_caret_offset = caret_offset;
        let mut mapped = false;
        for (index, piece) in self.pieces.drain(..).enumerate() {
            let is_caret = index == caret_piece;
            let merge_into_prev = matches!(
                (merged.last(), &piece),
                (Some(NotePiece::Text(_)), NotePiece::Text(_))
            );
            if merge_into_prev {
                let dest = merged.len() - 1;
                let NotePiece::Text(left) = merged.last_mut().unwrap() else {
                    unreachable!("merge_into_prev requires trailing text");
                };
                let left_len = Self::char_len(left);
                let NotePiece::Text(right) = piece else {
                    unreachable!("merge_into_prev requires incoming text");
                };
                left.push_str(&right);
                if is_caret {
                    new_caret_piece = dest;
                    new_caret_offset = left_len + caret_offset;
                    mapped = true;
                }
            } else {
                if is_caret {
                    new_caret_piece = merged.len();
                    new_caret_offset = caret_offset;
                    mapped = true;
                }
                merged.push(piece);
            }
        }
        if merged.is_empty() {
            merged.push(NotePiece::Text(String::new()));
        }
        self.pieces = merged;
        if mapped {
            self.caret_piece = new_caret_piece;
            self.caret_offset = new_caret_offset;
        }
        self.clamp_caret();
    }

    fn insert_str(&mut self, text: &str) {
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        if normalized.is_empty() {
            return;
        }
        self.clamp_caret();
        match &self.pieces[self.caret_piece] {
            NotePiece::Image(_) if self.caret_offset >= 1 => {
                let next = self.caret_piece + 1;
                if matches!(self.pieces.get(next), Some(NotePiece::Text(_))) {
                    self.caret_piece = next;
                    self.caret_offset = 0;
                } else {
                    self.pieces.insert(next, NotePiece::Text(String::new()));
                    self.caret_piece = next;
                    self.caret_offset = 0;
                }
            }
            NotePiece::Image(_) => {
                self.pieces
                    .insert(self.caret_piece, NotePiece::Text(String::new()));
                self.caret_offset = 0;
            }
            NotePiece::Text(_) => {}
        }
        if let NotePiece::Text(existing) = &mut self.pieces[self.caret_piece] {
            let mut chars = existing.chars();
            let left: String = chars.by_ref().take(self.caret_offset).collect();
            let right: String = chars.collect();
            let inserted = Self::char_len(&normalized);
            *existing = format!("{left}{normalized}{right}");
            self.caret_offset += inserted;
        }
        self.merge_text();
    }

    fn insert_char(&mut self, ch: char) {
        self.insert_str(&ch.to_string());
    }

    fn insert_image(&mut self, path: String) {
        self.clamp_caret();
        match &self.pieces[self.caret_piece] {
            NotePiece::Text(text) => {
                let mut chars = text.chars();
                let left: String = chars.by_ref().take(self.caret_offset).collect();
                let right: String = chars.collect();
                let idx = self.caret_piece;
                self.pieces[idx] = NotePiece::Text(left);
                self.pieces.insert(idx + 1, NotePiece::Image(path));
                self.pieces.insert(idx + 2, NotePiece::Text(right));
                self.caret_piece = idx + 1;
                self.caret_offset = 1;
            }
            NotePiece::Image(_) => {
                let idx = if self.caret_offset >= 1 {
                    self.caret_piece + 1
                } else {
                    self.caret_piece
                };
                self.pieces.insert(idx, NotePiece::Image(path));
                self.caret_piece = idx;
                self.caret_offset = 1;
            }
        }
        self.merge_text();
    }

    fn backspace(&mut self) -> bool {
        self.clamp_caret();
        match &self.pieces[self.caret_piece] {
            NotePiece::Image(_) if self.caret_offset >= 1 => false,
            NotePiece::Image(_) => {
                if self.caret_piece == 0 {
                    return false;
                }
                self.caret_piece -= 1;
                match &self.pieces[self.caret_piece] {
                    NotePiece::Text(text) => self.caret_offset = Self::char_len(text),
                    NotePiece::Image(_) => self.caret_offset = 1,
                }
                self.backspace()
            }
            NotePiece::Text(text) if self.caret_offset > 0 => {
                let mut chars = text.chars();
                let mut left: String = chars.by_ref().take(self.caret_offset - 1).collect();
                chars.next();
                left.push_str(&chars.collect::<String>());
                self.pieces[self.caret_piece] = NotePiece::Text(left);
                self.caret_offset -= 1;
                self.merge_text();
                true
            }
            NotePiece::Text(_) if self.caret_piece > 0 => {
                self.caret_piece -= 1;
                match &self.pieces[self.caret_piece] {
                    NotePiece::Text(text) => {
                        self.caret_offset = Self::char_len(text);
                        self.backspace()
                    }
                    NotePiece::Image(_) => {
                        self.caret_offset = 1;
                        false
                    }
                }
            }
            NotePiece::Text(_) => false,
        }
    }

    fn take_chip_at_caret(&mut self) -> Option<String> {
        self.clamp_caret();
        let NotePiece::Image(path) = &self.pieces[self.caret_piece] else {
            return None;
        };
        if self.caret_offset == 0 {
            return None;
        }
        let path = path.clone();
        self.pieces.remove(self.caret_piece);
        if self.pieces.is_empty() {
            self.pieces.push(NotePiece::Text(String::new()));
        }
        if self.caret_piece >= self.pieces.len() {
            self.caret_piece = self.pieces.len() - 1;
        }
        match &self.pieces[self.caret_piece] {
            NotePiece::Text(_) => self.caret_offset = 0,
            NotePiece::Image(_) => self.caret_offset = 1,
        }
        self.merge_text();
        Some(path)
    }

    fn delete_forward(&mut self) -> Option<String> {
        self.clamp_caret();
        match &self.pieces[self.caret_piece] {
            NotePiece::Text(text) if self.caret_offset < Self::char_len(text) => {
                let mut chars = text.chars();
                let mut left: String = chars.by_ref().take(self.caret_offset).collect();
                chars.next();
                left.push_str(&chars.collect::<String>());
                self.pieces[self.caret_piece] = NotePiece::Text(left);
                self.merge_text();
                None
            }
            NotePiece::Image(_) if self.caret_offset == 0 => self.take_chip_at_caret_before(),
            _ if self.caret_piece + 1 < self.pieces.len() => {
                self.caret_piece += 1;
                self.caret_offset = 0;
                match &self.pieces[self.caret_piece] {
                    NotePiece::Image(_) => self.take_chip_at_caret_before(),
                    NotePiece::Text(_) => {
                        let _ = self.delete_forward();
                        None
                    }
                }
            }
            _ => None,
        }
    }

    fn take_chip_at_caret_before(&mut self) -> Option<String> {
        self.clamp_caret();
        let NotePiece::Image(path) = &self.pieces[self.caret_piece] else {
            return None;
        };
        let path = path.clone();
        self.pieces.remove(self.caret_piece);
        if self.pieces.is_empty() {
            self.pieces.push(NotePiece::Text(String::new()));
        }
        if self.caret_piece >= self.pieces.len() {
            self.caret_piece = self.pieces.len() - 1;
        }
        self.caret_offset = 0;
        self.merge_text();
        Some(path)
    }

    fn move_left(&mut self) {
        self.clamp_caret();
        match &self.pieces[self.caret_piece] {
            NotePiece::Text(_) if self.caret_offset > 0 => self.caret_offset -= 1,
            NotePiece::Image(_) if self.caret_offset >= 1 => self.caret_offset = 0,
            _ if self.caret_piece > 0 => {
                self.caret_piece -= 1;
                match &self.pieces[self.caret_piece] {
                    NotePiece::Text(text) => self.caret_offset = Self::char_len(text),
                    NotePiece::Image(_) => self.caret_offset = 1,
                }
            }
            _ => {}
        }
    }

    fn move_right(&mut self) {
        self.clamp_caret();
        match &self.pieces[self.caret_piece] {
            NotePiece::Text(text) if self.caret_offset < Self::char_len(text) => {
                self.caret_offset += 1;
            }
            NotePiece::Image(_) if self.caret_offset == 0 => self.caret_offset = 1,
            _ if self.caret_piece + 1 < self.pieces.len() => {
                self.caret_piece += 1;
                self.caret_offset = 0;
            }
            _ => {}
        }
    }

    fn visual_lines(&self) -> Vec<Vec<NoteRun>> {
        let mut lines: Vec<Vec<NoteRun>> = vec![Vec::new()];
        let mut chip = 0usize;
        for (index, piece) in self.pieces.iter().enumerate() {
            match piece {
                NotePiece::Text(text) => {
                    let parts: Vec<&str> = text.split('\n').collect();
                    for (part_index, part) in parts.iter().enumerate() {
                        if part_index > 0 {
                            lines.push(Vec::new());
                        }
                        let start = if part_index == 0 {
                            0
                        } else {
                            parts
                                .iter()
                                .take(part_index)
                                .map(|p| Self::char_len(p) + 1)
                                .sum()
                        };
                        // Keep empty lines (trailing Shift+Enter) so the caret
                        // can sit on the new row before the next character.
                        lines.last_mut().unwrap().push(NoteRun {
                            piece: index,
                            start_offset: start,
                            text: (*part).to_string(),
                            image: false,
                            chip_index: 0,
                        });
                    }
                }
                NotePiece::Image(_) => {
                    lines.last_mut().unwrap().push(NoteRun {
                        piece: index,
                        start_offset: 0,
                        text: format!(" {}", Self::chip_label(chip)),
                        image: true,
                        chip_index: chip,
                    });
                    chip += 1;
                }
            }
        }
        if lines.is_empty() {
            lines.push(Vec::new());
        }
        lines
    }

    fn caret_line_cells(&self) -> (usize, usize) {
        let lines = self.visual_lines();
        for (line_index, runs) in lines.iter().enumerate() {
            let mut cells = 0;
            for run in runs {
                if run.piece != self.caret_piece {
                    cells += Self::display_width(&run.text);
                    continue;
                }
                if run.image {
                    if self.caret_offset >= 1 {
                        return (line_index, cells + Self::display_width(&run.text));
                    }
                    return (line_index, cells);
                }
                let local = self.caret_offset.saturating_sub(run.start_offset);
                if self.caret_offset < run.start_offset {
                    continue;
                }
                let run_len = Self::char_len(&run.text);
                if local > run_len {
                    cells += Self::display_width(&run.text);
                    continue;
                }
                let prefix: String = run.text.chars().take(local).collect();
                return (line_index, cells + Self::display_width(&prefix));
            }
        }
        (0, 0)
    }

    #[cfg(test)]
    fn caret_cell_col(&self) -> usize {
        self.caret_line_cells().1
    }

    fn click_to_cursor(&mut self, line: usize, cells: usize) {
        let lines = self.visual_lines();
        if lines.is_empty() {
            return;
        }
        let line = line.min(lines.len() - 1);
        let mut remaining = cells;
        for run in &lines[line] {
            let width = Self::display_width(&run.text);
            if remaining > width {
                remaining -= width;
                continue;
            }
            if run.image {
                self.caret_piece = run.piece;
                self.caret_offset = 1;
                return;
            }
            let index = Self::char_index_at_cells(&run.text, remaining);
            self.caret_piece = run.piece;
            self.caret_offset = run.start_offset + index;
            return;
        }
        if let Some(run) = lines[line].last() {
            self.caret_piece = run.piece;
            self.caret_offset = if run.image {
                1
            } else {
                run.start_offset + Self::char_len(&run.text)
            };
        }
    }

    fn first_visible_line(&self, max: usize) -> usize {
        let total = self.visual_lines().len();
        if total <= max {
            return 0;
        }
        let (line, _) = self.caret_line_cells();
        let max_first = total - max;
        line.saturating_sub(max.saturating_sub(1)).min(max_first)
    }

    fn move_line_start(&mut self) {
        let (line, _) = self.caret_line_cells();
        self.click_to_cursor(line, 0);
    }

    fn move_line_end(&mut self) {
        let (line, _) = self.caret_line_cells();
        self.click_to_cursor(line, usize::MAX / 4);
    }

    fn move_up(&mut self) {
        let (line, cells) = self.caret_line_cells();
        if line == 0 {
            self.click_to_cursor(0, 0);
            return;
        }
        self.click_to_cursor(line - 1, cells);
    }

    fn move_down(&mut self) {
        let (line, cells) = self.caret_line_cells();
        let last = self.visual_lines().len().saturating_sub(1);
        if line >= last {
            self.click_to_cursor(last, usize::MAX / 4);
            return;
        }
        self.click_to_cursor(line + 1, cells);
    }

    fn preview_runs(&self, max: usize) -> Vec<Vec<NoteRun>> {
        let first = self.first_visible_line(max);
        self.visual_lines()
            .into_iter()
            .skip(first)
            .take(max)
            .collect()
    }

    #[cfg(test)]
    fn caption_cursor(&self) -> usize {
        let mut chars = 0;
        for (index, piece) in self.pieces.iter().enumerate() {
            match piece {
                NotePiece::Text(text) => {
                    if index == self.caret_piece {
                        return chars + self.caret_offset;
                    }
                    chars += Self::char_len(text);
                }
                NotePiece::Image(_) if index == self.caret_piece => return chars,
                NotePiece::Image(_) => {}
            }
        }
        chars
    }

    #[cfg(test)]
    fn set_caption_cursor(&mut self, target: usize) {
        let mut chars = 0;
        for (index, piece) in self.pieces.iter().enumerate() {
            if let NotePiece::Text(text) = piece {
                let len = Self::char_len(text);
                if target <= chars + len {
                    self.caret_piece = index;
                    self.caret_offset = target - chars;
                    return;
                }
                chars += len;
            }
        }
        self.clamp_caret();
    }

    #[cfg(test)]
    fn caret_after_chip(&self) -> Option<usize> {
        match self.pieces.get(self.caret_piece) {
            Some(NotePiece::Image(_)) if self.caret_offset >= 1 => {
                let chip = self.pieces[..self.caret_piece]
                    .iter()
                    .filter(|piece| matches!(piece, NotePiece::Image(_)))
                    .count();
                Some(chip)
            }
            _ => None,
        }
    }
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
            mic_muted: false,
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
            note_footer: Rect::default(),
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
            "Resumed {id}. Enter appends take {next_take}. Clock continues from {clock} (break not added)."
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
            "Enter records another take in this session.".to_string(),
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
                match event::read()? {
                    Event::Key(key)
                        if key.kind == KeyEventKind::Press && self.handle_key(key)? =>
                    {
                        return Ok(TuiExit {
                            session_path: self.session_path.clone(),
                            detached_logs: self.detached_logs.clone(),
                        });
                    }
                    Event::Paste(text) if self.note_draft.is_some() => {
                        self.handle_terminal_paste(&text);
                    }
                    Event::Mouse(mouse) if self.note_draft.is_some() => {
                        self.handle_note_mouse(mouse);
                    }
                    _ => {}
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
            KeyCode::Enter => self.primary_recording_action()?,
            KeyCode::Char(' ') => self.toggle_mic_mute()?,
            KeyCode::Char('c') => self.toggle_consent(),
            KeyCode::Char('s') => self.toggle_append_next(),
            KeyCode::Char('p') => {
                self.toast =
                    "Pause is disabled for real recording. Press Enter to end.".to_string();
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

    fn toggle_mic_mute(&mut self) -> io::Result<()> {
        if !matches!(self.state, CaptureState::Recording) {
            self.toast = "Start recording before muting the mic.".to_string();
            return Ok(());
        }

        let elapsed = self.elapsed_label();
        if self.mic_muted {
            self.set_mic_muted(false)?;
            self.toast = "Mic unmuted.".to_string();
            self.record_mute_note(&elapsed, "Mic unmuted");
        } else {
            self.set_mic_muted(true)?;
            self.toast = "Mic muted for Recall. Call audio still recording. You are not muted in Zoom/Teams."
                .to_string();
            self.record_mute_note(&elapsed, "Mic muted for Recall");
        }
        Ok(())
    }

    fn record_mute_note(&mut self, elapsed: &str, caption: &str) {
        let live = format!("{caption} at `{elapsed}`.");
        if let Some(session_path) = &self.session_path {
            if let Err(error) = append_session_note(session_path, elapsed, caption) {
                self.live_notes.push(live);
                self.toast = format!("Mute noted in memory, but failed to save: {error}");
                return;
            }
        }
        self.live_notes.push(live);
    }

    fn set_mic_muted(&mut self, muted: bool) -> io::Result<()> {
        self.mic_muted = muted;
        if muted {
            self.mic_level_percent = 0;
            self.mic_level_db = Some(f32::NEG_INFINITY);
        }
        if let Some(recorder) = &self.mic_recorder {
            recorder.set_muted(muted)?;
        } else if let Some(path) = &self.session_path {
            set_mute_mic(path, muted)?;
        }
        Ok(())
    }

    fn clear_mic_mute(&mut self) {
        self.mic_muted = false;
        if let Some(path) = &self.session_path {
            clear_mute_mic(path);
        }
    }

    fn handle_note_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter
                if key
                    .modifiers
                    .intersects(KeyModifiers::SHIFT | KeyModifiers::ALT) =>
            {
                self.insert_note_newline();
            }
            KeyCode::Enter => self.save_manual_note(),
            KeyCode::Esc => self.cancel_manual_note(),
            KeyCode::Backspace => self.backspace_note_draft(),
            KeyCode::Delete => self.delete_note_forward(),
            KeyCode::Left => {
                if let Some(draft) = &mut self.note_draft {
                    draft.move_left();
                }
            }
            KeyCode::Right => {
                if let Some(draft) = &mut self.note_draft {
                    draft.move_right();
                }
            }
            KeyCode::Up => {
                if let Some(draft) = &mut self.note_draft {
                    draft.move_up();
                }
            }
            KeyCode::Down => {
                if let Some(draft) = &mut self.note_draft {
                    draft.move_down();
                }
            }
            KeyCode::Home => {
                if let Some(draft) = &mut self.note_draft {
                    draft.move_line_start();
                }
            }
            KeyCode::End => {
                if let Some(draft) = &mut self.note_draft {
                    draft.move_line_end();
                }
            }
            KeyCode::Tab => self.paste_clipboard_image(false),
            KeyCode::Char('\n') | KeyCode::Char('\r') => self.insert_note_newline(),
            KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.insert_note_newline();
            }
            KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(draft) = &mut self.note_draft {
                    draft.move_line_start();
                }
            }
            KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(draft) = &mut self.note_draft {
                    draft.move_line_end();
                }
            }
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.cancel_manual_note();
            }
            KeyCode::Char('v')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    || key.modifiers.contains(KeyModifiers::SUPER) =>
            {
                self.paste_clipboard_image(true);
            }
            KeyCode::Char('i') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.paste_clipboard_image(false);
            }
            KeyCode::Char(ch)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                if let Some(draft) = &mut self.note_draft {
                    draft.insert_char(ch);
                }
            }
            _ => {}
        }
    }

    fn insert_note_newline(&mut self) {
        if let Some(draft) = &mut self.note_draft {
            draft.insert_char('\n');
        }
    }

    fn handle_terminal_paste(&mut self, text: &str) {
        // Ghostty Cmd+V is often a paste event, not Super+V. Image clipboards
        // may arrive as empty paste; still read NSPasteboard.
        if self.try_attach_clipboard_image() {
            return;
        }
        if text.is_empty() {
            return;
        }
        self.handle_note_paste(text);
    }

    fn handle_note_paste(&mut self, text: &str) {
        if text.chars().count() > MAX_NOTE_PASTE_CHARS || paste_looks_binary(text) {
            self.toast = "Ignored a large clipboard paste.".to_string();
            return;
        }
        if let Some(path) = pasted_image_path(text) {
            self.import_note_image_from_path(&path);
            return;
        }
        if let Some(draft) = &mut self.note_draft {
            draft.insert_str(text);
        }
    }

    fn handle_note_mouse(&mut self, mouse: MouseEvent) {
        if !matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            return;
        }
        let area = self.note_footer;
        if area.width < 3 || area.height < 3 {
            return;
        }
        let inner_x = area.x.saturating_add(1);
        let inner_y = area.y.saturating_add(1);
        let inner_width = area.width.saturating_sub(2);
        let inner_height = area.height.saturating_sub(2);
        if mouse.column < inner_x
            || mouse.row < inner_y
            || mouse.column >= inner_x.saturating_add(inner_width)
            || mouse.row >= inner_y.saturating_add(inner_height)
        {
            return;
        }
        let row = (mouse.row - inner_y) as usize;
        if row >= NOTE_PREVIEW_LINES {
            return;
        }
        let Some(draft) = self.note_draft.as_mut() else {
            return;
        };
        let first = draft.first_visible_line(NOTE_PREVIEW_LINES);
        let caption_line = first + row;
        let prefix = if row == 0 {
            note_prefix_width()
        } else {
            UnicodeWidthStr::width(NOTE_CONT) as u16
        };
        let col = if mouse.column <= inner_x + prefix {
            0
        } else {
            (mouse.column - inner_x - prefix) as usize
        };
        draft.click_to_cursor(caption_line, col);
    }

    fn try_attach_clipboard_image(&mut self) -> bool {
        let Some(session_path) = self.session_path.clone() else {
            return false;
        };
        if self.note_draft.is_none() {
            return false;
        }
        let elapsed = self.elapsed_label();
        let relative = next_note_image_relative_path(&session_path, &elapsed, "png");
        let dest = session_path.join(&relative);
        match write_clipboard_image(&dest) {
            Ok(ClipboardImageOutcome::Saved(path)) => {
                let relative = path
                    .strip_prefix(&session_path)
                    .ok()
                    .map(|value| value.to_string_lossy().replace('\\', "/"))
                    .filter(|value| !value.is_empty())
                    .unwrap_or(relative);
                self.attach_note_image(relative);
                true
            }
            Ok(ClipboardImageOutcome::NoImage) => {
                let _ = fs::remove_file(&dest);
                let _ = remove_empty_images_dir(&session_path);
                false
            }
            Err(error) => {
                let _ = fs::remove_file(&dest);
                let _ = remove_empty_images_dir(&session_path);
                self.toast = format!("Could not paste image: {error}");
                false
            }
        }
    }

    fn paste_clipboard_image(&mut self, allow_text: bool) {
        if self.try_attach_clipboard_image() {
            return;
        }
        if allow_text {
            match read_clipboard_text() {
                Ok(ClipboardTextOutcome::Text(text)) => {
                    self.handle_note_paste(&text);
                    return;
                }
                Ok(ClipboardTextOutcome::NoText) => {}
                Err(error) => {
                    self.toast = format!("Could not paste clipboard text: {error}");
                    return;
                }
            }
        }
        self.toast = "Clipboard has no image. Copy a screenshot first.".to_string();
    }

    fn import_note_image_from_path(&mut self, source: &Path) {
        let Some(session_path) = self.session_path.clone() else {
            return;
        };
        if self.note_draft.is_none() {
            return;
        }
        let elapsed = self.elapsed_label();
        match copy_image_into_session(&session_path, &elapsed, source) {
            Ok(relative) => self.attach_note_image(relative),
            Err(error) => {
                self.toast = format!("Could not attach image: {error}");
            }
        }
    }

    fn attach_note_image(&mut self, relative: String) {
        if let Some(draft) = &mut self.note_draft {
            draft.insert_image(relative.clone());
            let number = draft.images().len().saturating_sub(1);
            self.toast = format!("{} saved as {relative}", NoteDraft::chip_label(number));
        }
    }

    fn delete_note_forward(&mut self) {
        let Some(draft) = self.note_draft.as_mut() else {
            return;
        };
        let Some(relative) = draft.delete_forward() else {
            return;
        };
        if let Some(session_path) = &self.session_path {
            let _ = discard_unused_note_images(session_path, std::slice::from_ref(&relative));
        }
    }

    fn backspace_note_draft(&mut self) {
        let relative = {
            let Some(draft) = self.note_draft.as_mut() else {
                return;
            };
            if let Some(relative) = draft.take_chip_at_caret() {
                relative
            } else if draft.backspace() {
                return;
            } else if let Some(relative) = draft.take_chip_at_caret() {
                relative
            } else {
                return;
            }
        };
        if let Some(session_path) = &self.session_path {
            let _ = discard_unused_note_images(session_path, std::slice::from_ref(&relative));
        }
    }

    fn set_note_mouse_capture(enable: bool) {
        if enable {
            let _ = execute!(io::stdout(), EnableMouseCapture);
        } else {
            let _ = execute!(io::stdout(), DisableMouseCapture);
        }
    }

    fn cancel_manual_note(&mut self) {
        Self::set_note_mouse_capture(false);
        let draft = self.note_draft.take();
        if let (Some(session_path), Some(draft)) = (self.session_path.clone(), draft) {
            let images = draft.images();
            if !images.is_empty() {
                if let Err(error) = discard_unused_note_images(&session_path, &images) {
                    self.toast = format!("Note cancelled, but unused images remain: {error}");
                    return;
                }
            }
        }
        self.toast = "Note cancelled.".to_string();
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
        self.clear_mic_mute();
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
            "Press Enter to end and start transcription.".to_string(),
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
        self.clear_mic_mute();
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
            "Next Enter will append another take to this session.".to_string()
        } else {
            "Next Enter will start a new session folder.".to_string()
        };
        if matches!(self.state, CaptureState::Ended) {
            self.replace_next_action_notes();
        }
    }

    fn replace_next_action_notes(&mut self) {
        self.live_notes.retain(|note| {
            !note.contains("Enter continues this session")
                && !note.contains("Enter starts a new session")
                && !note.contains("q leaves (next start is a new session)")
                && !note.contains("q leaves.")
        });
        self.live_notes.push(if self.append_next {
            "Audio finalized. Enter continues this session.".to_string()
        } else {
            "Audio finalized. Enter starts a new session folder.".to_string()
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
                self.clear_mic_mute();
                self.ended_at = Some(Instant::now());
                self.state = CaptureState::Ended;
                self.completed_take = self.take_index.max(1);
                self.live_notes
                    .retain(|note| !note.contains("Press Enter to end"));
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

        // Opening `n` must not read the pasteboard. Paste is explicit (Cmd+V / Ctrl+I / Paste).
        self.note_draft = Some(NoteDraft::default());
        Self::set_note_mouse_capture(true);
        self.toast =
            "Click or use arrows to move. Enter saves, Ctrl+J or Shift+Enter newline, Cmd+V pastes."
                .to_string();
    }

    fn save_manual_note(&mut self) {
        let Some(draft) = self.note_draft.take() else {
            return;
        };
        Self::set_note_mouse_capture(false);
        if draft.is_empty() {
            self.toast = "Empty note discarded.".to_string();
            return;
        }

        let caption = draft.caption();
        let caption = caption.trim();
        let images = draft.images();
        let elapsed = self.elapsed_label();
        let Some(session_path) = self.session_path.clone() else {
            self.toast = "Start or resume a session before adding notes.".to_string();
            return;
        };
        let save_result = if images.is_empty() {
            append_session_note(&session_path, &elapsed, caption)
        } else {
            append_session_note_with_images(&session_path, &elapsed, caption, &images)
        };
        if let Err(error) = save_result {
            self.note_draft = Some(draft);
            Self::set_note_mouse_capture(true);
            self.toast = format!("Note created in memory, but failed to save: {error}");
            return;
        }

        let live = format_session_note_bullet(&elapsed, caption, &images)
            .map(|note| {
                let body = note.strip_prefix("- ").unwrap_or(&note);
                match body.split_once('\n') {
                    Some((first, _)) => format!("{first} …"),
                    None => body.to_string(),
                }
            })
            .unwrap_or_else(|| format!("`{elapsed}` {caption}"));
        self.live_notes.push(live.clone());
        self.toast = format!("{live} saved with the session");
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
                    if self.mic_muted {
                        self.mic_level_db = Some(f32::NEG_INFINITY);
                        self.mic_level_percent = 0;
                    } else if let Some(level_db) = event.level_db {
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
                    self.clear_mic_mute();
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
                    self.clear_mic_mute();
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
                    self.clear_mic_mute();
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
        self.clear_mic_mute();
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

    fn render(&mut self, frame: &mut Frame) {
        let area = frame.area();
        let footer_height = if self.note_draft.is_some() { 6 } else { 3 };
        let main = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Length(7),
                Constraint::Min(5),
                Constraint::Length(6),
                Constraint::Length(footer_height),
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
        let mut spans = vec![
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
        ];
        if self.mic_muted && matches!(self.state, CaptureState::Recording) {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(
                "MIC MUTED",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        spans.extend([
            Span::raw("  "),
            Span::styled(consent, Style::default().fg(Color::Gray)),
            Span::raw("  "),
            Span::styled(next, Style::default().fg(Color::Gray)),
        ]);
        let title = Line::from(spans);
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
        if self.mic_muted && matches!(self.state, CaptureState::Recording) {
            return "MIC MUTED for Recall".to_string();
        }

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
        if self.mic_muted {
            0
        } else if self.mic_recorder.is_some() {
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
        if (self.mic_muted && matches!(self.state, CaptureState::Recording))
            || self.mic_capture_warning.is_some()
        {
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
            let preview = draft.preview_runs(2);
            let last = preview.len().saturating_sub(1);
            for (index, runs) in preview.into_iter().enumerate() {
                let mut spans = if index == 0 {
                    vec![Span::styled(
                        "Note draft: ",
                        Style::default().fg(Color::Blue),
                    )]
                } else {
                    vec![Span::raw("            ")]
                };
                for run in runs {
                    if run.image {
                        spans.push(Span::styled(
                            run.text,
                            Style::default().fg(Color::Black).bg(Color::Magenta),
                        ));
                    } else {
                        spans.push(Span::raw(run.text));
                    }
                }
                if index == last {
                    spans.push(Span::styled("_", Style::default().fg(Color::Blue)));
                }
                lines.push(Line::from(spans));
            }
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
        let mic_state = if self.mic_muted && matches!(self.state, CaptureState::Recording) {
            Span::styled(
                "MIC MUTED for Recall (call still recording)",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
        } else if let Some(warning) = &self.mic_capture_warning {
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
                "Press Enter to start".to_string(),
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
                if self.mic_muted {
                    "MIC MUTED for Recall. Space unmutes. Enter ends.".to_string()
                } else {
                    "Enter ends and starts transcription. Space mutes Recall mic.".to_string()
                },
                format!("Analysis agent: {}", self.agent_label()),
            ],
            CaptureState::Ended => vec![
                if self.resumed {
                    "Resumed previous session".to_string()
                } else {
                    "Audio finalized".to_string()
                },
                if self.append_next {
                    "Enter records another take in this session".to_string()
                } else {
                    "Enter starts a new session folder".to_string()
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

    fn render_footer(&mut self, frame: &mut Frame, area: Rect) {
        if let Some(draft) = &self.note_draft {
            self.note_footer = area;
            let first = draft.first_visible_line(NOTE_PREVIEW_LINES);
            let (caret_line, caret_cells) = draft.caret_line_cells();
            let preview = draft.preview_runs(NOTE_PREVIEW_LINES);
            let mut text = Vec::new();
            for (index, runs) in preview.into_iter().enumerate() {
                let mut spans = Vec::new();
                if index == 0 {
                    spans.push(Span::styled(
                        NOTE_LABEL,
                        Style::default().fg(Color::Black).bg(Color::Blue),
                    ));
                    spans.push(Span::raw(NOTE_GAP));
                } else {
                    spans.push(Span::raw(NOTE_CONT));
                }
                for run in runs {
                    if run.image {
                        spans.push(Span::raw(" "));
                        spans.push(Span::styled(
                            NoteDraft::chip_label(run.chip_index),
                            Style::default().fg(Color::Black).bg(Color::Magenta),
                        ));
                    } else {
                        spans.push(Span::raw(run.text));
                    }
                }
                text.push(Line::from(spans));
            }
            text.push(Line::from(vec![
                Span::styled(
                    " Enter ",
                    Style::default().fg(Color::Black).bg(Color::Green),
                ),
                Span::raw(" save  "),
                Span::styled(
                    " Ctrl+J ",
                    Style::default().fg(Color::Black).bg(Color::Cyan),
                ),
                Span::raw(" newline  "),
                Span::styled(" Esc ", Style::default().fg(Color::Black).bg(Color::Gray)),
                Span::raw(" cancel  "),
                Span::styled(
                    " Cmd+V ",
                    Style::default().fg(Color::Black).bg(Color::Magenta),
                ),
                Span::raw(" paste  click to move"),
            ]));
            frame.render_widget(
                Paragraph::new(text).block(Block::default().borders(Borders::ALL)),
                area,
            );
            if caret_line >= first && caret_line < first + NOTE_PREVIEW_LINES {
                let row = (caret_line - first) as u16;
                let prefix = if row == 0 {
                    note_prefix_width()
                } else {
                    UnicodeWidthStr::width(NOTE_CONT) as u16
                };
                let x = area
                    .x
                    .saturating_add(1)
                    .saturating_add(prefix)
                    .saturating_add(caret_cells as u16);
                let y = area.y.saturating_add(1).saturating_add(row);
                let max_x = area.x.saturating_add(area.width.saturating_sub(2));
                frame.set_cursor_position(Position::new(x.min(max_x), y));
            }
            return;
        }

        let help = Line::from(vec![
            Span::styled(
                " Enter ",
                Style::default().fg(Color::Black).bg(Color::Green),
            ),
            Span::raw(" start/end/continue  "),
            Span::styled(
                " Space ",
                Style::default().fg(Color::Black).bg(Color::Yellow),
            ),
            Span::raw(" mute  "),
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

fn paste_looks_binary(text: &str) -> bool {
    text.chars().any(|ch| ch == '\0')
        || text
            .chars()
            .filter(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t'))
            .count()
            > 8
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
            mic_muted: false,
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
            note_footer: Rect::default(),
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
    fn resume_opens_ended_session_and_enter_continues_the_same_folder() {
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

    fn unique_storage(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "recall-tui-note-image-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ))
    }

    fn tiny_png() -> Vec<u8> {
        vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ]
    }

    fn type_note(app: &mut App, text: &str) {
        for ch in text.chars() {
            app.handle_note_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
    }

    fn note_session(label: &str) -> (PathBuf, PathBuf) {
        let storage = unique_storage(label);
        let session = start_session(&StartOptions {
            title: "Note Images".to_string(),
            consent: ConsentMode::Noted,
            storage_dir: storage.clone(),
        })
        .unwrap();
        (storage, session.path)
    }

    #[test]
    fn opening_a_note_does_not_paste_or_create_images() {
        let (storage, session_path) = note_session("open-empty");
        let mut app = test_app(session_path.clone());
        app.start_manual_note();
        assert_eq!(app.note_draft, Some(NoteDraft::default()));
        assert!(app.note_draft.as_ref().unwrap().caption().is_empty());
        assert!(app.note_draft.as_ref().unwrap().images().is_empty());
        assert!(!session_path.join("images").exists());
        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn text_only_note_writes_one_bullet_without_images_dir() {
        let (storage, session_path) = note_session("text-only");
        let mut app = test_app(session_path.clone());
        app.accumulated = Duration::from_secs(12 * 60 + 4);
        app.start_manual_note();
        type_note(&mut app, "whiteboard");
        app.handle_note_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        let notes = fs::read_to_string(session_path.join(".recall/notes.md")).unwrap();
        assert!(notes.contains("- `12:04` whiteboard"));
        assert!(!notes.contains("[image]"));
        assert!(!session_path.join("images").exists());

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn image_paste_with_caption_writes_file_and_markdown_link() {
        let (storage, session_path) = note_session("captioned");
        let source = storage.join("board.png");
        fs::write(&source, tiny_png()).unwrap();
        let mut app = test_app(session_path.clone());
        app.accumulated = Duration::from_secs(12 * 60 + 4);
        app.start_manual_note();
        type_note(&mut app, "whiteboard");
        app.handle_note_paste(&source.to_string_lossy());
        app.handle_note_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        let notes = fs::read_to_string(session_path.join(".recall/notes.md")).unwrap();
        let meeting = fs::read_to_string(session_path.join("meeting.md")).unwrap();
        assert!(session_path.join("images/12-04-note.png").is_file());
        assert!(notes.contains("- `12:04` whiteboard · [image](images/12-04-note.png)"));
        assert!(meeting.contains("[image](images/12-04-note.png)"));
        assert!(app.note_draft.is_none());

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn esc_after_image_paste_removes_unused_files() {
        let (storage, session_path) = note_session("esc-discard");
        let source = storage.join("board.png");
        fs::write(&source, tiny_png()).unwrap();
        let mut app = test_app(session_path.clone());
        app.accumulated = Duration::from_secs(12 * 60 + 4);
        app.start_manual_note();
        app.handle_note_paste(&source.to_string_lossy());
        assert!(session_path.join("images/12-04-note.png").is_file());
        app.handle_note_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));

        assert!(app.note_draft.is_none());
        assert!(!session_path.join("images").exists());
        let notes = fs::read_to_string(session_path.join(".recall/notes.md")).unwrap();
        assert!(!notes.contains("[image]"));

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn image_paste_without_caption_still_saves() {
        let (storage, session_path) = note_session("no-caption");
        let source = storage.join("board.png");
        fs::write(&source, tiny_png()).unwrap();
        let mut app = test_app(session_path.clone());
        app.accumulated = Duration::from_secs(12 * 60 + 4);
        app.start_manual_note();
        app.handle_note_paste(&source.to_string_lossy());
        app.handle_note_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        let notes = fs::read_to_string(session_path.join(".recall/notes.md")).unwrap();
        assert!(session_path.join("images/12-04-note.png").is_file());
        assert!(notes.contains("- `12:04` [image](images/12-04-note.png)"));

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn non_image_paste_does_not_create_images_dir() {
        let (storage, session_path) = note_session("non-image");
        let mut app = test_app(session_path.clone());
        app.accumulated = Duration::from_secs(12 * 60 + 4);
        app.start_manual_note();
        app.handle_note_paste("just some copied text");
        app.handle_note_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        let notes = fs::read_to_string(session_path.join(".recall/notes.md")).unwrap();
        assert!(notes.contains("- `12:04` just some copied text"));
        assert!(!session_path.join("images").exists());

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn multiline_paste_does_not_save_until_enter() {
        let (storage, session_path) = note_session("multiline-paste");
        let mut app = test_app(session_path.clone());
        app.accumulated = Duration::from_secs(12 * 60 + 4);
        app.start_manual_note();
        app.handle_note_paste("first line\nsecond line");
        assert!(app.note_draft.is_some());
        assert_eq!(
            app.note_draft.as_ref().unwrap().caption(),
            "first line\nsecond line"
        );
        let notes_before = fs::read_to_string(session_path.join(".recall/notes.md")).unwrap();
        assert!(!notes_before.contains("first line"));

        app.handle_note_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        let notes = fs::read_to_string(session_path.join(".recall/notes.md")).unwrap();
        let meeting = fs::read_to_string(session_path.join("meeting.md")).unwrap();
        assert!(notes.contains("- `12:04` first line\n  second line"));
        assert!(meeting.contains("first line"));
        assert!(meeting.contains("second line"));
        assert!(app.note_draft.is_none());

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn shift_enter_inserts_a_newline_without_saving() {
        let (storage, session_path) = note_session("shift-enter");
        let mut app = test_app(session_path.clone());
        app.accumulated = Duration::from_secs(12 * 60 + 4);
        app.start_manual_note();
        type_note(&mut app, "first");
        app.handle_note_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::SHIFT));
        assert_eq!(app.note_draft.as_ref().unwrap().caption(), "first\n");
        assert_eq!(app.note_draft.as_ref().unwrap().caret_line_cells(), (1, 0));
        type_note(&mut app, "second");
        assert_eq!(app.note_draft.as_ref().unwrap().caption(), "first\nsecond");
        let notes_before = fs::read_to_string(session_path.join(".recall/notes.md")).unwrap();
        assert!(!notes_before.contains("first"));

        for _ in 0.."\nsecond".len() {
            app.handle_note_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE));
        }
        assert_eq!(app.note_draft.as_ref().unwrap().caption(), "first");

        app.handle_note_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        assert_eq!(app.note_draft.as_ref().unwrap().caption(), "first\n");
        assert!(app.note_draft.is_some());

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn typing_inserts_at_the_cursor_not_only_at_the_end() {
        let (storage, session_path) = note_session("cursor-insert");
        let mut app = test_app(session_path);
        app.start_manual_note();
        type_note(&mut app, "testing 123");
        app.note_draft
            .as_mut()
            .unwrap()
            .set_caption_cursor("testing ".chars().count());
        type_note(&mut app, "xx");
        assert_eq!(app.note_draft.as_ref().unwrap().caption(), "testing xx123");
        assert_eq!(
            app.note_draft.as_ref().unwrap().caption_cursor(),
            "testing xx".chars().count()
        );

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn click_moves_the_note_cursor() {
        let mut draft = NoteDraft::default();
        draft.insert_str("testing 123testing123");
        draft.click_to_cursor(0, 8);
        assert_eq!(draft.caption_cursor(), 8);
        draft.click_to_cursor(0, 0);
        assert_eq!(draft.caption_cursor(), 0);
        draft.insert_str("hello\nworld");
        draft.click_to_cursor(1, 2);
        assert_eq!(draft.caption_cursor(), "hello\nwo".chars().count());
    }

    #[test]
    fn caret_and_click_use_terminal_cell_width() {
        let mut draft = NoteDraft::default();
        draft.insert_str("测a试");
        // 测 is two cells, a is one, 试 is two.
        draft.set_caption_cursor(1);
        assert_eq!(draft.caret_cell_col(), 2);
        draft.click_to_cursor(0, 2);
        assert_eq!(draft.caption_cursor(), 1);
        draft.click_to_cursor(0, 1);
        assert_eq!(draft.caption_cursor(), 0);
        draft.click_to_cursor(0, 3);
        assert_eq!(draft.caption_cursor(), 2);
        draft.set_caption_cursor(2);
        assert_eq!(draft.caret_cell_col(), 3);
    }

    #[test]
    fn pasted_images_are_numbered_chips_the_caret_can_reach() {
        let (storage, session_path) = note_session("image-chips");
        let source = storage.join("board.png");
        fs::write(&source, tiny_png()).unwrap();
        let mut app = test_app(session_path);
        app.start_manual_note();
        type_note(&mut app, "testing");
        app.handle_note_paste(&source.to_string_lossy());
        app.handle_note_paste(&source.to_string_lossy());
        let draft = app.note_draft.as_ref().unwrap();
        assert_eq!(NoteDraft::chip_label(0), "[Image #1]");
        assert_eq!(NoteDraft::chip_label(1), "[Image #2]");
        assert_eq!(draft.caret_after_chip(), Some(1));
        assert_eq!(draft.images().len(), 2);

        let mut app_end = app;
        app_end.handle_note_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        app_end.handle_note_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(
            app_end.note_draft.as_ref().unwrap().caret_after_chip(),
            Some(0)
        );
        app_end.handle_note_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        app_end.handle_note_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        let draft = app_end.note_draft.as_ref().unwrap();
        assert_eq!(draft.caret_after_chip(), None);
        assert_eq!(draft.caption_cursor(), "testing".chars().count());
        app_end.handle_note_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE));
        let (line, cells) = app_end.note_draft.as_ref().unwrap().caret_line_cells();
        assert_eq!(line, 0);
        assert!(cells > "testing".chars().count());

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn ctrl_j_inserts_a_newline_without_saving() {
        let (storage, session_path) = note_session("ctrl-j");
        let mut app = test_app(session_path.clone());
        app.accumulated = Duration::from_secs(12 * 60 + 4);
        app.start_manual_note();
        type_note(&mut app, "first");
        app.handle_note_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL));
        type_note(&mut app, "second");
        assert_eq!(app.note_draft.as_ref().unwrap().caption(), "first\nsecond");
        let notes_before = fs::read_to_string(session_path.join(".recall/notes.md")).unwrap();
        assert!(!notes_before.contains("first"));

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn forward_delete_at_end_does_not_backspace() {
        let mut draft = NoteDraft::default();
        draft.insert_str("hello");
        assert_eq!(draft.delete_forward(), None);
        assert_eq!(draft.caption(), "hello");
        draft.set_caption_cursor(1);
        assert_eq!(draft.delete_forward(), None);
        assert_eq!(draft.caption(), "hllo");
    }

    #[test]
    fn typing_after_an_image_chip_stays_to_the_right() {
        let (storage, session_path) = note_session("type-after-chip");
        let source = storage.join("board.png");
        fs::write(&source, tiny_png()).unwrap();
        let mut app = test_app(session_path);
        app.start_manual_note();
        type_note(&mut app, "test");
        app.handle_note_paste(&source.to_string_lossy());
        assert_eq!(app.note_draft.as_ref().unwrap().caret_after_chip(), Some(0));
        type_note(&mut app, " word");
        let draft = app.note_draft.as_ref().unwrap();
        assert_eq!(draft.caption(), "test word");
        assert_eq!(draft.caret_after_chip(), None);
        assert_eq!(draft.caption_cursor(), "test word".chars().count());
        let lines = draft.visual_lines();
        assert!(lines[0].iter().any(|run| run.image));
        assert_eq!(lines[0].last().map(|run| run.text.as_str()), Some(" word"));

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn url_in_caption_becomes_a_markdown_link_on_save() {
        let (storage, session_path) = note_session("url-link");
        let mut app = test_app(session_path.clone());
        app.accumulated = Duration::from_secs(12 * 60 + 4);
        app.start_manual_note();
        type_note(&mut app, "see https://example.com/foo");
        app.handle_note_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        let notes = fs::read_to_string(session_path.join(".recall/notes.md")).unwrap();
        let meeting = fs::read_to_string(session_path.join("meeting.md")).unwrap();
        assert!(notes.contains("- `12:04` see [https://example.com/foo](https://example.com/foo)"));
        assert!(meeting.contains("[https://example.com/foo](https://example.com/foo)"));
        assert!(!session_path.join("images").exists());

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn two_image_pastes_in_the_same_second_get_counter_suffix() {
        let (storage, session_path) = note_session("two-pastes");
        let source = storage.join("board.png");
        fs::write(&source, tiny_png()).unwrap();
        let mut app = test_app(session_path.clone());
        app.accumulated = Duration::from_secs(12 * 60 + 4);
        app.start_manual_note();
        app.handle_note_paste(&source.to_string_lossy());
        app.handle_note_paste(&source.to_string_lossy());
        app.handle_note_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));

        let notes = fs::read_to_string(session_path.join(".recall/notes.md")).unwrap();
        assert!(session_path.join("images/12-04-note.png").is_file());
        assert!(session_path.join("images/12-04-note-2.png").is_file());
        assert!(notes.contains("[image](images/12-04-note.png)"));
        assert!(notes.contains("[image](images/12-04-note-2.png)"));

        let _ = fs::remove_dir_all(storage);
    }

    fn space_key() -> KeyEvent {
        KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE)
    }

    fn enter_key() -> KeyEvent {
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)
    }

    #[test]
    fn space_does_not_start_end_or_continue_outside_recording() {
        let mut ready = test_app(PathBuf::from("/tmp/recall-space-ready"));
        ready.state = CaptureState::Ready;
        ready.session_path = None;
        ready.handle_key(space_key()).unwrap();
        assert_eq!(ready.state, CaptureState::Ready);
        assert!(!ready.mic_muted);
        assert_eq!(ready.toast, "Start recording before muting the mic.");

        let mut ended = test_app(PathBuf::from("/tmp/recall-space-ended"));
        ended.state = CaptureState::Ended;
        ended.handle_key(space_key()).unwrap();
        assert_eq!(ended.state, CaptureState::Ended);
        assert!(!ended.mic_muted);
        assert_eq!(ended.toast, "Start recording before muting the mic.");
        assert_eq!(
            next_recording_action(ended.state, ended.session_path.is_some(), ended.append_next),
            RecordingAction::Continue
        );
    }

    #[test]
    fn space_toggles_mute_only_while_recording() {
        let (storage, session_path) = note_session("mute-toggle");
        let mut app = test_app(session_path.clone());
        app.state = CaptureState::Recording;
        app.started_at = Some(Instant::now());
        app.mic_level_percent = 40;

        app.handle_key(space_key()).unwrap();
        assert_eq!(app.state, CaptureState::Recording);
        assert!(app.mic_muted);
        assert_eq!(app.mic_signal_value(), 0);
        assert!(app.toast.contains("Mic muted for Recall"));
        assert!(app.toast.contains("You are not muted in Zoom/Teams."));
        assert!(app
            .live_notes
            .iter()
            .any(|note| note.contains("Mic muted for Recall at `")));
        assert!(crate::mic_recorder::mute_mic_path(&session_path).exists());
        let notes = fs::read_to_string(session_path.join(".recall/notes.md")).unwrap();
        assert!(notes.contains("Mic muted for Recall"));

        app.handle_key(space_key()).unwrap();
        assert_eq!(app.state, CaptureState::Recording);
        assert!(!app.mic_muted);
        assert_eq!(app.toast, "Mic unmuted.");
        assert!(app
            .live_notes
            .iter()
            .any(|note| note.contains("Mic unmuted at `")));
        assert!(!crate::mic_recorder::mute_mic_path(&session_path).exists());
        let notes = fs::read_to_string(session_path.join(".recall/notes.md")).unwrap();
        assert!(notes.contains("Mic unmuted"));

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn space_in_note_draft_inserts_a_space_and_does_not_mute() {
        let (storage, session_path) = note_session("mute-note-space");
        let mut app = test_app(session_path.clone());
        app.state = CaptureState::Recording;
        app.start_manual_note();
        type_note(&mut app, "hello");
        app.handle_key(space_key()).unwrap();
        type_note(&mut app, "world");
        assert_eq!(app.note_draft.as_ref().unwrap().caption(), "hello world");
        assert!(!app.mic_muted);
        assert_eq!(app.state, CaptureState::Recording);
        assert!(!crate::mic_recorder::mute_mic_path(&session_path).exists());

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn enter_still_ends_a_take_and_m_still_drops_a_marker() {
        let (storage, session_path) = note_session("enter-end-marker");
        let mut app = test_app(session_path.clone());
        app.state = CaptureState::Recording;
        app.started_at = Some(Instant::now());
        app.take_index = 1;
        app.completed_take = 0;
        app.auto_analyze = false;

        app.handle_key(KeyEvent::new(KeyCode::Char('m'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.state, CaptureState::Recording);
        assert!(app.markers.iter().any(|marker| marker.contains("marker")));
        let markers = fs::read_to_string(session_path.join(".recall/markers.md")).unwrap();
        assert!(markers.contains("Marker"));

        app.handle_key(space_key()).unwrap();
        assert!(app.mic_muted);
        app.handle_key(enter_key()).unwrap();
        assert_eq!(app.state, CaptureState::Ended);
        assert!(!app.mic_muted);
        assert!(!crate::mic_recorder::mute_mic_path(&session_path).exists());
        assert_eq!(
            next_recording_action(app.state, app.session_path.is_some(), app.append_next),
            RecordingAction::Continue
        );

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn ending_a_take_clears_mute_without_a_second_space() {
        let (storage, session_path) = note_session("end-clears-mute");
        let mut app = test_app(session_path.clone());
        app.state = CaptureState::Recording;
        app.started_at = Some(Instant::now());
        app.take_index = 1;
        app.completed_take = 0;
        app.auto_analyze = false;
        app.handle_key(space_key()).unwrap();
        assert!(app.mic_muted);
        app.end_capture();
        assert_eq!(app.state, CaptureState::Ended);
        assert!(!app.mic_muted);
        assert_eq!(app.mic_signal_value(), 0);
        assert!(!crate::mic_recorder::mute_mic_path(&session_path).exists());

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn muted_level_events_stay_at_zero_percent() {
        assert_eq!(db_to_percent(f32::NEG_INFINITY), 0);
        assert_eq!(db_to_percent(-160.0), 0);
        let mut app = test_app(PathBuf::from("/tmp/recall-muted-meter"));
        app.state = CaptureState::Recording;
        app.mic_muted = true;
        app.mic_level_percent = 88;
        assert_eq!(app.mic_signal_value(), 0);
    }
}
