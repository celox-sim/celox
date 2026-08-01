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
