//! VRChat output-log tailer and parser (M3).

/// Events parsed from VRChat log lines.
#[derive(Debug)]
pub enum LogEvent {}

/// Parse a single log line; pure and unit-tested.
pub fn parse_line(_line: &str) -> Option<LogEvent> {
    None
}
