use veryl_analyzer::ir::{Component, Ir, VarId, VarKind};
use veryl_parser::resource_table::{self, StrId};
use veryl_parser::token_range::TokenRange;
use veryl_parser::veryl_grammar_trait::{ForStatement, Veryl};
use veryl_parser::veryl_walker::VerylWalker;

use crate::HashMap;

/// Stable identity of a source-level `for` statement within a compilation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct LoopSourceId(pub(crate) usize);

/// Read-only source provenance retained across Veryl analyzer pass 2.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LoopSource {
    pub(crate) id: LoopSourceId,
    /// Token used by the analyzer for each synthetic unrolled loop variable.
    pub(crate) identifier_token: TokenRange,
    pub(crate) identifier: StrId,
    pub(crate) statement_token: TokenRange,
    pub(crate) body_token: TokenRange,
    pub(crate) parent: Option<LoopSourceId>,
}

/// Source-level loops collected before analyzer pass 2 flattens constant loops.
#[derive(Debug, Clone, Default)]
pub(crate) struct LoopSourceTable {
    sources: Vec<LoopSource>,
    by_identifier_token: HashMap<TokenRange, Vec<LoopSourceId>>,
}

impl LoopSourceTable {
    pub(crate) fn collect<'a>(asts: impl IntoIterator<Item = &'a Veryl>) -> Self {
        let mut collector = LoopSourceCollector::default();
        for ast in asts {
            collector.veryl(ast);
        }

        let mut by_identifier_token: HashMap<TokenRange, Vec<LoopSourceId>> = HashMap::default();
        for source in &collector.sources {
            by_identifier_token
                .entry(source.identifier_token)
                .or_default()
                .push(source.id);
        }

        Self {
            sources: collector.sources,
            by_identifier_token,
        }
    }

    pub(crate) fn source(&self, id: LoopSourceId) -> Option<&LoopSource> {
        self.sources.get(id.0)
    }

    /// Match the source table against analyzer IR without changing either side.
    pub(crate) fn match_unrolled(self, ir: &Ir) -> LoopProvenance {
        let mut grouped: HashMap<CandidateKey, Vec<UnrolledIteration>> = HashMap::default();

        for (component_index, component) in ir.components.iter().enumerate() {
            let Component::Module(module) = component else {
                continue;
            };

            for variable in module.variables.values() {
                let Some(source_ids) = self.by_identifier_token.get(&variable.token) else {
                    continue;
                };

                for &source_id in source_ids {
                    let Some(source) = self.source(source_id) else {
                        continue;
                    };
                    let Some(iteration) = match_unrolled_iteration(source, variable) else {
                        continue;
                    };
                    let parent_hierarchy =
                        iteration.hierarchy[..iteration.hierarchy.len().saturating_sub(1)].to_vec();
                    grouped
                        .entry(CandidateKey {
                            source: source_id,
                            component_index,
                            parent_hierarchy,
                        })
                        .or_default()
                        .push(iteration);
                }
            }
        }

        let mut candidates = grouped
            .into_iter()
            .map(|(key, mut iterations)| {
                // VarIds are allocated in the same order as `unroll_for` visits the
                // evaluated range. Sorting by value would corrupt reverse loops.
                iterations.sort_unstable_by_key(|iteration| iteration.loop_var);
                let module = match &ir.components[key.component_index] {
                    Component::Module(module) => module,
                    _ => unreachable!("candidate keys are only built for modules"),
                };
                UnrolledLoopCandidate {
                    source: key.source,
                    component_index: key.component_index,
                    module_name: module.name,
                    module_token: module.token,
                    parent_hierarchy: key.parent_hierarchy,
                    iterations,
                }
            })
            .collect::<Vec<_>>();
        candidates.sort_unstable_by(|lhs, rhs| {
            (lhs.component_index, lhs.source, &lhs.parent_hierarchy).cmp(&(
                rhs.component_index,
                rhs.source,
                &rhs.parent_hierarchy,
            ))
        });

        LoopProvenance {
            sources: self,
            candidates,
        }
    }
}

/// Analyzer-IR evidence for one source loop iteration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnrolledIteration {
    pub(crate) loop_var: VarId,
    pub(crate) value: usize,
    /// Full analyzer hierarchy prefix, including this iteration's `[i]` label.
    pub(crate) hierarchy: Vec<StrId>,
    pub(crate) iteration_label: StrId,
}

/// All synthetic iterations that may belong to one source loop in one IR hierarchy.
///
/// This is discovery metadata, not a semantic-equivalence proof. Consumers must
/// validate the recovered transition for every iteration before replacing IR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UnrolledLoopCandidate {
    pub(crate) source: LoopSourceId,
    pub(crate) component_index: usize,
    pub(crate) module_name: StrId,
    pub(crate) module_token: TokenRange,
    /// Hierarchy outside this loop. Nested loops therefore form one candidate
    /// per enclosing unrolled iteration instead of being accidentally fused.
    pub(crate) parent_hierarchy: Vec<StrId>,
    /// Execution order, not numeric order.
    pub(crate) iterations: Vec<UnrolledIteration>,
}

/// Owned, read-only candidate metadata passed from the analyzer boundary into
/// the parser. Merely appearing here never authorizes an IR rewrite.
#[derive(Debug, Clone, Default)]
pub(crate) struct LoopProvenance {
    sources: LoopSourceTable,
    candidates: Vec<UnrolledLoopCandidate>,
}

#[derive(Debug, Clone)]
pub(crate) struct LoopRecoveryCandidate {
    pub(crate) source: LoopSource,
    pub(crate) unrolled: UnrolledLoopCandidate,
}

impl LoopProvenance {
    /// Diagnostic/test API for the later semantic recovery phase.
    pub(crate) fn candidates(&self) -> &[UnrolledLoopCandidate] {
        &self.candidates
    }

    pub(crate) fn candidates_for_module(
        &self,
        module: &veryl_analyzer::ir::Module,
    ) -> Vec<LoopRecoveryCandidate> {
        self.candidates
            .iter()
            .filter(|candidate| {
                candidate.module_name == module.name
                    && candidate.module_token == module.token
                    && candidate
                        .iterations
                        .iter()
                        .all(|iteration| module.variables.contains_key(&iteration.loop_var))
            })
            .filter_map(|unrolled| {
                self.sources
                    .source(unrolled.source)
                    .map(|source| LoopRecoveryCandidate {
                        source: source.clone(),
                        unrolled: unrolled.clone(),
                    })
            })
            .collect()
    }

    /// Check only that the read-only metadata still names the IR it was derived
    /// from. This deliberately makes no semantic claim about a candidate body.
    pub(crate) fn is_consistent_with(&self, ir: &Ir) -> bool {
        self.candidates().iter().all(|candidate| {
            self.sources.source(candidate.source).is_some()
                && candidate
                    .iterations
                    .windows(2)
                    .all(|pair| pair[0].loop_var < pair[1].loop_var)
                && matches!(
                    ir.components.get(candidate.component_index),
                    Some(Component::Module(module))
                        if module.name == candidate.module_name
                            && module.token == candidate.module_token
                )
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct CandidateKey {
    source: LoopSourceId,
    component_index: usize,
    parent_hierarchy: Vec<StrId>,
}

#[derive(Default)]
struct LoopSourceCollector {
    sources: Vec<LoopSource>,
    nesting: Vec<LoopSourceId>,
}

impl VerylWalker for LoopSourceCollector {
    fn for_statement(&mut self, statement: &ForStatement) {
        let id = LoopSourceId(self.sources.len());
        self.sources.push(LoopSource {
            id,
            identifier_token: statement.identifier.as_ref().into(),
            identifier: statement.identifier.text(),
            statement_token: statement.into(),
            body_token: statement.statement_block.as_ref().into(),
            parent: self.nesting.last().copied(),
        });

        self.nesting.push(id);
        // A statement-level `for` can only contain another statement-level loop
        // through its body, so traversing the body is sufficient for nesting.
        self.statement_block(&statement.statement_block);
        let popped = self.nesting.pop();
        debug_assert_eq!(popped, Some(id));
    }
}

fn match_unrolled_iteration(
    source: &LoopSource,
    variable: &veryl_analyzer::ir::Variable,
) -> Option<UnrolledIteration> {
    if variable.kind != VarKind::Const
        || variable.token != source.identifier_token
        || variable.path.0.last().copied() != Some(source.identifier)
        || variable.path.0.len() < 2
        || variable.value.len() != 1
    {
        return None;
    }

    let hierarchy = variable.path.0[..variable.path.0.len() - 1].to_vec();
    let iteration_label = *hierarchy.last()?;
    let label_value = parse_iteration_label(iteration_label)?;
    let value = variable.value.first()?.to_usize()?;
    if label_value != value {
        return None;
    }

    Some(UnrolledIteration {
        loop_var: variable.id,
        value,
        hierarchy,
        iteration_label,
    })
}

fn parse_iteration_label(label: StrId) -> Option<usize> {
    let label = resource_table::get_str_value(label)?;
    label.strip_prefix('[')?.strip_suffix(']')?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use veryl_analyzer::{Analyzer, Context, attribute_table, symbol_table};
    use veryl_metadata::Metadata;
    use veryl_parser::Parser;

    fn analyze(code: &str) -> (veryl_parser::veryl_grammar_trait::Veryl, Ir) {
        symbol_table::clear();
        attribute_table::clear();

        let metadata = Metadata::create_default("prj").expect("default metadata must be valid");
        let analyzer = Analyzer::new(&metadata);
        let parsed = Parser::parse(code, &"").expect("test source must parse");
        let pass1_errors = analyzer.analyze_pass1("prj", &parsed.veryl);
        assert!(pass1_errors.is_empty(), "pass1 errors: {pass1_errors:?}");
        let post1_errors = Analyzer::analyze_post_pass1();
        assert!(
            post1_errors.is_empty(),
            "post-pass1 errors: {post1_errors:?}"
        );

        let mut context = Context::default();
        let mut ir = Ir::default();
        let pass2_errors =
            analyzer.analyze_pass2("prj", &parsed.veryl, &mut context, Some(&mut ir));
        assert!(pass2_errors.is_empty(), "pass2 errors: {pass2_errors:?}");
        let post2_errors = Analyzer::analyze_post_pass2(&ir);
        assert!(
            post2_errors.is_empty(),
            "post-pass2 errors: {post2_errors:?}"
        );
        (parsed.veryl, ir)
    }

    #[test]
    fn collects_nested_source_loop_parentage() {
        let parsed = Parser::parse(
            r#"
                module Top {
                    always_comb {
                        for i in 0..2 {
                            for j in 0..3 {}
                        }
                    }
                }
            "#,
            &"",
        )
        .expect("test source must parse");
        let table = LoopSourceTable::collect([&parsed.veryl]);

        assert_eq!(table.sources.len(), 2);
        assert_eq!(table.sources[0].parent, None);
        assert_eq!(table.sources[1].parent, Some(LoopSourceId(0)));
        assert_ne!(
            table.sources[0].identifier_token,
            table.sources[1].identifier_token
        );
    }

    #[test]
    fn collects_sf0_32_unrolled_iterations_as_one_candidate() {
        let code = r#"
            module Top (
                bits : input  logic<32>,
                value: output logic<6> ,
            ) {
                var sf0: logic<6>;
                always_comb {
                    sf0 = 6'd0;
                    for i in 0..32 {
                        if bits[i] {
                            sf0 = 6'd1;
                        }
                    }
                    value = sf0;
                }
            }
        "#;
        let (ast, ir) = analyze(code);
        let provenance = LoopSourceTable::collect([&ast]).match_unrolled(&ir);

        assert_eq!(provenance.sources.sources.len(), 1);
        assert_eq!(provenance.candidates().len(), 1);
        let candidate = &provenance.candidates()[0];
        assert_eq!(
            resource_table::get_str_value(candidate.module_name).as_deref(),
            Some("Top")
        );
        assert!(candidate.parent_hierarchy.is_empty());
        assert_eq!(candidate.iterations.len(), 32);
        assert_eq!(
            candidate
                .iterations
                .iter()
                .map(|iteration| iteration.value)
                .collect::<Vec<_>>(),
            (0..32).collect::<Vec<_>>()
        );
        for iteration in &candidate.iterations {
            assert_eq!(iteration.hierarchy.len(), 1);
            let expected_label = format!("[{}]", iteration.value);
            assert_eq!(
                resource_table::get_str_value(iteration.iteration_label).as_deref(),
                Some(expected_label.as_str())
            );
        }
    }

    #[test]
    fn token_range_keeps_same_named_loops_separate() {
        let code = r#"
            module Top (
                value: output logic<4>,
            ) {
                var tmp: logic<4>;
                always_comb {
                    tmp = 4'd0;
                    for i in 0..2 { tmp = (i as 4); }
                    for i in 0..3 { tmp = (i as 4); }
                    value = tmp;
                }
            }
        "#;
        let (ast, ir) = analyze(code);
        let provenance = LoopSourceTable::collect([&ast]).match_unrolled(&ir);

        assert_eq!(provenance.sources.sources.len(), 2);
        assert_eq!(provenance.candidates().len(), 2);
        assert_eq!(provenance.candidates()[0].iterations.len(), 2);
        assert_eq!(provenance.candidates()[1].iterations.len(), 3);
        assert_ne!(
            provenance.sources.sources[0].identifier_token,
            provenance.sources.sources[1].identifier_token
        );
    }
}
