//! Consolidated report (JSON and HTML).
//!
//! The JSON is the complete, stable representation for other programs/agents (field names
//! and identifiers never change with the language; human-readable texts follow it). The
//! HTML is self-contained (inline CSS and SVG, no external dependencies) and escapes every
//! text that comes from the system (event messages, models etc.).

pub mod html;

use crate::analysis::AnalysisResult;
use crate::benchmark::BenchmarkResult;
use crate::capacity::CapacityResult;
use crate::events::StorageEvent;
use crate::health::DiskHealth;
use crate::i18n::{Lang, t};
use crate::inventory::DiskInfo;
use crate::scan::ScanResult;
use anyhow::{Context, Result};
use chrono::{DateTime, Local, Utc};
use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DiagnosticReport {
    pub tool: String,
    pub version: String,
    /// Language of the human-readable texts.
    #[serde(default)]
    pub language: Lang,
    pub generated_at: DateTime<Utc>,
    pub computer: Option<String>,
    pub elevated: bool,
    pub smartctl: Option<String>,
    pub event_window_hours: u32,
    pub disks: Vec<DiskInfo>,
    pub health: Vec<DiskHealth>,
    pub events: Vec<StorageEvent>,
    pub scans: Vec<ScanResult>,
    pub benchmarks: Vec<BenchmarkResult>,
    #[serde(default)]
    pub capacity: Vec<CapacityResult>,
    pub analysis: AnalysisResult,
    pub limitations: Vec<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ReportFormat {
    Html,
    Json,
    #[value(alias = "ambos")]
    Both,
}

/// Technical limitations the reader of the report needs to know.
pub fn standard_limitations() -> Vec<String> {
    [
        t(
            "The Surface Scan goes through the normal Windows path (drive driver, controller driver, cable/slot). Errors and \
             timings reflect that whole chain; isolating the drive requires swapping components (cable, port, slot).",
            "O Surface Scan passa pelo caminho normal do Windows (driver de disco, driver da controladora, cabo/slot). Erros e \
             tempos refletem esse conjunto inteiro; isolar o disco exige trocar componentes (cabo, porta, slot).",
        ),
        t(
            "Windows applies its own timeout to drive commands (usually 30–60 s) and may reset the link before the diskdoctor \
             timeout; long hangs can show up as a driver error.",
            "O Windows aplica o próprio tempo limite aos comandos de disco (normalmente 30–60 s) e pode reiniciar o link antes do \
             tempo limite do diskdoctor; travamentos longos podem aparecer como erro do driver.",
        ),
        t(
            "RAID/Intel RST controllers, USB-SATA adapters and some hubs do not pass SMART commands through.",
            "Controladoras RAID/Intel RST, adaptadores USB-SATA e alguns hubs não repassam comandos SMART.",
        ),
        t(
            "On SSDs, areas never written or released by TRIM may be answered without reading the flash; the Surface Scan \
             does not exercise those cells.",
            "Em SSDs, áreas nunca gravadas ou liberadas por TRIM podem ser respondidas sem ler a memória flash; o Surface Scan \
             não exercita essas células.",
        ),
        t(
            "The Surface Scan does not write to the drive, so it does not force the reallocation of pending sectors (that only happens when the sector is rewritten).",
            "O Surface Scan não grava no disco; por isso não força a realocação de setores pendentes (isso só ocorre quando o setor é regravado).",
        ),
        t(
            "The Benchmark uses queue depth 1 (QD1) to expose latency; its numbers are below marketing figures, which are \
             measured with deep queues.",
            "O Benchmark usa fila de profundidade 1 (QD1) para expor latência; os números ficam abaixo dos valores de marketing, \
             medidos com filas profundas.",
        ),
        t(
            "Events depend on the Windows Event Log: if the system drive hangs, the event may never be written.",
            "Eventos dependem do Log de Eventos do Windows: se o disco do sistema travar, o evento pode nem ser gravado.",
        ),
        t(
            "SMART attributes vary between manufacturers; vendor-specific counters (e.g. vendor CRC) must be judged by their \
             trend (whether they grow), not by their absolute value.",
            "Atributos SMART variam entre fabricantes; contadores específicos (ex.: CRC do fabricante) devem ser avaliados pela \
             tendência (se crescem), não pelo valor absoluto.",
        ),
        t(
            "Capacity Verification tests only the free space of the volume; space already in use is not checked. USB flash \
             drives and memory cards have no SMART.",
            "A Capacity Verification testa só o espaço livre do volume; o espaço já ocupado não é conferido. Pendrives e \
             cartões de memória não têm SMART.",
        ),
    ]
    .iter()
    .map(|s| crate::util::one_line(s))
    .collect()
}

pub fn to_json(report: &DiagnosticReport) -> Result<String> {
    Ok(serde_json::to_string_pretty(report)?)
}

/// Output paths: `output` can be a folder, a file with an extension or a prefix.
pub fn output_paths(output: Option<&Path>, format: ReportFormat, now: DateTime<Local>) -> Vec<PathBuf> {
    let default_name = format!("diskdoctor-report-{}", now.format("%Y%m%d-%H%M%S"));
    let base: PathBuf = match output {
        None => PathBuf::from(&default_name),
        Some(p) if p.is_dir() => p.join(&default_name),
        Some(p) => {
            let ext = p.extension().map(|e| e.to_string_lossy().to_ascii_lowercase());
            if matches!(ext.as_deref(), Some("html" | "htm" | "json")) { p.with_extension("") } else { p.to_path_buf() }
        }
    };
    match format {
        ReportFormat::Html => vec![base.with_extension("html")],
        ReportFormat::Json => vec![base.with_extension("json")],
        ReportFormat::Both => vec![base.with_extension("html"), base.with_extension("json")],
    }
}

pub fn write_report(report: &DiagnosticReport, format: ReportFormat, output: Option<&Path>) -> Result<Vec<PathBuf>> {
    let paths = output_paths(output, format, Local::now());
    for path in &paths {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent).with_context(|| tr!("could not create {}", "não foi possível criar {}", parent.display()))?;
        }
        let content = if path.extension().is_some_and(|e| e == "json") { to_json(report)? } else { html::render(report) };
        fs::write(path, content).with_context(|| tr!("could not write {}", "não foi possível gravar {}", path.display()))?;
    }
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn computes_output_paths() {
        let now = Local.with_ymd_and_hms(2026, 11, 8, 15, 30, 0).unwrap();
        assert_eq!(output_paths(None, ReportFormat::Html, now), vec![PathBuf::from("diskdoctor-report-20261108-153000.html")]);
        assert_eq!(
            output_paths(Some(Path::new("out/rep.json")), ReportFormat::Both, now),
            vec![PathBuf::from("out/rep.html"), PathBuf::from("out/rep.json")]
        );
        assert_eq!(output_paths(Some(Path::new("my-report")), ReportFormat::Json, now), vec![PathBuf::from("my-report.json")]);
        let dir = tempfile::tempdir().unwrap();
        let p = output_paths(Some(dir.path()), ReportFormat::Html, now);
        assert_eq!(p[0], dir.path().join("diskdoctor-report-20261108-153000.html"));
    }

    #[test]
    fn limitations_are_single_line_in_both_languages() {
        for lang in crate::i18n::ALL {
            let _l = crate::i18n::scoped(lang);
            assert!(standard_limitations().iter().all(|l| !l.contains('\n') && !l.contains("  ")));
        }
    }
}
