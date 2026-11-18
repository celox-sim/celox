//! Coverage computation (IEEE 1800-2023 19.11).
//!
//! The coverage of a coverpoint is the share of its bins that are covered,
//! where a bin is covered with at least `option.at_least` hits and a bin
//! with no value counts on neither side (19.11.1). The coverage of a
//! covergroup is the average of that of its items by their weights. An item
//! without bins, and a covergroup whose weights add up to zero, has no
//! coverage to speak of: it takes no part in the average above it, reads as
//! 0, or 100 when its own weight is 0, and has no bins to count (19.11 a to
//! d). An item of weight 0 takes no part either but reads as its real
//! coverage.
//!
//! The coverage of a type is the average of its instances by their
//! weights, or with `type_option.merge_instances` that of the union of their
//! bins by name, where the hits of a bin add up (19.11.3). `option.goal`
//! and `type_option.goal` change no number.

use fxhash::FxHashMap;

use super::descriptor::BinKind;
use super::shape::{BinId, BinShape};
use super::{Instance, Registry, TypeEntry};

/// The covered bins and the bins.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Parts {
    pub covered: u64,
    pub total: u64,
}

impl Parts {
    fn add(&mut self, other: Parts) {
        self.covered += other.covered;
        self.total += other.total;
    }
}

/// A coverage in percent with the bins it is of, and whether it takes part
/// in the average above it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Score {
    pub coverage: f64,
    pub parts: Parts,
    pub contributes: bool,
}

/// The score where the denominator is zero, of something of weight
/// `weight` (19.11 a to d).
fn zero(weight: i32) -> Score {
    Score {
        coverage: if weight == 0 { 100.0 } else { 0.0 },
        parts: Parts::default(),
        contributes: false,
    }
}

fn ratio(parts: Parts) -> f64 {
    100.0 * parts.covered as f64 / parts.total as f64
}

/// The score of an item of weight `weight` with the bins `parts`.
fn score(parts: Parts, weight: i32) -> Score {
    if parts.total == 0 {
        zero(weight)
    } else {
        Score {
            coverage: ratio(parts),
            parts,
            contributes: true,
        }
    }
}

/// Whether a bin with `hits` hits is covered where `at_least` are needed.
pub(super) fn covered(hits: u64, at_least: i32) -> bool {
    hits >= u64::try_from(at_least).unwrap_or(0)
}

/// An average by weights.
#[derive(Default)]
struct Average {
    numerator: f64,
    denominator: f64,
    parts: Parts,
}

impl Average {
    fn add(&mut self, score: Score, weight: i32) {
        if score.contributes {
            self.numerator += f64::from(weight) * score.coverage;
            self.denominator += f64::from(weight);
        }
    }

    /// The average, of something of weight `weight`.
    fn finish(self, weight: i32) -> Score {
        if self.denominator == 0.0 {
            zero(weight)
        } else {
            Score {
                coverage: self.numerator / self.denominator,
                parts: self.parts,
                contributes: true,
            }
        }
    }
}

/// Whether a bin counts towards coverage: it is a `bins` bin with a value.
fn coverable(bin: &BinShape) -> bool {
    bin.kind == BinKind::Bins && !bin.empty
}

/// The bins of coverpoint `point` of `instance`, none before it is built.
pub(super) fn bins<'a>(
    registry: &'a Registry,
    instance: &Instance,
    point: usize,
) -> &'a [BinShape] {
    instance.shape.map_or(&[][..], |shape| {
        &registry.types[instance.ty as usize].shapes[shape as usize].points[point].bins[..]
    })
}

fn point_parts(registry: &Registry, instance: &Instance, point: usize) -> Parts {
    let at_least = instance.options.at_least(point);
    let mut parts = Parts::default();
    for bin in bins(registry, instance, point)
        .iter()
        .filter(|bin| coverable(bin))
    {
        parts.total += 1;
        parts.covered += u64::from(covered(instance.counts[bin.counter as usize], at_least));
    }
    parts
}

fn point_of_instance(registry: &Registry, instance: &Instance, point: usize) -> Score {
    score(
        point_parts(registry, instance, point),
        instance.options.points[point].weight,
    )
}

/// The score of the covergroup of `instance`, and the bins of its items
/// whether or not they take part.
fn group_of_instance(registry: &Registry, instance: &Instance) -> (Score, Parts) {
    let mut average = Average::default();
    for (point, options) in instance.options.points.iter().enumerate() {
        let score = point_of_instance(registry, instance, point);
        average.parts.add(score.parts);
        average.add(score, options.weight);
    }
    let parts = average.parts;
    (average.finish(instance.options.weight), parts)
}

/// What `get_inst_coverage` of item `item` of instance `instance` gives:
/// with `type_option.merge_instances`, the coverage of the type unless
/// `option.get_inst_coverage` is set (Table 19-1).
pub(super) fn instance(registry: &Registry, instance: usize, item: u32) -> Score {
    let instance = &registry.instances[instance];
    let ty = instance.ty as usize;
    if registry.types[ty].options.merge_instances && !instance.options.get_inst_coverage {
        return of_type(registry, ty, item);
    }
    match item {
        0 => group_of_instance(registry, instance).0,
        item => point_of_instance(registry, instance, item as usize - 1),
    }
}

/// The union of the bins of a coverpoint over the instances of a type.
pub(super) struct Merged<'a> {
    /// The most hits any instance needs for a covered bin: a bin of the
    /// type is covered with as many (19.11.1).
    pub at_least: i32,
    /// The bins that count towards coverage, in the order instances first
    /// have them, with the sum of their hits.
    pub bins: Vec<(&'a BinId, u64)>,
}

impl Merged<'_> {
    fn parts(&self) -> Parts {
        Parts {
            covered: self
                .bins
                .iter()
                .filter(|(_, hits)| covered(*hits, self.at_least))
                .count() as u64,
            total: self.bins.len() as u64,
        }
    }
}

/// The union of the bins of coverpoint `point` over the instances of
/// `entry`, where bins of one name are one bin (19.11.3).
pub(super) fn merged<'a>(registry: &'a Registry, entry: &'a TypeEntry, point: usize) -> Merged<'a> {
    let instances = || {
        entry
            .instances
            .iter()
            .map(|&instance| &registry.instances[instance as usize])
    };
    let at_least = instances()
        .map(|instance| instance.options.at_least(point))
        .max()
        .unwrap_or(1);
    let mut shapes = instances().filter_map(|instance| instance.shape);
    let first = shapes.next();
    let bins = if shapes.all(|shape| Some(shape) == first) {
        // Instances of one shape have the same bins.
        first.map_or_else(Vec::new, |shape| {
            entry.shapes[shape as usize].points[point]
                .bins
                .iter()
                .filter(|bin| coverable(bin))
                .map(|bin| {
                    let hits = instances()
                        .filter(|instance| instance.shape.is_some())
                        .map(|instance| instance.counts[bin.counter as usize])
                        .sum();
                    (&bin.id, hits)
                })
                .collect()
        })
    } else {
        let mut bins: Vec<(&BinId, u64)> = Vec::new();
        let mut index: FxHashMap<&BinId, usize> = FxHashMap::default();
        for instance in instances() {
            let Some(shape) = instance.shape else {
                continue;
            };
            for bin in entry.shapes[shape as usize].points[point]
                .bins
                .iter()
                .filter(|bin| coverable(bin))
            {
                let hits = instance.counts[bin.counter as usize];
                match index.get(&bin.id) {
                    Some(&at) => bins[at].1 += hits,
                    None => {
                        index.insert(&bin.id, bins.len());
                        bins.push((&bin.id, hits));
                    }
                }
            }
        }
        bins
    };
    Merged { at_least, bins }
}

fn point_of_type(registry: &Registry, entry: &TypeEntry, point: usize) -> Score {
    let weight = entry.options.points[point].weight;
    if entry.options.merge_instances {
        return score(merged(registry, entry, point).parts(), weight);
    }
    // The average of the coverpoint over the instances, by its weight in
    // each.
    let mut average = Average::default();
    for &instance in &entry.instances {
        let instance = &registry.instances[instance as usize];
        let score = point_of_instance(registry, instance, point);
        average.parts.add(score.parts);
        average.add(score, instance.options.points[point].weight);
    }
    average.finish(weight)
}

fn group_of_type(registry: &Registry, entry: &TypeEntry) -> Score {
    let mut average = Average::default();
    if entry.options.merge_instances {
        for (point, options) in entry.options.points.iter().enumerate() {
            let score = score(merged(registry, entry, point).parts(), options.weight);
            average.parts.add(score.parts);
            average.add(score, options.weight);
        }
    } else {
        // The type coverage "depends on the instances only, not its
        // coverpoints or crosses".
        for &instance in &entry.instances {
            let instance = &registry.instances[instance as usize];
            let (score, parts) = group_of_instance(registry, instance);
            average.parts.add(parts);
            average.add(score, instance.options.weight);
        }
    }
    average.finish(entry.options.weight)
}

/// What `get_coverage` of item `item` of type `ty` gives.
pub(super) fn of_type(registry: &Registry, ty: usize, item: u32) -> Score {
    let entry = &registry.types[ty];
    match item {
        0 => group_of_type(registry, entry),
        item => point_of_type(registry, entry, item as usize - 1),
    }
}

/// What `$get_coverage` gives (19.9): the average of the types by
/// `type_option.weight`, or 100 where none takes part, as without any
/// instance or with every weight 0 (19.11).
pub(super) fn overall(registry: &Registry) -> f64 {
    let mut average = Average::default();
    for entry in &registry.types {
        average.add(group_of_type(registry, entry), entry.options.weight);
    }
    if average.denominator == 0.0 {
        100.0
    } else {
        average.numerator / average.denominator
    }
}
