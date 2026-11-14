use std::env;
use std::io::{self, IsTerminal};

/// Stdout is a terminal that can take live repaint. `TERM=dumb` and pipes are not.
pub fn stdout_is_live() -> bool {
    terminal_is_live(io::stdout().is_terminal(), env::var("TERM").ok().as_deref())
}

fn terminal_is_live(is_terminal: bool, term: Option<&str>) -> bool {
    is_terminal && term != Some("dumb")
}

/// Stdout can take ANSI styling: a live terminal without `NO_COLOR`.
pub fn stdout_takes_style() -> bool {
    stdout_is_live() && env::var_os("NO_COLOR").is_none()
}

#[cfg(test)]
mod tests {
    use super::terminal_is_live;

    #[test]
    fn a_dumb_terminal_is_not_live_and_a_pipe_is_not_live() {
        assert!(!terminal_is_live(true, Some("dumb")));
        assert!(terminal_is_live(true, Some("xterm-256color")));
        assert!(terminal_is_live(true, None));
        assert!(!terminal_is_live(false, Some("xterm-256color")));
    }
}
