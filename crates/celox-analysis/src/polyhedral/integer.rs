//! Exact bounded integer feasibility with interval propagation and splitting.
//! Branching follows the objective order, so the first feasible leaf is the
//! lexicographic minimum. Clauses express nonzero nullspace components.

use super::{Error, Result, Work};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Constraint {
    pub coefficients: Vec<i128>,
    pub constant: i128,
}

impl Constraint {
    pub fn extent(&self, bounds: &[(i64, i64)]) -> Result<(i128, i128)> {
        let mut low = self.constant;
        let mut high = self.constant;
        for (&coefficient, &(a, b)) in self.coefficients.iter().zip(bounds) {
            let (a, b) = if coefficient < 0 { (b, a) } else { (a, b) };
            low = low
                .checked_add(
                    coefficient
                        .checked_mul(i128::from(a))
                        .ok_or(Error::ArithmeticOverflow)?,
                )
                .ok_or(Error::ArithmeticOverflow)?;
            high = high
                .checked_add(
                    coefficient
                        .checked_mul(i128::from(b))
                        .ok_or(Error::ArithmeticOverflow)?,
                )
                .ok_or(Error::ArithmeticOverflow)?;
        }
        Ok((low, high))
    }

    fn propagate(&self, bounds: &mut [(i64, i64)]) -> Result<bool> {
        let (_, maximum) = self.extent(bounds)?;
        if maximum < 0 {
            return Ok(false);
        }
        for (index, &coefficient) in self.coefficients.iter().enumerate() {
            if coefficient == 0 {
                continue;
            }
            // Recompute after tightening another coordinate; stale maxima
            // would still be safe but substantially weaken propagation.
            let (_, maximum) = self.extent(bounds)?;
            let (low, high) = bounds[index];
            let contribution = coefficient
                .checked_mul(i128::from(if coefficient > 0 { high } else { low }))
                .ok_or(Error::ArithmeticOverflow)?;
            let others = maximum
                .checked_sub(contribution)
                .ok_or(Error::ArithmeticOverflow)?;
            if coefficient > 0 {
                let numerator = others.checked_neg().ok_or(Error::ArithmeticOverflow)?;
                let minimum = numerator
                    .div_euclid(coefficient)
                    .checked_add(i128::from(numerator.rem_euclid(coefficient) != 0))
                    .ok_or(Error::ArithmeticOverflow)?;
                if minimum > i128::from(high) {
                    return Ok(false);
                }
                if minimum > i128::from(low) {
                    bounds[index].0 =
                        i64::try_from(minimum).map_err(|_| Error::ArithmeticOverflow)?;
                }
            } else {
                let maximum =
                    others.div_euclid(coefficient.checked_neg().ok_or(Error::ArithmeticOverflow)?);
                if maximum < i128::from(low) {
                    return Ok(false);
                }
                if maximum < i128::from(high) {
                    bounds[index].1 =
                        i64::try_from(maximum).map_err(|_| Error::ArithmeticOverflow)?;
                }
            }
        }
        Ok(true)
    }
}

pub(super) fn lex_min(
    constraints: &[Constraint],
    clauses: &[Vec<Constraint>],
    bounds: Vec<(i64, i64)>,
    order: &[usize],
    work: &mut Work,
) -> Result<Option<Vec<i64>>> {
    if bounds.iter().any(|(low, high)| low > high)
        || order.len() != bounds.len()
        || order
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            != (0..bounds.len()).collect()
        || constraints
            .iter()
            .chain(clauses.iter().flatten())
            .any(|c| c.coefficients.len() != bounds.len())
    {
        return Err(Error::Invalid("integer problem dimensions or bounds"));
    }
    search(constraints, clauses, bounds, order, work)
}

fn search(
    constraints: &[Constraint],
    clauses: &[Vec<Constraint>],
    mut bounds: Vec<(i64, i64)>,
    order: &[usize],
    work: &mut Work,
) -> Result<Option<Vec<i64>>> {
    work.spend()?;
    loop {
        let previous = bounds.clone();
        for constraint in constraints {
            if !constraint.propagate(&mut bounds)? {
                return Ok(None);
            }
        }
        for clause in clauses {
            let mut possible = None;
            let mut multiple = false;
            let mut satisfied = false;
            for constraint in clause {
                let (minimum, maximum) = constraint.extent(&bounds)?;
                if minimum >= 0 {
                    satisfied = true;
                    break;
                }
                if maximum >= 0 {
                    if possible.is_some() {
                        multiple = true;
                    }
                    possible = Some(constraint);
                }
            }
            if satisfied {
                continue;
            }
            let Some(constraint) = possible else {
                return Ok(None);
            };
            if !multiple && !constraint.propagate(&mut bounds)? {
                return Ok(None);
            }
        }
        if previous == bounds {
            break;
        }
        work.spend()?;
    }
    let Some(&variable) = order.iter().find(|&&i| bounds[i].0 != bounds[i].1) else {
        return Ok(Some(bounds.into_iter().map(|(value, _)| value).collect()));
    };
    let (low, high) = bounds[variable];
    let middle = i64::try_from(i128::from(low) + (i128::from(high) - i128::from(low)) / 2)
        .map_err(|_| Error::ArithmeticOverflow)?;
    let mut left = bounds.clone();
    left[variable].1 = middle;
    if let Some(solution) = search(constraints, clauses, left, order, work)? {
        return Ok(Some(solution));
    }
    bounds[variable].0 = middle.checked_add(1).ok_or(Error::ArithmeticOverflow)?;
    search(constraints, clauses, bounds, order, work)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn integer_optimum_is_not_a_rounded_rational_solution() {
        // 2x + 3y >= 7, x-y >= 0: lexicographic optimum (2,1).
        let constraints = vec![
            Constraint {
                coefficients: vec![2, 3],
                constant: -7,
            },
            Constraint {
                coefficients: vec![1, -1],
                constant: 0,
            },
        ];
        let mut work = Work {
            remaining: 1000,
            used: 0,
        };
        assert_eq!(
            lex_min(&constraints, &[], vec![(0, 8); 2], &[0, 1], &mut work).unwrap(),
            Some(vec![2, 1])
        );
    }
    #[test]
    fn agrees_with_exhaustive_search_for_signed_small_problems() {
        let mut seed = 19u64;
        for _ in 0..200 {
            let mut draw = || {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                ((seed >> 32) % 9) as i128 - 4
            };
            let constraints = (0..5)
                .map(|_| Constraint {
                    coefficients: (0..3).map(|_| draw()).collect(),
                    constant: draw(),
                })
                .collect::<Vec<_>>();
            let mut expected = None;
            'outer: for x in -2..=3 {
                for y in -2..=3 {
                    for z in -2..=3 {
                        if constraints.iter().all(|c| {
                            c.constant
                                + c.coefficients[0] * x
                                + c.coefficients[1] * y
                                + c.coefficients[2] * z
                                >= 0
                        }) {
                            expected = Some(vec![x as i64, y as i64, z as i64]);
                            break 'outer;
                        }
                    }
                }
            }
            let mut work = Work {
                remaining: 10_000,
                used: 0,
            };
            assert_eq!(
                lex_min(&constraints, &[], vec![(-2, 3); 3], &[0, 1, 2], &mut work).unwrap(),
                expected
            );
        }
    }
    #[test]
    fn a_disjunction_enforces_independence_and_budget_is_explicit() {
        let clause = vec![
            Constraint {
                coefficients: vec![1, -2],
                constant: -1,
            },
            Constraint {
                coefficients: vec![-1, 2],
                constant: -1,
            },
        ];
        let mut work = Work {
            remaining: 1000,
            used: 0,
        };
        assert_eq!(
            lex_min(&[], &[clause], vec![(0, 4); 2], &[0, 1], &mut work).unwrap(),
            Some(vec![0, 1])
        );
        work.remaining = 0;
        assert_eq!(
            lex_min(&[], &[], vec![(0, 4)], &[0], &mut work),
            Err(Error::WorkLimit)
        );
    }
}
