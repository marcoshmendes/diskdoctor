//! Saída no terminal (pt-BR): tabelas, cores, confirmações e barras de progresso.

use crate::analysis::{AnalysisResult, DiskStatus, Finding, Severity};
use crate::benchmark::{BenchMetrics, BenchmarkResult};
use crate::events::{Attribution, StorageEvent};
use crate::health::DiskHealth;
use crate::inventory::DiskInfo;
use crate::scan::ScanResult;
use crate::util::{decimal, format_bytes, format_bytes_binary, format_duration, format_mbps, format_ms, int, truncate_chars};
use anyhow::{Result, bail};
use chrono::Local;
use comfy_table::{Cell, Color, ContentArrangement, Table, presets::UTF8_FULL_CONDENSED};
use console::style;
use indicatif::{ProgressBar, ProgressState, ProgressStyle};
use std::collections::BTreeMap;
use std::io::{BufRead, IsTerminal, Write};
use std::time::Duration;

pub fn init_colors(no_color: bool) {
    if no_color || std::env::var_os("NO_COLOR").is_some() {
        console::set_colors_enabled(false);
        console::set_colors_enabled_stderr(false);
    }
}

pub fn heading(text: &str) {
    println!("\n{}", style(text).bold().cyan());
}

pub fn warn(text: &str) {
    eprintln!("{} {}", style("Aviso:").yellow().bold(), text);
}

pub fn note(text: &str) {
    println!("{}", style(text).dim());
}

pub fn error(text: &str) {
    eprintln!("{} {}", style("Erro:").red().bold(), text);
}

pub fn table() -> Table {
    let mut t = Table::new();
    t.load_style(UTF8_FULL_CONDENSED.with_rounded_corners()).set_content_arrangement(ContentArrangement::Dynamic);
    t
}

pub fn dim(text: &str) -> String {
    style(text).dim().to_string()
}

pub fn ok_mark() -> String {
    style("✔").green().bold().to_string()
}

/// Tags destacadas: `[Studio] [Windows]`.
pub fn tag_list(tags: &[String]) -> String {
    tags.iter().map(|t| style(format!("[{t}]")).magenta().bold().to_string()).collect::<Vec<_>>().join(" ")
}

pub fn status_text(s: DiskStatus) -> String {
    let label = s.label().to_uppercase();
    match s {
        DiskStatus::Critical => style(label).red().bold().to_string(),
        DiskStatus::Warning => style(label).color256(208).bold().to_string(),
        DiskStatus::Attention => style(label).yellow().bold().to_string(),
        DiskStatus::Healthy => style(label).green().bold().to_string(),
        DiskStatus::Unknown => style(label).dim().to_string(),
    }
}

fn severity_text(s: Severity) -> String {
    match s {
        Severity::Critical => style(s.label()).red().bold().to_string(),
        Severity::Warning => style(s.label()).color256(208).bold().to_string(),
        Severity::Attention => style(s.label()).yellow().to_string(),
        Severity::Info => style(s.label()).cyan().to_string(),
    }
}

pub fn windows_health_label(s: &str) -> &str {
    match s {
        "Healthy" | "0" => "Íntegro",
        "Warning" | "1" => "Aviso",
        "Unhealthy" | "2" => "Não íntegro",
        "" => "—",
        other => other,
    }
}

/// Barra de uso `██████░░░░` com cor pelo nível de ocupação.
pub fn usage_bar(used: u64, total: u64, width: usize) -> String {
    let frac = if total == 0 { 0.0 } else { (used as f64 / total as f64).clamp(0.0, 1.0) };
    let filled = ((frac * width as f64).round() as usize).min(width);
    let bar = format!("{}{}", "█".repeat(filled), "░".repeat(width - filled));
    match frac {
        f if f >= 0.9 => style(bar).red().to_string(),
        f if f >= 0.75 => style(bar).yellow().to_string(),
        _ => style(bar).green().to_string(),
    }
}

fn volumes_cell(d: &DiskInfo) -> String {
    let mut lines: Vec<String> = d
        .partitions
        .iter()
        .filter_map(|p| {
            let letter = p.drive_letter.as_ref()?;
            let label = p.label.as_deref().map(|l| format!(" {l}")).unwrap_or_default();
            let mut line = format!("{letter}:{label} ({})", p.file_system.as_deref().unwrap_or("?"));
            if let (Some(free), Some(size)) = (p.volume_free, p.volume_size) {
                let used = size.saturating_sub(free);
                line.push_str(&format!("\n{} {}% · {} livres", usage_bar(used, size, 10), percent(used, size), format_bytes(free)));
            }
            Some(line)
        })
        .collect();
    let hidden = d.partitions.iter().filter(|p| p.drive_letter.is_none()).count();
    if hidden > 0 {
        lines.push(format!("+{hidden} sem letra"));
    }
    if lines.is_empty() { "—".into() } else { lines.join("\n") }
}

pub fn print_disk_table(disks: &[DiskInfo]) {
    heading(&format!("Discos físicos detectados ({})", disks.len()));
    let mut t = table();
    t.set_header(vec!["#", "Modelo", "Serial", "Capacidade", "Tipo", "Volumes", "Windows"]);
    for d in disks {
        let mut model = d.model.clone();
        if !d.tags.is_empty() {
            model = format!("{}\n{model}", tag_list(&d.tags));
        }
        if d.is_boot || d.is_system {
            model.push_str("\n(disco do sistema)");
        }
        t.add_row(vec![
            Cell::new(d.number),
            Cell::new(model),
            Cell::new(&d.serial_masked),
            Cell::new(format_bytes(d.size_bytes)),
            Cell::new(d.class_label()),
            Cell::new(volumes_cell(d)),
            Cell::new(windows_health_label(&d.windows_health)),
        ]);
    }
    println!("{t}");
    note("O estado \"Íntegro\" do Windows é superficial. Use `diskdoctor health` para SMART, eventos e diagnóstico.");
}

/// Painel compacto (topo do modo interativo): disco, tipo, capacidade e uso de cada volume.
pub fn print_disk_overview(disks: &[DiskInfo]) {
    let mut t = table();
    t.set_header(vec!["#", "Disco", "Tipo", "Capacidade", "Uso"]);
    for d in disks {
        let mut name = d.model.clone();
        if !d.tags.is_empty() {
            name = format!("{} {name}", tag_list(&d.tags));
        }
        if d.is_boot || d.is_system {
            name.push_str(&format!(" {}", style("(sistema)").dim()));
        }
        let usage: Vec<String> = d
            .partitions
            .iter()
            .filter_map(|p| {
                let letter = p.drive_letter.as_ref()?;
                Some(match (p.volume_free, p.volume_size) {
                    (Some(free), Some(size)) => {
                        let used = size.saturating_sub(free);
                        format!("{letter}: {} {:>3}% · {} livres", usage_bar(used, size, 12), percent(used, size).split(',').next().unwrap_or("0"), format_bytes(free))
                    }
                    _ => format!("{letter}:"),
                })
            })
            .collect();
        t.add_row(vec![
            Cell::new(d.number),
            Cell::new(name),
            Cell::new(d.class_label()),
            Cell::new(format_bytes(d.size_bytes)),
            Cell::new(if usage.is_empty() { "—".into() } else { usage.join("\n") }),
        ]);
    }
    println!("{t}");
}

pub fn print_finding(f: &Finding, indent: &str) {
    println!(
        "{indent}{} {} {}",
        style("▸").bold(),
        severity_text(f.severity),
        style(format!("{} (confiança {})", f.title, f.confidence.label().to_lowercase())).bold()
    );
    for ev in &f.evidence {
        println!("{indent}    • [{}] {}", ev.source.label(), ev.text);
    }
    if !f.possible_causes.is_empty() {
        println!("{indent}    {} {}", style("Possíveis causas:").dim(), f.possible_causes.join("; "));
    }
    for r in &f.recommendations {
        println!("{indent}    {} {r}", style("→").green());
    }
}

fn smart_summary_lines(h: Option<&DiskHealth>) -> Vec<String> {
    let Some(s) = h.and_then(|h| h.smart.as_ref()) else {
        return vec!["SMART: não consultado (smartctl não encontrado)".into()];
    };
    if !s.available {
        return vec![format!("SMART: indisponível — {}", s.error.as_deref().unwrap_or("sem dados"))];
    }
    let o = |v: Option<u64>| v.map(int).unwrap_or_else(|| "—".into());
    let passed = match s.overall_passed {
        Some(true) => style("aprovado").green().to_string(),
        Some(false) => style("FALHOU").red().bold().to_string(),
        None => "—".into(),
    };
    let mut lines = vec![
        format!(
            "SMART: {passed} · temperatura {} · ligado {} h · ciclos {} · desgaste {}",
            s.temperature_c.map(|t| format!("{t} °C")).unwrap_or("—".into()),
            o(s.power_on_hours),
            o(s.power_cycles),
            s.wear_used_percent.map(|w| format!("{w}%")).unwrap_or("—".into())
        ),
        format!(
            "Setores realocados {} · pendentes {} · incorrigíveis {} · CRC interface {} · CRC fabricante {} · timeouts {}",
            o(s.reallocated_sectors),
            o(s.pending_sectors),
            o(s.offline_uncorrectable.or(s.reported_uncorrectable)),
            o(s.interface_crc_errors),
            o(s.vendor_crc_errors),
            o(s.command_timeouts)
        ),
    ];
    if s.is_nvme() {
        lines.push(format!("NVMe: erros de mídia {} · log de erros {} · reserva {}%", o(s.media_errors), o(s.error_log_entries), o(s.available_spare_percent)));
    }
    if let Some(u) = s.unsafe_shutdowns {
        lines.push(format!("Desligamentos bruscos: {}", int(u)));
    }
    if let Some(d) = h.and_then(|h| h.history.as_ref()) {
        let since = d.baseline_time.with_timezone(&Local).format("%d/%m/%Y");
        if d.changes.is_empty() {
            lines.push(format!("Histórico: nenhum contador de problema aumentou desde {since}"));
        } else {
            for c in &d.changes {
                lines.push(style(format!("Histórico: {} {} → {} desde {since}", c.label, int(c.before), int(c.after))).yellow().to_string());
            }
        }
    }
    lines
}

pub fn print_health(disks: &[DiskInfo], health: &[DiskHealth], analysis: &AnalysisResult) {
    for v in &analysis.verdicts {
        let Some(d) = disks.iter().find(|d| d.number == v.disk_number) else { continue };
        let letters = d.volume_letters();
        println!(
            "\n{} {}",
            style(format!(
                "━━ {} · {} · {}{}",
                d.title(),
                d.class_label(),
                format_bytes(d.size_bytes),
                if letters.is_empty() { String::new() } else { format!(" · {}", letters.join(" ")) }
            ))
            .bold(),
            if d.is_boot || d.is_system { style("(sistema)").dim().to_string() } else { String::new() }
        );
        for line in smart_summary_lines(health.iter().find(|h| h.disk_number == d.number)) {
            println!("   {line}");
        }
        println!("   Veredito: {} — {}", status_text(v.status), v.headline);
        for s in v.sources_used.iter().filter(|s| s.starts_with("Mudança registrada")) {
            println!("   {}", style(s).magenta());
        }
        for f in &v.findings {
            print_finding(f, "   ");
        }
        if !v.gaps.is_empty() {
            println!("   {}", style("Não verificado:").dim());
            for g in &v.gaps {
                println!("   {}", style(format!("  - {g}")).dim());
            }
        }
    }
    print_global(analysis);
}

pub fn print_global(analysis: &AnalysisResult) {
    if !analysis.global_findings.is_empty() {
        heading("Achados gerais (sem disco específico)");
        for f in &analysis.global_findings {
            print_finding(f, " ");
        }
    }
    println!();
    for n in &analysis.notes {
        note(&format!("ℹ {n}"));
    }
}

pub fn print_events(events: &[StorageEvent], hours: u32, limit: usize) {
    heading(&format!("Eventos de armazenamento nas últimas {hours} h ({})", events.len()));
    if events.is_empty() {
        println!("Nenhum evento de armazenamento encontrado no período.");
        note("Lembre-se: se o disco do sistema trava, o Windows pode não conseguir registrar o evento.");
        return;
    }
    let mut groups: BTreeMap<(String, String, String), (u64, chrono::DateTime<chrono::Utc>)> = BTreeMap::new();
    for e in events {
        let disk = e.disk_number.map(|n| if e.attribution == Attribution::Inferred { format!("{n} (inferido)") } else { n.to_string() }).unwrap_or("—".into());
        let entry = groups.entry((e.code(), e.kind.description().to_string(), disk)).or_insert((0, e.time));
        entry.0 += 1;
        entry.1 = entry.1.max(e.time);
    }
    let mut t = table();
    t.set_header(vec!["Origem/ID", "Significado", "Disco", "Qtde", "Último"]);
    let mut rows: Vec<_> = groups.into_iter().collect();
    rows.sort_by_key(|r| std::cmp::Reverse(r.1.0));
    for ((code, desc, disk), (count, last)) in rows {
        t.add_row(vec![
            Cell::new(code),
            Cell::new(desc),
            Cell::new(disk),
            Cell::new(int(count)),
            Cell::new(last.with_timezone(&Local).format("%d/%m %H:%M").to_string()),
        ]);
    }
    println!("{t}");

    println!("\n{}", style(format!("Mais recentes (até {limit}):")).bold());
    let mut t = table();
    t.set_header(vec!["Data/hora", "Origem/ID", "Nível", "Disco", "Mensagem"]);
    for e in events.iter().take(limit) {
        let level = match e.level {
            1 | 2 => Cell::new(e.level_label()).fg(Color::Red),
            3 => Cell::new(e.level_label()).fg(Color::Yellow),
            _ => Cell::new(e.level_label()),
        };
        t.add_row(vec![
            Cell::new(e.time.with_timezone(&Local).format("%d/%m/%Y %H:%M:%S").to_string()),
            Cell::new(e.code()),
            level,
            Cell::new(e.disk_number.map(|n| n.to_string()).unwrap_or("—".into())),
            Cell::new(truncate_chars(&e.message, 140)),
        ]);
    }
    println!("{t}");
}

pub fn print_scan_summary(r: &ScanResult) {
    let m = &r.metrics;
    heading(&format!("Resultado do teste de leitura — {}", m.status.label()));
    let mut t = table();
    let rows: Vec<(&str, String)> = vec![
        ("Disco", r.disk.title()),
        ("Lido", format!("{} de {}", format_bytes(m.bytes_processed), format_bytes(m.bytes_planned))),
        ("Duração", format_duration(Duration::from_secs_f64(m.wall_seconds))),
        ("Velocidade média", format_mbps(m.avg_mb_per_s)),
        (
            "Velocidade por região",
            format!(
                "mín {} · mediana {} · máx {}",
                m.min_segment_mb_per_s.map(format_mbps).unwrap_or("—".into()),
                m.median_segment_mb_per_s.map(format_mbps).unwrap_or("—".into()),
                m.max_segment_mb_per_s.map(format_mbps).unwrap_or("—".into())
            ),
        ),
        (
            "Latência",
            format!(
                "média {} · p99 {} · máx {}",
                format_ms(m.latency.avg_ms),
                format_ms(m.latency.p99_ms),
                format_ms(m.latency.max_ms)
            ),
        ),
        ("Leituras lentas", format!("{} (≥ {} ms)", int(m.slow_blocks), int(r.config.slow_threshold_ms))),
        (
            "Erros de leitura",
            format!("{} ({} de mídia, {} de dispositivo, {} transitórios)", int(m.error_blocks), int(m.media_errors), int(m.device_errors), int(m.transient_errors)),
        ),
        ("Timeouts", int(m.timeout_blocks)),
        ("Dados ilegíveis", format_bytes(m.bytes_unreadable)),
    ];
    for (k, v) in rows {
        t.add_row(vec![Cell::new(k), Cell::new(v)]);
    }
    println!("{t}");
    let buckets = &m.latency.buckets;
    if m.latency.count > 0 {
        let max = buckets.iter().map(|b| b.count).max().unwrap_or(1).max(1);
        println!("Distribuição de latência (blocos de {}):", format_bytes_binary(m.block_size as u64));
        for b in buckets {
            let width = ((b.count as f64 / max as f64) * 40.0).ceil() as usize;
            println!("  {:>9} {:>12} {}", b.label, int(b.count), "█".repeat(if b.count > 0 { width.max(1) } else { 0 }));
        }
    }
    if !m.bad_ranges.is_empty() {
        println!("{}", style("Regiões ilegíveis:").red().bold());
        for b in m.bad_ranges.iter().take(20) {
            println!("  LBA {} · {} setores{}", int(b.first_lba), int(b.sector_count), if b.refined { "" } else { " (não isolada)" });
        }
        if m.bad_ranges.len() > 20 {
            println!("  ... e mais {} (veja o JSON/relatório)", m.bad_ranges.len() - 20);
        }
    }
}

fn bench_rows(t: &mut Table, m: &BenchMetrics) {
    for s in &m.sequential {
        t.add_row(vec![
            Cell::new(&s.label),
            Cell::new(format_mbps(s.mb_per_s)),
            Cell::new("—"),
            Cell::new(format_ms(s.latency.avg_ms)),
            Cell::new(format_ms(s.latency.p99_ms)),
            Cell::new(format_ms(s.latency.max_ms)),
            Cell::new(int(s.errors + s.timeouts)),
        ]);
    }
    if let Some(r) = &m.random {
        t.add_row(vec![
            Cell::new(&r.label),
            Cell::new(format_mbps(r.mb_per_s)),
            Cell::new(int(r.iops.round() as u64)),
            Cell::new(format_ms(r.latency.avg_ms)),
            Cell::new(format_ms(r.latency.p99_ms)),
            Cell::new(format_ms(r.latency.max_ms)),
            Cell::new(int(r.errors + r.timeouts)),
        ]);
    }
}

pub fn print_benchmark(r: &BenchmarkResult) {
    heading(&format!("Benchmark — {} (fila QD1)", r.disk.title()));
    let mut t = table();
    t.set_header(vec!["Teste", "Velocidade", "IOPS", "Lat. média", "p99", "Máx", "Erros"]);
    if let Some(m) = &r.read {
        bench_rows(&mut t, m);
    }
    if let Some(w) = &r.write {
        bench_rows(&mut t, &w.metrics);
    }
    println!("{t}");
    note("QD1 = uma operação por vez: expõe latência e travamentos; números de fabricante usam filas profundas e são maiores.");
}

/// Pergunta sim/não (padrão: não). Exige terminal interativo.
pub fn confirm(prompt: &str) -> Result<bool> {
    if !std::io::stdin().is_terminal() {
        bail!("confirmação interativa necessária (use --yes para confirmar em scripts)");
    }
    print!("{} {} ", style(prompt).bold(), style("[s/N]").dim());
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(matches!(line.trim().to_lowercase().as_str(), "s" | "sim" | "y" | "yes"))
}

/// Confirmação forte: o usuário precisa digitar `word` exatamente.
pub fn confirm_typed(prompt: &str, word: &str) -> Result<bool> {
    if !std::io::stdin().is_terminal() {
        bail!("esta operação exige confirmação digitada em um terminal interativo");
    }
    print!("{} ", style(format!("{prompt} Digite {word} para continuar:")).bold());
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim() == word)
}

/// Barra de progresso em bytes com ETA e velocidade em pt-BR.
pub fn bytes_progress(total: u64) -> ProgressBar {
    let style = ProgressStyle::with_template(
        "{spinner:.cyan} {prefix}{percent:>3}% [{bar:36.cyan/blue}] {decimal_bytes}/{decimal_total_bytes} · {decimal_bytes_per_sec} · restante {eta_pt} {msg}",
    )
    .expect("template")
    .with_key("eta_pt", |state: &ProgressState, w: &mut dyn std::fmt::Write| {
        let _ = write!(w, "{}", format_duration(state.eta()));
    })
    .progress_chars("█▓░");
    let bar = ProgressBar::new(total).with_style(style);
    bar.enable_steady_tick(Duration::from_millis(200));
    bar
}

/// Barra de progresso em operações (benchmark aleatório).
pub fn ops_progress(total: u64, label: &str) -> ProgressBar {
    let style = ProgressStyle::with_template("{spinner:.cyan} {msg} [{bar:30.cyan/blue}] {pos}/{len} ops · {per_sec}").expect("template").progress_chars("█▓░");
    let bar = ProgressBar::new(total).with_style(style);
    bar.set_message(label.to_string());
    bar.enable_steady_tick(Duration::from_millis(200));
    bar
}

pub fn spinner(msg: &str) -> ProgressBar {
    let bar = ProgressBar::new_spinner().with_style(ProgressStyle::with_template("{spinner:.cyan} {msg}").expect("template"));
    bar.set_message(msg.to_string());
    bar.enable_steady_tick(Duration::from_millis(120));
    bar
}

/// Estimativa grosseira de duração para leitura completa por tipo de disco.
pub fn estimate_scan_time(d: &DiskInfo, bytes: u64) -> Duration {
    use crate::inventory::{BusKind, DeviceKind};
    let mbps = match (d.kind, d.bus, d.is_ssd(), d.is_hdd()) {
        (DeviceKind::FlashDrive | DeviceKind::MemoryCard, ..) => 30.0,
        (DeviceKind::ExternalDrive, _, true, _) => 350.0,
        (DeviceKind::ExternalDrive, ..) => 110.0,
        (_, BusKind::Nvme, _, _) => 1500.0,
        (_, _, true, _) => 450.0,
        (_, _, _, true) => 150.0,
        _ => 200.0,
    };
    Duration::from_secs_f64(bytes as f64 / (mbps * 1e6))
}

pub fn percent(part: u64, total: u64) -> String {
    if total == 0 { "0".into() } else { decimal(part as f64 * 100.0 / total as f64, 1) }
}
