use std::env;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use crate::session::{default_storage_dir, list_sessions};

pub const TRANSCRIPTION_CHUNK_SECONDS: u64 = 600;

#[derive(Debug, Clone)]
pub struct TranscribeOptions {
    pub target: TranscribeTarget,
    pub track: TrackSelection,
    pub storage_dir: Option<PathBuf>,
    pub ffmpeg_bin: Option<PathBuf>,
    pub model_path: Option<PathBuf>,
    pub whisper_bin: Option<PathBuf>,
    pub chunk_seconds: u64,
    pub keep_wav: bool,
}

#[derive(Debug, Clone)]
pub enum TranscribeTarget {
    Latest,
    Session(PathBuf),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackSelection {
    Both,
    Call,
    Mic,
}

#[derive(Debug, Clone)]
pub struct TranscribeResult {
    pub session_path: PathBuf,
    pub transcript_path: PathBuf,
    pub tracks: Vec<TrackResult>,
}

#[derive(Debug, Clone)]
pub struct TrackResult {
    pub label: &'static str,
    pub audio_path: PathBuf,
    pub text_len: usize,
    pub chunk_count: usize,
}

#[derive(Debug, Clone)]
pub enum TranscriptionProgress {
    Started {
        session_path: PathBuf,
    },
    TrackStarted {
        track: &'static str,
        chunks: usize,
    },
    ChunkStarted {
        track: &'static str,
        index: usize,
        total: usize,
    },
    TrackFinished {
        track: &'static str,
        text_len: usize,
        chunks: usize,
    },
    Finished {
        transcript_path: PathBuf,
    },
}

#[derive(Debug, Deserialize)]
struct RecallMetadata {
    title: Option<String>,
}

#[derive(Debug, Clone)]
struct TranscriptSegment {
    start_ms: u64,
    end_ms: u64,
    track: &'static str,
    text: String,
}

#[derive(Debug, Clone)]
struct AudioChunk {
    wav_path: PathBuf,
    output_base: PathBuf,
    start_ms: u64,
}

impl TrackSelection {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "both" => Some(Self::Both),
            "call" | "system" | "remote" => Some(Self::Call),
            "mic" | "microphone" => Some(Self::Mic),
            _ => None,
        }
    }

    fn tracks(self) -> Vec<Track> {
        match self {
            Self::Both => vec![Track::Call, Track::Mic],
            Self::Call => vec![Track::Call],
            Self::Mic => vec![Track::Mic],
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum Track {
    Call,
    Mic,
}

impl Track {
    fn label(self) -> &'static str {
        match self {
            Self::Call => "call",
            Self::Mic => "mic",
        }
    }

    fn file_name(self) -> &'static str {
        match self {
            Self::Call => "call.m4a",
            Self::Mic => "mic.m4a",
        }
    }

    fn heading(self) -> &'static str {
        match self {
            Self::Call => "Call Audio",
            Self::Mic => "Microphone",
        }
    }
}

#[allow(dead_code)]
pub fn transcribe(options: &TranscribeOptions) -> io::Result<TranscribeResult> {
    transcribe_with_progress(options, |_| {})
}

pub fn transcribe_with_progress<F>(
    options: &TranscribeOptions,
    mut progress: F,
) -> io::Result<TranscribeResult>
where
    F: FnMut(TranscriptionProgress),
{
    let session_path = resolve_session_path(options)?;
    let ffmpeg = resolve_ffmpeg_binary(options)?;
    let whisper = resolve_whisper_binary(options)?;
    let model = resolve_model_path(options)?;
    let title = read_session_title(&session_path)?;
    let work_dir = session_path.join("transcription-work");
    fs::create_dir_all(&work_dir)?;

    progress(TranscriptionProgress::Started {
        session_path: session_path.clone(),
    });

    let mut sections = Vec::new();
    let mut segments = Vec::new();
    let mut track_results = Vec::new();

    for track in options.track.tracks() {
        let audio_path = session_path.join("audio").join(track.file_name());
        if !audio_path.exists() {
            continue;
        }

        let chunks_dir = work_dir.join(format!("{}-chunks", track.label()));
        cleanup_legacy_track_outputs(&work_dir, track);
        let chunks =
            convert_to_wav_chunks(&ffmpeg, &audio_path, &chunks_dir, options.chunk_seconds)?;
        let mut track_text_parts = Vec::new();

        progress(TranscriptionProgress::TrackStarted {
            track: track.label(),
            chunks: chunks.len(),
        });

        for (index, chunk) in chunks.iter().enumerate() {
            progress(TranscriptionProgress::ChunkStarted {
                track: track.label(),
                index: index + 1,
                total: chunks.len(),
            });
            run_whisper(&whisper, &model, &chunk.wav_path, &chunk.output_base)?;
            let text = read_whisper_output(&chunk.output_base, &chunk.wav_path)?;
            let text = clean_whisper_text_block(&text);
            if !text.is_empty() {
                track_text_parts.push(text);
            }
            segments.extend(
                read_whisper_segments(&chunk.output_base, track.label(), chunk.start_ms)
                    .unwrap_or_default(),
            );

            if !options.keep_wav {
                let _ = fs::remove_file(&chunk.wav_path);
            }
        }

        let clean_text = track_text_parts.join("\n");

        sections.push(format!(
            "## {}\n\nSource: `audio/{}`\n\n{}\n",
            track.heading(),
            track.file_name(),
            if clean_text.is_empty() {
                "_No transcript text returned._"
            } else {
                &clean_text
            }
        ));
        track_results.push(TrackResult {
            label: track.label(),
            audio_path,
            text_len: clean_text.len(),
            chunk_count: chunks.len(),
        });
        progress(TranscriptionProgress::TrackFinished {
            track: track.label(),
            text_len: clean_text.len(),
            chunks: chunks.len(),
        });
    }

    if track_results.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "No audio files found for requested track selection under {}",
                session_path.join("audio").display()
            ),
        ));
    }

    let transcript_path = session_path.join("transcript.md");
    let debug_dir = session_path.join("transcription-debug");
    write_transcription_outputs(
        &transcript_path,
        &debug_dir,
        &title,
        &model,
        &segments,
        &sections,
    )?;

    progress(TranscriptionProgress::Finished {
        transcript_path: transcript_path.clone(),
    });

    Ok(TranscribeResult {
        session_path,
        transcript_path,
        tracks: track_results,
    })
}

fn resolve_session_path(options: &TranscribeOptions) -> io::Result<PathBuf> {
    match &options.target {
        TranscribeTarget::Session(path) => Ok(path.clone()),
        TranscribeTarget::Latest => {
            let storage_dir = match &options.storage_dir {
                Some(path) => path.clone(),
                None => default_storage_dir()?,
            };
            let sessions = list_sessions(&storage_dir)?;
            sessions.into_iter().next().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("No Recall sessions found in {}", storage_dir.display()),
                )
            })
        }
    }
}

fn resolve_ffmpeg_binary(options: &TranscribeOptions) -> io::Result<PathBuf> {
    if let Some(path) = &options.ffmpeg_bin {
        return Ok(path.clone());
    }

    if let Some(path) = env::var_os("RECALL_FFMPEG_BIN").map(PathBuf::from) {
        return Ok(path);
    }

    let candidates = [
        PathBuf::from("tools/ffmpeg/bin/ffmpeg"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tools/ffmpeg/bin/ffmpeg"),
    ];

    if let Some(path) = candidates.into_iter().find(|path| path.exists()) {
        return Ok(path);
    }

    find_required_binary(
        "ffmpeg",
        "Install ffmpeg, place it at tools/ffmpeg/bin/ffmpeg, or set RECALL_FFMPEG_BIN=/path/to/ffmpeg.",
    )
}

fn resolve_whisper_binary(options: &TranscribeOptions) -> io::Result<PathBuf> {
    if let Some(path) = &options.whisper_bin {
        return Ok(path.clone());
    }

    if let Some(path) = env::var_os("RECALL_WHISPER_BIN").map(PathBuf::from) {
        return Ok(path);
    }

    find_binary("whisper-cli")
        .or_else(|| find_binary("whisper-cpp"))
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "Missing local Whisper CLI. Install whisper.cpp, then make `whisper-cli` available on PATH or set RECALL_WHISPER_BIN=/path/to/whisper-cli.",
            )
        })
}

fn resolve_model_path(options: &TranscribeOptions) -> io::Result<PathBuf> {
    if let Some(path) = &options.model_path {
        return Ok(path.clone());
    }

    if let Some(path) = env::var_os("RECALL_WHISPER_MODEL").map(PathBuf::from) {
        return Ok(path);
    }

    let mut candidates = vec![
        PathBuf::from("models/ggml-base.en.bin"),
        PathBuf::from("models/ggml-base.bin"),
        PathBuf::from("models/ggml-small.en.bin"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("models/ggml-base.en.bin"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("models/ggml-base.bin"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("models/ggml-small.en.bin"),
    ];

    if let Some(home) = env::var_os("HOME").map(PathBuf::from) {
        candidates.push(home.join("Library/Application Support/recall/models/ggml-base.en.bin"));
        candidates.push(home.join("Library/Application Support/recall/models/ggml-base.bin"));
    }

    candidates.into_iter().find(|path| path.exists()).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "Missing Whisper model. Put a ggml model at `models/ggml-base.en.bin` or set RECALL_WHISPER_MODEL=/path/to/model.bin.",
        )
    })
}

fn find_required_binary(name: &str, hint: &str) -> io::Result<PathBuf> {
    find_binary(name)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("Missing `{name}`. {hint}")))
}

fn find_binary(name: &str) -> Option<PathBuf> {
    let path = Path::new(name);
    if path.components().count() > 1 && path.exists() {
        return Some(path.to_path_buf());
    }

    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|path| path.exists())
}

fn cleanup_legacy_track_outputs(work_dir: &Path, track: Track) {
    let output_base = work_dir.join(track.label());
    for extension in ["wav", "txt", "vtt"] {
        let _ = fs::remove_file(path_with_added_extension(&output_base, extension));
    }
}

fn convert_to_wav_chunks(
    ffmpeg: &Path,
    input: &Path,
    chunks_dir: &Path,
    chunk_seconds: u64,
) -> io::Result<Vec<AudioChunk>> {
    if chunks_dir.exists() {
        fs::remove_dir_all(chunks_dir)?;
    }
    fs::create_dir_all(chunks_dir)?;

    let output_pattern = chunks_dir.join("chunk-%05d.wav");
    let status = Command::new(ffmpeg)
        .arg("-y")
        .arg("-i")
        .arg(input)
        .arg("-vn")
        .arg("-ar")
        .arg("16000")
        .arg("-ac")
        .arg("1")
        .arg("-c:a")
        .arg("pcm_s16le")
        .arg("-f")
        .arg("segment")
        .arg("-segment_time")
        .arg(chunk_seconds.to_string())
        .arg("-reset_timestamps")
        .arg("1")
        .arg(output_pattern)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;

    if !status.success() {
        return Err(io::Error::other(format!(
            "ffmpeg failed to chunk {}",
            input.display()
        )));
    }

    let mut wav_paths = fs::read_dir(chunks_dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "wav"))
        .collect::<Vec<_>>();
    wav_paths.sort();

    if wav_paths.is_empty() {
        return Err(io::Error::other(format!(
            "ffmpeg produced no chunks for {}",
            input.display()
        )));
    }

    Ok(wav_paths
        .into_iter()
        .enumerate()
        .map(|(index, wav_path)| AudioChunk {
            output_base: chunks_dir.join(format!("chunk-{index:05}")),
            wav_path,
            start_ms: index as u64 * chunk_seconds * 1000,
        })
        .collect())
}

fn run_whisper(whisper: &Path, model: &Path, wav: &Path, output_base: &Path) -> io::Result<()> {
    if run_whisper_once(whisper, model, wav, output_base, false)? {
        return Ok(());
    }

    if run_whisper_once(whisper, model, wav, output_base, true)? {
        return Ok(());
    }

    Err(io::Error::other(format!(
        "Whisper transcription failed for {}",
        wav.display()
    )))
}

fn run_whisper_once(
    whisper: &Path,
    model: &Path,
    wav: &Path,
    output_base: &Path,
    no_gpu: bool,
) -> io::Result<bool> {
    let status = Command::new(whisper)
        .arg("-m")
        .arg(model)
        .arg("-otxt")
        .arg("-ovtt")
        .arg("-of")
        .arg(output_base)
        .arg("-np")
        .args(no_gpu.then_some("--no-gpu"))
        .arg(wav)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;

    Ok(status.success())
}

fn read_whisper_segments(
    output_base: &Path,
    track: &'static str,
    offset_ms: u64,
) -> io::Result<Vec<TranscriptSegment>> {
    let vtt_path = path_with_added_extension(output_base, "vtt");
    if !vtt_path.exists() {
        return Ok(Vec::new());
    }

    parse_vtt_segments_with_offset(track, &fs::read_to_string(vtt_path)?, offset_ms)
}

fn parse_vtt_segments_with_offset(
    track: &'static str,
    content: &str,
    offset_ms: u64,
) -> io::Result<Vec<TranscriptSegment>> {
    let mut segments = Vec::new();
    let mut lines = content.lines().peekable();

    while let Some(line) = lines.next() {
        let Some((start, end)) = parse_vtt_timing(line) else {
            continue;
        };

        let mut text_lines = Vec::new();
        while let Some(next) = lines.peek() {
            if next.trim().is_empty() {
                let _ = lines.next();
                break;
            }
            text_lines.push(lines.next().unwrap_or_default().trim().to_string());
        }

        let text = clean_transcript_text(&text_lines.join(" "));
        if !text.is_empty() {
            segments.push(TranscriptSegment {
                start_ms: start + offset_ms,
                end_ms: end + offset_ms,
                track,
                text,
            });
        }
    }

    Ok(segments)
}

fn clean_transcript_text(text: &str) -> String {
    let mut clean = text.trim();
    while let Some(stripped) = clean.strip_prefix(">>") {
        clean = stripped.trim_start();
    }
    clean.to_string()
}

fn clean_whisper_text_block(text: &str) -> String {
    text.lines()
        .map(clean_transcript_text)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

fn parse_vtt_timing(line: &str) -> Option<(u64, u64)> {
    let (start, rest) = line.split_once(" --> ")?;
    let end = rest.split_whitespace().next()?;
    Some((parse_vtt_time(start)?, parse_vtt_time(end)?))
}

fn parse_vtt_time(value: &str) -> Option<u64> {
    let mut parts = value.split(':').collect::<Vec<_>>();
    if parts.len() == 2 {
        parts.insert(0, "0");
    }
    if parts.len() != 3 {
        return None;
    }

    let hours = parts[0].parse::<u64>().ok()?;
    let minutes = parts[1].parse::<u64>().ok()?;
    let (seconds, millis) = parts[2].split_once('.')?;
    let seconds = seconds.parse::<u64>().ok()?;
    let millis = millis.parse::<u64>().ok()?;

    Some((((hours * 60 + minutes) * 60 + seconds) * 1000) + millis)
}

fn read_whisper_output(output_base: &Path, wav: &Path) -> io::Result<String> {
    let candidates = [
        path_with_added_extension(output_base, "txt"),
        path_with_added_extension(wav, "txt"),
        wav.with_extension("txt"),
    ];

    for candidate in candidates {
        if candidate.exists() {
            return fs::read_to_string(candidate);
        }
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "Whisper finished but no .txt output file was found.",
    ))
}

fn path_with_added_extension(path: &Path, extension: &str) -> PathBuf {
    let mut value = OsString::from(path.as_os_str());
    value.push(".");
    value.push(extension);
    PathBuf::from(value)
}

fn read_session_title(session_path: &Path) -> io::Result<String> {
    let metadata_path = session_path.join("recall.json");
    let metadata = fs::read_to_string(metadata_path)?;
    let metadata =
        serde_json::from_str::<RecallMetadata>(&metadata).unwrap_or(RecallMetadata { title: None });
    Ok(metadata
        .title
        .unwrap_or_else(|| "Recall Session".to_string()))
}

fn write_transcription_outputs(
    transcript_path: &Path,
    debug_dir: &Path,
    title: &str,
    model: &Path,
    segments: &[TranscriptSegment],
    sections: &[String],
) -> io::Result<()> {
    if debug_dir.exists() {
        fs::remove_dir_all(debug_dir)?;
    }
    fs::create_dir_all(debug_dir)?;

    fs::write(transcript_path, transcript_markdown(title, model, segments))?;
    fs::write(
        debug_dir.join("combined-timeline.md"),
        combined_timeline_debug_markdown(title, model, segments),
    )?;
    fs::write(
        debug_dir.join("raw-tracks.md"),
        raw_tracks_debug_markdown(title, model, sections),
    )?;
    fs::write(
        debug_dir.join("full-debug-transcript.md"),
        full_debug_transcript_markdown(title, model, segments, sections),
    )?;

    Ok(())
}

fn transcript_markdown(title: &str, model: &Path, segments: &[TranscriptSegment]) -> String {
    let clean_timeline = clean_conversation_markdown(segments);
    format!(
        "# Transcript: {title}\n\nGenerated by local Whisper transcription at Unix time {}.\n\nModel: `{}`\n\n{}",
        unix_timestamp(),
        model.display(),
        if clean_timeline.is_empty() {
            "_No clean transcript text returned._\n"
        } else {
            &clean_timeline
        }
    )
}

fn combined_timeline_debug_markdown(
    title: &str,
    model: &Path,
    segments: &[TranscriptSegment],
) -> String {
    format!(
        "# Combined Timeline Debug: {title}\n\nGenerated by local Whisper transcription at Unix time {}.\n\nModel: `{}`\n\n{}",
        unix_timestamp(),
        model.display(),
        combined_timeline_markdown(segments)
    )
}

fn raw_tracks_debug_markdown(title: &str, model: &Path, sections: &[String]) -> String {
    format!(
        "# Raw Track Transcripts: {title}\n\nGenerated by local Whisper transcription at Unix time {}.\n\nModel: `{}`\n\n{}\n",
        unix_timestamp(),
        model.display(),
        sections.join("\n")
    )
}

fn full_debug_transcript_markdown(
    title: &str,
    model: &Path,
    segments: &[TranscriptSegment],
    sections: &[String],
) -> String {
    let clean_timeline = clean_conversation_markdown(segments);
    let timeline = combined_timeline_markdown(segments);
    format!(
        "# Full Debug Transcript: {title}\n\nGenerated by local Whisper transcription at Unix time {}.\n\nModel: `{}`\n\n{}{}{}\n",
        unix_timestamp(),
        model.display(),
        clean_timeline,
        timeline,
        sections.join("\n")
    )
}

fn clean_conversation_markdown(segments: &[TranscriptSegment]) -> String {
    if segments.is_empty() {
        return String::new();
    }

    let (clean_segments, suppressed_count) = clean_conversation_segments(segments);
    if clean_segments.is_empty() {
        return String::new();
    }

    let mut markdown = String::from("## Clean Conversation\n\n");
    for segment in clean_segments {
        markdown.push_str(&format!(
            "- [{} - {}] **{}:** {}\n",
            format_timestamp(segment.start_ms),
            format_timestamp(segment.end_ms),
            segment.track,
            segment.text
        ));
    }

    if suppressed_count > 0 {
        markdown.push_str(&format!(
            "\n_Suppressed {suppressed_count} likely duplicate mic segment"
        ));
        if suppressed_count == 1 {
            markdown.push_str(" caused by speaker bleed._\n");
        } else {
            markdown.push_str("s caused by speaker bleed._\n");
        }
    }

    markdown.push('\n');
    markdown
}

fn clean_conversation_segments(segments: &[TranscriptSegment]) -> (Vec<TranscriptSegment>, usize) {
    let mut sorted = segments.to_vec();
    sorted.sort_by_key(|segment| (segment.start_ms, segment.end_ms, segment.track));

    let mut clean = Vec::new();
    let mut suppressed_count = 0;
    for segment in &sorted {
        let candidate = clean_segment_text(segment, &sorted);
        if is_likely_duplicate_mic_segment(segment, &sorted)
            || is_low_value_mic_filler(&candidate, &sorted)
            || is_repeated_mic_loop(&candidate, &clean)
        {
            suppressed_count += 1;
        } else {
            clean.push(candidate);
        }
    }

    (clean, suppressed_count)
}

fn clean_segment_text(
    segment: &TranscriptSegment,
    all_segments: &[TranscriptSegment],
) -> TranscriptSegment {
    if segment.track != "mic" {
        return segment.clone();
    }

    let Some(call_context) = call_context_for_mic_segment(segment, all_segments) else {
        return segment.clone();
    };

    let Some(clean_text) = mic_text_without_call_context(&segment.text, &call_context) else {
        return segment.clone();
    };

    TranscriptSegment {
        text: clean_text,
        ..segment.clone()
    }
}

fn is_likely_duplicate_mic_segment(
    segment: &TranscriptSegment,
    all_segments: &[TranscriptSegment],
) -> bool {
    if segment.track != "mic" {
        return false;
    }

    let Some(call_context) = call_context_for_mic_segment(segment, all_segments) else {
        return false;
    };

    if !is_likely_duplicate_text(&segment.text, &call_context) {
        return false;
    }

    mic_text_without_call_context(&segment.text, &call_context)
        .map(|text| meaningful_token_count(&text) < 2 || normalized_tokens(&text).len() < 6)
        .unwrap_or(true)
}

fn is_low_value_mic_filler(
    segment: &TranscriptSegment,
    all_segments: &[TranscriptSegment],
) -> bool {
    if segment.track != "mic" || !has_nearby_call_segment(segment, all_segments) {
        return false;
    }

    let tokens = normalized_tokens(&segment.text);
    if tokens.is_empty() || tokens.len() > 3 {
        return false;
    }

    tokens.iter().all(|token| {
        matches!(
            token.as_str(),
            "yeah" | "yes" | "yep" | "ok" | "okay" | "right" | "sure" | "mm" | "hmm"
        )
    })
}

fn is_repeated_mic_loop(segment: &TranscriptSegment, clean_segments: &[TranscriptSegment]) -> bool {
    if segment.track != "mic" {
        return false;
    }

    let segment_tokens = normalized_tokens(&segment.text);
    if segment_tokens.len() < 4 {
        return false;
    }

    clean_segments
        .iter()
        .rev()
        .filter(|candidate| candidate.track == "mic")
        .take(8)
        .any(|candidate| {
            let within_loop_window = segment.start_ms.saturating_sub(candidate.end_ms) <= 45_000;
            within_loop_window && is_likely_duplicate_text(&segment.text, &candidate.text)
        })
}

fn call_context_for_mic_segment(
    segment: &TranscriptSegment,
    all_segments: &[TranscriptSegment],
) -> Option<String> {
    let has_overlapping_call_segment = all_segments
        .iter()
        .filter(|candidate| candidate.track == "call")
        .any(|candidate| segments_temporally_overlap(segment, candidate));

    if !has_overlapping_call_segment {
        return None;
    }

    let nearby_call_segments = all_segments
        .iter()
        .filter(|candidate| candidate.track == "call")
        .filter(|candidate| segments_are_near(segment, candidate))
        .collect::<Vec<_>>();

    if nearby_call_segments.is_empty() {
        return None;
    }

    Some(
        nearby_call_segments
            .iter()
            .map(|candidate| candidate.text.as_str())
            .collect::<Vec<_>>()
            .join(" "),
    )
}

fn has_nearby_call_segment(
    segment: &TranscriptSegment,
    all_segments: &[TranscriptSegment],
) -> bool {
    all_segments
        .iter()
        .filter(|candidate| candidate.track == "call")
        .any(|candidate| segments_are_near(segment, candidate))
}

fn segments_temporally_overlap(a: &TranscriptSegment, b: &TranscriptSegment) -> bool {
    let overlap_start = a.start_ms.max(b.start_ms);
    let overlap_end = a.end_ms.min(b.end_ms);
    if overlap_end <= overlap_start {
        return false;
    }

    let overlap = overlap_end - overlap_start;
    let shortest = segment_duration(a).min(segment_duration(b));
    if shortest == 0 {
        return false;
    }

    overlap >= 500 && (overlap as f64 / shortest as f64) >= 0.40
}

fn segments_are_near(a: &TranscriptSegment, b: &TranscriptSegment) -> bool {
    const CONTEXT_WINDOW_MS: u64 = 1_500;

    let a_start = a.start_ms.saturating_sub(CONTEXT_WINDOW_MS);
    let a_end = a.end_ms.saturating_add(CONTEXT_WINDOW_MS);

    a_start <= b.end_ms && b.start_ms <= a_end
}

fn segment_duration(segment: &TranscriptSegment) -> u64 {
    segment.end_ms.saturating_sub(segment.start_ms)
}

fn is_likely_duplicate_text(mic_text: &str, call_text: &str) -> bool {
    let mic_tokens = normalized_tokens(mic_text);
    let call_tokens = normalized_tokens(call_text);

    if mic_tokens.len() < 4 || call_tokens.len() < 4 {
        return false;
    }

    let intersection = multiset_intersection_count(&mic_tokens, &call_tokens);
    let f1_similarity = (2.0 * intersection as f64) / (mic_tokens.len() + call_tokens.len()) as f64;
    let mic_containment = intersection as f64 / mic_tokens.len() as f64;
    let mic_unique_count = mic_tokens.len().saturating_sub(intersection);

    f1_similarity >= 0.82 || (mic_containment >= 0.80 && mic_unique_count <= 3)
}

fn mic_text_without_call_context(mic_text: &str, call_text: &str) -> Option<String> {
    let mic_tokens = text_tokens(mic_text);
    let call_tokens = normalized_tokens(call_text);

    if mic_tokens.len() < 4 || call_tokens.len() < 4 {
        return None;
    }

    let mut removed = vec![false; mic_tokens.len()];
    let mut removed_count = 0;

    while let Some((start, len)) =
        longest_common_unremoved_block(&mic_tokens, &call_tokens, &removed)
    {
        if len < 4 {
            break;
        }

        for was_removed in removed.iter_mut().skip(start).take(len) {
            *was_removed = true;
            removed_count += 1;
        }
    }

    let kept = mic_tokens
        .into_iter()
        .enumerate()
        .filter_map(|(index, token)| (!removed[index]).then_some(token.original))
        .collect::<Vec<_>>();

    if removed_count == 0 || kept.len() < 3 {
        return None;
    }

    let clean_text = kept.join(" ").trim().to_string();
    if clean_text.is_empty() || clean_text == mic_text.trim() {
        None
    } else {
        Some(clean_text)
    }
}

fn longest_common_unremoved_block(
    mic_tokens: &[TextToken],
    call_tokens: &[String],
    removed: &[bool],
) -> Option<(usize, usize)> {
    let mut best_start = 0;
    let mut best_len = 0;

    for mic_start in 0..mic_tokens.len() {
        if removed[mic_start] {
            continue;
        }

        for call_start in 0..call_tokens.len() {
            let mut len = 0;
            while mic_start + len < mic_tokens.len()
                && call_start + len < call_tokens.len()
                && !removed[mic_start + len]
                && mic_tokens[mic_start + len].normalized == call_tokens[call_start + len]
            {
                len += 1;
            }

            if len > best_len {
                best_start = mic_start;
                best_len = len;
            }
        }
    }

    (best_len > 0).then_some((best_start, best_len))
}

fn normalized_tokens(text: &str) -> Vec<String> {
    text_tokens(text)
        .into_iter()
        .map(|token| token.normalized)
        .collect()
}

#[derive(Debug)]
struct TextToken {
    normalized: String,
    original: String,
}

fn text_tokens(text: &str) -> Vec<TextToken> {
    text.split_whitespace()
        .filter_map(|token| {
            let normalized = normalize_token(token)?;
            Some(TextToken {
                normalized,
                original: token.to_string(),
            })
        })
        .collect()
}

fn normalize_token(token: &str) -> Option<String> {
    let normalized = token
        .trim_matches(|ch: char| !ch.is_ascii_alphanumeric() && ch != '\'')
        .to_ascii_lowercase();

    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

fn meaningful_token_count(text: &str) -> usize {
    const STOPWORDS: &[&str] = &[
        "a", "an", "and", "are", "as", "at", "be", "did", "for", "he", "i", "in", "is", "it", "me",
        "of", "or", "so", "that", "the", "then", "there", "to", "um", "we", "what", "with", "you",
    ];

    normalized_tokens(text)
        .into_iter()
        .filter(|token| !STOPWORDS.contains(&token.as_str()))
        .count()
}

fn multiset_intersection_count(left: &[String], right: &[String]) -> usize {
    let mut right_tokens = right.to_vec();
    let mut count = 0;

    for token in left {
        if let Some(index) = right_tokens.iter().position(|candidate| candidate == token) {
            right_tokens.swap_remove(index);
            count += 1;
        }
    }

    count
}

fn combined_timeline_markdown(segments: &[TranscriptSegment]) -> String {
    if segments.is_empty() {
        return String::new();
    }

    let mut sorted = segments.to_vec();
    sorted.sort_by_key(|segment| (segment.start_ms, segment.end_ms, segment.track));

    let mut markdown = String::from("## Combined Timeline\n\n");
    for segment in sorted {
        markdown.push_str(&format!(
            "- [{} - {}] **{}:** {}\n",
            format_timestamp(segment.start_ms),
            format_timestamp(segment.end_ms),
            segment.track,
            segment.text
        ));
    }
    markdown.push('\n');
    markdown
}

fn format_timestamp(ms: u64) -> String {
    let total_seconds = ms / 1000;
    let millis = ms % 1000;
    let minutes = total_seconds / 60;
    let seconds = total_seconds % 60;
    format!("{minutes:02}:{seconds:02}.{millis:03}")
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{
        clean_conversation_segments, clean_transcript_text, clean_whisper_text_block,
        format_timestamp, parse_vtt_segments_with_offset, path_with_added_extension,
        TrackSelection, TranscriptSegment,
    };
    use std::path::Path;

    #[test]
    fn parses_track_selection_aliases() {
        assert!(matches!(
            TrackSelection::parse("both"),
            Some(TrackSelection::Both)
        ));
        assert!(matches!(
            TrackSelection::parse("remote"),
            Some(TrackSelection::Call)
        ));
        assert!(matches!(
            TrackSelection::parse("microphone"),
            Some(TrackSelection::Mic)
        ));
        assert!(TrackSelection::parse("other").is_none());
    }

    #[test]
    fn adds_extensions_without_replacing_existing_one() {
        assert_eq!(
            path_with_added_extension(Path::new("work/call.wav"), "txt"),
            Path::new("work/call.wav.txt")
        );
    }

    #[test]
    fn parses_vtt_segments() {
        let segments = parse_vtt_segments_with_offset(
            "mic",
            "WEBVTT\n\n00:00:01.250 --> 00:00:03.500\n>> Hello there.\n\n00:00:04.000 --> 00:00:05.000\nNext line.\n",
            0,
        )
        .unwrap();

        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].start_ms, 1250);
        assert_eq!(segments[0].end_ms, 3500);
        assert_eq!(segments[0].track, "mic");
        assert_eq!(segments[0].text, "Hello there.");
    }

    #[test]
    fn removes_whisper_turn_markers_from_text() {
        assert_eq!(clean_transcript_text(">> Yeah."), "Yeah.");
        assert_eq!(clean_transcript_text(">> >> Hello?"), "Hello?");
        assert_eq!(
            clean_whisper_text_block(" >> Hello?\n\n>> Yeah.\n"),
            "Hello?\nYeah."
        );
    }

    #[test]
    fn parses_vtt_segments_with_chunk_offset() {
        let segments = parse_vtt_segments_with_offset(
            "call",
            "WEBVTT\n\n00:00:01.250 --> 00:00:03.500\nHello there.\n",
            600_000,
        )
        .unwrap();

        assert_eq!(segments[0].start_ms, 601_250);
        assert_eq!(segments[0].end_ms, 603_500);
        assert_eq!(segments[0].track, "call");
    }

    #[test]
    fn formats_timestamps() {
        assert_eq!(format_timestamp(65_432), "01:05.432");
    }

    #[test]
    fn clean_conversation_suppresses_duplicate_mic_bleed() {
        let segments = vec![
            TranscriptSegment {
                start_ms: 1_000,
                end_ms: 4_000,
                track: "call",
                text: "We got coffee and then went to the flower store.".to_string(),
            },
            TranscriptSegment {
                start_ms: 1_100,
                end_ms: 4_100,
                track: "mic",
                text: "we got coffee and then went to the flower store".to_string(),
            },
        ];

        let (clean, suppressed_count) = clean_conversation_segments(&segments);

        assert_eq!(suppressed_count, 1);
        assert_eq!(clean.len(), 1);
        assert_eq!(clean[0].track, "call");
    }

    #[test]
    fn clean_conversation_keeps_mic_segments_with_local_speech() {
        let segments = vec![
            TranscriptSegment {
                start_ms: 1_000,
                end_ms: 8_000,
                track: "call",
                text: "Well I went with my son and we got coffee.".to_string(),
            },
            TranscriptSegment {
                start_ms: 1_000,
                end_ms: 8_000,
                track: "mic",
                text: "Okay can you hear me now tell me a story about what you did today well I went with my son.".to_string(),
            },
        ];

        let (clean, suppressed_count) = clean_conversation_segments(&segments);

        assert_eq!(suppressed_count, 0);
        assert_eq!(clean.len(), 2);
        assert_eq!(
            clean[1].text,
            "Okay can you hear me now tell me a story about what you did today"
        );
    }

    #[test]
    fn clean_conversation_compares_mic_against_overlapping_call_context() {
        let segments = vec![
            TranscriptSegment {
                start_ms: 10_000,
                end_ms: 12_000,
                track: "call",
                text: "He bought me a beautiful plant.".to_string(),
            },
            TranscriptSegment {
                start_ms: 12_000,
                end_ms: 16_000,
                track: "call",
                text: "Then we drove back and made supper.".to_string(),
            },
            TranscriptSegment {
                start_ms: 11_000,
                end_ms: 15_000,
                track: "mic",
                text: "a beautiful plant then we drove back and made supper".to_string(),
            },
        ];

        let (clean, suppressed_count) = clean_conversation_segments(&segments);

        assert_eq!(suppressed_count, 1);
        assert_eq!(clean.len(), 2);
        assert!(clean.iter().all(|segment| segment.track == "call"));
    }

    #[test]
    fn clean_conversation_removes_call_words_from_mixed_mic_segments() {
        let segments = vec![
            TranscriptSegment {
                start_ms: 1_000,
                end_ms: 4_000,
                track: "call",
                text: "Then we drove back and made supper.".to_string(),
            },
            TranscriptSegment {
                start_ms: 1_000,
                end_ms: 5_000,
                track: "mic",
                text: "Then we drove back and made supper. Cool. Thank you. Hang on before we end."
                    .to_string(),
            },
        ];

        let (clean, suppressed_count) = clean_conversation_segments(&segments);

        assert_eq!(suppressed_count, 0);
        assert_eq!(clean.len(), 2);
        assert_eq!(clean[1].text, "Cool. Thank you. Hang on before we end.");
    }

    #[test]
    fn clean_conversation_suppresses_repeated_mic_loops() {
        let segments = vec![
            TranscriptSegment {
                start_ms: 1_000,
                end_ms: 5_000,
                track: "mic",
                text: "I'm going to put it on the other side.".to_string(),
            },
            TranscriptSegment {
                start_ms: 8_000,
                end_ms: 12_000,
                track: "mic",
                text: "I'm going to put it on the other side.".to_string(),
            },
            TranscriptSegment {
                start_ms: 15_000,
                end_ms: 18_000,
                track: "mic",
                text: "All right, should I watch this video?".to_string(),
            },
        ];

        let (clean, suppressed_count) = clean_conversation_segments(&segments);

        assert_eq!(suppressed_count, 1);
        assert_eq!(clean.len(), 2);
        assert_eq!(clean[0].text, "I'm going to put it on the other side.");
        assert_eq!(clean[1].text, "All right, should I watch this video?");
    }

    #[test]
    fn clean_conversation_suppresses_short_mic_fillers_near_call_audio() {
        let segments = vec![
            TranscriptSegment {
                start_ms: 1_000,
                end_ms: 5_000,
                track: "call",
                text: "We should go get ice cream.".to_string(),
            },
            TranscriptSegment {
                start_ms: 2_000,
                end_ms: 3_000,
                track: "mic",
                text: "Yeah.".to_string(),
            },
        ];

        let (clean, suppressed_count) = clean_conversation_segments(&segments);

        assert_eq!(suppressed_count, 1);
        assert_eq!(clean.len(), 1);
        assert_eq!(clean[0].track, "call");
    }

    #[test]
    fn primary_transcript_excludes_debug_sections() {
        let segments = vec![TranscriptSegment {
            start_ms: 1_000,
            end_ms: 2_000,
            track: "call",
            text: "Clean line.".to_string(),
        }];

        let markdown = super::transcript_markdown("Test", Path::new("models/test.bin"), &segments);

        assert!(markdown.contains("## Clean Conversation"));
        assert!(markdown.contains("Clean line."));
        assert!(!markdown.contains("## Combined Timeline"));
        assert!(!markdown.contains("## Call Audio"));
        assert!(!markdown.contains("## Microphone"));
    }
}
