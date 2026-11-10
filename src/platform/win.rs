//! Native Windows access (Win32). Every `unsafe` block of the project lives here.
//!
//! Key decisions (see docs/ARCHITECTURE.md):
//! - The physical drive is opened with `GENERIC_READ` only: Windows itself rejects any
//!   write through that handle. No code path writes to the drive.
//! - `FILE_FLAG_NO_BUFFERING` bypasses the system cache, so the measured latency is the
//!   device's. It requires sector-aligned buffers, offsets and sizes; buffers are
//!   aligned to 4096 bytes.
//! - Overlapped I/O (`FILE_FLAG_OVERLAPPED`) allows a timeout per operation and a real
//!   cancel with `CancelIoEx`, which synchronous reads cannot do.
//! - If not even the cancel completes (hung device), the handle is marked "poisoned" and
//!   the buffer/OVERLAPPED are leaked on purpose: the kernel may still write to them, and
//!   freeing that memory would be undefined behavior.

use super::OpenError;
use crate::io::{BlockReader, BlockWriter, IoOutcome, VerifyFile, VerifyTarget};
use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::ffi::c_void;
use std::mem::{ManuallyDrop, size_of};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr::{NonNull, null, null_mut};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_IO_PENDING, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE, INVALID_HANDLE_VALUE, WAIT_TIMEOUT,
};
use windows_sys::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows_sys::Win32::Storage::FileSystem::{
    CREATE_NEW, CreateFileW, FILE_FLAG_DELETE_ON_CLOSE, FILE_FLAG_NO_BUFFERING, FILE_FLAG_OVERLAPPED, FILE_FLAG_WRITE_THROUGH,
    FILE_SHARE_READ, FILE_SHARE_WRITE, GetDiskFreeSpaceExW, OPEN_EXISTING, QueryDosDeviceW, ReadFile, WriteFile,
};
use windows_sys::Win32::System::Diagnostics::Debug::{FORMAT_MESSAGE_FROM_SYSTEM, FORMAT_MESSAGE_IGNORE_INSERTS, FormatMessageW};
use windows_sys::Win32::System::IO::{CancelIoEx, DeviceIoControl, GetOverlappedResultEx, OVERLAPPED, OVERLAPPED_0, OVERLAPPED_0_0};
use windows_sys::Win32::System::Ioctl::{
    DISK_GEOMETRY_EX, GET_LENGTH_INFORMATION, IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, IOCTL_DISK_GET_LENGTH_INFO,
    IOCTL_STORAGE_QUERY_PROPERTY, PropertyStandardQuery, STORAGE_ACCESS_ALIGNMENT_DESCRIPTOR, STORAGE_PROPERTY_QUERY,
    StorageAccessAlignmentProperty,
};
use windows_sys::Win32::System::Threading::{CreateEventW, GetCurrentProcess, OpenProcessToken};

/// How long to wait for an I/O cancel before declaring the device hung.
const HUNG_WAIT_MS: u32 = 120_000;
const IOCTL_TIMEOUT_MS: u32 = 15_000;
const BUFFER_ALIGN: usize = 4096;
const MIN_BUFFER: usize = 64 * 1024;

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn last_error() -> u32 {
    unsafe { GetLastError() }
}

/// Turns on VT sequence processing on the console (cursor movement used by the menus).
/// Windows 10+ supports it; the `console` crate only enables it when colors are on, so
/// with `NO_COLOR` or `--no-color` the menus would print raw escape codes otherwise.
pub fn enable_virtual_terminal() {
    use windows_sys::Win32::System::Console::{ENABLE_VIRTUAL_TERMINAL_PROCESSING, GetConsoleMode, GetStdHandle, STD_OUTPUT_HANDLE, SetConsoleMode};
    unsafe {
        let handle = GetStdHandle(STD_OUTPUT_HANDLE);
        let mut mode = 0;
        if !handle.is_null() && handle != INVALID_HANDLE_VALUE && GetConsoleMode(handle, &mut mode) != 0 {
            SetConsoleMode(handle, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
        }
    }
}

/// Is the current process elevated (Administrator)?
pub fn is_elevated() -> bool {
    unsafe {
        let mut token: HANDLE = null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut returned = 0u32;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            &mut elevation as *mut TOKEN_ELEVATION as *mut c_void,
            size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        );
        CloseHandle(token);
        ok != 0 && elevation.TokenIsElevated != 0
    }
}

/// System message (in the Windows display language) for a Win32 code.
pub fn win32_error_message(code: u32) -> String {
    let mut buf = [0u16; 512];
    let n = unsafe {
        FormatMessageW(
            FORMAT_MESSAGE_FROM_SYSTEM | FORMAT_MESSAGE_IGNORE_INSERTS,
            null(),
            code,
            0,
            buf.as_mut_ptr(),
            buf.len() as u32,
            null(),
        )
    };
    if n == 0 {
        return tr!("error {code}", "erro {code}");
    }
    String::from_utf16_lossy(&buf[..n as usize]).trim().to_string()
}

/// `\\?\Volume{guid}\` → `\Device\HarddiskVolumeN` (links NTFS events to drives).
pub fn resolve_volume_device(volume_path: &str) -> Option<String> {
    let name = volume_path.trim_start_matches(r"\\?\").trim_end_matches('\\');
    if !name.to_ascii_lowercase().starts_with("volume{") {
        return None;
    }
    let w = wide(name);
    let mut buf = vec![0u16; 1024];
    let n = unsafe { QueryDosDeviceW(w.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) };
    if n == 0 {
        return None;
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(n as usize);
    Some(String::from_utf16_lossy(&buf[..end]))
}

/// Free space available to the user on the volume that contains `dir`.
pub fn free_space_bytes(dir: &Path) -> Option<u64> {
    let w: Vec<u16> = dir.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut free = 0u64;
    let ok = unsafe { GetDiskFreeSpaceExW(w.as_ptr(), &mut free, null_mut(), null_mut()) };
    (ok != 0).then_some(free)
}

/// Buffer aligned to 4096 bytes (required by `FILE_FLAG_NO_BUFFERING`).
struct AlignedBuffer {
    ptr: NonNull<u8>,
    layout: Layout,
}

impl AlignedBuffer {
    fn new(size: usize) -> Self {
        let size = size.max(MIN_BUFFER).next_multiple_of(BUFFER_ALIGN);
        let layout = Layout::from_size_align(size, BUFFER_ALIGN).expect("valid layout");
        let raw = unsafe { alloc_zeroed(layout) };
        let ptr = NonNull::new(raw).unwrap_or_else(|| std::alloc::handle_alloc_error(layout));
        Self { ptr, layout }
    }
    fn as_mut_ptr(&self) -> *mut u8 {
        self.ptr.as_ptr()
    }
    fn len(&self) -> usize {
        self.layout.size()
    }
    /// Fills with pseudo-random (incompressible) data for the write test.
    fn fill_pseudo_random(&mut self, seed: u64) {
        let slice = unsafe { std::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len()) };
        let mut rng = crate::util::SplitMix64::new(seed);
        for chunk in slice.chunks_mut(8) {
            let v = rng.next_u64().to_le_bytes();
            chunk.copy_from_slice(&v[..chunk.len()]);
        }
    }
}

impl Drop for AlignedBuffer {
    fn drop(&mut self) {
        unsafe { dealloc(self.ptr.as_ptr(), self.layout) }
    }
}

/// Handle opened with `FILE_FLAG_OVERLAPPED`, plus its own event and buffer.
struct OverlappedHandle {
    handle: HANDLE,
    event: HANDLE,
    overlapped: ManuallyDrop<Box<OVERLAPPED>>,
    buffer: ManuallyDrop<AlignedBuffer>,
    poisoned: bool,
}

impl OverlappedHandle {
    fn new(handle: HANDLE, buffer_size: usize) -> Result<Self, u32> {
        let event = unsafe { CreateEventW(null(), 1, 0, null()) };
        if event.is_null() {
            let err = last_error();
            unsafe { CloseHandle(handle) };
            return Err(err);
        }
        Ok(Self {
            handle,
            event,
            overlapped: ManuallyDrop::new(Box::new(unsafe { std::mem::zeroed() })),
            buffer: ManuallyDrop::new(AlignedBuffer::new(buffer_size)),
            poisoned: false,
        })
    }

    fn prepare(&mut self, offset: u64) -> *mut OVERLAPPED {
        **self.overlapped = OVERLAPPED {
            Internal: 0,
            InternalHigh: 0,
            Anonymous: OVERLAPPED_0 { Anonymous: OVERLAPPED_0_0 { Offset: offset as u32, OffsetHigh: (offset >> 32) as u32 } },
            hEvent: self.event,
        };
        &mut **self.overlapped as *mut OVERLAPPED
    }

    /// Waits for completion; on timeout, cancels and waits for the cancel.
    /// Returns `Ok(bytes)` or an already classified `Err(outcome)`.
    fn wait(&mut self, ov: *mut OVERLAPPED, timeout_ms: u32, start: Instant) -> Result<u32, IoOutcome> {
        let mut transferred = 0u32;
        let done = unsafe { GetOverlappedResultEx(self.handle, ov, &mut transferred, timeout_ms, 0) };
        if done != 0 {
            return Ok(transferred);
        }
        let err = last_error();
        if err != WAIT_TIMEOUT {
            return Err(IoOutcome::Failed { code: err, latency: start.elapsed() });
        }
        unsafe { CancelIoEx(self.handle, ov) };
        let done = unsafe { GetOverlappedResultEx(self.handle, ov, &mut transferred, HUNG_WAIT_MS, 0) };
        if done == 0 && last_error() == WAIT_TIMEOUT {
            self.poisoned = true;
            return Err(IoOutcome::Hung { latency: start.elapsed() });
        }
        Err(IoOutcome::TimedOut { latency: start.elapsed() })
    }

    fn transfer(&mut self, write: bool, offset: u64, len: u32, timeout: Duration) -> IoOutcome {
        if self.poisoned {
            return IoOutcome::Hung { latency: Duration::ZERO };
        }
        if len as usize > self.buffer.len() {
            return IoOutcome::Failed { code: crate::io::win32::ERROR_INVALID_PARAMETER, latency: Duration::ZERO };
        }
        let ov = self.prepare(offset);
        let buf = self.buffer.as_mut_ptr();
        let start = Instant::now();
        let ok = unsafe {
            if write { WriteFile(self.handle, buf, len, null_mut(), ov) } else { ReadFile(self.handle, buf, len, null_mut(), ov) }
        };
        if ok == 0 {
            let err = last_error();
            if err != ERROR_IO_PENDING {
                return IoOutcome::Failed { code: err, latency: start.elapsed() };
            }
        }
        let timeout_ms = timeout.as_millis().clamp(1, (u32::MAX - 1) as u128) as u32;
        match self.wait(ov, timeout_ms, start) {
            Ok(bytes) => IoOutcome::Completed { bytes, latency: start.elapsed() },
            Err(outcome) => outcome,
        }
    }

    /// IOCTL whose output goes to the handle's own buffer (which survives a hung device).
    fn ioctl(&mut self, code: u32, input: &[u8], out_len: usize) -> Result<&[u8], u32> {
        if self.poisoned {
            return Err(crate::io::win32::ERROR_NOT_READY);
        }
        let out_len = out_len.min(self.buffer.len());
        let ov = self.prepare(0);
        let mut returned = 0u32;
        let start = Instant::now();
        let ok = unsafe {
            DeviceIoControl(
                self.handle,
                code,
                if input.is_empty() { null() } else { input.as_ptr() as *const c_void },
                input.len() as u32,
                self.buffer.as_mut_ptr() as *mut c_void,
                out_len as u32,
                &mut returned,
                ov,
            )
        };
        if ok == 0 {
            let err = last_error();
            if err != ERROR_IO_PENDING {
                return Err(err);
            }
        }
        match self.wait(ov, IOCTL_TIMEOUT_MS, start) {
            Ok(n) => returned = n,
            Err(IoOutcome::Failed { code, .. }) => return Err(code),
            Err(_) => return Err(crate::io::win32::ERROR_SEM_TIMEOUT),
        }
        let n = (returned as usize).min(out_len);
        Ok(unsafe { std::slice::from_raw_parts(self.buffer.as_mut_ptr(), n) })
    }
}

impl Drop for OverlappedHandle {
    fn drop(&mut self) {
        if self.poisoned {
            // I/O may still be pending in the kernel: leaking is the only safe option.
            return;
        }
        unsafe {
            ManuallyDrop::drop(&mut self.buffer);
            ManuallyDrop::drop(&mut self.overlapped);
            CloseHandle(self.event);
            CloseHandle(self.handle);
        }
    }
}

fn read_struct<T: Copy>(bytes: &[u8]) -> Option<T> {
    (bytes.len() >= size_of::<T>()).then(|| unsafe { std::ptr::read_unaligned(bytes.as_ptr() as *const T) })
}

/// Physical drive (`\\.\PhysicalDriveN`) opened **read-only**.
pub struct RawDisk {
    io: OverlappedHandle,
    capacity: u64,
    logical: u32,
    physical: u32,
    max_io: u32,
}

impl RawDisk {
    pub fn open(disk_number: u32, max_io: u32) -> Result<Self, OpenError> {
        let path = wide(&format!(r"\\.\PhysicalDrive{disk_number}"));
        // GENERIC_READ only: Windows rejects any write made through this handle.
        let handle = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                null(),
                OPEN_EXISTING,
                FILE_FLAG_NO_BUFFERING | FILE_FLAG_OVERLAPPED,
                null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            let code = last_error();
            return Err(OpenError::from_code(code, win32_error_message(code)));
        }
        let max_io = (max_io.max(4096) as usize).next_multiple_of(BUFFER_ALIGN);
        let mut io = OverlappedHandle::new(handle, max_io).map_err(|c| OpenError::from_code(c, win32_error_message(c)))?;

        let capacity = io
            .ioctl(IOCTL_DISK_GET_LENGTH_INFO, &[], size_of::<GET_LENGTH_INFORMATION>())
            .ok()
            .and_then(read_struct::<GET_LENGTH_INFORMATION>)
            .map(|l| l.Length.max(0) as u64)
            .ok_or_else(|| OpenError::Other { code: 0, message: tr!("could not read the drive capacity", "não foi possível obter a capacidade do disco") })?;

        let logical = io
            .ioctl(IOCTL_DISK_GET_DRIVE_GEOMETRY_EX, &[], 1024)
            .ok()
            .and_then(read_struct::<DISK_GEOMETRY_EX>)
            .map(|g| g.Geometry.BytesPerSector)
            .filter(|&b| b >= 512 && b.is_power_of_two())
            .unwrap_or(512);

        let query = STORAGE_PROPERTY_QUERY {
            PropertyId: StorageAccessAlignmentProperty,
            QueryType: PropertyStandardQuery,
            AdditionalParameters: [0],
        };
        let query_bytes = unsafe {
            std::slice::from_raw_parts(&query as *const STORAGE_PROPERTY_QUERY as *const u8, size_of::<STORAGE_PROPERTY_QUERY>())
        }
        .to_vec();
        let physical = io
            .ioctl(IOCTL_STORAGE_QUERY_PROPERTY, &query_bytes, size_of::<STORAGE_ACCESS_ALIGNMENT_DESCRIPTOR>())
            .ok()
            .and_then(read_struct::<STORAGE_ACCESS_ALIGNMENT_DESCRIPTOR>)
            .map(|d| d.BytesPerPhysicalSector)
            .filter(|&b| b >= logical && b.is_power_of_two())
            .unwrap_or(logical);

        Ok(Self { io, capacity, logical, physical, max_io: max_io as u32 })
    }
}

impl BlockReader for RawDisk {
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
        self.io.transfer(false, offset, len, timeout)
    }
}

/// **New** temporary file for the write benchmark.
///
/// - `CREATE_NEW`: fails if the file already exists, so nothing is ever overwritten.
/// - `FILE_FLAG_DELETE_ON_CLOSE`: Windows deletes the file when the handle closes, even
///   if the process is killed.
/// - `FILE_FLAG_WRITE_THROUGH` + `NO_BUFFERING`: measures the device, not the cache.
pub struct TempBenchFile {
    io: OverlappedHandle,
    path: PathBuf,
    size: u64,
    max_io: u32,
}

impl TempBenchFile {
    pub fn create(dir: &Path, size: u64, max_io: u32) -> std::io::Result<Self> {
        let name = format!("diskdoctor-bench-{}-{}.tmp", std::process::id(), chrono::Utc::now().format("%Y%m%d%H%M%S"));
        let path = dir.join(name);
        let w: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        let handle = unsafe {
            CreateFileW(
                w.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                null(),
                CREATE_NEW,
                FILE_FLAG_NO_BUFFERING | FILE_FLAG_WRITE_THROUGH | FILE_FLAG_OVERLAPPED | FILE_FLAG_DELETE_ON_CLOSE,
                null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(std::io::Error::last_os_error());
        }
        let max_io = (max_io.max(4096) as usize).next_multiple_of(BUFFER_ALIGN);
        let mut io = OverlappedHandle::new(handle, max_io).map_err(|c| std::io::Error::from_raw_os_error(c as i32))?;
        io.buffer.fill_pseudo_random(0x5eed_d15c ^ size);
        Ok(Self { io, path, size, max_io: max_io as u32 })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl BlockWriter for TempBenchFile {
    fn capacity(&self) -> u64 {
        self.size
    }
    fn max_io_size(&self) -> u32 {
        self.max_io
    }
    fn write_at(&mut self, offset: u64, len: u32, timeout: Duration) -> IoOutcome {
        self.io.transfer(true, offset, len, timeout)
    }
}

/// Capacity Verification file (content is written and then checked).
struct VerifyHandle {
    io: OverlappedHandle,
}

impl VerifyHandle {
    /// `create = true`: NEW file (`CREATE_NEW`), read+write, `WRITE_THROUGH`.
    /// `create = false`: reopens read-only. Always `NO_BUFFERING` (no Windows cache).
    fn open(path: &Path, create: bool, max_io: u32) -> Result<Self, u32> {
        let w: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        let (access, disposition, extra) =
            if create { (GENERIC_READ | GENERIC_WRITE, CREATE_NEW, FILE_FLAG_WRITE_THROUGH) } else { (GENERIC_READ, OPEN_EXISTING, 0) };
        let handle = unsafe {
            CreateFileW(w.as_ptr(), access, 0, null(), disposition, FILE_FLAG_NO_BUFFERING | FILE_FLAG_OVERLAPPED | extra, null_mut())
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(last_error());
        }
        let max_io = (max_io.max(4096) as usize).next_multiple_of(BUFFER_ALIGN);
        Ok(Self { io: OverlappedHandle::new(handle, max_io)? })
    }
}

impl VerifyFile for VerifyHandle {
    fn write_block(&mut self, offset: u64, data: &[u8], timeout: Duration) -> IoOutcome {
        if self.io.poisoned {
            // The kernel may still be using the buffer: do not touch it.
            return IoOutcome::Hung { latency: Duration::ZERO };
        }
        if data.len() > self.io.buffer.len() {
            return IoOutcome::Failed { code: crate::io::win32::ERROR_INVALID_PARAMETER, latency: Duration::ZERO };
        }
        unsafe { std::ptr::copy_nonoverlapping(data.as_ptr(), self.io.buffer.as_mut_ptr(), data.len()) };
        self.io.transfer(true, offset, data.len() as u32, timeout)
    }

    fn read_block(&mut self, offset: u64, out: &mut [u8], timeout: Duration) -> IoOutcome {
        let outcome = self.io.transfer(false, offset, out.len() as u32, timeout);
        if let IoOutcome::Completed { bytes, .. } = outcome {
            let n = (bytes as usize).min(out.len());
            unsafe { std::ptr::copy_nonoverlapping(self.io.buffer.as_mut_ptr(), out.as_mut_ptr(), n) };
        }
        outcome
    }
}

/// **New** Capacity Verification folder: `<volume>\diskdoctor-capacity-YYYYMMDD-HHMMSS`.
///
/// - The folder is created with `create_dir` (fails if it already exists).
/// - Files are `CREATE_NEW` (`0001.ddcap`, `0002.ddcap`…): nothing that already exists is
///   opened for writing.
/// - [`CapacityDir::cleanup`] deletes only the files this instance created and then the
///   folder, which must be empty (`remove_dir`, never recursive). `Drop` cleans up too.
pub struct CapacityDir {
    dir: PathBuf,
    files: Vec<PathBuf>,
    max_io: u32,
}

impl CapacityDir {
    pub fn create(parent: &Path, max_io: u32) -> std::io::Result<Self> {
        let dir = parent.join(format!("diskdoctor-capacity-{}", chrono::Local::now().format("%Y%m%d-%H%M%S")));
        std::fs::create_dir(&dir)?;
        Ok(Self { dir, files: Vec::new(), max_io })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn file_path(&self, index: u32) -> PathBuf {
        self.dir.join(format!("{:04}.ddcap", index + 1))
    }

    /// Deletes the created files and the folder. Returns the failures (e.g. a file held by hung I/O).
    pub fn cleanup(&mut self) -> Vec<String> {
        let mut errors = Vec::new();
        for f in self.files.drain(..) {
            if let Err(e) = std::fs::remove_file(&f) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    errors.push(format!("{}: {e}", f.display()));
                }
            }
        }
        if self.dir.exists() {
            if let Err(e) = std::fs::remove_dir(&self.dir) {
                errors.push(format!("{}: {e}", self.dir.display()));
            }
        }
        errors
    }
}

impl Drop for CapacityDir {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

impl VerifyTarget for CapacityDir {
    fn create_file(&mut self, index: u32) -> Result<Box<dyn VerifyFile>, u32> {
        let path = self.file_path(index);
        let file = VerifyHandle::open(&path, true, self.max_io)?;
        self.files.push(path);
        Ok(Box::new(file))
    }

    fn open_file(&mut self, index: u32) -> Result<Box<dyn VerifyFile>, u32> {
        let path = self.file_path(index);
        if !self.files.contains(&path) {
            return Err(crate::io::win32::ERROR_FILE_NOT_FOUND);
        }
        Ok(Box::new(VerifyHandle::open(&path, false, self.max_io)?))
    }
}

fn shell_execute(verb: &str, file: &std::ffi::OsStr, params: &str) -> Result<(), String> {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let verb = wide(verb);
    let file: Vec<u16> = file.encode_wide().chain(std::iter::once(0)).collect();
    let params = wide(params);
    let r = unsafe { ShellExecuteW(null_mut(), verb.as_ptr(), file.as_ptr(), params.as_ptr(), null(), SW_SHOWNORMAL) };
    if r as isize > 32 {
        return Ok(());
    }
    Err(match last_error() {
        1223 => crate::i18n::t("the Administrator permission request was declined", "o pedido de permissão de Administrador foi recusado").into(),
        0 => tr!("could not open (code {})", "falha ao abrir (código {})", r as isize),
        c => win32_error_message(c),
    })
}

/// Opens diskdoctor itself in a new window **as Administrator** (shows the UAC prompt, which
/// the user approves or declines). Used by the interactive mode for direct-read tests.
pub fn relaunch_as_admin(args: &str) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    shell_execute("runas", exe.as_os_str(), args)
}

/// Opens a file with its default program (e.g. the HTML report in the browser).
pub fn open_path(path: &Path) -> Result<(), String> {
    shell_execute("open", path.as_os_str(), "")
}
