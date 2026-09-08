use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;
use time::{Duration, Month, OffsetDateTime, Weekday};

pub const INTERNAL_DIR: &str = ".recall";

#[derive(Debug, Clone)]
pub struct StartOptions {
    pub title: String,
    pub consent: ConsentMode,
    pub storage_dir: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    fs::create_dir_all(path.join(INTERNAL_DIR))?;

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
        if path.is_dir() && metadata_path(&path).exists() {
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

pub fn append_session_marker(session_path: &Path, elapsed: &str) -> io::Result<()> {
    append_session_entry(&markers_path(session_path), elapsed, "Marker")?;
    refresh_meeting_capture_context(session_path)
}

pub fn append_session_note(session_path: &Path, elapsed: &str, note: &str) -> io::Result<()> {
    append_session_note_with_images(session_path, elapsed, note, &[])
}

pub fn append_session_note_with_images(
    session_path: &Path,
    elapsed: &str,
    caption: &str,
    image_relative_paths: &[String],
) -> io::Result<()> {
    let Some(line) = format_session_note_bullet(elapsed, caption, image_relative_paths) else {
        return Ok(());
    };
    append_session_line(&notes_path(session_path), &line)?;
    refresh_meeting_capture_context(session_path)
}

pub fn format_session_note_bullet(
    elapsed: &str,
    caption: &str,
    image_relative_paths: &[String],
) -> Option<String> {
    let caption = caption.trim();
    if caption.is_empty() && image_relative_paths.is_empty() {
        return None;
    }

    let mut parts = Vec::new();
    if !caption.is_empty() {
        parts.push(caption.to_string());
    }
    for image in image_relative_paths {
        parts.push(format!("[image]({image})"));
    }
    Some(format!("- `{elapsed}` {}", parts.join(" · ")))
}

pub fn images_dir(session_path: &Path) -> PathBuf {
    session_path.join("images")
}

pub fn next_note_image_relative_path(
    session_path: &Path,
    elapsed: &str,
    extension: &str,
) -> String {
    let stem = note_image_stem(elapsed);
    let ext = extension.trim_start_matches('.').to_ascii_lowercase();
    let dir = images_dir(session_path);
    let first = format!("{stem}.{ext}");
    if !dir.join(&first).exists() {
        return format!("images/{first}");
    }

    let mut n = 2u32;
    loop {
        let name = format!("{stem}-{n}.{ext}");
        if !dir.join(&name).exists() {
            return format!("images/{name}");
        }
        n = n.saturating_add(1);
        if n == u32::MAX {
            return format!("images/{name}");
        }
    }
}

pub fn copy_image_into_session(
    session_path: &Path,
    elapsed: &str,
    source: &Path,
) -> io::Result<String> {
    let ext = source
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("png");
    let relative = next_note_image_relative_path(session_path, elapsed, ext);
    let dest = session_path.join(&relative);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(source, &dest)?;
    Ok(relative)
}

pub fn discard_unused_note_images(
    session_path: &Path,
    image_relative_paths: &[String],
) -> io::Result<()> {
    for relative in image_relative_paths {
        let path = session_path.join(relative);
        if path.exists() {
            fs::remove_file(path)?;
        }
    }
    remove_empty_images_dir(session_path)
}

pub fn remove_empty_images_dir(session_path: &Path) -> io::Result<()> {
    let dir = images_dir(session_path);
    if dir.is_dir() && fs::read_dir(&dir)?.next().is_none() {
        fs::remove_dir(dir)?;
    }
    Ok(())
}

pub fn pasted_image_path(text: &str) -> Option<PathBuf> {
    if text.chars().count() > 1024 {
        return None;
    }
    if text
        .chars()
        .any(|ch| ch.is_control() && !matches!(ch, '\n' | '\r' | '\t'))
    {
        return None;
    }

    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    if lines.len() != 1 {
        return None;
    }

    let candidate = strip_wrapping_quotes(lines[0]);
    let path = PathBuf::from(expand_pasted_path(candidate));
    if !is_image_extension(&path) {
        return None;
    }
    path.is_file().then_some(path)
}

fn note_image_stem(elapsed: &str) -> String {
    format!("{}-note", elapsed.replace(':', "-"))
}

fn strip_wrapping_quotes(value: &str) -> &str {
    if value.len() >= 2 {
        let bytes = value.as_bytes();
        if (bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\'')
        {
            return &value[1..value.len() - 1];
        }
    }
    value
}

fn expand_pasted_path(value: &str) -> String {
    let decoded = percent_decode(value);
    let without_scheme = decoded
        .strip_prefix("file://localhost")
        .or_else(|| decoded.strip_prefix("file://"))
        .map(str::to_string)
        .unwrap_or(decoded);
    if let Some(rest) = without_scheme.strip_prefix("~/") {
        if let Some(home) = env::var_os("HOME") {
            return PathBuf::from(home)
                .join(rest)
                .to_string_lossy()
                .into_owned();
        }
    }
    without_scheme
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let Some(hex) = std::str::from_utf8(&bytes[index + 1..index + 3])
                .ok()
                .and_then(|text| u8::from_str_radix(text, 16).ok())
            {
                out.push(hex);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn is_image_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|value| value.to_str())
        .map(|ext| {
            matches!(
                ext.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg" | "gif" | "tif" | "tiff" | "webp" | "bmp" | "heic"
            )
        })
        .unwrap_or(false)
}

fn write_session_files(session: &Session) -> io::Result<()> {
    fs::write(metadata_path(&session.path), session_json(session))?;
    fs::write(session.path.join("meeting.md"), meeting_markdown(session))?;
    fs::write(markers_path(&session.path), markers_markdown(session))?;
    fs::write(notes_path(&session.path), notes_markdown(session))?;
    fs::write(
        session.path.join("transcript.md"),
        transcript_markdown(session),
    )?;
    Ok(())
}

fn append_session_entry(path: &Path, elapsed: &str, text: &str) -> io::Result<()> {
    append_session_line(path, &format!("- `{elapsed}` {text}"))
}

fn append_session_line(path: &Path, line: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    writeln!(file, "{line}")
}

fn read_session_created_at_unix(session_path: &Path) -> Option<u64> {
    let metadata = fs::read_to_string(metadata_path(session_path)).ok()?;
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
    "meeting": "meeting.md",
    "transcript": "transcript.md",
    "markers": ".recall/markers.md",
    "notes": ".recall/notes.md",
    "analysis": ".recall/analysis",
    "transcription": ".recall/transcription",
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

fn meeting_markdown(session: &Session) -> String {
    format!(
        "# {}\n\n> Status: Awaiting capture and transcription  \n> Consent: {}\n\n## Summary\n\n_Pending transcription and analysis._\n\n## Decisions\n\n_None identified yet._\n\n## Action Items\n\n_None identified yet._\n\n## Open Questions\n\n_None identified yet._\n\n## Follow-ups\n\n_None identified yet._\n\n## Notes and Markers\n\n_No notes or markers captured yet._\n\n## Source Material\n\n- [Transcript](transcript.md)\n- [Microphone audio](audio/mic.m4a)\n- [System audio](audio/call.m4a)\n",
        session.title,
        session.consent.as_str()
    )
}

fn markers_markdown(session: &Session) -> String {
    format!(
        "# Markers: {}\n\nMarkers dropped during capture will appear here.\n\n",
        session.title
    )
}

fn notes_markdown(session: &Session) -> String {
    format!(
        "# Notes: {}\n\nManual notes captured during the session will appear here.\n\n",
        session.title
    )
}

fn transcript_markdown(session: &Session) -> String {
    format!(
        "# Transcript: {}\n\nTranscript will appear here after audio capture and transcription are wired in.\n",
        session.title
    )
}

pub fn internal_dir(session_path: &Path) -> PathBuf {
    session_path.join(INTERNAL_DIR)
}

pub fn metadata_path(session_path: &Path) -> PathBuf {
    prefer_current_or_legacy(
        &internal_dir(session_path).join("metadata.json"),
        &session_path.join("recall.json"),
    )
}

pub fn markers_path(session_path: &Path) -> PathBuf {
    prefer_current_or_legacy(
        &internal_dir(session_path).join("markers.md"),
        &session_path.join("markers.md"),
    )
}

pub fn notes_path(session_path: &Path) -> PathBuf {
    prefer_current_or_legacy(
        &internal_dir(session_path).join("notes.md"),
        &session_path.join("notes.md"),
    )
}

pub fn analysis_dir(session_path: &Path) -> PathBuf {
    internal_dir(session_path).join("analysis")
}

pub fn transcription_dir(session_path: &Path) -> PathBuf {
    internal_dir(session_path).join("transcription")
}

pub fn transcription_work_dir(session_path: &Path) -> PathBuf {
    internal_dir(session_path).join("work/transcription")
}

pub fn state_dir(session_path: &Path) -> PathBuf {
    internal_dir(session_path).join("state")
}

pub fn primary_document_path(session_path: &Path) -> PathBuf {
    for file_name in ["meeting.md", "summary.md", "transcript.md"] {
        let path = session_path.join(file_name);
        if path.exists() {
            return path;
        }
    }
    session_path.join("meeting.md")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorInvocation {
    pub program: String,
    pub args: Vec<String>,
}

pub fn editor_invocation(path: &Path, editor: Option<&str>) -> EditorInvocation {
    let path = path.to_string_lossy().into_owned();
    match editor
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "open")
    {
        None => EditorInvocation {
            program: "open".to_string(),
            args: vec![path],
        },
        Some(name) if looks_like_mac_app_name(name) => EditorInvocation {
            program: "open".to_string(),
            args: vec!["-a".to_string(), name.to_string(), path],
        },
        Some(name) => EditorInvocation {
            program: name.to_string(),
            args: vec![path],
        },
    }
}

fn looks_like_mac_app_name(name: &str) -> bool {
    name.ends_with(".app") || name.contains(' ')
}

pub fn open_path(path: &Path, editor: Option<&str>) -> io::Result<()> {
    let invocation = editor_invocation(path, editor);
    let status = Command::new(&invocation.program)
        .args(&invocation.args)
        .status()?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "`{}` failed to open {}",
            invocation.program,
            path.display()
        )))
    }
}

pub fn read_session_title(session_path: &Path) -> io::Result<String> {
    let metadata = fs::read_to_string(metadata_path(session_path))?;
    let value = serde_json::from_str::<Value>(&metadata)?;
    Ok(value
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .unwrap_or("Recall Session")
        .to_string())
}

pub fn latest_session(storage_dir: &Path) -> io::Result<PathBuf> {
    list_sessions(storage_dir)?
        .into_iter()
        .next()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("No Recall sessions found in {}", storage_dir.display()),
            )
        })
}

pub fn is_session_dir(path: &Path) -> bool {
    path.is_dir() && metadata_path(path).exists()
}

pub fn resolve_session_target(storage_dir: &Path, target: &str) -> io::Result<PathBuf> {
    let target = target.trim();
    if target.is_empty() || target == "latest" {
        return latest_session(storage_dir);
    }

    let as_path = PathBuf::from(target);
    if as_path.exists() {
        if is_session_dir(&as_path) {
            return Ok(as_path.canonicalize().unwrap_or_else(|_| as_path.clone()));
        }
        if as_path.is_absolute() || target.contains('/') || target.contains('\\') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is not a Recall session folder.", as_path.display()),
            ));
        }
    }

    let stored = storage_dir.join(target);
    if is_session_dir(&stored) {
        return Ok(stored);
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        format!(
            "No Recall session matching '{target}' in {}. Pass a session folder name, a session path, or latest.",
            storage_dir.display()
        ),
    ))
}

pub fn read_session_consent(session_path: &Path) -> Option<ConsentMode> {
    let metadata = fs::read_to_string(metadata_path(session_path)).ok()?;
    let value = serde_json::from_str::<Value>(&metadata).ok()?;
    let mode = value.get("consent")?.get("mode")?.as_str()?;
    ConsentMode::parse(mode)
}

pub fn resume_hint(session_path: &Path) -> Option<String> {
    let id = session_path.file_name()?.to_str()?.trim();
    if id.is_empty() {
        return None;
    }
    Some(format!(
        "Resume this session with:\n  recall --resume {id}\nOr: recall --resume latest"
    ))
}

pub fn export_session(session_path: &Path, output_path: Option<&Path>) -> io::Result<PathBuf> {
    let meeting_path = primary_document_path(session_path);
    if !meeting_path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("Missing meeting document at {}", meeting_path.display()),
        ));
    }

    let transcript_path = session_path.join("transcript.md");
    if !transcript_path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("Missing transcript at {}", transcript_path.display()),
        ));
    }

    let meeting = meeting_content_for_export(session_path, &meeting_path)?;
    let transcript = fs::read_to_string(&transcript_path)?;
    let meeting = meeting
        .split_once("\n## Source Material\n")
        .map(|(content, _)| content)
        .unwrap_or(meeting.as_str());
    let transcript_body = transcript
        .split_once('\n')
        .map(|(_, body)| body.trim_start())
        .unwrap_or(transcript.as_str());
    let export = format!(
        "{}\n\n> Exported by Recall. Audio remains in the original local session.\n\n---\n\n# Full Transcript\n\n{}",
        meeting.trim_end(),
        transcript_body
    );

    let output_path = output_path
        .map(Path::to_path_buf)
        .unwrap_or_else(|| session_path.join("meeting-export.md"));
    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&output_path, export)?;
    Ok(output_path)
}

pub fn mark_transcript_ready(session_path: &Path) -> io::Result<()> {
    let meeting_path = session_path.join("meeting.md");
    if !meeting_path.exists() {
        return Ok(());
    }

    let meeting = fs::read_to_string(&meeting_path)?;
    let meeting = meeting
        .replace(
            "> Status: Awaiting capture and transcription",
            "> Status: Transcript ready; analysis pending",
        )
        .replace(
            "_Pending transcription and analysis._",
            "_Transcript ready. Run agent analysis to generate meeting notes._",
        );
    fs::write(meeting_path, meeting)
}

fn refresh_meeting_capture_context(session_path: &Path) -> io::Result<()> {
    let meeting_path = session_path.join("meeting.md");
    if !meeting_path.exists() {
        return Ok(());
    }

    let meeting = fs::read_to_string(&meeting_path)?;
    let Some((before, remainder)) = meeting.split_once("## Notes and Markers\n") else {
        return Ok(());
    };
    let Some((_, after)) = remainder.split_once("## Source Material\n") else {
        return Ok(());
    };

    let notes = session_entries(&notes_path(session_path))?;
    let markers = session_entries(&markers_path(session_path))?;
    let mut context = String::from("## Notes and Markers\n\n");
    append_entry_section(&mut context, "Notes", &notes);
    append_entry_section(&mut context, "Markers", &markers);
    let updated = format!("{before}{context}## Source Material\n{after}");
    fs::write(meeting_path, updated)
}

fn session_entries(path: &Path) -> io::Result<Vec<String>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    Ok(fs::read_to_string(path)?
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("- `"))
        .map(str::to_string)
        .collect())
}

fn append_entry_section(output: &mut String, heading: &str, entries: &[String]) {
    output.push_str(&format!("### {heading}\n\n"));
    if entries.is_empty() {
        output.push_str("_None captured._\n\n");
    } else {
        output.push_str(&entries.join("\n"));
        output.push_str("\n\n");
    }
}

fn prefer_current_or_legacy(current: &Path, legacy: &Path) -> PathBuf {
    if current.exists() || !legacy.exists() {
        current.to_path_buf()
    } else {
        legacy.to_path_buf()
    }
}

fn meeting_content_for_export(session_path: &Path, meeting_path: &Path) -> io::Result<String> {
    if meeting_path
        .file_name()
        .is_some_and(|name| name == "meeting.md")
    {
        return fs::read_to_string(meeting_path);
    }

    let mut sections = Vec::new();
    for file_name in [
        "summary.md",
        "actions.md",
        "decisions.md",
        "questions.md",
        "followups.md",
        "notes.md",
        "markers.md",
    ] {
        let path = session_path.join(file_name);
        if path.exists() {
            sections.push(fs::read_to_string(path)?);
        }
    }
    Ok(sections.join("\n\n"))
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
        append_session_marker, append_session_note, append_session_note_with_images,
        copy_image_into_session, discard_unused_note_images, eastern_offset_hours,
        editor_invocation, escape_json, export_session, format_session_note_bullet, list_sessions,
        mark_transcript_ready, next_note_image_relative_path, pasted_image_path,
        read_session_consent, readable_eastern_timestamp_for, resolve_session_target, resume_hint,
        slugify, start_session, ConsentMode, StartOptions,
    };
    use std::fs;
    use std::path::{Path, PathBuf};
    use time::{Date, Month};

    #[test]
    fn editor_invocation_defaults_to_macos_open() {
        let path = Path::new("/tmp/recall-session");
        let open = editor_invocation(path, None);
        assert_eq!(open.program, "open");
        assert_eq!(open.args, vec!["/tmp/recall-session"]);

        let code = editor_invocation(path, Some("code"));
        assert_eq!(code.program, "code");
        assert_eq!(code.args, vec!["/tmp/recall-session"]);

        let app = editor_invocation(path, Some("Visual Studio Code"));
        assert_eq!(app.program, "open");
        assert_eq!(
            app.args,
            vec!["-a", "Visual Studio Code", "/tmp/recall-session"]
        );
    }

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

    #[test]
    fn appends_markers_and_notes_to_session_files() {
        let session_dir = std::env::temp_dir().join(format!(
            "recall-session-event-test-{}-{}",
            std::process::id(),
            super::unix_timestamp()
        ));
        fs::create_dir_all(&session_dir).unwrap();

        append_session_marker(&session_dir, "00:42").unwrap();
        append_session_note(&session_dir, "00:43", "Follow up on budget").unwrap();

        let markers = fs::read_to_string(session_dir.join(".recall/markers.md")).unwrap();
        let notes = fs::read_to_string(session_dir.join(".recall/notes.md")).unwrap();

        assert!(markers.contains("- `00:42` Marker"));
        assert!(notes.contains("- `00:43` Follow up on budget"));

        let _ = fs::remove_dir_all(session_dir);
    }

    #[test]
    fn new_sessions_keep_only_primary_documents_visible() {
        let storage_dir = std::env::temp_dir().join(format!(
            "recall-session-layout-test-{}-{}",
            std::process::id(),
            super::unix_timestamp()
        ));
        let session = start_session(&StartOptions {
            title: "Layout Test".to_string(),
            consent: ConsentMode::Noted,
            storage_dir: storage_dir.clone(),
        })
        .unwrap();

        assert!(session.path.join("meeting.md").exists());
        assert!(session.path.join("transcript.md").exists());
        assert!(session.path.join("audio").is_dir());
        assert!(session.path.join(".recall/metadata.json").exists());
        assert!(session.path.join(".recall/markers.md").exists());
        assert!(session.path.join(".recall/notes.md").exists());
        assert!(!session.path.join("summary.md").exists());
        assert!(!session.path.join("actions.md").exists());
        assert!(!session.path.join("recall.json").exists());

        append_session_note(&session.path, "00:12", "Capture this detail").unwrap();
        append_session_marker(&session.path, "00:20").unwrap();
        let meeting = fs::read_to_string(session.path.join("meeting.md")).unwrap();
        assert!(meeting.contains("### Notes"));
        assert!(meeting.contains("Capture this detail"));
        assert!(meeting.contains("### Markers"));
        assert!(meeting.contains("`00:20` Marker"));

        mark_transcript_ready(&session.path).unwrap();
        let meeting = fs::read_to_string(session.path.join("meeting.md")).unwrap();
        assert!(meeting.contains("Transcript ready; analysis pending"));

        let _ = fs::remove_dir_all(storage_dir);
    }

    #[test]
    fn lists_current_and_legacy_session_layouts() {
        let storage_dir = std::env::temp_dir().join(format!(
            "recall-mixed-layout-test-{}-{}",
            std::process::id(),
            super::unix_timestamp()
        ));
        let legacy = storage_dir.join("legacy");
        let current = storage_dir.join("current");
        fs::create_dir_all(&legacy).unwrap();
        fs::create_dir_all(current.join(".recall")).unwrap();
        fs::write(legacy.join("recall.json"), r#"{"created_at_unix": 100}"#).unwrap();
        fs::write(
            current.join(".recall/metadata.json"),
            r#"{"created_at_unix": 200}"#,
        )
        .unwrap();

        let sessions = list_sessions(&storage_dir).unwrap();
        assert_eq!(sessions, vec![current, legacy]);

        let _ = fs::remove_dir_all(storage_dir);
    }

    #[test]
    fn exports_meeting_and_transcript_as_one_markdown_file() {
        let session_dir = std::env::temp_dir().join(format!(
            "recall-export-test-{}-{}",
            std::process::id(),
            super::unix_timestamp()
        ));
        fs::create_dir_all(&session_dir).unwrap();
        fs::write(
            session_dir.join("meeting.md"),
            "# Project Sync\n\n## Summary\n\nDone.",
        )
        .unwrap();
        fs::write(
            session_dir.join("transcript.md"),
            "# Transcript: Project Sync\n\n## Clean Conversation\n\nHello.",
        )
        .unwrap();

        let output = export_session(&session_dir, None).unwrap();
        let exported = fs::read_to_string(&output).unwrap();
        assert!(exported.contains("# Project Sync"));
        assert!(exported.contains("# Full Transcript"));
        assert!(exported.contains("## Clean Conversation"));
        assert!(!exported.contains("# Transcript: Project Sync"));
        assert!(exported.contains("Audio remains in the original local session"));

        let _ = fs::remove_dir_all(session_dir);
    }

    #[test]
    fn export_preserves_split_files_from_legacy_sessions() {
        let session_dir = std::env::temp_dir().join(format!(
            "recall-legacy-export-test-{}-{}",
            std::process::id(),
            super::unix_timestamp()
        ));
        fs::create_dir_all(&session_dir).unwrap();
        fs::write(
            session_dir.join("summary.md"),
            "# Summary\n\nLaunch planning.",
        )
        .unwrap();
        fs::write(
            session_dir.join("actions.md"),
            "# Actions\n\n- [ ] Publish the plan",
        )
        .unwrap();
        fs::write(
            session_dir.join("transcript.md"),
            "# Transcript\n\nThe launch is Friday.",
        )
        .unwrap();

        let output = export_session(&session_dir, None).unwrap();
        let exported = fs::read_to_string(&output).unwrap();
        assert!(exported.contains("Launch planning."));
        assert!(exported.contains("Publish the plan"));
        assert!(exported.contains("The launch is Friday."));

        let _ = fs::remove_dir_all(session_dir);
    }

    #[test]
    fn resolves_latest_path_and_session_id_without_creating_a_folder() {
        let storage_dir = std::env::temp_dir().join(format!(
            "recall-resolve-session-{}-{}",
            std::process::id(),
            super::unix_timestamp()
        ));
        let older = storage_dir.join("05-26-2026_7-21pm-et-older");
        let newer = storage_dir.join("05-26-2026_8-21pm-et-design-sync");
        fs::create_dir_all(older.join(".recall")).unwrap();
        fs::create_dir_all(newer.join(".recall")).unwrap();
        fs::write(
            older.join(".recall/metadata.json"),
            r#"{"created_at_unix": 100, "title": "Older", "consent": {"mode": "none"}}"#,
        )
        .unwrap();
        fs::write(
            newer.join(".recall/metadata.json"),
            r#"{"created_at_unix": 200, "title": "Design sync", "consent": {"mode": "noted"}}"#,
        )
        .unwrap();

        let before = fs::read_dir(&storage_dir).unwrap().count();
        assert_eq!(
            resolve_session_target(&storage_dir, "latest").unwrap(),
            newer
        );
        assert_eq!(
            resolve_session_target(&storage_dir, "05-26-2026_8-21pm-et-design-sync").unwrap(),
            newer
        );
        assert_eq!(
            resolve_session_target(&storage_dir, newer.to_str().unwrap()).unwrap(),
            newer.canonicalize().unwrap()
        );
        assert_eq!(fs::read_dir(&storage_dir).unwrap().count(), before);
        assert_eq!(read_session_consent(&newer), Some(ConsentMode::Noted));
        assert!(resolve_session_target(&storage_dir, "missing-session")
            .unwrap_err()
            .to_string()
            .contains("No Recall session matching"));

        let _ = fs::remove_dir_all(storage_dir);
    }

    #[test]
    fn resume_hint_uses_the_session_folder_name() {
        let path = PathBuf::from("/tmp/sessions/05-26-2026_7-21pm-et-grill-supper-and-weber-gift");
        assert_eq!(
            resume_hint(&path).unwrap(),
            "Resume this session with:\n  recall --resume 05-26-2026_7-21pm-et-grill-supper-and-weber-gift\nOr: recall --resume latest"
        );
        assert!(resume_hint(Path::new("/")).is_none());
    }

    fn tiny_png() -> Vec<u8> {
        vec![
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ]
    }

    #[test]
    fn text_only_note_bullet_has_no_image_link() {
        assert_eq!(
            format_session_note_bullet("00:43", "Follow up on budget", &[]),
            Some("- `00:43` Follow up on budget".to_string())
        );
        assert_eq!(format_session_note_bullet("00:43", "  ", &[]), None);
    }

    #[test]
    fn image_note_bullet_uses_root_relative_markdown_link() {
        assert_eq!(
            format_session_note_bullet(
                "12:04",
                "whiteboard",
                &["images/12-04-note.png".to_string()]
            ),
            Some("- `12:04` whiteboard · [image](images/12-04-note.png)".to_string())
        );
        assert_eq!(
            format_session_note_bullet("12:04", "", &["images/12-04-note.png".to_string()]),
            Some("- `12:04` [image](images/12-04-note.png)".to_string())
        );
    }

    #[test]
    fn second_image_in_the_same_second_gets_a_counter_suffix() {
        let session_dir = std::env::temp_dir().join(format!(
            "recall-note-image-name-{}-{}",
            std::process::id(),
            super::unix_timestamp()
        ));
        fs::create_dir_all(session_dir.join("images")).unwrap();
        assert_eq!(
            next_note_image_relative_path(&session_dir, "12:04", "png"),
            "images/12-04-note.png"
        );
        fs::write(session_dir.join("images/12-04-note.png"), tiny_png()).unwrap();
        assert_eq!(
            next_note_image_relative_path(&session_dir, "12:04", "png"),
            "images/12-04-note-2.png"
        );
        fs::write(session_dir.join("images/12-04-note-2.png"), tiny_png()).unwrap();
        assert_eq!(
            next_note_image_relative_path(&session_dir, "12:04", "png"),
            "images/12-04-note-3.png"
        );
        let _ = fs::remove_dir_all(session_dir);
    }

    #[test]
    fn image_note_writes_file_and_meeting_markdown_link() {
        let storage_dir = std::env::temp_dir().join(format!(
            "recall-note-image-save-{}-{}",
            std::process::id(),
            super::unix_timestamp()
        ));
        let session = start_session(&StartOptions {
            title: "Image Note".to_string(),
            consent: ConsentMode::Noted,
            storage_dir: storage_dir.clone(),
        })
        .unwrap();
        let source = storage_dir.join("source.png");
        fs::write(&source, tiny_png()).unwrap();

        let relative = copy_image_into_session(&session.path, "12:04", &source).unwrap();
        assert_eq!(relative, "images/12-04-note.png");
        append_session_note_with_images(&session.path, "12:04", "whiteboard", &[relative.clone()])
            .unwrap();

        let notes = fs::read_to_string(session.path.join(".recall/notes.md")).unwrap();
        let meeting = fs::read_to_string(session.path.join("meeting.md")).unwrap();
        assert!(session.path.join("images/12-04-note.png").is_file());
        assert!(notes.contains("- `12:04` whiteboard · [image](images/12-04-note.png)"));
        assert!(meeting.contains("[image](images/12-04-note.png)"));

        let _ = fs::remove_dir_all(storage_dir);
    }

    #[test]
    fn discarding_unused_note_images_removes_orphans() {
        let storage_dir = std::env::temp_dir().join(format!(
            "recall-note-image-discard-{}-{}",
            std::process::id(),
            super::unix_timestamp()
        ));
        let session = start_session(&StartOptions {
            title: "Discard Images".to_string(),
            consent: ConsentMode::Noted,
            storage_dir: storage_dir.clone(),
        })
        .unwrap();
        let source = storage_dir.join("source.png");
        fs::write(&source, tiny_png()).unwrap();
        let relative = copy_image_into_session(&session.path, "12:04", &source).unwrap();
        assert!(session.path.join(&relative).is_file());

        discard_unused_note_images(&session.path, &[relative.clone()]).unwrap();
        assert!(!session.path.join("images").exists());

        let _ = fs::remove_dir_all(storage_dir);
    }

    #[test]
    fn pasted_image_path_ignores_non_image_text() {
        assert!(pasted_image_path("hello from the clipboard").is_none());
        assert!(pasted_image_path("see images/12-04-note.png later").is_none());

        let storage_dir = std::env::temp_dir().join(format!(
            "recall-pasted-image-path-{}-{}",
            std::process::id(),
            super::unix_timestamp()
        ));
        fs::create_dir_all(&storage_dir).unwrap();
        let image = storage_dir.join("board.png");
        fs::write(&image, tiny_png()).unwrap();
        assert_eq!(
            pasted_image_path(&format!("\"{}\"", image.display())),
            Some(image)
        );
        let _ = fs::remove_dir_all(storage_dir);
    }
}
