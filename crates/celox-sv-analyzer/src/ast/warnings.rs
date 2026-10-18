//! The warnings of an analysis.

use std::cell::RefCell;

use crate::procedural::SourceSpan;

/// A warning found while analyzing, before the frontend lowers the design.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnalyzerWarning {
    /// `$exit` outside a program block, here in a constant function, which
    /// IEEE 1800-2023 24.7 ignores.
    ExitOutsideProgram { site: Option<SourceSpan> },
}

thread_local! {
    static WARNINGS: RefCell<Option<Vec<AnalyzerWarning>>> = const { RefCell::new(None) };
}

/// Run one analysis and return the warnings it found.
pub(crate) fn analyzing<T>(f: impl FnOnce() -> T) -> (T, Vec<AnalyzerWarning>) {
    let saved = WARNINGS.with(|current| current.replace(Some(Vec::new())));
    let result = f();
    let warnings = WARNINGS.with(|current| current.replace(saved));
    (result, warnings.unwrap_or_default())
}

/// Report `warning` with the analysis that is running, if any.
pub(super) fn warn(warning: AnalyzerWarning) {
    WARNINGS.with(|current| {
        if let Some(warnings) = &mut *current.borrow_mut() {
            warnings.push(warning);
        }
    });
}
