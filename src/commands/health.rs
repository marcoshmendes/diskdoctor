//! `diskdoctor health`

use super::{CollectOptions, Ctx, analyze_collected, collect, exit_for_status, load_disks, print_json, record_diagnoses, select_disk};
use crate::cli::HealthArgs;
use crate::ui;
use anyhow::Result;
use serde_json::json;

pub fn run(ctx: &Ctx, args: &HealthArgs) -> Result<u8> {
    let all = load_disks(ctx)?;
    let only_disk = args.disk.as_ref().map(|r| select_disk(&all, r).map(|d| d.number)).transpose()?;
    let collected = collect(
        ctx,
        all,
        CollectOptions {
            only_disk,
            hours: args.hours,
            with_events: !args.no_events,
            record_history: !args.no_history,
            saved_results: true,
            extra_scans: vec![],
        },
    )?;
    let analysis = analyze_collected(ctx, &collected, args.hours);
    if !args.no_history {
        record_diagnoses(ctx, &collected, &analysis, args.hours);
    }
    if ctx.json {
        print_json(&json!({
            "elevated": ctx.elevated,
            "smartctl": collected.smartctl,
            "warnings": collected.warnings,
            "disks": collected.disks,
            "health": collected.health,
            "events_collected": collected.events_collected,
            "event_count": collected.events.len(),
            "analysis": analysis,
        }))?;
    } else {
        for w in &collected.warnings {
            ui::warn(w);
        }
        ui::print_health(&collected.disks, &collected.health, &analysis);
        ui::note("Relatório completo (HTML): `diskdoctor report`. Teste de superfície: `diskdoctor scan --disk N` (Administrador).");
    }
    Ok(exit_for_status(analysis.worst_status()))
}
