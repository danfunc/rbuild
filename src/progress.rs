//! Machine-readable progress lines (`--progress=json`) for editor integrations.
//!
//! Each line is a single JSON object on stderr tagged with `"event":"rbuild.progress"` or
//! `"event":"rbuild.job"` so consumers can tell it apart from forwarded build output.

use serde::Serialize;
use std::io::Write;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Minimum interval between two non-forced progress lines.
pub const MIN_INTERVAL: Duration = Duration::from_millis(100);

/// One progress observation. `total_*` is `None` when the total is not known up front.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Progress {
    pub event: &'static str,
    pub phase: &'static str,
    pub state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub done_files: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_files: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub done_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_bytes: Option<u64>,
}

impl Progress {
    /// Marks the start or end of a phase (`state` is `"start"` or `"done"`).
    pub fn phase(phase: &'static str, state: &'static str) -> Self {
        Self {
            event: "rbuild.progress",
            phase,
            state,
            done_files: None,
            total_files: None,
            done_bytes: None,
            total_bytes: None,
        }
    }

    /// A transfer observation; `state` is `"progress"` or `"done"`.
    pub fn transfer(
        phase: &'static str,
        state: &'static str,
        done_files: u64,
        total_files: Option<u64>,
        done_bytes: u64,
        total_bytes: Option<u64>,
    ) -> Self {
        Self {
            done_files: Some(done_files),
            total_files,
            done_bytes: Some(done_bytes),
            total_bytes,
            ..Self::phase(phase, state)
        }
    }

    pub fn to_line(&self) -> String {
        serde_json::to_string(self).expect("progress serializes")
    }
}

/// Start of one job in a (possibly multi-stage) `rbuild job` run.
pub fn job_line(name: &str, index: usize, total: usize) -> String {
    serde_json::json!({"event": "rbuild.job", "name": name, "index": index, "total": total})
        .to_string()
}

/// Throttled line writer: non-forced `progress` states are dropped inside [`MIN_INTERVAL`].
pub struct Reporter<W: Write> {
    out: W,
    last: Option<Instant>,
}

impl<W: Write> Reporter<W> {
    pub fn new(out: W) -> Self {
        Self { out, last: None }
    }

    /// Writes `progress` at `now`; returns whether a line was written. Anything other than
    /// the `"progress"` state (start/done) is never dropped.
    pub fn report(&mut self, now: Instant, progress: &Progress) -> bool {
        let droppable = progress.state == "progress";
        if droppable
            && self
                .last
                .is_some_and(|last| now.saturating_duration_since(last) < MIN_INTERVAL)
        {
            return false;
        }
        self.last = Some(now);
        let _ = writeln!(self.out, "{}", progress.to_line());
        let _ = self.out.flush();
        true
    }

    pub fn raw_line(&mut self, line: &str) {
        let _ = writeln!(self.out, "{line}");
        let _ = self.out.flush();
    }
}

static REPORTER: OnceLock<Mutex<Reporter<std::io::Stderr>>> = OnceLock::new();

/// Enables JSON progress output on stderr for this process.
pub fn enable() {
    let _ = REPORTER.set(Mutex::new(Reporter::new(std::io::stderr())));
}

pub fn enabled() -> bool {
    REPORTER.get().is_some()
}

pub fn emit(progress: &Progress) {
    if let Some(Ok(mut reporter)) = REPORTER.get().map(Mutex::lock) {
        reporter.report(Instant::now(), progress);
    }
}

pub fn emit_job(name: &str, index: usize, total: usize) {
    if let Some(Ok(mut reporter)) = REPORTER.get().map(Mutex::lock) {
        reporter.raw_line(&job_line(name, index, total));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transfer_line_is_single_line_json_with_counts() {
        let line = Progress::transfer("sync", "progress", 3, Some(10), 300, Some(1000)).to_line();
        assert_eq!(
            line,
            r#"{"event":"rbuild.progress","phase":"sync","state":"progress","done_files":3,"total_files":10,"done_bytes":300,"total_bytes":1000}"#
        );
        assert!(!line.contains('\n'));
    }

    #[test]
    fn unknown_totals_are_omitted() {
        let line = Progress::transfer("collect", "progress", 2, None, 50, None).to_line();
        assert!(!line.contains("total_"));
        assert!(line.contains(r#""done_files":2"#));
    }

    #[test]
    fn progress_is_throttled_but_start_and_done_are_not() {
        let mut reporter = Reporter::new(Vec::new());
        let t0 = Instant::now();
        let p = |n| Progress::transfer("sync", "progress", n, Some(9), n, Some(9));
        assert!(reporter.report(t0, &Progress::phase("sync", "start")));
        assert!(!reporter.report(t0 + Duration::from_millis(10), &p(1)));
        assert!(reporter.report(t0 + Duration::from_millis(100), &p(2)));
        assert!(!reporter.report(t0 + Duration::from_millis(150), &p(3)));
        assert!(reporter.report(t0 + Duration::from_millis(200), &p(4)));
        let done = Progress::transfer("sync", "done", 9, Some(9), 9, Some(9));
        assert!(reporter.report(t0 + Duration::from_millis(201), &done));
        let text = String::from_utf8(reporter.out).unwrap();
        assert_eq!(text.lines().count(), 4); // start, p(2), p(4), done
        assert!(text.lines().last().unwrap().contains(r#""state":"done""#));
    }

    #[test]
    fn job_line_carries_position() {
        assert_eq!(
            job_line("uf2", 1, 3),
            r#"{"event":"rbuild.job","index":1,"name":"uf2","total":3}"#
        );
    }
}
