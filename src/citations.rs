//! Checks the times an agent cites against the lines of the transcript.
//!
//! An agent returns a timestamp, and sometimes a quote, for each item of the
//! meeting notes. Nothing makes those true, so each one is looked up here.
//!
//! A traced item says where to look. It does not say the agent read the line
//! correctly: only that the time is on a line, and that quoted words were said.

use crate::transcription::{multiset_intersection_count, normalized_tokens};

/// A cited time may sit this far outside a line and still mean that line.
const TIME_TOLERANCE_MS: u64 = 2_000;
/// A shorter quote matches too many lines to pick one.
const MIN_QUOTE_TOKENS: usize = 4;
/// The share of a quote's words a line must hold. Agents tidy quotes.
const QUOTE_MATCH: f64 = 0.8;

#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptLine {
    pub start_ms: u64,
    pub end_ms: u64,
    pub speaker: String,
    pub text: String,
}

/// What became of the words an agent quoted for a traced item.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Quote {
    /// The words are in the cited line or the lines beside it.
    Found,
    /// A quote was given, and no line where the time points holds it.
    Missing,
    /// No quote long enough to compare.
    NotGiven,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Source {
    /// The citation points at this line of the transcript.
    Traced {
        start_ms: u64,
        speaker: String,
        quote: Quote,
    },
    /// A time was given and no line holds it.
    NotFound,
    /// No usable time was given.
    NoneGiven,
}

/// The timed lines of a transcript: `- [00:00.000 - 00:02.400] **mic:** text`.
pub fn timed_lines(transcript: &str) -> Vec<TranscriptLine> {
    transcript.lines().filter_map(timed_line).collect()
}

fn timed_line(line: &str) -> Option<TranscriptLine> {
    let rest = line.trim_start().strip_prefix("- [")?;
    let (range, rest) = rest.split_once("] ")?;
    let (start, end) = range.split_once(" - ")?;
    let rest = rest.strip_prefix("**")?;
    let (speaker, text) = rest.split_once(":**")?;
    Some(TranscriptLine {
        start_ms: parse_time(start)?,
        end_ms: parse_time(end)?,
        speaker: speaker.trim().to_string(),
        text: text.trim().to_string(),
    })
}

/// Looks one citation up.
///
/// The time picks a line. A quote, when one is given, must then be in that
/// line or the lines beside it. A quote found in exactly one other line moves
/// the citation there: the agent had the words right and the time wrong.
pub fn trace(lines: &[TranscriptLine], timestamp: Option<&str>, quote: Option<&str>) -> Source {
    let Some(cited_ms) = timestamp.and_then(parse_time) else {
        return Source::NoneGiven;
    };
    let quote_tokens = quote
        .map(normalized_tokens)
        .filter(|tokens| tokens.len() >= MIN_QUOTE_TOKENS);
    let traced = |index: usize, quote: Quote| Source::Traced {
        start_ms: lines[index].start_ms,
        speaker: lines[index].speaker.clone(),
        quote,
    };
    match (line_at(lines, cited_ms), quote_tokens) {
        (Some(at_time), None) => traced(at_time, Quote::NotGiven),
        (Some(at_time), Some(tokens)) => {
            if quoted_around(lines, at_time, &tokens) {
                traced(at_time, Quote::Found)
            } else if let Some(elsewhere) = only_line_quoting(lines, &tokens) {
                traced(elsewhere, Quote::Found)
            } else {
                traced(at_time, Quote::Missing)
            }
        }
        (None, Some(tokens)) => match only_line_quoting(lines, &tokens) {
            Some(elsewhere) => traced(elsewhere, Quote::Found),
            None => Source::NotFound,
        },
        (None, None) => Source::NotFound,
    }
}

fn line_at(lines: &[TranscriptLine], cited_ms: u64) -> Option<usize> {
    lines
        .iter()
        .enumerate()
        .filter(|(_, line)| distance_ms(line, cited_ms) <= TIME_TOLERANCE_MS)
        // Agents copy the start of a line, so among overlapping lines the
        // nearest start wins.
        .min_by_key(|(_, line)| {
            (
                distance_ms(line, cited_ms),
                line.start_ms.abs_diff(cited_ms),
            )
        })
        .map(|(index, _)| index)
}

fn distance_ms(line: &TranscriptLine, cited_ms: u64) -> u64 {
    if cited_ms < line.start_ms {
        line.start_ms - cited_ms
    } else {
        cited_ms.saturating_sub(line.end_ms)
    }
}

fn holds_quote(text_tokens: &[String], quote_tokens: &[String]) -> bool {
    let shared = multiset_intersection_count(quote_tokens, text_tokens);
    shared as f64 / quote_tokens.len() as f64 >= QUOTE_MATCH
}

/// A sentence can run over two lines, so the lines beside the cited one count.
fn quoted_around(lines: &[TranscriptLine], index: usize, quote_tokens: &[String]) -> bool {
    let nearby: Vec<String> = lines[index.saturating_sub(1)..(index + 2).min(lines.len())]
        .iter()
        .flat_map(|line| normalized_tokens(&line.text))
        .collect();
    holds_quote(&nearby, quote_tokens)
}

/// The one line that holds the quote, or nothing when no line or several do.
fn only_line_quoting(lines: &[TranscriptLine], quote_tokens: &[String]) -> Option<usize> {
    let mut matching = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| holds_quote(&normalized_tokens(&line.text), quote_tokens))
        .map(|(index, _)| index);
    let first = matching.next()?;
    matching.next().is_none().then_some(first)
}

/// Reads `MM:SS`, `MM:SS.mmm`, or `H:MM:SS` from the start of a citation.
/// Brackets and whatever follows the first time are ignored.
pub fn parse_time(text: &str) -> Option<u64> {
    let start = text.find(|ch: char| ch.is_ascii_digit())?;
    let time: String = text[start..]
        .chars()
        .take_while(|ch| ch.is_ascii_digit() || matches!(ch, ':' | '.'))
        .collect();
    let parts: Vec<&str> = time.trim_end_matches(['.', ':']).split(':').collect();
    let (whole, seconds) = match parts.as_slice() {
        [minutes, seconds] => (minutes.parse::<u64>().ok()?, seconds),
        [hours, minutes, seconds] => (
            hours.parse::<u64>().ok()? * 60 + minutes.parse::<u64>().ok()?,
            seconds,
        ),
        _ => return None,
    };
    let seconds = seconds.parse::<f64>().ok()?;
    if !(0.0..60.0).contains(&seconds) {
        return None;
    }
    Some(whole * 60_000 + (seconds * 1000.0).round() as u64)
}

/// `MM:SS`, as the notes show a traced time.
pub fn format_time(ms: u64) -> String {
    format!("{:02}:{:02}", ms / 60_000, (ms / 1000) % 60)
}

#[cfg(test)]
mod tests {
    use super::{format_time, parse_time, timed_lines, trace, Quote, Source};

    const TRANSCRIPT: &str = "# Sync\n\n## Clean Conversation\n\n\
- [00:00.000 - 00:02.400] **mic:** The quarterly review is on Thursday at ten.\n\
- [00:02.400 - 00:04.260] **call:** Please bring the budget numbers with you.\n\
- [01:10.000 - 01:14.500] **call:** We agreed to launch the beta on Friday morning.\n\
- [01:12.000 - 01:13.000] **mic:** Sounds good to me.\n\
not a timed line\n";

    fn traced(start_ms: u64, speaker: &str) -> Source {
        with_quote(start_ms, speaker, Quote::NotGiven)
    }

    fn with_quote(start_ms: u64, speaker: &str, quote: Quote) -> Source {
        Source::Traced {
            start_ms,
            speaker: speaker.to_string(),
            quote,
        }
    }

    #[test]
    fn reads_only_the_timed_lines_of_a_transcript() {
        let lines = timed_lines(TRANSCRIPT);
        assert_eq!(lines.len(), 4);
        assert_eq!(lines[1].start_ms, 2_400);
        assert_eq!(lines[1].end_ms, 4_260);
        assert_eq!(lines[1].speaker, "call");
        assert_eq!(lines[1].text, "Please bring the budget numbers with you.");
        assert!(timed_lines("## Microphone\n\nJust text.\n").is_empty());
    }

    #[test]
    fn reads_the_time_forms_agents_return() {
        assert_eq!(parse_time("00:34.100"), Some(34_100));
        assert_eq!(parse_time("01:05"), Some(65_000));
        assert_eq!(parse_time("75:02.400"), Some(4_502_400));
        assert_eq!(parse_time("1:02:03"), Some(3_723_000));
        assert_eq!(parse_time("[00:12.300 - 00:14.000]"), Some(12_300));
        assert_eq!(parse_time("at 00:12."), Some(12_000));
        assert_eq!(parse_time("unknown"), None);
        assert_eq!(parse_time("null"), None);
        assert_eq!(parse_time("12"), None);
        assert_eq!(parse_time("00:75"), None);
        assert_eq!(format_time(4_502_400), "75:02");
    }

    #[test]
    fn a_time_inside_a_line_is_traced_to_that_line() {
        let lines = timed_lines(TRANSCRIPT);
        assert_eq!(
            trace(&lines, Some("00:02.400"), None),
            traced(2_400, "call")
        );
        assert_eq!(trace(&lines, Some("00:01"), None), traced(0, "mic"));
        // Two lines overlap at 01:12; the one that starts there wins.
        assert_eq!(
            trace(&lines, Some("01:12.000"), None),
            traced(72_000, "mic")
        );
        assert_eq!(
            trace(&lines, Some("01:10.000"), None),
            traced(70_000, "call")
        );
        // A little past the end of a line still means that line.
        assert_eq!(
            trace(&lines, Some("00:05.500"), None),
            traced(2_400, "call")
        );
    }

    #[test]
    fn a_time_with_no_line_is_not_found_unless_the_quote_picks_one_line() {
        let lines = timed_lines(TRANSCRIPT);
        assert_eq!(trace(&lines, Some("00:40.000"), None), Source::NotFound);
        assert_eq!(
            trace(
                &lines,
                Some("00:40.000"),
                Some("agreed to launch the beta on Friday")
            ),
            with_quote(70_000, "call", Quote::Found)
        );
        // A quote nobody said, and a quote too short to pick a line.
        assert_eq!(
            trace(
                &lines,
                Some("00:40.000"),
                Some("we will hire three more engineers")
            ),
            Source::NotFound
        );
        assert_eq!(
            trace(&lines, Some("00:40.000"), Some("sounds good")),
            Source::NotFound
        );
    }

    #[test]
    fn a_quote_must_be_where_the_time_points() {
        let lines = timed_lines(TRANSCRIPT);
        // The words are in the cited line.
        assert_eq!(
            trace(
                &lines,
                Some("01:10.000"),
                Some("launch the beta on Friday morning")
            ),
            with_quote(70_000, "call", Quote::Found)
        );
        // A sentence that runs over two lines is found beside the cited one.
        assert_eq!(
            trace(
                &lines,
                Some("00:00.000"),
                Some("Thursday at ten. Please bring the budget numbers")
            ),
            with_quote(0, "mic", Quote::Found)
        );
        // The time lands on a line, but the words were said in one other
        // line: the citation moves to where they are.
        assert_eq!(
            trace(
                &lines,
                Some("00:01.000"),
                Some("agreed to launch the beta on Friday")
            ),
            with_quote(70_000, "call", Quote::Found)
        );
        // The time lands on a line and nobody said the words.
        assert_eq!(
            trace(
                &lines,
                Some("00:01.000"),
                Some("we will hire three more engineers")
            ),
            with_quote(0, "mic", Quote::Missing)
        );
        // Too few words to compare: the time alone decides.
        assert_eq!(
            trace(&lines, Some("00:01.000"), Some("sounds good")),
            with_quote(0, "mic", Quote::NotGiven)
        );
    }

    #[test]
    fn a_quote_that_fits_two_lines_rescues_nothing() {
        let transcript =
            "- [00:00.000 - 00:02.000] **call:** bring the budget numbers on Thursday\n\
- [00:30.000 - 00:32.000] **mic:** I will bring the budget numbers on Thursday\n";
        let lines = timed_lines(transcript);
        assert_eq!(
            trace(
                &lines,
                Some("05:00"),
                Some("bring the budget numbers on Thursday")
            ),
            Source::NotFound
        );
    }

    #[test]
    fn no_usable_time_means_no_source_was_given() {
        let lines = timed_lines(TRANSCRIPT);
        assert_eq!(
            trace(&lines, None, Some("launch the beta on Friday morning")),
            Source::NoneGiven
        );
        assert_eq!(trace(&lines, Some("null"), None), Source::NoneGiven);
        assert_eq!(trace(&lines, Some("  "), None), Source::NoneGiven);
    }
}
