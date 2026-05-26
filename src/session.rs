use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;
use time::{Duration, Month, OffsetDateTime, Weekday};

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
    let id = format!("{}-et-{slug}", readable_eastern_timestamp());
    let path = unique_session_path(&options.storage_dir, &id);
    let id = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(&id)
        .to_string();

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
            let created_at = read_session_created_at_unix(&path).unwrap_or(0);
            sessions.push((created_at, path));
        }
    }

    sessions.sort_by(|(left_created, left_path), (right_created, right_path)| {
        right_created
            .cmp(left_created)
            .then_with(|| right_path.cmp(left_path))
    });
    Ok(sessions.into_iter().map(|(_, path)| path).collect())
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

fn read_session_created_at_unix(session_path: &Path) -> Option<u64> {
    let metadata = fs::read_to_string(session_path.join("recall.json")).ok()?;
    let value = serde_json::from_str::<Value>(&metadata).ok()?;
    value.get("created_at_unix")?.as_u64()
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

fn readable_eastern_timestamp() -> String {
    let now_utc = OffsetDateTime::now_utc();
    readable_eastern_timestamp_for(now_utc)
}

fn readable_eastern_timestamp_for(now_utc: OffsetDateTime) -> String {
    let now = now_utc + Duration::hours(i64::from(eastern_offset_hours(now_utc)));
    let hour = now.hour();
    let suffix = if hour < 12 { "am" } else { "pm" };
    let hour_12 = match hour % 12 {
        0 => 12,
        value => value,
    };
    format!(
        "{:02}-{:02}-{:04}_{}-{:02}{suffix}",
        u8::from(now.month()),
        now.day(),
        now.year(),
        hour_12,
        now.minute()
    )
}

fn eastern_offset_hours(utc: OffsetDateTime) -> i8 {
    let year = utc.year();
    let dst_start = us_eastern_dst_start_utc(year);
    let dst_end = us_eastern_dst_end_utc(year);
    if utc >= dst_start && utc < dst_end {
        -4
    } else {
        -5
    }
}

fn us_eastern_dst_start_utc(year: i32) -> OffsetDateTime {
    let day = nth_weekday_of_month_day(year, Month::March, Weekday::Sunday, 2);
    time::Date::from_calendar_date(year, Month::March, day)
        .expect("valid DST start date")
        .with_hms(7, 0, 0)
        .expect("valid DST start time")
        .assume_utc()
}

fn us_eastern_dst_end_utc(year: i32) -> OffsetDateTime {
    let day = nth_weekday_of_month_day(year, Month::November, Weekday::Sunday, 1);
    time::Date::from_calendar_date(year, Month::November, day)
        .expect("valid DST end date")
        .with_hms(6, 0, 0)
        .expect("valid DST end time")
        .assume_utc()
}

fn nth_weekday_of_month_day(year: i32, month: Month, weekday: Weekday, occurrence: u8) -> u8 {
    let mut seen = 0;
    for day in 1..=31 {
        let Ok(date) = time::Date::from_calendar_date(year, month, day) else {
            break;
        };
        if date.weekday() == weekday {
            seen += 1;
            if seen == occurrence {
                return day;
            }
        }
    }
    unreachable!("requested weekday occurrence should exist")
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

fn unique_session_path(parent: &Path, base_name: &str) -> PathBuf {
    let mut candidate = parent.join(base_name);
    if !candidate.exists() {
        return candidate;
    }

    for index in 2.. {
        candidate = parent.join(format!("{base_name}-{index}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!("loop returns once a unique session path is found")
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
    use super::{
        eastern_offset_hours, escape_json, list_sessions, readable_eastern_timestamp_for, slugify,
        ConsentMode,
    };
    use std::fs;
    use time::{Date, Month};

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

    #[test]
    fn eastern_offset_obeys_us_dst_boundaries() {
        let winter = Date::from_calendar_date(2026, Month::January, 15)
            .unwrap()
            .with_hms(12, 0, 0)
            .unwrap()
            .assume_utc();
        let summer = Date::from_calendar_date(2026, Month::May, 26)
            .unwrap()
            .with_hms(12, 0, 0)
            .unwrap()
            .assume_utc();

        assert_eq!(eastern_offset_hours(winter), -5);
        assert_eq!(eastern_offset_hours(summer), -4);
    }

    #[test]
    fn formats_eastern_timestamp_for_paths() {
        let utc = Date::from_calendar_date(2026, Month::May, 26)
            .unwrap()
            .with_hms(23, 21, 45)
            .unwrap()
            .assume_utc();

        assert_eq!(readable_eastern_timestamp_for(utc), "05-26-2026_7-21pm");
    }

    #[test]
    fn lists_sessions_by_created_at_instead_of_folder_name() {
        let storage_dir = std::env::temp_dir().join(format!(
            "recall-session-list-test-{}",
            super::unix_timestamp()
        ));
        let older = storage_dir.join("05-26-2026_9-30pm-et-older");
        let newer = storage_dir.join("05-26-2026_11-00pm-et-newer");

        fs::create_dir_all(&older).unwrap();
        fs::create_dir_all(&newer).unwrap();
        fs::write(older.join("recall.json"), r#"{"created_at_unix": 100}"#).unwrap();
        fs::write(newer.join("recall.json"), r#"{"created_at_unix": 200}"#).unwrap();

        let sessions = list_sessions(&storage_dir).unwrap();

        assert_eq!(sessions.first(), Some(&newer));
        assert_eq!(sessions.get(1), Some(&older));

        let _ = fs::remove_dir_all(storage_dir);
    }
}
