//! Windows reliability counters (`Get-StorageReliabilityCounter`).
//!
//! A source that complements SMART: Windows itself queries the drive and exposes
//! temperature, wear, read/write errors and maximum latencies. Requires Administrator;
//! without elevation the cmdlet fails and the result is empty.

use crate::platform;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ReliabilityCounters {
    pub temperature_c: Option<u64>,
    pub temperature_max_c: Option<u64>,
    pub wear_percent: Option<u64>,
    pub read_errors_total: Option<u64>,
    pub read_errors_corrected: Option<u64>,
    pub read_errors_uncorrected: Option<u64>,
    pub write_errors_total: Option<u64>,
    pub write_errors_uncorrected: Option<u64>,
    pub read_latency_max_ms: Option<u64>,
    pub write_latency_max_ms: Option<u64>,
    pub flush_latency_max_ms: Option<u64>,
    pub power_on_hours: Option<u64>,
    pub start_stop_cycles: Option<u64>,
}

impl ReliabilityCounters {
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

const SCRIPT: &str = r#"
$out = foreach ($p in @(Get-PhysicalDisk)) {
  $r = $null
  try { $r = Get-StorageReliabilityCounter -PhysicalDisk $p -ErrorAction Stop } catch { }
  if ($r) {
    [pscustomobject]@{
      DeviceId = [string]$p.DeviceId
      Temperature = $r.Temperature
      TemperatureMax = $r.TemperatureMax
      Wear = $r.Wear
      ReadErrorsTotal = $r.ReadErrorsTotal
      ReadErrorsCorrected = $r.ReadErrorsCorrected
      ReadErrorsUncorrected = $r.ReadErrorsUncorrected
      WriteErrorsTotal = $r.WriteErrorsTotal
      WriteErrorsUncorrected = $r.WriteErrorsUncorrected
      ReadLatencyMax = $r.ReadLatencyMax
      WriteLatencyMax = $r.WriteLatencyMax
      FlushLatencyMax = $r.FlushLatencyMax
      PowerOnHours = $r.PowerOnHours
      StartStopCycleCount = $r.StartStopCycleCount
    }
  }
}
ConvertTo-Json -InputObject @($out) -Compress
"#;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Raw {
    device_id: String,
    temperature: Option<u64>,
    temperature_max: Option<u64>,
    wear: Option<u64>,
    read_errors_total: Option<u64>,
    read_errors_corrected: Option<u64>,
    read_errors_uncorrected: Option<u64>,
    write_errors_total: Option<u64>,
    write_errors_uncorrected: Option<u64>,
    read_latency_max: Option<u64>,
    write_latency_max: Option<u64>,
    flush_latency_max: Option<u64>,
    power_on_hours: Option<u64>,
    start_stop_cycle_count: Option<u64>,
}

/// Converts the script JSON into counters per drive number.
pub fn parse_counters_json(json: &str) -> Result<HashMap<u32, ReliabilityCounters>> {
    let json = json.trim();
    if json.is_empty() {
        return Ok(HashMap::new());
    }
    let raw: Vec<Raw> = serde_json::from_str(json).context(crate::i18n::t("invalid counters JSON", "JSON de contadores inválido"))?;
    let nonzero = |v: Option<u64>| v.filter(|&x| x > 0);
    Ok(raw
        .into_iter()
        .filter_map(|r| {
            let n: u32 = r.device_id.trim().parse().ok()?;
            let c = ReliabilityCounters {
                // 0 °C / 0% mean "not supported" in these counters.
                temperature_c: nonzero(r.temperature),
                temperature_max_c: nonzero(r.temperature_max),
                wear_percent: r.wear,
                read_errors_total: r.read_errors_total,
                read_errors_corrected: r.read_errors_corrected,
                read_errors_uncorrected: r.read_errors_uncorrected,
                write_errors_total: r.write_errors_total,
                write_errors_uncorrected: r.write_errors_uncorrected,
                read_latency_max_ms: r.read_latency_max,
                write_latency_max_ms: r.write_latency_max,
                flush_latency_max_ms: r.flush_latency_max,
                power_on_hours: r.power_on_hours,
                start_stop_cycles: r.start_stop_cycle_count,
            };
            (!c.is_empty()).then_some((n, c))
        })
        .collect())
}

pub fn load_counters() -> Result<HashMap<u32, ReliabilityCounters>> {
    let json = platform::powershell::run_script(SCRIPT, Duration::from_secs(90))?;
    parse_counters_json(&json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_counters() {
        let json = r#"[{"DeviceId":"2","Temperature":31,"TemperatureMax":0,"Wear":7,"ReadErrorsTotal":null,"ReadErrorsUncorrected":0,"ReadLatencyMax":1534,"WriteLatencyMax":88,"PowerOnHours":10719},{"DeviceId":"x"}]"#;
        let map = parse_counters_json(json).unwrap();
        assert_eq!(map.len(), 1);
        let c = &map[&2];
        assert_eq!(c.temperature_c, Some(31));
        assert_eq!(c.temperature_max_c, None);
        assert_eq!(c.read_latency_max_ms, Some(1534));
        assert_eq!(c.wear_percent, Some(7));
    }

    #[test]
    fn empty_input_is_ok() {
        assert!(parse_counters_json("").unwrap().is_empty());
        assert!(parse_counters_json("[]").unwrap().is_empty());
    }
}
