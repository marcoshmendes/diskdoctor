//! Abstrações de E/S em bloco usadas pelo teste de leitura (`scan`) e pelo `benchmark`.
//!
//! Os motores de teste dependem só destes traits, então podem ser testados com
//! dispositivos simulados ([`testing::ScriptedReader`]) sem tocar em disco real.
//! As implementações reais ficam em `platform::win` (`RawDisk`, `TempBenchFile`).
//!
//! Invariante de segurança: o disco físico só é exposto como [`BlockReader`].
//! Não existe implementação de [`BlockWriter`] para disco físico — gravação só
//! acontece em arquivo temporário novo (`TempBenchFile`).

use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Resultado de uma operação de E/S em bloco.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IoOutcome {
    /// Concluída. `bytes` pode ser menor que o pedido (leitura curta).
    Completed { bytes: u32, latency: Duration },
    /// Falhou com código de erro Win32.
    Failed { code: u32, latency: Duration },
    /// Excedeu o tempo limite configurado e foi cancelada com sucesso.
    TimedOut { latency: Duration },
    /// Excedeu o tempo limite e nem o cancelamento foi concluído: o dispositivo
    /// (ou a controladora) parou de responder. O leitor fica inutilizável.
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

/// Dispositivo que só permite leitura (disco físico).
pub trait BlockReader {
    /// Capacidade total em bytes.
    fn capacity(&self) -> u64;
    /// Tamanho do setor lógico (alinhamento mínimo de offset e tamanho).
    fn logical_sector_size(&self) -> u32;
    /// Tamanho do setor físico (granularidade usada para isolar setores ruins).
    fn physical_sector_size(&self) -> u32;
    /// Maior leitura suportada por chamada.
    fn max_io_size(&self) -> u32;
    /// Lê `len` bytes a partir de `offset` (ambos alinhados ao setor lógico).
    fn read_at(&mut self, offset: u64, len: u32, timeout: Duration) -> IoOutcome;
}

/// Destino de gravação de benchmark (somente arquivo temporário novo).
pub trait BlockWriter {
    /// Tamanho da área de teste em bytes.
    fn capacity(&self) -> u64;
    fn max_io_size(&self) -> u32;
    fn write_at(&mut self, offset: u64, len: u32, timeout: Duration) -> IoOutcome;
}

/// Arquivo do teste de capacidade: diferente do [`BlockWriter`], o **conteúdo** importa,
/// porque cada bloco é gravado com um padrão e conferido na leitura.
pub trait VerifyFile {
    /// Grava `data` (tamanho alinhado ao setor) em `offset` dentro do arquivo.
    fn write_block(&mut self, offset: u64, data: &[u8], timeout: Duration) -> IoOutcome;
    /// Lê `out.len()` bytes de `offset` dentro do arquivo, sem cache do sistema.
    fn read_block(&mut self, offset: u64, out: &mut [u8], timeout: Duration) -> IoOutcome;
}

/// Pasta **nova** onde o teste de capacidade cria seus arquivos. Só cria arquivos novos e
/// só reabre os que ela mesma criou (`index` = ordem de criação).
pub trait VerifyTarget {
    /// Cria o arquivo `index` (falha se já existir). `Err` = código de erro Win32.
    fn create_file(&mut self, index: u32) -> Result<Box<dyn VerifyFile>, u32>;
    /// Reabre, só para leitura, um arquivo criado antes por [`VerifyTarget::create_file`].
    fn open_file(&mut self, index: u32) -> Result<Box<dyn VerifyFile>, u32>;
}

/// Códigos de erro Win32 relevantes para E/S de disco.
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

/// Classe de um erro de E/S, usada pelo motor de leitura e pela análise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorClass {
    /// O disco reportou dado ilegível (setor defeituoso).
    Media,
    /// Erro genérico de dispositivo/comunicação.
    Device,
    /// Tempo limite estourado no driver.
    Timeout,
    /// Erro que impede continuar (dispositivo sumiu, acesso negado, parâmetro inválido).
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

/// Descrição curta em pt-BR de um código de erro Win32.
pub fn describe_error(code: u32) -> String {
    use win32::*;
    let text = match code {
        ERROR_CRC => "erro de dados (CRC): setor ilegível",
        ERROR_SECTOR_NOT_FOUND => "setor não encontrado",
        ERROR_READ_FAULT => "falha de leitura no dispositivo",
        ERROR_WRITE_FAULT => "falha de gravação no dispositivo",
        ERROR_IO_DEVICE => "erro de E/S do dispositivo (comunicação ou hardware)",
        ERROR_DEVICE_HARDWARE_ERROR => "erro de hardware reportado pelo dispositivo",
        ERROR_GEN_FAILURE => "o dispositivo não está funcionando corretamente",
        ERROR_SEM_TIMEOUT | ERROR_TIMEOUT => "tempo limite esgotado no driver",
        ERROR_NOT_READY => "dispositivo não está pronto",
        ERROR_DEVICE_NOT_CONNECTED | ERROR_DEV_NOT_EXIST | ERROR_NO_SUCH_DEVICE | ERROR_DEVICE_REMOVED => {
            "dispositivo desconectado ou removido"
        }
        ERROR_ACCESS_DENIED => "acesso negado",
        ERROR_INVALID_PARAMETER => "parâmetro inválido (alinhamento de E/S)",
        ERROR_OPERATION_ABORTED => "operação cancelada",
        ERROR_HANDLE_EOF => "leitura curta (fim do dispositivo)",
        ERROR_DISK_FULL | ERROR_HANDLE_DISK_FULL => "disco cheio",
        _ => return format!("erro do Windows {code}"),
    };
    format!("{text} (código {code})")
}

/// Dispositivos simulados para testes automatizados (não usados em produção).
#[doc(hidden)]
pub mod testing {
    use super::*;

    const MIB: u64 = 1 << 20;

    fn overlaps(offset: u64, len: u32, start: u64, end: u64) -> bool {
        offset < end && start < offset + len as u64
    }

    /// Leitor com comportamento roteirizado por faixas de bytes.
    #[derive(Debug, Clone)]
    pub struct ScriptedReader {
        pub capacity: u64,
        pub logical: u32,
        pub physical: u32,
        pub max_io: u32,
        pub latency_per_mib: Duration,
        /// Faixas `[início, fim)` sempre ilegíveis (ERROR_CRC).
        pub bad_ranges: Vec<(u64, u64)>,
        /// Faixas que falham (ERROR_IO_DEVICE) só na primeira leitura que as toca.
        pub transient_bad: Vec<(u64, u64)>,
        /// Faixas lentas: latência extra somada.
        pub slow_ranges: Vec<(u64, u64, Duration)>,
        /// Faixas que estouram o tempo limite.
        pub timeout_ranges: Vec<(u64, u64)>,
        /// Offset a partir do qual o dispositivo trava.
        pub hang_at: Option<u64>,
        /// Offset e código de erro fatal.
        pub fatal_at: Option<(u64, u32)>,
        /// Registro de todas as leituras feitas (offset, tamanho).
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
            assert_eq!(offset % self.logical as u64, 0, "offset desalinhado");
            assert_eq!(len % self.logical, 0, "tamanho desalinhado");
            assert!(offset + len as u64 <= self.capacity, "leitura além do fim");
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

    /// Gravador simulado para o benchmark de escrita.
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
            assert!(offset + len as u64 <= self.capacity, "gravação além do arquivo temporário");
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
    fn describes_errors_in_portuguese() {
        assert!(describe_error(23).contains("setor ilegível"));
        assert!(describe_error(23).contains("23"));
        assert_eq!(describe_error(4242), "erro do Windows 4242");
    }
}
