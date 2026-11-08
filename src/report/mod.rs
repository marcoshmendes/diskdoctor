//! Relatório consolidado (JSON e HTML).
//!
//! O JSON é a representação completa e estável para outros programas/agentes;
//! o HTML é autocontido (CSS e SVG embutidos, sem dependências externas) e
//! escapa todo texto vindo do sistema (mensagens de evento, modelos etc.).

pub mod html;

use crate::analysis::AnalysisResult;
use crate::benchmark::BenchmarkResult;
use crate::capacity::CapacityResult;
use crate::events::StorageEvent;
use crate::health::DiskHealth;
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

/// Limitações técnicas que o leitor do relatório precisa conhecer.
pub fn standard_limitations() -> Vec<String> {
    [
        "O teste de leitura passa pelo caminho normal do Windows (driver de disco, driver da controladora, cabo/slot). Erros e \
         tempos refletem esse conjunto inteiro; isolar o disco exige trocar componentes (cabo, porta, slot).",
        "O Windows aplica o próprio tempo limite aos comandos de disco (normalmente 30–60 s) e pode reiniciar o link antes do \
         tempo limite do diskdoctor; travamentos longos podem aparecer como erro do driver.",
        "Controladoras RAID/Intel RST, adaptadores USB-SATA e alguns hubs não repassam comandos SMART.",
        "Em SSDs, áreas nunca gravadas ou liberadas por TRIM podem ser respondidas sem ler a memória flash; o teste de leitura \
         não exercita essas células.",
        "O teste não grava no disco; por isso não força a realocação de setores pendentes (isso só ocorre quando o setor é regravado).",
        "Benchmarks usam fila de profundidade 1 (QD1) para expor latência; os números ficam abaixo dos valores de marketing, \
         medidos com filas profundas.",
        "Eventos dependem do Log de Eventos do Windows: se o disco do sistema travar, o evento pode nem ser gravado.",
        "Atributos SMART variam entre fabricantes; contadores específicos (ex.: CRC do fabricante) devem ser avaliados pela \
         tendência (se crescem), não pelo valor absoluto.",
        "A verificação de capacidade testa só o espaço livre do volume; o espaço já ocupado não é conferido. Pendrives e \
         cartões de memória não têm SMART.",
    ]
    .iter()
    .map(|s| crate::util::one_line(s))
    .collect()
}

pub fn to_json(report: &DiagnosticReport) -> Result<String> {
    Ok(serde_json::to_string_pretty(report)?)
}

/// Caminhos de saída: `output` pode ser pasta, arquivo com extensão ou prefixo.
pub fn output_paths(output: Option<&Path>, format: ReportFormat, now: DateTime<Local>) -> Vec<PathBuf> {
    let default_name = format!("diskdoctor-relatorio-{}", now.format("%Y%m%d-%H%M%S"));
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
            fs::create_dir_all(parent).with_context(|| format!("não foi possível criar {}", parent.display()))?;
        }
        let content = if path.extension().is_some_and(|e| e == "json") { to_json(report)? } else { html::render(report) };
        fs::write(path, content).with_context(|| format!("não foi possível gravar {}", path.display()))?;
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
        assert_eq!(output_paths(None, ReportFormat::Html, now), vec![PathBuf::from("diskdoctor-relatorio-20261108-153000.html")]);
        assert_eq!(
            output_paths(Some(Path::new("saida/rel.json")), ReportFormat::Both, now),
            vec![PathBuf::from("saida/rel.html"), PathBuf::from("saida/rel.json")]
        );
        assert_eq!(output_paths(Some(Path::new("meu-relatorio")), ReportFormat::Json, now), vec![PathBuf::from("meu-relatorio.json")]);
        let dir = tempfile::tempdir().unwrap();
        let p = output_paths(Some(dir.path()), ReportFormat::Html, now);
        assert_eq!(p[0], dir.path().join("diskdoctor-relatorio-20261108-153000.html"));
    }

    #[test]
    fn limitations_are_single_line() {
        assert!(standard_limitations().iter().all(|l| !l.contains('\n') && !l.contains("  ")));
    }
}
