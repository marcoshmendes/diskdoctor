//! Storage-related Windows events (System log).
//!
//! IDs alone are ambiguous (e.g. 129 is also used by Hyper-V, 153 by Kernel-Boot), so the
//! classification uses the (provider, ID) pair and, in some cases, the level. Events that
//! are not recognized are dropped.
//!
//! Attribution to a drive, from most to least reliable:
//! 1. `\Device\HarddiskN\...` in the properties/message → drive N (exact);
//! 2. "Disk N"/"Disco N" in the `disk` driver message → drive N (exact);
//! 3. `Volume{guid}` / `\Device\HarddiskVolumeN` / letter → drive holding the volume (exact);
//! 4. port driver events (storahci, stornvme...) → the only drive on that bus, if there is
//!    just one (inferred). Otherwise the event has no drive.
//!
//! Event messages come from Windows in its display language, so the patterns below also
//! match Portuguese words ("Disco", "unidade").

use crate::i18n::t;
use crate::inventory::{BusKind, DiskInfo, disk_for_letter, disk_for_volume_reference};
use crate::platform;
use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::LazyLock;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventKind {
    BadBlock,
    ControllerError,
    DeviceNotReady,
    PagingError,
    SmartPredictedFailure,
    IoRetried,
    IoFailedHardware,
    SurpriseRemoval,
    DeviceReset,
    DeviceTimeout,
    FsCorruption,
    VolumeNeedsRepair,
    TornWrite,
    FlushFailed,
    UnexpectedReboot,
    UnexpectedShutdown,
    WheaHardwareError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EventCategory {
    Media,
    Communication,
    Paging,
    FileSystem,
    Shutdown,
    Hardware,
}

impl EventCategory {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Media => t("Media", "Mídia"),
            Self::Communication => t("Communication/timeout", "Comunicação/timeout"),
            Self::Paging => t("Paging", "Paginação"),
            Self::FileSystem => t("File system", "Sistema de arquivos"),
            Self::Shutdown => t("Unexpected shutdown", "Desligamento inesperado"),
            Self::Hardware => "Hardware (WHEA)",
        }
    }

    /// Categories that point to a problem in the drive's own I/O path.
    pub fn is_storage_io(&self) -> bool {
        matches!(self, Self::Media | Self::Communication | Self::Paging)
    }
}

impl EventKind {
    pub fn description(&self) -> &'static str {
        match self {
            Self::BadBlock => t("bad block detected", "bloco defeituoso detectado (bad block)"),
            Self::ControllerError => t("controller error", "erro de controladora"),
            Self::DeviceNotReady => t("device was not ready", "dispositivo não estava pronto"),
            Self::PagingError => t("error during a paging operation (page file)", "erro durante operação de paginação (arquivo de paginação)"),
            Self::SmartPredictedFailure => t("SMART predicts a drive failure", "SMART prevê falha do disco"),
            Self::IoRetried => t("an I/O operation had to be retried", "operação de E/S precisou ser repetida (retry)"),
            Self::IoFailedHardware => t("an I/O operation failed due to a hardware error", "operação de E/S falhou por erro de hardware"),
            Self::SurpriseRemoval => t("drive removed/disconnected unexpectedly", "disco removido/desconectado inesperadamente"),
            Self::DeviceReset => t("reset issued to the device (command not answered)", "reset emitido para o dispositivo (comando sem resposta)"),
            Self::DeviceTimeout => t("device did not respond within the timeout", "dispositivo não respondeu dentro do tempo limite"),
            Self::FsCorruption => t("file system structure corrupted", "estrutura do sistema de arquivos corrompida"),
            Self::VolumeNeedsRepair => t("volume needs a check/repair (chkdsk)", "volume precisa de verificação/reparo (chkdsk)"),
            Self::TornWrite => t("interrupted write detected by NTFS", "gravação interrompida detectada pelo NTFS"),
            Self::FlushFailed => t("failed to write pending data (flush/delayed write)", "falha ao gravar dados pendentes (flush/delayed write)"),
            Self::UnexpectedReboot => t(
                "restart without a clean shutdown (hang or power loss)",
                "reinicialização sem desligamento correto (travamento ou queda de energia)",
            ),
            Self::UnexpectedShutdown => t("the previous shutdown was unexpected", "desligamento anterior foi inesperado"),
            Self::WheaHardwareError => t("hardware error reported by WHEA (PCIe/CPU/bus)", "erro de hardware reportado pelo WHEA (PCIe/CPU/barramento)"),
        }
    }

    pub fn category(&self) -> EventCategory {
        match self {
            Self::BadBlock | Self::SmartPredictedFailure | Self::IoFailedHardware => EventCategory::Media,
            Self::ControllerError
            | Self::DeviceNotReady
            | Self::IoRetried
            | Self::SurpriseRemoval
            | Self::DeviceReset
            | Self::DeviceTimeout => EventCategory::Communication,
            Self::PagingError => EventCategory::Paging,
            Self::FsCorruption | Self::VolumeNeedsRepair | Self::TornWrite | Self::FlushFailed => EventCategory::FileSystem,
            Self::UnexpectedReboot | Self::UnexpectedShutdown => EventCategory::Shutdown,
            Self::WheaHardwareError => EventCategory::Hardware,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Attribution {
    Exact,
    Inferred,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StorageEvent {
    pub time: DateTime<Utc>,
    pub provider: String,
    pub id: u32,
    /// 1 critical, 2 error, 3 warning, 4 information.
    pub level: u8,
    pub record_id: u64,
    pub kind: EventKind,
    pub category: EventCategory,
    pub disk_number: Option<u32>,
    pub attribution: Attribution,
    /// Device cited by the event (e.g. `\Device\Harddisk2\DR2`, `\Device\RaidPort0`).
    pub device: Option<String>,
    /// Cited LBA (153 events).
    pub lba: Option<u64>,
    pub message: String,
    #[serde(default)]
    pub properties: Vec<String>,
}

impl StorageEvent {
    pub fn code(&self) -> String {
        format!("{}/{}", self.provider, self.id)
    }

    pub fn level_label(&self) -> &'static str {
        match self.level {
            1 => t("Critical", "Crítico"),
            2 => t("Error", "Erro"),
            3 => t("Warning", "Aviso"),
            4 | 0 => t("Information", "Informação"),
            _ => t("Verbose", "Detalhe"),
        }
    }
}

/// IDs queried in the System log (filtered by provider afterwards).
pub const QUERY_EVENT_IDS: &[u32] = &[1, 7, 9, 11, 15, 17, 18, 19, 41, 50, 51, 52, 55, 57, 98, 129, 140, 153, 154, 157, 6008];

static PORT_PROVIDER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(stor(ahci|nvme|port|ufs)|iastor\w*|amd_?sata\w*|amdxata|amdsbs|nvstor\w*|nvme\w*|secnvme|rcraid|rste|lsi_\w+|megasas\w*|percsas\w*|arcsas|atapi|msahci|ahci\w*|uaspstor|usbstor|sdstor|microsoft-windows-storport)$",
    )
    .expect("regex")
});
// `Harddisk` followed directly by digits: does not match `HarddiskVolumeN`.
static RE_HARDDISK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\\Device\\Harddisk(\d+)(?:\\[A-Za-z]+\d*)?").expect("regex"));
static RE_DISK_WORD: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\b(?:disk|disco)\s+(\d+)\b").expect("regex"));
static RE_LETTER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\b(?:volume|unidade|drive)\s+([A-Z]):").expect("regex"));
static RE_PORT_DEVICE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\\Device\\[A-Za-z]+Port\d+|\\Device\\Ide\\\S+|\\Device\\Scsi\\\S+").expect("regex"));
static RE_HEX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^0x([0-9a-fA-F]+)$").expect("regex"));

pub fn is_storage_port_provider(provider: &str) -> bool {
    PORT_PROVIDER.is_match(provider)
}

/// Classifies (provider, ID, level). `None` = not a storage event.
pub fn classify(provider: &str, id: u32, level: u8) -> Option<EventKind> {
    use EventKind::*;
    let p = provider.trim().to_ascii_lowercase();
    if p == "disk" {
        return match id {
            7 => Some(BadBlock),
            11 => Some(ControllerError),
            15 => Some(DeviceNotReady),
            51 => Some(PagingError),
            52 => Some(SmartPredictedFailure),
            153 => Some(IoRetried),
            154 => Some(IoFailedHardware),
            157 => Some(SurpriseRemoval),
            _ => None,
        };
    }
    if is_storage_port_provider(&p) {
        return match id {
            129 => Some(DeviceReset),
            9 => Some(DeviceTimeout),
            11 => Some(ControllerError),
            15 => Some(DeviceNotReady),
            _ => None,
        };
    }
    if p == "ntfs" || p == "microsoft-windows-ntfs" {
        return match id {
            55 => Some(FsCorruption),
            98 if (1..=3).contains(&level) => Some(VolumeNeedsRepair),
            7 if p == "microsoft-windows-ntfs" => Some(TornWrite),
            50 | 57 | 140 => Some(FlushFailed),
            _ => None,
        };
    }
    match (p.as_str(), id) {
        ("microsoft-windows-kernel-power", 41) => Some(UnexpectedReboot),
        ("eventlog", 6008) => Some(UnexpectedShutdown),
        ("microsoft-windows-whea-logger", 1 | 17 | 18 | 19) => Some(WheaHardwareError),
        _ => None,
    }
}

/// Bus served by a port driver, to infer the drive of 129/9 events.
pub fn driver_bus(provider: &str) -> Option<BusKind> {
    let p = provider.to_ascii_lowercase();
    if p.contains("nvme") {
        Some(BusKind::Nvme)
    } else if p.starts_with("storahci") || p.starts_with("msahci") || p.starts_with("iastor") || p.contains("sata") || p == "atapi" || p == "amdxata" {
        Some(BusKind::Sata)
    } else if p == "uaspstor" || p == "usbstor" {
        Some(BusKind::Usb)
    } else {
        None
    }
}

/// Builds the PowerShell query script (window in hours, event limit).
pub fn build_query_script(hours: u32, max_events: u32) -> String {
    let ids = QUERY_EVENT_IDS.iter().map(|id| format!("EventID={id}")).collect::<Vec<_>>().join(" or ");
    let ms = hours as u64 * 3_600_000;
    format!(
        r#"
$xp = "*[System[({ids}) and TimeCreated[timediff(@SystemTime) <= {ms}]]]"
$ev = @(Get-WinEvent -LogName System -FilterXPath $xp -MaxEvents {max_events} -ErrorAction SilentlyContinue)
$out = foreach ($e in $ev) {{
  [pscustomobject]@{{
    TimeUtc = $e.TimeCreated.ToUniversalTime().ToString('o')
    Provider = [string]$e.ProviderName
    Id = [int]$e.Id
    Level = [int]$e.Level
    RecordId = [int64]$e.RecordId
    Properties = @($e.Properties | ForEach-Object {{ [string]$_.Value }})
    Message = [string]$e.Message
  }}
}}
ConvertTo-Json -InputObject @($out) -Depth 4 -Compress
"#
    )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawEvent {
    time_utc: String,
    provider: String,
    id: u32,
    level: Option<u8>,
    record_id: Option<u64>,
    #[serde(default)]
    properties: Option<Vec<Option<String>>>,
    message: Option<String>,
}

/// Converts the query JSON into classified events (drops irrelevant ones).
pub fn parse_events_json(json: &str) -> Result<Vec<StorageEvent>> {
    let json = json.trim();
    if json.is_empty() {
        return Ok(vec![]);
    }
    let raw: Vec<RawEvent> = serde_json::from_str(json).context(t("invalid events JSON", "JSON de eventos inválido"))?;
    let mut events: Vec<StorageEvent> = raw
        .into_iter()
        .filter_map(|e| {
            let level = e.level.unwrap_or(0);
            let kind = classify(&e.provider, e.id, level)?;
            let time = DateTime::parse_from_rfc3339(&e.time_utc).ok()?.with_timezone(&Utc);
            let properties: Vec<String> = e.properties.unwrap_or_default().into_iter().map(Option::unwrap_or_default).collect();
            let lba = if kind == EventKind::IoRetried {
                properties.get(1).and_then(|p| RE_HEX.captures(p.trim())).and_then(|c| u64::from_str_radix(&c[1], 16).ok())
            } else {
                None
            };
            Some(StorageEvent {
                time,
                provider: e.provider,
                id: e.id,
                level,
                record_id: e.record_id.unwrap_or(0),
                kind,
                category: kind.category(),
                disk_number: None,
                attribution: Attribution::None,
                device: None,
                lba,
                message: crate::util::one_line(&e.message.unwrap_or_default()),
                properties,
            })
        })
        .collect();
    events.sort_by_key(|e| std::cmp::Reverse(e.time));
    Ok(events)
}

/// Links each event to an inventory drive (see the rules at the top of the module).
pub fn attribute_events(events: &mut [StorageEvent], disks: &[DiskInfo]) {
    for e in events.iter_mut() {
        let haystack = format!("{} {}", e.properties.join(" "), e.message);

        if let Some(c) = RE_HARDDISK.captures(&haystack) {
            e.device = Some(c[0].to_string());
            if let Ok(n) = c[1].parse::<u32>() {
                if disks.is_empty() || disks.iter().any(|d| d.number == n) {
                    e.disk_number = Some(n);
                    e.attribution = Attribution::Exact;
                    continue;
                }
            }
        }
        if e.provider.eq_ignore_ascii_case("disk") {
            if let Some(n) = RE_DISK_WORD.captures(&e.message).and_then(|c| c[1].parse::<u32>().ok()) {
                e.disk_number = Some(n);
                e.attribution = Attribution::Exact;
                continue;
            }
        }
        if e.category == EventCategory::FileSystem {
            let by_volume = disk_for_volume_reference(disks, &haystack);
            let by_letter = || {
                RE_LETTER
                    .captures(&haystack)
                    .and_then(|c| disk_for_letter(disks, &c[1]))
                    .or_else(|| e.properties.iter().find(|p| p.len() == 2 && p.ends_with(':')).and_then(|p| disk_for_letter(disks, p)))
            };
            if let Some(n) = by_volume.or_else(by_letter) {
                e.disk_number = Some(n);
                e.attribution = Attribution::Exact;
                continue;
            }
        }
        if is_storage_port_provider(&e.provider) {
            e.device = RE_PORT_DEVICE.find(&haystack).map(|m| m.as_str().to_string());
            if let Some(bus) = driver_bus(&e.provider) {
                let mut candidates: Vec<&DiskInfo> = disks.iter().filter(|d| d.bus == bus).collect();
                if candidates.is_empty() && bus == BusKind::Sata {
                    // Intel RST/AMD RAID controllers expose SATA drives as "RAID".
                    candidates = disks.iter().filter(|d| d.bus == BusKind::Raid).collect();
                }
                if candidates.len() == 1 {
                    e.disk_number = Some(candidates[0].number);
                    e.attribution = Attribution::Inferred;
                }
            }
        }
    }
}

/// Queries, classifies and attributes the events of the last `hours` hours.
pub fn collect_events(hours: u32, max_events: u32, disks: &[DiskInfo]) -> Result<Vec<StorageEvent>> {
    let script = build_query_script(hours, max_events);
    let json = platform::powershell::run_script(&script, Duration::from_secs(120))
        .context(t("failed to query the Event Log (Get-WinEvent)", "falha ao consultar o Log de Eventos (Get-WinEvent)"))?;
    let mut events = parse_events_json(&json)?;
    attribute_events(&mut events, disks);
    Ok(events)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inventory::tests::fixture_disks;

    const FIXTURE: &str = include_str!("../tests/fixtures/events.json");

    #[test]
    fn classifies_by_provider_and_id() {
        assert_eq!(classify("disk", 153, 3), Some(EventKind::IoRetried));
        assert_eq!(classify("disk", 7, 2), Some(EventKind::BadBlock));
        assert_eq!(classify("storahci", 129, 3), Some(EventKind::DeviceReset));
        assert_eq!(classify("stornvme", 129, 3), Some(EventKind::DeviceReset));
        assert_eq!(classify("iaStorAC", 9, 2), Some(EventKind::DeviceTimeout));
        // The same IDs from other providers are not drive events.
        assert_eq!(classify("Microsoft-Windows-Hyper-V-Hypervisor", 129, 4), None);
        assert_eq!(classify("Microsoft-Windows-Kernel-Boot", 153, 4), None);
        assert_eq!(classify("Microsoft-Windows-Kernel-Processor-Power", 55, 4), None);
        // NTFS 98 only counts when it is not informational ("volume is healthy").
        assert_eq!(classify("Microsoft-Windows-Ntfs", 98, 2), Some(EventKind::VolumeNeedsRepair));
        assert_eq!(classify("Microsoft-Windows-Ntfs", 98, 4), None);
        assert_eq!(classify("Microsoft-Windows-Ntfs", 7, 3), Some(EventKind::TornWrite));
        assert_eq!(classify("Microsoft-Windows-Kernel-Power", 41, 1), Some(EventKind::UnexpectedReboot));
        assert_eq!(classify("EventLog", 6008, 2), Some(EventKind::UnexpectedShutdown));
        assert_eq!(classify("Microsoft-Windows-WHEA-Logger", 17, 3), Some(EventKind::WheaHardwareError));
    }

    #[test]
    fn query_script_contains_ids_and_window() {
        let s = build_query_script(48, 500);
        assert!(s.contains("EventID=129"));
        assert!(s.contains("EventID=153"));
        assert!(s.contains("172800000"));
        assert!(s.contains("-MaxEvents 500"));
    }

    #[test]
    fn parses_and_attributes_real_events() {
        let disks = fixture_disks();
        let mut events = parse_events_json(FIXTURE).unwrap();
        attribute_events(&mut events, &disks);

        // Irrelevant ones (Hyper-V 129, Kernel-Boot 153, Processor-Power 55, informational NTFS 98) are dropped.
        assert!(events.iter().all(|e| !e.provider.contains("Hyper-V")));
        assert!(events.iter().all(|e| !e.provider.contains("Kernel-Boot")));

        let retried: Vec<_> = events.iter().filter(|e| e.kind == EventKind::IoRetried).collect();
        assert_eq!(retried.len(), 4);
        assert!(retried.iter().all(|e| e.disk_number == Some(2) && e.attribution == Attribution::Exact));
        assert_eq!(retried[0].lba, Some(0x3af1e18));
        assert_eq!(retried[0].device.as_deref(), Some(r"\Device\Harddisk2\DR2"));

        let ntfs: Vec<_> = events.iter().filter(|e| e.kind == EventKind::VolumeNeedsRepair).collect();
        assert_eq!(ntfs.len(), 1);
        assert_eq!(ntfs[0].disk_number, Some(1), "Volume{{000000000104...}} is on drive 1");

        let reboot = events.iter().find(|e| e.kind == EventKind::UnexpectedReboot).unwrap();
        assert_eq!(reboot.disk_number, None);

        // Sorted from newest to oldest.
        assert!(events.windows(2).all(|w| w[0].time >= w[1].time));
    }

    #[test]
    fn infers_disk_for_port_driver_only_when_unique() {
        let disks = fixture_disks(); // three SATA drives
        let json = r#"[{"TimeUtc":"2026-11-08T10:00:00.0000000Z","Provider":"storahci","Id":129,"Level":3,"RecordId":1,"Properties":["\\Device\\RaidPort0"],"Message":"Reset to device, \\Device\\RaidPort0, was issued."}]"#;
        let mut events = parse_events_json(json).unwrap();
        attribute_events(&mut events, &disks);
        assert_eq!(events[0].disk_number, None, "three SATA drives: no way to tell which");
        assert_eq!(events[0].device.as_deref(), Some(r"\Device\RaidPort0"));

        let one_sata: Vec<DiskInfo> = disks.into_iter().filter(|d| d.number == 2).collect();
        let mut events = parse_events_json(json).unwrap();
        attribute_events(&mut events, &one_sata);
        assert_eq!(events[0].disk_number, Some(2));
        assert_eq!(events[0].attribution, Attribution::Inferred);
    }

    #[test]
    fn maps_drivers_to_bus() {
        assert_eq!(driver_bus("stornvme"), Some(BusKind::Nvme));
        assert_eq!(driver_bus("storahci"), Some(BusKind::Sata));
        assert_eq!(driver_bus("iaStorAC"), Some(BusKind::Sata));
        assert_eq!(driver_bus("uaspstor"), Some(BusKind::Usb));
        assert_eq!(driver_bus("disk"), None);
    }
}
