<div align="center">

# diskdoctor

**Find out whether your HDD, SSD, USB flash drive or memory card is failing — without risking your data.**

![Windows 10/11](https://img.shields.io/badge/Windows-10%20%7C%2011-0078D6?logo=windows)
![Rust](https://img.shields.io/badge/made%20with-Rust-B7410E?logo=rust)
![Read-only](https://img.shields.io/badge/drives-read--only-2ea44f)
![English | Português](https://img.shields.io/badge/UI-English%20%7C%20Portugu%C3%AAs-555)

[**⬇ Download the latest release**](../../releases/latest)

</div>

Does your PC freeze, a game stutter, Windows "disappear" for a few seconds — or did that new
1 TB USB stick cost suspiciously little? **diskdoctor** gathers everything Windows and the drives
themselves know and tells you **what is going on, how sure it is, and what to do next**.

- 🩺 **SMART & Drive Health**: SMART, Windows reliability counters and storage events (IDs 7, 51,
  129, 153…), correlated with each other. E.g. *"400 retries at 300 addresses spread across the
  drive → cable/port pattern, not a bad sector"*.
- 🔍 **Surface Scan**: reads the whole drive, measuring speed, slow reads, errors and hangs. Read-only.
- ⚡ **Benchmark**: Sequential Read at the start/middle/end and Random 4 KiB at QD1 (MB/s, IOPS, latency).
- 📦 **Capacity Verification** (like *f3*): catches **counterfeit** USB flash drives and memory cards.
- 🕘 **History & Comparison**: replaced the SATA cable? Record it and see whether the errors stopped.
- 🏷 **Drive Tags**: call your drives "Studio", "Games", "Backup" and use those names in commands.
- 📄 **HTML report** to open in the browser or send to whoever is fixing the PC.
- 🌐 **English or Português (Brasil)** — technical names (SMART, Surface Scan, Sequential Read,
  Random 4 KiB, QD1, Capacity Verification) stay in English in both.
- Works with **SATA HDDs and SSDs, NVMe SSDs, external USB drives, USB flash drives and memory cards**.

---

## Quick start (nothing to install)

1. **Download** `diskdoctor-vX.Y.Z-windows-x64.zip` from
   [Releases](../../releases/latest) and extract it to a folder.
2. **Install smartmontools** (recommended — it reads the drives' SMART data). In Terminal:
   ```powershell
   winget install smartmontools.smartmontools
   ```
3. **Double-click `diskdoctor.exe`**. A menu opens; pick an action with the arrow keys and Enter,
   or press its number.

```text
  ╭──╮  diskdoctor 0.3.0
  │◉ │  Storage diagnostics · SMART · Surface Scan · Benchmark · Capacity Verification
  ╰──╯  ● Administrator   ● smartctl

  DIAGNOSTICS
  ❯ 1  SMART & Drive Health      Quick   ~30 s
    2  Export Diagnostic Report  Quick   ~30 s–2 min
    3  Storage Event Log         Quick   seconds
    4  History & Comparison      Quick   seconds
  TESTS
    5  Surface Scan (read-only)  Long    minutes to hours
    6  Benchmark (read-only)     Quick   ~30 s–2 min
    7  Capacity Verification     Long    minutes to hours
  SETTINGS
    8  Drive Tags                Instant
    9  Refresh Drives            Quick   seconds
    0  Language: English         Instant
    q  Exit                      Instant

  SMART attributes, Windows counters and events, correlated into a verdict per drive
  ↑↓ move   Enter select   0–9 shortcut   Esc quit
```

- Every action says whether it is **Quick**, **Long** or **Instant** and roughly how long it takes.
  These are estimates: capacity, connection type and failing hardware can make tests much longer.
  The drive pickers of Surface Scan and Capacity Verification show an estimate per drive.
- The line under the menu describes the highlighted action. Narrow windows keep the duration class
  visible and drop the details.
- Each action shows the command it runs (e.g. `Running: diskdoctor scan --disk 1 --read-only`),
  a progress bar with speed and time left, and explains the result at the end.
- Surface Scan and Benchmark need **Administrator**: the menu offers to reopen diskdoctor elevated.
- **Language**: menu item `0`, or `diskdoctor language pt-BR`. English is the default.

> **"Windows protected your PC"?** `diskdoctor.exe` has no paid code-signing certificate, so
> SmartScreen may warn. Click **More info → Run anyway**. With Windows 11 **Smart App Control** on,
> the file may be blocked outright; in that case [build it from source](#build-from-source). To check
> that the file is the original one, compare its SHA-256 with the `.sha256` published next to it:
> `Get-FileHash .\diskdoctor-*.zip -Algorithm SHA256`.

---

## What each action does

| Menu | Command | Writes anything? | Needs Admin? |
|---|---|---|---|
| SMART & Drive Health | `diskdoctor health` | no | recommended (full SMART) |
| Export Diagnostic Report | `diskdoctor report --format html` | the report file | recommended |
| Storage Event Log | `diskdoctor events --hours 48` | no | no |
| History & Comparison | `diskdoctor history --disk N` | only diskdoctor's local history | no |
| Surface Scan (read-only) | `diskdoctor scan --disk N --read-only` | **no** | yes |
| Benchmark (read-only) | `diskdoctor benchmark --disk N --read-only` | no | yes |
| Capacity Verification | `diskdoctor capacity --disk N` | new temporary files, deleted at the end — asks you to type `WRITE` | no |
| Drive Tags | `diskdoctor tag add --disk N Studio` | only diskdoctor's local settings | no |
| Language | `diskdoctor language pt-BR` | only diskdoctor's local settings | no |
| — | `diskdoctor list` | no | no |

In every command, `--disk` takes the drive **number** (see `diskdoctor list`) **or a tag**:
`diskdoctor health --disk studio`.

### My PC freezes: where do I start?

1. **SMART & Drive Health** (`diskdoctor health`). It already points to the suspect drive and the kind of problem.
2. **Surface Scan** of the suspect drive, **alone** and with the PC otherwise idle.
3. If the diagnosis points to the **cable/port**, replace the SATA cable (or port) and **record the change**:
   `diskdoctor history note --disk 2 "Replaced the SATA cable"`.
4. Use the PC normally for 1–3 days and run `diskdoctor health` again.
5. `diskdoctor history --disk 2` shows **before × after**: if the errors are gone, it was the cable.

```text
  Before × after the change of 2026-11-08 23:10 — "Replaced the SATA cable" (3 days ago)
╭──────────────────────────┬──────────────────┬────────────────────────╮
│                          ┆ Before           ┆ After                  │
╞══════════════════════════╪══════════════════╪════════════════════════╡
│ Drive I/O events         ┆ 412 (137.3/day)  ┆ 0 (0.0/day)            │
│ Unexpected reboots (PC)  ┆ 3                ┆ 0                      │
│ Last verdict             ┆ WARNING          ┆ NO PROBLEMS DETECTED   │
│ SMART counters that rose ┆ CRC errors +120  ┆ none                   │
╰──────────────────────────┴──────────────────┴────────────────────────╯
  → Improved: Problems dropped from 412 to 0 events (3.0 days on each side): the change most likely fixed it.
```

### Suspicious USB flash drive or memory card

`diskdoctor capacity --disk N` (or **Capacity Verification** in the menu) writes test files to the
free space, each block tagged with its own position, reads everything back without cache and deletes
the files. A fake device "wraps around" and overwrites what it wrote before; diskdoctor detects that
and estimates the **real capacity**.

> ⚠️ On a fake device, writing past the real capacity can corrupt what is already on it.
> **Back up first** — ideally test an empty device. The test refuses the system drive.

---

## Reading the result

| Status | Meaning |
|---|---|
| **Critical** | Strong evidence of a defect (unreadable sectors, failed SMART, fake capacity, hanging device). Back up now. |
| **Warning** | Likely active problem (many retries/resets, recurring corruption, timeouts). |
| **Attention** | Weak or isolated signal; keep an eye on it. |
| **No problems detected** | Nothing found **in the analyzed sources**. It is not a guarantee of health. |
| **Undetermined** | Not enough data (no SMART, no Surface Scan, no events). |

Each finding lists its **evidence** (and where each piece came from), the **confidence** (low/medium/high —
higher when independent sources agree), the **possible causes** and **what to do**. The diagnosis also
lists what was **not** checked.

Run `diskdoctor health` now and then: diskdoctor keeps a local history and warns when an error counter
**grows**, which is the most reliable sign of an active problem.

---

## Language

diskdoctor speaks **English** (default) and **Português (Brasil)**:

```powershell
diskdoctor language            # show the current language
diskdoctor language pt-BR      # save Portuguese as the default
diskdoctor --lang en health    # one run in English
```

Priority: `--lang` → the `DISKDOCTOR_LANG` environment variable → the saved choice → English.
Technical names stay in English in both languages, so they match documentation, forums and other tools.
Numbers and dates follow the language (`1,234.5` / `2026-11-08` in English, `1.234,5` / `08/11/2026` in
Portuguese). `--json` field names never change; only human-readable texts do.

## Safety

- Every drive test opens the drive through Windows **read-only**.
- diskdoctor never formats, partitions, runs `chkdsk /f`, starts SMART self-tests or changes existing files.
- `smartctl` only receives query commands; test or configuration commands are blocked in the code.
- The only (optional) writes are **new files** — Benchmark write test and Capacity Verification —
  deleted at the end, and only after you **type `WRITE`**.
- Before long tests, it warns about backups and asks for confirmation.

## Privacy

- Nothing leaves your computer: no telemetry, no network connection.
- Serial numbers are shown **masked** (`S3Z*********1234`).
- History, tags, settings and results stay in `%LOCALAPPDATA%\diskdoctor`.
- The HTML report includes the computer name and the drive models: review it before publishing it.

---

## Command line

```powershell
diskdoctor                                   # interactive menu
diskdoctor list                              # drives, type, capacity, volumes and usage
diskdoctor health                            # SMART & Drive Health for every drive
diskdoctor health --disk studio --hours 720  # one drive, events of the last 30 days
diskdoctor events --hours 48                 # storage events
diskdoctor scan --disk 1 --read-only         # full Surface Scan (Admin)
diskdoctor scan --disk 1 --offset 100G --length 20G
diskdoctor benchmark --disk 1 --read-only    # Sequential Read + Random 4 KiB, QD1 (Admin)
diskdoctor capacity --volume F:              # counterfeit USB drive/card?
diskdoctor tag add --disk 2 Studio           # give the drive a name
diskdoctor history note --disk 2 "Replaced the SATA cable"
diskdoctor history --disk 2                  # timeline and before × after
diskdoctor report --format both -o C:\reports\pc   # pc.html + pc.json
diskdoctor language pt-BR                    # interface in Portuguese
```

- `--json` on any command: structured output for scripts and other programs.
- `--lang en|pt-BR` on any command: language for that run.
- `Ctrl+C` cancels tests safely (the partial result is saved).
- `diskdoctor <command> --help` shows every option.

Exit codes: `0` ok · `1` error · `2` usage error · `3` requires Administrator · `4` cancelled ·
`5` problems found · `6` confirmation declined.

---

## Build from source

Requires Windows 10/11, [Rust](https://rustup.rs) (MSVC toolchain) and Visual Studio Build Tools (C++).

```powershell
git clone https://github.com/marcoshmendes/diskdoctor
cd diskdoctor
cargo build --release
.\target\release\diskdoctor.exe
```

`cargo test` runs the test suite; it needs no Administrator rights and never touches your drives.

## Limitations

- The Surface Scan goes through the whole Windows path (driver, controller, cable): isolating the
  culprit requires swapping components — that is what the before × after history is for.
- RAID/Intel RST controllers, USB enclosures and card readers do not always pass SMART through.
  USB flash drives and memory cards have no SMART.
- On SSDs, empty (TRIMmed) areas answer without reading the flash.
- Capacity Verification only tests free space.

Technical details in [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md) and the diagnostic rules in
[`docs/DIAGNOSTICS.md`](docs/DIAGNOSTICS.md).

## For developers and AI agents

Read [`AGENTS.md`](AGENTS.md): code map, safety invariants, how to extend and how to test.

## License

[MIT](LICENSE).
