//! Keyboard-driven lists and prompts for the interactive mode.
//!
//! Flicker-free by construction:
//! - each frame is built as one string and written with a single `write` + flush;
//! - redraws move the cursor up over the previous frame and clear to the end of the
//!   screen, so the content above (results, drive panel) is never touched;
//! - every line is cut to the terminal width, so nothing wraps and the cursor math stays
//!   exact. The size is read again before every frame: after the window is resized, the
//!   rows the terminal re-wrapped are accounted for and the next frame uses the new width;
//! - lists taller than the window scroll inside a viewport;
//! - the cursor is hidden while a list is shown and always shown again when it closes
//!   (the Ctrl+C handler restores it too).

use crate::i18n::t;
use console::{Key, Term, measure_text_width, style, truncate_str};
use std::io::Write;

const HIDE_CURSOR: &str = "\x1b[?25l";
const SHOW_CURSOR: &str = "\x1b[?25h";
/// Left margin of every line.
const MARGIN: &str = "  ";

/// How long an action takes, shown next to it. Always an estimate, never a promise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Speed {
    Instant,
    Quick,
    Long,
}

impl Speed {
    pub fn label(self) -> &'static str {
        match self {
            Self::Instant => t("Instant", "Imediato"),
            Self::Quick => t("Quick", "Rápido"),
            Self::Long => t("Long", "Longo"),
        }
    }

    fn paint(self, text: &str) -> String {
        match self {
            Self::Instant => style(text).cyan().to_string(),
            Self::Quick => style(text).green().to_string(),
            Self::Long => style(text).yellow().bold().to_string(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct Item {
    /// Shortcut key (`1`–`9`, `0`, `q`…): pressing it selects the item at once.
    pub key: Option<char>,
    pub label: String,
    pub speed: Option<Speed>,
    /// Approximate duration ("~30 s", "minutes to hours").
    pub estimate: String,
    /// One-line description shown under the list while the item is highlighted.
    pub hint: String,
}

impl Item {
    pub fn new(label: impl Into<String>) -> Self {
        Self { label: label.into(), ..Default::default() }
    }

    pub fn key(mut self, key: char) -> Self {
        self.key = Some(key);
        self
    }

    pub fn speed(mut self, speed: Speed, estimate: impl Into<String>) -> Self {
        self.speed = Some(speed);
        self.estimate = estimate.into();
        self
    }

    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = hint.into();
        self
    }
}

#[derive(Debug, Clone)]
pub enum Row {
    /// Section title; never selectable.
    Header(String),
    Item(Item),
}

/// Plain-text width the item labels get, given the terminal width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Columns {
    /// `❯ 1  Label…   Quick  ~30 s`
    Full { label: usize, speed: usize },
    /// Estimates hidden: `❯ 1  Label…   Quick`
    NoEstimate { label: usize, speed: usize },
    /// Very narrow: the speed goes first so it is never cut: `❯ 1 Quick Label…`
    Compact,
}

const POINTER_W: usize = 2; // "❯ "
const KEY_W: usize = 3; // "1  "
const GAP: usize = 2;

fn columns(items: &[&Item], width: usize) -> Columns {
    let label = items.iter().map(|i| measure_text_width(&i.label)).max().unwrap_or(0);
    let speed = items.iter().filter_map(|i| i.speed).map(|s| measure_text_width(s.label())).max().unwrap_or(0);
    let estimate = items.iter().map(|i| measure_text_width(&i.estimate)).max().unwrap_or(0);
    let fixed = MARGIN.len() + POINTER_W + KEY_W;
    let speed_block = if speed > 0 { GAP + speed } else { 0 };
    let usable = width.saturating_sub(1); // never touch the last column (avoids auto-wrap)
    if fixed + label + speed_block + 1 + estimate <= usable {
        return Columns::Full { label, speed };
    }
    if fixed + label + speed_block <= usable {
        return Columns::NoEstimate { label, speed };
    }
    let min_label = 8;
    if fixed + min_label + speed_block <= usable {
        return Columns::NoEstimate { label: usable - fixed - speed_block, speed };
    }
    Columns::Compact
}

fn fit(text: &str, width: usize) -> String {
    truncate_str(text, width, "…").into_owned()
}

fn pad(text: &str, width: usize) -> String {
    let w = measure_text_width(text);
    format!("{text}{}", " ".repeat(width.saturating_sub(w)))
}

fn item_line(item: &Item, selected: bool, cols: Columns, width: usize) -> String {
    let usable = width.saturating_sub(1).max(1);
    let pointer = if selected { style("❯").cyan().bold().to_string() } else { " ".into() };
    let key = item.key.map(|k| k.to_string()).unwrap_or_default();
    let key = if selected { style(pad(&key, KEY_W)).cyan().to_string() } else { style(pad(&key, KEY_W)).dim().to_string() };
    let paint_label = |s: &str| if selected { style(s).cyan().bold().to_string() } else { s.to_string() };
    let speed_text = |w: usize| item.speed.map(|s| s.paint(&pad(s.label(), w))).unwrap_or_else(|| " ".repeat(w));
    let line = match cols {
        Columns::Full { label, speed } => {
            let mut l = format!("{MARGIN}{pointer} {key}{}", paint_label(&pad(&fit(&item.label, label), label)));
            if speed > 0 {
                l.push_str(&" ".repeat(GAP));
                l.push_str(&speed_text(speed));
                if !item.estimate.is_empty() {
                    l.push(' ');
                    l.push_str(&style(&item.estimate).dim().to_string());
                }
            }
            l
        }
        Columns::NoEstimate { label, speed } => {
            let mut l = format!("{MARGIN}{pointer} {key}{}", paint_label(&pad(&fit(&item.label, label), label)));
            if speed > 0 {
                l.push_str(&" ".repeat(GAP));
                l.push_str(&speed_text(speed));
            }
            l
        }
        Columns::Compact => {
            let speed = item.speed.map(|s| format!("{} ", s.paint(s.label()))).unwrap_or_default();
            format!("{pointer}{} {speed}{}", item.key.map(|k| k.to_string()).unwrap_or_default(), paint_label(&item.label))
        }
    };
    fit(&line, usable)
}

/// Keeps the selected row inside the visible window; returns `(first_row, visible_rows)`.
fn viewport(rows: usize, selected_row: usize, offset: usize, max_rows: usize) -> (usize, usize) {
    if rows <= max_rows {
        return (0, rows);
    }
    let visible = max_rows.max(1);
    let mut first = offset.min(rows - visible);
    if selected_row < first {
        first = selected_row;
    } else if selected_row >= first + visible {
        first = selected_row + 1 - visible;
    }
    (first, visible)
}

/// What the list looks like: pure function of the state, used for drawing and tests.
pub struct Frame {
    pub lines: Vec<String>,
    /// First row shown (keeps scrolling stable between frames).
    pub offset: usize,
}

pub fn render(prompt: Option<&str>, rows: &[Row], selected: usize, offset: usize, width: usize, height: usize, footer: &str) -> Frame {
    let usable = width.saturating_sub(1).max(1);
    let items: Vec<&Item> = rows
        .iter()
        .filter_map(|r| match r {
            Row::Item(i) => Some(i),
            Row::Header(_) => None,
        })
        .collect();
    let cols = columns(&items, width);
    let selected_row = rows.iter().enumerate().filter(|(_, r)| matches!(r, Row::Item(_))).nth(selected).map(|(i, _)| i).unwrap_or(0);

    let mut lines = Vec::new();
    if let Some(p) = prompt {
        lines.push(fit(&format!("{MARGIN}{} {}", style("?").cyan().bold(), style(p).bold()), usable));
    }
    // Prompt, blank line, hint, footer, the empty line under the frame and one spare row, so
    // the frame always fits on screen and the cursor can move back to its first line.
    let reserved = lines.len() + 5;
    let max_rows = height.saturating_sub(reserved).max(3);
    let needs_scroll = rows.len() > max_rows;
    let (first, visible) = viewport(rows.len(), selected_row, offset, if needs_scroll { max_rows.saturating_sub(2).max(1) } else { max_rows });
    if needs_scroll {
        let above = first;
        lines.push(style(fit(&format!("{MARGIN}  ↑ {}", if above > 0 { tr!("{above} more", "mais {above}") } else { String::new() }), usable)).dim().to_string());
    }
    let mut item_index = rows[..first].iter().filter(|r| matches!(r, Row::Item(_))).count();
    for row in &rows[first..first + visible] {
        match row {
            Row::Header(h) => lines.push(style(fit(&format!("{MARGIN}{}", h.to_uppercase()), usable)).dim().bold().to_string()),
            Row::Item(item) => {
                lines.push(item_line(item, item_index == selected, cols, width));
                item_index += 1;
            }
        }
    }
    if needs_scroll {
        let below = rows.len() - first - visible;
        lines.push(style(fit(&format!("{MARGIN}  ↓ {}", if below > 0 { tr!("{below} more", "mais {below}") } else { String::new() }), usable)).dim().to_string());
    }
    let hint = items.get(selected).map(|i| i.hint.as_str()).unwrap_or("");
    lines.push(String::new());
    lines.push(style(fit(&format!("{MARGIN}{hint}"), usable)).dim().to_string());
    lines.push(style(fit(&format!("{MARGIN}{footer}"), usable)).dim().to_string());
    Frame { lines, offset: first }
}

/// Rows the previous frame occupies now. Lines were cut to the old width, but if the
/// window got narrower the terminal may have re-wrapped them.
fn rows_on_screen(widths: &[usize], width: usize) -> usize {
    let width = width.max(1);
    widths.iter().map(|&w| w.div_ceil(width).max(1)).sum()
}

fn erase(widths: &[usize], width: usize) -> String {
    match rows_on_screen(widths, width) {
        0 => String::new(),
        n => format!("\r\x1b[{n}A\x1b[J"),
    }
}

fn write_frame(text: &str) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(text.as_bytes());
    let _ = out.flush();
}

fn footer_text(rows: &[Row], esc: &str) -> String {
    let keys: Vec<char> = rows
        .iter()
        .filter_map(|r| match r {
            Row::Item(i) => i.key,
            Row::Header(_) => None,
        })
        .filter(|c| c.is_ascii_digit())
        .collect();
    let shortcut = match (keys.iter().min(), keys.iter().max()) {
        (Some(a), Some(b)) if a != b => format!("   {a}–{b} {}", t("shortcut", "atalho")),
        _ => String::new(),
    };
    tr!(
        "↑↓ move   Enter select{shortcut}   Esc {esc}",
        "↑↓ mover   Enter seleciona{shortcut}   Esc {esc}"
    )
}

/// Shows a list and waits for a choice. Returns the index among the items (headers do not
/// count), or `None` for Esc. `back` names what Esc does ("back", "quit").
pub fn select(prompt: Option<&str>, rows: &[Row], default: usize, back: &str) -> Option<usize> {
    let items: Vec<&Item> = rows
        .iter()
        .filter_map(|r| match r {
            Row::Item(i) => Some(i),
            Row::Header(_) => None,
        })
        .collect();
    if items.is_empty() {
        return None;
    }
    crate::platform::enable_virtual_terminal();
    let term = Term::stdout();
    let footer = footer_text(rows, back);
    let n = items.len();
    let mut selected = default.min(n - 1);
    let mut offset = 0;
    let mut drawn: Vec<usize> = Vec::new();
    let mut frame = String::from(HIDE_CURSOR);
    let choice = loop {
        let (height, width) = term.size();
        let (height, width) = (usize::from(height), usize::from(width));
        frame.push_str(&erase(&drawn, width));
        let f = render(prompt, rows, selected, offset, width, height, &footer);
        offset = f.offset;
        drawn = f.lines.iter().map(|l| measure_text_width(l)).collect();
        for line in &f.lines {
            frame.push_str(line);
            frame.push('\n');
        }
        write_frame(&frame);
        frame.clear();
        match term.read_key() {
            Ok(Key::ArrowUp | Key::Char('k') | Key::BackTab) => selected = (selected + n - 1) % n,
            Ok(Key::ArrowDown | Key::Char('j') | Key::Tab) => selected = (selected + 1) % n,
            Ok(Key::Home | Key::PageUp) => selected = 0,
            Ok(Key::End | Key::PageDown) => selected = n - 1,
            Ok(Key::Enter) => break Some(selected),
            Ok(Key::Char(c)) => {
                if let Some(i) = items.iter().position(|it| it.key.is_some_and(|k| k.eq_ignore_ascii_case(&c))) {
                    break Some(i);
                }
                if c == 'q' || c == 'Q' {
                    break None;
                }
            }
            Ok(Key::Escape | Key::Backspace) => break None,
            Ok(_) => {}
            Err(_) => break None,
        }
    };
    // Collapse the list into one line, like a form that was filled in.
    let (_, width) = term.size();
    let width = usize::from(width);
    let mut tail = erase(&drawn, width);
    tail.push_str(SHOW_CURSOR);
    if let (Some(p), Some(i)) = (prompt, choice) {
        let line = format!("{MARGIN}{} {} {}", style("✔").green().bold(), style(p).dim(), style(&items[i].label).cyan());
        tail.push_str(&fit(&line, width.saturating_sub(1).max(1)));
        tail.push('\n');
    }
    write_frame(&tail);
    choice
}

/// Yes/no question answered with one key. Esc counts as "no".
pub fn confirm(prompt: &str, default_yes: bool) -> bool {
    crate::platform::enable_virtual_terminal();
    let term = Term::stdout();
    let choices = match (default_yes, crate::i18n::is_pt()) {
        (true, false) => "[Y/n]",
        (false, false) => "[y/N]",
        (true, true) => "[S/n]",
        (false, true) => "[s/N]",
    };
    write_frame(&format!("{MARGIN}{} {} {} ", style("?").cyan().bold(), style(prompt).bold(), style(choices).dim()));
    let answer = loop {
        match term.read_key() {
            Ok(Key::Enter) => break default_yes,
            Ok(Key::Char('y' | 'Y' | 's' | 'S')) => break true,
            Ok(Key::Char('n' | 'N') | Key::Escape) => break false,
            Ok(_) => {}
            Err(_) => break false,
        }
    };
    let text = if answer { t("Yes", "Sim") } else { t("No", "Não") };
    write_frame(&format!("{}\n", style(text).cyan()));
    answer
}

/// Waits for Enter (or Esc) without echoing what is typed.
pub fn pause(message: &str) {
    let term = Term::stdout();
    write_frame(&format!("{MARGIN}{}", style(message).dim()));
    loop {
        match term.read_key() {
            Ok(Key::Enter | Key::Escape | Key::Char(' ' | 'q')) | Err(_) => break,
            Ok(_) => {}
        }
    }
    write_frame("\r\x1b[2K");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<Row> {
        vec![
            Row::Header("Diagnostics".into()),
            Row::Item(Item::new("SMART & Drive Health").key('1').speed(Speed::Quick, "~30 s").hint("SMART attributes and events")),
            Row::Item(Item::new("Surface Scan (read-only)").key('2').speed(Speed::Long, "minutes to hours").hint("Reads every sector")),
            Row::Header("Tools".into()),
            Row::Item(Item::new("Exit").key('q').speed(Speed::Instant, "")),
        ]
    }

    fn plain(lines: &[String]) -> Vec<String> {
        lines.iter().map(|l| console::strip_ansi_codes(l).into_owned()).collect()
    }

    #[test]
    fn lines_never_reach_the_last_column_and_speed_stays_visible() {
        let _en = crate::i18n::scoped(crate::i18n::Lang::En);
        for width in [12, 20, 32, 40, 60, 80, 120] {
            let f = render(Some("Select an action"), &rows(), 1, 0, width, 30, "↑↓ move   Enter select   Esc quit");
            for line in &f.lines {
                assert!(measure_text_width(line) < width, "width {width}: {line:?}");
            }
            let text = plain(&f.lines).join("\n");
            if width >= 20 {
                assert!(text.contains("Quick") && text.contains("Long") && text.contains("Instant"), "width {width}:\n{text}");
            }
            if width >= 60 {
                assert!(text.contains("minutes to hours"), "estimates shown when there is room:\n{text}");
            }
        }
    }

    #[test]
    fn marks_the_selected_item_and_shows_its_hint() {
        let f = render(None, &rows(), 1, 0, 100, 30, "footer");
        let text = plain(&f.lines);
        let selected: Vec<&String> = text.iter().filter(|l| l.contains('❯')).collect();
        assert_eq!(selected.len(), 1);
        assert!(selected[0].contains("Surface Scan"));
        assert!(text.iter().any(|l| l.trim() == "Reads every sector"));
        assert!(text.iter().any(|l| l.trim() == "DIAGNOSTICS"), "headers are shown in upper case");
    }

    #[test]
    fn long_lists_scroll_and_keep_the_selection_visible() {
        let many: Vec<Row> = (0..40).map(|i| Row::Item(Item::new(format!("Drive {i}")))).collect();
        let height = 12;
        let f = render(Some("Select drive"), &many, 30, 0, 80, height, "footer");
        assert!(f.lines.len() < height, "{} lines for height {height}", f.lines.len());
        let text = plain(&f.lines).join("\n");
        assert!(text.contains("Drive 30") && text.contains('❯'));
        assert!(text.contains("↑") && text.contains("↓"));
        // Moving back up scrolls back.
        let g = render(Some("Select drive"), &many, 0, f.offset, 80, height, "footer");
        assert_eq!(g.offset, 0);
        assert!(plain(&g.lines).join("\n").contains("Drive 0"));
    }

    #[test]
    fn redraw_accounts_for_rewrapped_lines_after_a_resize() {
        assert_eq!(rows_on_screen(&[10, 0, 79], 80), 3);
        // The window shrank from 80 to 40 columns: a 79-column line now takes 2 rows.
        assert_eq!(rows_on_screen(&[10, 0, 79], 40), 4);
        assert_eq!(erase(&[], 80), "");
        assert_eq!(erase(&[5, 5], 80), "\r\x1b[2A\x1b[J");
    }

    #[test]
    fn footer_lists_shortcut_range() {
        let _en = crate::i18n::scoped(crate::i18n::Lang::En);
        let rows: Vec<Row> = (1..=9).map(|i| Row::Item(Item::new("x").key(char::from(b'0' + i)))).collect();
        assert!(footer_text(&rows, "quit").contains("1–9 shortcut"));
        // The main menu uses 1–9 and then 0: the range shown is 0–9, not "1–0".
        let mut with_zero = rows.clone();
        with_zero.push(Row::Item(Item::new("language").key('0')));
        assert!(footer_text(&with_zero, "quit").contains("0–9 shortcut"));
        let _pt = crate::i18n::scoped(crate::i18n::Lang::PtBr);
        assert!(footer_text(&rows, "sair").contains("1–9 atalho"));
    }
}
