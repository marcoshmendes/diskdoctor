//! Renderização HTML autocontida do relatório. Todo texto dinâmico passa por
//! [`e`] (escape HTML) — mensagens de eventos e nomes de modelo vêm do sistema
//! e não são confiáveis.

use super::DiagnosticReport;
use crate::analysis::{DiskStatus, Finding, Severity};
use crate::benchmark::BenchMetrics;
use crate::inventory::DiskInfo;
use crate::scan::{ScanResult, TimelinePoint};
use crate::smart::SmartReport;
use crate::stats::LatencySummary;
use crate::util::{decimal, format_bytes, format_bytes_binary, format_duration, format_mbps, format_ms, html_escape, int, truncate_chars};
use chrono::Local;
use std::fmt::Write;
use std::time::Duration;

fn e(s: &str) -> String {
    html_escape(s)
}

fn opt<T: std::fmt::Display>(v: Option<T>) -> String {
    v.map(|x| x.to_string()).unwrap_or_else(|| "—".into())
}

fn opt_int(v: Option<u64>) -> String {
    v.map(int).unwrap_or_else(|| "—".into())
}

fn status_class(s: DiskStatus) -> &'static str {
    match s {
        DiskStatus::Critical => "critical",
        DiskStatus::Warning => "warning",
        DiskStatus::Attention => "attention",
        DiskStatus::Healthy => "healthy",
        DiskStatus::Unknown => "unknown",
    }
}

fn severity_class(s: Severity) -> &'static str {
    match s {
        Severity::Critical => "critical",
        Severity::Warning => "warning",
        Severity::Attention => "attention",
        Severity::Info => "info",
    }
}

const CSS: &str = r#"
:root{--bg:#f6f7f9;--card:#fff;--text:#1d2330;--muted:#5d6676;--border:#dde1e7;--accent:#2f6fdb;
--critical:#c62828;--critical-bg:#fdecea;--warning:#b45309;--warning-bg:#fff4e0;--attention:#8a6d00;--attention-bg:#fffbe0;
--healthy:#1e7d36;--healthy-bg:#e7f6ec;--unknown:#5d6676;--unknown-bg:#eef0f3;--info:#2f6fdb;--info-bg:#e8f0fd;--chart:#2f6fdb}
@media (prefers-color-scheme:dark){:root{--bg:#12151b;--card:#1b2029;--text:#e6e9ef;--muted:#9aa3b2;--border:#2c3340;--accent:#6ea0ff;
--critical:#ff8a80;--critical-bg:#3a1d1d;--warning:#ffb74d;--warning-bg:#3a2a12;--attention:#e6cf5c;--attention-bg:#353014;
--healthy:#81c995;--healthy-bg:#17301f;--unknown:#9aa3b2;--unknown-bg:#252b35;--info:#6ea0ff;--info-bg:#1a2740;--chart:#6ea0ff}}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--text);font:15px/1.5 "Segoe UI",system-ui,sans-serif}
main{max-width:1100px;margin:0 auto;padding:24px 16px 64px}h1{font-size:26px;margin:0 0 4px}h2{font-size:20px;margin:36px 0 12px;border-bottom:1px solid var(--border);padding-bottom:6px}
h3{font-size:16px;margin:18px 0 8px}.meta{color:var(--muted);margin:0}.card{background:var(--card);border:1px solid var(--border);border-radius:10px;padding:16px;margin:12px 0}
.grid{display:grid;grid-template-columns:repeat(auto-fit,minmax(300px,1fr));gap:12px}.badge{display:inline-block;padding:2px 10px;border-radius:999px;font-size:12px;font-weight:600;white-space:nowrap}
.b-critical{color:var(--critical);background:var(--critical-bg)}.b-warning{color:var(--warning);background:var(--warning-bg)}.b-attention{color:var(--attention);background:var(--attention-bg)}
.b-healthy{color:var(--healthy);background:var(--healthy-bg)}.b-unknown{color:var(--unknown);background:var(--unknown-bg)}.b-info{color:var(--info);background:var(--info-bg)}
.disk-card{border-left:6px solid var(--unknown)}.disk-card.critical{border-left-color:var(--critical)}.disk-card.warning{border-left-color:var(--warning)}
.disk-card.attention{border-left-color:var(--attention)}.disk-card.healthy{border-left-color:var(--healthy)}
.finding{border-left:4px solid var(--border);padding:8px 12px;margin:10px 0;background:var(--bg);border-radius:6px}
.finding.critical{border-left-color:var(--critical)}.finding.warning{border-left-color:var(--warning)}.finding.attention{border-left-color:var(--attention)}.finding.info{border-left-color:var(--info)}
.finding h4{margin:0 0 6px;font-size:15px}.src{font-size:11px;color:var(--muted);border:1px solid var(--border);border-radius:4px;padding:0 5px;margin-right:6px;white-space:nowrap}
table{width:100%;border-collapse:collapse;font-size:13px}th,td{text-align:left;padding:6px 8px;border-bottom:1px solid var(--border);vertical-align:top}
th{color:var(--muted);font-weight:600}td.num{text-align:right;font-variant-numeric:tabular-nums}.muted{color:var(--muted)}ul{margin:4px 0;padding-left:20px}
details summary{cursor:pointer;color:var(--accent);margin:8px 0}.table-wrap{overflow-x:auto}.bar{height:10px;background:var(--chart);border-radius:3px;min-width:1px}
svg text{fill:var(--muted);font-size:11px}svg .grid-line{stroke:var(--border)}svg .line{fill:none;stroke:var(--chart);stroke-width:1.5}svg .err{stroke:var(--critical);stroke-width:1.5}svg .slow{fill:var(--warning)}
.kv{display:grid;grid-template-columns:max-content 1fr;gap:2px 16px;font-size:14px}.kv dt{color:var(--muted)}.kv dd{margin:0}footer{margin-top:48px;color:var(--muted);font-size:12px}
@media print{body{background:#fff}.card{break-inside:avoid}}
"#;

/// Renderiza o relatório completo.
pub fn render(r: &DiagnosticReport) -> String {
    let mut h = String::with_capacity(96 * 1024);
    let generated = r.generated_at.with_timezone(&Local).format("%d/%m/%Y %H:%M");
    let _ = write!(
        h,
        "<!doctype html><html lang=\"pt-BR\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">\
         <title>Relatório diskdoctor — {}</title><style>{CSS}</style></head><body><main>",
        e(&generated.to_string())
    );
    let _ = write!(
        h,
        "<h1>Relatório de diagnóstico de discos</h1><p class=\"meta\">{} · gerado em {} · diskdoctor {} · Administrador: {} · {} · eventos das últimas {} h</p>",
        e(r.computer.as_deref().unwrap_or("computador")),
        e(&generated.to_string()),
        e(&r.version),
        if r.elevated { "sim" } else { "não" },
        e(r.smartctl.as_deref().unwrap_or("smartctl não encontrado")),
        r.event_window_hours
    );

    // Resumo
    h.push_str("<h2>Resumo</h2><div class=\"grid\">");
    for v in &r.analysis.verdicts {
        let disk = r.disks.iter().find(|d| d.number == v.disk_number);
        let cls = status_class(v.status);
        let _ = write!(
            h,
            "<div class=\"card disk-card {cls}\"><span class=\"badge b-{cls}\">{}</span><h3>{}</h3><p class=\"muted\">{}</p><p>{}</p></div>",
            e(v.status.label()),
            e(&v.label),
            e(&disk.map(disk_line).unwrap_or_default()),
            e(&v.headline)
        );
    }
    h.push_str("</div>");
    if !r.warnings.is_empty() {
        h.push_str("<div class=\"card\"><strong>Avisos da coleta</strong><ul>");
        for w in &r.warnings {
            let _ = write!(h, "<li>{}</li>", e(w));
        }
        h.push_str("</ul></div>");
    }

    // Diagnóstico
    h.push_str("<h2>Diagnóstico e correlação</h2>");
    for v in &r.analysis.verdicts {
        let cls = status_class(v.status);
        let _ = write!(h, "<div class=\"card disk-card {cls}\"><h3>{} <span class=\"badge b-{cls}\">{}</span></h3>", e(&v.label), e(v.status.label()));
        if v.findings.is_empty() {
            let _ = write!(h, "<p>{}</p>", e(&v.headline));
        }
        for f in &v.findings {
            finding_html(&mut h, f);
        }
        let _ = write!(h, "<p class=\"muted\"><strong>Fontes analisadas:</strong> {}</p>", e(&v.sources_used.join(" · ")));
        if !v.gaps.is_empty() {
            h.push_str("<p class=\"muted\"><strong>Não verificado:</strong></p><ul class=\"muted\">");
            for g in &v.gaps {
                let _ = write!(h, "<li>{}</li>", e(g));
            }
            h.push_str("</ul>");
        }
        h.push_str("</div>");
    }
    if !r.analysis.global_findings.is_empty() {
        h.push_str("<div class=\"card\"><h3>Achados gerais (sem disco específico)</h3>");
        for f in &r.analysis.global_findings {
            finding_html(&mut h, f);
        }
        h.push_str("</div>");
    }
    h.push_str("<div class=\"card\"><strong>Notas importantes</strong><ul>");
    for n in &r.analysis.notes {
        let _ = write!(h, "<li>{}</li>", e(n));
    }
    h.push_str("</ul></div>");

    // Discos
    h.push_str("<h2>Discos</h2><div class=\"card table-wrap\"><table><tr><th>#</th><th>Tags</th><th>Modelo</th><th>Serial</th><th>Capacidade</th><th>Tipo</th><th>Firmware</th><th>Volumes</th><th>Windows</th></tr>");
    for d in &r.disks {
        let _ = write!(
            h,
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            d.number,
            if d.tags.is_empty() { "—".to_string() } else { e(&d.tags.join(", ")) },
            e(&d.model),
            e(&d.serial_masked),
            e(&format_bytes(d.size_bytes)),
            e(&d.class_label()),
            e(d.firmware.as_deref().unwrap_or("—")),
            e(&volumes_text(d)),
            e(&d.windows_health)
        );
    }
    h.push_str("</table></div>");

    // SMART
    h.push_str("<h2>SMART e contadores</h2>");
    for d in &r.disks {
        let health = r.health.iter().find(|x| x.disk_number == d.number);
        let _ = write!(h, "<div class=\"card\"><h3>{}</h3>", e(&d.title()));
        match health.and_then(|x| x.smart.as_ref()) {
            Some(s) if s.available => smart_html(&mut h, s),
            Some(s) => {
                let _ = write!(h, "<p class=\"muted\">SMART indisponível: {}</p>", e(s.error.as_deref().unwrap_or("sem dados")));
            }
            None => h.push_str("<p class=\"muted\">SMART não consultado.</p>"),
        }
        if let Some(w) = health.and_then(|x| x.windows.as_ref()) {
            let _ = write!(
                h,
                "<p class=\"muted\">Windows: temperatura {} °C · desgaste {}% · erros de leitura não corrigidos {} · latência máx. leitura {} ms / gravação {} ms</p>",
                opt(w.temperature_c),
                opt(w.wear_percent),
                opt_int(w.read_errors_uncorrected),
                opt_int(w.read_latency_max_ms),
                opt_int(w.write_latency_max_ms)
            );
        }
        if let Some(delta) = health.and_then(|x| x.history.as_ref()) {
            let since = delta.baseline_time.with_timezone(&Local).format("%d/%m/%Y");
            if delta.changes.is_empty() {
                let _ = write!(h, "<p class=\"muted\">Histórico: nenhum contador de problema aumentou desde {since}.</p>");
            } else {
                let _ = write!(h, "<p><strong>Contadores que aumentaram desde {since}:</strong></p><ul>");
                for c in &delta.changes {
                    let _ = write!(h, "<li>{}: {} → {}</li>", e(&c.label), int(c.before), int(c.after));
                }
                h.push_str("</ul>");
            }
        }
        h.push_str("</div>");
    }

    // Testes de leitura
    h.push_str("<h2>Testes de leitura</h2>");
    if r.scans.is_empty() {
        h.push_str("<p class=\"muted\">Nenhum teste de leitura incluído. Execute <code>diskdoctor scan --disk N</code> (como Administrador) e gere o relatório novamente.</p>");
    }
    for s in &r.scans {
        scan_html(&mut h, s);
    }

    // Benchmarks
    if !r.benchmarks.is_empty() {
        h.push_str("<h2>Benchmarks</h2>");
        for b in &r.benchmarks {
            let _ = write!(h, "<div class=\"card\"><h3>{}</h3>", e(&b.disk.title()));
            if let Some(m) = &b.read {
                bench_table(&mut h, "Leitura (disco físico)", m);
            }
            if let Some(w) = &b.write {
                bench_table(&mut h, &format!("Escrita (arquivo temporário em {}, {})", w.target_dir, format_bytes(w.file_size)), &w.metrics);
            }
            h.push_str("</div>");
        }
    }

    // Verificação de capacidade
    if !r.capacity.is_empty() {
        h.push_str("<h2>Verificação de capacidade</h2>");
        for c in &r.capacity {
            capacity_html(&mut h, c);
        }
    }

    // Eventos
    let _ = write!(h, "<h2>Eventos do Windows ({})</h2>", r.events.len());
    if r.events.is_empty() {
        h.push_str("<p class=\"muted\">Nenhum evento de armazenamento no período (ou o log não foi consultado).</p>");
    } else {
        h.push_str("<div class=\"card table-wrap\"><table><tr><th>Data/hora</th><th>Origem/ID</th><th>Nível</th><th>Disco</th><th>Tipo</th><th>Mensagem</th></tr>");
        for ev in r.events.iter().take(500) {
            let disk = match (ev.disk_number, ev.attribution) {
                (Some(n), crate::events::Attribution::Inferred) => format!("{n} (inferido)"),
                (Some(n), _) => n.to_string(),
                (None, _) => "—".into(),
            };
            let _ = write!(
                h,
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                ev.time.with_timezone(&Local).format("%d/%m/%Y %H:%M:%S"),
                e(&ev.code()),
                e(ev.level_label()),
                e(&disk),
                e(ev.kind.description()),
                e(&truncate_chars(&ev.message, 300))
            );
        }
        h.push_str("</table>");
        if r.events.len() > 500 {
            let _ = write!(h, "<p class=\"muted\">Mostrando 500 de {} eventos; todos estão no relatório JSON.</p>", r.events.len());
        }
        h.push_str("</div>");
    }

    // Limitações
    h.push_str("<h2>Limitações e avisos</h2><div class=\"card\"><ul>");
    for l in &r.limitations {
        let _ = write!(h, "<li>{}</li>", e(l));
    }
    h.push_str("</ul></div>");
    let _ = write!(
        h,
        "<footer>Gerado por diskdoctor {} — diagnóstico não destrutivo. Nenhum dado foi gravado nos discos analisados. Seriais exibidos parcialmente mascarados.</footer></main></body></html>",
        e(&r.version)
    );
    h
}

fn disk_line(d: &DiskInfo) -> String {
    let mut s = format!("{} · {}", format_bytes(d.size_bytes), d.class_label());
    let letters = d.volume_letters();
    if !letters.is_empty() {
        s.push_str(&format!(" · {}", letters.join(" ")));
    }
    if d.is_boot || d.is_system {
        s.push_str(" · disco do sistema");
    }
    s
}

fn volumes_text(d: &DiskInfo) -> String {
    let named: Vec<String> = d
        .partitions
        .iter()
        .filter_map(|p| {
            p.drive_letter.as_ref().map(|l| {
                let label = p.label.as_deref().map(|x| format!(" {x}")).unwrap_or_default();
                let free = p.volume_free.map(|f| format!(", {} livres", format_bytes(f))).unwrap_or_default();
                format!("{l}:{label} ({}{free})", p.file_system.as_deref().unwrap_or("?"))
            })
        })
        .collect();
    let hidden = d.partitions.iter().filter(|p| p.drive_letter.is_none()).count();
    let mut s = named.join("; ");
    if hidden > 0 {
        if !s.is_empty() {
            s.push_str("; ");
        }
        s.push_str(&format!("+{hidden} partição(ões) sem letra"));
    }
    if s.is_empty() { "—".into() } else { s }
}

fn finding_html(h: &mut String, f: &Finding) {
    let cls = severity_class(f.severity);
    let _ = write!(
        h,
        "<div class=\"finding {cls}\"><h4><span class=\"badge b-{cls}\">{}</span> {} <span class=\"muted\">· confiança {}</span></h4><ul>",
        e(f.severity.label()),
        e(&f.title),
        e(f.confidence.label())
    );
    for ev in &f.evidence {
        let _ = write!(h, "<li><span class=\"src\">{}</span>{}</li>", e(ev.source.label()), e(&ev.text));
    }
    h.push_str("</ul>");
    if !f.possible_causes.is_empty() {
        let _ = write!(h, "<p><strong>Possíveis causas:</strong> {}</p>", e(&f.possible_causes.join("; ")));
    }
    if !f.recommendations.is_empty() {
        h.push_str("<p><strong>Recomendações:</strong></p><ul>");
        for r in &f.recommendations {
            let _ = write!(h, "<li>{}</li>", e(r));
        }
        h.push_str("</ul>");
    }
    let _ = write!(h, "<p class=\"muted\">Base da confiança: {}</p></div>", e(&f.confidence_basis));
}

fn smart_html(h: &mut String, s: &SmartReport) {
    let passed = match s.overall_passed {
        Some(true) => "aprovado",
        Some(false) => "FALHOU",
        None => "—",
    };
    let rows: Vec<(&str, String)> = vec![
        ("Avaliação geral", passed.into()),
        ("Fonte", s.source.clone()),
        ("Temperatura", s.temperature_c.map(|t| format!("{t} °C")).unwrap_or("—".into())),
        ("Horas ligado", opt_int(s.power_on_hours)),
        ("Ciclos de energia", opt_int(s.power_cycles)),
        ("Desligamentos bruscos", opt_int(s.unsafe_shutdowns)),
        ("Vida útil consumida", s.wear_used_percent.map(|w| format!("{w}%")).unwrap_or("—".into())),
        ("Setores realocados", opt_int(s.reallocated_sectors)),
        ("Setores pendentes", opt_int(s.pending_sectors)),
        ("Incorrigíveis", opt_int(s.offline_uncorrectable.or(s.reported_uncorrectable))),
        ("Erros CRC (interface)", opt_int(s.interface_crc_errors)),
        ("Erros CRC (fabricante)", opt_int(s.vendor_crc_errors)),
        ("Timeouts de comando", opt_int(s.command_timeouts)),
        ("Erros de mídia (NVMe)", opt_int(s.media_errors)),
        ("Entradas no log de erros", opt_int(s.error_log_entries)),
    ];
    h.push_str("<dl class=\"kv\">");
    for (k, v) in rows {
        let _ = write!(h, "<dt>{}</dt><dd>{}</dd>", e(k), e(&v));
    }
    h.push_str("</dl>");
    if !s.exit_flags.is_empty() {
        let _ = write!(h, "<p class=\"muted\">smartctl: {}</p>", e(&s.exit_flags.join("; ")));
    }
    if !s.attributes.is_empty() {
        h.push_str("<details><summary>Todos os atributos SMART</summary><div class=\"table-wrap\"><table><tr><th>ID</th><th>Nome</th><th>Valor</th><th>Pior</th><th>Limite</th><th>Bruto</th><th>Falhou</th></tr>");
        for a in &s.attributes {
            let _ = write!(
                h,
                "<tr><td>{}</td><td>{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td>{}</td><td>{}</td></tr>",
                a.id,
                e(&a.name),
                opt(a.value),
                opt(a.worst),
                opt(a.thresh),
                e(&a.raw_string),
                e(a.when_failed.as_deref().unwrap_or(""))
            );
        }
        h.push_str("</table></div></details>");
    }
}

fn latency_html(h: &mut String, l: &LatencySummary) {
    let _ = write!(
        h,
        "<p class=\"muted\">Latência: mín {} · média {} · p50 {} · p95 {} · p99 {} · máx {}</p>",
        format_ms(l.min_ms),
        format_ms(l.avg_ms),
        format_ms(l.p50_ms),
        format_ms(l.p95_ms),
        format_ms(l.p99_ms),
        format_ms(l.max_ms)
    );
    let max = l.buckets.iter().map(|b| b.count).max().unwrap_or(0).max(1);
    h.push_str("<table><tr><th>Faixa</th><th class=\"num\">Leituras</th><th style=\"width:60%\"></th></tr>");
    for b in &l.buckets {
        let pct = if b.count == 0 { 0.0 } else { (b.count as f64 / max as f64 * 100.0).max(0.5) };
        let _ = write!(
            h,
            "<tr><td>{}</td><td class=\"num\">{}</td><td><div class=\"bar\" style=\"width:{}%\"></div></td></tr>",
            e(&b.label),
            int(b.count),
            decimal(pct, 1).replace(',', ".")
        );
    }
    h.push_str("</table>");
}

fn scan_html(h: &mut String, s: &ScanResult) {
    let m = &s.metrics;
    let _ = write!(
        h,
        "<div class=\"card\"><h3>{} — {}</h3><p class=\"muted\">{} → {} · {}</p>",
        e(&s.disk.title()),
        e(&m.status.label()),
        s.started_at.with_timezone(&Local).format("%d/%m/%Y %H:%M"),
        s.finished_at.with_timezone(&Local).format("%d/%m/%Y %H:%M"),
        e(&format_duration(Duration::from_secs_f64(m.wall_seconds)))
    );
    let rows: Vec<(&str, String)> = vec![
        ("Faixa lida", format!("{} de {} ({} por leitura)", format_bytes(m.bytes_processed), format_bytes(m.bytes_planned), format_bytes_binary(m.block_size as u64))),
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
        ("Leituras lentas (≥ limite)", int(m.slow_blocks)),
        ("Erros de leitura", format!("{} ({} de mídia, {} de dispositivo, {} transitórios)", int(m.error_blocks), int(m.media_errors), int(m.device_errors), int(m.transient_errors))),
        ("Timeouts", int(m.timeout_blocks)),
        ("Dados ilegíveis", format_bytes(m.bytes_unreadable)),
    ];
    h.push_str("<dl class=\"kv\">");
    for (k, v) in rows {
        let _ = write!(h, "<dt>{}</dt><dd>{}</dd>", e(k), e(&v));
    }
    h.push_str("</dl>");
    h.push_str(&timeline_svg(&m.timeline));
    latency_html(h, &m.latency);
    if !m.bad_ranges.is_empty() {
        h.push_str("<h3>Regiões ilegíveis</h3><table><tr><th>LBA inicial</th><th class=\"num\">Setores</th><th>Tamanho</th><th>Isolada</th></tr>");
        for b in m.bad_ranges.iter().take(200) {
            let _ = write!(
                h,
                "<tr><td>{}</td><td class=\"num\">{}</td><td>{}</td><td>{}</td></tr>",
                int(b.first_lba),
                int(b.sector_count),
                format_bytes(b.length),
                if b.refined { "sim" } else { "não (orçamento de releitura esgotado)" }
            );
        }
        h.push_str("</table>");
    }
    let important: Vec<_> = m.issues.iter().filter(|i| i.kind != crate::scan::IssueKind::Slow).take(100).collect();
    if !important.is_empty() {
        h.push_str("<details><summary>Ocorrências de erro/timeout</summary><table><tr><th>LBA</th><th>Tipo</th><th>Latência</th><th>Descrição</th></tr>");
        for i in important {
            let _ = write!(h, "<tr><td>{}</td><td>{:?}</td><td>{}</td><td>{}</td></tr>", int(i.lba), i.kind, format_ms(i.latency_ms), e(&i.description));
        }
        h.push_str("</table></details>");
    }
    h.push_str("</div>");
}

fn capacity_html(h: &mut String, c: &crate::capacity::CapacityResult) {
    let m = &c.metrics;
    let _ = write!(
        h,
        "<div class=\"card\"><h3>{} — {}</h3><p class=\"muted\">{} · volume {} ({}) · {}</p><dl class=\"kv\">",
        e(&c.disk.title()),
        e(m.verdict.label()),
        c.finished_at.with_timezone(&Local).format("%d/%m/%Y %H:%M"),
        e(&c.volume),
        e(c.file_system.as_deref().unwrap_or("?")),
        e(&m.status.label())
    );
    let mut rows: Vec<(&str, String)> = vec![
        ("Gravado", format!("{} de {} planejados · {}", format_bytes(m.written_bytes), format_bytes(m.planned_bytes), format_mbps(m.write_mb_per_s))),
        ("Conferido", format!("{} · {}", format_bytes(m.verified_bytes), format_mbps(m.read_mb_per_s))),
        ("Correto", format_bytes(m.ok_bytes)),
        (
            "Problemas",
            format!(
                "sobrescritos {} · perdidos {} · corrompidos {} · ilegíveis {} · falhas de gravação {}",
                format_bytes(m.overwritten_bytes),
                format_bytes(m.lost_bytes),
                format_bytes(m.corrupted_bytes),
                format_bytes(m.unreadable_bytes),
                int(m.write_errors)
            ),
        ),
        ("Não testado", format!("{} já ocupados antes do teste", format_bytes(c.used_before))),
    ];
    if let Some(real) = c.estimated_real_capacity {
        rows.push(("Capacidade real estimada", format!("{} (anunciada: {})", format_bytes(real), format_bytes(c.disk.size_bytes))));
    }
    for (k, v) in rows {
        let _ = write!(h, "<dt>{}</dt><dd>{}</dd>", e(k), e(&v));
    }
    h.push_str("</dl></div>");
}

/// Gráfico SVG da velocidade de leitura ao longo do disco.
pub fn timeline_svg(points: &[TimelinePoint]) -> String {
    if points.is_empty() {
        return String::new();
    }
    let (w, ht, left, right, top, bottom) = (800.0, 200.0, 64.0, 12.0, 12.0, 28.0);
    let (pw, ph) = (w - left - right, ht - top - bottom);
    let max = points.iter().map(|p| p.mb_per_s).fold(0.0_f64, f64::max).max(1.0) * 1.1;
    let x = |pct: f64| left + pct.clamp(0.0, 100.0) / 100.0 * pw;
    let y = |v: f64| top + (1.0 - (v / max).clamp(0.0, 1.0)) * ph;
    let mut s = format!("<svg viewBox=\"0 0 {w} {ht}\" width=\"100%\" role=\"img\" aria-label=\"Velocidade de leitura ao longo do disco\">");
    for frac in [0.0, 0.5, 1.0] {
        let v = max / 1.1 * frac;
        let _ = write!(
            s,
            "<line class=\"grid-line\" x1=\"{left}\" x2=\"{}\" y1=\"{:.1}\" y2=\"{:.1}\"/><text x=\"{}\" y=\"{:.1}\" text-anchor=\"end\">{} MB/s</text>",
            w - right,
            y(v),
            y(v),
            left - 6.0,
            y(v) + 4.0,
            decimal(v, 0)
        );
    }
    for pct in [0.0, 25.0, 50.0, 75.0, 100.0] {
        let _ = write!(s, "<text x=\"{:.1}\" y=\"{}\" text-anchor=\"middle\">{pct}%</text>", x(pct), ht - 8.0);
    }
    for p in points.iter().filter(|p| p.errors > 0) {
        let _ = write!(s, "<line class=\"err\" x1=\"{0:.1}\" x2=\"{0:.1}\" y1=\"{top}\" y2=\"{1}\"/>", x(p.percent), top + ph);
    }
    let path: Vec<String> = points.iter().filter(|p| p.mb_per_s > 0.0).map(|p| format!("{:.1},{:.1}", x(p.percent), y(p.mb_per_s))).collect();
    let _ = write!(s, "<polyline class=\"line\" points=\"{}\"/>", path.join(" "));
    for p in points.iter().filter(|p| p.slow > 0) {
        let _ = write!(s, "<circle class=\"slow\" cx=\"{:.1}\" cy=\"{:.1}\" r=\"3\"/>", x(p.percent), y(p.mb_per_s));
    }
    s.push_str("</svg><p class=\"muted\">Linha: velocidade por região do disco (0% = início). Traços vermelhos: erros/timeouts. Pontos laranja: leituras lentas.</p>");
    s
}

fn bench_table(h: &mut String, title: &str, m: &BenchMetrics) {
    let _ = write!(h, "<h4>{} — {}</h4><table><tr><th>Teste</th><th class=\"num\">Velocidade</th><th class=\"num\">IOPS</th><th class=\"num\">Lat. média</th><th class=\"num\">p99</th><th class=\"num\">Máx</th><th class=\"num\">Erros</th></tr>", e(title), e(&m.status.label()));
    for s in &m.sequential {
        let _ = write!(
            h,
            "<tr><td>{}</td><td class=\"num\">{}</td><td class=\"num\">—</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td></tr>",
            e(&s.label),
            format_mbps(s.mb_per_s),
            format_ms(s.latency.avg_ms),
            format_ms(s.latency.p99_ms),
            format_ms(s.latency.max_ms),
            int(s.errors + s.timeouts)
        );
    }
    if let Some(r) = &m.random {
        let _ = write!(
            h,
            "<tr><td>{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td><td class=\"num\">{}</td></tr>",
            e(&r.label),
            format_mbps(r.mb_per_s),
            int(r.iops.round() as u64),
            format_ms(r.latency.avg_ms),
            format_ms(r.latency.p99_ms),
            format_ms(r.latency.max_ms),
            int(r.errors + r.timeouts)
        );
    }
    h.push_str("</table>");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::{AnalysisResult, Confidence, DiskVerdict, Evidence, EvidenceSource};
    use crate::report::standard_limitations;
    use chrono::Utc;

    fn sample_report() -> DiagnosticReport {
        let disks = crate::inventory::tests::fixture_disks();
        let finding = Finding {
            rule: "comm.link".into(),
            disk_number: Some(2),
            severity: Severity::Warning,
            confidence: Confidence::Medium,
            confidence_basis: "x".into(),
            title: "Falhas <de> comunicação".into(),
            evidence: vec![Evidence { source: EvidenceSource::Events, weight: 2, text: "<script>alert('x')</script>".into() }],
            possible_causes: vec!["cabo".into()],
            recommendations: vec!["troque o cabo".into()],
        };
        DiagnosticReport {
            tool: "diskdoctor".into(),
            version: "0.1.0".into(),
            generated_at: Utc::now(),
            computer: Some("PC<1>".into()),
            elevated: false,
            smartctl: Some("smartctl 7.5".into()),
            event_window_hours: 168,
            health: vec![],
            events: vec![],
            scans: vec![],
            benchmarks: vec![],
            capacity: vec![],
            analysis: AnalysisResult {
                verdicts: vec![DiskVerdict {
                    disk_number: 2,
                    label: "Disco 2".into(),
                    status: DiskStatus::Warning,
                    headline: "h".into(),
                    findings: vec![finding],
                    sources_used: vec!["SMART".into()],
                    gaps: vec!["sem scan".into()],
                }],
                global_findings: vec![],
                notes: vec!["SMART normal não garante".into()],
            },
            disks,
            limitations: standard_limitations(),
            warnings: vec![],
        }
    }

    #[test]
    fn renders_and_escapes_everything() {
        let html = render(&sample_report());
        assert!(html.starts_with("<!doctype html>"));
        assert!(html.contains("lang=\"pt-BR\""));
        assert!(!html.contains("<script>"), "texto do sistema deve ser escapado");
        assert!(html.contains("&lt;script&gt;"));
        assert!(html.contains("Falhas &lt;de&gt; comunicação"));
        assert!(html.contains("PC&lt;1&gt;"));
        assert!(html.contains("KINGSTON SA400S37120G"));
        assert!(html.contains("Limitações e avisos"));
        assert!(!html.contains("50026B7600000002"), "serial completo nunca aparece");
        assert!(!html.contains("<link") && !html.contains("src=\"http"), "sem recursos externos");
    }

    #[test]
    fn timeline_svg_marks_errors_and_slow_points() {
        let pts = vec![
            TimelinePoint { offset: 0, percent: 0.0, mb_per_s: 500.0, max_latency_ms: 2.0, errors: 0, slow: 0 },
            TimelinePoint { offset: 1, percent: 50.0, mb_per_s: 100.0, max_latency_ms: 900.0, errors: 1, slow: 1 },
            TimelinePoint { offset: 2, percent: 100.0, mb_per_s: 480.0, max_latency_ms: 2.0, errors: 0, slow: 0 },
        ];
        let svg = timeline_svg(&pts);
        assert!(svg.contains("<polyline"));
        assert_eq!(svg.matches("class=\"err\"").count(), 1);
        assert_eq!(svg.matches("class=\"slow\"").count(), 1);
        assert!(timeline_svg(&[]).is_empty());
    }
}
