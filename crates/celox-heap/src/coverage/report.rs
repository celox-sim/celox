//! The coverage of a simulation as data: every covergroup type that has an
//! instance, with its items, bins and hits, for the type and for each
//! instance.

use fxhash::FxHashMap;
use serde::{Deserialize, Serialize};

use super::descriptor::{BinKind, PointDescriptor};
use super::shape::{BinClass, BinId, NameTag, bin_name};
use super::{Instance, Registry, compute};

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
    /// each value that was sampled.
    pub bins: Vec<BinReport>,
    /// The bins that are not listed. For a `default` bin array, the hits of
    /// the values beyond those it has a bin for
    /// ([`MAX_DEFAULT_ARRAY`](super::MAX_DEFAULT_ARRAY)).
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
    /// The name as 19.11.3 gives it, such as `b`, `auto[3]` or
    /// `auto[0:3]`.
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

/// The report of `registry`: the types by name, then by key.
pub(super) fn report(registry: &Registry) -> CoverageReport {
    let mut types: Vec<usize> = (0..registry.types.len())
        .filter(|&ty| !registry.types[ty].instances.is_empty())
        .collect();
    types.sort_by(|&first, &second| {
        let (first, second) = (&registry.types[first], &registry.types[second]);
        (&first.descriptor.name, first.key).cmp(&(&second.descriptor.name, second.key))
    });
    CoverageReport {
        coverage: compute::overall(registry),
        covergroups: types
            .into_iter()
            .map(|ty| covergroup(registry, ty))
            .collect(),
    }
}

fn covergroup(registry: &Registry, ty: usize) -> CovergroupReport {
    let entry = &registry.types[ty];
    let descriptor = &entry.descriptor;
    let score = compute::of_type(registry, ty, 0);
    let items = descriptor
        .points
        .iter()
        .enumerate()
        .map(|(index, point)| {
            let options = &entry.options.points[index];
            let at_least = compute::merged_at_least(registry, entry, index);
            let score = compute::of_type(registry, ty, index as u32 + 1);
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
            .map(|&instance| covergroup_instance(registry, instance as usize))
            .collect(),
    }
}

fn covergroup_instance(registry: &Registry, index: usize) -> CovergroupInstanceReport {
    let instance = &registry.instances[index];
    let descriptor = &registry.types[instance.ty as usize].descriptor;
    let options = &instance.options;
    let score = compute::instance(registry, index, 0);
    let items = descriptor
        .points
        .iter()
        .enumerate()
        .map(|(point_index, point)| {
            let point_options = &options.points[point_index];
            let at_least = options.at_least(point_index);
            let score = compute::instance(registry, index, point_index as u32 + 1);
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
