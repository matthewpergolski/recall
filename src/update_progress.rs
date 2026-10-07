use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Once};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::terminal;

const BAR_WIDTH: usize = 32;
const MIN_BAR_WIDTH: usize = 8;
const FALLBACK_COLUMNS: usize = 80;

/// Live progress bar for `recall update` on a terminal.
///
/// Piped and `TERM=dumb` output stays the old `  Tests... done` lines.
/// A terminal shows one bar for the whole update. `NO_COLOR` keeps the bar
/// shape and drops ANSI color. The bar shrinks to fit a narrow pane and drops
/// to elapsed time alone when there is no room for it.
pub struct UpdateProgress {
    interactive: bool,
    color: bool,
    running: bool,
    state: Arc<BarState>,
    draw: Arc<Mutex<()>>,
    ticker: Option<JoinHandle<()>>,
}

struct BarState {
    stop: AtomicBool,
    drawn: AtomicBool,
    cursor_hidden: AtomicBool,
    indeterminate: AtomicBool,
    spaced: AtomicBool,
    /// Visible widths of the two rows on screen while a block is painted.
    painted: Mutex<Option<[usize; 2]>>,
    delay_ms: AtomicU64,
    label: Mutex<String>,
    started: Mutex<Instant>,
    work_started: Mutex<Option<Instant>>,
    done: Mutex<f64>,
    current: Mutex<f64>,
    total: Mutex<f64>,
}

impl UpdateProgress {
    pub fn detect() -> Self {
        Self::with_mode(terminal::stdout_is_live(), terminal::stdout_takes_style())
    }

    #[cfg(test)]
    pub fn plain() -> Self {
        Self::with_mode(false, false)
    }

    fn with_mode(interactive: bool, color: bool) -> Self {
        Self {
            interactive,
            color,
            running: false,
            state: Arc::new(BarState {
                stop: AtomicBool::new(true),
                drawn: AtomicBool::new(false),
                cursor_hidden: AtomicBool::new(false),
                indeterminate: AtomicBool::new(false),
                spaced: AtomicBool::new(false),
                painted: Mutex::new(None),
                delay_ms: AtomicU64::new(0),
                label: Mutex::new(String::new()),
                started: Mutex::new(Instant::now()),
                work_started: Mutex::new(None),
                done: Mutex::new(0.0),
                current: Mutex::new(0.0),
                total: Mutex::new(0.0),
            }),
            draw: Arc::new(Mutex::new(())),
            ticker: None,
        }
    }

    pub fn is_interactive(&self) -> bool {
        self.interactive
    }

    /// Bar for a short wait such as `git fetch`. Stays hidden for 200ms so a
    /// fast check does not flash. Returns whether a line was actually drawn.
    pub fn arm_ephemeral(&mut self, label: &str) {
        if self.interactive {
            self.spawn_bar(label, true, 200);
        }
    }

    pub fn disarm_ephemeral(&mut self) -> bool {
        if !self.interactive {
            return false;
        }
        let drawn = self.stop_ticker();
        if let Some(painted) = lock(&self.state.painted).take() {
            let _ = self.write_raw(&format!("{}\x1b[J", rewind(painted, terminal_columns())));
        } else if drawn {
            let _ = self.write_raw("\r\x1b[K");
        }
        self.running = false;
        drawn
    }

    pub fn open_bar(&mut self, header: &str, total: f64) -> io::Result<()> {
        if self.interactive {
            *lock(&self.state.total) = total;
            *lock(&self.state.done) = 0.0;
            *lock(&self.state.current) = 0.0;
            *lock(&self.state.work_started) = Some(Instant::now());
            self.state.spaced.store(true, Ordering::SeqCst);
            self.write_raw(&format!("{header}\n"))?;
        }
        Ok(())
    }

    pub fn print_static_failure(&mut self, label: &str) -> io::Result<()> {
        if self.interactive {
            let text = format!("  {} {label}\n", glyph_fail(self.color));
            self.write_raw(&text)?;
        }
        Ok(())
    }

    pub fn start_step(&mut self, label: &str) -> io::Result<()> {
        if !self.interactive {
            print!("  {label}... ");
            io::stdout().flush()?;
            self.running = true;
            return Ok(());
        }
        self.finish_segment();
        self.spawn_bar(label, false, 0);
        Ok(())
    }

    pub fn succeed(&mut self) -> io::Result<()> {
        if !self.running {
            return Ok(());
        }
        if !self.interactive {
            println!("done");
            self.running = false;
            return Ok(());
        }
        self.finish_segment();
        Ok(())
    }

    pub fn fail(&mut self) -> io::Result<()> {
        if !self.running {
            return Ok(());
        }
        if !self.interactive {
            println!("failed");
            self.running = false;
            return Ok(());
        }
        let label = lock(&self.state.label).clone();
        self.stop_ticker();
        self.running = false;
        self.write_raw(&format!("\n  {} {label}\n", glyph_fail(self.color)))?;
        *lock(&self.state.painted) = None;
        Ok(())
    }

    pub fn rest(&mut self) {
        if self.running {
            let _ = self.succeed();
        }
        self.show_cursor();
    }

    /// Full bar, then the one-line result under it.
    pub fn conclude(&mut self, label: &str) -> io::Result<()> {
        self.rest();
        if !self.interactive {
            return Ok(());
        }
        let started = *lock(&self.state.work_started);
        let elapsed = started.map(|t| t.elapsed()).unwrap_or_default();
        let cols = terminal_columns();
        let bar = meter_row(Meter::Fraction(1.0), elapsed, cols, self.color).text;
        let summary = format!("  ◇ {label}");
        let painted = lock(&self.state.painted).take();
        if let Some(painted) = painted {
            let rewind = rewind(painted, cols);
            self.write_raw(&format!("{rewind}{bar}\x1b[K\n\r{summary}\x1b[K\n"))?;
        } else {
            self.write_raw(&format!("{bar}\n{summary}\n"))?;
        }
        Ok(())
    }

    fn finish_segment(&mut self) {
        if !self.running || !self.interactive {
            return;
        }
        let current = *lock(&self.state.current);
        *lock(&self.state.done) += current;
        *lock(&self.state.current) = 0.0;
        self.stop_ticker();
        self.running = false;
    }

    fn spawn_bar(&mut self, label: &str, indeterminate: bool, delay_ms: u64) {
        *lock(&self.state.label) = label.to_string();
        *lock(&self.state.started) = Instant::now();
        *lock(&self.state.current) = if indeterminate {
            0.0
        } else {
            step_weight(label)
        };
        self.state
            .indeterminate
            .store(indeterminate, Ordering::SeqCst);
        self.state.delay_ms.store(delay_ms, Ordering::SeqCst);
        self.state.drawn.store(false, Ordering::SeqCst);
        self.state.stop.store(false, Ordering::SeqCst);
        self.running = true;
        // The ticker hides the cursor. Install the Ctrl-C restore before it runs.
        install_cursor_restore();
        let state = Arc::clone(&self.state);
        let draw = Arc::clone(&self.draw);
        let color = self.color;
        self.ticker = Some(thread::spawn(move || ticker_loop(state, draw, color)));
    }

    fn stop_ticker(&mut self) -> bool {
        self.state.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.ticker.take() {
            let _ = handle.join();
        }
        self.state.drawn.load(Ordering::SeqCst)
    }

    fn show_cursor(&self) {
        if self.state.cursor_hidden.swap(false, Ordering::SeqCst) {
            let _ = self.write_raw("\x1b[?25h");
        }
    }

    fn write_raw(&self, text: &str) -> io::Result<()> {
        let _guard = lock(&self.draw);
        let mut out = io::stdout().lock();
        write!(out, "{text}")?;
        out.flush()
    }
}

impl Drop for UpdateProgress {
    fn drop(&mut self) {
        if self.running {
            let _ = self.fail();
        }
        self.show_cursor();
    }
}

fn ticker_loop(state: Arc<BarState>, draw: Arc<Mutex<()>>, color: bool) {
    let delay = Duration::from_millis(state.delay_ms.load(Ordering::SeqCst));
    let wait_started = Instant::now();
    while wait_started.elapsed() < delay {
        if state.stop.load(Ordering::SeqCst) {
            return;
        }
        thread::sleep(Duration::from_millis(20));
    }

    let mut frame = 0u32;
    loop {
        if state.stop.load(Ordering::SeqCst) {
            return;
        }
        let label = lock(&state.label).clone();
        let elapsed = lock(&state.started).elapsed();
        let indeterminate = state.indeterminate.load(Ordering::SeqCst);
        let meter = if indeterminate {
            Meter::Sliding(frame)
        } else {
            let done = *lock(&state.done);
            let current = *lock(&state.current);
            let total = *lock(&state.total);
            Meter::Fraction(shown_fraction(done, current, total, elapsed))
        };
        {
            let _guard = lock(&draw);
            if state.stop.load(Ordering::SeqCst) {
                return;
            }
            // Measured per frame so the block follows a resized pane.
            let cols = terminal_columns();
            let line1 = label_row(&label, cols);
            let line2 = meter_row(meter, elapsed, cols, color);
            let (line1, line2, widths) = (line1.text, line2.text, [line1.width, line2.width]);
            let mut out = io::stdout().lock();
            let mut on_screen = lock(&state.painted);
            let painted = if let Some(previous) = *on_screen {
                let rewind = rewind(previous, cols);
                write!(out, "{rewind}{line1}\x1b[K\n\r{line2}\x1b[K")
            } else if state.spaced.load(Ordering::SeqCst) {
                write!(out, "\n{line1}\n{line2}")
            } else {
                write!(out, "{line1}\n{line2}")
            };
            if painted.is_ok() {
                let _ = write!(out, "\x1b[?25l");
                let _ = out.flush();
                *on_screen = Some(widths);
                state.drawn.store(true, Ordering::SeqCst);
                state.cursor_hidden.store(true, Ordering::SeqCst);
            }
        }
        frame = frame.wrapping_add(1);
        thread::sleep(Duration::from_millis(80));
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|err| err.into_inner())
}

pub(crate) fn step_weight(label: &str) -> f64 {
    match label {
        "Fast-forward" => 1.0,
        "Tests" => 12.0,
        "Capture helper" => 3.0,
        "Install" => 4.0,
        _ => 2.0,
    }
}

pub(crate) fn plan_total(labels: &[&str]) -> f64 {
    labels.iter().map(|label| step_weight(label)).sum()
}

pub(crate) fn shown_fraction(done: f64, current: f64, total: f64, elapsed: Duration) -> f64 {
    if total <= 0.0 {
        return 0.0;
    }
    ((done + current * creep(elapsed)) / total).clamp(0.0, 0.99)
}

fn creep(elapsed: Duration) -> f64 {
    let t = elapsed.as_secs_f64();
    (1.0 - (-t / 30.0).exp()).min(0.92)
}

pub(crate) fn format_elapsed(elapsed: Duration) -> String {
    let total = elapsed.as_secs();
    let hours = total / 3600;
    let mins = (total % 3600) / 60;
    let secs = total % 60;
    if hours > 0 {
        format!("{hours}h {mins:02}m")
    } else if mins > 0 {
        format!("{mins}m {secs:02}s")
    } else {
        format!("{secs}s")
    }
}

fn terminal_columns() -> usize {
    crossterm::terminal::size()
        .ok()
        .map(|(cols, _)| usize::from(cols))
        .filter(|cols| *cols > 0)
        .unwrap_or(FALLBACK_COLUMNS)
}

/// Columns a repainted row may use: one short of the pane, so a row never
/// reaches the wrap column.
fn usable_columns(cols: usize) -> usize {
    cols.saturating_sub(1).max(1)
}

/// Rows the painted block covers at this width. A pane that got narrower
/// rewraps what is already on screen, so the block can be taller than the two
/// rows that were written.
pub(crate) fn block_rows(painted: [usize; 2], cols: usize) -> usize {
    let cols = cols.max(1);
    painted
        .iter()
        .map(|width| width.div_ceil(cols).max(1))
        .sum()
}

/// Moves from the block's last row to the start of its first. Clears below
/// when rewrapping made the block taller, so no wrapped remnant is left.
pub(crate) fn rewind(painted: [usize; 2], cols: usize) -> String {
    let rows = block_rows(painted, cols);
    let clear = if rows > 2 { "\x1b[J" } else { "" };
    format!("\x1b[{}A\r{clear}", rows - 1)
}

pub(crate) struct Row {
    pub(crate) text: String,
    pub(crate) width: usize,
}

#[derive(Clone, Copy)]
pub(crate) enum Meter {
    Fraction(f64),
    Sliding(u32),
}

fn clip(text: &str, max: usize) -> Row {
    let text: String = text.chars().take(max).collect();
    let width = text.chars().count();
    Row { text, width }
}

pub(crate) fn label_row(label: &str, cols: usize) -> Row {
    clip(&format!("  {label}..."), usable_columns(cols))
}

/// Bar cells that fit beside the elapsed time, or 0 when the pane is too
/// narrow for a readable bar.
pub(crate) fn bar_width(cols: usize, elapsed: Duration) -> usize {
    let time = format_elapsed(elapsed).chars().count() + 2;
    let room = usable_columns(cols).saturating_sub(4 + time);
    if room < MIN_BAR_WIDTH {
        0
    } else {
        room.min(BAR_WIDTH)
    }
}

pub(crate) fn meter_row(meter: Meter, elapsed: Duration, cols: usize, color: bool) -> Row {
    let width = bar_width(cols, elapsed);
    if width == 0 {
        // Plain text so it can be clipped to the pane.
        let time = format!("  ({})", format_elapsed(elapsed));
        return clip(&time, usable_columns(cols));
    }
    let bar = match meter {
        Meter::Fraction(fraction) => render_bar(fraction, width, color),
        Meter::Sliding(frame) => render_indeterminate(frame, width, color),
    };
    Row {
        text: render_meter_line(&bar, elapsed, color),
        width: 4 + width + format_elapsed(elapsed).chars().count() + 2,
    }
}

pub(crate) fn render_bar(fraction: f64, width: usize, color: bool) -> String {
    let filled = ((fraction.clamp(0.0, 1.0) * width as f64).round() as usize).min(width);
    let empty = width - filled;
    if color {
        format!(
            "\x1b[38;5;43m{}\x1b[38;5;240m{}\x1b[0m",
            "━".repeat(filled),
            "━".repeat(empty)
        )
    } else {
        format!("{}{}", "━".repeat(filled), "─".repeat(empty))
    }
}

pub(crate) fn render_indeterminate(frame: u32, width: usize, color: bool) -> String {
    const SPAN: usize = 8;
    if width == 0 {
        return String::new();
    }
    let span = SPAN.min(width / 2).max(1);
    let travel = width - span;
    let cycle = (travel * 2).max(1);
    let pos = (frame as usize) % cycle;
    let pos = if pos <= travel { pos } else { cycle - pos };
    if color {
        format!(
            "\x1b[38;5;240m{}\x1b[38;5;43m{}\x1b[38;5;240m{}\x1b[0m",
            "━".repeat(pos),
            "━".repeat(span),
            "━".repeat(width - pos - span)
        )
    } else {
        format!(
            "{}{}{}",
            "─".repeat(pos),
            "━".repeat(span),
            "─".repeat(width - pos - span)
        )
    }
}

pub(crate) fn render_meter_line(bar: &str, elapsed: Duration, color: bool) -> String {
    let time = format_elapsed(elapsed);
    let time = if color {
        format!("\x1b[2m({time})\x1b[0m")
    } else {
        format!("({time})")
    };
    format!("  {bar}  {time}")
}

fn glyph_fail(color: bool) -> String {
    if color {
        "\x1b[31m✖\x1b[0m".to_string()
    } else {
        "✖".to_string()
    }
}

#[cfg(unix)]
fn install_cursor_restore() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        // write, signal, and raise are async-signal-safe. The handler only
        // puts the cursor back, then restores the default action and re-raises.
        signal(SIGINT, restore_cursor as *const () as usize);
        signal(SIGTERM, restore_cursor as *const () as usize);
    });
}

#[cfg(not(unix))]
fn install_cursor_restore() {}

#[cfg(unix)]
const SIGINT: i32 = 2;
#[cfg(unix)]
const SIGTERM: i32 = 15;
#[cfg(unix)]
const SIG_DFL: usize = 0;

#[cfg(unix)]
unsafe extern "C" fn restore_cursor(sig: i32) {
    const SHOW: &[u8] = b"\r\x1b[?25h\n";
    unsafe {
        let _ = write(1, SHOW.as_ptr(), SHOW.len());
        signal(sig, SIG_DFL);
        raise(sig);
    }
}

#[cfg(unix)]
unsafe extern "C" {
    fn write(fd: i32, buf: *const u8, count: usize) -> isize;
    fn signal(sig: i32, handler: usize) -> usize;
    fn raise(sig: i32) -> i32;
}

#[cfg(test)]
mod tests {
    use super::{
        bar_width, block_rows, format_elapsed, label_row, meter_row, plan_total, render_bar,
        render_indeterminate, render_meter_line, rewind, shown_fraction, step_weight, Meter,
        UpdateProgress, BAR_WIDTH,
    };
    use std::time::Duration;

    #[test]
    fn formats_elapsed_for_the_active_step() {
        assert_eq!(format_elapsed(Duration::from_secs(0)), "0s");
        assert_eq!(format_elapsed(Duration::from_secs(9)), "9s");
        assert_eq!(format_elapsed(Duration::from_secs(60)), "1m 00s");
        assert_eq!(format_elapsed(Duration::from_secs(124)), "2m 04s");
        assert_eq!(format_elapsed(Duration::from_secs(3600)), "1h 00m");
        assert_eq!(format_elapsed(Duration::from_secs(3661)), "1h 01m");
    }

    #[test]
    fn plain_steps_keep_the_existing_line() {
        let mut progress = UpdateProgress::plain();
        progress.start_step("Tests").unwrap();
        progress.succeed().unwrap();
        progress.start_step("Capture helper").unwrap();
        progress.fail().unwrap();
    }

    #[test]
    fn bar_fills_from_the_left_and_colors_only_when_asked() {
        let half = "━".repeat(16) + &"─".repeat(16);
        assert_eq!(render_bar(0.5, BAR_WIDTH, false), half);
        assert_eq!(render_bar(0.0, BAR_WIDTH, false), "─".repeat(32));
        assert_eq!(render_bar(1.0, BAR_WIDTH, false), "━".repeat(32));
        let colored = render_bar(0.5, BAR_WIDTH, true);
        assert!(colored.contains("\u{1b}[38;5;43m"));
        assert!(colored.contains("\u{1b}[38;5;240m"));
        assert!(!half.contains('\u{1b}'));
    }

    #[test]
    fn meter_line_puts_elapsed_time_on_the_right() {
        let bar = render_bar(0.5, BAR_WIDTH, false);
        let line = render_meter_line(&bar, Duration::from_secs(4), false);
        assert_eq!(line, format!("  {bar}  (4s)"));
        let colored = render_meter_line(
            &render_bar(1.0, BAR_WIDTH, true),
            Duration::from_secs(4),
            true,
        );
        assert!(colored.contains("\u{1b}[2m(4s)\u{1b}[0m"));
    }

    #[test]
    fn indeterminate_window_slides() {
        let first = render_indeterminate(0, BAR_WIDTH, false);
        let later = render_indeterminate(4, BAR_WIDTH, false);
        assert!(first.starts_with("━━━━━━━━"));
        assert_ne!(first, later);
        assert!(!first.contains('\u{1b}'));
        assert!(render_indeterminate(0, BAR_WIDTH, true).contains("\u{1b}[38;5;43m"));
        for width in 0..=BAR_WIDTH {
            for frame in 0..80 {
                assert_eq!(
                    render_indeterminate(frame, width, false).chars().count(),
                    width
                );
            }
        }
    }

    /// Paints the real block for about two seconds. Needs a terminal:
    /// `cargo test live_block_smoke -- --ignored --nocapture`
    #[test]
    #[ignore = "paints to a real terminal"]
    fn live_block_smoke() {
        let pause = || std::thread::sleep(Duration::from_millis(500));
        // The harness leaves its own text on the row; the block starts at column 0.
        println!();
        let mut progress = UpdateProgress::with_mode(true, false);
        progress.arm_ephemeral("Checking origin/main");
        pause();
        progress.disarm_ephemeral();
        progress
            .open_bar("Updating recall", plan_total(&["Tests", "Install"]))
            .unwrap();
        for step in ["Tests", "Install"] {
            progress.start_step(step).unwrap();
            pause();
            progress.succeed().unwrap();
        }
        progress.conclude("recall updated").unwrap();
    }

    /// A step that fails: the bar stops and the failed step is named under it.
    /// `cargo test live_block_failure_smoke -- --ignored --nocapture`
    #[test]
    #[ignore = "paints to a real terminal"]
    fn live_block_failure_smoke() {
        let pause = || std::thread::sleep(Duration::from_millis(500));
        println!();
        let mut progress = UpdateProgress::with_mode(true, false);
        progress
            .open_bar("Updating recall", plan_total(&["Tests", "Install"]))
            .unwrap();
        progress.start_step("Tests").unwrap();
        pause();
        progress.succeed().unwrap();
        progress.start_step("Install").unwrap();
        pause();
        progress.fail().unwrap();
        progress.rest();
    }

    #[test]
    fn rows_never_reach_the_wrap_column() {
        let long = Duration::from_secs(3661);
        for cols in 1..=120 {
            for elapsed in [Duration::ZERO, Duration::from_secs(124), long] {
                for meter in [Meter::Fraction(0.4), Meter::Sliding(7)] {
                    let row = meter_row(meter, elapsed, cols, false);
                    assert_eq!(row.text.chars().count(), row.width);
                    assert!(row.width < cols.max(2), "meter at {cols} columns");
                }
            }
            let label = label_row("Checking origin/main", cols);
            assert_eq!(label.text.chars().count(), label.width);
            assert!(label.width < cols.max(2), "label at {cols} columns");
        }
    }

    #[test]
    fn bar_shrinks_then_gives_way_to_elapsed_time() {
        let elapsed = Duration::from_secs(4);
        assert_eq!(bar_width(80, elapsed), BAR_WIDTH);
        assert_eq!(bar_width(41, elapsed), BAR_WIDTH);
        assert_eq!(bar_width(30, elapsed), 21);
        assert_eq!(bar_width(17, elapsed), 8);
        assert_eq!(bar_width(16, elapsed), 0);
        assert_eq!(
            meter_row(Meter::Fraction(0.5), elapsed, 16, true).text,
            "  (4s)"
        );
        assert_eq!(
            meter_row(Meter::Fraction(0.5), elapsed, 5, true).text,
            "  (4"
        );
        // Color codes do not count toward the measured width.
        let colored = meter_row(Meter::Fraction(0.5), elapsed, 30, true);
        assert_eq!(colored.width, 29);
        assert!(colored.text.contains('\u{1b}'));
    }

    #[test]
    fn rewind_follows_rows_rewrapped_by_a_narrower_pane() {
        // Painted at 80 columns: label row 10 wide, meter row 40 wide.
        let painted = [10, 40];
        assert_eq!(block_rows(painted, 80), 2);
        assert_eq!(rewind(painted, 80), "\u{1b}[1A\r");
        assert_eq!(block_rows(painted, 40), 2);
        // Shrunk to 30 columns the meter row wraps once, to 15 it wraps twice
        // more and the label row stays single.
        assert_eq!(block_rows(painted, 30), 3);
        assert_eq!(rewind(painted, 30), "\u{1b}[2A\r\u{1b}[J");
        assert_eq!(block_rows(painted, 15), 4);
        assert_eq!(block_rows([0, 0], 0), 2);
    }

    #[test]
    fn tests_own_most_of_the_bar_and_creep_does_not_finish_early() {
        let steps = ["Fast-forward", "Tests", "Capture helper", "Install"];
        let total = plan_total(&steps);
        assert_eq!(step_weight("Tests"), 12.0);
        assert!(step_weight("Tests") > total / 2.0);
        let at_start = shown_fraction(
            step_weight("Fast-forward"),
            step_weight("Tests"),
            total,
            Duration::ZERO,
        );
        let late = shown_fraction(
            step_weight("Fast-forward"),
            step_weight("Tests"),
            total,
            Duration::from_secs(600),
        );
        assert!(at_start < 0.1);
        assert!(late < 0.99);
        assert!(late > at_start);
    }
}
