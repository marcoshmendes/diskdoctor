//! Persistência local de resultados (scan, benchmark) para o `report` usar depois.
//!
//! Pasta padrão: `%LOCALAPPDATA%\diskdoctor` (sobrescreva com `DISKDOCTOR_DATA_DIR`).
//! Os arquivos têm o serial mascarado e o fingerprint do disco — o `report`
//! associa resultados ao disco pelo fingerprint, porque o número do disco pode
//! mudar entre reinicializações.

use crate::benchmark::BenchmarkResult;
use crate::capacity::CapacityResult;
use crate::inventory::DiskIdentity;
use crate::scan::ScanResult;
use anyhow::{Context, Result};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

pub fn default_data_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("DISKDOCTOR_DATA_DIR") {
        return PathBuf::from(dir);
    }
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        return PathBuf::from(local).join("diskdoctor");
    }
    PathBuf::from("diskdoctor-data")
}

pub struct ResultStore {
    base: PathBuf,
}

impl ResultStore {
    pub fn new(base: &Path) -> Self {
        Self { base: base.to_path_buf() }
    }

    pub fn base(&self) -> &Path {
        &self.base
    }

    fn save<T: Serialize>(&self, sub: &str, prefix: &str, fingerprint: &str, value: &T) -> Result<PathBuf> {
        let dir = self.base.join(sub);
        fs::create_dir_all(&dir).with_context(|| format!("não foi possível criar {}", dir.display()))?;
        let fp: String = fingerprint.chars().filter(|c| c.is_ascii_alphanumeric()).take(8).collect();
        let path = dir.join(format!("{prefix}-{fp}-{}.json", Utc::now().format("%Y%m%d-%H%M%S")));
        fs::write(&path, serde_json::to_string_pretty(value)?).with_context(|| format!("não foi possível gravar {}", path.display()))?;
        Ok(path)
    }

    pub fn save_scan(&self, r: &ScanResult) -> Result<PathBuf> {
        self.save("scans", "scan", &r.disk.fingerprint, r)
    }

    pub fn save_benchmark(&self, r: &BenchmarkResult) -> Result<PathBuf> {
        self.save("benchmarks", "bench", &r.disk.fingerprint, r)
    }

    fn load_all<T: DeserializeOwned>(&self, sub: &str) -> Vec<T> {
        let Ok(entries) = fs::read_dir(self.base.join(sub)) else { return vec![] };
        entries
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
            .filter_map(|e| fs::read_to_string(e.path()).ok())
            .filter_map(|t| serde_json::from_str(&t).ok())
            .collect()
    }

    pub fn save_capacity(&self, r: &CapacityResult) -> Result<PathBuf> {
        self.save("capacity", "capacity", &r.disk.fingerprint, r)
    }

    /// Resultado mais recente de cada disco (por fingerprint) nos últimos `max_age_days` dias.
    fn latest<T: DeserializeOwned>(&self, sub: &str, max_age_days: i64, key: impl Fn(&T) -> (&DiskIdentity, DateTime<Utc>)) -> Vec<T> {
        let cutoff = Utc::now() - ChronoDuration::days(max_age_days);
        let mut best: HashMap<String, T> = HashMap::new();
        for r in self.load_all::<T>(sub).into_iter().filter(|r| key(r).1 >= cutoff) {
            let (disk, time) = key(&r);
            match best.get(&disk.fingerprint) {
                Some(cur) if key(cur).1 >= time => {}
                _ => {
                    best.insert(disk.fingerprint.clone(), r);
                }
            }
        }
        let mut v: Vec<T> = best.into_values().collect();
        v.sort_by_key(|r| key(r).0.number);
        v
    }

    pub fn latest_scans(&self, max_age_days: i64) -> Vec<ScanResult> {
        self.latest("scans", max_age_days, |r: &ScanResult| (&r.disk, r.finished_at))
    }

    pub fn latest_benchmarks(&self, max_age_days: i64) -> Vec<BenchmarkResult> {
        self.latest("benchmarks", max_age_days, |r: &BenchmarkResult| (&r.disk, r.finished_at))
    }

    pub fn latest_capacity(&self, max_age_days: i64) -> Vec<CapacityResult> {
        self.latest("capacity", max_age_days, |r: &CapacityResult| (&r.disk, r.finished_at))
    }

    /// Todos os scans salvos de um disco (para a linha do tempo do `history`).
    pub fn scans_for(&self, fingerprint: &str) -> Vec<ScanResult> {
        self.load_all::<ScanResult>("scans").into_iter().filter(|r| r.disk.fingerprint == fingerprint).collect()
    }

    pub fn capacity_for(&self, fingerprint: &str) -> Vec<CapacityResult> {
        self.load_all::<CapacityResult>("capacity").into_iter().filter(|r| r.disk.fingerprint == fingerprint).collect()
    }
}

pub fn load_scan_file(path: &Path) -> Result<ScanResult> {
    let text = fs::read_to_string(path).with_context(|| format!("não foi possível ler {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("{} não é um resultado de scan do diskdoctor", path.display()))
}
