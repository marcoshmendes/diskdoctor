//! diskdoctor — diagnóstico não destrutivo de HDs e SSDs (SATA/NVMe) no Windows.
//!
//! Mapa dos módulos (detalhes em `AGENTS.md` e `docs/ARCHITECTURE.md`):
//!
//! | Camada        | Módulos                                                        |
//! |---------------|----------------------------------------------------------------|
//! | Entrada       | [`app`], [`cli`], [`commands`]                                 |
//! | Coleta        | [`inventory`], [`smart`], [`windows_counters`], [`events`], [`health`] |
//! | Testes        | [`scan`], [`benchmark`], [`capacity`] (sobre os traits de [`io`]) |
//! | Análise       | [`analysis`], [`history`], [`journal`] (antes × depois)        |
//! | Saída         | [`report`], [`ui`], [`store`]                                  |
//! | Configuração  | [`tags`] — apelidos dos discos, aceitos em `--disk`            |
//! | Plataforma    | [`platform`] — único lugar com `unsafe`/Win32 e processos externos |
//! | Utilitários   | [`util`], [`stats`]                                            |

pub mod analysis;
pub mod app;
pub mod benchmark;
pub mod capacity;
pub mod cli;
pub mod commands;
pub mod events;
pub mod health;
pub mod history;
pub mod interactive;
pub mod inventory;
pub mod io;
pub mod journal;
pub mod platform;
pub mod report;
pub mod scan;
pub mod smart;
pub mod stats;
pub mod store;
pub mod tags;
pub mod ui;
pub mod util;
pub mod windows_counters;

/// Versão da ferramenta (do Cargo.toml).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
