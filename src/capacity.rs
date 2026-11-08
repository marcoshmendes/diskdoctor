//! Verificação de capacidade real — detecta pendrives, cartões e discos com capacidade
//! falsificada, no estilo do `f3write`/`f3read`.
//!
//! Como funciona:
//! 1. **Gravação**: cria arquivos **novos** (1 GiB cada) numa pasta **nova** do volume e
//!    preenche o espaço livre. Cada pedaço de 4 KiB leva um cabeçalho com a sua posição
//!    no fluxo de dados (`offset`) e a semente desta execução, seguido de bytes
//!    pseudoaleatórios derivados dos dois.
//! 2. **Verificação**: reabre cada arquivo só para leitura, sem cache do Windows, e confere
//!    pedaço por pedaço.
//!
//! Um dispositivo falso "dá a volta": o que é gravado além da capacidade real cai em cima
//! do que foi gravado antes. Isso aparece como pedaço com o cabeçalho de **outra posição**
//! ([`ChunkState::Overwritten`]). Outros falsos descartam a gravação e devolvem zeros
//! ([`ChunkState::Lost`]) a partir de certo ponto.
//!
//! O motor só conhece os traits [`VerifyTarget`]/[`VerifyFile`]; a implementação real
//! (pasta nova + `CREATE_NEW` + `NO_BUFFERING`) fica em `platform::win::CapacityDir`.

use crate::inventory::DiskIdentity;
use crate::io::{IoOutcome, VerifyFile, VerifyTarget, describe_error, win32};
use crate::scan::ScanStatus;
use crate::util::SplitMix64;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Granularidade da verificação.
pub const CHUNK: usize = 4096;
const MAGIC: [u8; 8] = *b"DDCAPv01";
const MAX_BAD_RANGES: usize = 200;
const MAX_ERROR_SAMPLES: usize = 20;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CapacityConfig {
    /// Quanto gravar (arredondado para baixo para múltiplo de `block_size`).
    pub total_bytes: u64,
    /// Tamanho de cada arquivo de teste (1 GiB cabe até em FAT32).
    pub file_size: u64,
    /// Tamanho de cada gravação/leitura (múltiplo de 4 KiB).
    pub block_size: u32,
    /// Tempo limite por operação. Flash barata pode levar segundos para gravar um bloco.
    pub io_timeout_ms: u64,
    /// Interrompe após esta quantidade de falhas de gravação seguidas.
    pub max_consecutive_errors: u32,
    /// Semente desta execução: dado de uma execução anterior nunca passa como válido.
    pub seed: u64,
}

impl Default for CapacityConfig {
    fn default() -> Self {
        Self { total_bytes: 0, file_size: 1 << 30, block_size: 1 << 20, io_timeout_ms: 30_000, max_consecutive_errors: 16, seed: 0 }
    }
}

/// Semente difícil de repetir entre execuções.
pub fn new_seed() -> u64 {
    let nanos = Utc::now().timestamp_nanos_opt().unwrap_or_default() as u64;
    SplitMix64::new(nanos ^ (std::process::id() as u64).rotate_left(32)).next_u64()
}

fn fill_chunk(chunk: &mut [u8], offset: u64, seed: u64) {
    chunk[..8].copy_from_slice(&MAGIC);
    chunk[8..16].copy_from_slice(&seed.to_le_bytes());
    chunk[16..24].copy_from_slice(&offset.to_le_bytes());
    let mut rng = SplitMix64::new(seed ^ offset.rotate_left(17));
    for w in chunk[24..].chunks_mut(8) {
        let v = rng.next_u64().to_le_bytes();
        w.copy_from_slice(&v[..w.len()]);
    }
}

/// Preenche um bloco cujo primeiro byte fica na posição `stream_offset` do fluxo de teste.
pub fn fill_block(buf: &mut [u8], stream_offset: u64, seed: u64) {
    for (i, chunk) in buf.chunks_mut(CHUNK).enumerate() {
        fill_chunk(chunk, stream_offset + (i * CHUNK) as u64, seed);
    }
}

/// Resultado da conferência de um pedaço de 4 KiB.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkState {
    Ok,
    /// Contém o dado de **outra posição** deste teste: o dispositivo sobrescreveu (capacidade falsa).
    Overwritten,
    /// Vazio (zeros/0xFF) ou de uma execução anterior: a gravação não persistiu.
    Lost,
    /// Conteúdo alterado (bits trocados).
    Corrupted,
}

pub fn check_chunk(chunk: &[u8], expected_offset: u64, seed: u64, scratch: &mut [u8]) -> ChunkState {
    if chunk[..8] == MAGIC {
        if chunk[8..16] != seed.to_le_bytes() {
            return ChunkState::Lost;
        }
        let found = u64::from_le_bytes(chunk[16..24].try_into().expect("8 bytes"));
        if found != expected_offset {
            return ChunkState::Overwritten;
        }
        fill_chunk(scratch, expected_offset, seed);
        return if scratch[..chunk.len()] == *chunk { ChunkState::Ok } else { ChunkState::Corrupted };
    }
    if chunk.iter().all(|&b| b == 0) || chunk.iter().all(|&b| b == 0xFF) {
        return ChunkState::Lost;
    }
    ChunkState::Corrupted
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapacityVerdict {
    /// Tudo o que foi gravado foi lido de volta corretamente.
    Genuine,
    /// Dados sobrescritos ou perdidos a partir de certo ponto: capacidade anunciada é falsa.
    Fake,
    /// Falhas de gravação/leitura ou dados corrompidos espalhados: dispositivo com defeito.
    Defective,
    /// Cancelado ou nada gravado.
    Inconclusive,
}

impl CapacityVerdict {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Genuine => "Capacidade confirmada",
            Self::Fake => "Capacidade FALSA",
            Self::Defective => "Dispositivo com defeito",
            Self::Inconclusive => "Inconclusivo",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CapacityMetrics {
    pub status: ScanStatus,
    pub verdict: CapacityVerdict,
    pub planned_bytes: u64,
    /// Bytes gravados com sucesso (segundo o dispositivo).
    pub written_bytes: u64,
    /// Bytes conferidos na leitura.
    pub verified_bytes: u64,
    pub ok_bytes: u64,
    pub overwritten_bytes: u64,
    pub lost_bytes: u64,
    pub corrupted_bytes: u64,
    pub unreadable_bytes: u64,
    pub write_errors: u64,
    /// A gravação parou porque o volume encheu antes do planejado.
    pub stopped_by_full_disk: bool,
    /// Primeira posição (no fluxo de teste) com dado incorreto.
    pub first_bad_offset: Option<u64>,
    /// Faixas `[início, fim)` do fluxo com problema (mescladas; até 200).
    pub bad_ranges: Vec<(u64, u64)>,
    pub files_created: u32,
    pub write_seconds: f64,
    pub read_seconds: f64,
    pub write_mb_per_s: f64,
    pub read_mb_per_s: f64,
    pub error_samples: Vec<String>,
}

impl CapacityMetrics {
    pub fn bad_bytes(&self) -> u64 {
        self.overwritten_bytes + self.lost_bytes + self.corrupted_bytes + self.unreadable_bytes
    }

    fn new(planned: u64) -> Self {
        Self {
            status: ScanStatus::Completed,
            verdict: CapacityVerdict::Inconclusive,
            planned_bytes: planned,
            written_bytes: 0,
            verified_bytes: 0,
            ok_bytes: 0,
            overwritten_bytes: 0,
            lost_bytes: 0,
            corrupted_bytes: 0,
            unreadable_bytes: 0,
            write_errors: 0,
            stopped_by_full_disk: false,
            first_bad_offset: None,
            bad_ranges: vec![],
            files_created: 0,
            write_seconds: 0.0,
            read_seconds: 0.0,
            write_mb_per_s: 0.0,
            read_mb_per_s: 0.0,
            error_samples: vec![],
        }
    }

    fn mark_bad(&mut self, start: u64, len: u64) {
        if self.first_bad_offset.is_none_or(|f| start < f) {
            self.first_bad_offset = Some(start);
        }
        let full = self.bad_ranges.len() >= MAX_BAD_RANGES;
        match self.bad_ranges.last_mut() {
            Some(last) if last.1 == start || full => last.1 = last.1.max(start + len),
            _ => self.bad_ranges.push((start, start + len)),
        }
    }

    fn sample(&mut self, text: String) {
        if self.error_samples.len() < MAX_ERROR_SAMPLES {
            self.error_samples.push(text);
        }
    }
}

/// Resultado completo, salvo em `<dados>\capacity\` e incluído no relatório.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CapacityResult {
    pub tool_version: String,
    pub disk: DiskIdentity,
    /// Volume testado ("E:").
    pub volume: String,
    pub file_system: Option<String>,
    pub volume_size: Option<u64>,
    /// Espaço já ocupado antes do teste (não é testado).
    pub used_before: u64,
    pub free_before: u64,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub config: CapacityConfig,
    pub metrics: CapacityMetrics,
    /// Estimativa da capacidade real quando o veredito é "falsa".
    pub estimated_real_capacity: Option<u64>,
    /// Os arquivos de teste foram apagados no fim.
    pub cleanup_ok: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapacityPhase {
    Write,
    Verify,
}

#[derive(Debug, Clone)]
pub struct CapacityLive {
    pub phase: CapacityPhase,
    pub done: u64,
    pub total: u64,
    pub ok_bytes: u64,
    pub bad_bytes: u64,
    pub write_errors: u64,
}

pub trait CapacityObserver {
    fn on_phase(&mut self, phase: CapacityPhase, total: u64);
    fn on_progress(&mut self, live: &CapacityLive);
    fn on_problem(&mut self, _text: &str) {}
}

pub struct NoopCapacityObserver;

impl CapacityObserver for NoopCapacityObserver {
    fn on_phase(&mut self, _phase: CapacityPhase, _total: u64) {}
    fn on_progress(&mut self, _live: &CapacityLive) {}
}

fn is_disk_full(code: u32) -> bool {
    code == win32::ERROR_DISK_FULL || code == win32::ERROR_HANDLE_DISK_FULL
}

fn mb_per_s(bytes: u64, secs: f64) -> f64 {
    if secs <= 0.0 { 0.0 } else { bytes as f64 / 1e6 / secs }
}

/// Os problemas se concentram no fim do fluxo (tudo certo até X, quase nada certo depois)?
/// É o padrão de dispositivo que descarta gravações além da capacidade real.
fn bad_tail(m: &CapacityMetrics, span: u64) -> bool {
    let Some(first) = m.first_bad_offset else { return false };
    let after = span.saturating_sub(first);
    after >= (span / 50).max(64 << 20) && m.bad_bytes() * 100 >= after * 95
}

fn decide(m: &CapacityMetrics, span: u64) -> CapacityVerdict {
    match &m.status {
        ScanStatus::Cancelled => CapacityVerdict::Inconclusive,
        _ if m.overwritten_bytes > 0 => CapacityVerdict::Fake,
        ScanStatus::Aborted(_) => CapacityVerdict::Defective,
        ScanStatus::Completed if m.bad_bytes() == 0 && m.write_errors == 0 => {
            if m.verified_bytes == 0 { CapacityVerdict::Inconclusive } else { CapacityVerdict::Genuine }
        }
        ScanStatus::Completed if m.write_errors == 0 && bad_tail(m, span) => CapacityVerdict::Fake,
        ScanStatus::Completed => CapacityVerdict::Defective,
    }
}

/// Executa gravação e verificação. Nunca toca em nada além dos arquivos que o `target` cria.
pub fn run_capacity_test(
    target: &mut dyn VerifyTarget,
    cfg: &CapacityConfig,
    obs: &mut dyn CapacityObserver,
    cancel: &AtomicBool,
) -> CapacityMetrics {
    let block = (cfg.block_size as u64).max(CHUNK as u64) / CHUNK as u64 * CHUNK as u64;
    let total = cfg.total_bytes / block * block;
    let file_size = (cfg.file_size / block).max(1) * block;
    let timeout = Duration::from_millis(cfg.io_timeout_ms.max(100));
    let mut m = CapacityMetrics::new(total);
    let mut buf = vec![0u8; block as usize];
    let mut scratch = vec![0u8; CHUNK];
    // Quais blocos foram gravados com sucesso (só eles são conferidos).
    let mut written_ok: Vec<bool> = Vec::with_capacity((total / block) as usize);

    // ---------------------------------------------------------------- gravação
    obs.on_phase(CapacityPhase::Write, total);
    let t0 = Instant::now();
    let mut file: Option<(u32, Box<dyn VerifyFile>)> = None;
    let mut consecutive = 0u32;
    let mut s = 0u64;
    while s < total {
        if cancel.load(Ordering::Relaxed) {
            m.status = ScanStatus::Cancelled;
            break;
        }
        let idx = (s / file_size) as u32;
        if file.as_ref().is_none_or(|(i, _)| *i != idx) {
            file = None; // fecha o anterior antes de criar o próximo
            match target.create_file(idx) {
                Ok(f) => {
                    file = Some((idx, f));
                    m.files_created = idx + 1;
                }
                Err(code) if is_disk_full(code) => {
                    m.stopped_by_full_disk = true;
                    break;
                }
                Err(code) => {
                    m.status = ScanStatus::Aborted(format!("não foi possível criar o arquivo de teste {}: {}", idx + 1, describe_error(code)));
                    break;
                }
            }
        }
        fill_block(&mut buf, s, cfg.seed);
        let (_, f) = file.as_mut().expect("arquivo aberto");
        match f.write_block(s % file_size, &buf, timeout) {
            IoOutcome::Completed { bytes, .. } if bytes as u64 == block => {
                written_ok.push(true);
                m.written_bytes += block;
                consecutive = 0;
            }
            IoOutcome::Failed { code, .. } if is_disk_full(code) => {
                m.stopped_by_full_disk = true;
                break;
            }
            IoOutcome::Hung { .. } => {
                m.status = ScanStatus::Aborted("o dispositivo parou de responder durante a gravação".into());
                break;
            }
            other => {
                written_ok.push(false);
                m.write_errors += 1;
                consecutive += 1;
                let why = match other {
                    IoOutcome::Failed { code, .. } => describe_error(code),
                    IoOutcome::TimedOut { .. } => "tempo limite esgotado".into(),
                    IoOutcome::Completed { bytes, .. } => format!("gravação curta ({bytes} bytes)"),
                    IoOutcome::Hung { .. } => unreachable!(),
                };
                let text = format!("falha ao gravar na posição {s}: {why}");
                obs.on_problem(&text);
                m.sample(text);
                m.mark_bad(s, block);
                if consecutive >= cfg.max_consecutive_errors.max(1) {
                    m.status = ScanStatus::Aborted(format!("{consecutive} falhas de gravação seguidas"));
                    break;
                }
            }
        }
        s += block;
        obs.on_progress(&CapacityLive {
            phase: CapacityPhase::Write,
            done: s,
            total,
            ok_bytes: m.written_bytes,
            bad_bytes: m.write_errors * block,
            write_errors: m.write_errors,
        });
    }
    drop(file);
    m.write_seconds = t0.elapsed().as_secs_f64();
    m.write_mb_per_s = mb_per_s(m.written_bytes, m.write_seconds);

    // ---------------------------------------------------------------- verificação
    let span = written_ok.len() as u64 * block;
    let hung = matches!(&m.status, ScanStatus::Aborted(r) if r.contains("parou de responder"));
    if m.status != ScanStatus::Cancelled && !hung && span > 0 {
        obs.on_phase(CapacityPhase::Verify, span);
        let t1 = Instant::now();
        let mut file: Option<(u32, Box<dyn VerifyFile>)> = None;
        let mut failed_open: Option<u32> = None;
        for (bi, &ok_written) in written_ok.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                m.status = ScanStatus::Cancelled;
                break;
            }
            let s = bi as u64 * block;
            let idx = (s / file_size) as u32;
            if ok_written && failed_open != Some(idx) && file.as_ref().is_none_or(|(i, _)| *i != idx) {
                file = None;
                match target.open_file(idx) {
                    Ok(f) => file = Some((idx, f)),
                    Err(code) => {
                        failed_open = Some(idx);
                        let text = format!("não foi possível reabrir o arquivo de teste {}: {}", idx + 1, describe_error(code));
                        obs.on_problem(&text);
                        m.sample(text);
                    }
                }
            }
            if ok_written {
                if failed_open == Some(idx) {
                    m.unreadable_bytes += block;
                    m.mark_bad(s, block);
                } else {
                    let (_, f) = file.as_mut().expect("arquivo aberto");
                    match f.read_block(s % file_size, &mut buf, timeout) {
                        IoOutcome::Completed { bytes, .. } if bytes as u64 == block => {
                            for (ci, chunk) in buf.chunks(CHUNK).enumerate() {
                                let off = s + (ci * CHUNK) as u64;
                                match check_chunk(chunk, off, cfg.seed, &mut scratch) {
                                    ChunkState::Ok => m.ok_bytes += CHUNK as u64,
                                    state => {
                                        match state {
                                            ChunkState::Overwritten => m.overwritten_bytes += CHUNK as u64,
                                            ChunkState::Lost => m.lost_bytes += CHUNK as u64,
                                            _ => m.corrupted_bytes += CHUNK as u64,
                                        }
                                        m.mark_bad(off, CHUNK as u64);
                                    }
                                }
                            }
                        }
                        IoOutcome::Hung { .. } => {
                            m.status = ScanStatus::Aborted("o dispositivo parou de responder durante a verificação".into());
                            break;
                        }
                        other => {
                            m.unreadable_bytes += block;
                            m.mark_bad(s, block);
                            let why = match other {
                                IoOutcome::Failed { code, .. } => describe_error(code),
                                IoOutcome::TimedOut { .. } => "tempo limite esgotado".into(),
                                IoOutcome::Completed { bytes, .. } => format!("leitura curta ({bytes} bytes)"),
                                IoOutcome::Hung { .. } => unreachable!(),
                            };
                            let text = format!("falha ao ler a posição {s}: {why}");
                            obs.on_problem(&text);
                            m.sample(text);
                        }
                    }
                }
                m.verified_bytes += block;
            }
            obs.on_progress(&CapacityLive {
                phase: CapacityPhase::Verify,
                done: s + block,
                total: span,
                ok_bytes: m.ok_bytes,
                bad_bytes: m.bad_bytes(),
                write_errors: m.write_errors,
            });
        }
        m.read_seconds = t1.elapsed().as_secs_f64();
        m.read_mb_per_s = mb_per_s(m.verified_bytes, m.read_seconds);
    }
    m.verdict = decide(&m, span);
    m
}

/// Capacidade real estimada de um dispositivo falso: o que já estava ocupado mais o que
/// foi lido de volta corretamente.
pub fn estimate_real_capacity(m: &CapacityMetrics, used_before: u64) -> Option<u64> {
    (m.verdict == CapacityVerdict::Fake).then_some(used_before + m.ok_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::rc::Rc;

    const MIB: u64 = 1 << 20;

    #[derive(Clone, Copy, PartialEq)]
    enum Mode {
        /// Dispositivo honesto.
        Normal,
        /// Endereços acima da capacidade real voltam para o início (falso clássico).
        Wrap,
        /// Gravações acima da capacidade real somem; a leitura devolve zeros.
        Drop,
    }

    struct Device {
        mode: Mode,
        real: u64,
        data: Vec<u8>,
        file_size: u64,
        /// Posições do fluxo cuja leitura volta com um bit trocado.
        flip_on_read: Vec<u64>,
        /// Posição a partir da qual o volume está cheio.
        full_at: Option<u64>,
        created: Vec<u32>,
    }

    struct Target(Rc<RefCell<Device>>);
    struct File {
        dev: Rc<RefCell<Device>>,
        base: u64,
    }

    impl VerifyFile for File {
        fn write_block(&mut self, offset: u64, data: &[u8], _t: Duration) -> IoOutcome {
            let mut d = self.dev.borrow_mut();
            let s = self.base + offset;
            if d.full_at.is_some_and(|f| s + data.len() as u64 > f) {
                return IoOutcome::Failed { code: win32::ERROR_DISK_FULL, latency: Duration::ZERO };
            }
            let real = d.real;
            let mut i = 0;
            while i < data.len() {
                let pos = s + i as u64;
                if d.mode == Mode::Drop && pos >= real {
                    break;
                }
                let p = (pos % real) as usize;
                let n = (data.len() - i).min(real as usize - p);
                d.data[p..p + n].copy_from_slice(&data[i..i + n]);
                i += n;
            }
            IoOutcome::Completed { bytes: data.len() as u32, latency: Duration::ZERO }
        }
        fn read_block(&mut self, offset: u64, out: &mut [u8], _t: Duration) -> IoOutcome {
            let d = self.dev.borrow();
            let s = self.base + offset;
            let mut i = 0;
            while i < out.len() {
                let pos = s + i as u64;
                if d.mode == Mode::Drop && pos >= d.real {
                    out[i..].fill(0);
                    break;
                }
                let p = (pos % d.real) as usize;
                let n = (out.len() - i).min(d.real as usize - p);
                out[i..i + n].copy_from_slice(&d.data[p..p + n]);
                i += n;
            }
            for &f in &d.flip_on_read {
                if f >= s && f < s + out.len() as u64 {
                    out[(f - s) as usize] ^= 0x10;
                }
            }
            IoOutcome::Completed { bytes: out.len() as u32, latency: Duration::ZERO }
        }
    }

    impl VerifyTarget for Target {
        fn create_file(&mut self, index: u32) -> Result<Box<dyn VerifyFile>, u32> {
            let mut d = self.0.borrow_mut();
            let base = index as u64 * d.file_size;
            if d.full_at.is_some_and(|f| base >= f) {
                return Err(win32::ERROR_DISK_FULL);
            }
            assert!(!d.created.contains(&index), "nunca recria um arquivo");
            d.created.push(index);
            Ok(Box::new(File { dev: self.0.clone(), base }))
        }
        fn open_file(&mut self, index: u32) -> Result<Box<dyn VerifyFile>, u32> {
            let d = self.0.borrow();
            assert!(d.created.contains(&index), "só reabre arquivos que criou");
            Ok(Box::new(File { dev: self.0.clone(), base: index as u64 * d.file_size }))
        }
    }

    fn device(mode: Mode, real: u64) -> Rc<RefCell<Device>> {
        Rc::new(RefCell::new(Device { mode, real, data: vec![0; real as usize], file_size: 16 * MIB, flip_on_read: vec![], full_at: None, created: vec![] }))
    }

    fn cfg(total: u64) -> CapacityConfig {
        CapacityConfig { total_bytes: total, file_size: 16 * MIB, block_size: MIB as u32, seed: 0xABCD, ..Default::default() }
    }

    fn run(dev: &Rc<RefCell<Device>>, total: u64) -> CapacityMetrics {
        run_capacity_test(&mut Target(dev.clone()), &cfg(total), &mut NoopCapacityObserver, &AtomicBool::new(false))
    }

    #[test]
    fn genuine_device_passes() {
        let dev = device(Mode::Normal, 64 * MIB);
        let m = run(&dev, 64 * MIB);
        assert_eq!(m.verdict, CapacityVerdict::Genuine);
        assert_eq!(m.ok_bytes, 64 * MIB);
        assert_eq!(m.bad_bytes(), 0);
        assert_eq!(m.files_created, 4);
        assert_eq!(estimate_real_capacity(&m, 0), None);
    }

    #[test]
    fn wraparound_fake_is_detected_with_real_size_estimate() {
        // Anuncia 64 MiB, tem 24 MiB: o que passa de 24 MiB cai por cima do começo.
        let dev = device(Mode::Wrap, 24 * MIB);
        let m = run(&dev, 64 * MIB);
        assert_eq!(m.verdict, CapacityVerdict::Fake);
        assert!(m.overwritten_bytes > 0);
        assert_eq!(m.ok_bytes, 24 * MIB, "só o último giro sobrevive");
        assert_eq!(estimate_real_capacity(&m, 2 * MIB), Some(26 * MIB));
    }

    #[test]
    fn fake_that_drops_writes_is_detected() {
        // Anuncia 256 MiB, tem 160 MiB: o que passa disso some e a leitura volta zerada.
        let dev = device(Mode::Drop, 160 * MIB);
        let m = run(&dev, 256 * MIB);
        assert_eq!(m.verdict, CapacityVerdict::Fake);
        assert_eq!(m.lost_bytes, 96 * MIB);
        assert_eq!(m.first_bad_offset, Some(160 * MIB));
        assert_eq!(m.bad_ranges, vec![(160 * MIB, 256 * MIB)]);
    }

    #[test]
    fn scattered_corruption_means_defective() {
        let dev = device(Mode::Normal, 64 * MIB);
        dev.borrow_mut().flip_on_read = vec![5 * MIB + 100, 40 * MIB + 9000];
        let m = run(&dev, 64 * MIB);
        assert_eq!(m.verdict, CapacityVerdict::Defective);
        assert_eq!(m.corrupted_bytes, 2 * CHUNK as u64);
        assert_eq!(m.bad_ranges.len(), 2);
    }

    #[test]
    fn full_volume_ends_writing_early_without_failing() {
        let dev = device(Mode::Normal, 64 * MIB);
        dev.borrow_mut().full_at = Some(40 * MIB);
        let m = run(&dev, 64 * MIB);
        assert!(m.stopped_by_full_disk);
        assert_eq!(m.written_bytes, 40 * MIB);
        assert_eq!(m.verdict, CapacityVerdict::Genuine);
    }

    #[test]
    fn cancel_is_inconclusive() {
        let dev = device(Mode::Normal, 64 * MIB);
        let m = run_capacity_test(&mut Target(dev.clone()), &cfg(64 * MIB), &mut NoopCapacityObserver, &AtomicBool::new(true));
        assert_eq!(m.status, ScanStatus::Cancelled);
        assert_eq!(m.verdict, CapacityVerdict::Inconclusive);
    }

    #[test]
    fn chunk_check_classifies_content() {
        let mut chunk = vec![0u8; CHUNK];
        let mut scratch = vec![0u8; CHUNK];
        fill_chunk(&mut chunk, 8192, 7);
        assert_eq!(check_chunk(&chunk, 8192, 7, &mut scratch), ChunkState::Ok);
        assert_eq!(check_chunk(&chunk, 4096, 7, &mut scratch), ChunkState::Overwritten);
        assert_eq!(check_chunk(&chunk, 8192, 8, &mut scratch), ChunkState::Lost, "dado de execução anterior");
        chunk[100] ^= 1;
        assert_eq!(check_chunk(&chunk, 8192, 7, &mut scratch), ChunkState::Corrupted);
        assert_eq!(check_chunk(&[0u8; CHUNK], 0, 7, &mut scratch), ChunkState::Lost);
        assert_eq!(check_chunk(&[0xFFu8; CHUNK], 0, 7, &mut scratch), ChunkState::Lost);
        assert_eq!(check_chunk(&[0x5Au8; CHUNK], 0, 7, &mut scratch), ChunkState::Corrupted);
    }
}
