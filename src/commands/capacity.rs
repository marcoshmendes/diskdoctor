//! `diskdoctor capacity` — Capacity Verification: checks that the advertised capacity is
//! real (f3 style).
//!
//! The only command, together with `benchmark --write-test`, that writes anything: **new**
//! files in a **new** folder of the tested volume, deleted at the end. See `capacity.rs` and
//! `platform::win::CapacityDir`.

use super::{Ctx, load_disks, select_disk};
use crate::app::{self, exit};
use crate::capacity::{CapacityConfig, CapacityLive, CapacityObserver, CapacityPhase, CapacityResult, CapacityVerdict, estimate_real_capacity, new_seed, run_capacity_test};
use crate::cli::CapacityArgs;
use crate::i18n::t;
use crate::inventory::{DiskInfo, PartitionInfo, disk_for_letter};
use crate::platform::{self, CapacityDir};
use crate::scan::ScanStatus;
use crate::store::ResultStore;
use crate::ui;
use crate::util::{format_bytes, format_duration, format_mbps, int, parse_size};
use anyhow::{Result, bail};
use chrono::Utc;
use indicatif::ProgressBar;
use std::path::PathBuf;

const MIB: u64 = 1 << 20;
const MAX_PRINTED_PROBLEMS: u32 = 15;

struct CliObserver {
    bar: Option<ProgressBar>,
    printed: u32,
}

impl CapacityObserver for CliObserver {
    fn on_phase(&mut self, phase: CapacityPhase, total: u64) {
        if let Some(b) = self.bar.take() {
            b.finish_and_clear();
        }
        let bar = ui::bytes_progress(total);
        bar.set_prefix(match phase {
            CapacityPhase::Write => t("1/2 Writing   ", "1/2 Gravando   "),
            CapacityPhase::Verify => t("2/2 Verifying ", "2/2 Conferindo "),
        });
        self.bar = Some(bar);
    }
    fn on_progress(&mut self, live: &CapacityLive) {
        if let Some(b) = &self.bar {
            b.set_position(live.done);
            if live.bad_bytes > 0 || live.write_errors > 0 {
                b.set_message(tr!("· problems: {}", "· problemas: {}", format_bytes(live.bad_bytes)));
            }
        }
    }
    fn on_problem(&mut self, text: &str) {
        if let Some(b) = &self.bar {
            if self.printed < MAX_PRINTED_PROBLEMS {
                b.println(format!("  ⚠ {text}"));
            } else if self.printed == MAX_PRINTED_PROBLEMS {
                b.println(t(
                    "  … more problems omitted (all of them are in the saved result)",
                    "  … mais problemas omitidos (todos ficam no resultado salvo)",
                ));
            }
        }
        self.printed += 1;
    }
}

/// Picks the volume: `--volume E:` or the drive's only lettered volume.
fn pick_volume<'a>(disks: &'a [DiskInfo], args: &CapacityArgs) -> Result<(&'a DiskInfo, &'a PartitionInfo)> {
    let disk = match (&args.disk, &args.volume) {
        (Some(r), _) => select_disk(disks, r)?,
        (None, Some(v)) => {
            let n = disk_for_letter(disks, v).ok_or_else(|| anyhow::anyhow!(tr!("volume {v} was not found on any drive", "o volume {v} não foi encontrado em nenhum disco")))?;
            disks.iter().find(|d| d.number == n).expect("drive exists")
        }
        (None, None) => bail!(t("give --disk or --volume", "informe --disk ou --volume")),
    };
    let lettered: Vec<&PartitionInfo> = disk.partitions.iter().filter(|p| p.drive_letter.is_some()).collect();
    let part = match &args.volume {
        Some(v) => {
            let want = v.trim().trim_end_matches(':').to_ascii_uppercase();
            lettered
                .iter()
                .find(|p| p.drive_letter.as_deref().is_some_and(|l| l.eq_ignore_ascii_case(&want)))
                .copied()
                .ok_or_else(|| anyhow::anyhow!(tr!("volume {want}: is not on drive {}", "o volume {want}: não pertence ao disco {}", disk.number)))?
        }
        None => match lettered.as_slice() {
            [one] => one,
            [] => bail!(tr!(
                "drive {} has no lettered volume; format it or assign a letter in Disk Management first",
                "o disco {} não tem volume com letra; formate-o ou atribua uma letra no Gerenciamento de Disco antes",
                disk.number
            )),
            many => {
                let list = many.iter().map(|p| p.display_name()).collect::<Vec<_>>().join(", ");
                bail!(tr!(
                    "drive {} has several volumes ({list}); pick one with --volume",
                    "o disco {} tem vários volumes ({list}); escolha um com --volume",
                    disk.number
                ))
            }
        },
    };
    Ok((disk, part))
}

pub fn run(ctx: &Ctx, args: &CapacityArgs) -> Result<u8> {
    if ctx.json {
        bail!(t(
            "Capacity Verification writes data and requires a typed confirmation; run it without --json",
            "a Capacity Verification grava dados e exige confirmação digitada; rode sem --json"
        ));
    }
    let disks = load_disks(ctx)?;
    let (disk, part) = pick_volume(&disks, args)?;
    let letter = part.drive_letter.clone().expect("lettered volume");
    if disk.is_boot || disk.is_system {
        bail!(tr!(
            "drive {} is the system drive: filling its free space would freeze Windows. Capacity Verification is for USB flash drives, memory cards and external drives",
            "o disco {} é o disco do sistema: encher o espaço livre dele travaria o Windows. A Capacity Verification é para pendrives, cartões e discos externos",
            disk.number
        ));
    }
    if disk.is_read_only || disk.is_offline {
        bail!(tr!("drive {} is offline or read-only in Windows", "o disco {} está offline ou somente leitura no Windows", disk.number));
    }
    let root = PathBuf::from(format!("{letter}:\\"));
    let free = platform::free_space_bytes(&root).or(part.volume_free).unwrap_or(0);
    let volume_size = part.volume_size.unwrap_or(part.size);
    let used = volume_size.saturating_sub(free);
    let margin = (64 * MIB).max(free / 200);
    let mut total = free.saturating_sub(margin) / MIB * MIB;
    if let Some(s) = &args.size {
        total = total.min(parse_size(s)? / MIB * MIB);
    }
    if total < 64 * MIB {
        bail!(tr!(
            "not enough free space on {letter}: ({} free; the test needs at least 64 MiB plus a margin)",
            "espaço livre insuficiente em {letter}: ({} livres; o teste precisa de pelo menos 64 MiB além de uma margem)",
            format_bytes(free)
        ));
    }
    let partial = args.size.is_some() && total + margin + 64 * MIB < free;

    ui::heading("Capacity Verification");
    println!("  {} {} · {} · {}", t("Device:     ", "Dispositivo:"), disk.title(), disk.class_label(), format_bytes(disk.size_bytes));
    println!(
        "  {}",
        tr!(
            "Volume:      {letter}: ({}) · {} · {} used · {} free",
            "Volume:      {letter}: ({}) · {} · {} ocupados · {} livres",
            part.file_system.as_deref().unwrap_or("?"),
            format_bytes(volume_size),
            format_bytes(used),
            format_bytes(free)
        )
    );
    println!(
        "  {}",
        tr!(
            "Plan:        write {} in 1 GiB files to a new folder on {letter}:\\, read everything back and delete it",
            "Plano:       gravar {} em arquivos de 1 GiB numa pasta nova em {letter}:\\, ler tudo de volta e apagar",
            format_bytes(total)
        )
    );
    let est = ui::estimate_scan_time(disk, total);
    println!(
        "  {}",
        tr!(
            "Estimate:    ~{} (writing is usually much slower than reading on USB drives and cards; not a guarantee)",
            "Estimativa:  ~{} (gravação costuma ser bem mais lenta que leitura em pendrives e cartões; não é garantia)",
            format_duration(est * 3)
        )
    );
    println!();
    println!(
        "  ✔ {}",
        t(
            "No existing file is opened or changed; only new files are created, and they are deleted at the end.",
            "Nenhum arquivo existente é aberto ou alterado; só são criados arquivos novos, apagados no final.",
        )
    );
    ui::warn(t(
        "on a FAKE device, writing past the real capacity can corrupt what is already on it. Back up first — ideally test an empty device.",
        "num dispositivo FALSO, gravar além da capacidade real pode corromper o que já está nele. Faça backup antes — o ideal é testar com o dispositivo vazio.",
    ));
    ui::warn(&tr!(
        "during the test the volume {letter}: is almost full: other programs may fail to write to it.",
        "durante o teste o volume {letter}: fica quase cheio: outros programas podem falhar ao gravar nele."
    ));
    if !disk.is_flash_media() && !disk.is_external() {
        ui::warn(t(
            "this test is meant for USB flash drives, memory cards and external drives. On an internal drive it only confirms the free space and, on an SSD, uses one write cycle.",
            "este teste foi feito para pendrives, cartões e discos externos. Num disco interno ele só confirma o espaço livre e, em SSD, gasta um ciclo de gravação.",
        ));
    }
    if partial {
        ui::warn(t(
            "with --size the test does not cover all the free space and may miss a counterfeit.",
            "com --size o teste não cobre todo o espaço livre e pode não detectar falsificação.",
        ));
    }
    if !ui::confirm_typed(t("Confirm writing the test files.", "Confirme a gravação dos arquivos de teste."), ui::WRITE_CONFIRMATION)? {
        println!("{}", t("Test cancelled; nothing was written.", "Teste cancelado; nada foi gravado."));
        return Ok(exit::DECLINED);
    }

    let cfg = CapacityConfig { total_bytes: total, io_timeout_ms: args.timeout_ms.max(1000), seed: new_seed(), ..Default::default() };
    let mut dir = CapacityDir::create(&root, cfg.block_size)
        .map_err(|e| anyhow::anyhow!(tr!("could not create the test folder on {letter}:\\ ({e})", "não foi possível criar a pasta de teste em {letter}:\\ ({e})")))?;
    println!("\n  {}", tr!("Test folder: {}", "Pasta de teste: {}", dir.dir().display()));
    println!("  {}\n", t("Ctrl+C cancels; the test files are deleted.", "Ctrl+C cancela; os arquivos de teste são apagados."));

    let operation = app::begin_operation();
    let started_at = Utc::now();
    let mut observer = CliObserver { bar: None, printed: 0 };
    let metrics = run_capacity_test(&mut dir, &cfg, &mut observer, app::cancel_flag());
    if let Some(b) = observer.bar.take() {
        b.finish_and_clear();
    }
    // Still inside the "operation": a Ctrl+C here does not interrupt the cleanup.
    let cleanup_errors = dir.cleanup();
    drop(operation);

    let result = CapacityResult {
        tool_version: crate::VERSION.into(),
        disk: disk.identity(),
        volume: format!("{letter}:"),
        file_system: part.file_system.clone(),
        volume_size: Some(volume_size),
        used_before: used,
        free_before: free,
        started_at,
        finished_at: Utc::now(),
        estimated_real_capacity: estimate_real_capacity(&metrics, used),
        config: cfg,
        metrics,
        cleanup_ok: cleanup_errors.is_empty(),
    };
    let saved = ResultStore::new(&ctx.data_dir).save_capacity(&result);
    print_summary(&result);
    for e in &cleanup_errors {
        ui::warn(&tr!(
            "could not delete {e}. Delete the folder {} manually",
            "não foi possível apagar {e}. Apague a pasta {} manualmente",
            dir.dir().display()
        ));
    }
    match &saved {
        Ok(p) => ui::note(&tr!(
            "\nResult saved to {} — `diskdoctor report` includes this verification.",
            "\nResultado salvo em {} — `diskdoctor report` inclui esta verificação.",
            p.display()
        )),
        Err(e) => ui::warn(&tr!("could not save the result: {e:#}", "não foi possível salvar o resultado: {e:#}")),
    }

    Ok(match (&result.metrics.status, result.metrics.verdict) {
        (ScanStatus::Cancelled, _) => exit::CANCELLED,
        (_, CapacityVerdict::Fake | CapacityVerdict::Defective) => exit::PROBLEMS_FOUND,
        (_, CapacityVerdict::Genuine) => exit::OK,
        (_, CapacityVerdict::Inconclusive) => exit::ERROR,
    })
}

fn print_summary(r: &CapacityResult) {
    let m = &r.metrics;
    let verdict = match m.verdict {
        CapacityVerdict::Genuine => console::style(m.verdict.label()).green().bold(),
        CapacityVerdict::Fake | CapacityVerdict::Defective => console::style(m.verdict.label()).red().bold(),
        CapacityVerdict::Inconclusive => console::style(m.verdict.label()).yellow().bold(),
    };
    ui::heading(t("Capacity Verification result", "Resultado da Capacity Verification"));
    println!("  {} {verdict}  ({})", t("Verdict:    ", "Veredito:   "), m.status.label());
    println!(
        "  {}",
        tr!(
            "Written:     {} of {} · {}",
            "Gravado:     {} de {} · {}",
            format_bytes(m.written_bytes),
            format_bytes(m.planned_bytes),
            format_mbps(m.write_mb_per_s)
        )
    );
    println!("  {} {} · {}", t("Verified:   ", "Conferido:  "), format_bytes(m.verified_bytes), format_mbps(m.read_mb_per_s));
    println!("  {} {}", t("Correct:    ", "Correto:    "), format_bytes(m.ok_bytes));
    if m.bad_bytes() > 0 || m.write_errors > 0 {
        println!(
            "  {}",
            tr!(
                "Problems:    overwritten {} · lost {} · corrupted {} · unreadable {} · write failures {}",
                "Problemas:   sobrescritos {} · perdidos {} · corrompidos {} · ilegíveis {} · falhas de gravação {}",
                format_bytes(m.overwritten_bytes),
                format_bytes(m.lost_bytes),
                format_bytes(m.corrupted_bytes),
                format_bytes(m.unreadable_bytes),
                int(m.write_errors)
            )
        );
    }
    println!("  {}", tr!("Not tested:  {} that were already in use", "Não testado: {} que já estavam ocupados", format_bytes(r.used_before)));
    println!();
    match m.verdict {
        CapacityVerdict::Genuine => println!(
            "  {}",
            tr!(
                "All the free space tested kept the data correctly: the advertised capacity is real{}.",
                "Todo o espaço livre testado guardou os dados corretamente: a capacidade anunciada é real{}.",
                if m.stopped_by_full_disk {
                    t(" (the volume filled up slightly earlier than expected, which is normal)", " (o volume encheu um pouco antes do previsto, o que é normal)")
                } else {
                    ""
                }
            )
        ),
        CapacityVerdict::Fake => {
            println!("  {} {}", console::style("✖").red().bold(), t("The device does not store everything it advertises.", "O dispositivo não guarda tudo o que anuncia."));
            if let Some(real) = r.estimated_real_capacity {
                println!(
                    "  {}",
                    tr!(
                        "Estimated real capacity: {} (advertised: {}).",
                        "Capacidade real estimada: {} (anunciada: {}).",
                        format_bytes(real),
                        format_bytes(r.disk.size_bytes)
                    )
                );
            }
            println!(
                "  {}",
                t(
                    "Do not keep important data on it, and ask the seller for a replacement or refund.",
                    "Não guarde dados importantes nele e peça troca ou reembolso ao vendedor.",
                )
            );
        }
        CapacityVerdict::Defective => {
            println!(
                "  {} {}",
                console::style("✖").red().bold(),
                t("The device did not return part of the written data correctly.", "O dispositivo não devolveu corretamente parte dos dados gravados.")
            );
            println!(
                "  {}",
                t(
                    "Copy what matters somewhere else and repeat on another USB port/reader to rule out poor contact.",
                    "Copie o que importa para outro lugar e repita em outra porta USB/leitor para descartar mau contato.",
                )
            );
        }
        CapacityVerdict::Inconclusive => println!("  {}", t("The test did not finish; nothing can be concluded.", "O teste não foi até o fim; nada pode ser concluído.")),
    }
    if !m.error_samples.is_empty() {
        println!("\n  {}", t("First problems:", "Primeiros problemas:"));
        for s in m.error_samples.iter().take(5) {
            println!("   - {s}");
        }
    }
}
