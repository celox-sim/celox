//! Preserve runtime termination before the analyzer folds loop bounds.

use veryl_analyzer::{
    AnalyzerError, analyzer_error::UnevaluableValueKind, symbol::SymbolKind, symbol_table,
};
use veryl_parser::{
    veryl_grammar_trait::{DollarIdentifier, FunctionDeclaration, InitialDeclaration, Veryl},
    veryl_walker::VerylWalker,
};

/// Source contexts where the kernel supports runtime case conditions.
#[derive(Default)]
pub struct RuntimeFunctions {
    initials: Vec<veryl_parser::token_range::TokenRange>,
}
impl RuntimeFunctions {
    /// Kernel initial blocks evaluate case patterns at runtime. Keep the
    /// analyzer's constant-case requirement for RTL and other contexts.
    pub fn is_runtime_case_condition(&self, error: &AnalyzerError) -> bool {
        let AnalyzerError::UnevaluableValue {
            kind: UnevaluableValueKind::CaseCondition,
            error_location,
            token_source,
            ..
        } = error
        else {
            return false;
        };
        self.initials.iter().any(|range| {
            let span: miette::SourceSpan = range.into();
            &range.beg.source == token_source
                && span.offset() <= error_location.offset()
                && error_location.offset() + error_location.len() <= span.offset() + span.len()
        })
    }
}

/// Call after pass 1 and before its global post-pass. A function containing
/// `$finish` cannot be constant evaluated even if its return value is constant.
/// Seeding that fact before the analyzer resolves functions also propagates it
/// to callers through its normal constant-function dependency analysis.
pub fn preserve_terminating_function_calls<'a>(
    asts: impl IntoIterator<Item = &'a Veryl>,
) -> RuntimeFunctions {
    #[derive(Default)]
    struct Finish {
        found: bool,
    }
    impl VerylWalker for Finish {
        fn dollar_identifier(&mut self, identifier: &DollarIdentifier) {
            self.found |= identifier.dollar_identifier_token.token.text.to_string() == "$finish";
        }
    }
    #[derive(Default)]
    struct Functions {
        tokens: crate::HashSet<veryl_parser::resource_table::TokenId>,
        initials: Vec<veryl_parser::token_range::TokenRange>,
    }
    impl VerylWalker for Functions {
        fn initial_declaration(&mut self, initial: &InitialDeclaration) {
            self.initials.push(initial.into());
            self.statement_block(&initial.statement_block);
        }
        fn function_declaration(&mut self, function: &FunctionDeclaration) {
            let mut finish = Finish::default();
            finish.statement_block(&function.statement_block);
            if finish.found {
                self.tokens
                    .insert(function.identifier.identifier_token.token.id);
            }
        }
    }
    let mut functions = Functions::default();
    for ast in asts {
        functions.veryl(ast);
    }
    for mut symbol in symbol_table::get_all() {
        if functions.tokens.contains(&symbol.token.id)
            && let SymbolKind::Function(property) = &mut symbol.kind
        {
            property.constantable = Some(false);
            symbol_table::update(symbol);
        }
    }
    RuntimeFunctions {
        initials: functions.initials,
    }
}
