//! Command-line definition (clap). Help text is localized with [`t`]: the language is
//! resolved before parsing (see `app::run`), so `--help` already follows `--lang`.

use crate::i18n::{self, Lang, t};
use crate::report::ReportFormat;
use crate::tags::DiskRef;
use clap::{ArgAction, Args, Parser, Subcommand};
use std::path::PathBuf;

fn root_template() -> &'static str {
    t(
        "{before-help}{name} {version} — {about}\n\nUsage: {usage}\n\n{all-args}{after-help}",
        "{before-help}{name} {version} — {about}\n\nUso: {usage}\n\n{all-args}{after-help}",
    )
}

fn sub_template() -> &'static str {
    t("{about}\n\nUsage: {usage}\n\n{all-args}{after-help}", "{about}\n\nUso: {usage}\n\n{all-args}{after-help}")
}

fn options() -> &'static str {
    t("Options", "Opções")
}

fn actions() -> &'static str {
    t("Actions", "Ações")
}

#[derive(Parser, Debug)]
#[command(
    name = "diskdoctor",
    version,
    about = t(
        "Non-destructive diagnostics for HDDs, SSDs, USB flash drives and memory cards on Windows",
        "Diagnóstico não destrutivo de HDs, SSDs, pendrives e cartões de memória no Windows"
    ),
    disable_help_flag = true,
    disable_version_flag = true,
    disable_help_subcommand = true,
    help_template = root_template(),
    override_usage = t(
        "diskdoctor [COMMAND] [options]      (no command: interactive menu)",
        "diskdoctor [COMANDO] [opções]      (sem comando: menu interativo)"
    ),
    subcommand_help_heading = t("Commands", "Comandos"),
    subcommand_value_name = t("COMMAND", "COMANDO"),
    next_help_heading = t("Global options", "Opções globais"),
    after_help = t(
        "Examples:\n  diskdoctor list\n  diskdoctor tag add --disk 2 Studio\n  diskdoctor health --disk studio\n  diskdoctor events --hours 48\n  diskdoctor scan --disk 1 --read-only\n  diskdoctor benchmark --disk 1 --read-only\n  diskdoctor report --format html\n  diskdoctor language pt-BR\n\nNo command writes to the drives being analyzed. Use an Administrator terminal\nfor full SMART data, Surface Scan and Benchmark.",
        "Exemplos:\n  diskdoctor list\n  diskdoctor tag add --disk 2 Studio\n  diskdoctor health --disk studio\n  diskdoctor events --hours 48\n  diskdoctor scan --disk 1 --read-only\n  diskdoctor benchmark --disk 1 --read-only\n  diskdoctor report --format html\n  diskdoctor language en\n\nNenhum comando grava nos discos analisados. Use um terminal de Administrador\npara SMART completo, Surface Scan e Benchmark."
    )
)]
pub struct Cli {
    /// No command opens the interactive menu.
    #[command(subcommand)]
    pub command: Option<Command>,

    #[arg(long, global = true, help = t("Print results as JSON (for scripts and other agents)", "Imprime o resultado em JSON (para scripts e outros agentes)"))]
    pub json: bool,

    #[arg(long, global = true, help = t("Disable terminal colors", "Desativa cores no terminal"))]
    pub no_color: bool,

    #[arg(
        long,
        global = true,
        value_name = "en|pt-BR",
        value_parser = i18n::parse_arg,
        help = t("Interface language for this run: en (English) or pt-BR (Português)", "Idioma da interface nesta execução: en (English) ou pt-BR (Português)")
    )]
    pub lang: Option<Lang>,

    #[arg(short = 'h', long = "help", action = ArgAction::Help, global = true, help = t("Show this help", "Mostra esta ajuda"))]
    help: Option<bool>,

    #[arg(short = 'V', long = "version", action = ArgAction::Version, help = t("Show the version", "Mostra a versão"))]
    version: Option<bool>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    #[command(
        about = t(
            "List physical drives: model, masked serial, capacity, interface and volumes",
            "Lista os discos físicos: modelo, serial mascarado, capacidade, interface e volumes"
        ),
        help_template = sub_template(),
        override_usage = "diskdoctor list [--json]"
    )]
    List,

    #[command(
        about = t(
            "SMART & Drive Health: SMART, Windows counters, events and a correlated diagnosis",
            "SMART e saúde dos discos: SMART, contadores do Windows, eventos e diagnóstico correlacionado"
        ),
        help_template = sub_template(),
        override_usage = "diskdoctor health [--disk <N|TAG>] [--hours <HOURS>] [options]"
    )]
    Health(HealthArgs),

    #[command(
        about = t(
            "Storage events from the Windows System log (IDs 7, 51, 129, 153 and others)",
            "Eventos de armazenamento do log System do Windows (IDs 7, 51, 129, 153 e outros)"
        ),
        help_template = sub_template(),
        override_usage = "diskdoctor events [--hours <HOURS>] [--disk <N|TAG>] [options]"
    )]
    Events(EventsArgs),

    #[command(
        about = t(
            "Surface Scan: sequential read of the whole drive (read-only; requires Administrator)",
            "Surface Scan: leitura sequencial do disco inteiro (somente leitura; exige Administrador)"
        ),
        help_template = sub_template(),
        override_usage = "diskdoctor scan --disk <N|TAG> [--read-only] [options]",
        after_help = t(
            "The scan never writes to the drive: Windows opens the device for reading only.\nCtrl+C cancels at any time and the partial result is saved.",
            "O teste nunca grava no disco: o Windows abre o dispositivo apenas para leitura.\nCtrl+C cancela a qualquer momento e o resultado parcial é salvo."
        )
    )]
    Scan(ScanArgs),

    #[command(
        about = t(
            "Benchmark: Sequential Read and Random 4 KiB at QD1; writes only to a temporary file after typed confirmation",
            "Benchmark: Sequential Read e Random 4 KiB em QD1; grava só em arquivo temporário, com confirmação digitada"
        ),
        help_template = sub_template(),
        override_usage = "diskdoctor benchmark --disk <N|TAG> [--read-only | --write-test <DIR>] [options]"
    )]
    Benchmark(BenchmarkArgs),

    #[command(
        about = t(
            "Capacity Verification: checks that the advertised capacity is real (counterfeit USB drives and cards) by writing and reading back temporary files",
            "Capacity Verification: confere se a capacidade anunciada é real (pendrive/cartão falsificado), gravando e lendo de volta arquivos temporários"
        ),
        help_template = sub_template(),
        override_usage = "diskdoctor capacity (--disk <N|TAG> | --volume <LETTER>) [options]",
        after_help = t(
            "Works like f3write/f3read: creates NEW files in the free space, each block tagged with its own\n\
             position, reads everything back without cache and deletes the files at the end. Existing files\n\
             are never opened or changed. On a FAKE device, however, writing past the real capacity can\n\
             corrupt data already on it: back up first and, ideally, test an empty device.\n\
             You must type WRITE to confirm. The system drive is refused.",
            "Como o f3write/f3read: cria arquivos NOVOS no espaço livre, cada bloco marcado com a própria\n\
             posição, lê tudo de volta sem cache e apaga os arquivos no fim. Arquivos existentes não são\n\
             abertos nem alterados. Num dispositivo FALSO, porém, gravar além da capacidade real pode\n\
             corromper o que já está nele: faça backup e, de preferência, teste com o dispositivo vazio.\n\
             É preciso digitar WRITE para confirmar. O disco do sistema é recusado."
        )
    )]
    Capacity(CapacityArgs),

    #[command(
        about = t(
            "History & Comparison: timeline of each drive and before × after a change (e.g. a new cable)",
            "Histórico e comparação: linha do tempo de cada disco e antes × depois de uma mudança (ex.: troca de cabo)"
        ),
        help_template = sub_template(),
        override_usage = "diskdoctor history [--disk <N|TAG>] [--limit <N>]\n       diskdoctor history note --disk <N|TAG> <TEXT>",
        args_conflicts_with_subcommands = true,
        disable_help_subcommand = true,
        subcommand_help_heading = actions(),
        subcommand_value_name = t("ACTION", "AÇÃO"),
        after_help = t(
            "Example: was it the cable?\n  1. diskdoctor health                                   (records the current state)\n  2. replace the SATA cable and record it:\n     diskdoctor history note --disk 2 \"Replaced the SATA cable\"\n  3. use the PC normally for 1–3 days and run diskdoctor health again\n  4. diskdoctor history --disk 2                          (compares before × after)\n\nAfter a change is recorded, the drive verdict only considers what happened after it.",
            "Exemplo — descobrir se o problema era o cabo:\n  1. diskdoctor health                                   (registra o estado atual)\n  2. troque o cabo SATA e registre:\n     diskdoctor history note --disk 2 \"Troquei o cabo SATA\"\n  3. use o PC normalmente por 1–3 dias e rode diskdoctor health de novo\n  4. diskdoctor history --disk 2                          (compara antes × depois)\n\nDepois de registrar uma mudança, o veredito do disco considera só o que aconteceu depois dela."
        )
    )]
    History(HistoryArgs),

    #[command(
        about = t(
            "Export Diagnostic Report: HTML and/or JSON correlating every source",
            "Exportar relatório de diagnóstico: HTML e/ou JSON correlacionando todas as fontes"
        ),
        help_template = sub_template(),
        override_usage = "diskdoctor report [--format html|json|both] [--output <FILE>] [options]"
    )]
    Report(ReportArgs),

    #[command(
        about = t(
            "Drive Tags: nicknames such as \"Studio\" or \"Games\"; tags work in --disk",
            "Tags dos discos: apelidos como \"Studio\" ou \"Jogos\"; as tags valem em --disk"
        ),
        help_template = sub_template(),
        override_usage = "diskdoctor tag [list]\n       diskdoctor tag add --disk <N|TAG> <TAG>...\n       diskdoctor tag remove --disk <N|TAG> (<TAG>... | --all)",
        disable_help_subcommand = true,
        subcommand_help_heading = actions(),
        subcommand_value_name = t("ACTION", "AÇÃO"),
        after_help = t(
            "Examples:\n  diskdoctor tag add --disk 2 Studio\n  diskdoctor tag add --disk 0 Games \"Steam Library\"\n  diskdoctor tag\n  diskdoctor health --disk studio\n  diskdoctor tag remove --disk 2 Studio\n\nTags are stored in %LOCALAPPDATA%\\diskdoctor\\tags.json, bound to the drive model + serial:\nthey keep working if the drive number changes. Case does not matter.",
            "Exemplos:\n  diskdoctor tag add --disk 2 Studio\n  diskdoctor tag add --disk 0 Jogos \"Biblioteca Steam\"\n  diskdoctor tag\n  diskdoctor health --disk studio\n  diskdoctor tag remove --disk 2 Studio\n\nAs tags ficam em %LOCALAPPDATA%\\diskdoctor\\tags.json, ligadas ao modelo + serial do\ndisco: continuam valendo se o número do disco mudar. Maiúsculas não importam."
        )
    )]
    Tag(TagArgs),

    #[command(
        about = t(
            "Interface language: show it or save English / Português (Brasil) as the default",
            "Idioma da interface: mostra ou salva English / Português (Brasil) como padrão"
        ),
        visible_alias = "lang",
        help_template = sub_template(),
        override_usage = "diskdoctor language [en|pt-BR]",
        after_help = t(
            "The choice is saved in %LOCALAPPDATA%\\diskdoctor\\settings.json. Priority: --lang,\nthen the DISKDOCTOR_LANG environment variable, then the saved choice, then English.\nTechnical names (SMART, Surface Scan, Sequential Read, Random 4 KiB, QD1,\nCapacity Verification) stay in English in every language.",
            "A escolha fica em %LOCALAPPDATA%\\diskdoctor\\settings.json. Prioridade: --lang,\ndepois a variável de ambiente DISKDOCTOR_LANG, depois a escolha salva, depois inglês.\nNomes técnicos (SMART, Surface Scan, Sequential Read, Random 4 KiB, QD1,\nCapacity Verification) ficam em inglês em qualquer idioma."
        )
    )]
    Language(LanguageArgs),
}

#[derive(Args, Debug, Clone)]
#[command(next_help_heading = options())]
pub struct LanguageArgs {
    #[arg(value_name = "en|pt-BR", value_parser = i18n::parse_arg, help = t("Language to save; without it, shows the current one", "Idioma a salvar; sem ele, mostra o atual"))]
    pub lang: Option<Lang>,
}

#[derive(Args, Debug, Clone)]
pub struct TagArgs {
    #[command(subcommand)]
    pub action: Option<TagAction>,
}

#[derive(Subcommand, Debug, Clone)]
pub enum TagAction {
    #[command(
        about = t("List drives and their tags (the default for `diskdoctor tag`)", "Lista os discos e suas tags (é o padrão de `diskdoctor tag`)"),
        help_template = sub_template(),
        override_usage = "diskdoctor tag list [--json]"
    )]
    List,

    #[command(
        about = t("Add one or more tags to a drive", "Adiciona uma ou mais tags a um disco"),
        help_template = sub_template(),
        override_usage = "diskdoctor tag add --disk <N|TAG> <TAG>..."
    )]
    Add(TagAddArgs),

    #[command(
        about = t("Remove tags from a drive", "Remove tags de um disco"),
        help_template = sub_template(),
        override_usage = "diskdoctor tag remove --disk <N|TAG> (<TAG>... | --all)"
    )]
    Remove(TagRemoveArgs),
}

#[derive(Args, Debug, Clone)]
#[command(next_help_heading = options())]
pub struct TagAddArgs {
    #[arg(long, value_name = "N|TAG", help = t("Drive: number or a tag it already has (see `diskdoctor list`)", "Disco: número ou uma tag que ele já tenha (veja `diskdoctor list`)"))]
    pub disk: DiskRef,
    #[arg(
        value_name = "TAG",
        required = true,
        num_args = 1..,
        help = t(
            "Tags to add: letters, digits, space, '-', '_' or '.' (up to 32 characters; quote tags with spaces)",
            "Tags a adicionar: letras, números, espaço, '-', '_' ou '.' (até 32 caracteres; use aspas se tiver espaço)"
        )
    )]
    pub tags: Vec<String>,
}

#[derive(Args, Debug, Clone)]
#[command(next_help_heading = options())]
pub struct TagRemoveArgs {
    #[arg(long, value_name = "N|TAG", help = t("Drive: number or one of its tags (see `diskdoctor list`)", "Disco: número ou uma tag que ele tenha (veja `diskdoctor list`)"))]
    pub disk: DiskRef,
    #[arg(value_name = "TAG", required_unless_present = "all", conflicts_with = "all", num_args = 1.., help = t("Tags to remove", "Tags a remover"))]
    pub tags: Vec<String>,
    #[arg(long, help = t("Remove every tag from the drive", "Remove todas as tags do disco"))]
    pub all: bool,
}

#[derive(Args, Debug, Clone)]
#[command(next_help_heading = options())]
pub struct HealthArgs {
    #[arg(long, value_name = "N|TAG", help = t("Analyze only this drive: number or tag (see `diskdoctor list`)", "Analisa só este disco: número ou tag (veja `diskdoctor list`)"))]
    pub disk: Option<DiskRef>,
    #[arg(long, value_name = "HOURS", default_value_t = 168, help = t("Windows event window, in hours", "Janela de eventos do Windows, em horas"))]
    pub hours: u32,
    #[arg(long, help = t("Do not query Windows events", "Não consulta os eventos do Windows"))]
    pub no_events: bool,
    #[arg(long, help = t("Do not save a snapshot to the local SMART history", "Não grava snapshot no histórico SMART local"))]
    pub no_history: bool,
}

#[derive(Args, Debug, Clone)]
#[command(next_help_heading = options())]
pub struct EventsArgs {
    #[arg(long, value_name = "HOURS", default_value_t = 168, help = t("Time window, in hours", "Janela de tempo, em horas"))]
    pub hours: u32,
    #[arg(long, value_name = "N|TAG", help = t("Show only events attributed to this drive: number or tag", "Mostra só eventos atribuídos a este disco: número ou tag"))]
    pub disk: Option<DiskRef>,
    #[arg(long, value_name = "N", default_value_t = 40, help = t("How many detailed events to show", "Quantos eventos detalhados exibir"))]
    pub limit: usize,
    #[arg(long, value_name = "N", default_value_t = 5000, help = t("Maximum events read from the log", "Máximo de eventos lidos do log"))]
    pub max_events: u32,
}

#[derive(Args, Debug, Clone)]
#[command(next_help_heading = options())]
pub struct ScanArgs {
    #[arg(long, value_name = "N|TAG", help = t("Drive: number or tag (see `diskdoctor list`)", "Disco: número ou tag (veja `diskdoctor list`)"))]
    pub disk: DiskRef,
    #[arg(long, help = t("Read-only (always on; accepted to make the intent explicit)", "Somente leitura (sempre ativo; aceito para deixar a intenção explícita)"))]
    pub read_only: bool,
    #[arg(long, value_name = "SIZE", default_value = "1M", help = t("Size of each read", "Tamanho de cada leitura"))]
    pub block_size: String,
    #[arg(long, value_name = "SIZE", help = t("Start at this offset (e.g. 100G)", "Começa neste deslocamento (ex.: 100G)"))]
    pub offset: Option<String>,
    #[arg(long, value_name = "SIZE", help = t("Read only this much (e.g. 20G); default: to the end of the drive", "Lê só esta quantidade (ex.: 20G); padrão: até o fim do disco"))]
    pub length: Option<String>,
    #[arg(long, value_name = "MS", default_value_t = 10_000, help = t("Timeout for each read, in ms (longer reads are cancelled)", "Tempo limite de cada leitura, em ms (leituras mais longas são canceladas)"))]
    pub timeout_ms: u64,
    #[arg(long, value_name = "MS", default_value_t = 500, help = t("A read slower than this many ms counts as slow", "A partir de quantos ms uma leitura é considerada lenta"))]
    pub slow_ms: u64,
    #[arg(long, value_name = "N", default_value_t = 128, help = t("Stop after this many consecutive failures", "Interrompe após esta quantidade de falhas seguidas"))]
    pub max_consecutive_errors: u32,
    #[arg(long, help = t("Do not isolate bad sectors by re-reading (faster on badly damaged drives)", "Não tenta isolar setores ruins por releitura (mais rápido em disco muito danificado)"))]
    pub no_refine: bool,
    #[arg(short = 'y', long, help = t("Do not ask for confirmation (for scripts)", "Não pede confirmação (uso em scripts)"))]
    pub yes: bool,
    #[arg(long, value_name = "FILE", help = t("Also save the result to this JSON file", "Também grava o resultado neste arquivo JSON"))]
    pub output: Option<PathBuf>,
}

#[derive(Args, Debug, Clone)]
#[command(next_help_heading = options())]
pub struct BenchmarkArgs {
    #[arg(long, value_name = "N|TAG", help = t("Drive: number or tag (see `diskdoctor list`)", "Disco: número ou tag (veja `diskdoctor list`)"))]
    pub disk: DiskRef,
    #[arg(long, conflicts_with = "write_test", help = t("Read-only (the default when --write-test is not used)", "Somente leitura (padrão quando --write-test não é usado)"))]
    pub read_only: bool,
    #[arg(long, value_name = "SIZE", default_value = "512M", help = t("Bytes read in each zone (start, middle, end) by Sequential Read", "Bytes lidos em cada zona (início, meio, fim) no Sequential Read"))]
    pub seq_size: String,
    #[arg(long, value_name = "SECONDS", default_value_t = 10, help = t("Duration of the Random 4 KiB test, in seconds", "Duração do teste Random 4 KiB, em segundos"))]
    pub random_seconds: u64,
    #[arg(
        long,
        value_name = "DIR",
        help = t(
            "Also run a WRITE test on a new temporary file in this folder (must be on a volume of the tested drive)",
            "Inclui teste de ESCRITA em um arquivo temporário novo nesta pasta (precisa estar em um volume do disco testado)"
        )
    )]
    pub write_test: Option<PathBuf>,
    #[arg(long, value_name = "SIZE", default_value = "1G", help = t("Size of the write test's temporary file", "Tamanho do arquivo temporário do teste de escrita"))]
    pub write_size: String,
    #[arg(long, value_name = "MS", default_value_t = 10_000, help = t("Timeout for each operation, in ms", "Tempo limite de cada operação, em ms"))]
    pub timeout_ms: u64,
}

#[derive(Args, Debug, Clone)]
#[command(next_help_heading = options())]
pub struct HistoryArgs {
    #[command(subcommand)]
    pub action: Option<HistoryAction>,
    #[arg(long, value_name = "N|TAG", help = t("Show only this drive: number or tag (default: all)", "Mostra só este disco: número ou tag (padrão: todos)"))]
    pub disk: Option<DiskRef>,
    #[arg(long, value_name = "N", default_value_t = 15, help = t("How many recent entries to show per drive", "Quantas entradas recentes mostrar por disco"))]
    pub limit: usize,
}

#[derive(Subcommand, Debug, Clone)]
pub enum HistoryAction {
    #[command(
        about = t(
            "Record a change made to the drive (e.g. \"Replaced the SATA cable\") to compare before × after",
            "Registra uma mudança feita no disco (ex.: \"Troquei o cabo SATA\") para comparar antes × depois"
        ),
        help_template = sub_template(),
        override_usage = "diskdoctor history note --disk <N|TAG> <TEXT>"
    )]
    Note(HistoryNoteArgs),
}

#[derive(Args, Debug, Clone)]
#[command(next_help_heading = options())]
pub struct HistoryNoteArgs {
    #[arg(long, value_name = "N|TAG", help = t("Drive: number or tag", "Disco: número ou tag"))]
    pub disk: DiskRef,
    #[arg(
        value_name = "TEXT",
        required = true,
        num_args = 1..,
        help = t(
            "What changed (e.g. \"Replaced the SATA cable\", \"Moved to SATA port 3\")",
            "O que mudou (ex.: \"Troquei o cabo SATA\", \"Mudei para a porta SATA 3\")"
        )
    )]
    pub text: Vec<String>,
}

#[derive(Args, Debug, Clone)]
#[command(next_help_heading = options())]
pub struct CapacityArgs {
    #[arg(long, value_name = "N|TAG", required_unless_present = "volume", help = t("Drive: number or tag (uses its lettered volume)", "Disco: número ou tag (usa o volume com letra dele)"))]
    pub disk: Option<DiskRef>,
    #[arg(long, value_name = "LETTER", help = t("Volume to test (e.g. E:)", "Volume a testar (ex.: E:)"))]
    pub volume: Option<String>,
    #[arg(
        long,
        value_name = "SIZE",
        help = t(
            "Test only this much (e.g. 8G). Default: all free space, which is needed to catch counterfeits",
            "Testa só esta quantidade (ex.: 8G). Padrão: todo o espaço livre, necessário para pegar falsificação"
        )
    )]
    pub size: Option<String>,
    #[arg(long, value_name = "MS", default_value_t = 30_000, help = t("Timeout for each write/read, in ms", "Tempo limite de cada gravação/leitura, em ms"))]
    pub timeout_ms: u64,
}

#[derive(Args, Debug, Clone)]
#[command(next_help_heading = options())]
pub struct ReportArgs {
    #[arg(long, value_enum, default_value_t = ReportFormat::Html, help = t("Report format", "Formato do relatório"))]
    pub format: ReportFormat,
    #[arg(short = 'o', long, value_name = "FILE", help = t("Output file or folder (default: current folder)", "Arquivo ou pasta de saída (padrão: pasta atual)"))]
    pub output: Option<PathBuf>,
    #[arg(long, value_name = "HOURS", default_value_t = 168, help = t("Windows event window, in hours", "Janela de eventos do Windows, em horas"))]
    pub hours: u32,
    #[arg(long, value_name = "FILE", help = t("Include this Surface Scan result (repeatable)", "Inclui este resultado de Surface Scan (pode repetir)"))]
    pub scan_file: Vec<PathBuf>,
    #[arg(long, help = t("Ignore previously saved scans and benchmarks", "Ignora scans e benchmarks salvos anteriormente"))]
    pub no_saved_results: bool,
    #[arg(long, help = t("Do not save a snapshot to the local SMART history", "Não grava snapshot no histórico SMART local"))]
    pub no_history: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::scoped;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid_in_both_languages() {
        for lang in i18n::ALL {
            let _l = scoped(lang);
            Cli::command().debug_assert();
        }
    }

    #[test]
    fn help_follows_the_language() {
        let render = |lang| {
            let _l = scoped(lang);
            Cli::command().render_help().to_string()
        };
        let en = render(Lang::En);
        assert!(en.contains("Usage:") && en.contains("Commands") && en.contains("Non-destructive"), "{en}");
        let pt = render(Lang::PtBr);
        assert!(pt.contains("Uso:") && pt.contains("Comandos") && pt.contains("Diagnóstico não destrutivo"), "{pt}");
        assert!(pt.contains("Surface Scan"), "technical names stay in English");
    }

    #[test]
    fn parses_documented_commands() {
        for args in [
            vec!["diskdoctor", "list"],
            vec!["diskdoctor", "health"],
            vec!["diskdoctor", "events", "--hours", "48"],
            vec!["diskdoctor", "scan", "--disk", "1", "--read-only"],
            vec!["diskdoctor", "benchmark", "--disk", "1", "--read-only"],
            vec!["diskdoctor", "report", "--format", "html"],
            vec!["diskdoctor", "report", "--format", "ambos", "--json"],
            vec!["diskdoctor", "tag"],
            vec!["diskdoctor", "tag", "list", "--json"],
            vec!["diskdoctor", "tag", "add", "--disk", "2", "Studio"],
            vec!["diskdoctor", "tag", "add", "--disk", "studio", "Windows", "Steam Library"],
            vec!["diskdoctor", "tag", "remove", "--disk", "2", "Studio"],
            vec!["diskdoctor", "tag", "remove", "--disk", "2", "--all"],
            vec!["diskdoctor", "health", "--disk", "studio"],
            vec!["diskdoctor", "scan", "--disk", "Games", "--read-only"],
            vec!["diskdoctor", "language"],
            vec!["diskdoctor", "language", "pt-BR"],
            vec!["diskdoctor", "lang", "en"],
            vec!["diskdoctor", "health", "--lang", "pt-BR"],
            vec!["diskdoctor", "--lang", "en", "list"],
        ] {
            Cli::try_parse_from(&args).unwrap_or_else(|e| panic!("{args:?}: {e}"));
        }
    }

    #[test]
    fn language_values_are_validated() {
        let Some(Command::Language(a)) = Cli::try_parse_from(["diskdoctor", "language", "pt_br"]).unwrap().command else { panic!() };
        assert_eq!(a.lang, Some(Lang::PtBr));
        assert!(Cli::try_parse_from(["diskdoctor", "language", "fr"]).is_err());
        assert!(Cli::try_parse_from(["diskdoctor", "list", "--lang", "xx"]).is_err());
        assert_eq!(Cli::try_parse_from(["diskdoctor", "list", "--lang", "pt-BR"]).unwrap().lang, Some(Lang::PtBr));
    }

    #[test]
    fn disk_accepts_number_or_tag() {
        let Some(Command::Scan(a)) = Cli::try_parse_from(["diskdoctor", "scan", "--disk", "2"]).unwrap().command else { panic!() };
        assert_eq!(a.disk, DiskRef::Number(2));
        let Some(Command::Scan(a)) = Cli::try_parse_from(["diskdoctor", "scan", "--disk", "Studio"]).unwrap().command else { panic!() };
        assert_eq!(a.disk, DiskRef::Tag("Studio".into()));
    }

    #[test]
    fn tag_actions_require_tags_or_all() {
        assert!(Cli::try_parse_from(["diskdoctor", "tag", "add", "--disk", "2"]).is_err());
        assert!(Cli::try_parse_from(["diskdoctor", "tag", "add", "Studio"]).is_err(), "--disk is required");
        assert!(Cli::try_parse_from(["diskdoctor", "tag", "remove", "--disk", "2"]).is_err());
        assert!(Cli::try_parse_from(["diskdoctor", "tag", "remove", "--disk", "2", "--all", "Studio"]).is_err());
    }

    #[test]
    fn write_test_conflicts_with_read_only() {
        assert!(Cli::try_parse_from(["diskdoctor", "benchmark", "--disk", "1", "--read-only", "--write-test", "D:\\"]).is_err());
    }

    #[test]
    fn scan_requires_disk() {
        assert!(Cli::try_parse_from(["diskdoctor", "scan"]).is_err());
    }
}
