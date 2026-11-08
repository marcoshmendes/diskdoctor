# Changelog

Formato: [Keep a Changelog](https://keepachangelog.com/pt-BR/1.1.0/). Datas no formato AAAA-MM-DD.

## [0.2.0] — 2026-11-08

### Adicionado
- **Menu interativo**: `diskdoctor` sem argumentos (ou duplo clique no `.exe`) abre um painel com os
  discos e um menu navegável com as setas. Cada ação mostra o comando equivalente e roda pelo
  mesmo caminho da CLI. Oferece reabrir como Administrador quando o teste precisa.
- **HD/SSD externo, pendrive e cartão de memória**: tipo reconhecido via `Win32_DiskDrive.MediaType`,
  barramento (USB, SD, MMC, FireWire) e modelo. Externos viram HD ou SSD pelo `rotation_rate` do SMART.
- **`diskdoctor capacity`**: verificação de capacidade real (estilo f3write/f3read) com arquivos novos
  numa pasta nova, conferência sem cache, estimativa da capacidade real e limpeza no fim.
  Novas regras `capacity.fake` e `capacity.errors`; seção no relatório.
- **`diskdoctor tag`**: tags por disco ("Studio", "Jogos"), mostradas em toda saída e aceitas em
  `--disk` no lugar do número.
- **`diskdoctor history`** e **`history note`**: linha do tempo por disco (diagnósticos, mudanças,
  testes) e comparação **antes × depois** de cada mudança registrada. Depois de uma mudança, o
  veredito considera só os eventos posteriores, e o crescimento SMART é medido a partir dela.
- Barra de uso por volume em `list` e no painel.
- CI (clippy + testes) e release automatizado no GitHub Actions (`.zip` + SHA-256).
- Publicado em <https://github.com/marcoshmendes/diskdoctor> sob a licença **MIT**.

### Alterado
- Ctrl+C: fora de um teste encerra restaurando o terminal; durante um teste cancela com segurança
  (necessário para vários testes no mesmo processo, no menu).
- README reescrito para quem baixa o `.exe` pronto.
- Fixtures de teste anonimizadas também nos GUIDs de volume e rótulos.

### Corrigido (encontrado com dados reais, como Administrador)
- Registros **ICRC** no log de erros ATA (CRC na interface) eram contados como "mídia degradada";
  agora são evidência de comunicação (`comm.link`), e só erros **UNC** contam como mídia.
- Muitos retries/resets com **todos os contadores de CRC zerados** agora mudam a ordem das causas:
  o próprio disco (pausas de firmware/controlador) e a alimentação vêm antes do cabo de dados.

### Decidido não fazer
- Testes de leitura em vários discos ao mesmo tempo: distorcem latência e poderiam gerar falso
  positivo de lentidão. Um teste por vez.

## [0.1.0] — 2026-11-08

MVP de diagnóstico não destrutivo.

### Adicionado
- Comandos `list`, `health`, `events`, `scan`, `benchmark` e `report`, com ajuda e mensagens em pt-BR e saída `--json`.
- Inventário via PowerShell 5.1/CIM, com seriais mascarados, tipo (SSD/HD × SATA/NVMe/USB) e volumes;
  resolução `Volume{guid}` → `\Device\HarddiskVolumeN` via `QueryDosDevice`.
- SMART via `smartctl -x --json=c` (ATA e NVMe), com lista de argumentos proibidos (sem autotestes
  nem alterações) e decodificação do código de saída.
- Contadores `Get-StorageReliabilityCounter` (temperatura, desgaste, erros, latência máxima).
- Eventos do log System classificados por (origem, ID, nível), atribuídos ao disco por dispositivo,
  número, volume ou inferência pelo driver.
- Teste de leitura com handle somente leitura, E/S sobreposta com tempo limite e `CancelIoEx`,
  detecção de travamento, bisseção de setores ilegíveis, erros transitórios, linha do tempo de
  velocidade e histograma de latência.
- Benchmark QD1 (sequencial em 3 zonas e 4 KiB aleatório); escrita opcional em arquivo temporário
  `CREATE_NEW` + `DELETE_ON_CLOSE`, com confirmação digitada.
- Motor de correlação com evidências ponderadas, confiança, causas e recomendações; correlação de
  eventos de disco com reinicializações inesperadas; padrão de LBAs dos eventos 153.
- Histórico local de contadores SMART para detectar crescimento.
- Relatório HTML autocontido (gráfico SVG de velocidade, latências, eventos, limitações) e JSON completo.
- 90 testes automatizados (parsers com fixtures reais anonimizadas, motores com dispositivos simulados, CLI).
- Documentação: `README.md`, `AGENTS.md`, `docs/ARCHITECTURE.md`, `docs/DIAGNOSTICS.md`.
