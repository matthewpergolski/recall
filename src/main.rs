mod analysis;
mod audio;
mod capture_sources;
mod config;
mod mic_recorder;
mod session;
mod system_recorder;
mod transcription;
mod tui;
mod update;

use std::env;
use std::path::{Path, PathBuf};

use analysis::{analyze, known_agents, AnalyzeOptions, AnalyzeTarget};
use capture_sources::{detect_sources, probe_audio_tap};
use config::{config_path, RecallConfig};
use session::{
    default_storage_dir, export_session, latest_session, list_sessions, open_path,
    primary_document_path, start_session, ConsentMode, StartOptions,
};
use transcription::{
    find_parakeet_binary, format_bytes, format_model_download_label, parakeet_binary_doctor_level,
    parakeet_model_is_cached, resolve_parakeet_cache_dir_for_options, resolve_parakeet_model_id,
    transcribe_with_progress, DoctorCheckLevel, TrackSelection, TranscribeOptions,
    TranscribeTarget, TranscriptionEngine, TranscriptionProgress, DEFAULT_PARAKEET_BIN,
    PARAKEET_INSTALL_HINT,
};
use tui::TuiOptions;
use update::{update, UpdateOptions};

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() {
    let raw_args = env::args().skip(1).collect::<Vec<_>>();
    let (tui_defaults, args) = match parse_leading_tui_defaults(raw_args) {
        Ok(parsed) => parsed,
        Err(message) => {
            eprintln!("{message}");
            eprintln!();
            print_help();
            std::process::exit(2);
        }
    };
    let mut args = args.into_iter();
    let command = args.next();

    match command.as_deref() {
        Some("start") => run_start(args.collect(), &tui_defaults),
        Some("list") => run_list(args.collect(), &tui_defaults),
        Some("show") => run_show(args.collect(), &tui_defaults),
        Some("open") => run_open(args.collect(), &tui_defaults),
        Some("export") => run_export(args.collect(), &tui_defaults),
        Some("sources") => run_sources(),
        Some("audio-tap-probe") => run_audio_tap_probe(),
        Some("transcribe") => run_transcribe(args.collect(), &tui_defaults),
        Some("analyze") => run_analyze(args.collect(), &tui_defaults),
        Some("agents") => run_agents(args.collect()),
        Some("update") => run_update(args.collect()),
        Some("doctor") => print_doctor(),
        Some("spec") => print_spec_hint(),
        Some("-h") | Some("--help") | Some("help") => print_help(),
        Some("-V") | Some("--version") | Some("version") => println!("recall {VERSION}"),
        Some(other) => {
            eprintln!("Unknown command: {other}");
            eprintln!();
            print_help();
            std::process::exit(2);
        }
        None => {
            run_tui_with_options(tui_defaults);
        }
    }
}

fn print_help() {
    println!(
        r#"Recall {VERSION}

USAGE:
    recall                                Open the interactive Recall TUI
    recall --consent provided             Open TUI with consent already marked
    recall --title "Project sync"         Open TUI with a session title
    recall start --title "Design Sync"    Create a local session folder
    recall list                           List local sessions
    recall show latest                    Show the latest session path
    recall open latest                    Open the latest meeting document
    recall open latest --dir              Open the latest session folder
    recall export latest                  Build one portable Markdown export
    recall sources                        List detected app and microphone sources
    recall audio-tap-probe                Probe CoreAudio process-tap availability
    recall transcribe latest              Transcribe the newest session locally
    recall analyze latest --agent grok    Generate summary/actions with an agent
    recall agents list                    List supported headless agent profiles
    recall update                         Safely update and reinstall Recall
    recall doctor                         Check local development prerequisites
    recall spec                           Show where the product spec lives

START OPTIONS:
    --title <title>                       Meeting title
    --consent <mode>                      provided, noted, verbal, written, policy, none
    --storage <path>                      Session storage directory

TRANSCRIBE OPTIONS:
    recall transcribe latest [options]
    recall transcribe <session-path> [options]
    --engine <whisper|parakeet>           ASR engine, default: parakeet on Apple Silicon
    --track <both|call|mic>               Audio track selection, default: both
    --ffmpeg <path>                       ffmpeg binary path
    --model <path>                        Whisper ggml model path
    --whisper <path>                      whisper-cli binary path
    --parakeet <path>                     parakeet-mlx binary path
    --parakeet-model <id>                 Parakeet Hugging Face model id
    --parakeet-cache-dir <path>           Parakeet Hugging Face cache directory
    --storage <path>                      Storage directory for latest lookup
    --chunk-seconds <seconds>             Transcription chunk size, default: 600
    --keep-wav                            Keep temporary converted WAV files

ANALYZE OPTIONS:
    recall analyze latest [options]
    recall analyze <session-path> [options]
    --agent <grok|cline|codex|claude|opencode|pi>  Headless agent profile to run
    --preset <general|work|personal>      Analysis prompt preset, default: general
    --storage <path>                      Storage directory for latest lookup
    --dry-run                             Write analysis prompt without running agent

TUI ANALYSIS OPTIONS:
    --storage <path>                      Session storage directory
    --engine <whisper|parakeet>           Auto-transcription engine, default: parakeet on Apple Silicon
    --ffmpeg <path>                       ffmpeg binary path for auto-transcription
    --model <path>                        Whisper model path for auto-transcription
    --whisper <path>                      whisper-cli path for auto-transcription
    --parakeet <path>                     parakeet-mlx path for auto-transcription
    --parakeet-model <id>                 Parakeet Hugging Face model id
    --parakeet-cache-dir <path>           Parakeet Hugging Face cache directory
    --chunk-seconds <seconds>             Auto-transcription chunk size
    --agent <name>                        Agent to use after transcription
    --auto-analyze                        Run analysis after transcription
    --no-auto-analyze                     Disable analysis after transcription
    --preset <name>                       Analysis prompt preset
    --editor <command>                    Editor/app used by TUI o to open the session folder

OPEN OPTIONS:
    recall open latest [options]
    recall open <session-path> [options]
    --dir                                 Open the session folder instead of meeting.md
    --editor <command>                    Folder opener: code, cursor, zed, or an app name
    --storage <path>                      Storage directory for latest lookup

EXPORT OPTIONS:
    recall export latest [options]
    recall export <session-path> [options]
    --output <path>                       Output path, default: meeting-export.md in session
    --storage <path>                      Storage directory for latest lookup

UPDATE OPTIONS:
    recall update [--repo <path>]
    --repo <path>                         Explicit Recall source checkout"#
    );
}

fn run_update(args: Vec<String>) {
    let config = RecallConfig::load();
    let options = match UpdateOptions::parse(args, config.source_dir) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            eprintln!("Usage: recall update [--repo <path>]");
            std::process::exit(2);
        }
    };

    if let Err(error) = update(&options) {
        eprintln!("Recall update failed: {error}");
        std::process::exit(1);
    }
}

fn run_tui_with_options(options: TuiOptions) {
    if let Err(error) = tui::run_with_options(options) {
        eprintln!("Recall TUI failed: {error}");
        std::process::exit(1);
    }
}

fn run_analyze(args: Vec<String>, tui_defaults: &TuiOptions) {
    let options = match parse_analyze_options(args, tui_defaults) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            eprintln!();
            print_help();
            std::process::exit(2);
        }
    };

    match analyze(&options) {
        Ok(result) => {
            println!("Recall analysis complete");
            println!("  Session: {}", result.session_path.display());
            println!("  Prompt: {}", result.prompt_path.display());
            if result.dry_run {
                println!("  Dry run: agent was not executed");
                return;
            }
            if let Some(path) = result.raw_output_path {
                println!("  Raw output: {}", path.display());
            }
            if let Some(path) = result.result_path {
                println!("  Result JSON: {}", path.display());
            }
            if let Some(title) = result.generated_title {
                println!("  Title: {title}");
            }
            for path in result.written_files {
                println!("  Wrote: {}", path.display());
            }
        }
        Err(error) => {
            eprintln!("Analysis failed: {error}");
            std::process::exit(1);
        }
    }
}

fn run_agents(args: Vec<String>) {
    match args.as_slice() {
        [command] if command == "list" => {
            println!("Recall agent profiles");
            for agent in known_agents() {
                println!("  - {agent}");
            }
        }
        [command] if command == "doctor" => {
            println!("Recall agent doctor");
            for agent in known_agents() {
                let status = if binary_exists(agent) {
                    "found"
                } else {
                    "missing"
                };
                println!("  - {agent}: {status}");
            }
        }
        _ => {
            eprintln!("Usage: recall agents list|doctor");
            std::process::exit(2);
        }
    }
}

fn run_start(args: Vec<String>, tui_defaults: &TuiOptions) {
    let options = match parse_start_options(args, tui_defaults) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            eprintln!();
            print_help();
            std::process::exit(2);
        }
    };

    match start_session(&options) {
        Ok(session) => {
            println!("Recall session initialized");
            println!("  Title: {}", session.title);
            println!("  Consent: {}", session.consent.as_str());
            println!("  Path: {}", session.path.display());
            println!();
            println!("Next: wire this session to the macOS capture helper.");
        }
        Err(error) => {
            eprintln!("Failed to start session: {error}");
            std::process::exit(1);
        }
    }
}

fn run_audio_tap_probe() {
    match probe_audio_tap() {
        Ok(output) => println!("{output}"),
        Err(error) => {
            eprintln!("Audio tap probe failed: {error}");
            std::process::exit(1);
        }
    }
}

fn run_sources() {
    let sources = detect_sources();

    println!("Recall sources");
    println!("  Status: {}", sources.status);
    println!();
    println!("  Apps:");
    for app in sources.apps {
        println!("    - {app}");
    }
    println!();
    println!("  Microphones:");
    for microphone in sources.microphones {
        println!("    - {microphone}");
    }
}

fn run_transcribe(args: Vec<String>, tui_defaults: &TuiOptions) {
    let options = match parse_transcribe_options(args, tui_defaults) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            eprintln!();
            print_help();
            std::process::exit(2);
        }
    };

    match transcribe_with_progress(&options, |progress| match progress {
        TranscriptionProgress::Started {
            engine,
            model,
            note,
            ..
        } => {
            eprintln!("Engine: {} ({model})", engine.as_str());
            if let Some(note) = note {
                eprintln!("{note}");
            }
        }
        TranscriptionProgress::ModelDownloadStarted {
            model,
            cache_path,
            expected_bytes,
        } => {
            eprintln!(
                "Downloading Parakeet model ({model}, ~{}) — one-time",
                format_bytes(expected_bytes)
            );
            eprintln!("Cache: {}", cache_path.display());
        }
        TranscriptionProgress::ModelDownloadProgress {
            model,
            downloaded_bytes,
            total_bytes,
            bytes_per_sec,
            ..
        } => {
            eprint!(
                "\r{}",
                format_model_download_label(&model, downloaded_bytes, total_bytes, bytes_per_sec,)
            );
        }
        TranscriptionProgress::ModelDownloadFinished { cache_path, .. } => {
            eprintln!();
            eprintln!("Parakeet model ready at {}", cache_path.display());
        }
        TranscriptionProgress::ChunkStarted {
            track,
            index,
            total,
            elapsed_secs,
        } => {
            eprintln!("Transcribing {track} chunk {index}/{total} ({elapsed_secs}s)...");
        }
        _ => {}
    }) {
        Ok(result) => {
            println!("Recall transcription complete");
            println!("  Session: {}", result.session_path.display());
            println!("  Transcript: {}", result.transcript_path.display());
            for track in result.tracks {
                println!(
                    "  Track: {} ({} chars, {} chunks from {})",
                    track.label,
                    track.text_len,
                    track.chunk_count,
                    track.audio_path.display()
                );
            }
        }
        Err(error) => {
            eprintln!("Transcription failed: {error}");
            std::process::exit(1);
        }
    }
}

fn run_list(args: Vec<String>, tui_defaults: &TuiOptions) {
    let storage_dir = match parse_storage_arg(args, tui_defaults.storage_dir.clone()) {
        Ok(Some(path)) => path,
        Ok(None) => match default_storage_dir() {
            Ok(path) => path,
            Err(error) => {
                eprintln!("Failed to resolve storage directory: {error}");
                std::process::exit(1);
            }
        },
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };

    match list_sessions(&storage_dir) {
        Ok(sessions) if sessions.is_empty() => {
            println!("No Recall sessions found in {}", storage_dir.display());
        }
        Ok(sessions) => {
            println!("Recall sessions in {}", storage_dir.display());
            for session in sessions {
                println!("  {}", session.display());
            }
        }
        Err(error) => {
            eprintln!("Failed to list sessions: {error}");
            std::process::exit(1);
        }
    }
}

fn run_show(args: Vec<String>, tui_defaults: &TuiOptions) {
    let mut storage_dir = tui_defaults.storage_dir.clone();
    let mut latest = false;
    let mut iter = args.into_iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "latest" => latest = true,
            "--storage" => {
                storage_dir = Some(PathBuf::from(iter.next().unwrap_or_else(|| {
                    eprintln!("Usage: recall show [--storage <path>] latest");
                    std::process::exit(2);
                })));
            }
            _ => {
                eprintln!("Usage: recall show [--storage <path>] latest");
                std::process::exit(2);
            }
        }
    }

    let storage_dir = match storage_dir {
        Some(path) => path,
        None => match default_storage_dir() {
            Ok(path) => path,
            Err(error) => {
                eprintln!("Failed to resolve storage directory: {error}");
                std::process::exit(1);
            }
        },
    };

    if !latest {
        eprintln!("Usage: recall show [--storage <path>] latest");
        std::process::exit(2);
    }

    match list_sessions(&storage_dir) {
        Ok(sessions) if sessions.is_empty() => {
            println!("No Recall sessions found in {}", storage_dir.display());
        }
        Ok(sessions) => println!("{}", sessions[0].display()),
        Err(error) => {
            eprintln!("Failed to show latest session: {error}");
            std::process::exit(1);
        }
    }
}

fn run_open(args: Vec<String>, tui_defaults: &TuiOptions) {
    let options = match parse_open_args(args, tui_defaults) {
        Ok(value) => value,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };
    let target_path = if options.open_dir {
        options.session_path.clone()
    } else {
        let document_path = primary_document_path(&options.session_path);
        if !document_path.exists() {
            eprintln!(
                "No meeting document found in {}",
                options.session_path.display()
            );
            std::process::exit(1);
        }
        document_path
    };
    let editor = if options.open_dir {
        options.editor.as_deref()
    } else {
        None
    };

    match open_path(&target_path, editor) {
        Ok(()) => println!("Opened {}", target_path.display()),
        Err(error) => {
            eprintln!("Failed to open {}: {error}", target_path.display());
            std::process::exit(1);
        }
    }
}

fn run_export(args: Vec<String>, tui_defaults: &TuiOptions) {
    let (session_path, output_path) = match parse_session_document_args(args, tui_defaults, true) {
        Ok(value) => value,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };

    match export_session(&session_path, output_path.as_deref()) {
        Ok(path) => println!("Recall export written to {}", path.display()),
        Err(error) => {
            eprintln!("Export failed: {error}");
            std::process::exit(1);
        }
    }
}

struct OpenCommandOptions {
    session_path: PathBuf,
    open_dir: bool,
    editor: Option<String>,
}

fn parse_open_args(
    args: Vec<String>,
    tui_defaults: &TuiOptions,
) -> Result<OpenCommandOptions, String> {
    let mut target = None;
    let mut storage_dir = tui_defaults.storage_dir.clone();
    let mut open_dir = false;
    let mut editor = tui_defaults.editor.clone();
    let mut iter = args.into_iter();

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "latest" => {
                if target.is_some() {
                    return Err("Only one session target is allowed.".to_string());
                }
                target = Some(None);
            }
            "--dir" | "--folder" => open_dir = true,
            "--editor" => {
                editor = Some(
                    iter.next()
                        .ok_or_else(|| "--editor requires a value".to_string())?,
                );
            }
            "--storage" => {
                storage_dir = Some(PathBuf::from(
                    iter.next()
                        .ok_or_else(|| "--storage requires a value".to_string())?,
                ));
            }
            value if value.starts_with("--") => {
                return Err(format!("Unknown option: {value}"));
            }
            path => {
                if target.is_some() {
                    return Err("Only one session target is allowed.".to_string());
                }
                target = Some(Some(PathBuf::from(path)));
            }
        }
    }

    let session_path = match target.unwrap_or(None) {
        Some(path) => path,
        None => {
            let storage_dir =
                storage_dir
                    .unwrap_or(default_storage_dir().map_err(|error| {
                        format!("Failed to resolve storage directory: {error}")
                    })?);
            latest_session(&storage_dir).map_err(|error| error.to_string())?
        }
    };

    Ok(OpenCommandOptions {
        session_path,
        open_dir,
        editor,
    })
}

fn parse_session_document_args(
    args: Vec<String>,
    tui_defaults: &TuiOptions,
    allow_output: bool,
) -> Result<(PathBuf, Option<PathBuf>), String> {
    let mut target = None;
    let mut storage_dir = tui_defaults.storage_dir.clone();
    let mut output_path = None;
    let mut iter = args.into_iter();

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "latest" => {
                if target.is_some() {
                    return Err("Only one session target is allowed.".to_string());
                }
                target = Some(None);
            }
            "--storage" => {
                storage_dir = Some(PathBuf::from(
                    iter.next()
                        .ok_or_else(|| "--storage requires a value".to_string())?,
                ));
            }
            "--output" if allow_output => {
                output_path = Some(PathBuf::from(
                    iter.next()
                        .ok_or_else(|| "--output requires a value".to_string())?,
                ));
            }
            value if value.starts_with("--") => {
                return Err(format!("Unknown option: {value}"));
            }
            path => {
                if target.is_some() {
                    return Err("Only one session target is allowed.".to_string());
                }
                target = Some(Some(PathBuf::from(path)));
            }
        }
    }

    match target.unwrap_or(None) {
        Some(path) => Ok((path, output_path)),
        None => {
            let storage_dir =
                storage_dir
                    .unwrap_or(default_storage_dir().map_err(|error| {
                        format!("Failed to resolve storage directory: {error}")
                    })?);
            latest_session(&storage_dir)
                .map(|path| (path, output_path))
                .map_err(|error| error.to_string())
        }
    }
}

fn parse_leading_tui_defaults(args: Vec<String>) -> Result<(TuiOptions, Vec<String>), String> {
    let config = RecallConfig::load();
    let mut options = tui_options_from_config(&config);
    let mut remainder = Vec::new();
    let mut iter = args.into_iter();

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--consent" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--consent requires a value".to_string())?;
                let consent = ConsentMode::parse(&value).ok_or_else(|| {
                    format!(
                        "Unknown consent mode '{value}'. Use provided, noted, verbal, written, policy, or none."
                    )
                })?;
                options.consent_noted = !matches!(consent, ConsentMode::NotYet);
            }
            "--title" => {
                options.title = iter
                    .next()
                    .ok_or_else(|| "--title requires a value".to_string())?;
            }
            "--storage" => {
                options.storage_dir = Some(PathBuf::from(
                    iter.next()
                        .ok_or_else(|| "--storage requires a value".to_string())?,
                ));
            }
            "--engine" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--engine requires a value".to_string())?;
                options.engine = TranscriptionEngine::parse(&value)
                    .ok_or_else(|| format!("Unknown engine '{value}'. Use whisper or parakeet."))?;
                options.require_parakeet = options.engine == TranscriptionEngine::Parakeet;
            }
            "--ffmpeg" => {
                options.ffmpeg_bin = Some(PathBuf::from(
                    iter.next()
                        .ok_or_else(|| "--ffmpeg requires a value".to_string())?,
                ));
            }
            "--whisper" => {
                options.whisper_bin = Some(PathBuf::from(
                    iter.next()
                        .ok_or_else(|| "--whisper requires a value".to_string())?,
                ));
            }
            "--parakeet" => {
                options.parakeet_bin = Some(PathBuf::from(
                    iter.next()
                        .ok_or_else(|| "--parakeet requires a value".to_string())?,
                ));
            }
            "--parakeet-model" => {
                options.parakeet_model = Some(
                    iter.next()
                        .ok_or_else(|| "--parakeet-model requires a value".to_string())?,
                );
            }
            "--parakeet-cache-dir" => {
                options.parakeet_cache_dir =
                    Some(PathBuf::from(iter.next().ok_or_else(|| {
                        "--parakeet-cache-dir requires a value".to_string()
                    })?));
            }
            "--model" => {
                options.model_path = Some(PathBuf::from(
                    iter.next()
                        .ok_or_else(|| "--model requires a value".to_string())?,
                ));
            }
            "--chunk-seconds" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--chunk-seconds requires a value".to_string())?;
                options.chunk_seconds = value
                    .parse::<u64>()
                    .map_err(|_| "--chunk-seconds must be a positive integer".to_string())?;
                if options.chunk_seconds == 0 {
                    return Err("--chunk-seconds must be greater than zero".to_string());
                }
            }
            "--agent" => {
                options.agent = Some(
                    iter.next()
                        .ok_or_else(|| "--agent requires a value".to_string())?,
                );
            }
            "--auto-analyze" => {
                options.auto_analyze = true;
            }
            "--no-auto-analyze" => {
                options.auto_analyze = false;
            }
            "--preset" => {
                options.preset = iter
                    .next()
                    .ok_or_else(|| "--preset requires a value".to_string())?;
            }
            "--editor" => {
                options.editor = Some(
                    iter.next()
                        .ok_or_else(|| "--editor requires a value".to_string())?,
                );
            }
            other => {
                remainder.push(other.to_string());
                remainder.extend(iter);
                break;
            }
        }
    }

    Ok((options, remainder))
}

fn tui_options_from_config(config: &RecallConfig) -> TuiOptions {
    let mut options = TuiOptions::default();
    if let Some(consent) = config.consent_default {
        options.consent_noted = !matches!(consent, ConsentMode::NotYet);
    }
    options.storage_dir = config.storage_dir.clone();
    options.editor = env::var("RECALL_EDITOR")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or(config.editor.clone());
    options.engine = config.transcription.engine;
    options.ffmpeg_bin = config.transcription.ffmpeg_bin.clone();
    options.whisper_bin = config.transcription.whisper_bin.clone();
    options.model_path = config.transcription.model_path.clone();
    options.parakeet_bin = config.transcription.parakeet_bin.clone();
    options.parakeet_model = config.transcription.parakeet_model.clone();
    options.parakeet_cache_dir = config.transcription.parakeet_cache_dir.clone();
    if let Some(chunk_seconds) = config.transcription.chunk_seconds {
        options.chunk_seconds = chunk_seconds;
    }
    if let Some(agent) = &config.analysis.default_agent {
        options.agent = Some(agent.clone());
    }
    if let Some(auto_analyze) = config.analysis.auto_analyze {
        options.auto_analyze = auto_analyze;
    }
    if let Some(preset) = &config.analysis.preset {
        options.preset = preset.clone();
    }
    options
}

fn parse_start_options(
    args: Vec<String>,
    tui_defaults: &TuiOptions,
) -> Result<StartOptions, String> {
    let mut options = StartOptions::default_for_cwd()
        .map_err(|error| format!("Failed to resolve current directory: {error}"))?;
    options.title = tui_defaults.title.clone();
    if let Some(storage_dir) = &tui_defaults.storage_dir {
        options.storage_dir = storage_dir.clone();
    }
    let mut iter = args.into_iter();

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--title" => {
                options.title = iter
                    .next()
                    .ok_or_else(|| "--title requires a value".to_string())?;
            }
            "--consent" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--consent requires a value".to_string())?;
                options.consent = ConsentMode::parse(&value).ok_or_else(|| {
                    format!("Unknown consent mode '{value}'. Use provided, noted, verbal, written, policy, or none.")
                })?;
            }
            "--storage" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--storage requires a value".to_string())?;
                options.storage_dir = PathBuf::from(value);
            }
            unknown => return Err(format!("Unknown start option: {unknown}")),
        }
    }

    Ok(options)
}

fn parse_transcribe_options(
    args: Vec<String>,
    tui_defaults: &TuiOptions,
) -> Result<TranscribeOptions, String> {
    let mut target: Option<TranscribeTarget> = None;
    let mut track = TrackSelection::Both;
    let mut storage_dir = tui_defaults.storage_dir.clone();
    let mut engine = tui_defaults.engine;
    let mut ffmpeg_bin = tui_defaults.ffmpeg_bin.clone();
    let mut model_path = tui_defaults.model_path.clone();
    let mut whisper_bin = tui_defaults.whisper_bin.clone();
    let mut parakeet_bin = tui_defaults.parakeet_bin.clone();
    let mut parakeet_model = tui_defaults.parakeet_model.clone();
    let mut parakeet_cache_dir = tui_defaults.parakeet_cache_dir.clone();
    let mut require_parakeet = false;
    let mut chunk_seconds = tui_defaults.chunk_seconds;
    let mut keep_wav = false;
    let mut iter = args.into_iter();

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "latest" => target = Some(TranscribeTarget::Latest),
            "--engine" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--engine requires a value".to_string())?;
                engine = TranscriptionEngine::parse(&value)
                    .ok_or_else(|| format!("Unknown engine '{value}'. Use whisper or parakeet."))?;
                require_parakeet = engine == TranscriptionEngine::Parakeet;
            }
            "--track" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--track requires a value".to_string())?;
                track = TrackSelection::parse(&value)
                    .ok_or_else(|| "Unknown track. Use both, call, or mic.".to_string())?;
            }
            "--ffmpeg" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--ffmpeg requires a value".to_string())?;
                ffmpeg_bin = Some(PathBuf::from(value));
            }
            "--storage" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--storage requires a value".to_string())?;
                storage_dir = Some(PathBuf::from(value));
            }
            "--model" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--model requires a value".to_string())?;
                model_path = Some(PathBuf::from(value));
            }
            "--whisper" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--whisper requires a value".to_string())?;
                whisper_bin = Some(PathBuf::from(value));
            }
            "--parakeet" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--parakeet requires a value".to_string())?;
                parakeet_bin = Some(PathBuf::from(value));
            }
            "--parakeet-model" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--parakeet-model requires a value".to_string())?;
                parakeet_model = Some(value);
            }
            "--parakeet-cache-dir" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--parakeet-cache-dir requires a value".to_string())?;
                parakeet_cache_dir = Some(PathBuf::from(value));
            }
            "--chunk-seconds" => {
                let value = iter
                    .next()
                    .ok_or_else(|| "--chunk-seconds requires a value".to_string())?;
                chunk_seconds = value
                    .parse::<u64>()
                    .map_err(|_| "--chunk-seconds must be a positive integer".to_string())?;
                if chunk_seconds == 0 {
                    return Err("--chunk-seconds must be greater than zero".to_string());
                }
            }
            "--keep-wav" => keep_wav = true,
            value if value.starts_with("--") => {
                return Err(format!("Unknown transcribe option: {value}"));
            }
            path => {
                if target.is_some() {
                    return Err("Only one transcribe target is allowed.".to_string());
                }
                target = Some(TranscribeTarget::Session(PathBuf::from(path)));
            }
        }
    }

    Ok(TranscribeOptions {
        target: target.unwrap_or(TranscribeTarget::Latest),
        track,
        storage_dir,
        engine,
        ffmpeg_bin,
        model_path,
        whisper_bin,
        parakeet_bin,
        parakeet_model,
        parakeet_cache_dir,
        chunk_seconds,
        keep_wav,
        require_parakeet,
        generation: None,
    })
}

fn parse_analyze_options(
    args: Vec<String>,
    tui_defaults: &TuiOptions,
) -> Result<AnalyzeOptions, String> {
    let mut target: Option<AnalyzeTarget> = None;
    let mut storage_dir = tui_defaults.storage_dir.clone();
    let mut agent = tui_defaults.agent.clone();
    let mut preset = tui_defaults.preset.clone();
    let mut dry_run = false;
    let mut iter = args.into_iter();

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "latest" => target = Some(AnalyzeTarget::Latest),
            "--agent" => {
                agent = Some(
                    iter.next()
                        .ok_or_else(|| "--agent requires a value".to_string())?,
                );
            }
            "--preset" => {
                preset = iter
                    .next()
                    .ok_or_else(|| "--preset requires a value".to_string())?;
            }
            "--storage" => {
                storage_dir = Some(PathBuf::from(
                    iter.next()
                        .ok_or_else(|| "--storage requires a value".to_string())?,
                ));
            }
            "--dry-run" => dry_run = true,
            value if value.starts_with("--") => {
                return Err(format!("Unknown analyze option: {value}"));
            }
            path => {
                if target.is_some() {
                    return Err("Only one analyze target is allowed.".to_string());
                }
                target = Some(AnalyzeTarget::Session(PathBuf::from(path)));
            }
        }
    }

    let config = RecallConfig::load();
    let agent = agent.or(config.analysis.default_agent).ok_or_else(|| {
        format!(
            "Missing --agent. Use --agent {}.",
            known_agents().join(", ")
        )
    })?;
    if preset.is_empty() {
        preset = config
            .analysis
            .preset
            .unwrap_or_else(|| "general".to_string());
    }

    Ok(AnalyzeOptions {
        target: target.unwrap_or(AnalyzeTarget::Latest),
        storage_dir,
        agent,
        preset,
        dry_run,
        generation: None,
    })
}

fn parse_storage_arg(
    args: Vec<String>,
    configured_storage_dir: Option<PathBuf>,
) -> Result<Option<PathBuf>, String> {
    match args.as_slice() {
        [] => Ok(configured_storage_dir),
        [flag, path] if flag == "--storage" => Ok(Some(PathBuf::from(path))),
        _ => Err("Usage: recall list [--storage <path>]".to_string()),
    }
}

fn print_spec_hint() {
    println!("Read docs/SPEC.md for the v0 product scope and docs/SETUP.md for setup.");
    println!(
        "Default transcription engine on Apple Silicon is Parakeet (`parakeet-mlx`, NVIDIA Parakeet TDT 0.6B v3, CC-BY-4.0). Whisper (`whisper-cli`) remains available with --engine whisper. Missing Parakeet falls back to Whisper unless you pass --engine parakeet."
    );
}

fn print_doctor() {
    let config = RecallConfig::load();
    let storage_dir = config
        .storage_dir
        .clone()
        .or_else(|| default_storage_dir().ok());
    let model_path = env::var_os("RECALL_WHISPER_MODEL")
        .map(PathBuf::from)
        .or(config.transcription.model_path.clone())
        .unwrap_or_else(|| PathBuf::from("models/ggml-base.en.bin"));
    let whisper_bin = env::var_os("RECALL_WHISPER_BIN")
        .map(PathBuf::from)
        .or(config.transcription.whisper_bin.clone());
    let ffmpeg_bin = env::var_os("RECALL_FFMPEG_BIN")
        .map(PathBuf::from)
        .or(config.transcription.ffmpeg_bin.clone());
    let parakeet_bin = env::var_os("RECALL_PARAKEET_BIN")
        .map(PathBuf::from)
        .or(config.transcription.parakeet_bin.clone());
    let parakeet_model = env::var("RECALL_PARAKEET_MODEL")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            resolve_parakeet_model_id(None, config.transcription.parakeet_model.as_deref())
        });
    let parakeet_cache_dir = resolve_parakeet_cache_dir_for_options(
        None,
        config.transcription.parakeet_cache_dir.as_deref(),
    );

    println!("Recall doctor");
    println!();
    println!("Core tools:");
    print_binary_check("rustc", "Rust compiler");
    print_binary_check("cargo", "Cargo package manager");
    print_binary_check("swift", "Swift toolchain");
    println!();
    println!("Capture:");
    print_binary_check("swift", "Swift helper runner");
    println!("  - CoreAudio process tap probe: run `recall audio-tap-probe`");
    println!();
    println!("Transcription:");
    if let Some(path) = ffmpeg_bin {
        print_path_check(&path, "ffmpeg");
    } else {
        print_binary_check("ffmpeg", "audio conversion/chunking");
    }
    if let Some(path) = whisper_bin {
        print_path_check(&path, "whisper-cli");
    } else {
        print_binary_check("whisper-cli", "whisper.cpp CLI");
    }
    print_path_check(&model_path, "Whisper model");
    print_parakeet_doctor(
        parakeet_bin.as_deref(),
        &parakeet_model,
        parakeet_cache_dir.as_deref(),
        config.transcription.engine,
        config.transcription.invalid_engine.as_deref(),
    );
    println!();
    println!("Storage:");
    if let Some(path) = storage_dir {
        println!("  - sessions: {}", path.display());
    } else {
        println!("  - sessions: unresolved");
    }
    if let Some(path) = config_path() {
        println!("  - config: {}", path.display());
    }
    println!();
    println!("Agents:");
    for agent in known_agents() {
        print_binary_check(agent, agent);
    }
}

fn binary_exists(name: &str) -> bool {
    binary_path(name).is_some()
}

fn binary_path(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|path| path.exists())
}

fn print_binary_check(name: &str, label: &str) {
    match binary_path(name) {
        Some(path) => println!("  ok   {label}: {}", path.display()),
        None => println!("  miss {label}: `{name}` not found on PATH"),
    }
}

fn print_path_check(path: &Path, label: &str) {
    if path.exists() {
        println!("  ok   {label}: {}", path.display());
    } else {
        println!("  miss {label}: {} not found", path.display());
    }
}

fn print_parakeet_doctor(
    explicit_bin: Option<&Path>,
    model: &str,
    cache_dir: Option<&Path>,
    engine: TranscriptionEngine,
    invalid_engine: Option<&str>,
) {
    let resolved = find_parakeet_binary(explicit_bin);
    let role = match engine {
        TranscriptionEngine::Parakeet => "selected engine",
        TranscriptionEngine::Whisper => "available fallback",
    };
    match parakeet_binary_doctor_level(explicit_bin, engine) {
        DoctorCheckLevel::Ok => match resolved {
            Some(path) => println!("  ok   parakeet-mlx ({role}): {}", path.display()),
            None => println!("  ok   parakeet-mlx ({role}): `{DEFAULT_PARAKEET_BIN}`"),
        },
        DoctorCheckLevel::Warn => {
            println!("  warn parakeet-mlx ({role}): `{DEFAULT_PARAKEET_BIN}` not found");
            println!("        {PARAKEET_INSTALL_HINT}");
        }
        DoctorCheckLevel::Miss => {
            println!("  miss parakeet-mlx ({role}): `{DEFAULT_PARAKEET_BIN}` not found");
            println!("        {PARAKEET_INSTALL_HINT}");
        }
    }

    if parakeet_model_is_cached(model, cache_dir) {
        println!("  ok   Parakeet model cache: `{model}`");
    } else {
        println!(
            "  warn Parakeet model cache: `{model}` not cached yet. First Parakeet run may download it."
        );
    }
    println!(
        "  note Default engine on Apple Silicon is Parakeet (NVIDIA CC-BY-4.0). Whisper remains available with --engine whisper."
    );
    if matches!(engine, TranscriptionEngine::Whisper) {
        println!("  note Config currently selects engine = \"whisper\".");
    }
    if let Some(invalid) = invalid_engine {
        println!(
            "  warn Unknown [transcription].engine = \"{invalid}\"; using {}.",
            engine.as_str()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transcribe_defaults_to_parakeet_engine() {
        let options =
            parse_transcribe_options(vec!["latest".into()], &TuiOptions::default()).unwrap();
        assert_eq!(options.engine, TranscriptionEngine::Parakeet);
        assert!(!options.require_parakeet);
        assert!(options.parakeet_bin.is_none());
        assert!(options.whisper_bin.is_none());
    }

    #[test]
    fn transcribe_parses_parakeet_engine_and_binary() {
        let options = parse_transcribe_options(
            vec![
                "latest".into(),
                "--engine".into(),
                "parakeet".into(),
                "--parakeet".into(),
                "/tmp/parakeet-mlx".into(),
                "--whisper".into(),
                "/tmp/whisper-cli".into(),
            ],
            &TuiOptions::default(),
        )
        .unwrap();

        assert_eq!(options.engine, TranscriptionEngine::Parakeet);
        assert!(options.require_parakeet);
        assert_eq!(
            options.parakeet_bin.as_deref(),
            Some(Path::new("/tmp/parakeet-mlx"))
        );
        assert_eq!(
            options.whisper_bin.as_deref(),
            Some(Path::new("/tmp/whisper-cli"))
        );
    }

    #[test]
    fn transcribe_engine_whisper_remains_valid_when_parakeet_is_configured() {
        let mut defaults = TuiOptions::default();
        defaults.engine = TranscriptionEngine::Parakeet;
        defaults.parakeet_bin = Some(PathBuf::from("/tmp/parakeet-mlx"));

        let options = parse_transcribe_options(
            vec!["latest".into(), "--engine".into(), "whisper".into()],
            &defaults,
        )
        .unwrap();

        assert_eq!(options.engine, TranscriptionEngine::Whisper);
        assert!(!options.require_parakeet);
        assert_eq!(
            options.parakeet_bin.as_deref(),
            Some(Path::new("/tmp/parakeet-mlx"))
        );
    }

    #[test]
    fn transcribe_rejects_unknown_engine() {
        let error = parse_transcribe_options(
            vec!["--engine".into(), "nemo".into()],
            &TuiOptions::default(),
        )
        .unwrap_err();
        assert!(error.contains("whisper"));
        assert!(error.contains("parakeet"));
    }

    #[test]
    fn tui_options_from_config_default_engine_is_parakeet() {
        let options = tui_options_from_config(&RecallConfig::default());
        assert_eq!(options.engine, TranscriptionEngine::Parakeet);
        assert!(!options.require_parakeet);
        assert!(options.parakeet_bin.is_none());
        assert!(options.whisper_bin.is_none());
    }

    #[test]
    fn leading_engine_parakeet_requires_the_parakeet_binary() {
        let (options, remainder) =
            parse_leading_tui_defaults(vec!["--engine".into(), "parakeet".into(), "list".into()])
                .unwrap();
        assert_eq!(options.engine, TranscriptionEngine::Parakeet);
        assert!(options.require_parakeet);
        assert_eq!(remainder, vec!["list".to_string()]);
    }
}
