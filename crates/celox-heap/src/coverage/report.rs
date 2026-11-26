//! The coverage of a simulation as data: every covergroup type that has an
//! instance, with its items, bins and hits, for the type and for each
//! instance.

use fxhash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};

use super::compute::{Score, TypeScores};
use super::cross::Crossed;
use super::descriptor::{BinKind, CrossDescriptor, PointDescriptor, TypeDescriptor};
use super::shape::{BinClass, BinId, NameTag, bin_name};
use super::{Instance, MAX_CROSS_MISSING_ROWS, Registry, compute};

/// The covergroup coverage of a simulation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverageReport {
    /// What `$get_coverage()` gives now, in percent (IEEE 1800-2023 19.9):
    /// 100 without an instance.
    pub coverage: f64,
    /// The covergroup types that have an instance, by name.
    pub covergroups: Vec<CovergroupReport>,
}

/// A covergroup type.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CovergroupReport {
    pub name: String,
    /// `type_option.comment`, and so on.
    pub comment: String,
    pub weight: i32,
    pub goal: i32,
    pub merge_instances: bool,
    /// What `get_coverage()` gives, in percent, with the covered bins and
    /// the bins that its two-argument form gives (19.8).
    pub coverage: f64,
    pub covered: u64,
    pub total: u64,
    /// The items with the bins of all instances, where the bins of one name
    /// are one bin and their hits add up (19.11.3). Such a bin is covered
    /// with the hits that the instance that asks for most needs.
    pub items: Vec<CoverItemReport>,
    /// The instances in the order they were created, also those whose
    /// object is no longer reachable.
    pub instances: Vec<CovergroupInstanceReport>,
}

/// An instance of a covergroup.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CovergroupInstanceReport {
    /// `option.name`, and so on.
    pub name: String,
    pub comment: String,
    pub weight: i32,
    pub goal: i32,
    pub at_least: i32,
    pub per_instance: bool,
    /// Whether the covergroup is collecting: `stop()` was not the last of
    /// its `start()` and `stop()` calls.
    pub collecting: bool,
    /// Whether the object of the instance is still reachable.
    pub live: bool,
    /// The samples taken while the covergroup was collecting.
    pub samples: u64,
    /// What `get_inst_coverage()` gives, in percent, with the covered bins
    /// and the bins that its two-argument form gives (19.8).
    pub coverage: f64,
    pub covered: u64,
    pub total: u64,
    /// The coverpoints in declaration order, then the crosses.
    pub items: Vec<CoverItemReport>,
}

/// A coverpoint or cross.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverItemReport {
    pub name: String,
    pub kind: CoverItemKind,
    /// The coverpoints a cross crosses; empty for a coverpoint.
    pub coverpoints: Vec<String>,
    pub comment: String,
    pub weight: i32,
    pub goal: i32,
    pub at_least: i32,
    /// What `get_coverage()` or `get_inst_coverage()` of the item gives, as
    /// for the covergroup.
    pub coverage: f64,
    pub covered: u64,
    pub total: u64,
    /// The bins that have a value, and of a `default` bin array a bin for
    /// each value that was sampled. Of a cross, the bins it declares that
    /// have a cross product, then its automatic bins that were sampled,
    /// then as many of the others as `option.cross_num_print_missing` asks
    /// for (IEEE 1800-2023 Table 19-1), for the type as many as the
    /// instance that asks for most, and at most
    /// [`MAX_CROSS_MISSING_ROWS`](super::MAX_CROSS_MISSING_ROWS).
    pub bins: Vec<BinReport>,
    /// The bins that are not listed. For a `default` bin array, the hits of
    /// the values beyond those it has a bin for
    /// ([`MAX_DEFAULT_ARRAY`](super::MAX_DEFAULT_ARRAY)). For a cross, the
    /// number of automatic bins that were not sampled and are not listed.
    pub omitted_bins: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CoverItemKind {
    Coverpoint,
    Cross,
}

/// A bin with its hits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BinReport {
    /// The name as 19.11.3 gives it, such as `b`, `auto[3]`, `auto[0:3]`
    /// or, for an automatic bin of a cross, `<b,auto[3]>`.
    pub name: String,
    pub kind: BinReportKind,
    pub hits: u64,
    /// Whether the bin has the hits its item needs. Only a bin of kind
    /// [`BinReportKind::Bins`] counts towards coverage.
    pub covered: bool,
}

/// What a bin is to coverage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum BinReportKind {
    /// A bin that counts towards coverage.
    Bins,
    /// A `default` bin.
    Default,
    /// An `ignore_bins` bin.
    Ignore,
    /// An `illegal_bins` bin.
    Illegal,
}

/// A bin as a report lists it, before it has its name.
struct Listed {
    id: BinId,
    kind: BinReportKind,
    hits: u64,
}

/// The bins of coverpoint `point` of `instance` that a report lists, in the
/// order of their declarations, the automatic bins first, with the hits a
/// `default` bin array has no bin for.
fn listed(registry: &Registry, instance: &Instance, point: usize) -> (Vec<Listed>, u64) {
    let mut bins = Vec::new();
    let mut omitted = 0;
    for bin in compute::bins(registry, instance, point)
        .iter()
        .filter(|bin| !bin.empty)
    {
        let kind = match (bin.kind, bin.class) {
            (BinKind::Illegal, _) => BinReportKind::Illegal,
            (BinKind::Ignore, _) => BinReportKind::Ignore,
            (BinKind::Bins, BinClass::Value) => BinReportKind::Bins,
            (BinKind::Bins, BinClass::Default | BinClass::DefaultArray(_)) => {
                BinReportKind::Default
            }
        };
        match bin.class {
            BinClass::Value | BinClass::Default => bins.push(Listed {
                id: bin.id.clone(),
                kind,
                hits: instance.counts[bin.counter as usize],
            }),
            BinClass::DefaultArray(array) => {
                let array = &instance.default_arrays[array as usize];
                omitted += array.overflow;
                bins.extend(array.hits.iter().map(|(value, &hits)| Listed {
                    id: BinId {
                        decl: bin.id.decl,
                        tag: NameTag::Value(value.clone()),
                    },
                    kind,
                    hits,
                }));
            }
        }
    }
    (bins, omitted)
}

/// The bins as a report has them, where a bin is covered with `at_least`
/// hits.
fn bin_reports(point: &PointDescriptor, bins: Vec<Listed>, at_least: i32) -> Vec<BinReport> {
    bins.into_iter()
        .map(|bin| BinReport {
            name: bin_name(point, &bin.id),
            kind: bin.kind,
            hits: bin.hits,
            covered: compute::covered(bin.hits, at_least),
        })
        .collect()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn bin_kind(kind: BinKind) -> BinReportKind {
    match kind {
        BinKind::Bins => BinReportKind::Bins,
        BinKind::Ignore => BinReportKind::Ignore,
        BinKind::Illegal => BinReportKind::Illegal,
    }
}

/// The number of `automatic` automatic bins of a cross that are not among
/// the `listed` ones of a report.
fn omitted(automatic: u128, listed: usize) -> u64 {
    u64::try_from(automatic.saturating_sub(listed as u128)).unwrap_or(u64::MAX)
}

/// The names of the crossed coverpoints of `cross`.
fn crossed_names(descriptor: &TypeDescriptor, cross: &CrossDescriptor) -> Vec<String> {
    cross
        .points
        .iter()
        .map(|&point| descriptor.points[point as usize].name.clone())
        .collect()
}

/// The number of uncovered automatic bins of a cross that a report lists
/// where `option.cross_num_print_missing` is `option`.
fn print_missing(option: i32) -> usize {
    usize::try_from(option)
        .unwrap_or(0)
        .min(MAX_CROSS_MISSING_ROWS)
}

/// Cross `cross` of the type `ty`, with the bins of all its instances,
/// where `unions` are the automatic bins of its crosses united and `score`
/// is its coverage.
fn cross_of_type(
    registry: &Registry,
    ty: usize,
    cross: usize,
    unions: &[u128],
    score: Score,
) -> CoverItemReport {
    let entry = &registry.types[ty];
    let descriptor = &entry.descriptor;
    let declared = &descriptor.crosses[cross];
    let item = descriptor.points.len() + cross;
    let options = &entry.options.items[item];
    let at_least = compute::merged_at_least(registry, entry, item);
    let mut bins: Vec<BinReport> = Vec::new();
    for (index, bin) in declared.bins.iter().enumerate() {
        if let Some(hits) = compute::united_bin(registry, entry, cross, index) {
            bins.push(BinReport {
                name: bin.name.clone(),
                kind: bin_kind(bin.kind),
                hits,
                covered: compute::covered(hits, at_least),
            });
        }
    }
    let name = |shape: u32, key: u128| {
        let crossed = compute::crossed(entry, shape, cross);
        crossed.name(descriptor, crossed.cross.product(key))
    };
    let sampled = compute::united_hits(registry, entry, cross, true);
    bins.extend(sampled.iter().map(|hit| BinReport {
        name: name(hit.shape, hit.key),
        kind: BinReportKind::Bins,
        hits: hit.hits,
        covered: compute::covered(hit.hits, at_least),
    }));
    let mut listed = sampled.len();
    let mut missing = entry
        .instances
        .iter()
        .map(|&instance| {
            print_missing(
                registry.instances[instance as usize]
                    .options
                    .num_print_missing(item),
            )
        })
        .max()
        .unwrap_or(0);
    if missing != 0 {
        // The automatic bins that are listed, by what names them.
        let mut seen: FxHashSet<Vec<&BinId>> = sampled
            .iter()
            .map(|hit| compute::crossed(entry, hit.shape, cross).ids(hit.key))
            .collect();
        for shape in compute::shapes(registry, entry) {
            // A type whose constructions share no shape has one for each
            // instance: look at no more of them than the listing needs.
            if missing == 0 {
                break;
            }
            let crossed = compute::crossed(entry, shape, cross);
            let mut keys = Vec::new();
            crossed.cross.each_automatic(|key| {
                if missing != 0 && seen.insert(crossed.ids(key)) {
                    keys.push(key);
                    missing -= 1;
                }
                missing != 0
            });
            keys.sort_unstable();
            listed += keys.len();
            bins.extend(keys.into_iter().map(|key| BinReport {
                name: name(shape, key),
                kind: BinReportKind::Bins,
                hits: 0,
                covered: compute::covered(0, at_least),
            }));
        }
    }
    let automatic = unions.get(cross).copied().unwrap_or(0);
    CoverItemReport {
        name: declared.name.clone(),
        kind: CoverItemKind::Cross,
        coverpoints: crossed_names(descriptor, declared),
        comment: text(&options.comment),
        weight: options.weight,
        goal: options.goal,
        at_least,
        coverage: score.coverage,
        covered: score.parts.covered,
        total: score.parts.total,
        bins,
        omitted_bins: omitted(automatic, listed),
    }
}

/// Cross `cross` of the instance of index `index`, whose coverage is
/// `score`.
fn cross_of_instance(
    registry: &Registry,
    index: usize,
    cross: usize,
    score: Score,
) -> CoverItemReport {
    let instance = &registry.instances[index];
    let descriptor = &registry.types[instance.ty as usize].descriptor;
    let declared = &descriptor.crosses[cross];
    let item = descriptor.points.len() + cross;
    let options = &instance.options.items[item];
    let at_least = instance.options.at_least(item);
    let mut bins: Vec<BinReport> = Vec::new();
    let mut omitted_bins = 0;
    if let Some(shape) = compute::shape(registry, instance) {
        let crossed = Crossed {
            cross: &shape.crosses[cross],
            points: &shape.points,
        };
        for (bin, shape) in declared.bins.iter().zip(&crossed.cross.bins) {
            if !shape.empty {
                let hits = instance.counts[shape.counter as usize];
                bins.push(BinReport {
                    name: bin.name.clone(),
                    kind: bin_kind(bin.kind),
                    hits,
                    covered: compute::covered(hits, at_least),
                });
            }
        }
        let hits = &instance.crosses[cross];
        let mut sampled: Vec<(u128, u64)> = hits.iter().map(|(&key, &hits)| (key, hits)).collect();
        sampled.sort_unstable();
        let mut automatic = |key: u128, hits: u64| {
            bins.push(BinReport {
                name: crossed.name(descriptor, crossed.cross.product(key)),
                kind: BinReportKind::Bins,
                hits,
                covered: compute::covered(hits, at_least),
            })
        };
        for &(key, hits) in &sampled {
            automatic(key, hits);
        }
        let mut missing = print_missing(instance.options.num_print_missing(item));
        let mut keys = Vec::new();
        if missing != 0 {
            crossed.cross.each_automatic(|key| {
                if !hits.contains_key(&key) {
                    keys.push(key);
                    missing -= 1;
                }
                missing != 0
            });
        }
        keys.sort_unstable();
        for &key in &keys {
            automatic(key, 0);
        }
        omitted_bins = omitted(crossed.cross.auto_bins, sampled.len() + keys.len());
    }
    CoverItemReport {
        name: declared.name.clone(),
        kind: CoverItemKind::Cross,
        coverpoints: crossed_names(descriptor, declared),
        comment: text(&options.comment),
        weight: options.weight,
        goal: options.goal,
        at_least,
        coverage: score.coverage,
        covered: score.parts.covered,
        total: score.parts.total,
        bins,
        omitted_bins,
    }
}

/// The report of `registry`: the types by name, then by key.
/// `found` counts the unions of the automatic cross bins of instances of
/// different bins that are looked for, which the types do not keep.
pub(super) fn report(registry: &Registry, found: &mut u64) -> CoverageReport {
    let mut types: Vec<usize> = (0..registry.types.len())
        .filter(|&ty| !registry.types[ty].instances.is_empty())
        .collect();
    types.sort_by(|&first, &second| {
        let (first, second) = (&registry.types[first], &registry.types[second]);
        (&first.descriptor.name, first.key).cmp(&(&second.descriptor.name, second.key))
    });
    // The union of the automatic cross bins and the coverage of each type,
    // each found once.
    let unions: Vec<_> = registry
        .types
        .iter()
        .map(|entry| compute::unions(registry, entry, found))
        .collect();
    let scores: Vec<TypeScores> = (0..registry.types.len())
        .map(|ty| compute::type_scores(registry, ty, &unions[ty]))
        .collect();
    CoverageReport {
        coverage: compute::average_types(
            registry
                .types
                .iter()
                .zip(&scores)
                .map(|(entry, scores)| (scores.group, entry.options.weight)),
        ),
        covergroups: types
            .into_iter()
            .map(|ty| covergroup(registry, ty, &unions[ty], &scores[ty]))
            .collect(),
    }
}

/// Type `ty`, where `unions` are the automatic bins of its crosses united
/// and `scores` its coverage.
fn covergroup(
    registry: &Registry,
    ty: usize,
    unions: &[u128],
    scores: &TypeScores,
) -> CovergroupReport {
    let entry = &registry.types[ty];
    let descriptor = &entry.descriptor;
    let score = scores.group;
    let items = descriptor
        .points
        .iter()
        .enumerate()
        .map(|(index, point)| {
            let options = &entry.options.items[index];
            let at_least = compute::merged_at_least(registry, entry, index);
            let score = scores.items[index];
            // The union of the bins of the instances by name (19.11.3), in
            // the order instances first have them.
            let mut bins: Vec<Listed> = Vec::new();
            let mut omitted_bins = 0;
            let mut at: FxHashMap<BinId, usize> = FxHashMap::default();
            for &instance in &entry.instances {
                let (listed, omitted) =
                    listed(registry, &registry.instances[instance as usize], index);
                omitted_bins += omitted;
                for bin in listed {
                    match at.get(&bin.id) {
                        Some(&at) => bins[at].hits += bin.hits,
                        None => {
                            at.insert(bin.id.clone(), bins.len());
                            bins.push(bin);
                        }
                    }
                }
            }
            CoverItemReport {
                name: point.name.clone(),
                kind: CoverItemKind::Coverpoint,
                coverpoints: Vec::new(),
                comment: text(&options.comment),
                weight: options.weight,
                goal: options.goal,
                at_least,
                coverage: score.coverage,
                covered: score.parts.covered,
                total: score.parts.total,
                bins: bin_reports(point, bins, at_least),
                omitted_bins,
            }
        })
        .chain((0..descriptor.crosses.len()).map(|cross| {
            let score = scores.items[descriptor.points.len() + cross];
            cross_of_type(registry, ty, cross, unions, score)
        }))
        .collect();
    CovergroupReport {
        name: descriptor.name.clone(),
        comment: text(&entry.options.comment),
        weight: entry.options.weight,
        goal: entry.options.goal,
        merge_instances: entry.options.merge_instances,
        coverage: score.coverage,
        covered: score.parts.covered,
        total: score.parts.total,
        items,
        instances: entry
            .instances
            .iter()
            .map(|&instance| covergroup_instance(registry, instance as usize, scores))
            .collect(),
    }
}

/// The instance of index `index`, of a type whose coverage is `scores`.
fn covergroup_instance(
    registry: &Registry,
    index: usize,
    scores: &TypeScores,
) -> CovergroupInstanceReport {
    let instance = &registry.instances[index];
    let descriptor = &registry.types[instance.ty as usize].descriptor;
    let options = &instance.options;
    // What `get_inst_coverage` gives of item `item`: that of the type, or
    // the instance's own.
    let takes_type = compute::takes_type(registry, instance);
    let score_of = |item: usize| {
        if takes_type {
            match item {
                0 => scores.group,
                item => scores.items[item - 1],
            }
        } else {
            compute::own(registry, instance, item as u32)
        }
    };
    let score = score_of(0);
    let items = descriptor
        .points
        .iter()
        .enumerate()
        .map(|(point_index, point)| {
            let point_options = &options.items[point_index];
            let at_least = options.at_least(point_index);
            let score = score_of(point_index + 1);
            let (bins, omitted_bins) = listed(registry, instance, point_index);
            CoverItemReport {
                name: point.name.clone(),
                kind: CoverItemKind::Coverpoint,
                coverpoints: Vec::new(),
                comment: text(&point_options.comment),
                weight: point_options.weight,
                goal: point_options.goal,
                at_least,
                coverage: score.coverage,
                covered: score.parts.covered,
                total: score.parts.total,
                bins: bin_reports(point, bins, at_least),
                omitted_bins,
            }
        })
        .chain((0..descriptor.crosses.len()).map(|cross| {
            let score = score_of(descriptor.points.len() + cross + 1);
            cross_of_instance(registry, index, cross, score)
        }))
        .collect();
    CovergroupInstanceReport {
        name: text(&options.name),
        comment: text(&options.comment),
        weight: options.weight,
        goal: options.goal,
        at_least: options.at_least,
        per_instance: options.per_instance,
        collecting: instance.collecting,
        live: instance.live,
        samples: instance.samples,
        coverage: score.coverage,
        covered: score.parts.covered,
        total: score.parts.total,
        items,
    }
}
