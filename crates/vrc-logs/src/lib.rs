//! VRChat output-log tailer and parser (M3).
//!
//! [`parse_line`] is a pure function over one physical log line; it
//! recognises the documented `[Behaviour]`/`[VRC Camera]` client messages a
//! companion app cares about (world joins/leaves, player joins/leaves,
//! screenshots) and attaches the log's local timestamp to every event.
//! [`LogTailer::watch`] follows the newest `output_log_*.txt` in a directory,
//! switching files when VRChat rotates its per-session logs. Log files are
//! only ever read — the VRChat process is never touched (RULES.md §5).

mod tail;

pub use tail::{LogEvents, LogTailer};

use std::fmt;
use std::path::PathBuf;

use time::{Date, Month, OffsetDateTime, PrimitiveDateTime, Time, UtcOffset};

/// Events parsed from VRChat log lines.
///
/// Every variant carries `at`, the log line's local wall-clock timestamp.
/// String fields are empty when the log line did not carry that detail
/// (e.g. pre-2024.4.1 client builds log player joins without a user id).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogEvent {
    /// The local player joined a world. `Joining wrld_…` lines carry
    /// `world_id`/`instance_id`/`location`; `Entering Room: <name>` lines
    /// carry `world_name` — a single join typically produces both.
    JoinedWorld {
        /// `wrld_…` world id (empty when unknown).
        world_id: String,
        /// World display name (empty when unknown).
        world_name: String,
        /// Instance spec after `wrld_…:` — may contain `~hidden(…)`,
        /// `~region(…)`, `~nonce(…)` parts (empty for bare world joins).
        instance_id: String,
        /// Full `wrld_…:instance` location string (empty when unknown).
        location: String,
        /// Log timestamp (local time).
        at: OffsetDateTime,
    },
    /// The local player left the room/world (`OnPlayerLeftRoom`).
    LeftWorld {
        /// Log timestamp (local time).
        at: OffsetDateTime,
    },
    /// Another player joined the local player's instance
    /// (`OnPlayerJoined <name> [(usr_…)]`).
    PlayerJoined {
        /// `usr_…` id when the client logged it (2024.4.1+); empty otherwise.
        user_id: String,
        /// Display name.
        display_name: String,
        /// Log timestamp (local time).
        at: OffsetDateTime,
    },
    /// Another player left the instance (`OnPlayerLeft <name> [(usr_…)]`).
    PlayerLeft {
        /// `usr_…` id when the client logged it; empty otherwise.
        user_id: String,
        /// Display name.
        display_name: String,
        /// Log timestamp (local time).
        at: OffsetDateTime,
    },
    /// A screenshot/photo was written (`[VRC Camera] Took screenshot to:`).
    PhotoTaken {
        /// Absolute path of the written image file.
        path: String,
        /// Log timestamp (local time).
        at: OffsetDateTime,
    },
}

impl LogEvent {
    /// The event's log timestamp.
    pub fn at(&self) -> OffsetDateTime {
        match self {
            Self::JoinedWorld { at, .. }
            | Self::LeftWorld { at }
            | Self::PlayerJoined { at, .. }
            | Self::PlayerLeft { at, .. }
            | Self::PhotoTaken { at, .. } => *at,
        }
    }
}

impl fmt::Display for LogEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let (kind, detail) = match self {
            Self::JoinedWorld {
                world_id,
                world_name,
                location,
                ..
            } => {
                let what = if !location.is_empty() {
                    location.as_str()
                } else if !world_name.is_empty() {
                    world_name.as_str()
                } else if !world_id.is_empty() {
                    world_id.as_str()
                } else {
                    "a world"
                };
                ("joined world", what.to_string())
            }
            Self::LeftWorld { .. } => ("left world", String::new()),
            Self::PlayerJoined {
                user_id,
                display_name,
                ..
            } => ("player joined", player_label(display_name, user_id)),
            Self::PlayerLeft {
                user_id,
                display_name,
                ..
            } => ("player left", player_label(display_name, user_id)),
            Self::PhotoTaken { path, .. } => ("photo taken", path.clone()),
        };
        if detail.is_empty() {
            write!(f, "{} {}", format_ts(self.at()), kind)
        } else {
            write!(f, "{} {}: {}", format_ts(self.at()), kind, detail)
        }
    }
}

fn player_label(name: &str, id: &str) -> String {
    if id.is_empty() {
        name.to_string()
    } else {
        format!("{name} ({id})")
    }
}

/// `YYYY-MM-DD HH:MM:SS` in the event's own offset (the log's local time).
fn format_ts(at: OffsetDateTime) -> String {
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        at.year(),
        at.month() as u8,
        at.day(),
        at.hour(),
        at.minute(),
        at.second(),
    )
}

/// The directory VRChat writes `output_log_*.txt` into.
///
/// Windows: `%USERPROFILE%\AppData\LocalLow\VRChat\VRChat` (the documented
/// location). Other platforms return `None` — Proton installs are not
/// auto-detected (callers may pass an explicit directory instead).
pub fn default_log_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var_os("USERPROFILE").map(|home| {
            PathBuf::from(home)
                .join("AppData")
                .join("LocalLow")
                .join("VRChat")
                .join("VRChat")
        })
    }
    #[cfg(not(target_os = "windows"))]
    {
        None
    }
}

/// Parse one physical log line into a [`LogEvent`]. Pure: no I/O, safe to
/// unit-test with synthetic lines (RULES.md §6).
///
/// Log lines look like `2024.05.14 20:45:00 Log - [Behaviour] <message>`
/// (the level field may be padded; `Debug`/`Warning`/`Error` occur too).
/// Anything that is not a recognised event — including partial lines,
/// stack traces and unrelated `[Behaviour]` chatter — yields `None`.
pub fn parse_line(line: &str) -> Option<LogEvent> {
    let (at, rest) = split_record(line.trim_end())?;
    let message = record_message(rest)?;
    let at = at.assume_offset(local_offset());

    // `[Behaviour] OnPlayerLeftRoom` — the local player left the instance.
    // Checked before the `OnPlayerLeft ` player-event patterns for clarity;
    // the prefixes are disjoint either way (`OnPlayerLeftRoom` has no space).
    if message == "OnPlayerLeftRoom" || message == "OnLeftRoom" {
        return Some(LogEvent::LeftWorld { at });
    }

    if let Some(arg) = message.strip_prefix("Joining ") {
        // `Joining wrld_x:inst~…` carries the instance location.
        // `Joining or Creating Room:`, `Joining friend:`, … do not start
        // with `wrld_` and fall through to `None`.
        let location = arg.split_whitespace().next()?;
        if !location.starts_with("wrld_") {
            return None;
        }
        let (world_id, instance_id) = match location.split_once(':') {
            Some((id, inst)) => (id.to_string(), inst.to_string()),
            None => (location.to_string(), String::new()),
        };
        return Some(LogEvent::JoinedWorld {
            world_id,
            world_name: String::new(),
            instance_id,
            location: location.to_string(),
            at,
        });
    }

    if let Some(name) = message.strip_prefix("Entering Room: ") {
        return Some(LogEvent::JoinedWorld {
            world_id: String::new(),
            world_name: name.trim().to_string(),
            instance_id: String::new(),
            location: String::new(),
            at,
        });
    }

    if let Some(rest) = message.strip_prefix("OnPlayerJoined ") {
        let (display_name, user_id) = parse_player(rest);
        if display_name.is_empty() {
            return None;
        }
        return Some(LogEvent::PlayerJoined {
            user_id,
            display_name,
            at,
        });
    }

    if let Some(rest) = message.strip_prefix("OnPlayerLeft ") {
        let (display_name, user_id) = parse_player(rest);
        if display_name.is_empty() {
            return None;
        }
        return Some(LogEvent::PlayerLeft {
            user_id,
            display_name,
            at,
        });
    }

    if let Some(path) = message.strip_prefix("Took screenshot to: ") {
        let path = path.trim();
        if path.is_empty() {
            return None;
        }
        return Some(LogEvent::PhotoTaken {
            path: path.to_string(),
            at,
        });
    }

    None
}

/// Split `YYYY.MM.DD HH:MM:SS[.fff] <level> - <record>` into the parsed
/// local timestamp and the record text after the `-` separator.
fn split_record(line: &str) -> Option<(PrimitiveDateTime, &str)> {
    let bytes = line.as_bytes();
    // Minimum shape: `2024.05.14 20:45:00` = 19 bytes.
    if bytes.len() < 19
        || bytes[4] != b'.'
        || bytes[7] != b'.'
        || bytes[10] != b' '
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return None;
    }
    let digits = |range: std::ops::Range<usize>| bytes[range].iter().all(u8::is_ascii_digit);
    if !(digits(0..4)
        && digits(5..7)
        && digits(8..10)
        && digits(11..13)
        && digits(14..16)
        && digits(17..19))
    {
        return None;
    }
    let year: i32 = line[0..4].parse().ok()?;
    let month = u8::try_from(line[5..7].parse::<u32>().ok()?).ok()?;
    let day: u8 = line[8..10].parse().ok()?;
    let hour: u8 = line[11..13].parse().ok()?;
    let minute: u8 = line[14..16].parse().ok()?;
    let second: u8 = line[17..19].parse().ok()?;

    let date = Date::from_calendar_date(year, Month::try_from(month).ok()?, day).ok()?;
    let time = Time::from_hms(hour, minute, second).ok()?;
    let stamp = PrimitiveDateTime::new(date, time);

    // Optional fractional seconds, then required whitespace.
    let mut pos = 19;
    if bytes.get(pos) == Some(&b'.') {
        pos += 1;
        let start = pos;
        while bytes.get(pos).is_some_and(u8::is_ascii_digit) {
            pos += 1;
        }
        if pos == start {
            return None;
        }
    }
    if !bytes.get(pos).is_some_and(u8::is_ascii_whitespace) {
        return None;
    }
    let rest = line[pos..].trim_start();

    // `<level> - <record>`: level is a single token, then `-`, then the
    // (optionally `[Component]`-prefixed) record text.
    let level_end = rest.find(char::is_whitespace)?;
    let after_level = rest[level_end..].trim_start();
    let record = after_level.strip_prefix('-')?.trim_start();
    if record.is_empty() {
        return None;
    }
    Some((stamp, record))
}

/// Strip the `[Component]` prefix, returning the bare message text.
fn record_message(record: &str) -> Option<&str> {
    if let Some(inner) = record.strip_prefix('[') {
        let end = inner.find(']')?;
        let message = inner[end + 1..].trim_start();
        return (!message.is_empty()).then_some(message);
    }
    (!record.is_empty()).then_some(record)
}

/// `Name` or `Name (usr_…)` → `(display_name, user_id)`.
fn parse_player(rest: &str) -> (String, String) {
    let rest = rest.trim();
    if let Some(idx) = rest.rfind(" (usr_") {
        if rest.ends_with(')') {
            let id = &rest[idx + 2..rest.len() - 1];
            if id.starts_with("usr_") && !id.contains(char::is_whitespace) {
                return (rest[..idx].to_string(), id.to_string());
            }
        }
    }
    (rest.to_string(), String::new())
}

/// The system's local UTC offset for interpreting log timestamps; UTC when
/// it cannot be determined (the offset only shifts absolute instants —
/// relative ordering inside one log file is unaffected).
fn local_offset() -> UtcOffset {
    UtcOffset::current_local_offset().unwrap_or(UtcOffset::UTC)
}

#[cfg(test)]
mod tests {
    use super::*;
    use time::Month;

    #[test]
    fn parses_joined_world_location() {
        let line = "2024.05.14 20:45:01 Log        -  [Behaviour] Joining wrld_00000000-0000-0000-0000-000000000001:12345~hidden(usr_00000000-0000-0000-0000-000000000002)~region(us)~nonce(ABCDEF)";
        let Some(LogEvent::JoinedWorld {
            world_id,
            world_name,
            instance_id,
            location,
            at,
        }) = parse_line(line)
        else {
            panic!("expected JoinedWorld, got {line:?}");
        };
        assert_eq!(world_id, "wrld_00000000-0000-0000-0000-000000000001");
        assert!(world_name.is_empty());
        assert_eq!(
            instance_id,
            "12345~hidden(usr_00000000-0000-0000-0000-000000000002)~region(us)~nonce(ABCDEF)"
        );
        assert_eq!(location, "wrld_00000000-0000-0000-0000-000000000001:12345~hidden(usr_00000000-0000-0000-0000-000000000002)~region(us)~nonce(ABCDEF)");
        assert_eq!(
            (
                at.year(),
                at.month() as u8,
                at.day(),
                at.hour(),
                at.minute(),
                at.second()
            ),
            (2024, 5, 14, 20, 45, 1)
        );
    }

    #[test]
    fn parses_joined_world_name_and_bare_wrld() {
        let name =
            parse_line("2024.05.14 20:45:00 Log - [Behaviour] Entering Room: Test World Name");
        let Some(LogEvent::JoinedWorld { world_name, .. }) = name else {
            panic!("expected JoinedWorld for Entering Room");
        };
        assert_eq!(world_name, "Test World Name");

        let bare = parse_line("2021.06.23 12:02:56 Log - [RoomManager] Joining wrld_4432ea9b-729c-46e3-8eaf-846aa0a37fdd");
        let Some(LogEvent::JoinedWorld {
            world_id,
            instance_id,
            location,
            ..
        }) = bare
        else {
            panic!("expected JoinedWorld for bare wrld");
        };
        assert_eq!(world_id, "wrld_4432ea9b-729c-46e3-8eaf-846aa0a37fdd");
        assert!(instance_id.is_empty());
        assert_eq!(location, "wrld_4432ea9b-729c-46e3-8eaf-846aa0a37fdd");
    }

    #[test]
    fn parses_player_join_and_left_with_and_without_user_id() {
        let joined = parse_line(
            "2026.08.31 18:35:59 Debug - [Behaviour] OnPlayerJoined Test User (usr_00000000-0000-0000-0000-000000000003)",
        );
        let Some(LogEvent::PlayerJoined {
            user_id,
            display_name,
            ..
        }) = joined
        else {
            panic!("expected PlayerJoined");
        };
        assert_eq!(user_id, "usr_00000000-0000-0000-0000-000000000003");
        assert_eq!(display_name, "Test User");

        // Pre-2024.4.1 logs carry no user id.
        let left = parse_line("2022.12.15 22:27:23 Log - [Behaviour] OnPlayerLeft vrcbot");
        let Some(LogEvent::PlayerLeft {
            user_id,
            display_name,
            ..
        }) = left
        else {
            panic!("expected PlayerLeft");
        };
        assert!(user_id.is_empty());
        assert_eq!(display_name, "vrcbot");
    }

    #[test]
    fn parses_left_world_and_photo_taken() {
        assert!(matches!(
            parse_line("2023.12.13 14:19:54 Log - [Behaviour] OnPlayerLeftRoom"),
            Some(LogEvent::LeftWorld { .. })
        ));
        let photo = parse_line(
            "2023.02.08 12:31:35 Log - [VRC Camera] Took screenshot to: C:\\Users\\test\\Pictures\\VRChat\\VRChat_2023-02-08_12-31-35.104_1920x1080.png",
        );
        let Some(LogEvent::PhotoTaken { path, .. }) = photo else {
            panic!("expected PhotoTaken");
        };
        assert_eq!(
            path,
            "C:\\Users\\test\\Pictures\\VRChat\\VRChat_2023-02-08_12-31-35.104_1920x1080.png"
        );
    }

    #[test]
    fn ignores_unrelated_and_malformed_lines() {
        for line in [
            // Not a timestamped record at all.
            "",
            "partial line without newline yet",
            "2024.05.14 20:45:01",                     // timestamp only
            "2024.05.14",                              // date only
            "not a log line",
            // Lookalikes that must not parse.
            "2024.05.14 20:45:01 Log - [Behaviour] Joining or Creating Room: Test World",
            "2024.05.14 20:45:01 Log - [Behaviour] Joining friend: Test User",
            "2024.05.14 20:45:01 Log - [Behaviour] OnPlayerJoined:",
            "2024.05.14 20:45:01 Log - [Behaviour] OnPlayerJoinComplete",
            "2024.05.14 20:45:01 Log - [Behaviour] Removed player Test User",
            "2024.05.14 20:45:01 Log - [Behaviour] Entering world",
            "2024.05.14 20:45:01 Log - [Behaviour] Destination set: wrld_00000000-0000-0000-0000-000000000001",
            "2024.05.14 20:45:01 Error - something broke",
            "2024.13.40 99:99:99 Log - [Behaviour] OnPlayerJoined X",
        ] {
            assert_eq!(parse_line(line), None, "should not parse: {line:?}");
        }
    }

    #[test]
    fn timestamp_variants_parse() {
        // Padded level + fractional seconds.
        let event =
            parse_line("2024.05.14 20:45:01.123 Log        -  [Behaviour] OnPlayerLeftRoom");
        assert!(matches!(event, Some(LogEvent::LeftWorld { .. })));
        // CRLF-terminated content is trimmed by the caller; trim_end here too.
        let event = parse_line("2024.05.14 20:45:01 Log - [Behaviour] OnPlayerLeftRoom\r");
        assert!(matches!(event, Some(LogEvent::LeftWorld { .. })));
    }

    #[test]
    fn display_renders_one_line_summary() {
        let event = LogEvent::PlayerJoined {
            user_id: "usr_00000000-0000-0000-0000-000000000003".into(),
            display_name: "Test User".into(),
            at: PrimitiveDateTime::new(
                Date::from_calendar_date(2024, Month::May, 14).unwrap(),
                Time::from_hms(20, 45, 1).unwrap(),
            )
            .assume_offset(UtcOffset::UTC),
        };
        let text = event.to_string();
        assert!(text.contains("2024-05-14 20:45:01"), "{text}");
        assert!(text.contains("player joined"), "{text}");
        assert!(text.contains("Test User"), "{text}");
    }
}
