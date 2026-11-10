//! Gathers each drive's health sources: SMART (smartctl), Windows reliability counters
//! and the history (counter growth).

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
    /// Usable SMART data, if any.
    pub fn smart_ok(&self) -> Option<&SmartReport> {
        self.smart.as_ref().filter(|s| s.available)
    }
}

pub struct HealthOptions {
    pub data_dir: PathBuf,
    /// Saves a snapshot to the history (off in tests and with `--no-history`).
    pub record_history: bool,
}

/// Collects the health of the given drives. Also returns warnings for the user.
pub fn collect_health(disks: &[DiskInfo], smartctl: Option<&Smartctl>, opts: &HealthOptions) -> (Vec<DiskHealth>, Vec<String>) {
    let mut warnings = Vec::new();
    let counters = match windows_counters::load_counters() {
        Ok(c) => c,
        Err(e) => {
            warnings.push(tr!("Windows reliability counters unavailable: {e:#}", "contadores de confiabilidade do Windows indisponíveis: {e:#}"));
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
                // After a recorded change (e.g. a cable swap), growth is measured from it:
                // whatever rose before has already been dealt with.
                let since = journal.latest_note(&disk.fingerprint).map(|n| n.time);
                let previous: Vec<SmartSnapshot> =
                    history.load(&disk.fingerprint).into_iter().filter(|s| since.is_none_or(|t| s.time >= t)).collect();
                delta = compute_delta(&previous, &current, now);
                if opts.record_history {
                    if let Err(e) = history.append(&disk.fingerprint, &SmartSnapshot { time: now, counters: current }) {
                        warnings.push(tr!("could not save the SMART history: {e:#}", "não foi possível gravar o histórico SMART: {e:#}"));
                    }
                }
            }
            DiskHealth { disk_number: disk.number, smart, windows: counters.get(&disk.number).cloned(), history: delta }
        })
        .collect();
    (result, warnings)
}
