//! Implementação dos comandos da CLI. Cada submódulo expõe `run(ctx, args) -> Result<u8>`
//! retornando um código de [`crate::app::exit`].

pub mod benchmark;
pub mod capacity;
pub mod events;
pub mod health;
pub mod history;
pub mod list;
pub mod report;
pub mod scan;
pub mod tag;

use crate::analysis::{AnalysisInput, AnalysisResult, DiskStatus, analyze};
use crate::app::exit;
use crate::benchmark::BenchmarkResult;
use crate::capacity::CapacityResult;
use crate::events::{StorageEvent, collect_events};
use crate::health::{DiskHealth, HealthOptions, collect_health};
use crate::inventory::{self, DiskIdentity, DiskInfo, MediaKind};
use crate::journal::{DiagnosisRecord, Journal, JournalEntry, NoteRecord};
use std::collections::BTreeMap;
use crate::scan::ScanResult;
use crate::smart::Smartctl;
use crate::store::ResultStore;
use crate::tags::{DiskRef, TagStore};
use crate::ui;
use anyhow::{Result, bail};
use serde::Serialize;
use std::path::PathBuf;

/// Contexto comum a todos os comandos.
pub struct Ctx {
    pub json: bool,
    pub elevated: bool,
    pub data_dir: PathBuf,
}

pub fn print_json<T: Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

/// Inventário + tags do usuário. Sempre use esta função (e não `inventory::load_inventory`
/// direto) para que títulos e `--disk <TAG>` enxerguem as tags.
pub fn load_disks(ctx: &Ctx) -> Result<Vec<DiskInfo>> {
    let spinner = (!ctx.json).then(|| ui::spinner("Listando discos…"));
    let disks = inventory::load_inventory();
    if let Some(s) = spinner {
        s.finish_and_clear();
    }
    let mut disks = disks?;
    if disks.is_empty() {
        bail!("nenhum disco físico encontrado pelo Windows (Get-Disk não retornou discos)");
    }
    apply_saved_tags(ctx, &mut disks);
    Ok(disks)
}

/// Preenche as tags. Um `tags.json` ilegível vira aviso: nunca impede o diagnóstico.
pub fn apply_saved_tags(ctx: &Ctx, disks: &mut [DiskInfo]) {
    match TagStore::new(&ctx.data_dir).load() {
        Ok(book) => book.apply(disks),
        Err(e) => ui::warn(&format!("tags ignoradas: {e:#}")),
    }
}

pub fn select_disk<'a>(disks: &'a [DiskInfo], disk: &DiskRef) -> Result<&'a DiskInfo> {
    disk.resolve(disks)
}

pub fn admin_required(what: &str) -> String {
    format!(
        "{what} e exige privilégios de Administrador. Abra o Terminal como Administrador \
         (botão direito no menu Iniciar → Terminal (Admin)) e rode o comando novamente."
    )
}

/// Código de saída a partir do pior status encontrado.
pub fn exit_for_status(status: Option<DiskStatus>) -> u8 {
    match status {
        Some(DiskStatus::Critical | DiskStatus::Warning) => exit::PROBLEMS_FOUND,
        _ => exit::OK,
    }
}

/// Tudo o que `health` e `report` coletam antes da análise.
#[derive(Serialize)]
pub struct Collected {
    pub smartctl: Option<String>,
    pub disks: Vec<DiskInfo>,
    pub health: Vec<DiskHealth>,
    pub events: Vec<StorageEvent>,
    pub events_collected: bool,
    pub scans: Vec<ScanResult>,
    pub benchmarks: Vec<BenchmarkResult>,
    pub capacity: Vec<CapacityResult>,
    /// Última mudança registrada de cada disco (fingerprint → nota).
    pub changes: BTreeMap<String, NoteRecord>,
    pub warnings: Vec<String>,
}

pub struct CollectOptions {
    pub only_disk: Option<u32>,
    pub hours: u32,
    pub with_events: bool,
    pub record_history: bool,
    pub saved_results: bool,
    pub extra_scans: Vec<ScanResult>,
}

pub fn collect(ctx: &Ctx, all_disks: Vec<DiskInfo>, opts: CollectOptions) -> Result<Collected> {
    let mut warnings = Vec::new();
    if !ctx.elevated {
        warnings.push("Sem privilégios de Administrador: SMART pode vir incompleto e os contadores do Windows ficam indisponíveis.".into());
    }
    let mut disks: Vec<DiskInfo> = match opts.only_disk {
        Some(n) => vec![select_disk(&all_disks, &DiskRef::Number(n))?.clone()],
        None => all_disks.clone(),
    };

    let spinner = (!ctx.json).then(|| ui::spinner("Lendo SMART e contadores de confiabilidade…"));
    let smartctl = Smartctl::detect();
    if smartctl.is_none() {
        warnings.push("smartctl não encontrado. Instale o smartmontools (winget install smartmontools.smartmontools) ou defina DISKDOCTOR_SMARTCTL.".into());
    }
    let (health, mut health_warnings) =
        collect_health(&disks, smartctl.as_ref(), &HealthOptions { data_dir: ctx.data_dir.clone(), record_history: opts.record_history });
    warnings.append(&mut health_warnings);
    // Discos externos raramente dizem ao Windows se são HD ou SSD; o SMART diz (rotation_rate).
    for d in &mut disks {
        if d.media == MediaKind::Unknown {
            if let Some(rpm) = health.iter().find(|h| h.disk_number == d.number).and_then(|h| h.smart_ok()).and_then(|s| s.rotation_rate) {
                d.media = if rpm == 0 { MediaKind::Ssd } else { MediaKind::Hdd };
            }
        }
    }

    let mut events = Vec::new();
    let mut events_collected = false;
    if opts.with_events {
        if let Some(s) = &spinner {
            s.set_message(format!("Consultando eventos do Windows (últimas {} h)…", opts.hours));
        }
        match collect_events(opts.hours, 5000, &all_disks) {
            Ok(ev) => {
                events = ev;
                events_collected = true;
            }
            Err(e) => warnings.push(format!("não foi possível ler os eventos do Windows: {e:#}")),
        }
    }
    if let Some(s) = spinner {
        s.finish_and_clear();
    }

    let store = ResultStore::new(&ctx.data_dir);
    let mut scans = if opts.saved_results { store.latest_scans(30) } else { vec![] };
    scans.extend(opts.extra_scans);
    let benchmarks = if opts.saved_results { store.latest_benchmarks(30) } else { vec![] };
    let fingerprints: Vec<&str> = disks.iter().map(|d| d.fingerprint.as_str()).collect();
    scans.retain(|s| fingerprints.contains(&s.disk.fingerprint.as_str()));
    let mut benchmarks: Vec<BenchmarkResult> = benchmarks.into_iter().filter(|b| fingerprints.contains(&b.disk.fingerprint.as_str())).collect();
    let mut capacity: Vec<CapacityResult> = if opts.saved_results { store.latest_capacity(30) } else { vec![] };
    capacity.retain(|c| fingerprints.contains(&c.disk.fingerprint.as_str()));
    // Resultados salvos guardam número e tags da época do teste; mostre os atuais.
    let refresh = |id: &mut DiskIdentity| {
        if let Some(d) = disks.iter().find(|d| d.fingerprint == id.fingerprint) {
            id.number = d.number;
            id.tags = d.tags.clone();
        }
    };
    scans.iter_mut().for_each(|s| refresh(&mut s.disk));
    benchmarks.iter_mut().for_each(|b| refresh(&mut b.disk));
    capacity.iter_mut().for_each(|c| refresh(&mut c.disk));
    let journal = Journal::new(&ctx.data_dir);
    let changes = disks.iter().filter_map(|d| journal.latest_note(&d.fingerprint).map(|n| (d.fingerprint.clone(), n))).collect();

    Ok(Collected { smartctl: smartctl.map(|s| s.label()), disks, health, events, events_collected, scans, benchmarks, capacity, changes, warnings })
}

/// Grava no diário de cada disco o veredito desta execução (base do `history`).
pub fn record_diagnoses(ctx: &Ctx, c: &Collected, analysis: &AnalysisResult, hours: u32) {
    let journal = Journal::new(&ctx.data_dir);
    let now = chrono::Utc::now();
    for v in &analysis.verdicts {
        let Some(d) = c.disks.iter().find(|d| d.number == v.disk_number) else { continue };
        let since = c.changes.get(&d.fingerprint).map(|n| n.time);
        let io_events = c
            .events
            .iter()
            .filter(|e| e.disk_number == Some(d.number) && e.category.is_storage_io() && since.is_none_or(|t| e.time >= t))
            .count() as u64;
        let entry = JournalEntry::Diagnosis(DiagnosisRecord {
            time: now,
            disk_number: d.number,
            status: v.status,
            headline: v.headline.clone(),
            findings: v.findings.iter().map(|f| f.rule.clone()).collect(),
            io_events,
            window_hours: hours,
        });
        if let Err(e) = journal.append(&d.fingerprint, &entry) {
            ui::warn(&format!("não foi possível gravar o histórico de diagnósticos: {e:#}"));
            return;
        }
    }
}

pub fn analyze_collected(ctx: &Ctx, c: &Collected, hours: u32) -> AnalysisResult {
    analyze(&AnalysisInput {
        disks: &c.disks,
        health: &c.health,
        events: &c.events,
        scans: &c.scans,
        benchmarks: &c.benchmarks,
        capacity: &c.capacity,
        changes: &c.changes,
        event_window_hours: hours,
        events_collected: c.events_collected,
        elevated: ctx.elevated,
    })
}
