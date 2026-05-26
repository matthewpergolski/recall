use std::fs;
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct SystemEvent {
    #[serde(rename = "type")]
    pub event_type: String,
    pub path: Option<String>,
    #[serde(rename = "elapsedSeconds")]
    pub elapsed_seconds: Option<f64>,
    #[serde(rename = "levelDb")]
    pub level_db: Option<f32>,
    pub message: Option<String>,
}

pub struct SystemRecorder {
    child: Child,
    stop_file: PathBuf,
    receiver: Receiver<SystemEvent>,
}

impl SystemRecorder {
    pub fn start(session_dir: &Path) -> io::Result<Self> {
        let stop_file = session_dir.join(".recall-stop-system");
        if stop_file.exists() {
            fs::remove_file(&stop_file)?;
        }

        let mut child = spawn_helper(session_dir, &stop_file)?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("failed to capture recall-capture stdout"))?;
        let (sender, receiver) = mpsc::channel();

        thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines().map_while(Result::ok) {
                if let Ok(event) = serde_json::from_str::<SystemEvent>(&line) {
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

    pub fn drain_events(&mut self) -> Vec<SystemEvent> {
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
}

impl Drop for SystemRecorder {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn spawn_helper(session_dir: &Path, stop_file: &Path) -> io::Result<Child> {
    let helper_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("capture-helper");

    let mut command = if let Some(binary) = helper_binary(&helper_dir) {
        let mut command = Command::new(binary);
        command.arg("record-audio-tap");
        command
    } else {
        let mut command = Command::new("swift");
        command
            .arg("run")
            .arg("recall-capture")
            .arg("record-audio-tap")
            .current_dir(&helper_dir);
        command
    };

    command
        .arg("--session-dir")
        .arg(session_dir)
        .arg("--stop-file")
        .arg(stop_file)
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
