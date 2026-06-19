use std::env;
use std::fs;
use std::path::PathBuf;

use crate::session::ConsentMode;

#[derive(Debug, Clone, Default)]
pub struct RecallConfig {
    pub consent_default: Option<ConsentMode>,
    pub storage_dir: Option<PathBuf>,
    pub analysis: AnalysisConfig,
    pub transcription: TranscriptionConfig,
}

#[derive(Debug, Clone, Default)]
pub struct AnalysisConfig {
    pub default_agent: Option<String>,
    pub auto_analyze: Option<bool>,
    pub preset: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct TranscriptionConfig {
    pub ffmpeg_bin: Option<PathBuf>,
    pub whisper_bin: Option<PathBuf>,
    pub model_path: Option<PathBuf>,
    pub chunk_seconds: Option<u64>,
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

    fn parse(content: &str) -> Self {
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
                ("analysis", "default_agent") => {
                    config.analysis.default_agent = parse_string(value);
                }
                ("analysis", "auto_analyze") => {
                    config.analysis.auto_analyze = parse_bool(value);
                }
                ("analysis", "preset") => {
                    config.analysis.preset = parse_string(value);
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
                ("transcription", "chunk_seconds") => {
                    config.transcription.chunk_seconds = value.trim().parse::<u64>().ok();
                }
                _ => {}
            }
        }

        config
    }
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

fn expand_path(value: String) -> PathBuf {
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
    use super::RecallConfig;
    use crate::session::ConsentMode;

    #[test]
    fn parses_recall_config() {
        let config = RecallConfig::parse(
            r#"
            consent_default = "provided"
            storage_dir = "~/Recall/Sessions"

            [analysis]
            default_agent = "grok"
            auto_analyze = true
            preset = "work"

            [transcription]
            ffmpeg_bin = "~/tools/ffmpeg"
            whisper_bin = "~/tools/whisper-cli"
            model_path = "~/models/ggml-base.en.bin"
            chunk_seconds = 300
            "#,
        );

        assert!(matches!(config.consent_default, Some(ConsentMode::Noted)));
        assert!(config.storage_dir.is_some());
        assert_eq!(config.analysis.default_agent.as_deref(), Some("grok"));
        assert_eq!(config.analysis.auto_analyze, Some(true));
        assert_eq!(config.analysis.preset.as_deref(), Some("work"));
        assert!(config.transcription.ffmpeg_bin.is_some());
        assert!(config.transcription.whisper_bin.is_some());
        assert!(config.transcription.model_path.is_some());
        assert_eq!(config.transcription.chunk_seconds, Some(300));
    }
}
