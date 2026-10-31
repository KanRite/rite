//! Zooming the desktop board (ticket 024): Ctrl + and Ctrl - step through
//! these sizes, Ctrl 0 goes back to 100%. The size is remembered on this
//! machine (`ui.zoom` in KanRite's own config), not per project.

/// The sizes Ctrl + and Ctrl - step through, in percent.
pub const STEPS: [u32; 11] = [50, 67, 75, 80, 90, 100, 110, 125, 150, 175, 200];

/// The size when nothing has been chosen.
pub const DEFAULT: u32 = 100;

/// `percent` as a size the board uses: the nearest step, so a hand-edited
/// or out-of-range value can't make the board unusable.
pub fn normalise(percent: u32) -> u32 {
    STEPS
        .iter()
        .copied()
        .min_by_key(|step| step.abs_diff(percent))
        .unwrap_or(DEFAULT)
}

/// One step larger (`bigger`) or smaller than `current`, stopping at the
/// ends.
pub fn step(current: u32, bigger: bool) -> u32 {
    let current = normalise(current);
    let at = STEPS.iter().position(|&s| s == current).unwrap_or(5);
    let next = if bigger {
        (at + 1).min(STEPS.len() - 1)
    } else {
        at.saturating_sub(1)
    };
    STEPS[next]
}

/// Reads a saved size; anything unreadable is the default.
pub fn parse(saved: &str) -> u32 {
    saved
        .trim()
        .trim_end_matches('%')
        .parse()
        .map(normalise)
        .unwrap_or(DEFAULT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_stop_at_the_ends() {
        assert_eq!(step(100, true), 110);
        assert_eq!(step(100, false), 90);
        assert_eq!(step(200, true), 200);
        assert_eq!(step(50, false), 50);
    }

    #[test]
    fn odd_values_snap_to_a_step() {
        assert_eq!(normalise(103), 100);
        assert_eq!(normalise(1000), 200);
        assert_eq!(normalise(0), 50);
        assert_eq!(step(103, true), 110);
    }

    #[test]
    fn saved_values_are_read_forgivingly() {
        assert_eq!(parse("125"), 125);
        assert_eq!(parse(" 150% "), 150);
        assert_eq!(parse(""), DEFAULT);
        assert_eq!(parse("big"), DEFAULT);
    }
}
