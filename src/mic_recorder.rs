use std::fs;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::session::state_dir;

#[derive(Debug, Clone, Deserialize)]
pub struct MicEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    pub path: Option<String>,
    #[serde(rename = "elapsedSeconds")]
    pub elapsed_seconds: Option<f64>,
    #[serde(rename = "levelDb")]
    pub level_db: Option<f32>,
    pub message: Option<String>,
    #[serde(rename = "deviceName")]
    pub device_name: Option<String>,
    #[serde(rename = "deviceID")]
    pub device_id: Option<String>,
}

pub struct MicRecorder {
    child: Child,
    stop_file: PathBuf,
    mute_file: PathBuf,
    receiver: Receiver<MicEvent>,
}

pub fn mute_mic_path(session_dir: &Path) -> PathBuf {
    state_dir(session_dir).join("mute-mic")
}

pub fn stop_mic_path(session_dir: &Path) -> PathBuf {
    state_dir(session_dir).join("stop-mic")
}

pub fn set_mute_mic(session_dir: &Path, muted: bool) -> io::Result<()> {
    let path = mute_mic_path(session_dir);
    if muted {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, b"mute")
    } else {
        clear_mute_mic(session_dir);
        Ok(())
    }
}

pub fn clear_mute_mic(session_dir: &Path) {
    let _ = fs::remove_file(mute_mic_path(session_dir));
}

fn prepare_control_files(session_dir: &Path) -> io::Result<(PathBuf, PathBuf)> {
    fs::create_dir_all(state_dir(session_dir))?;
    let stop_file = stop_mic_path(session_dir);
    let mute_file = mute_mic_path(session_dir);
    if stop_file.exists() {
        fs::remove_file(&stop_file)?;
    }
    if mute_file.exists() {
        fs::remove_file(&mute_file)?;
    }
    Ok((stop_file, mute_file))
}

impl MicRecorder {
    pub fn start(session_dir: &Path, output_name: &str) -> io::Result<Self> {
        let (stop_file, mute_file) = prepare_control_files(session_dir)?;

        let mut child = spawn_helper(session_dir, &stop_file, &mute_file, output_name)?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("failed to capture recall-capture stdout"))?;
        let (sender, receiver) = mpsc::channel();

        thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                if let Ok(event) = serde_json::from_str::<MicEvent>(&line) {
                    let _ = sender.send(event);
                }
            }
        });

        Ok(Self {
            child,
            stop_file,
            mute_file,
            receiver,
        })
    }

    pub fn drain_events(&mut self) -> Vec<MicEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.receiver.try_recv() {
            events.push(event);
        }
        events
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    pub fn set_muted(&self, muted: bool) -> io::Result<()> {
        if muted {
            fs::write(&self.mute_file, b"mute")
        } else if self.mute_file.exists() {
            fs::remove_file(&self.mute_file)
        } else {
            Ok(())
        }
    }

    pub fn stop(&mut self) -> io::Result<()> {
        let result = self.stop_child();
        self.clear_control_files();
        result
    }

    fn stop_child(&mut self) -> io::Result<()> {
        if self.child.try_wait()?.is_some() {
            return Ok(());
        }

        fs::write(&self.stop_file, b"stop")?;
        let deadline = Instant::now() + Duration::from_secs(2);

        while Instant::now() < deadline {
            if self.child.try_wait()?.is_some() {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(50));
        }

        self.child.kill()?;
        let _ = self.child.wait();
        Ok(())
    }

    fn clear_control_files(&self) {
        let _ = fs::remove_file(&self.stop_file);
        let _ = fs::remove_file(&self.mute_file);
    }
}

impl Drop for MicRecorder {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn spawn_helper(
    session_dir: &Path,
    stop_file: &Path,
    mute_file: &Path,
    output_name: &str,
) -> io::Result<Child> {
    let helper_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("capture-helper");

    let mut command = if let Some(binary) = helper_binary(&helper_dir) {
        let mut command = Command::new(binary);
        command.arg("record-mic");
        command
    } else {
        let mut command = Command::new("swift");
        command
            .arg("run")
            .arg("recall-capture")
            .arg("record-mic")
            .current_dir(&helper_dir);
        command
    };

    command
        .arg("--session-dir")
        .arg(session_dir)
        .arg("--stop-file")
        .arg(stop_file)
        .arg("--mute-file")
        .arg(mute_file)
        .arg("--output-name")
        .arg(output_name)
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    command.spawn()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_session(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "recall-mic-mute-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0)
        ))
    }

    #[test]
    fn leftover_mute_file_is_deleted_before_a_new_take() {
        let session = unique_session("leftover");
        fs::create_dir_all(state_dir(&session)).unwrap();
        let mute_path = mute_mic_path(&session);
        let stop_path = stop_mic_path(&session);
        fs::write(&mute_path, b"mute").unwrap();
        fs::write(&stop_path, b"stop").unwrap();
        assert!(mute_path.exists());
        assert!(stop_path.exists());

        let (stop_file, mute_file) = prepare_control_files(&session).unwrap();
        assert_eq!(stop_file, stop_path);
        assert_eq!(mute_file, mute_path);
        assert!(!mute_path.exists());
        assert!(!stop_path.exists());

        let _ = fs::remove_dir_all(session);
    }

    #[test]
    fn set_mute_mic_creates_and_clears_the_control_file() {
        let session = unique_session("toggle");
        let mute_path = mute_mic_path(&session);
        assert!(!mute_path.exists());

        set_mute_mic(&session, true).unwrap();
        assert_eq!(fs::read_to_string(&mute_path).unwrap(), "mute");

        set_mute_mic(&session, false).unwrap();
        assert!(!mute_path.exists());

        clear_mute_mic(&session);
        assert!(!mute_path.exists());

        let _ = fs::remove_dir_all(session);
    }
}
