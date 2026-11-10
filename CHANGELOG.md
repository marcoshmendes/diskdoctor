# Changelog

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/). Dates are YYYY-MM-DD.

## [0.3.0] — 2026-11-10

### Added
- **Two interface languages**: English (default) and Português (Brasil). Pick one with menu item `0`,
  `diskdoctor language pt-BR` (saved in `settings.json`), the `DISKDOCTOR_LANG` environment variable
  or `--lang` for a single run. Help, menus, results, diagnoses, errors, confirmations and the HTML
  report are translated; technical names (SMART, Surface Scan, Benchmark, Sequential Read,
  Random 4 KiB, QD1, Capacity Verification) stay in English in both languages. Numbers and dates
  follow the language.
- `diskdoctor language [en|pt-BR]` command (alias `lang`) and a global `--lang` option.
- New interactive menu (`menu.rs`): sections (Diagnostics, Tests, Settings), shortcuts `0`–`9` and `q`,
  a duration class and estimate on every action (Quick / Long / Instant), a description line for the
  highlighted action, and per-drive time estimates in the Surface Scan and Capacity Verification pickers.
  Drive pickers accept the drive number as a shortcut.
- `language` field in the JSON report; `abort` (stable reason) in Surface Scan results; `change` in
  each verdict.

### Changed
- Everything in the repository is now in English: README, AGENTS.md, docs, code comments.
- The typed confirmation word for writes is now `WRITE` (it was `GRAVAR`), the same in every language.
- Menu redraws write each frame at once, cut lines to the terminal width, account for rows the
  terminal re-wraps after a resize, scroll inside a viewport and keep the scrollback, the last
  selection and the drive panel (shown again only after Refresh Drives, Drive Tags or Language).
  Validated in a pseudo console: navigation, Esc, `q`, Ctrl+C, resize and cursor restoration.
- All selection lists (submenus included) use the new menu with the same width handling.
- Device type labels follow the language ("SATA SSD", "External USB HDD" / "SSD SATA", "HD externo USB").
- Default report file name: `diskdoctor-report-YYYYMMDD-HHMMSS`; Capacity Verification folder:
  `diskdoctor-capacity-YYYYMMDD-HHMMSS`.
- Benchmark results store canonical English labels ("Sequential Read · start"); labels saved by
  0.2.x are still recognized.

### Fixed
- Logic no longer depends on Portuguese text: SMART failing attributes use smartctl's own terms
  (`FAILING_NOW`, `In_the_past`), Surface Scan aborts carry a stable kind (results saved by 0.2.x are
  still understood), the recorded change is a separate field, and the history timeline uses stable kinds.
- Drive panel percentage was parsed from the formatted number; it is now computed directly.
- Escape sequences of the menus work with `NO_COLOR`/`--no-color` (VT processing is enabled explicitly).
- Helper queries on Windows use `CREATE_NO_WINDOW`, so no auxiliary console windows appear.
- Returning to the menu no longer clears the whole screen.

## [0.2.0] — 2026-11-08

### Added
- **Interactive menu**: `diskdoctor` with no arguments (or a double-click on the `.exe`) opens a panel
  with the drives and an arrow-key menu. Each action shows the equivalent command and runs through
  the same path as the CLI. It offers to reopen as Administrator when a test needs it.
- **External HDD/SSD, USB flash drive and memory card**: type recognized from `Win32_DiskDrive.MediaType`,
  the bus (USB, SD, MMC, FireWire) and the model. External drives become HDD or SSD from SMART's
  `rotation_rate`.
- **`diskdoctor capacity`**: real capacity verification (f3write/f3read style) with new files in a new
  folder, uncached verification, real capacity estimate and cleanup at the end.
  New rules `capacity.fake` and `capacity.errors`; report section.
- **`diskdoctor tag`**: per-drive tags ("Studio", "Games"), shown everywhere and accepted by `--disk`
  instead of the number.
- **`diskdoctor history`** and **`history note`**: per-drive timeline (diagnoses, changes, tests) and a
  **before × after** comparison of each recorded change. After a change, the verdict only considers
  later events, and SMART growth is measured from it.
- Usage bar per volume in `list` and in the panel.
- CI (clippy + tests) and automated release on GitHub Actions (`.zip` + SHA-256).
- Published at <https://github.com/marcoshmendes/diskdoctor> under the **MIT** license.

### Changed
- Ctrl+C: outside a test it exits restoring the terminal; during a test it cancels safely (needed for
  several tests in the same process, in the menu).
- README rewritten for people who download the ready-made `.exe`.
- Test fixtures anonymized in volume GUIDs and labels too.

### Fixed (found with real data, as Administrator)
- **ICRC** entries in the ATA error log (interface CRC) were counted as "degraded media"; they are now
  communication evidence (`comm.link`), and only **UNC** errors count as media.
- Many retries/resets with **every CRC counter at zero** now reorder the causes: the drive itself
  (firmware/controller pauses) and the power come before the data cable.

### Decided against
- Reading several drives at the same time: it distorts latency and could produce false slowness
  positives. One test at a time.

## [0.1.0] — 2026-11-08

Non-destructive diagnostics MVP.

### Added
- `list`, `health`, `events`, `scan`, `benchmark` and `report` commands, with help and messages in
  Portuguese and `--json` output.
- Inventory through PowerShell 5.1/CIM, with masked serials, type (SSD/HDD × SATA/NVMe/USB) and volumes;
  `Volume{guid}` → `\Device\HarddiskVolumeN` resolution through `QueryDosDevice`.
- SMART through `smartctl -x --json=c` (ATA and NVMe), with a forbidden-argument list (no self-tests or
  changes) and exit-status decoding.
- `Get-StorageReliabilityCounter` counters (temperature, wear, errors, maximum latency).
- System log events classified by (provider, ID, level), attributed to a drive by device, number,
  volume or inference from the driver.
- Read test with a read-only handle, overlapped I/O with timeout and `CancelIoEx`, hang detection,
  bisection of unreadable sectors, transient errors, speed timeline and latency histogram.
- QD1 benchmark (sequential in 3 zones and random 4 KiB); optional write test on a temporary
  `CREATE_NEW` + `DELETE_ON_CLOSE` file, with typed confirmation.
- Correlation engine with weighted evidence, confidence, causes and recommendations; correlation of
  drive events with unexpected reboots; LBA pattern of 153 events.
- Local SMART counter history to detect growth.
- Self-contained HTML report (SVG speed chart, latencies, events, limitations) and complete JSON.
- 90 automated tests (parsers with real anonymized fixtures, engines with simulated devices, CLI).
- Documentation: `README.md`, `AGENTS.md`, `docs/ARCHITECTURE.md`, `docs/DIAGNOSTICS.md`.
