//! Correlation engine: combines SMART, Windows counters, events, Surface Scan, Benchmark,
//! Capacity Verification and history into findings with a likely cause and a confidence
//! level.
//!
//! Each rule (`rule_*`) gathers weighted **evidence** (1 weak, 2 moderate, 3 strong,
//! 4 very strong, 5 conclusive) from a source. Confidence comes from [`confidence_for`]:
//! conclusive evidence, or a high total weight from independent sources, gives high
//! confidence. A rule without evidence produces no finding.
//!
//! Principle: normal SMART does **not** mean a healthy drive. Without evidence the status
//! is "no problems detected in the analyzed sources", always listing the gaps (what was
//! not checked).
//!
//! Texts are produced in the active language (see [`crate::i18n`]); rule ids, severities
//! and sources are stable identifiers for scripts.

use crate::benchmark::BenchmarkResult;
use crate::capacity::{CapacityResult, CapacityVerdict};
use crate::events::{Attribution, EventKind, StorageEvent};
use crate::health::DiskHealth;
use crate::history::SmartDelta;
use crate::i18n::t;
use crate::inventory::{BusKind, DiskInfo};
use crate::journal::NoteRecord;
use crate::scan::{AbortKind, ScanResult, ScanStatus};
use crate::smart::SmartReport;
use crate::util::{decimal, format_bytes, format_mbps, format_ms, int, local_date, local_datetime, plural};
use crate::windows_counters::ReliabilityCounters;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Attention,
    Warning,
    Critical,
}

impl Severity {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Info => t("Info", "Informativo"),
            Self::Attention => t("Attention", "Atenção"),
            Self::Warning => t("Warning", "Alerta"),
            Self::Critical => t("Critical", "Crítico"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Low,
    Medium,
    High,
}

impl Confidence {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Low => t("Low", "Baixa"),
            Self::Medium => t("Medium", "Média"),
            Self::High => t("High", "Alta"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceSource {
    Smart,
    WindowsCounters,
    Events,
    Scan,
    Benchmark,
    History,
    Capacity,
}

impl EvidenceSource {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Smart => "SMART",
            Self::WindowsCounters => t("Windows counters", "Contadores do Windows"),
            Self::Events => t("Windows events", "Eventos do Windows"),
            Self::Scan => "Surface Scan",
            Self::Benchmark => "Benchmark",
            Self::History => t("SMART history", "Histórico SMART"),
            Self::Capacity => "Capacity Verification",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Evidence {
    pub source: EvidenceSource,
    pub weight: u8,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Finding {
    /// Stable rule identifier (e.g. `comm.link`).
    pub rule: String,
    pub disk_number: Option<u32>,
    pub severity: Severity,
    pub confidence: Confidence,
    /// Why this confidence (how much evidence, from which sources).
    pub confidence_basis: String,
    pub title: String,
    pub evidence: Vec<Evidence>,
    pub possible_causes: Vec<String>,
    pub recommendations: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiskStatus {
    Unknown,
    Healthy,
    Attention,
    Warning,
    Critical,
}

impl DiskStatus {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Unknown => t("Undetermined", "Indeterminado"),
            Self::Healthy => t("No problems detected", "Sem problemas detectados"),
            Self::Attention => t("Attention", "Atenção"),
            Self::Warning => t("Warning", "Alerta"),
            Self::Critical => t("Critical", "Crítico"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DiskVerdict {
    pub disk_number: u32,
    pub label: String,
    pub status: DiskStatus,
    pub headline: String,
    pub findings: Vec<Finding>,
    pub sources_used: Vec<String>,
    /// What was not checked (limits the confidence in the verdict).
    pub gaps: Vec<String>,
    /// The recorded change that cuts the analysis window, if any (also in `sources_used`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub change: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AnalysisResult {
    pub verdicts: Vec<DiskVerdict>,
    pub global_findings: Vec<Finding>,
    pub notes: Vec<String>,
}

impl AnalysisResult {
    pub fn worst_status(&self) -> Option<DiskStatus> {
        self.verdicts.iter().map(|v| v.status).max()
    }
}

pub struct AnalysisInput<'a> {
    pub disks: &'a [DiskInfo],
    pub health: &'a [DiskHealth],
    pub events: &'a [StorageEvent],
    pub scans: &'a [ScanResult],
    pub benchmarks: &'a [BenchmarkResult],
    /// Saved Capacity Verification results (`diskdoctor capacity`).
    pub capacity: &'a [CapacityResult],
    /// Latest recorded change per drive (fingerprint → note): events before it do not
    /// count toward the verdict. Use [`NO_CHANGES`] when there are none.
    pub changes: &'a BTreeMap<String, NoteRecord>,
    pub event_window_hours: u32,
    pub events_collected: bool,
    pub elevated: bool,
}

pub const NO_CHANGES: &BTreeMap<String, NoteRecord> = &BTreeMap::new();

/// Confidence level from the evidence.
pub fn confidence_for(evidence: &[Evidence]) -> Confidence {
    let total: u32 = evidence.iter().map(|e| e.weight as u32).sum();
    let sources: BTreeSet<_> = evidence.iter().map(|e| e.source).collect();
    let max = evidence.iter().map(|e| e.weight).max().unwrap_or(0);
    if max >= 5 || (total >= 6 && sources.len() >= 2) {
        Confidence::High
    } else if total >= 3 {
        Confidence::Medium
    } else {
        Confidence::Low
    }
}

fn confidence_basis(evidence: &[Evidence]) -> String {
    let sources: BTreeSet<_> = evidence.iter().map(|e| e.source.label()).collect();
    let total: u32 = evidence.iter().map(|e| e.weight as u32).sum();
    tr!(
        "{} from {} ({}), total weight {total}",
        "{} de {} ({}), peso total {total}",
        plural(evidence.len() as u64, t("piece of evidence", "evidência"), t("pieces of evidence", "evidências")),
        plural(sources.len() as u64, t("source", "fonte"), t("sources", "fontes")),
        sources.into_iter().collect::<Vec<_>>().join(", ")
    )
}

struct Builder {
    rule: &'static str,
    title: String,
    severity: Severity,
    evidence: Vec<Evidence>,
    causes: Vec<String>,
    recs: Vec<String>,
}

impl Builder {
    fn new(rule: &'static str, title: impl Into<String>) -> Self {
        Self { rule, title: title.into(), severity: Severity::Info, evidence: vec![], causes: vec![], recs: vec![] }
    }
    fn add(&mut self, source: EvidenceSource, weight: u8, severity: Severity, text: impl Into<String>) {
        self.severity = self.severity.max(severity);
        self.evidence.push(Evidence { source, weight, text: text.into() });
    }
    fn total(&self) -> u32 {
        self.evidence.iter().map(|e| e.weight as u32).sum()
    }
    fn causes(mut self, c: &[&str]) -> Self {
        self.causes.extend(c.iter().map(|s| s.to_string()));
        self
    }
    fn recs(mut self, r: &[&str]) -> Self {
        self.recs.extend(r.iter().map(|s| s.to_string()));
        self
    }
    fn finish(self, disk: Option<u32>) -> Option<Finding> {
        if self.evidence.is_empty() {
            return None;
        }
        Some(Finding {
            rule: self.rule.to_string(),
            disk_number: disk,
            severity: self.severity,
            confidence: confidence_for(&self.evidence),
            confidence_basis: confidence_basis(&self.evidence),
            title: self.title,
            evidence: self.evidence,
            possible_causes: self.causes,
            recommendations: self.recs,
        })
    }
}

struct EventSummary {
    count: u64,
    last: DateTime<Utc>,
    code: String,
    inferred: bool,
}

fn summarize(events: &[&StorageEvent], kind: EventKind) -> Option<EventSummary> {
    let matching: Vec<&&StorageEvent> = events.iter().filter(|e| e.kind == kind).collect();
    let last = matching.iter().map(|e| e.time).max()?;
    let codes: BTreeSet<String> = matching.iter().map(|e| e.code()).collect();
    Some(EventSummary {
        count: matching.len() as u64,
        last,
        code: codes.into_iter().collect::<Vec<_>>().join(", "),
        inferred: matching.iter().all(|e| e.attribution == Attribution::Inferred),
    })
}

fn event_text(s: &EventSummary, kind: EventKind, hours: u32) -> String {
    tr!(
        "{} — {}: {} in the last {hours} h (latest {}){}",
        "{} — {}: {} nas últimas {hours} h (última em {}){}",
        s.code,
        kind.description(),
        plural(s.count, t("occurrence", "ocorrência"), t("occurrences", "ocorrências")),
        local_datetime(s.last),
        if s.inferred { t(" — drive inferred from the controller driver", " — disco inferido pelo driver da controladora") } else { "" }
    )
}

/// Weight of event evidence, growing with the count (maximum 4).
fn event_weight(base: u8, s: &EventSummary) -> u8 {
    let w = if s.count >= 20 {
        base + 2
    } else if s.count >= 5 {
        base + 1
    } else {
        base
    };
    let w = if s.inferred { w.saturating_sub(1).max(1) } else { w };
    w.min(4)
}

struct DiskCtx<'a> {
    disk: &'a DiskInfo,
    smart: Option<&'a SmartReport>,
    windows: Option<&'a ReliabilityCounters>,
    delta: Option<&'a SmartDelta>,
    events: Vec<&'a StorageEvent>,
    all_events: &'a [StorageEvent],
    scan: Option<&'a ScanResult>,
    bench: Option<&'a BenchmarkResult>,
    capacity: Option<&'a CapacityResult>,
    hours: u32,
}

impl DiskCtx<'_> {
    fn add_events(&self, b: &mut Builder, kind: EventKind, base: u8, severity: Severity, suffix: &str) {
        if let Some(s) = summarize(&self.events, kind) {
            let w = event_weight(base, &s);
            b.add(EvidenceSource::Events, w, severity, format!("{}{suffix}", event_text(&s, kind, self.hours)));
        }
    }

    fn add_growth(&self, b: &mut Builder, key: &str, weight: u8, severity: Severity, suffix: &str) {
        if let Some(c) = self.delta.and_then(|d| d.change(key)) {
            let since = self.delta.map(|d| local_date(d.baseline_time)).unwrap_or_default();
            b.add(
                EvidenceSource::History,
                weight,
                severity,
                tr!(
                    "{} rose from {} to {} since {since}{suffix}",
                    "{} aumentou de {} para {} desde {since}{suffix}",
                    c.label,
                    int(c.before),
                    int(c.after)
                ),
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Per-drive rules
// ---------------------------------------------------------------------------

fn rule_capacity(ctx: &DiskCtx) -> Option<Finding> {
    let c = ctx.capacity?;
    let m = &c.metrics;
    let when = local_date(c.finished_at);
    match m.verdict {
        CapacityVerdict::Fake => {
            let mut b = Builder::new(
                "capacity.fake",
                t("Fake capacity: the device does not store everything it advertises", "Capacidade falsa: o dispositivo não guarda tudo o que anuncia"),
            );
            b.add(
                EvidenceSource::Capacity,
                5,
                Severity::Critical,
                tr!(
                    "Capacity Verification on {when} for {}: {} written, only {} read back correctly ({} overwritten, {} lost). Estimated real capacity: {} (advertised: {})",
                    "Capacity Verification de {when} em {}: {} gravados, só {} lidos de volta corretamente ({} sobrescritos, {} perdidos). Capacidade real estimada: {} (anunciada: {})",
                    c.volume,
                    format_bytes(m.written_bytes),
                    format_bytes(m.ok_bytes),
                    format_bytes(m.overwritten_bytes),
                    format_bytes(m.lost_bytes),
                    c.estimated_real_capacity.map(format_bytes).unwrap_or_else(|| "?".into()),
                    format_bytes(c.disk.size_bytes)
                ),
            );
            b.causes(&[t(
                "Counterfeit USB drive/card/disk: the controller advertises more capacity than the memory it has",
                "Pendrive/cartão/disco falsificado: o controlador anuncia mais capacidade do que a memória que existe",
            )])
            .recs(&[
                t(
                    "Do not keep important data on it: whatever goes past the real capacity disappears or overwrites older files",
                    "Não guarde dados importantes nele: o que passa da capacidade real some ou sobrescreve arquivos antigos",
                ),
                t(
                    "Ask the seller for a replacement or refund; the diskdoctor report works as evidence",
                    "Peça troca ou reembolso ao vendedor; o relatório do diskdoctor serve de evidência",
                ),
                t(
                    "If you use it anyway, use only up to the estimated real capacity (resizing the partition erases the device and is done outside diskdoctor)",
                    "Se for usar mesmo assim, só até a capacidade real estimada (ajustar a partição apaga o dispositivo e é feito fora do diskdoctor)",
                ),
            ])
            .finish(Some(ctx.disk.number))
        }
        CapacityVerdict::Defective => {
            let mut b = Builder::new(
                "capacity.errors",
                t("Failures writing or reading back the test data", "Falhas ao gravar ou ler de volta os dados de teste"),
            );
            let data_bad = m.corrupted_bytes + m.unreadable_bytes + m.lost_bytes;
            if data_bad > 0 {
                b.add(
                    EvidenceSource::Capacity,
                    5,
                    Severity::Critical,
                    tr!(
                        "Capacity Verification on {when} for {}: {} came back wrong or unreadable (corrupted {}, unreadable {}, lost {})",
                        "Capacity Verification de {when} em {}: {} voltaram errados ou ilegíveis (corrompidos {}, ilegíveis {}, perdidos {})",
                        c.volume,
                        format_bytes(data_bad),
                        format_bytes(m.corrupted_bytes),
                        format_bytes(m.unreadable_bytes),
                        format_bytes(m.lost_bytes)
                    ),
                );
            }
            if m.write_errors > 0 {
                b.add(
                    EvidenceSource::Capacity,
                    4,
                    Severity::Critical,
                    tr!("{} failed to write", "{} falharam na gravação", plural(m.write_errors, t("block", "bloco"), t("blocks", "blocos"))),
                );
            }
            if let ScanStatus::Aborted(r) = &m.status {
                b.add(EvidenceSource::Capacity, 4, Severity::Critical, tr!("Verification stopped: {r}", "Verificação interrompida: {r}"));
            }
            b.causes(&[
                t("Worn or defective flash memory", "Memória flash desgastada ou defeituosa"),
                t("Poor contact in the connector, card reader or USB cable", "Mau contato no conector, no leitor de cartão ou no cabo USB"),
                t("Not enough power on the USB port", "Energia insuficiente na porta USB"),
            ])
            .recs(&[
                t("Copy what matters somewhere else", "Copie o que importa para outro lugar"),
                t(
                    "Repeat on another USB port (rear, directly on the motherboard) or another reader to rule out poor contact",
                    "Repita em outra porta USB (traseira, direto na placa-mãe) ou outro leitor para descartar mau contato",
                ),
                t("If the failures repeat, replace the device", "Se as falhas se repetirem, substitua o dispositivo"),
            ])
            .finish(Some(ctx.disk.number))
        }
        CapacityVerdict::Genuine | CapacityVerdict::Inconclusive => None,
    }
}

fn rule_smart_failed(ctx: &DiskCtx) -> Option<Finding> {
    let s = ctx.smart?;
    let mut b = Builder::new("smart.failed", t("The drive itself reports a failure (SMART)", "O próprio disco reporta falha (SMART)"));
    if s.overall_passed == Some(false) {
        b.add(
            EvidenceSource::Smart,
            5,
            Severity::Critical,
            t("Overall SMART assessment: FAILED (the drive firmware predicts a failure)", "Avaliação geral do SMART: FALHOU (o firmware do disco prevê falha)"),
        );
    }
    for f in &s.failing_attributes {
        if f.contains("FAILING_NOW") {
            b.add(EvidenceSource::Smart, 4, Severity::Critical, tr!("Attribute below its failure threshold: {f}", "Atributo abaixo do limite de falha: {f}"));
        } else {
            b.add(EvidenceSource::Smart, 1, Severity::Attention, tr!("Attribute was below its threshold in the past: {f}", "Atributo já esteve abaixo do limite: {f}"));
        }
    }
    if let Some(w) = s.nvme_critical_warning.filter(|&w| w != 0) {
        let bits: [(u64, u8, Severity, &str); 5] = [
            (1, 4, Severity::Critical, t("available spare below threshold", "reserva de blocos abaixo do limite")),
            (2, 2, Severity::Warning, t("temperature out of range", "temperatura fora do limite")),
            (4, 4, Severity::Critical, t("reliability degraded by media or internal errors", "confiabilidade degradada por erros de mídia ou internos")),
            (8, 5, Severity::Critical, t("the SSD switched to read-only mode", "o SSD entrou em modo somente leitura")),
            (16, 2, Severity::Warning, t("volatile memory backup failed", "falha no backup de memória volátil")),
        ];
        for (mask, weight, sev, text) in bits {
            if w & mask != 0 {
                b.add(EvidenceSource::Smart, weight, sev, tr!("NVMe critical warning: {text}", "Aviso crítico NVMe: {text}"));
            }
        }
    }
    b.causes(&[t("Internal defect or wear detected by the drive's own firmware", "Defeito ou desgaste interno detectado pelo próprio firmware do disco")])
        .recs(&[
            t("Back up immediately", "Faça backup imediatamente"),
            t("Replace the drive and do not trust it with new data", "Substitua o disco e não confie novos dados a ele"),
        ])
        .finish(Some(ctx.disk.number))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AtaLogKind {
    /// UNC errors (unreadable data) among the samples.
    Media,
    /// Only ICRC errors (interface CRC): cable/connector/port.
    Interface,
    Other,
}

/// Classifies the ATA error log by the samples `smartctl -x` shows.
fn ata_error_log_kind(s: &SmartReport) -> AtaLogKind {
    if s.error_log_samples.iter().any(|e| e.contains("UNC")) {
        AtaLogKind::Media
    } else if s.error_log_samples.iter().any(|e| e.contains("ICRC")) {
        AtaLogKind::Interface
    } else {
        AtaLogKind::Other
    }
}

/// Does the drive report CRC counters, all at zero (lifetime)? A bad cable or connector
/// almost always makes them rise.
fn crc_counters_clean(s: &SmartReport) -> bool {
    let reported = [s.interface_crc_errors, s.vendor_crc_errors, s.sata_phy_errors, s.phy_interface_errors];
    reported.iter().any(Option::is_some)
        && reported.iter().flatten().all(|&n| n == 0)
        && ata_error_log_kind(s) != AtaLogKind::Interface
}

fn rule_media(ctx: &DiskCtx) -> Option<Finding> {
    use EvidenceSource::*;
    let mut b = Builder::new("media.degradation", t("Unreadable sectors or degraded media", "Setores ilegíveis ou mídia degradada"));
    if let Some(s) = ctx.smart {
        if let Some(n) = s.pending_sectors.filter(|&n| n > 0) {
            b.add(
                Smart,
                3,
                Severity::Critical,
                tr!(
                    "Pending sectors (197): {} — the drive could not read them and is waiting to reallocate",
                    "Setores pendentes (197): {} — o disco não conseguiu ler e aguarda realocação",
                    int(n)
                ),
            );
        }
        if let Some(n) = s.offline_uncorrectable.filter(|&n| n > 0) {
            b.add(Smart, 3, Severity::Critical, tr!("Offline uncorrectable sectors (198): {}", "Setores incorrigíveis offline (198): {}", int(n)));
        }
        if let Some(n) = s.reported_uncorrectable.filter(|&n| n > 0) {
            b.add(Smart, 2, Severity::Warning, tr!("Uncorrectable errors reported to the host (187): {}", "Erros incorrigíveis reportados ao sistema (187): {}", int(n)));
        }
        if let Some(n) = s.reallocated_sectors.filter(|&n| n > 0) {
            let (w, sev) = if n >= 100 { (2, Severity::Warning) } else { (1, Severity::Attention) };
            b.add(
                Smart,
                w,
                sev,
                tr!(
                    "Reallocated sectors (5): {} — bad sectors already replaced by spares",
                    "Setores realocados (5): {} — setores ruins já substituídos pela reserva",
                    int(n)
                ),
            );
        }
        if let Some(n) = s.grown_bad_blocks.filter(|&n| n > 0) {
            let (w, sev) = if n >= 50 { (2, Severity::Warning) } else { (1, Severity::Attention) };
            b.add(Smart, w, sev, tr!("Bad blocks grown during use: {}", "Blocos defeituosos adquiridos durante o uso: {}", int(n)));
        }
        if let Some(n) = s.program_erase_failures.filter(|&n| n > 0) {
            b.add(Smart, 2, Severity::Warning, tr!("Flash program/erase failures: {}", "Falhas de programação/apagamento da memória flash: {}", int(n)));
        }
        if let Some(n) = s.media_errors.filter(|&n| n > 0) {
            b.add(Smart, 3, Severity::Critical, tr!("Media and data integrity errors (NVMe): {}", "Erros de integridade de mídia (NVMe): {}", int(n)));
        }
        if let Some(n) = s.self_test_failures.filter(|&n| n > 0) {
            b.add(Smart, 3, Severity::Critical, tr!("SMART self-test logged as failed: {}", "Autoteste SMART registrado com falha: {}", int(n)));
        }
        if !s.is_nvme() {
            // ICRC errors (interface CRC) without UNC are communication: handled in `comm.link`.
            if let Some(n) = s.error_log_entries.filter(|&n| n > 0) {
                match ata_error_log_kind(s) {
                    AtaLogKind::Media => b.add(
                        Smart,
                        2,
                        Severity::Warning,
                        tr!(
                            "ATA error log with {} entries (including UNC errors: unreadable data)",
                            "Log de erros ATA com {} registros (inclui erros UNC: dado ilegível)",
                            int(n)
                        ),
                    ),
                    AtaLogKind::Interface => {}
                    AtaLogKind::Other => b.add(Smart, 1, Severity::Attention, tr!("ATA error log with {} entries", "Log de erros ATA com {} registros", int(n))),
                }
            }
        }
    }
    ctx.add_growth(&mut b, "pending_sectors", 3, Severity::Critical, "");
    let ongoing = t(" — degradation in progress", " — degradação em andamento");
    for key in ["reallocated_sectors", "offline_uncorrectable", "reported_uncorrectable", "media_errors", "grown_bad_blocks", "program_erase_failures"] {
        ctx.add_growth(&mut b, key, 2, Severity::Warning, ongoing);
    }
    if let Some(w) = ctx.windows {
        if let Some(n) = w.read_errors_uncorrected.filter(|&n| n > 0) {
            b.add(
                WindowsCounters,
                2,
                Severity::Warning,
                tr!("Windows recorded {} uncorrected read errors", "O Windows registrou {} erros de leitura não corrigidos", int(n)),
            );
        }
        if let Some(n) = w.write_errors_uncorrected.filter(|&n| n > 0) {
            b.add(
                WindowsCounters,
                2,
                Severity::Warning,
                tr!("Windows recorded {} uncorrected write errors", "O Windows registrou {} erros de gravação não corrigidos", int(n)),
            );
        }
    }
    ctx.add_events(&mut b, EventKind::BadBlock, 3, Severity::Critical, "");
    ctx.add_events(&mut b, EventKind::IoFailedHardware, 2, Severity::Warning, "");
    ctx.add_events(&mut b, EventKind::SmartPredictedFailure, 4, Severity::Critical, "");
    ctx.add_events(
        &mut b,
        EventKind::PagingError,
        1,
        Severity::Warning,
        t(" — reading the paging file failed, a typical cause of freezes", " — leitura do arquivo de paginação falhou, causa típica de congelamento"),
    );
    if let Some(scan) = ctx.scan {
        let m = &scan.metrics;
        if !m.bad_ranges.is_empty() {
            let first = m.bad_ranges[0].first_lba;
            b.add(
                Scan,
                5,
                Severity::Critical,
                tr!(
                    "Surface Scan on {}: {} unreadable in {} (first at LBA {})",
                    "Surface Scan de {}: {} ilegíveis em {} (primeira no LBA {})",
                    local_date(scan.finished_at),
                    format_bytes(m.bytes_unreadable),
                    plural(m.bad_ranges.len() as u64, t("region", "região"), t("regions", "regiões")),
                    int(first)
                ),
            );
        }
    }
    let causes: Vec<&str> = if ctx.disk.is_ssd() {
        vec![
            t("Degraded flash cells (wear or loss of data retention)", "Células de memória flash degradadas (desgaste ou perda de retenção de dados)"),
            t("SSD controller or firmware failure", "Falha do controlador ou do firmware do SSD"),
        ]
    } else if ctx.disk.is_hdd() {
        vec![
            t("Damaged magnetic surface", "Superfície magnética danificada"),
            t("Weakening read head", "Cabeçote de leitura enfraquecendo"),
            t("Shock or vibration damage", "Dano por impacto ou vibração"),
        ]
    } else {
        vec![t("Storage media degradation", "Degradação da mídia de armazenamento"), t("Internal device failure", "Falha interna do dispositivo")]
    };
    let mut b = b.causes(&causes).recs(&[
        t("Back up the important data on this drive now", "Faça backup imediato dos dados importantes deste disco"),
        t("Plan to replace the drive", "Planeje a substituição do disco"),
        t(
            "Repeat the Surface Scan in a few days: if the number of bad sectors grows, the failure is progressing",
            "Repita o Surface Scan em alguns dias: se a quantidade de setores ruins crescer, a falha está progredindo",
        ),
    ]);
    if ctx.disk.is_ssd() {
        b.recs.push(t("Check the manufacturer's website for a firmware update", "Verifique se há atualização de firmware no site do fabricante").into());
    }
    b.finish(Some(ctx.disk.number))
}

fn comm_causes_and_recs(disk: &DiskInfo) -> (Vec<&'static str>, Vec<&'static str>) {
    match disk.bus {
        BusKind::Nvme => (
            vec![
                t("Poor contact in the M.2/PCIe slot", "Contato ruim no slot M.2/PCIe"),
                t("Aggressive power management (APST on the SSD or ASPM on PCIe)", "Gerenciamento de energia agressivo (APST no SSD ou ASPM no PCIe)"),
                t("Faulty SSD firmware or NVMe driver", "Firmware do SSD ou driver NVMe com defeito"),
                t("Overheating leading to throttling or a controller hang", "Superaquecimento levando a throttling ou travamento do controlador"),
            ],
            vec![
                t("Power off the PC, remove and reseat the SSD in its M.2 slot (or try another slot)", "Desligue o PC, remova e reassente o SSD no slot M.2 (ou teste outro slot)"),
                t(
                    "Update the SSD firmware, the motherboard BIOS/UEFI and the storage driver",
                    "Atualize o firmware do SSD, o BIOS/UEFI da placa-mãe e o driver de armazenamento",
                ),
                t(
                    "Try disabling link power management: Power Options → PCI Express → Link State Power Management = Off",
                    "Teste desativar o gerenciamento de energia do link: Opções de energia → PCI Express → Gerenciamento de Energia do Estado do Link = Desativado",
                ),
                t(
                    "After each change, run `diskdoctor health` and `diskdoctor events` to see whether the events stop",
                    "Depois de cada mudança, rode `diskdoctor health` e `diskdoctor events` para ver se os eventos param",
                ),
            ],
        ),
        BusKind::Usb => (
            vec![
                t("Faulty USB cable or adapter", "Cabo ou adaptador USB defeituoso"),
                t("Not enough power on the USB port", "Energia insuficiente na porta USB"),
                t("USB-SATA bridge with problematic firmware", "Ponte USB-SATA com firmware problemático"),
            ],
            vec![
                t(
                    "Replace the cable/adapter and use a rear USB port (directly on the motherboard)",
                    "Troque o cabo/adaptador e use uma porta USB traseira (direto na placa-mãe)",
                ),
                t("Use external power if the drive/enclosure supports it", "Use alimentação externa se o disco/case suportar"),
            ],
        ),
        _ => (
            vec![
                t("Damaged, kinked or loose SATA cable", "Cabo SATA danificado, dobrado ou mal encaixado"),
                t("Faulty SATA port or motherboard controller", "Porta SATA ou controladora da placa-mãe com problema"),
                t("Unstable power (power cable, adapter or PSU)", "Alimentação instável (cabo de energia, adaptador ou fonte)"),
                t("Drive firmware or controller driver (AHCI/RST)", "Firmware do disco ou driver da controladora (AHCI/RST)"),
                t(
                    "Internal pauses of the drive itself (firmware, SSD garbage collection) that exceed the Windows timeout — more common on SSDs without DRAM cache",
                    "Pausas internas do próprio disco (firmware, coleta de lixo do SSD) que estouram o tempo limite do Windows — mais comum em SSDs sem cache DRAM",
                ),
            ],
            vec![
                t(
                    "Replace the SATA cable with a new one and connect the drive to another SATA port on the motherboard",
                    "Troque o cabo SATA por um novo e ligue o disco em outra porta SATA da placa-mãe",
                ),
                t("Use another power connector from the PSU (avoid adapters and extensions)", "Use outro conector de energia da fonte (evite adaptadores e extensões)"),
                t("Update the drive firmware and the BIOS/controller driver", "Atualize o firmware do disco e o BIOS/driver da controladora"),
                t(
                    "After the swap, follow up with `diskdoctor health`: the counters do not reset, but they must stop rising",
                    "Depois da troca, acompanhe com `diskdoctor health`: os contadores não zeram, mas não devem continuar aumentando",
                ),
            ],
        ),
    }
}

/// Spread of the LBAs cited by retry events (153): scattered across the drive they point
/// to the link/controller/firmware; concentrated, to one region of the media.
fn retry_lba_pattern(ctx: &DiskCtx) -> Option<String> {
    let lbas: Vec<u64> = ctx.events.iter().filter(|e| e.kind == EventKind::IoRetried).filter_map(|e| e.lba).collect();
    if lbas.len() < 5 {
        return None;
    }
    let mut counts: BTreeMap<u64, u32> = BTreeMap::new();
    for l in &lbas {
        *counts.entry(*l).or_default() += 1;
    }
    let sector = ctx.disk.logical_sector_size.max(512) as u64;
    let total_sectors = (ctx.disk.size_bytes / sector).max(1);
    let (min, max) = (*counts.keys().next()?, *counts.keys().next_back()?);
    let spread = (max - min) as f64 * 100.0 / total_sectors as f64;
    let mut top: Vec<(u64, u32)> = counts.iter().map(|(l, c)| (*l, *c)).filter(|(_, c)| *c >= 3).collect();
    top.sort_by_key(|(_, c)| std::cmp::Reverse(*c));
    let hot = if top.is_empty() {
        String::new()
    } else {
        tr!(
            "; the most repeated: {} — check that region with the Surface Scan",
            "; os que mais se repetem: {} — confira essa região no Surface Scan",
            top.iter().take(3).map(|(l, c)| format!("LBA {} ({c}×)", int(*l))).collect::<Vec<_>>().join(", ")
        )
    };
    if counts.len() >= 5 && spread >= 25.0 {
        Some(tr!(
            "The retries hit {} different addresses spread across {}% of the drive — the typical pattern of a communication, controller or firmware failure, not of an isolated bad sector{hot}",
            "Os retries ocorreram em {} endereços diferentes, espalhados por {}% do disco — padrão típico de falha de comunicação, controladora ou firmware, não de um setor ruim isolado{hot}",
            int(counts.len() as u64),
            decimal(spread, 0)
        ))
    } else {
        Some(tr!(
            "The retries are concentrated on {} address(es) within {}% of the drive — consistent with a weak region of the media{hot}",
            "Os retries se concentram em {} endereço(s) numa faixa de {}% do disco — compatível com região fraca da mídia{hot}",
            int(counts.len() as u64),
            decimal(spread, 1)
        ))
    }
}

fn rule_communication(ctx: &DiskCtx) -> Option<Finding> {
    use EvidenceSource::*;
    let mut b = Builder::new(
        "comm.link",
        t("Communication failures, timeouts or device hangs", "Falhas de comunicação, timeouts ou travamentos do dispositivo"),
    );
    if let Some(s) = ctx.smart {
        if let Some(n) = s.interface_crc_errors.filter(|&n| n > 0) {
            b.add(
                Smart,
                2,
                Severity::Warning,
                tr!(
                    "Interface CRC errors (199): {} — data corrupted on the cable/connector/port path",
                    "Erros CRC na interface (199): {} — dados corrompidos no caminho cabo/conector/porta",
                    int(n)
                ),
            );
        }
        if let Some(n) = s.vendor_crc_errors.filter(|&n| n > 0) {
            let sev = if n >= 10 { Severity::Attention } else { Severity::Info };
            b.add(
                Smart,
                1,
                sev,
                tr!(
                    "Vendor-specific CRC error counter: {} (the meaning varies by model; what matters is whether it keeps rising)",
                    "Contador de erros CRC específico do fabricante: {} (o significado varia por modelo; o importante é se continua aumentando)",
                    int(n)
                ),
            );
        }
        if let Some(n) = s.sata_phy_errors.filter(|&n| n > 0) {
            b.add(Smart, 1, Severity::Attention, tr!("SATA PHY errors (168): {}", "Erros de PHY SATA (168): {}", int(n)));
        }
        if let Some(n) = s.phy_interface_errors.filter(|&n| n > 0) {
            b.add(
                Smart,
                2,
                Severity::Warning,
                tr!(
                    "SATA Phy log: {} transmission errors (CRC/R_ERR) since the drive was powered on",
                    "Log SATA Phy: {} erros de transmissão (CRC/R_ERR) desde que o disco foi ligado",
                    int(n)
                ),
            );
        }
        if let Some(n) = s.command_timeouts.filter(|&n| n > 0) {
            // A few timeouts over a drive's whole life are common (e.g. power losses).
            let (w, sev) = match n {
                10.. => (2, Severity::Warning),
                3.. => (1, Severity::Attention),
                _ => (1, Severity::Info),
            };
            b.add(Smart, w, sev, tr!("Command timeouts (188): {} over the drive's lifetime", "Timeouts de comando (188): {} no total da vida do disco", int(n)));
        }
        if let Some(n) = s.end_to_end_errors.filter(|&n| n > 0) {
            b.add(Smart, 2, Severity::Warning, tr!("End-to-end integrity errors (184): {}", "Erros de integridade ponta a ponta (184): {}", int(n)));
        }
        if !s.is_nvme() && ata_error_log_kind(s) == AtaLogKind::Interface {
            if let Some(n) = s.error_log_entries.filter(|&n| n > 0) {
                b.add(
                    Smart,
                    2,
                    Severity::Attention,
                    tr!(
                        "ATA error log with {} ICRC entries (interface CRC error) — typical of a SATA cable or connection; the log covers the drive's whole life, what matters is whether it keeps growing",
                        "Log de erros ATA com {} registros do tipo ICRC (erro de CRC na interface) — típico de cabo ou conexão SATA; o log é da vida inteira do disco, o que importa é se continua crescendo",
                        int(n)
                    ),
                );
            }
        }
    }
    ctx.add_growth(&mut b, "error_log_entries", 2, Severity::Warning, t(" — new errors in the drive log", " — erros novos no log do disco"));
    ctx.add_growth(&mut b, "interface_crc_errors", 3, Severity::Warning, t(" — active link problem", " — problema de link ativo"));
    for key in ["vendor_crc_errors", "sata_phy_errors", "command_timeouts"] {
        ctx.add_growth(&mut b, key, 2, Severity::Warning, t(" — active problem", " — problema ativo"));
    }
    ctx.add_events(&mut b, EventKind::DeviceReset, 3, Severity::Warning, "");
    ctx.add_events(&mut b, EventKind::DeviceTimeout, 3, Severity::Warning, "");
    ctx.add_events(&mut b, EventKind::ControllerError, 2, Severity::Warning, "");
    ctx.add_events(&mut b, EventKind::IoRetried, 2, Severity::Warning, "");
    if let Some(text) = retry_lba_pattern(ctx) {
        b.add(Events, 1, Severity::Attention, text);
    }
    ctx.add_events(&mut b, EventKind::DeviceNotReady, 1, Severity::Attention, "");
    ctx.add_events(&mut b, EventKind::SurpriseRemoval, 3, Severity::Critical, "");

    // Controller resets/timeouts with no identified drive, in the same period as this
    // drive's I/O events.
    let own_io: Vec<DateTime<Utc>> = ctx.events.iter().filter(|e| e.category.is_storage_io()).map(|e| e.time).collect();
    let near: Vec<&StorageEvent> = ctx
        .all_events
        .iter()
        .filter(|e| e.disk_number.is_none() && matches!(e.kind, EventKind::DeviceReset | EventKind::DeviceTimeout | EventKind::ControllerError))
        .filter(|e| own_io.iter().any(|t| (*t - e.time).num_minutes().abs() <= 30))
        .collect();
    if let Some(e) = near.first() {
        b.add(
            Events,
            2,
            Severity::Warning,
            tr!(
                "{} with no identified drive ({}, {}) happened within 30 min of I/O events of this drive",
                "{} sem disco identificado ({}, {}) ocorreu a menos de 30 min de eventos de E/S deste disco",
                e.code(),
                e.kind.description(),
                local_datetime(e.time)
            ),
        );
    }

    if let Some(scan) = ctx.scan {
        let m = &scan.metrics;
        if m.timeout_blocks > 0 {
            b.add(
                Scan,
                3,
                Severity::Warning,
                tr!(
                    "Surface Scan: {} exceeded {} ms and were cancelled",
                    "Surface Scan: {} excederam {} ms e foram canceladas",
                    plural(m.timeout_blocks, t("read", "leitura"), t("reads", "leituras")),
                    int(scan.config.io_timeout_ms)
                ),
            );
        }
        if m.transient_errors > 0 {
            b.add(
                Scan,
                2,
                Severity::Warning,
                tr!(
                    "Surface Scan: {} that went away on re-read — typical of a communication failure, not of a bad sector",
                    "Surface Scan: {} que sumiram na releitura — típico de falha de comunicação, não de setor ruim",
                    plural(m.transient_errors, t("error", "erro"), t("errors", "erros"))
                ),
            );
        }
        match (m.abort_kind(), &m.status) {
            (Some(AbortKind::Hung), _) => b.add(
                Scan,
                5,
                Severity::Critical,
                t("Surface Scan: the device stopped responding during the read", "Surface Scan: o dispositivo parou de responder durante a leitura"),
            ),
            (Some(AbortKind::Fatal), ScanStatus::Aborted(reason)) => {
                b.add(Scan, 4, Severity::Critical, tr!("Surface Scan stopped: {reason}", "Surface Scan interrompido: {reason}"))
            }
            _ => {}
        }
    }
    let sources: BTreeSet<_> = b.evidence.iter().map(|e| e.source).collect();
    if b.total() >= 8 && sources.len() >= 2 {
        b.severity = b.severity.max(Severity::Critical);
    }
    // Many retries/resets with zero CRC: the link is probably fine and it is the drive that pauses.
    let device_side = ctx.disk.bus != BusKind::Nvme
        && ctx.disk.bus != BusKind::Usb
        && ctx.smart.is_some_and(crc_counters_clean)
        && b.evidence.iter().any(|e| e.source == Events && e.weight >= 2);
    if device_side {
        b.add(
            Smart,
            0,
            Severity::Info,
            t(
                "The drive has never logged an interface CRC error. A bad cable or connector almost always makes that counter rise; \
                 with many retries and zero CRC, the most likely culprit is the drive itself pausing (firmware/controller) or the power, not the data cable",
                "O disco não registrou nenhum erro de CRC na interface na vida inteira. Cabo ou conector ruim quase sempre faz esse contador subir; \
                 com muitos retries e CRC zerado, o mais provável é o próprio disco pausar (firmware/controlador) ou a alimentação, e não o cabo de dados",
            ),
        );
    }
    let (causes, recs) = if device_side { device_stall_causes_and_recs() } else { comm_causes_and_recs(ctx.disk) };
    b.causes(&causes).recs(&recs).finish(Some(ctx.disk.number))
}

/// Causes when the link looks fine (zero CRC) but the drive pauses or stops responding.
fn device_stall_causes_and_recs() -> (Vec<&'static str>, Vec<&'static str>) {
    (
        vec![
            t(
                "Internal pauses of the drive itself (firmware, controller, garbage collection) that exceed the Windows timeout — common on SSDs without DRAM cache",
                "Pausas internas do próprio disco (firmware, controlador, coleta de lixo) que estouram o tempo limite do Windows — comum em SSDs sem cache DRAM",
            ),
            t("Drive firmware with a known defect", "Firmware do disco com defeito conhecido"),
            t("Unstable power (power cable, adapter or PSU)", "Alimentação instável (cabo de energia, adaptador ou fonte)"),
            t("SATA port, controller or driver (AHCI/RST)", "Porta SATA, controladora ou driver (AHCI/RST)"),
            t("SATA data cable (less likely: the drive logged no CRC errors)", "Cabo SATA de dados (menos provável: o disco não registrou erros de CRC)"),
        ],
        vec![
            t("Back up the important data on this drive", "Faça backup dos dados importantes deste disco"),
            t("Update the drive firmware with the manufacturer's tool", "Atualize o firmware do disco com a ferramenta do fabricante"),
            t(
                "Swap the power connector (no adapters/extensions) and the SATA port — cheap tests to rule them out",
                "Troque o conector de energia (sem adaptadores/extensões) e a porta SATA — testes baratos para descartar",
            ),
            t(
                "Record each change with `diskdoctor history note` and compare later with `diskdoctor history`",
                "Registre cada mudança com `diskdoctor history note` e compare depois com `diskdoctor history`",
            ),
            t(
                "If the retries continue after that, the drive is the main suspect: plan to replace it",
                "Se os retries continuarem depois disso, o disco é o suspeito principal: planeje substituí-lo",
            ),
        ],
    )
}

fn rule_stalls(ctx: &DiskCtx) -> Option<Finding> {
    use EvidenceSource::*;
    let mut b = Builder::new("perf.stalls", t("Very slow reads or device pauses", "Leituras muito lentas ou pausas do dispositivo"));
    if let Some(scan) = ctx.scan {
        let m = &scan.metrics;
        let above = m.latency.count_above(500);
        let reads = |n| plural(n, t("read", "leitura"), t("reads", "leituras"));
        if above >= 20 {
            b.add(
                Scan,
                3,
                Severity::Warning,
                tr!("{} took over 500 ms (max {})", "{} levaram mais de 500 ms (máximo {})", reads(above), format_ms(m.latency.max_ms)),
            );
        } else if above >= 1 {
            b.add(
                Scan,
                2,
                Severity::Attention,
                tr!("{} took over 500 ms (max {})", "{} levaram mais de 500 ms (máximo {})", reads(above), format_ms(m.latency.max_ms)),
            );
        } else if m.latency.max_ms >= 150.0 {
            b.add(Scan, 1, Severity::Attention, tr!("Read latency peak of {}", "Pico de latência de leitura de {}", format_ms(m.latency.max_ms)));
        }
        if let (Some(min), Some(median)) = (m.min_segment_mb_per_s, m.median_segment_mb_per_s) {
            if median > 0.0 && min < median * 0.35 {
                b.add(
                    Scan,
                    1,
                    Severity::Attention,
                    tr!(
                        "Speed dropped to {} in part of the drive (median {})",
                        "A velocidade caiu para {} em parte do disco (mediana {})",
                        format_mbps(min),
                        format_mbps(median)
                    ),
                );
            }
        }
    }
    if let Some(read) = ctx.bench.and_then(|bn| bn.read.as_ref()) {
        if let Some(r) = read.random.as_ref().filter(|r| r.latency.max_ms >= 1000.0) {
            b.add(
                Benchmark,
                2,
                Severity::Warning,
                tr!("Benchmark: a Random 4 KiB read reached {}", "Benchmark: leitura Random 4 KiB chegou a {}", format_ms(r.latency.max_ms)),
            );
        }
    }
    if let Some(w) = ctx.windows {
        if let Some(ms) = w.read_latency_max_ms {
            let text = tr!(
                "Windows recorded a maximum read latency of {}",
                "O Windows registrou latência máxima de leitura de {}",
                format_ms(ms as f64)
            );
            if ms >= 2000 {
                b.add(WindowsCounters, 2, Severity::Warning, text);
            } else if ms >= 500 {
                b.add(WindowsCounters, 1, Severity::Attention, text);
            }
        }
        for (label, v) in [(t("write", "gravação"), w.write_latency_max_ms), ("flush", w.flush_latency_max_ms)] {
            if let Some(ms) = v.filter(|&ms| ms >= 2000) {
                b.add(
                    WindowsCounters,
                    1,
                    Severity::Attention,
                    tr!(
                        "Windows recorded a maximum {label} latency of {}",
                        "O Windows registrou latência máxima de {label} de {}",
                        format_ms(ms as f64)
                    ),
                );
            }
        }
    }
    let causes: Vec<&str> = if ctx.disk.is_ssd() {
        vec![
            t(
                "Old data requiring heavy error correction (flash data-retention loss)",
                "Dados antigos exigindo correção de erros intensiva (perda de retenção da memória flash)",
            ),
            t("SSD firmware/controller (garbage-collection pauses, exhausted cache)", "Firmware/controlador do SSD (pausas de coleta de lixo, cache esgotado)"),
            t("Overheating (throttling)", "Superaquecimento (throttling)"),
        ]
    } else {
        vec![
            t("Weak sectors that need several read attempts", "Setores fracos que exigem várias tentativas de leitura"),
            t("Mechanical or head wear", "Desgaste mecânico ou do cabeçote"),
            t("Communication problem (see the link findings)", "Problema de comunicação (ver achados de link)"),
        ]
    };
    let mut b = b.causes(&causes).recs(&[t(
        "Repeat the test: slowness at the same positions points to the media; at random positions, to firmware, temperature or communication",
        "Repita o teste: lentidão nas mesmas posições indica mídia; em posições aleatórias aponta para firmware, temperatura ou comunicação",
    )]);
    if ctx.disk.is_ssd() {
        b.recs.push(t("Update the SSD firmware", "Atualize o firmware do SSD").into());
    }
    b.finish(Some(ctx.disk.number))
}

fn rule_temperature(ctx: &DiskCtx) -> Option<Finding> {
    let s = ctx.smart;
    let (warn, crit) = if ctx.disk.bus == BusKind::Nvme {
        (s.and_then(|s| s.temp_warning_threshold_c).unwrap_or(70), s.and_then(|s| s.temp_critical_threshold_c).unwrap_or(80))
    } else if ctx.disk.is_ssd() {
        (65, 75)
    } else if ctx.disk.is_hdd() {
        (50, 55)
    } else {
        (55, 65)
    };
    let mut b = Builder::new("temp.high", t("High temperature", "Temperatura elevada"));
    let current = s.and_then(|s| s.temperature_c).or(ctx.windows.and_then(|w| w.temperature_c.map(|t| t as i64)));
    let source = if s.and_then(|s| s.temperature_c).is_some() { EvidenceSource::Smart } else { EvidenceSource::WindowsCounters };
    if let Some(temp) = current {
        if temp >= crit {
            b.add(source, 3, Severity::Critical, tr!("Current temperature {temp} °C (critical limit {crit} °C)", "Temperatura atual {temp} °C (limite crítico {crit} °C)"));
        } else if temp >= warn {
            b.add(source, 2, Severity::Warning, tr!("Current temperature {temp} °C (warning limit {warn} °C)", "Temperatura atual {temp} °C (limite de aviso {warn} °C)"));
        }
    }
    let max = s.and_then(|s| s.temperature_max_c).or(ctx.windows.and_then(|w| w.temperature_max_c.map(|t| t as i64)));
    if let Some(m) = max.filter(|&m| m >= crit) {
        b.add(EvidenceSource::Smart, 1, Severity::Attention, tr!("Highest temperature ever recorded: {m} °C", "Temperatura máxima já registrada: {m} °C"));
    }
    let minutes = |n| plural(n, t("minute", "minuto"), t("minutes", "minutos"));
    if let Some(min) = s.and_then(|s| s.nvme_critical_temp_minutes).filter(|&m| m > 0) {
        b.add(
            EvidenceSource::Smart,
            2,
            Severity::Warning,
            tr!("Has run {} above the critical temperature", "Já operou {} acima da temperatura crítica", minutes(min)),
        );
    }
    if let Some(min) = s.and_then(|s| s.nvme_warning_temp_minutes).filter(|&m| m > 0) {
        b.add(
            EvidenceSource::Smart,
            1,
            Severity::Attention,
            tr!("Has run {} above the warning temperature", "Já operou {} acima da temperatura de aviso", minutes(min)),
        );
    }
    b.causes(&[
        t("Insufficient airflow in the case", "Ventilação insuficiente no gabinete"),
        t("NVMe SSD without a heatsink or close to the graphics card", "SSD NVMe sem dissipador ou perto da placa de vídeo"),
        t("Accumulated dust", "Poeira acumulada"),
    ])
    .recs(&[
        t("Improve the case airflow", "Melhore o fluxo de ar do gabinete"),
        t("On NVMe, use a heatsink; on HDDs, make sure there is front airflow", "Em NVMe, use dissipador; em HD, garanta ventilação frontal"),
        t("High temperature speeds up wear and can cause throttling or hangs", "Temperatura alta acelera desgaste e pode causar throttling ou travamentos"),
    ])
    .finish(Some(ctx.disk.number))
}

fn rule_wear(ctx: &DiskCtx) -> Option<Finding> {
    if !ctx.disk.is_ssd() {
        return None;
    }
    let mut b = Builder::new("ssd.wear", t("Advanced flash wear", "Desgaste avançado da memória flash"));
    let wear = ctx.smart.and_then(|s| s.wear_used_percent).or(ctx.windows.and_then(|w| w.wear_percent));
    if let Some(w) = wear {
        let text = tr!("Endurance used: {w}%", "Vida útil consumida: {w}%");
        if w >= 90 {
            b.add(EvidenceSource::Smart, 3, Severity::Critical, text);
        } else if w >= 75 {
            b.add(EvidenceSource::Smart, 2, Severity::Warning, text);
        }
    }
    if let Some(s) = ctx.smart {
        if let (Some(spare), Some(th)) = (s.available_spare_percent, s.available_spare_threshold) {
            if th > 0 && spare <= th {
                b.add(
                    EvidenceSource::Smart,
                    4,
                    Severity::Critical,
                    tr!("Available spare {spare}% reached the {th}% threshold", "Reserva disponível {spare}% atingiu o limite de {th}%"),
                );
            }
        }
    }
    b.causes(&[t("Accumulated writes close to the SSD's rated endurance", "Volume de gravação acumulado próximo da resistência nominal do SSD")])
        .recs(&[
            t("Plan the replacement", "Planeje a substituição"),
            t(
                "Keep backups current: SSDs at end of life may switch to read-only mode or fail suddenly",
                "Mantenha backup atualizado: SSDs no fim da vida podem entrar em modo somente leitura ou falhar de repente",
            ),
        ])
        .finish(Some(ctx.disk.number))
}

fn rule_unsafe_shutdowns(ctx: &DiskCtx) -> Option<Finding> {
    let mut b = Builder::new("power.unsafe", t("Frequent unsafe shutdowns", "Desligamentos bruscos frequentes"));
    if let Some(s) = ctx.smart {
        if let (Some(u), Some(pc)) = (s.unsafe_shutdowns, s.power_cycles.filter(|&p| p > 0)) {
            let ratio = u as f64 / pc as f64;
            if u >= 50 && ratio >= 0.05 {
                b.add(
                    EvidenceSource::Smart,
                    1,
                    Severity::Attention,
                    tr!(
                        "{} unsafe shutdowns in {} power cycles ({}%)",
                        "{} desligamentos bruscos em {} ciclos de energia ({}%)",
                        int(u),
                        int(pc),
                        decimal(ratio * 100.0, 1)
                    ),
                );
            }
        }
    }
    if let Some(c) = ctx.delta.and_then(|d| d.change("unsafe_shutdowns")).filter(|c| c.increase() >= 3) {
        let since = ctx.delta.map(|d| local_date(d.baseline_time)).unwrap_or_default();
        b.add(
            EvidenceSource::History,
            1,
            Severity::Attention,
            tr!("{} unsafe shutdowns since {since}", "{} desligamentos bruscos desde {since}", int(c.increase())),
        );
    }
    b.causes(&[
        t("Hangs that required holding the power button", "Travamentos que exigiram desligar pelo botão"),
        t("Power outages", "Quedas de energia"),
        t("Unstable power supply", "Fonte de alimentação instável"),
    ])
    .recs(&[
        t(
            "Unsafe shutdowns can corrupt data in flight (especially on SSDs without power-loss protection)",
            "Desligamentos bruscos podem corromper dados em trânsito (especialmente em SSDs sem proteção contra queda de energia)",
        ),
        t("If the cause is a hang, see this drive's communication/hang findings", "Se a causa for travamento, veja os achados de comunicação/travamento deste disco"),
        t("Consider a UPS", "Considere um nobreak"),
    ])
    .finish(Some(ctx.disk.number))
}

fn rule_filesystem(ctx: &DiskCtx) -> Option<Finding> {
    // File system corruption is usually a consequence (unsafe shutdown, failed I/O), not
    // proof of a drive defect: it only becomes "Warning" when it recurs.
    let corruption_sev = match summarize(&ctx.events, EventKind::FsCorruption) {
        Some(s) if s.count >= 5 => Severity::Warning,
        _ => Severity::Attention,
    };
    let mut b = Builder::new("fs.corruption", t("File system inconsistencies", "Inconsistências no sistema de arquivos"));
    ctx.add_events(&mut b, EventKind::FsCorruption, 2, corruption_sev, "");
    ctx.add_events(&mut b, EventKind::VolumeNeedsRepair, 1, Severity::Attention, "");
    ctx.add_events(&mut b, EventKind::FlushFailed, 2, Severity::Warning, t(" — Windows could not write data to the drive", " — o Windows não conseguiu gravar dados no disco"));
    ctx.add_events(&mut b, EventKind::TornWrite, 1, Severity::Attention, "");

    // Which partitions the events cite (by volume GUID, device or letter).
    let mut affected: BTreeSet<String> = BTreeSet::new();
    let mut hidden = false;
    for e in ctx.events.iter().filter(|e| e.category == crate::events::EventCategory::FileSystem) {
        let text = format!("{} {}", e.properties.join(" "), e.message);
        let part = crate::inventory::partition_for_reference(ctx.disk, &text).or_else(|| {
            ctx.disk.partitions.iter().find(|p| {
                p.drive_letter.as_ref().is_some_and(|l| e.properties.iter().any(|x| x.trim().eq_ignore_ascii_case(&format!("{l}:"))))
            })
        });
        if let Some(p) = part {
            hidden |= p.drive_letter.is_none();
            affected.insert(p.display_name());
        }
    }
    if !affected.is_empty() {
        b.add(
            EvidenceSource::Events,
            0,
            Severity::Info,
            tr!("Volumes cited: {}", "Volumes citados: {}", affected.iter().cloned().collect::<Vec<_>>().join(", ")),
        );
    }
    let letters: Vec<String> = affected.iter().filter(|a| a.len() == 2 && a.ends_with(':')).cloned().collect();
    let check = if !letters.is_empty() {
        tr!(
            "Check in read-only mode: {} (without /f)",
            "Verifique em modo somente leitura: {} (sem /f)",
            letters.iter().map(|l| format!("`chkdsk {l}`")).collect::<Vec<_>>().join(", ")
        )
    } else {
        tr!(
            "Check the affected volume in read-only mode with `chkdsk <letter>:` (without /f)",
            "Verifique o volume afetado em modo somente leitura com `chkdsk <letra>:` (sem /f)"
        )
    };
    let mut b = b.causes(&[
        t("Consequence of unsafe shutdowns or hangs", "Consequência de desligamentos bruscos ou travamentos"),
        t("Drive I/O errors causing data to be written or read incorrectly", "Erros de E/S do disco fazendo com que dados fossem gravados ou lidos incorretamente"),
    ]);
    b.recs.push(check);
    if hidden {
        b.recs.push(
            t(
                "Partitions without a letter (recovery/system) are checked by Windows at startup; if the warning persists, \
                 assign a temporary letter in Disk Management to run chkdsk",
                "Partições sem letra (recuperação/sistema) são verificadas pelo Windows na inicialização; se o aviso persistir, \
                 atribua uma letra temporária no Gerenciamento de Disco para rodar o chkdsk",
            )
            .into(),
        );
    }
    b.recs.push(t("Repair with `chkdsk <letter>: /f` only after a backup", "Corrija com `chkdsk <letra>: /f` somente depois do backup").into());
    b.finish(Some(ctx.disk.number))
}

/// This drive's I/O events in the 30 min before unexpected reboots.
fn rule_freeze_correlation(ctx: &DiskCtx) -> Option<Finding> {
    // A boot after a hang logs Kernel-Power 41 and, seconds later, EventLog 6008.
    // The 41 is used; a 6008 only counts when there is no 41 of the same boot nearby.
    let times = |kind| ctx.all_events.iter().filter(move |e| e.kind == kind).map(|e| e.time);
    let mut reboots: Vec<DateTime<Utc>> = times(EventKind::UnexpectedReboot).collect();
    let shutdowns: Vec<DateTime<Utc>> = times(EventKind::UnexpectedShutdown).filter(|t| !reboots.iter().any(|r| (*t - *r).num_minutes().abs() <= 2)).collect();
    reboots.extend(shutdowns);
    reboots.sort();
    if reboots.is_empty() {
        return None;
    }
    let window = ChronoDuration::minutes(30);
    let mut hits = Vec::new();
    for r in &reboots {
        let n = ctx.events.iter().filter(|e| e.category.is_storage_io() && e.time < *r && e.time >= *r - window).count();
        if n > 0 {
            hits.push((*r, n));
        }
    }
    let mut b = Builder::new(
        "freeze.correlation",
        t("Problems on this drive right before hangs/reboots", "Problemas deste disco logo antes de travamentos/reinicializações"),
    );
    if !hits.is_empty() {
        hits.sort_by_key(|h| std::cmp::Reverse(h.0));
        let examples = hits
            .iter()
            .take(3)
            .map(|(t_, n)| tr!("{} ({} before)", "{} ({} antes)", local_datetime(*t_), plural(*n as u64, t("event", "evento"), t("events", "eventos"))))
            .collect::<Vec<_>>()
            .join("; ");
        let weight = if hits.len() >= 2 { 3 } else { 2 };
        b.add(
            EvidenceSource::Events,
            weight,
            Severity::Warning,
            tr!(
                "{} of {} unexpected reboots were preceded, within 30 min, by I/O events of this drive: {examples}",
                "{} de {} reinicializações inesperadas foram precedidas, em até 30 min, por eventos de E/S deste disco: {examples}",
                hits.len(),
                reboots.len()
            ),
        );
    }
    let mut b = b
        .causes(&[t(
            "The drive (or its link) stopped responding and the system froze — with the drive hung, Windows often cannot even log the error",
            "O disco (ou o link com ele) parou de responder e o sistema congelou — com o disco travado, o Windows muitas vezes nem consegue registrar o erro",
        )])
        .recs(&[t(
            "Try another cable/port (or M.2 slot) and watch whether the hangs stop",
            "Teste com outro cabo/porta (ou slot M.2) e observe se os travamentos param",
        )]);
    if ctx.disk.is_boot || ctx.disk.is_system {
        b.recs.insert(
            0,
            t("This is the system drive: treat it as a priority and back up your data", "Este é o disco do sistema: trate como prioridade e faça backup dos dados").into(),
        );
    }
    b.finish(Some(ctx.disk.number))
}

// ---------------------------------------------------------------------------
// Global findings and verdict
// ---------------------------------------------------------------------------

fn global_findings(input: &AnalysisInput) -> Vec<Finding> {
    let mut out = Vec::new();
    let unattributed: Vec<&StorageEvent> = input.events.iter().filter(|e| e.disk_number.is_none() && e.category.is_storage_io()).collect();
    if !unattributed.is_empty() {
        let mut b = Builder::new(
            "global.controller",
            t("Controller/storage events with no identified drive", "Eventos de controladora/armazenamento sem disco identificado"),
        );
        let mut by_kind: BTreeMap<EventKind, Vec<&StorageEvent>> = BTreeMap::new();
        for e in &unattributed {
            by_kind.entry(e.kind).or_default().push(e);
        }
        for (kind, list) in by_kind {
            let s = summarize(&list, kind).expect("non-empty list");
            let sev = if matches!(kind, EventKind::DeviceReset | EventKind::DeviceTimeout | EventKind::SurpriseRemoval) { Severity::Warning } else { Severity::Attention };
            b.add(EvidenceSource::Events, event_weight(2, &s), sev, event_text(&s, kind, input.event_window_hours));
        }
        if let Some(f) = b
            .causes(&[t(
                "A drive on that controller stopped responding; with several drives on the same driver the event alone cannot tell which",
                "Algum disco ligado a essa controladora deixou de responder; com vários discos no mesmo driver não dá para saber qual só pelo evento",
            )])
            .recs(&[t(
                "Compare the times with the events attributed to each drive (the drive's communication finding cites coincidences)",
                "Compare os horários com os eventos atribuídos a cada disco (o achado de comunicação do disco cita coincidências)",
            )])
            .finish(None)
        {
            out.push(f);
        }
    }
    let whea: Vec<&StorageEvent> = input.events.iter().filter(|e| e.kind == EventKind::WheaHardwareError).collect();
    if let Some(s) = summarize(&whea, EventKind::WheaHardwareError) {
        let mut b = Builder::new("global.whea", t("Hardware errors reported by WHEA", "Erros de hardware reportados pelo WHEA"));
        b.add(EvidenceSource::Events, 1, Severity::Attention, event_text(&s, EventKind::WheaHardwareError, input.event_window_hours));
        if let Some(f) = b
            .causes(&[t("PCIe bus errors (can affect NVMe SSDs), CPU or memory", "Erros no barramento PCIe (pode afetar SSDs NVMe), CPU ou memória")])
            .recs(&[
                t("If there is an NVMe SSD, check its slot and PCIe power management", "Se houver SSD NVMe, confira o slot e o gerenciamento de energia do PCIe"),
                t("Update BIOS/UEFI and chipset drivers", "Atualize BIOS/UEFI e drivers do chipset"),
            ])
            .finish(None)
        {
            out.push(f);
        }
    }
    let shutdowns: Vec<&StorageEvent> = input.events.iter().filter(|e| e.kind == EventKind::UnexpectedReboot).collect();
    if let Some(s) = summarize(&shutdowns, EventKind::UnexpectedReboot) {
        let mut b = Builder::new("global.unexpected_reboots", t("Unexpected reboots in the period", "Reinicializações inesperadas no período"));
        let sev = if s.count >= 3 { Severity::Attention } else { Severity::Info };
        b.add(EvidenceSource::Events, 1, sev, event_text(&s, EventKind::UnexpectedReboot, input.event_window_hours));
        if let Some(f) = b
            .causes(&[t(
                "Hangs that required a reset, power outages, PSU, overheating or drivers — not necessarily the drive",
                "Travamentos que exigiram reset, quedas de energia, fonte, superaquecimento ou drivers — não necessariamente o disco",
            )])
            .recs(&[t(
                "Check each drive's findings for I/O events right before these reboots",
                "Veja nos achados de cada disco se houve eventos de E/S logo antes dessas reinicializações",
            )])
            .finish(None)
        {
            out.push(f);
        }
    }
    out
}

fn sort_findings(v: &mut [Finding]) {
    v.sort_by(|a, b| b.severity.cmp(&a.severity).then(b.confidence.cmp(&a.confidence)));
}

fn analyze_disk(input: &AnalysisInput, disk: &DiskInfo) -> DiskVerdict {
    let health = input.health.iter().find(|h| h.disk_number == disk.number);
    let smart = health.and_then(|h| h.smart_ok());
    let scan = input.scans.iter().filter(|s| s.disk.fingerprint == disk.fingerprint).max_by_key(|s| s.finished_at);
    let bench = input.benchmarks.iter().filter(|s| s.disk.fingerprint == disk.fingerprint).max_by_key(|s| s.finished_at);
    let capacity = input.capacity.iter().filter(|c| c.disk.fingerprint == disk.fingerprint).max_by_key(|c| c.finished_at);
    let change = input.changes.get(&disk.fingerprint);
    let ctx = DiskCtx {
        disk,
        smart,
        windows: health.and_then(|h| h.windows.as_ref()),
        delta: health.and_then(|h| h.history.as_ref()),
        events: input
            .events
            .iter()
            .filter(|e| e.disk_number == Some(disk.number) && change.is_none_or(|c| e.time >= c.time))
            .collect(),
        all_events: input.events,
        scan,
        bench,
        capacity,
        hours: input.event_window_hours,
    };

    let mut findings: Vec<Finding> = [
        rule_capacity(&ctx),
        rule_smart_failed(&ctx),
        rule_media(&ctx),
        rule_communication(&ctx),
        rule_stalls(&ctx),
        rule_temperature(&ctx),
        rule_wear(&ctx),
        rule_unsafe_shutdowns(&ctx),
        rule_filesystem(&ctx),
        rule_freeze_correlation(&ctx),
    ]
    .into_iter()
    .flatten()
    .collect();
    sort_findings(&mut findings);

    let no_data = || t("no data", "sem dados").to_string();
    let mut sources = Vec::new();
    let mut gaps = Vec::new();
    match (smart, health.and_then(|h| h.smart.as_ref())) {
        (Some(s), _) => sources.push(s.source.clone()),
        (None, _) if disk.is_flash_media() => gaps.push(
            t(
                "USB flash drives and memory cards have no SMART: Surface Scan (`scan`) and Capacity Verification (`capacity`) are the available diagnostics",
                "Pendrives e cartões de memória não têm SMART: Surface Scan (`scan`) e Capacity Verification (`capacity`) são os diagnósticos possíveis",
            )
            .into(),
        ),
        (None, Some(s)) if disk.is_external() => gaps.push(tr!(
            "SMART unavailable ({}): many USB enclosures/adapters do not pass the drive's SMART through",
            "SMART indisponível ({}): muitos cases/adaptadores USB não repassam o SMART do disco",
            s.error.clone().unwrap_or_else(no_data)
        )),
        (None, Some(s)) => gaps.push(tr!("SMART unavailable: {}", "SMART indisponível: {}", s.error.clone().unwrap_or_else(no_data))),
        (None, None) => gaps.push(t("SMART not queried (smartctl not found or not run)", "SMART não consultado (smartctl não encontrado ou não executado)").into()),
    }
    if ctx.windows.is_some() {
        sources.push(t("Windows reliability counters", "Contadores de confiabilidade do Windows").into());
    }
    if input.events_collected {
        sources.push(tr!("Windows events ({} h)", "Eventos do Windows ({} h)", input.event_window_hours));
    } else {
        gaps.push(t("Windows events not queried", "Eventos do Windows não consultados").into());
    }
    match scan {
        Some(s) => {
            sources.push(tr!("Surface Scan on {} ({})", "Surface Scan de {} ({})", local_date(s.finished_at), s.metrics.status.label()));
            let cap = s.metrics.capacity.max(1);
            let pct = s.metrics.bytes_processed as f64 * 100.0 / cap as f64;
            if pct < 99.0 {
                gaps.push(tr!("The last Surface Scan covered only {}% of the drive", "O último Surface Scan cobriu só {}% do disco", decimal(pct, 1)));
            }
        }
        None => gaps.push(tr!(
            "No full Surface Scan yet (`diskdoctor scan --disk {}`)",
            "Surface Scan completo ainda não executado (`diskdoctor scan --disk {}`)",
            disk.number
        )),
    }
    if bench.is_some() {
        sources.push("Benchmark".into());
    }
    if let Some(c) = capacity {
        sources.push(tr!("Capacity Verification on {} ({})", "Capacity Verification de {} ({})", local_date(c.finished_at), c.metrics.verdict.label()));
    }
    let mut change_line = None;
    if let Some(c) = change {
        let text = tr!(
            "Change recorded on {}: \"{}\" — only later events count toward the verdict (`diskdoctor history --disk {}` compares before × after)",
            "Mudança registrada em {}: \"{}\" — só eventos posteriores a ela entram no veredito (`diskdoctor history --disk {}` compara antes × depois)",
            local_datetime(c.time),
            c.text,
            disk.number
        );
        sources.push(text.clone());
        change_line = Some(text);
        let hours = (Utc::now() - c.time).num_hours();
        if hours < 24 {
            gaps.push(tr!(
                "The change is recent ({} h ago): use the computer normally and repeat the diagnosis in 1–3 days",
                "A mudança é recente (há {} h): use o computador normalmente e repita o diagnóstico em 1–3 dias",
                hours.max(0)
            ));
        }
    }
    match ctx.delta {
        Some(d) => sources.push(tr!("SMART history since {}", "Histórico SMART desde {}", local_date(d.baseline_time))),
        None if smart.is_some() => gaps.push(
            t(
                "No earlier history: run `diskdoctor health` periodically to detect counters that grow",
                "Sem histórico anterior: rode `diskdoctor health` periodicamente para detectar contadores que crescem",
            )
            .into(),
        ),
        None => {}
    }

    let worst = findings.iter().map(|f| f.severity).max();
    let has_data = smart.is_some() || scan.is_some() || ctx.windows.is_some() || capacity.is_some();
    let status = match worst {
        Some(Severity::Critical) => DiskStatus::Critical,
        Some(Severity::Warning) => DiskStatus::Warning,
        Some(Severity::Attention) => DiskStatus::Attention,
        Some(Severity::Info) | None if has_data || input.events_collected => DiskStatus::Healthy,
        _ => DiskStatus::Unknown,
    };
    let headline = match status {
        DiskStatus::Healthy => t(
            "No problems found in the analyzed sources (this does not guarantee the drive is healthy)",
            "Nenhum problema encontrado nas fontes analisadas (isso não garante que o disco esteja saudável)",
        )
        .into(),
        DiskStatus::Unknown => t("Not enough data to assess this drive", "Dados insuficientes para avaliar este disco").into(),
        _ => findings.first().map(|f| f.title.clone()).unwrap_or_default(),
    };
    DiskVerdict {
        disk_number: disk.number,
        label: format!("{} ({})", disk.title(), disk.class_label()),
        status,
        headline,
        findings,
        sources_used: sources,
        gaps,
        change: change_line,
    }
}

pub fn analyze(input: &AnalysisInput) -> AnalysisResult {
    let verdicts = input.disks.iter().map(|d| analyze_disk(input, d)).collect();
    let mut global = global_findings(input);
    sort_findings(&mut global);
    let mut notes = vec![
        t(
            "Normal SMART attributes do not guarantee a healthy drive: many failures happen with no SMART warning. \
             Combine them with a full Surface Scan and the event history.",
            "Atributos SMART normais não garantem que o disco está saudável: parte importante das falhas acontece sem aviso no SMART. \
             Combine com o Surface Scan completo e com o histórico de eventos.",
        )
        .to_string(),
        t(
            "When the system drive freezes, Windows often cannot write the event: no events does not rule out a drive problem.",
            "Quando o disco do sistema congela, o Windows muitas vezes não consegue gravar o evento: ausência de eventos não descarta problema no disco.",
        )
        .into(),
    ];
    if !input.elevated {
        notes.push(
            t(
                "Run without Administrator privileges: SMART data may be incomplete and Surface Scan is unavailable.",
                "Executado sem privilégios de Administrador: dados SMART podem estar incompletos e o Surface Scan não está disponível.",
            )
            .into(),
        );
    }
    if !input.events_collected {
        notes.push(t("Windows events were not queried in this run.", "Os eventos do Windows não foram consultados nesta execução.").into());
    }
    AnalysisResult { verdicts, global_findings: global, notes }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{attribute_events, parse_events_json};
    use crate::history::CounterChange;
    use crate::i18n::{Lang, scoped};
    use crate::inventory::tests::fixture_disks;
    use crate::io::testing::ScriptedReader;
    use crate::scan::{NoopObserver, ScanConfig, run_scan};
    use crate::smart::parse_smartctl_json;
    use std::sync::atomic::AtomicBool;

    fn health_with(disk: u32, smart: Option<SmartReport>) -> DiskHealth {
        DiskHealth { disk_number: disk, smart, windows: None, history: None }
    }

    fn scan_for(disk: &DiskInfo, reader: &mut ScriptedReader) -> ScanResult {
        let cfg = ScanConfig::default();
        let metrics = run_scan(reader, &cfg, &mut NoopObserver, &AtomicBool::new(false));
        ScanResult {
            tool_version: "test".into(),
            disk: disk.identity(),
            started_at: Utc::now(),
            finished_at: Utc::now(),
            elevated: true,
            config: cfg,
            metrics,
        }
    }

    fn input<'a>(disks: &'a [DiskInfo], health: &'a [DiskHealth], events: &'a [StorageEvent], scans: &'a [ScanResult]) -> AnalysisInput<'a> {
        AnalysisInput {
            disks,
            health,
            events,
            scans,
            benchmarks: &[],
            capacity: &[],
            changes: NO_CHANGES,
            event_window_hours: 168,
            events_collected: true,
            elevated: true,
        }
    }

    fn verdict(result: &AnalysisResult, n: u32) -> &DiskVerdict {
        result.verdicts.iter().find(|v| v.disk_number == n).unwrap()
    }

    #[test]
    fn confidence_levels() {
        let ev = |s, w| Evidence { source: s, weight: w, text: String::new() };
        assert_eq!(confidence_for(&[ev(EvidenceSource::Smart, 1)]), Confidence::Low);
        assert_eq!(confidence_for(&[ev(EvidenceSource::Smart, 3)]), Confidence::Medium);
        assert_eq!(confidence_for(&[ev(EvidenceSource::Scan, 5)]), Confidence::High);
        assert_eq!(confidence_for(&[ev(EvidenceSource::Smart, 3), ev(EvidenceSource::Smart, 3)]), Confidence::Medium, "a single source");
        assert_eq!(confidence_for(&[ev(EvidenceSource::Smart, 3), ev(EvidenceSource::Events, 3)]), Confidence::High);
    }

    #[test]
    fn real_event_pattern_flags_system_disk() {
        let _en = scoped(Lang::En);
        // Real fixture: 4× disk/153 on drive 2 minutes before Kernel-Power 41.
        let disks = fixture_disks();
        let mut events = parse_events_json(include_str!("../tests/fixtures/events.json")).unwrap();
        attribute_events(&mut events, &disks);
        let smart2 = parse_smartctl_json(2, include_str!("../tests/fixtures/smartctl-sata-ssd-kingston120.json")).unwrap();
        let health = vec![health_with(2, Some(smart2))];
        let result = analyze(&input(&disks, &health, &events, &[]));

        let v2 = verdict(&result, 2);
        assert!(v2.status >= DiskStatus::Warning, "status {:?}", v2.status);
        let comm = v2.findings.iter().find(|f| f.rule == "comm.link").expect("communication finding");
        assert!(comm.evidence.iter().any(|e| e.text.contains("disk/153")));
        let freeze = v2.findings.iter().find(|f| f.rule == "freeze.correlation").expect("correlation with a hang");
        assert!(freeze.recommendations[0].contains("system drive"));
        assert!(v2.findings.iter().any(|f| f.rule == "power.unsafe"), "219 unsafe shutdowns in 2943 cycles");
        assert!(v2.gaps.iter().any(|g| g.contains("scan --disk 2")));

        // NTFS 98 of volume 000000000104 belongs to drive 1.
        let v1 = verdict(&result, 1);
        assert!(v1.findings.iter().any(|f| f.rule == "fs.corruption"));
        // Always warns that normal SMART does not guarantee health.
        assert!(result.notes[0].contains("do not guarantee"));
        assert!(result.global_findings.iter().any(|f| f.rule == "global.unexpected_reboots"));
    }

    #[test]
    fn findings_are_written_in_the_active_language() {
        let disks = fixture_disks();
        let mut events = parse_events_json(include_str!("../tests/fixtures/events.json")).unwrap();
        attribute_events(&mut events, &disks);
        let run = |lang| {
            let _l = scoped(lang);
            analyze(&input(&disks, &[], &events, &[]))
        };
        let pt = run(Lang::PtBr);
        let comm = verdict(&pt, 2).findings.iter().find(|f| f.rule == "comm.link").unwrap();
        assert!(comm.title.starts_with("Falhas de comunicação"), "{}", comm.title);
        assert!(pt.notes[0].contains("Surface Scan"), "technical names stay in English: {}", pt.notes[0]);
        let en = run(Lang::En);
        let comm = verdict(&en, 2).findings.iter().find(|f| f.rule == "comm.link").unwrap();
        assert!(comm.title.starts_with("Communication failures"), "{}", comm.title);
        assert_eq!(verdict(&pt, 2).status, verdict(&en, 2).status, "the language never changes the verdict");
    }

    #[test]
    fn many_retries_with_clean_crc_point_to_the_disk_not_the_cable() {
        let _en = scoped(Lang::En);
        let disks = fixture_disks();
        let mut events = parse_events_json(include_str!("../tests/fixtures/events.json")).unwrap();
        attribute_events(&mut events, &disks);
        let smart2 = parse_smartctl_json(2, include_str!("../tests/fixtures/smartctl-sata-ssd-kingston120.json")).unwrap();
        assert_eq!(smart2.vendor_crc_errors.or(smart2.interface_crc_errors), Some(0), "fixture with zero CRC");
        let health = vec![health_with(2, Some(smart2))];
        let result = analyze(&input(&disks, &health, &events, &[]));
        let comm = verdict(&result, 2).findings.iter().find(|f| f.rule == "comm.link").unwrap().clone();
        assert!(comm.evidence.iter().any(|e| e.text.contains("never logged an interface CRC error")), "{:?}", comm.evidence);
        assert!(comm.possible_causes[0].contains("Internal pauses"));
        assert!(comm.possible_causes.last().unwrap().contains("less likely"));
    }

    #[test]
    fn icrc_error_log_is_communication_not_media() {
        let _en = scoped(Lang::En);
        let disks = fixture_disks();
        let mut smart = parse_smartctl_json(1, include_str!("../tests/fixtures/smartctl-sata-ssd-kingston240.json")).unwrap();
        smart.error_log_entries = Some(36);
        smart.error_log_samples = vec!["Error: ICRC, ABRT 16 sectors at LBA = 0x00000000 = 0".into(); 3];
        let health = vec![health_with(1, Some(smart))];
        let result = analyze(&input(&disks, &health, &[], &[]));
        let v = verdict(&result, 1);
        assert!(!v.findings.iter().any(|f| f.rule == "media.degradation"), "ICRC is not a media defect: {:?}", v.findings);
        let comm = v.findings.iter().find(|f| f.rule == "comm.link").unwrap();
        assert!(comm.evidence.iter().any(|e| e.text.contains("ICRC")));
        assert!(comm.possible_causes[0].contains("SATA cable"), "with CRC logged, the cable stays the first suspect");
    }

    #[test]
    fn failing_hdd_with_bad_scan_is_critical_with_high_confidence() {
        let _en = scoped(Lang::En);
        let mut disks = fixture_disks();
        disks.truncate(1);
        disks[0].media = crate::inventory::MediaKind::Hdd;
        let smart = parse_smartctl_json(0, include_str!("../tests/fixtures/smartctl-sata-hdd-failing.json")).unwrap();
        let mut reader = ScriptedReader::new(64 << 20);
        reader.bad_ranges.push((5 << 20, (5 << 20) + 4096));
        let scans = vec![scan_for(&disks[0], &mut reader)];
        let health = vec![health_with(0, Some(smart))];
        let result = analyze(&input(&disks, &health, &[], &scans));
        let v = verdict(&result, 0);
        assert_eq!(v.status, DiskStatus::Critical);
        let media = &v.findings[0];
        assert_eq!(media.rule, "media.degradation");
        assert_eq!(media.severity, Severity::Critical);
        assert_eq!(media.confidence, Confidence::High);
        assert!(media.evidence.iter().any(|e| e.source == EvidenceSource::Scan));
        assert!(media.possible_causes.iter().any(|c| c.contains("magnetic surface")));
        assert!(media.recommendations[0].contains("Back up"));
    }

    #[test]
    fn failing_now_attribute_is_strong_evidence() {
        let disks: Vec<DiskInfo> = fixture_disks().into_iter().take(1).collect();
        let smart = SmartReport {
            disk_number: 0,
            available: true,
            failing_attributes: vec!["5 Reallocated_Sector_Ct (FAILING_NOW)".into(), "190 Airflow_Temperature_Cel (In_the_past)".into()],
            ..Default::default()
        };
        let health = vec![health_with(0, Some(smart))];
        let result = analyze(&input(&disks, &health, &[], &[]));
        let f = verdict(&result, 0).findings.iter().find(|f| f.rule == "smart.failed").unwrap().clone();
        assert_eq!(f.severity, Severity::Critical);
        assert_eq!(f.evidence.iter().map(|e| e.weight).collect::<Vec<_>>(), vec![4, 1]);
    }

    #[test]
    fn clean_data_is_healthy_but_never_guaranteed() {
        let _en = scoped(Lang::En);
        let disks: Vec<DiskInfo> = fixture_disks().into_iter().take(1).collect();
        let smart = SmartReport { disk_number: 0, available: true, source: "smartctl".into(), overall_passed: Some(true), temperature_c: Some(35), ..Default::default() };
        let mut reader = ScriptedReader::new(32 << 20);
        let scans = vec![scan_for(&disks[0], &mut reader)];
        let health = vec![health_with(0, Some(smart))];
        let result = analyze(&input(&disks, &health, &[], &scans));
        let v = verdict(&result, 0);
        assert_eq!(v.status, DiskStatus::Healthy);
        assert!(v.findings.is_empty());
        assert!(v.headline.contains("does not guarantee"));
        let _pt = scoped(Lang::PtBr);
        let v = analyze(&input(&disks, &health, &[], &scans)).verdicts.remove(0);
        assert!(v.headline.contains("não garante"));
    }

    #[test]
    fn no_sources_means_unknown() {
        let disks: Vec<DiskInfo> = fixture_disks().into_iter().take(1).collect();
        let mut i = input(&disks, &[], &[], &[]);
        i.events_collected = false;
        let result = analyze(&i);
        assert_eq!(verdict(&result, 0).status, DiskStatus::Unknown);
    }

    #[test]
    fn growing_crc_plus_events_gives_high_confidence_link_problem() {
        let _en = scoped(Lang::En);
        let disks = fixture_disks();
        let smart = SmartReport { disk_number: 1, available: true, interface_crc_errors: Some(120), ..Default::default() };
        let history = SmartDelta {
            baseline_time: Utc::now() - ChronoDuration::days(10),
            snapshots: 3,
            changes: vec![CounterChange { key: "interface_crc_errors".into(), label: "Interface CRC errors (199)".into(), before: 20, after: 120 }],
        };
        let health = vec![DiskHealth { disk_number: 1, smart: Some(smart), windows: None, history: Some(history) }];
        let json = r#"[{"TimeUtc":"2026-11-08T10:00:00.0000000Z","Provider":"disk","Id":153,"Level":3,"RecordId":1,"Properties":["\\Device\\Harddisk1\\DR1","0x10","1"],"Message":"retried"}]"#;
        let mut events = parse_events_json(json).unwrap();
        attribute_events(&mut events, &disks);
        let result = analyze(&input(&disks, &health, &events, &[]));
        let comm = verdict(&result, 1).findings.iter().find(|f| f.rule == "comm.link").unwrap().clone();
        assert_eq!(comm.confidence, Confidence::High);
        assert!(comm.evidence.iter().any(|e| e.source == EvidenceSource::History && e.text.contains("rose from 20 to 120")));
        assert!(comm.recommendations.iter().any(|r| r.contains("SATA cable")));
    }

    #[test]
    fn transient_scan_errors_point_to_communication() {
        let _en = scoped(Lang::En);
        let disks: Vec<DiskInfo> = fixture_disks().into_iter().take(1).collect();
        let mut reader = ScriptedReader::new(32 << 20);
        reader.transient_bad.push((4 << 20, (4 << 20) + 512));
        reader.timeout_ranges.push((8 << 20, (8 << 20) + 1));
        let scans = vec![scan_for(&disks[0], &mut reader)];
        let result = analyze(&input(&disks, &[], &[], &scans));
        let v = verdict(&result, 0);
        let comm = v.findings.iter().find(|f| f.rule == "comm.link").unwrap();
        assert!(comm.evidence.iter().any(|e| e.text.contains("went away on re-read")));
        assert!(comm.evidence.iter().any(|e| e.text.contains("cancelled")));
        assert!(v.findings.iter().all(|f| f.rule != "media.degradation"), "a transient error is not a bad sector");
    }

    #[test]
    fn hung_scan_is_conclusive_even_for_results_saved_by_older_versions() {
        let disks: Vec<DiskInfo> = fixture_disks().into_iter().take(1).collect();
        let mut reader = ScriptedReader::new(16 << 20);
        reader.hang_at = Some(4 << 20);
        let mut scan = scan_for(&disks[0], &mut reader);
        assert_eq!(scan.metrics.abort, Some(AbortKind::Hung));
        let check = |scan: &ScanResult| {
            let result = analyze(&input(&disks, &[], &[], std::slice::from_ref(scan)));
            let comm = verdict(&result, 0).findings.iter().find(|f| f.rule == "comm.link").unwrap().clone();
            assert!(comm.evidence.iter().any(|e| e.weight == 5), "{:?}", comm.evidence);
        };
        check(&scan);
        // 0.2.x saved only the Portuguese reason.
        scan.metrics.abort = None;
        scan.metrics.status = ScanStatus::Aborted("o dispositivo parou de responder (nem o cancelamento da leitura foi concluído)".into());
        check(&scan);
    }

    #[test]
    fn hot_nvme_and_worn_ssd() {
        let mut disks: Vec<DiskInfo> = fixture_disks().into_iter().take(1).collect();
        disks[0].bus = BusKind::Nvme;
        let mut smart = parse_smartctl_json(0, include_str!("../tests/fixtures/smartctl-nvme-warning.json")).unwrap();
        smart.wear_used_percent = Some(93);
        let health = vec![health_with(0, Some(smart))];
        let result = analyze(&input(&disks, &health, &[], &[]));
        let v = verdict(&result, 0);
        assert!(v.findings.iter().any(|f| f.rule == "ssd.wear" && f.severity == Severity::Critical));
        assert!(v.findings.iter().any(|f| f.rule == "temp.high"), "ran above the warning temperature");
        assert!(v.findings.iter().any(|f| f.rule == "media.degradation"), "3 NVMe media errors");
        assert_eq!(v.status, DiskStatus::Critical);
    }

    fn retry_event(minutes_ago: i64, disk: u32, lba: u64) -> String {
        let t = (Utc::now() - ChronoDuration::minutes(minutes_ago)).to_rfc3339();
        format!(
            r#"{{"TimeUtc":"{t}","Provider":"disk","Id":153,"Level":3,"RecordId":1,"Properties":["\\Device\\Harddisk{disk}\\DR{disk}","0x{lba:x}","{disk}"],"Message":"retried"}}"#
        )
    }

    #[test]
    fn spread_retry_lbas_point_to_link_not_media() {
        let _en = scoped(Lang::En);
        let disks = fixture_disks();
        let sectors = disks[2].size_bytes / 512;
        let lbas = [sectors / 100, sectors / 10, sectors / 3, sectors / 2, sectors * 8 / 10, sectors / 100];
        let json = format!("[{}]", lbas.iter().enumerate().map(|(i, l)| retry_event(60 + i as i64, 2, *l)).collect::<Vec<_>>().join(","));
        let mut events = parse_events_json(&json).unwrap();
        attribute_events(&mut events, &disks);
        let result = analyze(&input(&disks, &[], &events, &[]));
        let comm = verdict(&result, 2).findings.iter().find(|f| f.rule == "comm.link").unwrap().clone();
        let text = comm.evidence.iter().find(|e| e.text.contains("different addresses")).expect("LBA pattern");
        assert!(text.text.contains("spread across 79%"), "{}", text.text);
        assert!(verdict(&result, 2).findings.iter().all(|f| f.rule != "media.degradation"));
    }

    #[test]
    fn few_lifetime_timeouts_are_informational() {
        let disks: Vec<DiskInfo> = fixture_disks().into_iter().take(1).collect();
        let smart = parse_smartctl_json(0, include_str!("../tests/fixtures/smartctl-sata-ssd-wdgreen.json")).unwrap();
        let health = vec![health_with(0, Some(smart))];
        let result = analyze(&input(&disks, &health, &[], &[]));
        let v = verdict(&result, 0);
        assert_eq!(v.status, DiskStatus::Healthy, "2 timeouts over the drive's life do not justify a warning");
        assert!(v.findings.iter().all(|f| f.severity == Severity::Info));
    }

    #[test]
    fn filesystem_finding_names_hidden_partition() {
        let _en = scoped(Lang::En);
        let disks = fixture_disks();
        let mut events = parse_events_json(include_str!("../tests/fixtures/events.json")).unwrap();
        attribute_events(&mut events, &disks);
        let result = analyze(&input(&disks, &[], &events, &[]));
        let fs = verdict(&result, 1).findings.iter().find(|f| f.rule == "fs.corruption").unwrap().clone();
        assert_eq!(fs.severity, Severity::Attention, "isolated corruption is a consequence, not a warning");
        assert!(fs.evidence.iter().any(|e| e.text.contains("partition 4 without letter")), "{:?}", fs.evidence);
        assert!(fs.recommendations.iter().any(|r| r.contains("temporary letter")));
        assert!(!fs.recommendations.iter().any(|r| r.contains("`chkdsk D:`")), "D: is not the affected volume");
    }

    #[test]
    fn reboot_pairs_are_not_merged_across_boots() {
        let _en = scoped(Lang::En);
        let disks = fixture_disks();
        let mk = |provider: &str, id: u32, minutes_ago: i64| {
            let t = (Utc::now() - ChronoDuration::minutes(minutes_ago)).to_rfc3339();
            format!(r#"{{"TimeUtc":"{t}","Provider":"{provider}","Id":{id},"Level":1,"RecordId":1,"Properties":[],"Message":"x"}}"#)
        };
        // Boot A 60 min ago (41 + 6008), boot B 57 min ago; drive 2 retry between them.
        let parts = [
            mk("Microsoft-Windows-Kernel-Power", 41, 60),
            mk("EventLog", 6008, 60),
            retry_event(58, 2, 1000),
            mk("Microsoft-Windows-Kernel-Power", 41, 57),
            mk("EventLog", 6008, 57),
        ];
        let mut events = parse_events_json(&format!("[{}]", parts.join(","))).unwrap();
        attribute_events(&mut events, &disks);
        let result = analyze(&input(&disks, &[], &events, &[]));
        let freeze = verdict(&result, 2).findings.iter().find(|f| f.rule == "freeze.correlation").unwrap().clone();
        assert!(freeze.evidence[0].text.starts_with("1 of 2 unexpected reboots"), "{}", freeze.evidence[0].text);
    }

    #[test]
    fn unattributed_reset_near_disk_events_is_cited() {
        let disks = fixture_disks();
        let json = r#"[
          {"TimeUtc":"2026-11-03T22:20:00.0000000Z","Provider":"disk","Id":153,"Level":3,"RecordId":1,"Properties":["\\Device\\Harddisk2\\DR2","0x10","2"],"Message":"retried"},
          {"TimeUtc":"2026-11-03T22:31:27.0000000Z","Provider":"storahci","Id":129,"Level":3,"RecordId":2,"Properties":["\\Device\\RaidPort0"],"Message":"Reset to device, \\Device\\RaidPort0, was issued."}
        ]"#;
        let mut events = parse_events_json(json).unwrap();
        attribute_events(&mut events, &disks);
        let result = analyze(&input(&disks, &[], &events, &[]));
        let comm = verdict(&result, 2).findings.iter().find(|f| f.rule == "comm.link").unwrap().clone();
        assert!(comm.evidence.iter().any(|e| e.text.contains("storahci/129")));
        assert!(result.global_findings.iter().any(|f| f.rule == "global.controller"));
        assert!(verdict(&result, 0).findings.iter().all(|f| f.rule != "comm.link"), "drive 0 had no events of its own");
    }

    #[test]
    fn recorded_change_is_exposed_separately() {
        let _en = scoped(Lang::En);
        let disks = fixture_disks();
        let note = NoteRecord { time: Utc::now() - ChronoDuration::hours(2), disk_number: 2, text: "Replaced the SATA cable".into() };
        let changes: BTreeMap<String, NoteRecord> = [(disks[2].fingerprint.clone(), note)].into();
        let mut i = input(&disks, &[], &[], &[]);
        i.changes = &changes;
        let result = analyze(&i);
        let v = verdict(&result, 2);
        assert!(v.change.as_deref().is_some_and(|c| c.contains("Replaced the SATA cable")));
        assert!(v.gaps.iter().any(|g| g.contains("recent")));
        assert!(verdict(&result, 0).change.is_none());
    }
}
