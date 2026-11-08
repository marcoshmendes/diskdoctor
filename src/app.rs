//! Ponto de entrada: parse da CLI, despacho dos comandos, códigos de saída e Ctrl+C.

use crate::cli::{Cli, Command};
use crate::commands::{self, Ctx};
use crate::{platform, store, ui};
use clap::Parser;
use std::process::ExitCode;
use std::sync::Once;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

/// Códigos de saída (documentados no README e no AGENTS.md).
pub mod exit {
    /// Executado; nenhum problema de severidade alerta/crítico.
    pub const OK: u8 = 0;
    /// Erro de execução.
    pub const ERROR: u8 = 1;
    /// Uso incorreto da linha de comando.
    pub const USAGE: u8 = 2;
    /// A operação exige terminal de Administrador.
    pub const NEEDS_ADMIN: u8 = 3;
    /// Cancelado pelo usuário (Ctrl+C).
    pub const CANCELLED: u8 = 4;
    /// Executado, e foram encontrados problemas de severidade alerta ou crítica.
    pub const PROBLEMS_FOUND: u8 = 5;
    /// O usuário recusou a confirmação.
    pub const DECLINED: u8 = 6;
}

static CANCEL: AtomicBool = AtomicBool::new(false);
static CTRL_C_PRESSES: AtomicU32 = AtomicU32::new(0);
/// Há um teste cancelável em andamento (scan, benchmark, capacity)?
static OPERATION: AtomicBool = AtomicBool::new(false);
static HANDLER: Once = Once::new();

/// Flag de cancelamento compartilhada com os motores de teste.
pub fn cancel_flag() -> &'static AtomicBool {
    &CANCEL
}

fn restore_terminal() {
    let _ = console::Term::stdout().show_cursor();
    let _ = console::Term::stderr().show_cursor();
}

/// Instala (uma vez) o tratador de Ctrl+C:
/// - durante um teste: o 1º Ctrl+C pede cancelamento gracioso; o 2º encerra na hora;
/// - fora de teste (menus, coleta): encerra restaurando o cursor.
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
                eprintln!("\nCancelando… aguardando a operação atual terminar (Ctrl+C de novo encerra imediatamente).");
            } else {
                restore_terminal();
                std::process::exit(130);
            }
        });
    });
}

/// Marca o início de um teste cancelável; o cancelamento é zerado (importante no modo
/// interativo, que roda vários testes no mesmo processo).
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

/// Traduz as mensagens de erro mais comuns do clap para pt-BR.
pub fn translate_clap_error(text: &str) -> String {
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
        ("possible values:", "valores possíveis:"),
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

pub fn run() -> ExitCode {
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            use clap::error::ErrorKind;
            return match e.kind() {
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => {
                    let _ = e.print();
                    ExitCode::from(exit::OK)
                }
                _ => {
                    eprint!("{}", translate_clap_error(&e.render().to_string()));
                    ExitCode::from(exit::USAGE)
                }
            };
        }
    };
    ui::init_colors(cli.no_color);
    install_cancel_handler();
    let ctx = Ctx { json: cli.json, elevated: platform::is_elevated(), data_dir: store::default_data_dir() };
    let Some(command) = &cli.command else {
        // Sem comando: menu interativo num terminal (inclui duplo clique no .exe); ajuda em scripts.
        use std::io::IsTerminal;
        if !cli.json && std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
            return ExitCode::from(crate::interactive::run(&ctx));
        }
        use clap::CommandFactory;
        let _ = Cli::command().print_help();
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

/// Executa um comando já interpretado (usado pela CLI e pelo modo interativo).
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
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_common_clap_errors() {
        let t = translate_clap_error("error: unexpected argument '--foo' found\n\nUsage: diskdoctor\n\nFor more information, try '--help'.\n");
        assert!(t.starts_with("erro: argumento inesperado '--foo' encontrado"));
        assert!(t.contains("Uso: diskdoctor"));
        assert!(t.contains("Para mais informações"));
    }
}
