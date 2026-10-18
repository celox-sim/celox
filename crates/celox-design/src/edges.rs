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

/// Whether a level change from `was` to `is` that the scheduler applies to
/// an event signal, or that a process drives, is an active edge of a domain
/// of `kind`. A domain of unknown kind reacts to a rising edge.
///
/// This differs from [`settled_edge`] for domains of unknown kind; the two
/// rules stay separate.
pub const fn scheduled_edge(kind: Option<DomainKind>, was: bool, is: bool) -> bool {
    match kind {
        Some(DomainKind::ClockPosedge | DomainKind::ResetAsyncHigh) => !was && is,
        Some(DomainKind::ClockNegedge | DomainKind::ResetAsyncLow) => was && !is,
        _ => !was && is,
    }
}

/// Whether a settled level change from `was` to `is` is an active edge of a
/// domain of `kind`. A domain of unknown kind reacts to any change.
pub const fn settled_edge(kind: Option<DomainKind>, was: bool, is: bool) -> bool {
    match kind {
        Some(DomainKind::ClockPosedge | DomainKind::ResetAsyncHigh) => !was && is,
        Some(DomainKind::ClockNegedge | DomainKind::ResetAsyncLow) => was && !is,
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
            assert!(scheduled_edge(kind, false, true));
            assert!(!scheduled_edge(kind, true, false));
            assert!(settled_edge(kind, false, true));
            assert!(settled_edge(kind, true, false));
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
            for (was, is) in [(false, true), (true, false), (false, false), (true, true)] {
                let edge = if rising { !was && is } else { was && !is };
                assert_eq!(scheduled_edge(Some(kind), was, is), edge);
                assert_eq!(settled_edge(Some(kind), was, is), edge);
            }
            assert_eq!(active_level(Some(kind)), u8::from(rising));
        }
    }
}
