# Arquitetura e limitações técnicas

## Visão geral

```
                    ┌────────────────────────── diskdoctor.exe ──────────────────────────┐
  sem argumentos ►  │ interactive (menu dialoguer) ─┐                                    │
  CLI (clap) ────►  │ app::dispatch ◄───────────────┘                                    │
                    │ commands/*  ──► coleta ──► analysis::analyze ──► ui / report       │
                    │                  │                                                 │
                    │   inventory ─────┤ PowerShell 5.1 + CIM (Get-Disk, Win32_DiskDrive…)│
                    │   smart ─────────┤ smartctl -x --json=c (processo externo)          │
                    │   windows_counters┤ Get-StorageReliabilityCounter                   │
                    │   events ────────┤ Get-WinEvent -FilterXPath (log System)           │
                    │   scan/benchmark ┤ platform::win::RawDisk (Win32, E/S sobreposta)   │
                    │   capacity ──────┤ platform::win::CapacityDir (arquivos novos)      │
                    │   history/journal┤                                                 │
                    │   tags/store ────┘ %LOCALAPPDATA%\diskdoctor (JSON/JSONL)           │
                    └─────────────────────────────────────────────────────────────────────┘
```

As camadas:

1. **Entrada** (`app`, `cli`, `commands`, `interactive`): interpreta argumentos, pede
   confirmações e transforma resultados em códigos de saída. O menu interativo não tem lógica
   própria: monta a linha de comando equivalente, mostra ao usuário e executa por
   `app::dispatch`, então confirmações e travas são as mesmas da CLI.
2. **Coleta** (`inventory`, `smart`, `windows_counters`, `events`, `health`): cada fonte vira
   uma struct serializável. Parsers são funções puras sobre texto JSON, testadas com fixtures reais.
3. **Testes ativos** (`scan`, `benchmark`, `capacity`): motores genéricos sobre os traits
   `BlockReader`, `BlockWriter`, `VerifyTarget`/`VerifyFile` (`io.rs`). Não conhecem o Windows,
   o que permite testá-los com dispositivos simulados que injetam erros, lentidão, timeouts,
   travamentos e capacidade falsa.
4. **Análise** (`analysis`, `history`, `journal`): regras independentes que juntam evidências
   ponderadas de várias fontes; histórico SMART e diário de diagnósticos/mudanças para comparar
   antes × depois. Detalhes em [`DIAGNOSTICS.md`](DIAGNOSTICS.md).
5. **Saída** (`ui`, `report`, `store`, `tags`): terminal pt-BR, relatório JSON e HTML
   autocontido, persistência local dos testes e das tags.
6. **Plataforma** (`platform`): único ponto com `unsafe`, Win32 e processos externos.

## Tipos de dispositivo

`inventory::infer_device_kind` combina o barramento (`Get-Disk.BusType`), o
`Win32_DiskDrive.MediaType` ("Fixed hard disk media", "External hard disk media",
"Removable Media") e o modelo:

| Tipo | Como é reconhecido | Rótulo |
|---|---|---|
| Interno | barramento SATA/NVMe/SAS… e mídia fixa | `SSD SATA`, `HD SATA`, `SSD NVMe` |
| Externo | barramento USB/1394 ou "External hard disk media" | `HD externo USB`, `SSD externo USB`, `Disco externo USB` |
| Pendrive | "Removable Media" | `Pendrive USB` |
| Cartão | barramento SD/MMC, ou removível/USB com modelo de leitor (`SD`, `MMC`, `Card`, `Reader`…) | `Cartão de memória` |

Discos externos raramente dizem ao Windows se são HD ou SSD; quando o SMART passa pelo case
USB, o `rotation_rate` do `smartctl` resolve (0 = SSD).

## Verificação de capacidade (estilo f3)

```
diskdoctor ─► create_dir("<X>:\diskdoctor-capacidade-AAAAMMDD-HHMMSS")   (falha se existir)
          ─► CreateFileW("0001.ddcap", CREATE_NEW, NO_BUFFERING | WRITE_THROUGH | OVERLAPPED)
          ─► grava blocos de 1 MiB: cada 4 KiB = "DDCAPv01" + semente + posição + pseudoaleatório
          ─► fecha; reabre só para leitura (OPEN_EXISTING, NO_BUFFERING) e confere cada 4 KiB
          ─► apaga os arquivos que ele mesmo criou e a pasta (remove_dir, nunca recursivo)
```

- Pedaço com cabeçalho de **outra posição** = o dispositivo sobrescreveu dados (falso clássico).
- Pedaço **vazio** ou de outra execução (semente diferente) = gravação perdida; se isso se
  concentra no fim, é o falso que descarta gravações acima da capacidade real.
- Corrupção espalhada ou erro de E/S = dispositivo com defeito ou mau contato.
- Capacidade real estimada = espaço já ocupado + bytes lidos de volta corretamente.
- O disco do sistema é recusado (encher o espaço livre travaria o Windows).
- Risco inerente, avisado ao usuário: num dispositivo falso, gravar além da capacidade real pode
  corromper o que já está nele (o controlador sobrescreve endereços baixos).

## Tags e histórico

- `tags.json`: fingerprint → tags. `--disk` aceita número ou tag (`tags::DiskRef`); uma tag
  precisa estar em um único disco para ser usada como seletor e não pode ser só dígitos.
- `journal\<fp>.jsonl`: diagnósticos (`health`/`report`) e mudanças (`history note`). A última
  mudança vira o ponto de corte: eventos anteriores não entram no veredito, e o crescimento dos
  contadores SMART é medido a partir dela. `journal::compare` compara janelas de mesma duração
  antes e depois (até 14 dias de cada lado).

## Por que Rust (e não Node.js)

O objetivo central é diagnosticar **travamentos e timeouts**. Para isso a ferramenta precisa:

- impor **tempo limite por leitura** e **cancelar de verdade** uma E/S presa (`CancelIoEx`).
  No Node, a leitura ocupa uma thread do pool do libuv, que fica presa e não pode ser abortada;
- ler **sem o cache do Windows** (`FILE_FLAG_NO_BUFFERING`), o que exige buffer alinhado ao
  setor. Em Rust é trivial (`Layout` alinhado a 4096); no Node não há garantia;
- chamar `DeviceIoControl` (capacidade, setor lógico e físico) sem addon nativo;
- gerar um `.exe` único, sem runtime, fácil de levar para a máquina com problema.

## Como o teste de leitura acessa o disco

```
diskdoctor ─► CreateFileW("\\.\PhysicalDriveN", GENERIC_READ, NO_BUFFERING | OVERLAPPED)
          ─► disk.sys (driver de classe) ─► partmgr ─► storport + miniport
             (storahci / stornvme / iaStorAC / ...) ─► controladora ─► cabo/slot ─► disco
```

- **Somente leitura garantida pelo SO**: o handle é aberto só com `GENERIC_READ`.
- **Alinhamento**: buffer alinhado a 4096 B; offsets e tamanhos múltiplos do setor lógico.
- **Tempo limite**: cada leitura é sobreposta; `GetOverlappedResultEx` espera até `--timeout-ms`.
  Se estourar, `CancelIoEx` cancela e a leitura conta como **timeout**. Se nem o cancelamento
  conclui em 120 s, o dispositivo é considerado **travado**: o scan aborta e a memória da E/S
  é vazada de propósito, porque o kernel ainda pode escrever nela.
- **Isolamento de setores ruins**: um bloco que falha é dividido ao meio repetidamente até o
  tamanho do setor físico, com orçamento de releituras. Se as metades lerem bem, o erro foi
  **transitório**, o que indica comunicação e não mídia.

## Limitações técnicas de acesso direto a discos no Windows

Estas limitações valem para qualquer linguagem: são do sistema operacional e do hardware.

1. **Privilégio de Administrador.** Abrir `\\.\PhysicalDriveN` e enviar comandos SMART exige
   elevação. Sem ela: `list` e `events` funcionam; `health` lê só o que o `smartctl` consegue
   (em alguns SATA, atributos sem limiares nem logs); `scan` e `benchmark` recusam com código 3.
2. **A pilha de drivers sempre fica no meio.** O que medimos é disco + cabo + controladora +
   driver. Um erro pode vir de qualquer um deles. Isolar o culpado exige trocar componentes
   (cabo, porta, slot) e repetir o teste. A análise sinaliza qual hipótese é mais provável.
3. **O Windows tem o próprio tempo limite** (valor `TimeOutValue` do driver de disco, em geral
   30–60 s) e reseta o link quando ele estoura (evento 129). Um travamento longo pode virar
   erro do driver antes do tempo limite do diskdoctor.
4. **Gravação bruta em volumes montados é bloqueada** desde o Vista. Isso não afeta o
   diskdoctor, que nunca grava no disco físico; o teste de escrita e a verificação de
   capacidade usam arquivos novos dentro do sistema de arquivos.
5. **SMART nem sempre passa.** Controladoras RAID/Intel RST, pontes USB-SATA e hubs podem
   bloquear comandos ATA/NVMe de passagem. Nesses casos o SMART fica "indisponível" e a análise
   se apoia em eventos e no scan.
6. **SSDs remapeiam endereços (FTL).** Um LBA não corresponde a uma posição física fixa, e áreas
   nunca gravadas ou com TRIM podem ser respondidas sem ler a flash. O scan detecta dados
   ilegíveis e lentidão, mas não testa todas as células.
7. **Ler não conserta.** Setores pendentes só são realocados quando regravados. O diskdoctor
   nunca regrava; essa decisão (com backup) fica com o usuário.
8. **O disco do sistema compete com o uso normal.** Velocidade e latência oscilam durante o
   teste; picos isolados não são defeito.
9. **Log de eventos pode perder o que importa.** Se o disco do sistema congela, o Windows nem
   sempre consegue gravar o evento. Por isso a correlação usa também reinicializações
   inesperadas (Kernel-Power 41) e eventos imediatamente anteriores a elas.
10. **SMART normal não prova saúde.** Muitos discos falham sem alterar atributos. O veredito
    "sem problemas detectados" sempre vem acompanhado das lacunas (o que não foi verificado).
11. **Pendrives e cartões não têm SMART**, e muitos cases USB não repassam o do disco. Para eles,
    o teste de leitura e a verificação de capacidade são as fontes de diagnóstico.
12. **A verificação de capacidade testa só o espaço livre** e depende do sistema de arquivos;
    detectar falsificação exige preencher quase todo o espaço livre (o padrão sem `--size`).

## Persistência e privacidade

- Seriais aparecem só mascarados (`S3Z*********1234`). Resultados, histórico, diário e tags
  usam um `fingerprint` (hash FNV-1a de modelo+serial) para reconhecer o mesmo disco entre
  execuções, mesmo que o número do disco mude.
- Tudo fica local em `%LOCALAPPDATA%\diskdoctor`; nada é enviado para a rede.
- O relatório inclui o nome do computador e os modelos dos discos.
- As fixtures de teste vêm de uma máquina real com seriais, GUIDs de volume e rótulos trocados
  por valores fictícios. Ao criar novas, anonimize do mesmo jeito.
