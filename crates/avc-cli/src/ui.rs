//! Terminal presentation: color detection, aligned ASCII tables, and the
//! per-line vocabulary every command shares.
//!
//! Two rules shape everything here. Output stays ASCII, so it survives a log
//! viewer, a CI transcript, and a terminal with no Unicode font. And color is
//! decoration only: every line reads identically once the escape codes are
//! stripped, which is exactly what a pipe, a redirect, `NO_COLOR`, or
//! `--color never` produces. Scripts should still prefer `--porcelain`, which
//! is a contract rather than a layout.

use std::io::IsTerminal;
use std::sync::atomic::{AtomicBool, Ordering};

/// Whether escape codes are written to each stream. Decided once, at startup,
/// because stdout can be a terminal while stderr is a file, and vice versa.
static STDOUT_COLOR: AtomicBool = AtomicBool::new(false);
static STDERR_COLOR: AtomicBool = AtomicBool::new(false);

/// Width of the leading verb column on an action line, sized for the longest
/// verb any command prints. Fixed rather than computed, so lines can be
/// streamed as work finishes instead of buffered until it is done.
const VERB_WIDTH: usize = 13;

/// How much of a digest is shown to a human. Enough to recognize and to grep
/// for; `--porcelain` prints the whole thing.
const SHORT_HASH: usize = 12;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
#[value(rename_all = "lower")]
pub enum ColorChoice {
    /// Color when writing to a terminal that wants it.
    #[default]
    Auto,
    Always,
    Never,
}

/// Resolve the color decision for both streams. Call once, before any output.
pub fn init(choice: ColorChoice) {
    STDOUT_COLOR.store(
        resolve(choice, std::io::stdout().is_terminal()),
        Ordering::Relaxed,
    );
    STDERR_COLOR.store(
        resolve(choice, std::io::stderr().is_terminal()),
        Ordering::Relaxed,
    );
}

/// The de-facto conventions, in the order they win: an explicit choice, then
/// `NO_COLOR` over a terminal, then `CLICOLOR_FORCE` over a pipe — which is how
/// a CI runner that renders ANSI in its log viewer asks for color it would
/// otherwise be denied — then the terminal itself, unless it is `TERM=dumb`.
fn resolve(choice: ColorChoice, is_terminal: bool) -> bool {
    match choice {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto => {
            if flag_set("NO_COLOR") {
                return false;
            }
            if flag_set("CLICOLOR_FORCE") {
                return true;
            }
            is_terminal && std::env::var("TERM").map_or(true, |term| term != "dumb")
        }
    }
}

fn flag_set(name: &str) -> bool {
    std::env::var(name).is_ok_and(|value| !value.is_empty() && value != "0")
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Style {
    #[default]
    Plain,
    Bold,
    Dim,
    /// Something is as it should be.
    Ok,
    /// Something differs, but nothing is lost.
    Warn,
    /// Something is absent or wrong.
    Bad,
    /// A failure, on the way to a non-zero exit code.
    Error,
}

impl Style {
    fn code(self) -> Option<&'static str> {
        match self {
            Style::Plain => None,
            Style::Bold => Some("1"),
            Style::Dim => Some("2"),
            Style::Ok => Some("32"),
            Style::Warn => Some("33"),
            Style::Bad => Some("31"),
            Style::Error => Some("1;31"),
        }
    }
}

/// Style `text` for stdout.
pub fn paint(text: &str, style: Style) -> String {
    wrap(text, style, STDOUT_COLOR.load(Ordering::Relaxed))
}

/// Style `text` for stderr, which may be a terminal when stdout is not.
pub fn paint_err(text: &str, style: Style) -> String {
    wrap(text, style, STDERR_COLOR.load(Ordering::Relaxed))
}

fn wrap(text: &str, style: Style, enabled: bool) -> String {
    match (enabled, style.code()) {
        (true, Some(code)) => format!("\x1b[{code}m{text}\x1b[0m"),
        _ => text.to_owned(),
    }
}

/// A line introducing what a command is about to do.
pub fn heading(text: &str) {
    println!("{}", paint(text, Style::Bold));
}

/// A closing line counting what happened. Preceded by a blank line, so a run of
/// per-artifact lines and its total are visually separate.
pub fn summary(text: &str) {
    println!("\n{}", paint(text, Style::Dim));
}

/// A one-off line with no rows around it — `initialized AVC in …`.
pub fn line(text: &str, style: Style) {
    println!("{}", paint(text, style));
}

/// Advice that is not part of the result: what to run next, or what was left
/// deliberately untouched.
pub fn note(text: &str) {
    println!("{}", paint(&format!("note: {text}"), Style::Dim));
}

/// One thing that happened to one artifact.
///
/// `verb` is padded into a fixed column so a run of these lines aligns without
/// being buffered first — a push of ten artifacts prints each as it lands
/// rather than all of them at the end.
pub fn action(verb: &str, style: Style, subject: &str, detail: Option<&str>) {
    let verb = paint(&format!("{verb:<VERB_WIDTH$}"), style);
    match detail {
        Some(detail) => println!(
            "{verb}{subject} {}",
            paint(&format!("({detail})"), Style::Dim)
        ),
        None => println!("{verb}{subject}"),
    }
}

/// An indented `key   value` pair, for the handful of places that describe a
/// configuration rather than a list of artifacts.
pub fn field(key: &str, value: &str) {
    println!("  {}  {value}", paint(&format!("{key:<9}"), Style::Dim));
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Right,
}

pub struct Column {
    pub title: &'static str,
    pub align: Align,
}

impl Column {
    pub fn left(title: &'static str) -> Self {
        Self {
            title,
            align: Align::Left,
        }
    }

    /// A column of numbers, which only line up when they end in the same place.
    pub fn right(title: &'static str) -> Self {
        Self {
            title,
            align: Align::Right,
        }
    }
}

pub struct Cell {
    text: String,
    style: Style,
}

impl Cell {
    pub fn new(text: impl Into<String>, style: Style) -> Self {
        Self {
            text: text.into(),
            style,
        }
    }

    pub fn plain(text: impl Into<String>) -> Self {
        Self::new(text, Style::Plain)
    }

    pub fn dim(text: impl Into<String>) -> Self {
        Self::new(text, Style::Dim)
    }
}

/// A column-aligned listing.
///
/// Text and style are kept apart until the moment of printing, because a cell
/// padded after it has been wrapped in escape codes is padded to the wrong
/// width — the codes are bytes the terminal never shows.
pub struct Table {
    columns: Vec<Column>,
    rows: Vec<Vec<Cell>>,
}

impl Table {
    pub fn new(columns: Vec<Column>) -> Self {
        Self {
            columns,
            rows: Vec::new(),
        }
    }

    pub fn row(&mut self, cells: Vec<Cell>) {
        debug_assert_eq!(cells.len(), self.columns.len());
        self.rows.push(cells);
    }

    pub fn print(&self) {
        if self.rows.is_empty() {
            return;
        }
        let widths: Vec<usize> = self
            .columns
            .iter()
            .enumerate()
            .map(|(index, column)| {
                self.rows
                    .iter()
                    .map(|row| display_width(&row[index].text))
                    .chain(std::iter::once(display_width(column.title)))
                    .max()
                    .unwrap_or(0)
            })
            .collect();

        let header: Vec<String> = self
            .columns
            .iter()
            .enumerate()
            .map(|(index, column)| pad(column.title, widths[index], column.align))
            .collect();
        println!("{}", paint(header.join("  ").trim_end(), Style::Bold));

        for row in &self.rows {
            let line: Vec<String> = row
                .iter()
                .enumerate()
                .map(|(index, cell)| {
                    paint(
                        &pad(&cell.text, widths[index], self.columns[index].align),
                        cell.style,
                    )
                })
                .collect();
            // Trailing padding on the last column is invisible but still
            // widens a copied line, so it goes.
            println!("{}", line.join("  ").trim_end());
        }
    }
}

fn pad(text: &str, width: usize, align: Align) -> String {
    let padding = " ".repeat(width.saturating_sub(display_width(text)));
    match align {
        Align::Left => format!("{text}{padding}"),
        Align::Right => format!("{padding}{text}"),
    }
}

/// Columns are laid out in characters rather than bytes, so a path in Japanese
/// or Portuguese lines up with an ASCII one. Double-width glyphs still drift;
/// correcting that needs a Unicode width table, which is not worth a dependency
/// here.
fn display_width(text: &str) -> usize {
    text.chars().count()
}

/// Byte counts for humans, so a size is readable at a glance.
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[0])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// A count with its noun, pluralized — `1 artifact`, `3 artifacts`.
pub fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("{count} {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

/// The leading characters of a digest, which is what a human compares.
pub fn short_hash(hash: &str) -> String {
    hash.chars().take(SHORT_HASH).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_follows_the_environment_conventions() {
        assert!(resolve(ColorChoice::Always, false));
        assert!(!resolve(ColorChoice::Never, true));
        // Auto depends on process-wide environment variables, so only the
        // terminal half is safe to assert in a parallel test run.
        assert!(!resolve(ColorChoice::Auto, false) || flag_set("CLICOLOR_FORCE"));
    }

    #[test]
    fn cells_are_padded_by_character_not_by_byte() {
        assert_eq!(pad("ok", 5, Align::Left), "ok   ");
        assert_eq!(pad("17 B", 6, Align::Right), "  17 B");
        // Six characters, ten bytes: padding to eight must add two spaces, not
        // fewer, or a table of Unicode paths shears.
        assert_eq!(pad("模型.bin", 8, Align::Left), "模型.bin  ");
    }

    #[test]
    fn sizes_and_counts_read_as_english() {
        assert_eq!(size(0), "0 B");
        assert_eq!(size(1023), "1023 B");
        assert_eq!(size(1024), "1.0 KiB");
        assert_eq!(size(4 * 1024 * 1024 * 1024), "4.0 GiB");
        assert_eq!(plural(1, "artifact"), "1 artifact");
        assert_eq!(plural(0, "object"), "0 objects");
    }

    #[test]
    fn plain_style_never_emits_escape_codes() {
        assert_eq!(wrap("text", Style::Plain, true), "text");
        assert_eq!(wrap("text", Style::Ok, false), "text");
        assert_eq!(wrap("text", Style::Ok, true), "\x1b[32mtext\x1b[0m");
    }
}
