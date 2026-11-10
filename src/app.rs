//! Entry point: language selection, CLI parsing, command dispatch, exit codes and Ctrl+C.

use crate::cli::{Cli, Command};
use crate::commands::{self, Ctx};
use crate::i18n::{self, t};
use crate::{platform, store, ui};
use clap::{CommandFactory, FromArgMatches};
use std::ffi::OsString;
use std::io::IsTerminal;
use std::process::ExitCode;
use std::sync::Once;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// Exit codes (documented in README and AGENTS.md).
pub mod exit {
    /// Ran; no problem of warning or critical severity.
    pub const OK: u8 = 0;
    /// Runtime error.
    pub const ERROR: u8 = 1;
    /// Command-line usage error.
    pub const USAGE: u8 = 2;
    /// The operation requires an Administrator terminal.
    pub const NEEDS_ADMIN: u8 = 3;
    /// Cancelled by the user (Ctrl+C).
    pub const CANCELLED: u8 = 4;
    /// Ran, and problems of warning or critical severity were found.
    pub const PROBLEMS_FOUND: u8 = 5;
    /// The user declined the confirmation.
    pub const DECLINED: u8 = 6;
}

static CANCEL: AtomicBool = AtomicBool::new(false);
static CTRL_C_PRESSES: AtomicU32 = AtomicU32::new(0);
/// Is a cancellable test running (scan, benchmark, capacity)?
static OPERATION: AtomicBool = AtomicBool::new(false);
static HANDLER: Once = Once::new();

/// Cancellation flag shared with the test engines.
pub fn cancel_flag() -> &'static AtomicBool {
    &CANCEL
}

/// Shows the cursor again (menus hide it while drawing).
pub fn restore_terminal() {
    let _ = console::Term::stdout().show_cursor();
    let _ = console::Term::stderr().show_cursor();
}

/// Installs the Ctrl+C handler (once):
/// - during a test: the 1st Ctrl+C requests a graceful cancel; the 2nd exits immediately;
/// - outside a test (menus, collection): exits restoring the cursor.
pub fn install_cancel_handler() {
    HANDLER.call_once(|| {
        let _ = ctrlc::set_handler(|| {
            if !OPERATION.load(Ordering::SeqCst) {
                restore_terminal();
                eprintln!();
                std::process::exit(130);
            }
            let n = CTRL_C_PRESSES.fetch_add(1, Ordering::SeqCst);
            CANCEL.store(true, Ordering::SeqCst);
            if n == 0 {
                eprintln!(
                    "\n{}",
                    t(
                        "Cancelling… waiting for the current operation to finish (press Ctrl+C again to exit immediately).",
                        "Cancelando… aguardando a operação atual terminar (Ctrl+C de novo encerra imediatamente)."
                    )
                );
            } else {
                restore_terminal();
                std::process::exit(130);
            }
        });
    });
}

/// Marks the start of a cancellable test and resets cancellation (important in the
/// interactive mode, which runs several tests in the same process).
pub fn begin_operation() -> OperationGuard {
    install_cancel_handler();
    CANCEL.store(false, Ordering::SeqCst);
    CTRL_C_PRESSES.store(0, Ordering::SeqCst);
    OPERATION.store(true, Ordering::SeqCst);
    OperationGuard
}

pub struct OperationGuard;

impl Drop for OperationGuard {
    fn drop(&mut self) {
        OPERATION.store(false, Ordering::SeqCst);
    }
}

/// clap writes its own messages in English. In pt-BR, the most common ones are translated.
pub fn localize_clap_text(text: &str) -> String {
    if !i18n::is_pt() {
        return text.to_string();
    }
    const PAIRS: &[(&str, &str)] = &[
        ("error:", "erro:"),
        ("Usage:", "Uso:"),
        ("For more information, try '--help'.", "Para mais informações, use '--help'."),
        ("unexpected argument", "argumento inesperado"),
        ("unrecognized subcommand", "comando desconhecido"),
        ("invalid value", "valor inválido"),
        ("for '", "para '"),
        ("the following required arguments were not provided:", "os seguintes argumentos obrigatórios não foram informados:"),
        ("the argument", "o argumento"),
        ("cannot be used with", "não pode ser usado com"),
        ("a value is required for", "é necessário um valor para"),
        ("but none was supplied", "mas nenhum foi informado"),
        ("[possible values:", "[valores possíveis:"),
        ("possible values:", "valores possíveis:"),
        ("[default:", "[padrão:"),
        ("[aliases:", "[apelidos:"),
        ("tip:", "dica:"),
        ("to pass", "para passar"),
        ("as a value, use", "como valor, use"),
        ("invalid digit found in string", "dígito inválido"),
        ("found", "encontrado"),
    ];
    let mut out = text.to_string();
    for (en, pt) in PAIRS {
        out = out.replace(en, pt);
    }
    out
}

/// Prints a clap error (or help/version) in the active language.
pub fn print_clap_error(e: &clap::Error) {
    use clap::error::ErrorKind;
    let to_stdout = matches!(e.kind(), ErrorKind::DisplayHelp | ErrorKind::DisplayVersion);
    if !i18n::is_pt() {
        let _ = e.print();
        return;
    }
    let colored = console::colors_enabled() && if to_stdout { std::io::stdout().is_terminal() } else { std::io::stderr().is_terminal() };
    let rendered = e.render();
    let text = if colored { rendered.ansi().to_string() } else { rendered.to_string() };
    if to_stdout {
        print!("{}", localize_clap_text(&text));
    } else {
        eprint!("{}", localize_clap_text(&text));
    }
}

/// Parses `args` with the localized command definition.
pub fn parse_cli<I, T>(args: I) -> Result<Cli, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let matches = Cli::command().try_get_matches_from(args)?;
    Cli::from_arg_matches(&matches)
}

pub fn run() -> ExitCode {
    let argv: Vec<OsString> = std::env::args_os().collect();
    let data_dir = store::default_data_dir();
    // The language must be known before clap builds the (localized) help.
    let (lang, source) = i18n::resolve(i18n::flag_value(&argv).as_deref(), std::env::var("DISKDOCTOR_LANG").ok().as_deref(), &data_dir);
    i18n::set(lang, source);
    if argv.iter().any(|a| a == "--no-color") {
        ui::init_colors(true);
    }

    let cli = match parse_cli(&argv) {
        Ok(c) => c,
        Err(e) => {
            use clap::error::ErrorKind;
            print_clap_error(&e);
            return ExitCode::from(match e.kind() {
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => exit::OK,
                _ => exit::USAGE,
            });
        }
    };
    ui::init_colors(cli.no_color);
    install_cancel_handler();
    let ctx = Ctx { json: cli.json, elevated: platform::is_elevated(), data_dir };
    let Some(command) = &cli.command else {
        // No command: interactive menu in a terminal (includes double-clicking the .exe);
        // help in scripts.
        if !cli.json && std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
            return ExitCode::from(crate::interactive::run(&ctx));
        }
        print!("{}", localize_clap_text(&Cli::command().render_help().to_string()));
        return ExitCode::from(exit::USAGE);
    };
    match dispatch(&ctx, command) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            ui::error(&format!("{e:#}"));
            ExitCode::from(exit::ERROR)
        }
    }
}

/// Runs an already parsed command (used by the CLI and by the interactive mode).
pub fn dispatch(ctx: &Ctx, command: &Command) -> anyhow::Result<u8> {
    match command {
        Command::List => commands::list::run(ctx),
        Command::Health(a) => commands::health::run(ctx, a),
        Command::Events(a) => commands::events::run(ctx, a),
        Command::Scan(a) => commands::scan::run(ctx, a),
        Command::Benchmark(a) => commands::benchmark::run(ctx, a),
        Command::Report(a) => commands::report::run(ctx, a),
        Command::Tag(a) => commands::tag::run(ctx, a),
        Command::Capacity(a) => commands::capacity::run(ctx, a),
        Command::History(a) => commands::history::run(ctx, a),
        Command::Language(a) => commands::language::run(ctx, a),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{Lang, scoped};

    #[test]
    fn translates_common_clap_errors_only_in_portuguese() {
        let raw = "error: unexpected argument '--foo' found\n\nUsage: diskdoctor\n\nFor more information, try '--help'.\n";
        {
            let _en = scoped(Lang::En);
            assert_eq!(localize_clap_text(raw), raw);
        }
        let _pt = scoped(Lang::PtBr);
        let t = localize_clap_text(raw);
        assert!(t.starts_with("erro: argumento inesperado '--foo' encontrado"));
        assert!(t.contains("Uso: diskdoctor"));
        assert!(t.contains("Para mais informações"));
        assert!(localize_clap_text("  [default: 168]").contains("[padrão: 168]"));
    }

    #[test]
    fn parse_cli_uses_localized_definition() {
        let _pt = scoped(Lang::PtBr);
        let err = parse_cli(["diskdoctor", "scan"]).unwrap_err();
        assert!(err.render().to_string().contains("--disk"));
    }
}
