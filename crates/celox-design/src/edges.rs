//! The edge rules of event domains. Each rule is written here once; the
//! scheduler, the derived-clock ticks, assembly and the VPI layer use these.

use crate::DomainKind;

/// The level change of a one-bit event that a domain of `kind` reacts to:
/// `Some(true)` for a rising edge, `Some(false)` for a falling one and
/// `None` for a domain without an edge.
pub const fn polarity(kind: DomainKind) -> Option<bool> {
    match kind {
        DomainKind::ClockPosedge | DomainKind::ResetAsyncHigh => Some(true),
        DomainKind::ClockNegedge | DomainKind::ResetAsyncLow => Some(false),
        DomainKind::Other => None,
    }
}

/// The bit of an edge level that marks an unknown (x or z) value; the value
/// bit then tells x (1) from z (0), as in the state image. An edge level is
/// 0, 1, x (`LEVEL_UNKNOWN | 1`) or z (`LEVEL_UNKNOWN`).
pub const LEVEL_UNKNOWN: u8 = 2;

/// Whether a change between the edge levels `was` and `is` is a rising
/// (`rising`) or a falling edge (IEEE 1800-2023 9.4.2, Table 9-2): a posedge
/// leaves 0 or reaches 1, a negedge leaves 1 or reaches 0, and a change
/// between x and z is neither. With `FOUR_STATE` false the levels are 0 or
/// 1 and this is a rise or a fall.
#[inline]
pub const fn level_edge<const FOUR_STATE: bool>(rising: bool, was: u8, is: u8) -> bool {
    let between_unknowns = FOUR_STATE && was & is & LEVEL_UNKNOWN != 0;
    let (from, to) = if rising { (0, 1) } else { (1, 0) };
    was != to && is != from && !between_unknowns
}

/// Whether a change from edge level `was` to `is` that the scheduler
/// applies to an event signal, or that a process drives, is an active edge
/// of a domain of `kind`. A domain of unknown kind reacts to a rising edge.
///
/// This differs from [`settled_edge`] for domains of unknown kind; the two
/// rules stay separate.
#[inline]
pub const fn scheduled_edge<const FOUR_STATE: bool>(
    kind: Option<DomainKind>,
    was: u8,
    is: u8,
) -> bool {
    match kind {
        Some(DomainKind::ClockNegedge | DomainKind::ResetAsyncLow) => {
            level_edge::<FOUR_STATE>(false, was, is)
        }
        _ => level_edge::<FOUR_STATE>(true, was, is),
    }
}

/// Whether a settled change from edge level `was` to `is` is an active edge
/// of a domain of `kind`. A domain of unknown kind reacts to any change.
#[inline]
pub const fn settled_edge<const FOUR_STATE: bool>(
    kind: Option<DomainKind>,
    was: u8,
    is: u8,
) -> bool {
    match kind {
        Some(DomainKind::ClockPosedge | DomainKind::ResetAsyncHigh) => {
            level_edge::<FOUR_STATE>(true, was, is)
        }
        Some(DomainKind::ClockNegedge | DomainKind::ResetAsyncLow) => {
            level_edge::<FOUR_STATE>(false, was, is)
        }
        _ => was != is,
    }
}

/// The level of its event signal after an active edge of a domain of
/// `kind`.
pub const fn active_level(kind: Option<DomainKind>) -> u8 {
    match kind {
        Some(DomainKind::ClockNegedge | DomainKind::ResetAsyncLow) => 0,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domains_of_unknown_kind_differ_between_the_rules() {
        for kind in [None, Some(DomainKind::Other)] {
            assert!(scheduled_edge::<false>(kind, 0, 1));
            assert!(!scheduled_edge::<false>(kind, 1, 0));
            assert!(settled_edge::<false>(kind, 0, 1));
            assert!(settled_edge::<false>(kind, 1, 0));
        }
    }

    /// IEEE 1800-2023 Table 9-2, rows `from` and columns `to` in the order
    /// 0, 1, x, z: `P` posedge, `N` negedge, `-` no edge.
    #[test]
    fn four_state_levels_follow_table_9_2() {
        const LEVELS: [u8; 4] = [0, 1, LEVEL_UNKNOWN | 1, LEVEL_UNKNOWN];
        const TABLE: [&str; 4] = ["-PPP", "N-NN", "NP--", "NP--"];
        for (from, row) in LEVELS.iter().zip(TABLE) {
            for (to, cell) in LEVELS.iter().zip(row.chars()) {
                for (kind, mark) in [
                    (DomainKind::ClockPosedge, 'P'),
                    (DomainKind::ResetAsyncHigh, 'P'),
                    (DomainKind::ClockNegedge, 'N'),
                    (DomainKind::ResetAsyncLow, 'N'),
                ] {
                    let edge = cell == mark;
                    assert_eq!(settled_edge::<true>(Some(kind), *from, *to), edge);
                    assert_eq!(scheduled_edge::<true>(Some(kind), *from, *to), edge);
                }
            }
        }
    }

    #[test]
    fn edged_domains_agree_between_the_rules() {
        for kind in [
            DomainKind::ClockPosedge,
            DomainKind::ClockNegedge,
            DomainKind::ResetAsyncHigh,
            DomainKind::ResetAsyncLow,
        ] {
            let rising = polarity(kind) == Some(true);
            for (was, is) in [(0, 1), (1, 0), (0, 0), (1, 1)] {
                let edge = if rising {
                    was == 0 && is == 1
                } else {
                    was == 1 && is == 0
                };
                assert_eq!(scheduled_edge::<false>(Some(kind), was, is), edge);
                assert_eq!(settled_edge::<false>(Some(kind), was, is), edge);
                assert_eq!(scheduled_edge::<true>(Some(kind), was, is), edge);
                assert_eq!(settled_edge::<true>(Some(kind), was, is), edge);
            }
            assert_eq!(active_level(Some(kind)), u8::from(rising));
        }
    }
}
