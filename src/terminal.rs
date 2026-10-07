use std::env;
use std::io::{self, IsTerminal};

/// Stdout is a terminal that can take live repaint. `TERM=dumb` and pipes are not.
pub fn stdout_is_live() -> bool {
    io::stdout().is_terminal() && env::var("TERM").ok().as_deref() != Some("dumb")
}

/// Stdout can take ANSI styling: a live terminal without `NO_COLOR`.
pub fn stdout_takes_style() -> bool {
    stdout_is_live() && env::var_os("NO_COLOR").is_none()
}
