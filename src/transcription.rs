use std::env;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::session::{
    default_storage_dir, list_sessions, mark_transcript_ready, read_session_title,
    transcription_dir, transcription_work_dir,
};

pub const TRANSCRIPTION_CHUNK_SECONDS: u64 = 600;
pub const DEFAULT_PARAKEET_BIN: &str = "parakeet-mlx";
pub const DEFAULT_PARAKEET_MODEL: &str = "mlx-community/parakeet-tdt-0.6b-v3";
pub const PARAKEET_CHUNK_SECONDS: u64 = 120;
pub const PARAKEET_OVERLAP_SECONDS: u64 = 15;
pub const PARAKEET_INSTALL_HINT: &str =
    "Install with `uv tool install parakeet-mlx`. Whisper fallback: --engine whisper";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptionEngine {
    Whisper,
    Parakeet,
}

impl Default for TranscriptionEngine {
    fn default() -> Self {
        default_transcription_engine()
    }
}

pub fn default_transcription_engine() -> TranscriptionEngine {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        TranscriptionEngine::Parakeet
    } else {
        TranscriptionEngine::Whisper
    }
}

impl TranscriptionEngine {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "whisper" => Some(Self::Whisper),
            "parakeet" => Some(Self::Parakeet),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Whisper => "whisper",
            Self::Parakeet => "parakeet",
        }
    }

    fn display_name(self) -> &'static str {
        match self {
            Self::Whisper => "Whisper",
            Self::Parakeet => "Parakeet",
        }
    }
}

pub fn engine_status_parts(
    engine: TranscriptionEngine,
    parakeet_model: Option<&str>,
    whisper_model: Option<&Path>,
) -> (String, String) {
    let model = match engine {
        TranscriptionEngine::Parakeet => {
            let env_model = env_parakeet_model();
            short_model_label(&resolve_parakeet_model_id(
                parakeet_model,
                env_model.as_deref(),
            ))
        }
        TranscriptionEngine::Whisper => whisper_model
            .and_then(|path| path.file_name())
            .and_then(|name| name.to_str())
            .map(ToString::to_string)
            .unwrap_or_else(|| "ggml (auto)".to_string()),
    };
    (engine.as_str().to_string(), model)
}

fn short_model_label(model: &str) -> String {
    model
        .rsplit(['/', '\\'])
        .next()
        .filter(|value| !value.is_empty())
        .unwrap_or(model)
        .to_string()
}

#[derive(Debug, Clone)]
pub struct TranscribeOptions {
    pub target: TranscribeTarget,
    pub track: TrackSelection,
    pub storage_dir: Option<PathBuf>,
    pub engine: TranscriptionEngine,
    pub ffmpeg_bin: Option<PathBuf>,
    pub model_path: Option<PathBuf>,
    pub whisper_bin: Option<PathBuf>,
    pub parakeet_bin: Option<PathBuf>,
    pub parakeet_model: Option<String>,
    pub parakeet_cache_dir: Option<PathBuf>,
    pub chunk_seconds: u64,
    pub keep_wav: bool,
    pub require_parakeet: bool,
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
        engine: TranscriptionEngine,
        model: String,
        note: Option<String>,
    },
    TrackStarted {
        track: &'static str,
        chunks: usize,
        elapsed_secs: u64,
    },
    ChunkStarted {
        track: &'static str,
        index: usize,
        total: usize,
        elapsed_secs: u64,
    },
    TrackFinished {
        track: &'static str,
        text_len: usize,
        chunks: usize,
        elapsed_secs: u64,
    },
    Finished {
        transcript_path: PathBuf,
        elapsed_secs: u64,
    },
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoctorCheckLevel {
    Ok,
    Warn,
    Miss,
}

#[derive(Debug, Clone)]
enum AsrEngine {
    Whisper {
        bin: PathBuf,
        model: PathBuf,
        fallback_from_parakeet: bool,
    },
    Parakeet {
        bin: PathBuf,
        model: String,
        cache_dir: Option<PathBuf>,
    },
}

impl AsrEngine {
    fn kind(&self) -> TranscriptionEngine {
        match self {
            Self::Whisper { .. } => TranscriptionEngine::Whisper,
            Self::Parakeet { .. } => TranscriptionEngine::Parakeet,
        }
    }

    fn model_label(&self) -> String {
        match self {
            Self::Whisper { model, .. } => model.display().to_string(),
            Self::Parakeet { model, .. } => model.clone(),
        }
    }

    fn fallback_note(&self) -> Option<String> {
        match self {
            Self::Whisper {
                fallback_from_parakeet: true,
                ..
            } => Some(format!(
                "Parakeet CLI missing; falling back to Whisper. {PARAKEET_INSTALL_HINT}"
            )),
            _ => None,
        }
    }

    fn transcribe_chunk(&self, wav: &Path, output_base: &Path) -> io::Result<()> {
        match self {
            Self::Whisper { bin, model, .. } => run_whisper(bin, model, wav, output_base),
            Self::Parakeet {
                bin,
                model,
                cache_dir,
            } => {
                let output_dir = output_base.parent().unwrap_or(Path::new("."));
                run_parakeet(
                    bin,
                    model,
                    &[wav.to_path_buf()],
                    output_dir,
                    cache_dir.as_deref(),
                )
            }
        }
    }

    fn transcribe_track(&self, chunks: &[AudioChunk]) -> io::Result<()> {
        match self {
            Self::Whisper { .. } => Ok(()),
            Self::Parakeet {
                bin,
                model,
                cache_dir,
            } => {
                let Some(first) = chunks.first() else {
                    return Ok(());
                };
                let output_dir = first.output_base.parent().unwrap_or(Path::new("."));
                let wavs = chunks
                    .iter()
                    .map(|chunk| chunk.wav_path.clone())
                    .collect::<Vec<_>>();
                run_parakeet(bin, model, &wavs, output_dir, cache_dir.as_deref())
            }
        }
    }

    fn read_text(&self, output_base: &Path, wav: &Path) -> io::Result<String> {
        match self {
            Self::Whisper { .. } => {
                let text = read_whisper_output(output_base, wav)?;
                Ok(clean_whisper_text_block(&text))
            }
            Self::Parakeet { .. } => Ok(clean_whisper_text_block(&read_parakeet_text(
                output_base,
                wav,
            )?)),
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
    let started = Instant::now();
    let mut stages: Vec<(String, Duration)> = Vec::new();
    let mut stage_started = started;
    let mark_stage =
        |name: &str, stages: &mut Vec<(String, Duration)>, stage_started: &mut Instant| {
            let now = Instant::now();
            stages.push((
                name.to_string(),
                now.saturating_duration_since(*stage_started),
            ));
            *stage_started = now;
        };
    let elapsed_secs = || started.elapsed().as_secs();

    let session_path = resolve_session_path(options)?;
    let ffmpeg = resolve_ffmpeg_binary(options)?;
    let engine = resolve_asr_engine(options)?;
    let title = read_session_title(&session_path)?;
    let work_dir = transcription_work_dir(&session_path);
    fs::create_dir_all(&work_dir)?;
    mark_stage("setup", &mut stages, &mut stage_started);

    let mut start_note = engine.fallback_note();
    if matches!(engine.kind(), TranscriptionEngine::Parakeet)
        && !parakeet_model_is_cached(
            &engine.model_label(),
            match &engine {
                AsrEngine::Parakeet { cache_dir, .. } => cache_dir.as_deref(),
                AsrEngine::Whisper { .. } => None,
            },
        )
    {
        let download =
            "Parakeet model is not cached yet; first run may download weights from Hugging Face."
                .to_string();
        start_note = Some(match start_note {
            Some(existing) => format!("{existing} {download}"),
            None => download,
        });
    }

    progress(TranscriptionProgress::Started {
        session_path: session_path.clone(),
        engine: engine.kind(),
        model: engine.model_label(),
        note: start_note,
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
        mark_stage(
            &format!("{} ffmpeg", track.label()),
            &mut stages,
            &mut stage_started,
        );
        let mut track_text_parts = Vec::new();

        progress(TranscriptionProgress::TrackStarted {
            track: track.label(),
            chunks: chunks.len(),
            elapsed_secs: elapsed_secs(),
        });

        match engine.kind() {
            TranscriptionEngine::Parakeet => {
                progress(TranscriptionProgress::ChunkStarted {
                    track: track.label(),
                    index: 1,
                    total: chunks.len(),
                    elapsed_secs: elapsed_secs(),
                });
                engine.transcribe_track(&chunks)?;
                mark_stage(
                    &format!("{} parakeet-mlx ({} file(s))", track.label(), chunks.len()),
                    &mut stages,
                    &mut stage_started,
                );
            }
            TranscriptionEngine::Whisper => {
                for (index, chunk) in chunks.iter().enumerate() {
                    progress(TranscriptionProgress::ChunkStarted {
                        track: track.label(),
                        index: index + 1,
                        total: chunks.len(),
                        elapsed_secs: elapsed_secs(),
                    });
                    engine.transcribe_chunk(&chunk.wav_path, &chunk.output_base)?;
                    mark_stage(
                        &format!(
                            "{} whisper chunk {}/{}",
                            track.label(),
                            index + 1,
                            chunks.len()
                        ),
                        &mut stages,
                        &mut stage_started,
                    );
                }
            }
        }

        for chunk in &chunks {
            let text = engine.read_text(&chunk.output_base, &chunk.wav_path)?;
            if !text.is_empty() {
                track_text_parts.push(text);
            }
            segments.extend(
                read_vtt_segments(
                    &chunk.output_base,
                    &chunk.wav_path,
                    track.label(),
                    chunk.start_ms,
                )
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
            elapsed_secs: elapsed_secs(),
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
    let debug_dir = transcription_dir(&session_path);
    write_transcription_outputs(
        &transcript_path,
        &debug_dir,
        &title,
        engine.kind(),
        &engine.model_label(),
        &segments,
        &sections,
    )?;
    mark_stage("write outputs", &mut stages, &mut stage_started);
    write_transcription_timing(
        &debug_dir,
        engine.kind(),
        &engine.model_label(),
        &stages,
        started.elapsed(),
    )?;
    mark_transcript_ready(&session_path)?;

    progress(TranscriptionProgress::Finished {
        transcript_path: transcript_path.clone(),
        elapsed_secs: elapsed_secs(),
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

fn resolve_asr_engine(options: &TranscribeOptions) -> io::Result<AsrEngine> {
    match options.engine {
        TranscriptionEngine::Whisper => Ok(AsrEngine::Whisper {
            bin: resolve_whisper_binary(options)?,
            model: resolve_model_path(options)?,
            fallback_from_parakeet: false,
        }),
        TranscriptionEngine::Parakeet => {
            if let Some(bin) = find_parakeet_binary(options.parakeet_bin.as_deref())
                .filter(|path| parakeet_binary_is_present(path))
            {
                return Ok(AsrEngine::Parakeet {
                    bin,
                    model: resolve_parakeet_model(options),
                    cache_dir: resolve_parakeet_cache_dir(options),
                });
            }

            if options.require_parakeet {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!(
                        "Missing Parakeet CLI (`{DEFAULT_PARAKEET_BIN}`). {PARAKEET_INSTALL_HINT}"
                    ),
                ));
            }

            match (
                resolve_whisper_binary(options),
                resolve_model_path(options),
            ) {
                (Ok(bin), Ok(model)) => Ok(AsrEngine::Whisper {
                    bin,
                    model,
                    fallback_from_parakeet: true,
                }),
                _ => Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!(
                        "Missing Parakeet CLI (`{DEFAULT_PARAKEET_BIN}`) and Whisper is not available either. {PARAKEET_INSTALL_HINT}"
                    ),
                )),
            }
        }
    }
}

pub fn find_parakeet_binary(explicit: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = explicit {
        return Some(path.to_path_buf());
    }

    if let Some(path) = env::var_os("RECALL_PARAKEET_BIN").map(PathBuf::from) {
        return Some(path);
    }

    find_binary(DEFAULT_PARAKEET_BIN)
}

pub fn resolve_parakeet_model_id(explicit: Option<&str>, configured: Option<&str>) -> String {
    if let Some(model) = explicit.filter(|value| !value.trim().is_empty()) {
        return model.to_string();
    }
    if let Some(model) = configured.filter(|value| !value.trim().is_empty()) {
        return model.to_string();
    }
    DEFAULT_PARAKEET_MODEL.to_string()
}

fn env_parakeet_model() -> Option<String> {
    env::var("RECALL_PARAKEET_MODEL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn resolve_parakeet_model(options: &TranscribeOptions) -> String {
    resolve_parakeet_model_id(
        options.parakeet_model.as_deref(),
        env_parakeet_model().as_deref(),
    )
}

pub fn resolve_parakeet_cache_dir_for_options(
    explicit: Option<&Path>,
    configured: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(path) = explicit {
        return Some(path.to_path_buf());
    }
    if let Some(path) = env::var_os("RECALL_PARAKEET_CACHE").map(PathBuf::from) {
        return Some(path);
    }
    if let Some(path) = configured {
        return Some(path.to_path_buf());
    }

    let local_candidates = [
        PathBuf::from("models/parakeet"),
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("models/parakeet"),
    ];
    if let Some(path) = local_candidates.into_iter().find(|path| path.exists()) {
        return Some(path);
    }

    if let Some(home) = env::var_os("HOME").map(PathBuf::from) {
        let default = home.join("Library/Application Support/recall/models/parakeet");
        if default.exists() {
            return Some(default);
        }
    }

    None
}

fn resolve_parakeet_cache_dir(options: &TranscribeOptions) -> Option<PathBuf> {
    resolve_parakeet_cache_dir_for_options(options.parakeet_cache_dir.as_deref(), None)
}

pub fn parakeet_binary_doctor_level(
    explicit: Option<&Path>,
    selected_engine: TranscriptionEngine,
) -> DoctorCheckLevel {
    match find_parakeet_binary(explicit) {
        Some(path) if parakeet_binary_is_present(&path) => DoctorCheckLevel::Ok,
        _ if selected_engine == TranscriptionEngine::Parakeet => DoctorCheckLevel::Miss,
        _ => DoctorCheckLevel::Warn,
    }
}

fn parakeet_binary_is_present(path: &Path) -> bool {
    path.exists()
        || (path.components().count() == 1 && find_binary(&path.to_string_lossy()).is_some())
}

pub fn parakeet_model_is_cached(model: &str, cache_dir: Option<&Path>) -> bool {
    parakeet_model_cache_roots(cache_dir)
        .into_iter()
        .any(|root| huggingface_snapshot_present(&root, model))
}

fn parakeet_model_cache_roots(cache_dir: Option<&Path>) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(dir) = cache_dir {
        roots.push(dir.to_path_buf());
        roots.push(dir.join("hub"));
    }
    if let Some(hf_home) = env::var_os("HF_HOME").map(PathBuf::from) {
        roots.push(hf_home.join("hub"));
        roots.push(hf_home);
    }
    if let Some(home) = env::var_os("HOME").map(PathBuf::from) {
        roots.push(home.join("Library/Application Support/recall/models/parakeet"));
        roots.push(home.join(".cache/huggingface/hub"));
        roots.push(home.join(".cache/huggingface"));
    }
    roots
}

fn huggingface_snapshot_present(root: &Path, model: &str) -> bool {
    let encoded = format!("models--{}", model.replace('/', "--"));
    let snapshots = root.join(encoded).join("snapshots");
    snapshots.is_dir()
        && fs::read_dir(&snapshots)
            .map(|entries| {
                entries.filter_map(Result::ok).any(|entry| {
                    let path = entry.path();
                    path.is_dir()
                        && (path.join("config.json").exists()
                            || path.join("model.safetensors").exists()
                            || fs::read_dir(&path)
                                .map(|mut files| files.next().is_some())
                                .unwrap_or(false))
                })
            })
            .unwrap_or(false)
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

fn run_parakeet(
    parakeet: &Path,
    model: &str,
    wavs: &[PathBuf],
    output_dir: &Path,
    cache_dir: Option<&Path>,
) -> io::Result<()> {
    if wavs.is_empty() {
        return Ok(());
    }
    if let Some(cache_dir) = cache_dir {
        fs::create_dir_all(cache_dir)?;
    }

    let mut command = Command::new(parakeet);
    for wav in wavs {
        command.arg(wav);
    }
    command
        .arg("--output-format")
        .arg("vtt")
        .arg("--output-dir")
        .arg(output_dir)
        .arg("--model")
        .arg(model)
        .arg("--chunk-duration")
        .arg(PARAKEET_CHUNK_SECONDS.to_string())
        .arg("--overlap-duration")
        .arg(PARAKEET_OVERLAP_SECONDS.to_string())
        .stdin(Stdio::null());

    if let Some(cache_dir) = cache_dir {
        command.arg("--cache-dir").arg(cache_dir);
    }

    let output = command.output().map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("Missing Parakeet CLI (`{DEFAULT_PARAKEET_BIN}`). {PARAKEET_INSTALL_HINT}"),
            )
        } else {
            error
        }
    })?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut detail = [stderr.trim(), stdout.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if detail.len() > 4_000 {
        detail.truncate(4_000);
        detail.push('…');
    }
    let labels = wavs
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    if detail.is_empty() {
        Err(io::Error::other(format!(
            "Parakeet transcription failed for {labels}"
        )))
    } else {
        Err(io::Error::other(format!(
            "Parakeet transcription failed for {labels}: {detail}"
        )))
    }
}

fn read_vtt_segments(
    output_base: &Path,
    wav: &Path,
    track: &'static str,
    offset_ms: u64,
) -> io::Result<Vec<TranscriptSegment>> {
    let Some(vtt_path) = find_vtt_output(output_base, wav) else {
        return Ok(Vec::new());
    };

    parse_vtt_segments_with_offset(track, &fs::read_to_string(vtt_path)?, offset_ms)
}

fn read_parakeet_text(output_base: &Path, wav: &Path) -> io::Result<String> {
    let txt_candidates = [
        path_with_added_extension(output_base, "txt"),
        path_with_added_extension(wav, "txt"),
        wav.with_extension("txt"),
    ];
    for candidate in txt_candidates {
        if candidate.exists() {
            return fs::read_to_string(candidate);
        }
    }

    let Some(vtt_path) = find_vtt_output(output_base, wav) else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "Parakeet finished but no .vtt output file was found.",
        ));
    };

    let segments = parse_vtt_segments_with_offset("parakeet", &fs::read_to_string(vtt_path)?, 0)?;
    Ok(segments
        .into_iter()
        .map(|segment| segment.text)
        .collect::<Vec<_>>()
        .join("\n"))
}

fn find_vtt_output(output_base: &Path, wav: &Path) -> Option<PathBuf> {
    let candidates = [
        path_with_added_extension(output_base, "vtt"),
        wav.with_extension("vtt"),
        path_with_added_extension(wav, "vtt"),
    ];
    candidates.into_iter().find(|path| path.exists())
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

        let text = clean_transcript_text(&strip_vtt_markup(&text_lines.join(" ")));
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

fn strip_vtt_markup(text: &str) -> String {
    let mut stripped = String::with_capacity(text.len());
    let mut in_tag = false;
    for ch in text.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => stripped.push(ch),
            _ => {}
        }
    }
    stripped
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

fn write_transcription_outputs(
    transcript_path: &Path,
    debug_dir: &Path,
    title: &str,
    engine: TranscriptionEngine,
    model_label: &str,
    segments: &[TranscriptSegment],
    sections: &[String],
) -> io::Result<()> {
    if debug_dir.exists() {
        fs::remove_dir_all(debug_dir)?;
    }
    fs::create_dir_all(debug_dir)?;

    fs::write(
        transcript_path,
        transcript_markdown(title, engine, model_label, segments),
    )?;
    fs::write(
        debug_dir.join("combined-timeline.md"),
        combined_timeline_debug_markdown(title, engine, model_label, segments),
    )?;
    fs::write(
        debug_dir.join("raw-tracks.md"),
        raw_tracks_debug_markdown(title, engine, model_label, sections),
    )?;
    fs::write(
        debug_dir.join("full-debug-transcript.md"),
        full_debug_transcript_markdown(title, engine, model_label, segments, sections),
    )?;

    Ok(())
}

fn write_transcription_timing(
    debug_dir: &Path,
    engine: TranscriptionEngine,
    model_label: &str,
    stages: &[(String, Duration)],
    total: Duration,
) -> io::Result<()> {
    let mut markdown = format!(
        "# Transcription timing\n\nEngine: `{}`\nModel: `{model_label}`\n\n",
        engine.as_str()
    );
    for (name, duration) in stages {
        markdown.push_str(&format!("- {name}: {}\n", format_duration(*duration)));
    }
    markdown.push_str(&format!("- total: {}\n", format_duration(total)));
    fs::write(debug_dir.join("timing.md"), markdown)
}

fn format_duration(duration: Duration) -> String {
    let millis = duration.as_millis();
    if millis >= 10_000 {
        format!("{:.1}s", duration.as_secs_f64())
    } else {
        format!("{millis}ms")
    }
}

fn transcript_header(engine: TranscriptionEngine, model_label: &str) -> String {
    let mut header = format!(
        "Generated by local {} transcription at Unix time {}.\n\nEngine: `{}`\nModel: `{model_label}`",
        engine.display_name(),
        unix_timestamp(),
        engine.as_str(),
    );
    if let Some(line) = parakeet_attribution_line(engine, model_label) {
        header.push('\n');
        header.push_str(line);
    }
    header
}

fn parakeet_attribution_line(
    engine: TranscriptionEngine,
    model_label: &str,
) -> Option<&'static str> {
    if !matches!(engine, TranscriptionEngine::Parakeet) {
        return None;
    }
    let id = model_label.trim();
    if id.contains("parakeet-tdt-0.6b-v3") {
        Some("Attribution: NVIDIA Parakeet TDT 0.6B v3 (CC-BY-4.0)")
    } else if id.starts_with("nvidia/parakeet") || id.starts_with("mlx-community/parakeet") {
        Some("Attribution: NVIDIA Parakeet weights (CC-BY-4.0)")
    } else {
        None
    }
}

fn transcript_markdown(
    title: &str,
    engine: TranscriptionEngine,
    model_label: &str,
    segments: &[TranscriptSegment],
) -> String {
    let clean_timeline = clean_conversation_markdown(segments);
    format!(
        "# Transcript: {title}\n\n{}\n\n{}",
        transcript_header(engine, model_label),
        if clean_timeline.is_empty() {
            "_No clean transcript text returned._\n"
        } else {
            &clean_timeline
        }
    )
}

fn combined_timeline_debug_markdown(
    title: &str,
    engine: TranscriptionEngine,
    model_label: &str,
    segments: &[TranscriptSegment],
) -> String {
    format!(
        "# Combined Timeline Debug: {title}\n\n{}\n\n{}",
        transcript_header(engine, model_label),
        combined_timeline_markdown(segments)
    )
}

fn raw_tracks_debug_markdown(
    title: &str,
    engine: TranscriptionEngine,
    model_label: &str,
    sections: &[String],
) -> String {
    format!(
        "# Raw Track Transcripts: {title}\n\n{}\n\n{}\n",
        transcript_header(engine, model_label),
        sections.join("\n")
    )
}

fn full_debug_transcript_markdown(
    title: &str,
    engine: TranscriptionEngine,
    model_label: &str,
    segments: &[TranscriptSegment],
    sections: &[String],
) -> String {
    let clean_timeline = clean_conversation_markdown(segments);
    let timeline = combined_timeline_markdown(segments);
    format!(
        "# Full Debug Transcript: {title}\n\n{}\n\n{}{}{}\n",
        transcript_header(engine, model_label),
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
    // Thresholds were tuned on Whisper segment sizes. Parakeet sentence cues can
    // be longer or shorter; re-check mic bleed on a real dual-track call before
    // tightening.
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
        format_timestamp, parakeet_binary_doctor_level, parse_vtt_segments_with_offset,
        path_with_added_extension, resolve_parakeet_model_id, DoctorCheckLevel, TrackSelection,
        TranscriptSegment, TranscriptionEngine, DEFAULT_PARAKEET_MODEL,
    };
    use std::path::Path;
    use std::time::Duration;

    #[test]
    fn engine_status_parts_shorten_model_ids() {
        let (engine, model) = super::engine_status_parts(TranscriptionEngine::Parakeet, None, None);
        assert_eq!(engine, "parakeet");
        assert_eq!(model, "parakeet-tdt-0.6b-v3");

        let (engine, model) = super::engine_status_parts(
            TranscriptionEngine::Whisper,
            None,
            Some(Path::new(
                "/Users/me/Models/whisper.cpp/ggml-large-v3-turbo.bin",
            )),
        );
        assert_eq!(engine, "whisper");
        assert_eq!(model, "ggml-large-v3-turbo.bin");
    }

    #[test]
    fn parakeet_is_the_default_transcription_engine() {
        assert_eq!(
            TranscriptionEngine::default(),
            TranscriptionEngine::Parakeet
        );
        assert_eq!(
            TranscriptionEngine::parse("whisper"),
            Some(TranscriptionEngine::Whisper)
        );
        assert_eq!(
            TranscriptionEngine::parse("PARAKEET"),
            Some(TranscriptionEngine::Parakeet)
        );
        assert!(TranscriptionEngine::parse("nemo").is_none());
        assert_eq!(TranscriptionEngine::Whisper.as_str(), "whisper");
    }

    #[test]
    fn parakeet_model_defaults_to_mlx_v3() {
        assert_eq!(
            resolve_parakeet_model_id(None, None),
            DEFAULT_PARAKEET_MODEL
        );
        assert_eq!(
            resolve_parakeet_model_id(Some("mlx-community/custom"), None),
            "mlx-community/custom"
        );
    }

    #[test]
    fn missing_parakeet_binary_is_a_miss_when_it_is_the_selected_engine() {
        let missing = Path::new("/definitely-missing-recall-parakeet-mlx");
        assert_eq!(
            parakeet_binary_doctor_level(Some(missing), TranscriptionEngine::Parakeet),
            DoctorCheckLevel::Miss
        );
        assert_eq!(
            parakeet_binary_doctor_level(Some(missing), TranscriptionEngine::Whisper),
            DoctorCheckLevel::Warn
        );
    }

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
    fn parses_parakeet_sentence_vtt_with_chunk_offset() {
        let fixture = "WEBVTT\n\n\
00:00:00.080 --> 00:00:02.320\n\
Hello there, this is a test.\n\n\
00:00:02.400 --> 00:00:04.160\n\
How are you today?\n";
        let segments = parse_vtt_segments_with_offset("call", fixture, 600_000).unwrap();

        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].start_ms, 600_080);
        assert_eq!(segments[0].end_ms, 602_320);
        assert_eq!(segments[0].text, "Hello there, this is a test.");
        assert_eq!(segments[1].start_ms, 602_400);
        assert_eq!(segments[1].text, "How are you today?");
    }

    #[test]
    fn parses_parakeet_word_highlight_vtt() {
        let fixture = "WEBVTT\n\n\
00:00:00.080 --> 00:00:00.280\n\
<b>Hello</b> there.\n\n\
00:00:00.280 --> 00:00:00.520\n\
Hello <b>there.</b>\n";
        let segments = parse_vtt_segments_with_offset("mic", fixture, 1_000).unwrap();

        assert_eq!(segments.len(), 2);
        assert_eq!(segments[0].start_ms, 1_080);
        assert_eq!(segments[0].text, "Hello there.");
        assert_eq!(segments[1].text, "Hello there.");
    }

    #[test]
    fn formats_timestamps() {
        assert_eq!(format_timestamp(65_432), "01:05.432");
        assert_eq!(super::format_duration(Duration::from_millis(250)), "250ms");
        assert_eq!(super::format_duration(Duration::from_secs(12)), "12.0s");
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

        let markdown = super::transcript_markdown(
            "Test",
            TranscriptionEngine::Whisper,
            "models/test.bin",
            &segments,
        );

        assert!(markdown.contains("## Clean Conversation"));
        assert!(markdown.contains("Clean line."));
        assert!(markdown.contains("Engine: `whisper`"));
        assert!(markdown.contains("Model: `models/test.bin`"));
        assert!(!markdown.contains("## Combined Timeline"));
        assert!(!markdown.contains("## Call Audio"));
        assert!(!markdown.contains("## Microphone"));
        assert!(!markdown.contains("CC-BY-4.0"));
    }

    #[test]
    fn parakeet_transcript_header_includes_engine_and_attribution() {
        let segments = vec![TranscriptSegment {
            start_ms: 1_000,
            end_ms: 2_000,
            track: "call",
            text: "Clean line.".to_string(),
        }];

        let markdown = super::transcript_markdown(
            "Test",
            TranscriptionEngine::Parakeet,
            DEFAULT_PARAKEET_MODEL,
            &segments,
        );

        assert!(markdown.contains("Engine: `parakeet`"));
        assert!(markdown.contains(DEFAULT_PARAKEET_MODEL));
        assert!(markdown.contains("NVIDIA Parakeet TDT 0.6B v3 (CC-BY-4.0)"));
    }

    #[test]
    fn parakeet_attribution_does_not_pin_v3_for_other_model_ids() {
        let segments = vec![TranscriptSegment {
            start_ms: 1_000,
            end_ms: 2_000,
            track: "call",
            text: "Clean line.".to_string(),
        }];

        let markdown = super::transcript_markdown(
            "Test",
            TranscriptionEngine::Parakeet,
            "acme/custom-asr",
            &segments,
        );

        assert!(!markdown.contains("CC-BY-4.0"));
        assert!(!markdown.contains("NVIDIA Parakeet"));
    }
}
