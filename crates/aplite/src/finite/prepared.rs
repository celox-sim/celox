//! Reusable scalar encoding with temporary Boolean assumptions.
use super::*;

mod cell;
mod hashing;
pub use cell::{Cell, CellStatus};
pub use hashing::HashCells;

/// Select the polarity of a predicate supplied when preparing the query.
#[derive(Clone, Copy, Debug)]
pub struct Assumption {
    pub predicate: usize,
    pub value: bool,
}

/// A compiled scalar query. Predicate definitions are permanent, but asserting
/// their truth values is local to each call. A successful search retains its
/// learned clauses and heuristic state; UNSAT and interrupted searches discard
/// search state while preserving the compiled encoding.
///
/// This provides reusable solving, not uniform sampling. Seed reproducibility
/// requires the same query history and solver version. Preparation and each
/// solve have separate explicit budgets; callers must account for an aggregate
/// operation budget themselves.
pub struct PreparedQuery {
    formula: Term,
    context: Env,
    predicates: Vec<Term>,
    literals: Vec<Lit>,
    blast: Blast,
    sat: Option<Sat>,
    preparation_work: u64,
}

impl PreparedQuery {
    pub fn new(formula: &Term, context: &Env, predicates: &[Term], limits: Limits) -> Res<Self> {
        let mut budget = Budget {
            limits,
            start: Instant::now(),
            work: 0,
            time_check_in: 0,
        };
        budget.tick(1)?;
        // Add predicates to the compile context so source validation sees dead
        // branches, sort conflicts and variables erased by normalization too.
        let mut compile_context = context.clone();
        for (index, term) in predicates.iter().enumerate() {
            budget.tick(1)?;
            if term.0.sort != Sort::Bool {
                return Err("prepared predicates must have Bool sort".into());
            }
            let mut key = format!("__aplite_predicate_{index}");
            while compile_context.contains_key(&key) {
                budget.tick(1)?;
                key.push('_');
            }
            compile_context.insert(key, term.clone());
        }
        let mut blast = Blast::new();
        compile_scalar(&mut blast, formula, &compile_context, &mut budget, true)?;
        let mut literals = Vec::with_capacity(predicates.len());
        for term in predicates {
            literals.push(blast.term(term, &mut budget, 0)?.bits[0]);
        }
        budget.check_time()?;
        Ok(Self {
            formula: formula.clone(),
            context: context.clone(),
            predicates: predicates.to_vec(),
            literals,
            blast,
            sat: None,
            preparation_work: budget.work,
        })
    }

    /// Work charged for the one-time source validation and bit blasting.
    pub fn preparation_work(&self) -> u64 {
        self.preparation_work
    }

    /// Solve with temporary assumptions. Invalid indexes and exhausted budgets
    /// yield Unknown. SAT models are replayed against the original formula,
    /// context and each asserted predicate before becoming visible.
    pub fn solve(&mut self, assumptions: &[Assumption], limits: Limits, seed: u64) -> Outcome {
        let mut outcome = Outcome {
            search_hint: SearchHint::Sat,
            search_strategy: SearchStrategy::SingleSearch,
            verdict: Verdict::Unknown,
            reason: None,
            assignments: BTreeMap::new(),
            context_values: BTreeMap::new(),
            array_assignments: BTreeMap::new(),
            array_context_values: BTreeMap::new(),
            original_formula_validated: false,
            stats: Stats::default(),
        };
        let mut budget = Budget {
            limits,
            start: Instant::now(),
            work: 0,
            time_check_in: 0,
        };
        // Take ownership so any error drops possibly interrupted mutations.
        let mut sat = self.sat.take();
        let result = (|| -> Res<Verdict> {
            budget.tick(1)?;
            if self.blast.vars > budget.limits.max_variables
                || self.blast.memo.len() > budget.limits.max_terms
            {
                return Err("prepared query exceeds solve size limits".into());
            }
            let mut literals = Vec::with_capacity(assumptions.len());
            for assumption in assumptions {
                budget.tick(1)?;
                let p = *self
                    .literals
                    .get(assumption.predicate)
                    .ok_or("invalid prepared predicate index")?;
                literals.push(if assumption.value { p } else { -p });
            }
            if self.blast.clauses.len() > budget.limits.max_clauses {
                return Err("prepared query exceeds solve clause limit".into());
            }
            if sat.is_none() {
                let mut clauses = Vec::with_capacity(self.blast.clauses.len());
                for clause in &self.blast.clauses {
                    budget.tick(1 + clause.len() as u64)?;
                    clauses.push(clause.clone());
                }
                sat = Some(Sat::new(self.blast.vars, clauses));
            }
            let solver = sat.as_mut().unwrap();
            if solver.clauses.len() > budget.limits.max_clauses {
                return Err("prepared query exceeds solve clause limit".into());
            }
            solver.backtrack(0, &mut budget)?;
            solver.seed_phases(seed, &mut budget)?;
            let decisions = solver.decisions;
            let conflicts = solver.conflicts;
            let result = solver.run_slice_assuming(&mut budget, u64::MAX, &literals);
            outcome.stats.decisions = solver.decisions - decisions;
            outcome.stats.conflicts = solver.conflicts - conflicts;
            outcome.stats.search_slices = 1;
            let verdict = match result? {
                Search::Complete(verdict) => verdict,
                Search::Pending => return Err("prepared search did not complete".into()),
            };
            if verdict != Verdict::Sat {
                budget.check_time()?;
                return Ok(verdict);
            }
            let (assignments, values) = replay_model(
                &self.formula,
                &self.context,
                &self.blast,
                solver,
                &mut budget,
            )?;
            let mut memo = HashMap::new();
            for assumption in assumptions {
                if evaluate(
                    &self.predicates[assumption.predicate],
                    &assignments,
                    &mut memo,
                    &mut budget,
                    0,
                )? != Scalar::Bool(assumption.value)
                {
                    return Err("prepared model failed assumption replay".into());
                }
            }
            budget.check_time()?;
            outcome.assignments = assignments;
            outcome.context_values = values;
            outcome.original_formula_validated = true;
            Ok(Verdict::Sat)
        })();
        outcome.stats.work = budget.work;
        outcome.stats.base_cnf_reused = true;
        outcome.stats.terms = self.blast.memo.len();
        outcome.stats.variables = self.blast.vars;
        outcome.stats.base_clauses = self.blast.clauses.len();
        outcome.stats.clauses = sat
            .as_ref()
            .map_or(self.blast.clauses.len(), |sat| sat.clauses.len());
        outcome.stats.peak_live_clauses = outcome.stats.clauses;
        match result {
            Ok(verdict) => outcome.verdict = verdict,
            Err(reason) => outcome.reason = Some(reason),
        }
        if outcome.verdict == Verdict::Sat {
            self.sat = sat;
        }
        outcome
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{and, boolv, bv, eq, node, not, var};

    fn assumption(predicate: usize, value: bool) -> Assumption {
        Assumption { predicate, value }
    }

    #[test]
    fn assumptions_do_not_escape_calls_or_become_alias_definitions() {
        let x = var("x".into(), Sort::Bv(8));
        let y = var("y".into(), Sort::Bv(8));
        let predicates = [eq(x.clone(), y.clone()), eq(x.clone(), bv(8, 37))];
        let context = Env::from([("x".into(), x), ("y".into(), y)]);
        let mut prepared =
            PreparedQuery::new(&boolv(true), &context, &predicates, Limits::default()).unwrap();
        assert!(prepared.preparation_work() > 0);
        for value in [true, false, true, false] {
            let outcome = prepared.solve(
                &[assumption(0, value), assumption(1, true)],
                Limits::default(),
                42,
            );
            assert_eq!(outcome.verdict, Verdict::Sat);
            assert!(outcome.original_formula_validated);
            assert_eq!(
                outcome.assignments["x"],
                Scalar::Bv {
                    width: 8,
                    value: 37
                }
            );
            assert_eq!(outcome.assignments["x"] == outcome.assignments["y"], value);
            assert!(prepared.sat.is_some());
        }
        let bad = prepared.solve(
            &[assumption(0, true), assumption(0, false)],
            Limits::default(),
            0,
        );
        assert_eq!(bad.verdict, Verdict::Unsat);
        assert!(bad.assignments.is_empty());
        assert_eq!(
            prepared.solve(&[], Limits::default(), 0).verdict,
            Verdict::Sat
        );
    }

    #[test]
    fn constant_aliased_and_duplicate_assumptions_are_sound() {
        let x = var("x".into(), Sort::Bool);
        let y = var("y".into(), Sort::Bool);
        let formula = and(x.clone(), eq(x.clone(), y.clone()));
        let predicates = [boolv(true), boolv(false), x, y];
        let mut query =
            PreparedQuery::new(&formula, &Env::new(), &predicates, Limits::default()).unwrap();
        for (index, required) in [(0, true), (1, false), (2, true), (3, true)] {
            let sat = query.solve(
                &[assumption(index, required), assumption(index, required)],
                Limits::default(),
                0,
            );
            assert_eq!(sat.verdict, Verdict::Sat);
            assert_eq!(
                query
                    .solve(&[assumption(index, !required)], Limits::default(), 0)
                    .verdict,
                Verdict::Unsat
            );
        }
    }

    #[test]
    fn exhaustive_three_boolean_queries_match_truth_tables_across_reuse() {
        let vars = [
            var("x".into(), Sort::Bool),
            var("y".into(), Sort::Bool),
            var("z".into(), Sort::Bool),
        ];
        let context = Env::from([
            ("x".into(), vars[0].clone()),
            ("y".into(), vars[1].clone()),
            ("z".into(), vars[2].clone()),
        ]);
        for mask in 0..256u16 {
            let mut formula = boolv(false);
            for row in 0..8 {
                if mask & (1 << row) != 0 {
                    let mut cube = boolv(true);
                    for (index, variable) in vars.iter().enumerate() {
                        cube = and(
                            cube,
                            if row & (1 << index) != 0 {
                                variable.clone()
                            } else {
                                not(variable.clone())
                            },
                        );
                    }
                    formula = node(Sort::Bool, "or", vec![formula, cube]);
                }
            }
            let predicates = [
                vars[0].clone(),
                vars[1].clone(),
                vars[2].clone(),
                node(Sort::Bool, "xor", vec![vars[0].clone(), vars[1].clone()]),
            ];
            let mut query =
                PreparedQuery::new(&formula, &context, &predicates, Limits::default()).unwrap();
            for choice in 0..16 {
                // Alternate partial assumptions and XOR predicates to leave
                // search decisions/conflicts beyond the assumption prefix.
                let conditions = [
                    assumption(choice % 3, choice & 1 != 0),
                    assumption(3, choice & 2 != 0),
                ];
                let legal: Vec<_> = (0..8)
                    .filter(|row| {
                        mask & (1 << row) != 0
                            && ((*row >> (choice % 3)) & 1 != 0) == (choice & 1 != 0)
                            && (((*row & 1) != 0) ^ ((*row & 2) != 0)) == (choice & 2 != 0)
                    })
                    .collect();
                let outcome = query.solve(&conditions, Limits::default(), choice as u64);
                assert_eq!(
                    outcome.verdict,
                    if legal.is_empty() {
                        Verdict::Unsat
                    } else {
                        Verdict::Sat
                    },
                    "mask={mask}, choice={choice}"
                );
                if outcome.verdict == Verdict::Sat {
                    let mut row = 0;
                    for (index, name) in ["x", "y", "z"].iter().enumerate() {
                        if outcome.assignments[*name] == Scalar::Bool(true) {
                            row |= 1 << index;
                        }
                    }
                    assert!(legal.contains(&row));
                    assert!(outcome.original_formula_validated);
                }
            }
        }
    }

    #[test]
    fn interruptions_and_invalid_indexes_leave_the_encoding_reusable() {
        let x = var("x".into(), Sort::Bv(8));
        let context = Env::from([("x".into(), x.clone())]);
        let predicate = eq(x, bv(8, 127));
        let mut query =
            PreparedQuery::new(&boolv(true), &context, &[predicate], Limits::default()).unwrap();
        for work in 0..100 {
            let result = query.solve(
                &[assumption(0, true)],
                Limits {
                    max_work: work,
                    ..Limits::default()
                },
                0,
            );
            if result.verdict == Verdict::Unknown {
                assert!(query.sat.is_none());
                assert!(result.assignments.is_empty());
            }
            let next = query.solve(&[assumption(0, false)], Limits::default(), 0);
            assert_eq!(next.verdict, Verdict::Sat);
            assert_ne!(
                next.assignments["x"],
                Scalar::Bv {
                    width: 8,
                    value: 127
                }
            );
        }
        for conditions in [
            vec![assumption(usize::MAX, true)],
            vec![assumption(1, false)],
        ] {
            assert_eq!(
                query.solve(&conditions, Limits::default(), 0).verdict,
                Verdict::Unknown
            );
            assert_eq!(query.solve(&[], Limits::default(), 0).verdict, Verdict::Sat);
        }
        for limits in [
            Limits {
                max_variables: 0,
                ..Limits::default()
            },
            Limits {
                max_clauses: 0,
                ..Limits::default()
            },
            Limits {
                timeout_ms: 0,
                ..Limits::default()
            },
        ] {
            assert_eq!(query.solve(&[], limits, 0).verdict, Verdict::Unknown);
            assert_eq!(query.solve(&[], Limits::default(), 0).verdict, Verdict::Sat);
        }
    }

    #[test]
    fn learned_clauses_survive_success_but_do_not_turn_assumptions_into_facts() {
        // Resolution proves x, but there is no x unit in the source. Exercise
        // a conflict and retained learning before changing the assumptions.
        let x = var("x".into(), Sort::Bool);
        let y = var("y".into(), Sort::Bool);
        let formula = and(
            node(Sort::Bool, "or", vec![x.clone(), y.clone()]),
            node(Sort::Bool, "or", vec![x.clone(), not(y.clone())]),
        );
        let mut exercised_learning = false;
        for seed in 0..64 {
            let mut query = PreparedQuery::new(
                &formula,
                &Env::new(),
                &[x.clone(), y.clone()],
                Limits::default(),
            )
            .unwrap();
            let first = query.solve(&[], Limits::default(), seed);
            assert_eq!(first.verdict, Verdict::Sat);
            if first.stats.conflicts == 0 {
                continue;
            }
            exercised_learning = true;
            assert!(query.sat.as_ref().unwrap().clauses.len() > query.blast.clauses.len());
            for value in [false, true, false] {
                let result = query.solve(&[assumption(1, value)], Limits::default(), seed);
                assert_eq!(result.verdict, Verdict::Sat);
                assert_eq!(result.assignments["x"], Scalar::Bool(true));
                assert_eq!(result.assignments["y"], Scalar::Bool(value));
            }
            assert_eq!(
                query
                    .solve(&[assumption(0, false)], Limits::default(), seed)
                    .verdict,
                Verdict::Unsat
            );
            assert_eq!(
                query
                    .solve(&[assumption(0, true)], Limits::default(), seed)
                    .verdict,
                Verdict::Sat
            );
            break;
        }
        assert!(exercised_learning);
    }

    #[test]
    fn preparation_checks_predicates_even_when_unasserted() {
        for predicate in [
            bv(8, 1),
            node(Sort::Bool, "unknown", vec![]),
            eq(
                var("m".into(), Sort::Mem(4, 8)),
                var("m".into(), Sort::Mem(4, 8)),
            ),
        ] {
            assert!(
                PreparedQuery::new(&boolv(true), &Env::new(), &[predicate], Limits::default())
                    .is_err()
            );
        }
        let context = Env::from([("x".into(), var("x".into(), Sort::Bv(8)))]);
        assert!(
            PreparedQuery::new(
                &boolv(true),
                &context,
                &[var("x".into(), Sort::Bool)],
                Limits::default()
            )
            .is_err()
        );
        assert!(
            PreparedQuery::new(
                &boolv(true),
                &Env::new(),
                &[],
                Limits {
                    max_work: 0,
                    ..Limits::default()
                }
            )
            .is_err()
        );
    }
}
