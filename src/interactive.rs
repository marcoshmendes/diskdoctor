//! Modo interativo: `diskdoctor` sem argumentos (ou duplo clique no `.exe`).
//!
//! Menu navegável com as setas (dialoguer). Cada ação monta a **linha de comando
//! equivalente**, mostra ao usuário ("Executando: diskdoctor scan --disk 2 --read-only")
//! e a executa pelo mesmo caminho da CLI ([`app::dispatch`]). Por isso confirmações,
//! avisos de backup e travas de segurança são exatamente os mesmos da CLI — o menu
//! nunca pula uma confirmação.

use crate::app::{self, exit};
use crate::cli::Cli;
use crate::commands::{Ctx, load_disks};
use crate::inventory::DiskInfo;
use crate::smart::Smartctl;
use crate::util::format_bytes;
use crate::{platform, ui};
use clap::Parser;
use console::{Term, style};
use dialoguer::theme::ColorfulTheme;
use dialoguer::{Confirm, Input, Select};
use std::io::Write;
use std::path::PathBuf;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Health,
    Scan,
    Capacity,
    Benchmark,
    Events,
    History,
    Tags,
    Report,
    Quit,
}

const MENU: &[(Action, &str, &str)] = &[
    (Action::Health, "Diagnóstico completo", "SMART + eventos do Windows + análise · ~30 s"),
    (Action::Scan, "Teste de leitura (scan)", "lê o disco inteiro, sem gravar · Administrador"),
    (Action::Capacity, "Verificar capacidade real", "pendrive/cartão falsificado · grava arquivos temporários"),
    (Action::Benchmark, "Benchmark de leitura", "velocidade e latência · Administrador"),
    (Action::Events, "Eventos do Windows", "erros, resets e travamentos registrados"),
    (Action::History, "Histórico e antes × depois", "registre mudanças (ex.: troca de cabo) e compare"),
    (Action::Tags, "Tags dos discos", "dê nomes como Studio, Jogos, Backup"),
    (Action::Report, "Gerar relatório HTML", "tudo num arquivo para abrir no navegador"),
    (Action::Quit, "Sair", ""),
];

enum Flow {
    Continue,
    Exit(u8),
}

pub fn run(ctx: &Ctx) -> u8 {
    let term = Term::stdout();
    let theme = ColorfulTheme::default();
    let smartctl = Smartctl::locate();
    loop {
        let _ = term.clear_screen();
        banner(ctx, smartctl.is_some());
        let disks = match load_disks(ctx) {
            Ok(d) => d,
            Err(e) => {
                ui::error(&format!("{e:#}"));
                pause("Pressione Enter para sair");
                return exit::ERROR;
            }
        };
        ui::print_disk_overview(&disks);
        println!();
        let items: Vec<String> = MENU.iter().map(|(_, title, hint)| format!("{title:<28} {}", style(hint).dim())).collect();
        let choice = Select::with_theme(&theme)
            .with_prompt("O que você quer fazer?  (↑ ↓ para escolher, Enter confirma, Esc sai)")
            .items(&items)
            .default(0)
            .interact_on_opt(&term);
        let Ok(Some(i)) = choice else { return exit::OK };
        let action = MENU[i].0;
        if action == Action::Quit {
            return exit::OK;
        }
        println!();
        match handle(ctx, &theme, action, &disks) {
            Flow::Continue => pause("\nPressione Enter para voltar ao menu"),
            Flow::Exit(code) => return code,
        }
    }
}

fn banner(ctx: &Ctx, smartctl: bool) {
    let bar = style("┃").cyan().bold();
    println!();
    println!("  {bar} {}", style(format!("diskdoctor {}", crate::VERSION)).bold());
    println!("  {bar} Diagnóstico de HDs, SSDs, pendrives e cartões de memória");
    println!("  {bar} {}", style("Nada é gravado nos seus discos sem você confirmar.").dim());
    println!();
    let admin = if ctx.elevated {
        format!("{} Administrador", style("✔").green())
    } else {
        format!("{} sem Administrador {}", style("•").yellow(), style("(scan e benchmark pedem para reabrir)").dim())
    };
    let smart = if smartctl {
        format!("{} smartctl", style("✔").green())
    } else {
        format!("{} smartctl ausente {}", style("✖").red(), style("(instale: winget install smartmontools.smartmontools)").dim())
    };
    println!("  {admin}   {smart}");
}

fn pause(msg: &str) {
    print!("{}", style(msg).dim());
    let _ = std::io::stdout().flush();
    let _ = Term::stdout().read_line();
}

/// Executa uma linha de comando do diskdoctor pelo caminho normal da CLI, mostrando-a antes.
fn run_args(ctx: &Ctx, args: &[String]) -> u8 {
    let shown: Vec<String> = args.iter().map(|a| if a.contains(' ') { format!("\"{a}\"") } else { a.clone() }).collect();
    println!("{} {}\n", style("Executando:").dim(), style(format!("diskdoctor {}", shown.join(" "))).cyan());
    let argv = std::iter::once("diskdoctor".to_string()).chain(args.iter().cloned());
    match Cli::try_parse_from(argv) {
        Ok(Cli { command: Some(cmd), .. }) => match app::dispatch(ctx, &cmd) {
            Ok(code) => code,
            Err(e) => {
                ui::error(&format!("{e:#}"));
                exit::ERROR
            }
        },
        Ok(_) => exit::USAGE,
        Err(e) => {
            eprint!("{}", app::translate_clap_error(&e.render().to_string()));
            exit::USAGE
        }
    }
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn disk_line(d: &DiskInfo) -> String {
    let letters = d.volume_letters();
    format!(
        "{} · {} · {}{}{}",
        d.title(),
        d.class_label(),
        format_bytes(d.size_bytes),
        if letters.is_empty() { String::new() } else { format!(" · {}", letters.join(" ")) },
        if d.is_boot || d.is_system { " (sistema)" } else { "" }
    )
}

/// Escolha de disco. `Ok(None)` = "todos" (quando permitido); `Err(())` = voltar.
fn pick_disk<'a>(theme: &ColorfulTheme, disks: &[&'a DiskInfo], prompt: &str, allow_all: bool) -> Result<Option<&'a DiskInfo>, ()> {
    let mut items: Vec<String> = Vec::new();
    if allow_all {
        items.push("Todos os discos".into());
    }
    items.extend(disks.iter().map(|d| disk_line(d)));
    let i = Select::with_theme(theme).with_prompt(prompt).items(&items).default(0).interact_opt().ok().flatten().ok_or(())?;
    if allow_all {
        Ok(if i == 0 { None } else { Some(disks[i - 1]) })
    } else {
        Ok(Some(disks[i]))
    }
}

/// Testes de leitura direta exigem Administrador: oferece reabrir elevado.
fn ensure_admin(ctx: &Ctx, theme: &ColorfulTheme, what: &str) -> Option<Flow> {
    if ctx.elevated {
        return None;
    }
    println!("{what} lê o disco diretamente e precisa de permissão de Administrador.");
    let reopen = Confirm::with_theme(theme)
        .with_prompt("Reabrir o diskdoctor como Administrador? (o Windows vai pedir sua confirmação)")
        .default(true)
        .interact()
        .unwrap_or(false);
    if reopen {
        match platform::relaunch_as_admin("") {
            Ok(()) => {
                println!("Abrindo uma nova janela como Administrador…");
                return Some(Flow::Exit(exit::OK));
            }
            Err(e) => ui::error(&e),
        }
    }
    Some(Flow::Continue)
}

fn reports_dir() -> PathBuf {
    std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join("Documents").join("diskdoctor")).unwrap_or_else(|| PathBuf::from("."))
}

fn handle(ctx: &Ctx, theme: &ColorfulTheme, action: Action, disks: &[DiskInfo]) -> Flow {
    let all: Vec<&DiskInfo> = disks.iter().collect();
    match action {
        Action::Health => {
            let Ok(d) = pick_disk(theme, &all, "Qual disco?", true) else { return Flow::Continue };
            let mut a = args(&["health"]);
            if let Some(d) = d {
                a.extend(args(&["--disk", &d.number.to_string()]));
            }
            run_args(ctx, &a);
        }
        Action::Scan | Action::Benchmark => {
            let what = if action == Action::Scan { "O teste de leitura" } else { "O benchmark" };
            if let Some(flow) = ensure_admin(ctx, theme, what) {
                return flow;
            }
            if action == Action::Scan {
                println!("{}", style("Dica: para investigar travamentos, teste um disco por vez e evite usar o PC pesado durante o teste.").dim());
            }
            let Ok(Some(d)) = pick_disk(theme, &all, "Qual disco?", false) else { return Flow::Continue };
            let cmd = if action == Action::Scan { "scan" } else { "benchmark" };
            run_args(ctx, &args(&[cmd, "--disk", &d.number.to_string(), "--read-only"]));
        }
        Action::Capacity => {
            let eligible: Vec<&DiskInfo> =
                disks.iter().filter(|d| !d.is_boot && !d.is_system && d.partitions.iter().any(|p| p.drive_letter.is_some())).collect();
            if eligible.is_empty() {
                println!("Nenhum disco elegível: conecte o pendrive, cartão ou disco externo (o disco do sistema não pode ser testado).");
                return Flow::Continue;
            }
            let mut sorted = eligible;
            sorted.sort_by_key(|d| !d.is_external());
            let Ok(Some(d)) = pick_disk(theme, &sorted, "Qual dispositivo? (pendrives, cartões e discos externos primeiro)", false) else {
                return Flow::Continue;
            };
            let letters = d.volume_letters();
            let mut a = args(&["capacity", "--disk", &d.number.to_string()]);
            if letters.len() > 1 {
                let Ok(Some(i)) = Select::with_theme(theme).with_prompt("Qual volume?").items(&letters).default(0).interact_opt() else {
                    return Flow::Continue;
                };
                a.extend(args(&["--volume", &letters[i]]));
            }
            run_args(ctx, &a);
        }
        Action::Events => {
            let periods = [("Últimas 24 horas", "24"), ("Últimas 48 horas", "48"), ("Últimos 7 dias", "168"), ("Últimos 30 dias", "720")];
            let labels: Vec<&str> = periods.iter().map(|p| p.0).collect();
            let Ok(Some(i)) = Select::with_theme(theme).with_prompt("Período").items(&labels).default(2).interact_opt() else {
                return Flow::Continue;
            };
            run_args(ctx, &args(&["events", "--hours", periods[i].1]));
        }
        Action::History => {
            let options = ["Ver histórico e comparação antes × depois", "Registrar uma mudança (ex.: troquei o cabo SATA)"];
            let Ok(Some(i)) = Select::with_theme(theme).with_prompt("Histórico").items(options).default(0).interact_opt() else {
                return Flow::Continue;
            };
            if i == 0 {
                let Ok(d) = pick_disk(theme, &all, "Qual disco?", true) else { return Flow::Continue };
                let mut a = args(&["history"]);
                if let Some(d) = d {
                    a.extend(args(&["--disk", &d.number.to_string()]));
                }
                run_args(ctx, &a);
            } else {
                let Ok(Some(d)) = pick_disk(theme, &all, "Em qual disco foi a mudança?", false) else { return Flow::Continue };
                let text: String = match Input::with_theme(theme).with_prompt("O que mudou?").interact_text() {
                    Ok(t) => t,
                    Err(_) => return Flow::Continue,
                };
                if text.trim().is_empty() {
                    return Flow::Continue;
                }
                run_args(ctx, &args(&["history", "note", "--disk", &d.number.to_string(), text.trim()]));
            }
        }
        Action::Tags => {
            let Ok(Some(d)) = pick_disk(theme, &all, "Qual disco?", false) else { return Flow::Continue };
            let mut options = vec!["Adicionar tag".to_string()];
            options.extend(d.tags.iter().map(|t| format!("Remover a tag \"{t}\"")));
            let Ok(Some(i)) = Select::with_theme(theme).with_prompt("Tags").items(&options).default(0).interact_opt() else {
                return Flow::Continue;
            };
            if i == 0 {
                let tag: String = match Input::with_theme(theme).with_prompt("Nome da tag (ex.: Studio, Jogos)").interact_text() {
                    Ok(t) => t,
                    Err(_) => return Flow::Continue,
                };
                if tag.trim().is_empty() {
                    return Flow::Continue;
                }
                run_args(ctx, &args(&["tag", "add", "--disk", &d.number.to_string(), tag.trim()]));
            } else {
                run_args(ctx, &args(&["tag", "remove", "--disk", &d.number.to_string(), &d.tags[i - 1]]));
            }
        }
        Action::Report => {
            let dir = reports_dir();
            let file = dir.join(format!("diskdoctor-relatorio-{}.html", chrono::Local::now().format("%Y%m%d-%H%M%S")));
            let code = run_args(ctx, &args(&["report", "--format", "html", "-o", &file.display().to_string()]));
            if file.exists() && code != exit::ERROR {
                let open = Confirm::with_theme(theme).with_prompt("Abrir o relatório no navegador?").default(true).interact().unwrap_or(false);
                if open {
                    if let Err(e) = platform::open_path(&file) {
                        ui::error(&e);
                    }
                }
            }
        }
        Action::Quit => return Flow::Exit(exit::OK),
    }
    Flow::Continue
}
