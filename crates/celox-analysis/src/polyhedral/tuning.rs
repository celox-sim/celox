//! Bounded numeric tuning after schedule selection, following Verma et al.
//! (arXiv:2609.03114), sections 3.2–3.4. This searches numeric coordinates,
//! never changes the transformation structure, and delegates legality and
//! measurement to the caller. It has no clock, compiler or runtime dependency.
//!
//! Neighbors are pooled across all coordinates. A larger graph radius enables
//! expanded neighborhoods; optional refinement moves by each axis's smallest
//! positive coarse value, including values absent from the original grid.
//! Our explicit statistical convention is a one-sided, exact permutation
//! Mann–Whitney test, including ties. With three samples, the smallest possible
//! one-sided p is 1/20 (a two-sided 5% test could never accept an improvement).

use super::{Error, Result};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug)]
pub struct Parameter {
    /// Strictly increasing positive values; first/last also bound refinement.
    pub values: Vec<u64>,
}

#[derive(Clone, Debug)]
pub struct TuningOptions {
    pub samples: usize,
    pub radius: usize,
    pub shortest_hop: bool,
    pub max_steps: usize,
    pub max_evaluations: usize,
    pub alpha: Probability,
}

impl Default for TuningOptions {
    fn default() -> Self {
        Self {
            samples: 3,
            radius: 2,
            shortest_hop: true,
            max_steps: 100,
            max_evaluations: 64,
            alpha: Probability {
                numerator: 1,
                denominator: 20,
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Probability {
    pub numerator: u64,
    pub denominator: u64,
}

impl Probability {
    fn at_most(self, other: Self) -> bool {
        u128::from(self.numerator) * u128::from(other.denominator)
            <= u128::from(other.numerator) * u128::from(self.denominator)
    }
}

/// Conditional permutation probability of a rank sum no greater than the
/// candidate's. Twice the average rank represents ties without floating point.
/// Each group is limited to 16 observations; the largest count is C(32,16).
pub fn faster_probability(candidate: &[u64], incumbent: &[u64]) -> Result<Probability> {
    if candidate.is_empty() || incumbent.is_empty() || candidate.len() > 16 || incumbent.len() > 16
    {
        return Err(Error::Invalid("rank test sample count"));
    }
    let n = candidate.len();
    let mut pooled = candidate
        .iter()
        .map(|&v| (v, true))
        .chain(incumbent.iter().map(|&v| (v, false)))
        .collect::<Vec<_>>();
    pooled.sort_unstable_by_key(|&(v, _)| v);
    let mut ranks = vec![0; pooled.len()];
    let mut observed = 0;
    let mut start = 0;
    while start < pooled.len() {
        let mut end = start + 1;
        while end < pooled.len() && pooled[end].0 == pooled[start].0 {
            end += 1;
        }
        let rank = start + end + 1;
        for i in start..end {
            ranks[i] = rank;
            if pooled[i].1 {
                observed += rank;
            }
        }
        start = end;
    }
    let limit = n * pooled.len() * 2;
    let mut counts = vec![vec![0u64; limit + 1]; n + 1];
    counts[0][0] = 1;
    for (index, rank) in ranks.into_iter().enumerate() {
        for size in (1..=n.min(index + 1)).rev() {
            for sum in rank..=limit {
                counts[size][sum] += counts[size - 1][sum - rank];
            }
        }
    }
    Ok(Probability {
        numerator: counts[n][..=observed].iter().sum(),
        denominator: counts[n].iter().sum(),
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Coarse,
    ShortestHop,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    Converged,
    EvaluationLimit,
    StepLimit,
}

#[derive(Clone, Debug)]
pub struct Selection {
    pub point: Vec<u64>,
    pub samples: Vec<u64>,
    pub phase: Phase,
    pub probability: Option<Probability>,
}

#[derive(Clone, Debug)]
pub struct TuningResult {
    /// The seed followed by statistically accepted improvements only.
    pub trajectory: Vec<Selection>,
    pub evaluations: usize,
    pub rejected: usize,
    pub cache_hits: usize,
    pub stop: Stop,
}

impl TuningResult {
    pub fn best(&self) -> &Selection {
        self.trajectory.last().unwrap()
    }
}

fn median(samples: &[u64]) -> u64 {
    let mut ordered = samples.to_vec();
    ordered.sort_unstable();
    ordered[ordered.len() / 2]
}

fn neighbors(space: &[Parameter], point: &[u64], radius: usize, phase: Phase) -> Vec<Vec<u64>> {
    let mut seen = BTreeSet::from([point.to_vec()]);
    let mut frontier = vec![point.to_vec()];
    for _ in 0..radius {
        let mut next = Vec::new();
        for current in frontier {
            for (axis, parameter) in space.iter().enumerate() {
                let values = &parameter.values;
                let adjacent = match phase {
                    Phase::Coarse => {
                        let index = values.binary_search(&current[axis]).unwrap();
                        [
                            index.checked_sub(1).map(|i| values[i]),
                            values.get(index + 1).copied(),
                        ]
                    }
                    Phase::ShortestHop => [
                        current[axis].checked_sub(values[0]),
                        current[axis].checked_add(values[0]),
                    ],
                };
                for value in adjacent.into_iter().flatten() {
                    if value < values[0] || value > *values.last().unwrap() {
                        continue;
                    }
                    let mut candidate = current.clone();
                    candidate[axis] = value;
                    if seen.insert(candidate.clone()) {
                        next.push(candidate);
                    }
                }
            }
        }
        frontier = next;
    }
    seen.remove(point);
    seen.into_iter().collect()
}

/// Best-fit coordinate hill climbing. `None` from the oracle marks an illegal,
/// unsupported or otherwise rejected candidate and is cached. Successful
/// measurements must contain exactly `options.samples` positive costs; smaller
/// is better. The callback must validate candidate semantics before timing it.
/// Cached samples guide search only: a caller should remeasure the chosen unit
/// against its original optimized baseline with fresh, interleaved samples.
pub fn hill_climb(
    space: &[Parameter],
    seed: &[u64],
    options: &TuningOptions,
    mut measure: impl FnMut(&[u64]) -> Option<Vec<u64>>,
) -> Result<TuningResult> {
    if space.is_empty()
        || space.len() > 8
        || space.len() != seed.len()
        || !(1..=3).contains(&options.radius)
        || !(3..=15).contains(&options.samples)
        || options.samples.is_multiple_of(2)
        || options.max_evaluations == 0
        || options.alpha.numerator == 0
        || options.alpha.denominator == 0
        || options.alpha.numerator >= options.alpha.denominator
        || space.iter().zip(seed).any(|(p, s)| {
            p.values.is_empty()
                || p.values.len() > 1024
                || p.values[0] == 0
                || p.values.windows(2).any(|v| v[0] >= v[1])
                || p.values.binary_search(s).is_err()
        })
    {
        return Err(Error::Invalid("numeric tuning configuration"));
    }
    let validate = |samples: Vec<u64>| {
        if samples.len() != options.samples || samples.contains(&0) {
            Err(Error::Invalid("numeric tuning measurements"))
        } else {
            Ok(samples)
        }
    };
    let samples = validate(measure(seed).ok_or(Error::Invalid("numeric tuning seed rejected"))?)?;
    let mut cache = BTreeMap::from([(seed.to_vec(), Some(samples.clone()))]);
    let mut result = TuningResult {
        trajectory: vec![Selection {
            point: seed.to_vec(),
            samples,
            phase: Phase::Coarse,
            probability: None,
        }],
        evaluations: 1,
        rejected: 0,
        cache_hits: 0,
        stop: Stop::Converged,
    };
    let phases = if options.shortest_hop {
        vec![Phase::Coarse, Phase::ShortestHop]
    } else {
        vec![Phase::Coarse]
    };
    for phase in phases {
        loop {
            if result.trajectory.len() > options.max_steps {
                result.stop = Stop::StepLimit;
                return Ok(result);
            }
            let current = result.best().clone();
            let radius = if phase == Phase::Coarse {
                options.radius
            } else {
                1
            };
            let mut best: Option<Selection> = None;
            let mut exhausted = false;
            for point in neighbors(space, &current.point, radius, phase) {
                let samples = if let Some(cached) = cache.get(&point) {
                    result.cache_hits += 1;
                    cached.clone()
                } else {
                    if result.evaluations == options.max_evaluations {
                        exhausted = true;
                        break;
                    }
                    result.evaluations += 1;
                    let samples = measure(&point).map(&validate).transpose()?;
                    if samples.is_none() {
                        result.rejected += 1;
                    }
                    cache.insert(point.clone(), samples.clone());
                    samples
                };
                let Some(samples) = samples else {
                    continue;
                };
                if median(&samples) >= median(&current.samples) {
                    continue;
                }
                let probability = faster_probability(&samples, &current.samples)?;
                if probability.at_most(options.alpha)
                    && best
                        .as_ref()
                        .is_none_or(|b| (median(&samples), &point) < (median(&b.samples), &b.point))
                {
                    best = Some(Selection {
                        point,
                        samples,
                        phase,
                        probability: Some(probability),
                    });
                }
            }
            let improved = best.is_some();
            if let Some(best) = best {
                result.trajectory.push(best);
            }
            if exhausted {
                result.stop = Stop::EvaluationLimit;
                return Ok(result);
            }
            if !improved {
                break;
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn axis(values: &[u64]) -> Parameter {
        Parameter {
            values: values.to_vec(),
        }
    }
    fn samples(cost: u64) -> Option<Vec<u64>> {
        Some(vec![cost, cost + 1, cost + 2])
    }

    #[test]
    fn exact_rank_probabilities_match_label_permutations_including_ties() {
        let probability = faster_probability(&[1, 2, 3], &[4, 5, 6]).unwrap();
        assert_eq!(
            probability,
            Probability {
                numerator: 1,
                denominator: 20
            }
        );
        for pattern in 0..729usize {
            let mut digits = pattern;
            let mut values = [0; 6];
            for value in &mut values {
                *value = (digits % 3) as u64;
                digits /= 3;
            }
            let score = |mask: u32| {
                let mut twice_u = 0;
                for i in 0..6 {
                    for j in 0..6 {
                        if mask & (1 << i) != 0 && mask & (1 << j) == 0 {
                            twice_u += if values[i] > values[j] {
                                2
                            } else {
                                u32::from(values[i] == values[j])
                            };
                        }
                    }
                }
                twice_u
            };
            let observed = score(7);
            let expected = (0..64u32)
                .filter(|m| m.count_ones() == 3 && score(*m) <= observed)
                .count() as u64;
            assert_eq!(
                faster_probability(&values[..3], &values[3..]).unwrap(),
                Probability {
                    numerator: expected,
                    denominator: 20
                }
            );
        }
    }

    #[test]
    fn chooses_across_all_coordinates_and_can_cross_a_worse_neighbor() {
        let space = [axis(&[1, 2]), axis(&[1, 2])];
        let immediate = TuningOptions {
            radius: 1,
            shortest_hop: false,
            ..Default::default()
        };
        let result = hill_climb(&space, &[1, 1], &immediate, |p| {
            samples(1000 - 100 * p[0] - 200 * p[1])
        })
        .unwrap();
        assert_eq!(result.trajectory[1].point, [1, 2]);
        let valley = |p: &[u64]| {
            samples(if p == [1, 1] {
                200
            } else if p == [2, 2] {
                100
            } else {
                300
            })
        };
        assert_eq!(
            hill_climb(&space, &[1, 1], &immediate, valley)
                .unwrap()
                .best()
                .point,
            [1, 1]
        );
        let expanded = TuningOptions {
            radius: 2,
            ..immediate
        };
        assert_eq!(
            hill_climb(&space, &[1, 1], &expanded, valley)
                .unwrap()
                .best()
                .point,
            [2, 2]
        );
    }

    #[test]
    fn shortest_hops_find_values_outside_the_coarse_grid() {
        let options = TuningOptions {
            radius: 1,
            ..Default::default()
        };
        let result = hill_climb(&[axis(&[2, 4, 8, 16, 32])], &[16], &options, |p| {
            samples(100 + 10 * p[0].abs_diff(18).pow(2))
        })
        .unwrap();
        assert_eq!(result.best().point, [18]);
        assert_eq!(result.best().phase, Phase::ShortestHop);
        assert_eq!(result.stop, Stop::Converged);
    }

    #[test]
    fn respects_limits_caches_rejections_and_rejects_unreliable_measurements() {
        let space = [axis(&[1, 2, 4])];
        let options = TuningOptions {
            shortest_hop: false,
            ..Default::default()
        };
        let mut visited = BTreeSet::new();
        let result = hill_climb(&space, &[1], &options, |p| {
            assert!(visited.insert(p.to_vec()));
            if p[0] == 2 {
                None
            } else {
                samples(1000 / p[0])
            }
        })
        .unwrap();
        assert_eq!(result.best().point, [4]);
        assert_eq!(result.evaluations, 3);
        assert_eq!(result.rejected, 1);
        assert!(result.cache_hits > 0);
        let bounded = TuningOptions {
            max_evaluations: 1,
            ..options.clone()
        };
        let result = hill_climb(&space, &[1], &bounded, |p| samples(1000 / p[0])).unwrap();
        assert_eq!(result.best().point, [1]);
        assert_eq!(result.stop, Stop::EvaluationLimit);
        let bounded = TuningOptions {
            max_steps: 0,
            ..options.clone()
        };
        assert_eq!(
            hill_climb(&space, &[1], &bounded, |_| samples(100))
                .unwrap()
                .stop,
            Stop::StepLimit
        );
        let result = hill_climb(&space, &[1], &options, |p| {
            Some(vec![100 - p[0], 200 - p[0], 300 - p[0]])
        })
        .unwrap();
        assert_eq!(result.best().point, [1]);
        assert!(hill_climb(&space, &[1], &options, |_| Some(vec![1])).is_err());
        assert!(hill_climb(&space, &[3], &options, |_| samples(1)).is_err());
        let strict = TuningOptions {
            alpha: Probability {
                numerator: 1,
                denominator: 100,
            },
            ..options
        };
        assert_eq!(
            hill_climb(&space, &[1], &strict, |p| samples(1000 / p[0]))
                .unwrap()
                .best()
                .point,
            [1]
        );
    }
}
