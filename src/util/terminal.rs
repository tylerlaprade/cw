//! Terminal helpers: column width detection, OSC title.

pub fn columns() -> usize {
    terminal_size::terminal_size()
        .map(|(w, _)| w.0 as usize)
        .unwrap_or(100)
}
