//! `diskdoctor report`

use super::{CollectOptions, Ctx, analyze_collected, collect, exit_for_status, load_disks, print_json, record_diagnoses};
use crate::cli::ReportArgs;
use crate::i18n::{self, t};
use crate::report::{DiagnosticReport, standard_limitations, write_report};
use crate::store::load_scan_file;
use crate::ui;
use anyhow::Result;
use chrono::Utc;
use serde_json::json;

pub fn run(ctx: &Ctx, args: &ReportArgs) -> Result<u8> {
    let all = load_disks(ctx)?;
    let extra_scans = args.scan_file.iter().map(|p| load_scan_file(p)).collect::<Result<Vec<_>>>()?;
    let collected = collect(
        ctx,
        all,
        CollectOptions {
            only_disk: None,
            hours: args.hours,
            with_events: true,
            record_history: !args.no_history,
            saved_results: !args.no_saved_results,
            extra_scans,
        },
    )?;
    let analysis = analyze_collected(ctx, &collected, args.hours);
    if !args.no_history {
        record_diagnoses(ctx, &collected, &analysis, args.hours);
    }
    let worst = analysis.worst_status();
    let report = DiagnosticReport {
        tool: "diskdoctor".into(),
        version: crate::VERSION.into(),
        language: i18n::current(),
        generated_at: Utc::now(),
        computer: std::env::var("COMPUTERNAME").ok(),
        elevated: ctx.elevated,
        smartctl: collected.smartctl,
        event_window_hours: args.hours,
        disks: collected.disks,
        health: collected.health,
        events: collected.events,
        scans: collected.scans,
        benchmarks: collected.benchmarks,
        capacity: collected.capacity,
        analysis,
        limitations: standard_limitations(),
        warnings: collected.warnings,
    };
    let paths = write_report(&report, args.format, args.output.as_deref())?;

    if ctx.json {
        print_json(&json!({
            "files": paths,
            "worst_status": worst,
            "verdicts": report.analysis.verdicts.iter().map(|v| json!({"disk": v.disk_number, "status": v.status, "headline": v.headline})).collect::<Vec<_>>(),
        }))?;
    } else {
        for w in &report.warnings {
            ui::warn(w);
        }
        ui::heading(t("Summary", "Resumo"));
        for v in &report.analysis.verdicts {
            println!("  {} — {}: {}", v.label, ui::status_text(v.status), v.headline);
        }
        ui::heading(t("Report written", "Relatório gerado"));
        for p in &paths {
            let shown = std::fs::canonicalize(p).unwrap_or_else(|_| p.clone());
            println!("  {}", shown.display().to_string().trim_start_matches(r"\\?\"));
        }
    }
    Ok(exit_for_status(worst))
}
