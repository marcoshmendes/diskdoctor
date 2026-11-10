# Diagnostic engine (correlation)

Code: `src/analysis.rs`. Each rule gathers **evidence** and, if there is at least one piece, produces a
**finding** (`Finding`) with severity, confidence, possible causes and recommendations. Rule ids,
severities and sources are stable identifiers; titles, evidence texts, causes and recommendations are
written in the active language (English or Português), and the language never changes a verdict.

## Evidence, weights and confidence

| Weight | Meaning | Examples |
|---|---|---|
| 1 | weak | vendor CRC counter, a few timeouts over the drive's life |
| 2 | moderate | interface CRC (199), a small number of 153 events |
| 3 | strong | pending sectors, attributed 129 event, Surface Scan timeouts |
| 4 | very strong | SMART attribute failing now (`FAILING_NOW`), NVMe critical warning |
| 5 | conclusive | SMART FAILED, unreadable sectors confirmed by the Surface Scan, device hung |

Events gain weight with their count (+1 with 5 or more, +2 with 20 or more, capped at 4) and lose 1
when the drive was only inferred from the driver.

**Confidence** (`confidence_for`):
- **High**: some evidence of weight 5, **or** total weight ≥ 6 from ≥ 2 independent sources.
- **Medium**: total weight ≥ 3.
- **Low**: the rest.

Sources: SMART, Windows counters, Windows events, Surface Scan, Benchmark, SMART history,
Capacity Verification.

**Recorded change** (`diskdoctor history note`): the latest one of each drive becomes the cutoff. Events
before it do not count toward the rules, and SMART counter growth is measured from it. The verdict
cites the change (`DiskVerdict::change`) and, if it is less than 24 h old, lists as a gap that it is too
early to tell.

**Drive status** = worst severity among the findings: Critical, Warning, Attention, or
"No problems detected". With no data source at all, the status is "Undetermined". Findings that are
informational only keep "no problems detected".

## Per-drive rules

| Rule (`rule`) | Title | Main evidence |
|---|---|---|
| `smart.failed` | The drive itself reports a failure | `smart_status.passed=false` (5); attribute `when_failed=now` → `FAILING_NOW` (4); in the past → `In_the_past` (1); NVMe `critical_warning` bits |
| `media.degradation` | Unreadable sectors or degraded media | 197 pending (3), 198 offline (3), 187 (2), 5 reallocated (1–2), grown bad blocks, program/erase failures, NVMe media errors (3), failed self-test (3), ATA log with UNC (2), **growth** in the history (2–3), Windows counters (2), events 7/154/52/51, **Surface Scan with unreadable regions (5)** |
| `comm.link` | Communication failures, timeouts or device hangs | 199 CRC (2), vendor CRC (1), SATA PHY, SATA Phy log (2), 188 timeouts (Info if ≤ 2, 1 if ≥ 3, 2 if ≥ 10), 184 (2), growth (2–3), events 129/9/11/153/15/157, **LBA pattern of 153 events**, reset with no identified drive close to the drive's events (2), Surface Scan: timeouts (3), transient errors (2), hang (5), fatal stop (4), ATA error log with **ICRC** only (2; with UNC it counts in `media.degradation`). Becomes **Critical** with weight ≥ 8 from ≥ 2 sources. If the SATA drive reports CRC counters and **all are zero** despite the events, adds a note (weight 0) and reorders the causes: the drive's own pauses/firmware and power before the data cable |
| `perf.stalls` | Very slow reads or device pauses | Surface Scan: reads > 500 ms (2–3), peak ≥ 150 ms (1), region < 35% of the median (1); Benchmark Random 4 KiB ≥ 1 s (2); Windows maximum latency |
| `temp.high` | High temperature | current ≥ warning/critical (HDD 50/55, SATA SSD 65/75, NVMe the drive's own thresholds or 70/80); recorded maximum; NVMe minutes above the limit |
| `ssd.wear` | Advanced flash wear | endurance used ≥ 75% (2) / ≥ 90% (3); spare ≤ threshold (4) |
| `power.unsafe` | Frequent unsafe shutdowns | ≥ 50 and ≥ 5% of power cycles; growth ≥ 3 in the history |
| `fs.corruption` | File system inconsistencies | NTFS 55 (Warning if ≥ 5, otherwise Attention), 98 level ≤ 3, 50/57/140 (flush failure), 7 (torn write); cites the affected volumes and partitions |
| `freeze.correlation` | Problems on this drive right before hangs/reboots | drive I/O events in the 30 min before Kernel-Power 41 (or a 6008 without a 41 of the same boot) |
| `capacity.fake` | Fake capacity | Capacity Verification with overwritten data, or lost data concentrated at the end (5, Critical) |
| `capacity.errors` | Failures writing or reading back the test data | scattered corrupted/unreadable/lost data (5), write failures (4), verification stopped (4) |

## Before × after (`journal::compare`)

For each recorded change, compares windows of the same length (the time since the change, up to 14
days) before and after it:

| Effect | Rule (drive I/O events: before `b`, after `a`) |
|---|---|
| Too early to tell | less than 24 h since the change |
| Improved | `b ≥ 3` and `a ≤ b/5` |
| Got worse | `a ≥ 1.5·b + 2`, or `b < 3` and `a ≥ 3` |
| No clear change | the rest with `b ≥ 3` |
| Not measurable | `b < 3` and `a = 0` — then the SMART counters decide (growth before and none after = improved) |

It also shows unexpected reboots (Kernel-Power 41), the last verdict on each side and which SMART
counters grew.

### LBA pattern of 153 events

With 5 or more retry events that cite an LBA:
- **distinct addresses spread across ≥ 25% of the drive** point to the link, controller or firmware;
- **concentrated** addresses point to a weak region of the media.

The most repeated LBAs (3× or more) appear in the text so they can be checked with the Surface Scan.

## Global findings (no drive)

- `global.controller`: I/O events with no identified drive (e.g. `storahci/129` with several SATA drives).
- `global.whea`: WHEA errors (PCIe/CPU), relevant to NVMe.
- `global.unexpected_reboots`: unexpected reboots in the period.

## Event classification (`events.rs`)

| Provider | IDs → kind |
|---|---|
| `disk` | 7 bad block · 11 controller error · 15 not ready · 51 paging error · 52 SMART predicts failure · 153 I/O retried · 154 I/O failed due to hardware · 157 surprise removal |
| port drivers (`storahci`, `stornvme`, `iaStor*`, `amd_sata`, `nvme*`, `storport`...) | 129 device reset · 9 timeout · 11 controller · 15 not ready |
| `Ntfs` / `Microsoft-Windows-Ntfs` | 55 corruption · 98 (level ≤ 3) repair needed · 7 torn write · 50/57/140 flush failure |
| `Microsoft-Windows-Kernel-Power` | 41 restart without a clean shutdown |
| `EventLog` | 6008 unexpected shutdown |
| `Microsoft-Windows-WHEA-Logger` | 1/17/18/19 hardware error |

**Attribution to a drive**, in order: `\Device\HarddiskN` (exact) → "Disk N"/"Disco N" in the `disk`
message (exact) → `Volume{guid}` / `\Device\HarddiskVolumeN` / letter (exact) → the only drive on the
port driver's bus (inferred). Event messages come in the Windows display language, so the patterns
accept English and Portuguese words.

## Notes always present

- Normal SMART does not guarantee health.
- No events does not rule out a problem on the system drive.
- Running without Administrator limits the data.
