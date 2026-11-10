//! Pure helpers: localized number and date formatting, serial masking, sizes, RNG.

use crate::i18n::is_pt;
use anyhow::{Result, bail};
use chrono::{DateTime, Local, Utc};
use std::time::Duration;

/// Partially masks a serial number, keeping its start and end.
///
/// `50026B7600001234` → `500*********1234`. Separators (`_ . - space`) are kept so the
/// format stays readable (e.g. NVMe EUI).
pub fn mask_serial(serial: &str) -> String {
    let chars: Vec<char> = serial.trim().chars().filter(|c| !c.is_control()).collect();
    let n = chars.len();
    if n == 0 {
        return String::new();
    }
    if n <= 4 {
        return "*".repeat(n);
    }
    let (head, tail) = if n <= 8 { (1, 2) } else { (3, 4) };
    chars
        .iter()
        .enumerate()
        .map(|(i, c)| if i < head || i >= n - tail || matches!(c, '_' | '.' | '-' | ' ') { *c } else { '*' })
        .collect()
}

/// Stable identifier (64-bit FNV-1a) that links results of the same drive across runs
/// without storing the full serial in reports.
pub fn fingerprint(input: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for b in input.trim().as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{hash:016x}")
}

/// Number with decimal places: `1.5` (en) or `1,5` (pt-BR).
pub fn decimal(value: f64, places: usize) -> String {
    let s = format!("{value:.places$}");
    if is_pt() { s.replace('.', ",") } else { s }
}

/// Integer with thousands separators: `10,719` (en) or `10.719` (pt-BR).
pub fn int(value: u64) -> String {
    let sep = if is_pt() { '.' } else { ',' };
    let s = value.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(sep);
        }
        out.push(ch);
    }
    out
}

/// Size in decimal units (as drive makers advertise).
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    if bytes < 1000 {
        return format!("{bytes} B");
    }
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1000.0 && i < UNITS.len() - 1 {
        v /= 1000.0;
        i += 1;
    }
    let places = if v >= 100.0 { 0 } else { 1 };
    format!("{} {}", decimal(v, places), UNITS[i])
}

/// Size in binary units (for I/O blocks).
pub fn format_bytes_binary(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if (v - v.round()).abs() < 1e-9 { format!("{} {}", v.round() as u64, UNITS[i]) } else { format!("{} {}", decimal(v, 1), UNITS[i]) }
}

pub fn format_mbps(mbps: f64) -> String {
    format!("{} MB/s", decimal(mbps, 1))
}

pub fn format_ms(ms: f64) -> String {
    if ms >= 1000.0 {
        format!("{} s", decimal(ms / 1000.0, 2))
    } else if ms >= 10.0 {
        format!("{} ms", decimal(ms, 0))
    } else {
        format!("{} ms", decimal(ms, 2))
    }
}

pub fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    if secs == 0 {
        return format!("{} ms", d.as_millis());
    }
    let (h, m, s) = (secs / 3600, (secs % 3600) / 60, secs % 60);
    if h > 0 {
        format!("{h} h {m:02} min")
    } else if m > 0 {
        format!("{m} min {s:02} s")
    } else {
        format!("{s} s")
    }
}

fn local(t: DateTime<Utc>, en: &str, pt: &str) -> String {
    t.with_timezone(&Local).format(if is_pt() { pt } else { en }).to_string()
}

/// Local date and time: `2026-11-08 23:10` (en) or `08/11/2026 23:10` (pt-BR).
pub fn local_datetime(t: DateTime<Utc>) -> String {
    local(t, "%Y-%m-%d %H:%M", "%d/%m/%Y %H:%M")
}

/// Local date and time with seconds.
pub fn local_datetime_secs(t: DateTime<Utc>) -> String {
    local(t, "%Y-%m-%d %H:%M:%S", "%d/%m/%Y %H:%M:%S")
}

/// Local date: `2026-11-08` (en) or `08/11/2026` (pt-BR).
pub fn local_date(t: DateTime<Utc>) -> String {
    local(t, "%Y-%m-%d", "%d/%m/%Y")
}

/// Short local date and time for tables: `Nov 8 23:10` (en) or `08/11 23:10` (pt-BR).
pub fn local_short(t: DateTime<Utc>) -> String {
    local(t, "%b %-d %H:%M", "%d/%m %H:%M")
}

/// Parses sizes such as `512M`, `10G`, `1.5T`, `1,5T` or `4096` (base 1024).
pub fn parse_size(input: &str) -> Result<u64> {
    let s = input.trim().replace('_', "");
    if s.is_empty() {
        bail!("{}", crate::i18n::t("empty size", "tamanho vazio"));
    }
    let split = s.find(|c: char| !c.is_ascii_digit() && c != '.' && c != ',').unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let num: f64 = num.replace(',', ".").parse().map_err(|_| anyhow::anyhow!(tr!("invalid number in '{input}'", "número inválido em '{input}'")))?;
    let mult: u64 = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" | "kib" => 1 << 10,
        "m" | "mb" | "mib" => 1 << 20,
        "g" | "gb" | "gib" => 1 << 30,
        "t" | "tb" | "tib" => 1 << 40,
        other => bail!(tr!("unknown unit '{other}' in '{input}' (use K, M, G or T)", "unidade desconhecida '{other}' em '{input}' (use K, M, G ou T)")),
    };
    if !num.is_finite() || num < 0.0 {
        bail!(tr!("invalid size '{input}'", "tamanho inválido '{input}'"));
    }
    Ok((num * mult as f64).round() as u64)
}

pub fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

/// Collapses whitespace and line breaks into single spaces.
pub fn one_line(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn align_down(v: u64, a: u64) -> u64 {
    if a == 0 { v } else { v - v % a }
}

pub fn align_up(v: u64, a: u64) -> u64 {
    if a == 0 || v % a == 0 { v } else { v + (a - v % a) }
}

pub fn plural(n: u64, singular: &str, plural: &str) -> String {
    format!("{} {}", int(n), if n == 1 { singular } else { plural })
}

/// Small deterministic pseudo-random generator (SplitMix64).
#[derive(Debug, Clone)]
pub struct SplitMix64(u64);

impl SplitMix64 {
    pub fn new(seed: u64) -> Self {
        Self(seed)
    }
    pub fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next_u64() % n }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::i18n::{Lang, scoped};
    use chrono::TimeZone;

    #[test]
    fn masks_serials() {
        assert_eq!(mask_serial("50026B7600001234"), "500*********1234");
        assert_eq!(mask_serial("  23441V000003 "), "234*****0003");
        assert_eq!(mask_serial("ABCDEFG"), "A****FG");
        assert_eq!(mask_serial("ABC"), "***");
        assert_eq!(mask_serial(""), "");
        assert_eq!(mask_serial("0025_3881_1234_5678."), "002*_****_****_*678.");
        // The full serial must never leak.
        let s = "S4EWNX0R123456A";
        assert_ne!(mask_serial(s), s);
    }

    #[test]
    fn fingerprint_is_stable_and_trimmed() {
        assert_eq!(fingerprint("abc"), fingerprint(" abc "));
        assert_ne!(fingerprint("abc"), fingerprint("abd"));
        assert_eq!(fingerprint("abc").len(), 16);
    }

    #[test]
    fn formats_numbers_in_english() {
        let _en = scoped(Lang::En);
        assert_eq!(format_bytes(120_034_123_776), "120 GB");
        assert_eq!(format_bytes(1_000_204_886_016), "1.0 TB");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes_binary(1 << 20), "1 MiB");
        assert_eq!(format_bytes_binary(1536), "1.5 KiB");
        assert_eq!(int(10719), "10,719");
        assert_eq!(int(999), "999");
        assert_eq!(int(1_000_000), "1,000,000");
        assert_eq!(format_ms(1.234), "1.23 ms");
        assert_eq!(format_ms(2500.0), "2.50 s");
        assert_eq!(format_mbps(523.44), "523.4 MB/s");
        assert_eq!(format_duration(Duration::from_secs(3 * 3600 + 5 * 60)), "3 h 05 min");
        assert_eq!(format_duration(Duration::from_secs(75)), "1 min 15 s");
        assert_eq!(format_duration(Duration::from_millis(250)), "250 ms");
    }

    #[test]
    fn formats_numbers_in_portuguese() {
        let _pt = scoped(Lang::PtBr);
        assert_eq!(format_bytes(1_000_204_886_016), "1,0 TB");
        assert_eq!(format_bytes_binary(1536), "1,5 KiB");
        assert_eq!(int(10719), "10.719");
        assert_eq!(int(1_000_000), "1.000.000");
        assert_eq!(format_ms(1.234), "1,23 ms");
        assert_eq!(format_mbps(523.44), "523,4 MB/s");
    }

    #[test]
    fn formats_dates_per_language() {
        let t = Local.with_ymd_and_hms(2026, 11, 8, 23, 10, 5).unwrap().with_timezone(&Utc);
        {
            let _en = scoped(Lang::En);
            assert_eq!(local_datetime(t), "2026-11-08 23:10");
            assert_eq!(local_date(t), "2026-11-08");
            assert_eq!(local_short(t), "Nov 8 23:10");
            assert_eq!(local_datetime_secs(t), "2026-11-08 23:10:05");
        }
        let _pt = scoped(Lang::PtBr);
        assert_eq!(local_datetime(t), "08/11/2026 23:10");
        assert_eq!(local_short(t), "08/11 23:10");
    }

    #[test]
    fn parses_sizes() {
        assert_eq!(parse_size("4096").unwrap(), 4096);
        assert_eq!(parse_size("1M").unwrap(), 1 << 20);
        assert_eq!(parse_size("512mb").unwrap(), 512 << 20);
        assert_eq!(parse_size("10G").unwrap(), 10 << 30);
        assert_eq!(parse_size("1,5G").unwrap(), 3 << 29);
        assert_eq!(parse_size("1.5G").unwrap(), 3 << 29);
        assert_eq!(parse_size("2TiB").unwrap(), 2 << 40);
        assert!(parse_size("10X").is_err());
        assert!(parse_size("").is_err());
        assert!(parse_size("abc").is_err());
    }

    #[test]
    fn escapes_html() {
        assert_eq!(html_escape("<script>\"x\" & 'y'</script>"), "&lt;script&gt;&quot;x&quot; &amp; &#39;y&#39;&lt;/script&gt;");
    }

    #[test]
    fn alignment_helpers() {
        assert_eq!(align_down(4097, 4096), 4096);
        assert_eq!(align_up(4097, 4096), 8192);
        assert_eq!(align_up(8192, 4096), 8192);
    }

    #[test]
    fn rng_is_deterministic() {
        let mut a = SplitMix64::new(42);
        let mut b = SplitMix64::new(42);
        for _ in 0..10 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
        assert!(a.below(10) < 10);
        assert_eq!(a.below(0), 0);
    }

    #[test]
    fn truncates_and_flattens() {
        assert_eq!(truncate_chars("abcdef", 4), "abc…");
        assert_eq!(truncate_chars("abc", 4), "abc");
        assert_eq!(one_line("a\n  b\r\n c"), "a b c");
    }
}
