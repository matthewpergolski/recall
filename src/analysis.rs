use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Deserialize;
use serde_json::Value;

use crate::session::{default_storage_dir, list_sessions};

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

    let debug_dir = session_path.join("analysis-debug");
    if debug_dir.exists() {
        fs::remove_dir_all(&debug_dir)?;
    }
    fs::create_dir_all(&debug_dir)?;

    let prompt = analysis_prompt(&transcript_path, &options.preset);
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
                "Unknown agent '{}'. Use grok, cline, codex, or claude.",
                options.agent
            ),
        )
    })?;

    let output = run_agent(&profile, &prompt)?;
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

    let prompt_path = session_path.join("analysis-debug").join("prompt.md");
    let raw_output_path = session_path
        .join("analysis-debug")
        .join(profile.raw_output_file);
    let result_path = session_path
        .join("analysis-debug")
        .join("agent-result.json");
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
    vec!["grok", "cline", "codex", "claude"]
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
        _ => None,
    }
}

fn run_agent(profile: &AgentProfile, prompt: &str) -> io::Result<String> {
    let mut command = Command::new(profile.command);
    for arg in profile.args {
        if *arg == "{prompt}" {
            command.arg(prompt);
        } else {
            command.arg(arg);
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

fn analysis_prompt(transcript_path: &Path, preset: &str) -> String {
    format!(
        r#"You are analyzing a Recall meeting transcript.

Read this clean transcript file from disk:
{transcript}

Preset: {preset}

Use only the clean transcript as the source of truth. Do not use transcription-debug files unless explicitly asked.

Return exactly one JSON object and no prose outside JSON. Use this schema:

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
        transcript = transcript_path.display(),
        preset = preset
    )
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

    extract_json_object(output).and_then(|json_text| serde_json::from_str::<Value>(json_text).ok())
}

fn find_meeting_result(value: &Value) -> Option<Value> {
    if looks_like_meeting_result(value) {
        return Some(value.clone());
    }

    for key in ["result", "content", "message", "text", "output"] {
        if let Some(candidate) = value.get(key) {
            if looks_like_meeting_result(candidate) {
                return Some(candidate.clone());
            }
            if let Some(text) = candidate.as_str() {
                if let Some(value) = extract_json_object(text)
                    .and_then(|json_text| serde_json::from_str::<Value>(json_text).ok())
                {
                    if looks_like_meeting_result(&value) {
                        return Some(value);
                    }
                }
            }
        }
    }

    None
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

    let files = [
        (
            "summary.md",
            format!(
                "# {title}\n\n## Summary\n\n{}\n",
                result
                    .summary
                    .as_deref()
                    .unwrap_or("_No summary returned._")
            ),
        ),
        ("actions.md", actions_markdown(&title, &result.action_items)),
        (
            "decisions.md",
            decisions_markdown(&title, &result.decisions),
        ),
        (
            "questions.md",
            questions_markdown(&title, &result.questions),
        ),
        (
            "followups.md",
            followups_markdown(&title, &result.followups),
        ),
    ];

    for (file_name, content) in files {
        let path = session_path.join(file_name);
        fs::write(&path, content)?;
        written.push(path);
    }

    Ok(written)
}

fn read_session_title(session_path: &Path) -> io::Result<String> {
    let metadata_path = session_path.join("recall.json");
    let metadata = fs::read_to_string(metadata_path)?;
    let value = serde_json::from_str::<Value>(&metadata)?;
    let title = value
        .get("title")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .unwrap_or("Meeting Analysis");
    Ok(title.to_string())
}

fn update_session_title_files(session_path: &Path, title: &str) -> io::Result<Vec<PathBuf>> {
    let mut written = Vec::new();

    let metadata_path = session_path.join("recall.json");
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

fn actions_markdown(title: &str, items: &[ActionItem]) -> String {
    let mut markdown = format!("# Action Items: {title}\n\n");
    if items.is_empty() {
        markdown.push_str("_No action items returned._\n");
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
    markdown
}

fn decisions_markdown(title: &str, items: &[Decision]) -> String {
    let mut markdown = format!("# Decisions: {title}\n\n");
    if items.is_empty() {
        markdown.push_str("_No decisions returned._\n");
        return markdown;
    }

    for item in items {
        markdown.push_str(&format!("- {}", item.decision));
        append_detail(&mut markdown, "Timestamp", item.timestamp.as_deref());
        append_detail(&mut markdown, "Evidence", item.evidence.as_deref());
        markdown.push('\n');
    }
    markdown
}

fn questions_markdown(title: &str, items: &[Question]) -> String {
    let mut markdown = format!("# Questions: {title}\n\n");
    if items.is_empty() {
        markdown.push_str("_No open questions returned._\n");
        return markdown;
    }

    for item in items {
        markdown.push_str(&format!("- {}", item.question));
        append_detail(&mut markdown, "Timestamp", item.timestamp.as_deref());
        append_detail(&mut markdown, "Context", item.context.as_deref());
        markdown.push('\n');
    }
    markdown
}

fn followups_markdown(title: &str, items: &[Followup]) -> String {
    let mut markdown = format!("# Follow-ups: {title}\n\n");
    if items.is_empty() {
        markdown.push_str("_No follow-ups returned._\n");
        return markdown;
    }

    for item in items {
        markdown.push_str(&format!("- {}", item.item));
        append_detail(&mut markdown, "Reason", item.reason.as_deref());
        markdown.push('\n');
    }
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
    use super::{extract_agent_result_json, known_agents, session_timestamp_prefix, title_slug};

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
    fn lists_builtin_agents() {
        assert!(known_agents().contains(&"grok"));
        assert!(known_agents().contains(&"cline"));
        assert!(known_agents().contains(&"codex"));
        assert!(known_agents().contains(&"claude"));
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
}
