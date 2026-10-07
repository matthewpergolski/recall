//! Loudness of an audio track over time, and two questions asked of it.
//!
//! The transcript's words cannot say whether the mic heard the user or only
//! the far side coming out of the speakers: a recognizer garbles bleed, and
//! invents words from room noise. The audio can. This module turns a track
//! into its loudness in 10 ms steps and answers:
//!
//! - how closely the mic's loudness follows the call's over a span, and
//! - whether the mic's audio ever rises above the room over a span.

use std::io::{self, Read};
use std::path::Path;
use std::process::{Command, Stdio};

use crate::audio::mono_downmix_filter;

/// One loudness value covers this much audio.
pub const FRAME_MS: u64 = 10;
const SAMPLE_RATE: usize = 8_000;
const SAMPLES_PER_FRAME: usize = SAMPLE_RATE * FRAME_MS as usize / 1000;

/// A span shorter than this has too few values to compare.
const MIN_MATCH_FRAMES: usize = 30;
/// How far either side of the call the mic may sit and still be the same
/// sound: the tolerance the timeline was built to.
pub const NEAR_LAG_MS: u64 = 250;
/// A wider search. A match found only out here means the tracks were placed
/// wrong, not that the mic heard something else.
pub const WIDE_LAG_MS: u64 = 1_000;
/// A frame this far over the part's quiet level counts as loud: 6 dB.
const LOUD_OVER_QUIET: f32 = 2.0;
/// A part shorter than this is too short to say what its quiet level is.
const MIN_PART_MS_FOR_QUIET: u64 = 5_000;
/// A call frame under this level is silence: about -60 dB of full scale.
const CALL_SILENCE: f32 = 0.001;
/// How far ahead of the mic the call is fed to the cancelling filter.
const CANCEL_HEAD_START_MS: i64 = 100;
/// Cancelling must remove this much of a frame, 6 dB, to have explained it.
const EXPLAINED_BY_CALL: f32 = 0.5;

/// The loudness of one track: the RMS of each 10 ms of its audio.
#[derive(Debug, Clone, Default)]
pub struct Loudness {
    frames: Vec<f32>,
}

impl Loudness {
    /// Decodes a track with ffmpeg and keeps only its loudness. An hour of
    /// audio becomes about 360,000 numbers.
    pub fn from_audio(ffmpeg: &Path, audio: &Path) -> io::Result<Self> {
        let mut command = Command::new(ffmpeg);
        command
            .args(["-nostdin", "-v", "error", "-i"])
            .arg(audio)
            .args(["-map", "0:a:0", "-af"])
            .arg(mono_downmix_filter())
            .args(["-ar", "8000", "-f", "f32le", "-"]);
        Self::read(command, audio)
    }

    /// The loudness of what is left of the mic after the call's sound is
    /// cancelled out of it. Nothing is written; this is a measurement.
    ///
    /// ffmpeg's adaptive filter learns how the call's sound reaches the mic
    /// through the speakers and the room, and subtracts its estimate. What
    /// the mic heard that the call did not play stays. `mic_after_call_ms` is
    /// how much later the mic track starts than the call track.
    ///
    /// Fails on an ffmpeg without the `anlms` filter (older than 5.1).
    pub fn of_mic_without_call(
        ffmpeg: &Path,
        call_audio: &Path,
        mic_audio: &Path,
        mic_after_call_ms: i64,
    ) -> io::Result<Self> {
        // The filter can only look back, so the call must reach it first.
        // Its taps span 256 ms; the call is given a 100 ms head start.
        let lead_ms = mic_after_call_ms + CANCEL_HEAD_START_MS;
        let align = if lead_ms >= 0 {
            format!("atrim=start={:.3},asetpts=N/SR/TB", lead_ms as f64 / 1000.0)
        } else {
            format!("adelay={}:all=1", -lead_ms)
        };
        let downmix = mono_downmix_filter();
        let graph = format!(
            "[0:a:0]{downmix},aresample=8000,{align}[call];\
             [1:a:0]{downmix},aresample=8000[mic];\
             [call][mic]anlms=order=2048:mu=0.3:eps=0.001:out_mode=o[left]"
        );
        let mut command = Command::new(ffmpeg);
        command
            .args(["-nostdin", "-v", "error", "-i"])
            .arg(call_audio)
            .arg("-i")
            .arg(mic_audio)
            .args(["-filter_complex", &graph, "-map", "[left]"])
            .args(["-f", "f32le", "-"]);
        Self::read(command, mic_audio)
    }

    /// Runs ffmpeg and folds its mono 8 kHz float output into loudness.
    fn read(mut command: Command, audio: &Path) -> io::Result<Self> {
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("ffmpeg gave no output stream"))?;
        let mut frames = Vec::new();
        let mut bytes = vec![0u8; SAMPLES_PER_FRAME * 4];
        // read_exact leaves a last partial frame out, which is under 10 ms.
        while stdout.read_exact(&mut bytes).is_ok() {
            let sum: f32 = bytes
                .chunks_exact(4)
                .map(|sample| {
                    let value = f32::from_le_bytes([sample[0], sample[1], sample[2], sample[3]]);
                    value * value
                })
                .sum();
            frames.push((sum / SAMPLES_PER_FRAME as f32).sqrt());
        }
        if !child.wait()?.success() {
            return Err(io::Error::other(format!(
                "ffmpeg could not decode {}",
                audio.display()
            )));
        }
        Ok(Self { frames })
    }

    #[cfg(test)]
    pub fn from_frames(frames: Vec<f32>) -> Self {
        Self { frames }
    }

    /// The frames of `start_ms..end_ms`, counted from the track's own start.
    fn span(&self, start_ms: u64, end_ms: u64) -> &[f32] {
        let start = ((start_ms / FRAME_MS) as usize).min(self.frames.len());
        let end = ((end_ms / FRAME_MS) as usize).clamp(start, self.frames.len());
        &self.frames[start..end]
    }

    /// The level the quietest tenth of a part stays under: the room, with
    /// nobody speaking. Silence inserted for lost time is not the room and
    /// is skipped. None for a part too short to tell.
    pub fn quiet_level(&self, part_start_ms: u64, part_end_ms: u64) -> Option<f32> {
        if part_end_ms.saturating_sub(part_start_ms) < MIN_PART_MS_FOR_QUIET {
            return None;
        }
        let mut heard: Vec<f32> = self
            .span(part_start_ms, part_end_ms)
            .iter()
            .copied()
            .filter(|frame| *frame > 0.0)
            .collect();
        if heard.is_empty() {
            return None;
        }
        heard.sort_by(f32::total_cmp);
        Some(heard[heard.len() / 10])
    }

    /// How a span of this track stands against a quiet level.
    pub fn rise(&self, start_ms: u64, end_ms: u64, quiet: f32) -> Option<Rise> {
        let span = self.span(start_ms, end_ms);
        if span.is_empty() || quiet <= 0.0 {
            return None;
        }
        let loud = span
            .iter()
            .filter(|frame| **frame > quiet * LOUD_OVER_QUIET)
            .count();
        Some(Rise {
            loud_share: loud as f32 / span.len() as f32,
            over_quiet_db: 20.0 * (rms(span) / quiet).max(1e-6).log10(),
        })
    }

    /// True when the track is silent for nearly all of a span.
    pub fn is_silent(&self, start_ms: u64, end_ms: u64) -> bool {
        let span = self.span(start_ms, end_ms);
        let heard = span.iter().filter(|frame| **frame > CALL_SILENCE).count();
        heard * 10 < span.len().max(1)
    }
}

/// The share of a mic line's loud frames that the call's sound does not
/// account for. A frame is left over when cancelling the call removed less
/// than 6 dB of it and what remains is still loud against the room.
///
/// `cancelled` is `Loudness::of_mic_without_call` for the same track, and the
/// times are on the mic track's own clock. None when the span has no loud
/// frame to judge.
pub fn left_over_share(
    mic: &Loudness,
    cancelled: &Loudness,
    start_ms: u64,
    end_ms: u64,
    quiet: f32,
) -> Option<f32> {
    let loud_level = quiet * LOUD_OVER_QUIET;
    let cancelled = cancelled.span(start_ms, end_ms);
    let mut loud = 0usize;
    let mut left_over = 0usize;
    for (index, heard) in mic.span(start_ms, end_ms).iter().enumerate() {
        if *heard <= loud_level {
            continue;
        }
        loud += 1;
        // Past the end of the cancelled audio nothing was explained: the call
        // track can end before the mic does.
        let explained = cancelled
            .get(index)
            .is_some_and(|left| *left <= heard * EXPLAINED_BY_CALL || *left <= loud_level);
        if !explained {
            left_over += 1;
        }
    }
    (loud > 0).then(|| left_over as f32 / loud as f32)
}

/// How far a span of audio rises above the quiet level of its part.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rise {
    /// The share of the span that is loud.
    pub loud_share: f32,
    /// The span's level over the quiet level, in dB.
    pub over_quiet_db: f32,
}

fn rms(frames: &[f32]) -> f32 {
    if frames.is_empty() {
        return 0.0;
    }
    (frames.iter().map(|frame| frame * frame).sum::<f32>() / frames.len() as f32).sqrt()
}

/// How closely the mic's loudness follows the call's over one span: 1.0 is
/// the same shape, 0 is unrelated. The mic is tried at every shift up to
/// `max_lag_ms` either side, and the best shift counts. Keep the shift small:
/// over a short span, some shift of any two sounds will look alike.
///
/// `mic_start_ms` and `call_start_ms` are the same moment on each track's own
/// clock. None when the span is too short to compare.
pub fn follow(
    mic: &Loudness,
    mic_start_ms: u64,
    call: &Loudness,
    call_start_ms: u64,
    length_ms: u64,
    max_lag_ms: u64,
) -> Option<f32> {
    let call_span = call.span(call_start_ms, call_start_ms + length_ms);
    if call_span.len() < MIN_MATCH_FRAMES {
        return None;
    }
    let mic_first = (mic_start_ms / FRAME_MS) as i64;
    let mut best: Option<f32> = None;
    let max_lag = (max_lag_ms / FRAME_MS) as i64;
    for lag in -max_lag..=max_lag {
        let first = mic_first + lag;
        if first < 0 {
            continue;
        }
        let first = first as usize;
        let Some(mic_span) = mic.frames.get(first..first + call_span.len()) else {
            continue;
        };
        if let Some(score) = correlation(mic_span, call_span) {
            best = Some(best.map_or(score, |seen| seen.max(score)));
        }
    }
    best
}

/// Pearson correlation. None when either side does not vary.
fn correlation(left: &[f32], right: &[f32]) -> Option<f32> {
    let count = left.len() as f32;
    let left_mean = left.iter().sum::<f32>() / count;
    let right_mean = right.iter().sum::<f32>() / count;
    let mut both = 0.0;
    let mut left_power = 0.0;
    let mut right_power = 0.0;
    for (a, b) in left.iter().zip(right) {
        let (a, b) = (a - left_mean, b - right_mean);
        both += a * b;
        left_power += a * a;
        right_power += b * b;
    }
    // A side whose spread is rounding error has no shape to compare.
    let varies = |power: f32, mean: f32| (power / count).sqrt() > mean.abs() * 1e-3 + 1e-7;
    if !varies(left_power, left_mean) || !varies(right_power, right_mean) {
        return None;
    }
    Some(both / (left_power * right_power).sqrt())
}

#[cfg(test)]
mod tests {
    use super::{follow, left_over_share, Loudness, Rise, NEAR_LAG_MS, WIDE_LAG_MS};

    /// A repeatable stand-in for speech: bursts and pauses of uneven length.
    fn speech(seed: u32, frames: usize) -> Vec<f32> {
        let mut state = seed;
        let mut out = Vec::with_capacity(frames);
        let mut level = 0.0f32;
        for index in 0..frames {
            if index % 7 == 0 {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                level = if (state >> 24) % 3 == 0 {
                    0.0
                } else {
                    0.02 + ((state >> 16) % 100) as f32 / 1000.0
                };
            }
            out.push(level);
        }
        out
    }

    fn with_room(frames: &[f32], room: f32) -> Vec<f32> {
        frames.iter().map(|frame| frame + room).collect()
    }

    #[test]
    fn the_mic_follows_the_call_when_it_only_hears_the_speakers() {
        let call = speech(1, 600);
        // The same sound in the room: quieter, with room noise, 30 ms late.
        let mut bleed = vec![0.002; 3];
        bleed.extend(with_room(
            &call.iter().map(|frame| frame * 0.3).collect::<Vec<_>>(),
            0.002,
        ));
        let score = follow(
            &Loudness::from_frames(bleed),
            1_000,
            &Loudness::from_frames(call),
            1_000,
            3_000,
            NEAR_LAG_MS,
        )
        .unwrap();
        assert!(score > 0.95, "{score}");
    }

    #[test]
    fn a_placement_that_is_off_by_half_a_second_matches_only_in_the_wide_search() {
        let call = Loudness::from_frames(speech(1, 600));
        let mut early = speech(1, 600)[60..].to_vec();
        early.extend(vec![0.0; 60]);
        let early = Loudness::from_frames(early);
        let wide = follow(&early, 1_000, &call, 1_000, 3_000, WIDE_LAG_MS).unwrap();
        assert!(wide > 0.95, "{wide}");
        let near = follow(&early, 1_000, &call, 1_000, 3_000, NEAR_LAG_MS).unwrap();
        assert!(near < 0.6, "{near}");
    }

    #[test]
    fn the_mic_does_not_follow_the_call_when_someone_else_speaks() {
        let call = Loudness::from_frames(speech(1, 600));
        let own_voice = Loudness::from_frames(with_room(&speech(99, 600), 0.002));
        let score = follow(&own_voice, 1_000, &call, 1_000, 3_000, NEAR_LAG_MS).unwrap();
        assert!(score < 0.5, "{score}");
    }

    #[test]
    fn a_span_too_short_or_with_no_change_gives_no_score() {
        let call = Loudness::from_frames(speech(1, 600));
        let mic = Loudness::from_frames(speech(1, 600));
        assert_eq!(follow(&mic, 1_000, &call, 1_000, 200, NEAR_LAG_MS), None);
        // A call that does not vary has no shape to follow.
        let flat = Loudness::from_frames(vec![0.01; 600]);
        assert_eq!(follow(&mic, 1_000, &flat, 1_000, 3_000, NEAR_LAG_MS), None);
        // A span past the end of the audio.
        assert_eq!(follow(&mic, 1_000, &call, 9_000, 3_000, NEAR_LAG_MS), None);
    }

    #[test]
    fn the_quiet_level_is_the_room_and_ignores_inserted_silence() {
        let mut frames = vec![0.0; 100]; // silence kept for lost time
        frames.extend(with_room(&speech(5, 900), 0.001));
        let track = Loudness::from_frames(frames);
        let quiet = track.quiet_level(0, 10_000).unwrap();
        assert!((quiet - 0.001).abs() < 0.0005, "{quiet}");
        // Too short a part to say.
        assert_eq!(track.quiet_level(0, 4_000), None);
        assert_eq!(
            Loudness::from_frames(vec![0.0; 900]).quiet_level(0, 9_000),
            None
        );
    }

    #[test]
    fn room_noise_does_not_rise_and_speech_does() {
        let mut frames = vec![0.0011; 500]; // five seconds of room
        frames.extend(with_room(&speech(5, 500), 0.001));
        let track = Loudness::from_frames(frames);
        let quiet = 0.001;

        let room = track.rise(1_000, 4_000, quiet).unwrap();
        assert!(
            room.loud_share < 0.01 && room.over_quiet_db < 2.0,
            "{room:?}"
        );
        let spoken = track.rise(5_000, 10_000, quiet).unwrap();
        assert!(
            spoken.loud_share > 0.4 && spoken.over_quiet_db > 15.0,
            "{spoken:?}"
        );
        assert_eq!(track.rise(20_000, 21_000, quiet), None);
        assert_eq!(track.rise(1_000, 4_000, 0.0), None);
        let _: Rise = room;
    }

    #[test]
    fn what_cancelling_leaves_is_the_sound_the_call_did_not_play() {
        let quiet = 0.001;
        // Three seconds of bleed, then three where someone also speaks.
        let bleed = with_room(&speech(1, 600), quiet);
        let mut cancelled = vec![quiet; 300]; // the call explained all of it
        cancelled.extend(with_room(&speech(77, 300), quiet)); // a voice remains
        let mic = Loudness::from_frames(bleed);
        let cancelled = Loudness::from_frames(cancelled);

        let only_bleed = left_over_share(&mic, &cancelled, 0, 3_000, quiet).unwrap();
        assert!(only_bleed < 0.05, "{only_bleed}");
        let with_voice = left_over_share(&mic, &cancelled, 3_000, 6_000, quiet).unwrap();
        assert!(with_voice > 0.3, "{with_voice}");

        // Loud bleed that cancelling brought down by 12 dB, but not to the
        // room, is still explained by the call.
        let loud = Loudness::from_frames(vec![0.08; 300]);
        let reduced = Loudness::from_frames(vec![0.02; 300]);
        assert_eq!(left_over_share(&loud, &reduced, 0, 3_000, quiet), Some(0.0));
        // Cancelling that removed almost nothing explains nothing.
        let untouched = Loudness::from_frames(vec![0.07; 300]);
        assert_eq!(
            left_over_share(&loud, &untouched, 0, 3_000, quiet),
            Some(1.0)
        );
        // A span with nothing loud in it cannot be judged.
        let silent = Loudness::from_frames(vec![quiet; 300]);
        assert_eq!(left_over_share(&silent, &silent, 0, 3_000, quiet), None);
        // The cancelled audio ends halfway through the line: the call track
        // stopped first. The half it does not cover is not explained.
        let short = Loudness::from_frames(vec![0.02; 150]);
        assert_eq!(left_over_share(&loud, &short, 0, 3_000, quiet), Some(0.5));
    }

    fn find_ffmpeg() -> Option<std::path::PathBuf> {
        [
            "/opt/homebrew/bin/ffmpeg",
            "/usr/local/bin/ffmpeg",
            "/usr/bin/ffmpeg",
        ]
        .iter()
        .map(std::path::PathBuf::from)
        .find(|path| path.exists())
    }

    #[test]
    fn cancelling_the_call_keeps_a_second_sound_and_removes_the_first() {
        let Some(ffmpeg) = find_ffmpeg() else {
            return;
        };
        let dir = std::env::temp_dir().join(format!("recall-cancel-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let call = dir.join("call.wav");
        let mic = dir.join("mic.wav");
        let run = |args: &[&str], output: &std::path::Path| {
            let status = std::process::Command::new(&ffmpeg)
                .args(["-nostdin", "-v", "error", "-y"])
                .args(args)
                .arg(output)
                .status()
                .unwrap();
            assert!(status.success());
        };
        // The call: 30 s of noise that swells and fades, as speech does.
        run(
            &[
                "-f",
                "lavfi",
                "-i",
                "anoisesrc=color=pink:amplitude=0.3:duration=30:sample_rate=16000:seed=7",
                "-af",
                "tremolo=f=3:d=0.9",
            ],
            &call,
        );
        // The mic: the call 30 ms late and quieter, a little room noise, and
        // from 24 s to 27 s a tone the call never played.
        run(
            &[
                "-i", call.to_str().unwrap(),
                "-f", "lavfi", "-i", "sine=frequency=700:sample_rate=16000:duration=30",
                "-f", "lavfi", "-i", "anoisesrc=color=white:amplitude=0.002:duration=30:sample_rate=16000:seed=11",
                "-filter_complex",
                "[0:a]adelay=30:all=1,volume=0.3[bleed];[1:a]volume='if(between(t,24,27),0.3,0)':eval=frame[voice];[bleed][voice][2:a]amix=inputs=3:normalize=0",
            ],
            &mic,
        );

        let heard = Loudness::from_audio(&ffmpeg, &mic).unwrap();
        let Ok(left) = Loudness::of_mic_without_call(&ffmpeg, &call, &mic, 0) else {
            // An ffmpeg without the adaptive filter: nothing to test here.
            let _ = std::fs::remove_dir_all(&dir);
            return;
        };
        let quiet = 0.002;
        // After the filter has settled, the bleed alone is explained...
        let bleed_only = left_over_share(&heard, &left, 16_000, 23_000, quiet).unwrap();
        assert!(bleed_only < 0.1, "{bleed_only}");
        // ...and the tone is not.
        let with_tone = left_over_share(&heard, &left, 24_300, 26_700, quiet).unwrap();
        assert!(with_tone > 0.7, "{with_tone}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_track_is_silent_only_when_nearly_nothing_is_heard() {
        let mut frames = vec![0.0; 300];
        frames.extend(speech(1, 300));
        let call = Loudness::from_frames(frames);
        assert!(call.is_silent(0, 3_000));
        assert!(!call.is_silent(3_000, 6_000));
        assert!(call.is_silent(50_000, 51_000));
    }
}
