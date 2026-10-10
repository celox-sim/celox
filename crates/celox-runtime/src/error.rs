#[derive(Debug, Clone, Eq)]
pub enum SimulatorErrorCode {
    DetectedTrueLoop,
    DetectedTrueLoopCode(i64),
    DetectedTrueLoopAt {
        signals: Vec<String>,
    },
    Runtime {
        message: String,
        signals: Vec<String>,
    },
    InternalError,
    NotAnEvent(String),
}

impl PartialEq for SimulatorErrorCode {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::DetectedTrueLoop, Self::DetectedTrueLoop)
            | (Self::DetectedTrueLoop, Self::DetectedTrueLoopCode(_))
            | (Self::DetectedTrueLoop, Self::DetectedTrueLoopAt { .. })
            | (Self::DetectedTrueLoopCode(_), Self::DetectedTrueLoop)
            | (Self::DetectedTrueLoopCode(_), Self::DetectedTrueLoopCode(_))
            | (Self::DetectedTrueLoopCode(_), Self::DetectedTrueLoopAt { .. })
            | (Self::DetectedTrueLoopAt { .. }, Self::DetectedTrueLoopCode(_))
            | (Self::DetectedTrueLoopAt { .. }, Self::DetectedTrueLoop)
            | (Self::DetectedTrueLoopAt { .. }, Self::DetectedTrueLoopAt { .. }) => true,
            (Self::InternalError, Self::InternalError) => true,
            (
                Self::Runtime {
                    message: a,
                    signals: sa,
                },
                Self::Runtime {
                    message: b,
                    signals: sb,
                },
            ) => a == b && sa == sb,
            (Self::NotAnEvent(a), Self::NotAnEvent(b)) => a == b,
            _ => false,
        }
    }
}

impl std::fmt::Display for SimulatorErrorCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DetectedTrueLoop | Self::DetectedTrueLoopCode(_) => {
                write!(f, "Detected True Loop")
            }
            Self::DetectedTrueLoopAt { signals } if signals.is_empty() => {
                write!(f, "Detected True Loop")
            }
            Self::DetectedTrueLoopAt { signals } => {
                write!(f, "Detected True Loop: {}", signals.join(", "))
            }
            Self::Runtime { message, signals } if signals.is_empty() => write!(f, "{message}"),
            Self::Runtime { message, signals } => {
                write!(f, "{}: {}", message, signals.join(", "))
            }
            Self::InternalError => write!(f, "Internal Error"),
            Self::NotAnEvent(name) => write!(
                f,
                "Signal '{}' is not an event (only clock and async reset signals can be scheduled). Use `modify()` for synchronous signals.",
                name
            ),
        }
    }
}

impl std::error::Error for SimulatorErrorCode {}
/// Generated combinational fatal captures use negative status codes. Positive
/// statuses belong to loop/runtime errors; zero means success. Keep the range
/// bounded by the event site's u32 ID so internal failures such as a panicked
/// lane (`i64::MIN`) cannot be mistaken for an assertion.
pub fn comb_fatal_code(site_id: u32) -> i64 {
    -1 - i64::from(site_id)
}

/// Decode the site of a generated combinational fatal status.
pub fn comb_fatal_site(code: i64) -> Option<u32> {
    u32::try_from(code.checked_neg()?.checked_sub(1)?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fatal_statuses_are_disjoint_from_success_loops_and_internal_failures() {
        for site in [0, 1, 2, 1999, 2000, u32::MAX] {
            assert_eq!(comb_fatal_site(comb_fatal_code(site)), Some(site));
        }
        for code in [0, 1, 2, 1999, 2000, i64::MIN, -1 - i64::from(u32::MAX) - 1] {
            assert_eq!(comb_fatal_site(code), None);
        }
    }
}
