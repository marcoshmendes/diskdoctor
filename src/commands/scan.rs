//! `diskdoctor scan` — Surface Scan (read-only).

use super::{Ctx, admin_required, load_disks, print_json, select_disk};
use crate::analysis::{AnalysisInput, analyze};
use crate::app::{self, exit};
use crate::cli::ScanArgs;
use crate::i18n::t;
use crate::io::BlockReader;
use crate::platform::{OpenError, RawDisk};
use crate::scan::{LiveStats, ScanConfig, ScanIssue, ScanObserver, ScanResult, ScanStatus, run_scan};
use crate::store::ResultStore;
use crate::ui;
use crate::util::{format_bytes, format_bytes_binary, format_duration, int, parse_size};
use anyhow::{Context, Result, bail};
use chrono::Utc;
use indicatif::ProgressBar;

const MAX_PRINTED_ISSUES: u32 = 30;

struct CliObserver {
    bar: ProgressBar,
    printed: u32,
}

impl ScanObserver for CliObserver {
    fn on_progress(&mut self, s: &LiveStats) {
        self.bar.set_position(s.bytes_done);
        if s.errors + s.timeouts + s.slow > 0 {
            self.bar.set_message(tr!(
                "· errors {} · timeouts {} · slow {}",
                "· erros {} · timeouts {} · lentas {}",
                int(s.errors),
                int(s.timeouts),
                int(s.slow)
            ));
        }
    }
    fn on_issue(&mut self, i: &ScanIssue) {
        if self.printed < MAX_PRINTED_ISSUES {
            self.bar.println(format!("  ⚠ LBA {} ({}): {}", int(i.lba), format_bytes(i.offset), i.description));
        } else if self.printed == MAX_PRINTED_ISSUES {
            self.bar.println(t(
                "  … more occurrences omitted (all of them are in the saved result)",
                "  … mais ocorrências omitidas (todas ficam no resultado salvo)",
            ));
        }
        self.printed += 1;
    }
}

pub fn run(ctx: &Ctx, args: &ScanArgs) -> Result<u8> {
    let disks = load_disks(ctx)?;
    let disk = select_disk(&disks, &args.disk)?.clone();
    if !ctx.elevated {
        ui::error(&admin_required(t("The Surface Scan reads the physical drive directly", "O Surface Scan acessa o disco físico diretamente")));
        return Ok(exit::NEEDS_ADMIN);
    }
    if ctx.json && !args.yes {
        bail!(t(
            "with --json also use --yes (the interactive confirmation would mix text into the JSON)",
            "com --json use também --yes (a confirmação interativa misturaria texto ao JSON)"
        ));
    }
    let block = parse_size(&args.block_size)?;
    if !(4096..=(16 << 20)).contains(&block) || block % 4096 != 0 {
        bail!(t("--block-size must be a multiple of 4 KiB between 4K and 16M", "--block-size deve ser múltiplo de 4 KiB entre 4K e 16M"));
    }
    let start = args.offset.as_deref().map(parse_size).transpose()?.unwrap_or(0);
    let length = args.length.as_deref().map(parse_size).transpose()?;
    let cfg = ScanConfig {
        start,
        end: length.map(|l| start.saturating_add(l)),
        block_size: block as u32,
        io_timeout_ms: args.timeout_ms.max(100),
        slow_threshold_ms: args.slow_ms.max(1),
        max_consecutive_errors: args.max_consecutive_errors.max(1),
        refine_errors: !args.no_refine,
        ..Default::default()
    };

    let mut reader = match RawDisk::open(disk.number, block as u32) {
        Ok(r) => r,
        Err(OpenError::AccessDenied) => {
            ui::error(&OpenError::AccessDenied.to_string());
            return Ok(exit::NEEDS_ADMIN);
        }
        Err(e) => bail!(tr!("could not open drive {}: {e}", "não foi possível abrir o disco {}: {e}", disk.number)),
    };
    let capacity = reader.capacity();
    if start >= capacity {
        bail!(tr!("--offset is past the end of the drive ({})", "--offset está além do fim do disco ({})", format_bytes(capacity)));
    }
    let end = cfg.end.unwrap_or(capacity).min(capacity);
    let bytes = end - start;

    if !ctx.json {
        ui::heading(t("Surface Scan (read-only)", "Surface Scan (somente leitura)"));
        println!("  {} {} · {} · {}", t("Drive:     ", "Disco:     "), disk.title(), disk.class_label(), format_bytes(capacity));
        println!("  {} {} → {} ({})", t("Range:     ", "Faixa:     "), format_bytes(start), format_bytes(end), format_bytes(bytes));
        println!(
            "  {}",
            tr!(
                "Reads:      {} blocks · logical sector {} B / physical {} B · timeout {} ms",
                "Leituras:   blocos de {} · setor lógico {} B / físico {} B · tempo limite {} ms",
                format_bytes_binary(block),
                reader.logical_sector_size(),
                reader.physical_sector_size(),
                int(cfg.io_timeout_ms)
            )
        );
        println!(
            "  {}",
            tr!(
                "Estimate:   ~{} (depends on the drive and on current use; it is not a guarantee)",
                "Estimativa: ~{} (depende do disco e do uso atual; não é garantia)",
                format_duration(ui::estimate_scan_time(&disk, bytes))
            )
        );
        println!();
        println!(
            "  ✔ {}",
            t(
                "This test writes NOTHING: Windows opens the drive for reading only.",
                "Este teste NÃO grava nada: o Windows abre o disco apenas para leitura.",
            )
        );
        ui::warn(t(
            "back up important data before long tests — a failing drive can get worse under continuous reads.",
            "faça backup dos dados importantes antes de testes longos — um disco com defeito pode piorar sob leitura contínua.",
        ));
        if disk.is_boot || disk.is_system {
            ui::warn(t(
                "this is the system drive: normal use competes with the test and speed may vary. Close heavy programs.",
                "este é o disco do sistema: o uso normal compete com o teste e a velocidade pode oscilar. Feche programas pesados.",
            ));
        }
        println!(
            "  {}\n",
            t("Ctrl+C cancels at any time; the partial result is saved.", "Ctrl+C cancela a qualquer momento; o resultado parcial é salvo.")
        );
    }
    if !args.yes && !ui::confirm(t("Start the Surface Scan?", "Iniciar o Surface Scan?"))? {
        println!("{}", t("Scan not started.", "Teste não iniciado."));
        return Ok(exit::DECLINED);
    }

    let _operation = app::begin_operation();
    let started_at = Utc::now();
    let bar = if ctx.json { ProgressBar::hidden() } else { ui::bytes_progress(bytes) };
    let mut observer = CliObserver { bar, printed: 0 };
    let metrics = run_scan(&mut reader, &cfg, &mut observer, app::cancel_flag());
    observer.bar.finish_and_clear();
    drop(reader);

    let result = ScanResult {
        tool_version: crate::VERSION.into(),
        disk: disk.identity(),
        started_at,
        finished_at: Utc::now(),
        elevated: ctx.elevated,
        config: cfg,
        metrics,
    };
    let saved = ResultStore::new(&ctx.data_dir).save_scan(&result);
    if let Some(out) = &args.output {
        std::fs::write(out, serde_json::to_string_pretty(&result)?).with_context(|| tr!("could not write {}", "não foi possível gravar {}", out.display()))?;
    }

    if ctx.json {
        print_json(&result)?;
    } else {
        ui::print_scan_summary(&result);
        let analysis = analyze(&AnalysisInput {
            disks: std::slice::from_ref(&disk),
            health: &[],
            events: &[],
            scans: std::slice::from_ref(&result),
            benchmarks: &[],
            capacity: &[],
            changes: crate::analysis::NO_CHANGES,
            event_window_hours: 0,
            events_collected: false,
            elevated: true,
        });
        let findings = &analysis.verdicts[0].findings;
        if findings.is_empty() {
            println!(
                "\n{}",
                t(
                    "No problems detected in this test. That does not guarantee the drive is healthy: combine it with `diskdoctor health`.",
                    "Nenhum problema detectado neste teste. Isso não garante a saúde do disco: combine com `diskdoctor health`.",
                )
            );
        } else {
            ui::heading(t("What the test shows", "Interpretação do teste"));
            for f in findings {
                ui::print_finding(f, " ");
            }
        }
        match &saved {
            Ok(p) => ui::note(&tr!(
                "\nResult saved to {} — `diskdoctor report` includes this test automatically.",
                "\nResultado salvo em {} — `diskdoctor report` inclui este teste automaticamente.",
                p.display()
            )),
            Err(e) => ui::warn(&tr!("could not save the result: {e:#}", "não foi possível salvar o resultado: {e:#}")),
        }
    }

    let m = &result.metrics;
    Ok(match m.status {
        ScanStatus::Cancelled => exit::CANCELLED,
        ScanStatus::Aborted(_) => exit::PROBLEMS_FOUND,
        ScanStatus::Completed if m.error_blocks + m.timeout_blocks > 0 || !m.bad_ranges.is_empty() => exit::PROBLEMS_FOUND,
        ScanStatus::Completed => exit::OK,
    })
}
