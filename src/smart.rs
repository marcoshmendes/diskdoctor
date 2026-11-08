//! Integração com `smartctl` (smartmontools) e parsing do JSON (`--json=c`).
//!
//! Segurança: só executamos consultas. [`ensure_read_only_args`] rejeita qualquer
//! argumento que inicie autoteste, altere configuração SMART ou grave no disco,
//! e é chamado antes de toda execução.
//!
//! Mapeamento de dispositivos no Windows: o smartctl nomeia `\\.\PhysicalDriveN`
//! como `/dev/sdX` (a = 0, b = 1, ..., z = 25, aa = 26...). O tipo (`ata`, `sat`,
//! `nvme`...) vem de `smartctl --scan`.

use crate::platform::process::run_with_timeout;
use crate::util::mask_serial;
use anyhow::{Result, bail};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::LazyLock;
use std::time::Duration;

const SMARTCTL_TIMEOUT: Duration = Duration::from_secs(90);

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SmartAttribute {
    pub id: u32,
    pub name: String,
    pub value: Option<u32>,
    pub worst: Option<u32>,
    pub thresh: Option<u32>,
    pub raw_value: u64,
    pub raw_string: String,
    pub prefailure: bool,
    pub when_failed: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PhyCounter {
    pub id: u32,
    pub name: String,
    pub value: u64,
}

/// Dados SMART normalizados (ATA e NVMe) de um disco.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SmartReport {
    pub disk_number: u32,
    /// `true` se obtivemos dados SMART utilizáveis.
    pub available: bool,
    pub source: String,
    pub device: Option<String>,
    pub device_type: Option<String>,
    pub protocol: Option<String>,
    pub model: Option<String>,
    pub model_family: Option<String>,
    pub serial_masked: Option<String>,
    pub firmware: Option<String>,
    pub capacity_bytes: Option<u64>,
    /// RPM informado pelo disco; 0 = SSD. Usado para saber se um disco externo é HD ou SSD.
    #[serde(default)]
    pub rotation_rate: Option<u32>,
    pub smart_enabled: Option<bool>,
    /// Avaliação geral do próprio disco (PASSED/FAILED).
    pub overall_passed: Option<bool>,
    pub temperature_c: Option<i64>,
    pub temperature_max_c: Option<i64>,
    pub temp_warning_threshold_c: Option<i64>,
    pub temp_critical_threshold_c: Option<i64>,
    pub power_on_hours: Option<u64>,
    pub power_cycles: Option<u64>,
    pub unsafe_shutdowns: Option<u64>,
    pub wear_used_percent: Option<u64>,
    pub available_spare_percent: Option<u64>,
    pub available_spare_threshold: Option<u64>,
    pub reallocated_sectors: Option<u64>,
    pub reallocation_events: Option<u64>,
    pub pending_sectors: Option<u64>,
    pub offline_uncorrectable: Option<u64>,
    pub reported_uncorrectable: Option<u64>,
    pub grown_bad_blocks: Option<u64>,
    pub program_erase_failures: Option<u64>,
    /// Atributo 199 (UDMA/SATA CRC): erros no link SATA (cabo/conector/porta).
    pub interface_crc_errors: Option<u64>,
    /// Outros contadores "CRC" específicos do fabricante (ex.: 218 em controladores Phison).
    pub vendor_crc_errors: Option<u64>,
    /// Atributo 168 (SATA PHY errors) quando existir.
    pub sata_phy_errors: Option<u64>,
    /// Soma dos contadores de erro de FIS/CRC do log SATA Phy (desde a última energização).
    pub phy_interface_errors: Option<u64>,
    /// COMRESETs desde a energização (reinicializações do link).
    pub phy_link_resets: Option<u64>,
    pub command_timeouts: Option<u64>,
    pub end_to_end_errors: Option<u64>,
    /// NVMe: erros de integridade de mídia/dados.
    pub media_errors: Option<u64>,
    pub error_log_entries: Option<u64>,
    pub nvme_critical_warning: Option<u64>,
    pub nvme_warning_temp_minutes: Option<u64>,
    pub nvme_critical_temp_minutes: Option<u64>,
    pub self_test_failures: Option<u64>,
    pub phy_counters: Vec<PhyCounter>,
    pub attributes: Vec<SmartAttribute>,
    pub failing_attributes: Vec<String>,
    pub error_log_samples: Vec<String>,
    pub exit_status: Option<i64>,
    pub exit_flags: Vec<String>,
    pub messages: Vec<String>,
    pub error: Option<String>,
}

impl SmartReport {
    pub fn unavailable(disk_number: u32, source: &str, error: impl Into<String>) -> Self {
        Self { disk_number, source: source.to_string(), error: Some(error.into()), ..Default::default() }
    }

    pub fn is_nvme(&self) -> bool {
        self.protocol.as_deref().map(|p| p.eq_ignore_ascii_case("nvme")).unwrap_or(false)
    }

    /// Contadores numéricos usados no histórico (ver `history`).
    pub fn counters(&self) -> BTreeMap<String, u64> {
        let pairs = [
            ("reallocated_sectors", self.reallocated_sectors),
            ("pending_sectors", self.pending_sectors),
            ("offline_uncorrectable", self.offline_uncorrectable),
            ("reported_uncorrectable", self.reported_uncorrectable),
            ("interface_crc_errors", self.interface_crc_errors),
            ("vendor_crc_errors", self.vendor_crc_errors),
            ("sata_phy_errors", self.sata_phy_errors),
            ("command_timeouts", self.command_timeouts),
            ("media_errors", self.media_errors),
            ("error_log_entries", self.error_log_entries),
            ("grown_bad_blocks", self.grown_bad_blocks),
            ("program_erase_failures", self.program_erase_failures),
            ("unsafe_shutdowns", self.unsafe_shutdowns),
            ("power_on_hours", self.power_on_hours),
            ("power_cycles", self.power_cycles),
            ("wear_used_percent", self.wear_used_percent),
        ];
        pairs.into_iter().filter_map(|(k, v)| v.map(|v| (k.to_string(), v))).collect()
    }
}

// ---------------------------------------------------------------------------
// Execução do smartctl
// ---------------------------------------------------------------------------

/// Nome de dispositivo do smartctl para `\\.\PhysicalDriveN`.
pub fn device_name_for_disk(n: u32) -> String {
    let letter = |i: u32| char::from(b'a' + i as u8);
    if n < 26 { format!("/dev/sd{}", letter(n)) } else { format!("/dev/sd{}{}", letter((n - 26) / 26), letter((n - 26) % 26)) }
}

/// Argumentos da consulta (somente leitura).
pub fn build_query_args(device: &str, device_type: Option<&str>) -> Vec<String> {
    let mut args = vec!["-x".to_string(), "--json=c".to_string()];
    if let Some(t) = device_type.filter(|t| !t.is_empty()) {
        args.push("-d".into());
        args.push(t.to_string());
    }
    args.push(device.to_string());
    args
}

/// Rejeita argumentos que iniciem testes, alterem configuração ou gravem dados.
pub fn ensure_read_only_args(args: &[String]) -> Result<()> {
    const FORBIDDEN_LONG: &[&str] =
        &["--test", "--captive", "--abort", "--smart", "--offlineauto", "--saveauto", "--set", "--log=sataphy,reset", "--log-reset"];
    const FORBIDDEN_SHORT: &[&str] = &["-t", "-C", "-X", "-s", "-o", "-S", "-g"];
    for a in args {
        let lower = a.to_ascii_lowercase();
        if FORBIDDEN_LONG.iter().any(|f| lower == *f || lower.starts_with(&format!("{f}="))) || lower.contains("reset") {
            bail!("argumento proibido para o smartctl (somente leitura): {a}");
        }
        if FORBIDDEN_SHORT.iter().any(|f| a == f || (a.starts_with(f) && a.len() > f.len() && !a.starts_with("--"))) {
            bail!("argumento proibido para o smartctl (somente leitura): {a}");
        }
    }
    Ok(())
}

/// Executor do smartctl com tipos de dispositivo detectados por `--scan`.
#[derive(Debug, Clone)]
pub struct Smartctl {
    pub path: PathBuf,
    pub version: Option<String>,
    device_types: HashMap<String, String>,
}

impl Smartctl {
    /// Procura o executável: `DISKDOCTOR_SMARTCTL`, PATH e pastas padrão de instalação.
    pub fn locate() -> Option<PathBuf> {
        if let Some(p) = std::env::var_os("DISKDOCTOR_SMARTCTL").map(PathBuf::from) {
            if p.is_file() {
                return Some(p);
            }
        }
        if let Some(path) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&path) {
                let candidate = dir.join("smartctl.exe");
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
        }
        [r"C:\Program Files\smartmontools\bin\smartctl.exe", r"C:\Program Files (x86)\smartmontools\bin\smartctl.exe"]
            .iter()
            .map(PathBuf::from)
            .find(|p| p.is_file())
    }

    /// Localiza e executa `--scan` (rápido, não abre os discos).
    pub fn detect() -> Option<Self> {
        let path = Self::locate()?;
        let mut s = Self { path, version: None, device_types: HashMap::new() };
        if let Ok(out) = run_with_timeout(&s.path, &["--scan", "--json=c"], Duration::from_secs(30)) {
            if let Ok((version, types)) = parse_scan_json(&out.stdout) {
                s.version = version;
                s.device_types = types;
            }
        }
        Some(s)
    }

    pub fn label(&self) -> String {
        match &self.version {
            Some(v) => format!("smartctl {v}"),
            None => "smartctl".into(),
        }
    }

    pub fn query(&self, disk_number: u32) -> SmartReport {
        let device = device_name_for_disk(disk_number);
        let args = build_query_args(&device, self.device_types.get(&device).map(String::as_str));
        if let Err(e) = ensure_read_only_args(&args) {
            return SmartReport::unavailable(disk_number, &self.label(), e.to_string());
        }
        match run_with_timeout(&self.path, &args, SMARTCTL_TIMEOUT) {
            Err(e) => SmartReport::unavailable(disk_number, &self.label(), format!("{e:#}")),
            Ok(out) => match parse_smartctl_json(disk_number, &out.stdout) {
                Ok(mut r) => {
                    r.source = self.label();
                    r
                }
                Err(e) => SmartReport::unavailable(disk_number, &self.label(), format!("saída inesperada do smartctl: {e:#}")),
            },
        }
    }
}

/// Lê `smartctl --scan --json=c`: versão e mapa dispositivo → tipo.
pub fn parse_scan_json(json: &str) -> Result<(Option<String>, HashMap<String, String>)> {
    let v: Value = serde_json::from_str(json.trim())?;
    let version = v.pointer("/smartctl/version").and_then(Value::as_array).map(|parts| {
        parts.iter().filter_map(Value::as_u64).map(|p| p.to_string()).collect::<Vec<_>>().join(".")
    });
    let mut types = HashMap::new();
    for d in v.get("devices").and_then(Value::as_array).into_iter().flatten() {
        if let (Some(name), Some(t)) = (d.get("name").and_then(Value::as_str), d.get("type").and_then(Value::as_str)) {
            types.insert(name.to_string(), t.to_string());
        }
    }
    Ok((version, types))
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

fn get_u64(v: &Value, ptr: &str) -> Option<u64> {
    v.pointer(ptr).and_then(|x| x.as_u64().or_else(|| x.as_i64().filter(|i| *i >= 0).map(|i| i as u64)))
}
fn get_i64(v: &Value, ptr: &str) -> Option<i64> {
    v.pointer(ptr).and_then(Value::as_i64)
}
fn get_str(v: &Value, ptr: &str) -> Option<String> {
    v.pointer(ptr).and_then(Value::as_str).map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}
fn get_bool(v: &Value, ptr: &str) -> Option<bool> {
    v.pointer(ptr).and_then(Value::as_bool)
}

/// Interpreta os bits do código de saída do smartctl (ver `man smartctl`, RETURN VALUES).
pub fn decode_exit_status(status: i64) -> Vec<String> {
    const BITS: [&str; 8] = [
        "erro de linha de comando do smartctl",
        "não foi possível abrir o dispositivo (permissão ou dispositivo não suportado)",
        "algum comando SMART/ATA falhou (comum sem privilégios de Administrador)",
        "SMART indica FALHA IMINENTE do disco",
        "atributo de pré-falha abaixo do limite",
        "algum atributo já esteve abaixo do limite no passado",
        "o log de erros do dispositivo contém registros",
        "o log de autoteste contém falhas",
    ];
    BITS.iter().enumerate().filter(|(i, _)| status & (1 << i) != 0).map(|(_, t)| t.to_string()).collect()
}

/// Seagate empacota 3 contadores de 16 bits no atributo 188; o total fica nos bits baixos.
pub fn normalize_command_timeouts(raw: u64) -> u64 {
    if raw > 0xFFFF { raw & 0xFFFF } else { raw }
}

static MIN_MAX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"Min/Max\s+(-?\d+)/(-?\d+)").expect("regex"));
static FIRST_NUMBER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\s*(\d+)").expect("regex"));

/// Atributos cujo valor normalizado representa vida restante (100 = novo).
const LIFE_LEFT_ATTRS: &[&str] = &["ssd_life_left", "percent_lifetime_remain", "wear_leveling_count", "media_wearout_indicator", "remaining_lifetime_perc"];

/// IDs do log SATA Phy que indicam erro de transmissão (CRC/R_ERR em FIS).
const PHY_ERROR_IDS: &[u32] = &[0x1, 0x3, 0x4, 0x6, 0x7, 0xb, 0xd, 0xf, 0x10, 0x12, 0x13];
const PHY_COMRESET_ID: u32 = 0xa;

fn add(slot: &mut Option<u64>, v: u64) {
    *slot = Some(slot.unwrap_or(0) + v);
}

pub fn parse_smartctl_json(disk_number: u32, json: &str) -> Result<SmartReport> {
    let v: Value = serde_json::from_str(json.trim())?;
    let mut r = SmartReport { disk_number, source: "smartctl".into(), ..Default::default() };

    r.exit_status = get_i64(&v, "/smartctl/exit_status");
    r.exit_flags = r.exit_status.map(decode_exit_status).unwrap_or_default();
    r.messages = v
        .pointer("/smartctl/messages")
        .and_then(Value::as_array)
        .map(|m| m.iter().filter_map(|x| x.get("string").and_then(Value::as_str)).map(str::to_string).collect())
        .unwrap_or_default();

    r.device = get_str(&v, "/device/name");
    r.device_type = get_str(&v, "/device/type");
    r.protocol = get_str(&v, "/device/protocol");
    r.model = get_str(&v, "/model_name");
    r.model_family = get_str(&v, "/model_family");
    r.serial_masked = get_str(&v, "/serial_number").map(|s| mask_serial(&s));
    r.firmware = get_str(&v, "/firmware_version");
    r.rotation_rate = get_u64(&v, "/rotation_rate").map(|x| x.min(u32::MAX as u64) as u32);
    r.capacity_bytes = get_u64(&v, "/user_capacity/bytes");
    r.smart_enabled = get_bool(&v, "/smart_support/enabled");
    r.overall_passed = get_bool(&v, "/smart_status/passed");
    r.temperature_c = get_i64(&v, "/temperature/current");
    r.temperature_max_c = get_i64(&v, "/temperature/lifetime_max");
    r.temp_warning_threshold_c = get_i64(&v, "/nvme_composite_temperature_threshold/warning");
    r.temp_critical_threshold_c = get_i64(&v, "/nvme_composite_temperature_threshold/critical");
    r.power_on_hours = get_u64(&v, "/power_on_time/hours");
    r.power_cycles = get_u64(&v, "/power_cycle_count");
    r.wear_used_percent = get_u64(&v, "/endurance_used/current_percent");
    r.available_spare_percent = get_u64(&v, "/spare_available/current_percent");
    r.available_spare_threshold = get_u64(&v, "/spare_available/threshold_percent");

    // --- ATA: tabela de atributos ---
    let mut life_left_fallback: Option<u64> = None;
    for a in v.pointer("/ata_smart_attributes/table").and_then(Value::as_array).into_iter().flatten() {
        let attr = SmartAttribute {
            id: a.get("id").and_then(Value::as_u64).unwrap_or(0) as u32,
            name: a.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
            value: a.get("value").and_then(Value::as_u64).map(|x| x as u32),
            worst: a.get("worst").and_then(Value::as_u64).map(|x| x as u32),
            thresh: a.get("thresh").and_then(Value::as_u64).map(|x| x as u32),
            raw_value: get_u64(a, "/raw/value").unwrap_or(0),
            raw_string: get_str(a, "/raw/string").unwrap_or_default(),
            prefailure: get_bool(a, "/flags/prefailure").unwrap_or(false),
            when_failed: a.get("when_failed").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string),
        };
        let name = attr.name.to_ascii_lowercase();
        let raw = attr.raw_value;
        match attr.id {
            5 => r.reallocated_sectors = Some(raw),
            196 => r.reallocation_events = Some(raw),
            197 if name.contains("pending") => r.pending_sectors = Some(raw),
            198 if name.contains("uncorrect") || name.contains("offline") => r.offline_uncorrectable = Some(raw),
            187 => r.reported_uncorrectable = Some(raw),
            188 if name.contains("timeout") => r.command_timeouts = Some(normalize_command_timeouts(raw)),
            184 => r.end_to_end_errors = Some(raw),
            199 => r.interface_crc_errors = Some(raw),
            168 if name.contains("phy") => r.sata_phy_errors = Some(raw),
            190 | 194 => {
                if let Some(c) = MIN_MAX.captures(&attr.raw_string) {
                    let max: i64 = c[2].parse().unwrap_or(0);
                    r.temperature_max_c = Some(r.temperature_max_c.map_or(max, |m| m.max(max)));
                }
            }
            _ => {}
        }
        if attr.id != 199 && name.contains("crc") {
            add(&mut r.vendor_crc_errors, raw);
        }
        if name.contains("program_fail") || name.contains("erase_fail") {
            add(&mut r.program_erase_failures, raw);
        }
        if name.starts_with("bad_blk_ct") {
            // Kingston/Phison: "tardios/precoces" → blocos ruins adquiridos em uso = primeiro número.
            if let Some(c) = FIRST_NUMBER.captures(&attr.raw_string) {
                add(&mut r.grown_bad_blocks, c[1].parse().unwrap_or(0));
            }
        } else if name == "runtime_bad_block" || name.contains("grown_failing") {
            add(&mut r.grown_bad_blocks, raw);
        }
        if name.contains("unsafe_shutdown") || name.contains("unexpect_power_loss") || name.contains("power-off_retract") {
            r.unsafe_shutdowns = Some(r.unsafe_shutdowns.unwrap_or(0).max(raw));
        }
        if LIFE_LEFT_ATTRS.contains(&name.as_str()) {
            if let Some(val) = attr.value.filter(|&x| x <= 100) {
                life_left_fallback = Some(life_left_fallback.map_or(val as u64, |p: u64| p.min(val as u64)));
            }
        }
        if let Some(when) = &attr.when_failed {
            let state = if when.eq_ignore_ascii_case("now") || when.contains("FAILING_NOW") { "FALHANDO AGORA" } else { "falhou no passado" };
            r.failing_attributes.push(format!("{} {} ({state})", attr.id, attr.name));
        }
        r.attributes.push(attr);
    }
    if r.wear_used_percent.is_none() {
        r.wear_used_percent = life_left_fallback.map(|left| 100 - left.min(100));
    }

    // --- ATA: logs ---
    r.error_log_entries =
        get_u64(&v, "/ata_smart_error_log/extended/count").or_else(|| get_u64(&v, "/ata_smart_error_log/summary/count"));
    for path in ["/ata_smart_error_log/extended/table", "/ata_smart_error_log/summary/table"] {
        if let Some(table) = v.pointer(path).and_then(Value::as_array) {
            r.error_log_samples =
                table.iter().filter_map(|e| e.get("error_description").and_then(Value::as_str)).take(5).map(str::to_string).collect();
            if !r.error_log_samples.is_empty() {
                break;
            }
        }
    }
    let mut self_test_failures = None;
    for path in ["/ata_smart_self_test_log/standard/table", "/ata_smart_self_test_log/extended/table"] {
        if let Some(table) = v.pointer(path).and_then(Value::as_array) {
            let failed = table.iter().filter(|t| get_bool(t, "/status/passed") == Some(false)).count() as u64;
            self_test_failures = Some(self_test_failures.unwrap_or(0).max(failed));
        }
    }
    r.self_test_failures = self_test_failures;

    for c in v.pointer("/sata_phy_event_counters/table").and_then(Value::as_array).into_iter().flatten() {
        let counter = PhyCounter {
            id: c.get("id").and_then(Value::as_u64).unwrap_or(0) as u32,
            name: c.get("name").and_then(Value::as_str).unwrap_or("").to_string(),
            value: c.get("value").and_then(Value::as_u64).unwrap_or(0),
        };
        if PHY_ERROR_IDS.contains(&counter.id) {
            add(&mut r.phy_interface_errors, counter.value);
        } else if counter.id == PHY_COMRESET_ID {
            r.phy_link_resets = Some(counter.value);
        }
        r.phy_counters.push(counter);
    }

    // --- NVMe ---
    if let Some(h) = v.get("nvme_smart_health_information_log") {
        let n = |k: &str| h.get(k).and_then(Value::as_u64);
        r.nvme_critical_warning = n("critical_warning");
        r.temperature_c = r.temperature_c.or(h.get("temperature").and_then(Value::as_i64));
        r.available_spare_percent = n("available_spare").or(r.available_spare_percent);
        r.available_spare_threshold = n("available_spare_threshold").or(r.available_spare_threshold);
        r.wear_used_percent = n("percentage_used").or(r.wear_used_percent);
        r.unsafe_shutdowns = n("unsafe_shutdowns");
        r.media_errors = n("media_errors");
        r.error_log_entries = n("num_err_log_entries");
        r.nvme_warning_temp_minutes = n("warning_temp_time");
        r.nvme_critical_temp_minutes = n("critical_comp_time");
        r.power_on_hours = r.power_on_hours.or(n("power_on_hours"));
        r.power_cycles = r.power_cycles.or(n("power_cycles"));
        if r.overall_passed.is_none() {
            r.overall_passed = r.nvme_critical_warning.map(|w| w == 0);
        }
    }

    r.available = r.model.is_some() || !r.attributes.is_empty() || v.get("nvme_smart_health_information_log").is_some();
    if !r.available {
        let mut msg = String::from("o smartctl não retornou dados SMART deste disco");
        if r.exit_status.map(|s| s & 0b10 != 0).unwrap_or(false) {
            msg.push_str(
                ": não foi possível abrir o dispositivo. Execute como Administrador; discos atrás de controladoras \
                 RAID/Intel RST ou adaptadores USB podem não repassar comandos SMART",
            );
        }
        if let Some(m) = r.messages.first() {
            msg.push_str(&format!(" ({m})"));
        }
        r.error = Some(msg);
    }
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KINGSTON_120: &str = include_str!("../tests/fixtures/smartctl-sata-ssd-kingston120.json");
    const KINGSTON_240: &str = include_str!("../tests/fixtures/smartctl-sata-ssd-kingston240.json");
    const WD_GREEN: &str = include_str!("../tests/fixtures/smartctl-sata-ssd-wdgreen.json");
    const NVME: &str = include_str!("../tests/fixtures/smartctl-nvme-warning.json");
    const HDD_FAILING: &str = include_str!("../tests/fixtures/smartctl-sata-hdd-failing.json");

    #[test]
    fn maps_disk_numbers_to_device_names() {
        assert_eq!(device_name_for_disk(0), "/dev/sda");
        assert_eq!(device_name_for_disk(2), "/dev/sdc");
        assert_eq!(device_name_for_disk(25), "/dev/sdz");
        assert_eq!(device_name_for_disk(26), "/dev/sdaa");
        assert_eq!(device_name_for_disk(27), "/dev/sdab");
        assert_eq!(device_name_for_disk(52), "/dev/sdba");
    }

    #[test]
    fn query_args_are_read_only() {
        let args = build_query_args("/dev/sda", Some("nvme"));
        assert_eq!(args, vec!["-x", "--json=c", "-d", "nvme", "/dev/sda"]);
        ensure_read_only_args(&args).unwrap();
    }

    #[test]
    fn rejects_destructive_or_mutating_args() {
        for bad in [
            vec!["-t", "long"],
            vec!["--test=short"],
            vec!["-tlong"],
            vec!["-X"],
            vec!["-s", "off"],
            vec!["--smart=off"],
            vec!["-o", "on"],
            vec!["-S", "on"],
            vec!["--set=standby,now"],
            vec!["-l", "sataphy,reset"],
            vec!["-C"],
        ] {
            let args: Vec<String> = bad.iter().map(|s| s.to_string()).collect();
            assert!(ensure_read_only_args(&args).is_err(), "deveria rejeitar {args:?}");
        }
    }

    #[test]
    fn parses_scan_output() {
        let json = r#"{"smartctl":{"version":[7,5]},"devices":[{"name":"/dev/sda","type":"ata"},{"name":"/dev/sdb","type":"nvme"}]}"#;
        let (version, types) = parse_scan_json(json).unwrap();
        assert_eq!(version.as_deref(), Some("7.5"));
        assert_eq!(types.get("/dev/sdb").map(String::as_str), Some("nvme"));
    }

    #[test]
    fn parses_real_kingston_120() {
        let r = parse_smartctl_json(2, KINGSTON_120).unwrap();
        assert!(r.available);
        assert_eq!(r.model.as_deref(), Some("KINGSTON SA400S37120G"));
        assert_eq!(r.overall_passed, Some(true));
        assert_eq!(r.power_on_hours, Some(10719));
        assert_eq!(r.power_cycles, Some(2943));
        assert_eq!(r.unsafe_shutdowns, Some(219));
        assert_eq!(r.interface_crc_errors, Some(0));
        assert_eq!(r.vendor_crc_errors, Some(0));
        assert_eq!(r.grown_bad_blocks, Some(0), "Bad_Blk_Ct_Lat/Erl = 0/58 → 0 adquiridos");
        assert_eq!(r.program_erase_failures, Some(0));
        assert_eq!(r.reported_uncorrectable, Some(0));
        assert_eq!(r.wear_used_percent, Some(7));
        assert_eq!(r.temperature_max_c, Some(29));
        assert!(r.serial_masked.as_deref().unwrap().contains('*'));
        assert!(r.exit_flags.iter().any(|f| f.contains("comando SMART/ATA falhou")));
        assert!(r.failing_attributes.is_empty());
    }

    #[test]
    fn parses_vendor_crc_on_kingston_240() {
        let r = parse_smartctl_json(1, KINGSTON_240).unwrap();
        assert_eq!(r.vendor_crc_errors, Some(3485));
        assert_eq!(r.interface_crc_errors, Some(0));
        assert_eq!(r.sata_phy_errors, Some(0));
    }

    #[test]
    fn parses_wd_green_timeouts() {
        let r = parse_smartctl_json(0, WD_GREEN).unwrap();
        assert_eq!(r.command_timeouts, Some(2));
        assert_eq!(r.reallocated_sectors, Some(0));
        assert_eq!(r.available_spare_percent, Some(100));
        assert_eq!(r.wear_used_percent, Some(0));
    }

    #[test]
    fn parses_nvme_health_log() {
        let r = parse_smartctl_json(3, NVME).unwrap();
        assert!(r.is_nvme());
        assert_eq!(r.media_errors, Some(3));
        assert_eq!(r.wear_used_percent, Some(12));
        assert_eq!(r.unsafe_shutdowns, Some(40));
        assert_eq!(r.nvme_warning_temp_minutes, Some(15));
        assert_eq!(r.temp_warning_threshold_c, Some(82));
        assert_eq!(r.temperature_c, Some(74));
        assert_eq!(r.error_log_entries, Some(120));
        assert_eq!(r.serial_masked.as_deref(), Some("S5G********456A"));
    }

    #[test]
    fn reads_rotation_rate() {
        let hdd = parse_smartctl_json(3, r#"{"model_name":"WDC WD20SPZX","rotation_rate":5400}"#).unwrap();
        assert_eq!(hdd.rotation_rate, Some(5400));
        let ssd = parse_smartctl_json(3, r#"{"model_name":"X","rotation_rate":0}"#).unwrap();
        assert_eq!(ssd.rotation_rate, Some(0));
    }

    #[test]
    fn parses_failing_hdd() {
        let r = parse_smartctl_json(4, HDD_FAILING).unwrap();
        assert_eq!(r.reallocated_sectors, Some(48));
        assert_eq!(r.pending_sectors, Some(8));
        assert_eq!(r.offline_uncorrectable, Some(8));
        assert_eq!(r.reported_uncorrectable, Some(12));
        assert_eq!(r.command_timeouts, Some(1), "contador Seagate empacotado");
        assert_eq!(r.unsafe_shutdowns, Some(310));
        assert_eq!(r.error_log_entries, Some(23));
        assert_eq!(r.error_log_samples.len(), 2);
        assert_eq!(r.self_test_failures, Some(1));
        assert_eq!(r.phy_interface_errors, Some(0));
        assert_eq!(r.phy_link_resets, Some(2));
        assert!(r.exit_flags.iter().any(|f| f.contains("log de erros")));
        assert!(r.exit_flags.iter().any(|f| f.contains("autoteste")));
    }

    #[test]
    fn reports_unavailable_when_device_cannot_be_opened() {
        let json = r#"{"smartctl":{"exit_status":2,"messages":[{"string":"Smartctl open device: /dev/sdc failed: Access denied","severity":"error"}]}}"#;
        let r = parse_smartctl_json(2, json).unwrap();
        assert!(!r.available);
        let err = r.error.unwrap();
        assert!(err.contains("Administrador"));
        assert!(err.contains("Access denied"));
    }

    #[test]
    fn decodes_exit_bits() {
        assert!(decode_exit_status(0).is_empty());
        assert_eq!(decode_exit_status(8).len(), 1);
        assert!(decode_exit_status(8)[0].contains("FALHA IMINENTE"));
        assert_eq!(decode_exit_status(192).len(), 2);
    }

    #[test]
    fn counters_skip_missing_values() {
        let r = parse_smartctl_json(2, KINGSTON_120).unwrap();
        let c = r.counters();
        assert_eq!(c.get("unsafe_shutdowns"), Some(&219));
        assert!(!c.contains_key("pending_sectors"));
    }
}
