//! Definição da linha de comando (clap). Textos de ajuda em pt-BR.

use crate::report::ReportFormat;
use crate::tags::DiskRef;
use clap::{ArgAction, Args, Parser, Subcommand};
use std::path::PathBuf;

const ROOT_TEMPLATE: &str = "{before-help}{name} {version} — {about}\n\nUso: {usage}\n\n{all-args}{after-help}";
const SUB_TEMPLATE: &str = "{about}\n\nUso: {usage}\n\n{all-args}{after-help}";

#[derive(Parser, Debug)]
#[command(
    name = "diskdoctor",
    version,
    about = "Diagnóstico não destrutivo de HDs, SSDs, pendrives e cartões de memória no Windows",
    disable_help_flag = true,
    disable_version_flag = true,
    disable_help_subcommand = true,
    help_template = ROOT_TEMPLATE,
    override_usage = "diskdoctor [COMANDO] [opções]      (sem comando: menu interativo)",
    subcommand_help_heading = "Comandos",
    subcommand_value_name = "COMANDO",
    next_help_heading = "Opções globais",
    after_help = "Exemplos:\n  diskdoctor list\n  diskdoctor tag add --disk 2 Studio\n  diskdoctor health --disk studio\n  diskdoctor events --hours 48\n  diskdoctor scan --disk 1 --read-only\n  diskdoctor benchmark --disk 1 --read-only\n  diskdoctor report --format html\n\nNenhum comando grava nos discos analisados. Use um terminal de Administrador\npara SMART completo e para os testes de leitura."
)]
pub struct Cli {
    /// Sem comando, abre o menu interativo.
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Imprime o resultado em JSON (para scripts e outros agentes)
    #[arg(long, global = true)]
    pub json: bool,

    /// Desativa cores no terminal
    #[arg(long, global = true)]
    pub no_color: bool,

    /// Mostra esta ajuda
    #[arg(short = 'h', long = "help", action = ArgAction::Help, global = true)]
    help: Option<bool>,

    /// Mostra a versão
    #[arg(short = 'V', long = "version", action = ArgAction::Version)]
    version: Option<bool>,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Lista os discos físicos: modelo, serial mascarado, capacidade, interface e volumes
    #[command(help_template = SUB_TEMPLATE, override_usage = "diskdoctor list [--json]")]
    List,

    /// Saúde dos discos: SMART, contadores do Windows, eventos e diagnóstico correlacionado
    #[command(help_template = SUB_TEMPLATE, override_usage = "diskdoctor health [--disk <N|TAG>] [--hours <HORAS>] [opções]")]
    Health(HealthArgs),

    /// Eventos do Windows ligados a armazenamento (IDs 7, 51, 129, 153 e outros)
    #[command(help_template = SUB_TEMPLATE, override_usage = "diskdoctor events [--hours <HORAS>] [--disk <N|TAG>] [opções]")]
    Events(EventsArgs),

    /// Teste de leitura sequencial do disco inteiro (somente leitura; exige Administrador)
    #[command(
        help_template = SUB_TEMPLATE,
        override_usage = "diskdoctor scan --disk <N|TAG> [--read-only] [opções]",
        after_help = "O teste nunca grava no disco: o dispositivo é aberto pelo Windows apenas para leitura.\nCtrl+C cancela a qualquer momento e o resultado parcial é salvo."
    )]
    Scan(ScanArgs),

    /// Benchmark de leitura; escrita só em arquivo temporário e com confirmação digitada
    #[command(help_template = SUB_TEMPLATE, override_usage = "diskdoctor benchmark --disk <N|TAG> [--read-only | --write-test <PASTA>] [opções]")]
    Benchmark(BenchmarkArgs),

    /// Confere se a capacidade é verdadeira (pendrive/cartão falsificado), gravando e lendo de volta arquivos temporários
    #[command(
        help_template = SUB_TEMPLATE,
        override_usage = "diskdoctor capacity (--disk <N|TAG> | --volume <LETRA>) [opções]",
        after_help = "Como o f3write/f3read: cria arquivos NOVOS no espaço livre, cada bloco marcado com a própria\n\
                      posição, lê tudo de volta sem cache e apaga os arquivos no fim. Arquivos existentes não são\n\
                      abertos nem alterados. Num dispositivo FALSO, porém, gravar além da capacidade real pode\n\
                      corromper o que já está nele: faça backup e, de preferência, teste com o dispositivo vazio.\n\
                      Exige digitar GRAVAR para confirmar. O disco do sistema é recusado."
    )]
    Capacity(CapacityArgs),

    /// Histórico de cada disco e comparação antes × depois de mudanças (ex.: troca de cabo)
    #[command(
        help_template = SUB_TEMPLATE,
        override_usage = "diskdoctor history [--disk <N|TAG>] [--limit <N>]\n       diskdoctor history note --disk <N|TAG> <TEXTO>",
        args_conflicts_with_subcommands = true,
        disable_help_subcommand = true,
        subcommand_help_heading = "Ações",
        subcommand_value_name = "AÇÃO",
        after_help = "Exemplo — descobrir se o problema era o cabo:\n  1. diskdoctor health                                   (registra o estado atual)\n  2. troque o cabo SATA e registre:\n     diskdoctor history note --disk 2 \"Troquei o cabo SATA\"\n  3. use o PC normalmente por 1–3 dias e rode diskdoctor health de novo\n  4. diskdoctor history --disk 2                          (compara antes × depois)\n\nDepois de registrar uma mudança, o veredito do disco considera só o que aconteceu depois dela."
    )]
    History(HistoryArgs),

    /// Gera relatório completo (HTML e/ou JSON) correlacionando todas as fontes
    #[command(help_template = SUB_TEMPLATE, override_usage = "diskdoctor report [--format html|json|both] [--output <ARQUIVO>] [opções]")]
    Report(ReportArgs),

    /// Dá apelidos (tags) aos discos, como "Studio" ou "Jogos"; as tags valem em --disk
    #[command(
        help_template = SUB_TEMPLATE,
        override_usage = "diskdoctor tag [list]\n       diskdoctor tag add --disk <N|TAG> <TAG>...\n       diskdoctor tag remove --disk <N|TAG> (<TAG>... | --all)",
        disable_help_subcommand = true,
        subcommand_help_heading = "Ações",
        subcommand_value_name = "AÇÃO",
        after_help = "Exemplos:\n  diskdoctor tag add --disk 2 Studio\n  diskdoctor tag add --disk 0 Jogos \"Biblioteca Steam\"\n  diskdoctor tag\n  diskdoctor health --disk studio\n  diskdoctor tag remove --disk 2 Studio\n\nAs tags ficam em %LOCALAPPDATA%\\diskdoctor\\tags.json, ligadas ao modelo + serial do\ndisco: continuam valendo se o número do disco mudar. Maiúsculas não importam."
    )]
    Tag(TagArgs),
}

#[derive(Args, Debug, Clone)]
pub struct TagArgs {
    #[command(subcommand)]
    pub action: Option<TagAction>,
}

#[derive(Subcommand, Debug, Clone)]
pub enum TagAction {
    /// Lista os discos e suas tags (é o padrão de `diskdoctor tag`)
    #[command(help_template = SUB_TEMPLATE, override_usage = "diskdoctor tag list [--json]")]
    List,

    /// Adiciona uma ou mais tags a um disco
    #[command(help_template = SUB_TEMPLATE, override_usage = "diskdoctor tag add --disk <N|TAG> <TAG>...")]
    Add(TagAddArgs),

    /// Remove tags de um disco
    #[command(help_template = SUB_TEMPLATE, override_usage = "diskdoctor tag remove --disk <N|TAG> (<TAG>... | --all)")]
    Remove(TagRemoveArgs),
}

#[derive(Args, Debug, Clone)]
#[command(next_help_heading = "Opções")]
pub struct TagAddArgs {
    /// Disco: número ou uma tag que ele já tenha (veja `diskdoctor list`)
    #[arg(long, value_name = "N|TAG")]
    pub disk: DiskRef,
    /// Tags a adicionar: letras, números, espaço, '-', '_' ou '.' (até 32 caracteres; use aspas se tiver espaço)
    #[arg(value_name = "TAG", required = true, num_args = 1..)]
    pub tags: Vec<String>,
}

#[derive(Args, Debug, Clone)]
#[command(next_help_heading = "Opções")]
pub struct TagRemoveArgs {
    /// Disco: número ou uma tag que ele tenha (veja `diskdoctor list`)
    #[arg(long, value_name = "N|TAG")]
    pub disk: DiskRef,
    /// Tags a remover
    #[arg(value_name = "TAG", required_unless_present = "all", conflicts_with = "all", num_args = 1..)]
    pub tags: Vec<String>,
    /// Remove todas as tags do disco
    #[arg(long)]
    pub all: bool,
}

#[derive(Args, Debug, Clone)]
#[command(next_help_heading = "Opções")]
pub struct HealthArgs {
    /// Analisa só este disco: número ou tag (veja `diskdoctor list`)
    #[arg(long, value_name = "N|TAG")]
    pub disk: Option<DiskRef>,
    /// Janela de eventos do Windows, em horas
    #[arg(long, value_name = "HORAS", default_value_t = 168)]
    pub hours: u32,
    /// Não consulta os eventos do Windows
    #[arg(long)]
    pub no_events: bool,
    /// Não grava snapshot no histórico SMART local
    #[arg(long)]
    pub no_history: bool,
}

#[derive(Args, Debug, Clone)]
#[command(next_help_heading = "Opções")]
pub struct EventsArgs {
    /// Janela de tempo, em horas
    #[arg(long, value_name = "HORAS", default_value_t = 168)]
    pub hours: u32,
    /// Mostra só eventos atribuídos a este disco: número ou tag
    #[arg(long, value_name = "N|TAG")]
    pub disk: Option<DiskRef>,
    /// Quantos eventos detalhados exibir
    #[arg(long, value_name = "N", default_value_t = 40)]
    pub limit: usize,
    /// Máximo de eventos lidos do log
    #[arg(long, value_name = "N", default_value_t = 5000)]
    pub max_events: u32,
}

#[derive(Args, Debug, Clone)]
#[command(next_help_heading = "Opções")]
pub struct ScanArgs {
    /// Disco: número ou tag (veja `diskdoctor list`)
    #[arg(long, value_name = "N|TAG")]
    pub disk: DiskRef,
    /// Somente leitura (sempre ativo; aceito para deixar a intenção explícita)
    #[arg(long)]
    pub read_only: bool,
    /// Tamanho de cada leitura
    #[arg(long, value_name = "TAMANHO", default_value = "1M")]
    pub block_size: String,
    /// Começa neste deslocamento (ex.: 100G)
    #[arg(long, value_name = "TAMANHO")]
    pub offset: Option<String>,
    /// Lê só esta quantidade (ex.: 20G); padrão: até o fim do disco
    #[arg(long, value_name = "TAMANHO")]
    pub length: Option<String>,
    /// Tempo limite de cada leitura, em ms (leituras mais longas são canceladas)
    #[arg(long, value_name = "MS", default_value_t = 10_000)]
    pub timeout_ms: u64,
    /// A partir de quantos ms uma leitura é considerada lenta
    #[arg(long, value_name = "MS", default_value_t = 500)]
    pub slow_ms: u64,
    /// Interrompe após esta quantidade de falhas seguidas
    #[arg(long, value_name = "N", default_value_t = 128)]
    pub max_consecutive_errors: u32,
    /// Não tenta isolar setores ruins por releitura (mais rápido em disco muito danificado)
    #[arg(long)]
    pub no_refine: bool,
    /// Não pede confirmação (uso em scripts)
    #[arg(short = 'y', long)]
    pub yes: bool,
    /// Também grava o resultado neste arquivo JSON
    #[arg(long, value_name = "ARQUIVO")]
    pub output: Option<PathBuf>,
}

#[derive(Args, Debug, Clone)]
#[command(next_help_heading = "Opções")]
pub struct BenchmarkArgs {
    /// Disco: número ou tag (veja `diskdoctor list`)
    #[arg(long, value_name = "N|TAG")]
    pub disk: DiskRef,
    /// Somente leitura (padrão quando --write-test não é usado)
    #[arg(long, conflicts_with = "write_test")]
    pub read_only: bool,
    /// Bytes lidos em cada zona (início, meio, fim) no teste sequencial
    #[arg(long, value_name = "TAMANHO", default_value = "512M")]
    pub seq_size: String,
    /// Duração do teste aleatório de 4 KiB, em segundos
    #[arg(long, value_name = "SEGUNDOS", default_value_t = 10)]
    pub random_seconds: u64,
    /// Inclui teste de ESCRITA em um arquivo temporário novo nesta pasta (precisa estar em um volume do disco testado)
    #[arg(long, value_name = "PASTA")]
    pub write_test: Option<PathBuf>,
    /// Tamanho do arquivo temporário do teste de escrita
    #[arg(long, value_name = "TAMANHO", default_value = "1G")]
    pub write_size: String,
    /// Tempo limite de cada operação, em ms
    #[arg(long, value_name = "MS", default_value_t = 10_000)]
    pub timeout_ms: u64,
}

#[derive(Args, Debug, Clone)]
#[command(next_help_heading = "Opções")]
pub struct HistoryArgs {
    #[command(subcommand)]
    pub action: Option<HistoryAction>,
    /// Mostra só este disco: número ou tag (padrão: todos)
    #[arg(long, value_name = "N|TAG")]
    pub disk: Option<DiskRef>,
    /// Quantas entradas recentes mostrar por disco
    #[arg(long, value_name = "N", default_value_t = 15)]
    pub limit: usize,
}

#[derive(Subcommand, Debug, Clone)]
pub enum HistoryAction {
    /// Registra uma mudança feita no disco (ex.: "Troquei o cabo SATA") para comparar antes × depois
    #[command(help_template = SUB_TEMPLATE, override_usage = "diskdoctor history note --disk <N|TAG> <TEXTO>")]
    Note(HistoryNoteArgs),
}

#[derive(Args, Debug, Clone)]
#[command(next_help_heading = "Opções")]
pub struct HistoryNoteArgs {
    /// Disco: número ou tag
    #[arg(long, value_name = "N|TAG")]
    pub disk: DiskRef,
    /// O que mudou (ex.: "Troquei o cabo SATA", "Mudei para a porta SATA 3")
    #[arg(value_name = "TEXTO", required = true, num_args = 1..)]
    pub text: Vec<String>,
}

#[derive(Args, Debug, Clone)]
#[command(next_help_heading = "Opções")]
pub struct CapacityArgs {
    /// Disco: número ou tag (usa o volume com letra dele)
    #[arg(long, value_name = "N|TAG", required_unless_present = "volume")]
    pub disk: Option<DiskRef>,
    /// Volume a testar (ex.: E:)
    #[arg(long, value_name = "LETRA")]
    pub volume: Option<String>,
    /// Testa só esta quantidade (ex.: 8G). Padrão: todo o espaço livre — necessário para pegar falsificação
    #[arg(long, value_name = "TAMANHO")]
    pub size: Option<String>,
    /// Tempo limite de cada gravação/leitura, em ms
    #[arg(long, value_name = "MS", default_value_t = 30_000)]
    pub timeout_ms: u64,
}

#[derive(Args, Debug, Clone)]
#[command(next_help_heading = "Opções")]
pub struct ReportArgs {
    /// Formato do relatório
    #[arg(long, value_enum, default_value_t = ReportFormat::Html)]
    pub format: ReportFormat,
    /// Arquivo ou pasta de saída (padrão: pasta atual)
    #[arg(short = 'o', long, value_name = "ARQUIVO")]
    pub output: Option<PathBuf>,
    /// Janela de eventos do Windows, em horas
    #[arg(long, value_name = "HORAS", default_value_t = 168)]
    pub hours: u32,
    /// Inclui este resultado de scan (pode repetir)
    #[arg(long, value_name = "ARQUIVO")]
    pub scan_file: Vec<PathBuf>,
    /// Ignora scans/benchmarks salvos anteriormente
    #[arg(long)]
    pub no_saved_results: bool,
    /// Não grava snapshot no histórico SMART local
    #[arg(long)]
    pub no_history: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
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
            vec!["diskdoctor", "tag", "add", "--disk", "studio", "Windows", "Biblioteca Steam"],
            vec!["diskdoctor", "tag", "remove", "--disk", "2", "Studio"],
            vec!["diskdoctor", "tag", "remove", "--disk", "2", "--all"],
            vec!["diskdoctor", "health", "--disk", "studio"],
            vec!["diskdoctor", "scan", "--disk", "Jogos", "--read-only"],
        ] {
            Cli::try_parse_from(&args).unwrap_or_else(|e| panic!("{args:?}: {e}"));
        }
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
        assert!(Cli::try_parse_from(["diskdoctor", "tag", "add", "Studio"]).is_err(), "--disk é obrigatório");
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
