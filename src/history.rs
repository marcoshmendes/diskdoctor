//! Local history of SMART counters per drive.
//!
//! A high counter may be old and stable (e.g. CRC errors from a cable already replaced);
//! what really signals an active problem is **growth**. Every `health`/`report` saves a
//! snapshot to `<data>/history/<fingerprint>.jsonl` and compares it with the oldest one of
//! the last 90 days.

use anyhow::{Context, Result};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Counters that signal a problem when they grow: (key, English label, pt-BR label).
pub const PROBLEM_COUNTERS: &[(&str, &str, &str)] = &[
    ("reallocated_sectors", "Reallocated sectors", "Setores realocados"),
    ("pending_sectors", "Pending sectors", "Setores pendentes"),
    ("offline_uncorrectable", "Offline uncorrectable sectors", "Setores incorrigíveis (offline)"),
    ("reported_uncorrectable", "Reported uncorrectable errors", "Erros incorrigíveis reportados"),
    ("interface_crc_errors", "Interface CRC errors (199)", "Erros CRC de interface (199)"),
    ("vendor_crc_errors", "CRC errors (vendor counter)", "Erros CRC (contador do fabricante)"),
    ("sata_phy_errors", "SATA PHY errors", "Erros de PHY SATA"),
    ("command_timeouts", "Command timeouts", "Timeouts de comando"),
    ("media_errors", "Media errors (NVMe)", "Erros de mídia (NVMe)"),
    ("error_log_entries", "Error log entries", "Entradas no log de erros"),
    ("grown_bad_blocks", "Grown bad blocks", "Blocos defeituosos adquiridos"),
    ("program_erase_failures", "Program/erase failures", "Falhas de programação/apagamento"),
    ("unsafe_shutdowns", "Unsafe shutdowns", "Desligamentos bruscos"),
];

/// Label of a problem counter in the active language (the key itself if unknown).
pub fn counter_label(key: &str) -> String {
    PROBLEM_COUNTERS
        .iter()
        .find(|(k, ..)| *k == key)
        .map(|(_, en, pt)| crate::i18n::t(en, pt).to_string())
        .unwrap_or_else(|| key.to_string())
}

pub const MAX_BASELINE_AGE_DAYS: i64 = 90;
const MAX_LINES: usize = 500;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SmartSnapshot {
    pub time: DateTime<Utc>,
    pub counters: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CounterChange {
    pub key: String,
    pub label: String,
    pub before: u64,
    pub after: u64,
}

impl CounterChange {
    pub fn increase(&self) -> u64 {
        self.after.saturating_sub(self.before)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SmartDelta {
    pub baseline_time: DateTime<Utc>,
    pub snapshots: usize,
    pub changes: Vec<CounterChange>,
}

impl SmartDelta {
    pub fn change(&self, key: &str) -> Option<&CounterChange> {
        self.changes.iter().find(|c| c.key == key)
    }
}

/// Compares the current state with the oldest snapshot inside the window.
pub fn compute_delta(history: &[SmartSnapshot], current: &BTreeMap<String, u64>, now: DateTime<Utc>) -> Option<SmartDelta> {
    let oldest_allowed = now - ChronoDuration::days(MAX_BASELINE_AGE_DAYS);
    let newest_allowed = now - ChronoDuration::minutes(5);
    let baseline = history.iter().filter(|s| s.time >= oldest_allowed && s.time <= newest_allowed).min_by_key(|s| s.time)?;
    let changes = PROBLEM_COUNTERS
        .iter()
        .filter_map(|(key, ..)| {
            let before = *baseline.counters.get(*key)?;
            let after = *current.get(*key)?;
            (after > before).then(|| CounterChange { key: key.to_string(), label: counter_label(key), before, after })
        })
        .collect();
    Some(SmartDelta { baseline_time: baseline.time, snapshots: history.len(), changes })
}

pub struct HistoryStore {
    dir: PathBuf,
}

impl HistoryStore {
    pub fn new(data_dir: &Path) -> Self {
        Self { dir: data_dir.join("history") }
    }

    fn path(&self, fingerprint: &str) -> PathBuf {
        let safe: String = fingerprint.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        self.dir.join(format!("{safe}.jsonl"))
    }

    pub fn load(&self, fingerprint: &str) -> Vec<SmartSnapshot> {
        fs::read_to_string(self.path(fingerprint))
            .map(|text| text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect())
            .unwrap_or_default()
    }

    pub fn append(&self, fingerprint: &str, snapshot: &SmartSnapshot) -> Result<()> {
        fs::create_dir_all(&self.dir).with_context(|| tr!("could not create {}", "não foi possível criar {}", self.dir.display()))?;
        let path = self.path(fingerprint);
        let mut existing = self.load(fingerprint);
        if existing.len() >= MAX_LINES {
            existing.drain(..existing.len() + 1 - MAX_LINES);
            let text: String = existing.iter().filter_map(|s| serde_json::to_string(s).ok()).map(|l| l + "\n").collect();
            fs::write(&path, text)?;
        }
        let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
        writeln!(f, "{}", serde_json::to_string(snapshot)?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(days_ago: i64, pairs: &[(&str, u64)]) -> SmartSnapshot {
        SmartSnapshot {
            time: Utc::now() - ChronoDuration::days(days_ago),
            counters: pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
        }
    }

    #[test]
    fn detects_growth_against_oldest_baseline() {
        let history = vec![snap(120, &[("interface_crc_errors", 0)]), snap(30, &[("interface_crc_errors", 10)]), snap(1, &[("interface_crc_errors", 15)])];
        let current: BTreeMap<String, u64> = [("interface_crc_errors".to_string(), 42), ("power_on_hours".to_string(), 9)].into();
        let delta = compute_delta(&history, &current, Utc::now()).unwrap();
        let c = delta.change("interface_crc_errors").unwrap();
        assert_eq!((c.before, c.after, c.increase()), (10, 42, 32), "oldest baseline within 90 days");
        assert_eq!(delta.snapshots, 3);
        assert!(delta.change("power_on_hours").is_none(), "power-on hours is not a problem counter");
    }

    #[test]
    fn counter_labels_follow_the_language() {
        {
            let _en = crate::i18n::scoped(crate::i18n::Lang::En);
            assert_eq!(counter_label("pending_sectors"), "Pending sectors");
        }
        let _pt = crate::i18n::scoped(crate::i18n::Lang::PtBr);
        assert_eq!(counter_label("pending_sectors"), "Setores pendentes");
        assert_eq!(counter_label("unknown_key"), "unknown_key");
    }

    #[test]
    fn no_baseline_means_no_delta() {
        let current: BTreeMap<String, u64> = [("pending_sectors".to_string(), 1)].into();
        assert!(compute_delta(&[], &current, Utc::now()).is_none());
        // A snapshot saved moments ago (in the same run) is not a baseline.
        let recent = vec![SmartSnapshot { time: Utc::now(), counters: BTreeMap::new() }];
        assert!(compute_delta(&recent, &current, Utc::now()).is_none());
    }

    #[test]
    fn store_roundtrip_and_trim() {
        let dir = tempfile::tempdir().unwrap();
        let store = HistoryStore::new(dir.path());
        assert!(store.load("abc").is_empty());
        for i in 0..(MAX_LINES + 3) {
            store.append("abc", &snap(0, &[("unsafe_shutdowns", i as u64)])).unwrap();
        }
        let loaded = store.load("abc");
        assert_eq!(loaded.len(), MAX_LINES);
        assert_eq!(loaded.last().unwrap().counters["unsafe_shutdowns"], (MAX_LINES + 2) as u64);
        // Sanitized file name.
        store.append("../../x", &snap(0, &[])).unwrap();
        assert!(dir.path().join("history").join("x.jsonl").exists());
    }
}
