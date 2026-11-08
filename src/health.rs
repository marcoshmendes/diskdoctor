//! Agrega as fontes de saúde de cada disco: SMART (smartctl), contadores de
//! confiabilidade do Windows e o histórico (crescimento de contadores).

use crate::history::{HistoryStore, SmartDelta, SmartSnapshot, compute_delta};
use crate::inventory::DiskInfo;
use crate::smart::{SmartReport, Smartctl};
use crate::windows_counters::{self, ReliabilityCounters};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DiskHealth {
    pub disk_number: u32,
    pub smart: Option<SmartReport>,
    pub windows: Option<ReliabilityCounters>,
    pub history: Option<SmartDelta>,
}

impl DiskHealth {
    /// SMART utilizável, se houver.
    pub fn smart_ok(&self) -> Option<&SmartReport> {
        self.smart.as_ref().filter(|s| s.available)
    }
}

pub struct HealthOptions {
    pub data_dir: PathBuf,
    /// Grava snapshot no histórico (desligado em testes e com `--no-history`).
    pub record_history: bool,
}

/// Coleta saúde dos discos informados. Retorna também avisos para o usuário.
pub fn collect_health(disks: &[DiskInfo], smartctl: Option<&Smartctl>, opts: &HealthOptions) -> (Vec<DiskHealth>, Vec<String>) {
    let mut warnings = Vec::new();
    let counters = match windows_counters::load_counters() {
        Ok(c) => c,
        Err(e) => {
            warnings.push(format!("contadores de confiabilidade do Windows indisponíveis: {e:#}"));
            Default::default()
        }
    };
    let history = HistoryStore::new(&opts.data_dir);
    let journal = crate::journal::Journal::new(&opts.data_dir);
    let now = Utc::now();

    let result = disks
        .iter()
        .map(|disk| {
            let smart = smartctl.map(|s| s.query(disk.number));
            let mut delta = None;
            if let Some(report) = smart.as_ref().filter(|s| s.available) {
                let current = report.counters();
                // Depois de uma mudança registrada (ex.: troca de cabo), o crescimento é
                // medido a partir dela: o que subiu antes já foi tratado.
                let since = journal.latest_note(&disk.fingerprint).map(|n| n.time);
                let previous: Vec<SmartSnapshot> =
                    history.load(&disk.fingerprint).into_iter().filter(|s| since.is_none_or(|t| s.time >= t)).collect();
                delta = compute_delta(&previous, &current, now);
                if opts.record_history {
                    if let Err(e) = history.append(&disk.fingerprint, &SmartSnapshot { time: now, counters: current }) {
                        warnings.push(format!("não foi possível gravar o histórico SMART: {e:#}"));
                    }
                }
            }
            DiskHealth { disk_number: disk.number, smart, windows: counters.get(&disk.number).cloned(), history: delta }
        })
        .collect();
    (result, warnings)
}
