# Architecture and technical limitations

## Overview

```
                    ┌─────────────────────────── diskdoctor.exe ───────────────────────────┐
  no arguments   ►  │ interactive + menu (keyboard lists) ─┐                               │
  CLI (clap) ────►  │ app::dispatch ◄───────────────────────┘     i18n (en / pt-BR)        │
                    │ commands/*  ──► collection ──► analysis::analyze ──► ui / report     │
                    │                  │                                                   │
                    │   inventory ─────┤ PowerShell 5.1 + CIM (Get-Disk, Win32_DiskDrive…) │
                    │   smart ─────────┤ smartctl -x --json=c (external process)           │
                    │   windows_counters┤ Get-StorageReliabilityCounter                    │
                    │   events ────────┤ Get-WinEvent -FilterXPath (System log)            │
                    │   scan/benchmark ┤ platform::win::RawDisk (Win32, overlapped I/O)    │
                    │   capacity ──────┤ platform::win::CapacityDir (new files)            │
                    │   history/journal┤                                                   │
                    │   tags/store ────┘ %LOCALAPPDATA%\diskdoctor (JSON/JSONL)            │
                    └──────────────────────────────────────────────────────────────────────┘
```

The layers:

1. **Entry** (`app`, `cli`, `commands`, `interactive`, `menu`): parses arguments, asks for
   confirmations and turns results into exit codes. The interactive menu has no logic of its own:
   it builds the equivalent command line, shows it to the user and runs it through `app::dispatch`,
   so confirmations and safety checks are the CLI's.
2. **Collection** (`inventory`, `smart`, `windows_counters`, `events`, `health`): each source becomes a
   serializable struct. Parsers are pure functions over JSON text, tested with real fixtures.
3. **Active tests** (`scan`, `benchmark`, `capacity`): generic engines over the `BlockReader`,
   `BlockWriter`, `VerifyTarget`/`VerifyFile` traits (`io.rs`). They know nothing about Windows, which
   lets them be tested with simulated devices that inject errors, slowness, timeouts, hangs and fake
   capacity.
4. **Analysis** (`analysis`, `history`, `journal`): independent rules that combine weighted evidence
   from several sources; SMART history and a journal of diagnoses/changes for before × after
   comparisons. Details in [`DIAGNOSTICS.md`](DIAGNOSTICS.md).
5. **Output** (`ui`, `report`, `store`, `tags`): localized terminal output, JSON and self-contained HTML
   report, local storage of tests, tags and settings.
6. **Platform** (`platform`): the only place with `unsafe`, Win32 and external processes.

## Languages (`i18n.rs`)

- Two languages: English (default) and Português (Brasil). Every user-facing string is written inline
  in both: `t("Drive", "Disco")` for static text and `tr!("{n} drives", "{n} discos")` for formatted
  text. A missing translation is a compile error, and both texts sit next to each other in the code.
- Technical names stay in English in both languages (SMART, Surface Scan, Benchmark, Sequential Read,
  Random 4 KiB, QD1, Capacity Verification, LBA, CRC…), so they match documentation and other tools.
- The language is resolved **before** clap builds the command (help texts are evaluated when the
  command is built): `--lang` (read from the raw arguments), `DISKDOCTOR_LANG`, `settings.json`, English.
  clap's own messages are translated afterwards in pt-BR (`app::localize_clap_text`).
- The active language is a process-wide atomic; tests use a per-thread override (`i18n::scoped`).
- Logic never depends on translated text: rule ids, severities, `AbortKind`, smartctl's
  `FAILING_NOW`, canonical benchmark labels and JSON field names are stable. Saved results keep the
  texts of the language they were produced in; labels known to diskdoctor are re-localized on display.
- Numbers and dates follow the language (`util::int`, `decimal`, `local_datetime`…).

## Interactive mode and flicker-free menus (`menu.rs`)

- Each frame is built as one string and written with a single `write` + flush. A redraw moves the
  cursor up over the previous frame and clears to the end of the screen: the content above (drive
  panel, previous results) is never repainted, and the scrollback is kept.
- Lines are cut to the terminal width minus one column, so nothing wraps and the cursor arithmetic is
  exact. The size is read again before every frame. If the window got narrower, the previous frame's
  rows may have been re-wrapped by the terminal; the redraw computes how many rows each line occupies
  now and moves up that many.
- Lists taller than the window scroll inside a viewport with "↑ more"/"↓ more" markers.
- Column layout degrades in steps: full (label, duration class, estimate) → no estimate → compact
  (duration class before the label). The duration class is always visible.
- The cursor is hidden while a list is open and shown again when it closes; the Ctrl+C handler restores
  it too. VT processing is enabled explicitly so it also works with `NO_COLOR`.
- `menu::render` is a pure function and is unit-tested for widths from 12 to 140 columns. The behavior
  was also validated in a Windows pseudo console (navigation, shortcuts, submenu + Esc, resize to 48
  and 120 columns, `q`, Ctrl+C, cursor state, bytes per redraw).

## Device types

`inventory::infer_device_kind` combines the bus (`Get-Disk.BusType`), `Win32_DiskDrive.MediaType`
("Fixed hard disk media", "External hard disk media", "Removable Media") and the model:

| Type | How it is recognized | Label (en / pt-BR) |
|---|---|---|
| Internal | SATA/NVMe/SAS… bus and fixed media | `SATA SSD`, `SATA HDD`, `NVMe SSD` / `SSD SATA`, `HD SATA`, `SSD NVMe` |
| External | USB/1394 bus or "External hard disk media" | `External USB HDD`, `External USB SSD`, `External USB drive` / `HD externo USB`, `SSD externo USB`, `Disco externo USB` |
| USB flash drive | "Removable Media" | `USB flash drive` / `Pendrive USB` |
| Memory card | SD/MMC bus, or removable/USB with a reader-like model (`SD`, `MMC`, `Card`, `Reader`…) | `Memory card` / `Cartão de memória` |

External drives rarely tell Windows whether they are HDDs or SSDs; when SMART passes through the USB
enclosure, smartctl's `rotation_rate` settles it (0 = SSD).

## Capacity Verification (f3 style)

```
diskdoctor ─► create_dir("<X>:\diskdoctor-capacity-YYYYMMDD-HHMMSS")   (fails if it exists)
          ─► CreateFileW("0001.ddcap", CREATE_NEW, NO_BUFFERING | WRITE_THROUGH | OVERLAPPED)
          ─► writes 1 MiB blocks: every 4 KiB = "DDCAPv01" + seed + position + pseudo-random data
          ─► closes; reopens read-only (OPEN_EXISTING, NO_BUFFERING) and checks every 4 KiB
          ─► deletes the files it created itself and the folder (remove_dir, never recursive)
```

- A chunk with the header of **another position** = the device overwrote data (classic fake).
- An **empty** chunk or one from another run (different seed) = lost write; if that concentrates at the
  end, it is the fake that drops writes past the real capacity.
- Scattered corruption or I/O errors = defective device or poor contact.
- Estimated real capacity = space already used + bytes read back correctly.
- The system drive is refused (filling its free space would freeze Windows).
- Writing requires typing `WRITE` (the same word in every language).
- Inherent risk, told to the user: on a fake device, writing past the real capacity can corrupt what is
  already on it (the controller overwrites low addresses).

## Tags, settings and history

- `tags.json`: fingerprint → tags. `--disk` takes a number or a tag (`tags::DiskRef`); a tag must be on
  a single drive to work as a selector and cannot be digits only.
- `settings.json`: the saved interface language.
- `journal\<fp>.jsonl`: diagnoses (`health`/`report`) and changes (`history note`). The latest change
  becomes the cutoff: earlier events do not count toward the verdict, and SMART counter growth is
  measured from it. `journal::compare` compares windows of the same length before and after (up to 14
  days on each side).

## Why Rust (and not Node.js)

The core goal is diagnosing **hangs and timeouts**. For that the tool must:

- enforce a **timeout per read** and **really cancel** a stuck I/O (`CancelIoEx`). In Node, the read
  occupies a libuv pool thread, which gets stuck and cannot be aborted;
- read **without the Windows cache** (`FILE_FLAG_NO_BUFFERING`), which requires a sector-aligned
  buffer. In Rust it is trivial (`Layout` aligned to 4096); Node gives no guarantee;
- call `DeviceIoControl` (capacity, logical and physical sector) without a native addon;
- ship as a single `.exe`, with no runtime, easy to take to the machine with the problem.

## How the Surface Scan accesses the drive

```
diskdoctor ─► CreateFileW("\\.\PhysicalDriveN", GENERIC_READ, NO_BUFFERING | OVERLAPPED)
          ─► disk.sys (class driver) ─► partmgr ─► storport + miniport
             (storahci / stornvme / iaStorAC / ...) ─► controller ─► cable/slot ─► drive
```

- **Read-only guaranteed by the OS**: the handle is opened with `GENERIC_READ` only.
- **Alignment**: buffer aligned to 4096 B; offsets and sizes are multiples of the logical sector.
- **Timeout**: each read is overlapped; `GetOverlappedResultEx` waits up to `--timeout-ms`. If it
  expires, `CancelIoEx` cancels it and the read counts as a **timeout**. If not even the cancel
  completes within 120 s, the device is considered **hung**: the scan stops and the I/O memory is
  leaked on purpose, because the kernel may still write to it.
- **Isolating bad sectors**: a failed block is split in half repeatedly down to the physical sector
  size, with a re-read budget. If the halves read fine, the error was **transient**, which points to
  communication rather than the media.

## Technical limitations of direct drive access on Windows

These apply to any language: they come from the operating system and the hardware.

1. **Administrator privilege.** Opening `\\.\PhysicalDriveN` and sending SMART commands requires
   elevation. Without it: `list` and `events` work; `health` reads only what `smartctl` can (on some
   SATA drives, attributes without thresholds or logs); `scan` and `benchmark` refuse with code 3.
2. **The driver stack is always in the middle.** What is measured is drive + cable + controller +
   driver. An error can come from any of them. Isolating the culprit requires swapping components
   (cable, port, slot) and repeating the test. The analysis says which hypothesis is more likely.
3. **Windows has its own timeout** (the disk driver's `TimeOutValue`, usually 30–60 s) and resets the
   link when it expires (event 129). A long hang may become a driver error before the diskdoctor
   timeout.
4. **Raw writes to mounted volumes are blocked** since Vista. That does not affect diskdoctor, which
   never writes to the physical drive; the write test and Capacity Verification use new files inside
   the file system.
5. **SMART does not always pass through.** RAID/Intel RST controllers, USB-SATA bridges and hubs can
   block ATA/NVMe pass-through commands. SMART is then "unavailable" and the analysis relies on events
   and the Surface Scan.
6. **SSDs remap addresses (FTL).** An LBA has no fixed physical position, and areas never written or
   TRIMmed may be answered without reading the flash. The Surface Scan detects unreadable data and
   slowness, but does not test every cell.
7. **Reading does not repair.** Pending sectors are only reallocated when rewritten. diskdoctor never
   rewrites; that decision (with a backup) is the user's.
8. **The system drive competes with normal use.** Speed and latency vary during the test; isolated
   spikes are not a defect.
9. **The event log can miss what matters.** If the system drive freezes, Windows cannot always write the
   event. That is why the correlation also uses unexpected reboots (Kernel-Power 41) and the events
   right before them.
10. **Normal SMART does not prove health.** Many drives fail without changing attributes. The
    "no problems detected" verdict always comes with the gaps (what was not checked).
11. **USB flash drives and memory cards have no SMART**, and many USB enclosures do not pass the drive's
    through. For them, the Surface Scan and Capacity Verification are the diagnostic sources.
12. **Capacity Verification only tests free space** and depends on the file system; detecting a fake
    requires filling almost all the free space (the default without `--size`).

## Storage and privacy

- Serials appear masked only (`S3Z*********1234`). Results, history, journal and tags use a
  `fingerprint` (FNV-1a hash of model+serial) to recognize the same drive across runs, even if the drive
  number changes.
- Everything stays local in `%LOCALAPPDATA%\diskdoctor`; nothing is sent over the network.
- The report includes the computer name and the drive models.
- The test fixtures come from a real machine with serials, volume GUIDs and labels replaced by
  fictitious values. Anonymize new ones the same way.
