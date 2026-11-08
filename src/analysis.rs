//! Motor de correlação: cruza SMART, contadores do Windows, eventos, teste de
//! leitura, benchmark e histórico para produzir achados com causa provável e
//! grau de confiança.
//!
//! Cada regra (`rule_*`) junta **evidências** com peso (1 fraca, 2 moderada,
//! 3 forte, 4 muito forte, 5 definitiva) e uma fonte. A confiança sai de
//! [`confidence_for`]: evidência definitiva, ou peso total alto vindo de fontes
//! independentes, dá confiança alta. Uma regra sem evidências não gera achado.
//!
//! Princípio: SMART normal **não** significa disco saudável; sem evidências o
//! status é "sem problemas detectados nas fontes analisadas", sempre com as
//! lacunas (o que não foi verificado) listadas.

use crate::benchmark::BenchmarkResult;
use crate::capacity::{CapacityResult, CapacityVerdict};
use crate::events::{Attribution, EventKind, StorageEvent};
use crate::health::DiskHealth;
use crate::history::SmartDelta;
use crate::inventory::{BusKind, DiskInfo};
use crate::journal::NoteRecord;
use crate::scan::{ScanResult, ScanStatus};
use crate::smart::SmartReport;
use crate::util::{decimal, format_bytes, format_mbps, format_ms, int, plural};
use crate::windows_counters::ReliabilityCounters;
use chrono::{DateTime, Duration as ChronoDuration, Local, Utc};
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
            Self::Info => "Informativo",
            Self::Attention => "Atenção",
            Self::Warning => "Alerta",
            Self::Critical => "Crítico",
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
            Self::Low => "Baixa",
            Self::Medium => "Média",
            Self::High => "Alta",
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
            Self::WindowsCounters => "Contadores do Windows",
            Self::Events => "Eventos do Windows",
            Self::Scan => "Teste de leitura",
            Self::Benchmark => "Benchmark",
            Self::History => "Histórico SMART",
            Self::Capacity => "Verificação de capacidade",
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
    /// Identificador estável da regra (ex.: `comm.link`).
    pub rule: String,
    pub disk_number: Option<u32>,
    pub severity: Severity,
    pub confidence: Confidence,
    /// Por que essa confiança (quantas evidências, de quais fontes).
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
            Self::Unknown => "Indeterminado",
            Self::Healthy => "Sem problemas detectados",
            Self::Attention => "Atenção",
            Self::Warning => "Alerta",
            Self::Critical => "Crítico",
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
    /// O que não foi verificado (limita a confiança no veredito).
    pub gaps: Vec<String>,
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
    /// Verificações de capacidade salvas (`diskdoctor capacity`).
    pub capacity: &'a [CapacityResult],
    /// Última mudança registrada por disco (fingerprint → nota): eventos anteriores a ela
    /// não entram no veredito. Use [`NO_CHANGES`] quando não houver.
    pub changes: &'a BTreeMap<String, NoteRecord>,
    pub event_window_hours: u32,
    pub events_collected: bool,
    pub elevated: bool,
}

pub const NO_CHANGES: &BTreeMap<String, NoteRecord> = &BTreeMap::new();

/// Nível de confiança a partir das evidências.
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
    format!(
        "{} de {} ({}), peso total {total}",
        plural(evidence.len() as u64, "evidência", "evidências"),
        plural(sources.len() as u64, "fonte", "fontes"),
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

fn local_time(t: DateTime<Utc>) -> String {
    t.with_timezone(&Local).format("%d/%m/%Y %H:%M").to_string()
}

fn local_date(t: DateTime<Utc>) -> String {
    t.with_timezone(&Local).format("%d/%m/%Y").to_string()
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
    format!(
        "{} — {}: {} nas últimas {hours} h (última em {}){}",
        s.code,
        kind.description(),
        plural(s.count, "ocorrência", "ocorrências"),
        local_time(s.last),
        if s.inferred { " — disco inferido pelo driver da controladora" } else { "" }
    )
}

/// Peso de evidência de evento, crescendo com a quantidade (máximo 4).
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
            b.add(EvidenceSource::History, weight, severity, format!("{} aumentou de {} para {} desde {since}{suffix}", c.label, int(c.before), int(c.after)));
        }
    }
}

// ---------------------------------------------------------------------------
// Regras por disco
// ---------------------------------------------------------------------------

fn rule_capacity(ctx: &DiskCtx) -> Option<Finding> {
    let c = ctx.capacity?;
    let m = &c.metrics;
    let when = local_date(c.finished_at);
    match m.verdict {
        CapacityVerdict::Fake => {
            let mut b = Builder::new("capacity.fake", "Capacidade falsa: o dispositivo não guarda tudo o que anuncia");
            b.add(
                EvidenceSource::Capacity,
                5,
                Severity::Critical,
                format!(
                    "Verificação de {when} em {}: {} gravados, só {} lidos de volta corretamente ({} sobrescritos, {} perdidos). Capacidade real estimada: {} (anunciada: {})",
                    c.volume,
                    format_bytes(m.written_bytes),
                    format_bytes(m.ok_bytes),
                    format_bytes(m.overwritten_bytes),
                    format_bytes(m.lost_bytes),
                    c.estimated_real_capacity.map(format_bytes).unwrap_or_else(|| "?".into()),
                    format_bytes(c.disk.size_bytes)
                ),
            );
            b.causes(&["Pendrive/cartão/disco falsificado: o controlador anuncia mais capacidade do que a memória que existe"])
                .recs(&[
                    "Não guarde dados importantes nele: o que passa da capacidade real some ou sobrescreve arquivos antigos",
                    "Peça troca ou reembolso ao vendedor; o relatório do diskdoctor serve de evidência",
                    "Se for usar mesmo assim, só até a capacidade real estimada (ajustar a partição apaga o dispositivo e é feito fora do diskdoctor)",
                ])
                .finish(Some(ctx.disk.number))
        }
        CapacityVerdict::Defective => {
            let mut b = Builder::new("capacity.errors", "Falhas ao gravar ou ler de volta os dados de teste");
            let data_bad = m.corrupted_bytes + m.unreadable_bytes + m.lost_bytes;
            if data_bad > 0 {
                b.add(
                    EvidenceSource::Capacity,
                    5,
                    Severity::Critical,
                    format!(
                        "Verificação de {when} em {}: {} voltaram errados ou ilegíveis (corrompidos {}, ilegíveis {}, perdidos {})",
                        c.volume,
                        format_bytes(data_bad),
                        format_bytes(m.corrupted_bytes),
                        format_bytes(m.unreadable_bytes),
                        format_bytes(m.lost_bytes)
                    ),
                );
            }
            if m.write_errors > 0 {
                b.add(EvidenceSource::Capacity, 4, Severity::Critical, format!("{} falharam na gravação", plural(m.write_errors, "bloco", "blocos")));
            }
            if let ScanStatus::Aborted(r) = &m.status {
                b.add(EvidenceSource::Capacity, 4, Severity::Critical, format!("Verificação interrompida: {r}"));
            }
            b.causes(&[
                "Memória flash desgastada ou defeituosa",
                "Mau contato no conector, no leitor de cartão ou no cabo USB",
                "Energia insuficiente na porta USB",
            ])
            .recs(&[
                "Copie o que importa para outro lugar",
                "Repita em outra porta USB (traseira, direto na placa-mãe) ou outro leitor para descartar mau contato",
                "Se as falhas se repetirem, substitua o dispositivo",
            ])
            .finish(Some(ctx.disk.number))
        }
        CapacityVerdict::Genuine | CapacityVerdict::Inconclusive => None,
    }
}

fn rule_smart_failed(ctx: &DiskCtx) -> Option<Finding> {
    let s = ctx.smart?;
    let mut b = Builder::new("smart.failed", "O próprio disco reporta falha (SMART)");
    if s.overall_passed == Some(false) {
        b.add(EvidenceSource::Smart, 5, Severity::Critical, "Avaliação geral do SMART: FALHOU (o firmware do disco prevê falha)");
    }
    for f in &s.failing_attributes {
        if f.contains("AGORA") {
            b.add(EvidenceSource::Smart, 4, Severity::Critical, format!("Atributo abaixo do limite de falha: {f}"));
        } else {
            b.add(EvidenceSource::Smart, 1, Severity::Attention, format!("Atributo já esteve abaixo do limite: {f}"));
        }
    }
    if let Some(w) = s.nvme_critical_warning.filter(|&w| w != 0) {
        let bits: [(u64, u8, Severity, &str); 5] = [
            (1, 4, Severity::Critical, "reserva de blocos abaixo do limite"),
            (2, 2, Severity::Warning, "temperatura fora do limite"),
            (4, 4, Severity::Critical, "confiabilidade degradada por erros de mídia ou internos"),
            (8, 5, Severity::Critical, "o SSD entrou em modo somente leitura"),
            (16, 2, Severity::Warning, "falha no backup de memória volátil"),
        ];
        for (mask, weight, sev, text) in bits {
            if w & mask != 0 {
                b.add(EvidenceSource::Smart, weight, sev, format!("Aviso crítico NVMe: {text}"));
            }
        }
    }
    b.causes(&["Defeito ou desgaste interno detectado pelo próprio firmware do disco"])
        .recs(&["Faça backup imediatamente", "Substitua o disco e não confie novos dados a ele"])
        .finish(Some(ctx.disk.number))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AtaLogKind {
    /// Há erros UNC (dado ilegível) nas amostras.
    Media,
    /// Só erros ICRC (CRC na interface): cabo/conector/porta.
    Interface,
    Other,
}

/// Classifica o log de erros ATA pelas amostras que o `smartctl -x` mostra.
fn ata_error_log_kind(s: &SmartReport) -> AtaLogKind {
    if s.error_log_samples.iter().any(|e| e.contains("UNC")) {
        AtaLogKind::Media
    } else if s.error_log_samples.iter().any(|e| e.contains("ICRC")) {
        AtaLogKind::Interface
    } else {
        AtaLogKind::Other
    }
}

/// O disco reporta contadores de CRC e todos estão zerados (a vida inteira)?
/// Cabo/conector ruim quase sempre faz esses contadores subirem.
fn crc_counters_clean(s: &SmartReport) -> bool {
    let reported = [s.interface_crc_errors, s.vendor_crc_errors, s.sata_phy_errors, s.phy_interface_errors];
    reported.iter().any(Option::is_some)
        && reported.iter().flatten().all(|&n| n == 0)
        && ata_error_log_kind(s) != AtaLogKind::Interface
}

fn rule_media(ctx: &DiskCtx) -> Option<Finding> {
    use EvidenceSource::*;
    let mut b = Builder::new("media.degradation", "Setores ilegíveis ou mídia degradada");
    if let Some(s) = ctx.smart {
        if let Some(n) = s.pending_sectors.filter(|&n| n > 0) {
            b.add(Smart, 3, Severity::Critical, format!("Setores pendentes (197): {} — o disco não conseguiu ler e aguarda realocação", int(n)));
        }
        if let Some(n) = s.offline_uncorrectable.filter(|&n| n > 0) {
            b.add(Smart, 3, Severity::Critical, format!("Setores incorrigíveis offline (198): {}", int(n)));
        }
        if let Some(n) = s.reported_uncorrectable.filter(|&n| n > 0) {
            b.add(Smart, 2, Severity::Warning, format!("Erros incorrigíveis reportados ao sistema (187): {}", int(n)));
        }
        if let Some(n) = s.reallocated_sectors.filter(|&n| n > 0) {
            let (w, sev) = if n >= 100 { (2, Severity::Warning) } else { (1, Severity::Attention) };
            b.add(Smart, w, sev, format!("Setores realocados (5): {} — setores ruins já substituídos pela reserva", int(n)));
        }
        if let Some(n) = s.grown_bad_blocks.filter(|&n| n > 0) {
            let (w, sev) = if n >= 50 { (2, Severity::Warning) } else { (1, Severity::Attention) };
            b.add(Smart, w, sev, format!("Blocos defeituosos adquiridos durante o uso: {}", int(n)));
        }
        if let Some(n) = s.program_erase_failures.filter(|&n| n > 0) {
            b.add(Smart, 2, Severity::Warning, format!("Falhas de programação/apagamento da memória flash: {}", int(n)));
        }
        if let Some(n) = s.media_errors.filter(|&n| n > 0) {
            b.add(Smart, 3, Severity::Critical, format!("Erros de integridade de mídia (NVMe): {}", int(n)));
        }
        if let Some(n) = s.self_test_failures.filter(|&n| n > 0) {
            b.add(Smart, 3, Severity::Critical, format!("Autoteste SMART registrado com falha: {}", int(n)));
        }
        if !s.is_nvme() {
            // Erros ICRC (CRC na interface) sem UNC são de comunicação: tratados em `comm.link`.
            if let Some(n) = s.error_log_entries.filter(|&n| n > 0) {
                match ata_error_log_kind(s) {
                    AtaLogKind::Media => {
                        b.add(Smart, 2, Severity::Warning, format!("Log de erros ATA com {} registros (inclui erros UNC: dado ilegível)", int(n)))
                    }
                    AtaLogKind::Interface => {}
                    AtaLogKind::Other => b.add(Smart, 1, Severity::Attention, format!("Log de erros ATA com {} registros", int(n))),
                }
            }
        }
    }
    ctx.add_growth(&mut b, "pending_sectors", 3, Severity::Critical, "");
    for key in ["reallocated_sectors", "offline_uncorrectable", "reported_uncorrectable", "media_errors", "grown_bad_blocks", "program_erase_failures"] {
        ctx.add_growth(&mut b, key, 2, Severity::Warning, " — degradação em andamento");
    }
    if let Some(w) = ctx.windows {
        if let Some(n) = w.read_errors_uncorrected.filter(|&n| n > 0) {
            b.add(WindowsCounters, 2, Severity::Warning, format!("O Windows registrou {} erros de leitura não corrigidos", int(n)));
        }
        if let Some(n) = w.write_errors_uncorrected.filter(|&n| n > 0) {
            b.add(WindowsCounters, 2, Severity::Warning, format!("O Windows registrou {} erros de gravação não corrigidos", int(n)));
        }
    }
    ctx.add_events(&mut b, EventKind::BadBlock, 3, Severity::Critical, "");
    ctx.add_events(&mut b, EventKind::IoFailedHardware, 2, Severity::Warning, "");
    ctx.add_events(&mut b, EventKind::SmartPredictedFailure, 4, Severity::Critical, "");
    ctx.add_events(&mut b, EventKind::PagingError, 1, Severity::Warning, " — leitura do arquivo de paginação falhou, causa típica de congelamento");
    if let Some(scan) = ctx.scan {
        let m = &scan.metrics;
        if !m.bad_ranges.is_empty() {
            let first = m.bad_ranges[0].first_lba;
            b.add(
                Scan,
                5,
                Severity::Critical,
                format!(
                    "Teste de leitura de {}: {} ilegíveis em {} (primeira no LBA {})",
                    local_date(scan.finished_at),
                    format_bytes(m.bytes_unreadable),
                    plural(m.bad_ranges.len() as u64, "região", "regiões"),
                    int(first)
                ),
            );
        }
    }
    let causes: &[&str] = if ctx.disk.is_ssd() {
        &["Células de memória flash degradadas (desgaste ou perda de retenção de dados)", "Falha do controlador ou do firmware do SSD"]
    } else if ctx.disk.is_hdd() {
        &["Superfície magnética danificada", "Cabeçote de leitura enfraquecendo", "Dano por impacto ou vibração"]
    } else {
        &["Degradação da mídia de armazenamento", "Falha interna do dispositivo"]
    };
    let mut b = b.causes(causes).recs(&[
        "Faça backup imediato dos dados importantes deste disco",
        "Planeje a substituição do disco",
        "Repita o teste de leitura em alguns dias: se a quantidade de setores ruins crescer, a falha está progredindo",
    ]);
    if ctx.disk.is_ssd() {
        b.recs.push("Verifique se há atualização de firmware no site do fabricante".into());
    }
    b.finish(Some(ctx.disk.number))
}

fn comm_causes_and_recs(disk: &DiskInfo) -> (Vec<&'static str>, Vec<&'static str>) {
    match disk.bus {
        BusKind::Nvme => (
            vec![
                "Contato ruim no slot M.2/PCIe",
                "Gerenciamento de energia agressivo (APST no SSD ou ASPM no PCIe)",
                "Firmware do SSD ou driver NVMe com defeito",
                "Superaquecimento levando a throttling ou travamento do controlador",
            ],
            vec![
                "Desligue o PC, remova e reassente o SSD no slot M.2 (ou teste outro slot)",
                "Atualize o firmware do SSD, o BIOS/UEFI da placa-mãe e o driver de armazenamento",
                "Teste desativar o gerenciamento de energia do link: Opções de energia → PCI Express → Gerenciamento de Energia do Estado do Link = Desativado",
                "Depois de cada mudança, rode `diskdoctor health` e `diskdoctor events` para ver se os eventos param",
            ],
        ),
        BusKind::Usb => (
            vec!["Cabo ou adaptador USB defeituoso", "Energia insuficiente na porta USB", "Ponte USB-SATA com firmware problemático"],
            vec!["Troque o cabo/adaptador e use uma porta USB traseira (direto na placa-mãe)", "Use alimentação externa se o disco/case suportar"],
        ),
        _ => (
            vec![
                "Cabo SATA danificado, dobrado ou mal encaixado",
                "Porta SATA ou controladora da placa-mãe com problema",
                "Alimentação instável (cabo de energia, adaptador ou fonte)",
                "Firmware do disco ou driver da controladora (AHCI/RST)",
                "Pausas internas do próprio disco (firmware, coleta de lixo do SSD) que estouram o tempo limite do Windows — mais comum em SSDs sem cache DRAM",
            ],
            vec![
                "Troque o cabo SATA por um novo e ligue o disco em outra porta SATA da placa-mãe",
                "Use outro conector de energia da fonte (evite adaptadores e extensões)",
                "Atualize o firmware do disco e o BIOS/driver da controladora",
                "Depois da troca, acompanhe com `diskdoctor health`: os contadores não zeram, mas não devem continuar aumentando",
            ],
        ),
    }
}

/// Distribuição dos LBAs citados nos eventos de retry (153): espalhados pelo disco
/// apontam para link/controladora/firmware; concentrados, para uma região da mídia.
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
        format!(
            "; os que mais se repetem: {} — confira essa região no teste de leitura",
            top.iter().take(3).map(|(l, c)| format!("LBA {} ({c}×)", int(*l))).collect::<Vec<_>>().join(", ")
        )
    };
    if counts.len() >= 5 && spread >= 25.0 {
        Some(format!(
            "Os retries ocorreram em {} endereços diferentes, espalhados por {}% do disco — padrão típico de falha de comunicação, controladora ou firmware, não de um setor ruim isolado{hot}",
            int(counts.len() as u64),
            decimal(spread, 0)
        ))
    } else {
        Some(format!(
            "Os retries se concentram em {} endereço(s) numa faixa de {}% do disco — compatível com região fraca da mídia{hot}",
            int(counts.len() as u64),
            decimal(spread, 1)
        ))
    }
}

fn rule_communication(ctx: &DiskCtx) -> Option<Finding> {
    use EvidenceSource::*;
    let mut b = Builder::new("comm.link", "Falhas de comunicação, timeouts ou travamentos do dispositivo");
    if let Some(s) = ctx.smart {
        if let Some(n) = s.interface_crc_errors.filter(|&n| n > 0) {
            b.add(Smart, 2, Severity::Warning, format!("Erros CRC na interface (199): {} — dados corrompidos no caminho cabo/conector/porta", int(n)));
        }
        if let Some(n) = s.vendor_crc_errors.filter(|&n| n > 0) {
            let sev = if n >= 10 { Severity::Attention } else { Severity::Info };
            b.add(
                Smart,
                1,
                sev,
                format!("Contador de erros CRC específico do fabricante: {} (o significado varia por modelo; o importante é se continua aumentando)", int(n)),
            );
        }
        if let Some(n) = s.sata_phy_errors.filter(|&n| n > 0) {
            b.add(Smart, 1, Severity::Attention, format!("Erros de PHY SATA (168): {}", int(n)));
        }
        if let Some(n) = s.phy_interface_errors.filter(|&n| n > 0) {
            b.add(Smart, 2, Severity::Warning, format!("Log SATA Phy: {} erros de transmissão (CRC/R_ERR) desde que o disco foi ligado", int(n)));
        }
        if let Some(n) = s.command_timeouts.filter(|&n| n > 0) {
            // Poucos timeouts na vida inteira do disco são comuns (ex.: quedas de energia).
            let (w, sev) = match n {
                10.. => (2, Severity::Warning),
                3.. => (1, Severity::Attention),
                _ => (1, Severity::Info),
            };
            b.add(Smart, w, sev, format!("Timeouts de comando (188): {} no total da vida do disco", int(n)));
        }
        if let Some(n) = s.end_to_end_errors.filter(|&n| n > 0) {
            b.add(Smart, 2, Severity::Warning, format!("Erros de integridade ponta a ponta (184): {}", int(n)));
        }
        if !s.is_nvme() && ata_error_log_kind(s) == AtaLogKind::Interface {
            if let Some(n) = s.error_log_entries.filter(|&n| n > 0) {
                b.add(
                    Smart,
                    2,
                    Severity::Attention,
                    format!(
                        "Log de erros ATA com {} registros do tipo ICRC (erro de CRC na interface) — típico de cabo ou conexão SATA; o log é da vida inteira do disco, o que importa é se continua crescendo",
                        int(n)
                    ),
                );
            }
        }
    }
    ctx.add_growth(&mut b, "error_log_entries", 2, Severity::Warning, " — erros novos no log do disco");
    ctx.add_growth(&mut b, "interface_crc_errors", 3, Severity::Warning, " — problema de link ativo");
    for key in ["vendor_crc_errors", "sata_phy_errors", "command_timeouts"] {
        ctx.add_growth(&mut b, key, 2, Severity::Warning, " — problema ativo");
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

    // Resets/timeouts da controladora sem disco identificado, mas no mesmo período
    // em que este disco teve eventos de E/S.
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
            format!(
                "{} sem disco identificado ({}, {}) ocorreu a menos de 30 min de eventos de E/S deste disco",
                e.code(),
                e.kind.description(),
                local_time(e.time)
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
                format!("Teste de leitura: {} excederam {} ms e foram canceladas", plural(m.timeout_blocks, "leitura", "leituras"), int(scan.config.io_timeout_ms)),
            );
        }
        if m.transient_errors > 0 {
            b.add(
                Scan,
                2,
                Severity::Warning,
                format!("Teste de leitura: {} que sumiram na releitura — típico de falha de comunicação, não de setor ruim", plural(m.transient_errors, "erro", "erros")),
            );
        }
        if let ScanStatus::Aborted(reason) = &m.status {
            if reason.contains("parou de responder") {
                b.add(Scan, 5, Severity::Critical, "Teste de leitura: o dispositivo parou de responder durante a leitura");
            } else if reason.contains("fatal") {
                b.add(Scan, 4, Severity::Critical, format!("Teste de leitura interrompido: {reason}"));
            }
        }
    }
    let sources: BTreeSet<_> = b.evidence.iter().map(|e| e.source).collect();
    if b.total() >= 8 && sources.len() >= 2 {
        b.severity = b.severity.max(Severity::Critical);
    }
    // Muitos retries/resets com CRC zerado: o link provavelmente está íntegro e quem pausa é o disco.
    let device_side = ctx.disk.bus != BusKind::Nvme
        && ctx.disk.bus != BusKind::Usb
        && ctx.smart.is_some_and(crc_counters_clean)
        && b.evidence.iter().any(|e| e.source == Events && e.weight >= 2);
    if device_side {
        b.add(
            Smart,
            0,
            Severity::Info,
            "O disco não registrou nenhum erro de CRC na interface na vida inteira. Cabo ou conector ruim quase sempre faz esse contador subir; \
             com muitos retries e CRC zerado, o mais provável é o próprio disco pausar (firmware/controlador) ou a alimentação, e não o cabo de dados",
        );
    }
    let (causes, recs) = if device_side { device_stall_causes_and_recs() } else { comm_causes_and_recs(ctx.disk) };
    b.causes(&causes).recs(&recs).finish(Some(ctx.disk.number))
}

/// Causas quando o link parece íntegro (CRC zerado) mas o disco pausa/não responde.
fn device_stall_causes_and_recs() -> (Vec<&'static str>, Vec<&'static str>) {
    (
        vec![
            "Pausas internas do próprio disco (firmware, controlador, coleta de lixo) que estouram o tempo limite do Windows — comum em SSDs sem cache DRAM",
            "Firmware do disco com defeito conhecido",
            "Alimentação instável (cabo de energia, adaptador ou fonte)",
            "Porta SATA, controladora ou driver (AHCI/RST)",
            "Cabo SATA de dados (menos provável: o disco não registrou erros de CRC)",
        ],
        vec![
            "Faça backup dos dados importantes deste disco",
            "Atualize o firmware do disco com a ferramenta do fabricante",
            "Troque o conector de energia (sem adaptadores/extensões) e a porta SATA — testes baratos para descartar",
            "Registre cada mudança com `diskdoctor history note` e compare depois com `diskdoctor history`",
            "Se os retries continuarem depois disso, o disco é o suspeito principal: planeje substituí-lo",
        ],
    )
}

fn rule_stalls(ctx: &DiskCtx) -> Option<Finding> {
    use EvidenceSource::*;
    let mut b = Builder::new("perf.stalls", "Leituras muito lentas ou pausas do dispositivo");
    if let Some(scan) = ctx.scan {
        let m = &scan.metrics;
        let above = m.latency.count_above(500);
        if above >= 20 {
            b.add(Scan, 3, Severity::Warning, format!("{} levaram mais de 500 ms (máximo {})", plural(above, "leitura", "leituras"), format_ms(m.latency.max_ms)));
        } else if above >= 1 {
            b.add(Scan, 2, Severity::Attention, format!("{} levaram mais de 500 ms (máximo {})", plural(above, "leitura", "leituras"), format_ms(m.latency.max_ms)));
        } else if m.latency.max_ms >= 150.0 {
            b.add(Scan, 1, Severity::Attention, format!("Pico de latência de leitura de {}", format_ms(m.latency.max_ms)));
        }
        if let (Some(min), Some(median)) = (m.min_segment_mb_per_s, m.median_segment_mb_per_s) {
            if median > 0.0 && min < median * 0.35 {
                b.add(
                    Scan,
                    1,
                    Severity::Attention,
                    format!("A velocidade caiu para {} em parte do disco (mediana {})", format_mbps(min), format_mbps(median)),
                );
            }
        }
    }
    if let Some(read) = ctx.bench.and_then(|bn| bn.read.as_ref()) {
        if let Some(r) = read.random.as_ref().filter(|r| r.latency.max_ms >= 1000.0) {
            b.add(Benchmark, 2, Severity::Warning, format!("Benchmark: leitura aleatória de 4 KiB chegou a {}", format_ms(r.latency.max_ms)));
        }
    }
    if let Some(w) = ctx.windows {
        if let Some(ms) = w.read_latency_max_ms {
            if ms >= 2000 {
                b.add(WindowsCounters, 2, Severity::Warning, format!("O Windows registrou latência máxima de leitura de {}", format_ms(ms as f64)));
            } else if ms >= 500 {
                b.add(WindowsCounters, 1, Severity::Attention, format!("O Windows registrou latência máxima de leitura de {}", format_ms(ms as f64)));
            }
        }
        for (label, v) in [("gravação", w.write_latency_max_ms), ("flush", w.flush_latency_max_ms)] {
            if let Some(ms) = v.filter(|&ms| ms >= 2000) {
                b.add(WindowsCounters, 1, Severity::Attention, format!("O Windows registrou latência máxima de {label} de {}", format_ms(ms as f64)));
            }
        }
    }
    let causes: &[&str] = if ctx.disk.is_ssd() {
        &[
            "Dados antigos exigindo correção de erros intensiva (perda de retenção da memória flash)",
            "Firmware/controlador do SSD (pausas de coleta de lixo, cache esgotado)",
            "Superaquecimento (throttling)",
        ]
    } else {
        &["Setores fracos que exigem várias tentativas de leitura", "Desgaste mecânico ou do cabeçote", "Problema de comunicação (ver achados de link)"]
    };
    let mut b = b.causes(causes).recs(&[
        "Repita o teste: lentidão nas mesmas posições indica mídia; em posições aleatórias aponta para firmware, temperatura ou comunicação",
    ]);
    if ctx.disk.is_ssd() {
        b.recs.push("Atualize o firmware do SSD".into());
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
    let mut b = Builder::new("temp.high", "Temperatura elevada");
    let current = s.and_then(|s| s.temperature_c).or(ctx.windows.and_then(|w| w.temperature_c.map(|t| t as i64)));
    let source = if s.and_then(|s| s.temperature_c).is_some() { EvidenceSource::Smart } else { EvidenceSource::WindowsCounters };
    if let Some(t) = current {
        if t >= crit {
            b.add(source, 3, Severity::Critical, format!("Temperatura atual {t} °C (limite crítico {crit} °C)"));
        } else if t >= warn {
            b.add(source, 2, Severity::Warning, format!("Temperatura atual {t} °C (limite de aviso {warn} °C)"));
        }
    }
    let max = s.and_then(|s| s.temperature_max_c).or(ctx.windows.and_then(|w| w.temperature_max_c.map(|t| t as i64)));
    if let Some(m) = max.filter(|&m| m >= crit) {
        b.add(EvidenceSource::Smart, 1, Severity::Attention, format!("Temperatura máxima já registrada: {m} °C"));
    }
    if let Some(min) = s.and_then(|s| s.nvme_critical_temp_minutes).filter(|&m| m > 0) {
        b.add(EvidenceSource::Smart, 2, Severity::Warning, format!("Já operou {} acima da temperatura crítica", plural(min, "minuto", "minutos")));
    }
    if let Some(min) = s.and_then(|s| s.nvme_warning_temp_minutes).filter(|&m| m > 0) {
        b.add(EvidenceSource::Smart, 1, Severity::Attention, format!("Já operou {} acima da temperatura de aviso", plural(min, "minuto", "minutos")));
    }
    b.causes(&["Ventilação insuficiente no gabinete", "SSD NVMe sem dissipador ou perto da placa de vídeo", "Poeira acumulada"])
        .recs(&["Melhore o fluxo de ar do gabinete", "Em NVMe, use dissipador; em HD, garanta ventilação frontal", "Temperatura alta acelera desgaste e pode causar throttling ou travamentos"])
        .finish(Some(ctx.disk.number))
}

fn rule_wear(ctx: &DiskCtx) -> Option<Finding> {
    if !ctx.disk.is_ssd() {
        return None;
    }
    let mut b = Builder::new("ssd.wear", "Desgaste avançado da memória flash");
    let wear = ctx.smart.and_then(|s| s.wear_used_percent).or(ctx.windows.and_then(|w| w.wear_percent));
    if let Some(w) = wear {
        if w >= 90 {
            b.add(EvidenceSource::Smart, 3, Severity::Critical, format!("Vida útil consumida: {w}%"));
        } else if w >= 75 {
            b.add(EvidenceSource::Smart, 2, Severity::Warning, format!("Vida útil consumida: {w}%"));
        }
    }
    if let Some(s) = ctx.smart {
        if let (Some(spare), Some(th)) = (s.available_spare_percent, s.available_spare_threshold) {
            if th > 0 && spare <= th {
                b.add(EvidenceSource::Smart, 4, Severity::Critical, format!("Reserva disponível {spare}% atingiu o limite de {th}%"));
            }
        }
    }
    b.causes(&["Volume de gravação acumulado próximo da resistência nominal do SSD"])
        .recs(&["Planeje a substituição", "Mantenha backup atualizado: SSDs no fim da vida podem entrar em modo somente leitura ou falhar de repente"])
        .finish(Some(ctx.disk.number))
}

fn rule_unsafe_shutdowns(ctx: &DiskCtx) -> Option<Finding> {
    let mut b = Builder::new("power.unsafe", "Desligamentos bruscos frequentes");
    if let Some(s) = ctx.smart {
        if let (Some(u), Some(pc)) = (s.unsafe_shutdowns, s.power_cycles.filter(|&p| p > 0)) {
            let ratio = u as f64 / pc as f64;
            if u >= 50 && ratio >= 0.05 {
                b.add(
                    EvidenceSource::Smart,
                    1,
                    Severity::Attention,
                    format!("{} desligamentos bruscos em {} ciclos de energia ({}%)", int(u), int(pc), decimal(ratio * 100.0, 1)),
                );
            }
        }
    }
    if let Some(c) = ctx.delta.and_then(|d| d.change("unsafe_shutdowns")).filter(|c| c.increase() >= 3) {
        let since = ctx.delta.map(|d| local_date(d.baseline_time)).unwrap_or_default();
        b.add(EvidenceSource::History, 1, Severity::Attention, format!("{} desligamentos bruscos desde {since}", int(c.increase())));
    }
    b.causes(&["Travamentos que exigiram desligar pelo botão", "Quedas de energia", "Fonte de alimentação instável"])
        .recs(&[
            "Desligamentos bruscos podem corromper dados em trânsito (especialmente em SSDs sem proteção contra queda de energia)",
            "Se a causa for travamento, veja os achados de comunicação/travamento deste disco",
            "Considere um nobreak",
        ])
        .finish(Some(ctx.disk.number))
}

fn rule_filesystem(ctx: &DiskCtx) -> Option<Finding> {
    // Corrupção de sistema de arquivos costuma ser consequência (desligamento brusco,
    // E/S falha), não prova de defeito do disco: só vira "Alerta" se for recorrente.
    let corruption_sev = match summarize(&ctx.events, EventKind::FsCorruption) {
        Some(s) if s.count >= 5 => Severity::Warning,
        _ => Severity::Attention,
    };
    let mut b = Builder::new("fs.corruption", "Inconsistências no sistema de arquivos");
    ctx.add_events(&mut b, EventKind::FsCorruption, 2, corruption_sev, "");
    ctx.add_events(&mut b, EventKind::VolumeNeedsRepair, 1, Severity::Attention, "");
    ctx.add_events(&mut b, EventKind::FlushFailed, 2, Severity::Warning, " — o Windows não conseguiu gravar dados no disco");
    ctx.add_events(&mut b, EventKind::TornWrite, 1, Severity::Attention, "");

    // Quais partições os eventos citam (por GUID de volume, dispositivo ou letra).
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
        b.add(EvidenceSource::Events, 0, Severity::Info, format!("Volumes citados: {}", affected.iter().cloned().collect::<Vec<_>>().join(", ")));
    }
    let letters: Vec<String> = affected.iter().filter(|a| a.len() == 2 && a.ends_with(':')).cloned().collect();
    let check = if !letters.is_empty() {
        format!("Verifique em modo somente leitura: {} (sem /f)", letters.iter().map(|l| format!("`chkdsk {l}`")).collect::<Vec<_>>().join(", "))
    } else {
        "Verifique o volume afetado em modo somente leitura com `chkdsk <letra>:` (sem /f)".to_string()
    };
    let mut b = b.causes(&[
        "Consequência de desligamentos bruscos ou travamentos",
        "Erros de E/S do disco fazendo com que dados fossem gravados ou lidos incorretamente",
    ]);
    b.recs.push(check);
    if hidden {
        b.recs.push(
            "Partições sem letra (recuperação/sistema) são verificadas pelo Windows na inicialização; se o aviso persistir, \
             atribua uma letra temporária no Gerenciamento de Disco para rodar o chkdsk"
                .into(),
        );
    }
    b.recs.push("Corrija com `chkdsk <letra>: /f` somente depois do backup".into());
    b.finish(Some(ctx.disk.number))
}

/// Eventos de E/S deste disco nos 30 min anteriores a reinicializações inesperadas.
fn rule_freeze_correlation(ctx: &DiskCtx) -> Option<Finding> {
    // Um boot após travamento gera Kernel-Power 41 e, segundos depois, EventLog 6008.
    // Usa o 41; o 6008 só conta quando não há 41 do mesmo boot por perto.
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
    let mut b = Builder::new("freeze.correlation", "Problemas deste disco logo antes de travamentos/reinicializações");
    if !hits.is_empty() {
        hits.sort_by_key(|h| std::cmp::Reverse(h.0));
        let examples = hits.iter().take(3).map(|(t, n)| format!("{} ({} antes)", local_time(*t), plural(*n as u64, "evento", "eventos"))).collect::<Vec<_>>().join("; ");
        let weight = if hits.len() >= 2 { 3 } else { 2 };
        b.add(
            EvidenceSource::Events,
            weight,
            Severity::Warning,
            format!(
                "{} de {} reinicializações inesperadas foram precedidas, em até 30 min, por eventos de E/S deste disco: {examples}",
                hits.len(),
                reboots.len()
            ),
        );
    }
    let mut b = b
        .causes(&["O disco (ou o link com ele) parou de responder e o sistema congelou — com o disco travado, o Windows muitas vezes nem consegue registrar o erro"])
        .recs(&["Teste com outro cabo/porta (ou slot M.2) e observe se os travamentos param"]);
    if ctx.disk.is_boot || ctx.disk.is_system {
        b.recs.insert(0, "Este é o disco do sistema: trate como prioridade e faça backup dos dados".into());
    }
    b.finish(Some(ctx.disk.number))
}

// ---------------------------------------------------------------------------
// Achados globais e veredito
// ---------------------------------------------------------------------------

fn global_findings(input: &AnalysisInput) -> Vec<Finding> {
    let mut out = Vec::new();
    let unattributed: Vec<&StorageEvent> = input.events.iter().filter(|e| e.disk_number.is_none() && e.category.is_storage_io()).collect();
    if !unattributed.is_empty() {
        let mut b = Builder::new("global.controller", "Eventos de controladora/armazenamento sem disco identificado");
        let mut by_kind: BTreeMap<EventKind, Vec<&StorageEvent>> = BTreeMap::new();
        for e in &unattributed {
            by_kind.entry(e.kind).or_default().push(e);
        }
        for (kind, list) in by_kind {
            let s = summarize(&list, kind).expect("lista não vazia");
            let sev = if matches!(kind, EventKind::DeviceReset | EventKind::DeviceTimeout | EventKind::SurpriseRemoval) { Severity::Warning } else { Severity::Attention };
            b.add(EvidenceSource::Events, event_weight(2, &s), sev, event_text(&s, kind, input.event_window_hours));
        }
        if let Some(f) = b
            .causes(&["Algum disco ligado a essa controladora deixou de responder; com vários discos no mesmo driver não dá para saber qual só pelo evento"])
            .recs(&["Compare os horários com os eventos atribuídos a cada disco (o achado de comunicação do disco cita coincidências)"])
            .finish(None)
        {
            out.push(f);
        }
    }
    let whea: Vec<&StorageEvent> = input.events.iter().filter(|e| e.kind == EventKind::WheaHardwareError).collect();
    if let Some(s) = summarize(&whea, EventKind::WheaHardwareError) {
        let mut b = Builder::new("global.whea", "Erros de hardware reportados pelo WHEA");
        b.add(EvidenceSource::Events, 1, Severity::Attention, event_text(&s, EventKind::WheaHardwareError, input.event_window_hours));
        if let Some(f) = b
            .causes(&["Erros no barramento PCIe (pode afetar SSDs NVMe), CPU ou memória"])
            .recs(&["Se houver SSD NVMe, confira o slot e o gerenciamento de energia do PCIe", "Atualize BIOS/UEFI e drivers do chipset"])
            .finish(None)
        {
            out.push(f);
        }
    }
    let shutdowns: Vec<&StorageEvent> = input.events.iter().filter(|e| e.kind == EventKind::UnexpectedReboot).collect();
    if let Some(s) = summarize(&shutdowns, EventKind::UnexpectedReboot) {
        let mut b = Builder::new("global.unexpected_reboots", "Reinicializações inesperadas no período");
        let sev = if s.count >= 3 { Severity::Attention } else { Severity::Info };
        b.add(EvidenceSource::Events, 1, sev, event_text(&s, EventKind::UnexpectedReboot, input.event_window_hours));
        if let Some(f) = b
            .causes(&["Travamentos que exigiram reset, quedas de energia, fonte, superaquecimento ou drivers — não necessariamente o disco"])
            .recs(&["Veja nos achados de cada disco se houve eventos de E/S logo antes dessas reinicializações"])
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

    let mut sources = Vec::new();
    let mut gaps = Vec::new();
    match (smart, health.and_then(|h| h.smart.as_ref())) {
        (Some(s), _) => sources.push(s.source.clone()),
        (None, _) if disk.is_flash_media() => gaps.push(
            "Pendrives e cartões de memória não têm SMART: o teste de leitura (`scan`) e a verificação de capacidade (`capacity`) são os diagnósticos possíveis".into(),
        ),
        (None, Some(s)) if disk.is_external() => gaps.push(format!(
            "SMART indisponível ({}): muitos cases/adaptadores USB não repassam o SMART do disco",
            s.error.clone().unwrap_or_else(|| "sem dados".into())
        )),
        (None, Some(s)) => gaps.push(format!("SMART indisponível: {}", s.error.clone().unwrap_or_else(|| "sem dados".into()))),
        (None, None) => gaps.push("SMART não consultado (smartctl não encontrado ou não executado)".into()),
    }
    if ctx.windows.is_some() {
        sources.push("Contadores de confiabilidade do Windows".into());
    }
    if input.events_collected {
        sources.push(format!("Eventos do Windows ({} h)", input.event_window_hours));
    } else {
        gaps.push("Eventos do Windows não consultados".into());
    }
    match scan {
        Some(s) => {
            sources.push(format!("Teste de leitura de {} ({})", local_date(s.finished_at), s.metrics.status.label()));
            let cap = s.metrics.capacity.max(1);
            let pct = s.metrics.bytes_processed as f64 * 100.0 / cap as f64;
            if pct < 99.0 {
                gaps.push(format!("O último teste de leitura cobriu só {}% do disco", decimal(pct, 1)));
            }
        }
        None => gaps.push(format!("Teste de leitura completo ainda não executado (`diskdoctor scan --disk {}`)", disk.number)),
    }
    if bench.is_some() {
        sources.push("Benchmark".into());
    }
    if let Some(c) = capacity {
        sources.push(format!("Verificação de capacidade de {} ({})", local_date(c.finished_at), c.metrics.verdict.label()));
    }
    if let Some(c) = change {
        sources.push(format!(
            "Mudança registrada em {}: \"{}\" — só eventos posteriores a ela entram no veredito (`diskdoctor history --disk {}` compara antes × depois)",
            local_time(c.time),
            c.text,
            disk.number
        ));
        let hours = (Utc::now() - c.time).num_hours();
        if hours < 24 {
            gaps.push(format!("A mudança é recente (há {} h): use o computador normalmente e repita o diagnóstico em 1–3 dias", hours.max(0)));
        }
    }
    match ctx.delta {
        Some(d) => sources.push(format!("Histórico SMART desde {}", local_date(d.baseline_time))),
        None if smart.is_some() => gaps.push("Sem histórico anterior: rode `diskdoctor health` periodicamente para detectar contadores que crescem".into()),
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
        DiskStatus::Healthy => "Nenhum problema encontrado nas fontes analisadas (isso não garante que o disco esteja saudável)".into(),
        DiskStatus::Unknown => "Dados insuficientes para avaliar este disco".into(),
        _ => findings.first().map(|f| f.title.clone()).unwrap_or_default(),
    };
    DiskVerdict { disk_number: disk.number, label: format!("{} ({})", disk.title(), disk.class_label()), status, headline, findings, sources_used: sources, gaps }
}

pub fn analyze(input: &AnalysisInput) -> AnalysisResult {
    let verdicts = input.disks.iter().map(|d| analyze_disk(input, d)).collect();
    let mut global = global_findings(input);
    sort_findings(&mut global);
    let mut notes = vec![
        "Atributos SMART normais não garantem que o disco está saudável: parte importante das falhas acontece sem aviso no SMART. \
         Combine com o teste de leitura completo e com o histórico de eventos."
            .to_string(),
        "Quando o disco do sistema congela, o Windows muitas vezes não consegue gravar o evento: ausência de eventos não descarta problema no disco.".into(),
    ];
    if !input.elevated {
        notes.push("Executado sem privilégios de Administrador: dados SMART podem estar incompletos e testes de leitura não estão disponíveis.".into());
    }
    if !input.events_collected {
        notes.push("Os eventos do Windows não foram consultados nesta execução.".into());
    }
    AnalysisResult { verdicts, global_findings: global, notes }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::{attribute_events, parse_events_json};
    use crate::history::CounterChange;
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
        assert_eq!(confidence_for(&[ev(EvidenceSource::Smart, 3), ev(EvidenceSource::Smart, 3)]), Confidence::Medium, "uma fonte só");
        assert_eq!(confidence_for(&[ev(EvidenceSource::Smart, 3), ev(EvidenceSource::Events, 3)]), Confidence::High);
    }

    #[test]
    fn real_event_pattern_flags_system_disk() {
        // Fixture real: 4× disk/153 no disco 2 minutos antes de Kernel-Power 41.
        let disks = fixture_disks();
        let mut events = parse_events_json(include_str!("../tests/fixtures/events.json")).unwrap();
        attribute_events(&mut events, &disks);
        let smart2 = parse_smartctl_json(2, include_str!("../tests/fixtures/smartctl-sata-ssd-kingston120.json")).unwrap();
        let health = vec![health_with(2, Some(smart2))];
        let result = analyze(&input(&disks, &health, &events, &[]));

        let v2 = verdict(&result, 2);
        assert!(v2.status >= DiskStatus::Warning, "status {:?}", v2.status);
        let comm = v2.findings.iter().find(|f| f.rule == "comm.link").expect("achado de comunicação");
        assert!(comm.evidence.iter().any(|e| e.text.contains("disk/153")));
        let freeze = v2.findings.iter().find(|f| f.rule == "freeze.correlation").expect("correlação com travamento");
        assert!(freeze.recommendations[0].contains("disco do sistema"));
        assert!(v2.findings.iter().any(|f| f.rule == "power.unsafe"), "219 desligamentos bruscos em 2943 ciclos");
        assert!(v2.gaps.iter().any(|g| g.contains("scan --disk 2")));

        // NTFS 98 do volume 000000000104 pertence ao disco 1.
        let v1 = verdict(&result, 1);
        assert!(v1.findings.iter().any(|f| f.rule == "fs.corruption"));
        // Sempre avisa que SMART normal não garante saúde.
        assert!(result.notes[0].contains("não garantem"));
        assert!(result.global_findings.iter().any(|f| f.rule == "global.unexpected_reboots"));
    }

    #[test]
    fn many_retries_with_clean_crc_point_to_the_disk_not_the_cable() {
        let disks = fixture_disks();
        let mut events = parse_events_json(include_str!("../tests/fixtures/events.json")).unwrap();
        attribute_events(&mut events, &disks);
        let smart2 = parse_smartctl_json(2, include_str!("../tests/fixtures/smartctl-sata-ssd-kingston120.json")).unwrap();
        assert_eq!(smart2.vendor_crc_errors.or(smart2.interface_crc_errors), Some(0), "fixture com CRC zerado");
        let health = vec![health_with(2, Some(smart2))];
        let result = analyze(&input(&disks, &health, &events, &[]));
        let comm = verdict(&result, 2).findings.iter().find(|f| f.rule == "comm.link").unwrap().clone();
        assert!(comm.evidence.iter().any(|e| e.text.contains("nenhum erro de CRC")), "{:?}", comm.evidence);
        assert!(comm.possible_causes[0].contains("Pausas internas"));
        assert!(comm.possible_causes.last().unwrap().contains("menos provável"));
    }

    #[test]
    fn icrc_error_log_is_communication_not_media() {
        let disks = fixture_disks();
        let mut smart = parse_smartctl_json(1, include_str!("../tests/fixtures/smartctl-sata-ssd-kingston240.json")).unwrap();
        smart.error_log_entries = Some(36);
        smart.error_log_samples = vec!["Error: ICRC, ABRT 16 sectors at LBA = 0x00000000 = 0".into(); 3];
        let health = vec![health_with(1, Some(smart))];
        let result = analyze(&input(&disks, &health, &[], &[]));
        let v = verdict(&result, 1);
        assert!(!v.findings.iter().any(|f| f.rule == "media.degradation"), "ICRC não é defeito de mídia: {:?}", v.findings);
        let comm = v.findings.iter().find(|f| f.rule == "comm.link").unwrap();
        assert!(comm.evidence.iter().any(|e| e.text.contains("ICRC")));
        assert!(comm.possible_causes[0].contains("Cabo SATA"), "com CRC registrado, o cabo continua o primeiro suspeito");
    }

    #[test]
    fn failing_hdd_with_bad_scan_is_critical_with_high_confidence() {
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
        assert!(media.possible_causes.iter().any(|c| c.contains("Superfície")));
        assert!(media.recommendations[0].contains("backup"));
    }

    #[test]
    fn clean_data_is_healthy_but_never_guaranteed() {
        let disks: Vec<DiskInfo> = fixture_disks().into_iter().take(1).collect();
        let smart = SmartReport { disk_number: 0, available: true, source: "smartctl".into(), overall_passed: Some(true), temperature_c: Some(35), ..Default::default() };
        let mut reader = ScriptedReader::new(32 << 20);
        let scans = vec![scan_for(&disks[0], &mut reader)];
        let health = vec![health_with(0, Some(smart))];
        let result = analyze(&input(&disks, &health, &[], &scans));
        let v = verdict(&result, 0);
        assert_eq!(v.status, DiskStatus::Healthy);
        assert!(v.findings.is_empty());
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
        let disks = fixture_disks();
        let smart = SmartReport { disk_number: 1, available: true, interface_crc_errors: Some(120), ..Default::default() };
        let history = SmartDelta {
            baseline_time: Utc::now() - ChronoDuration::days(10),
            snapshots: 3,
            changes: vec![CounterChange { key: "interface_crc_errors".into(), label: "Erros CRC de interface (199)".into(), before: 20, after: 120 }],
        };
        let health = vec![DiskHealth { disk_number: 1, smart: Some(smart), windows: None, history: Some(history) }];
        let json = r#"[{"TimeUtc":"2026-11-08T10:00:00.0000000Z","Provider":"disk","Id":153,"Level":3,"RecordId":1,"Properties":["\\Device\\Harddisk1\\DR1","0x10","1"],"Message":"retried"}]"#;
        let mut events = parse_events_json(json).unwrap();
        attribute_events(&mut events, &disks);
        let result = analyze(&input(&disks, &health, &events, &[]));
        let comm = verdict(&result, 1).findings.iter().find(|f| f.rule == "comm.link").unwrap().clone();
        assert_eq!(comm.confidence, Confidence::High);
        assert!(comm.evidence.iter().any(|e| e.source == EvidenceSource::History && e.text.contains("aumentou de 20 para 120")));
        assert!(comm.recommendations.iter().any(|r| r.contains("cabo SATA")));
    }

    #[test]
    fn transient_scan_errors_point_to_communication() {
        let disks: Vec<DiskInfo> = fixture_disks().into_iter().take(1).collect();
        let mut reader = ScriptedReader::new(32 << 20);
        reader.transient_bad.push((4 << 20, (4 << 20) + 512));
        reader.timeout_ranges.push((8 << 20, (8 << 20) + 1));
        let scans = vec![scan_for(&disks[0], &mut reader)];
        let result = analyze(&input(&disks, &[], &[], &scans));
        let v = verdict(&result, 0);
        let comm = v.findings.iter().find(|f| f.rule == "comm.link").unwrap();
        assert!(comm.evidence.iter().any(|e| e.text.contains("sumiram na releitura")));
        assert!(comm.evidence.iter().any(|e| e.text.contains("canceladas")));
        assert!(v.findings.iter().all(|f| f.rule != "media.degradation"), "erro transitório não é setor ruim");
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
        assert!(v.findings.iter().any(|f| f.rule == "temp.high"), "operou acima da temperatura de aviso");
        assert!(v.findings.iter().any(|f| f.rule == "media.degradation"), "3 erros de mídia NVMe");
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
        let disks = fixture_disks();
        let sectors = disks[2].size_bytes / 512;
        let lbas = [sectors / 100, sectors / 10, sectors / 3, sectors / 2, sectors * 8 / 10, sectors / 100];
        let json = format!("[{}]", lbas.iter().enumerate().map(|(i, l)| retry_event(60 + i as i64, 2, *l)).collect::<Vec<_>>().join(","));
        let mut events = parse_events_json(&json).unwrap();
        attribute_events(&mut events, &disks);
        let result = analyze(&input(&disks, &[], &events, &[]));
        let comm = verdict(&result, 2).findings.iter().find(|f| f.rule == "comm.link").unwrap().clone();
        let text = comm.evidence.iter().find(|e| e.text.contains("endereços diferentes")).expect("padrão de LBAs");
        assert!(text.text.contains("espalhados por 79%"), "{}", text.text);
        assert!(verdict(&result, 2).findings.iter().all(|f| f.rule != "media.degradation"));
    }

    #[test]
    fn few_lifetime_timeouts_are_informational() {
        let disks: Vec<DiskInfo> = fixture_disks().into_iter().take(1).collect();
        let smart = parse_smartctl_json(0, include_str!("../tests/fixtures/smartctl-sata-ssd-wdgreen.json")).unwrap();
        let health = vec![health_with(0, Some(smart))];
        let result = analyze(&input(&disks, &health, &[], &[]));
        let v = verdict(&result, 0);
        assert_eq!(v.status, DiskStatus::Healthy, "2 timeouts na vida do disco não justificam alerta");
        assert!(v.findings.iter().all(|f| f.severity == Severity::Info));
    }

    #[test]
    fn filesystem_finding_names_hidden_partition() {
        let disks = fixture_disks();
        let mut events = parse_events_json(include_str!("../tests/fixtures/events.json")).unwrap();
        attribute_events(&mut events, &disks);
        let result = analyze(&input(&disks, &[], &events, &[]));
        let fs = verdict(&result, 1).findings.iter().find(|f| f.rule == "fs.corruption").unwrap().clone();
        assert_eq!(fs.severity, Severity::Attention, "corrupção isolada é consequência, não alerta");
        assert!(fs.evidence.iter().any(|e| e.text.contains("partição 4 sem letra")), "{:?}", fs.evidence);
        assert!(fs.recommendations.iter().any(|r| r.contains("letra temporária")));
        assert!(!fs.recommendations.iter().any(|r| r.contains("`chkdsk D:`")), "D: não é o volume afetado");
    }

    #[test]
    fn reboot_pairs_are_not_merged_across_boots() {
        let disks = fixture_disks();
        let mk = |provider: &str, id: u32, minutes_ago: i64| {
            let t = (Utc::now() - ChronoDuration::minutes(minutes_ago)).to_rfc3339();
            format!(r#"{{"TimeUtc":"{t}","Provider":"{provider}","Id":{id},"Level":1,"RecordId":1,"Properties":[],"Message":"x"}}"#)
        };
        // Boot A há 60 min (41 + 6008), boot B há 57 min; retry do disco 2 entre os dois.
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
        assert!(freeze.evidence[0].text.starts_with("1 de 2 reinicializações"), "{}", freeze.evidence[0].text);
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
        assert!(verdict(&result, 0).findings.iter().all(|f| f.rule != "comm.link"), "disco 0 não teve eventos próprios");
    }
}
