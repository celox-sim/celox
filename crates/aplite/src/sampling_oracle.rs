//! Test-only bounded enumeration oracle for projected scalar solution sets.
//! Never use whole-space enumeration on the runtime randomization path.
use crate::{
    Env, Limits, Outcome, Scalar, SearchHint, Sort, Term, Verdict, and, boolv, bv, eq, not,
    solve_with_hint,
};
use std::{collections::BTreeSet, time::Instant};

/// One budget for the complete enumeration, not a fresh budget per model.
#[derive(Clone, Debug)]
pub struct SamplingLimits {
    pub solver: Limits,
    /// Maximum number of distinct projected models. One final UNSAT query is
    /// permitted to establish that this limit exactly covers the solution set.
    pub max_models: u64,
}

impl Default for SamplingLimits {
    fn default() -> Self {
        Self {
            solver: Limits::default(),
            max_models: 256,
        }
    }
}

#[derive(Debug)]
pub struct Sample {
    pub verdict: Verdict,
    /// Present only after complete enumeration of a nonempty solution set.
    pub model: Option<Outcome>,
    /// Distinct projected assignments processed within the model-count limit.
    pub models: u64,
    pub work: u64,
    pub reason: Option<String>,
}

/// Uniform reservoir sampling over distinct assignments to `projection`.
///
/// Projection values must be distinct Boolean or 1..=64-bit variable terms;
/// labels are arbitrary. Variables outside the projection are existentially
/// quantified: the number of their witnesses does not weight a projected value.
/// An empty projection has exactly one assignment if the formula is satisfiable.
///
/// This enumerates all projected models and is intended for small domains.
/// It returns no sample on exhaustion or unsupported input. The PRNG is local
/// SplitMix64 and bounded draws use rejection to avoid modulo bias. As with any
/// seeded PRNG, this is not a cryptographic entropy source. Reproducibility is
/// within a solver version, for identical inputs and sufficient budgets.
/// Arrays, weighting, and variable-order distribution policies are unsupported.
pub fn sample_uniform(
    formula: &Term,
    projection: &Env,
    limits: SamplingLimits,
    seed: u64,
) -> Sample {
    let start = Instant::now();
    let mut result = Sample {
        verdict: Verdict::Unknown,
        model: None,
        models: 0,
        work: 0,
        reason: None,
    };
    let mut selected = None;
    let mut rng = seed;
    let run = (|| -> Result<Verdict, String> {
        if formula.contains_memory() {
            return Err("sampling supports scalar formulas only".into());
        }
        let mut names = BTreeSet::new();
        for term in projection.values() {
            charge(&mut result.work, 1, limits.solver.max_work)?;
            let Some(name) = term.0.op.strip_prefix('@') else {
                return Err("sampling projection must contain variables".into());
            };
            if !term.0.args.is_empty() || !matches!(term.0.sort, Sort::Bool | Sort::Bv(1..=64)) {
                return Err(
                    "sampling projection requires scalar variables of supported width".into(),
                );
            }
            if !names.insert(name) {
                return Err("sampling projection contains a duplicate variable".into());
            }
        }
        let mut clauses = vec![formula.clone()];
        loop {
            // Rebuild a balanced conjunction so enumeration does not consume
            // the depth limit just by accumulating blocking clauses.
            charge(
                &mut result.work,
                clauses.len() as u64,
                limits.solver.max_work,
            )?;
            let query = conjunction(&clauses);
            let remaining_ms = limits
                .solver
                .timeout_ms
                .checked_sub(u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX))
                .filter(|ms| *ms > 0)
                .ok_or("sampling timeout exhausted")?;
            let mut solver_limits = limits.solver.clone();
            solver_limits.max_work = limits.solver.max_work - result.work;
            solver_limits.timeout_ms = remaining_ms;
            let model = solve_with_hint(&query, projection, solver_limits, SearchHint::Sat);
            charge(&mut result.work, model.stats.work, limits.solver.max_work)?;
            match model.verdict {
                Verdict::Unknown => {
                    return Err(model
                        .reason
                        .unwrap_or_else(|| "sampling solver returned Unknown".into()));
                }
                Verdict::Unsat => {
                    return Ok(if result.models == 0 {
                        Verdict::Unsat
                    } else {
                        Verdict::Sat
                    });
                }
                Verdict::Sat => {}
            }
            if result.models == limits.max_models {
                return Err("sampling model limit exhausted".into());
            }
            result.models += 1;
            let mut equalities = Vec::with_capacity(projection.len());
            for (label, term) in projection {
                charge(&mut result.work, 1, limits.solver.max_work)?;
                let value = match &model.context_values[label] {
                    Scalar::Bool(value) => boolv(*value),
                    Scalar::Bv { width, value } => bv(*width, *value),
                };
                equalities.push(eq(term.clone(), value));
            }
            clauses.push(not(conjunction(&equalities)));
            // Reservoir step: after k models each one is retained with 1/k
            // probability, regardless of the SAT solver's enumeration order.
            if draw_below(
                &mut rng,
                result.models,
                &mut result.work,
                limits.solver.max_work,
            )? == 0
            {
                selected = Some(model);
            }
        }
    })();
    match run {
        Ok(verdict) if start.elapsed().as_millis() < u128::from(limits.solver.timeout_ms) => {
            result.verdict = verdict;
            if verdict == Verdict::Sat {
                result.model = selected;
            }
        }
        Ok(_) => result.reason = Some("sampling timeout exhausted".into()),
        Err(reason) => result.reason = Some(reason),
    }
    result
}

fn charge(work: &mut u64, amount: u64, limit: u64) -> Result<(), String> {
    *work = work.checked_add(amount).ok_or("sampling work overflow")?;
    if *work > limit {
        return Err("sampling work limit exhausted".into());
    }
    Ok(())
}

fn conjunction(terms: &[Term]) -> Term {
    match terms.len() {
        0 => boolv(true),
        1 => terms[0].clone(),
        n => and(conjunction(&terms[..n / 2]), conjunction(&terms[n / 2..])),
    }
}

fn draw_below(state: &mut u64, bound: u64, work: &mut u64, limit: u64) -> Result<u64, String> {
    let threshold = bound.wrapping_neg() % bound;
    loop {
        charge(work, 1, limit)?;
        *state = state.wrapping_add(0x9e3779b97f4a7c15);
        let mut value = *state;
        value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
        value ^= value >> 31;
        if value >= threshold {
            return Ok(value % bound);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{node, var};

    #[test]
    fn projected_models_are_not_weighted_by_hidden_witnesses() {
        let x = var("x".into(), Sort::Bool);
        let y = var("y".into(), Sort::Bool);
        // x=false has two y witnesses, x=true has one. Projecting x must
        // enumerate two models, while projecting both must enumerate three.
        let formula = node(Sort::Bool, "or", vec![not(x.clone()), y.clone()]);
        let projection = Env::from([("chosen".into(), x.clone())]);
        let both = Env::from([("x".into(), x), ("y".into(), y)]);
        let mut counts = [0; 2];
        for seed in 0..512 {
            let sample = sample_uniform(&formula, &projection, SamplingLimits::default(), seed);
            assert_eq!(sample.verdict, Verdict::Sat);
            assert_eq!(sample.models, 2);
            let model = sample.model.unwrap();
            assert!(model.original_formula_validated);
            let Scalar::Bool(x) = model.context_values["chosen"] else {
                panic!()
            };
            counts[usize::from(x)] += 1;
        }
        // Fixed seeds, broad regression threshold: distinguish 1/2 from the
        // 1/3 weighting caused by accidentally enumerating hidden witnesses.
        assert!(
            counts.iter().all(|count| (210..302).contains(count)),
            "{counts:?}"
        );
        assert_eq!(
            sample_uniform(&formula, &both, SamplingLimits::default(), 0).models,
            3
        );
    }

    #[test]
    fn exact_model_limit_allows_final_unsat_check_but_never_partial_samples() {
        let x = var("x".into(), Sort::Bv(2));
        let projection = Env::from([("x".into(), x)]);
        for limit in 0..=4 {
            let result = sample_uniform(
                &boolv(true),
                &projection,
                SamplingLimits {
                    max_models: limit,
                    ..SamplingLimits::default()
                },
                42,
            );
            assert_eq!(result.models, limit);
            if limit < 4 {
                assert_eq!(result.verdict, Verdict::Unknown);
                assert!(result.model.is_none());
                assert!(result.reason.unwrap().contains("model limit"));
            } else {
                assert_eq!(result.verdict, Verdict::Sat);
                assert!(result.model.is_some());
            }
        }
    }

    #[test]
    fn empty_projection_and_unsat_have_exact_cardinality() {
        for (formula, verdict, count) in [
            (boolv(true), Verdict::Sat, 1),
            (boolv(false), Verdict::Unsat, 0),
        ] {
            let sample = sample_uniform(&formula, &Env::new(), SamplingLimits::default(), 0);
            assert_eq!(sample.verdict, verdict);
            assert_eq!(sample.models, count);
            assert_eq!(sample.model.is_some(), verdict == Verdict::Sat);
        }
    }

    #[test]
    fn work_budget_is_global_and_seed_is_reproducible() {
        let projection = Env::from([("x".into(), var("x".into(), Sort::Bv(3)))]);
        let a = sample_uniform(&boolv(true), &projection, SamplingLimits::default(), 13);
        let b = sample_uniform(&boolv(true), &projection, SamplingLimits::default(), 13);
        assert_eq!(a.verdict, Verdict::Sat);
        assert_eq!(a.models, 8);
        assert_eq!(a.work, b.work);
        assert_eq!(a.model.unwrap().assignments, b.model.unwrap().assignments);
        for budget in [0, a.work - 1] {
            let limited = sample_uniform(
                &boolv(true),
                &projection,
                SamplingLimits {
                    solver: Limits {
                        max_work: budget,
                        ..Limits::default()
                    },
                    ..SamplingLimits::default()
                },
                13,
            );
            assert_eq!(limited.verdict, Verdict::Unknown);
            assert!(limited.model.is_none());
        }
        let timed_out = sample_uniform(
            &boolv(true),
            &projection,
            SamplingLimits {
                solver: Limits {
                    timeout_ms: 0,
                    ..Limits::default()
                },
                ..SamplingLimits::default()
            },
            13,
        );
        assert_eq!(timed_out.verdict, Verdict::Unknown);
        assert!(timed_out.model.is_none());
    }

    #[test]
    fn malformed_projection_and_unsupported_formula_fail_closed() {
        let x = var("x".into(), Sort::Bool);
        for projection in [
            Env::from([("literal".into(), boolv(true))]),
            Env::from([("x".into(), x.clone()), ("alias".into(), x)]),
            Env::from([("wide".into(), var("wide".into(), Sort::Bv(65)))]),
            Env::from([("m".into(), var("m".into(), Sort::Mem(4, 8)))]),
        ] {
            let result = sample_uniform(&boolv(true), &projection, SamplingLimits::default(), 0);
            assert_eq!(result.verdict, Verdict::Unknown);
            assert!(result.model.is_none());
        }
        let formula = node(Sort::Bool, "unsupported", vec![]);
        let result = sample_uniform(&formula, &Env::new(), SamplingLimits::default(), 0);
        assert_eq!(result.verdict, Verdict::Unknown);
        assert!(result.model.is_none());
    }

    #[test]
    fn all_two_boolean_truth_tables_match_exhaustive_counts() {
        let x = var("x".into(), Sort::Bool);
        let y = var("y".into(), Sort::Bool);
        let projection = Env::from([("x".into(), x.clone()), ("y".into(), y.clone())]);
        for mask in 0..16u8 {
            let mut formula = boolv(false);
            for row in 0..4u8 {
                if mask & (1 << row) != 0 {
                    let case = and(
                        eq(x.clone(), boolv(row & 1 != 0)),
                        eq(y.clone(), boolv(row & 2 != 0)),
                    );
                    formula = node(Sort::Bool, "or", vec![formula, case]);
                }
            }
            for seed in 0..8 {
                let result = sample_uniform(&formula, &projection, SamplingLimits::default(), seed);
                assert_eq!(result.models, u64::from(mask.count_ones()));
                assert_eq!(
                    result.verdict,
                    if mask == 0 {
                        Verdict::Unsat
                    } else {
                        Verdict::Sat
                    }
                );
                if let Some(model) = result.model {
                    let Scalar::Bool(x) = model.context_values["x"] else {
                        panic!()
                    };
                    let Scalar::Bool(y) = model.context_values["y"] else {
                        panic!()
                    };
                    assert_ne!(mask & (1 << (u8::from(x) | u8::from(y) << 1)), 0);
                }
            }
        }
    }
}
