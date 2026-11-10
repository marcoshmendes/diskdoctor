//! `diskdoctor benchmark` — reads on the physical drive (read-only) and, optionally, a
//! write test on a new temporary file after a typed confirmation.

use super::{Ctx, admin_required, load_disks, print_json, select_disk};
use crate::app::{self, exit};
use crate::benchmark::{BenchConfig, BenchObserver, BenchmarkResult, WriteBenchInfo, display_label, run_read_benchmark, run_write_benchmark};
use crate::cli::BenchmarkArgs;
use crate::i18n::t;
use crate::inventory::{DiskInfo, disk_for_letter};
use crate::platform::{self, OpenError, RawDisk, TempBenchFile};
use crate::scan::ScanStatus;
use crate::store::ResultStore;
use crate::ui;
use crate::util::{format_bytes, parse_size};
use anyhow::{Context, Result, bail};
use chrono::Utc;
use indicatif::ProgressBar;
use std::path::{Component, Path, PathBuf, Prefix};

struct CliBenchObserver {
    json: bool,
    bar: Option<ProgressBar>,
}

impl BenchObserver for CliBenchObserver {
    fn on_phase(&mut self, label: &str, total: u64) {
        if let Some(b) = self.bar.take() {
            b.finish_and_clear();
        }
        if self.json {
            return;
        }
        let shown = display_label(label);
        let bar = if label.contains("4 KiB") { ui::ops_progress(total, &shown) } else { ui::bytes_progress(total) };
        if !label.contains("4 KiB") {
            bar.set_message(format!("· {shown}"));
        }
        self.bar = Some(bar);
    }
    fn on_progress(&mut self, done: u64) {
        if let Some(b) = &self.bar {
            b.set_position(done);
        }
    }
}

impl CliBenchObserver {
    fn finish(&mut self) {
        if let Some(b) = self.bar.take() {
            b.finish_and_clear();
        }
    }
}

fn drive_letter(path: &Path) -> Option<String> {
    match path.components().next()? {
        Component::Prefix(p) => match p.kind() {
            Prefix::Disk(l) | Prefix::VerbatimDisk(l) => Some((l as char).to_ascii_uppercase().to_string()),
            _ => None,
        },
        _ => None,
    }
}

/// Validates the write test folder: it must exist, be on the tested drive and have room.
fn validate_write_target(disks: &[DiskInfo], disk: &DiskInfo, dir: &Path, size: u64) -> Result<PathBuf> {
    let canonical = dir.canonicalize().with_context(|| tr!("the folder {} does not exist", "a pasta {} não existe", dir.display()))?;
    if !canonical.is_dir() {
        bail!(tr!("{} is not a folder", "{} não é uma pasta", dir.display()));
    }
    let letter = drive_letter(&canonical)
        .ok_or_else(|| anyhow::anyhow!(tr!("could not find the drive letter of {}", "não foi possível identificar a letra da unidade de {}", dir.display())))?;
    match disk_for_letter(disks, &letter) {
        Some(n) if n == disk.number => {}
        Some(n) => bail!(tr!(
            "the folder {} is on drive {n}, not on drive {}. The write test must use a volume of the tested drive",
            "a pasta {} fica no disco {n}, não no disco {}. O teste de escrita precisa usar um volume do disco testado",
            dir.display(),
            disk.number
        )),
        None => bail!(tr!("could not link drive {letter}: to a physical drive", "não foi possível associar a unidade {letter}: a um disco físico")),
    }
    let free = platform::free_space_bytes(&canonical).unwrap_or(0);
    let margin = (1u64 << 30).max(size / 10);
    if free < size + margin {
        bail!(tr!(
            "not enough free space on {letter}: ({} free; {} needed plus a {} margin)",
            "espaço livre insuficiente em {letter}: ({} livres; necessário {} + margem de {})",
            format_bytes(free),
            format_bytes(size),
            format_bytes(margin)
        ));
    }
    Ok(canonical)
}

pub fn run(ctx: &Ctx, args: &BenchmarkArgs) -> Result<u8> {
    let disks = load_disks(ctx)?;
    let disk = select_disk(&disks, &args.disk)?.clone();
    if !ctx.elevated {
        ui::error(&admin_required(t("The Benchmark reads the physical drive directly", "O Benchmark lê o disco físico diretamente")));
        return Ok(exit::NEEDS_ADMIN);
    }
    let cfg = BenchConfig {
        seq_bytes_per_zone: parse_size(&args.seq_size)?,
        random_duration_ms: args.random_seconds.max(1) * 1000,
        io_timeout_ms: args.timeout_ms.max(100),
        ..Default::default()
    };
    let write_size = parse_size(&args.write_size)?;
    let write_target = match &args.write_test {
        Some(dir) => {
            if write_size < (64 << 20) || write_size % 4096 != 0 {
                bail!(t("--write-size must be at least 64M and a multiple of 4 KiB", "--write-size deve ser de pelo menos 64M e múltiplo de 4 KiB"));
            }
            Some(validate_write_target(&disks, &disk, dir, write_size)?)
        }
        None => None,
    };

    if let Some(dir) = &write_target {
        ui::heading(t("WRITE test requested", "Teste de ESCRITA solicitado"));
        println!(
            "  {}",
            tr!(
                "A NEW temporary file of {} will be created in {}.",
                "Será criado um arquivo temporário NOVO de {} em {}.",
                format_bytes(write_size),
                dir.display()
            )
        );
        println!(
            "  {}",
            t(
                "Random data is written to it and the file is deleted at the end (Windows removes it even if the program is killed).",
                "Dados aleatórios serão gravados nele e o arquivo será apagado ao final (o Windows o remove mesmo se o programa for encerrado).",
            )
        );
        println!(
            "  {}",
            t(
                "No existing file is opened or changed. On an SSD this uses a tiny fraction of its endurance.",
                "Nenhum arquivo existente é aberto ou alterado. Em SSD, isso consome uma fração mínima da vida útil.",
            )
        );
        if !ui::confirm_typed(t("Confirm the write test.", "Confirme o teste de escrita."), ui::WRITE_CONFIRMATION)? {
            println!("{}", t("Write test cancelled; nothing was written.", "Teste de escrita cancelado; nada foi gravado."));
            return Ok(exit::DECLINED);
        }
    } else if !ctx.json {
        ui::heading(&tr!("Benchmark — {} (read-only)", "Benchmark — {} (somente leitura)", disk.title()));
    }

    let _operation = app::begin_operation();
    let started_at = Utc::now();
    let mut observer = CliBenchObserver { json: ctx.json, bar: None };

    let mut reader = match RawDisk::open(disk.number, cfg.seq_block) {
        Ok(r) => r,
        Err(OpenError::AccessDenied) => {
            ui::error(&OpenError::AccessDenied.to_string());
            return Ok(exit::NEEDS_ADMIN);
        }
        Err(e) => bail!(tr!("could not open drive {}: {e}", "não foi possível abrir o disco {}: {e}", disk.number)),
    };
    let read = run_read_benchmark(&mut reader, &cfg, &mut observer, app::cancel_flag());
    observer.finish();
    drop(reader);

    let mut write = None;
    if let Some(dir) = &write_target {
        if read.status == ScanStatus::Completed {
            let mut file = TempBenchFile::create(dir, write_size, cfg.seq_block)
                .with_context(|| tr!("could not create the temporary file in {}", "não foi possível criar o arquivo temporário em {}", dir.display()))?;
            let temp_path = file.path().to_path_buf();
            let metrics = run_write_benchmark(&mut file, &cfg, &mut observer, app::cancel_flag());
            observer.finish();
            drop(file);
            if temp_path.exists() {
                ui::warn(&tr!(
                    "the temporary file {} was not removed automatically; delete it manually",
                    "o arquivo temporário {} não foi removido automaticamente; apague-o manualmente",
                    temp_path.display()
                ));
            }
            write = Some(WriteBenchInfo { target_dir: dir.display().to_string(), file_size: write_size, metrics });
        }
    }

    let result = BenchmarkResult {
        tool_version: crate::VERSION.into(),
        disk: disk.identity(),
        started_at,
        finished_at: Utc::now(),
        config: cfg,
        read: Some(read),
        write,
    };
    let saved = ResultStore::new(&ctx.data_dir).save_benchmark(&result);
    if ctx.json {
        print_json(&result)?;
    } else {
        ui::print_benchmark(&result);
        if let Ok(p) = &saved {
            ui::note(&tr!("Result saved to {}", "Resultado salvo em {}", p.display()));
        }
    }
    let cancelled = result.read.as_ref().is_some_and(|r| r.status == ScanStatus::Cancelled)
        || result.write.as_ref().is_some_and(|w| w.metrics.status == ScanStatus::Cancelled);
    Ok(if cancelled { exit::CANCELLED } else { exit::OK })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_drive_letters() {
        assert_eq!(drive_letter(Path::new(r"D:\folder")).as_deref(), if cfg!(windows) { Some("D") } else { None });
        assert_eq!(drive_letter(Path::new(r"\\?\e:\x")).as_deref(), if cfg!(windows) { Some("E") } else { None });
        assert_eq!(drive_letter(Path::new("relative")), None);
    }
}
