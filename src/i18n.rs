//! Interface language: English (default) or Brazilian Portuguese.
//!
//! Every user-facing string is written inline in both languages, so a missing
//! translation is a compile error instead of a runtime surprise:
//!
//! - [`t`] picks one of two static strings;
//! - [`tr!`](crate::tr) formats in the active language: `tr!("{n} drives", "{n} discos")`.
//!   Both format strings must use the same arguments.
//!
//! Technical names stay in English in both languages: SMART, Surface Scan, Benchmark,
//! Sequential Read, Random 4 KiB, QD1, Capacity Verification, LBA, CRC, TRIM, IOPS.
//!
//! The language is chosen at startup, highest priority first: `--lang`,
//! `DISKDOCTOR_LANG`, the `language` saved in `settings.json` (written by
//! `diskdoctor language` and by the interactive menu), English.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};

/// Formats in the active language. Both format strings must use the same arguments.
/// Declared before everything else: `macro_rules!` is textually scoped (`lib.rs`
/// declares this module first with `#[macro_use]`).
#[macro_export]
macro_rules! tr {
    ($en:literal, $pt:literal $(,)?) => {
        if $crate::i18n::is_pt() { ::std::format!($pt) } else { ::std::format!($en) }
    };
    ($en:literal, $pt:literal, $($arg:tt)+) => {
        if $crate::i18n::is_pt() { ::std::format!($pt, $($arg)+) } else { ::std::format!($en, $($arg)+) }
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Lang {
    #[default]
    #[serde(rename = "en")]
    En,
    #[serde(rename = "pt-BR")]
    PtBr,
}

/// Supported languages, in menu order.
pub const ALL: [Lang; 2] = [Lang::En, Lang::PtBr];

impl Lang {
    /// Code used by `--lang`, `DISKDOCTOR_LANG` and `settings.json`.
    pub fn code(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::PtBr => "pt-BR",
        }
    }

    /// Name written in the language itself (what a menu shows).
    pub fn native_name(self) -> &'static str {
        match self {
            Self::En => "English",
            Self::PtBr => "Português (Brasil)",
        }
    }

    /// Accepts `en`, `pt-BR` and common spellings (`pt`, `pt_br`, `english`, `portugues`).
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_lowercase().replace('_', "-").as_str() {
            "en" | "en-us" | "en-gb" | "english" | "inglês" | "ingles" => Some(Self::En),
            "pt" | "pt-br" | "br" | "portuguese" | "português" | "portugues" => Some(Self::PtBr),
            _ => None,
        }
    }
}

impl std::fmt::Display for Lang {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

/// Value parser for `--lang` and `diskdoctor language <LANG>`.
pub fn parse_arg(raw: &str) -> Result<Lang, String> {
    Lang::parse(raw).ok_or_else(|| tr!("unknown language '{raw}' (use en or pt-BR)", "idioma desconhecido '{raw}' (use en ou pt-BR)"))
}

static CURRENT: AtomicU8 = AtomicU8::new(0);
static SOURCE: AtomicU8 = AtomicU8::new(0);

thread_local! {
    /// Per-thread override used by tests (see [`scoped`]); production code never sets it.
    static OVERRIDE: Cell<Option<Lang>> = const { Cell::new(None) };
}

/// Where the active language came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Default,
    Flag,
    Environment,
    Settings,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Self::Default => t("default", "padrão"),
            Self::Flag => t("--lang option", "opção --lang"),
            Self::Environment => t("DISKDOCTOR_LANG environment variable", "variável de ambiente DISKDOCTOR_LANG"),
            Self::Settings => t("saved preference", "preferência salva"),
        }
    }

    fn to_u8(self) -> u8 {
        match self {
            Self::Default => 0,
            Self::Flag => 1,
            Self::Environment => 2,
            Self::Settings => 3,
        }
    }

    fn from_u8(v: u8) -> Self {
        match v {
            1 => Self::Flag,
            2 => Self::Environment,
            3 => Self::Settings,
            _ => Self::Default,
        }
    }
}

pub fn current() -> Lang {
    OVERRIDE.with(Cell::get).unwrap_or_else(|| match CURRENT.load(Ordering::Relaxed) {
        1 => Lang::PtBr,
        _ => Lang::En,
    })
}

pub fn source() -> Source {
    Source::from_u8(SOURCE.load(Ordering::Relaxed))
}

/// Changes the language of the whole process (startup and `diskdoctor language`).
pub fn set(lang: Lang, source: Source) {
    let v = match lang {
        Lang::En => 0,
        Lang::PtBr => 1,
    };
    CURRENT.store(v, Ordering::Relaxed);
    SOURCE.store(source.to_u8(), Ordering::Relaxed);
}

pub fn is_pt() -> bool {
    current() == Lang::PtBr
}

/// One of two static strings, by the active language.
pub fn t(en: &'static str, pt: &'static str) -> &'static str {
    if is_pt() { pt } else { en }
}

/// Restores the previous per-thread language when dropped.
#[doc(hidden)]
pub struct LangScope(Option<Lang>);

impl Drop for LangScope {
    fn drop(&mut self) {
        let previous = self.0;
        OVERRIDE.with(|o| o.set(previous));
    }
}

/// Uses `lang` on the current thread only, until the guard is dropped. Lets parallel
/// tests check both languages without touching the process-wide setting.
#[doc(hidden)]
pub fn scoped(lang: Lang) -> LangScope {
    LangScope(OVERRIDE.with(|o| o.replace(Some(lang))))
}

// ---------------------------------------------------------------------------
// Saved preference (`<data folder>\settings.json`)
// ---------------------------------------------------------------------------

const SETTINGS_FILE: &str = "settings.json";

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Settings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<Lang>,
}

pub fn settings_path(data_dir: &Path) -> PathBuf {
    data_dir.join(SETTINGS_FILE)
}

/// A missing or unreadable file means defaults: a bad settings file never blocks a diagnosis.
pub fn load_settings(data_dir: &Path) -> Settings {
    fs::read_to_string(settings_path(data_dir)).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
}

/// Saves the language preference (temporary file + rename, like `tags.json`).
pub fn save_language(data_dir: &Path, lang: Lang) -> Result<PathBuf> {
    let mut settings = load_settings(data_dir);
    settings.language = Some(lang);
    fs::create_dir_all(data_dir).with_context(|| tr!("could not create {}", "não foi possível criar {}", data_dir.display()))?;
    let path = settings_path(data_dir);
    let tmp = path.with_extension("json.tmp");
    fs::write(&tmp, serde_json::to_string_pretty(&settings)?).with_context(|| tr!("could not write {}", "não foi possível gravar {}", tmp.display()))?;
    fs::rename(&tmp, &path).with_context(|| tr!("could not write {}", "não foi possível gravar {}", path.display()))?;
    Ok(path)
}

/// Picks the language: `--lang`, then `DISKDOCTOR_LANG`, then `settings.json`, then English.
/// Invalid values are skipped (clap reports a bad `--lang` itself).
pub fn resolve(flag: Option<&str>, env: Option<&str>, data_dir: &Path) -> (Lang, Source) {
    if let Some(lang) = flag.and_then(Lang::parse) {
        return (lang, Source::Flag);
    }
    if let Some(lang) = env.and_then(Lang::parse) {
        return (lang, Source::Environment);
    }
    if let Some(lang) = load_settings(data_dir).language {
        return (lang, Source::Settings);
    }
    (Lang::En, Source::Default)
}

/// Value of `--lang` in the raw arguments. Read before clap so the help text is localized.
pub fn flag_value(args: &[OsString]) -> Option<String> {
    let mut it = args.iter().skip(1).map(|a| a.to_string_lossy().into_owned());
    while let Some(arg) = it.next() {
        if arg == "--" {
            break;
        }
        if let Some(v) = arg.strip_prefix("--lang=") {
            return Some(v.to_string());
        }
        if arg == "--lang" {
            return it.next();
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_language_codes() {
        assert_eq!(Lang::parse("en"), Some(Lang::En));
        assert_eq!(Lang::parse(" EN-us "), Some(Lang::En));
        assert_eq!(Lang::parse("pt-BR"), Some(Lang::PtBr));
        assert_eq!(Lang::parse("pt_br"), Some(Lang::PtBr));
        assert_eq!(Lang::parse("Português"), Some(Lang::PtBr));
        assert_eq!(Lang::parse("fr"), None);
        assert!(parse_arg("xx").is_err());
    }

    #[test]
    fn scoped_override_is_per_thread_and_restored() {
        let before = current();
        {
            let _pt = scoped(Lang::PtBr);
            assert!(is_pt());
            assert_eq!(t("Drive", "Disco"), "Disco");
            let n = 3;
            assert_eq!(tr!("{n} drives", "{n} discos"), "3 discos");
            {
                let _en = scoped(Lang::En);
                assert_eq!(tr!("{} drives", "{} discos", n), "3 drives");
            }
            assert!(is_pt(), "inner scope restores the outer one");
        }
        assert_eq!(current(), before);
    }

    #[test]
    fn resolve_follows_priority() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(resolve(None, None, dir.path()), (Lang::En, Source::Default));
        save_language(dir.path(), Lang::PtBr).unwrap();
        assert_eq!(load_settings(dir.path()).language, Some(Lang::PtBr));
        assert!(!dir.path().join("settings.json.tmp").exists());
        assert_eq!(resolve(None, None, dir.path()), (Lang::PtBr, Source::Settings));
        assert_eq!(resolve(None, Some("en"), dir.path()), (Lang::En, Source::Environment));
        assert_eq!(resolve(Some("en"), Some("pt-BR"), dir.path()), (Lang::En, Source::Flag));
        assert_eq!(resolve(Some("??"), Some("??"), dir.path()), (Lang::PtBr, Source::Settings), "invalid values are skipped");
        fs::write(settings_path(dir.path()), "{ broken").unwrap();
        assert_eq!(resolve(None, None, dir.path()), (Lang::En, Source::Default), "corrupt file = defaults");
    }

    #[test]
    fn finds_lang_flag_in_raw_args() {
        let args = |v: &[&str]| v.iter().map(OsString::from).collect::<Vec<_>>();
        assert_eq!(flag_value(&args(&["diskdoctor", "health", "--lang", "pt-BR"])).as_deref(), Some("pt-BR"));
        assert_eq!(flag_value(&args(&["diskdoctor", "--lang=en", "list"])).as_deref(), Some("en"));
        assert_eq!(flag_value(&args(&["diskdoctor", "list"])), None);
        assert_eq!(flag_value(&args(&["diskdoctor", "history", "note", "--disk", "2", "--", "--lang", "x"])), None);
    }
}
