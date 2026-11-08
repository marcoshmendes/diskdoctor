//! Camada de plataforma: único lugar com `unsafe`, chamadas Win32 e processos externos.
//!
//! - [`process`]: executa processos com tempo limite (PowerShell, smartctl).
//! - [`powershell`]: roda scripts PowerShell e devolve o JSON gerado.
//! - `win` (somente Windows): disco físico somente leitura (`RawDisk`), arquivo
//!   temporário de benchmark (`TempBenchFile`), elevação, mensagens de erro.
//! - `unsupported` (outros SOs): stubs que retornam erro, para que o restante do
//!   projeto compile e os testes de lógica rodem em qualquer SO.

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

/// Falha ao abrir um disco físico.
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
        match self {
            Self::AccessDenied => write!(
                f,
                "acesso negado ao disco. Execute o terminal como Administrador; se já estiver, \
                 algum software (antivírus, criptografia, ferramenta de disco) pode estar bloqueando o dispositivo"
            ),
            Self::NotFound => write!(f, "disco não encontrado (confira o número com `diskdoctor list`)"),
            Self::SharingViolation => write!(f, "o dispositivo está bloqueado por outro processo (uso exclusivo)"),
            Self::Unsupported => write!(f, "acesso direto a disco só é suportado no Windows"),
            Self::Other { code, message } => write!(f, "erro {code} ao abrir o disco: {message}"),
        }
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
