//! Places the parts of the mic and call tracks on one timeline.
//!
//! The capture helper writes the host-clock time of each part's first buffer.
//! From those times this module works out where each track starts and how
//! much silence sits between two parts, so that a time in the transcript
//! means the same moment on both tracks.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Two parts may overlap by this much before their times are called wrong.
/// Durations come from the container and are a little long.
pub const TOLERANCE_MS: u64 = 250;
/// The recorders of one take start within seconds of each other. A wider
/// spread means a start time came from the wrong clock.
const MAX_START_SKEW_MS: u64 = 60_000;

#[derive(Debug, Clone, PartialEq)]
pub struct Part {
    pub take: u32,
    /// Host time of the part's first buffer, in nanoseconds.
    pub start_ns: Option<u64>,
    pub duration_ms: u64,
}

/// Silence that belongs before a part: where it begins on the shared
/// timeline, and how long it is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Gap {
    pub at_ms: u64,
    pub len_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct TrackPlacement {
    /// Where the track's first part starts on the shared timeline.
    pub offset_ms: u64,
    /// One entry for each part after the first, in order. `len_ms` may be 0.
    pub gaps: Vec<Gap>,
}

impl TrackPlacement {
    /// The time the same moment had before placement: counted from the
    /// track's own first sample, with its parts joined back to back.
    pub fn unaligned_ms(&self, aligned_ms: u64) -> u64 {
        let silence: u64 = self
            .gaps
            .iter()
            .filter(|gap| gap.at_ms < aligned_ms)
            .map(|gap| gap.len_ms.min(aligned_ms - gap.at_ms))
            .sum();
        aligned_ms
            .saturating_sub(self.offset_ms)
            .saturating_sub(silence)
    }

    pub fn gap_lengths_ms(&self) -> Vec<u64> {
        self.gaps.iter().map(|gap| gap.len_ms).collect()
    }
}

/// Places every track. `tracks[i]` holds the parts of one track in order.
/// Returns one placement per track, or the reason the times cannot be used;
/// the caller then joins parts back to back, as before start times existed.
pub fn place(tracks: &[Vec<Part>]) -> Result<Vec<TrackPlacement>, String> {
    let mut starts: Vec<Vec<u64>> = Vec::new();
    for track in tracks {
        let mut track_starts = Vec::new();
        for part in track {
            track_starts.push(
                part.start_ns
                    .ok_or_else(|| "a part has no start time".to_string())?,
            );
        }
        starts.push(track_starts);
    }

    let mut takes: Vec<u32> = tracks.iter().flatten().map(|part| part.take).collect();
    takes.sort_unstable();
    takes.dedup();

    let mut placements = vec![TrackPlacement::default(); tracks.len()];
    // The end of each track so far, and of the take before, on the shared timeline.
    let mut track_end: Vec<Option<u64>> = vec![None; tracks.len()];
    let mut take_start = 0u64;

    for take in takes {
        let base_ns = tracks
            .iter()
            .zip(&starts)
            .flat_map(|(track, track_starts)| {
                track
                    .iter()
                    .zip(track_starts)
                    .filter(|(part, _)| part.take == take)
                    .map(|(_, start)| *start)
            })
            .min()
            .unwrap_or(0);

        for (index, track) in tracks.iter().enumerate() {
            let mut first_of_take = true;
            for (part, start_ns) in track.iter().zip(&starts[index]) {
                if part.take != take {
                    continue;
                }
                let since_base_ms = (start_ns - base_ns) / 1_000_000;
                if first_of_take && since_base_ms > MAX_START_SKEW_MS {
                    return Err(format!(
                        "a track starts {} s after the other in take {take}",
                        since_base_ms / 1000
                    ));
                }
                first_of_take = false;
                let wanted = take_start + since_base_ms;
                let position = match track_end[index] {
                    None => {
                        placements[index].offset_ms = wanted;
                        wanted
                    }
                    Some(end) => {
                        if wanted + TOLERANCE_MS < end {
                            return Err(format!(
                                "a part starts {} ms before the part before it ends",
                                end - wanted
                            ));
                        }
                        let position = wanted.max(end);
                        placements[index].gaps.push(Gap {
                            at_ms: end,
                            len_ms: position - end,
                        });
                        position
                    }
                };
                track_end[index] = Some(position + part.duration_ms);
            }
        }
        take_start = track_end
            .iter()
            .flatten()
            .copied()
            .max()
            .unwrap_or(take_start);
    }

    Ok(placements)
}

#[derive(Debug, Deserialize)]
struct PartRecord {
    host_time_ns: u64,
}

/// `<session>/audio/<name>` is described by `<session>/.recall/timeline/<name>.json`.
pub fn part_record_path(audio_path: &Path) -> Option<PathBuf> {
    let name = audio_path.file_name()?.to_str()?;
    let session = audio_path.parent()?.parent()?;
    Some(
        session
            .join(".recall")
            .join("timeline")
            .join(format!("{name}.json")),
    )
}

/// The recorded start of an audio part, or None when the helper wrote none.
pub fn part_start_ns(audio_path: &Path) -> Option<u64> {
    let record = fs::read_to_string(part_record_path(audio_path)?).ok()?;
    serde_json::from_str::<PartRecord>(&record)
        .ok()
        .map(|record| record.host_time_ns)
}

#[cfg(test)]
mod tests {
    use super::{part_record_path, part_start_ns, place, Gap, Part, TrackPlacement};
    use std::fs;
    use std::path::Path;

    const SECOND_NS: u64 = 1_000_000_000;

    fn part(take: u32, start_ms: u64, duration_ms: u64) -> Part {
        Part {
            take,
            // An arbitrary boot-relative base, as a host clock gives.
            start_ns: Some(370_000 * SECOND_NS + start_ms * 1_000_000),
            duration_ms,
        }
    }

    #[test]
    fn the_later_track_is_shifted_by_its_late_start() {
        // The call recorder started first; the mic followed 400 ms later.
        let placements = place(&[vec![part(1, 400, 60_000)], vec![part(1, 0, 60_400)]]).unwrap();
        assert_eq!(placements[0].offset_ms, 400);
        assert_eq!(placements[1].offset_ms, 0);
        assert!(placements[0].gaps.is_empty());
    }

    #[test]
    fn a_gap_between_two_parts_becomes_silence() {
        // Mic: 7.5 s, then 442 ms lost at a format change, then 300 s.
        let mic = vec![part(1, 0, 7_500), part(1, 7_942, 300_000)];
        let call = vec![part(1, 100, 310_000)];
        let placements = place(&[mic, call]).unwrap();
        assert_eq!(placements[0].offset_ms, 0);
        assert_eq!(
            placements[0].gaps,
            vec![Gap {
                at_ms: 7_500,
                len_ms: 442
            }]
        );
        assert_eq!(placements[0].gap_lengths_ms(), vec![442]);
        assert_eq!(placements[1].offset_ms, 100);
    }

    #[test]
    fn a_small_overlap_is_no_gap_and_a_large_one_is_a_bad_time() {
        // A container duration runs a little long: 40 ms of overlap is fine.
        let placements = place(&[vec![part(1, 0, 7_540), part(1, 7_500, 1_000)]]).unwrap();
        assert_eq!(
            placements[0].gaps,
            vec![Gap {
                at_ms: 7_540,
                len_ms: 0
            }]
        );

        let error = place(&[vec![part(1, 0, 7_500), part(1, 3_000, 1_000)]]).unwrap_err();
        assert!(error.contains("before the part before it ends"), "{error}");
    }

    #[test]
    fn a_missing_time_or_a_wild_start_refuses_the_whole_plan() {
        let mut untimed = part(1, 0, 1_000);
        untimed.start_ns = None;
        let error = place(&[vec![part(1, 0, 1_000)], vec![untimed]]).unwrap_err();
        assert_eq!(error, "a part has no start time");

        // An hour apart inside one take is a wrong clock, not a late start.
        let error = place(&[vec![part(1, 0, 1_000)], vec![part(1, 3_600_000, 1_000)]]).unwrap_err();
        assert!(error.contains("after the other in take 1"), "{error}");
    }

    #[test]
    fn each_take_has_its_own_base_and_follows_the_take_before() {
        // Take 2 was recorded an hour later, after a reboot: its clock is smaller.
        let later = |start_ms: u64, duration_ms: u64| Part {
            take: 2,
            start_ns: Some(5 * SECOND_NS + start_ms * 1_000_000),
            duration_ms,
        };
        let mic = vec![part(1, 300, 10_000), later(0, 5_000)];
        let call = vec![part(1, 0, 10_500), later(200, 5_000)];
        let placements = place(&[mic, call]).unwrap();

        // Take 1 ends at 10.5 s on the call track; take 2 starts there.
        assert_eq!(placements[0].offset_ms, 300);
        assert_eq!(
            placements[0].gaps,
            vec![Gap {
                at_ms: 10_300,
                len_ms: 200
            }]
        );
        assert_eq!(placements[1].offset_ms, 0);
        assert_eq!(
            placements[1].gaps,
            vec![Gap {
                at_ms: 10_500,
                len_ms: 200
            }]
        );
    }

    #[test]
    fn a_track_with_no_parts_is_left_alone() {
        let placements = place(&[vec![], vec![part(1, 0, 1_000)]]).unwrap();
        assert_eq!(placements[0], TrackPlacement::default());
        assert_eq!(placements[1].offset_ms, 0);
    }

    #[test]
    fn unaligned_time_removes_the_offset_and_the_silence_before_it() {
        let placement = TrackPlacement {
            offset_ms: 400,
            gaps: vec![Gap {
                at_ms: 7_900,
                len_ms: 442,
            }],
        };
        // Before the gap only the offset applies.
        assert_eq!(placement.unaligned_ms(5_400), 5_000);
        // After it, the silence is removed too.
        assert_eq!(placement.unaligned_ms(10_342), 9_500);
        // A time inside the silence maps to the join.
        assert_eq!(placement.unaligned_ms(8_000), 7_500);
        assert_eq!(placement.unaligned_ms(100), 0);
    }

    #[test]
    fn reads_the_start_time_the_helper_wrote_beside_the_session() {
        let session = std::env::temp_dir().join(format!("recall-timeline-{}", std::process::id()));
        let audio = session.join("audio/mic-002-part-02.m4a");
        assert_eq!(
            part_record_path(&audio).unwrap(),
            session.join(".recall/timeline/mic-002-part-02.m4a.json")
        );
        assert_eq!(part_start_ns(&audio), None);

        fs::create_dir_all(session.join(".recall/timeline")).unwrap();
        fs::write(
            session.join(".recall/timeline/mic-002-part-02.m4a.json"),
            r#"{"file":"mic-002-part-02.m4a","source":"mic","unix_ms":1791345553706,"host_time_ns":369998987028416}"#,
        )
        .unwrap();
        assert_eq!(part_start_ns(&audio), Some(369_998_987_028_416));

        fs::write(
            session.join(".recall/timeline/mic-002-part-02.m4a.json"),
            "not json",
        )
        .unwrap();
        assert_eq!(part_start_ns(&audio), None);
        assert_eq!(part_start_ns(Path::new("mic.m4a")), None);

        let _ = fs::remove_dir_all(session);
    }
}
