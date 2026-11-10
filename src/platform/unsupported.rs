//! Stubs for operating systems other than Windows: let the logic build and be tested.

use super::OpenError;
use crate::io::{BlockReader, BlockWriter, IoOutcome, VerifyFile, VerifyTarget};
use std::path::Path;
use std::time::Duration;

const WINDOWS_ONLY: &str = "Windows only";

pub struct CapacityDir;

impl CapacityDir {
    pub fn create(_parent: &Path, _max_io: u32) -> std::io::Result<Self> {
        Err(std::io::Error::new(std::io::ErrorKind::Unsupported, WINDOWS_ONLY))
    }
    pub fn dir(&self) -> &Path {
        Path::new("")
    }
    pub fn cleanup(&mut self) -> Vec<String> {
        vec![]
    }
}

impl VerifyTarget for CapacityDir {
    fn create_file(&mut self, _index: u32) -> Result<Box<dyn VerifyFile>, u32> {
        Err(50)
    }
    fn open_file(&mut self, _index: u32) -> Result<Box<dyn VerifyFile>, u32> {
        Err(50)
    }
}

pub fn enable_virtual_terminal() {}

pub fn is_elevated() -> bool {
    false
}

pub fn win32_error_message(code: u32) -> String {
    format!("error {code}")
}

pub fn resolve_volume_device(_volume_path: &str) -> Option<String> {
    None
}

pub fn free_space_bytes(_dir: &Path) -> Option<u64> {
    None
}

pub struct RawDisk;

impl RawDisk {
    pub fn open(_disk_number: u32, _max_io: u32) -> Result<Self, OpenError> {
        Err(OpenError::Unsupported)
    }
}

impl BlockReader for RawDisk {
    fn capacity(&self) -> u64 {
        0
    }
    fn logical_sector_size(&self) -> u32 {
        512
    }
    fn physical_sector_size(&self) -> u32 {
        512
    }
    fn max_io_size(&self) -> u32 {
        0
    }
    fn read_at(&mut self, _offset: u64, _len: u32, _timeout: Duration) -> IoOutcome {
        IoOutcome::Failed { code: 50, latency: Duration::ZERO }
    }
}

pub struct TempBenchFile;

impl TempBenchFile {
    pub fn create(_dir: &Path, _size: u64, _max_io: u32) -> std::io::Result<Self> {
        Err(std::io::Error::new(std::io::ErrorKind::Unsupported, WINDOWS_ONLY))
    }
    pub fn path(&self) -> &Path {
        Path::new("")
    }
}

impl BlockWriter for TempBenchFile {
    fn capacity(&self) -> u64 {
        0
    }
    fn max_io_size(&self) -> u32 {
        0
    }
    fn write_at(&mut self, _offset: u64, _len: u32, _timeout: Duration) -> IoOutcome {
        IoOutcome::Failed { code: 50, latency: Duration::ZERO }
    }
}

pub fn relaunch_as_admin(_args: &str) -> Result<(), String> {
    Err(WINDOWS_ONLY.into())
}

pub fn open_path(_path: &Path) -> Result<(), String> {
    Err(WINDOWS_ONLY.into())
}
