//! Cross coverage (IEEE 1800-2023 19.6): what a construction makes of the
//! crosses of a covergroup type.
//!
//! A cross of N coverpoints has a bin for each cross product: each choice of
//! one bin of every coverpoint among the bins that count towards its
//! coverage, so no `default`, ignore or illegal bin (19.6) and no bin
//! without a value. The bins that a cross declares give a name and a count
//! to the cross products their select expressions select (19.6.1); the
//! cross products in none of them are the automatic bins of the cross.
//!
//! No list of the cross products is ever made. A `binsof` term selects some
//! bins of one coverpoint, so the bins of each crossed coverpoint fall into
//! classes: two bins are of one class when every term of the cross selects
//! both or neither. A select expression then holds of all cross products of
//! a tuple of classes or of none of them, and a [`CrossShape`] has for each
//! tuple of classes the declared bins of its cross products. That gives the
//! number of automatic bins and the bins a sample counts in without looking
//! at more than the classes, and an instance keeps the hits of the automatic
//! bins it sampled by the key of their cross product.

use fxhash::FxHashMap;
use serde::{Deserialize, Serialize};

use super::descriptor::{BinKind, Constant, CrossDescriptor, Select, TypeDescriptor};
use super::shape::{
    BinClass, BinId, PointShape, ResolvedItem, Warning, bin_name, evaluate, key, resolve,
};
use super::{MAX_CROSS_CLASSES, MAX_CROSS_WORK, MAX_WILDCARD_EXPANSION};

/// No position and no class: a bin that takes no part in a cross.
const NONE: u32 = u32::MAX;

/// The bins of a cross of an instance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CrossShape {
    /// The crossed coverpoints in the order of the cross.
    pub dims: Vec<Dimension>,
    /// The bins of the declarations, in their order.
    pub bins: Vec<CrossBinShape>,
    /// By tuple of classes, the set among `sets` of the declared bins that
    /// hold its cross products. The class of the first coverpoint counts
    /// most.
    table: Vec<u32>,
    /// The first is that of the cross products in no declared bin.
    sets: Vec<ProductSet>,
    /// Whether the cross products in no declared bin are automatic bins:
    /// with `option.cross_retain_auto_bins`, or without any declared bin
    /// (19.6.1).
    pub automatic: bool,
    /// The number of automatic bins: `Bc` of 19.11.2.
    pub auto_bins: u128,
}

/// A crossed coverpoint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Dimension {
    /// The coverpoint, by its index in the type.
    pub point: u32,
    /// The bins of the coverpoint that take part in the cross, by their
    /// index among its bins, ascending. The position of a bin here is what
    /// a cross product has for the coverpoint.
    pub bins: Vec<u32>,
    /// By bin of the coverpoint, its position among `bins`, or [`NONE`].
    position: Vec<u32>,
    /// By position, the class of the bin; empty where all are of class 0.
    class: Vec<u32>,
    classes: u32,
    /// What the key of a cross product grows by with the position: the
    /// number of choices of bins of the coverpoints after this one.
    stride: u128,
    /// The same for the index of a tuple of classes.
    class_stride: u32,
}

impl Dimension {
    fn class_of(&self, position: u32) -> u32 {
        self.class.get(position as usize).copied().unwrap_or(0)
    }
}

/// A declared bin of a cross.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CrossBinShape {
    pub kind: BinKind,
    /// The hit counter of the bin in an instance.
    pub counter: u32,
    /// The predicate bit of the guard of the declaration.
    pub iff: Option<u32>,
    /// Whether the bin holds no cross product, for a `bins` bin none that
    /// no ignore or illegal bin holds as well: it takes no part in coverage
    /// then (19.11.2).
    pub empty: bool,
}

/// The declared bins of the cross products of a tuple of classes.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct ProductSet {
    /// Ascending. Where an ignore or illegal bin holds the cross products
    /// they are those of no other bin (19.6.2, 19.6.3): only such bins are
    /// listed then.
    bins: Vec<u32>,
}

/// A select expression over the terms of a cross, by their number.
enum Term {
    BinsOf(u32),
    All(Vec<Term>),
    Any(Vec<Term>),
}

impl Term {
    fn holds(&self, bins_of: &dyn Fn(u32) -> bool) -> bool {
        match self {
            Self::BinsOf(term) => bins_of(*term),
            Self::All(terms) => terms.iter().all(|term| term.holds(bins_of)),
            Self::Any(terms) => terms.iter().any(|term| term.holds(bins_of)),
        }
    }

    /// The number of nodes, which telling whether it holds looks at most.
    fn nodes(&self) -> u64 {
        match self {
            Self::BinsOf(_) => 1,
            Self::All(terms) | Self::Any(terms) => 1 + terms.iter().map(Self::nodes).sum::<u64>(),
        }
    }

    /// A term that holds wherever the expression does, if there is one:
    /// the expression holds of no cross product whose bin of the coverpoint
    /// of that term the term does not select.
    fn guard(&self) -> Option<u32> {
        match self {
            Self::BinsOf(term) => Some(*term),
            Self::All(terms) => terms.iter().find_map(Self::guard),
            Self::Any(terms) => match &terms[..] {
                [term] => term.guard(),
                _ => None,
            },
        }
    }
}

/// A `binsof` term of a cross: the bins of one crossed coverpoint it
/// selects, by their position.
struct BinsOf {
    dim: u32,
    selects: Vec<bool>,
}

/// What building the crosses of a shape reads.
pub(crate) struct CrossInputs<'a> {
    pub descriptor: &'a TypeDescriptor,
    /// The bins of the coverpoints, which are built.
    pub points: &'a [PointShape],
    /// The integral inputs of the construction by input index.
    pub inputs: &'a [Option<Constant>],
}

impl CrossInputs<'_> {
    /// The bins that the term `select` of `cross` selects of its
    /// coverpoint, whose bins that take part are those of `dims`
    /// (19.6.1): all of them or those of one declaration, of which
    /// `intersect` keeps the bins that hold a value of its range list, and
    /// with `!` the others. The warnings of resolving the range list
    /// (19.5.7) are added to `warnings`. `subject` names the declared bin
    /// of the term.
    ///
    /// Which values a bin holds is known here but for those a sample alone
    /// sorts out: the values that an ignore or illegal wildcard value of
    /// more than [`MAX_WILDCARD_EXPANSION`] ranges takes, which is matched
    /// as a pattern, and the values of a bin whose `with` filter a sample
    /// evaluates. A range list that meets such a value of a bin the term
    /// may select is an error.
    fn bins_of(
        &self,
        dims: &[Dimension],
        select: &Select,
        subject: &str,
        warnings: &mut Vec<String>,
    ) -> Result<BinsOf, String> {
        let Select::BinsOf {
            position,
            bin,
            intersect,
            negate,
        } = select
        else {
            return Err("a combination of selections is no `binsof` term".into());
        };
        let dim = dims
            .get(*position as usize)
            .ok_or_else(|| format!("no cross item {position}"))?;
        let point = &self.descriptor.points[dim.point as usize];
        let shape = &self.points[dim.point as usize];
        let mut selects: Vec<bool> = dim
            .bins
            .iter()
            .map(|&index| {
                let id = &shape.bins[index as usize].id;
                bin.is_none_or(|declaration| !id.automatic() && id.decl == declaration)
            })
            .collect();
        if let Some(ranges) = intersect {
            // The bins of the coverpoint that hold a value of the list.
            let mut holding = vec![false; shape.bins.len()];
            // Those of one item of the list.
            let mut marked: Vec<u32> = Vec::new();
            let candidate = |bin: u32| {
                let position = dim.position[bin as usize];
                position != NONE && selects[position as usize]
            };
            for range in ranges {
                let resolved = resolve(&point.ty, evaluate(range, self.inputs)?);
                warnings.extend(resolved.warnings);
                for item in resolved.items {
                    marked.clear();
                    let mut mark = |bin: u32| marked.push(bin);
                    // A transition bin holds the values of its last steps:
                    // "the binsof operator uses the last value of the
                    // transition" (19.6.1.1).
                    let last = shape
                        .transitions
                        .as_ref()
                        .map(|transitions| &transitions.last);
                    let removed = match item {
                        ResolvedItem::Range { low, high } => {
                            let (low, high) = (key(&point.ty, &low), key(&point.ty, &high));
                            if let Some(last) = last {
                                last.holding(point.ty.width, &low, &high, &mut mark);
                            }
                            shape
                                .matcher
                                .holding(point.ty.width, &low, &high, &mut mark)
                        }
                        ResolvedItem::Exact { value, mask } => {
                            if let Some(last) = last {
                                last.holding_exact(&value, &mask, &mut mark);
                            }
                            shape.matcher.holding_exact(&value, &mask, &mut mark);
                            false
                        }
                    };
                    for &bin in &marked {
                        holding[bin as usize] = true;
                        let bin_shape = &shape.bins[bin as usize];
                        let filtered = bin_shape.filter.is_some();
                        // A transition keeps the values that ignore and
                        // illegal bins take (19.5.5, 19.5.6).
                        let removed = removed && bin_shape.class != BinClass::Transition;
                        if (removed || filtered) && candidate(bin) {
                            return Err(format!(
                                "covergroup cross `intersect` of {subject} meets values of \
                                 coverpoint `{}` that only a sample sorts into bins: those of \
                                 an ignore or illegal wildcard value of more than \
                                 {MAX_WILDCARD_EXPANSION} ranges, or of a bin whose `with` \
                                 filter a sample evaluates (IEEE 1800-2023 19.6.1)",
                                point.name
                            ));
                        }
                    }
                }
            }
            for (selects, &index) in selects.iter_mut().zip(&dim.bins) {
                *selects &= holding[index as usize];
            }
        }
        if *negate {
            for selects in &mut selects {
                *selects = !*selects;
            }
        }
        Ok(BinsOf {
            dim: *position,
            selects,
        })
    }

    /// The bins of `cross`, where `retain` is its
    /// `option.cross_retain_auto_bins` and the counters of its declared
    /// bins are taken from `counters`. An error is a limit that the cross
    /// exceeds.
    pub(crate) fn build(
        &self,
        cross: &CrossDescriptor,
        retain: bool,
        counters: &mut u32,
        warnings: &mut Vec<Warning>,
    ) -> Result<CrossShape, String> {
        let mut dims: Vec<Dimension> = cross
            .points
            .iter()
            .map(|&point| {
                let shape = &self.points[point as usize];
                let mut position = vec![NONE; shape.bins.len()];
                let mut bins = Vec::new();
                for (index, bin) in shape.bins.iter().enumerate() {
                    if bin.coverable() {
                        position[index] = bins.len() as u32;
                        bins.push(index as u32);
                    }
                }
                Dimension {
                    point,
                    bins,
                    position,
                    class: Vec::new(),
                    classes: 1,
                    stride: 0,
                    class_stride: 0,
                }
            })
            .collect();
        // The number of cross products: the product of `Bj` of 19.11.2.
        let mut products: u128 = 1;
        for dim in dims.iter_mut().rev() {
            dim.stride = products;
            products = products
                .checked_mul(dim.bins.len() as u128)
                .ok_or_else(|| {
                    format!("cross `{}` has more than 2^128 cross products", cross.name)
                })?;
        }
        // What the build has done so far (see [`MAX_CROSS_WORK`]): a step is
        // a bin of a coverpoint that a term looks at or tells apart, a
        // class that a declared bin may hold cross products of, a tuple of
        // classes, or a node of a select expression evaluated for one.
        let mut steps: u64 = 0;
        // The terms of the select expressions, each once.
        let mut terms: Vec<BinsOf> = Vec::new();
        let mut known: FxHashMap<&Select, u32> = FxHashMap::default();
        let mut selections = Vec::with_capacity(cross.bins.len());
        for bin in &cross.bins {
            let mut found = Vec::new();
            let subject = format!(
                "{} `{}` of cross `{}`",
                bin.kind.keyword(),
                bin.name,
                cross.name
            );
            let selection = compile(&bin.select, &mut |select| {
                if let Some(&term) = known.get(select) {
                    return Ok(term);
                }
                let term = terms.len() as u32;
                if let Select::BinsOf { position, .. } = select {
                    let bins = dims.get(*position as usize).map_or(0, |dim| dim.bins.len());
                    spend(&mut steps, 2 * bins as u64, &cross.name)?;
                }
                terms.push(self.bins_of(&dims, select, &subject, &mut found)?);
                known.insert(select, term);
                Ok(term)
            })?;
            selections.push(selection);
            warnings.extend(found.into_iter().map(|text| Warning {
                subject: subject.clone(),
                text,
            }));
        }
        // The classes of the bins of each coverpoint, with the terms of the
        // coverpoint that select the bins of each class as a bit set of
        // `words` words. The limits are checked as the classes grow, before
        // the bit sets do.
        let mut local = vec![0usize; terms.len()];
        let mut words = vec![0usize; dims.len()];
        for (index, term) in terms.iter().enumerate() {
            local[index] = words[term.dim as usize];
            words[term.dim as usize] += 1;
        }
        for words in &mut words {
            *words = words.div_ceil(64);
        }
        let mut selecting: Vec<Vec<u64>> = words.iter().map(|&words| vec![0; words]).collect();
        let too_many = || {
            format!(
                "the bins of cross `{}` tell more than {MAX_CROSS_CLASSES} classes of cross products \
                 apart",
                cross.name
            )
        };
        for (index, term) in terms.iter().enumerate() {
            let at = term.dim as usize;
            let words = words[at];
            let dim = &mut dims[at];
            if dim.bins.is_empty() {
                continue;
            }
            if dim.class.is_empty() {
                dim.class = vec![0; dim.bins.len()];
            }
            let before = &selecting[at];
            let mut after: Vec<u64> = Vec::new();
            // By class before and whether the term selects it, the class
            // after.
            let mut split = vec![NONE; 2 * dim.classes as usize];
            let mut classes = 0;
            let bit = local[index];
            for (class, &selects) in dim.class.iter_mut().zip(&term.selects) {
                let slot = 2 * *class as usize + usize::from(selects);
                if split[slot] == NONE {
                    split[slot] = classes;
                    classes += 1;
                    let from = *class as usize * words;
                    after.extend_from_slice(&before[from..from + words]);
                    if selects {
                        let word = after.len() - words + bit / 64;
                        after[word] |= 1 << (bit % 64);
                    }
                }
                *class = split[slot];
            }
            dim.classes = classes;
            selecting[at] = after;
            let classes =
                dims.iter()
                    .map(|dim| dim.classes as usize)
                    .try_fold(1usize, |tuples, classes| {
                        tuples
                            .checked_mul(classes)
                            .filter(|&tuples| tuples <= MAX_CROSS_CLASSES)
                    });
            if classes.is_none() {
                return Err(too_many());
            }
        }
        let mut tuples: usize = 1;
        for dim in dims.iter_mut().rev() {
            if dim.classes == 1 {
                dim.class = Vec::new();
            }
            dim.class_stride = tuples as u32;
            tuples = tuples
                .checked_mul(dim.classes as usize)
                .filter(|&tuples| tuples <= MAX_CROSS_CLASSES)
                .ok_or_else(too_many)?;
        }
        // The number of bins of each class.
        let sizes: Vec<Vec<u128>> = dims
            .iter()
            .map(|dim| {
                let mut sizes = vec![0; dim.classes as usize];
                for position in 0..dim.bins.len() as u32 {
                    sizes[dim.class_of(position) as usize] += 1;
                }
                sizes
            })
            .collect();
        // Whether term `term` selects the bins of class `class` of its
        // coverpoint.
        let selects = |term: usize, class: usize| {
            let dim = terms[term].dim as usize;
            let word = selecting[dim][class * words[dim] + local[term] / 64];
            word >> (local[term] % 64) & 1 != 0
        };
        // The declared bins that a class of a coverpoint may hold cross
        // products of, by the term that guards them; the others may hold
        // any.
        let mut guarded: Vec<Vec<Vec<u32>>> = dims
            .iter()
            .map(|dim| vec![Vec::new(); dim.classes as usize])
            .collect();
        let mut unguarded: Vec<u32> = Vec::new();
        for (bin, selection) in selections.iter().enumerate() {
            match selection.guard() {
                Some(term) => {
                    let dim = terms[term as usize].dim as usize;
                    spend(&mut steps, u64::from(dims[dim].classes), &cross.name)?;
                    for (class, bins) in guarded[dim].iter_mut().enumerate() {
                        if selects(term as usize, class) {
                            bins.push(bin as u32);
                        }
                    }
                }
                None => unguarded.push(bin as u32),
            }
        }
        let nodes: Vec<u64> = selections.iter().map(Term::nodes).collect();
        let removes = |bin: u32| cross.bins[bin as usize].kind != BinKind::Bins;
        let mut sets = vec![ProductSet::default()];
        let mut set_index: FxHashMap<Vec<u32>, u32> = FxHashMap::default();
        let mut table = vec![0; tuples];
        // The cross products in a declared bin: `Bb` of 19.11.2.
        let mut declared: u128 = 0;
        let mut holds = vec![false; cross.bins.len()];
        // The declared bins of the cross products of the tuple being looked
        // at.
        let mut bins: Vec<u32> = Vec::new();
        if !cross.bins.is_empty() {
            for (tuple, entry) in table.iter_mut().enumerate() {
                spend(&mut steps, 1, &cross.name)?;
                let class = |dim: usize| {
                    tuple / dims[dim].class_stride as usize % dims[dim].classes as usize
                };
                let bins_of = |term: u32| {
                    let dim = terms[term as usize].dim as usize;
                    selects(term as usize, class(dim))
                };
                bins.clear();
                let candidates = unguarded
                    .iter()
                    .chain((0..dims.len()).flat_map(|dim| guarded[dim][class(dim)].iter()));
                for &bin in candidates {
                    spend(&mut steps, nodes[bin as usize], &cross.name)?;
                    if selections[bin as usize].holds(&bins_of) {
                        bins.push(bin);
                    }
                }
                if bins.is_empty() {
                    continue;
                }
                bins.sort_unstable();
                if bins.iter().any(|&bin| removes(bin)) {
                    bins.retain(|&bin| removes(bin));
                }
                let count: u128 = (0..dims.len()).map(|dim| sizes[dim][class(dim)]).product();
                if count != 0 {
                    declared += count;
                    for &bin in &bins {
                        holds[bin as usize] = true;
                    }
                }
                *entry = match set_index.get(&bins[..]) {
                    Some(&set) => set,
                    None => {
                        let set = sets.len() as u32;
                        set_index.insert(bins.clone(), set);
                        sets.push(ProductSet { bins: bins.clone() });
                        set
                    }
                };
            }
        }
        let automatic = retain || cross.bins.is_empty();
        let bins = cross
            .bins
            .iter()
            .zip(holds)
            .map(|(bin, holds)| {
                let counter = *counters;
                *counters += 1;
                CrossBinShape {
                    kind: bin.kind,
                    counter,
                    iff: bin.iff,
                    empty: !holds,
                }
            })
            .collect();
        Ok(CrossShape {
            dims,
            bins,
            table,
            sets,
            automatic,
            auto_bins: if automatic { products - declared } else { 0 },
        })
    }
}

/// Add `count` steps to the `steps` a build of cross `cross` has taken: an
/// error beyond [`MAX_CROSS_WORK`].
fn spend(steps: &mut u64, count: u64, cross: &str) -> Result<(), String> {
    *steps = steps.saturating_add(count);
    if *steps > MAX_CROSS_WORK {
        return Err(format!(
            "sorting the cross products of cross `{cross}` into its bins takes more than \
             {MAX_CROSS_WORK} steps"
        ));
    }
    Ok(())
}

/// The select expression `select` over the terms that `term` numbers.
fn compile<'a>(
    select: &'a Select,
    term: &mut dyn FnMut(&'a Select) -> Result<u32, String>,
) -> Result<Term, String> {
    let all = |parts: &'a [Select], term: &mut dyn FnMut(&'a Select) -> Result<u32, String>| {
        parts
            .iter()
            .map(|part| compile(part, term))
            .collect::<Result<Vec<Term>, String>>()
    };
    Ok(match select {
        Select::BinsOf { .. } => Term::BinsOf(term(select)?),
        Select::And(parts) => Term::All(all(parts, term)?),
        Select::Or(parts) => Term::Any(all(parts, term)?),
    })
}

/// What counting a sample in the crosses of an instance uses, kept between
/// samples so that a sample allocates nothing.
#[derive(Clone, Debug, Default)]
pub(crate) struct CrossScratch {
    /// The bins of the coverpoints that counted the sample, coverpoint
    /// after coverpoint in the order they were sampled.
    pub hits: Vec<u32>,
    /// By coverpoint, where its bins are among `hits`: none without a
    /// sample of it.
    pub spans: Vec<(u32, u32)>,
    /// The positions of the hit bins of each crossed coverpoint.
    positions: Vec<u32>,
    /// Where those of each coverpoint start, and where the last end.
    starts: Vec<u32>,
    /// The hit bin of each coverpoint in the cross product being counted.
    at: Vec<u32>,
    /// The cross product being counted.
    product: Vec<u32>,
    /// The declared bins that counted the sample.
    counted: Vec<u32>,
}

impl CrossShape {
    /// Count the sample whose hit bins `scratch` has in the cross: each
    /// cross product of the bins that counted it is one of the declared
    /// bins that hold it, or else an automatic bin (19.6.1). A declared
    /// bin counts a sample once, in `counts`, unless its guard does not
    /// hold, where `holds` tells whether a predicate bit of the cross is
    /// set; an automatic bin counts in `automatic` by the key of its cross
    /// product. `illegal` is called with each illegal bin that counted and
    /// the first of its cross products (19.6.3).
    pub(crate) fn sample(
        &self,
        scratch: &mut CrossScratch,
        holds: &dyn Fn(u32) -> bool,
        counts: &mut [u64],
        automatic: &mut FxHashMap<u128, u64>,
        illegal: &mut dyn FnMut(u32, &[u32]),
    ) {
        let CrossScratch {
            hits,
            spans,
            positions,
            starts,
            at,
            product,
            counted,
        } = scratch;
        positions.clear();
        starts.clear();
        for dim in &self.dims {
            let start = positions.len();
            starts.push(start as u32);
            let (from, to) = spans[dim.point as usize];
            positions.extend(
                hits[from as usize..to as usize]
                    .iter()
                    .map(|&bin| dim.position[bin as usize])
                    .filter(|&position| position != NONE),
            );
            if positions.len() == start {
                // No bin of this coverpoint that takes part was hit.
                return;
            }
        }
        starts.push(positions.len() as u32);
        at.clear();
        at.resize(self.dims.len(), 0);
        product.clear();
        product.resize(self.dims.len(), 0);
        counted.clear();
        loop {
            let mut key = 0;
            let mut tuple = 0;
            for (index, dim) in self.dims.iter().enumerate() {
                let position = positions[(starts[index] + at[index]) as usize];
                product[index] = position;
                key += u128::from(position) * dim.stride;
                tuple += (dim.class_of(position) * dim.class_stride) as usize;
            }
            let set = &self.sets[self.table[tuple] as usize];
            if set.bins.is_empty() {
                if self.automatic {
                    *automatic.entry(key).or_default() += 1;
                }
            } else {
                for &bin in &set.bins {
                    let shape = &self.bins[bin as usize];
                    if shape.iff.is_some_and(|bit| !holds(bit)) || counted.contains(&bin) {
                        continue;
                    }
                    counted.push(bin);
                    counts[shape.counter as usize] += 1;
                    if shape.kind == BinKind::Illegal {
                        illegal(bin, product.as_slice());
                    }
                }
            }
            // The next choice of hit bins: the last coverpoint first.
            let mut index = self.dims.len();
            loop {
                let Some(dim) = index.checked_sub(1) else {
                    return;
                };
                at[dim] += 1;
                if starts[dim] + at[dim] < starts[dim + 1] {
                    break;
                }
                at[dim] = 0;
                index = dim;
            }
        }
    }

    /// The position of the bin of each coverpoint in the cross product of
    /// key `key`.
    pub(crate) fn product(&self, key: u128) -> impl Iterator<Item = u32> + '_ {
        self.dims.iter().map(move |dim| {
            key.checked_div(dim.stride)
                .and_then(|rest| rest.checked_rem(dim.bins.len() as u128))
                .unwrap_or(0) as u32
        })
    }

    /// Whether the cross products of the tuple of classes `tuple` are
    /// automatic bins.
    fn automatic_tuple(&self, tuple: usize) -> bool {
        self.automatic && self.sets[self.table[tuple] as usize].bins.is_empty()
    }

    /// Call `visit` with the key of each automatic bin, tuple of classes
    /// after tuple of classes and in the order of the keys within one,
    /// until it returns `false`. A cross with an automatic bin has a bin of
    /// every coverpoint, so every class has one.
    pub(crate) fn each_automatic(&self, mut visit: impl FnMut(u128) -> bool) {
        if self.auto_bins == 0 {
            return;
        }
        // The positions of the bins of each class.
        let members: Vec<Vec<Vec<u32>>> = self
            .dims
            .iter()
            .map(|dim| {
                let mut members = vec![Vec::new(); dim.classes as usize];
                for position in 0..dim.bins.len() as u32 {
                    members[dim.class_of(position) as usize].push(position);
                }
                members
            })
            .collect();
        let mut at = vec![0; self.dims.len()];
        for tuple in (0..self.table.len()).filter(|&tuple| self.automatic_tuple(tuple)) {
            let lists: Vec<&[u32]> = self
                .dims
                .iter()
                .zip(&members)
                .map(|(dim, members)| {
                    &members[tuple / dim.class_stride as usize % dim.classes as usize][..]
                })
                .collect();
            at.fill(0);
            'products: loop {
                let key = self
                    .dims
                    .iter()
                    .zip(&lists)
                    .zip(&at)
                    .map(|((dim, list), &at)| u128::from(list[at]) * dim.stride)
                    .sum::<u128>();
                if !visit(key) {
                    return;
                }
                let mut index = lists.len();
                loop {
                    let Some(dim) = index.checked_sub(1) else {
                        break 'products;
                    };
                    at[dim] += 1;
                    if at[dim] < lists[dim].len() {
                        break;
                    }
                    at[dim] = 0;
                    index = dim;
                }
            }
        }
    }
}

/// A cross of a shape with the bins of the coverpoints of the shape.
#[derive(Clone, Copy)]
pub(crate) struct Crossed<'a> {
    pub cross: &'a CrossShape,
    pub points: &'a [PointShape],
}

impl<'a> Crossed<'a> {
    /// What names the bin at `position` of crossed coverpoint `dim`.
    fn id(&self, dim: usize, position: u32) -> &'a BinId {
        let dim = &self.cross.dims[dim];
        &self.points[dim.point as usize].bins[dim.bins[position as usize] as usize].id
    }

    /// What names the automatic bin of key `key`: the bins of its cross
    /// product. Two automatic bins of that are equal are one bin of the
    /// type (19.11.3).
    pub(crate) fn ids(&self, key: u128) -> Vec<&'a BinId> {
        self.cross
            .product(key)
            .enumerate()
            .map(|(dim, position)| self.id(dim, position))
            .collect()
    }

    /// The name of the cross product of the bins at `positions` of the
    /// coverpoints of `cross`: `<bin1,...,binN>` (19.11.3).
    pub(crate) fn name(
        &self,
        descriptor: &TypeDescriptor,
        positions: impl Iterator<Item = u32>,
    ) -> String {
        let names: Vec<String> = positions
            .enumerate()
            .map(|(dim, position)| {
                let point = &descriptor.points[self.cross.dims[dim].point as usize];
                bin_name(point, self.id(dim, position))
            })
            .collect();
        format!("<{}>", names.join(","))
    }
}

/// The automatic bins of a cross of a type whose instances have it as
/// some shapes, united: bins of one name are one bin (19.11.3).
///
/// The bins of each crossed coverpoint over all the shapes fall into
/// classes again: two bins are of one class when each shape has both or
/// neither, and both in one of its classes. Whether a cross product is an
/// automatic bin of a shape is then the same for all cross products of a
/// choice of such classes.
pub(crate) struct United<'a> {
    shapes: &'a [Crossed<'a>],
    /// By crossed coverpoint, its classes: the shapes that have the bins,
    /// ascending, each with the class of the bins in it, and the number of
    /// bins.
    classes: Vec<Vec<(Vec<(u32, u32)>, u128)>>,
}

impl<'a> United<'a> {
    /// The union of the automatic bins of `shapes`.
    pub(crate) fn new(shapes: &'a [Crossed<'a>]) -> Self {
        let dims = shapes.first().map_or(0, |shape| shape.cross.dims.len());
        let classes = (0..dims)
            .map(|dim| {
                let mut bins: FxHashMap<&BinId, Vec<(u32, u32)>> = FxHashMap::default();
                for (index, shape) in shapes.iter().enumerate() {
                    let shape_dim = &shape.cross.dims[dim];
                    for position in 0..shape_dim.bins.len() as u32 {
                        bins.entry(shape.id(dim, position))
                            .or_default()
                            .push((index as u32, shape_dim.class_of(position)));
                    }
                }
                let mut counts: FxHashMap<Vec<(u32, u32)>, u128> = FxHashMap::default();
                for class in bins.into_values() {
                    *counts.entry(class).or_default() += 1;
                }
                counts.into_iter().collect()
            })
            .collect();
        Self { shapes, classes }
    }

    /// The number of bins, or [`u128::MAX`] if there are more: every
    /// instance has fewer than 2^128 cross products, but a union of
    /// instances of different bins may have more. None where finding it
    /// takes more than `bound` steps: a step is a coverpoint of a choice of
    /// a class for every coverpoint, or a coverpoint of a shape that may
    /// have the bins of such a choice.
    pub(crate) fn automatic(&self, bound: u64) -> Option<u128> {
        if self.shapes.is_empty() || self.classes.iter().any(Vec::is_empty) {
            return Some(0);
        }
        let mut steps: u64 = 0;
        let mut total: u128 = 0;
        let mut at = vec![0; self.classes.len()];
        let mut lists: Vec<&[(u32, u32)]> = Vec::with_capacity(self.classes.len());
        loop {
            steps = steps.saturating_add(self.classes.len() as u64);
            // The shapes that have the bins of every class of the choice:
            // those of the class with the fewest shapes that the others
            // have too.
            lists.clear();
            lists.extend(
                self.classes
                    .iter()
                    .zip(&at)
                    .map(|(classes, &at)| &classes[at].0[..]),
            );
            let (fewest, candidates) = lists
                .iter()
                .enumerate()
                .min_by_key(|(_, list)| list.len())
                .map(|(dim, list)| (dim, *list))
                .unwrap_or((0, &[]));
            let automatic = candidates.iter().any(|&(shape, class)| {
                steps = steps.saturating_add(lists.len() as u64);
                let cross = self.shapes[shape as usize].cross;
                let mut tuple = 0;
                for (dim, list) in lists.iter().enumerate() {
                    let class = if dim == fewest {
                        class
                    } else {
                        match list.binary_search_by_key(&shape, |&(shape, _)| shape) {
                            Ok(found) => list[found].1,
                            Err(_) => return false,
                        }
                    };
                    tuple += (class * cross.dims[dim].class_stride) as usize;
                }
                cross.automatic_tuple(tuple)
            });
            if steps > bound {
                return None;
            }
            if automatic {
                let bins = saturating_product(
                    self.classes
                        .iter()
                        .zip(&at)
                        .map(|(classes, &at)| classes[at].1),
                );
                total = total.saturating_add(bins);
            }
            let mut index = self.classes.len();
            loop {
                let Some(dim) = index.checked_sub(1) else {
                    return Some(total);
                };
                at[dim] += 1;
                if at[dim] < self.classes[dim].len() {
                    break;
                }
                at[dim] = 0;
                index = dim;
            }
        }
    }
}

/// The product of `factors`, or [`u128::MAX`] if it does not fit.
fn saturating_product(factors: impl Iterator<Item = u128>) -> u128 {
    factors.fold(1, u128::saturating_mul)
}

#[cfg(test)]
mod tests {
    use super::saturating_product;

    #[test]
    fn a_product_too_large_for_the_union_saturates() {
        assert_eq!(saturating_product([3, 5, 7].into_iter()), 105);
        assert_eq!(saturating_product(std::iter::empty()), 1);
        // Six coverpoints of 2^24 bins each in a union: 2^144 bins.
        assert_eq!(
            saturating_product(std::iter::repeat_n(1 << 24, 6)),
            u128::MAX
        );
        assert_eq!(saturating_product([u128::MAX, 0].into_iter()), 0);
    }
}
