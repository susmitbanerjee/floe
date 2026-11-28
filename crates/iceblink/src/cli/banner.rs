//! The `iceblink serve` banner: the mark, then the word, in half blocks (ADR 0015).
//!
//! Each run of cells has one paint, and a cell never needs two, so only foreground colours
//! are used. Letters keep the terminal's own foreground, "blink" is accent and both i-dots are
//! glint. Colours are 256-colour codes: accent 25, glint 214.

use std::ffi::OsStr;

/// How the banner may look on this stdout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Style {
    /// A terminal that takes colour: the half-block banner.
    Banner,
    /// A pipe, a file, or NO_COLOR: the plain word.
    Plain,
}

impl Style {
    /// The banner on a terminal, unless NO_COLOR is set. An empty NO_COLOR counts as unset,
    /// as no-color.org says.
    pub(crate) fn detect(is_terminal: bool, no_color: Option<&OsStr>) -> Self {
        let no_color = no_color.is_some_and(|value| !value.is_empty());
        if is_terminal && !no_color {
            Self::Banner
        } else {
            Self::Plain
        }
    }
}

/// One colour, or the terminal's default foreground.
#[derive(Clone, Copy)]
enum Paint {
    Ink,
    Accent,
    /// The mark's reflection: accent at 35% in the SVG, faint accent here.
    Reflection,
    Glint,
}

impl Paint {
    fn code(self) -> Option<&'static str> {
        match self {
            Self::Ink => None,
            Self::Accent => Some("\x1b[38;5;25m"),
            Self::Reflection => Some("\x1b[2;38;5;25m"),
            Self::Glint => Some("\x1b[38;5;214m"),
        }
    }
}

const RESET: &str = "\x1b[0m";

use Paint::{Accent, Glint, Ink, Reflection};

/// Four rows of eight pixel rows: the mark (crescent, slab, reflection) in 8 columns, three
/// spaces, then "ice" in ink and "blink" in accent. Every i-dot is a full glint cell above an
/// empty half, so a dot never shares a cell with its stem. 49 columns wide.
const ROWS: [&[(Paint, &str)]; 4] = [
    &[
        (Ink, "           "),
        (Glint, "██"),
        (Ink, "             "),
        (Accent, "██    ██ "),
        (Glint, "██"),
        (Ink, "       "),
        (Accent, "██"),
    ],
    &[
        (Accent, "▄▄▀▀▀▀▄▄"),
        (Ink, "   ▄▄  ▄▄▄▄  ▄▄▄  "),
        (Accent, "██▄▄  ██ ▄▄ ▄▄▄▄  ██ ▄▄"),
    ],
    &[
        (Ink, "▐██████▌   ██ ██    ██▄██ "),
        (Accent, "██ ██ ██ ██ ██ ██ ███▀"),
    ],
    &[
        (Ink, "  "),
        (Reflection, "▄▄▄▄"),
        (Ink, "     ██ ▀█▄▄▄ ▀█▄▄▄ "),
        (Accent, "██▄█▀ ██ ██ ██ ██ ██▀█▄"),
    ],
];

/// The banner in `style`, ending with a newline: four rows and a blank line, or the word.
pub(crate) fn render(style: Style) -> String {
    match style {
        Style::Plain => "iceblink\n".to_owned(),
        Style::Banner => {
            let mut text = String::new();
            for row in ROWS {
                for &(paint, cells) in row {
                    match paint.code() {
                        Some(code) => text.extend([code, cells, RESET]),
                        None => text.push_str(cells),
                    }
                }
                text.push('\n');
            }
            text.push('\n');
            text
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    /// Compares `actual` with `src/cli/snapshots/<name>`, escape codes written as `\e`.
    /// `UPDATE_GOLDEN=1` rewrites the file instead; reviewers read the diff.
    fn assert_snapshot(name: &str, actual: &str) -> Result<(), std::io::Error> {
        let readable = actual.replace('\x1b', "\\e");
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src/cli/snapshots")
            .join(name);
        if std::env::var("UPDATE_GOLDEN").as_deref() == Ok("1") {
            std::fs::write(&path, &readable)?;
        }
        assert_eq!(
            readable,
            std::fs::read_to_string(&path)?,
            "{}",
            path.display()
        );
        Ok(())
    }

    #[test]
    fn banner_style_matches_its_snapshot() -> Result<(), std::io::Error> {
        assert_snapshot("banner.txt", &render(Style::Banner))
    }

    #[test]
    fn plain_style_matches_its_snapshot() -> Result<(), std::io::Error> {
        assert_snapshot("plain.txt", &render(Style::Plain))
    }

    #[test]
    fn the_banner_fits_in_53_columns() {
        for row in ROWS {
            let width: usize = row.iter().map(|(_, cells)| cells.chars().count()).sum();
            assert!(width <= 53, "a row is {width} columns");
        }
    }

    #[test]
    fn plain_output_carries_no_escape_code() {
        assert!(!render(Style::Plain).contains('\x1b'));
    }

    #[test]
    fn only_a_terminal_without_no_color_gets_the_banner() {
        let cases: [(bool, Option<&str>, Style); 6] = [
            (true, None, Style::Banner),
            (true, Some(""), Style::Banner),
            (true, Some("1"), Style::Plain),
            (false, None, Style::Plain),
            (false, Some(""), Style::Plain),
            (false, Some("1"), Style::Plain),
        ];
        for (is_terminal, no_color, want) in cases {
            assert_eq!(
                Style::detect(is_terminal, no_color.map(OsStr::new)),
                want,
                "terminal {is_terminal}, NO_COLOR {no_color:?}"
            );
        }
    }
}
