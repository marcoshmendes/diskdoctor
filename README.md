<div align="center">

# diskdoctor

**Descubra se o seu HD, SSD, pendrive ou cartão de memória está com problema — sem arriscar seus dados.**

![Windows 10/11](https://img.shields.io/badge/Windows-10%20%7C%2011-0078D6?logo=windows)
![Rust](https://img.shields.io/badge/feito%20em-Rust-B7410E?logo=rust)
![Somente leitura](https://img.shields.io/badge/discos-somente%20leitura-2ea44f)

[**⬇ Baixar a versão mais recente**](../../releases/latest)

</div>

O PC trava, o jogo congela, o Windows "some" por alguns segundos, ou o pendrive novo de 1 TB
custou barato demais? O **diskdoctor** junta tudo o que o Windows e os próprios discos sabem e diz,
em português, **o que está acontecendo, com que grau de certeza e o que fazer**.

- 🩺 **Diagnóstico completo**: SMART do disco, contadores do Windows e eventos de erro (IDs 7, 51,
  129, 153…), cruzados entre si. Ex.: *"400 operações repetidas em 300 pontos espalhados do disco →
  padrão de cabo/porta, não de setor ruim"*.
- 🔍 **Teste de leitura** do disco inteiro, medindo velocidade, lentidão, erros e travamentos. Só lê.
- 📦 **Verificação de capacidade real** (como o *f3*): descobre pendrive e cartão **falsificados**.
- 🕘 **Histórico antes × depois**: trocou o cabo SATA? Registre e veja se os erros pararam.
- 🏷 **Tags**: chame seus discos de "Studio", "Jogos", "Backup" e use esses nomes nos comandos.
- 📄 **Relatório HTML** para abrir no navegador ou mandar para quem vai consertar o PC.
- Funciona com **HD e SSD SATA, SSD NVMe, HD/SSD externo USB, pendrive e cartão de memória**.

---

## Como usar (sem instalar nada)

1. **Baixe** o arquivo `diskdoctor-vX.Y.Z-windows-x64.zip` em
   [Releases](../../releases/latest) e extraia para uma pasta.
2. **Instale o smartmontools** (recomendado, é o que lê o SMART dos discos). No Terminal:
   ```powershell
   winget install smartmontools.smartmontools
   ```
3. **Dê dois cliques em `diskdoctor.exe`**. Abre um menu; é só escolher com as setas e Enter.

```text
  ┃ diskdoctor 0.2.0
  ┃ Diagnóstico de HDs, SSDs, pendrives e cartões de memória
  ┃ Nada é gravado nos seus discos sem você confirmar.

  ✔ Administrador   ✔ smartctl
╭───┬──────────────────────────────┬──────────────┬────────────┬────────────────────────────────────────╮
│ # ┆ Disco                        ┆ Tipo         ┆ Capacidade ┆ Uso                                    │
╞═══╪══════════════════════════════╪══════════════╪════════════╪════════════════════════════════════════╡
│ 0 ┆ [Jogos] WDC WD10EZEX-08WN4A0 ┆ HD SATA      ┆ 1,0 TB     ┆ E: ██████████░░  85% · 150 GB livres   │
│ 1 ┆ [Windows] Samsung SSD 870    ┆ SSD SATA     ┆ 500 GB     ┆ C: ███████░░░░░  61% · 195 GB livres   │
│ 2 ┆ SanDisk Cruzer Blade         ┆ Pendrive USB ┆ 32,0 GB    ┆ F: █░░░░░░░░░░░   4% · 30,6 GB livres  │
╰───┴──────────────────────────────┴──────────────┴────────────┴────────────────────────────────────────╯

? O que você quer fazer?  (↑ ↓ para escolher, Enter confirma, Esc sai)
❯ Diagnóstico completo         SMART + eventos do Windows + análise · ~30 s
  Teste de leitura (scan)      lê o disco inteiro, sem gravar · Administrador
  Verificar capacidade real    pendrive/cartão falsificado · grava arquivos temporários
  Benchmark de leitura         velocidade e latência · Administrador
  Eventos do Windows           erros, resets e travamentos registrados
  Histórico e antes × depois   registre mudanças (ex.: troca de cabo) e compare
  Tags dos discos              dê nomes como Studio, Jogos, Backup
  Gerar relatório HTML         tudo num arquivo para abrir no navegador
  Sair
```

Cada opção mostra o comando que está rodando (ex.: `Executando: diskdoctor scan --disk 1 --read-only`),
a barra de progresso com velocidade e tempo restante, e no fim explica o resultado.
O teste de leitura e o benchmark precisam de **Administrador**: o menu oferece reabrir o
diskdoctor como Administrador quando você escolhe um deles.

> **"O Windows protegeu o computador"?** O `diskdoctor.exe` não tem assinatura digital paga, então
> o SmartScreen pode avisar. Clique em **Mais informações → Executar assim mesmo**. Com o
> **Smart App Control** do Windows 11 ligado, o arquivo pode ser bloqueado de vez; nesse caso,
> [compile a partir do código](#compilar-a-partir-do-código). Para conferir que o arquivo é o
> original, compare o SHA-256 com o `.sha256` publicado junto:
> `Get-FileHash .\diskdoctor-*.zip -Algorithm SHA256`.

---

## O que cada função faz

| No menu | Comando | Grava algo? | Precisa de Admin? |
|---|---|---|---|
| Diagnóstico completo | `diskdoctor health` | não | recomendado (SMART completo) |
| Teste de leitura | `diskdoctor scan --disk N --read-only` | **não** | sim |
| Verificar capacidade real | `diskdoctor capacity --disk N` | arquivos temporários novos, apagados no fim — pede para digitar `GRAVAR` | não |
| Benchmark de leitura | `diskdoctor benchmark --disk N --read-only` | não | sim |
| Eventos do Windows | `diskdoctor events --hours 48` | não | não |
| Histórico | `diskdoctor history --disk N` | só o histórico local do diskdoctor | não |
| Tags | `diskdoctor tag add --disk N Studio` | só a configuração local do diskdoctor | não |
| Relatório | `diskdoctor report --format html` | o arquivo do relatório | recomendado |
| — | `diskdoctor list` | não | não |

Em qualquer comando, `--disk` aceita o **número** (veja em `diskdoctor list`) **ou uma tag**:
`diskdoctor health --disk studio`.

### Meu PC trava: por onde começo?

1. **Diagnóstico completo** (`diskdoctor health`). Ele já aponta o disco suspeito e o tipo de problema.
2. **Teste de leitura** do disco suspeito, **sozinho** e com o PC sem uso pesado.
3. Se o diagnóstico apontar **cabo/porta**, troque o cabo SATA (ou a porta) e **registre a mudança**:
   `diskdoctor history note --disk 2 "Troquei o cabo SATA"`.
4. Use o PC normalmente por 1–3 dias e rode `diskdoctor health` de novo.
5. `diskdoctor history --disk 2` mostra **antes × depois**: se os erros sumiram, era o cabo.

```text
  Antes × depois da mudança de 08/11/2026 23:10 — "Troquei o cabo SATA" (há 3 dias)
╭───────────────────────────────────┬────────────────────┬───────────────────────────╮
│                                   ┆ Antes              ┆ Depois                    │
╞═══════════════════════════════════╪════════════════════╪═══════════════════════════╡
│ Eventos de E/S do disco           ┆ 412 (137,3/dia)    ┆ 0 (0,0/dia)               │
│ Reinicializações inesperadas (PC) ┆ 3                  ┆ 0                         │
│ Último veredito                   ┆ ALERTA             ┆ SEM PROBLEMAS DETECTADOS  │
│ Contadores SMART que subiram      ┆ Erros CRC +120     ┆ nenhum                    │
╰───────────────────────────────────┴────────────────────┴───────────────────────────╯
  → Melhorou: os problemas caíram de 412 para 0 eventos: a mudança provavelmente resolveu.
```

### Pendrive ou cartão suspeito

`diskdoctor capacity --disk N` (ou **Verificar capacidade real** no menu) grava arquivos de teste no
espaço livre, cada bloco marcado com a própria posição, lê tudo de volta sem cache e apaga os
arquivos. Um dispositivo falso "dá a volta" e sobrescreve o que gravou antes; o diskdoctor detecta
isso e estima a **capacidade real**.

> ⚠️ Num dispositivo falso, gravar além da capacidade real pode corromper o que já está nele.
> **Faça backup antes** — o ideal é testar com o dispositivo vazio. O teste recusa o disco do sistema.

---

## Como ler o resultado

| Status | Significado |
|---|---|
| **Crítico** | Evidência forte de defeito (setores ilegíveis, SMART reprovado, capacidade falsa, dispositivo travando). Faça backup já. |
| **Alerta** | Problema ativo provável (muitas repetições/resets, corrupção recorrente, timeouts). |
| **Atenção** | Sinal fraco ou isolado; acompanhe. |
| **Sem problemas detectados** | Nada encontrado **nas fontes analisadas**. Não é garantia de saúde. |
| **Indeterminado** | Faltaram dados (sem SMART, sem teste de leitura, sem eventos). |

Cada achado traz as **evidências** (e de onde veio cada uma), a **confiança** (baixa/média/alta —
maior quando fontes independentes concordam), as **possíveis causas** e **o que fazer**. O
diagnóstico também lista o que **não** foi verificado.

Rode `diskdoctor health` de vez em quando: o diskdoctor guarda um histórico local e avisa quando um
contador de erro **aumenta**, que é o sinal mais confiável de problema ativo.

---

## Segurança

- O disco é aberto pelo Windows **somente para leitura** em todos os testes de disco.
- Nunca formata, particiona, roda `chkdsk /f`, autotestes SMART nem altera arquivos existentes.
- O `smartctl` só recebe comandos de consulta; comandos de teste ou configuração são bloqueados no código.
- As únicas gravações (opcionais) são **arquivos novos** — benchmark de escrita e verificação de
  capacidade — apagados no fim e só depois de você **digitar `GRAVAR`**.
- Antes de testes longos, avisa sobre backup e pede confirmação.

## Privacidade

- Nada sai do seu computador: não há telemetria nem conexão com a internet.
- Seriais aparecem **mascarados** (`S3Z*********1234`).
- Histórico, tags e resultados ficam em `%LOCALAPPDATA%\diskdoctor`.
- O relatório HTML inclui o nome do computador e os modelos dos discos: revise antes de publicar.

---

## Linha de comando

```powershell
diskdoctor                                   # menu interativo
diskdoctor list                              # discos, tipo, capacidade, volumes e uso
diskdoctor health                            # diagnóstico de todos os discos
diskdoctor health --disk studio --hours 720  # um disco, eventos dos últimos 30 dias
diskdoctor events --hours 48                 # eventos de armazenamento
diskdoctor scan --disk 1 --read-only         # teste de leitura completo (Admin)
diskdoctor scan --disk 1 --offset 100G --length 20G
diskdoctor benchmark --disk 1 --read-only    # velocidade sequencial e 4K aleatório (Admin)
diskdoctor capacity --volume F:              # pendrive/cartão falsificado?
diskdoctor tag add --disk 2 Studio           # dá um nome ao disco
diskdoctor history note --disk 2 "Troquei o cabo SATA"
diskdoctor history --disk 2                  # linha do tempo e antes × depois
diskdoctor report --format both -o C:\relatorios\pc   # pc.html + pc.json
```

- `--json` em qualquer comando: saída estruturada para scripts e outros programas.
- `Ctrl+C` cancela testes com segurança (o resultado parcial é salvo).
- `diskdoctor <comando> --help` mostra todas as opções.

Códigos de saída: `0` ok · `1` erro · `2` uso incorreto · `3` exige Administrador · `4` cancelado ·
`5` problemas encontrados · `6` confirmação recusada.

---

## Compilar a partir do código

Requer Windows 10/11, [Rust](https://rustup.rs) (toolchain MSVC) e Visual Studio Build Tools (C++).

```powershell
git clone https://github.com/marcoshmendes/diskdoctor
cd diskdoctor
cargo build --release
.\target\release\diskdoctor.exe
```

`cargo test` roda a suíte de testes; ela não exige Administrador nem toca nos seus discos.

## Limitações

- O teste de leitura passa por todo o caminho do Windows (driver, controladora, cabo): para isolar o
  culpado é preciso trocar componentes — e o histórico antes × depois existe para isso.
- Controladoras RAID/Intel RST, cases USB e leitores de cartão nem sempre repassam o SMART.
  Pendrives e cartões não têm SMART.
- Em SSDs, áreas vazias (TRIM) respondem sem ler a memória flash.
- A verificação de capacidade testa só o espaço livre.

Detalhes técnicos em [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) e regras de diagnóstico em
[`docs/DIAGNOSTICS.md`](docs/DIAGNOSTICS.md).

## Para desenvolvedores e agentes de IA

Leia [`AGENTS.md`](AGENTS.md): mapa do código, invariantes de segurança, como estender e como testar.

## Licença

[MIT](LICENSE).
