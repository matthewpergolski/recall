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
    receiver: Receiver<MicEvent>,
}

impl MicRecorder {
    pub fn start(session_dir: &Path, output_name: &str) -> io::Result<Self> {
        let state_dir = state_dir(session_dir);
        fs::create_dir_all(&state_dir)?;
        let stop_file = state_dir.join("stop-mic");
        if stop_file.exists() {
            fs::remove_file(&stop_file)?;
        }

        let mut child = spawn_helper(session_dir, &stop_file, output_name)?;
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

    pub fn stop(&mut self) -> io::Result<()> {
        if self.child.try_wait()?.is_some() {
            let _ = fs::remove_file(&self.stop_file);
            return Ok(());
        }

        fs::write(&self.stop_file, b"stop")?;
        let deadline = Instant::now() + Duration::from_secs(2);

        while Instant::now() < deadline {
            if self.child.try_wait()?.is_some() {
                let _ = fs::remove_file(&self.stop_file);
                return Ok(());
            }
            thread::sleep(Duration::from_millis(50));
        }

        self.child.kill()?;
        let _ = self.child.wait();
        let _ = fs::remove_file(&self.stop_file);
        Ok(())
    }
}

impl Drop for MicRecorder {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn spawn_helper(session_dir: &Path, stop_file: &Path, output_name: &str) -> io::Result<Child> {
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
