//! diskdoctor — non-destructive diagnostics for HDDs, SSDs (SATA/NVMe), USB drives and
//! memory cards on Windows.
//!
//! Module map (details in `AGENTS.md` and `docs/ARCHITECTURE.md`):
//!
//! | Layer         | Modules                                                          |
//! |---------------|------------------------------------------------------------------|
//! | Entry         | [`app`], [`cli`], [`commands`], [`interactive`], [`menu`]        |
//! | Collection    | [`inventory`], [`smart`], [`windows_counters`], [`events`], [`health`] |
//! | Tests         | [`scan`], [`benchmark`], [`capacity`] (on top of the [`io`] traits) |
//! | Analysis      | [`analysis`], [`history`], [`journal`] (before × after)          |
//! | Output        | [`report`], [`ui`], [`store`]                                    |
//! | Configuration | [`tags`] (drive nicknames for `--disk`), [`i18n`] (language)     |
//! | Platform      | [`platform`]: the only place with `unsafe`/Win32 and external processes |
//! | Utilities     | [`util`], [`stats`]                                              |

// First, so `tr!` is in scope for every module below.
#[macro_use]
pub mod i18n;

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
pub mod menu;
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

/// Tool version (from Cargo.toml).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
