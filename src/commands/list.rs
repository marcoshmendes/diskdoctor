//! `diskdoctor list`

use super::{Ctx, load_disks, print_json};
use crate::app::exit;
use crate::ui;
use anyhow::Result;

pub fn run(ctx: &Ctx) -> Result<u8> {
    let disks = load_disks(ctx)?;
    if ctx.json {
        print_json(&disks)?;
        return Ok(exit::OK);
    }
    ui::print_disk_table(&disks);
    if !ctx.elevated {
        ui::note("Dica: em um terminal de Administrador, `diskdoctor health` lê o SMART completo.");
    }
    Ok(exit::OK)
}
