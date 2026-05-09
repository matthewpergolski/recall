use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct SourceSummary {
    pub apps: Vec<String>,
    pub microphones: Vec<String>,
    pub status: String,
}

#[derive(Debug, Deserialize)]
struct HelperSourceList {
    candidates: Vec<AppCandidate>,
    microphones: Vec<AudioDevice>,
    permissions: Permissions,
}

#[derive(Debug, Deserialize)]
struct AppCandidate {
    name: String,
    confidence: String,
    reason: String,
}

#[derive(Debug, Deserialize)]
struct AudioDevice {
    name: String,
}

#[derive(Debug, Deserialize)]
struct Permissions {
    microphone: String,
}

impl SourceSummary {
    pub fn fallback(message: impl Into<String>) -> Self {
        Self {
            apps: vec![
                "Microsoft Teams / app audio placeholder".to_string(),
                "Browser call source placeholder".to_string(),
            ],
            microphones: vec!["MacBook Pro Microphone placeholder".to_string()],
            status: message.into(),
        }
    }
}

pub fn detect_sources() -> SourceSummary {
    match run_helper() {
        Ok(list) => list.into_summary(),
        Err(error) => SourceSummary::fallback(format!("Source helper unavailable: {error}")),
    }
}

pub fn probe_audio_tap() -> io::Result<String> {
    let output = run_helper_command("probe-audio-tap")?;
    if !output.status.success() {
        return Err(io::Error::other(command_error(&output.stderr)));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn parse_source_list(bytes: &[u8]) -> io::Result<HelperSourceList> {
    serde_json::from_slice(bytes).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("failed to parse helper JSON: {error}"),
        )
    })
}

impl HelperSourceList {
    fn into_summary(self) -> SourceSummary {
        let apps = if self.candidates.is_empty() {
            vec!["No meeting apps or browsers detected".to_string()]
        } else {
            self.candidates
                .into_iter()
                .take(4)
                .map(|candidate| {
                    format!(
                        "{} ({}, {})",
                        candidate.name, candidate.confidence, candidate.reason
                    )
                })
                .collect()
        };

        let microphones = if self.microphones.is_empty() {
            vec!["No microphones detected".to_string()]
        } else {
            self.microphones
                .into_iter()
                .take(4)
                .map(|device| device.name)
                .collect()
        };

        Self::summary(apps, microphones, self.permissions.microphone)
    }

    fn summary(
        apps: Vec<String>,
        microphones: Vec<String>,
        microphone_permission: String,
    ) -> SourceSummary {
        SourceSummary {
            apps,
            microphones,
            status: format!(
                "Helper source scan OK, microphone permission: {microphone_permission}"
            ),
        }
    }
}

fn run_helper() -> io::Result<HelperSourceList> {
    let output = run_helper_command("list-sources")?;

    if !output.status.success() {
        return Err(io::Error::other(command_error(&output.stderr)));
    }

    parse_source_list(&output.stdout)
}

fn run_helper_command(command_name: &str) -> io::Result<std::process::Output> {
    let helper_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("capture-helper");

    if let Some(binary) = helper_binary(&helper_dir) {
        Command::new(binary).arg(command_name).output()
    } else {
        Command::new("swift")
            .arg("run")
            .arg("recall-capture")
            .arg(command_name)
            .current_dir(&helper_dir)
            .output()
    }
}

fn helper_binary(helper_dir: &Path) -> Option<PathBuf> {
    [
        helper_dir.join(".build/debug/recall-capture"),
        helper_dir.join(".build/arm64-apple-macosx/debug/recall-capture"),
        helper_dir.join(".build/x86_64-apple-macosx/debug/recall-capture"),
    ]
    .into_iter()
    .find(|path| path.exists())
}

fn command_error(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr).trim().to_string();
    if text.is_empty() {
        "helper exited with no error output".to_string()
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use super::parse_source_list;

    #[test]
    fn parses_helper_source_list() {
        let json = br#"{
          "type": "source_list",
          "version": "0.1.0",
          "generatedAtUnix": 1778221302,
          "candidates": [
            {
              "kind": "running_app",
              "name": "Microsoft Teams",
              "bundleIdentifier": "com.microsoft.teams2",
              "processIdentifier": 42,
              "confidence": "high",
              "reason": "Matched meeting app term 'microsoft teams'"
            }
          ],
          "microphones": [
            {
              "kind": "microphone",
              "name": "MacBook Pro Microphone",
              "uniqueID": "BuiltInMicrophoneDevice"
            }
          ],
          "permissions": {
            "microphone": "authorized"
          }
        }"#;

        let parsed = parse_source_list(json).expect("helper JSON should parse");
        let summary = parsed.into_summary();

        assert_eq!(summary.apps.len(), 1);
        assert!(summary.apps[0].contains("Microsoft Teams"));
        assert_eq!(summary.microphones, ["MacBook Pro Microphone"]);
        assert!(summary.status.contains("authorized"));
    }
}
