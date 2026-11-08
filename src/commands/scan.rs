//! `diskdoctor scan` — teste de leitura de superfície (somente leitura).

use super::{Ctx, admin_required, load_disks, print_json, select_disk};
use crate::analysis::{AnalysisInput, analyze};
use crate::app::{self, exit};
use crate::cli::ScanArgs;
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
            self.bar.set_message(format!("· erros {} · timeouts {} · lentas {}", int(s.errors), int(s.timeouts), int(s.slow)));
        }
    }
    fn on_issue(&mut self, i: &ScanIssue) {
        if self.printed < MAX_PRINTED_ISSUES {
            self.bar.println(format!("  ⚠ LBA {} ({}): {}", int(i.lba), format_bytes(i.offset), i.description));
        } else if self.printed == MAX_PRINTED_ISSUES {
            self.bar.println("  … mais ocorrências omitidas (todas ficam no resultado salvo)");
        }
        self.printed += 1;
    }
}

pub fn run(ctx: &Ctx, args: &ScanArgs) -> Result<u8> {
    let disks = load_disks(ctx)?;
    let disk = select_disk(&disks, &args.disk)?.clone();
    if !ctx.elevated {
        ui::error(&admin_required("O teste de leitura acessa o disco físico diretamente"));
        return Ok(exit::NEEDS_ADMIN);
    }
    if ctx.json && !args.yes {
        bail!("com --json use também --yes (a confirmação interativa misturaria texto ao JSON)");
    }
    let block = parse_size(&args.block_size)?;
    if !(4096..=(16 << 20)).contains(&block) || block % 4096 != 0 {
        bail!("--block-size deve ser múltiplo de 4 KiB entre 4K e 16M");
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
            ui::error(&format!("{}", OpenError::AccessDenied));
            return Ok(exit::NEEDS_ADMIN);
        }
        Err(e) => bail!("não foi possível abrir o disco {}: {e}", disk.number),
    };
    let capacity = reader.capacity();
    if start >= capacity {
        bail!("--offset está além do fim do disco ({})", format_bytes(capacity));
    }
    let end = cfg.end.unwrap_or(capacity).min(capacity);
    let bytes = end - start;

    if !ctx.json {
        ui::heading("Teste de leitura de superfície (somente leitura)");
        println!("  Disco:     {} · {} · {}", disk.title(), disk.class_label(), format_bytes(capacity));
        println!("  Faixa:     {} → {} ({})", format_bytes(start), format_bytes(end), format_bytes(bytes));
        println!(
            "  Leituras:  blocos de {} · setor lógico {} B / físico {} B · tempo limite {} ms",
            format_bytes_binary(block),
            reader.logical_sector_size(),
            reader.physical_sector_size(),
            int(cfg.io_timeout_ms)
        );
        println!("  Estimativa: ~{} (depende do disco e do uso atual)", format_duration(ui::estimate_scan_time(&disk, bytes)));
        println!();
        println!("  ✔ Este teste NÃO grava nada: o disco é aberto pelo Windows apenas para leitura.");
        ui::warn("faça backup dos dados importantes antes de testes longos — um disco com defeito pode piorar sob leitura contínua.");
        if disk.is_boot || disk.is_system {
            ui::warn("este é o disco do sistema: o uso normal compete com o teste e a velocidade pode oscilar. Feche programas pesados.");
        }
        println!("  Ctrl+C cancela a qualquer momento; o resultado parcial é salvo.\n");
    }
    if !args.yes && !ui::confirm("Iniciar o teste de leitura?")? {
        println!("Teste não iniciado.");
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
        std::fs::write(out, serde_json::to_string_pretty(&result)?).with_context(|| format!("não foi possível gravar {}", out.display()))?;
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
            println!("\nNenhum problema detectado neste teste. Isso não garante a saúde do disco: combine com `diskdoctor health`.");
        } else {
            ui::heading("Interpretação do teste");
            for f in findings {
                ui::print_finding(f, " ");
            }
        }
        match &saved {
            Ok(p) => ui::note(&format!("\nResultado salvo em {} — `diskdoctor report` inclui este teste automaticamente.", p.display())),
            Err(e) => ui::warn(&format!("não foi possível salvar o resultado: {e:#}")),
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
