use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::session::state_dir;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioTrack {
    Mic,
    Call,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContinuedTake {
    pub take_index: u32,
    pub mic_name: String,
    pub call_name: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureProgress {
    pub take_count: u32,
    pub completed_take: u32,
    #[serde(default)]
    pub elapsed_ms: u64,
    #[serde(default)]
    pub transcribed_take: u32,
}

impl CaptureProgress {
    pub fn take_in_progress(self) -> bool {
        self.take_count > self.completed_take
    }
}

impl AudioTrack {
    pub fn label(self) -> &'static str {
        match self {
            Self::Mic => "mic",
            Self::Call => "call",
        }
    }

    pub fn alias_name(self) -> &'static str {
        match self {
            Self::Mic => "mic.m4a",
            Self::Call => "call.m4a",
        }
    }

    pub fn segment_name(self, take: u32) -> String {
        format!("{}-{take:03}.m4a", self.label())
    }

    pub fn part_segment_name(self, take: u32, part: u32) -> String {
        if part <= 1 {
            self.segment_name(take)
        } else {
            format!("{}-{take:03}-part-{part:02}.m4a", self.label())
        }
    }
}

pub const MAX_HELPER_RESTARTS: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct SegmentRef {
    take: u32,
    part: u32,
}

pub fn helper_restart_allowed(
    is_recording: bool,
    stop_requested: bool,
    restarts_used: u32,
) -> bool {
    is_recording && !stop_requested && restarts_used < MAX_HELPER_RESTARTS
}

pub fn next_helper_part_name(track: AudioTrack, take: u32, restarts_used: u32) -> Option<String> {
    if !helper_restart_allowed(true, false, restarts_used) {
        return None;
    }
    Some(track.part_segment_name(take, restarts_used + 2))
}

pub fn audio_dir(session_path: &Path) -> PathBuf {
    session_path.join("audio")
}

pub fn capture_progress_path(session_path: &Path) -> PathBuf {
    state_dir(session_path).join("capture.json")
}

pub fn capture_lock_path(session_path: &Path) -> PathBuf {
    state_dir(session_path).join("capture.lock")
}

pub struct CaptureLock {
    path: PathBuf,
    contents: String,
}

pub fn acquire_capture_lock(session_path: &Path) -> io::Result<CaptureLock> {
    let path = capture_lock_path(session_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    for _ in 0..3 {
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                let contents = new_capture_lock_contents();
                file.write_all(contents.as_bytes())?;
                return Ok(CaptureLock { path, contents });
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if lock_holder_is_dead(&path) && try_remove_stale_lock(&path) {
                    continue;
                }
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    live_capture_lock_message(session_path, &path),
                ));
            }
            Err(error) => return Err(error),
        }
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        live_capture_lock_message(session_path, &path),
    ))
}

impl Drop for CaptureLock {
    fn drop(&mut self) {
        // Only unlink if this file is still the instance we created. A stale
        // cleanup race must not let our Drop delete another process's lock.
        if lock_file_matches(&self.path, &self.contents) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

pub fn resume_block_reason(session_path: &Path) -> Option<String> {
    let lock_path = capture_lock_path(session_path);
    let progress = read_capture_progress(session_path);
    if lock_path.exists() && !lock_holder_is_dead(&lock_path) {
        return Some(live_capture_lock_message(session_path, &lock_path));
    }
    // Leave stale lock files in place. Acquire reclaims them with an identity
    // check so resume inspection cannot unlink a lock another process just created.
    if progress.take_in_progress() {
        return Some(unfinished_take_message(session_path));
    }
    None
}

fn new_capture_lock_contents() -> String {
    let token = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    format!("{}\n{token}\n", std::process::id())
}

fn try_remove_stale_lock(path: &Path) -> bool {
    let Ok(observed) = fs::read_to_string(path) else {
        return true;
    };
    if !lock_contents_holder_is_dead(&observed) {
        return false;
    }
    remove_lock_if_identity_matches(path, &observed)
}

pub(crate) fn remove_lock_if_identity_matches(path: &Path, observed: &str) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let Ok(file) = fs::File::open(path) else {
            return !path.exists();
        };
        let Ok(fd_meta) = file.metadata() else {
            return false;
        };
        let Ok(path_meta) = fs::metadata(path) else {
            return !path.exists();
        };
        if fd_meta.dev() != path_meta.dev() || fd_meta.ino() != path_meta.ino() {
            return false;
        }
    }
    if !lock_file_matches(path, observed) {
        return false;
    }
    fs::remove_file(path).is_ok()
}

fn lock_file_matches(path: &Path, observed: &str) -> bool {
    fs::read_to_string(path).is_ok_and(|current| current == observed)
}

fn live_capture_lock_message(session_path: &Path, lock_path: &Path) -> String {
    let id = session_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("this session");
    match lock_pid(lock_path) {
        Some(pid) => {
            format!("Session {id} is already recording in another Recall process (pid {pid}).")
        }
        None => format!("Session {id} is already recording in another Recall process."),
    }
}

fn unfinished_take_message(session_path: &Path) -> String {
    let id = session_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("this session");
    format!(
        "Session {id} has an unfinished take. Crash resume is not supported; refuse rather than attach a second recorder."
    )
}

fn lock_pid(path: &Path) -> Option<u32> {
    lock_pid_from_contents(&fs::read_to_string(path).ok()?)
}

fn lock_pid_from_contents(contents: &str) -> Option<u32> {
    contents.lines().next()?.trim().parse().ok()
}

fn lock_contents_holder_is_dead(contents: &str) -> bool {
    let Some(pid) = lock_pid_from_contents(contents) else {
        return true;
    };
    !process_is_alive(pid)
}

fn process_is_alive(pid: u32) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

pub fn read_capture_progress(session_path: &Path) -> CaptureProgress {
    let path = capture_progress_path(session_path);
    let Ok(contents) = fs::read_to_string(path) else {
        return CaptureProgress::default();
    };
    serde_json::from_str(&contents).unwrap_or_default()
}

pub fn write_capture_progress(
    session_path: &Path,
    progress: CaptureProgress,
) -> io::Result<CaptureProgress> {
    let _lock = lock_session_publish(session_path)?;
    write_capture_progress_unlocked(session_path, progress)
}

fn write_capture_progress_unlocked(
    session_path: &Path,
    progress: CaptureProgress,
) -> io::Result<CaptureProgress> {
    let path = capture_progress_path(session_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(&path, serde_json::to_string_pretty(&progress)?)?;
    Ok(progress)
}

pub struct SessionPublishLock {
    path: PathBuf,
}

pub fn lock_session_publish(session_path: &Path) -> io::Result<SessionPublishLock> {
    let path = state_dir(session_path).join("publish.lock");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                let _ = writeln!(file, "{}", std::process::id());
                return Ok(SessionPublishLock { path });
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if lock_holder_is_dead(&path) {
                    let _ = fs::remove_file(&path);
                    continue;
                }
                if Instant::now() >= deadline {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        format!("Timed out waiting for {}", path.display()),
                    ));
                }
                thread::sleep(Duration::from_millis(20));
            }
            Err(error) => return Err(error),
        }
    }
}

impl Drop for SessionPublishLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn lock_holder_is_dead(path: &Path) -> bool {
    let Ok(contents) = fs::read_to_string(path) else {
        return true;
    };
    lock_contents_holder_is_dead(&contents)
}

pub fn generation_is_current(session_path: &Path, generation: u32) -> bool {
    read_capture_progress(session_path).completed_take == generation
}

pub fn session_folder_is_sticky(session_path: &Path) -> bool {
    read_capture_progress(session_path).take_count > 1
}

pub fn prepare_continued_take(session_path: &Path) -> io::Result<ContinuedTake> {
    promote_legacy_aliases_to_take1(session_path)?;
    let take_index = next_take_index(session_path);
    Ok(ContinuedTake {
        take_index,
        mic_name: AudioTrack::Mic.segment_name(take_index),
        call_name: AudioTrack::Call.segment_name(take_index),
    })
}

pub fn promote_legacy_aliases_to_take1(session_path: &Path) -> io::Result<()> {
    let audio_dir = audio_dir(session_path);
    if !audio_dir.exists() {
        fs::create_dir_all(&audio_dir)?;
        return Ok(());
    }

    for track in [AudioTrack::Mic, AudioTrack::Call] {
        if !discover_numbered_segments(&audio_dir, track, None).is_empty() {
            continue;
        }
        let alias = audio_dir.join(track.alias_name());
        if !alias.exists() {
            continue;
        }
        let take1 = audio_dir.join(track.segment_name(1));
        fs::copy(&alias, &take1)?;
    }
    Ok(())
}

pub fn next_take_index(session_path: &Path) -> u32 {
    let audio_dir = audio_dir(session_path);
    let max_numbered = [AudioTrack::Mic, AudioTrack::Call]
        .into_iter()
        .flat_map(|track| discover_numbered_segments(&audio_dir, track, None))
        .filter_map(|path| segment_take_index(&path))
        .max()
        .unwrap_or(0);
    let from_progress = read_capture_progress(session_path).take_count;
    let max_numbered = max_numbered.max(from_progress);
    if max_numbered > 0 {
        return max_numbered + 1;
    }
    let has_alias = [AudioTrack::Mic, AudioTrack::Call]
        .into_iter()
        .any(|track| audio_dir.join(track.alias_name()).exists());
    if has_alias {
        2
    } else {
        1
    }
}

pub fn discover_track_segments(
    session_path: &Path,
    track: AudioTrack,
    max_take: Option<u32>,
) -> Vec<PathBuf> {
    let audio_dir = audio_dir(session_path);
    let numbered = discover_numbered_segments(&audio_dir, track, max_take);
    if !numbered.is_empty() {
        return numbered;
    }
    let alias = audio_dir.join(track.alias_name());
    if alias.exists() {
        vec![alias]
    } else {
        Vec::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioRetention {
    pub keep_audio: bool,
    pub is_recording: bool,
    pub is_open_session: bool,
}

pub fn session_has_successful_transcript(session_path: &Path) -> bool {
    let path = session_path.join("transcript.md");
    let Ok(contents) = fs::read_to_string(path) else {
        return false;
    };
    let trimmed = contents.trim();
    !trimmed.is_empty() && !contents.contains("Transcript will appear here after audio capture")
}

pub fn mark_audio_transcribed(session_path: &Path) -> io::Result<CaptureProgress> {
    let mut progress = read_capture_progress(session_path);
    progress.transcribed_take = progress.completed_take.max(progress.take_count);
    // Caller (transcribe) already holds SessionPublishLock; do not reacquire.
    write_capture_progress_unlocked(session_path, progress)
}

fn transcript_covers_latest_take(session_path: &Path) -> bool {
    if !session_has_successful_transcript(session_path) {
        return false;
    }
    let progress = read_capture_progress(session_path);
    if progress.take_in_progress() {
        return false;
    }
    let latest = progress.completed_take.max(progress.take_count);
    if progress.transcribed_take > 0 {
        return progress.transcribed_take >= latest;
    }
    latest <= 1
}

pub fn maybe_discard_session_audio(
    session_path: &Path,
    retention: AudioRetention,
) -> io::Result<bool> {
    if retention.keep_audio || retention.is_recording || retention.is_open_session {
        return Ok(false);
    }
    if !transcript_covers_latest_take(session_path) {
        return Ok(false);
    }
    discard_audio_files(session_path)
}

fn discard_audio_files(session_path: &Path) -> io::Result<bool> {
    let dir = audio_dir(session_path);
    if !dir.is_dir() {
        return Ok(false);
    }
    let mut removed = false;
    for entry in fs::read_dir(&dir)? {
        let path = entry?.path();
        if path.is_file() && path.extension().is_some_and(|ext| ext == "m4a") {
            fs::remove_file(&path)?;
            removed = true;
        }
    }
    Ok(removed)
}

pub fn earlier_takes_are_transcript_only(session_path: &Path) -> bool {
    let progress = read_capture_progress(session_path);
    let completed = progress.completed_take.max(progress.take_count);
    if completed == 0 {
        return false;
    }
    let dir = audio_dir(session_path);
    for take in 1..=completed {
        if take_has_audio(&dir, take) {
            continue;
        }
        if take == 1 {
            let mic_alias = dir.join(AudioTrack::Mic.alias_name());
            let call_alias = dir.join(AudioTrack::Call.alias_name());
            if mic_alias.exists() || call_alias.exists() {
                continue;
            }
        }
        return true;
    }
    false
}

fn take_has_audio(audio_dir: &Path, take: u32) -> bool {
    [AudioTrack::Mic, AudioTrack::Call]
        .into_iter()
        .any(|track| {
            discover_numbered_segments(audio_dir, track, Some(take))
                .iter()
                .any(|path| segment_ref(path).is_some_and(|segment| segment.take == take))
        })
}

pub fn usable_audio_segments(ffmpeg: &Path, segments: &[PathBuf]) -> Vec<PathBuf> {
    segments
        .iter()
        .filter(|path| audio_segment_is_readable(ffmpeg, path))
        .cloned()
        .collect()
}

fn audio_segment_is_readable(ffmpeg: &Path, path: &Path) -> bool {
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    if meta.len() < 64 {
        return false;
    }
    Command::new(ffmpeg)
        .arg("-v")
        .arg("error")
        .arg("-i")
        .arg(path)
        .arg("-t")
        .arg("0.05")
        .arg("-f")
        .arg("null")
        .arg("-")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

pub fn concat_audio_segments(ffmpeg: &Path, segments: &[PathBuf], output: &Path) -> io::Result<()> {
    let segments = usable_audio_segments(ffmpeg, segments);
    if segments.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "No readable audio segments to concatenate",
        ));
    }
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    if segments.len() == 1 {
        fs::copy(&segments[0], output)?;
        return Ok(());
    }

    // Route changes can switch channel count, sample rate, and AAC/ALAC codec.
    // A successful packet-copy concat does not prove those packets decode correctly.
    let work = output.with_extension(format!(
        "concat-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));
    fs::create_dir(&work)?;
    let result = (|| {
        let mut normalized = Vec::new();
        for (index, segment) in segments.iter().enumerate() {
            let path = work.join(format!("{index:05}.wav"));
            let status = Command::new(ffmpeg)
                .args(["-nostdin", "-y", "-v", "error", "-i"])
                .arg(segment)
                .args(["-map", "0:a:0", "-af"])
                .arg(mono_downmix_filter())
                .args(["-ar", "48000", "-c:a", "pcm_f32le"])
                .arg(&path)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()?;
            if !status.success() {
                return Err(io::Error::other(format!(
                    "ffmpeg failed to normalize {}",
                    segment.display()
                )));
            }
            normalized.push(path);
        }
        let list_path = work.join("concat.txt");
        write_ffmpeg_concat_list(&list_path, &normalized)?;
        if !run_ffmpeg_concat(ffmpeg, &list_path, output)? {
            return Err(io::Error::other(format!(
                "ffmpeg failed to concatenate {}",
                output.display()
            )));
        }
        Ok(())
    })();
    let _ = fs::remove_dir_all(work);
    result
}

pub(crate) fn mono_downmix_filter() -> String {
    // Use channel indices, not surround labels: a mic-array channel is not LFE.
    // '<' normalizes over channels actually present, preserving mono gain.
    format!(
        "pan=mono|c0<{}",
        (0..32)
            .map(|i| format!("c{i}"))
            .collect::<Vec<_>>()
            .join("+")
    )
}

pub fn refresh_track_alias(
    session_path: &Path,
    track: AudioTrack,
    source: &Path,
) -> io::Result<PathBuf> {
    let alias = audio_dir(session_path).join(track.alias_name());
    if source == alias {
        return Ok(alias);
    }
    if let Some(parent) = alias.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::copy(source, &alias)?;
    Ok(alias)
}

pub fn copy_take1_aliases(session_path: &Path) -> io::Result<()> {
    let audio_dir = audio_dir(session_path);
    for track in [AudioTrack::Mic, AudioTrack::Call] {
        let take1 = audio_dir.join(track.segment_name(1));
        if take1.exists() {
            refresh_track_alias(session_path, track, &take1)?;
        }
    }
    Ok(())
}

pub fn ffmpeg_concat_list(segments: &[PathBuf]) -> String {
    segments
        .iter()
        .map(|path| {
            let absolute = if path.is_absolute() {
                path.clone()
            } else {
                std::env::current_dir()
                    .map(|cwd| cwd.join(path))
                    .unwrap_or_else(|_| path.clone())
            };
            let escaped = absolute.to_string_lossy().replace('\'', r"'\''");
            format!("file '{escaped}'")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn write_ffmpeg_concat_list(path: &Path, segments: &[PathBuf]) -> io::Result<()> {
    let mut file = fs::File::create(path)?;
    writeln!(file, "{}", ffmpeg_concat_list(segments))?;
    Ok(())
}

fn run_ffmpeg_concat(ffmpeg: &Path, list_path: &Path, output: &Path) -> io::Result<bool> {
    let mut command = Command::new(ffmpeg);
    command
        .arg("-y")
        .arg("-f")
        .arg("concat")
        .arg("-safe")
        .arg("0")
        .arg("-i")
        .arg(list_path);
    command.args(["-c:a", "aac", "-b:a", "192k", "-nostdin"]);
    let status = command
        .arg(output)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    Ok(status.success())
}

fn discover_numbered_segments(
    audio_dir: &Path,
    track: AudioTrack,
    max_take: Option<u32>,
) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(audio_dir) else {
        return Vec::new();
    };
    let mut segments = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            parse_segment_name(path, track)
                .is_some_and(|segment| max_take.is_none_or(|max| segment.take <= max))
        })
        .collect::<Vec<_>>();
    segments.sort_by(
        |left, right| match (segment_ref(left), segment_ref(right)) {
            (Some(left_ref), Some(right_ref)) => left_ref
                .cmp(&right_ref)
                .then_with(|| left.file_name().cmp(&right.file_name())),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => left.file_name().cmp(&right.file_name()),
        },
    );
    segments
}

fn segment_take_index(path: &Path) -> Option<u32> {
    segment_ref(path).map(|segment| segment.take)
}

fn segment_ref(path: &Path) -> Option<SegmentRef> {
    parse_segment_name(path, AudioTrack::Mic).or_else(|| parse_segment_name(path, AudioTrack::Call))
}

fn parse_segment_name(path: &Path, track: AudioTrack) -> Option<SegmentRef> {
    let name = path.file_name()?.to_str()?;
    let (stem, ext) = name.rsplit_once('.')?;
    if !ext.eq_ignore_ascii_case("m4a") {
        return None;
    }
    let rest = stem.strip_prefix(&format!("{}-", track.label()))?;
    if let Some((take_str, part_str)) = rest.split_once("-part-") {
        return Some(SegmentRef {
            take: parse_fixed_digits(take_str, 3)?,
            part: parse_fixed_digits(part_str, 2)?,
        });
    }
    Some(SegmentRef {
        take: parse_fixed_digits(rest, 3)?,
        part: 1,
    })
}

fn parse_fixed_digits(value: &str, width: usize) -> Option<u32> {
    if value.len() != width || !value.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::{
        acquire_capture_lock, capture_lock_path, discover_track_segments, ffmpeg_concat_list,
        helper_restart_allowed, lock_session_publish, mark_audio_transcribed,
        maybe_discard_session_audio, next_helper_part_name, next_take_index,
        prepare_continued_take, promote_legacy_aliases_to_take1, read_capture_progress,
        remove_lock_if_identity_matches, resume_block_reason, session_folder_is_sticky,
        usable_audio_segments, write_capture_progress, AudioRetention, AudioTrack, CaptureProgress,
        MAX_HELPER_RESTARTS,
    };
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    fn unique_session_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "recall-audio-{label}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ))
    }

    #[test]
    fn numbered_segment_names_use_three_digits() {
        assert_eq!(AudioTrack::Mic.segment_name(1), "mic-001.m4a");
        assert_eq!(AudioTrack::Call.segment_name(2), "call-002.m4a");
        assert_eq!(
            AudioTrack::Mic.part_segment_name(1, 2),
            "mic-001-part-02.m4a"
        );
        assert_eq!(
            AudioTrack::Call.part_segment_name(3, 4),
            "call-003-part-04.m4a"
        );
        assert_eq!(AudioTrack::Mic.part_segment_name(1, 1), "mic-001.m4a");
    }

    #[test]
    fn existing_single_take_sessions_still_discover_alias_files() {
        let session = unique_session_dir("legacy");
        let audio = session.join("audio");
        fs::create_dir_all(&audio).unwrap();
        fs::write(audio.join("mic.m4a"), b"mic").unwrap();
        fs::write(audio.join("call.m4a"), b"call").unwrap();

        assert_eq!(
            discover_track_segments(&session, AudioTrack::Mic, None),
            vec![audio.join("mic.m4a")]
        );
        assert_eq!(
            discover_track_segments(&session, AudioTrack::Call, None),
            vec![audio.join("call.m4a")]
        );

        let _ = fs::remove_dir_all(session);
    }

    #[test]
    fn numbered_segments_are_discovered_in_take_order_and_honor_generation() {
        let session = unique_session_dir("segments");
        let audio = session.join("audio");
        fs::create_dir_all(&audio).unwrap();
        fs::write(audio.join("mic-002.m4a"), b"two").unwrap();
        fs::write(audio.join("mic-001.m4a"), b"one").unwrap();
        fs::write(audio.join("mic.m4a"), b"alias").unwrap();

        assert_eq!(
            discover_track_segments(&session, AudioTrack::Mic, None),
            vec![audio.join("mic-001.m4a"), audio.join("mic-002.m4a")]
        );
        assert_eq!(
            discover_track_segments(&session, AudioTrack::Mic, Some(1)),
            vec![audio.join("mic-001.m4a")]
        );

        let _ = fs::remove_dir_all(session);
    }

    #[test]
    fn restart_parts_are_discovered_in_take_then_part_order() {
        let session = unique_session_dir("parts");
        let audio = session.join("audio");
        fs::create_dir_all(&audio).unwrap();
        fs::write(audio.join("mic-002.m4a"), b"take2").unwrap();
        fs::write(audio.join("mic-001-part-03.m4a"), b"p3").unwrap();
        fs::write(audio.join("mic-001.m4a"), b"p1").unwrap();
        fs::write(audio.join("mic-001-part-02.m4a"), b"p2").unwrap();
        fs::write(audio.join("call-001-part-02.m4a"), b"call").unwrap();

        assert_eq!(
            discover_track_segments(&session, AudioTrack::Mic, None),
            vec![
                audio.join("mic-001.m4a"),
                audio.join("mic-001-part-02.m4a"),
                audio.join("mic-001-part-03.m4a"),
                audio.join("mic-002.m4a"),
            ]
        );
        assert_eq!(
            discover_track_segments(&session, AudioTrack::Mic, Some(1)),
            vec![
                audio.join("mic-001.m4a"),
                audio.join("mic-001-part-02.m4a"),
                audio.join("mic-001-part-03.m4a"),
            ]
        );
        assert_eq!(next_take_index(&session), 3);
        let _ = fs::remove_dir_all(session);
    }

    #[test]
    fn usable_audio_segments_skips_tiny_crash_files() {
        let session = unique_session_dir("unreadable-part");
        let audio = session.join("audio");
        fs::create_dir_all(&audio).unwrap();
        let truncated = audio.join("mic-001.m4a");
        let part = audio.join("mic-001-part-02.m4a");
        fs::write(&truncated, b"nope").unwrap();
        fs::write(&part, vec![0u8; 128]).unwrap();
        let usable = usable_audio_segments(Path::new("/usr/bin/false"), &[truncated, part.clone()]);
        assert!(usable.is_empty() || usable == vec![part]);
        let _ = fs::remove_dir_all(session);
    }

    #[test]
    fn helper_restarts_are_capped_and_name_part_files() {
        assert_eq!(MAX_HELPER_RESTARTS, 3);
        assert_eq!(
            next_helper_part_name(AudioTrack::Mic, 1, 0).as_deref(),
            Some("mic-001-part-02.m4a")
        );
        assert_eq!(
            next_helper_part_name(AudioTrack::Call, 2, 1).as_deref(),
            Some("call-002-part-03.m4a")
        );
        assert_eq!(
            next_helper_part_name(AudioTrack::Mic, 1, 2).as_deref(),
            Some("mic-001-part-04.m4a")
        );
        assert_eq!(next_helper_part_name(AudioTrack::Mic, 1, 3), None);
        assert!(helper_restart_allowed(true, false, 0));
        assert!(helper_restart_allowed(true, false, 2));
        assert!(!helper_restart_allowed(false, false, 0));
        assert!(!helper_restart_allowed(true, true, 0));
        assert!(!helper_restart_allowed(true, false, 3));
    }

    #[test]
    fn continue_prep_reuses_the_same_session_and_names_take_two() {
        let storage = unique_session_dir("continue-storage");
        let session = storage.join("meeting");
        let audio = session.join("audio");
        fs::create_dir_all(&audio).unwrap();
        fs::write(audio.join("mic-001.m4a"), b"mic").unwrap();
        fs::write(audio.join("call-001.m4a"), b"call").unwrap();
        write_capture_progress(
            &session,
            CaptureProgress {
                take_count: 1,
                completed_take: 1,
                elapsed_ms: 0,
                transcribed_take: 0,
            },
        )
        .unwrap();

        let continued = prepare_continued_take(&session).unwrap();
        assert_eq!(continued.take_index, 2);
        assert_eq!(continued.mic_name, "mic-002.m4a");
        assert_eq!(continued.call_name, "call-002.m4a");
        assert_eq!(next_take_index(&session), 2);

        let sibling_count = fs::read_dir(&storage)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.path().is_dir())
            .count();
        assert_eq!(sibling_count, 1);

        let _ = fs::remove_dir_all(storage);
    }

    #[test]
    fn continue_promotes_legacy_aliases_to_take_one_without_dropping_them() {
        let session = unique_session_dir("promote");
        let audio = session.join("audio");
        fs::create_dir_all(&audio).unwrap();
        fs::write(audio.join("mic.m4a"), b"mic").unwrap();
        fs::write(audio.join("call.m4a"), b"call").unwrap();

        promote_legacy_aliases_to_take1(&session).unwrap();
        assert!(audio.join("mic-001.m4a").exists());
        assert!(audio.join("call-001.m4a").exists());
        assert!(audio.join("mic.m4a").exists());
        assert_eq!(next_take_index(&session), 2);

        let _ = fs::remove_dir_all(session);
    }

    #[test]
    fn concat_list_keeps_timeline_order_and_quotes_paths() {
        let list = ffmpeg_concat_list(&[
            PathBuf::from("/tmp/recall/mic-001.m4a"),
            PathBuf::from("/tmp/recall/mic-002.m4a"),
        ]);
        assert!(list.contains("file '/tmp/recall/mic-001.m4a'"));
        assert!(list.contains("file '/tmp/recall/mic-002.m4a'"));
        let mic_one = list.find("mic-001").unwrap();
        let mic_two = list.find("mic-002").unwrap();
        assert!(mic_one < mic_two);

        let parts = ffmpeg_concat_list(&[
            PathBuf::from("/tmp/recall/mic-001.m4a"),
            PathBuf::from("/tmp/recall/mic-001-part-02.m4a"),
            PathBuf::from("/tmp/recall/mic-002.m4a"),
        ]);
        let first = parts.find("mic-001.m4a").unwrap();
        let part = parts.find("mic-001-part-02.m4a").unwrap();
        let second = parts.find("mic-002.m4a").unwrap();
        assert!(first < part && part < second);
    }

    #[test]
    fn sticky_folder_follows_continued_take_count() {
        let session = unique_session_dir("sticky");
        fs::create_dir_all(&session).unwrap();
        assert!(!session_folder_is_sticky(&session));
        write_capture_progress(
            &session,
            CaptureProgress {
                take_count: 2,
                completed_take: 1,
                elapsed_ms: 0,
                transcribed_take: 0,
            },
        )
        .unwrap();
        assert!(session_folder_is_sticky(&session));
        assert_eq!(
            read_capture_progress(&session),
            CaptureProgress {
                take_count: 2,
                completed_take: 1,
                elapsed_ms: 0,
                transcribed_take: 0,
            }
        );
        let _ = fs::remove_dir_all(session);
    }

    #[test]
    fn concat_keeps_take_order_when_ffmpeg_is_available() {
        let Some(ffmpeg) = find_ffmpeg() else {
            return;
        };
        let session = unique_session_dir("concat");
        let audio = session.join("audio");
        fs::create_dir_all(&audio).unwrap();
        let first = audio.join("mic-001.m4a");
        let second = audio.join("mic-002.m4a");
        write_silent_m4a(&ffmpeg, &first, 1);
        write_silent_m4a(&ffmpeg, &second, 1);
        let output = session.join("mic-concat.m4a");
        super::concat_audio_segments(&ffmpeg, &[first, second], &output).unwrap();
        assert!(output.exists());
        assert!(fs::metadata(&output).unwrap().len() > 0);
        let _ = fs::remove_dir_all(session);
    }

    #[test]
    fn concat_preserves_speech_on_third_channel_after_aac_to_alac_switch() {
        let Some(ffmpeg) = find_ffmpeg() else { return };
        let session = unique_session_dir("concat-format-switch");
        fs::create_dir_all(&session).unwrap();
        let first = session.join("mic-001.m4a");
        let second = session.join("mic-001-part-02.m4a");
        for (path, source, codec) in [
            (
                &first,
                "sine=frequency=220:sample_rate=44100:duration=1",
                "aac",
            ),
            (
                &second,
                "aevalsrc=0|0|0.3*sin(2*PI*880*t):s=48000:d=1:c=3.0",
                "alac",
            ),
        ] {
            let status = std::process::Command::new(&ffmpeg)
                .args([
                    "-nostdin", "-v", "error", "-y", "-f", "lavfi", "-i", source, "-c:a", codec,
                ])
                .arg(path)
                .status()
                .unwrap();
            assert!(status.success());
        }
        let joined = session.join("joined.m4a");
        super::concat_audio_segments(&ffmpeg, &[first, second], &joined).unwrap();
        let decoded = std::process::Command::new(&ffmpeg)
            .args(["-v", "error", "-i"])
            .arg(&joined)
            .args(["-ac", "1", "-ar", "16000", "-f", "f32le", "-"])
            .output()
            .unwrap();
        assert!(decoded.status.success());
        let samples: Vec<f32> = decoded
            .stdout
            .chunks_exact(4)
            .map(|bytes| f32::from_le_bytes(bytes.try_into().unwrap()))
            .collect();
        assert!(
            (31000..35000).contains(&samples.len()),
            "joined frames: {}",
            samples.len()
        );
        let energy: f64 = samples[24000..30000]
            .iter()
            .map(|s| f64::from(*s).powi(2))
            .sum();
        assert!(
            (energy / 6000.0).sqrt() > 0.02,
            "third-channel speech was lost"
        );
        assert!(!fs::read_dir(&session)
            .unwrap()
            .filter_map(Result::ok)
            .any(|entry| entry.path().is_dir()));
        fs::remove_dir_all(session).unwrap();
    }

    fn find_ffmpeg() -> Option<PathBuf> {
        [
            PathBuf::from("ffmpeg"),
            PathBuf::from("/opt/homebrew/bin/ffmpeg"),
            PathBuf::from("/usr/local/bin/ffmpeg"),
        ]
        .into_iter()
        .find(|path| {
            path.exists()
                || (path.as_os_str() == "ffmpeg"
                    && std::process::Command::new("ffmpeg")
                        .arg("-version")
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .status()
                        .map(|status| status.success())
                        .unwrap_or(false))
        })
        .map(|path| {
            if path.as_os_str() == "ffmpeg" {
                PathBuf::from("ffmpeg")
            } else {
                path
            }
        })
    }

    fn write_silent_m4a(ffmpeg: &PathBuf, path: &PathBuf, seconds: u32) {
        let status = std::process::Command::new(ffmpeg)
            .arg("-y")
            .arg("-f")
            .arg("lavfi")
            .arg("-i")
            .arg("anullsrc=r=44100:cl=mono")
            .arg("-t")
            .arg(seconds.to_string())
            .arg("-c:a")
            .arg("aac")
            .arg(path)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(
            status.success(),
            "ffmpeg failed to write {}",
            path.display()
        );
    }

    #[test]
    fn elapsed_ms_round_trips_through_capture_json() {
        let session = unique_session_dir("elapsed");
        fs::create_dir_all(&session).unwrap();
        write_capture_progress(
            &session,
            CaptureProgress {
                take_count: 1,
                completed_take: 1,
                elapsed_ms: 724_000,
                transcribed_take: 0,
            },
        )
        .unwrap();
        assert_eq!(
            read_capture_progress(&session),
            CaptureProgress {
                take_count: 1,
                completed_take: 1,
                elapsed_ms: 724_000,
                transcribed_take: 0,
            }
        );
        let _ = fs::remove_dir_all(session);
    }

    #[test]
    fn missing_elapsed_ms_deserializes_as_zero() {
        let session = unique_session_dir("legacy-elapsed");
        let state = session.join(".recall/state");
        fs::create_dir_all(&state).unwrap();
        fs::write(
            state.join("capture.json"),
            r#"{"take_count":1,"completed_take":1}"#,
        )
        .unwrap();
        assert_eq!(
            read_capture_progress(&session),
            CaptureProgress {
                take_count: 1,
                completed_take: 1,
                elapsed_ms: 0,
                transcribed_take: 0,
            }
        );
        let _ = fs::remove_dir_all(session);
    }

    #[test]
    fn resume_refuses_a_live_capture_lock() {
        let session = unique_session_dir("live-lock");
        fs::create_dir_all(&session).unwrap();
        write_capture_progress(
            &session,
            CaptureProgress {
                take_count: 1,
                completed_take: 0,
                elapsed_ms: 0,
                transcribed_take: 0,
            },
        )
        .unwrap();
        fs::write(
            capture_lock_path(&session),
            format!("{}", std::process::id()),
        )
        .unwrap();

        let reason = resume_block_reason(&session).expect("live lock should block resume");
        assert!(reason.contains("already recording"));
        assert!(capture_lock_path(&session).exists());

        let _ = fs::remove_dir_all(session);
    }

    #[test]
    fn resume_refuses_an_unfinished_take_after_a_stale_lock() {
        let session = unique_session_dir("stale-lock");
        fs::create_dir_all(&session).unwrap();
        write_capture_progress(
            &session,
            CaptureProgress {
                take_count: 1,
                completed_take: 0,
                elapsed_ms: 12_000,
                transcribed_take: 0,
            },
        )
        .unwrap();
        fs::write(capture_lock_path(&session), "not-a-pid").unwrap();

        let reason = resume_block_reason(&session).expect("unfinished take should block resume");
        assert!(reason.contains("unfinished take"));
        assert!(
            capture_lock_path(&session).exists(),
            "resume inspection must not unlink a lock file"
        );

        let _ = fs::remove_dir_all(session);
    }

    #[test]
    fn resume_allows_a_completed_session_even_with_a_stale_lock() {
        let session = unique_session_dir("completed-lock");
        fs::create_dir_all(&session).unwrap();
        write_capture_progress(
            &session,
            CaptureProgress {
                take_count: 1,
                completed_take: 1,
                elapsed_ms: 5_000,
                transcribed_take: 0,
            },
        )
        .unwrap();
        fs::write(capture_lock_path(&session), "not-a-pid").unwrap();

        assert!(resume_block_reason(&session).is_none());
        assert!(capture_lock_path(&session).exists());

        let _ = fs::remove_dir_all(session);
    }

    #[test]
    fn acquire_replaces_a_stale_lock() {
        let session = unique_session_dir("acquire-stale");
        let path = capture_lock_path(&session);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "not-a-pid").unwrap();

        let lock = acquire_capture_lock(&session).expect("stale lock should be reclaimable");
        let contents = fs::read_to_string(capture_lock_path(&session)).unwrap();
        assert!(contents.starts_with(&format!("{}\n", std::process::id())));
        drop(lock);
        assert!(!capture_lock_path(&session).exists());

        let _ = fs::remove_dir_all(session);
    }

    #[test]
    fn stale_lock_cleanup_does_not_unlink_a_replaced_live_lock() {
        let session = unique_session_dir("identity-lock");
        fs::create_dir_all(&session).unwrap();
        let path = capture_lock_path(&session);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, "not-a-pid").unwrap();
        let observed = fs::read_to_string(&path).unwrap();
        fs::write(&path, format!("{}\n", std::process::id())).unwrap();

        assert!(!remove_lock_if_identity_matches(&path, &observed));
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            format!("{}\n", std::process::id())
        );

        let _ = fs::remove_dir_all(session);
    }

    fn session_with_audio_and_transcript(label: &str, successful: bool) -> PathBuf {
        let session = unique_session_dir(label);
        let audio = session.join("audio");
        fs::create_dir_all(&audio).unwrap();
        fs::write(audio.join("mic-001.m4a"), b"mic").unwrap();
        fs::write(audio.join("call-001.m4a"), b"call").unwrap();
        fs::write(session.join("notes.md"), "- `00:01` keep me\n").unwrap();
        if successful {
            fs::write(
                session.join("transcript.md"),
                "# Transcript\n\nGenerated by local Whisper transcription.\n",
            )
            .unwrap();
            write_capture_progress(
                &session,
                CaptureProgress {
                    take_count: 1,
                    completed_take: 1,
                    elapsed_ms: 0,
                    transcribed_take: 1,
                },
            )
            .unwrap();
        } else {
            fs::write(
                session.join("transcript.md"),
                "# Transcript: Pending\n\nTranscript will appear here after audio capture and transcription are wired in.\n",
            )
            .unwrap();
        }
        session
    }

    #[test]
    fn keep_audio_false_discards_m4as_after_leaving_session() {
        let session = session_with_audio_and_transcript("discard-leave", true);
        let discarded = maybe_discard_session_audio(
            &session,
            AudioRetention {
                keep_audio: false,
                is_recording: false,
                is_open_session: false,
            },
        )
        .unwrap();
        assert!(discarded);
        assert!(!session.join("audio/mic-001.m4a").exists());
        assert!(!session.join("audio/call-001.m4a").exists());
        assert!(session.join("audio").is_dir());
        assert!(session.join("transcript.md").exists());
        assert!(session.join("notes.md").exists());
        let _ = fs::remove_dir_all(session);
    }

    #[test]
    fn keep_audio_false_keeps_audio_while_session_is_still_open() {
        let session = session_with_audio_and_transcript("discard-open", true);
        let discarded = maybe_discard_session_audio(
            &session,
            AudioRetention {
                keep_audio: false,
                is_recording: false,
                is_open_session: true,
            },
        )
        .unwrap();
        assert!(!discarded);
        assert!(session.join("audio/mic-001.m4a").exists());
        let _ = fs::remove_dir_all(session);
    }

    #[test]
    fn keep_audio_true_never_discards() {
        let session = session_with_audio_and_transcript("keep-true", true);
        let discarded = maybe_discard_session_audio(
            &session,
            AudioRetention {
                keep_audio: true,
                is_recording: false,
                is_open_session: false,
            },
        )
        .unwrap();
        assert!(!discarded);
        assert!(session.join("audio/mic-001.m4a").exists());
        let _ = fs::remove_dir_all(session);
    }

    #[test]
    fn missing_or_failed_transcript_never_discards_audio() {
        let missing = unique_session_dir("discard-missing");
        let audio = missing.join("audio");
        fs::create_dir_all(&audio).unwrap();
        fs::write(audio.join("mic-001.m4a"), b"mic").unwrap();
        assert!(!maybe_discard_session_audio(
            &missing,
            AudioRetention {
                keep_audio: false,
                is_recording: false,
                is_open_session: false,
            },
        )
        .unwrap());
        assert!(audio.join("mic-001.m4a").exists());

        let placeholder = session_with_audio_and_transcript("discard-placeholder", false);
        assert!(!maybe_discard_session_audio(
            &placeholder,
            AudioRetention {
                keep_audio: false,
                is_recording: false,
                is_open_session: false,
            },
        )
        .unwrap());
        assert!(placeholder.join("audio/mic-001.m4a").exists());

        let _ = fs::remove_dir_all(missing);
        let _ = fs::remove_dir_all(placeholder);
    }

    #[test]
    fn stale_take_one_transcript_does_not_discard_newer_untranscribed_audio() {
        let session = session_with_audio_and_transcript("discard-stale-take", true);
        let audio = session.join("audio");
        fs::write(audio.join("mic-002.m4a"), b"mic2").unwrap();
        fs::write(audio.join("call-002.m4a"), b"call2").unwrap();
        write_capture_progress(
            &session,
            CaptureProgress {
                take_count: 2,
                completed_take: 2,
                elapsed_ms: 0,
                transcribed_take: 1,
            },
        )
        .unwrap();
        assert!(!maybe_discard_session_audio(
            &session,
            AudioRetention {
                keep_audio: false,
                is_recording: false,
                is_open_session: false,
            },
        )
        .unwrap());
        assert!(audio.join("mic-002.m4a").exists());

        write_capture_progress(
            &session,
            CaptureProgress {
                take_count: 2,
                completed_take: 2,
                elapsed_ms: 0,
                transcribed_take: 2,
            },
        )
        .unwrap();
        assert!(maybe_discard_session_audio(
            &session,
            AudioRetention {
                keep_audio: false,
                is_recording: false,
                is_open_session: false,
            },
        )
        .unwrap());
        assert!(!audio.join("mic-002.m4a").exists());

        let _ = fs::remove_dir_all(session);
    }

    #[test]
    fn mark_audio_transcribed_does_not_reacquire_the_publish_lock() {
        let session = unique_session_dir("transcribed-lock");
        fs::create_dir_all(&session).unwrap();
        write_capture_progress(
            &session,
            CaptureProgress {
                take_count: 1,
                completed_take: 1,
                elapsed_ms: 0,
                transcribed_take: 0,
            },
        )
        .unwrap();
        let _lock = lock_session_publish(&session).unwrap();
        let started = Instant::now();
        let progress = mark_audio_transcribed(&session).unwrap();
        assert!(started.elapsed() < Duration::from_secs(1));
        assert_eq!(progress.transcribed_take, 1);
        assert_eq!(read_capture_progress(&session).transcribed_take, 1);
        let _ = fs::remove_dir_all(session);
    }

    #[test]
    fn discard_audio_only_removes_m4a_files() {
        let session = session_with_audio_and_transcript("discard-m4a-only", true);
        let audio = session.join("audio");
        fs::write(audio.join("sidecar.txt"), b"keep").unwrap();
        assert!(maybe_discard_session_audio(
            &session,
            AudioRetention {
                keep_audio: false,
                is_recording: false,
                is_open_session: false,
            },
        )
        .unwrap());
        assert!(!audio.join("mic-001.m4a").exists());
        assert!(audio.join("sidecar.txt").exists());
        let _ = fs::remove_dir_all(session);
    }

    #[test]
    fn discarded_audio_still_counts_the_next_take_from_capture_progress() {
        let session = unique_session_dir("discard-next-take");
        let audio = session.join("audio");
        fs::create_dir_all(&audio).unwrap();
        write_capture_progress(
            &session,
            CaptureProgress {
                take_count: 1,
                completed_take: 1,
                elapsed_ms: 1_000,
                transcribed_take: 0,
            },
        )
        .unwrap();
        assert_eq!(next_take_index(&session), 2);
        let _ = fs::remove_dir_all(session);
    }
}
