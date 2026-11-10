//! Physical drive inventory through PowerShell/CIM (`Get-Disk`, `Get-PhysicalDisk`,
//! `Win32_DiskDrive`, `Get-Partition`, `Get-Volume`).
//!
//! The script projects every property with an explicit cast (`[string]`, `[uint64]`) so
//! the JSON has a predictable shape on PowerShell 5.1. Enums (BusType, MediaType) may come
//! as a name ("SATA") or a number ("11") depending on the environment; the parsers accept
//! both.

use crate::i18n::{is_pt, t};
use crate::platform;
use crate::util::{fingerprint, mask_serial};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BusKind {
    Sata,
    Nvme,
    Usb,
    Sas,
    Scsi,
    Ata,
    Raid,
    /// SD card reader attached directly to the bus (laptops).
    Sd,
    Mmc,
    /// FireWire.
    Ieee1394,
    Virtual,
    Other,
}

impl BusKind {
    pub fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "sata" | "11" => Self::Sata,
            "nvme" | "17" => Self::Nvme,
            "usb" | "7" => Self::Usb,
            "sas" | "10" => Self::Sas,
            "scsi" | "1" => Self::Scsi,
            "ata" | "3" | "atapi" | "2" => Self::Ata,
            "raid" | "8" => Self::Raid,
            "sd" | "12" => Self::Sd,
            "mmc" | "13" => Self::Mmc,
            "1394" | "4" => Self::Ieee1394,
            "virtual"
            | "14"
            | "file backed virtual"
            | "15"
            | "storage spaces"
            | "spaces"
            | "16" => Self::Virtual,
            _ => Self::Other,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Sata => "SATA",
            Self::Nvme => "NVMe",
            Self::Usb => "USB",
            Self::Sas => "SAS",
            Self::Scsi => "SCSI",
            Self::Ata => "ATA",
            Self::Raid => "RAID",
            Self::Sd => "SD",
            Self::Mmc => "MMC",
            Self::Ieee1394 => "FireWire",
            Self::Virtual => "Virtual",
            Self::Other => t("Other", "Outro"),
        }
    }

    /// Buses of devices that connect from outside the case.
    pub fn is_external_bus(&self) -> bool {
        matches!(self, Self::Usb | Self::Sd | Self::Mmc | Self::Ieee1394)
    }
}

/// Form of the device, from the user's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    /// Internal drive (SATA, NVMe, SAS…).
    Internal,
    /// External HDD or SSD (USB enclosure, portable drive).
    ExternalDrive,
    /// USB flash drive (USB removable media).
    FlashDrive,
    /// SD/microSD/CF card in an internal or USB reader.
    MemoryCard,
}

static RE_CARD_MODEL: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
    regex::Regex::new(r"(?i)\b(sd|sdhc|sdxc|micro ?sd|mmc|card|reader|cf|compact ?flash|ms ?pro|memory ?stick|xd)\b").expect("regex")
});

/// Classifies the device from the bus, `Win32_DiskDrive.MediaType` ("Fixed hard disk
/// media", "External hard disk media", "Removable Media") and the model.
pub fn infer_device_kind(bus: BusKind, drive_media_type: Option<&str>, model: &str) -> DeviceKind {
    let media = drive_media_type.unwrap_or_default().to_ascii_lowercase();
    let removable = media.contains("removable");
    let external = media.contains("external");
    if matches!(bus, BusKind::Sd | BusKind::Mmc)
        || ((removable || bus == BusKind::Usb) && RE_CARD_MODEL.is_match(model))
    {
        return DeviceKind::MemoryCard;
    }
    if removable {
        return DeviceKind::FlashDrive;
    }
    if external || bus.is_external_bus() {
        return DeviceKind::ExternalDrive;
    }
    DeviceKind::Internal
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    Hdd,
    Ssd,
    Unknown,
}

/// Infers the media type (HDD/SSD) from what Windows reports and from heuristics.
pub fn infer_media(
    raw_media: Option<&str>,
    bus: BusKind,
    spindle_rpm: Option<u32>,
    model: &str,
) -> MediaKind {
    match raw_media.map(|s| s.trim().to_ascii_uppercase()).as_deref() {
        Some("SSD" | "4" | "SCM" | "5") => return MediaKind::Ssd,
        Some("HDD" | "3") => return MediaKind::Hdd,
        _ => {}
    }
    if bus == BusKind::Nvme {
        return MediaKind::Ssd;
    }
    if let Some(rpm) = spindle_rpm {
        if rpm > 0 && rpm != u32::MAX {
            return MediaKind::Hdd;
        }
    }
    let m = model.to_ascii_uppercase();
    if m.contains("SSD") || m.contains("NVME") {
        MediaKind::Ssd
    } else {
        MediaKind::Unknown
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PartitionInfo {
    pub number: u32,
    pub drive_letter: Option<String>,
    pub offset: u64,
    pub size: u64,
    pub kind: String,
    pub is_boot: bool,
    pub is_system: bool,
    pub file_system: Option<String>,
    pub label: Option<String>,
    pub volume_size: Option<u64>,
    pub volume_free: Option<u64>,
    /// `\\?\Volume{guid}\` paths (as they appear in NTFS events).
    pub volume_paths: Vec<String>,
    /// `\Device\HarddiskVolumeN` resolved with `QueryDosDevice`.
    pub device_path: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DiskInfo {
    /// Windows drive number (= N of `\\.\PhysicalDriveN`).
    pub number: u32,
    pub model: String,
    pub serial_masked: String,
    /// Hash of model+serial, to link runs without exposing the serial.
    pub fingerprint: String,
    pub size_bytes: u64,
    pub bus: BusKind,
    pub bus_raw: String,
    pub media: MediaKind,
    /// Internal, external, USB flash drive or memory card.
    #[serde(default = "default_kind")]
    pub kind: DeviceKind,
    pub firmware: Option<String>,
    pub partition_style: String,
    pub is_boot: bool,
    pub is_system: bool,
    pub is_offline: bool,
    pub is_read_only: bool,
    pub logical_sector_size: u32,
    pub physical_sector_size: u32,
    pub windows_health: String,
    pub operational_status: String,
    pub location: Option<String>,
    pub spindle_rpm: Option<u32>,
    pub partitions: Vec<PartitionInfo>,
    /// User tags ("Studio", "Games"…), filled from `tags.json` (`tags::TagBook::apply`);
    /// empty right after the inventory.
    #[serde(default)]
    pub tags: Vec<String>,
}

/// Short identity of the drive, saved together with test results.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DiskIdentity {
    pub number: u32,
    pub model: String,
    pub serial_masked: String,
    pub fingerprint: String,
    pub size_bytes: u64,
    pub class_label: String,
    /// Tags at test time (refreshed with the current tags by `report`).
    #[serde(default)]
    pub tags: Vec<String>,
}

/// "Drive 2 · KINGSTON SA400S37120G" or, with tags, "Drive 2 [Studio, Windows] · KINGSTON SA400S37120G"
/// ("Disco 2 …" in pt-BR).
pub fn disk_title(number: u32, model: &str, tags: &[String]) -> String {
    let drive = t("Drive", "Disco");
    if tags.is_empty() {
        format!("{drive} {number} · {model}")
    } else {
        format!("{drive} {number} [{}] · {model}", tags.join(", "))
    }
}

impl DiskIdentity {
    pub fn title(&self) -> String {
        disk_title(self.number, &self.model, &self.tags)
    }
}

fn default_kind() -> DeviceKind {
    DeviceKind::Internal
}

impl DiskInfo {
    pub fn is_ssd(&self) -> bool {
        self.media == MediaKind::Ssd || self.bus == BusKind::Nvme
    }

    pub fn is_hdd(&self) -> bool {
        self.media == MediaKind::Hdd
    }

    /// USB flash drive or memory card: no SMART, and the typical target of fake capacity.
    pub fn is_flash_media(&self) -> bool {
        matches!(self.kind, DeviceKind::FlashDrive | DeviceKind::MemoryCard)
    }

    pub fn is_external(&self) -> bool {
        self.kind != DeviceKind::Internal
    }

    /// "NVMe SSD", "SATA HDD", "External USB SSD", "USB flash drive", "Memory card"… in
    /// English; "SSD NVMe", "HD SATA", "SSD externo USB", "Pendrive USB", "Cartão de memória"…
    /// in pt-BR.
    pub fn class_label(&self) -> String {
        let bus = self.bus.label();
        let ssd = self.media == MediaKind::Ssd || (self.media == MediaKind::Unknown && self.bus == BusKind::Nvme);
        let media = match (ssd, self.media) {
            (true, _) => "SSD",
            (false, MediaKind::Hdd) => t("HDD", "HD"),
            _ => t("drive", "Disco"),
        };
        if is_pt() {
            match self.kind {
                DeviceKind::MemoryCard => "Cartão de memória".into(),
                DeviceKind::FlashDrive => format!("Pendrive {bus}"),
                DeviceKind::ExternalDrive => format!("{media} externo {bus}"),
                DeviceKind::Internal => format!("{media} {bus}"),
            }
        } else {
            match self.kind {
                DeviceKind::MemoryCard => "Memory card".into(),
                DeviceKind::FlashDrive => format!("{bus} flash drive"),
                DeviceKind::ExternalDrive => format!("External {bus} {media}"),
                DeviceKind::Internal => format!("{bus} {media}"),
            }
        }
    }

    pub fn volume_letters(&self) -> Vec<String> {
        self.partitions
            .iter()
            .filter_map(|p| p.drive_letter.as_ref().map(|l| format!("{l}:")))
            .collect()
    }

    pub fn identity(&self) -> DiskIdentity {
        DiskIdentity {
            number: self.number,
            model: self.model.clone(),
            serial_masked: self.serial_masked.clone(),
            fingerprint: self.fingerprint.clone(),
            size_bytes: self.size_bytes,
            class_label: self.class_label(),
            tags: self.tags.clone(),
        }
    }

    /// Short name for headings: "Drive 2 [Studio] · KINGSTON SA400S37120G".
    pub fn title(&self) -> String {
        disk_title(self.number, &self.model, &self.tags)
    }
}

pub fn find_disk(disks: &[DiskInfo], number: u32) -> Option<&DiskInfo> {
    disks.iter().find(|d| d.number == number)
}

fn partition_matches(p: &PartitionInfo, lower: &str) -> bool {
    let guid_match = p.volume_paths.iter().any(|vp| {
        let key = vp
            .trim_start_matches(r"\\?\")
            .trim_end_matches('\\')
            .to_ascii_lowercase();
        key.starts_with("volume{") && lower.contains(&key)
    });
    let device_match = p.device_path.as_ref().is_some_and(|dp| {
        let dp = dp.to_ascii_lowercase();
        // Keeps HarddiskVolume1 from matching HarddiskVolume10.
        lower
            .match_indices(&dp)
            .any(|(i, _)| !lower[i + dp.len()..].starts_with(|c: char| c.is_ascii_digit()))
    });
    guid_match || device_match
}

/// Partition of this drive cited in `text` (`Volume{guid}` or `\Device\HarddiskVolumeN`).
pub fn partition_for_reference<'a>(disk: &'a DiskInfo, text: &str) -> Option<&'a PartitionInfo> {
    let lower = text.to_ascii_lowercase();
    disk.partitions
        .iter()
        .find(|p| partition_matches(p, &lower))
}

/// Drive holding the volume cited in `text` (`Volume{guid}` or `\Device\HarddiskVolumeN`).
pub fn disk_for_volume_reference(disks: &[DiskInfo], text: &str) -> Option<u32> {
    disks
        .iter()
        .find(|d| partition_for_reference(d, text).is_some())
        .map(|d| d.number)
}

impl PartitionInfo {
    /// "C:" or "partition 4 without letter (Recovery)".
    pub fn display_name(&self) -> String {
        match &self.drive_letter {
            Some(l) => format!("{l}:"),
            None if self.kind.is_empty() => tr!("partition {} without letter", "partição {} sem letra", self.number),
            None => tr!("partition {} without letter ({})", "partição {} sem letra ({})", self.number, self.kind),
        }
    }
}

pub fn disk_for_letter(disks: &[DiskInfo], letter: &str) -> Option<u32> {
    let letter = letter.trim().trim_end_matches(':').to_ascii_uppercase();
    disks
        .iter()
        .find(|d| {
            d.partitions.iter().any(|p| {
                p.drive_letter
                    .as_deref()
                    .map(|l| l.eq_ignore_ascii_case(&letter))
                    .unwrap_or(false)
            })
        })
        .map(|d| d.number)
}

// ---------------------------------------------------------------------------
// Collection through PowerShell
// ---------------------------------------------------------------------------

const INVENTORY_SCRIPT: &str = r#"
$phys = @{}
try { foreach ($p in @(Get-PhysicalDisk)) { $phys[[string]$p.DeviceId] = $p } } catch { }
$wdd = @{}
try { foreach ($w in @(Get-CimInstance Win32_DiskDrive)) { $wdd[[string]$w.Index] = $w } } catch { }
$out = foreach ($d in @(Get-Disk | Sort-Object Number)) {
  $p = $phys[[string]$d.Number]
  $w = $wdd[[string]$d.Number]
  $parts = @(foreach ($pt in @(Get-Partition -DiskNumber $d.Number -ErrorAction SilentlyContinue)) {
    $v = $null
    try { $v = $pt | Get-Volume -ErrorAction Stop } catch { }
    $letter = $null
    if ($pt.DriveLetter -and [int][char]$pt.DriveLetter -ne 0) { $letter = [string]$pt.DriveLetter }
    [pscustomobject]@{
      PartitionNumber = [int]$pt.PartitionNumber
      DriveLetter = $letter
      Offset = [uint64]$pt.Offset
      Size = [uint64]$pt.Size
      Type = [string]$pt.Type
      IsBoot = [bool]$pt.IsBoot
      IsSystem = [bool]$pt.IsSystem
      AccessPaths = @($pt.AccessPaths | Where-Object { $_ } | ForEach-Object { [string]$_ })
      FileSystem = if ($v) { [string]$v.FileSystem } else { $null }
      Label = if ($v) { [string]$v.FileSystemLabel } else { $null }
      VolumeSize = if ($v) { [uint64]$v.Size } else { $null }
      VolumeFree = if ($v) { [uint64]$v.SizeRemaining } else { $null }
    }
  })
  [pscustomobject]@{
    Number = [int]$d.Number
    FriendlyName = [string]$d.FriendlyName
    Model = [string]$d.Model
    SerialNumber = [string]$d.SerialNumber
    Size = [uint64]$d.Size
    BusType = [string]$d.BusType
    PartitionStyle = [string]$d.PartitionStyle
    IsBoot = [bool]$d.IsBoot
    IsSystem = [bool]$d.IsSystem
    IsOffline = [bool]$d.IsOffline
    IsReadOnly = [bool]$d.IsReadOnly
    LogicalSectorSize = [int]$d.LogicalSectorSize
    PhysicalSectorSize = [int]$d.PhysicalSectorSize
    HealthStatus = [string]$d.HealthStatus
    OperationalStatus = [string](@($d.OperationalStatus) -join ', ')
    Location = [string]$d.Location
    MediaType = if ($p) { [string]$p.MediaType } else { $null }
    SpindleSpeed = if ($p -and $null -ne $p.SpindleSpeed) { [uint32]$p.SpindleSpeed } else { $null }
    FirmwareVersion = if ($p) { [string]$p.FirmwareVersion } else { $null }
    DriveMediaType = if ($w) { [string]$w.MediaType } else { $null }
    Partitions = $parts
  }
}
ConvertTo-Json -InputObject @($out) -Depth 6 -Compress
"#;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawPartition {
    partition_number: u32,
    drive_letter: Option<String>,
    #[serde(default)]
    offset: u64,
    #[serde(default)]
    size: u64,
    #[serde(rename = "Type", default)]
    kind: Option<String>,
    #[serde(default)]
    is_boot: bool,
    #[serde(default)]
    is_system: bool,
    #[serde(default)]
    access_paths: Option<Vec<String>>,
    file_system: Option<String>,
    label: Option<String>,
    volume_size: Option<u64>,
    volume_free: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawDisk {
    number: u32,
    friendly_name: Option<String>,
    model: Option<String>,
    serial_number: Option<String>,
    #[serde(default)]
    size: u64,
    bus_type: Option<String>,
    partition_style: Option<String>,
    #[serde(default)]
    is_boot: bool,
    #[serde(default)]
    is_system: bool,
    #[serde(default)]
    is_offline: bool,
    #[serde(default)]
    is_read_only: bool,
    #[serde(default)]
    logical_sector_size: u32,
    #[serde(default)]
    physical_sector_size: u32,
    health_status: Option<String>,
    operational_status: Option<String>,
    location: Option<String>,
    media_type: Option<String>,
    spindle_speed: Option<u32>,
    firmware_version: Option<String>,
    /// `Win32_DiskDrive.MediaType`: "Fixed hard disk media", "External hard disk media", "Removable Media".
    #[serde(default)]
    drive_media_type: Option<String>,
    #[serde(default)]
    partitions: Option<Vec<RawPartition>>,
}

fn non_empty(s: Option<String>) -> Option<String> {
    s.map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

/// Converts the inventory script JSON into [`DiskInfo`] (without resolving volume devices).
pub fn parse_inventory_json(json: &str) -> Result<Vec<DiskInfo>> {
    let json = json.trim();
    if json.is_empty() {
        return Ok(vec![]);
    }
    let raw: Vec<RawDisk> = serde_json::from_str(json).context(t("invalid inventory JSON", "JSON de inventário inválido"))?;
    let mut disks: Vec<DiskInfo> = raw
        .into_iter()
        .map(|d| {
            let model = non_empty(d.model)
                .or(non_empty(d.friendly_name))
                .unwrap_or_else(|| t("(unknown model)", "(modelo desconhecido)").into());
            let serial = d.serial_number.unwrap_or_default();
            let bus_raw = d.bus_type.unwrap_or_default();
            let bus = BusKind::parse(&bus_raw);
            let spindle_rpm = d.spindle_speed.filter(|&s| s != u32::MAX);
            let media = infer_media(d.media_type.as_deref(), bus, spindle_rpm, &model);
            let kind = infer_device_kind(bus, d.drive_media_type.as_deref(), &model);
            let fp_source = if serial.trim().is_empty() {
                format!("{model}|{}", d.size)
            } else {
                format!("{model}|{}", serial.trim())
            };
            let partitions = d
                .partitions
                .unwrap_or_default()
                .into_iter()
                .map(|p| PartitionInfo {
                    number: p.partition_number,
                    drive_letter: non_empty(p.drive_letter)
                        .filter(|l| l.chars().all(|c| c.is_ascii_alphabetic())),
                    offset: p.offset,
                    size: p.size,
                    kind: p.kind.unwrap_or_default(),
                    is_boot: p.is_boot,
                    is_system: p.is_system,
                    file_system: non_empty(p.file_system),
                    label: non_empty(p.label),
                    volume_size: p.volume_size,
                    volume_free: p.volume_free,
                    volume_paths: p
                        .access_paths
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|a| a.starts_with(r"\\?\Volume"))
                        .collect(),
                    device_path: None,
                })
                .collect();
            DiskInfo {
                number: d.number,
                serial_masked: mask_serial(&serial),
                fingerprint: fingerprint(&fp_source),
                model,
                size_bytes: d.size,
                bus,
                bus_raw,
                media,
                kind,
                firmware: non_empty(d.firmware_version),
                partition_style: d.partition_style.unwrap_or_default(),
                is_boot: d.is_boot,
                is_system: d.is_system,
                is_offline: d.is_offline,
                is_read_only: d.is_read_only,
                logical_sector_size: d.logical_sector_size,
                physical_sector_size: d.physical_sector_size,
                windows_health: d.health_status.unwrap_or_default(),
                operational_status: d.operational_status.unwrap_or_default(),
                location: non_empty(d.location),
                spindle_rpm,
                partitions,
                tags: Vec::new(),
            }
        })
        .collect();
    disks.sort_by_key(|d| d.number);
    Ok(disks)
}

/// Collects the real inventory (Windows).
pub fn load_inventory() -> Result<Vec<DiskInfo>> {
    let json = platform::powershell::run_script(INVENTORY_SCRIPT, Duration::from_secs(90))
        .context(t("failed to list drives through PowerShell (Get-Disk)", "falha ao listar discos via PowerShell (Get-Disk)"))?;
    let mut disks = parse_inventory_json(&json)?;
    for disk in &mut disks {
        for p in &mut disk.partitions {
            p.device_path = p
                .volume_paths
                .iter()
                .find_map(|vp| platform::resolve_volume_device(vp));
        }
    }
    Ok(disks)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Real, anonymized inventory (3 SATA SSDs), also used by other tests.
    pub const FIXTURE: &str = include_str!("../tests/fixtures/inventory.json");

    pub fn fixture_disks() -> Vec<DiskInfo> {
        let mut disks = parse_inventory_json(FIXTURE).unwrap();
        // Simulates the resolution done by QueryDosDevice.
        for d in &mut disks {
            for p in &mut d.partitions {
                if p.volume_paths.iter().any(|v| v.contains("000000000104")) {
                    p.device_path = Some(r"\Device\HarddiskVolume5".into());
                }
            }
        }
        disks
    }

    #[test]
    fn parses_fixture_inventory() {
        let _en = crate::i18n::scoped(crate::i18n::Lang::En);
        let disks = fixture_disks();
        assert_eq!(disks.len(), 3);
        assert_eq!(
            disks.iter().map(|d| d.number).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        let d2 = find_disk(&disks, 2).unwrap();
        assert_eq!(d2.model, "KINGSTON SA400S37120G");
        assert_eq!(d2.bus, BusKind::Sata);
        assert_eq!(d2.media, MediaKind::Ssd);
        assert_eq!(d2.class_label(), "SATA SSD");
        assert!(d2.is_boot);
        assert_eq!(d2.volume_letters(), vec!["C:"]);
        assert!(!d2.serial_masked.is_empty());
        assert!(d2.serial_masked.contains('*'));
    }

    #[test]
    fn accepts_numeric_enums() {
        let _en = crate::i18n::scoped(crate::i18n::Lang::En);
        let json = r#"[{"Number":3,"Model":"X NVMe","SerialNumber":"ABCDEFGHIJ","Size":1000,"BusType":"17","MediaType":"0","Partitions":[]}]"#;
        let disks = parse_inventory_json(json).unwrap();
        assert_eq!(disks[0].bus, BusKind::Nvme);
        assert_eq!(disks[0].media, MediaKind::Ssd);
        assert_eq!(disks[0].class_label(), "NVMe SSD");
    }

    #[test]
    fn classifies_external_flash_and_cards() {
        let json = r#"[
          {"Number":3,"Model":"WD Elements 25A2","SerialNumber":"WX11A1","Size":2000398934016,"BusType":"USB","MediaType":"Unspecified","DriveMediaType":"External hard disk media","Partitions":[]},
          {"Number":4,"Model":"SanDisk Cruzer Blade","SerialNumber":"4C5300","Size":32015679488,"BusType":"7","DriveMediaType":"Removable Media","Partitions":[]},
          {"Number":5,"Model":"Generic- SD/MMC","SerialNumber":"000000","Size":63864569856,"BusType":"USB","DriveMediaType":"Removable Media","Partitions":[]},
          {"Number":6,"Model":"SDXC Card","SerialNumber":"","Size":127865454592,"BusType":"12","Partitions":[]},
          {"Number":7,"Model":"Samsung PSSD T7","SerialNumber":"S5T1","Size":1000204886016,"BusType":"USB","MediaType":"SSD","DriveMediaType":"External hard disk media","Partitions":[]}
        ]"#;
        let disks = parse_inventory_json(json).unwrap();
        let label = |n: u32| find_disk(&disks, n).unwrap().class_label();
        {
            let _en = crate::i18n::scoped(crate::i18n::Lang::En);
            assert_eq!(label(3), "External USB drive");
            assert_eq!(label(4), "USB flash drive");
            assert_eq!(label(5), "Memory card");
            assert_eq!(label(6), "Memory card");
            assert_eq!(label(7), "External USB SSD");
            assert_eq!(find_disk(&fixture_disks(), 2).unwrap().class_label(), "SATA SSD");
        }
        {
            let _pt = crate::i18n::scoped(crate::i18n::Lang::PtBr);
            assert_eq!(label(3), "Disco externo USB");
            assert_eq!(label(4), "Pendrive USB");
            assert_eq!(label(5), "Cartão de memória");
            assert_eq!(label(7), "SSD externo USB");
            assert_eq!(find_disk(&fixture_disks(), 2).unwrap().class_label(), "SSD SATA");
        }
        assert!(find_disk(&disks, 4).unwrap().is_flash_media());
        assert!(!find_disk(&disks, 7).unwrap().is_flash_media());
        assert!(find_disk(&disks, 7).unwrap().is_external());
        // Internal drives stay as before (the real fixture has no DriveMediaType).
        assert!(
            fixture_disks()
                .iter()
                .all(|d| d.kind == DeviceKind::Internal)
        );
        // "SSD" must not be mistaken for an "SD" card.
        assert_eq!(
            infer_device_kind(
                BusKind::Usb,
                Some("External hard disk media"),
                "Crucial X6 SSD"
            ),
            DeviceKind::ExternalDrive
        );
    }

    #[test]
    fn infers_hdd_from_spindle() {
        assert_eq!(
            infer_media(
                Some("Unspecified"),
                BusKind::Sata,
                Some(7200),
                "ST2000DM008"
            ),
            MediaKind::Hdd
        );
        assert_eq!(
            infer_media(None, BusKind::Sata, Some(u32::MAX), "ST2000DM008"),
            MediaKind::Unknown
        );
        assert_eq!(
            infer_media(Some("3"), BusKind::Sata, None, "x"),
            MediaKind::Hdd
        );
    }

    #[test]
    fn maps_volumes_and_letters_to_disks() {
        let disks = fixture_disks();
        assert_eq!(disk_for_letter(&disks, "C:"), Some(2));
        assert_eq!(disk_for_letter(&disks, "e"), Some(0));
        assert_eq!(disk_for_letter(&disks, "Z:"), None);
        assert_eq!(
            disk_for_volume_reference(
                &disks,
                r"Volume \\?\Volume{00000000-0000-4000-8000-000000000104} needs repair"
            ),
            Some(1)
        );
        assert_eq!(
            disk_for_volume_reference(&disks, r"(\Device\HarddiskVolume5)"),
            Some(1)
        );
        assert_eq!(
            disk_for_volume_reference(&disks, r"(\Device\HarddiskVolume55)"),
            None
        );
        assert_eq!(disk_for_volume_reference(&disks, "nothing here"), None);
    }

    #[test]
    fn empty_output_means_no_disks() {
        assert!(parse_inventory_json("  ").unwrap().is_empty());
        assert!(parse_inventory_json("[]").unwrap().is_empty());
    }
}
