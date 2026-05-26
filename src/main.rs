mod analysis;
mod capture_sources;
mod config;
mod mic_recorder;
mod session;
mod system_recorder;
mod transcription;
mod tui;

use std::env;
use std::path::PathBuf;

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
        Some("start") => run_start(args.collect()),
        Some("list") => run_list(args.collect()),
        Some("show") => run_show(args.collect()),
        Some("sources") => run_sources(),
        Some("audio-tap-probe") => run_audio_tap_probe(),
        Some("transcribe") => run_transcribe(args.collect()),
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

fn run_start(args: Vec<String>) {
    let options = match parse_start_options(args) {
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

fn run_transcribe(args: Vec<String>) {
    let options = match parse_transcribe_options(args) {
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

fn run_list(args: Vec<String>) {
    let storage_dir = match parse_storage_arg(args) {
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

fn run_show(args: Vec<String>) {
    let storage_dir = match default_storage_dir() {
        Ok(path) => path,
        Err(error) => {
            eprintln!("Failed to resolve storage directory: {error}");
            std::process::exit(1);
        }
    };

    match args.as_slice() {
        [which] if which == "latest" => match list_sessions(&storage_dir) {
            Ok(sessions) if sessions.is_empty() => {
                println!("No Recall sessions found in {}", storage_dir.display());
            }
            Ok(sessions) => println!("{}", sessions[0].display()),
            Err(error) => {
                eprintln!("Failed to show latest session: {error}");
                std::process::exit(1);
            }
        },
        _ => {
            eprintln!("Usage: recall show latest");
            std::process::exit(2);
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

fn parse_start_options(args: Vec<String>) -> Result<StartOptions, String> {
    let mut options = StartOptions::default_for_cwd()
        .map_err(|error| format!("Failed to resolve current directory: {error}"))?;
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

fn parse_transcribe_options(args: Vec<String>) -> Result<TranscribeOptions, String> {
    let mut target: Option<TranscribeTarget> = None;
    let mut track = TrackSelection::Both;
    let mut storage_dir = None;
    let mut model_path = None;
    let mut whisper_bin = None;
    let mut chunk_seconds = transcription::TRANSCRIPTION_CHUNK_SECONDS;
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
    let mut storage_dir = None;
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

fn parse_storage_arg(args: Vec<String>) -> Result<Option<PathBuf>, String> {
    match args.as_slice() {
        [] => Ok(None),
        [flag, path] if flag == "--storage" => Ok(Some(PathBuf::from(path))),
        _ => Err("Usage: recall list [--storage <path>]".to_string()),
    }
}

fn print_spec_hint() {
    println!("Read docs/SPEC.md for the v0 product scope and docs/SETUP.md for setup.");
}

fn print_doctor() {
    println!("Recall doctor");
    println!();
    println!("Required now:");
    println!("  - Rust + Cargo: run `rustc --version` and `cargo --version`");
    println!("  - Swift toolchain: run `swift --version`");
    println!("  - ffmpeg for transcription audio conversion: run `ffmpeg -version`");
    println!();
    println!("Recommended before transcription:");
    println!("  - whisper.cpp CLI on PATH as `whisper-cli`");
    println!("  - Whisper ggml model at `models/ggml-base.en.bin` or RECALL_WHISPER_MODEL");
    println!();
    println!("Recommended before capture work:");
    println!("  - Update Rust with `rustup update stable`");
    println!("  - Confirm Xcode command line tools are installed");
    println!("  - Grant Microphone permission when prompted");
    if let Some(path) = config_path() {
        println!("  - Optional config path: {}", path.display());
    }
}

fn binary_exists(name: &str) -> bool {
    let Some(path) = env::var_os("PATH") else {
        return false;
    };
    env::split_paths(&path).any(|dir| dir.join(name).exists())
}
