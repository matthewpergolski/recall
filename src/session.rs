use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use time::OffsetDateTime;

#[derive(Debug, Clone)]
pub struct StartOptions {
    pub title: String,
    pub consent: ConsentMode,
    pub storage_dir: PathBuf,
}

#[derive(Debug, Clone, Copy)]
pub enum ConsentMode {
    Noted,
    Verbal,
    Written,
    MeetingPolicy,
    NotYet,
}

#[derive(Debug, Clone)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub consent: ConsentMode,
    pub created_at_unix: u64,
    pub path: PathBuf,
}

impl ConsentMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "noted" | "provided" => Some(Self::Noted),
            "verbal" => Some(Self::Verbal),
            "written" => Some(Self::Written),
            "policy" | "meeting-policy" => Some(Self::MeetingPolicy),
            "none" | "not-yet" => Some(Self::NotYet),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Noted => "noted",
            Self::Verbal => "verbal",
            Self::Written => "written",
            Self::MeetingPolicy => "meeting-policy",
            Self::NotYet => "not-yet",
        }
    }
}

impl StartOptions {
    pub fn default_for_cwd() -> io::Result<Self> {
        Ok(Self {
            title: "Untitled meeting".to_string(),
            consent: ConsentMode::NotYet,
            storage_dir: env::current_dir()?.join("sessions"),
        })
    }
}

pub fn start_session(options: &StartOptions) -> io::Result<Session> {
    let created_at_unix = unix_timestamp();
    let slug = slugify(&options.title);
    let id = format!("{}-{slug}", readable_local_timestamp());
    let path = options.storage_dir.join(&id);

    fs::create_dir_all(path.join("audio"))?;

    let session = Session {
        id,
        title: options.title.clone(),
        consent: options.consent,
        created_at_unix,
        path,
    };

    write_session_files(&session)?;

    Ok(session)
}

pub fn list_sessions(storage_dir: &Path) -> io::Result<Vec<PathBuf>> {
    if !storage_dir.exists() {
        return Ok(Vec::new());
    }

    let mut sessions = Vec::new();
    for entry in fs::read_dir(storage_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() && path.join("recall.json").exists() {
            sessions.push(path);
        }
    }

    sessions.sort();
    sessions.reverse();
    Ok(sessions)
}

pub fn default_storage_dir() -> io::Result<PathBuf> {
    Ok(env::current_dir()?.join("sessions"))
}

fn write_session_files(session: &Session) -> io::Result<()> {
    fs::write(session.path.join("recall.json"), session_json(session))?;
    fs::write(session.path.join("summary.md"), summary_markdown(session))?;
    fs::write(session.path.join("actions.md"), actions_markdown(session))?;
    fs::write(
        session.path.join("transcript.md"),
        transcript_markdown(session),
    )?;
    Ok(())
}

fn session_json(session: &Session) -> String {
    format!(
        r#"{{
  "id": "{}",
  "title": "{}",
  "created_at_unix": {},
  "status": "initialized",
  "consent": {{
    "mode": "{}"
  }},
  "sources": {{
    "microphone": null,
    "call_audio": null
  }},
  "files": {{
    "summary": "summary.md",
    "actions": "actions.md",
    "transcript": "transcript.md",
    "audio_dir": "audio"
  }}
}}
"#,
        escape_json(&session.id),
        escape_json(&session.title),
        session.created_at_unix,
        session.consent.as_str()
    )
}

fn summary_markdown(session: &Session) -> String {
    format!(
        "# {}\n\nStatus: initialized\nConsent: {}\n\n## Summary\n\nPending audio capture and transcription.\n\n## Decisions\n\n- Pending\n\n## Questions\n\n- Pending\n",
        session.title,
        session.consent.as_str()
    )
}

fn actions_markdown(session: &Session) -> String {
    format!(
        "# Action Items: {}\n\n- [ ] Pending audio capture and transcription\n",
        session.title
    )
}

fn transcript_markdown(session: &Session) -> String {
    format!(
        "# Transcript: {}\n\nTranscript will appear here after audio capture and transcription are wired in.\n",
        session.title
    )
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn readable_local_timestamp() -> String {
    let now = OffsetDateTime::now_local().unwrap_or_else(|_| OffsetDateTime::now_utc());
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        now.year(),
        u8::from(now.month()),
        now.day(),
        now.hour(),
        now.minute(),
        now.second()
    )
}

fn slugify(title: &str) -> String {
    let mut slug = String::new();
    let mut last_was_dash = false;

    for ch in title.chars().flat_map(|ch| ch.to_lowercase()) {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_was_dash = false;
        } else if !last_was_dash {
            slug.push('-');
            last_was_dash = true;
        }
    }

    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "meeting".to_string()
    } else {
        slug.to_string()
    }
}

fn escape_json(value: &str) -> String {
    let mut escaped = String::new();
    for ch in value.chars() {
        match ch {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::{escape_json, slugify, ConsentMode};

    #[test]
    fn parses_consent_modes() {
        assert!(matches!(
            ConsentMode::parse("verbal"),
            Some(ConsentMode::Verbal)
        ));
        assert!(matches!(
            ConsentMode::parse("provided"),
            Some(ConsentMode::Noted)
        ));
        assert!(matches!(
            ConsentMode::parse("meeting-policy"),
            Some(ConsentMode::MeetingPolicy)
        ));
        assert!(ConsentMode::parse("mystery").is_none());
    }

    #[test]
    fn slugifies_titles_for_paths() {
        assert_eq!(slugify("Design Sync"), "design-sync");
        assert_eq!(
            slugify("  Billing: retry behavior! "),
            "billing-retry-behavior"
        );
        assert_eq!(slugify("!!!"), "meeting");
    }

    #[test]
    fn escapes_json_strings() {
        assert_eq!(escape_json("a \"quoted\" value"), "a \\\"quoted\\\" value");
        assert_eq!(escape_json("line\nbreak"), "line\\nbreak");
    }
}
