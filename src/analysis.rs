use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Deserialize;
use serde_json::Value;

use crate::session::{
    analysis_dir, default_storage_dir, list_sessions, markers_path, metadata_path, notes_path,
    read_session_title,
};

#[derive(Debug, Clone)]
pub struct AnalyzeOptions {
    pub target: AnalyzeTarget,
    pub storage_dir: Option<PathBuf>,
    pub agent: String,
    pub preset: String,
    pub dry_run: bool,
}

#[derive(Debug, Clone)]
pub enum AnalyzeTarget {
    Latest,
    Session(PathBuf),
}

#[derive(Debug, Clone)]
pub struct AnalyzeResult {
    pub session_path: PathBuf,
    pub prompt_path: PathBuf,
    pub raw_output_path: Option<PathBuf>,
    pub result_path: Option<PathBuf>,
    pub written_files: Vec<PathBuf>,
    pub generated_title: Option<String>,
    pub dry_run: bool,
}

#[derive(Debug, Clone)]
struct AgentProfile {
    command: &'static str,
    args: &'static [&'static str],
    raw_output_file: &'static str,
}

#[derive(Debug, Deserialize, Default)]
struct AgentMeetingResult {
    #[serde(default, alias = "suggested_title", alias = "meeting_title")]
    title: Option<String>,
    summary: Option<String>,
    #[serde(default)]
    decisions: Vec<Decision>,
    #[serde(default, alias = "actionItems")]
    action_items: Vec<ActionItem>,
    #[serde(default)]
    questions: Vec<Question>,
    #[serde(default)]
    followups: Vec<Followup>,
}

#[derive(Debug, Deserialize)]
struct Decision {
    decision: String,
    evidence: Option<String>,
    timestamp: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ActionItem {
    task: String,
    owner: Option<String>,
    due: Option<String>,
    evidence: Option<String>,
    timestamp: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Question {
    question: String,
    context: Option<String>,
    timestamp: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Followup {
    item: String,
    reason: Option<String>,
}

pub fn analyze(options: &AnalyzeOptions) -> io::Result<AnalyzeResult> {
    let mut session_path = resolve_session_path(options)?;
    let transcript_path = session_path.join("transcript.md");
    if !transcript_path.exists() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("Missing clean transcript at {}", transcript_path.display()),
        ));
    }

    let debug_dir = analysis_dir(&session_path);
    if debug_dir.exists() {
        fs::remove_dir_all(&debug_dir)?;
    }
    fs::create_dir_all(&debug_dir)?;

    let prompt = analysis_prompt(&transcript_path, &options.preset)?;
    let prompt_path = debug_dir.join("prompt.md");
    fs::write(&prompt_path, &prompt)?;

    if options.dry_run {
        return Ok(AnalyzeResult {
            session_path,
            prompt_path,
            raw_output_path: None,
            result_path: None,
            written_files: Vec::new(),
            generated_title: None,
            dry_run: true,
        });
    }

    let profile = agent_profile(&options.agent).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "Unknown agent '{}'. Use {}.",
                options.agent,
                known_agents().join(", ")
            ),
        )
    })?;

    let output = run_agent(&profile, &prompt, &transcript_path)?;
    let raw_output_path = debug_dir.join(profile.raw_output_file);
    fs::write(&raw_output_path, &output)?;

    let result_value = extract_agent_result_json(&output).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "Agent finished, but Recall could not extract the expected JSON result.",
        )
    })?;

    let result_path = debug_dir.join("agent-result.json");
    fs::write(&result_path, serde_json::to_string_pretty(&result_value)?)?;

    let result = serde_json::from_value::<AgentMeetingResult>(result_value).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("Agent JSON did not match Recall's analysis schema: {error}"),
        )
    })?;

    let generated_title = normalized_title(result.title.as_deref());
    if let Some(title) = &generated_title {
        session_path = rename_session_dir_for_title(&session_path, title)?;
    }

    let debug_dir = analysis_dir(&session_path);
    let prompt_path = debug_dir.join("prompt.md");
    let raw_output_path = debug_dir.join(profile.raw_output_file);
    let result_path = debug_dir.join("agent-result.json");
    let written_files =
        write_analysis_markdown(&session_path, &result, generated_title.as_deref())?;

    Ok(AnalyzeResult {
        session_path,
        prompt_path,
        raw_output_path: Some(raw_output_path),
        result_path: Some(result_path),
        written_files,
        generated_title,
        dry_run: false,
    })
}

pub fn known_agents() -> Vec<&'static str> {
    vec!["grok", "cline", "codex", "claude", "opencode", "pi"]
}

fn resolve_session_path(options: &AnalyzeOptions) -> io::Result<PathBuf> {
    match &options.target {
        AnalyzeTarget::Session(path) => Ok(path.clone()),
        AnalyzeTarget::Latest => {
            let storage_dir = match &options.storage_dir {
                Some(path) => path.clone(),
                None => default_storage_dir()?,
            };
            let sessions = list_sessions(&storage_dir)?;
            sessions.into_iter().next().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("No Recall sessions found in {}", storage_dir.display()),
                )
            })
        }
    }
}

fn agent_profile(agent: &str) -> Option<AgentProfile> {
    match agent {
        "grok" => Some(AgentProfile {
            command: "grok",
            args: &["-p", "{prompt}", "--output-format", "json"],
            raw_output_file: "agent-raw-output.json",
        }),
        "cline" => Some(AgentProfile {
            command: "cline",
            args: &["--json", "{prompt}"],
            raw_output_file: "agent-raw-output.jsonl",
        }),
        "codex" => Some(AgentProfile {
            command: "codex",
            args: &["exec", "--json", "{prompt}"],
            raw_output_file: "agent-raw-output.json",
        }),
        "claude" => Some(AgentProfile {
            command: "claude",
            args: &["--bare", "-p", "{prompt}", "--output-format", "json"],
            raw_output_file: "agent-raw-output.json",
        }),
        "opencode" => Some(AgentProfile {
            command: "opencode",
            args: &[
                "run",
                "--format",
                "json",
                "--pure",
                "--title",
                "Recall analysis",
                "--file",
                "{transcript}",
                "{prompt}",
            ],
            raw_output_file: "agent-raw-output.jsonl",
        }),
        "pi" => Some(AgentProfile {
            command: "pi",
            args: &["--mode", "json", "--no-session", "{prompt}"],
            raw_output_file: "agent-raw-output.jsonl",
        }),
        _ => None,
    }
}

fn run_agent(profile: &AgentProfile, prompt: &str, transcript_path: &Path) -> io::Result<String> {
    let mut command = Command::new(profile.command);
    for arg in profile.args {
        match *arg {
            "{prompt}" => {
                command.arg(prompt);
            }
            "{transcript}" => {
                command.arg(transcript_path);
            }
            other => {
                command.arg(other);
            }
        }
    }

    let output = command
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .stdout(Stdio::piped())
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(io::Error::other(format!(
            "{} exited with status {}: {}",
            profile.command,
            output.status,
            stderr.trim()
        )));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn analysis_prompt(transcript_path: &Path, preset: &str) -> io::Result<String> {
    let transcript = fs::read_to_string(transcript_path)?;
    Ok(format!(
        r#"You are analyzing a Recall meeting transcript.

Preset: {preset}

Use only the transcript below as the source of truth. Do not browse the filesystem, edit files, or call tools unless required to return the JSON. Return exactly one JSON object and no prose outside JSON.

--- transcript.md ({transcript_path}) ---
{transcript}
--- end transcript ---

Use this schema:

{{
  "summary": "Concise meeting summary.",
  "title": "Concise meeting title, 3 to 8 words, no date.",
  "decisions": [
    {{
      "decision": "What was decided",
      "evidence": "Short transcript reference or quote",
      "timestamp": "00:12.300"
    }}
  ],
  "action_items": [
    {{
      "task": "What needs to happen",
      "owner": "Name or unknown",
      "due": "Date or null",
      "evidence": "Short transcript reference",
      "timestamp": "00:34.100"
    }}
  ],
  "questions": [
    {{
      "question": "Open question",
      "context": "Why it matters",
      "timestamp": "01:02.000"
    }}
  ],
  "followups": [
    {{
      "item": "Follow-up item",
      "reason": "Why it should be followed up"
    }}
  ]
}}

If a field has no items, return an empty array. Use null when owner, due, evidence, or timestamp is unknown.
If the transcript title is generic, such as Quick Capture, infer a specific useful title from the conversation.
"#,
        transcript_path = transcript_path.display(),
        transcript = transcript,
        preset = preset
    ))
}

fn extract_agent_result_json(output: &str) -> Option<Value> {
    if let Ok(value) = serde_json::from_str::<Value>(output.trim()) {
        if let Some(result) = find_meeting_result(&value) {
            return Some(result);
        }
    }

    let mut last_result = None;
    for line in output.lines() {
        if let Ok(value) = serde_json::from_str::<Value>(line.trim()) {
            if let Some(result) = find_meeting_result(&value) {
                last_result = Some(result);
            }
        }
    }
    if last_result.is_some() {
        return last_result;
    }

    json_from_text(output).filter(looks_like_meeting_result)
}

fn find_meeting_result(value: &Value) -> Option<Value> {
    find_meeting_result_inner(value, 0)
}

fn find_meeting_result_inner(value: &Value, depth: usize) -> Option<Value> {
    if depth > 6 {
        return None;
    }

    if looks_like_meeting_result(value) {
        return Some(value.clone());
    }

    for text in candidate_texts(value) {
        if let Some(parsed) = json_from_text(&text) {
            if looks_like_meeting_result(&parsed) {
                return Some(parsed);
            }
        }
    }

    for key in ["result", "content", "message", "part", "output", "response"] {
        if let Some(nested) = value.get(key) {
            if let Some(found) = find_meeting_result_inner(nested, depth + 1) {
                return Some(found);
            }
            if let Some(items) = nested.as_array() {
                for item in items.iter().rev() {
                    if let Some(found) = find_meeting_result_inner(item, depth + 1) {
                        return Some(found);
                    }
                }
            }
        }
    }

    None
}

fn candidate_texts(value: &Value) -> Vec<String> {
    let mut texts = Vec::new();
    push_text(&mut texts, value.as_str());
    for key in ["text", "content", "result", "output", "response"] {
        push_text(&mut texts, value.get(key).and_then(Value::as_str));
    }
    if let Some(part) = value.get("part") {
        push_text(&mut texts, part.get("text").and_then(Value::as_str));
    }
    if let Some(message) = value.get("message") {
        push_text(&mut texts, message.get("text").and_then(Value::as_str));
        if let Some(items) = message.get("content").and_then(Value::as_array) {
            for item in items {
                push_text(&mut texts, item.as_str());
                push_text(&mut texts, item.get("text").and_then(Value::as_str));
            }
        }
    }
    texts
}

fn push_text(texts: &mut Vec<String>, value: Option<&str>) {
    if let Some(text) = value {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            texts.push(trimmed.to_string());
        }
    }
}

fn json_from_text(text: &str) -> Option<Value> {
    let trimmed = text.trim();
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        return Some(value);
    }
    extract_json_object(trimmed).and_then(|json_text| serde_json::from_str(json_text).ok())
}

fn looks_like_meeting_result(value: &Value) -> bool {
    value.is_object()
        && (value.get("summary").is_some()
            || value.get("title").is_some()
            || value.get("action_items").is_some()
            || value.get("decisions").is_some())
}

fn extract_json_object(output: &str) -> Option<&str> {
    let start = output.find('{')?;
    let end = output.rfind('}')?;
    (start <= end).then_some(&output[start..=end])
}

fn rename_session_dir_for_title(session_path: &Path, title: &str) -> io::Result<PathBuf> {
    let Some(parent) = session_path.parent() else {
        return Ok(session_path.to_path_buf());
    };
    let Some(current_name) = session_path.file_name().and_then(|name| name.to_str()) else {
        return Ok(session_path.to_path_buf());
    };
    let Some(prefix) = session_timestamp_prefix(current_name) else {
        return Ok(session_path.to_path_buf());
    };

    let slug = title_slug(title);
    let base_name = format!("{prefix}-et-{slug}");
    let target = unique_session_path(parent, &base_name);

    if target == session_path {
        return Ok(session_path.to_path_buf());
    }

    fs::rename(session_path, &target)?;
    Ok(target)
}

fn session_timestamp_prefix(name: &str) -> Option<String> {
    if let Some((prefix, _rest)) = name.split_once("-et-") {
        if prefix.is_empty() {
            return None;
        }
        return compact_timestamp_to_readable(prefix).or_else(|| Some(prefix.to_string()));
    }

    compact_timestamp_to_readable(name)
}

fn compact_timestamp_to_readable(prefix: &str) -> Option<String> {
    let name = prefix;
    let bytes = name.as_bytes();
    if bytes.len() < 15 {
        return None;
    }
    let date_ok = bytes[0..8].iter().all(u8::is_ascii_digit);
    let dash_ok = bytes[8] == b'-';
    let time_ok = bytes[9..15].iter().all(u8::is_ascii_digit);
    if !(date_ok && dash_ok && time_ok) {
        return None;
    }

    let year = &name[0..4];
    let month = &name[4..6];
    let day = &name[6..8];
    let hour_24: u8 = name[9..11].parse().ok()?;
    let minute = &name[11..13];
    let suffix = if hour_24 < 12 { "am" } else { "pm" };
    let hour_12 = match hour_24 % 12 {
        0 => 12,
        value => value,
    };

    Some(format!("{month}-{day}-{year}_{hour_12}-{minute}{suffix}"))
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

fn title_slug(title: &str) -> String {
    let mut slug = String::new();
    let mut last_was_dash = false;

    for ch in title.chars().flat_map(|ch| ch.to_lowercase()) {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_was_dash = false;
        } else if ch == '\'' || ch == '\u{2019}' {
            continue;
        } else if !last_was_dash {
            slug.push('-');
            last_was_dash = true;
        }
    }

    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "meeting".to_string()
    } else {
        slug.chars().take(80).collect()
    }
}

fn write_analysis_markdown(
    session_path: &Path,
    result: &AgentMeetingResult,
    generated_title: Option<&str>,
) -> io::Result<Vec<PathBuf>> {
    let title = generated_title
        .map(str::to_string)
        .or_else(|| read_session_title(session_path).ok())
        .unwrap_or_else(|| "Meeting Analysis".to_string());
    let mut written = if generated_title.is_some() {
        update_session_title_files(session_path, &title)?
    } else {
        Vec::new()
    };

    let path = session_path.join("meeting.md");
    fs::write(&path, meeting_markdown(session_path, &title, result)?)?;
    written.push(path);

    Ok(written)
}

fn update_session_title_files(session_path: &Path, title: &str) -> io::Result<Vec<PathBuf>> {
    let mut written = Vec::new();

    let metadata_path = metadata_path(session_path);
    if metadata_path.exists() {
        let metadata = fs::read_to_string(&metadata_path)?;
        if let Ok(mut value) = serde_json::from_str::<Value>(&metadata) {
            if let Some(object) = value.as_object_mut() {
                if let Some(id) = session_path.file_name().and_then(|name| name.to_str()) {
                    object.insert("id".to_string(), Value::String(id.to_string()));
                }
                object.insert("title".to_string(), Value::String(title.to_string()));
                fs::write(&metadata_path, serde_json::to_string_pretty(&value)?)?;
                written.push(metadata_path);
            }
        }
    }

    let transcript_path = session_path.join("transcript.md");
    if transcript_path.exists() {
        let transcript = fs::read_to_string(&transcript_path)?;
        if transcript.starts_with("# Transcript: ") {
            let updated = replace_first_line(&transcript, &format!("# Transcript: {title}"));
            fs::write(&transcript_path, updated)?;
            written.push(transcript_path);
        }
    }

    for (path, prefix) in [
        (markers_path(session_path), "# Markers: "),
        (notes_path(session_path), "# Notes: "),
    ] {
        if path.exists() {
            let content = fs::read_to_string(&path)?;
            if content.starts_with(prefix) {
                let updated = replace_first_line(&content, &format!("{prefix}{title}"));
                fs::write(&path, updated)?;
                written.push(path);
            }
        }
    }

    Ok(written)
}

fn replace_first_line(text: &str, replacement: &str) -> String {
    match text.find('\n') {
        Some(index) => format!("{replacement}{}", &text[index..]),
        None => replacement.to_string(),
    }
}

fn normalized_title(value: Option<&str>) -> Option<String> {
    let title = value?
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches(['"', '\''])
        .trim()
        .to_string();

    if title.is_empty() || title.eq_ignore_ascii_case("quick capture") {
        return None;
    }

    Some(title.chars().take(80).collect())
}

fn meeting_markdown(
    session_path: &Path,
    title: &str,
    result: &AgentMeetingResult,
) -> io::Result<String> {
    let notes = read_session_entries(&notes_path(session_path))?;
    let markers = read_session_entries(&markers_path(session_path))?;
    let mut markdown = format!(
        "# {title}\n\n> Analysis generated from the [clean transcript](transcript.md).\n\n## Summary\n\n{}\n\n",
        result
            .summary
            .as_deref()
            .unwrap_or("_No summary returned._")
    );
    markdown.push_str(&decisions_markdown(&result.decisions));
    markdown.push_str(&actions_markdown(&result.action_items));
    markdown.push_str(&questions_markdown(&result.questions));
    markdown.push_str(&followups_markdown(&result.followups));
    markdown.push_str("## Notes and Markers\n\n");
    append_captured_entries(&mut markdown, "Notes", &notes);
    append_captured_entries(&mut markdown, "Markers", &markers);
    markdown.push_str(
        "## Source Material\n\n- [Transcript](transcript.md)\n- [Microphone audio](audio/mic.m4a)\n- [System audio](audio/call.m4a)\n",
    );
    Ok(markdown)
}

fn read_session_entries(path: &Path) -> io::Result<Vec<String>> {
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

fn append_captured_entries(markdown: &mut String, heading: &str, entries: &[String]) {
    markdown.push_str(&format!("### {heading}\n\n"));
    if entries.is_empty() {
        markdown.push_str("_None captured._\n\n");
    } else {
        markdown.push_str(&entries.join("\n"));
        markdown.push_str("\n\n");
    }
}

fn actions_markdown(items: &[ActionItem]) -> String {
    let mut markdown = "## Action Items\n\n".to_string();
    if items.is_empty() {
        markdown.push_str("_No action items identified._\n\n");
        return markdown;
    }

    for item in items {
        markdown.push_str(&format!("- [ ] {}", item.task));
        append_detail(&mut markdown, "Owner", item.owner.as_deref());
        append_detail(&mut markdown, "Due", item.due.as_deref());
        append_detail(&mut markdown, "Timestamp", item.timestamp.as_deref());
        append_detail(&mut markdown, "Evidence", item.evidence.as_deref());
        markdown.push('\n');
    }
    markdown.push('\n');
    markdown
}

fn decisions_markdown(items: &[Decision]) -> String {
    let mut markdown = "## Decisions\n\n".to_string();
    if items.is_empty() {
        markdown.push_str("_No decisions identified._\n\n");
        return markdown;
    }

    for item in items {
        markdown.push_str(&format!("- {}", item.decision));
        append_detail(&mut markdown, "Timestamp", item.timestamp.as_deref());
        append_detail(&mut markdown, "Evidence", item.evidence.as_deref());
        markdown.push('\n');
    }
    markdown.push('\n');
    markdown
}

fn questions_markdown(items: &[Question]) -> String {
    let mut markdown = "## Open Questions\n\n".to_string();
    if items.is_empty() {
        markdown.push_str("_No open questions identified._\n\n");
        return markdown;
    }

    for item in items {
        markdown.push_str(&format!("- {}", item.question));
        append_detail(&mut markdown, "Timestamp", item.timestamp.as_deref());
        append_detail(&mut markdown, "Context", item.context.as_deref());
        markdown.push('\n');
    }
    markdown.push('\n');
    markdown
}

fn followups_markdown(items: &[Followup]) -> String {
    let mut markdown = "## Follow-ups\n\n".to_string();
    if items.is_empty() {
        markdown.push_str("_No follow-ups identified._\n\n");
        return markdown;
    }

    for item in items {
        markdown.push_str(&format!("- {}", item.item));
        append_detail(&mut markdown, "Reason", item.reason.as_deref());
        markdown.push('\n');
    }
    markdown.push('\n');
    markdown
}

fn append_detail(markdown: &mut String, label: &str, value: Option<&str>) {
    let Some(value) = value else {
        return;
    };
    if value.trim().is_empty() || value == "null" {
        return;
    }
    markdown.push_str(&format!(" ({label}: {value})"));
}

#[cfg(test)]
mod tests {
    use super::{
        extract_agent_result_json, known_agents, meeting_markdown, session_timestamp_prefix,
        title_slug, ActionItem, AgentMeetingResult, Decision,
    };
    use std::fs;

    #[test]
    fn extracts_direct_agent_json() {
        let value = extract_agent_result_json(
            r#"{"summary":"Done","decisions":[],"action_items":[],"questions":[],"followups":[]}"#,
        )
        .unwrap();

        assert_eq!(value["summary"], "Done");
    }

    #[test]
    fn extracts_ndjson_agent_json() {
        let value = extract_agent_result_json(
            r#"{"type":"start"}
{"type":"result","content":"{\"summary\":\"Done\",\"decisions\":[],\"action_items\":[],\"questions\":[],\"followups\":[]}"}"#,
        )
        .unwrap();

        assert_eq!(value["summary"], "Done");
    }

    #[test]
    fn extracts_opencode_text_event_json() {
        let value = extract_agent_result_json(
            r#"{"type":"step_start"}
{"type":"text","timestamp":1,"sessionID":"s1","part":{"type":"text","text":"{\"summary\":\"OpenCode summary\",\"title\":\"Call Notes\",\"decisions\":[],\"action_items\":[],\"questions\":[],\"followups\":[]}"}}"#,
        )
        .unwrap();

        assert_eq!(value["summary"], "OpenCode summary");
        assert_eq!(value["title"], "Call Notes");
    }

    #[test]
    fn extracts_pi_message_end_json() {
        let value = extract_agent_result_json(
            r#"{"type":"session","id":"abc"}
{"type":"message_end","message":{"role":"assistant","content":[{"type":"text","text":"{\"summary\":\"Pi summary\",\"decisions\":[],\"action_items\":[],\"questions\":[],\"followups\":[]}"}]}}"#,
        )
        .unwrap();

        assert_eq!(value["summary"], "Pi summary");
    }

    #[test]
    fn lists_builtin_agents() {
        assert!(known_agents().contains(&"grok"));
        assert!(known_agents().contains(&"cline"));
        assert!(known_agents().contains(&"codex"));
        assert!(known_agents().contains(&"claude"));
        assert!(known_agents().contains(&"opencode"));
        assert!(known_agents().contains(&"pi"));
    }

    #[test]
    fn derives_session_prefix_and_title_slug() {
        assert_eq!(
            session_timestamp_prefix("20260526-185332-quick-capture"),
            Some("05-26-2026_6-53pm".to_string())
        );
        assert_eq!(
            session_timestamp_prefix("20260526-185332-et-rain-chat"),
            Some("05-26-2026_6-53pm".to_string())
        );
        assert_eq!(
            session_timestamp_prefix("05-26-2026_7-21pm-et-rain-chat"),
            Some("05-26-2026_7-21pm".to_string())
        );
        assert_eq!(
            title_slug("Rain, Birthdays and Jersey Mike's Chat"),
            "rain-birthdays-and-jersey-mikes-chat"
        );
    }

    #[test]
    fn renders_one_complete_meeting_document() {
        let session_dir = std::env::temp_dir().join(format!(
            "recall-meeting-markdown-test-{}",
            std::process::id()
        ));
        fs::create_dir_all(session_dir.join(".recall")).unwrap();
        fs::write(
            session_dir.join(".recall/notes.md"),
            "# Notes\n\n- `00:10` Check the budget\n",
        )
        .unwrap();
        fs::write(
            session_dir.join(".recall/markers.md"),
            "# Markers\n\n- `00:20` Marker\n",
        )
        .unwrap();
        let result = AgentMeetingResult {
            title: Some("Project Sync".to_string()),
            summary: Some("The team agreed on the launch plan.".to_string()),
            decisions: vec![Decision {
                decision: "Launch Friday".to_string(),
                evidence: None,
                timestamp: Some("00:30".to_string()),
            }],
            action_items: vec![ActionItem {
                task: "Publish the checklist".to_string(),
                owner: Some("Sam".to_string()),
                due: None,
                evidence: None,
                timestamp: None,
            }],
            ..AgentMeetingResult::default()
        };

        let markdown = meeting_markdown(&session_dir, "Project Sync", &result).unwrap();
        assert!(markdown.contains("## Summary"));
        assert!(markdown.contains("## Decisions"));
        assert!(markdown.contains("Launch Friday"));
        assert!(markdown.contains("## Action Items"));
        assert!(markdown.contains("Publish the checklist"));
        assert!(markdown.contains("Check the budget"));
        assert!(markdown.contains("[Transcript](transcript.md)"));

        let _ = fs::remove_dir_all(session_dir);
    }
}
