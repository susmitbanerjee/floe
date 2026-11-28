# 0015. The iceblink serve banner

- Status: Accepted
- Date: 2026-11-28

## Context

The rename brief asks `iceblink serve` to start with a banner: the mark, then "iceblink", in
half-block characters, within 53 columns. Letters keep the terminal's default foreground,
"blink" is accent, and both i-dots are glint, with 256-colour fallbacks accent 25 and glint 214.
It must print the plain word instead when stdout isn't a terminal or NO_COLOR is set.

ADR 0010 kept phase 1 free of colour and asked any later styling to stay plain for pipes and
NO_COLOR.

The brief's true-colour values for accent and glint come from a Brand table that was not part
of the brief, so only the 256-colour values are known.

## Options

1. A banner crate or a terminal-colour crate. A new dependency for four rows of text.
2. The banner as data in `cli/banner.rs`: runs of cells, each with one paint, rendered with
   256-colour SGR codes. `cli/output.rs` decides the style and prints it.

## Decision

Option 2.

- **Detection.** The banner only when `std::io::IsTerminal` says stdout is a terminal and
  NO_COLOR is unset or empty (no-color.org); otherwise the line `iceblink`. `config check`
  prints no banner.
- **Drawing.** Eight pixel rows in four terminal rows, 49 columns: the mark in 8 columns
  (crescent and reflection in accent, slab in the default foreground), three spaces, then the
  word. Each i-dot is a full glint cell above an empty half, so no cell needs two colours and
  only foreground codes are used. The reflection is faint accent, standing in for 35% opacity.
- **Colours.** `38;5;25` for accent and `38;5;214` for glint, reset after each run.
- **Tests.** Snapshot files in `crates/iceblink/src/cli/snapshots/` hold both outputs, escape
  codes written as `\e`; `UPDATE_GOLDEN=1` rewrites them. A table test covers detection, and
  a CLI test checks that piped `serve` prints the plain word.

## Consequences

- Log lines, help and failures stay plain, as ADR 0010 says.
- True colour waits for the Brand table's accent and glint values. Adding them is one more
  code per paint, chosen when COLORTERM says the terminal supports it.
- Scripts that read `serve`'s stdout see one extra first line, `iceblink`; the lab workflow
  greps for `^ready`, so it is unaffected.
