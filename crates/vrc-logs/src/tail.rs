//! Tailer for VRChat `output_log_*.txt` files.
//!
//! [`LogTailer::watch`] polls a directory for the newest
//! `output_log*.txt`, follows it as VRChat appends, and switches to a newer
//! file when the client rotates logs (one file per launch). Files that
//! already existed when watching started are tailed from their end — only
//! bytes appended afterwards are read; a file that appears *during* the
//! watch (a fresh client launch) is read from the start so the new
//! session's early lines are not lost. Reads are strictly read-only
//! (RULES.md §5) and tolerant of missing directories, partial lines and
//! mid-write truncation.

use std::collections::HashSet;
use std::fmt;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime};

use futures_util::Stream;
use tokio::sync::mpsc;
use tokio::time::interval;

use crate::parse_line;
use crate::LogEvent;

/// How often the log directory is re-scanned for appends/newest file.
const POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Starts log-tail streams. Stateless — every call to [`LogTailer::watch`]
/// is independent.
pub struct LogTailer;

impl LogTailer {
    /// Follow the newest `output_log*.txt` under `dir`, yielding parsed
    /// [`LogEvent`]s until the stream is dropped.
    ///
    /// Must be called from within a Tokio runtime (like
    /// [`vrc_pipeline::Pipeline::connect`]). A missing directory or absent
    /// log files are not errors: the stream simply yields nothing until a
    /// matching file appears.
    pub fn watch(dir: PathBuf) -> LogEvents {
        Self::watch_with_poll(dir, POLL_INTERVAL)
    }

    /// [`LogTailer::watch`] with a custom poll interval (tests).
    #[doc(hidden)]
    pub fn watch_with_poll(dir: PathBuf, poll: Duration) -> LogEvents {
        let (tx, rx) = mpsc::channel(256);
        tokio::spawn(async move { tail_loop(dir, poll, tx).await });
        LogEvents { rx }
    }
}

/// Stream of parsed [`LogEvent`]s produced by [`LogTailer::watch`].
///
/// Implements [`Stream`]; [`LogEvents::recv`] is a small convenience that
/// avoids needing `StreamExt` for the common "await next event" loop.
pub struct LogEvents {
    rx: mpsc::Receiver<LogEvent>,
}

impl LogEvents {
    /// Await the next event; `None` once the tailer has stopped for good
    /// (only happens if the receiver is dropped).
    pub async fn recv(&mut self) -> Option<LogEvent> {
        self.rx.recv().await
    }
}

impl Stream for LogEvents {
    type Item = LogEvent;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<LogEvent>> {
        self.rx.poll_recv(cx)
    }
}

impl fmt::Debug for LogEvents {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("LogEvents(..)")
    }
}

/// The file currently being tailed.
struct ActiveFile {
    path: PathBuf,
    /// Byte offset up to which the file has been consumed.
    offset: u64,
    /// Undelivered partial line (no `\n` seen yet).
    pending: Vec<u8>,
}

#[derive(Default)]
struct TailState {
    current: Option<ActiveFile>,
    /// Log files present when the watch first scanned the directory. These
    /// are tailed from their end; files appearing later are read fully.
    initial_files: Option<HashSet<PathBuf>>,
}

async fn tail_loop(dir: PathBuf, poll: Duration, tx: mpsc::Sender<LogEvent>) {
    let mut state = TailState::default();
    let mut ticker = interval(poll);
    loop {
        ticker.tick().await;
        if tx.is_closed() {
            return;
        }
        if !poll_once(&dir, &mut state, &tx).await {
            return;
        }
    }
}

/// One scan+read pass. Returns `false` when the receiver is gone.
async fn poll_once(dir: &Path, state: &mut TailState, tx: &mpsc::Sender<LogEvent>) -> bool {
    // Snapshot the directory once so files that appear later are
    // distinguishable from files that were already there.
    if state.initial_files.is_none() {
        state.initial_files = Some(log_files(dir));
    }
    let Some((path, _)) = newest_log(dir) else {
        return true;
    };
    if state.current.as_ref().map(|c| &c.path) != Some(&path) {
        let pre_existing = state
            .initial_files
            .as_ref()
            .is_some_and(|files| files.contains(&path));
        state.current = match open_at(&path, pre_existing) {
            Ok(file) => Some(file),
            Err(e) => {
                tracing::debug!("vrc-logs: cannot open {}: {e}", path.display());
                None
            }
        };
    }
    let Some(current) = state.current.as_mut() else {
        return true;
    };
    read_appends(current, tx).await
}

/// All `output_log*.txt` files directly inside `dir`.
fn log_files(dir: &Path) -> HashSet<PathBuf> {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| is_log_file(p))
                .collect()
        })
        .unwrap_or_default()
}

fn is_log_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("output_log") && n.ends_with(".txt"))
}

/// The newest log file by modification time (ties → lexicographic name).
fn newest_log(dir: &Path) -> Option<(PathBuf, SystemTime)> {
    std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .filter(|e| is_log_file(&e.path()))
        .filter_map(|e| {
            let modified = e.metadata().ok()?.modified().ok()?;
            Some((e.path(), modified))
        })
        .max_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)))
}

/// Modification time of the newest `output_log*.txt` in `dir`, if any.
///
/// The file's last write is the best available "VRChat was last alive"
/// timestamp — callers use it to bound orphaned-session cleanup instead
/// of the (much later) app-start time.
pub fn newest_log_mtime(dir: &Path) -> Option<SystemTime> {
    newest_log(dir).map(|(_, modified)| modified)
}

/// Open `path` for tailing. Pre-existing files start at their end (only
/// appended bytes are read); new files start at 0 so their whole content —
/// written between creation and our next poll — is not lost.
fn open_at(path: &Path, pre_existing: bool) -> std::io::Result<ActiveFile> {
    let file = File::open(path)?;
    let offset = if pre_existing {
        file.metadata()?.len()
    } else {
        0
    };
    Ok(ActiveFile {
        path: path.to_path_buf(),
        offset,
        pending: Vec::new(),
    })
}

/// Read bytes appended since `offset`, emit every complete parsed line.
/// Returns `false` when the event receiver is gone.
async fn read_appends(current: &mut ActiveFile, tx: &mpsc::Sender<LogEvent>) -> bool {
    let mut file = match File::open(&current.path) {
        Ok(file) => file,
        Err(e) => {
            tracing::debug!("vrc-logs: lost {}: {e}", current.path.display());
            return true;
        }
    };
    let len = file.metadata().map(|m| m.len()).unwrap_or(0);
    if len < current.offset {
        // Truncated/rotated in place: restart from the top.
        current.offset = 0;
        current.pending.clear();
    }
    if file.seek(SeekFrom::Start(current.offset)).is_err() {
        return true;
    }
    let mut buf = Vec::new();
    if file.read_to_end(&mut buf).is_err() {
        return true;
    }
    if buf.is_empty() {
        return true;
    }
    current.offset += buf.len() as u64;
    current.pending.extend_from_slice(&buf);

    let mut start = 0;
    for (i, &b) in current.pending.iter().enumerate() {
        if b != b'\n' {
            continue;
        }
        // UTF-8 is self-synchronising: a `\n` byte never appears inside a
        // multi-byte sequence, so complete lines are safe to lossy-decode.
        let line = String::from_utf8_lossy(&current.pending[start..i]);
        start = i + 1;
        if let Some(event) = parse_line(line.trim_end()) {
            if tx.send(event).await.is_err() {
                return false;
            }
        }
    }
    current.pending.drain(..start);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::time::Duration;

    const POLL: Duration = Duration::from_millis(50);

    fn log_name(n: u32) -> String {
        format!("output_log_2024-05-14_20-45-{n:02}.txt")
    }

    fn join_line(world: &str) -> String {
        format!("2024.05.14 20:45:01 Log - [Behaviour] Joining {world}\n")
    }

    async fn recv_within(events: &mut LogEvents, wait: Duration) -> Option<LogEvent> {
        tokio::time::timeout(wait, events.recv())
            .await
            .ok()
            .flatten()
    }

    #[tokio::test]
    async fn tails_appends_and_ignores_partial_lines() {
        let dir = tempfile::tempdir().unwrap();
        let log_path = dir.path().join(log_name(1));
        // Pre-existing content: only appended bytes may be read.
        std::fs::write(&log_path, join_line("wrld_old:1")).unwrap();

        let mut events = LogTailer::watch_with_poll(dir.path().to_path_buf(), POLL);
        // Give the first poll a moment, then append new lines.
        tokio::time::sleep(POLL * 3).await;
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&log_path)
            .unwrap();
        // A partial line must not be emitted until completed.
        file.write_all(b"2024.05.14 20:45:02 Log - [Behaviour] OnPlayerJoined Partial")
            .unwrap();
        file.flush().unwrap();
        tokio::time::sleep(POLL * 3).await;
        assert!(recv_within(&mut events, Duration::from_millis(50))
            .await
            .is_none());

        file.write_all(b" User (usr_00000000-0000-0000-0000-000000000003)\n")
            .unwrap();
        file.write_all(b"2024.05.14 20:45:03 Log - [Behaviour] OnLeftRoom\n")
            .unwrap();
        file.flush().unwrap();

        let first = recv_within(&mut events, Duration::from_secs(2)).await;
        assert!(matches!(first, Some(LogEvent::PlayerJoined { .. })));
        let second = recv_within(&mut events, Duration::from_secs(2)).await;
        assert!(matches!(second, Some(LogEvent::LeftWorld { .. })));
        // The pre-existing line was never emitted.
        assert!(recv_within(&mut events, Duration::from_millis(100))
            .await
            .is_none());
    }

    #[tokio::test]
    async fn switches_to_newer_log_file() {
        let dir = tempfile::tempdir().unwrap();
        let old_path = dir.path().join(log_name(1));
        std::fs::write(&old_path, join_line("wrld_old:1")).unwrap();

        let mut events = LogTailer::watch_with_poll(dir.path().to_path_buf(), POLL);
        tokio::time::sleep(POLL * 3).await;

        // A new session log appears later: it is read from the start.
        let new_path = dir.path().join(log_name(2));
        std::fs::write(
            &new_path,
            "2024.05.14 21:00:00 Log - [Behaviour] Entering Room: Test World\n",
        )
        .unwrap();
        let event = recv_within(&mut events, Duration::from_secs(2)).await;
        assert!(matches!(
            event,
            Some(LogEvent::JoinedWorld { ref world_name, .. }) if world_name == "Test World"
        ));
    }

    #[tokio::test]
    async fn missing_dir_and_absent_files_yield_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        let mut events = LogTailer::watch_with_poll(missing, POLL);
        assert!(recv_within(&mut events, Duration::from_millis(150))
            .await
            .is_none());

        // Directory exists but has no log files yet; creating one later
        // (a fresh session) is picked up from the start.
        let mut events = LogTailer::watch_with_poll(dir.path().to_path_buf(), POLL);
        tokio::time::sleep(POLL * 3).await;
        std::fs::write(
            dir.path().join(log_name(1)),
            "2024.05.14 22:00:00 Log - [VRC Camera] Took screenshot to: C:\\fake\\pic.png\n",
        )
        .unwrap();
        let event = recv_within(&mut events, Duration::from_secs(2)).await;
        assert!(matches!(event, Some(LogEvent::PhotoTaken { .. })));
    }

    #[test]
    fn log_file_name_matching() {
        assert!(is_log_file(Path::new("output_log_2024-01-01_09-58-09.txt")));
        assert!(is_log_file(Path::new("output_log.txt")));
        assert!(!is_log_file(Path::new("output_log_2024.txt.bak")));
        assert!(!is_log_file(Path::new("config.json")));
        assert!(!is_log_file(Path::new("player.log")));
    }

    #[test]
    fn newest_log_mtime_tracks_latest_write() {
        let dir = tempfile::tempdir().unwrap();
        assert!(newest_log_mtime(dir.path()).is_none());
        let a = dir.path().join("output_log_a.txt");
        let b = dir.path().join("output_log_b.txt");
        std::fs::write(&a, b"x").unwrap();
        std::fs::write(&b, b"x").unwrap();
        // Non-log files are ignored.
        std::fs::write(dir.path().join("notes.txt"), b"x").unwrap();
        let mtime = newest_log_mtime(dir.path()).expect("a log file exists");
        for file in [&a, &b] {
            let file_mtime = std::fs::metadata(file).unwrap().modified().unwrap();
            assert!(mtime >= file_mtime);
        }
    }
}
