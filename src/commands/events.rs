//! `diskdoctor events`

use super::{Ctx, apply_saved_tags, print_json, select_disk};
use crate::app::exit;
use crate::cli::EventsArgs;
use crate::events::collect_events;
use crate::tags::DiskRef;
use crate::{inventory, ui};
use anyhow::Result;

pub fn run(ctx: &Ctx, args: &EventsArgs) -> Result<u8> {
    let spinner = (!ctx.json).then(|| ui::spinner(&format!("Consultando eventos do Windows (últimas {} h)…", args.hours)));
    // Sem inventário ainda dá para listar eventos, só sem atribuição por volume.
    let mut disks = inventory::load_inventory().unwrap_or_else(|e| {
        ui::warn(&format!("inventário indisponível ({e:#}); eventos ficarão sem disco associado quando depender de volume"));
        vec![]
    });
    apply_saved_tags(ctx, &mut disks);
    let filter = match &args.disk {
        // Sem inventário, um número ainda filtra; uma tag precisa do inventário.
        Some(DiskRef::Number(n)) if disks.is_empty() => Some(*n),
        Some(r) => Some(select_disk(&disks, r)?.number),
        None => None,
    };
    let events = collect_events(args.hours, args.max_events, &disks);
    if let Some(s) = spinner {
        s.finish_and_clear();
    }
    let mut events = events?;
    if let Some(n) = filter {
        events.retain(|e| e.disk_number == Some(n));
    }
    if ctx.json {
        print_json(&events)?;
    } else {
        ui::print_events(&events, args.hours, args.limit);
    }
    Ok(exit::OK)
}
