//! Interactive mode: `diskdoctor` with no arguments (or a double-click on the `.exe`).
//!
//! A keyboard menu ([`crate::menu`]). Each action builds the **equivalent command line**,
//! shows it ("Running: diskdoctor scan --disk 2 --read-only") and runs it through the same
//! path as the CLI ([`app::dispatch`]). Confirmations, backup warnings and safety checks
//! are therefore exactly the CLI's: the menu never skips a confirmation.
//!
//! The drive panel is cached for display only; every action that picks a drive reloads
//! the inventory first, so numbers and connections are always current.

use crate::app::{self, exit};
use crate::commands::{Ctx, apply_saved_tags, load_disks};
use crate::i18n::{self, Source, t};
use crate::inventory::DiskInfo;
use crate::menu::{self, Item, Row, Speed};
use crate::smart::Smartctl;
use crate::util::{format_bytes, format_duration};
use crate::{platform, ui};
use console::{Term, style};
use dialoguer::Input;
use dialoguer::theme::ColorfulTheme;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Action {
    Health,
    Report,
    Events,
    History,
    Scan,
    Benchmark,
    Capacity,
    Tags,
    Refresh,
    Language,
    Quit,
}

/// What happened after an action.
enum Flow {
    /// A command ran: wait for Enter before showing the menu again.
    Ran,
    /// Cancelled before running anything: back to the menu at once.
    Back,
    Exit(u8),
}

fn seconds() -> &'static str {
    t("seconds", "segundos")
}

fn minutes_to_hours() -> &'static str {
    t("minutes to hours", "minutos a horas")
}

/// Main menu in display order, built on every draw so it follows the language.
fn main_menu() -> (Vec<Row>, Vec<Action>) {
    let sections: Vec<(&str, Vec<(Action, Item)>)> = vec![
        (
            t("Diagnostics", "Diagnóstico"),
            vec![
                (
                    Action::Health,
                    Item::new(t("SMART & Drive Health", "SMART e saúde dos discos")).key('1').speed(Speed::Quick, "~30 s").hint(t(
                        "SMART attributes, Windows counters and events, correlated into a verdict per drive",
                        "Atributos SMART, contadores e eventos do Windows, correlacionados num veredito por disco",
                    )),
                ),
                (
                    Action::Report,
                    Item::new(t("Export Diagnostic Report", "Exportar relatório de diagnóstico")).key('2').speed(Speed::Quick, "~30 s–2 min").hint(t(
                        "Self-contained HTML report to open in the browser or send to a technician",
                        "Relatório HTML autocontido para abrir no navegador ou enviar a um técnico",
                    )),
                ),
                (
                    Action::Events,
                    Item::new(t("Storage Event Log", "Eventos de armazenamento")).key('3').speed(Speed::Quick, seconds()).hint(t(
                        "I/O errors, timeouts and device resets recorded by Windows",
                        "Erros de E/S, timeouts e resets de dispositivo registrados pelo Windows",
                    )),
                ),
                (
                    Action::History,
                    Item::new(t("History & Comparison", "Histórico e comparação")).key('4').speed(Speed::Quick, seconds()).hint(t(
                        "Record a change (cable, port, slot) and compare before × after",
                        "Registre uma mudança (cabo, porta, slot) e compare antes × depois",
                    )),
                ),
            ],
        ),
        (
            t("Tests", "Testes"),
            vec![
                (
                    Action::Scan,
                    Item::new(t("Surface Scan (read-only)", "Surface Scan (somente leitura)")).key('5').speed(Speed::Long, minutes_to_hours()).hint(t(
                        "Reads every sector: bad sectors, timeouts and latency · needs Administrator",
                        "Lê todos os setores: setores ruins, timeouts e latência · exige Administrador",
                    )),
                ),
                (
                    Action::Benchmark,
                    Item::new(t("Benchmark (read-only)", "Benchmark (somente leitura)")).key('6').speed(Speed::Quick, "~30 s–2 min").hint(t(
                        "Sequential Read at start/middle/end + Random 4 KiB, QD1 · needs Administrator",
                        "Sequential Read no início/meio/fim + Random 4 KiB, QD1 · exige Administrador",
                    )),
                ),
                (
                    Action::Capacity,
                    Item::new("Capacity Verification").key('7').speed(Speed::Long, minutes_to_hours()).hint(t(
                        "Writes and reads back new temporary files to catch counterfeit USB drives and cards",
                        "Grava e relê arquivos temporários novos para pegar pendrives e cartões falsificados",
                    )),
                ),
            ],
        ),
        (
            t("Settings", "Configurações"),
            vec![
                (
                    Action::Tags,
                    Item::new(t("Drive Tags", "Tags dos discos")).key('8').speed(Speed::Instant, "").hint(t(
                        "Name your drives (Studio, Games…) and use the names in commands",
                        "Dê nomes aos discos (Studio, Jogos…) e use os nomes nos comandos",
                    )),
                ),
                (
                    Action::Refresh,
                    Item::new(t("Refresh Drives", "Atualizar discos")).key('9').speed(Speed::Quick, seconds()).hint(t(
                        "Detect connected devices and free space again",
                        "Detecta de novo os dispositivos conectados e o espaço livre",
                    )),
                ),
                (
                    Action::Language,
                    Item::new(tr!("Language: {}", "Idioma: {}", i18n::current().native_name()))
                        .key('0')
                        .speed(Speed::Instant, "")
                        .hint("English · Português (Brasil)"),
                ),
                (Action::Quit, Item::new(t("Exit", "Sair")).key('q').speed(Speed::Instant, "").hint(t("Close diskdoctor", "Fecha o diskdoctor"))),
            ],
        ),
    ];
    let mut rows = Vec::new();
    let mut actions = Vec::new();
    for (title, entries) in sections {
        rows.push(Row::Header(title.to_string()));
        for (action, item) in entries {
            rows.push(Row::Item(item));
            actions.push(action);
        }
    }
    (rows, actions)
}

fn nth_item(rows: &[Row], n: usize) -> Option<&Item> {
    rows.iter()
        .filter_map(|r| match r {
            Row::Item(i) => Some(i),
            Row::Header(_) => None,
        })
        .nth(n)
}

fn width() -> usize {
    usize::from(Term::stdout().size().1).saturating_sub(1).max(1)
}

fn line(text: &str) {
    println!("{}", console::truncate_str(text, width(), "…"));
}

fn status_line(ctx: &Ctx, smartctl: bool) -> String {
    let admin = if ctx.elevated {
        format!("{} {}", style("●").green(), t("Administrator", "Administrador"))
    } else {
        format!("{} {}", style("●").yellow(), t("Standard user", "Usuário padrão"))
    };
    let smart = if smartctl {
        format!("{} smartctl", style("●").green())
    } else {
        format!("{} {}", style("●").yellow(), t("smartctl missing", "smartctl ausente"))
    };
    format!("{admin}   {smart}")
}

/// Full header (start, refresh, language or tag changes): logo, status and drive panel.
fn banner(ctx: &Ctx, smartctl: bool, disks: &[DiskInfo]) {
    let frame = |s: &str| style(s).cyan().to_string();
    println!();
    line(&format!("  {}  {}", frame("╭──╮"), style(format!("diskdoctor {}", crate::VERSION)).bold()));
    line(&format!(
        "  {}  {}",
        frame("│◉ │"),
        style(t(
            "Storage diagnostics · SMART · Surface Scan · Benchmark · Capacity Verification",
            "Diagnóstico de armazenamento · SMART · Surface Scan · Benchmark · Capacity Verification",
        ))
        .dim()
    ));
    line(&format!("  {}  {}", frame("╰──╯"), status_line(ctx, smartctl)));
    if !smartctl {
        line(&format!(
            "        {}",
            style(t("Install SMART support: winget install smartmontools.smartmontools", "Instale o suporte a SMART: winget install smartmontools.smartmontools")).dim()
        ));
    }
    ui::print_disk_overview(disks);
}

/// Compact header shown when coming back from an action.
fn compact_header(ctx: &Ctx, smartctl: bool) {
    println!();
    line(&format!("  {}  {}", style(format!("diskdoctor {}", crate::VERSION)).bold(), status_line(ctx, smartctl)));
}

pub fn run(ctx: &Ctx) -> u8 {
    platform::enable_virtual_terminal();
    let smartctl = Smartctl::locate().is_some();
    let mut disks = match load_disks(ctx) {
        Ok(d) => d,
        Err(e) => {
            ui::error(&format!("{e:#}"));
            menu::pause(t("Press Enter to exit", "Pressione Enter para sair"));
            return exit::ERROR;
        }
    };
    let mut full_header = true;
    let mut selected = 0;
    loop {
        if full_header {
            banner(ctx, smartctl, &disks);
        } else {
            compact_header(ctx, smartctl);
        }
        println!();
        let (rows, actions) = main_menu();
        let Some(i) = menu::select(None, &rows, selected, t("quit", "sair")) else {
            return exit::OK;
        };
        selected = i;
        let action = actions[i];
        match action {
            Action::Quit => return exit::OK,
            Action::Refresh => {
                match load_disks(ctx) {
                    Ok(d) => disks = d,
                    Err(e) => ui::error(&format!("{e:#}")),
                }
                full_header = true;
                continue;
            }
            _ => {}
        }
        if let Some(item) = nth_item(&rows, i) {
            section_title(item);
        }
        match handle(ctx, action, &mut disks) {
            Flow::Ran => menu::pause(t("Press Enter to return to the menu", "Pressione Enter para voltar ao menu")),
            Flow::Back => {}
            Flow::Exit(code) => return code,
        }
        if action == Action::Tags {
            apply_saved_tags(ctx, &mut disks);
        }
        // Tags and language change what the panel shows; otherwise keep the screen compact.
        full_header = matches!(action, Action::Tags | Action::Language);
    }
}

fn section_title(item: &Item) {
    let speed = match (item.speed, item.estimate.is_empty()) {
        (Some(s), false) => format!("{} · {}", s.label(), item.estimate),
        (Some(s), true) => s.label().to_string(),
        (None, _) => String::new(),
    };
    println!();
    line(&format!("  {} {}  {}", style("▌").cyan().bold(), style(&item.label).bold(), style(speed).dim()));
}

/// Runs a diskdoctor command line through the normal CLI path, showing it first.
fn run_args(ctx: &Ctx, args: &[String]) -> u8 {
    let shown: Vec<String> = args.iter().map(|a| if a.contains(' ') { format!("\"{a}\"") } else { a.clone() }).collect();
    println!(
        "  {} {}\n",
        style(t("Running:", "Executando:")).dim(),
        style(format!("diskdoctor {}", shown.join(" "))).cyan()
    );
    let argv = std::iter::once("diskdoctor".to_string()).chain(args.iter().cloned());
    match app::parse_cli(argv) {
        Ok(crate::cli::Cli { command: Some(cmd), .. }) => match app::dispatch(ctx, &cmd) {
            Ok(code) => code,
            Err(e) => {
                ui::error(&format!("{e:#}"));
                exit::ERROR
            }
        },
        Ok(_) => exit::USAGE,
        Err(e) => {
            app::print_clap_error(&e);
            exit::USAGE
        }
    }
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

fn drive_item(d: &DiskInfo) -> Item {
    let letters = d.volume_letters();
    let mut label = format!("{} · {} · {}", d.title(), d.class_label(), format_bytes(d.size_bytes));
    if !letters.is_empty() {
        label.push_str(&format!(" · {}", letters.join(" ")));
    }
    if d.is_boot || d.is_system {
        label.push_str(t(" (system)", " (sistema)"));
    }
    let mut item = Item::new(label).hint(tr!(
        "Serial {} · Windows status: {}",
        "Serial {} · estado no Windows: {}",
        if d.serial_masked.is_empty() { "—" } else { &d.serial_masked },
        ui::windows_health_label(&d.windows_health)
    ));
    // Drive numbers 0–9 double as shortcuts: press 2 for Drive 2.
    if d.number < 10 {
        item = item.key(char::from(b'0' + d.number as u8));
    }
    item
}

fn estimate_item(item: Item, estimate: Duration) -> Item {
    let speed = if estimate > Duration::from_secs(15 * 60) { Speed::Long } else { Speed::Quick };
    item.speed(speed, format!("~{}", format_duration(estimate)))
}

/// Drive picker. `Ok(None)` = "all drives" (when allowed); `Err(())` = back.
fn pick_disk<'a>(disks: &[&'a DiskInfo], prompt: &str, allow_all: bool, item: impl Fn(&DiskInfo) -> Item) -> Result<Option<&'a DiskInfo>, ()> {
    let mut rows = Vec::new();
    if allow_all {
        rows.push(Row::Item(Item::new(t("All drives", "Todos os discos")).key('a').hint(t("Analyze every connected drive", "Analisa todos os discos conectados"))));
    }
    rows.extend(disks.iter().map(|d| Row::Item(item(d))));
    let i = menu::select(Some(prompt), &rows, 0, t("back", "voltar")).ok_or(())?;
    if allow_all {
        Ok(if i == 0 { None } else { Some(disks[i - 1]) })
    } else {
        Ok(Some(disks[i]))
    }
}

/// Direct-read tests need Administrator: offers to reopen elevated.
fn ensure_admin(ctx: &Ctx, what: &str) -> Option<Flow> {
    if ctx.elevated {
        return None;
    }
    println!(
        "  {}",
        tr!(
            "{what} reads the drive directly and requires Administrator access.",
            "{what} lê o disco diretamente e exige acesso de Administrador."
        )
    );
    let reopen = menu::confirm(
        t("Restart diskdoctor as Administrator? Windows will ask for permission", "Reabrir o diskdoctor como Administrador? O Windows vai pedir permissão"),
        true,
    );
    if !reopen {
        return Some(Flow::Back);
    }
    // A language picked only with --lang must survive the relaunch.
    let relaunch_args = if i18n::source() == Source::Flag { format!("--lang {}", i18n::current().code()) } else { String::new() };
    match platform::relaunch_as_admin(&relaunch_args) {
        Ok(()) => {
            println!("  {}", t("Opening a new window as Administrator…", "Abrindo uma nova janela como Administrador…"));
            Some(Flow::Exit(exit::OK))
        }
        Err(e) => {
            ui::error(&e);
            Some(Flow::Ran)
        }
    }
}

fn reports_dir() -> PathBuf {
    std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join("Documents").join("diskdoctor")).unwrap_or_else(|| PathBuf::from("."))
}

fn ask_text(prompt: &str) -> Option<String> {
    let text: String = Input::with_theme(&ColorfulTheme::default()).with_prompt(prompt).allow_empty(true).interact_text().ok()?;
    let text = text.trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn reload(ctx: &Ctx, disks: &mut Vec<DiskInfo>) -> bool {
    // The cache is for display only: selection must reflect current connections and numbers.
    match load_disks(ctx) {
        Ok(d) => {
            *disks = d;
            true
        }
        Err(e) => {
            ui::error(&format!("{e:#}"));
            false
        }
    }
}

fn handle(ctx: &Ctx, action: Action, disks: &mut Vec<DiskInfo>) -> Flow {
    match action {
        Action::Health => {
            if !reload(ctx, disks) {
                return Flow::Ran;
            }
            let all: Vec<&DiskInfo> = disks.iter().collect();
            let Ok(d) = pick_disk(&all, t("Select drive", "Escolha o disco"), true, drive_item) else {
                return Flow::Back;
            };
            let mut a = args(&["health"]);
            if let Some(d) = d {
                a.extend(args(&["--disk", &d.number.to_string()]));
            }
            run_args(ctx, &a);
        }
        Action::Report => {
            let file = reports_dir().join(format!("diskdoctor-report-{}.html", chrono::Local::now().format("%Y%m%d-%H%M%S")));
            let code = run_args(ctx, &args(&["report", "--format", "html", "-o", &file.display().to_string()]));
            if file.exists() && code != exit::ERROR && menu::confirm(t("Open the report in the browser?", "Abrir o relatório no navegador?"), true) {
                if let Err(e) = platform::open_path(&file) {
                    ui::error(&e);
                }
            }
        }
        Action::Events => {
            let periods = [
                (t("Last 24 hours", "Últimas 24 horas"), "24"),
                (t("Last 48 hours", "Últimas 48 horas"), "48"),
                (t("Last 7 days", "Últimos 7 dias"), "168"),
                (t("Last 30 days", "Últimos 30 dias"), "720"),
            ];
            let rows: Vec<Row> = periods
                .iter()
                .enumerate()
                .map(|(i, (label, _))| Row::Item(Item::new(*label).key(char::from(b'1' + i as u8)).speed(Speed::Quick, seconds())))
                .collect();
            let Some(i) = menu::select(Some(t("Time range", "Período")), &rows, 2, t("back", "voltar")) else {
                return Flow::Back;
            };
            run_args(ctx, &args(&["events", "--hours", periods[i].1]));
        }
        Action::History => {
            let rows = vec![
                Row::Item(Item::new(t("View timeline and before × after comparison", "Ver linha do tempo e comparação antes × depois")).key('1').speed(Speed::Quick, seconds())),
                Row::Item(Item::new(t("Record a change (e.g. replaced the SATA cable)", "Registrar uma mudança (ex.: troquei o cabo SATA)")).key('2').speed(Speed::Instant, "")),
            ];
            let Some(choice) = menu::select(Some(t("History", "Histórico")), &rows, 0, t("back", "voltar")) else {
                return Flow::Back;
            };
            if !reload(ctx, disks) {
                return Flow::Ran;
            }
            let all: Vec<&DiskInfo> = disks.iter().collect();
            if choice == 0 {
                let Ok(d) = pick_disk(&all, t("Select drive", "Escolha o disco"), true, drive_item) else {
                    return Flow::Back;
                };
                let mut a = args(&["history"]);
                if let Some(d) = d {
                    a.extend(args(&["--disk", &d.number.to_string()]));
                }
                run_args(ctx, &a);
            } else {
                let Ok(Some(d)) = pick_disk(&all, t("Which drive changed?", "Qual disco mudou?"), false, drive_item) else {
                    return Flow::Back;
                };
                let Some(text) = ask_text(t("Describe the change (empty to cancel)", "Descreva a mudança (vazio para cancelar)")) else {
                    return Flow::Back;
                };
                run_args(ctx, &args(&["history", "note", "--disk", &d.number.to_string(), &text]));
            }
        }
        Action::Scan | Action::Benchmark => {
            let what = if action == Action::Scan { "Surface Scan" } else { "Benchmark" };
            if let Some(flow) = ensure_admin(ctx, what) {
                return flow;
            }
            if action == Action::Scan {
                ui::note(t(
                    "  Reads the whole drive and measures bad sectors, timeouts and latency. Nothing is written.",
                    "  Lê o disco inteiro e mede setores ruins, timeouts e latência. Nada é gravado.",
                ));
                ui::note(t(
                    "  Test one drive at a time and avoid heavy workloads while it runs.",
                    "  Teste um disco por vez e evite uso pesado durante o teste.",
                ));
            } else {
                ui::note(t(
                    "  Sequential Read at the start, middle and end; Random 4 KiB at queue depth 1 (QD1).",
                    "  Sequential Read no início, meio e fim; Random 4 KiB com fila de profundidade 1 (QD1).",
                ));
                ui::note(t("  Reports MB/s, IOPS and latency. No writes in this mode.", "  Mostra MB/s, IOPS e latência. Nada é gravado neste modo."));
            }
            println!();
            if !reload(ctx, disks) {
                return Flow::Ran;
            }
            let all: Vec<&DiskInfo> = disks.iter().collect();
            let picked = if action == Action::Scan {
                pick_disk(&all, t("Select drive", "Escolha o disco"), false, |d| estimate_item(drive_item(d), ui::estimate_scan_time(d, d.size_bytes)))
            } else {
                pick_disk(&all, t("Select drive", "Escolha o disco"), false, drive_item)
            };
            let Ok(Some(d)) = picked else {
                return Flow::Back;
            };
            let cmd = if action == Action::Scan { "scan" } else { "benchmark" };
            run_args(ctx, &args(&[cmd, "--disk", &d.number.to_string(), "--read-only"]));
        }
        Action::Capacity => {
            if !reload(ctx, disks) {
                return Flow::Ran;
            }
            let mut eligible: Vec<&DiskInfo> =
                disks.iter().filter(|d| !d.is_boot && !d.is_system && d.partitions.iter().any(|p| p.drive_letter.is_some())).collect();
            if eligible.is_empty() {
                println!(
                    "  {}",
                    t(
                        "No eligible device. Connect a drive with a mounted volume; the system drive cannot be tested.",
                        "Nenhum dispositivo elegível. Conecte um disco com volume montado; o disco do sistema não pode ser testado.",
                    )
                );
                return Flow::Ran;
            }
            eligible.sort_by_key(|d| !d.is_external());
            ui::note(t(
                "  Writes new temporary files to the free space, reads them back and deletes them.",
                "  Grava arquivos temporários novos no espaço livre, lê de volta e apaga no fim.",
            ));
            println!();
            let free = |d: &DiskInfo| d.partitions.iter().filter(|p| p.drive_letter.is_some()).filter_map(|p| p.volume_free).sum::<u64>();
            let Ok(Some(d)) = pick_disk(&eligible, t("Select device (external first)", "Escolha o dispositivo (externos primeiro)"), false, |d| {
                estimate_item(drive_item(d), ui::estimate_scan_time(d, free(d)) * 3)
            }) else {
                return Flow::Back;
            };
            let letters = d.volume_letters();
            let mut a = args(&["capacity", "--disk", &d.number.to_string()]);
            if letters.len() > 1 {
                let rows: Vec<Row> = d
                    .partitions
                    .iter()
                    .filter_map(|p| {
                        let l = p.drive_letter.as_ref()?;
                        let free = p.volume_free.map(|f| tr!(" · {} free", " · {} livres", format_bytes(f))).unwrap_or_default();
                        Some(Row::Item(Item::new(format!("{l}: {}{free}", p.label.as_deref().unwrap_or(""))).key(l.chars().next().unwrap_or(' ').to_ascii_lowercase())))
                    })
                    .collect();
                let Some(i) = menu::select(Some(t("Select volume", "Escolha o volume")), &rows, 0, t("back", "voltar")) else {
                    return Flow::Back;
                };
                a.extend(args(&["--volume", &letters[i]]));
            }
            run_args(ctx, &a);
        }
        Action::Tags => {
            if !reload(ctx, disks) {
                return Flow::Ran;
            }
            let all: Vec<&DiskInfo> = disks.iter().collect();
            let Ok(Some(d)) = pick_disk(&all, t("Select drive", "Escolha o disco"), false, drive_item) else {
                return Flow::Back;
            };
            let mut rows = vec![Row::Item(Item::new(t("Add tag", "Adicionar tag")).key('+'))];
            rows.extend(d.tags.iter().map(|tag| Row::Item(Item::new(tr!("Remove tag \"{tag}\"", "Remover tag \"{tag}\"")))));
            let Some(i) = menu::select(Some(t("Tags", "Tags")), &rows, 0, t("back", "voltar")) else {
                return Flow::Back;
            };
            if i == 0 {
                let Some(tag) = ask_text(t("Tag name, e.g. Studio or Games (empty to cancel)", "Nome da tag, ex.: Studio ou Jogos (vazio para cancelar)")) else {
                    return Flow::Back;
                };
                run_args(ctx, &args(&["tag", "add", "--disk", &d.number.to_string(), &tag]));
            } else {
                run_args(ctx, &args(&["tag", "remove", "--disk", &d.number.to_string(), &d.tags[i - 1]]));
            }
        }
        Action::Language => {
            let current = i18n::current();
            let rows: Vec<Row> = i18n::ALL
                .iter()
                .enumerate()
                .map(|(i, l)| {
                    let label = if *l == current { format!("{} ✔", l.native_name()) } else { l.native_name().to_string() };
                    Row::Item(Item::new(label).key(char::from(b'1' + i as u8)).hint(l.code()))
                })
                .collect();
            let default = i18n::ALL.iter().position(|l| *l == current).unwrap_or(0);
            let Some(i) = menu::select(Some("Language / Idioma"), &rows, default, t("back", "voltar")) else {
                return Flow::Back;
            };
            if i18n::ALL[i] == current {
                return Flow::Back;
            }
            run_args(ctx, &args(&["language", i18n::ALL[i].code()]));
            return Flow::Back;
        }
        Action::Refresh | Action::Quit => return Flow::Back,
    }
    Flow::Ran
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{Lang, scoped};

    #[test]
    fn every_main_menu_item_has_a_duration_and_a_shortcut_in_both_languages() {
        for lang in i18n::ALL {
            let _l = scoped(lang);
            let (rows, actions) = main_menu();
            let items: Vec<&Item> = rows
                .iter()
                .filter_map(|r| match r {
                    Row::Item(i) => Some(i),
                    Row::Header(_) => None,
                })
                .collect();
            assert_eq!(items.len(), actions.len());
            assert!(items.iter().all(|i| i.speed.is_some() && i.key.is_some()), "{lang:?}");
            let keys: std::collections::BTreeSet<char> = items.iter().filter_map(|i| i.key).collect();
            assert_eq!(keys.len(), items.len(), "shortcuts are unique");
        }
    }

    #[test]
    fn technical_names_stay_in_english() {
        let _pt = scoped(Lang::PtBr);
        let (rows, _) = main_menu();
        let labels: Vec<String> = rows
            .iter()
            .filter_map(|r| match r {
                Row::Item(i) => Some(format!("{} {}", i.label, i.hint)),
                Row::Header(_) => None,
            })
            .collect();
        let all = labels.join("\n");
        for term in ["SMART", "Surface Scan", "Benchmark", "Capacity Verification", "Sequential Read", "Random 4 KiB", "QD1"] {
            assert!(all.contains(term), "{term} missing in:\n{all}");
        }
        assert!(all.contains("Idioma: Português (Brasil)"));
    }

    #[test]
    fn main_menu_fits_narrow_and_wide_terminals() {
        for lang in i18n::ALL {
            let _l = scoped(lang);
            let (rows, _) = main_menu();
            for width in [24, 40, 60, 80, 100, 140] {
                let frame = menu::render(None, &rows, 0, 0, width, 40, "footer");
                for l in &frame.lines {
                    assert!(console::measure_text_width(l) < width, "{lang:?} width {width}: {l:?}");
                }
                let text = console::strip_ansi_codes(&frame.lines.join("\n")).into_owned();
                let speed = Speed::Quick.label();
                assert!(text.contains(speed), "{lang:?} width {width}: duration class visible\n{text}");
            }
        }
    }
}
