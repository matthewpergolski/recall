use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

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
}

pub fn audio_dir(session_path: &Path) -> PathBuf {
    session_path.join("audio")
}

pub fn capture_progress_path(session_path: &Path) -> PathBuf {
    state_dir(session_path).join("capture.json")
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
    let Ok(pid) = contents.trim().parse::<u32>() else {
        return true;
    };
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| !status.success())
        .unwrap_or(true)
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

pub fn concat_audio_segments(ffmpeg: &Path, segments: &[PathBuf], output: &Path) -> io::Result<()> {
    if segments.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "No audio segments to concatenate",
        ));
    }
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    if segments.len() == 1 {
        fs::copy(&segments[0], output)?;
        return Ok(());
    }

    let list_path = output.with_extension("concat.txt");
    write_ffmpeg_concat_list(&list_path, segments)?;
    let copy_ok = run_ffmpeg_concat(ffmpeg, &list_path, output, true)?;
    if !copy_ok {
        let reencode_ok = run_ffmpeg_concat(ffmpeg, &list_path, output, false)?;
        if !reencode_ok {
            let _ = fs::remove_file(&list_path);
            return Err(io::Error::other(format!(
                "ffmpeg failed to concatenate {}",
                output.display()
            )));
        }
    }
    let _ = fs::remove_file(&list_path);
    Ok(())
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

fn run_ffmpeg_concat(
    ffmpeg: &Path,
    list_path: &Path,
    output: &Path,
    copy_codec: bool,
) -> io::Result<bool> {
    let mut command = Command::new(ffmpeg);
    command
        .arg("-y")
        .arg("-f")
        .arg("concat")
        .arg("-safe")
        .arg("0")
        .arg("-i")
        .arg(list_path);
    if copy_codec {
        command.arg("-c").arg("copy");
    } else {
        command.arg("-c:a").arg("aac").arg("-b:a").arg("192k");
    }
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
            segment_take_index(path)
                .filter(|take| {
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.starts_with(&format!("{}-", track.label())))
                        && max_take.is_none_or(|max| *take <= max)
                })
                .is_some()
        })
        .collect::<Vec<_>>();
    segments.sort_by_key(|path| segment_take_index(path).unwrap_or(0));
    segments
}

fn segment_take_index(path: &Path) -> Option<u32> {
    let name = path.file_name()?.to_str()?;
    let (stem, ext) = name.rsplit_once('.')?;
    if !ext.eq_ignore_ascii_case("m4a") {
        return None;
    }
    let (_label, index) = stem.rsplit_once('-')?;
    if index.len() != 3 || !index.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    index.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::{
        discover_track_segments, ffmpeg_concat_list, next_take_index, prepare_continued_take,
        promote_legacy_aliases_to_take1, read_capture_progress, session_folder_is_sticky,
        write_capture_progress, AudioTrack, CaptureProgress,
    };
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

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
            },
        )
        .unwrap();
        assert!(session_folder_is_sticky(&session));
        assert_eq!(
            read_capture_progress(&session),
            CaptureProgress {
                take_count: 2,
                completed_take: 1,
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
}
