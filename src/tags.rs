//! Tags (nicknames) the user gives to drives: "Studio", "Games", "Backup"…
//!
//! Stored in `<data folder>\tags.json`, bound to the drive **fingerprint** (hash of model +
//! serial) rather than its number: the Windows drive number can change between reboots or
//! when a drive is plugged into another port.
//!
//! Tags also select a drive: `--disk studio` instead of `--disk 2` ([`DiskRef`]). To keep
//! that unambiguous, a tag cannot be digits only.

use crate::inventory::DiskInfo;
use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

pub const MAX_TAG_CHARS: usize = 32;
pub const MAX_TAGS_PER_DISK: usize = 10;
const FILE_NAME: &str = "tags.json";
const FORMAT_VERSION: u32 = 1;

/// Comparison key: collapsed spaces and lower case ("  studio " == "Studio").
fn tag_key(tag: &str) -> String {
    tag.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

pub fn same_tag(a: &str, b: &str) -> bool {
    tag_key(a) == tag_key(b)
}

/// Validates and normalizes a new tag.
///
/// Rules: spaces trimmed and collapsed; 1 to 32 characters; only letters (accents
/// allowed), digits, space, `-`, `_` and `.`; it cannot start with `-` (it would be read as
/// an option) nor be digits only (it would be mistaken for the drive number in `--disk`).
pub fn normalize_tag(raw: &str) -> Result<String> {
    let tag = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if tag.is_empty() {
        bail!(crate::i18n::t("empty tag", "tag vazia"));
    }
    let len = tag.chars().count();
    if len > MAX_TAG_CHARS {
        bail!(tr!(
            "the tag \"{tag}\" has {len} characters; the maximum is {MAX_TAG_CHARS}",
            "a tag \"{tag}\" tem {len} caracteres; o máximo é {MAX_TAG_CHARS}"
        ));
    }
    if let Some(c) = tag.chars().find(|c| !(c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.'))) {
        bail!(tr!(
            "the tag \"{tag}\" has the character '{c}', which is not allowed. Use letters, digits, space, '-', '_' or '.'",
            "a tag \"{tag}\" tem o caractere '{c}', que não é aceito. Use letras, números, espaço, '-', '_' ou '.'"
        ));
    }
    if tag.starts_with('-') {
        bail!(tr!("the tag \"{tag}\" cannot start with '-'", "a tag \"{tag}\" não pode começar com '-'"));
    }
    if tag.chars().all(|c| c.is_ascii_digit()) {
        bail!(tr!(
            "the tag \"{tag}\" cannot be digits only (it would be mistaken for the drive number)",
            "a tag \"{tag}\" não pode ser só números (seria confundida com o número do disco)"
        ));
    }
    Ok(tag)
}

/// Tags of a drive, with the last identification seen (to list disconnected drives).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaggedDisk {
    pub tags: Vec<String>,
    pub model: String,
    pub serial_masked: String,
    pub size_bytes: u64,
    pub updated_at: DateTime<Utc>,
}

/// Content of `tags.json`: fingerprint → tags.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TagBook {
    pub version: u32,
    #[serde(default)]
    pub disks: BTreeMap<String, TaggedDisk>,
}

impl Default for TagBook {
    fn default() -> Self {
        Self { version: FORMAT_VERSION, disks: BTreeMap::new() }
    }
}

impl TagBook {
    pub fn tags_for(&self, fingerprint: &str) -> &[String] {
        self.disks.get(fingerprint).map(|d| d.tags.as_slice()).unwrap_or(&[])
    }

    /// Adds already normalized tags. A tag that exists with another spelling takes the new
    /// spelling (lets the user fix capitalization). Returns the tags that were new.
    pub fn add(&mut self, disk: &DiskInfo, tags: &[String]) -> Result<Vec<String>> {
        let mut list = self.tags_for(&disk.fingerprint).to_vec();
        let mut added = Vec::new();
        for t in tags {
            match list.iter_mut().find(|e| same_tag(e, t)) {
                Some(existing) => *existing = t.clone(),
                None => {
                    list.push(t.clone());
                    added.push(t.clone());
                }
            }
        }
        if list.len() > MAX_TAGS_PER_DISK {
            bail!(tr!(
                "a drive can have at most {MAX_TAGS_PER_DISK} tags (it would have {})",
                "um disco pode ter no máximo {MAX_TAGS_PER_DISK} tags (ficaria com {})",
                list.len()
            ));
        }
        self.disks.insert(
            disk.fingerprint.clone(),
            TaggedDisk {
                tags: list,
                model: disk.model.clone(),
                serial_masked: disk.serial_masked.clone(),
                size_bytes: disk.size_bytes,
                updated_at: Utc::now(),
            },
        );
        Ok(added)
    }

    /// Removes the given tags (case-insensitive). Returns the removed ones. The drive leaves
    /// the file when it has no tags left.
    pub fn remove(&mut self, fingerprint: &str, tags: &[String]) -> Vec<String> {
        let Some(entry) = self.disks.get_mut(fingerprint) else { return vec![] };
        let mut removed = Vec::new();
        entry.tags.retain(|t| {
            let hit = tags.iter().any(|r| same_tag(t, r));
            if hit {
                removed.push(t.clone());
            }
            !hit
        });
        if !removed.is_empty() {
            entry.updated_at = Utc::now();
        }
        if entry.tags.is_empty() {
            self.disks.remove(fingerprint);
        }
        removed
    }

    /// Removes every tag of the drive. Returns the removed ones.
    pub fn clear(&mut self, fingerprint: &str) -> Vec<String> {
        self.disks.remove(fingerprint).map(|d| d.tags).unwrap_or_default()
    }

    /// Fills `DiskInfo::tags` of the connected drives.
    pub fn apply(&self, disks: &mut [DiskInfo]) {
        for d in disks {
            d.tags = self.tags_for(&d.fingerprint).to_vec();
        }
    }
}

/// Reads and writes `tags.json` in the data folder.
pub struct TagStore {
    path: PathBuf,
}

impl TagStore {
    pub fn new(data_dir: &Path) -> Self {
        Self { path: data_dir.join(FILE_NAME) }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Missing file = no tags. An unreadable file is an error (never overwritten blindly).
    pub fn load(&self) -> Result<TagBook> {
        let text = match fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(TagBook::default()),
            Err(e) => return Err(e).with_context(|| tr!("could not read {}", "não foi possível ler {}", self.path.display())),
        };
        let book: TagBook = serde_json::from_str(&text).with_context(|| {
            tr!(
                "{} is corrupted; fix or delete the file to start the tags over",
                "{} está corrompido; corrija ou apague o arquivo para recomeçar as tags",
                self.path.display()
            )
        })?;
        if book.version > FORMAT_VERSION {
            bail!(tr!("{} was written by a newer diskdoctor version", "{} foi gravado por uma versão mais nova do diskdoctor", self.path.display()));
        }
        Ok(book)
    }

    /// Writes to a temporary file and renames it, so `tags.json` is never left half-written.
    pub fn save(&self, book: &TagBook) -> Result<()> {
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir).with_context(|| tr!("could not create {}", "não foi possível criar {}", dir.display()))?;
        }
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, serde_json::to_string_pretty(book)?).with_context(|| tr!("could not write {}", "não foi possível gravar {}", tmp.display()))?;
        fs::rename(&tmp, &self.path).with_context(|| tr!("could not write {}", "não foi possível gravar {}", self.path.display()))?;
        Ok(())
    }
}

/// How the user picks a drive in `--disk`: by number or by tag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiskRef {
    Number(u32),
    Tag(String),
}

impl FromStr for DiskRef {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let t = s.trim();
        if t.is_empty() {
            return Err(crate::i18n::t("give the drive number or a tag", "informe o número do disco ou uma tag").into());
        }
        if t.chars().all(|c| c.is_ascii_digit()) {
            return t.parse().map(Self::Number).map_err(|_| tr!("invalid drive number: {t}", "número de disco inválido: {t}"));
        }
        Ok(Self::Tag(t.to_string()))
    }
}

impl fmt::Display for DiskRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Number(n) => write!(f, "{n}"),
            Self::Tag(t) => write!(f, "\"{t}\""),
        }
    }
}

impl DiskRef {
    /// Finds the connected drive. A tag must be on exactly one drive.
    pub fn resolve<'a>(&self, disks: &'a [DiskInfo]) -> Result<&'a DiskInfo> {
        match self {
            Self::Number(n) => disks.iter().find(|d| d.number == *n).ok_or_else(|| {
                let available = disks.iter().map(|d| d.number.to_string()).collect::<Vec<_>>().join(", ");
                anyhow::anyhow!(tr!(
                    "drive {n} not found. Available drives: {available} (see `diskdoctor list`)",
                    "disco {n} não encontrado. Discos disponíveis: {available} (veja `diskdoctor list`)"
                ))
            }),
            Self::Tag(tag) => {
                let matches: Vec<&DiskInfo> = disks.iter().filter(|d| d.tags.iter().any(|t| same_tag(t, tag))).collect();
                match matches.as_slice() {
                    [one] => Ok(one),
                    [] => {
                        let known: Vec<String> = disks
                            .iter()
                            .flat_map(|d| d.tags.iter().map(move |t| tr!("{t} (drive {})", "{t} (disco {})", d.number)))
                            .collect();
                        if known.is_empty() {
                            bail!(tr!(
                                "no drive has the tag \"{tag}\" (no tags defined). Create it with `diskdoctor tag add --disk N {tag}`",
                                "nenhum disco tem a tag \"{tag}\" (nenhuma tag definida). Crie com `diskdoctor tag add --disk N {tag}`"
                            ))
                        }
                        bail!(tr!(
                            "no connected drive has the tag \"{tag}\". Existing tags: {}",
                            "nenhum disco conectado tem a tag \"{tag}\". Tags existentes: {}",
                            known.join(", ")
                        ))
                    }
                    many => {
                        let numbers = many.iter().map(|d| d.number.to_string()).collect::<Vec<_>>().join(", ");
                        bail!(tr!(
                            "the tag \"{tag}\" is on more than one drive ({numbers}). Use the drive number or a unique tag",
                            "a tag \"{tag}\" está em mais de um disco ({numbers}). Use o número do disco ou uma tag exclusiva"
                        ))
                    }
                }
            }
        }
    }
}

/// Tag ready to paste on the command line (quoted when it has a space).
pub fn quote_for_cli(tag: &str) -> String {
    if tag.contains(' ') { format!("\"{tag}\"") } else { tag.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inventory::tests::fixture_disks;

    fn tags(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn normalizes_and_validates_tags() {
        assert_eq!(normalize_tag("  Studio  ").unwrap(), "Studio");
        assert_eq!(normalize_tag("External   HDD").unwrap(), "External HDD");
        assert_eq!(normalize_tag("Músicas_2024.v2").unwrap(), "Músicas_2024.v2");
        assert_eq!(normalize_tag("Win-11").unwrap(), "Win-11");
        assert!(normalize_tag("   ").is_err());
        assert!(normalize_tag("2").is_err(), "digits only would be mistaken for the drive number");
        assert!(normalize_tag("-x").is_err());
        assert!(normalize_tag("a,b").is_err());
        assert!(normalize_tag("games/steam").is_err());
        assert!(normalize_tag(&"x".repeat(MAX_TAG_CHARS)).is_ok());
        assert!(normalize_tag(&"x".repeat(MAX_TAG_CHARS + 1)).is_err());
    }

    #[test]
    fn adds_without_duplicates_and_updates_spelling() {
        let disks = fixture_disks();
        let d2 = &disks[2];
        let mut book = TagBook::default();
        assert_eq!(book.add(d2, &tags(&["studio", "Windows"])).unwrap(), tags(&["studio", "Windows"]));
        // Same tag with another spelling: no duplicate, only the spelling changes.
        assert_eq!(book.add(d2, &tags(&["Studio", "STUDIO"])).unwrap(), Vec::<String>::new());
        assert_eq!(book.tags_for(&d2.fingerprint), tags(&["STUDIO", "Windows"]).as_slice());
        assert_eq!(book.disks[&d2.fingerprint].model, d2.model);
        assert!(book.tags_for(&disks[0].fingerprint).is_empty());
    }

    #[test]
    fn limits_tags_per_disk_without_partial_changes() {
        let disks = fixture_disks();
        let mut book = TagBook::default();
        let many: Vec<String> = (0..MAX_TAGS_PER_DISK).map(|i| format!("t{i}")).collect();
        book.add(&disks[0], &many).unwrap();
        assert!(book.add(&disks[0], &tags(&["extra"])).is_err());
        assert_eq!(book.tags_for(&disks[0].fingerprint).len(), MAX_TAGS_PER_DISK);
    }

    #[test]
    fn removes_tags_and_drops_empty_entries() {
        let disks = fixture_disks();
        let fp = disks[1].fingerprint.clone();
        let mut book = TagBook::default();
        book.add(&disks[1], &tags(&["Games", "Windows"])).unwrap();
        assert_eq!(book.remove(&fp, &tags(&["windows", "missing"])), tags(&["Windows"]));
        assert_eq!(book.tags_for(&fp), tags(&["Games"]).as_slice());
        assert_eq!(book.clear(&fp), tags(&["Games"]));
        assert!(!book.disks.contains_key(&fp));
        assert!(book.remove(&fp, &tags(&["Games"])).is_empty());
    }

    #[test]
    fn store_round_trip_and_missing_or_corrupt_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = TagStore::new(dir.path());
        assert_eq!(store.load().unwrap(), TagBook::default(), "no file = no tags");

        let disks = fixture_disks();
        let mut book = TagBook::default();
        book.add(&disks[2], &tags(&["Studio"])).unwrap();
        store.save(&book).unwrap();
        assert_eq!(store.load().unwrap(), book);
        assert!(!store.path().with_extension("json.tmp").exists());

        fs::write(store.path(), "{ broken").unwrap();
        let _en = crate::i18n::scoped(crate::i18n::Lang::En);
        let err = format!("{:#}", store.load().unwrap_err());
        assert!(err.contains("corrupted"), "{err}");
    }

    #[test]
    fn parses_disk_refs() {
        assert_eq!("2".parse::<DiskRef>().unwrap(), DiskRef::Number(2));
        assert_eq!(" 10 ".parse::<DiskRef>().unwrap(), DiskRef::Number(10));
        assert_eq!("Studio".parse::<DiskRef>().unwrap(), DiskRef::Tag("Studio".into()));
        assert_eq!("2a".parse::<DiskRef>().unwrap(), DiskRef::Tag("2a".into()));
        assert!("".parse::<DiskRef>().is_err());
        assert!("99999999999".parse::<DiskRef>().is_err());
    }

    #[test]
    fn resolves_disks_by_number_or_unique_tag() {
        let _en = crate::i18n::scoped(crate::i18n::Lang::En);
        let mut disks = fixture_disks();
        let mut book = TagBook::default();
        book.add(&disks[2], &tags(&["Studio", "Windows"])).unwrap();
        book.add(&disks[1], &tags(&["Games", "Windows"])).unwrap();
        book.apply(&mut disks);

        assert_eq!(DiskRef::Number(0).resolve(&disks).unwrap().number, 0);
        assert_eq!(DiskRef::Tag("studio".into()).resolve(&disks).unwrap().number, 2);
        assert_eq!(DiskRef::Tag("GAMES".into()).resolve(&disks).unwrap().number, 1);

        let missing = DiskRef::Number(7).resolve(&disks).unwrap_err().to_string();
        assert!(missing.contains("0, 1, 2"), "{missing}");
        let unknown = DiskRef::Tag("Backup".into()).resolve(&disks).unwrap_err().to_string();
        assert!(unknown.contains("Studio (drive 2)"), "{unknown}");
        let ambiguous = DiskRef::Tag("windows".into()).resolve(&disks).unwrap_err().to_string();
        assert!(ambiguous.contains("more than one drive (1, 2)"), "{ambiguous}");
    }

    #[test]
    fn titles_show_tags() {
        let _en = crate::i18n::scoped(crate::i18n::Lang::En);
        let mut disks = fixture_disks();
        let mut book = TagBook::default();
        book.add(&disks[2], &tags(&["Studio"])).unwrap();
        book.apply(&mut disks);
        assert_eq!(disks[2].title(), "Drive 2 [Studio] · KINGSTON SA400S37120G");
        assert_eq!(disks[2].identity().title(), disks[2].title());
        assert!(!disks[0].title().contains('['));
        let _pt = crate::i18n::scoped(crate::i18n::Lang::PtBr);
        assert_eq!(disks[2].title(), "Disco 2 [Studio] · KINGSTON SA400S37120G");
    }
}
