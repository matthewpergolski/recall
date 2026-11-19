use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::session::ConsentMode;
use crate::transcription::TranscriptionEngine;

#[derive(Debug, Clone, Default)]
pub struct RecallConfig {
    pub consent_default: Option<ConsentMode>,
    pub storage_dir: Option<PathBuf>,
    pub source_dir: Option<PathBuf>,
    pub editor: Option<String>,
    pub timezone: Option<String>,
    pub keep_audio: Option<bool>,
    pub analysis: AnalysisConfig,
    pub transcription: TranscriptionConfig,
    pub live_transcription: LiveTranscriptionConfig,
}

#[derive(Debug, Clone, Default)]
pub struct AnalysisConfig {
    pub default_agent: Option<String>,
    pub auto_analyze: Option<bool>,
    pub preset: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct TranscriptionConfig {
    pub engine: TranscriptionEngine,
    pub engine_specified: bool,
    pub ffmpeg_bin: Option<PathBuf>,
    pub whisper_bin: Option<PathBuf>,
    pub model_path: Option<PathBuf>,
    pub parakeet_bin: Option<PathBuf>,
    pub parakeet_model: Option<String>,
    pub parakeet_cache_dir: Option<PathBuf>,
    pub chunk_seconds: Option<u64>,
    pub invalid_engine: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct LiveTranscriptionConfig {
    pub enabled: Option<bool>,
}

impl RecallConfig {
    pub fn load() -> Self {
        let Some(path) = config_path() else {
            return Self::default();
        };
        let Ok(content) = fs::read_to_string(path) else {
            return Self::default();
        };
        Self::parse(&content)
    }

    pub(crate) fn parse(content: &str) -> Self {
        let mut config = Self::default();
        let mut section = String::new();

        for line in content.lines() {
            let line = line.split('#').next().unwrap_or_default().trim();
            if line.is_empty() {
                continue;
            }

            if line.starts_with('[') && line.ends_with(']') {
                section = line
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .trim()
                    .to_string();
                continue;
            }

            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            let value = value.trim();

            match (section.as_str(), key) {
                ("", "consent_default") => {
                    config.consent_default = parse_string(value).and_then(|value| {
                        if value == "provided" {
                            Some(ConsentMode::Noted)
                        } else {
                            ConsentMode::parse(&value)
                        }
                    });
                }
                ("", "storage_dir") => {
                    config.storage_dir = parse_string(value).map(expand_path);
                }
                ("", "source_dir") => {
                    config.source_dir = parse_string(value).map(expand_path);
                }
                ("", "editor") => {
                    config.editor = parse_string(value);
                }
                ("", "timezone") => {
                    config.timezone = parse_string(value).filter(|value| !value.trim().is_empty());
                }
                ("", "keep_audio") => {
                    config.keep_audio = parse_bool(value);
                }
                ("analysis", "default_agent") => {
                    config.analysis.default_agent = parse_string(value);
                }
                ("analysis", "auto_analyze") => {
                    config.analysis.auto_analyze = parse_bool(value);
                }
                ("analysis", "preset") => {
                    config.analysis.preset = parse_string(value);
                }
                ("transcription", "engine") => {
                    if let Some(raw) = parse_string(value) {
                        if let Some(engine) = TranscriptionEngine::parse(&raw) {
                            config.transcription.engine = engine;
                            config.transcription.engine_specified = true;
                        } else {
                            config.transcription.invalid_engine = Some(raw);
                        }
                    }
                }
                ("transcription", "ffmpeg_bin") => {
                    config.transcription.ffmpeg_bin = parse_string(value).map(expand_path);
                }
                ("transcription", "whisper_bin") => {
                    config.transcription.whisper_bin = parse_string(value).map(expand_path);
                }
                ("transcription", "model_path") => {
                    config.transcription.model_path = parse_string(value).map(expand_path);
                }
                ("transcription", "parakeet_bin") => {
                    config.transcription.parakeet_bin = parse_string(value).map(expand_path);
                }
                ("transcription", "parakeet_model") => {
                    config.transcription.parakeet_model = parse_string(value);
                }
                ("transcription", "parakeet_cache_dir") => {
                    config.transcription.parakeet_cache_dir = parse_string(value).map(expand_path);
                }
                ("transcription", "chunk_seconds") => {
                    config.transcription.chunk_seconds = value.trim().parse::<u64>().ok();
                }
                ("live_transcription", "enabled") => {
                    config.live_transcription.enabled = parse_bool(value);
                }
                _ => {}
            }
        }

        config
    }
}

/// Rewrite only the top-level `source_dir` line. Preserve comments, unknown keys,
/// and every other setting. Insert before the first `[section]` when missing.
pub fn patch_top_level_source_dir(config_file: &Path, checkout: &Path) -> io::Result<()> {
    let absolute = canonical_or_absolute(checkout)?;
    let quoted = toml_quoted_path(&absolute);
    let line = format!("source_dir = {quoted}");

    if let Some(parent) = config_file.parent() {
        fs::create_dir_all(parent)?;
    }

    let original = match fs::read_to_string(config_file) {
        Ok(content) => content,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut body = line;
            body.push('\n');
            fs::write(config_file, body)?;
            return Ok(());
        }
        Err(error) => return Err(error),
    };

    let patched = patch_source_dir_text(&original, &line);
    if patched != original {
        fs::write(config_file, patched)?;
    }
    Ok(())
}

fn canonical_or_absolute(path: &Path) -> io::Result<PathBuf> {
    fs::canonicalize(path).or_else(|_| {
        if path.is_absolute() {
            Ok(path.to_path_buf())
        } else {
            env::current_dir().map(|cwd| cwd.join(path))
        }
    })
}

fn toml_quoted_path(path: &Path) -> String {
    let raw = path.to_string_lossy();
    let escaped = raw.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

fn patch_source_dir_text(content: &str, new_line: &str) -> String {
    let mut out = String::with_capacity(content.len() + new_line.len() + 2);
    let mut section = false;
    let mut replaced = false;
    let had_trailing_newline = content.ends_with('\n');

    for raw_line in content.split_inclusive('\n') {
        let line_no_nl = raw_line.trim_end_matches(['\n', '\r']);
        let trimmed = line_no_nl.split('#').next().unwrap_or_default().trim();

        if !section && trimmed.starts_with('[') && trimmed.ends_with(']') {
            if !replaced {
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                out.push_str(new_line);
                out.push('\n');
                replaced = true;
            }
            section = true;
        }

        if !section {
            let key = trimmed.split_once('=').map(|(k, _)| k.trim());
            if key == Some("source_dir") {
                out.push_str(new_line);
                if raw_line.ends_with('\n') {
                    out.push('\n');
                }
                replaced = true;
                continue;
            }
        }

        out.push_str(raw_line);
    }

    if !replaced {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(new_line);
        out.push('\n');
    } else if had_trailing_newline && !out.ends_with('\n') {
        out.push('\n');
    }

    out
}

pub fn config_path() -> Option<PathBuf> {
    if let Some(config_home) = env::var_os("XDG_CONFIG_HOME").map(PathBuf::from) {
        return Some(config_home.join("recall/config.toml"));
    }

    env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".config/recall/config.toml"))
}

fn parse_string(value: &str) -> Option<String> {
    let value = value.trim();
    if value.starts_with('"') && value.ends_with('"') && value.len() >= 2 {
        Some(value[1..value.len() - 1].to_string())
    } else if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

pub(crate) fn expand_path(value: String) -> PathBuf {
    if value == "~" {
        return env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or(value.into());
    }
    if let Some(rest) = value.strip_prefix("~/") {
        if let Some(home) = env::var_os("HOME").map(PathBuf::from) {
            return home.join(rest);
        }
    }
    PathBuf::from(value)
}

#[cfg(test)]
mod tests {
    use super::{patch_top_level_source_dir, RecallConfig};
    use crate::session::ConsentMode;
    use crate::transcription::TranscriptionEngine;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    fn temp_dir(label: &str) -> PathBuf {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "recall-config-patch-{}-{}-{id}",
            label,
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[test]
    fn patch_keeps_comments_unknown_keys_and_storage_dir() {
        let dir = temp_dir("keep");
        let checkout = dir.join("checkout");
        fs::create_dir_all(&checkout).unwrap();
        let config = dir.join("config.toml");
        let places = dir.join("session-places.txt");
        fs::write(&places, "/tmp/sessions\n").unwrap();
        fs::write(
            &config,
            "# keep me\nweird_key = 1\nstorage_dir = \"~/Documents\"\nsource_dir = \"/old\"\n",
        )
        .unwrap();
        patch_top_level_source_dir(&config, &checkout).unwrap();
        let text = fs::read_to_string(&config).unwrap();
        assert!(text.contains("# keep me"), "{text}");
        assert!(text.contains("weird_key = 1"), "{text}");
        assert!(text.contains("storage_dir"), "{text}");
        assert_eq!(text.matches("source_dir").count(), 1, "{text}");
        let parsed = RecallConfig::parse(&text);
        assert_eq!(
            parsed
                .source_dir
                .as_ref()
                .and_then(|p| p.canonicalize().ok()),
            checkout.canonicalize().ok()
        );
        assert_eq!(fs::read_to_string(&places).unwrap(), "/tmp/sessions\n");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn patch_inserts_before_section_and_is_idempotent() {
        let dir = temp_dir("section");
        let checkout = dir.join("space path");
        fs::create_dir_all(&checkout).unwrap();
        let config = dir.join("config.toml");
        fs::write(
            &config,
            "storage_dir = \"~/x\"\n[transcription]\nengine = apple\n",
        )
        .unwrap();
        patch_top_level_source_dir(&config, &checkout).unwrap();
        patch_top_level_source_dir(&config, &checkout).unwrap();
        let text = fs::read_to_string(&config).unwrap();
        assert_eq!(text.matches("source_dir").count(), 1, "{text}");
        assert!(text.find("source_dir").unwrap() < text.find("[transcription]").unwrap());
        let parsed = RecallConfig::parse(&text);
        assert_eq!(
            parsed
                .source_dir
                .as_ref()
                .and_then(|p| p.canonicalize().ok()),
            checkout.canonicalize().ok()
        );
        assert!(parsed.storage_dir.is_some());
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn patch_handles_missing_file_and_no_trailing_newline() {
        let dir = temp_dir("edge");
        let checkout = dir.join("c");
        fs::create_dir_all(&checkout).unwrap();
        let missing = dir.join("new.toml");
        patch_top_level_source_dir(&missing, &checkout).unwrap();
        assert!(fs::read_to_string(&missing).unwrap().ends_with('\n'));

        let no_nl = dir.join("nonl.toml");
        fs::write(&no_nl, "keep_audio = false").unwrap();
        patch_top_level_source_dir(&no_nl, &checkout).unwrap();
        let text = fs::read_to_string(&no_nl).unwrap();
        assert!(text.starts_with("keep_audio = false\n"), "{text:?}");
        assert!(text.contains("source_dir"), "{text}");
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn transcription_engine_defaults_to_platform_engine() {
        let config = RecallConfig::parse(
            r#"
            [transcription]
            ffmpeg_bin = "~/tools/ffmpeg"
            whisper_bin = "~/tools/whisper-cli"
            "#,
        );

        assert_eq!(config.transcription.engine, TranscriptionEngine::default());
        assert!(!config.transcription.engine_specified);
        assert!(config.transcription.parakeet_bin.is_none());
        assert!(config.transcription.parakeet_model.is_none());
    }

    #[test]
    fn parses_apple_engine_as_explicit_selection() {
        let config = RecallConfig::parse(
            r#"
            [transcription]
            engine = "apple"
            "#,
        );

        assert_eq!(config.transcription.engine, TranscriptionEngine::Apple);
        assert!(config.transcription.engine_specified);
        assert!(config.transcription.invalid_engine.is_none());
    }

    #[test]
    fn parses_recall_config() {
        let config = RecallConfig::parse(
            r#"
            consent_default = "provided"
            storage_dir = "~/Recall/Sessions"
            source_dir = "~/Projects/recall"
            editor = "code"
            timezone = "America/Chicago"
            keep_audio = false

            [analysis]
            default_agent = "grok"
            auto_analyze = true
            preset = "work"

            [transcription]
            engine = "whisper"
            ffmpeg_bin = "~/tools/ffmpeg"
            whisper_bin = "~/tools/whisper-cli"
            model_path = "~/models/ggml-base.en.bin"
            parakeet_bin = "parakeet-mlx"
            parakeet_model = "mlx-community/parakeet-tdt-0.6b-v3"
            parakeet_cache_dir = "~/Recall/models/parakeet"
            chunk_seconds = 300
            "#,
        );

        assert!(matches!(config.consent_default, Some(ConsentMode::Noted)));
        assert!(config.storage_dir.is_some());
        assert!(config.source_dir.is_some());
        assert_eq!(config.editor.as_deref(), Some("code"));
        assert_eq!(config.timezone.as_deref(), Some("America/Chicago"));
        assert_eq!(config.keep_audio, Some(false));
        assert_eq!(config.analysis.default_agent.as_deref(), Some("grok"));
        assert_eq!(config.analysis.auto_analyze, Some(true));
        assert_eq!(config.analysis.preset.as_deref(), Some("work"));
        assert_eq!(config.transcription.engine, TranscriptionEngine::Whisper);
        assert!(config.transcription.engine_specified);
        assert!(config.transcription.ffmpeg_bin.is_some());
        assert!(config.transcription.whisper_bin.is_some());
        assert!(config.transcription.model_path.is_some());
        assert!(config.transcription.parakeet_bin.is_some());
        assert_eq!(
            config.transcription.parakeet_model.as_deref(),
            Some("mlx-community/parakeet-tdt-0.6b-v3")
        );
        assert!(config.transcription.parakeet_cache_dir.is_some());
        assert_eq!(config.transcription.chunk_seconds, Some(300));
        assert!(config.live_transcription.enabled.is_none());
    }

    #[test]
    fn parses_live_transcription_enabled() {
        let enabled = RecallConfig::parse(
            r#"
            [live_transcription]
            enabled = true
            "#,
        );
        assert_eq!(enabled.live_transcription.enabled, Some(true));

        let disabled = RecallConfig::parse(
            r#"
            [live_transcription]
            enabled = false
            "#,
        );
        assert_eq!(disabled.live_transcription.enabled, Some(false));
    }

    #[test]
    fn parses_parakeet_engine_without_replacing_whisper_paths() {
        let config = RecallConfig::parse(
            r#"
            [transcription]
            engine = "parakeet"
            whisper_bin = "~/tools/whisper-cli"
            model_path = "~/models/ggml-base.en.bin"
            parakeet_bin = "~/tools/parakeet-mlx"
            "#,
        );

        assert_eq!(config.transcription.engine, TranscriptionEngine::Parakeet);
        assert!(config.transcription.engine_specified);
        assert!(config.transcription.whisper_bin.is_some());
        assert!(config.transcription.model_path.is_some());
        assert!(config.transcription.parakeet_bin.is_some());
    }

    #[test]
    fn records_invalid_transcription_engine_without_changing_the_default() {
        let config = RecallConfig::parse(
            r#"
            [transcription]
            engine = "nemo"
            "#,
        );

        assert_eq!(config.transcription.engine, TranscriptionEngine::default());
        assert!(!config.transcription.engine_specified);
        assert_eq!(config.transcription.invalid_engine.as_deref(), Some("nemo"));
    }
}
