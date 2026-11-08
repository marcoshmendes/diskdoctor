//! Benchmark de leitura (disco físico, somente leitura) e de escrita (somente em
//! arquivo temporário novo, após confirmação explícita no CLI).
//!
//! Tudo em fila de profundidade 1 (QD1): é o padrão de acesso que mais expõe
//! travamentos e latências altas — o objetivo é diagnóstico, não marketing.
//!
//! Leitura:
//! - sequencial em 3 zonas (início, meio, fim) — em HD o fim é naturalmente mais
//!   lento (trilhas internas); em SSD as zonas devem ser parecidas;
//! - aleatória 4 KiB em todo o disco: IOPS e percentis de latência.

use crate::inventory::DiskIdentity;
use crate::io::{BlockReader, BlockWriter, ErrorClass, IoOutcome, classify_error};
use crate::scan::ScanStatus;
use crate::stats::{LatencyRecorder, LatencySummary};
use crate::util::{SplitMix64, align_down};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BenchConfig {
    pub seq_bytes_per_zone: u64,
    pub seq_block: u32,
    pub random_block: u32,
    pub random_max_ops: u64,
    pub random_duration_ms: u64,
    pub io_timeout_ms: u64,
    pub seed: u64,
}

impl Default for BenchConfig {
    fn default() -> Self {
        Self {
            seq_bytes_per_zone: 512 << 20,
            seq_block: 1 << 20,
            random_block: 4096,
            random_max_ops: 200_000,
            random_duration_ms: 10_000,
            io_timeout_ms: 10_000,
            seed: 0xd15c_d0c7,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BenchMode {
    Read,
    Write,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SequentialResult {
    pub label: String,
    pub offset: u64,
    pub bytes: u64,
    pub mb_per_s: f64,
    pub latency: LatencySummary,
    pub errors: u64,
    pub timeouts: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RandomResult {
    pub label: String,
    pub block_size: u32,
    pub ops: u64,
    pub iops: f64,
    pub mb_per_s: f64,
    pub latency: LatencySummary,
    pub errors: u64,
    pub timeouts: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BenchMetrics {
    pub mode: BenchMode,
    pub queue_depth: u32,
    pub sequential: Vec<SequentialResult>,
    pub random: Option<RandomResult>,
    pub status: ScanStatus,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WriteBenchInfo {
    pub target_dir: String,
    pub file_size: u64,
    pub metrics: BenchMetrics,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BenchmarkResult {
    pub tool_version: String,
    pub disk: DiskIdentity,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub config: BenchConfig,
    pub read: Option<BenchMetrics>,
    pub write: Option<WriteBenchInfo>,
}

pub trait BenchObserver {
    fn on_phase(&mut self, _label: &str, _total: u64) {}
    fn on_progress(&mut self, _done: u64) {}
}

pub struct NoopBenchObserver;
impl BenchObserver for NoopBenchObserver {}

struct PhaseOutcome {
    latency: LatencyRecorder,
    bytes: u64,
    device_time: Duration,
    ops: u64,
    errors: u64,
    timeouts: u64,
    stop: Option<ScanStatus>,
}

fn account(outcome: IoOutcome, len: u32, p: &mut PhaseOutcome) {
    match outcome {
        IoOutcome::Completed { bytes, latency } if bytes == len => {
            p.latency.record(latency);
            p.bytes += len as u64;
            p.device_time += latency;
            p.ops += 1;
        }
        IoOutcome::Completed { .. } => p.errors += 1,
        IoOutcome::Failed { code, .. } => {
            p.errors += 1;
            if classify_error(code) == ErrorClass::Fatal {
                p.stop = Some(ScanStatus::Aborted(format!("erro fatal: {}", crate::io::describe_error(code))));
            }
        }
        IoOutcome::TimedOut { latency } => {
            p.timeouts += 1;
            p.device_time += latency;
        }
        IoOutcome::Hung { .. } => {
            p.timeouts += 1;
            p.stop = Some(ScanStatus::Aborted("o dispositivo parou de responder".into()));
        }
    }
}

fn new_phase() -> PhaseOutcome {
    PhaseOutcome { latency: LatencyRecorder::new(), bytes: 0, device_time: Duration::ZERO, ops: 0, errors: 0, timeouts: 0, stop: None }
}

fn mbps(bytes: u64, t: Duration) -> f64 {
    if t.is_zero() { 0.0 } else { bytes as f64 / t.as_secs_f64() / 1e6 }
}

type IoFn<'a> = dyn FnMut(u64, u32, Duration) -> IoOutcome + 'a;

#[allow(clippy::too_many_arguments)]
fn sequential_phase(
    io: &mut IoFn<'_>,
    label: &str,
    offset: u64,
    bytes: u64,
    block: u32,
    timeout: Duration,
    observer: &mut dyn BenchObserver,
    cancel: &AtomicBool,
) -> (SequentialResult, Option<ScanStatus>) {
    observer.on_phase(label, bytes);
    let mut p = new_phase();
    let mut pos = 0u64;
    while pos < bytes {
        if cancel.load(Ordering::Relaxed) {
            p.stop = Some(ScanStatus::Cancelled);
            break;
        }
        let len = (bytes - pos).min(block as u64) as u32;
        let outcome = io(offset + pos, len, timeout);
        account(outcome, len, &mut p);
        pos += len as u64;
        observer.on_progress(pos);
        if p.stop.is_some() {
            break;
        }
    }
    let result = SequentialResult {
        label: label.to_string(),
        offset,
        bytes: p.bytes,
        mb_per_s: mbps(p.bytes, p.device_time),
        latency: p.latency.summary(),
        errors: p.errors,
        timeouts: p.timeouts,
    };
    (result, p.stop)
}

#[allow(clippy::too_many_arguments)]
fn random_phase(
    io: &mut IoFn<'_>,
    label: &str,
    area: u64,
    cfg: &BenchConfig,
    alignment: u64,
    timeout: Duration,
    observer: &mut dyn BenchObserver,
    cancel: &AtomicBool,
) -> (RandomResult, Option<ScanStatus>) {
    let block = cfg.random_block.max(alignment as u32);
    let slots = area / block as u64;
    let budget = Duration::from_millis(cfg.random_duration_ms);
    observer.on_phase(label, cfg.random_max_ops);
    let mut rng = SplitMix64::new(cfg.seed);
    let mut p = new_phase();
    let mut attempts = 0u64;
    while slots > 0 && attempts < cfg.random_max_ops && p.device_time < budget {
        if cancel.load(Ordering::Relaxed) {
            p.stop = Some(ScanStatus::Cancelled);
            break;
        }
        let offset = rng.below(slots) * block as u64;
        let outcome = io(offset, block, timeout);
        account(outcome, block, &mut p);
        attempts += 1;
        observer.on_progress(attempts);
        if p.stop.is_some() {
            break;
        }
    }
    let secs = p.device_time.as_secs_f64();
    let result = RandomResult {
        label: label.to_string(),
        block_size: block,
        ops: p.ops,
        iops: if secs > 0.0 { p.ops as f64 / secs } else { 0.0 },
        mb_per_s: mbps(p.bytes, p.device_time),
        latency: p.latency.summary(),
        errors: p.errors,
        timeouts: p.timeouts,
    };
    (result, p.stop)
}

/// Benchmark de leitura no disco físico (QD1).
pub fn run_read_benchmark<R: BlockReader + ?Sized>(
    reader: &mut R,
    cfg: &BenchConfig,
    observer: &mut dyn BenchObserver,
    cancel: &AtomicBool,
) -> BenchMetrics {
    let logical = reader.logical_sector_size().max(512) as u64;
    let alignment = logical.max(4096);
    let capacity = align_down(reader.capacity(), logical);
    let block = align_down((cfg.seq_block as u64).min(reader.max_io_size() as u64), alignment).max(alignment) as u32;
    let zone = align_down(cfg.seq_bytes_per_zone.min(capacity), alignment);
    let timeout = Duration::from_millis(cfg.io_timeout_ms.max(1));
    let mut io = |o: u64, l: u32, t: Duration| reader.read_at(o, l, t);

    let zones: Vec<(&str, u64)> = if capacity >= zone * 3 && zone > 0 {
        vec![
            ("Sequencial — início", 0),
            ("Sequencial — meio", align_down(capacity / 2, alignment)),
            ("Sequencial — fim", align_down(capacity - zone, alignment)),
        ]
    } else {
        vec![("Sequencial", 0)]
    };

    let mut metrics = BenchMetrics { mode: BenchMode::Read, queue_depth: 1, sequential: vec![], random: None, status: ScanStatus::Completed };
    for (label, offset) in zones {
        let bytes = zone.min(capacity - offset);
        let (r, stop) = sequential_phase(&mut io, label, offset, bytes, block, timeout, observer, cancel);
        metrics.sequential.push(r);
        if let Some(s) = stop {
            metrics.status = s;
            return metrics;
        }
    }
    let (r, stop) = random_phase(&mut io, "Aleatória 4 KiB (todo o disco)", capacity, cfg, alignment, timeout, observer, cancel);
    metrics.random = Some(r);
    if let Some(s) = stop {
        metrics.status = s;
    }
    metrics
}

/// Benchmark de escrita em arquivo temporário (QD1): sequencial e depois aleatória 4 KiB.
pub fn run_write_benchmark<W: BlockWriter + ?Sized>(
    writer: &mut W,
    cfg: &BenchConfig,
    observer: &mut dyn BenchObserver,
    cancel: &AtomicBool,
) -> BenchMetrics {
    let alignment = 4096u64;
    let size = align_down(writer.capacity(), alignment);
    let block = align_down((cfg.seq_block as u64).min(writer.max_io_size() as u64), alignment).max(alignment) as u32;
    let timeout = Duration::from_millis(cfg.io_timeout_ms.max(1));
    let mut io = |o: u64, l: u32, t: Duration| writer.write_at(o, l, t);

    let mut metrics = BenchMetrics { mode: BenchMode::Write, queue_depth: 1, sequential: vec![], random: None, status: ScanStatus::Completed };
    let (r, stop) = sequential_phase(&mut io, "Escrita sequencial (arquivo temporário)", 0, size, block, timeout, observer, cancel);
    metrics.sequential.push(r);
    if let Some(s) = stop {
        metrics.status = s;
        return metrics;
    }
    let (r, stop) = random_phase(&mut io, "Escrita aleatória 4 KiB (arquivo temporário)", size, cfg, alignment, timeout, observer, cancel);
    metrics.random = Some(r);
    if let Some(s) = stop {
        metrics.status = s;
    }
    metrics
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::testing::{ScriptedReader, ScriptedWriter};
    use crate::io::win32;

    const MIB: u64 = 1 << 20;

    fn small_cfg() -> BenchConfig {
        BenchConfig { seq_bytes_per_zone: 8 * MIB, random_max_ops: 500, random_duration_ms: 60_000, ..Default::default() }
    }

    #[test]
    fn read_benchmark_uses_three_zones_and_random() {
        let mut r = ScriptedReader::new(256 * MIB);
        let m = run_read_benchmark(&mut r, &small_cfg(), &mut NoopBenchObserver, &AtomicBool::new(false));
        assert_eq!(m.status, ScanStatus::Completed);
        assert_eq!(m.sequential.len(), 3);
        assert_eq!(m.sequential[2].offset, 248 * MIB);
        assert!(m.sequential.iter().all(|s| s.bytes == 8 * MIB && s.errors == 0));
        assert!((m.sequential[0].mb_per_s - 524.288).abs() < 1.0);
        let rnd = m.random.unwrap();
        assert_eq!(rnd.ops, 500);
        assert!(rnd.iops > 0.0);
        // Leituras aleatórias alinhadas a 4 KiB e dentro do disco.
        assert!(r.reads.iter().all(|&(o, l)| o % 4096 == 0 && o + l as u64 <= 256 * MIB));
    }

    #[test]
    fn small_disk_uses_single_zone() {
        let mut r = ScriptedReader::new(16 * MIB);
        let m = run_read_benchmark(&mut r, &small_cfg(), &mut NoopBenchObserver, &AtomicBool::new(false));
        assert_eq!(m.sequential.len(), 1);
        assert_eq!(m.sequential[0].bytes, 8 * MIB);
    }

    #[test]
    fn random_phase_stops_on_time_budget() {
        let mut r = ScriptedReader::new(64 * MIB);
        r.latency_per_mib = Duration::from_millis(256); // 4 KiB = 1 ms
        let cfg = BenchConfig { seq_bytes_per_zone: MIB, random_duration_ms: 50, random_max_ops: 1_000_000, ..Default::default() };
        let m = run_read_benchmark(&mut r, &cfg, &mut NoopBenchObserver, &AtomicBool::new(false));
        let rnd = m.random.unwrap();
        assert!((50..=51).contains(&rnd.ops), "ops = {}", rnd.ops);
        assert!((rnd.iops - 1000.0).abs() < 1.0);
    }

    #[test]
    fn fatal_error_aborts_benchmark() {
        let mut r = ScriptedReader::new(256 * MIB);
        r.fatal_at = Some((MIB, win32::ERROR_DEVICE_NOT_CONNECTED));
        let m = run_read_benchmark(&mut r, &small_cfg(), &mut NoopBenchObserver, &AtomicBool::new(false));
        assert!(matches!(m.status, ScanStatus::Aborted(_)));
        assert_eq!(m.sequential.len(), 1);
        assert_eq!(m.sequential[0].errors, 1);
        assert!(m.random.is_none());
    }

    #[test]
    fn write_benchmark_stays_inside_temp_file() {
        let mut w = ScriptedWriter::new(32 * MIB);
        let m = run_write_benchmark(&mut w, &small_cfg(), &mut NoopBenchObserver, &AtomicBool::new(false));
        assert_eq!(m.mode, BenchMode::Write);
        assert_eq!(m.sequential[0].bytes, 32 * MIB);
        assert_eq!(m.random.as_ref().unwrap().ops, 500);
        assert!(w.writes.iter().all(|&(o, l)| o + l as u64 <= 32 * MIB));
    }

    #[test]
    fn cancelled_benchmark_reports_status() {
        let mut r = ScriptedReader::new(256 * MIB);
        let cancel = AtomicBool::new(true);
        let m = run_read_benchmark(&mut r, &small_cfg(), &mut NoopBenchObserver, &cancel);
        assert_eq!(m.status, ScanStatus::Cancelled);
        assert!(r.reads.is_empty());
    }
}
