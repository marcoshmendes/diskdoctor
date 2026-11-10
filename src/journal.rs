//! Journal of each drive: the diagnoses (saved by `health` and `report`) and the
//! **changes** the user records ("replaced the SATA cable", "moved to another port"), to
//! compare **before × after**.
//!
//! File: `<data>\journal\<fingerprint>.jsonl` (one entry per line).
//!
//! Effect of recording a change:
//! - the verdict only considers Windows events **after** it (`analysis::analyze_disk`),
//!   and SMART counter growth is measured from it (`health::collect_health`), so a problem
//!   that was already solved does not keep being reported for days;
//! - `diskdoctor history` compares both periods ([`compare`]).

use crate::analysis::DiskStatus;
use crate::history::{PROBLEM_COUNTERS, SmartSnapshot, counter_label};
use crate::i18n::t;
use anyhow::{Context, Result};
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

const MAX_LINES: usize = if cfg!(test) { 50 } else { 2000 };
/// Maximum window on each side of the change in the comparison.
pub const MAX_COMPARE_DAYS: i64 = 14;
/// Before this, the effect of a change cannot be assessed yet.
pub const MIN_HOURS_AFTER: f64 = 24.0;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DiagnosisRecord {
    pub time: DateTime<Utc>,
    pub disk_number: u32,
    pub status: DiskStatus,
    pub headline: String,
    /// Rules that produced a finding (e.g. `comm.link`).
    pub findings: Vec<String>,
    /// The drive's I/O events (retries, resets, errors) counted in the verdict.
    pub io_events: u64,
    pub window_hours: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NoteRecord {
    pub time: DateTime<Utc>,
    pub disk_number: u32,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JournalEntry {
    Diagnosis(DiagnosisRecord),
    Note(NoteRecord),
}

impl JournalEntry {
    pub fn time(&self) -> DateTime<Utc> {
        match self {
            Self::Diagnosis(d) => d.time,
            Self::Note(n) => n.time,
        }
    }
}

pub struct Journal {
    dir: PathBuf,
}

impl Journal {
    pub fn new(data_dir: &Path) -> Self {
        Self { dir: data_dir.join("journal") }
    }

    fn path(&self, fingerprint: &str) -> PathBuf {
        let safe: String = fingerprint.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        self.dir.join(format!("{safe}.jsonl"))
    }

    /// Entries in chronological order (invalid lines are skipped).
    pub fn load(&self, fingerprint: &str) -> Vec<JournalEntry> {
        let mut v: Vec<JournalEntry> = fs::read_to_string(self.path(fingerprint))
            .map(|text| text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect())
            .unwrap_or_default();
        v.sort_by_key(|e| e.time());
        v
    }

    pub fn append(&self, fingerprint: &str, entry: &JournalEntry) -> Result<()> {
        fs::create_dir_all(&self.dir).with_context(|| tr!("could not create {}", "não foi possível criar {}", self.dir.display()))?;
        let path = self.path(fingerprint);
        let existing = self.load(fingerprint);
        if existing.len() >= MAX_LINES {
            // Drops the oldest diagnoses; recorded changes are never dropped.
            let mut excess = existing.len() + 1 - MAX_LINES;
            let kept: Vec<&JournalEntry> = existing
                .iter()
                .filter(|e| {
                    if excess > 0 && matches!(e, JournalEntry::Diagnosis(_)) {
                        excess -= 1;
                        false
                    } else {
                        true
                    }
                })
                .collect();
            let text: String = kept.iter().filter_map(|e| serde_json::to_string(e).ok()).map(|l| l + "\n").collect();
            fs::write(&path, text)?;
        }
        let mut f = OpenOptions::new().create(true).append(true).open(&path)?;
        writeln!(f, "{}", serde_json::to_string(entry)?)?;
        Ok(())
    }

    pub fn notes(&self, fingerprint: &str) -> Vec<NoteRecord> {
        self.load(fingerprint)
            .into_iter()
            .filter_map(|e| match e {
                JournalEntry::Note(n) => Some(n),
                _ => None,
            })
            .collect()
    }

    pub fn latest_note(&self, fingerprint: &str) -> Option<NoteRecord> {
        self.notes(fingerprint).into_iter().max_by_key(|n| n.time)
    }

    pub fn diagnoses(&self, fingerprint: &str) -> Vec<DiagnosisRecord> {
        self.load(fingerprint)
            .into_iter()
            .filter_map(|e| match e {
                JournalEntry::Diagnosis(d) => Some(d),
                _ => None,
            })
            .collect()
    }
}

/// One side of the comparison.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PeriodStats {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
    /// I/O events attributed to the drive.
    pub io_events: u64,
    /// Kernel-Power 41 (restarts without a clean shutdown) on the whole PC.
    pub unexpected_reboots: u64,
    /// Last verdict recorded in the period.
    pub status: Option<DiskStatus>,
    /// How much each SMART problem counter grew in the period (needs 2 readings in it).
    pub counter_growth: Vec<(String, u64)>,
    pub smart_readings: usize,
}

impl PeriodStats {
    pub fn days(&self) -> f64 {
        (self.to - self.from).num_seconds() as f64 / 86_400.0
    }
    pub fn counter_growth_total(&self) -> u64 {
        self.counter_growth.iter().map(|(_, g)| g).sum()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Effect {
    /// Less than 24 h since the change.
    TooEarly,
    Improved,
    NoChange,
    Worse,
    /// Almost nothing before the change to compare with.
    NotMeasurable,
}

impl Effect {
    pub fn label(&self) -> &'static str {
        match self {
            Self::TooEarly => t("Too early to tell", "Cedo demais para avaliar"),
            Self::Improved => t("Improved", "Melhorou"),
            Self::NoChange => t("No clear change", "Sem mudança clara"),
            Self::Worse => t("Got worse", "Piorou"),
            Self::NotMeasurable => t("Not measurable", "Não dá para medir"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Comparison {
    pub note: NoteRecord,
    pub hours_since: f64,
    pub before: PeriodStats,
    pub after: PeriodStats,
    pub effect: Effect,
    pub summary: String,
}

fn growth(snapshots: &[SmartSnapshot], from: DateTime<Utc>, to: DateTime<Utc>) -> (Vec<(String, u64)>, usize) {
    let inside: Vec<&SmartSnapshot> = snapshots.iter().filter(|s| s.time >= from && s.time <= to).collect();
    let (Some(first), Some(last)) = (inside.iter().min_by_key(|s| s.time), inside.iter().max_by_key(|s| s.time)) else {
        return (vec![], inside.len());
    };
    let changes = PROBLEM_COUNTERS
        .iter()
        .filter_map(|(key, ..)| {
            let a = *first.counters.get(*key)?;
            let b = *last.counters.get(*key)?;
            (b > a).then(|| (counter_label(key), b - a))
        })
        .collect();
    (changes, inside.len())
}

fn period(
    from: DateTime<Utc>,
    to: DateTime<Utc>,
    io_events: &[DateTime<Utc>],
    reboots: &[DateTime<Utc>],
    diagnoses: &[DiagnosisRecord],
    snapshots: &[SmartSnapshot],
) -> PeriodStats {
    let within = |t: &DateTime<Utc>| *t >= from && *t < to;
    let (counter_growth, smart_readings) = growth(snapshots, from, to);
    PeriodStats {
        from,
        to,
        io_events: io_events.iter().filter(|t| within(t)).count() as u64,
        unexpected_reboots: reboots.iter().filter(|t| within(t)).count() as u64,
        status: diagnoses.iter().filter(|d| within(&d.time)).max_by_key(|d| d.time).map(|d| d.status),
        counter_growth,
        smart_readings,
    }
}

fn decide(hours_since: f64, before: &PeriodStats, after: &PeriodStats) -> Effect {
    if hours_since < MIN_HOURS_AFTER {
        return Effect::TooEarly;
    }
    let (b, a) = (before.io_events, after.io_events);
    let by_events = if b < 3 && a == 0 {
        Effect::NotMeasurable
    } else if b < 3 {
        if a >= 3 { Effect::Worse } else { Effect::NoChange }
    } else if a * 5 <= b {
        Effect::Improved
    } else if a >= b * 3 / 2 + 2 {
        Effect::Worse
    } else {
        Effect::NoChange
    };
    if by_events != Effect::NotMeasurable {
        return by_events;
    }
    // Without events, the SMART counters (e.g. CRC, which rises with a bad cable) can still decide.
    let (gb, ga) = (before.counter_growth_total(), after.counter_growth_total());
    match (before.smart_readings >= 2 && after.smart_readings >= 2, gb, ga) {
        (true, gb, 0) if gb > 0 => Effect::Improved,
        (true, gb, ga) if ga > gb => Effect::Worse,
        _ => Effect::NotMeasurable,
    }
}

fn summary(effect: Effect, before: &PeriodStats, after: &PeriodStats) -> String {
    let days = |p: &PeriodStats| crate::util::decimal(p.days(), 1);
    match effect {
        Effect::TooEarly => tr!(
            "The change was recorded less than {MIN_HOURS_AFTER} h ago. Use the computer normally and run `diskdoctor health` again in 1–3 days to compare.",
            "A mudança foi registrada há menos de {MIN_HOURS_AFTER} h. Use o computador normalmente e rode `diskdoctor health` de novo daqui a 1–3 dias para comparar."
        ),
        Effect::Improved => tr!(
            "Problems dropped from {} to {} events ({} days on each side): the change most likely fixed it. Keep watching — intermittent failures can take days to come back.",
            "Os problemas caíram de {} para {} eventos ({} dias de cada lado): a mudança provavelmente resolveu. Continue observando — falhas intermitentes podem levar dias para reaparecer.",
            before.io_events,
            after.io_events,
            days(after)
        ),
        Effect::NoChange => tr!(
            "{} events before and {} after ({} days on each side): the change did not fix it. The cause is probably another component.",
            "{} eventos antes e {} depois ({} dias de cada lado): a mudança não resolveu. A causa provavelmente está em outro componente.",
            before.io_events,
            after.io_events,
            days(after)
        ),
        Effect::Worse => tr!(
            "Problems increased ({} → {} events). Undo the change or check how it was done.",
            "Os problemas aumentaram ({} → {} eventos). Desfaça a mudança ou verifique a instalação dela.",
            before.io_events,
            after.io_events
        ),
        Effect::NotMeasurable => t(
            "There were almost no errors recorded before the change, so its effect cannot be measured by events.",
            "Quase não havia erros registrados antes da mudança, então não dá para medir o efeito dela pelos eventos.",
        )
        .to_string(),
    }
}

/// Compares periods of the same length before and after a change.
///
/// `io_events` are the times of the I/O events of **this drive**; `reboots`, the PC's
/// Kernel-Power 41 events; `snapshots`, the saved SMART readings.
pub fn compare(
    note: &NoteRecord,
    io_events: &[DateTime<Utc>],
    reboots: &[DateTime<Utc>],
    diagnoses: &[DiagnosisRecord],
    snapshots: &[SmartSnapshot],
    now: DateTime<Utc>,
) -> Comparison {
    let hours_since = (now - note.time).num_seconds().max(0) as f64 / 3600.0;
    let window = (now - note.time).clamp(ChronoDuration::hours(1), ChronoDuration::days(MAX_COMPARE_DAYS));
    let before = period(note.time - window, note.time, io_events, reboots, diagnoses, snapshots);
    let after = period(note.time, (note.time + window).min(now), io_events, reboots, diagnoses, snapshots);
    let effect = decide(hours_since, &before, &after);
    Comparison { note: note.clone(), hours_since, summary: summary(effect, &before, &after), before, after, effect }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn at(days_ago: f64, now: DateTime<Utc>) -> DateTime<Utc> {
        now - ChronoDuration::seconds((days_ago * 86_400.0) as i64)
    }

    fn note(days_ago: f64, now: DateTime<Utc>) -> NoteRecord {
        NoteRecord { time: at(days_ago, now), disk_number: 2, text: "Replaced the SATA cable".into() }
    }

    fn events(days_ago: &[f64], now: DateTime<Utc>) -> Vec<DateTime<Utc>> {
        days_ago.iter().map(|d| at(*d, now)).collect()
    }

    #[test]
    fn cable_swap_that_fixed_the_problem_is_improved() {
        let _en = crate::i18n::scoped(crate::i18n::Lang::En);
        let now = Utc::now();
        // Change 3 days ago; 40 retries in the 3 days before, none after.
        let before: Vec<f64> = (0..40).map(|i| 3.1 + i as f64 * 0.07).collect();
        let c = compare(&note(3.0, now), &events(&before, now), &events(&[4.0, 5.0], now), &[], &[], now);
        assert_eq!(c.effect, Effect::Improved);
        assert_eq!(c.before.io_events, 40);
        assert_eq!(c.after.io_events, 0);
        assert_eq!(c.before.unexpected_reboots, 2);
        assert!(c.summary.contains("most likely fixed it"));
    }

    #[test]
    fn same_error_rate_after_change_is_no_change() {
        let now = Utc::now();
        let ev: Vec<f64> = (0..60).map(|i| 0.05 + i as f64 * 0.1).collect(); // spread over 6 days
        let c = compare(&note(3.0, now), &events(&ev, now), &[], &[], &[], now);
        assert_eq!(c.effect, Effect::NoChange, "{c:?}");
    }

    #[test]
    fn more_errors_after_change_is_worse() {
        let now = Utc::now();
        let mut ev: Vec<f64> = (0..5).map(|i| 2.5 + i as f64 * 0.1).collect();
        ev.extend((0..30).map(|i| 0.1 + i as f64 * 0.05));
        let c = compare(&note(2.0, now), &events(&ev, now), &[], &[], &[], now);
        assert_eq!(c.effect, Effect::Worse);
    }

    #[test]
    fn recent_change_is_too_early() {
        let _en = crate::i18n::scoped(crate::i18n::Lang::En);
        let now = Utc::now();
        let c = compare(&note(0.25, now), &events(&[0.5, 0.6, 0.7], now), &[], &[], &[], now);
        assert_eq!(c.effect, Effect::TooEarly);
        assert!(c.summary.contains("run `diskdoctor health`"));
        let _pt = crate::i18n::scoped(crate::i18n::Lang::PtBr);
        let c = compare(&note(0.25, now), &[], &[], &[], &[], now);
        assert!(c.summary.contains("rode `diskdoctor health`"));
    }

    #[test]
    fn crc_growth_decides_when_there_are_no_events() {
        let _en = crate::i18n::scoped(crate::i18n::Lang::En);
        let now = Utc::now();
        let snap = |days: f64, crc: u64| SmartSnapshot { time: at(days, now), counters: BTreeMap::from([("interface_crc_errors".to_string(), crc)]) };
        let snaps = vec![snap(5.5, 100), snap(4.0, 180), snap(2.9, 230), snap(1.0, 230), snap(0.1, 230)];
        let c = compare(&note(3.0, now), &[], &[], &[], &snaps, now);
        assert_eq!(c.before.counter_growth, vec![("Interface CRC errors (199)".to_string(), 80)]);
        assert!(c.after.counter_growth.is_empty());
        assert_eq!(c.effect, Effect::Improved);
    }

    #[test]
    fn no_errors_before_is_not_measurable() {
        let now = Utc::now();
        let c = compare(&note(3.0, now), &[], &[], &[], &[], now);
        assert_eq!(c.effect, Effect::NotMeasurable);
    }

    #[test]
    fn journal_round_trip_keeps_notes_when_trimming() {
        let dir = tempfile::tempdir().unwrap();
        let j = Journal::new(dir.path());
        let now = Utc::now();
        j.append("fp1", &JournalEntry::Note(note(10.0, now))).unwrap();
        for i in 0..(MAX_LINES + 5) {
            let d = DiagnosisRecord {
                time: at(9.0 - i as f64 * 0.001, now),
                disk_number: 2,
                status: DiskStatus::Warning,
                headline: "x".into(),
                findings: vec!["comm.link".into()],
                io_events: i as u64,
                window_hours: 168,
            };
            j.append("fp1", &JournalEntry::Diagnosis(d)).unwrap();
        }
        let all = j.load("fp1");
        assert_eq!(all.len(), MAX_LINES);
        assert_eq!(j.notes("fp1").len(), 1, "a recorded change is never dropped");
        assert_eq!(j.latest_note("fp1").unwrap().text, "Replaced the SATA cable");
        assert!(all.windows(2).all(|w| w[0].time() <= w[1].time()));
    }
}
