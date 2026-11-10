//! Surface Scan engine: sequential read test of the drive surface (read-only).
//!
//! Walks the requested range in blocks (default 1 MiB), timing every read. When a block
//! fails, it bisects it (halves, then halves of halves...) down to the physical sector
//! size to isolate the unreadable sectors, with a limited re-read budget so a dying drive
//! is not punished. If re-reading the halves succeeds, the error is classified as
//! **transient** — typical of a communication problem (cable/controller), not of the media.
//!
//! The engine knows nothing about Windows: it takes any [`BlockReader`]. Tests use
//! [`crate::io::testing::ScriptedReader`].

use crate::i18n::t;
use crate::inventory::DiskIdentity;
use crate::io::{BlockReader, ErrorClass, IoOutcome, classify_error, describe_error, win32};
use crate::stats::{LatencyRecorder, LatencySummary};
use crate::util::align_down;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScanConfig {
    pub start: u64,
    pub end: Option<u64>,
    pub block_size: u32,
    pub io_timeout_ms: u64,
    pub slow_threshold_ms: u64,
    pub max_consecutive_errors: u32,
    pub refine_errors: bool,
    pub max_refine_reads: u32,
    pub max_recorded_issues: usize,
    pub timeline_points: usize,
}

impl Default for ScanConfig {
    fn default() -> Self {
        Self {
            start: 0,
            end: None,
            block_size: 1 << 20,
            io_timeout_ms: 10_000,
            slow_threshold_ms: 500,
            max_consecutive_errors: 128,
            refine_errors: true,
            max_refine_reads: 4096,
            max_recorded_issues: 2000,
            timeline_points: 400,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IssueKind {
    Slow,
    ReadError,
    Timeout,
    Hung,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScanIssue {
    pub kind: IssueKind,
    pub offset: u64,
    pub length: u32,
    pub lba: u64,
    pub latency_ms: f64,
    pub error_code: Option<u32>,
    pub error_class: Option<ErrorClass>,
    /// Error that went away on re-read (communication) instead of a bad sector.
    #[serde(default)]
    pub transient: bool,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BadRange {
    pub offset: u64,
    pub length: u64,
    pub first_lba: u64,
    pub sector_count: u64,
    /// `false` = the re-read budget ran out; the whole range was kept as suspect.
    pub refined: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TimelinePoint {
    pub offset: u64,
    /// Relative position on the drive (0–100%).
    pub percent: f64,
    pub mb_per_s: f64,
    pub max_latency_ms: f64,
    pub errors: u32,
    pub slow: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", content = "reason", rename_all = "snake_case")]
pub enum ScanStatus {
    Completed,
    Cancelled,
    /// Stopped early; the reason is human-readable text in the language of the run
    /// (the machine-readable kind is in [`ScanMetrics::abort`]).
    Aborted(String),
}

impl ScanStatus {
    pub fn label(&self) -> String {
        match self {
            Self::Completed => t("completed", "concluído").into(),
            Self::Cancelled => t("cancelled by the user", "cancelado pelo usuário").into(),
            Self::Aborted(r) => tr!("stopped: {r}", "interrompido: {r}"),
        }
    }
}

/// Why a scan stopped early (stable, language-independent).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AbortKind {
    /// The device stopped responding (not even the cancel completed).
    Hung,
    /// An error that makes going on pointless (device gone, access denied…).
    Fatal,
    /// Too many consecutive failures.
    TooManyErrors,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScanMetrics {
    pub status: ScanStatus,
    /// Why the scan stopped early, if it did. Absent in results saved before 0.3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abort: Option<AbortKind>,
    pub start_offset: u64,
    pub end_offset: u64,
    pub capacity: u64,
    pub bytes_planned: u64,
    pub bytes_processed: u64,
    pub bytes_read_ok: u64,
    pub bytes_unreadable: u64,
    pub block_size: u32,
    pub logical_sector_size: u32,
    pub physical_sector_size: u32,
    pub blocks_total: u64,
    pub blocks_ok: u64,
    pub slow_blocks: u64,
    pub error_blocks: u64,
    pub timeout_blocks: u64,
    pub media_errors: u64,
    pub device_errors: u64,
    pub transient_errors: u64,
    pub refine_reads: u32,
    pub wall_seconds: f64,
    pub avg_mb_per_s: f64,
    pub min_segment_mb_per_s: Option<f64>,
    pub median_segment_mb_per_s: Option<f64>,
    pub max_segment_mb_per_s: Option<f64>,
    pub latency: LatencySummary,
    pub issues: Vec<ScanIssue>,
    pub issues_truncated: bool,
    pub bad_ranges: Vec<BadRange>,
    pub timeline: Vec<TimelinePoint>,
}

impl ScanMetrics {
    /// Why the scan stopped early. Results saved by 0.2.x carry only the (pt-BR) text.
    pub fn abort_kind(&self) -> Option<AbortKind> {
        if self.abort.is_some() {
            return self.abort;
        }
        match &self.status {
            ScanStatus::Aborted(r) if r.contains("parou de responder") => Some(AbortKind::Hung),
            ScanStatus::Aborted(r) if r.contains("fatal") => Some(AbortKind::Fatal),
            ScanStatus::Aborted(r) if r.contains("consecutivas") => Some(AbortKind::TooManyErrors),
            _ => None,
        }
    }
}

/// Saved result of a scan (what goes to JSON/the report).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScanResult {
    pub tool_version: String,
    pub disk: DiskIdentity,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub elevated: bool,
    pub config: ScanConfig,
    pub metrics: ScanMetrics,
}

#[derive(Debug, Clone, Default)]
pub struct LiveStats {
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub slow: u64,
    pub errors: u64,
    pub timeouts: u64,
}

/// Receives notifications during the scan (progress bar in the CLI).
pub trait ScanObserver {
    fn on_progress(&mut self, _stats: &LiveStats) {}
    fn on_issue(&mut self, _issue: &ScanIssue) {}
}

pub struct NoopObserver;
impl ScanObserver for NoopObserver {}

fn hung_reason() -> String {
    tr!(
        "the device stopped responding (not even the read cancel completed). \
         This points to a hang of the drive, the controller or the link",
        "o dispositivo parou de responder (nem o cancelamento da leitura foi concluído). \
         Isso indica travamento do disco, da controladora ou do link"
    )
}

#[derive(Debug, Clone, Default)]
struct Segment {
    bytes: u64,
    device_us: u64,
    max_latency_us: u64,
    errors: u32,
    slow: u32,
    touched: bool,
}

fn push_issue(m: &mut ScanMetrics, max: usize, issue: ScanIssue, observer: &mut dyn ScanObserver) {
    if issue.kind != IssueKind::Slow {
        observer.on_issue(&issue);
    }
    if m.issues.len() < max {
        m.issues.push(issue);
    } else {
        m.issues_truncated = true;
    }
}

/// Bisects a failed block. Returns `true` if the device hung.
#[allow(clippy::too_many_arguments)]
fn refine<R: BlockReader + ?Sized>(
    reader: &mut R,
    offset: u64,
    len: u64,
    unit: u64,
    timeout: Duration,
    budget: &mut u32,
    used: &mut u32,
    cancel: &AtomicBool,
    bad: &mut Vec<(u64, u64, bool)>,
) -> bool {
    if len <= unit {
        bad.push((offset, len, true));
        return false;
    }
    let half = ((len / 2) / unit).max(1) * unit;
    for (o, l) in [(offset, half), (offset + half, len - half)] {
        if l == 0 {
            continue;
        }
        if *budget == 0 || cancel.load(Ordering::Relaxed) {
            bad.push((o, l, false));
            continue;
        }
        *budget -= 1;
        *used += 1;
        match reader.read_at(o, l as u32, timeout) {
            IoOutcome::Completed { bytes, .. } if bytes as u64 == l => {}
            IoOutcome::Hung { .. } => {
                bad.push((o, l, false));
                return true;
            }
            IoOutcome::Failed { code, .. } if classify_error(code) == ErrorClass::Fatal => {
                bad.push((o, l, false));
                *budget = 0;
            }
            _ => {
                if refine(reader, o, l, unit, timeout, budget, used, cancel, bad) {
                    return true;
                }
            }
        }
    }
    false
}

fn merge_ranges(mut v: Vec<(u64, u64, bool)>, logical: u64) -> Vec<BadRange> {
    v.sort_by_key(|r| r.0);
    let mut out: Vec<BadRange> = Vec::new();
    for (offset, length, refined) in v {
        if let Some(last) = out.last_mut() {
            if last.refined == refined && last.offset + last.length == offset {
                last.length += length;
                last.sector_count = last.length / logical;
                continue;
            }
        }
        out.push(BadRange { offset, length, first_lba: offset / logical, sector_count: length / logical, refined });
    }
    out
}

/// Runs the scan. Never writes: it only uses [`BlockReader::read_at`].
pub fn run_scan<R: BlockReader + ?Sized>(reader: &mut R, cfg: &ScanConfig, observer: &mut dyn ScanObserver, cancel: &AtomicBool) -> ScanMetrics {
    let wall = Instant::now();
    let logical = reader.logical_sector_size().max(512) as u64;
    let physical = (reader.physical_sector_size() as u64).max(logical);
    let capacity = align_down(reader.capacity(), logical);
    let start = align_down(cfg.start.min(capacity), logical);
    let end = align_down(cfg.end.unwrap_or(capacity).min(capacity), logical).max(start);
    let max_io = (reader.max_io_size() as u64).max(logical);
    let block = align_down((cfg.block_size as u64).min(max_io), logical).max(logical);
    let timeout = Duration::from_millis(cfg.io_timeout_ms.max(1));
    let slow = Duration::from_millis(cfg.slow_threshold_ms.max(1));
    let total = end - start;
    let points = cfg.timeline_points.max(1) as u64;

    let mut segments = vec![Segment::default(); points as usize];
    let mut latency = LatencyRecorder::new();
    let mut bad: Vec<(u64, u64, bool)> = Vec::new();
    let mut consecutive = 0u32;
    let mut budget = cfg.max_refine_reads;
    let mut status = ScanStatus::Completed;
    let mut abort = None;
    let mut m = ScanMetrics {
        status: ScanStatus::Completed,
        abort: None,
        start_offset: start,
        end_offset: end,
        capacity,
        bytes_planned: total,
        bytes_processed: 0,
        bytes_read_ok: 0,
        bytes_unreadable: 0,
        block_size: block as u32,
        logical_sector_size: logical as u32,
        physical_sector_size: physical as u32,
        blocks_total: 0,
        blocks_ok: 0,
        slow_blocks: 0,
        error_blocks: 0,
        timeout_blocks: 0,
        media_errors: 0,
        device_errors: 0,
        transient_errors: 0,
        refine_reads: 0,
        wall_seconds: 0.0,
        avg_mb_per_s: 0.0,
        min_segment_mb_per_s: None,
        median_segment_mb_per_s: None,
        max_segment_mb_per_s: None,
        latency: LatencySummary::default(),
        issues: vec![],
        issues_truncated: false,
        bad_ranges: vec![],
        timeline: vec![],
    };
    let mut live = LiveStats { bytes_total: total, ..Default::default() };

    let mut offset = start;
    while offset < end {
        if cancel.load(Ordering::Relaxed) {
            status = ScanStatus::Cancelled;
            break;
        }
        let len = (end - offset).min(block);
        let seg_idx = ((((offset - start) as u128) * points as u128 / total.max(1) as u128) as u64).min(points - 1) as usize;
        let outcome = reader.read_at(offset, len as u32, timeout);
        let lat = outcome.latency();
        let lat_us = lat.as_micros().min(u64::MAX as u128) as u64;
        m.blocks_total += 1;
        {
            let seg = &mut segments[seg_idx];
            seg.touched = true;
            seg.max_latency_us = seg.max_latency_us.max(lat_us);
        }
        let issue = |kind, code: Option<u32>, description: String| ScanIssue {
            kind,
            offset,
            length: len as u32,
            lba: offset / logical,
            latency_ms: lat_us as f64 / 1000.0,
            error_code: code,
            error_class: code.map(classify_error),
            transient: false,
            description,
        };

        let mut stop = false;
        match outcome {
            IoOutcome::Completed { bytes, .. } if bytes as u64 == len => {
                consecutive = 0;
                m.blocks_ok += 1;
                m.bytes_read_ok += len;
                latency.record(lat);
                let seg = &mut segments[seg_idx];
                seg.bytes += len;
                seg.device_us += lat_us;
                if lat >= slow {
                    m.slow_blocks += 1;
                    seg.slow += 1;
                    push_issue(&mut m, cfg.max_recorded_issues, issue(IssueKind::Slow, None, t("slow read", "leitura lenta").into()), observer);
                }
            }
            IoOutcome::Completed { .. } | IoOutcome::Failed { .. } => {
                let code = match outcome {
                    IoOutcome::Failed { code, .. } => code,
                    _ => win32::ERROR_HANDLE_EOF,
                };
                let class = classify_error(code);
                segments[seg_idx].errors += 1;
                if class == ErrorClass::Fatal {
                    m.error_blocks += 1;
                    push_issue(&mut m, cfg.max_recorded_issues, issue(IssueKind::ReadError, Some(code), describe_error(code)), observer);
                    status = ScanStatus::Aborted(tr!("fatal I/O error: {}", "erro fatal de E/S: {}", describe_error(code)));
                    abort = Some(AbortKind::Fatal);
                    break;
                }
                consecutive += 1;
                if class == ErrorClass::Timeout {
                    m.timeout_blocks += 1;
                    push_issue(&mut m, cfg.max_recorded_issues, issue(IssueKind::Timeout, Some(code), describe_error(code)), observer);
                } else {
                    m.error_blocks += 1;
                    if class == ErrorClass::Media {
                        m.media_errors += 1;
                    } else {
                        m.device_errors += 1;
                    }
                    let mut entry = issue(IssueKind::ReadError, Some(code), describe_error(code));
                    if cfg.refine_errors {
                        let before = bad.len();
                        let hung = refine(reader, offset, len, physical, timeout, &mut budget, &mut m.refine_reads, cancel, &mut bad);
                        if hung {
                            push_issue(&mut m, cfg.max_recorded_issues, entry, observer);
                            status = ScanStatus::Aborted(hung_reason());
                            abort = Some(AbortKind::Hung);
                            offset += len;
                            break;
                        }
                        if bad.len() == before {
                            m.transient_errors += 1;
                            entry.transient = true;
                            entry.description.push_str(t(" — re-read succeeded (transient error)", " — releitura bem-sucedida (erro transitório)"));
                        }
                    } else {
                        bad.push((offset, len, false));
                    }
                    push_issue(&mut m, cfg.max_recorded_issues, entry, observer);
                }
                stop = consecutive >= cfg.max_consecutive_errors;
            }
            IoOutcome::TimedOut { .. } => {
                consecutive += 1;
                m.timeout_blocks += 1;
                segments[seg_idx].errors += 1;
                let text = tr!(
                    "read did not complete within {} ms and was cancelled",
                    "leitura não concluiu em {} ms e foi cancelada",
                    cfg.io_timeout_ms
                );
                push_issue(&mut m, cfg.max_recorded_issues, issue(IssueKind::Timeout, None, text), observer);
                stop = consecutive >= cfg.max_consecutive_errors;
            }
            IoOutcome::Hung { .. } => {
                m.timeout_blocks += 1;
                segments[seg_idx].errors += 1;
                push_issue(&mut m, cfg.max_recorded_issues, issue(IssueKind::Hung, None, hung_reason()), observer);
                status = ScanStatus::Aborted(hung_reason());
                abort = Some(AbortKind::Hung);
                offset += len;
                break;
            }
        }
        offset += len;
        live.bytes_done = offset - start;
        live.slow = m.slow_blocks;
        live.errors = m.error_blocks;
        live.timeouts = m.timeout_blocks;
        observer.on_progress(&live);
        if stop {
            status = ScanStatus::Aborted(tr!(
                "{consecutive} consecutive failures; scan stopped to avoid stressing the drive (adjust with --max-consecutive-errors)",
                "{consecutive} falhas consecutivas; teste interrompido para não forçar o disco (ajuste com --max-consecutive-errors)"
            ));
            abort = Some(AbortKind::TooManyErrors);
            break;
        }
    }

    m.status = status;
    m.abort = abort;
    m.bytes_processed = offset.min(end) - start;
    m.bad_ranges = merge_ranges(bad, logical);
    m.bytes_unreadable = m.bad_ranges.iter().map(|r| r.length).sum();
    m.latency = latency.summary();
    m.wall_seconds = wall.elapsed().as_secs_f64();
    if m.wall_seconds > 0.0 {
        m.avg_mb_per_s = m.bytes_read_ok as f64 / m.wall_seconds / 1e6;
    }

    let mut rates = Vec::new();
    for (i, seg) in segments.iter().enumerate() {
        if !seg.touched {
            continue;
        }
        let seg_offset = start + ((i as u128 * total as u128) / points as u128) as u64;
        let mbps = if seg.device_us > 0 { seg.bytes as f64 / seg.device_us as f64 } else { 0.0 };
        if seg.bytes >= block && seg.device_us > 0 {
            rates.push(mbps);
        }
        m.timeline.push(TimelinePoint {
            offset: seg_offset,
            percent: if capacity > 0 { seg_offset as f64 * 100.0 / capacity as f64 } else { 0.0 },
            mb_per_s: mbps,
            max_latency_ms: seg.max_latency_us as f64 / 1000.0,
            errors: seg.errors,
            slow: seg.slow,
        });
    }
    if !rates.is_empty() {
        rates.sort_by(|a, b| a.total_cmp(b));
        m.min_segment_mb_per_s = rates.first().copied();
        m.max_segment_mb_per_s = rates.last().copied();
        m.median_segment_mb_per_s = Some(rates[rates.len() / 2]);
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::testing::ScriptedReader;

    const MIB: u64 = 1 << 20;

    fn run(reader: &mut ScriptedReader, cfg: &ScanConfig) -> ScanMetrics {
        run_scan(reader, cfg, &mut NoopObserver, &AtomicBool::new(false))
    }

    #[test]
    fn clean_disk_completes_without_issues() {
        let mut r = ScriptedReader::new(64 * MIB);
        let m = run(&mut r, &ScanConfig::default());
        assert_eq!(m.status, ScanStatus::Completed);
        assert_eq!(m.abort, None);
        assert_eq!(m.blocks_total, 64);
        assert_eq!(m.blocks_ok, 64);
        assert_eq!(m.bytes_read_ok, 64 * MIB);
        assert_eq!(m.bytes_processed, 64 * MIB);
        assert!(m.issues.is_empty());
        assert!(m.bad_ranges.is_empty());
        assert_eq!(m.latency.count, 64);
        assert!(!m.timeline.is_empty());
        // 1 MiB in 2 ms ≈ 524 MB/s in every segment.
        let med = m.median_segment_mb_per_s.unwrap();
        assert!((med - 524.288).abs() < 1.0, "median {med}");
    }

    #[test]
    fn isolates_single_bad_sector_by_bisection() {
        let mut r = ScriptedReader::new(64 * MIB);
        let bad = 10 * MIB + 8192;
        r.bad_ranges.push((bad, bad + 4096));
        let m = run(&mut r, &ScanConfig::default());
        assert_eq!(m.status, ScanStatus::Completed);
        assert_eq!(m.error_blocks, 1);
        assert_eq!(m.media_errors, 1);
        assert_eq!(m.transient_errors, 0);
        assert_eq!(m.bad_ranges.len(), 1);
        let range = &m.bad_ranges[0];
        assert_eq!((range.offset, range.length, range.refined), (bad, 4096, true));
        assert_eq!(range.first_lba, bad / 512);
        assert_eq!(range.sector_count, 8);
        assert_eq!(m.bytes_unreadable, 4096);
        assert!(m.refine_reads <= 2 * 8, "bisecting 1 MiB down to 4 KiB must take ≤ 16 reads, took {}", m.refine_reads);
        assert_eq!(m.issues[0].kind, IssueKind::ReadError);
        assert_eq!(m.issues[0].error_class, Some(ErrorClass::Media));
    }

    #[test]
    fn transient_errors_are_not_bad_sectors() {
        let _en = crate::i18n::scoped(crate::i18n::Lang::En);
        let mut r = ScriptedReader::new(16 * MIB);
        r.transient_bad.push((3 * MIB, 3 * MIB + 512));
        let m = run(&mut r, &ScanConfig::default());
        assert_eq!(m.error_blocks, 1);
        assert_eq!(m.transient_errors, 1);
        assert_eq!(m.device_errors, 1);
        assert!(m.bad_ranges.is_empty());
        assert!(m.issues[0].transient);
        assert!(m.issues[0].description.contains("transient"));
    }

    #[test]
    fn counts_slow_blocks() {
        let mut r = ScriptedReader::new(32 * MIB);
        r.slow_ranges.push((5 * MIB, 7 * MIB, Duration::from_millis(800)));
        let m = run(&mut r, &ScanConfig::default());
        assert_eq!(m.slow_blocks, 2);
        assert_eq!(m.issues.iter().filter(|i| i.kind == IssueKind::Slow).count(), 2);
        assert_eq!(m.latency.count_above(500), 2);
        assert!(m.latency.max_ms >= 800.0);
        assert!(m.timeline.iter().any(|p| p.slow > 0));
    }

    #[test]
    fn timeouts_are_counted_and_not_refined() {
        let mut r = ScriptedReader::new(16 * MIB);
        r.timeout_ranges.push((2 * MIB, 2 * MIB + 1));
        let m = run(&mut r, &ScanConfig::default());
        assert_eq!(m.timeout_blocks, 1);
        assert_eq!(m.refine_reads, 0);
        assert_eq!(m.issues[0].kind, IssueKind::Timeout);
        assert_eq!(m.status, ScanStatus::Completed);
    }

    #[test]
    fn hung_device_aborts_scan() {
        let mut r = ScriptedReader::new(16 * MIB);
        r.hang_at = Some(4 * MIB);
        let m = run(&mut r, &ScanConfig::default());
        assert!(matches!(m.status, ScanStatus::Aborted(_)));
        assert_eq!(m.abort_kind(), Some(AbortKind::Hung));
        assert_eq!(m.blocks_total, 5);
        assert_eq!(m.issues.last().unwrap().kind, IssueKind::Hung);
        assert_eq!(r.reads.len(), 5, "no read after the hang");
    }

    #[test]
    fn fatal_error_aborts_scan() {
        let mut r = ScriptedReader::new(16 * MIB);
        r.fatal_at = Some((MIB, win32::ERROR_DEVICE_NOT_CONNECTED));
        let m = run(&mut r, &ScanConfig::default());
        assert_eq!(m.abort_kind(), Some(AbortKind::Fatal));
        assert_eq!(m.blocks_total, 2);
    }

    #[test]
    fn aborts_after_too_many_consecutive_errors() {
        let mut r = ScriptedReader::new(256 * MIB);
        r.bad_ranges.push((0, 200 * MIB));
        let cfg = ScanConfig { max_consecutive_errors: 8, refine_errors: false, ..Default::default() };
        let m = run(&mut r, &cfg);
        assert_eq!(m.abort_kind(), Some(AbortKind::TooManyErrors));
        assert_eq!(m.error_blocks, 8);
        assert_eq!(m.bad_ranges.len(), 1, "contiguous unrefined blocks are merged");
        assert_eq!(m.bad_ranges[0].length, 8 * MIB);
        assert!(!m.bad_ranges[0].refined);
    }

    #[test]
    fn abort_kind_of_results_saved_by_older_versions() {
        let mut r = ScriptedReader::new(16 * MIB);
        let mut m = run(&mut r, &ScanConfig::default());
        m.status = ScanStatus::Aborted("erro fatal de E/S: dispositivo desconectado ou removido (código 1167)".into());
        assert_eq!(m.abort_kind(), Some(AbortKind::Fatal));
        m.status = ScanStatus::Aborted("128 falhas consecutivas; teste interrompido".into());
        assert_eq!(m.abort_kind(), Some(AbortKind::TooManyErrors));
        // The new field is optional in JSON: old files still load.
        let mut json: serde_json::Value = serde_json::to_value(&m).unwrap();
        json.as_object_mut().unwrap().remove("abort");
        assert!(serde_json::from_value::<ScanMetrics>(json).is_ok());
    }

    #[test]
    fn refine_budget_is_respected() {
        let mut r = ScriptedReader::new(64 * MIB);
        r.bad_ranges.push((0, 64 * MIB));
        let cfg = ScanConfig { max_refine_reads: 10, max_consecutive_errors: 1000, ..Default::default() };
        let m = run(&mut r, &cfg);
        assert_eq!(m.refine_reads, 10);
        assert_eq!(m.bytes_unreadable, 64 * MIB, "everything is marked, refined or not");
        assert!(m.bad_ranges.iter().any(|b| !b.refined));
    }

    #[test]
    fn cancellation_stops_between_blocks() {
        struct CancelAfter<'a>(u32, &'a AtomicBool);
        impl ScanObserver for CancelAfter<'_> {
            fn on_progress(&mut self, _s: &LiveStats) {
                self.0 -= 1;
                if self.0 == 0 {
                    self.1.store(true, Ordering::Relaxed);
                }
            }
        }
        let cancel = AtomicBool::new(false);
        let mut r = ScriptedReader::new(64 * MIB);
        let m = run_scan(&mut r, &ScanConfig::default(), &mut CancelAfter(3, &cancel), &cancel);
        assert_eq!(m.status, ScanStatus::Cancelled);
        assert_eq!(m.blocks_total, 3);
        assert_eq!(m.bytes_processed, 3 * MIB);
    }

    #[test]
    fn partial_range_and_short_last_block() {
        let mut r = ScriptedReader::new(10 * MIB + 4096);
        let cfg = ScanConfig { start: 8 * MIB + 100, end: None, ..Default::default() };
        let m = run(&mut r, &cfg);
        assert_eq!(m.start_offset, 8 * MIB, "start aligned down to the sector");
        assert_eq!(r.reads, vec![(8 * MIB, MIB as u32), (9 * MIB, MIB as u32), (10 * MIB, 4096)]);
        assert_eq!(m.bytes_read_ok, 2 * MIB + 4096);
    }

    #[test]
    fn issue_list_is_capped() {
        let mut r = ScriptedReader::new(64 * MIB);
        r.slow_ranges.push((0, 64 * MIB, Duration::from_millis(600)));
        let cfg = ScanConfig { max_recorded_issues: 5, ..Default::default() };
        let m = run(&mut r, &cfg);
        assert_eq!(m.slow_blocks, 64);
        assert_eq!(m.issues.len(), 5);
        assert!(m.issues_truncated);
    }
}
