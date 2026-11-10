# AGENTS.md — guide for agents working on diskdoctor

> Read this file before changing the code. It is the project map, lists the safety
> invariants and explains how to extend things. **Keep it up to date** in the same change
> that alters behavior (and record it in `CHANGELOG.md`).

## What it is

`diskdoctor` is a CLI for Windows 10/11, written in Rust, that diagnoses HDDs and SSDs
(SATA/NVMe), external USB drives, USB flash drives and memory cards, focused on freezes,
timeouts, read errors, communication failures and counterfeit capacity. It correlates SMART
(through `smartctl`), Windows reliability counters, System log events and its own tests
(Surface Scan, Benchmark, Capacity Verification). For each drive it produces findings with a
likely cause and a confidence level, and it keeps a history to compare before × after changes.

- Everything in the repository is in **English**: code, identifiers, comments, docs, commit messages.
- The **interface** is bilingual: **English (default)** and **Português (Brasil)** (`src/i18n.rs`).
  Every user-facing string is written inline in both languages with `t("en", "pt")` (static) or
  `tr!("en {x}", "pt {x}")` (formatted). Both format strings must use the same arguments.
- **Technical names stay in English in both languages**: SMART, Surface Scan, Benchmark,
  Sequential Read, Random 4 KiB, QD1, Capacity Verification, plus acronyms (LBA, CRC, UNC, TRIM,
  IOPS, NVMe…). Do not translate them in pt-BR strings.
- Language selection, highest priority first: `--lang`, `DISKDOCTOR_LANG`, `settings.json`
  (written by `diskdoctor language` and by the menu), English.
- Menu: every action shows an approximate duration (Quick / Long / Instant, Rápido / Longo /
  Imediato) and never promises a maximum time. Keep that visible on narrow terminals.
- With no arguments, it opens an **interactive menu** (`interactive.rs` + `menu.rs`) that only builds
  and runs CLI command lines.
- Current status: see `CHANGELOG.md`. The repository is **public**: never commit real machine data
  (serials, volume GUIDs, computer name, user paths, volume labels).

## Build, tests and running

```powershell
# Rust installed through rustup (stable MSVC toolchain). In new shells:
$env:Path = "$env:USERPROFILE\.cargo\bin;" + $env:Path

cargo build                     # debug
cargo build --release           # binary at target\release\diskdoctor.exe (~4 MB)
cargo test                      # unit + integration (tests\cli.rs); no admin needed
cargo clippy --all-targets      # must stay warning-free (CI uses -D warnings)
```

- Tests **never touch a real drive**: the engines use simulated devices
  (`io::testing::ScriptedReader`/`ScriptedWriter`, fake devices in `capacity::tests`) and the parsers
  use fixtures in `tests/fixtures/`.
- Language in tests: the process default is English. To check a language without races between
  parallel tests, use the per-thread guard `let _pt = i18n::scoped(Lang::PtBr);`. Integration tests
  run the binary with an isolated `DISKDOCTOR_DATA_DIR` and without `DISKDOCTOR_LANG`.
- `cargo test --test capacity_win -- --ignored` exercises the native Capacity Verification path with
  ~64 MiB of files in a temporary folder (ignored by default).
- The logic builds and is tested on any OS. Win32 code is behind `#[cfg(windows)]`, with stubs in
  `platform/unsupported.rs`.
- To run without polluting the user's data, use `DISKDOCTOR_DATA_DIR=<temporary folder>`.
- `scan` and `benchmark` require an Administrator terminal. `list` and `events` work without
  elevation; `health` works without elevation, with partial SMART.
- Real drive tests (Surface Scan, Benchmark, Capacity Verification) on a developer machine: never
  target the system drive while it is in use; prefer a secondary volume.

## Repository, commits and releases

- Public repository: <https://github.com/marcoshmendes/diskdoctor> (MIT license, `LICENSE`).
- Commits use the GitHub **noreply** e-mail, configured for this repository only
  (`git config user.email "<id>+<login>@users.noreply.github.com"`). Never use a personal e-mail.
- CI: `.github/workflows/ci.yml` (clippy `-D warnings` + tests on `windows-latest`) on every push to
  `main` and on pull requests.
- Release: update `version` in `Cargo.toml` and `CHANGELOG.md`, commit, then
  `git tag -a vX.Y.Z -m "diskdoctor X.Y.Z" && git push origin vX.Y.Z`. `release.yml` runs the tests,
  builds on GitHub and publishes `diskdoctor-vX.Y.Z-windows-x64.zip` + `.sha256` to Releases.
- Do not publish locally built binaries: they carry machine paths (e.g. `C:\Users\<name>\.cargo\...`).
- If `git push` asks for a user/password, run `gh auth setup-git` (uses the GitHub CLI login).

## Code map

| File | Responsibility |
|---|---|
| `src/main.rs` | Only calls `app::run()`. |
| `src/app.rs` | Resolves the language before parsing, CLI parsing (`parse_cli`), localization of clap messages (`localize_clap_text`), `dispatch`, **exit codes** (`app::exit`), Ctrl+C (`begin_operation`). |
| `src/i18n.rs` | `Lang`, `t()`, `tr!`, the per-thread test guard `scoped`, `settings.json` (`save_language`), priority resolution (`resolve`). Declared first in `lib.rs` with `#[macro_use]`. |
| `src/cli.rs` | clap definition. Help texts use `help = t(...)`, evaluated when the command is built, so `--help` follows the language. `command` is `Option`: no command → menu. Global `--lang`. |
| `src/interactive.rs` | Interactive mode: header, drive panel, main menu (sections, shortcuts 0–9/q, duration per item), drive pickers (drive number as shortcut, per-drive estimate for Surface Scan/Capacity Verification). Each action becomes CLI argv ("Running: …") and runs through `app::dispatch`. The panel cache is display-only; actions that pick a drive reload the inventory. Offers to reopen as Admin. |
| `src/menu.rs` | Keyboard lists and prompts (`select`, `confirm`, `pause`): one write per frame, lines cut to the terminal width, redraw that accounts for rows re-wrapped after a resize, viewport scrolling, cursor hidden while open and always restored. `render` is pure and unit-tested. |
| `src/commands/*.rs` | One file per command (`list`, `health`, `events`, `scan`, `benchmark`, `capacity`, `history`, `tag`, `report`, `language`). `commands/mod.rs` has `Ctx`, `load_disks()` (inventory **+ tags**), `select_disk()` (number or tag), `collect()` (health/report collection) and `record_diagnoses()`. |
| `src/inventory.rs` | Drives through PowerShell/CIM (`Get-Disk`, `Get-PhysicalDisk`, `Win32_DiskDrive`, `Get-Partition`, `Get-Volume`); `DiskInfo` (with `kind`: internal/external/flash drive/card and `tags`), `DiskIdentity`, `disk_title`, volume→drive mapping. |
| `src/tags.rs` | User tags (`tags.json`, by fingerprint), validation, `DiskRef` (`--disk` = number or tag). |
| `src/capacity.rs` | Capacity Verification engine (f3 style): pattern per block, check, verdict (`Genuine`/`Fake`/`Defective`), real capacity estimate. |
| `src/journal.rs` | Per-drive journal: diagnoses and recorded changes; `compare()` before × after. |
| `src/smart.rs` | Finds and runs `smartctl -x --json=c`, **forbidden argument list**, ATA and NVMe parsing → `SmartReport`. Failing attributes use smartctl's own terms (`FAILING_NOW`, `In_the_past`). |
| `src/windows_counters.rs` | `Get-StorageReliabilityCounter` (requires admin). |
| `src/events.rs` | System log query (XPath), classification by **(provider, ID, level)**, event → drive attribution. |
| `src/health.rs` | Aggregates SMART, counters and history per drive (`DiskHealth`). |
| `src/history.rs` | JSONL snapshots of SMART counters; detects counters that **grow** (`PROBLEM_COUNTERS`, `counter_label`). |
| `src/io.rs` | `BlockReader` and `BlockWriter` traits, `IoOutcome`, Win32 error classification and description, simulated devices for tests. |
| `src/scan.rs` | Surface Scan engine: blocks, timeout, bad-block bisection, transient errors, timeline, latencies. Early stops carry a stable `AbortKind` (`ScanMetrics::abort`, with a fallback for 0.2.x results). |
| `src/benchmark.rs` | QD1 Benchmark: Sequential Read in 3 zones plus Random 4 KiB; writes only to a temporary file. Saved labels are canonical English (`SEQ_START`…); `display_label` localizes them (and old 0.2.x labels). |
| `src/stats.rs` | Logarithmic latency histogram (percentiles in constant memory). |
| `src/analysis.rs` | **Correlation engine**: rules → `Finding` with weighted evidence, confidence, causes and recommendations; verdict per drive (`DiskVerdict::change` holds the recorded change line). |
| `src/report/mod.rs`, `report/html.rs` | JSON and self-contained HTML report (inline CSS and SVG, every text escaped, `<html lang>` from `DiagnosticReport::language`). |
| `src/ui.rs` | Terminal: tables (comfy-table), colors, progress bars (indicatif), confirmations (`confirm`, `confirm_typed`, `WRITE_CONFIRMATION`). |
| `src/store.rs` | Data folder (`%LOCALAPPDATA%\diskdoctor`), saved Surface Scan, Benchmark and Capacity Verification results. |
| `src/platform/` | **The only place with `unsafe`/Win32 and external processes**: `win.rs` (RawDisk, TempBenchFile, CapacityDir, elevation, console VT mode, QueryDosDevice, ShellExecute to reopen as Admin and open the report), `powershell.rs`, `process.rs`. |
| `src/util.rs` | Localized number and date formatting (`int`, `decimal`, `format_bytes`, `local_datetime`…), serial masking, `parse_size`, HTML escaping, RNG. |

Data flow: `inventory` → (`health` + `events` + saved `scan`/`benchmark`/`capacity` results)
→ `analysis::analyze` → `ui` (terminal) or `report` (HTML/JSON).

## Safety invariants (DO NOT break)

| Invariant | Where it is enforced | Test that protects it |
|---|---|---|
| The physical drive is opened **only with `GENERIC_READ`**; there is no `BlockWriter` for a physical drive. | `platform/win.rs::RawDisk::open`, trait split in `io.rs`. | Review; the type system prevents writes. |
| Writes go only to a **new** file (`CREATE_NEW`), deleted on close (`FILE_FLAG_DELETE_ON_CLOSE`), in a folder on a volume **of the tested drive**, after the user **types `WRITE`** (there is no `--yes` for writes). | `platform/win.rs::TempBenchFile`, `commands/benchmark.rs::validate_write_target`, `ui::WRITE_CONFIRMATION`. | `benchmark::tests::write_benchmark_stays_inside_temp_file`, `tests/cli.rs::write_test_cannot_be_combined_with_read_only`. |
| Capacity Verification: only creates a **new** folder and `CREATE_NEW` files inside it; only reopens and deletes what it created; removes the folder with `remove_dir` (never recursive); refuses the system drive; requires **typing `WRITE`** and rejects `--json`/`--yes`; warns about the risk on a fake device. | `platform/win.rs::CapacityDir`, `commands/capacity.rs`. | `capacity::tests::*` (the simulated target fails if the engine recreates or reopens a foreign file), `tests/capacity_win.rs` (ignored), `tests/cli.rs::capacity_refuses_json_mode`. |
| The confirmation word is the same in every language (`WRITE`), so instructions never diverge. | `ui::WRITE_CONFIRMATION`. | `tests/cli.rs::capacity_help_explains_what_is_written`. |
| The interactive menu never skips confirmations: it runs the same CLI command line. | `interactive.rs::run_args` → `app::dispatch`. | Review. |
| Files diskdoctor writes on its own stay in the data folder (`tags.json`, `settings.json`, `journal\`, `history\`, results). | `tags.rs`, `i18n.rs`, `journal.rs`, `history.rs`, `store.rs`. | `tags::tests::store_round_trip_and_missing_or_corrupt_file`, `i18n::tests::resolve_follows_priority`. |
| `smartctl` only gets query arguments: no `-t`, `-X`, `-s`, `-o`, `-S`, `--set`, nor log resets. | `smart.rs::ensure_read_only_args` (called before every run). | `smart::tests::rejects_destructive_or_mutating_args`. |
| Never format, partition, run `chkdsk /f` or change existing files. diskdoctor only **recommends** `chkdsk X:` (read-only). | There is no code for that. | — |
| Confirmation and backup warning before long tests (`scan`); `--yes` is for scripts only. | `commands/scan.rs`. | — |
| The full serial never appears in output or reports; use `serial_masked` and `fingerprint`. | `util::mask_serial`, `inventory.rs`, `smart.rs`. | `util::tests::masks_serials`, `report::html::tests::renders_and_escapes_everything`. |
| Every text that comes from the system is escaped in the HTML. | `report/html.rs::e`. | `renders_and_escapes_everything`. |
| Normal SMART does **not** mean a healthy drive: the verdict always lists gaps and the warning note. | `analysis.rs::analyze`, `analyze_disk` (`gaps`, `notes`). | `analysis::tests::clean_data_is_healthy_but_never_guaranteed`. |
| If I/O hangs (not even the cancel completes), the buffer and `OVERLAPPED` are **leaked** on purpose (the kernel may still write to them). | `platform/win.rs::OverlappedHandle` (`poisoned`). | `scan::tests::hung_device_aborts_scan` (engine). |
| Logic never depends on translated text: rule ids, `AbortKind`, `FAILING_NOW`, canonical benchmark labels and `RowKind` are stable. The language never changes a verdict. | `analysis.rs`, `scan.rs`, `smart.rs`, `benchmark.rs`. | `analysis::tests::findings_are_written_in_the_active_language`. |

## Conventions

- `unsafe` only in `src/platform/win.rs`. New Win32 access goes there, behind a safe wrapper.
- External processes only through `platform::process::run_with_timeout`, always with a timeout.
- Helper queries on Windows use `CREATE_NO_WINDOW`: they must not open consoles or steal focus.
  The explicit relaunch as Administrator stays visible.
- The menu keeps the scrollback and the last selection, never clears the screen, writes each frame
  at once and cuts lines to the terminal width. New lists go through `menu::select`, not dialoguer
  (dialoguer is only used for free-text input).
- Errors: `anyhow` with localized context (`.with_context(|| tr!("could not …", "não foi possível …"))`).
  Expected errors become a specific exit code, never a panic.
- New strings: always both languages, English first. Keep pt-BR natural, but keep technical names in
  English. Format numbers and dates only through `util` (they follow the language).
- PowerShell scripts run on **Windows PowerShell 5.1** (`powershell.exe`), not `pwsh`. Project every
  property with an explicit cast (`[string]`, `[uint64]`) and use `ConvertTo-Json -InputObject @($x)`
  so single-element arrays stay arrays.
- Windows enums may arrive as a name (`"SATA"`) or a number (`"11"`); the parsers accept both.
- Windows event messages come in the system display language; patterns that match message text
  also accept Portuguese words (`events.rs`).
- Run `cargo fmt` only on the files you change (the codebase is not fully fmt-formatted); keep `clippy` clean.

## How to extend

**New diagnostic rule** (`analysis.rs`):
1. Create `fn rule_xxx(ctx: &DiskCtx) -> Option<Finding>` using `Builder::new("area.name", t("Title", "Título"))`.
2. Add evidence with `b.add(source, weight, severity, tr!(...))`. Weights: 1 weak, 2 moderate,
   3 strong, 4 very strong, 5 conclusive. Confidence comes from `confidence_for` (see `docs/DIAGNOSTICS.md`).
3. Add `causes(...)` and `recs(...)` in both languages, actionable and **non-destructive**.
4. Register the rule in the list in `analyze_disk` and write a test in `analysis::tests`.
5. Document the rule in `docs/DIAGNOSTICS.md`.

**New Windows event** (`events.rs`): add the ID to `QUERY_EVENT_IDS`, the variant to `EventKind`
(with `description` and `category`) and the (provider, ID) pair to `classify`. Test in
`events::tests::classifies_by_provider_and_id`. Beware: the same ID exists for several providers.

**New SMART attribute** (`smart.rs`): add the field to `SmartReport`, extract it in
`parse_smartctl_json` (prefer matching the attribute **name**, which comes from smartctl's drivedb,
over the ID alone, because IDs vary between vendors), include it in `counters()` if it is a problem
counter and in `history::PROBLEM_COUNTERS` (with English and pt-BR labels).

**New command**: variant in `cli::Command` + `Args` with `help = t(...)` texts, a file in
`src/commands/`, dispatch in `app::dispatch`, a test in `tests/cli.rs`, a section in `README.md` and,
if it makes sense, an entry in `interactive::main_menu` (which only builds the command line, with a
shortcut and a duration). To pick a drive, use `Option<DiskRef>`/`DiskRef` and `commands::select_disk`
— tags then work for free. Long, cancellable tests: `let _op = app::begin_operation();`.

**New device type**: `inventory::infer_device_kind` + label in `DiskInfo::class_label` (both
languages) + a case in `inventory::tests::classifies_external_flash_and_cards`.

**New language**: add a `Lang` variant, its code/name/parse, and turn `t`/`tr!` into a match on the
language (every call site already passes all translations as arguments).

## Fixtures (`tests/fixtures/`)

- `inventory.json`: real output of the inventory script (3 SATA SSDs), with serials, volume GUIDs and
  labels replaced by fictitious values (`00000000-0000-4000-8000-000000000104`…).
- `smartctl-sata-ssd-*.json`: real `smartctl -x --json=c` outputs without admin, serials replaced.
- `smartctl-nvme-warning.json`, `smartctl-sata-hdd-failing.json`: **synthetic**, to cover NVMe and an
  HDD with pending sectors, UNC errors and a failed self-test.
- `events.json`: real subset of the System log (Portuguese Windows). It has 4× `disk/153` of drive 2,
  false positives (Hyper-V 129, Kernel-Boot 153, Processor-Power 55), NTFS 98 at levels 2 and 4,
  NTFS 7, Kernel-Power 41 and EventLog 6008.

To regenerate: run the module's script in `powershell.exe` 5.1 and, before saving, **replace serials,
WWN, volume GUIDs, volume labels, computer name and user paths** with fictitious values. The
repository is public. Check with `git grep -iE "users\\\\|<your-user>|<pc-name>"` before committing.

## Exit codes (`app::exit`)

| Code | Meaning |
|---|---|
| 0 | Ran; no problem of warning or critical severity. |
| 1 | Runtime error. |
| 2 | Command-line usage error. |
| 3 | Requires an Administrator terminal. |
| 4 | Cancelled (Ctrl+C). |
| 5 | Ran, and problems were found (warning or critical, or Surface Scan errors). |
| 6 | The user declined the confirmation. |

## Local data and environment variables

- `%LOCALAPPDATA%\diskdoctor\` (or `DISKDOCTOR_DATA_DIR`):
  - `scans\scan-<fp>-<date>.json` and `benchmarks\bench-<fp>-<date>.json`. `report` automatically
    includes the latest of each drive from the last 30 days.
  - `history\<fingerprint>.jsonl`: SMART snapshots (at most 500 lines per drive).
  - `journal\<fingerprint>.jsonl`: diagnoses and recorded changes (at most 2000 lines; changes are
    never dropped).
  - `capacity\capacity-<fp>-<date>.json`: Capacity Verification results.
  - `tags.json`: tags by fingerprint (written through a temporary file + rename).
  - `settings.json`: saved interface language (`{"language": "pt-BR"}`), same atomic write.
- Capacity Verification: temporary folder `<volume>\diskdoctor-capacity-YYYYMMDD-HHMMSS\` with
  `0001.ddcap`, `0002.ddcap`…, deleted at the end.
- Interactive-mode reports: `%USERPROFILE%\Documents\diskdoctor\diskdoctor-report-*.html`.
- `DISKDOCTOR_LANG`: `en` or `pt-BR` (overrides the saved choice; `--lang` overrides both).
- `DISKDOCTOR_SMARTCTL`: explicit path of `smartctl.exe`.
- `NO_COLOR`: disables colors (same as `--no-color`).
- `--json` (global): structured output on stdout, for agents and scripts. Field names are stable;
  human-readable texts follow the language.

## Known pitfalls

- **Smart App Control (Windows 11)**: with it on, the release `.exe` built with `strip = true` was
  blocked ("An Application Control policy has blocked this file", CodeIntegrity event 3077). The same
  build without strip runs. Keep `strip = false` in `Cargo.toml`. If a new build is blocked, check
  `Get-WinEvent -LogName 'Microsoft-Windows-CodeIntegrity/Operational'` and use the debug binary,
  whose I/O behavior is identical. Do **not** recommend turning Smart App Control off: on Windows 11 it
  cannot be turned back on without reinstalling the system.
- **Ambiguous event IDs**: 129 also comes from Hyper-V; 153 from Kernel-Boot and `nvlddmkm` (NVIDIA
  driver); 55 from Kernel-Processor-Power. Always classify by provider.
- **NTFS 98 level 4** means "volume is healthy" and is not a problem.
- **Kernel-Power 41** is written **at the next boot** after the hang. To correlate, the rule looks at
  drive events in the 30 min **before** the 41. The 6008 of the same boot is deduplicated.
- **Events 129 from `storahci`/`stornvme`** cite `\Device\RaidPortN`, not the drive. The drive is only
  inferred when there is a single drive on that bus; otherwise it becomes a global finding, with the
  time coincidence cited in the drive's finding.
- **Kingston/Phison attribute 218** (`CRC_Error_Count`) has a vendor-specific meaning. It is treated
  as weak evidence; what matters is growth (history).
- `smartctl` names `PhysicalDriveN` as `/dev/sdX` (a = 0). Without admin it still reads attributes of
  some SATA drives, but with exit code 4 and no thresholds or logs.
- Reading the system drive competes with normal use: the speed varies. That is not a defect.
- **Parallel tests were ruled out on purpose**: several drives reading at once compete for the
  controller/USB and distort latency — exactly what the freeze diagnosis measures. Keep one test at a time.
- **Ctrl+C**: the handler is installed once (`app::install_cancel_handler`). Outside
  `begin_operation()` it exits restoring the cursor (menus hide it); inside, the 1st Ctrl+C cancels
  the test and the 2nd exits. Always hold the `begin_operation` guard until cleanup ends (e.g.
  deleting the Capacity Verification files).
- **VT sequences**: the `console` crate only enables `ENABLE_VIRTUAL_TERMINAL_PROCESSING` when colors
  are on. `menu.rs` calls `platform::enable_virtual_terminal()` so cursor movement works with
  `NO_COLOR`/`--no-color` too.
- **Installers and UAC**: the Git installer asks for Administrator even with `winget --scope user`.
  The UAC prompt is on the secure desktop and only the user can approve it.
- On SSDs, TRIMmed areas return without reading the flash. The Surface Scan does not exercise those cells.

## Checklist before finishing a change

- [ ] `cargo test` and `cargo clippy --all-targets -- -D warnings` clean.
- [ ] Safety invariants preserved (table above).
- [ ] New user-facing strings exist in both languages, technical names in English.
- [ ] `AGENTS.md`, `README.md` and `docs/` updated if behavior changed.
- [ ] Entry in `CHANGELOG.md`.
- [ ] No real machine data in the diff (serials, GUIDs, PC name, user paths).
