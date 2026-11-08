//! `diskdoctor tag` — apelidos ("Studio", "Jogos") para os discos.
//!
//! Grava só `tags.json` na pasta de dados do diskdoctor (ver [`crate::tags`]).

use super::{Ctx, load_disks, print_json, select_disk};
use crate::app::exit;
use crate::cli::{TagAction, TagAddArgs, TagArgs, TagRemoveArgs};
use crate::inventory::{DiskInfo, disk_title};
use crate::tags::{TagBook, TagStore, normalize_tag, quote_for_cli, same_tag};
use crate::ui;
use crate::util::format_bytes;
use anyhow::{Context, Result};
use comfy_table::Cell;
use serde::Serialize;

pub fn run(ctx: &Ctx, args: &TagArgs) -> Result<u8> {
    match &args.action {
        None | Some(TagAction::List) => list(ctx),
        Some(TagAction::Add(a)) => add(ctx, a),
        Some(TagAction::Remove(a)) => remove(ctx, a),
    }
}

/// Linha da listagem: disco conectado (com ou sem tags) ou disco com tags que não está conectado.
#[derive(Serialize)]
struct TagRow {
    number: Option<u32>,
    connected: bool,
    model: String,
    serial_masked: String,
    fingerprint: String,
    size_bytes: u64,
    volumes: Vec<String>,
    tags: Vec<String>,
}

fn rows(disks: &[DiskInfo], book: &TagBook) -> Vec<TagRow> {
    let mut out: Vec<TagRow> = disks
        .iter()
        .map(|d| TagRow {
            number: Some(d.number),
            connected: true,
            model: d.model.clone(),
            serial_masked: d.serial_masked.clone(),
            fingerprint: d.fingerprint.clone(),
            size_bytes: d.size_bytes,
            volumes: d.volume_letters(),
            tags: book.tags_for(&d.fingerprint).to_vec(),
        })
        .collect();
    for (fp, t) in &book.disks {
        if !disks.iter().any(|d| &d.fingerprint == fp) {
            out.push(TagRow {
                number: None,
                connected: false,
                model: t.model.clone(),
                serial_masked: t.serial_masked.clone(),
                fingerprint: fp.clone(),
                size_bytes: t.size_bytes,
                volumes: vec![],
                tags: t.tags.clone(),
            });
        }
    }
    out
}

/// Tags presentes em mais de um disco conectado (não servem para `--disk`).
fn shared_tags(disks: &[DiskInfo]) -> Vec<(String, Vec<u32>)> {
    let mut out: Vec<(String, Vec<u32>)> = Vec::new();
    for d in disks {
        for t in &d.tags {
            match out.iter_mut().find(|(k, _)| same_tag(k, t)) {
                Some((_, nums)) => nums.push(d.number),
                None => out.push((t.clone(), vec![d.number])),
            }
        }
    }
    out.retain(|(_, nums)| nums.len() > 1);
    out
}

fn warn_shared(disks: &[DiskInfo]) {
    for (tag, nums) in shared_tags(disks) {
        let list = nums.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(", ");
        ui::warn(&format!("a tag \"{tag}\" está nos discos {list}; enquanto for compartilhada, use o número em --disk"));
    }
}

fn list(ctx: &Ctx) -> Result<u8> {
    let store = TagStore::new(&ctx.data_dir);
    let book = store.load()?;
    let disks = load_disks(ctx)?;
    let rows = rows(&disks, &book);
    if ctx.json {
        print_json(&serde_json::json!({ "file": store.path(), "disks": rows }))?;
        return Ok(exit::OK);
    }
    ui::heading("Tags dos discos");
    let mut t = ui::table();
    t.set_header(vec!["#", "Tags", "Modelo", "Capacidade", "Volumes"]);
    for r in &rows {
        let tags = if r.tags.is_empty() { ui::dim("—") } else { ui::tag_list(&r.tags) };
        let model = if r.connected { r.model.clone() } else { format!("{}\n(não conectado)", r.model) };
        t.add_row(vec![
            Cell::new(r.number.map(|n| n.to_string()).unwrap_or("—".into())),
            Cell::new(tags),
            Cell::new(model),
            Cell::new(format_bytes(r.size_bytes)),
            Cell::new(if r.volumes.is_empty() { "—".into() } else { r.volumes.join(" ") }),
        ]);
    }
    println!("{t}");
    warn_shared(&disks);
    if book.disks.is_empty() {
        ui::note("Nenhuma tag ainda. Exemplo: diskdoctor tag add --disk 2 Studio");
    } else if let Some(example) = disks.iter().find_map(|d| d.tags.first()) {
        ui::note(&format!("Use a tag no lugar do número: diskdoctor health --disk {}", quote_for_cli(example)));
    }
    Ok(exit::OK)
}

fn add(ctx: &Ctx, a: &TagAddArgs) -> Result<u8> {
    let tags = a.tags.iter().map(|t| normalize_tag(t)).collect::<Result<Vec<_>>>()?;
    let store = TagStore::new(&ctx.data_dir);
    let mut book = store.load()?;
    let mut disks = load_disks(ctx)?;
    let disk = select_disk(&disks, &a.disk)?.clone();
    let added = book.add(&disk, &tags)?;
    store.save(&book).context("as tags não foram salvas")?;
    book.apply(&mut disks);
    let current = book.tags_for(&disk.fingerprint).to_vec();

    if ctx.json {
        print_json(&serde_json::json!({ "disk": disk.number, "fingerprint": disk.fingerprint, "tags": current, "added": added }))?;
        return Ok(exit::OK);
    }
    let title = disk_title(disk.number, &disk.model, &current);
    if added.is_empty() {
        println!("{} {title} — nenhuma tag nova (já existiam).", ui::ok_mark());
    } else {
        println!("{} {title}", ui::ok_mark());
        println!("  Adicionada{}: {}", if added.len() > 1 { "s" } else { "" }, ui::tag_list(&added));
    }
    warn_shared(&disks);
    ui::note(&format!("Agora você pode usar: diskdoctor health --disk {}", quote_for_cli(&tags[0])));
    Ok(exit::OK)
}

fn remove(ctx: &Ctx, a: &TagRemoveArgs) -> Result<u8> {
    let store = TagStore::new(&ctx.data_dir);
    let mut book = store.load()?;
    let disks = load_disks(ctx)?;
    let disk = select_disk(&disks, &a.disk)?.clone();
    let removed = if a.all { book.clear(&disk.fingerprint) } else { book.remove(&disk.fingerprint, &a.tags) };
    if !removed.is_empty() {
        store.save(&book).context("as tags não foram salvas")?;
    }
    let missing: Vec<&String> = a.tags.iter().filter(|t| !removed.iter().any(|r| same_tag(r, t))).collect();
    let current = book.tags_for(&disk.fingerprint).to_vec();

    if ctx.json {
        print_json(&serde_json::json!({ "disk": disk.number, "fingerprint": disk.fingerprint, "tags": current, "removed": removed }))?;
        return Ok(exit::OK);
    }
    let title = disk_title(disk.number, &disk.model, &current);
    if removed.is_empty() {
        println!("{title} — nada removido.");
    } else {
        println!("{} {title}", ui::ok_mark());
        println!("  Removida{}: {}", if removed.len() > 1 { "s" } else { "" }, ui::tag_list(&removed));
    }
    for m in missing {
        ui::warn(&format!("o disco {} não tinha a tag \"{m}\"", disk.number));
    }
    Ok(exit::OK)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inventory::tests::fixture_disks;

    #[test]
    fn lists_disconnected_tagged_disks_and_shared_tags() {
        let mut disks = fixture_disks();
        let mut book = TagBook::default();
        book.add(&disks[1], &["Windows".to_string()]).unwrap();
        book.add(&disks[2], &["Studio".to_string(), "windows".to_string()]).unwrap();
        let gone = disks.remove(0);
        book.add(&gone, &["Jogos".to_string()]).unwrap();
        book.apply(&mut disks);

        let r = rows(&disks, &book);
        assert_eq!(r.len(), 3);
        let disconnected = r.iter().find(|x| !x.connected).unwrap();
        assert_eq!(disconnected.tags, vec!["Jogos"]);
        assert_eq!(disconnected.number, None);

        let shared = shared_tags(&disks);
        assert_eq!(shared.len(), 1);
        assert_eq!(shared[0].1, vec![1, 2]);
    }
}
