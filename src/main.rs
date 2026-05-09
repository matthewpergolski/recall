mod capture_sources;
mod mic_recorder;
mod session;
mod system_recorder;
mod transcription;
mod tui;

use std::env;
use std::path::PathBuf;

use capture_sources::{detect_sources, probe_audio_tap};
use session::{default_storage_dir, list_sessions, start_session, ConsentMode, StartOptions};
use transcription::{transcribe, TrackSelection, TranscribeOptions, TranscribeTarget};
use tui::TuiOptions;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() {
    let mut args = env::args().skip(1);
    let command = args.next();

    match command.as_deref() {
        Some("start") => run_start(args.collect()),
        Some("list") => run_list(args.collect()),
        Some("show") => run_show(args.collect()),
        Some("sources") => run_sources(),
        Some("audio-tap-probe") => run_audio_tap_probe(),
        Some("transcribe") => run_transcribe(args.collect()),
        Some("doctor") => print_doctor(),
        Some("spec") => print_spec_hint(),
        Some("-h") | Some("--help") | Some("help") => print_help(),
        Some("-V") | Some("--version") | Some("version") => println!("recall {VERSION}"),
        Some(flag @ ("--consent" | "--title")) => {
            run_tui_with_args(vec![flag.to_string()].into_iter().chain(args).collect())
        }
        Some(other) => {
            eprintln!("Unknown command: {other}");
            eprintln!();
            print_help();
            std::process::exit(2);
        }
        None => {
            run_tui_with_args(Vec::new());
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
    --keep-wav                            Keep temporary converted WAV files

NEXT MILESTONE:
    Add local transcription, then generate summaries and actions
    from transcript text."#
    );
}

fn run_tui_with_args(args: Vec<String>) {
    let options = match parse_tui_options(args) {
        Ok(options) => options,
        Err(message) => {
            eprintln!("{message}");
            eprintln!();
            print_help();
            std::process::exit(2);
        }
    };

    if let Err(error) = tui::run_with_options(options) {
        eprintln!("Recall TUI failed: {error}");
        std::process::exit(1);
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

    match transcribe(&options) {
        Ok(result) => {
            println!("Recall transcription complete");
            println!("  Session: {}", result.session_path.display());
            println!("  Transcript: {}", result.transcript_path.display());
            for track in result.tracks {
                println!(
                    "  Track: {} ({} chars from {})",
                    track.label,
                    track.text_len,
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

fn parse_tui_options(args: Vec<String>) -> Result<TuiOptions, String> {
    let mut options = TuiOptions::default();
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
            unknown => return Err(format!("Unknown TUI option: {unknown}")),
        }
    }

    Ok(options)
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
        keep_wav,
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
}
