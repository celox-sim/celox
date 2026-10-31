//! Seeded affine XOR partitions, with bounded projected cell queries.
use super::*;
use crate::{boolv, bv, eq, node};
use std::collections::BTreeSet;

/// A prepared family of affine XOR rows over projected scalar bits.
///
/// Each row includes each bit and a constant with seed-selected coefficients.
/// With ideal independent random coefficients this is the standard affine XOR
/// hash family. The implementation uses a deterministic SplitMix64 PRNG, not a
/// source of cryptographic entropy. Rows may be zero or dependent: preserving
/// those cases is essential to the family; we do not silently repair them.
/// Prefix-preserving Gaussian elimination simplifies rows and transforms target
/// bits together, preserving exactly the original random cells.
///
/// `cell` selects a prefix of the rows and a target value for each selected row.
/// A caller can measure differently sized cells without recompiling the formula.
/// This is NOT a uniform sampler: choosing a cell and then a model arbitrarily
/// can bias results. A sampling policy must separately control cell acceptance,
/// target selection, and the distribution of models within accepted cells.
pub struct HashCells {
    query: PreparedQuery,
    projection: Vec<String>,
    rows: usize,
    transforms: Vec<Vec<usize>>,
    preparation_work: u64,
}

impl HashCells {
    /// Build at most `rows` XOR constraints. Projection values must be distinct
    /// Bool or 1..=64-bit variable terms. Context labels do not affect model keys.
    /// The single budget covers hash construction and query preparation.
    pub fn new(
        formula: &Term,
        projection: &Env,
        rows: usize,
        seed: u64,
        limits: Limits,
    ) -> Res<Self> {
        let mut budget = Budget {
            limits: limits.clone(),
            start: Instant::now(),
            work: 0,
            time_check_in: 0,
        };
        budget.tick(1)?;
        let mut created = 0usize;
        let mut names = BTreeSet::new();
        let mut bits = Vec::new();
        for variable in projection.values() {
            budget.tick(1)?;
            let name = variable
                .0
                .op
                .strip_prefix('@')
                .ok_or("hash projection requires variables")?;
            if !variable.0.args.is_empty()
                || !valid_sort(&variable.0.sort)
                || !names.insert(name.to_owned())
            {
                return Err("hash projection requires distinct supported scalar variables".into());
            }
            let width = match variable.0.sort {
                Sort::Bool => 1,
                Sort::Bv(width) => width,
                _ => unreachable!(),
            };
            if bits.len().saturating_add(width as usize) > limits.max_variables {
                return Err("hash projection bit budget exhausted".into());
            }
            for bit in 0..width {
                budget.tick(1)?;
                let term = if variable.0.sort == Sort::Bool {
                    variable.clone()
                } else {
                    reserve_terms(&mut created, 3, &budget)?;
                    eq(
                        node(
                            Sort::Bv(1),
                            format!("(_ extract {bit} {bit})"),
                            vec![variable.clone()],
                        ),
                        bv(1, 1),
                    )
                };
                bits.push(term);
            }
        }
        if rows > bits.len() || rows > limits.max_terms {
            return Err("hash row limit exceeds projected width or term budget".into());
        }
        let mut rng = seed;
        let mut predicates = Vec::new();
        let mut echelon: Vec<(Vec<bool>, bool, Vec<bool>, Option<usize>)> = Vec::new();
        let mut transforms = Vec::new();
        for index in 0..rows {
            budget.tick(1)?;
            let mut constant = next(&mut rng) & 1 != 0;
            let mut coefficients = Vec::new();
            for _ in &bits {
                budget.tick(1)?;
                coefficients.push(next(&mut rng) & 1 != 0);
            }
            budget.tick(rows as u64)?;
            let mut transform = vec![false; rows];
            transform[index] = true;
            // Only earlier rows participate: every prefix remains equivalent
            // to the same prefix of the original random affine hash family.
            for (previous, offset, mapping, pivot) in &echelon {
                budget.tick(1)?;
                if pivot.is_some_and(|pivot| coefficients[pivot]) {
                    for (bit, other) in coefficients.iter_mut().zip(previous) {
                        budget.tick(1)?;
                        *bit ^= other;
                    }
                    constant ^= offset;
                    for (bit, other) in transform.iter_mut().zip(mapping) {
                        budget.tick(1)?;
                        *bit ^= other;
                    }
                }
            }
            let mut pivot = None;
            let mut terms = Vec::new();
            reserve_terms(&mut created, 1, &budget)?;
            terms.push(boolv(constant));
            for (position, (&include, bit)) in coefficients.iter().zip(&bits).enumerate() {
                budget.tick(1)?;
                if include {
                    pivot.get_or_insert(position);
                    terms.push(bit.clone());
                }
            }
            predicates.push(parity(&terms, &mut budget, &mut created)?);
            transforms.push(
                transform
                    .iter()
                    .enumerate()
                    .filter_map(|(i, &bit)| bit.then_some(i))
                    .collect(),
            );
            echelon.push((coefficients, constant, transform, pivot));
        }
        budget.check_time()?;
        let mut remaining = limits;
        remaining.max_work -= budget.work;
        remaining.timeout_ms = remaining
            .timeout_ms
            .checked_sub(u64::try_from(budget.start.elapsed().as_millis()).unwrap_or(u64::MAX))
            .filter(|&ms| ms > 0)
            .ok_or("hash preparation time budget exhausted")?;
        let query = PreparedQuery::new(formula, projection, &predicates, remaining)?;
        budget.tick(query.preparation_work())?;
        budget.check_time()?;
        Ok(Self {
            query,
            projection: names.into_iter().collect(),
            rows,
            transforms,
            preparation_work: budget.work,
        })
    }

    pub fn preparation_work(&self) -> u64 {
        self.preparation_work
    }

    /// Targets select a prefix of the prepared XOR rows. Empty targets query
    /// the unpartitioned space with the same bounded model limit.
    pub fn cell(&self, targets: &[bool], max_models: usize, mut limits: Limits) -> Cell {
        let start = Instant::now();
        if targets.len() > self.rows
            || targets.len() as u64 >= limits.max_work
            || limits.timeout_ms == 0
        {
            return Cell {
                status: CellStatus::Unknown,
                models: Vec::new(),
                work: 0,
                reason: Some("hash targets exceed prepared rows or query budget".into()),
            };
        }
        let mut overhead = targets.len() as u64;
        let mut assumptions = Vec::new();
        for (predicate, mapping) in self.transforms.iter().take(targets.len()).enumerate() {
            overhead = overhead.saturating_add(mapping.len() as u64);
            if overhead >= limits.max_work
                || start.elapsed().as_millis() >= u128::from(limits.timeout_ms)
            {
                return Cell {
                    status: CellStatus::Unknown,
                    models: Vec::new(),
                    work: overhead,
                    reason: Some("hash target transformation budget exhausted".into()),
                };
            }
            let value = mapping
                .iter()
                .fold(false, |value, &index| value ^ targets[index]);
            assumptions.push(Assumption { predicate, value });
        }
        limits.max_work -= overhead;
        let timeout = limits.timeout_ms;
        let mut cell =
            self.query
                .enumerate_cell(&assumptions, &self.projection, max_models, limits);
        cell.work = cell.work.saturating_add(overhead);
        if start.elapsed().as_millis() >= u128::from(timeout) {
            cell.status = CellStatus::Unknown;
            cell.models.clear();
            cell.reason = Some("hash cell time budget exhausted".into());
        }
        cell
    }
}

fn next(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e3779b97f4a7c15);
    let mut value = *state;
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
    value ^ (value >> 31)
}

fn reserve_terms(created: &mut usize, amount: usize, budget: &Budget) -> Res<()> {
    *created = created
        .checked_add(amount)
        .ok_or("hash construction term overflow")?;
    if *created > budget.limits.max_terms {
        return Err("hash construction term budget exhausted".into());
    }
    Ok(())
}

fn parity(terms: &[Term], budget: &mut Budget, created: &mut usize) -> Res<Term> {
    budget.tick(1)?;
    if terms.len() == 1 {
        return Ok(terms[0].clone());
    }
    reserve_terms(created, 1, budget)?;
    let mid = terms.len() / 2;
    Ok(node(
        Sort::Bool,
        "xor",
        vec![
            parity(&terms[..mid], budget, created)?,
            parity(&terms[mid..], budget, created)?,
        ],
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::var;

    #[test]
    fn random_affine_cells_partition_and_reproduce_the_projected_domain() {
        let x = var("x".into(), Sort::Bv(3));
        let context = Env::from([("label".into(), x)]);
        let mut signatures = BTreeSet::new();
        for seed in 0..16 {
            let cells = HashCells::new(&boolv(true), &context, 2, seed, Limits::default()).unwrap();
            let same = HashCells::new(&boolv(true), &context, 2, seed, Limits::default()).unwrap();
            let mut found = BTreeSet::new();
            let mut signature = Vec::new();
            for target in 0..4 {
                let bits = [target & 1 != 0, target & 2 != 0];
                let cell = cells.cell(&bits, 8, Limits::default());
                assert_eq!(cell.status, CellStatus::Complete);
                assert_eq!(cell.models, same.cell(&bits, 8, Limits::default()).models);
                let mut values = Vec::new();
                for model in cell.models {
                    let Scalar::Bv { value, .. } = model["x"] else {
                        panic!()
                    };
                    assert!(found.insert(value));
                    values.push(value);
                }
                values.sort();
                signature.push(values);
            }
            assert_eq!(found.len(), 8);
            signatures.insert(signature);
            assert_eq!(
                cells.cell(&[], 4, Limits::default()).status,
                CellStatus::TooLarge
            );
            assert_eq!(
                cells.cell(&[false; 3], 8, Limits::default()).status,
                CellStatus::Unknown
            );
        }
        assert!(signatures.len() > 1);
    }

    #[test]
    fn elimination_preserves_every_original_affine_prefix_and_target() {
        let context = Env::from([("x".into(), var("x".into(), Sort::Bv(3)))]);
        for seed in 0..32 {
            let cells = HashCells::new(&boolv(true), &context, 3, seed, Limits::default()).unwrap();
            let mut rng = seed;
            let original: Vec<_> = (0..3)
                .map(|_| {
                    let constant = next(&mut rng) & 1 != 0;
                    let mask: Vec<_> = (0..3).map(|_| next(&mut rng) & 1 != 0).collect();
                    (constant, mask)
                })
                .collect();
            for prefix in 0..=3 {
                for target in 0..(1 << prefix) {
                    let targets: Vec<_> = (0..prefix).map(|i| target & (1 << i) != 0).collect();
                    let cell = cells.cell(&targets, 8, Limits::default());
                    assert_eq!(cell.status, CellStatus::Complete);
                    let actual: BTreeSet<_> = cell
                        .models
                        .iter()
                        .map(|model| {
                            let Scalar::Bv { value, .. } = model["x"] else {
                                panic!()
                            };
                            value
                        })
                        .collect();
                    let expected: BTreeSet<_> = (0..8u64)
                        .filter(|value| {
                            original
                                .iter()
                                .zip(&targets)
                                .all(|((constant, mask), target)| {
                                    mask.iter().enumerate().fold(
                                        *constant,
                                        |parity, (i, &include)| {
                                            parity ^ (include && value & (1 << i) != 0)
                                        },
                                    ) == *target
                                })
                        })
                        .collect();
                    assert_eq!(
                        actual, expected,
                        "seed={seed}, prefix={prefix}, target={target}"
                    );
                }
            }
        }
    }

    #[test]
    fn construction_and_cell_budgets_fail_closed() {
        let context = Env::from([("x".into(), var("x".into(), Sort::Bv(8)))]);
        assert!(HashCells::new(&boolv(true), &context, 9, 0, Limits::default()).is_err());
        assert!(
            HashCells::new(
                &boolv(true),
                &context,
                3,
                0,
                Limits {
                    max_work: 1,
                    ..Limits::default()
                }
            )
            .is_err()
        );
        assert!(
            HashCells::new(
                &boolv(true),
                &context,
                3,
                0,
                Limits {
                    timeout_ms: 0,
                    ..Limits::default()
                }
            )
            .is_err()
        );
        assert!(
            HashCells::new(
                &boolv(true),
                &context,
                3,
                0,
                Limits {
                    max_terms: 2,
                    ..Limits::default()
                }
            )
            .is_err()
        );
        let hash = HashCells::new(&boolv(true), &context, 3, 0, Limits::default()).unwrap();
        assert!(hash.preparation_work() > 0);
        let failed = hash.cell(
            &[true],
            8,
            Limits {
                max_work: 2,
                ..Limits::default()
            },
        );
        assert_eq!(failed.status, CellStatus::Unknown);
        assert!(failed.models.is_empty());
        let duplicates = Env::from([
            ("x".into(), var("x".into(), Sort::Bool)),
            ("alias".into(), var("x".into(), Sort::Bool)),
        ]);
        assert!(HashCells::new(&boolv(true), &duplicates, 1, 0, Limits::default()).is_err());
    }
}
