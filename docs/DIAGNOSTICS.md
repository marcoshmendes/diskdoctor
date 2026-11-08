# Motor de diagnóstico (correlação)

Código: `src/analysis.rs`. Cada regra junta **evidências** e, se houver ao menos uma, gera um
**achado** (`Finding`) com severidade, confiança, possíveis causas e recomendações.

## Evidências, pesos e confiança

| Peso | Significado | Exemplos |
|---|---|---|
| 1 | fraca | contador CRC do fabricante, poucos timeouts na vida do disco |
| 2 | moderada | CRC de interface (199), eventos 153 em pequena quantidade |
| 3 | forte | setores pendentes, evento 129 atribuído, timeouts no scan |
| 4 | muito forte | atributo SMART falhando agora, aviso crítico NVMe |
| 5 | definitiva | SMART FAILED, setores ilegíveis confirmados pelo scan, dispositivo travou |

Eventos ganham peso com a quantidade (+1 com 5 ou mais, +2 com 20 ou mais, teto 4) e perdem 1
quando o disco foi apenas inferido pelo driver.

**Confiança** (`confidence_for`):
- **Alta**: alguma evidência de peso 5, **ou** peso total ≥ 6 vindo de ≥ 2 fontes independentes.
- **Média**: peso total ≥ 3.
- **Baixa**: o restante.

Fontes: SMART, Contadores do Windows, Eventos do Windows, Teste de leitura, Benchmark,
Histórico SMART, Verificação de capacidade.

**Mudança registrada** (`diskdoctor history note`): a mais recente de cada disco vira ponto de
corte. Eventos anteriores a ela não entram nas regras, e o crescimento dos contadores SMART é
medido a partir dela. O veredito cita a mudança e, se ela tiver menos de 24 h, lista como lacuna
que ainda é cedo para avaliar.

**Status do disco** = pior severidade entre os achados: Crítico, Alerta, Atenção, ou
"Sem problemas detectados". Sem nenhuma fonte de dados, o status é "Indeterminado". Achados
só informativos mantêm "sem problemas".

## Regras por disco

| Regra (`rule`) | Título | Principais evidências |
|---|---|---|
| `smart.failed` | O próprio disco reporta falha | `smart_status.passed=false` (5); atributo `when_failed=now` (4); bits do `critical_warning` NVMe |
| `media.degradation` | Setores ilegíveis ou mídia degradada | 197 pendentes (3), 198 offline (3), 187 (2), 5 realocados (1–2), blocos ruins adquiridos, falhas de programação/apagamento, erros de mídia NVMe (3), autoteste com falha (3), log ATA com UNC (2), **crescimento** no histórico (2–3), contadores do Windows (2), eventos 7/154/52/51, **scan com regiões ilegíveis (5)** |
| `comm.link` | Falhas de comunicação, timeouts ou travamentos | 199 CRC (2), CRC do fabricante (1), PHY SATA, log SATA Phy (2), 188 timeouts (Info se ≤ 2, 1 se ≥ 3, 2 se ≥ 10), 184 (2), crescimento (2–3), eventos 129/9/11/153/15/157, **padrão de LBAs dos 153**, reset sem disco identificado próximo de eventos do disco (2), scan: timeouts (3), erros transitórios (2), travamento (5), log de erros ATA só com **ICRC** (2; com UNC conta em `media.degradation`). Vira **Crítico** com peso ≥ 8 vindo de ≥ 2 fontes. Se o disco SATA reporta contadores de CRC e **todos estão zerados** apesar dos eventos, adiciona nota (peso 0) e reordena as causas: pausas do próprio disco/firmware e alimentação antes do cabo de dados |
| `perf.stalls` | Leituras muito lentas ou pausas | scan: leituras > 500 ms (2–3), pico ≥ 150 ms (1), região < 35% da mediana (1); benchmark aleatório ≥ 1 s (2); latência máxima do Windows |
| `temp.high` | Temperatura elevada | atual ≥ aviso/crítico (HD 50/55, SSD SATA 65/75, NVMe limiares do próprio disco ou 70/80); máxima registrada; minutos NVMe acima do limite |
| `ssd.wear` | Desgaste avançado | vida consumida ≥ 75% (2) / ≥ 90% (3); reserva ≤ limite (4) |
| `power.unsafe` | Desligamentos bruscos frequentes | ≥ 50 e ≥ 5% dos ciclos de energia; crescimento ≥ 3 no histórico |
| `fs.corruption` | Inconsistências no sistema de arquivos | NTFS 55 (Alerta se ≥ 5, senão Atenção), 98 nível ≤ 3, 50/57/140 (falha de flush), 7 (gravação interrompida); cita os volumes e partições afetados |
| `freeze.correlation` | Problemas do disco antes de travamentos | eventos de E/S do disco nos 30 min anteriores a Kernel-Power 41 (ou 6008 sem 41 no mesmo boot) |
| `capacity.fake` | Capacidade falsa | verificação de capacidade com dados sobrescritos, ou perdidos concentrados no fim (5, Crítico) |
| `capacity.errors` | Falhas ao gravar ou ler de volta os dados de teste | dados corrompidos/ilegíveis/perdidos espalhados (5), falhas de gravação (4), verificação interrompida (4) |

## Antes × depois (`journal::compare`)

Para cada mudança registrada, compara janelas de mesma duração (o tempo desde a mudança, até 14
dias) antes e depois dela:

| Efeito | Regra (eventos de E/S do disco: antes `b`, depois `a`) |
|---|---|
| Cedo demais | menos de 24 h desde a mudança |
| Melhorou | `b ≥ 3` e `a ≤ b/5` |
| Piorou | `a ≥ 1,5·b + 2`, ou `b < 3` e `a ≥ 3` |
| Sem mudança clara | o restante com `b ≥ 3` |
| Não dá para medir | `b < 3` e `a = 0` — aí decidem os contadores SMART (crescimento antes e nenhum depois = melhorou) |

Também mostra reinicializações inesperadas (Kernel-Power 41), o último veredito de cada lado e
quais contadores SMART cresceram.

### Padrão de LBAs nos eventos 153

Com 5 ou mais eventos de retry que citam LBA:
- endereços **distintos espalhados por ≥ 25% do disco** apontam para link, controladora ou firmware;
- endereços **concentrados** apontam para uma região fraca da mídia.

Os LBAs mais repetidos (3× ou mais) aparecem no texto para serem conferidos no teste de leitura.

## Achados globais (sem disco)

- `global.controller`: eventos de E/S sem disco identificado (ex.: `storahci/129` com vários discos SATA).
- `global.whea`: erros WHEA (PCIe/CPU), relevantes para NVMe.
- `global.unexpected_reboots`: reinicializações inesperadas no período.

## Classificação de eventos (`events.rs`)

| Origem | IDs → tipo |
|---|---|
| `disk` | 7 bloco ruim · 11 erro de controladora · 15 não pronto · 51 erro de paginação · 52 SMART prevê falha · 153 E/S repetida · 154 E/S falhou por hardware · 157 remoção inesperada |
| drivers de porta (`storahci`, `stornvme`, `iaStor*`, `amd_sata`, `nvme*`, `storport`...) | 129 reset do dispositivo · 9 tempo limite · 11 controladora · 15 não pronto |
| `Ntfs` / `Microsoft-Windows-Ntfs` | 55 corrupção · 98 (nível ≤ 3) reparo necessário · 7 gravação interrompida · 50/57/140 falha de flush |
| `Microsoft-Windows-Kernel-Power` | 41 reinicialização sem desligamento correto |
| `EventLog` | 6008 desligamento inesperado |
| `Microsoft-Windows-WHEA-Logger` | 1/17/18/19 erro de hardware |

**Atribuição ao disco**, em ordem: `\Device\HarddiskN` (exata) → "Disco N" no texto do `disk`
(exata) → `Volume{guid}` / `\Device\HarddiskVolumeN` / letra (exata) → único disco no
barramento do driver de porta (inferida).

## Notas sempre presentes

- SMART normal não garante saúde.
- Ausência de eventos não descarta problema no disco do sistema.
- Execução sem Administrador limita os dados.
