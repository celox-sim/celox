//! Bounded projected model enumeration in a temporary SAT database.
use super::*;
use std::collections::BTreeSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellStatus {
    /// The entire selected cell was enumerated. It may be empty.
    Complete,
    /// At least max_models + 1 distinct projected models exist in the cell.
    TooLarge,
    /// Invalid input, unsupported query, or an exhausted resource budget.
    Unknown,
}

#[derive(Debug)]
pub struct Cell {
    pub status: CellStatus,
    /// Exposed only for Complete cells; never a biased truncated prefix.
    /// Keys are original variable names, not context labels.
    pub models: Vec<BTreeMap<String, Scalar>>,
    pub work: u64,
    pub reason: Option<String>,
}

impl PreparedQuery {
    /// Enumerate a bounded cell selected by temporary predicate assumptions.
    ///
    /// `projection` names distinct original scalar variables known at preparation.
    /// Hidden witnesses do not multiply a projected assignment. One extra SAT
    /// query distinguishes an exact-size cell from an oversized cell. The work
    /// and time budgets cover all searches, blocking and replay in this call.
    ///
    /// This is a building block for hash-based sampling, not a sampling policy.
    /// It never enumerates more than max_models + 1 witnesses. Blocking clauses
    /// and learned clauses derived from them live only inside this call, so they
    /// cannot change later cells or ordinary solves on the prepared query.
    pub fn enumerate_cell(
        &self,
        assumptions: &[Assumption],
        projection: &[String],
        max_models: usize,
        limits: Limits,
    ) -> Cell {
        let mut cell = Cell {
            status: CellStatus::Unknown,
            models: Vec::new(),
            work: 0,
            reason: None,
        };
        let mut budget = Budget {
            limits,
            start: Instant::now(),
            work: 0,
            time_check_in: 0,
        };
        let run = (|| -> Res<CellStatus> {
            budget.tick(1)?;
            if self.blast.vars > budget.limits.max_variables
                || self.blast.memo.len() > budget.limits.max_terms
                || self.blast.clauses.len() > budget.limits.max_clauses
            {
                return Err("prepared cell exceeds solve size limits".into());
            }
            let mut seen = BTreeSet::new();
            let mut bits = Vec::new();
            for name in projection {
                budget.tick(1)?;
                if !seen.insert(name) {
                    return Err("duplicate cell projection variable".into());
                }
                let input = self
                    .blast
                    .inputs
                    .get(name)
                    .ok_or("unknown cell projection variable")?;
                budget.tick(input.bits.len() as u64)?;
                bits.extend_from_slice(&input.bits);
            }
            let mut literals = Vec::new();
            for assumption in assumptions {
                budget.tick(1)?;
                let lit = *self
                    .literals
                    .get(assumption.predicate)
                    .ok_or("invalid prepared predicate index")?;
                literals.push(if assumption.value { lit } else { -lit });
            }
            let mut clauses = Vec::new();
            for clause in &self.blast.clauses {
                budget.tick(1 + clause.len() as u64)?;
                clauses.push(clause.clone());
            }
            // One scoped CDCL instance for the whole cell. Reuse encoding,
            // propagation and local learning between models, without copying
            // or recompiling the base formula on each enumeration step.
            let mut solver = Sat::new(self.blast.vars, clauses);
            loop {
                match solver.run_slice_assuming(&mut budget, u64::MAX, &literals)? {
                    Search::Complete(Verdict::Unsat) => {
                        budget.check_time()?;
                        return Ok(CellStatus::Complete);
                    }
                    Search::Complete(Verdict::Sat) => {}
                    _ => return Err("cell search did not complete".into()),
                }
                let (assignments, _) = replay_model(
                    &self.formula,
                    &self.context,
                    &self.blast,
                    &solver,
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
                        return Err("cell model failed assumption replay".into());
                    }
                }
                if cell.models.len() == max_models {
                    budget.check_time()?;
                    return Ok(CellStatus::TooLarge);
                }
                let mut model = BTreeMap::new();
                for name in projection {
                    budget.tick(1)?;
                    model.insert(name.clone(), assignments[name].clone());
                }
                let mut blocking = Vec::new();
                for &bit in &bits {
                    budget.tick(1)?;
                    blocking.push(if truth(&solver.values, bit) > 0 {
                        -bit
                    } else {
                        bit
                    });
                }
                cell.models.push(model);
                solver.backtrack(0, &mut budget)?;
                if !add_blocking_clause(&mut solver, blocking, &mut budget)? {
                    budget.check_time()?;
                    return Ok(CellStatus::Complete);
                }
            }
        })();
        cell.work = budget.work;
        match run {
            Ok(status) => cell.status = status,
            Err(reason) => cell.reason = Some(reason),
        }
        if cell.status != CellStatus::Complete {
            cell.models.clear();
        }
        cell
    }
}

/// Insert a model blocker after backtracking to root. Root assignments are
/// permanent within this scoped database. Simplify by them before watching the
/// clause, so a newly inserted unit is propagated and an empty clause closes
/// the cell immediately. Assumption decisions have already been removed.
fn add_blocking_clause(solver: &mut Sat, clause: Vec<Lit>, budget: &mut Budget) -> Res<bool> {
    debug_assert!(solver.starts.is_empty());
    let mut remaining = BTreeSet::new();
    for lit in clause {
        budget.tick(1)?;
        match truth(&solver.values, lit) {
            1 => return Ok(true),
            -1 => {}
            _ => {
                remaining.insert(lit);
            }
        }
    }
    if remaining.is_empty() {
        return Ok(false);
    }
    if solver.clauses.len() >= budget.limits.max_clauses {
        return Err("cell blocking-clause budget exhausted".into());
    }
    let clause: Vec<_> = remaining.into_iter().collect();
    let unit = (clause.len() == 1).then_some(clause[0]);
    let id = solver.clauses.len();
    solver.clauses.push(clause);
    solver.attach(id);
    if let Some(unit) = unit {
        return Ok(solver.enqueue(unit, Some(id)));
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{and, boolv, bv, eq, node, not, var};

    fn selected(predicate: usize, value: bool) -> Assumption {
        Assumption { predicate, value }
    }

    #[test]
    fn projected_enumeration_ignores_hidden_witnesses_and_is_scoped() {
        let x = var("x".into(), Sort::Bool);
        let y = var("y".into(), Sort::Bool);
        let formula = node(Sort::Bool, "or", vec![not(x.clone()), y.clone()]);
        let mut query =
            PreparedQuery::new(&formula, &Env::new(), &[x, y], Limits::default()).unwrap();
        for _ in 0..3 {
            let cell = query.enumerate_cell(&[], &["x".into()], 2, Limits::default());
            assert_eq!(cell.status, CellStatus::Complete);
            assert_eq!(cell.models.len(), 2);
            assert_ne!(cell.models[0], cell.models[1]);
            let both = query.enumerate_cell(&[], &["x".into(), "y".into()], 3, Limits::default());
            assert_eq!(both.status, CellStatus::Complete);
            assert_eq!(both.models.len(), 3);
            assert_eq!(
                query
                    .solve(&[selected(0, true)], Limits::default(), 1)
                    .verdict,
                Verdict::Sat
            );
            assert_eq!(
                query
                    .solve(&[selected(0, false)], Limits::default(), 1)
                    .verdict,
                Verdict::Sat
            );
        }
    }

    #[test]
    fn exact_limit_overflow_empty_and_unsat_are_distinct() {
        let x = var("x".into(), Sort::Bv(2));
        let query = PreparedQuery::new(
            &boolv(true),
            &Env::new(),
            &[eq(x.clone(), bv(2, 0))],
            Limits::default(),
        )
        .unwrap();
        for bound in 0..=4 {
            let cell = query.enumerate_cell(&[], &["x".into()], bound, Limits::default());
            assert_eq!(
                cell.status,
                if bound == 4 {
                    CellStatus::Complete
                } else {
                    CellStatus::TooLarge
                }
            );
            assert_eq!(cell.models.len(), if bound == 4 { 4 } else { 0 });
        }
        let empty = query.enumerate_cell(
            &[selected(0, true), selected(0, false)],
            &["x".into()],
            0,
            Limits::default(),
        );
        assert_eq!(empty.status, CellStatus::Complete);
        assert!(empty.models.is_empty());
        let unit = query.enumerate_cell(&[], &[], 1, Limits::default());
        assert_eq!(unit.status, CellStatus::Complete);
        assert_eq!(unit.models, vec![BTreeMap::new()]);
    }

    #[test]
    fn alias_and_constant_bits_do_not_duplicate_models() {
        let x = var("x".into(), Sort::Bv(3));
        let y = var("y".into(), Sort::Bv(3));
        let z = var("z".into(), Sort::Bv(3));
        let formula = and(eq(x, y), eq(z, bv(3, 5)));
        let query = PreparedQuery::new(&formula, &Env::new(), &[], Limits::default()).unwrap();
        let cell = query.enumerate_cell(
            &[],
            &["x".into(), "y".into(), "z".into()],
            8,
            Limits::default(),
        );
        assert_eq!(cell.status, CellStatus::Complete);
        assert_eq!(cell.models.len(), 8);
        for model in cell.models {
            assert_eq!(model["x"], model["y"]);
            assert_eq!(model["z"], Scalar::Bv { width: 3, value: 5 });
        }
    }

    #[test]
    fn xor_cells_cover_small_truth_tables_without_overlap() {
        let x = var("x".into(), Sort::Bool);
        let y = var("y".into(), Sort::Bool);
        let z = var("z".into(), Sort::Bool);
        let predicates = [
            node(Sort::Bool, "xor", vec![x.clone(), y.clone()]),
            node(Sort::Bool, "xor", vec![y.clone(), z.clone()]),
        ];
        for mask in 0..256u16 {
            let mut formula = boolv(false);
            for row in 0..8 {
                if mask & (1 << row) != 0 {
                    let cube = and(
                        and(
                            eq(x.clone(), boolv(row & 1 != 0)),
                            eq(y.clone(), boolv(row & 2 != 0)),
                        ),
                        eq(z.clone(), boolv(row & 4 != 0)),
                    );
                    formula = node(Sort::Bool, "or", vec![formula, cube]);
                }
            }
            let query =
                PreparedQuery::new(&formula, &Env::new(), &predicates, Limits::default()).unwrap();
            let mut found = BTreeSet::new();
            for target in 0..4 {
                let cell = query.enumerate_cell(
                    &[selected(0, target & 1 != 0), selected(1, target & 2 != 0)],
                    &["x".into(), "y".into(), "z".into()],
                    2,
                    Limits::default(),
                );
                assert_eq!(cell.status, CellStatus::Complete);
                for model in cell.models {
                    let row = usize::from(model["x"] == Scalar::Bool(true))
                        | (usize::from(model["y"] == Scalar::Bool(true)) << 1)
                        | (usize::from(model["z"] == Scalar::Bool(true)) << 2);
                    assert_ne!(mask & (1 << row), 0);
                    assert!(found.insert(row));
                }
            }
            assert_eq!(found.len(), mask.count_ones() as usize);
        }
    }

    #[test]
    fn wide_space_is_bounded_by_xor_cell_not_total_cardinality() {
        let x = var("x".into(), Sort::Bv(32));
        let bit = |i| {
            eq(
                node(Sort::Bv(1), format!("(_ extract {i} {i})"), vec![x.clone()]),
                bv(1, 1),
            )
        };
        let predicates: Vec<_> = (4..32)
            .map(|i| node(Sort::Bool, "xor", vec![bit(i), bit(i - 1)]))
            .collect();
        let query =
            PreparedQuery::new(&boolv(true), &Env::new(), &predicates, Limits::default()).unwrap();
        let assumptions: Vec<_> = (0..28).map(|i| selected(i, i % 3 == 0)).collect();
        let cell = query.enumerate_cell(&assumptions, &["x".into()], 16, Limits::default());
        assert_eq!(cell.status, CellStatus::Complete, "{:?}", cell.reason);
        assert_eq!(cell.models.len(), 16);
        let mut lows = BTreeSet::new();
        for model in cell.models {
            let Scalar::Bv { value, .. } = model["x"] else {
                panic!()
            };
            lows.insert(value & 15);
            for i in 4..32 {
                assert_eq!(
                    ((value >> i) ^ (value >> (i - 1))) & 1 != 0,
                    (i - 4) % 3 == 0
                );
            }
        }
        assert_eq!(lows.len(), 16);
        assert_eq!(
            query
                .enumerate_cell(&[], &["x".into()], 16, Limits::default())
                .status,
            CellStatus::TooLarge
        );
    }

    #[test]
    fn failures_clear_partial_results_and_leave_prepared_state_untouched() {
        let x = var("x".into(), Sort::Bv(3));
        let mut query = PreparedQuery::new(
            &boolv(true),
            &Env::from([("label".into(), x.clone())]),
            &[eq(x, bv(3, 3))],
            Limits::default(),
        )
        .unwrap();
        query.solve(&[], Limits::default(), 0);
        let original_clauses = query.sat.as_ref().unwrap().clauses.len();
        let full = query.enumerate_cell(&[], &["x".into()], 8, Limits::default());
        assert_eq!(full.status, CellStatus::Complete);
        for work in [0, full.work / 2, full.work - 1] {
            let cell = query.enumerate_cell(
                &[],
                &["x".into()],
                8,
                Limits {
                    max_work: work,
                    ..Limits::default()
                },
            );
            assert_eq!(cell.status, CellStatus::Unknown);
            assert!(cell.models.is_empty());
            assert_eq!(query.sat.as_ref().unwrap().clauses.len(), original_clauses);
        }
        for projection in [vec!["label".into()], vec!["x".into(), "x".into()]] {
            assert_eq!(
                query
                    .enumerate_cell(&[], &projection, 8, Limits::default())
                    .status,
                CellStatus::Unknown
            );
        }
        assert_eq!(
            query
                .enumerate_cell(&[selected(1, true)], &[], 1, Limits::default())
                .status,
            CellStatus::Unknown
        );
        assert_eq!(
            query
                .enumerate_cell(
                    &[],
                    &[],
                    1,
                    Limits {
                        timeout_ms: 0,
                        ..Limits::default()
                    }
                )
                .status,
            CellStatus::Unknown
        );
        assert_eq!(
            query
                .solve(&[selected(0, true)], Limits::default(), 0)
                .verdict,
            Verdict::Sat
        );
    }
}
