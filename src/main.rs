mod analysis;
mod capture_sources;
mod config;
mod mic_recorder;
mod session;
mod system_recorder;
mod transcription;
mod tui;

use std::env;
use std::path::{Path, PathBuf};

use analysis::{analyze, known_agents, AnalyzeOptions, AnalyzeTarget};
use capture_sources::{detect_sources, probe_audio_tap};
use config::{config_path, RecallConfig};
use session::{default_storage_dir, list_sessions, start_session, ConsentMode, StartOptions};
use transcription::{
    transcribe_with_progress, TrackSelection, TranscribeOptions, TranscribeTarget,
    TranscriptionProgress,
};
use tui::TuiOptions;

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
        Some("sources") => run_sources(),
        Some("audio-tap-probe") => run_audio_tap_probe(),
        Some("transcribe") => run_transcribe(args.collect(), &tui_defaults),
        Some("analyze") => run_analyze(args.collect(), &tui_defaults),
        Some("agents") => run_agents(args.collect()),
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
    recall sources                        List detected app and microphone sources
    recall audio-tap-probe                Probe CoreAudio process-tap availability
    recall transcribe latest              Transcribe the newest session locally
    recall analyze latest --agent grok    Generate summary/actions with an agent
    recall agents list                    List supported headless agent profiles
    recall doctor                         Check local development prerequisites
    recall spec                           Show where the product spec lives

START OPTIONS:
    --title <title>                       Meeting title
    --consent <mode>                      provided, noted, verbal, written, policy, none
    --storage <path>                      Session storage directory

TRANSCRIBE OPTIONS:
    recall transcribe latest [options]
    recall transcribe <session-path> [options]
    --track <both|call|mic>               Audio track selection, default: both
    --ffmpeg <path>                       ffmpeg binary path
    --model <path>                        Whisper ggml model path
    --whisper <path>                      whisper-cli binary path
    --storage <path>                      Storage directory for latest lookup
    --chunk-seconds <seconds>             Transcription chunk size, default: 600
    --keep-wav                            Keep temporary converted WAV files

ANALYZE OPTIONS:
    recall analyze latest [options]
    recall analyze <session-path> [options]
    --agent <grok|cline|codex|claude>     Headless agent profile to run
    --preset <general|work|personal>      Analysis prompt preset, default: general
    --storage <path>                      Storage directory for latest lookup
    --dry-run                             Write analysis prompt without running agent

TUI ANALYSIS OPTIONS:
    --storage <path>                      Session storage directory
    --ffmpeg <path>                       ffmpeg binary path for auto-transcription
    --model <path>                        Whisper model path for auto-transcription
    --whisper <path>                      whisper-cli path for auto-transcription
    --chunk-seconds <seconds>             Auto-transcription chunk size
    --agent <name>                        Agent to use after transcription
    --auto-analyze                        Run analysis after transcription
    --no-auto-analyze                     Disable analysis after transcription
    --preset <name>                       Analysis prompt preset

NEXT MILESTONE:
    Add local transcription, then generate summaries and actions
    from transcript text."#
    );
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

    match transcribe_with_progress(&options, |progress| {
        if let TranscriptionProgress::ChunkStarted {
            track,
            index,
            total,
        } = progress
        {
            eprintln!("Transcribing {track} chunk {index}/{total}...");
        }
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
    options.ffmpeg_bin = config.transcription.ffmpeg_bin.clone();
    options.whisper_bin = config.transcription.whisper_bin.clone();
    options.model_path = config.transcription.model_path.clone();
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
    let mut ffmpeg_bin = tui_defaults.ffmpeg_bin.clone();
    let mut model_path = tui_defaults.model_path.clone();
    let mut whisper_bin = tui_defaults.whisper_bin.clone();
    let mut chunk_seconds = tui_defaults.chunk_seconds;
    let mut keep_wav = false;
    let mut iter = args.into_iter();

    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "latest" => target = Some(TranscribeTarget::Latest),
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
        ffmpeg_bin,
        model_path,
        whisper_bin,
        chunk_seconds,
        keep_wav,
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
    let agent = agent
        .or(config.analysis.default_agent)
        .ok_or_else(|| "Missing --agent. Use --agent grok, cline, codex, or claude.".to_string())?;
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
