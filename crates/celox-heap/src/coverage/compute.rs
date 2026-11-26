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
//! The bins of a cross are the bins it declares, but for ignore and illegal
//! bins and bins without a cross product, and its automatic bins: the cross
//! products in no declared bin (19.11.2).
//!
//! The coverage of a type is the average of its instances by their
//! weights, or with `type_option.merge_instances` that of the union of their
//! bins by name, where the hits of a bin add up (19.11.3). `option.goal`
//! and `type_option.goal` change no number.

use std::borrow::Cow;

use fxhash::FxHashMap;

use super::cross::{CrossBinShape, Crossed, United};
use super::descriptor::BinKind;
use super::shape::{BinId, BinShape, Shape};
use super::{Instance, MAX_CROSS_WORK, Registry, TypeEntry};

/// The covered bins and the bins.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct Parts {
    pub covered: u64,
    pub total: u64,
}

impl Parts {
    fn add(&mut self, other: Parts) {
        self.covered = self.covered.saturating_add(other.covered);
        self.total = self.total.saturating_add(other.total);
    }
}

/// The covered bins and the bins of a cross, which may be more than a
/// [`Parts`] counts.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct CrossParts {
    covered: u128,
    total: u128,
}

impl CrossParts {
    /// The score of a cross of weight `weight` with these bins. Its
    /// coverage is of all the bins; the numbers of bins stop at the largest
    /// that fits.
    fn score(self, weight: i32) -> Score {
        if self.total == 0 {
            return zero(weight);
        }
        let fit = |count: u128| u64::try_from(count).unwrap_or(u64::MAX);
        Score {
            coverage: 100.0 * self.covered as f64 / self.total as f64,
            parts: Parts {
                covered: fit(self.covered),
                total: fit(self.total),
            },
            contributes: true,
        }
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

/// The shape of `instance`, none before it is built.
pub(super) fn shape<'a>(registry: &'a Registry, instance: &Instance) -> Option<&'a Shape> {
    instance
        .shape
        .map(|shape| &*registry.types[instance.ty as usize].shapes[shape as usize])
}

/// The bins of coverpoint `point` of `instance`, none before it is built.
pub(super) fn bins<'a>(
    registry: &'a Registry,
    instance: &Instance,
    point: usize,
) -> &'a [BinShape] {
    shape(registry, instance).map_or(&[][..], |shape| &shape.points[point].bins[..])
}

fn point_parts(registry: &Registry, instance: &Instance, point: usize) -> Parts {
    let at_least = instance.options.at_least(point);
    let mut parts = Parts::default();
    for bin in bins(registry, instance, point)
        .iter()
        .filter(|bin| bin.coverable())
    {
        parts.total += 1;
        parts.covered += u64::from(covered(instance.counts[bin.counter as usize], at_least));
    }
    parts
}

/// Whether a declared bin of a cross counts towards coverage: it is a
/// `bins` bin (19.11.2) with a cross product that is not ignored or
/// illegal.
fn significant(bin: &CrossBinShape) -> bool {
    bin.kind == BinKind::Bins && !bin.empty
}

/// The bins of cross `cross` of `instance` (19.11.2): its automatic bins,
/// `Bc`, of which those that were sampled have their hits, and the bins it
/// declares that count, `Bu`. `item` is the index of the cross among the
/// items.
fn cross_parts(registry: &Registry, instance: &Instance, cross: usize, item: usize) -> CrossParts {
    let Some(shape) = shape(registry, instance) else {
        return CrossParts::default();
    };
    let shape = &shape.crosses[cross];
    let at_least = instance.options.at_least(item);
    let mut parts = CrossParts {
        covered: if at_least <= 0 {
            // A bin without a hit is covered too.
            shape.auto_bins
        } else {
            instance.crosses[cross]
                .values()
                .filter(|&&hits| covered(hits, at_least))
                .count() as u128
        },
        total: shape.auto_bins,
    };
    for bin in shape.bins.iter().filter(|bin| significant(bin)) {
        let hits = instance.counts[bin.counter as usize];
        parts.total = parts.total.saturating_add(1);
        parts.covered = parts
            .covered
            .saturating_add(u128::from(covered(hits, at_least)));
    }
    parts
}

/// The score of item `item` of `instance`, which is the index of a
/// coverpoint or, after those, of a cross.
fn item_of_instance(registry: &Registry, instance: &Instance, item: usize) -> Score {
    let weight = instance.options.items[item].weight;
    let points = registry.types[instance.ty as usize].descriptor.points.len();
    match item.checked_sub(points) {
        None => score(point_parts(registry, instance, item), weight),
        Some(cross) => cross_parts(registry, instance, cross, item).score(weight),
    }
}

/// The score of the covergroup of `instance`, and the bins of its items
/// whether or not they take part.
fn group_of_instance(registry: &Registry, instance: &Instance) -> (Score, Parts) {
    let mut average = Average::default();
    for (item, options) in instance.options.items.iter().enumerate() {
        let score = item_of_instance(registry, instance, item);
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
    if takes_type(registry, instance) {
        return of_type(registry, ty, item);
    }
    own(registry, instance, item)
}

/// Whether `get_inst_coverage` of `instance` is that of its type:
/// with `type_option.merge_instances` and `option.get_inst_coverage` clear
/// (Table 19-1).
pub(super) fn takes_type(registry: &Registry, instance: &Instance) -> bool {
    registry.types[instance.ty as usize].options.merge_instances
        && !instance.options.get_inst_coverage
}

/// The coverage of item `item` of `instance` itself.
pub(super) fn own(registry: &Registry, instance: &Instance, item: u32) -> Score {
    match item {
        0 => group_of_instance(registry, instance).0,
        item => item_of_instance(registry, instance, item as usize - 1),
    }
}

/// The union of the bins of a coverpoint over the instances of a type.
struct Merged<'a> {
    /// The most hits any instance needs for a covered bin: a bin of the
    /// type is covered with as many (19.11.1).
    at_least: i32,
    /// The bins that count towards coverage, in the order instances first
    /// have them, with the sum of their hits.
    bins: Vec<(&'a BinId, u64)>,
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

/// The most hits that an instance of `entry` needs for a covered bin of
/// the coverpoint or cross of index `point` among the items.
pub(super) fn merged_at_least(registry: &Registry, entry: &TypeEntry, point: usize) -> i32 {
    entry
        .instances
        .iter()
        .map(|&instance| {
            registry.instances[instance as usize]
                .options
                .at_least(point)
        })
        .max()
        .unwrap_or(1)
}

/// The union of the bins of coverpoint `point` over the instances of
/// `entry`, where bins of one name are one bin (19.11.3).
fn merged<'a>(registry: &'a Registry, entry: &'a TypeEntry, point: usize) -> Merged<'a> {
    let instances = || {
        entry
            .instances
            .iter()
            .map(|&instance| &registry.instances[instance as usize])
    };
    let at_least = merged_at_least(registry, entry, point);
    let mut shapes = instances().filter_map(|instance| instance.shape);
    let first = shapes.next();
    let bins = if shapes.all(|shape| Some(shape) == first) {
        // Instances of one shape have the same bins.
        first.map_or_else(Vec::new, |shape| {
            entry.shapes[shape as usize].points[point]
                .bins
                .iter()
                .filter(|bin| bin.coverable())
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
                .filter(|bin| bin.coverable())
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

/// The shapes of the built instances of `entry`, each once, in the order
/// instances first have them.
pub(super) fn shapes(registry: &Registry, entry: &TypeEntry) -> Vec<u32> {
    let mut shapes = Vec::new();
    let mut seen = vec![false; entry.shapes.len()];
    for &instance in &entry.instances {
        if let Some(shape) = registry.instances[instance as usize].shape
            && !std::mem::replace(&mut seen[shape as usize], true)
        {
            shapes.push(shape);
        }
    }
    shapes
}

/// Cross `cross` of shape `shape` of `entry`.
pub(super) fn crossed(entry: &TypeEntry, shape: u32, cross: usize) -> Crossed<'_> {
    let shape = &entry.shapes[shape as usize];
    Crossed {
        cross: &shape.crosses[cross],
        points: &shape.points,
    }
}

/// Cross `cross` in each of the shapes of the built instances of `entry`.
fn crossed_shapes<'a>(registry: &Registry, entry: &'a TypeEntry, cross: usize) -> Vec<Crossed<'a>> {
    shapes(registry, entry)
        .into_iter()
        .map(|shape| crossed(entry, shape, cross))
        .collect()
}

/// The number of automatic bins of cross `cross` of the type `entry`: the
/// union of those of its instances by name (19.11.3), or an error where
/// finding it takes more than `bound` steps (see [`MAX_CROSS_WORK`]).
/// `found` counts the unions of instances of different bins that are
/// looked for.
pub(super) fn united_automatic(
    registry: &Registry,
    entry: &TypeEntry,
    cross: usize,
    bound: u64,
    found: &mut u64,
) -> Result<u128, String> {
    let shapes = crossed_shapes(registry, entry, cross);
    match shapes[..] {
        [] => Ok(0),
        // Instances of one shape have the same bins.
        [shape] => Ok(shape.cross.auto_bins),
        _ => {
            *found += 1;
            United::new(&shapes).automatic(bound).ok_or_else(|| {
                format!(
                    "uniting the automatic bins of cross `{}` over the instances \
                 (type_option.merge_instances) takes more than {MAX_CROSS_WORK} steps",
                    entry.descriptor.crosses[cross].name
                )
            })
        }
    }
}

/// The automatic bins of each cross of `entry` united over its instances
/// (19.11.3): those the type keeps where they are of its shapes, else found
/// now however many steps that takes, counted in `found` (see
/// [`united_automatic`]).
pub(super) fn unions<'a>(
    registry: &Registry,
    entry: &'a TypeEntry,
    found: &mut u64,
) -> Cow<'a, [u128]> {
    match entry.unions.fresh(entry.shapes.len()) {
        Some(counts) => Cow::Borrowed(counts),
        None => Cow::Owned(
            (0..entry.descriptor.crosses.len())
                .map(|cross| {
                    united_automatic(registry, entry, cross, u64::MAX, found).unwrap_or(u128::MAX)
                })
                .collect(),
        ),
    }
}

/// What names an automatic bin of a cross among those of the instances of
/// a type: its key where all of them have one shape, else the bins of its
/// cross product.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Product<'a> {
    Key(u128),
    Bins(Vec<&'a BinId>),
}

/// An automatic bin of a cross of a type that was sampled.
pub(super) struct UnitedHit {
    /// A shape that has the bin, with the key of the bin in it.
    pub shape: u32,
    pub key: u128,
    /// The hits of the bin in all instances.
    pub hits: u64,
}

/// The automatic bins of cross `cross` of the type `entry` that were
/// sampled, where bins of one name are one bin and their hits add up
/// (19.11.3). With `ordered` they are in the order instances first have
/// them, those of an instance by key.
pub(super) fn united_hits(
    registry: &Registry,
    entry: &TypeEntry,
    cross: usize,
    ordered: bool,
) -> Vec<UnitedHit> {
    let one_shape = shapes(registry, entry).len() <= 1;
    let mut hits: Vec<UnitedHit> = Vec::new();
    let mut index: FxHashMap<Product<'_>, usize> = FxHashMap::default();
    let mut sampled: Vec<(u128, u64)> = Vec::new();
    for &instance in &entry.instances {
        let instance = &registry.instances[instance as usize];
        let Some(shape) = instance.shape else {
            continue;
        };
        sampled.clear();
        sampled.extend(
            instance.crosses[cross]
                .iter()
                .map(|(&key, &hits)| (key, hits)),
        );
        if ordered {
            sampled.sort_unstable();
        }
        for &(key, count) in &sampled {
            let product = if one_shape {
                Product::Key(key)
            } else {
                Product::Bins(crossed(entry, shape, cross).ids(key))
            };
            match index.get(&product) {
                Some(&at) => hits[at].hits += count,
                None => {
                    index.insert(product, hits.len());
                    hits.push(UnitedHit {
                        shape,
                        key,
                        hits: count,
                    });
                }
            }
        }
    }
    hits
}

/// The hits of the declared bin `bin` of cross `cross` in all instances of
/// `entry`, unless the bin is one of none of them: it holds no cross
/// product in any.
pub(super) fn united_bin(
    registry: &Registry,
    entry: &TypeEntry,
    cross: usize,
    bin: usize,
) -> Option<u64> {
    let mut hits = None;
    for &instance in &entry.instances {
        let instance = &registry.instances[instance as usize];
        let Some(shape) = shape(registry, instance) else {
            continue;
        };
        let bin = &shape.crosses[cross].bins[bin];
        if !bin.empty {
            *hits.get_or_insert(0) += instance.counts[bin.counter as usize];
        }
    }
    hits
}

/// The bins of cross `cross` of the type `entry` as the union of those of
/// its instances (19.11.3), where `item` is the index of the cross among
/// the items.
/// `unions` are the automatic bins of the crosses united (see [`unions`]).
fn merged_cross(
    registry: &Registry,
    entry: &TypeEntry,
    cross: usize,
    item: usize,
    unions: &[u128],
) -> CrossParts {
    let at_least = merged_at_least(registry, entry, item);
    let automatic = unions.get(cross).copied().unwrap_or(0);
    let mut parts = CrossParts {
        covered: if at_least <= 0 {
            automatic
        } else {
            united_hits(registry, entry, cross, false)
                .iter()
                .filter(|hit| covered(hit.hits, at_least))
                .count() as u128
        },
        total: automatic,
    };
    for (bin, declared) in entry.descriptor.crosses[cross].bins.iter().enumerate() {
        if declared.kind == BinKind::Bins
            && let Some(hits) = united_bin(registry, entry, cross, bin)
        {
            // The automatic bins of a union may be as many as fit.
            parts.total = parts.total.saturating_add(1);
            parts.covered = parts
                .covered
                .saturating_add(u128::from(covered(hits, at_least)));
        }
    }
    parts
}

/// The score of item `item` of the type `entry` with
/// `type_option.merge_instances`, which is the index of a coverpoint or,
/// after those, of a cross.
fn merged_item(registry: &Registry, entry: &TypeEntry, item: usize, unions: &[u128]) -> Score {
    let weight = entry.options.items[item].weight;
    match item.checked_sub(entry.descriptor.points.len()) {
        None => score(merged(registry, entry, item).parts(), weight),
        Some(cross) => merged_cross(registry, entry, cross, item, unions).score(weight),
    }
}

/// The score of item `item` of the type `entry`, where `unions` are the
/// automatic bins of its crosses united if it merges its instances.
fn item_of_type(registry: &Registry, entry: &TypeEntry, item: usize, unions: &[u128]) -> Score {
    if entry.options.merge_instances {
        return merged_item(registry, entry, item, unions);
    }
    // The average of the item over the instances, by its weight in each.
    let mut average = Average::default();
    for &instance in &entry.instances {
        let instance = &registry.instances[instance as usize];
        let score = item_of_instance(registry, instance, item);
        average.parts.add(score.parts);
        average.add(score, instance.options.items[item].weight);
    }
    average.finish(entry.options.items[item].weight)
}

/// The score of the covergroup of the type `entry` that merges its
/// instances, of the scores `items` of its items.
fn merged_group(entry: &TypeEntry, items: impl Iterator<Item = Score>) -> Score {
    let mut average = Average::default();
    for (score, options) in items.zip(&entry.options.items) {
        average.parts.add(score.parts);
        average.add(score, options.weight);
    }
    average.finish(entry.options.weight)
}

fn group_of_type(registry: &Registry, entry: &TypeEntry, unions: &[u128]) -> Score {
    if entry.options.merge_instances {
        return merged_group(
            entry,
            (0..entry.options.items.len()).map(|item| merged_item(registry, entry, item, unions)),
        );
    }
    // The type coverage "depends on the instances only, not its
    // coverpoints or crosses".
    let mut average = Average::default();
    for &instance in &entry.instances {
        let instance = &registry.instances[instance as usize];
        let (score, parts) = group_of_instance(registry, instance);
        average.parts.add(parts);
        average.add(score, instance.options.weight);
    }
    average.finish(entry.options.weight)
}

/// The automatic bins of the crosses of `entry` united, where its
/// coverage is of the union (see [`unions`]).
fn merged_unions<'a>(registry: &Registry, entry: &'a TypeEntry) -> Cow<'a, [u128]> {
    if entry.options.merge_instances {
        unions(registry, entry, &mut 0)
    } else {
        Cow::Borrowed(&[])
    }
}

/// What `get_coverage` of item `item` of type `ty` gives.
pub(super) fn of_type(registry: &Registry, ty: usize, item: u32) -> Score {
    let entry = &registry.types[ty];
    let unions = merged_unions(registry, entry);
    match item {
        0 => group_of_type(registry, entry, &unions),
        item => item_of_type(registry, entry, item as usize - 1, &unions),
    }
}

/// What `get_coverage` gives for the covergroup type `ty` and each of its
/// items, where `unions` are the automatic bins of its crosses united (see
/// [`unions`]).
pub(super) struct TypeScores {
    pub group: Score,
    /// By index among the items.
    pub items: Vec<Score>,
}

/// The scores of type `ty`, each found once.
pub(super) fn type_scores(registry: &Registry, ty: usize, unions: &[u128]) -> TypeScores {
    let entry = &registry.types[ty];
    let items: Vec<Score> = (0..entry.options.items.len())
        .map(|item| item_of_type(registry, entry, item, unions))
        .collect();
    let group = if entry.options.merge_instances {
        merged_group(entry, items.iter().copied())
    } else {
        group_of_type(registry, entry, unions)
    };
    TypeScores { group, items }
}

/// The weighted average of the covergroup types of `groups`, each the
/// score of a type with its `type_option.weight`, or 100 where none takes
/// part (19.9, 19.11).
pub(super) fn average_types(groups: impl Iterator<Item = (Score, i32)>) -> f64 {
    let mut average = Average::default();
    for (score, weight) in groups {
        average.add(score, weight);
    }
    if average.denominator == 0.0 {
        100.0
    } else {
        average.numerator / average.denominator
    }
}

/// What `$get_coverage` gives (19.9): the average of the types by
/// `type_option.weight`, or 100 where none takes part, as without any
/// instance or with every weight 0 (19.11).
pub(super) fn overall(registry: &Registry) -> f64 {
    average_types(registry.types.iter().map(|entry| {
        let unions = merged_unions(registry, entry);
        (
            group_of_type(registry, entry, &unions),
            entry.options.weight,
        )
    }))
}
