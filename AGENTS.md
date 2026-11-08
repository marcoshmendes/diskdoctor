# AGENTS.md — guia para agentes que trabalham no diskdoctor

> Leia este arquivo antes de alterar o código. Ele é o mapa do projeto, lista as
> invariantes de segurança e diz como estender. **Mantenha-o atualizado** na mesma
> mudança que alterar comportamento (e registre no `CHANGELOG.md`).

## O que é

`diskdoctor` é uma CLI para Windows 10/11, em Rust, que diagnostica HDs e SSDs (SATA/NVMe),
discos externos USB, pendrives e cartões de memória, com foco em congelamentos, timeouts,
erros de leitura, falhas de comunicação e capacidade falsificada. Ela correlaciona SMART (via
`smartctl`), contadores de confiabilidade do Windows, eventos do log System e testes próprios
(leitura, benchmark, verificação de capacidade). Para cada disco gera achados com causa
provável e grau de confiança, e guarda um histórico para comparar antes × depois de mudanças.

- Código, identificadores e comentários técnicos: **inglês** nos nomes e **pt-BR** nos comentários e docs.
- Mensagens ao usuário (terminal, HTML, erros): **pt-BR**.
- Sem argumentos, abre um **menu interativo** (`interactive.rs`) que só monta e executa linhas de
  comando da CLI.
- Status atual: ver `CHANGELOG.md`. Repositório **público**: nunca commite dados reais de máquina
  (seriais, GUIDs de volume, nome do computador, caminhos de usuário, rótulos de volume).

## Build, testes e execução

```powershell
# Rust instalado via rustup (toolchain stable MSVC). Em shells novos:
$env:Path = "$env:USERPROFILE\.cargo\bin;" + $env:Path

cargo build                     # debug
cargo build --release           # binário em target\release\diskdoctor.exe (~4 MB)
cargo test                      # unidade + integração (tests\cli.rs); não exige admin
cargo clippy --all-targets      # deve ficar sem avisos
```

- Os testes **não tocam em disco real**: os motores usam leitores simulados
  (`io::testing::ScriptedReader`/`ScriptedWriter`, dispositivos falsos em `capacity::tests`) e os
  parsers usam fixtures em `tests/fixtures/`.
- `cargo test --test capacity_win -- --ignored` exercita o caminho nativo do teste de capacidade
  com ~64 MiB de arquivos numa pasta temporária (ignorado por padrão).
- A lógica compila e testa em qualquer SO. O código Win32 fica atrás de `#[cfg(windows)]`
  e tem stubs em `platform/unsupported.rs`.
- Para executar sem poluir os dados do usuário, use `DISKDOCTOR_DATA_DIR=<pasta temporária>`.
- `scan` e `benchmark` exigem terminal de Administrador. `list` e `events` funcionam sem
  elevação; `health` funciona sem elevação, mas com SMART parcial.

## Repositório, commits e releases

- Repositório público: <https://github.com/marcoshmendes/diskdoctor> (licença MIT, `LICENSE`).
- Commits usam o e-mail **noreply** do GitHub, configurado só neste repositório
  (`git config user.email "<id>+<login>@users.noreply.github.com"`). Nunca use o e-mail pessoal.
- CI: `.github/workflows/ci.yml` (clippy `-D warnings` + testes em `windows-latest`) a cada push
  na `main` e em pull requests.
- Release: atualize `version` no `Cargo.toml` e o `CHANGELOG.md`, faça o commit e depois
  `git tag -a vX.Y.Z -m "diskdoctor X.Y.Z" && git push origin vX.Y.Z`. O `release.yml` roda os
  testes, compila no GitHub e publica `diskdoctor-vX.Y.Z-windows-x64.zip` + `.sha256` em Releases.
- Não publique binários compilados localmente: eles carregam caminhos da máquina
  (ex.: `C:\Users\<nome>\.cargo\...`).
- Se o `git push` pedir usuário/senha, rode `gh auth setup-git` (usa o login do GitHub CLI).

## Mapa do código

| Arquivo | Responsabilidade |
|---|---|
| `src/main.rs` | Só chama `app::run()`. |
| `src/app.rs` | Parse da CLI, tradução de erros do clap, `dispatch`, **códigos de saída** (`app::exit`), Ctrl+C (`begin_operation`). |
| `src/cli.rs` | Definição clap (ajuda em pt-BR, templates próprios). `command` é `Option`: sem comando → menu. |
| `src/interactive.rs` | Menu (dialoguer): painel de discos + ações. Cada ação vira argv da CLI, é mostrada ("Executando: …") e roda via `app::dispatch`. Oferece reabrir como Admin (`platform::relaunch_as_admin`). |
| `src/commands/*.rs` | Um arquivo por comando (`list`, `health`, `events`, `scan`, `benchmark`, `capacity`, `history`, `tag`, `report`). `commands/mod.rs` tem `Ctx`, `load_disks()` (inventário **+ tags**), `select_disk()` (número ou tag), `collect()` (coleta de health/report) e `record_diagnoses()`. |
| `src/inventory.rs` | Discos via PowerShell/CIM (`Get-Disk`, `Get-PhysicalDisk`, `Win32_DiskDrive`, `Get-Partition`, `Get-Volume`); `DiskInfo` (com `kind`: interno/externo/pendrive/cartão e `tags`), `DiskIdentity`, `disk_title`, mapeamento volume→disco. |
| `src/tags.rs` | Tags do usuário (`tags.json`, por fingerprint), validação, `DiskRef` (`--disk` = número ou tag). |
| `src/capacity.rs` | Motor da verificação de capacidade (estilo f3): padrão por bloco, conferência, veredito (`Genuine`/`Fake`/`Defective`), estimativa da capacidade real. |
| `src/journal.rs` | Diário por disco: diagnósticos e mudanças registradas; `compare()` antes × depois. |
| `src/smart.rs` | Localiza e executa `smartctl -x --json=c`, **lista de argumentos proibidos**, parsing ATA e NVMe → `SmartReport`. |
| `src/windows_counters.rs` | `Get-StorageReliabilityCounter` (requer admin). |
| `src/events.rs` | Consulta do log System (XPath), classificação por **(origem, ID, nível)**, atribuição de evento → disco. |
| `src/health.rs` | Agrega SMART, contadores e histórico por disco (`DiskHealth`). |
| `src/history.rs` | Snapshots JSONL de contadores SMART; detecta contadores que **crescem**. |
| `src/io.rs` | Traits `BlockReader` e `BlockWriter`, `IoOutcome`, classificação de erros Win32, dispositivos simulados para testes. |
| `src/scan.rs` | Motor do teste de leitura: blocos, tempo limite, bisseção de blocos ruins, erros transitórios, linha do tempo, latências. |
| `src/benchmark.rs` | Benchmark QD1: leitura sequencial em 3 zonas mais 4 KiB aleatório; escrita só em arquivo temporário. |
| `src/stats.rs` | Histograma logarítmico de latência (percentis com memória constante). |
| `src/analysis.rs` | **Motor de correlação**: regras → `Finding` com evidências ponderadas, confiança, causas e recomendações; veredito por disco. |
| `src/report/mod.rs`, `report/html.rs` | Relatório JSON e HTML autocontido (CSS e SVG embutidos, todo texto escapado). |
| `src/ui.rs` | Terminal: tabelas (comfy-table), cores, barras de progresso (indicatif), confirmações. |
| `src/store.rs` | Pasta de dados (`%LOCALAPPDATA%\diskdoctor`), resultados de scan e benchmark salvos. |
| `src/platform/` | **Único lugar com `unsafe`/Win32 e processos externos**: `win.rs` (RawDisk, TempBenchFile, CapacityDir, elevação, QueryDosDevice, ShellExecute para reabrir como Admin e abrir o relatório), `powershell.rs`, `process.rs`. |
| `src/util.rs` | Formatação pt-BR, máscara de serial, `parse_size`, escape HTML, RNG. |

Fluxo de dados: `inventory` → (`health` + `events` + resultados salvos de `scan`/`benchmark`)
→ `analysis::analyze` → `ui` (terminal) ou `report` (HTML/JSON).

## Invariantes de segurança (NÃO quebrar)

| Invariante | Onde é garantida | Teste que protege |
|---|---|---|
| O disco físico é aberto **só com `GENERIC_READ`**; não existe `BlockWriter` para disco físico. | `platform/win.rs::RawDisk::open`, separação de traits em `io.rs`. | Revisão; o tipo impede a escrita. |
| Escrita só em arquivo **novo** (`CREATE_NEW`), apagado ao fechar (`FILE_FLAG_DELETE_ON_CLOSE`), em pasta de um volume **do disco testado**, após o usuário **digitar `GRAVAR`** (não há `--yes` para escrita). | `platform/win.rs::TempBenchFile`, `commands/benchmark.rs::validate_write_target`. | `benchmark::tests::write_benchmark_stays_inside_temp_file`, `tests/cli.rs::write_test_cannot_be_combined_with_read_only`. |
| Verificação de capacidade: só cria uma pasta **nova** e arquivos `CREATE_NEW` dentro dela; só reabre e apaga o que ela criou; remove a pasta com `remove_dir` (nunca recursivo); recusa o disco do sistema; exige **digitar `GRAVAR`** e não aceita `--json`/`--yes`; avisa do risco em dispositivo falso. | `platform/win.rs::CapacityDir`, `commands/capacity.rs`. | `capacity::tests::*` (o alvo simulado falha se o motor recriar ou reabrir arquivo alheio), `tests/capacity_win.rs` (ignorado). |
| O menu interativo nunca pula confirmações: ele executa a mesma linha de comando da CLI. | `interactive.rs::run_args` → `app::dispatch`. | Revisão. |
| Arquivos que o diskdoctor grava por conta própria ficam só na pasta de dados (`tags.json`, `journal\`, `history\`, resultados). | `tags.rs`, `journal.rs`, `history.rs`, `store.rs`. | `tags::tests::store_round_trip_and_missing_or_corrupt_file`. |
| `smartctl` só com argumentos de consulta: nada de `-t`, `-X`, `-s`, `-o`, `-S`, `--set`, nem reset de logs. | `smart.rs::ensure_read_only_args` (chamada antes de toda execução). | `smart::tests::rejects_destructive_or_mutating_args`. |
| Nunca formatar, particionar, rodar `chkdsk /f` nem alterar arquivos existentes. O diskdoctor só **recomenda** `chkdsk X:` (somente leitura). | Não há código para isso. | — |
| Confirmação e aviso de backup antes de testes longos (`scan`); `--yes` só para scripts. | `commands/scan.rs`. | — |
| Serial completo nunca aparece em saída ou relatório; usar `serial_masked` e `fingerprint`. | `util::mask_serial`, `inventory.rs`, `smart.rs`. | `util::tests::masks_serials`, `report::html::tests::renders_and_escapes_everything`. |
| Todo texto vindo do sistema é escapado no HTML. | `report/html.rs::e`. | `renders_and_escapes_everything`. |
| SMART normal **não** significa disco saudável: o veredito sempre lista lacunas e a nota de aviso. | `analysis.rs::analyze`, `analyze_disk` (`gaps`, `notes`). | `analysis::tests::clean_data_is_healthy_but_never_guaranteed`. |
| Se a E/S travar (nem o cancelamento conclui), buffer e `OVERLAPPED` são **vazados** de propósito (o kernel ainda pode escrever neles). | `platform/win.rs::OverlappedHandle` (`poisoned`). | `scan::tests::hung_device_aborts_scan` (motor). |

## Convenções

- `unsafe` só em `src/platform/win.rs`. Novos acessos Win32 vão para lá, com wrapper seguro.
- Processos externos só via `platform::process::run_with_timeout`, sempre com tempo limite.
- Erros: `anyhow` com contexto em pt-BR (`.context("falha ao ...")`). Erros esperados viram
  código de saída próprio, não pânico.
- Scripts PowerShell rodam no **Windows PowerShell 5.1** (`powershell.exe`), não no `pwsh`.
  Projete cada propriedade com cast explícito (`[string]`, `[uint64]`) e use
  `ConvertTo-Json -InputObject @($x)` para manter arrays de um elemento como array.
- Enums do Windows podem chegar como nome (`"SATA"`) ou número (`"11"`); os parsers aceitam os dois.
- Formatação de números ao usuário: `util::int`, `decimal`, `format_bytes` (vírgula decimal, ponto de milhar).
- Rode `cargo fmt` se disponível; mantenha o `clippy` limpo.

## Como estender

**Nova regra de diagnóstico** (`analysis.rs`):
1. Crie `fn rule_xxx(ctx: &DiskCtx) -> Option<Finding>` usando `Builder::new("area.nome", "Título pt-BR")`.
2. Adicione evidências com `b.add(fonte, peso, severidade, texto)`. Pesos: 1 fraca, 2 moderada,
   3 forte, 4 muito forte, 5 definitiva. A confiança sai de `confidence_for` (ver `docs/DIAGNOSTICS.md`).
3. Inclua `causes(...)` e `recs(...)` em pt-BR, acionáveis e **não destrutivas**.
4. Registre a regra na lista em `analyze_disk` e escreva um teste em `analysis::tests`.
5. Documente a regra em `docs/DIAGNOSTICS.md`.

**Novo evento do Windows** (`events.rs`): adicione o ID em `QUERY_EVENT_IDS`, a variante
em `EventKind` (com `description` e `category`) e o par (origem, ID) em `classify`. Teste em
`events::tests::classifies_by_provider_and_id`. Atenção: o mesmo ID existe em várias origens.

**Novo atributo SMART** (`smart.rs`): adicione o campo em `SmartReport`, extraia em
`parse_smartctl_json` (prefira casar por **nome** do atributo, que vem do drivedb do smartctl,
em vez de só por ID, porque IDs variam entre fabricantes), inclua em `counters()` se for
contador de problema e em `history::PROBLEM_COUNTERS`.

**Novo comando**: variante em `cli::Command` + `Args` com textos pt-BR, arquivo em
`src/commands/`, despacho em `app::dispatch`, teste em `tests/cli.rs`, seção no `README.md` e,
se fizer sentido, uma entrada no `MENU` de `interactive.rs` (que só monta a linha de comando).
Para escolher disco, use `Option<DiskRef>`/`DiskRef` e `commands::select_disk` — assim tags
funcionam de graça. Testes longos e canceláveis: `let _op = app::begin_operation();`.

**Novo tipo de dispositivo**: `inventory::infer_device_kind` + rótulo em `DiskInfo::class_label`
+ caso em `inventory::tests::classifies_external_flash_and_cards`.

## Fixtures (`tests/fixtures/`)

- `inventory.json`: saída real do script de inventário (3 SSDs SATA), com seriais, GUIDs de
  volume e rótulos trocados por valores fictícios (`00000000-0000-4000-8000-000000000104`…).
- `smartctl-sata-ssd-*.json`: saídas reais de `smartctl -x --json=c` sem admin, seriais trocados.
- `smartctl-nvme-warning.json`, `smartctl-sata-hdd-failing.json`: **sintéticas**, para cobrir
  NVMe e HD com setores pendentes, erros UNC e autoteste com falha.
- `events.json`: subconjunto real do log System. Contém 4× `disk/153` do disco 2, falsos
  positivos (Hyper-V 129, Kernel-Boot 153, Processor-Power 55), NTFS 98 nos níveis 2 e 4,
  NTFS 7, Kernel-Power 41 e EventLog 6008.

Para regenerar: rode o script do módulo no `powershell.exe` 5.1 e, antes de salvar, **troque
seriais, WWN, GUIDs de volume, rótulos de volume, nome do computador e caminhos de usuário**
por valores fictícios. O repositório é público. Confira com
`git grep -iE "users\\\\|<seu-usuário>|<nome-do-pc>"` antes de commitar.

## Códigos de saída (`app::exit`)

| Código | Significado |
|---|---|
| 0 | Executado; nenhum problema de severidade alerta ou crítica. |
| 1 | Erro de execução. |
| 2 | Uso incorreto da linha de comando. |
| 3 | Exige terminal de Administrador. |
| 4 | Cancelado (Ctrl+C). |
| 5 | Executado, e foram encontrados problemas (alerta ou crítico, ou erros no scan). |
| 6 | O usuário recusou a confirmação. |

## Dados locais e variáveis de ambiente

- `%LOCALAPPDATA%\diskdoctor\` (ou `DISKDOCTOR_DATA_DIR`):
  - `scans\scan-<fp>-<data>.json` e `benchmarks\bench-<fp>-<data>.json`. O `report` inclui
    automaticamente o mais recente de cada disco dos últimos 30 dias.
  - `history\<fingerprint>.jsonl`: snapshots SMART (máximo de 500 linhas por disco).
  - `journal\<fingerprint>.jsonl`: diagnósticos e mudanças registradas (máx. 2000 linhas;
    mudanças nunca são descartadas).
  - `capacity\capacity-<fp>-<data>.json`: verificações de capacidade.
  - `tags.json`: tags por fingerprint (gravado via arquivo temporário + rename).
- Verificação de capacidade: pasta temporária `<volume>\diskdoctor-capacidade-AAAAMMDD-HHMMSS\`
  com `0001.ddcap`, `0002.ddcap`…, apagada no fim.
- Relatórios do menu interativo: `%USERPROFILE%\Documents\diskdoctor\`.
- `DISKDOCTOR_SMARTCTL`: caminho explícito do `smartctl.exe`.
- `NO_COLOR`: desativa cores (equivale a `--no-color`).
- `--json` (global): saída estruturada no stdout, para agentes e scripts.

## Armadilhas conhecidas

- **Smart App Control (Windows 11)**: com ele ativado, o `.exe` release compilado com
  `strip = true` foi bloqueado ("Uma política de Controle de Aplicativo bloqueou este arquivo",
  evento CodeIntegrity 3077). O mesmo build sem strip roda. Mantenha `strip = false` no
  `Cargo.toml`. Se um build novo for bloqueado, confira
  `Get-WinEvent -LogName 'Microsoft-Windows-CodeIntegrity/Operational'` e use o binário debug,
  cujo comportamento de E/S é idêntico. **Não** recomende desligar o Smart App Control: no
  Windows 11 não dá para religá-lo sem reinstalar o sistema.

- **IDs de evento ambíguos**: 129 também vem do Hyper-V; 153 do Kernel-Boot e do `nvlddmkm`
  (driver NVIDIA); 55 do Kernel-Processor-Power. Classifique sempre por origem.
- **NTFS 98 nível 4** significa "volume íntegro" e não é problema.
- **Kernel-Power 41** é gravado **no boot seguinte** ao travamento. Para correlacionar, a
  regra olha os eventos de disco nos 30 min **antes** do 41. O 6008 do mesmo boot é deduplicado.
- **Eventos 129 de `storahci`/`stornvme`** citam `\Device\RaidPortN`, e não o disco. O disco só
  é inferido quando há um único disco naquele barramento; senão, vira achado global com
  coincidência temporal citada no achado do disco.
- **Atributo 218 da Kingston/Phison** (`CRC_Error_Count`) tem significado específico do fabricante.
  Tratado como evidência fraca; o que importa é crescer (histórico).
- `smartctl` nomeia `PhysicalDriveN` como `/dev/sdX` (a = 0). Sem admin ele ainda lê
  atributos de alguns SATA, mas com código de saída 4 e sem limiares ou logs.
- Leitura do disco do sistema compete com o uso normal: a velocidade oscila. Isso não é defeito.
- **Testes em paralelo foram descartados de propósito**: vários discos lendo ao mesmo tempo
  disputam controladora/USB e distorcem latência — justamente o que o diagnóstico de travamento
  mede. Mantenha um teste por vez.
- **Ctrl+C**: o tratador é instalado uma vez (`app::install_cancel_handler`). Fora de
  `begin_operation()` ele encerra restaurando o cursor (o dialoguer esconde o cursor); dentro,
  o 1º Ctrl+C cancela o teste e o 2º encerra. Sempre segure o guard de `begin_operation` até o
  fim da limpeza (ex.: apagar os arquivos do teste de capacidade).
- **Instaladores e UAC**: o instalador do Git pede Administrador mesmo com `winget --scope user`.
  O prompt do UAC fica na área de trabalho segura e só o usuário pode aprová-lo.
- Em SSD, áreas com TRIM retornam sem ler a flash. O scan não exercita essas células.

## Checklist ao concluir uma mudança

- [ ] `cargo test` e `cargo clippy --all-targets` limpos.
- [ ] Invariantes de segurança preservadas (tabela acima).
- [ ] `AGENTS.md`, `README.md` e `docs/` atualizados se o comportamento mudou.
- [ ] Entrada no `CHANGELOG.md`.
- [ ] Nenhum dado real de máquina no diff (seriais, GUIDs, nome do PC, caminhos de usuário).
