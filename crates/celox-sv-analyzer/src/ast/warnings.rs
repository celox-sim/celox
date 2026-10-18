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

/// Run an analysis and return every warning it found, even if it fails.
/// Nested analyses also contribute their warnings to the enclosing collector.
pub fn analyzing<T>(f: impl FnOnce() -> T) -> (T, Vec<AnalyzerWarning>) {
    struct Restore(Option<Vec<AnalyzerWarning>>);
    impl Drop for Restore {
        fn drop(&mut self) {
            WARNINGS.with(|current| current.replace(self.0.take()));
        }
    }
    let saved = Restore(WARNINGS.with(|current| current.replace(Some(Vec::new()))));
    let result = f();
    let warnings = WARNINGS.with(|current| current.borrow_mut().take().unwrap_or_default());
    drop(saved);
    for warning in &warnings {
        warn(warning.clone());
    }
    (result, warnings)
}

/// Report `warning` with the analysis that is running, if any. Elaboration
/// can evaluate a construct many times; it is reported once.
pub(super) fn warn(warning: AnalyzerWarning) {
    WARNINGS.with(|current| {
        if let Some(warnings) = &mut *current.borrow_mut()
            && !warnings.contains(&warning)
        {
            warnings.push(warning);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_failed_analyses_preserve_warnings() {
        let (result, warnings) = analyzing(|| {
            let (result, warnings) = analyzing(|| {
                warn(AnalyzerWarning::ExitOutsideProgram { site: None });
                Err::<(), _>("failed")
            });
            assert_eq!(warnings.len(), 1);
            result
        });
        assert_eq!(result, Err("failed"));
        assert_eq!(
            warnings,
            vec![AnalyzerWarning::ExitOutsideProgram { site: None }]
        );
    }

    #[test]
    fn unwinding_restores_the_enclosing_collector() {
        let (_, warnings) = analyzing(|| {
            assert!(std::panic::catch_unwind(|| analyzing(|| panic!("failed"))).is_err());
            warn(AnalyzerWarning::ExitOutsideProgram { site: None });
        });
        assert_eq!(warnings.len(), 1);
        assert!(WARNINGS.with(|current| current.borrow().is_none()));
    }
}
