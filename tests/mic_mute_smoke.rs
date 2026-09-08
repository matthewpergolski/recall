use std::fs;
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn unique_session() -> PathBuf {
    std::env::temp_dir().join(format!(
        "recall-agent-smoke-mute-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0)
    ))
}

fn helper_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("capture-helper")
}

fn helper_binary() -> Option<PathBuf> {
    let helper_dir = helper_dir();
    [
        helper_dir.join(".build/debug/recall-capture"),
        helper_dir.join(".build/arm64-apple-macosx/debug/recall-capture"),
        helper_dir.join(".build/x86_64-apple-macosx/debug/recall-capture"),
    ]
    .into_iter()
    .find(|path| path.exists())
}

fn wav_pcm16_samples(path: &Path) -> (u32, Vec<i16>) {
    let bytes = fs::read(path).expect("wav exists");
    assert!(bytes.len() > 44, "wav too small: {} bytes", bytes.len());
    let mut offset = 12;
    let mut data_offset = None;
    let mut data_size = 0usize;
    let mut sample_rate = 44_100u32;
    while offset + 8 <= bytes.len() {
        let chunk_id = &bytes[offset..offset + 4];
        let chunk_size =
            u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
        if chunk_id == b"fmt " && chunk_size >= 16 {
            sample_rate = u32::from_le_bytes(
                bytes[offset + 12..offset + 16]
                    .try_into()
                    .expect("wav sample rate"),
            );
        }
        if chunk_id == b"data" {
            data_offset = Some(offset + 8);
            data_size = chunk_size;
            break;
        }
        offset += 8 + chunk_size + (chunk_size % 2);
    }
    let start = data_offset.expect("wav data chunk");
    let end = (start + data_size).min(bytes.len());
    let samples = bytes[start..end]
        .chunks_exact(2)
        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();
    (sample_rate.max(1), samples)
}

fn rms(samples: &[i16]) -> f64 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_squares: f64 = samples
        .iter()
        .map(|sample| {
            let value = f64::from(*sample) / f64::from(i16::MAX);
            value * value
        })
        .sum();
    (sum_squares / samples.len() as f64).sqrt()
}

#[test]
fn mic_mute_file_writes_silence_into_one_take() {
    if std::env::var_os("RECALL_CAPTURE_SMOKE").is_none() {
        eprintln!("skipping mic mute capture smoke; set RECALL_CAPTURE_SMOKE=1 to run it");
        return;
    }

    let session = unique_session();
    fs::create_dir_all(session.join(".recall/state")).unwrap();
    let stop_file = session.join(".recall/state/stop-mic");
    let mute_file = session.join(".recall/state/mute-mic");
    let _ = fs::remove_file(&stop_file);
    let _ = fs::remove_file(&mute_file);

    let mut command = if let Some(binary) = helper_binary() {
        let mut command = Command::new(binary);
        command.arg("record-mic");
        command
    } else {
        let mut command = Command::new("swift");
        command
            .arg("run")
            .arg("recall-capture")
            .arg("record-mic")
            .current_dir(helper_dir());
        command
    };

    command
        .arg("--session-dir")
        .arg(&session)
        .arg("--duration")
        .arg("20")
        .arg("--stop-file")
        .arg(&stop_file)
        .arg("--mute-file")
        .arg(&mute_file)
        .arg("--output-name")
        .arg("mic-001.m4a")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            let _ = fs::remove_dir_all(&session);
            panic!("failed to spawn recall-capture: {error}");
        }
    };

    let stdout = child.stdout.take().expect("helper stdout");
    let stderr = child.stderr.take().expect("helper stderr");
    let stdout_handle = thread::spawn(move || {
        BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
            .collect::<Vec<_>>()
    });
    let stderr_handle = thread::spawn(move || {
        BufReader::new(stderr)
            .lines()
            .map_while(Result::ok)
            .collect::<Vec<_>>()
    });

    let started = Instant::now();
    let mut saw_start = false;
    while started.elapsed() < Duration::from_secs(8) {
        if session.join("audio/mic-001.m4a").exists() {
            saw_start = true;
            break;
        }
        if let Ok(Some(status)) = child.try_wait() {
            let lines = stdout_handle.join().unwrap_or_default();
            let err_lines = stderr_handle.join().unwrap_or_default();
            let combined = format!("{}\n{}", lines.join("\n"), err_lines.join("\n"));
            let _ = fs::remove_dir_all(&session);
            if combined.to_lowercase().contains("permission")
                || combined.to_lowercase().contains("denied")
            {
                eprintln!(
                    "mic mute smoke: microphone permission denied; not bypassing.\n{combined}"
                );
                return;
            }
            panic!("helper exited before recording started ({status}): {combined}");
        }
        thread::sleep(Duration::from_millis(50));
    }
    if !saw_start {
        let _ = child.kill();
        let _ = child.wait();
        let lines = stdout_handle.join().unwrap_or_default();
        let err_lines = stderr_handle.join().unwrap_or_default();
        let combined = format!("{}\n{}", lines.join("\n"), err_lines.join("\n"));
        let _ = fs::remove_dir_all(&session);
        if combined.to_lowercase().contains("permission")
            || combined.to_lowercase().contains("denied")
        {
            eprintln!("mic mute smoke: microphone permission denied; not bypassing.\n{combined}");
            return;
        }
        panic!("helper did not start writing audio/mic-001.m4a: {combined}");
    }

    thread::sleep(Duration::from_secs(2));
    fs::write(&mute_file, b"mute").unwrap();
    thread::sleep(Duration::from_secs(2));
    let _ = fs::remove_file(&mute_file);
    thread::sleep(Duration::from_secs(2));
    fs::write(&stop_file, b"stop").unwrap();

    let deadline = Instant::now() + Duration::from_secs(5);
    let status = loop {
        if let Ok(Some(status)) = child.try_wait() {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            break child.wait().expect("helper wait after kill");
        }
        thread::sleep(Duration::from_millis(50));
    };

    let lines = stdout_handle.join().unwrap_or_default();
    let err_lines = stderr_handle.join().unwrap_or_default();
    let combined = format!("{}\n{}", lines.join("\n"), err_lines.join("\n"));
    let audio = session.join("audio/mic-001.m4a");
    let wav = session.join("audio/mic-001.wav");

    let cleanup = || {
        let _ = fs::remove_dir_all(&session);
    };

    if combined.to_lowercase().contains("permission") || combined.to_lowercase().contains("denied")
    {
        cleanup();
        eprintln!("mic mute smoke: microphone permission denied; not bypassing.\n{combined}");
        return;
    }

    assert!(status.success(), "helper failed ({status}): {combined}");
    assert!(audio.is_file(), "missing {audio:?}: {combined}");

    let convert = Command::new("afconvert")
        .args(["-f", "WAVE", "-d", "LEI16"])
        .arg(&audio)
        .arg(&wav)
        .output()
        .expect("afconvert");
    assert!(
        convert.status.success(),
        "afconvert failed: {}",
        String::from_utf8_lossy(&convert.stderr)
    );

    let (sample_rate, samples) = wav_pcm16_samples(&wav);
    let duration = samples.len() as f64 / f64::from(sample_rate);
    assert!(
        (5.0..8.5).contains(&duration),
        "expected ~6s mic file, got {duration:.2}s ({} samples)",
        samples.len()
    );

    let third = samples.len() / 3;
    let first = rms(&samples[..third]);
    let middle = rms(&samples[third..third * 2]);
    let last = rms(&samples[third * 2..]);
    eprintln!("mic mute smoke RMS first={first:.6} middle={middle:.6} last={last:.6} duration={duration:.2}s");
    assert!(
        middle < 0.02,
        "muted middle third should be near silence, rms={middle:.6} (first={first:.6} last={last:.6})"
    );

    cleanup();
}
