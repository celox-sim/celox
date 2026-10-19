//! Native immediate predicates feed the shared procedural truth conversion.
use super::*;
use celox_sv_syntax::AssertionHeader;

impl BodyBuilder<'_, '_, '_> {
    pub(super) fn native_assertion_condition(
        &self,
        keyword: &str,
        header: &AssertionHeader,
    ) -> Result<Expr, AnalyzerError> {
        let AssertionHeader::Immediate {
            condition,
            deferred,
            ..
        } = header
        else {
            return Err(unsupported("concurrent assertion"));
        };
        if keyword == "cover" && deferred.is_some() {
            return Err(unsupported("deferred immediate cover"));
        }
        if !matches!(keyword, "assert" | "assume" | "cover") {
            return Err(unsupported("immediate assertion keyword"));
        }
        self.native_value(*condition)
            .map(procedural_truth_condition)
    }
}
