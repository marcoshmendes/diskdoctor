//! `diskdoctor history` — linha do tempo de cada disco e comparação antes × depois das
//! mudanças registradas (`history note`).

use super::{Ctx, load_disks, print_json, select_disk};
use crate::app::exit;
use crate::cli::{HistoryAction, HistoryArgs, HistoryNoteArgs};
use crate::events::{EventKind, StorageEvent, collect_events};
use crate::history::HistoryStore;
use crate::inventory::DiskInfo;
use crate::journal::{Comparison, Effect, Journal, JournalEntry, MAX_COMPARE_DAYS, NoteRecord, compare};
use crate::store::ResultStore;
use crate::tags::quote_for_cli;
use crate::ui;
use crate::util::{decimal, format_bytes, format_duration, format_mbps, int, truncate_chars};
use anyhow::{Result, bail};
use chrono::{DateTime, Duration as ChronoDuration, Local, Utc};
use comfy_table::Cell;
use console::style;
use serde::Serialize;
use std::time::Duration;

/// Quantas mudanças (as mais recentes) comparar por disco.
const MAX_COMPARISONS: usize = 3;
/// Limite da consulta de eventos para as comparações.
const MAX_EVENT_HOURS: i64 = 24 * 60;

pub fn run(ctx: &Ctx, args: &HistoryArgs) -> Result<u8> {
    match &args.action {
        Some(HistoryAction::Note(a)) => note(ctx, a),
        None => show(ctx, args),
    }
}

/// Como referir-se ao disco em exemplos de comando: a primeira tag, ou o número.
fn disk_arg(d: &DiskInfo) -> String {
    d.tags.first().map(|t| quote_for_cli(t)).unwrap_or_else(|| d.number.to_string())
}

fn note(ctx: &Ctx, a: &HistoryNoteArgs) -> Result<u8> {
    let text = truncate_chars(a.text.join(" ").trim(), 200);
    if text.is_empty() {
        bail!("descreva a mudança (ex.: \"Troquei o cabo SATA\")");
    }
    let disks = load_disks(ctx)?;
    let disk = select_disk(&disks, &a.disk)?.clone();
    let journal = Journal::new(&ctx.data_dir);
    let had_diagnosis = !journal.diagnoses(&disk.fingerprint).is_empty();
    let rec = NoteRecord { time: Utc::now(), disk_number: disk.number, text };
    journal.append(&disk.fingerprint, &JournalEntry::Note(rec.clone()))?;

    if ctx.json {
        print_json(&rec)?;
        return Ok(exit::OK);
    }
    println!("{} Mudança registrada — {}", ui::ok_mark(), disk.title());
    println!("  \"{}\" em {}", rec.text, rec.time.with_timezone(&Local).format("%d/%m/%Y %H:%M"));
    println!();
    println!("  A partir de agora o veredito deste disco considera só o que acontecer depois desta mudança.");
    println!("  Próximos passos:");
    println!("    1. use o computador normalmente por 1–3 dias;");
    println!("    2. rode `diskdoctor health`;");
    println!("    3. compare com `diskdoctor history --disk {}`.", disk_arg(&disk));
    if !had_diagnosis {
        ui::note("\nAinda não há diagnóstico salvo deste disco antes da mudança; a comparação vai usar os eventos do Windows, que guardam o histórico recente.");
    }
    Ok(exit::OK)
}

/// Linha da linha do tempo.
#[derive(Serialize)]
struct Row {
    time: DateTime<Utc>,
    kind: &'static str,
    detail: String,
}

#[derive(Serialize)]
struct DiskHistory {
    number: u32,
    title: String,
    fingerprint: String,
    timeline: Vec<Row>,
    comparisons: Vec<Comparison>,
}

fn timeline(d: &DiskInfo, journal: &Journal, store: &ResultStore, colored: bool) -> Vec<Row> {
    let status = |s: crate::analysis::DiskStatus| if colored { ui::status_text(s) } else { s.label().to_string() };
    let mut rows: Vec<Row> = journal
        .load(&d.fingerprint)
        .into_iter()
        .map(|e| match e {
            JournalEntry::Diagnosis(x) => Row {
                time: x.time,
                kind: "Diagnóstico",
                detail: format!(
                    "{} — {}{}",
                    status(x.status),
                    truncate_chars(&x.headline, 90),
                    if x.io_events > 0 { format!(" · {} eventos de E/S", int(x.io_events)) } else { String::new() }
                ),
            },
            JournalEntry::Note(n) => Row {
                time: n.time,
                kind: "Mudança",
                detail: if colored { style(format!("\"{}\"", n.text)).magenta().bold().to_string() } else { format!("\"{}\"", n.text) },
            },
        })
        .collect();
    for s in store.scans_for(&d.fingerprint) {
        let m = &s.metrics;
        rows.push(Row {
            time: s.finished_at,
            kind: "Teste de leitura",
            detail: format!(
                "{} · {} · erros {} · timeouts {} · lentas {}",
                m.status.label(),
                format_mbps(m.avg_mb_per_s),
                int(m.error_blocks),
                int(m.timeout_blocks),
                int(m.slow_blocks)
            ),
        });
    }
    for c in store.capacity_for(&d.fingerprint) {
        rows.push(Row {
            time: c.finished_at,
            kind: "Capacidade",
            detail: format!("{} · {} conferidos", c.metrics.verdict.label(), format_bytes(c.metrics.verified_bytes)),
        });
    }
    rows.sort_by_key(|r| r.time);
    rows
}

fn show(ctx: &Ctx, args: &HistoryArgs) -> Result<u8> {
    let disks = load_disks(ctx)?;
    let selected: Vec<&DiskInfo> = match &args.disk {
        Some(r) => vec![select_disk(&disks, r)?],
        None => disks.iter().collect(),
    };
    let journal = Journal::new(&ctx.data_dir);
    let store = ResultStore::new(&ctx.data_dir);
    let smart_history = HistoryStore::new(&ctx.data_dir);
    let now = Utc::now();

    // Eventos do Windows só são necessários para comparar mudanças.
    let notes: Vec<(u32, Vec<NoteRecord>)> = selected
        .iter()
        .map(|d| {
            let mut n = journal.notes(&d.fingerprint);
            n.sort_by_key(|x| std::cmp::Reverse(x.time));
            n.truncate(MAX_COMPARISONS);
            (d.number, n)
        })
        .collect();
    let oldest = notes.iter().flat_map(|(_, n)| n.iter().map(|x| x.time)).min();
    let mut events: Vec<StorageEvent> = Vec::new();
    let mut events_ok = false;
    if let Some(t) = oldest {
        let since_note = now - t;
        let window = since_note.min(ChronoDuration::days(MAX_COMPARE_DAYS));
        let hours = ((since_note + window).num_hours() + 1).clamp(1, MAX_EVENT_HOURS);
        let spinner = (!ctx.json).then(|| ui::spinner(&format!("Consultando eventos do Windows (últimos {} dias) para comparar…", hours / 24 + 1)));
        match collect_events(hours as u32, 20_000, &disks) {
            Ok(ev) => {
                events = ev;
                events_ok = true;
            }
            Err(e) => ui::warn(&format!("eventos do Windows indisponíveis ({e:#}); a comparação usa só diagnósticos e SMART salvos")),
        }
        if let Some(s) = spinner {
            s.finish_and_clear();
        }
    }
    let reboots: Vec<DateTime<Utc>> = events.iter().filter(|e| e.kind == EventKind::UnexpectedReboot).map(|e| e.time).collect();

    let mut out = Vec::new();
    for d in &selected {
        let io: Vec<DateTime<Utc>> =
            events.iter().filter(|e| e.disk_number == Some(d.number) && e.category.is_storage_io()).map(|e| e.time).collect();
        let diagnoses = journal.diagnoses(&d.fingerprint);
        let snapshots = smart_history.load(&d.fingerprint);
        let comparisons: Vec<Comparison> = notes
            .iter()
            .find(|(n, _)| *n == d.number)
            .map(|(_, ns)| ns.iter().map(|n| compare(n, &io, &reboots, &diagnoses, &snapshots, now)).collect())
            .unwrap_or_default();
        let mut rows = timeline(d, &journal, &store, !ctx.json);
        let skip = rows.len().saturating_sub(args.limit.max(1));
        rows.drain(..skip);
        out.push(DiskHistory { number: d.number, title: d.title(), fingerprint: d.fingerprint.clone(), timeline: rows, comparisons });
    }

    if ctx.json {
        print_json(&serde_json::json!({ "events_collected": events_ok, "disks": out }))?;
        return Ok(exit::OK);
    }
    for h in &out {
        ui::heading(&format!("Histórico — {}", h.title));
        if h.timeline.is_empty() {
            println!("  Nada registrado ainda. Rode `diskdoctor health` para começar o histórico deste disco.");
        } else {
            let mut t = ui::table();
            t.set_header(vec!["Data", "Evento", "Resultado"]);
            for r in &h.timeline {
                let kind = if r.kind == "Mudança" { Cell::new(style(r.kind).magenta().bold().to_string()) } else { Cell::new(r.kind) };
                t.add_row(vec![Cell::new(r.time.with_timezone(&Local).format("%d/%m/%Y %H:%M").to_string()), kind, Cell::new(&r.detail)]);
            }
            println!("{t}");
        }
        for c in &h.comparisons {
            print_comparison(c, events_ok);
        }
        if h.comparisons.is_empty() {
            let arg = selected.iter().find(|d| d.number == h.number).map(|d| disk_arg(d)).unwrap_or_default();
            ui::note(&format!("Trocou cabo, porta ou configuração? Registre com: diskdoctor history note --disk {arg} \"Troquei o cabo SATA\""));
        }
    }
    Ok(exit::OK)
}

fn per_day(n: u64, days: f64) -> String {
    if days <= 0.0 { int(n) } else { format!("{} ({}/dia)", int(n), decimal(n as f64 / days, 1)) }
}

fn print_comparison(c: &Comparison, events_ok: bool) {
    let ago = format_duration(Duration::from_secs_f64(c.hours_since * 3600.0));
    println!(
        "\n  {} {} — \"{}\" (há {ago})",
        style("Antes × depois da mudança de").bold(),
        c.note.time.with_timezone(&Local).format("%d/%m/%Y %H:%M"),
        c.note.text
    );
    let range = |p: &crate::journal::PeriodStats| {
        format!("{} → {}", p.from.with_timezone(&Local).format("%d/%m %H:%M"), p.to.with_timezone(&Local).format("%d/%m %H:%M"))
    };
    let growth = |p: &crate::journal::PeriodStats| {
        if p.smart_readings < 2 {
            "sem leituras suficientes".to_string()
        } else if p.counter_growth.is_empty() {
            "nenhum".to_string()
        } else {
            p.counter_growth.iter().map(|(k, g)| format!("{k} +{}", int(*g))).collect::<Vec<_>>().join("; ")
        }
    };
    let status = |p: &crate::journal::PeriodStats| p.status.map(ui::status_text).unwrap_or_else(|| "—".into());
    let mut t = ui::table();
    t.set_header(vec!["", "Antes", "Depois"]);
    t.add_row(vec!["Período".to_string(), range(&c.before), range(&c.after)]);
    t.add_row(vec!["Eventos de E/S do disco".to_string(), per_day(c.before.io_events, c.before.days()), per_day(c.after.io_events, c.after.days())]);
    t.add_row(vec!["Reinicializações inesperadas (PC)".to_string(), int(c.before.unexpected_reboots), int(c.after.unexpected_reboots)]);
    t.add_row(vec!["Último veredito".to_string(), status(&c.before), status(&c.after)]);
    t.add_row(vec!["Contadores SMART que subiram".to_string(), growth(&c.before), growth(&c.after)]);
    println!("{t}");
    let label = match c.effect {
        Effect::Improved => style(c.effect.label()).green().bold(),
        Effect::Worse => style(c.effect.label()).red().bold(),
        Effect::NoChange => style(c.effect.label()).color256(208).bold(),
        Effect::TooEarly | Effect::NotMeasurable => style(c.effect.label()).yellow().bold(),
    };
    println!("  → {label}: {}", c.summary);
    if !events_ok {
        ui::warn("sem eventos do Windows nesta comparação; os números de eventos podem estar zerados por isso.");
    }
}
