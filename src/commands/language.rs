//! `diskdoctor language [en|pt-BR]`: shows the interface language or saves a new default.
//!
//! Writes only `settings.json` in the diskdoctor data folder (see [`crate::i18n`]).

use super::{Ctx, print_json};
use crate::app::exit;
use crate::cli::LanguageArgs;
use crate::i18n::{self, Lang, Source};
use crate::ui;
use anyhow::Result;

pub fn run(ctx: &Ctx, args: &LanguageArgs) -> Result<u8> {
    let Some(lang) = args.lang else {
        return show(ctx);
    };
    let path = i18n::save_language(&ctx.data_dir, lang)?;
    // Applies right away (the interactive menu redraws in the new language).
    i18n::set(lang, Source::Settings);
    let env = std::env::var("DISKDOCTOR_LANG").ok().and_then(|v| Lang::parse(&v)).filter(|e| *e != lang);
    if ctx.json {
        print_json(&serde_json::json!({ "language": lang, "file": path, "overridden_by_environment": env }))?;
        return Ok(exit::OK);
    }
    println!("{} {}", ui::ok_mark(), tr!("Language set to {}.", "Idioma definido: {}.", lang.native_name()));
    ui::note(&tr!("Saved in {}", "Salvo em {}", path.display()));
    if let Some(e) = env {
        ui::warn(&tr!(
            "DISKDOCTOR_LANG={} is set and takes priority over this choice in new sessions.",
            "DISKDOCTOR_LANG={} está definida e tem prioridade sobre esta escolha nas próximas sessões.",
            e.code()
        ));
    }
    Ok(exit::OK)
}

fn show(ctx: &Ctx) -> Result<u8> {
    let lang = i18n::current();
    let source = i18n::source();
    if ctx.json {
        print_json(&serde_json::json!({
            "language": lang,
            "source": source,
            "available": i18n::ALL.iter().map(|l| serde_json::json!({ "code": l.code(), "name": l.native_name() })).collect::<Vec<_>>(),
        }))?;
        return Ok(exit::OK);
    }
    println!("{}", tr!("Interface language: {} ({})", "Idioma da interface: {} ({})", lang.native_name(), source.label()));
    for l in i18n::ALL {
        let mark = if l == lang { ui::ok_mark() } else { " ".to_string() };
        println!("  {mark} {:<6} {}", l.code(), l.native_name());
    }
    ui::note(&tr!(
        "Change it with: diskdoctor language pt-BR   (or for one run: --lang pt-BR)",
        "Altere com: diskdoctor language en   (ou só nesta execução: --lang en)"
    ));
    Ok(exit::OK)
}
