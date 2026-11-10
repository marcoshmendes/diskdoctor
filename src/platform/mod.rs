//! Platform layer: the only place with `unsafe`, Win32 calls and external processes.
//!
//! - [`process`]: runs processes with a timeout (PowerShell, smartctl).
//! - [`powershell`]: runs PowerShell scripts and returns the JSON they print.
//! - `win` (Windows only): read-only physical drive (`RawDisk`), benchmark temporary file
//!   (`TempBenchFile`), Capacity Verification folder (`CapacityDir`), elevation, console
//!   mode, error messages.
//! - `unsupported` (other OSes): stubs that return errors, so the rest of the project
//!   builds and the logic tests run on any OS.

pub mod powershell;
pub mod process;

#[cfg(windows)]
mod win;
#[cfg(windows)]
pub use win::*;

#[cfg(not(windows))]
mod unsupported;
#[cfg(not(windows))]
pub use unsupported::*;

use std::fmt;

/// Failure to open a physical drive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    AccessDenied,
    NotFound,
    SharingViolation,
    Unsupported,
    Other { code: u32, message: String },
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let text = match self {
            Self::AccessDenied => tr!(
                "access to the drive was denied. Run the terminal as Administrator; if it already is, \
                 some software (antivirus, encryption, disk tool) may be locking the device",
                "acesso negado ao disco. Execute o terminal como Administrador; se já estiver, \
                 algum software (antivírus, criptografia, ferramenta de disco) pode estar bloqueando o dispositivo"
            ),
            Self::NotFound => tr!("drive not found (check the number with `diskdoctor list`)", "disco não encontrado (confira o número com `diskdoctor list`)"),
            Self::SharingViolation => tr!("the device is locked by another process (exclusive use)", "o dispositivo está bloqueado por outro processo (uso exclusivo)"),
            Self::Unsupported => tr!("direct drive access is only supported on Windows", "acesso direto a disco só é suportado no Windows"),
            Self::Other { code, message } => tr!("error {code} opening the drive: {message}", "erro {code} ao abrir o disco: {message}"),
        };
        f.write_str(&text)
    }
}

impl std::error::Error for OpenError {}

impl OpenError {
    pub fn from_code(code: u32, message: String) -> Self {
        match code {
            5 => Self::AccessDenied,
            2 | 3 => Self::NotFound,
            32 => Self::SharingViolation,
            _ => Self::Other { code, message },
        }
    }
}
