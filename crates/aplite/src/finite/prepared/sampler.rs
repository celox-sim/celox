//! UniGen2-style projected batch sampling. See algorithms 1 and 2 in
//! <https://people.eecs.berkeley.edu/~sseshia/pubdir/tacas15.pdf>
use super::*;

// kappa=0.638: pivot=ceil(4.03*(1+1/kappa)^2)=27,
// lo=floor(pivot/(sqrt(2)*(1+kappa)))=11,
// hi=ceil(1+sqrt(2)*(1+kappa)*pivot)=64.
const PIVOT: f64 = 27.;
const LOW: usize = 11;
const HIGH: usize = 64;
const ESTIMATE_BOUND: usize = 60;

type Model = BTreeMap<String, Scalar>;

enum Mode {
    Small(Vec<Model>),
    Hashed { bits: isize },
}

/// Outcome of one bounded batch request. SAT batches contain only validated
/// projected assignments. Unknown never exposes an incomplete batch.
#[derive(Debug)]
pub struct SampleBatch {
    pub verdict: Verdict,
    pub models: Vec<Model>,
    pub work: u64,
    pub cells: usize,
    pub reason: Option<String>,
}

/// Seeded approximate projected sampler following the UniGen2 two-stage policy.
///
/// A bounded probe handles at most 64 total models exactly. Larger spaces use
/// randomized XOR count estimation, then fresh hash families at the
/// estimated row count and its two predecessors. Only complete cells with
/// 11..=63 models are accepted; each returns 11 distinct randomly chosen models.
/// Small spaces return one uniformly selected model per call.
///
/// The large-space policy is approximate, not exact uniform or independent
/// sampling. The paper's ideal-randomness theorems are not a certification of this
/// seeded, resource-bounded implementation. There is no heuristic fallback on
/// failure. Unknown includes failed parameter estimation or cell acceptance,
/// and must not be translated into SystemVerilog randomize()==0.
///
/// Formula and projection are immutable snapshots. Preparation and each batch
/// have separate aggregate budgets. Seed replay requires identical call history,
/// solver version and budgets sufficient to finish. No global RNG is used.
pub struct Sampler {
    formula: Term,
    projection: Env,
    width: usize,
    rng: u64,
    mode: Mode,
    preparation_work: u64,
}

impl Sampler {
    pub fn new(formula: &Term, projection: &Env, seed: u64, limits: Limits) -> Res<Self> {
        let mut budget = new_budget(limits);
        budget.tick(1)?;
        let mut sampler = Self {
            formula: formula.clone(),
            projection: projection.clone(),
            width: 0,
            rng: seed,
            mode: Mode::Small(Vec::new()),
            preparation_work: 0,
        };
        // HashCells validates variable identity, widths and all source terms.
        let tiny = sampler.query_cell(0, HIGH, &mut budget)?;
        if tiny.status == CellStatus::Complete {
            sampler.mode = Mode::Small(tiny.models);
        } else {
            for term in projection.values() {
                budget.tick(1)?;
                sampler.width += match term.0.sort {
                    Sort::Bool => 1,
                    Sort::Bv(w) => w as usize,
                    _ => unreachable!(),
                };
            }
            let mut estimate = None;
            // Every attempt uses a fresh hash, as required by Algorithm 1.
            for rows in 1..=sampler.width {
                let cell = sampler.query_cell(rows, ESTIMATE_BOUND, &mut budget)?;
                if cell.status == CellStatus::Complete && !cell.models.is_empty() {
                    let bits = ((cell.models.len() as f64).log2() + rows as f64 + 1.8f64.log2()
                        - PIVOT.log2())
                    .round() as isize;
                    estimate = Some(bits);
                    break;
                }
            }
            sampler.mode = Mode::Hashed {
                bits: estimate.ok_or("sampling parameter estimation failed")?,
            };
        }
        budget.check_time()?;
        sampler.preparation_work = budget.work;
        Ok(sampler)
    }

    pub fn preparation_work(&self) -> u64 {
        self.preparation_work
    }

    pub fn sample_batch(&mut self, limits: Limits) -> SampleBatch {
        let mut budget = new_budget(limits);
        let mut result = SampleBatch {
            verdict: Verdict::Unknown,
            models: Vec::new(),
            work: 0,
            cells: 0,
            reason: None,
        };
        let run = (|| -> Res<Verdict> {
            budget.tick(1)?;
            match &self.mode {
                Mode::Small(models) => {
                    if models.is_empty() {
                        return Ok(Verdict::Unsat);
                    }
                    let index = draw(&mut self.rng, models.len(), &mut budget)?;
                    budget.tick(1 + models[index].len() as u64)?;
                    result.models.push(models[index].clone());
                }
                Mode::Hashed { bits } => {
                    let bits = *bits;
                    for rows in [bits - 2, bits - 1, bits] {
                        // Do not clamp an invalid estimate into a different hash
                        // policy. Skip undefined row counts and fail if none work.
                        if rows < 0 || rows as usize > self.width {
                            continue;
                        }
                        result.cells += 1;
                        let mut cell = self.query_cell(rows as usize, HIGH - 1, &mut budget)?;
                        if cell.status != CellStatus::Complete || cell.models.len() < LOW {
                            continue;
                        }
                        // Partial Fisher-Yates: unbiased bounded draws and no
                        // replacement, including order within the returned batch.
                        for index in 0..LOW {
                            let chosen = index
                                + draw(&mut self.rng, cell.models.len() - index, &mut budget)?;
                            cell.models.swap(index, chosen);
                        }
                        cell.models.truncate(LOW);
                        result.models = cell.models;
                        return Ok(Verdict::Sat);
                    }
                    return Err("sampling found no cell in the accepted size interval".into());
                }
            }
            Ok(Verdict::Sat)
        })();
        match run.and_then(|verdict| {
            budget.check_time()?;
            Ok(verdict)
        }) {
            Ok(verdict) => result.verdict = verdict,
            Err(reason) => {
                result.models.clear();
                result.reason = Some(reason);
            }
        }
        result.work = budget.work;
        result
    }

    fn query_cell(&mut self, rows: usize, bound: usize, budget: &mut Budget) -> Res<Cell> {
        budget.tick(1)?;
        let seed = next(&mut self.rng);
        let prepare_limits = remaining(budget)?;
        let cells =
            match HashCells::new(&self.formula, &self.projection, rows, seed, prepare_limits) {
                Ok(cells) => cells,
                Err(reason) => {
                    // Preparation's error API has no partial work count. Reserve
                    // the remaining work budget conservatively and abort, never retry.
                    budget.work = budget.limits.max_work;
                    return Err(reason);
                }
            };
        budget.tick(cells.preparation_work())?;
        let mut targets = Vec::new();
        for _ in 0..rows {
            budget.tick(1)?;
            targets.push(next(&mut self.rng) & 1 != 0);
        }
        let cell = cells.cell(&targets, bound, remaining(budget)?);
        budget.tick(cell.work)?;
        budget.check_time()?;
        if cell.status == CellStatus::Unknown {
            return Err(cell
                .reason
                .unwrap_or_else(|| "sampling cell query was incomplete".into()));
        }
        Ok(cell)
    }
}

fn new_budget(limits: Limits) -> Budget {
    Budget {
        limits,
        start: Instant::now(),
        work: 0,
        time_check_in: 0,
    }
}
fn remaining(budget: &Budget) -> Res<Limits> {
    budget.check_time()?;
    let mut limits = budget.limits.clone();
    limits.max_work = limits
        .max_work
        .checked_sub(budget.work)
        .ok_or("sampling work budget exhausted")?;
    limits.timeout_ms = limits
        .timeout_ms
        .checked_sub(u64::try_from(budget.start.elapsed().as_millis()).unwrap_or(u64::MAX))
        .filter(|&ms| ms > 0)
        .ok_or("sampling time budget exhausted")?;
    Ok(limits)
}
fn next(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e3779b97f4a7c15);
    let mut value = *state;
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
    value ^ (value >> 31)
}
fn draw(state: &mut u64, count: usize, budget: &mut Budget) -> Res<usize> {
    let count = count as u64;
    let threshold = count.wrapping_neg() % count;
    loop {
        budget.tick(1)?;
        let value = next(state);
        if value >= threshold {
            return Ok((value % count) as usize);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{boolv, bv, eq, node, var};
    use std::collections::BTreeSet;

    #[test]
    fn small_domain_distribution_replay_and_hidden_projection() {
        let x = var("x".into(), Sort::Bv(2));
        let hidden = var("hidden".into(), Sort::Bool);
        let formula = node(Sort::Bool, "or", vec![eq(x.clone(), bv(2, 0)), hidden]);
        let projection = Env::from([("label".into(), x)]);
        let mut a = Sampler::new(&formula, &projection, 42, Limits::default()).unwrap();
        let mut b = Sampler::new(&formula, &projection, 42, Limits::default()).unwrap();
        let mut counts = [0; 4];
        for _ in 0..2048 {
            let batch = a.sample_batch(Limits::default());
            assert_eq!(batch.verdict, Verdict::Sat);
            assert_eq!(batch.models, b.sample_batch(Limits::default()).models);
            let Scalar::Bv { value, .. } = batch.models[0]["x"] else {
                panic!()
            };
            counts[value as usize] += 1;
        }
        assert!(
            counts.iter().all(|&count| (400..625).contains(&count)),
            "{counts:?}"
        );
    }

    #[test]
    fn hashed_batches_are_complete_valid_unique_and_reproducible() {
        let projection = Env::from([("x".into(), var("x".into(), Sort::Bv(7)))]);
        let mut a = Sampler::new(&boolv(true), &projection, 7, Limits::default()).unwrap();
        let mut b = Sampler::new(&boolv(true), &projection, 7, Limits::default()).unwrap();
        assert!(matches!(a.mode, Mode::Hashed { .. }));
        let mut counts = [0; 128];
        let mut successes = 0;
        for _ in 0..64 {
            let batch = a.sample_batch(Limits::default());
            let replay = b.sample_batch(Limits::default());
            assert_eq!(batch.verdict, replay.verdict);
            assert_eq!(batch.models, replay.models);
            if batch.verdict == Verdict::Unknown {
                assert!(batch.models.is_empty());
                continue;
            }
            assert_eq!(batch.verdict, Verdict::Sat);
            assert_eq!(batch.models.len(), LOW);
            let mut distinct = BTreeSet::new();
            for model in batch.models {
                let Scalar::Bv { value, .. } = model["x"] else {
                    panic!()
                };
                assert!(value < 128);
                assert!(distinct.insert(value));
                counts[value as usize] += 1;
            }
            successes += 1;
        }
        assert!(successes > 32);
        assert!(counts.iter().filter(|&&n| n > 0).count() > 110);
    }

    #[test]
    fn unsat_unknown_and_empty_projection_are_distinct() {
        let mut empty = Sampler::new(&boolv(false), &Env::new(), 0, Limits::default()).unwrap();
        assert_eq!(
            empty.sample_batch(Limits::default()).verdict,
            Verdict::Unsat
        );
        let mut unit = Sampler::new(&boolv(true), &Env::new(), 0, Limits::default()).unwrap();
        assert_eq!(
            unit.sample_batch(Limits::default()).models,
            vec![BTreeMap::new()]
        );
        let failed = unit.sample_batch(Limits {
            max_work: 0,
            ..Limits::default()
        });
        assert_eq!(failed.verdict, Verdict::Unknown);
        assert!(failed.models.is_empty());
        assert_eq!(unit.sample_batch(Limits::default()).verdict, Verdict::Sat);
        assert!(
            Sampler::new(
                &boolv(true),
                &Env::new(),
                0,
                Limits {
                    timeout_ms: 0,
                    ..Limits::default()
                }
            )
            .is_err()
        );
    }

    #[test]
    fn batch_budget_is_global_and_failure_does_not_return_partial_models() {
        let projection = Env::from([("x".into(), var("x".into(), Sort::Bv(7)))]);
        let mut complete = Sampler::new(&boolv(true), &projection, 7, Limits::default()).unwrap();
        let batch = complete.sample_batch(Limits::default());
        assert_eq!(batch.verdict, Verdict::Sat);
        for work in [0, 5, batch.work - 1] {
            let mut limited =
                Sampler::new(&boolv(true), &projection, 7, Limits::default()).unwrap();
            let result = limited.sample_batch(Limits {
                max_work: work,
                ..Limits::default()
            });
            assert_eq!(result.verdict, Verdict::Unknown);
            assert!(result.models.is_empty());
        }
        let timed = complete.sample_batch(Limits {
            timeout_ms: 0,
            ..Limits::default()
        });
        assert_eq!(timed.verdict, Verdict::Unknown);
        assert!(timed.models.is_empty());
        assert!(
            Sampler::new(
                &boolv(true),
                &projection,
                7,
                Limits {
                    max_work: 100,
                    ..Limits::default()
                }
            )
            .is_err()
        );
    }

    #[test]
    fn constrained_large_branch_never_returns_outside_projected_support() {
        let x = var("x".into(), Sort::Bv(8));
        let formula = node(Sort::Bool, "bvult", vec![x.clone(), bv(8, 100)]);
        let projection = Env::from([("x".into(), x)]);
        let mut sampler = Sampler::new(&formula, &projection, 13, Limits::default()).unwrap();
        assert!(matches!(sampler.mode, Mode::Hashed { .. }));
        let mut seen = BTreeSet::new();
        let mut batches = 0;
        for _ in 0..64 {
            let batch = sampler.sample_batch(Limits::default());
            if batch.verdict == Verdict::Unknown {
                assert!(batch.models.is_empty());
                continue;
            }
            assert_eq!(batch.verdict, Verdict::Sat);
            batches += 1;
            for model in batch.models {
                let Scalar::Bv { value, .. } = model["x"] else {
                    panic!()
                };
                assert!(value < 100);
                seen.insert(value);
            }
        }
        assert!(batches > 32);
        assert!(seen.len() > 90);
    }

    #[test]
    fn permutation_batch_contains_valid_distinct_projected_assignments() {
        let variables: Vec<_> = (0..8).map(|i| var(format!("x{i}"), Sort::Bv(3))).collect();
        let mut formula = boolv(true);
        for i in 0..8 {
            for j in i + 1..8 {
                formula = crate::and(
                    formula,
                    crate::not(eq(variables[i].clone(), variables[j].clone())),
                );
            }
        }
        let context: Env = variables
            .into_iter()
            .enumerate()
            .map(|(i, term)| (format!("x{i}"), term))
            .collect();
        let limits = Limits {
            timeout_ms: 30_000,
            ..Limits::default()
        };
        let mut sampler = Sampler::new(&formula, &context, 42, limits.clone()).unwrap();
        let batch = sampler.sample_batch(limits);
        assert_eq!(batch.verdict, Verdict::Sat, "{:?}", batch.reason);
        assert_eq!(batch.models.len(), LOW);
        let mut assignments = BTreeSet::new();
        for model in batch.models {
            let values: Vec<_> = model
                .values()
                .map(|value| {
                    let Scalar::Bv { value, .. } = value else {
                        panic!()
                    };
                    *value
                })
                .collect();
            assert_eq!(
                values.iter().copied().collect::<BTreeSet<_>>(),
                (0..8).collect()
            );
            assert!(assignments.insert(values));
        }
    }

    #[test]
    fn wide_domain_is_sampled_without_full_enumeration() {
        let projection = Env::from([("x".into(), var("x".into(), Sort::Bv(32)))]);
        let mut sampler = Sampler::new(&boolv(true), &projection, 42, Limits::default()).unwrap();
        assert!(matches!(sampler.mode, Mode::Hashed { .. }));
        for _ in 0..4 {
            let batch = sampler.sample_batch(Limits::default());
            if batch.verdict == Verdict::Sat {
                assert_eq!(batch.models.len(), LOW);
                return;
            }
            assert!(batch.models.is_empty());
        }
        panic!("no complete batch for wide-domain smoke test");
    }
}
