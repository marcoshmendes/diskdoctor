//! Block I/O abstractions used by the Surface Scan (`scan`), the Benchmark and the
//! Capacity Verification.
//!
//! The test engines depend only on these traits, so they are tested with simulated
//! devices ([`testing::ScriptedReader`]) without touching a real drive. The real
//! implementations live in `platform::win` (`RawDisk`, `TempBenchFile`, `CapacityDir`).
//!
//! Safety invariant: the physical drive is only ever exposed as a [`BlockReader`].
//! There is no [`BlockWriter`] for a physical drive — writes only happen to a new
//! temporary file (`TempBenchFile`).

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Result of a block I/O operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IoOutcome {
    /// Completed. `bytes` may be less than requested (short read).
    Completed { bytes: u32, latency: Duration },
    /// Failed with a Win32 error code.
    Failed { code: u32, latency: Duration },
    /// Exceeded the configured timeout and was cancelled successfully.
    TimedOut { latency: Duration },
    /// Exceeded the timeout and not even the cancel completed: the device (or the
    /// controller) stopped responding. The reader is unusable afterwards.
    Hung { latency: Duration },
}

impl IoOutcome {
    pub fn latency(&self) -> Duration {
        match self {
            Self::Completed { latency, .. }
            | Self::Failed { latency, .. }
            | Self::TimedOut { latency }
            | Self::Hung { latency } => *latency,
        }
    }
}

/// Device that only allows reads (physical drive).
pub trait BlockReader {
    /// Total capacity in bytes.
    fn capacity(&self) -> u64;
    /// Logical sector size (minimum alignment of offset and length).
    fn logical_sector_size(&self) -> u32;
    /// Physical sector size (granularity used to isolate bad sectors).
    fn physical_sector_size(&self) -> u32;
    /// Largest read supported per call.
    fn max_io_size(&self) -> u32;
    /// Reads `len` bytes from `offset` (both aligned to the logical sector).
    fn read_at(&mut self, offset: u64, len: u32, timeout: Duration) -> IoOutcome;
}

/// Benchmark write target (only a new temporary file).
pub trait BlockWriter {
    /// Size of the test area in bytes.
    fn capacity(&self) -> u64;
    fn max_io_size(&self) -> u32;
    fn write_at(&mut self, offset: u64, len: u32, timeout: Duration) -> IoOutcome;
}

/// Capacity Verification file: unlike [`BlockWriter`], the **content** matters, because
/// every block is written with a pattern and checked on read-back.
pub trait VerifyFile {
    /// Writes `data` (sector-aligned length) at `offset` inside the file.
    fn write_block(&mut self, offset: u64, data: &[u8], timeout: Duration) -> IoOutcome;
    /// Reads `out.len()` bytes from `offset` inside the file, bypassing the system cache.
    fn read_block(&mut self, offset: u64, out: &mut [u8], timeout: Duration) -> IoOutcome;
}

/// **New** folder where the Capacity Verification creates its files. It only creates new
/// files and only reopens the ones it created itself (`index` = creation order).
pub trait VerifyTarget {
    /// Creates file `index` (fails if it already exists). `Err` = Win32 error code.
    fn create_file(&mut self, index: u32) -> Result<Box<dyn VerifyFile>, u32>;
    /// Reopens, read-only, a file created earlier by [`VerifyTarget::create_file`].
    fn open_file(&mut self, index: u32) -> Result<Box<dyn VerifyFile>, u32>;
}

/// Win32 error codes relevant to drive I/O.
pub mod win32 {
    pub const ERROR_FILE_NOT_FOUND: u32 = 2;
    pub const ERROR_PATH_NOT_FOUND: u32 = 3;
    pub const ERROR_ACCESS_DENIED: u32 = 5;
    pub const ERROR_INVALID_HANDLE: u32 = 6;
    pub const ERROR_NOT_READY: u32 = 21;
    pub const ERROR_CRC: u32 = 23;
    pub const ERROR_SECTOR_NOT_FOUND: u32 = 27;
    pub const ERROR_WRITE_FAULT: u32 = 29;
    pub const ERROR_READ_FAULT: u32 = 30;
    pub const ERROR_GEN_FAILURE: u32 = 31;
    pub const ERROR_SHARING_VIOLATION: u32 = 32;
    pub const ERROR_HANDLE_EOF: u32 = 38;
    pub const ERROR_HANDLE_DISK_FULL: u32 = 39;
    pub const ERROR_DEV_NOT_EXIST: u32 = 55;
    pub const ERROR_FILE_EXISTS: u32 = 80;
    pub const ERROR_INVALID_PARAMETER: u32 = 87;
    pub const ERROR_DISK_FULL: u32 = 112;
    pub const ERROR_SEM_TIMEOUT: u32 = 121;
    pub const ERROR_NO_SUCH_DEVICE: u32 = 433;
    pub const ERROR_DEVICE_HARDWARE_ERROR: u32 = 483;
    pub const ERROR_OPERATION_ABORTED: u32 = 995;
    pub const ERROR_IO_DEVICE: u32 = 1117;
    pub const ERROR_DEVICE_NOT_CONNECTED: u32 = 1167;
    pub const ERROR_TIMEOUT: u32 = 1460;
    pub const ERROR_DEVICE_REMOVED: u32 = 1617;
}

/// Class of an I/O error, used by the Surface Scan engine and by the analysis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorClass {
    /// The drive reported unreadable data (bad sector).
    Media,
    /// Generic device/communication error.
    Device,
    /// Timeout in the driver.
    Timeout,
    /// Error that prevents going on (device gone, access denied, invalid parameter).
    Fatal,
    Other,
}

pub fn classify_error(code: u32) -> ErrorClass {
    use win32::*;
    match code {
        ERROR_CRC | ERROR_SECTOR_NOT_FOUND | ERROR_READ_FAULT | ERROR_WRITE_FAULT => ErrorClass::Media,
        ERROR_IO_DEVICE | ERROR_DEVICE_HARDWARE_ERROR | ERROR_GEN_FAILURE => ErrorClass::Device,
        ERROR_SEM_TIMEOUT | ERROR_TIMEOUT => ErrorClass::Timeout,
        ERROR_FILE_NOT_FOUND
        | ERROR_PATH_NOT_FOUND
        | ERROR_ACCESS_DENIED
        | ERROR_INVALID_HANDLE
        | ERROR_NOT_READY
        | ERROR_DEV_NOT_EXIST
        | ERROR_INVALID_PARAMETER
        | ERROR_NO_SUCH_DEVICE
        | ERROR_DEVICE_NOT_CONNECTED
        | ERROR_DEVICE_REMOVED
        | ERROR_DISK_FULL
        | ERROR_HANDLE_DISK_FULL => ErrorClass::Fatal,
        _ => ErrorClass::Other,
    }
}

/// Short, localized description of a Win32 error code.
pub fn describe_error(code: u32) -> String {
    use crate::i18n::t;
    use win32::*;
    let text = match code {
        ERROR_CRC => t("data error (CRC): unreadable sector", "erro de dados (CRC): setor ilegível"),
        ERROR_SECTOR_NOT_FOUND => t("sector not found", "setor não encontrado"),
        ERROR_READ_FAULT => t("device read fault", "falha de leitura no dispositivo"),
        ERROR_WRITE_FAULT => t("device write fault", "falha de gravação no dispositivo"),
        ERROR_IO_DEVICE => t("device I/O error (communication or hardware)", "erro de E/S do dispositivo (comunicação ou hardware)"),
        ERROR_DEVICE_HARDWARE_ERROR => t("hardware error reported by the device", "erro de hardware reportado pelo dispositivo"),
        ERROR_GEN_FAILURE => t("the device is not working properly", "o dispositivo não está funcionando corretamente"),
        ERROR_SEM_TIMEOUT | ERROR_TIMEOUT => t("timeout in the driver", "tempo limite esgotado no driver"),
        ERROR_NOT_READY => t("device not ready", "dispositivo não está pronto"),
        ERROR_DEVICE_NOT_CONNECTED | ERROR_DEV_NOT_EXIST | ERROR_NO_SUCH_DEVICE | ERROR_DEVICE_REMOVED => {
            t("device disconnected or removed", "dispositivo desconectado ou removido")
        }
        ERROR_ACCESS_DENIED => t("access denied", "acesso negado"),
        ERROR_INVALID_PARAMETER => t("invalid parameter (I/O alignment)", "parâmetro inválido (alinhamento de E/S)"),
        ERROR_OPERATION_ABORTED => t("operation cancelled", "operação cancelada"),
        ERROR_HANDLE_EOF => t("short read (end of device)", "leitura curta (fim do dispositivo)"),
        ERROR_DISK_FULL | ERROR_HANDLE_DISK_FULL => t("disk full", "disco cheio"),
        _ => return tr!("Windows error {code}", "erro do Windows {code}"),
    };
    tr!("{text} (code {code})", "{text} (código {code})")
}

/// Simulated devices for automated tests (not used in production).
#[doc(hidden)]
pub mod testing {
    use super::*;

    const MIB: u64 = 1 << 20;

    fn overlaps(offset: u64, len: u32, start: u64, end: u64) -> bool {
        offset < end && start < offset + len as u64
    }

    /// Reader whose behavior is scripted by byte ranges.
    #[derive(Debug, Clone)]
    pub struct ScriptedReader {
        pub capacity: u64,
        pub logical: u32,
        pub physical: u32,
        pub max_io: u32,
        pub latency_per_mib: Duration,
        /// `[start, end)` ranges that are always unreadable (ERROR_CRC).
        pub bad_ranges: Vec<(u64, u64)>,
        /// Ranges that fail (ERROR_IO_DEVICE) only on the first read that touches them.
        pub transient_bad: Vec<(u64, u64)>,
        /// Slow ranges: extra latency added.
        pub slow_ranges: Vec<(u64, u64, Duration)>,
        /// Ranges that exceed the timeout.
        pub timeout_ranges: Vec<(u64, u64)>,
        /// Offset from which the device hangs.
        pub hang_at: Option<u64>,
        /// Offset and code of a fatal error.
        pub fatal_at: Option<(u64, u32)>,
        /// Log of every read made (offset, length).
        pub reads: Vec<(u64, u32)>,
        transient_hit: Vec<bool>,
    }

    impl ScriptedReader {
        pub fn new(capacity: u64) -> Self {
            Self {
                capacity,
                logical: 512,
                physical: 4096,
                max_io: 4 * MIB as u32,
                latency_per_mib: Duration::from_millis(2),
                bad_ranges: vec![],
                transient_bad: vec![],
                slow_ranges: vec![],
                timeout_ranges: vec![],
                hang_at: None,
                fatal_at: None,
                reads: vec![],
                transient_hit: vec![],
            }
        }
    }

    impl BlockReader for ScriptedReader {
        fn capacity(&self) -> u64 {
            self.capacity
        }
        fn logical_sector_size(&self) -> u32 {
            self.logical
        }
        fn physical_sector_size(&self) -> u32 {
            self.physical
        }
        fn max_io_size(&self) -> u32 {
            self.max_io
        }
        fn read_at(&mut self, offset: u64, len: u32, timeout: Duration) -> IoOutcome {
            self.reads.push((offset, len));
            assert_eq!(offset % self.logical as u64, 0, "misaligned offset");
            assert_eq!(len % self.logical, 0, "misaligned length");
            assert!(offset + len as u64 <= self.capacity, "read past the end");
            if let Some(h) = self.hang_at {
                if overlaps(offset, len, h, h + 1) {
                    return IoOutcome::Hung { latency: timeout * 2 };
                }
            }
            if let Some((p, code)) = self.fatal_at {
                if overlaps(offset, len, p, p + 1) {
                    return IoOutcome::Failed { code, latency: Duration::from_millis(1) };
                }
            }
            if self.timeout_ranges.iter().any(|&(s, e)| overlaps(offset, len, s, e)) {
                return IoOutcome::TimedOut { latency: timeout };
            }
            if self.bad_ranges.iter().any(|&(s, e)| overlaps(offset, len, s, e)) {
                return IoOutcome::Failed { code: win32::ERROR_CRC, latency: Duration::from_millis(40) };
            }
            if self.transient_hit.len() < self.transient_bad.len() {
                self.transient_hit.resize(self.transient_bad.len(), false);
            }
            for (i, &(s, e)) in self.transient_bad.iter().enumerate() {
                if overlaps(offset, len, s, e) && !self.transient_hit[i] {
                    self.transient_hit[i] = true;
                    return IoOutcome::Failed { code: win32::ERROR_IO_DEVICE, latency: Duration::from_millis(30) };
                }
            }
            let mut latency = self.latency_per_mib.mul_f64(len as f64 / MIB as f64);
            for &(s, e, extra) in &self.slow_ranges {
                if overlaps(offset, len, s, e) {
                    latency += extra;
                }
            }
            if latency > timeout {
                return IoOutcome::TimedOut { latency: timeout };
            }
            IoOutcome::Completed { bytes: len, latency }
        }
    }

    /// Simulated writer for the write benchmark.
    #[derive(Debug, Clone)]
    pub struct ScriptedWriter {
        pub capacity: u64,
        pub max_io: u32,
        pub latency_per_mib: Duration,
        pub fail_at: Option<(u64, u32)>,
        pub writes: Vec<(u64, u32)>,
    }

    impl ScriptedWriter {
        pub fn new(capacity: u64) -> Self {
            Self { capacity, max_io: 4 * MIB as u32, latency_per_mib: Duration::from_millis(4), fail_at: None, writes: vec![] }
        }
    }

    impl BlockWriter for ScriptedWriter {
        fn capacity(&self) -> u64 {
            self.capacity
        }
        fn max_io_size(&self) -> u32 {
            self.max_io
        }
        fn write_at(&mut self, offset: u64, len: u32, _timeout: Duration) -> IoOutcome {
            self.writes.push((offset, len));
            assert!(offset + len as u64 <= self.capacity, "write past the temporary file");
            if let Some((p, code)) = self.fail_at {
                if overlaps(offset, len, p, p + 1) {
                    return IoOutcome::Failed { code, latency: Duration::from_millis(1) };
                }
            }
            let latency = self.latency_per_mib.mul_f64(len as f64 / MIB as f64).max(Duration::from_micros(50));
            IoOutcome::Completed { bytes: len, latency }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_common_errors() {
        assert_eq!(classify_error(23), ErrorClass::Media);
        assert_eq!(classify_error(1117), ErrorClass::Device);
        assert_eq!(classify_error(121), ErrorClass::Timeout);
        assert_eq!(classify_error(1167), ErrorClass::Fatal);
        assert_eq!(classify_error(5), ErrorClass::Fatal);
        assert_eq!(classify_error(9999), ErrorClass::Other);
    }

    #[test]
    fn describes_errors_in_both_languages() {
        {
            let _en = crate::i18n::scoped(crate::i18n::Lang::En);
            assert_eq!(describe_error(23), "data error (CRC): unreadable sector (code 23)");
            assert_eq!(describe_error(4242), "Windows error 4242");
        }
        let _pt = crate::i18n::scoped(crate::i18n::Lang::PtBr);
        assert!(describe_error(23).contains("setor ilegível"));
        assert_eq!(describe_error(4242), "erro do Windows 4242");
    }
}
