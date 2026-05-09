use std::env;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Deserialize;

use crate::session::{default_storage_dir, list_sessions};

#[derive(Debug, Clone)]
pub struct TranscribeOptions {
    pub target: TranscribeTarget,
    pub track: TrackSelection,
    pub storage_dir: Option<PathBuf>,
    pub model_path: Option<PathBuf>,
    pub whisper_bin: Option<PathBuf>,
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

pub fn transcribe(options: &TranscribeOptions) -> io::Result<TranscribeResult> {
    let session_path = resolve_session_path(options)?;
    let ffmpeg = find_required_binary("ffmpeg", "Install ffmpeg with `brew install ffmpeg`.")?;
    let whisper = resolve_whisper_binary(options)?;
    let model = resolve_model_path(options)?;
    let title = read_session_title(&session_path)?;
    let work_dir = session_path.join("transcription-work");
    fs::create_dir_all(&work_dir)?;

    let mut sections = Vec::new();
    let mut segments = Vec::new();
    let mut track_results = Vec::new();

    for track in options.track.tracks() {
        let audio_path = session_path.join("audio").join(track.file_name());
        if !audio_path.exists() {
            continue;
        }

        let wav_path = work_dir.join(format!("{}.wav", track.label()));
        convert_to_wav(&ffmpeg, &audio_path, &wav_path)?;

        let output_base = work_dir.join(track.label());
        run_whisper(&whisper, &model, &wav_path, &output_base)?;
        let text = read_whisper_output(&output_base, &wav_path)?;
        let clean_text = text.trim();
        segments.extend(read_whisper_segments(&output_base, track.label()).unwrap_or_default());

        sections.push(format!(
            "## {}\n\nSource: `audio/{}`\n\n{}\n",
            track.heading(),
            track.file_name(),
            if clean_text.is_empty() {
                "_No transcript text returned._"
            } else {
                clean_text
            }
        ));
        track_results.push(TrackResult {
            label: track.label(),
            audio_path,
            text_len: clean_text.len(),
        });

        if !options.keep_wav {
            let _ = fs::remove_file(&wav_path);
        }
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
    fs::write(
        &transcript_path,
        transcript_markdown(&title, &model, &segments, &sections),
    )?;

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

fn convert_to_wav(ffmpeg: &Path, input: &Path, output: &Path) -> io::Result<()> {
    let status = Command::new(ffmpeg)
        .arg("-y")
        .arg("-i")
        .arg(input)
        .arg("-ar")
        .arg("16000")
        .arg("-ac")
        .arg("1")
        .arg("-c:a")
        .arg("pcm_s16le")
        .arg(output)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;

    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "ffmpeg failed to convert {}",
            input.display()
        )))
    }
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
) -> io::Result<Vec<TranscriptSegment>> {
    let vtt_path = path_with_added_extension(output_base, "vtt");
    if !vtt_path.exists() {
        return Ok(Vec::new());
    }

    parse_vtt_segments(track, &fs::read_to_string(vtt_path)?)
}

fn parse_vtt_segments(track: &'static str, content: &str) -> io::Result<Vec<TranscriptSegment>> {
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

        let text = text_lines.join(" ").trim().to_string();
        if !text.is_empty() {
            segments.push(TranscriptSegment {
                start_ms: start,
                end_ms: end,
                track,
                text,
            });
        }
    }

    Ok(segments)
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

fn transcript_markdown(
    title: &str,
    model: &Path,
    segments: &[TranscriptSegment],
    sections: &[String],
) -> String {
    let timeline = combined_timeline_markdown(segments);
    format!(
        "# Transcript: {title}\n\nGenerated by local Whisper transcription at Unix time {}.\n\nModel: `{}`\n\n{}{}\n",
        unix_timestamp(),
        model.display(),
        timeline,
        sections.join("\n")
    )
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
    use super::{format_timestamp, parse_vtt_segments, path_with_added_extension, TrackSelection};
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
        let segments = parse_vtt_segments(
            "mic",
            "WEBVTT\n\n00:00:01.250 --> 00:00:03.500\nHello there.\n\n00:00:04.000 --> 00:00:05.000\nNext line.\n",
        )
        .unwrap();

        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].start_ms, 1250);
        assert_eq!(segments[0].end_ms, 3500);
        assert_eq!(segments[0].track, "mic");
        assert_eq!(segments[0].text, "Hello there.");
    }

    #[test]
    fn formats_timestamps() {
        assert_eq!(format_timestamp(65_432), "01:05.432");
    }
}
