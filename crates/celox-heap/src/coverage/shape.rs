//! What a construction makes of a covergroup type: the bins of each
//! coverpoint with the values they hold (IEEE 1800-2023 19.5).
//!
//! A shape is a function of the type's descriptor and of what a
//! construction hands over, so the instances that are built from equal
//! values share one.

use std::collections::{BTreeMap, BTreeSet};

use num_bigint::{BigInt, BigUint};
use serde::{Deserialize, Serialize};

use super::cross::{CrossInputs, CrossShape};
use super::descriptor::{
    BinArray, BinDescriptor, BinFilter, BinKind, BinValues, Bound, Constant, Operand,
    PointDescriptor, PointType, RangeDescriptor, TransitionStep, TypeDescriptor, ValueType,
};
use super::transitions::{self, TransitionBuilder, TransitionShape};
use super::value::{
    Exact, Interval, KeyValue, Matched, Matcher, Partition, Values, Wild, bias, numeric, ones,
    value_of_key,
};
use super::{MAX_BINS_PER_ITEM, MAX_WILDCARD_EXPANSION};

/// An element of a bins range list (19.5.1) with its operands evaluated.
#[derive(Clone, Copy, Debug)]
pub enum RangeValue<'a> {
    /// A single value.
    Value(&'a Constant),
    /// `[low:high]`, where `None` is `$`.
    Range {
        low: Option<&'a Constant>,
        high: Option<&'a Constant>,
    },
}

/// The values of an element of a bins range list that take part in the bin
/// (19.5.7), with the warnings its resolution gives.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Resolved {
    /// In the order of the element's values.
    pub items: Vec<ResolvedItem>,
    pub warnings: Vec<String>,
}

/// Values of a coverpoint that a bin holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ResolvedItem {
    /// The values `low..=high` of the coverpoint type.
    Range { low: BigInt, high: BigInt },
    /// One value with x or z bits, as the bits of its two planes in the
    /// width of the coverpoint: only an equal value is in the bin (19.5.4).
    Exact { value: BigUint, mask: BigUint },
}

const CLAUSE: &str = "(IEEE 1800-2023 19.5.7)";

/// Resolve the element `range` of a bins range list against the effective
/// type `ty` of its coverpoint (19.5.7).
///
/// A value `b` is cast to the type of the coverpoint, and takes part unless
/// the type is unsigned and `b` is signed and negative, or the cast value
/// differs from `b` as `==` compares them (11.4.5), or `b` has x or z
/// bits. So the values that take part are those from 0 to the largest
/// bit pattern of the type, or for a signed `b` on a signed type the values
/// of the type; an unsigned `b` on a signed type stands for the value its low
/// bits read as, so `8'hFF` is -1 on an 8-bit signed coverpoint. A range
/// stands for those of its values that take part, with a warning when
/// some do not; its bounds are first brought to their common type as the
/// operands of a binary operator are (11.8.1), with a warning where that
/// reads a negative bound as a large value because the other bound is
/// unsigned. On a 4-state type a single value with x or z bits takes part
/// as it is written.
pub fn resolve(ty: &PointType, range: RangeValue<'_>) -> Resolved {
    let mut resolved = Resolved::default();
    match range {
        RangeValue::Value(value) => resolve_value(ty, value, &mut resolved),
        RangeValue::Range { low, high } => resolve_range(ty, low, high, &mut resolved),
    }
    resolved
}

/// The warnings that the element `range` of a bin of a coverpoint of type
/// `ty` gives when an instance is built (19.5.7), where `wildcard` is
/// whether the declaration of the bin is `wildcard` (19.5.4).
///
/// They are those of [`resolve`], but for a single value with x or z bits
/// in a wildcard bin: such a bit stands for both 0 and 1 before the value
/// is resolved, so the value warns when some of the values it stands for
/// take no part, not of the bits.
pub fn resolve_warnings(ty: &PointType, wildcard: bool, range: RangeValue<'_>) -> Vec<String> {
    let mut warnings = Vec::new();
    resolve_into(ty, wildcard, range, &mut Vec::new(), &mut warnings);
    warnings
}

/// How many values the element `range` of a bin of a coverpoint of type
/// `ty` holds, each as often as the element lists it, where `wildcard` is
/// whether the declaration of the bin is `wildcard` (19.5.4): the number of
/// values that a `with` filter of the bin is asked about for the element
/// (19.5.1.1). `None` for a wildcard value of more than
/// [`MAX_WILDCARD_EXPANSION`] ranges of values, which is matched as a
/// pattern and whose values are not listed.
pub fn value_count(ty: &PointType, wildcard: bool, range: RangeValue<'_>) -> Option<BigUint> {
    let mut items = Vec::new();
    resolve_into(ty, wildcard, range, &mut items, &mut Vec::new());
    items
        .iter()
        .map(|item| match item {
            Item::Wild { .. } => None,
            item => Some(item.count()),
        })
        .sum()
}

/// The bits of the two planes of `constant` in its width.
pub(crate) fn planes(constant: &Constant) -> (BigUint, BigUint) {
    let ones = ones(constant.ty.width);
    (&constant.value & &ones, &constant.mask & &ones)
}

/// The lowest and highest value a `b` of signedness `signed` may have to
/// take part in a bin of a coverpoint of type `ty`.
fn passing(ty: &PointType, signed: bool) -> (BigInt, BigInt) {
    if ty.signed && signed {
        type_range(ty)
    } else {
        (BigInt::ZERO, BigInt::from(ones(ty.width)))
    }
}

/// The lowest and highest value of type `ty`: those of the lowest key and
/// of the key with every bit set.
fn type_range(ty: &PointType) -> (BigInt, BigInt) {
    (
        value_of_key(&BigUint::ZERO, ty.width, ty.signed),
        value_of_key(&ones(ty.width), ty.width, ty.signed),
    )
}

/// The value of type `ty` that the passing value `value` of a `b` of
/// signedness `signed` casts to.
fn cast(ty: &PointType, signed: bool, value: &BigInt) -> BigInt {
    if ty.signed && !signed && *value > type_range(ty).1 {
        value - (BigInt::from(1) << ty.width)
    } else {
        value.clone()
    }
}

/// Add the values of type `ty` that the passing values `low..=high` of a
/// `b` of signedness `signed` cast to.
fn push_cast(
    ty: &PointType,
    signed: bool,
    low: BigInt,
    high: BigInt,
    items: &mut Vec<ResolvedItem>,
) {
    let top = type_range(ty).1;
    if ty.signed && !signed && low <= top && high > top {
        // The bit patterns from the sign bit on read as negative values.
        items.push(ResolvedItem::Range {
            low,
            high: top.clone(),
        });
        items.push(ResolvedItem::Range {
            low: cast(ty, signed, &(top + 1)),
            high: cast(ty, signed, &high),
        });
    } else {
        items.push(ResolvedItem::Range {
            low: cast(ty, signed, &low),
            high: cast(ty, signed, &high),
        });
    }
}

/// `constant` as source code writes it: a number, or its bits where some
/// are x or z.
pub(crate) fn show(constant: &Constant) -> String {
    let (value, mask) = planes(constant);
    if mask.bits() == 0 {
        return numeric(&value, constant.ty.width, constant.ty.signed).to_string();
    }
    show_planes(&value, &mask, constant.ty.width)
}

/// The sized binary literal of the bits `value` with the x and z bits
/// `mask`.
pub(crate) fn show_planes(value: &BigUint, mask: &BigUint, width: u32) -> String {
    let digits: String = (0..u64::from(width))
        .rev()
        .map(|bit| match (mask.bit(bit), value.bit(bit)) {
            (false, false) => '0',
            (false, true) => '1',
            (true, true) => 'x',
            (true, false) => 'z',
        })
        .collect();
    format!("{width}'b{digits}")
}

fn show_bound(bound: Option<&Constant>) -> String {
    bound.map_or_else(|| "$".to_string(), show)
}

fn resolve_value(ty: &PointType, b: &Constant, resolved: &mut Resolved) {
    let (value, mask) = planes(b);
    if mask.bits() != 0 {
        if !ty.four_state {
            resolved.warnings.push(format!(
                "value {} has x or z bits and does not take part {CLAUSE}",
                show(b)
            ));
        } else if let Some(item) = exact(ty, b, value, mask) {
            resolved.items.push(item);
        } else {
            resolved.warnings.push(format!(
                "value {} is not a value of the coverpoint type and does not take part {CLAUSE}",
                show(b)
            ));
        }
        return;
    }
    let value = numeric(&value, b.ty.width, b.ty.signed);
    let (low, high) = passing(ty, b.ty.signed);
    if value < low || value > high {
        resolved.warnings.push(format!(
            "value {value} is not a value of the coverpoint type and does not take part {CLAUSE}"
        ));
        return;
    }
    push_cast(ty, b.ty.signed, value.clone(), value, &mut resolved.items);
}

/// The value of bits `value` with the x and z bits `mask` of `b` in the
/// width of the 4-state coverpoint type `ty`, unless the cast changes it.
fn exact(
    ty: &PointType,
    b: &Constant,
    mut value: BigUint,
    mut mask: BigUint,
) -> Option<ResolvedItem> {
    let (from, to) = (b.ty.width, ty.width);
    let top = u64::from(from.checked_sub(1)?);
    if b.ty.signed && !ty.signed && value.bit(top) && !mask.bit(top) {
        // Signed and negative on an unsigned type.
        return None;
    }
    if from < to {
        let extension = ones(to) ^ ones(from);
        if b.ty.signed && value.bit(top) {
            value |= &extension;
        }
        if b.ty.signed && mask.bit(top) {
            mask |= &extension;
        }
    } else if from > to {
        let kept = u64::from(to.checked_sub(1)?);
        let fill = |plane: &BigUint| {
            if b.ty.signed && ty.signed && plane.bit(kept) {
                ones(from - to)
            } else {
                BigUint::ZERO
            }
        };
        if &value >> to != fill(&value) || &mask >> to != fill(&mask) {
            return None;
        }
        value &= ones(to);
        mask &= ones(to);
    }
    Some(ResolvedItem::Exact { value, mask })
}

fn resolve_range(
    ty: &PointType,
    low: Option<&Constant>,
    high: Option<&Constant>,
    resolved: &mut Resolved,
) {
    let shown = || format!("[{}:{}]", show_bound(low), show_bound(high));
    if [low, high]
        .into_iter()
        .flatten()
        .any(|bound| planes(bound).1.bits() != 0)
    {
        resolved.warnings.push(format!(
            "range {} has x or z bits in a bound and does not take part {CLAUSE}",
            shown()
        ));
        return;
    }
    let (type_low, type_high) = type_range(ty);
    let dropped = |resolved: &mut Resolved| {
        resolved.warnings.push(format!(
            "range {} holds no value of the coverpoint type and does not take part {CLAUSE}",
            shown()
        ));
    };
    let limited = |resolved: &mut Resolved| {
        resolved.warnings.push(format!(
            "range {} is limited to the values of the coverpoint type {CLAUSE}",
            shown()
        ));
    };
    let value = |bound: &Constant| numeric(&planes(bound).0, bound.ty.width, bound.ty.signed);
    match (low, high) {
        (None, None) => resolved.items.push(ResolvedItem::Range {
            low: type_low,
            high: type_high,
        }),
        (None, Some(high)) => {
            // `$` is the lowest value of the coverpoint type.
            let (first, last) = passing(ty, high.ty.signed);
            let bound = value(high);
            if bound < first {
                dropped(resolved);
            } else if bound > last {
                limited(resolved);
                resolved.items.push(ResolvedItem::Range {
                    low: type_low,
                    high: type_high,
                });
            } else {
                resolved.items.push(ResolvedItem::Range {
                    low: type_low,
                    high: cast(ty, high.ty.signed, &bound),
                });
            }
        }
        (Some(low), None) => {
            let (first, last) = passing(ty, low.ty.signed);
            let bound = value(low);
            if bound > last {
                dropped(resolved);
            } else if bound < first {
                limited(resolved);
                resolved.items.push(ResolvedItem::Range {
                    low: type_low,
                    high: type_high,
                });
            } else {
                resolved.items.push(ResolvedItem::Range {
                    low: cast(ty, low.ty.signed, &bound),
                    high: type_high,
                });
            }
        }
        (Some(low), Some(high)) => {
            // The common type of the bounds (11.8.1): a bound is extended
            // by its sign only where both are signed.
            let signed = low.ty.signed && high.ty.signed;
            let common = |bound: &Constant| {
                if signed {
                    value(bound)
                } else {
                    BigInt::from(planes(bound).0)
                }
            };
            // A negative bound that is not read as its value warns, as a
            // negative value does on an unsigned coverpoint (19.5.7 b) 1),
            // whatever the range then holds.
            for bound in [low, high] {
                let (written, read) = (value(bound), common(bound));
                if written != read {
                    resolved.warnings.push(format!(
                        "bound {written} of range {} is read as {read} beside an unsigned bound \
                         {CLAUSE}",
                        shown()
                    ));
                }
            }
            let (low, high) = (common(low), common(high));
            if low > high {
                // An empty range (11.4.13).
                return;
            }
            let (first, last) = passing(ty, signed);
            let (from, to) = (low.clone().max(first), high.clone().min(last));
            if from > to {
                dropped(resolved);
                return;
            }
            if from != low || to != high {
                limited(resolved);
            }
            push_cast(ty, signed, from, to, &mut resolved.items);
        }
    }
}

/// The bins of the coverpoints and crosses of an instance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Shape {
    pub points: Vec<PointShape>,
    pub crosses: Vec<CrossShape>,
    /// The number of hit counters an instance of the shape has: those of
    /// the bins of the coverpoints, then those of the bins that the crosses
    /// declare.
    pub counters: u32,
    /// The number of `default` bin arrays, each of which an instance keeps
    /// the sampled values of.
    pub arrays: u32,
    /// What building the shape warns of.
    pub warnings: Vec<Warning>,
}

/// A warning of a construction: what it is about and what is wrong.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Warning {
    pub subject: String,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PointShape {
    /// The automatic bins, then the bins of the declarations in their
    /// order: one of a scalar declaration and of a `default` one, and one
    /// for each element of an array.
    pub bins: Vec<BinShape>,
    pub matcher: Matcher,
    /// The `default` bins among `bins` (19.5).
    pub defaults: Vec<u32>,
    /// Whether one of them is a `default` bin array.
    pub default_array: bool,
    /// The sequences of the transition bins and the `default sequence`
    /// bins (19.5.2), if the coverpoint has any.
    pub transitions: Option<Box<TransitionShape>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BinShape {
    pub id: BinId,
    pub kind: BinKind,
    pub class: BinClass,
    /// Whether no value is associated with the bin, which then takes no
    /// part in coverage (19.11.1).
    pub empty: bool,
    /// The hit counter of the bin in an instance.
    pub counter: u32,
    /// The predicate bit of the guard of the declaration: a sample with
    /// the bit clear does not count in the bin.
    pub iff: Option<u32>,
    /// The predicate bit of a filter evaluated at each sample: a value
    /// with the bit clear is none of the bin.
    pub filter: Option<u32>,
}

impl BinShape {
    /// Whether the bin counts towards coverage: it is a `bins` bin with a
    /// value or a sequence (19.11.1) that is no `default` or `default
    /// sequence` bin (19.5, 19.5.2). These are the bins of a coverpoint
    /// that a cross has cross products of (19.6).
    pub(crate) fn coverable(&self) -> bool {
        self.kind == BinKind::Bins
            && matches!(self.class, BinClass::Value | BinClass::Transition)
            && !self.empty
    }
}

/// What the values of a bin are.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum BinClass {
    /// Those of its declaration, or of the partition of automatic bins.
    Value,
    /// Those in no other bin (19.5).
    Default,
    /// The same, with a bin for each value that is sampled: the array of
    /// this number among those of the shape. The counter of the bin has the
    /// hits of all of them.
    DefaultArray(u32),
    /// Sequences of values of the coverpoint (19.5.2).
    Transition,
    /// The samples that end no sequence of another transition bin and go
    /// on with none (`default sequence`, 19.5.2).
    DefaultSequence,
}

/// What names a bin of a coverpoint: the bins of two instances that are
/// equal here are one bin of the type (19.11.3).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct BinId {
    /// The bin declaration, or 0 for an automatic bin.
    pub decl: u32,
    pub tag: NameTag,
}

impl BinId {
    /// Whether the bin is an automatic bin, of no declaration.
    pub(crate) fn automatic(&self) -> bool {
        matches!(self.tag, NameTag::Auto(..) | NameTag::Member(_))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) enum NameTag {
    /// The bin of a scalar declaration, which has its name.
    Scalar,
    /// `auto[low:high]`, or `auto[value]` for one value: an automatic bin
    /// of the values of these keys (19.5.3).
    Auto(KeyValue, KeyValue),
    /// `auto[NAME]`: the automatic bin of the enumerator of this index.
    Member(u32),
    /// `name[value]`: the bin of this value in an array of a bin for each
    /// value (19.5.1).
    Value(ValueKey),
    /// `name[index]`: the bin of this index in an array of a fixed number
    /// of bins.
    Index(u32),
    /// `name[v1=>v2=>...]`: the bin of this sequence of values in an array
    /// of a bin for each sequence of a transition (19.5.2).
    Sequence(Vec<ValueKey>),
}

/// A value of a coverpoint as the identity of a bin and of what a `default`
/// bin array counts. The order is that of the values, those with x or z
/// bits last.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub(crate) enum ValueKey {
    /// The value of this key.
    Known(KeyValue),
    /// The value of these bits with the x and z bits `mask`.
    Unknown { value: KeyValue, mask: KeyValue },
}

impl ValueKey {
    /// The value as source code writes it in a type of `width` bits.
    pub(crate) fn show(&self, width: u32, signed: bool) -> String {
        match self {
            Self::Known(key) => value_of_key(&key.key(), width, signed).to_string(),
            Self::Unknown { value, mask } => show_planes(&value.key(), &mask.key(), width),
        }
    }
}

/// The name of bin `id` of coverpoint `point` (19.11.3).
pub(crate) fn bin_name(point: &PointDescriptor, id: &BinId) -> String {
    let declared = || {
        point
            .bins
            .get(id.decl as usize)
            .map_or("", |bin| bin.name.as_str())
    };
    match &id.tag {
        NameTag::Scalar => declared().to_string(),
        NameTag::Auto(low, high) => {
            let value = |key: &KeyValue| value_of_key(&key.key(), point.ty.width, point.ty.signed);
            if low == high {
                format!("auto[{}]", value(low))
            } else {
                format!("auto[{}:{}]", value(low), value(high))
            }
        }
        NameTag::Member(index) => point
            .ty
            .members
            .get(*index as usize)
            .map_or_else(String::new, |(name, _)| format!("auto[{name}]")),
        NameTag::Value(value) => format!(
            "{}[{}]",
            declared(),
            value.show(point.ty.width, point.ty.signed)
        ),
        NameTag::Index(index) => format!("{}[{index}]", declared()),
        NameTag::Sequence(values) => format!(
            "{}[{}]",
            declared(),
            show_sequence(values.iter(), point.ty.width, point.ty.signed)
        ),
    }
}

/// The sequence of values `values` of a type of `width` bits and signedness
/// `signed` as 19.5.2 writes a transition: `4=>5=>6`.
pub(crate) fn show_sequence<'a>(
    values: impl Iterator<Item = &'a ValueKey>,
    width: u32,
    signed: bool,
) -> String {
    let shown: Vec<String> = values.map(|value| value.show(width, signed)).collect();
    shown.join("=>")
}

/// The options of a coverpoint that decide its bins.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct PointOptions {
    pub auto_bin_max: i32,
    pub detect_overlap: bool,
}

/// The key of value `value` of type `ty`.
pub(crate) fn key(ty: &PointType, value: &BigInt) -> BigUint {
    BigUint::try_from(value + BigInt::from(bias(ty.width, ty.signed))).unwrap_or_default()
}

/// Values of a bin in the order its declaration lists them.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) enum Item {
    /// The values of the keys `low..=high`.
    Range { low: BigUint, high: BigUint },
    /// One value with x or z bits, as the bits of its two planes.
    Exact { value: BigUint, mask: BigUint },
    /// The values whose bits are those of `value` where `care` has a bit
    /// set: a wildcard value of more than [`MAX_WILDCARD_EXPANSION`] ranges
    /// of values.
    Wild { value: BigUint, care: BigUint },
}

impl Item {
    /// The number of values, which a wildcard pattern does not tell.
    pub(crate) fn count(&self) -> BigUint {
        match self {
            Self::Range { low, high } => high - low + 1u8,
            Self::Exact { .. } => BigUint::from(1u8),
            Self::Wild { .. } => BigUint::ZERO,
        }
    }

    /// The bits of the two planes of value `offset` of the item in a type
    /// of `width` bits and signedness `signed`.
    pub(crate) fn planes(&self, offset: u64, width: u32, signed: bool) -> (BigUint, BigUint) {
        match self {
            Self::Range { low, .. } => ((low + offset) ^ bias(width, signed), BigUint::ZERO),
            Self::Exact { value, mask } => (value.clone(), mask.clone()),
            Self::Wild { .. } => (BigUint::ZERO, BigUint::ZERO),
        }
    }
}

/// The elements of an array that a construction hands over (19.5.1.2): a
/// value word and a mask word for each 64 bits of each element.
#[derive(Clone, Copy)]
pub(crate) struct SetInput<'a> {
    pub ty: ValueType,
    pub words: &'a [u64],
}

impl SetInput<'_> {
    fn elements(&self) -> impl Iterator<Item = Constant> + '_ {
        let size = self.ty.width.div_ceil(64) as usize * 2;
        self.words.chunks_exact(size.max(2)).map(|words| Constant {
            ty: self.ty,
            value: super::big(words.iter().copied().step_by(2)),
            mask: super::big(words.iter().copied().skip(1).step_by(2)),
        })
    }
}

/// What a construction hands over to build a shape from.
pub(crate) struct Construction<'a> {
    /// The integral inputs by input index.
    pub inputs: &'a [Option<Constant>],
    pub sets: &'a [SetInput<'a>],
    /// By filter index, which of the values the filter was asked about it
    /// keeps: a bit for each, in their order.
    pub keeps: &'a [Vec<u64>],
    /// The options of each coverpoint.
    pub options: &'a [PointOptions],
    /// `type_option.distribute_first`: a filter is applied to the values
    /// of a bin array once they are distributed (19.5.1.1).
    pub distribute_first: bool,
    /// `option.cross_retain_auto_bins` of each cross.
    pub retain: &'a [bool],
}

/// The values of the declaration `bin` of `point` in their order, before any
/// filter, with the warnings of their resolution (19.5.7).
pub(crate) fn value_items(
    point: &PointDescriptor,
    bin: &BinDescriptor,
    inputs: &[Option<Constant>],
    sets: &[SetInput<'_>],
) -> Result<(Vec<Item>, Vec<String>), String> {
    let mut items = Vec::new();
    let mut warnings = Vec::new();
    match &bin.values {
        BinValues::Ranges { ranges, .. } => {
            return range_items(&point.ty, bin.wildcard, ranges, inputs);
        }
        BinValues::Set(index) => {
            let set = sets
                .get(*index as usize)
                .ok_or_else(|| format!("set {index} was not handed over"))?;
            // A set may have many elements that take no part for the same
            // reason: the first of them is reported.
            let mut first = None;
            let mut more = 0u64;
            let mut found = Vec::new();
            for element in set.elements() {
                let element = RangeValue::Value(&element);
                resolve_into(&point.ty, bin.wildcard, element, &mut items, &mut found);
                for text in found.drain(..) {
                    if first.is_none() {
                        first = Some(text);
                    } else {
                        more += 1;
                    }
                }
            }
            match first {
                Some(first) if more != 0 => {
                    warnings.push(format!("{first}; so do {more} more of its elements"))
                }
                Some(first) => warnings.push(first),
                None => {}
            }
        }
        BinValues::Default | BinValues::Transitions(_) | BinValues::DefaultSequence => {}
    }
    Ok((items, warnings))
}

/// The values of the range list `ranges` of a bin of a coverpoint of type
/// `ty` in their order, where `wildcard` is whether the declaration is
/// `wildcard`, with the warnings of their resolution (19.5.7).
pub(crate) fn range_items(
    ty: &PointType,
    wildcard: bool,
    ranges: &[RangeDescriptor],
    inputs: &[Option<Constant>],
) -> Result<(Vec<Item>, Vec<String>), String> {
    let mut items = Vec::new();
    let mut warnings = Vec::new();
    for range in ranges {
        let range = evaluate(range, inputs)?;
        resolve_into(ty, wildcard, range, &mut items, &mut warnings);
    }
    Ok((items, warnings))
}

/// The lowest value that the bound `b` of a range of a wildcard bin stands
/// for, or with `highest` the highest: an x or z bit stands for 0 and for 1
/// (19.5.4). `signed` is whether the range reads the bound as a signed
/// value, which it does not beside an unsigned bound (see
/// [`resolve_range`]).
fn wildcard_bound(b: &Constant, highest: bool, signed: bool) -> Constant {
    let (value, mask) = planes(b);
    let mut bits = if highest {
        &value | &mask
    } else {
        &value & (ones(b.ty.width) ^ &mask)
    };
    // The sign of a value that is read as signed.
    if let Some(top) = b.ty.width.checked_sub(1).map(u64::from)
        && signed
        && mask.bit(top)
    {
        bits.set_bit(top, !highest);
    }
    Constant {
        ty: b.ty,
        value: bits,
        mask: BigUint::ZERO,
    }
}

/// Add the values of the element `range` of a bin of a coverpoint of type
/// `ty` to `items`.
///
/// In a wildcard bin the x and z bits of a value are "treated as all
/// possible 0 and 1 values prior to applying these rules" (19.5.7). For a
/// single value those are the values it stands for. A range each of whose
/// bounds stands for several values holds what the ranges of all of them
/// hold: the values from its lowest low bound to its highest high bound,
/// lowest and highest as the range compares its bounds, in their common
/// type (11.8.1).
fn resolve_into(
    ty: &PointType,
    wildcard: bool,
    range: RangeValue<'_>,
    items: &mut Vec<Item>,
    warnings: &mut Vec<String>,
) {
    let bounds;
    let range = match range {
        RangeValue::Value(value) if wildcard && planes(value).1.bits() != 0 => {
            wildcard_items(ty, value, items, warnings);
            return;
        }
        RangeValue::Range { low, high }
            if wildcard
                && [low, high]
                    .into_iter()
                    .flatten()
                    .any(|bound| planes(bound).1.bits() != 0) =>
        {
            let signed = [low, high]
                .into_iter()
                .flatten()
                .all(|bound| bound.ty.signed);
            bounds = (
                low.map(|low| wildcard_bound(low, false, signed)),
                high.map(|high| wildcard_bound(high, true, signed)),
            );
            RangeValue::Range {
                low: bounds.0.as_ref(),
                high: bounds.1.as_ref(),
            }
        }
        range => range,
    };
    let resolved = resolve(ty, range);
    warnings.extend(resolved.warnings);
    for item in resolved.items {
        match item {
            ResolvedItem::Range { low, high } => push_range(items, key(ty, &low), key(ty, &high)),
            ResolvedItem::Exact { value, mask } => items.push(Item::Exact { value, mask }),
        }
    }
}

/// Add the values of the keys `low..=high` to `items`: values that follow
/// those of the last item are values of it.
fn push_range(items: &mut Vec<Item>, low: BigUint, high: BigUint) {
    if let Some(Item::Range { high: last, .. }) = items.last_mut()
        && &*last + 1u8 == low
    {
        *last = high;
    } else {
        items.push(Item::Range { low, high });
    }
}

/// The values that the wildcard value `b` stands for in a bin of a
/// coverpoint of type `ty`, as patterns of the bits of the type: the bits
/// to have and which bits those are. An x or z bit of `b` stands for 0 and
/// for 1 (19.5.4) before the value is resolved (19.5.7), so of the values
/// `b` stands for those take part that a value without such bits would.
/// With the patterns comes whether some do not.
fn wildcard_patterns(ty: &PointType, b: &Constant) -> (Vec<(BigUint, BigUint)>, bool) {
    let (from, to) = (b.ty.width, ty.width);
    let (value, mask) = planes(b);
    let care = ones(from) ^ mask;
    let value = value & &care;
    let (Some(top), true) = (from.checked_sub(1).map(u64::from), to != 0) else {
        return (Vec::new(), true);
    };
    let sign = BigUint::from(1u8) << top;
    // A signed `b` is extended by its sign, so the values of either sign
    // are a pattern of their own.
    let alternatives = if b.ty.signed && !care.bit(top) {
        vec![
            (value.clone(), &care | &sign),
            (&value | &sign, &care | &sign),
        ]
    } else {
        vec![(value, care)]
    };
    let mut patterns = Vec::new();
    let mut dropped = false;
    for (value, care) in alternatives {
        let negative = b.ty.signed && value.bit(top);
        if negative && !ty.signed {
            // Signed and negative on an unsigned type (19.5.7 b) 1).
            dropped = true;
            continue;
        }
        if from > to {
            // The bits that the cast drops are those the value is
            // extended by when it is compared with `b` (19.5.7 b) 2):
            // the sign of the cast value where both are signed, else 0.
            let mut fixed = ones(from) ^ ones(to);
            if b.ty.signed && ty.signed {
                fixed |= BigUint::from(1u8) << (to - 1);
            }
            let fill = if negative {
                fixed.clone()
            } else {
                BigUint::ZERO
            };
            if ((&value ^ &fill) & &care & &fixed).bits() != 0 {
                dropped = true;
                continue;
            }
            if (&fixed & (ones(from) ^ &care)).bits() != 0 {
                dropped = true;
            }
            let care = (care | &fixed) & ones(to);
            patterns.push(((value | fill) & &care, care));
        } else {
            let extension = ones(to) ^ ones(from);
            let value = if negative { value | &extension } else { value };
            patterns.push((value, care | extension));
        }
    }
    (patterns, dropped)
}

/// The number of bits of a pattern of a type of `width` bits that are free
/// above the lowest bit that `care` has set: each choice of them is a range
/// of values. Without a bit to have, every value is one range.
fn free_bits(care: &BigUint, width: u32) -> Vec<u64> {
    let Some(lowest) = care.trailing_zeros() else {
        return Vec::new();
    };
    (lowest..u64::from(width))
        .filter(|&bit| !care.bit(bit))
        .collect()
}

/// Add the values the wildcard value `b` stands for to `items` (19.5.4): as
/// ranges of values in their order, or where those are more than
/// [`MAX_WILDCARD_EXPANSION`] as patterns.
fn wildcard_items(ty: &PointType, b: &Constant, items: &mut Vec<Item>, warnings: &mut Vec<String>) {
    let (patterns, dropped) = wildcard_patterns(ty, b);
    if patterns.is_empty() {
        warnings.push(format!(
            "wildcard value {} stands for no value of the coverpoint type and does not take part \
             {CLAUSE}",
            show(b)
        ));
        return;
    }
    if dropped {
        warnings.push(format!(
            "wildcard value {} is limited to the values of the coverpoint type {CLAUSE}",
            show(b)
        ));
    }
    let free: Vec<Vec<u64>> = patterns
        .iter()
        .map(|(_, care)| free_bits(care, ty.width))
        .collect();
    let ranges: u64 = free
        .iter()
        .map(|free| 1u64.checked_shl(free.len() as u32).unwrap_or(u64::MAX))
        .fold(0, u64::saturating_add);
    if ranges > MAX_WILDCARD_EXPANSION as u64 {
        items.extend(
            patterns
                .into_iter()
                .map(|(value, care)| Item::Wild { value, care }),
        );
        return;
    }
    let sign = bias(ty.width, ty.signed);
    let mut expanded: Vec<(BigUint, BigUint)> = Vec::with_capacity(ranges as usize);
    for ((value, care), free) in patterns.iter().zip(&free) {
        // The bits below the lowest bit to have are free in a range.
        let low_bits = match care.trailing_zeros() {
            Some(lowest) => ones(lowest as u32),
            None => ones(ty.width),
        };
        for choice in 0..1u64 << free.len() {
            let mut bits = value.clone();
            for (index, &bit) in free.iter().enumerate() {
                bits.set_bit(bit, choice >> index & 1 != 0);
            }
            // The sign bit is the same in all values of a range unless
            // the range is every value.
            let (low, high) = if care.bits() == 0 {
                (BigUint::ZERO, ones(ty.width))
            } else {
                (&bits ^ &sign, (&bits | &low_bits) ^ &sign)
            };
            expanded.push((low, high));
        }
    }
    expanded.sort_unstable();
    for (low, high) in expanded {
        push_range(items, low, high);
    }
}

/// The items of `items` that the filter whose answers are the bits of
/// `keeps` keeps, where the first value of `items` is the value number
/// `position` that the filter was asked about.
fn kept(items: &[Item], mut position: u64, keeps: &[u64]) -> Vec<Item> {
    let keep = |position: u64| {
        keeps
            .get((position / 64) as usize)
            .is_some_and(|word| word >> (position % 64) & 1 != 0)
    };
    // No value beyond the answers is kept.
    let end = keeps.len() as u64 * 64;
    let mut out = Vec::new();
    for item in items {
        match item {
            Item::Range { low, high } => {
                let count = u64::try_from(high - low + 1u8).unwrap_or(u64::MAX);
                let mut run: Option<(u64, u64)> = None;
                let mut flush = |run: &mut Option<(u64, u64)>| {
                    if let Some((first, last)) = run.take() {
                        out.push(Item::Range {
                            low: low + first,
                            high: low + last,
                        });
                    }
                };
                for offset in 0..count.min(end.saturating_sub(position)) {
                    if keep(position + offset) {
                        run = Some((run.map_or(offset, |(first, _)| first), offset));
                    } else {
                        flush(&mut run);
                    }
                }
                flush(&mut run);
                position = position.saturating_add(count);
            }
            Item::Exact { .. } => {
                if keep(position) {
                    out.push(item.clone());
                }
                position = position.saturating_add(1);
            }
            Item::Wild { .. } => {}
        }
    }
    out
}

/// Takes the values of items in their order.
struct Cursor<'a> {
    items: &'a [Item],
    item: usize,
    /// The values of the item that are taken.
    offset: BigUint,
    /// The number of values that are taken.
    position: BigUint,
}

impl Cursor<'_> {
    /// The next `count` values, or what is left of them: without a count
    /// all that is left. With them comes the number of values before them.
    fn take(&mut self, count: Option<&BigUint>) -> (Vec<Item>, BigUint) {
        let start = self.position.clone();
        let mut left = count.cloned();
        let mut out = Vec::new();
        while let Some(item) = self.items.get(self.item) {
            if left.as_ref().is_some_and(|left| left.bits() == 0) {
                break;
            }
            match item {
                Item::Range { low, high } => {
                    let available = high - low + 1u8 - &self.offset;
                    let take = match &left {
                        Some(left) => available.clone().min(left.clone()),
                        None => available.clone(),
                    };
                    let from = low + &self.offset;
                    out.push(Item::Range {
                        high: &from + &take - 1u8,
                        low: from,
                    });
                    if let Some(left) = &mut left {
                        *left -= &take;
                    }
                    self.position += &take;
                    if take == available {
                        self.item += 1;
                        self.offset = BigUint::ZERO;
                    } else {
                        self.offset += take;
                    }
                }
                Item::Exact { .. } | Item::Wild { .. } => {
                    out.push(item.clone());
                    if let Some(left) = &mut left {
                        *left -= 1u8;
                    }
                    self.position += 1u8;
                    self.item += 1;
                }
            }
        }
        (out, start)
    }
}

/// The values of keys that were seen, as disjoint ranges by their low key,
/// and those with x or z bits.
#[derive(Default)]
pub(crate) struct Seen {
    ranges: BTreeMap<BigUint, BigUint>,
    pub exact: BTreeSet<(BigUint, BigUint)>,
}

impl Seen {
    /// See the values of the keys `low..=high`: those that were not seen
    /// yet, in their order.
    pub(crate) fn fresh(&mut self, low: &BigUint, high: &BigUint) -> Vec<(BigUint, BigUint)> {
        // The ranges that meet `low..=high`, in order.
        let mut meeting: Vec<(BigUint, BigUint)> = Vec::new();
        if let Some((start, end)) = self.ranges.range(..low).next_back()
            && end >= low
        {
            meeting.push((start.clone(), end.clone()));
        }
        meeting.extend(
            self.ranges
                .range(low..=high)
                .map(|(start, end)| (start.clone(), end.clone())),
        );
        let mut fresh = Vec::new();
        // The first key that no range before is known to hold.
        let mut at = Some(low.clone());
        for (start, end) in &meeting {
            let Some(from) = at.take() else {
                break;
            };
            if *start > from {
                fresh.push((from, start - 1u8));
            }
            if end < high {
                at = Some(end + 1u8);
            }
        }
        if let Some(from) = at {
            fresh.push((from, high.clone()));
        }
        let start = meeting.first().map_or(low, |(start, _)| start.min(low));
        let end = meeting.last().map_or(high, |(_, end)| end.max(high));
        let (start, end) = (start.clone(), end.clone());
        for (start, _) in &meeting {
            self.ranges.remove(start);
        }
        self.ranges.insert(start, end);
        fresh
    }

    fn holds(&self, key: &BigUint) -> bool {
        self.ranges
            .range(..=key)
            .next_back()
            .is_some_and(|(_, end)| end >= key)
    }
}

/// Build the shape of an instance of `descriptor` from what its
/// construction hands over. An error is a limit that the construction
/// exceeds or a value it may not have.
pub(crate) fn build(
    descriptor: &TypeDescriptor,
    construction: &Construction<'_>,
) -> Result<Shape, String> {
    let mut shape = Shape {
        points: Vec::with_capacity(descriptor.points.len()),
        crosses: Vec::with_capacity(descriptor.crosses.len()),
        counters: 0,
        arrays: 0,
        warnings: Vec::new(),
    };
    for (point, options) in descriptor.points.iter().zip(construction.options) {
        let mut builder = PointBuilder {
            point,
            bins: Vec::new(),
            values: Values::default(),
            defaults: Vec::new(),
            default_array: false,
            transitions: TransitionBuilder::default(),
            arrays: &mut shape.arrays,
            counters: &mut shape.counters,
            warnings: &mut shape.warnings,
        };
        if point.automatic() {
            builder.automatic(options.auto_bin_max)?;
        }
        for (decl, bin) in point.bins.iter().enumerate() {
            match &bin.values {
                BinValues::Default => builder.default(decl as u32, bin)?,
                BinValues::Ranges { .. } | BinValues::Set(_) => {
                    builder.declared(decl as u32, bin, construction, options)?
                }
                BinValues::Transitions(sequences) => {
                    builder.transitions(decl as u32, bin, sequences, construction)?
                }
                BinValues::DefaultSequence => builder.default_sequence(decl as u32, bin)?,
            }
        }
        let PointBuilder {
            mut bins,
            values,
            defaults,
            default_array,
            transitions,
            ..
        } = builder;
        let transitions = transitions.finish(point, &mut bins)?;
        let removes: Vec<bool> = bins.iter().map(|bin| bin.kind != BinKind::Bins).collect();
        let Matched {
            matcher,
            holds,
            overlaps,
        } = Matcher::build(
            point.ty.width,
            point.ty.signed,
            bins.len(),
            values,
            &|bin| removes[bin as usize],
            options.detect_overlap,
        );
        for (bin, holds) in bins.iter_mut().zip(holds) {
            if bin.class == BinClass::Value {
                bin.empty = !holds;
            }
        }
        let subject = || format!("coverpoint `{}`", point.name);
        for (first, second) in overlaps.pairs {
            let name = |bin: u32| bin_name(point, &bins[bin as usize].id);
            shape.warnings.push(Warning {
                subject: subject(),
                text: format!(
                    "bins `{}` and `{}` overlap (option.detect_overlap)",
                    name(first),
                    name(second)
                ),
            });
        }
        if overlaps.more {
            shape.warnings.push(Warning {
                subject: subject(),
                text: "more bins overlap (option.detect_overlap)".into(),
            });
        }
        shape.points.push(PointShape {
            bins,
            matcher,
            defaults,
            default_array,
            transitions,
        });
    }
    let inputs = CrossInputs {
        descriptor,
        points: &shape.points,
        inputs: construction.inputs,
    };
    for (cross, &retain) in descriptor.crosses.iter().zip(construction.retain) {
        let cross = inputs.build(cross, retain, &mut shape.counters, &mut shape.warnings)?;
        shape.crosses.push(cross);
    }
    Ok(shape)
}

struct PointBuilder<'a> {
    point: &'a PointDescriptor,
    bins: Vec<BinShape>,
    values: Values,
    defaults: Vec<u32>,
    default_array: bool,
    transitions: TransitionBuilder,
    arrays: &'a mut u32,
    counters: &'a mut u32,
    warnings: &'a mut Vec<Warning>,
}

/// The number of bins of the bin array `name[count]`, which is positive
/// (19.5.1).
fn array_size(count: &Constant) -> Result<u32, String> {
    let (value, mask) = planes(count);
    if mask.bits() != 0 {
        return Err(format!("has the size {}", show(count)));
    }
    let size = numeric(&value, count.ty.width, count.ty.signed);
    if size <= BigInt::ZERO {
        return Err(format!("has the size {size}"));
    }
    u32::try_from(&size)
        .ok()
        .filter(|&size| size as usize <= MAX_BINS_PER_ITEM)
        .ok_or_else(|| format!("has more than {MAX_BINS_PER_ITEM} bins"))
}

impl PointBuilder<'_> {
    /// A bin of declaration `bin`, or an automatic bin without one.
    fn bin(
        &mut self,
        id: BinId,
        class: BinClass,
        bin: Option<&BinDescriptor>,
    ) -> Result<u32, String> {
        if self.bins.len() >= MAX_BINS_PER_ITEM {
            return Err(format!(
                "coverpoint `{}` has more than {MAX_BINS_PER_ITEM} bins",
                self.point.name
            ));
        }
        let counter = *self.counters;
        *self.counters += 1;
        let filter = match bin.map(|bin| &bin.values) {
            Some(BinValues::Ranges {
                filter: Some(BinFilter::Predicate(bit)),
                ..
            }) => Some(*bit),
            _ => None,
        };
        self.bins.push(BinShape {
            id,
            kind: bin.map_or(BinKind::Bins, |bin| bin.kind),
            class,
            empty: class == BinClass::Value,
            counter,
            iff: bin.and_then(|bin| bin.iff),
            filter,
        });
        Ok(self.bins.len() as u32 - 1)
    }

    /// The automatic bins (19.5.3): one for each enumerator of an
    /// enumeration, which is empty where the enumerator has x or z bits,
    /// else the values of the type in at most `auto_bin_max` bins of equal
    /// size, where the last takes what is left over. The bins of a signed
    /// type start at its lowest value.
    fn automatic(&mut self, auto_bin_max: i32) -> Result<(), String> {
        let point = self.point;
        let ty = &point.ty;
        if !ty.members.is_empty() {
            for (index, (_, value)) in ty.members.iter().enumerate() {
                let bin = self.bin(
                    BinId {
                        decl: 0,
                        tag: NameTag::Member(index as u32),
                    },
                    BinClass::Value,
                    None,
                )?;
                // Automatic bins hold no value with x or z bits, so the bin
                // of such an enumerator has no value and takes no part in
                // coverage (19.11.1).
                if (&value.mask & ones(ty.width)).bits() == 0 {
                    let key = (&value.value & ones(ty.width)) ^ bias(ty.width, ty.signed);
                    self.values.intervals.push(Interval {
                        low: key.clone(),
                        high: key,
                        bin,
                    });
                }
            }
            return Ok(());
        }
        if auto_bin_max <= 0 {
            self.warnings.push(Warning {
                subject: format!("coverpoint `{}`", point.name),
                text: format!(
                    "option.auto_bin_max is {auto_bin_max}: one automatic bin is created"
                ),
            });
        }
        let values = BigUint::from(1u8) << ty.width;
        let bins = values
            .clone()
            .min(BigUint::from(auto_bin_max.max(1) as u32));
        let bins = u32::try_from(&bins)
            .ok()
            .filter(|&bins| bins as usize <= MAX_BINS_PER_ITEM)
            .ok_or_else(|| {
                format!(
                    "coverpoint `{}` has more than {MAX_BINS_PER_ITEM} automatic bins",
                    point.name
                )
            })?;
        let per = &values / bins;
        for index in 0..bins {
            let low = &per * index;
            let high = if index + 1 == bins {
                &values - 1u8
            } else {
                &low + &per - 1u8
            };
            self.bin(
                BinId {
                    decl: 0,
                    tag: NameTag::Auto(KeyValue::new(&low), KeyValue::new(&high)),
                },
                BinClass::Value,
                None,
            )?;
        }
        self.values.partition = Some(Partition {
            per,
            bins,
            first: 0,
        });
        Ok(())
    }

    /// The bin of the `default` declaration `decl` (19.5).
    fn default(&mut self, decl: u32, bin: &BinDescriptor) -> Result<(), String> {
        let class = if bin.array == BinArray::Scalar {
            BinClass::Default
        } else {
            self.default_array = true;
            *self.arrays += 1;
            BinClass::DefaultArray(*self.arrays - 1)
        };
        let id = BinId {
            decl,
            tag: NameTag::Scalar,
        };
        let index = self.bin(id, class, Some(bin))?;
        self.defaults.push(index);
        Ok(())
    }

    /// The bin of the `default sequence` declaration `decl` (19.5.2).
    fn default_sequence(&mut self, decl: u32, bin: &BinDescriptor) -> Result<(), String> {
        let id = BinId {
            decl,
            tag: NameTag::Scalar,
        };
        let index = self.bin(id, BinClass::DefaultSequence, Some(bin))?;
        self.transitions.default_sequence(index, bin.kind);
        Ok(())
    }

    /// The bins of the transition declaration `decl` of `sequences`
    /// (19.5.2): one for all of them, or with `[]` one for each sequence of
    /// values they stand for. The values of a step are resolved as those of
    /// a bin (19.5.7), and no ignore or illegal value is taken from them
    /// (19.5.5, 19.5.6).
    fn transitions(
        &mut self,
        decl: u32,
        bin: &BinDescriptor,
        sequences: &[Vec<TransitionStep>],
        construction: &Construction<'_>,
    ) -> Result<(), String> {
        let point = self.point;
        let subject = format!(
            "{} `{}` of coverpoint `{}`",
            bin.kind.keyword(),
            bin.name,
            point.name
        );
        let mut compiled = Vec::with_capacity(sequences.len());
        for steps in sequences {
            let mut list = Vec::with_capacity(steps.len());
            for step in steps {
                let (items, warnings) =
                    range_items(&point.ty, bin.wildcard, &step.values, construction.inputs)?;
                self.warnings
                    .extend(warnings.into_iter().map(|text| Warning {
                        subject: subject.clone(),
                        text,
                    }));
                let step = transitions::step(items, &step.repeat, construction.inputs)
                    .map_err(|reason| format!("{subject} {reason}"))?;
                list.push(step);
            }
            // "Transition bin specifications of length 0 shall be illegal",
            // and a repetition range stands for each of its counts.
            if let [only] = &list[..]
                && only.low == 1
            {
                return Err(format!(
                    "{subject} has a transition of length 0 (IEEE 1800-2023 19.5.2)"
                ));
            }
            compiled.push(list);
        }
        match &bin.array {
            BinArray::Scalar => {
                let id = BinId {
                    decl,
                    tag: NameTag::Scalar,
                };
                let index = self.bin(id, BinClass::Transition, Some(bin))?;
                for steps in compiled {
                    self.transitions.sequence(point, index, bin.kind, steps)?;
                }
            }
            BinArray::Open => {
                let mut transitions = std::mem::take(&mut self.transitions);
                let made =
                    transitions.array(point, &subject, decl, bin.kind, compiled, &mut |id| {
                        self.bin(id, BinClass::Transition, Some(bin))
                    });
                self.transitions = transitions;
                made?;
            }
            BinArray::Fixed(_) => {
                return Err(format!("{subject} is a transition bin array with a size"));
            }
        }
        Ok(())
    }

    /// Give bin `bin` the values `items`.
    fn hold(&mut self, bin: u32, items: Vec<Item>) {
        for item in items {
            match item {
                Item::Range { low, high } => {
                    self.values.intervals.push(Interval { low, high, bin })
                }
                Item::Exact { value, mask } => self.values.exact.push(Exact { value, mask, bin }),
                Item::Wild { value, care } => self.values.wild.push(Wild { value, care, bin }),
            }
        }
    }

    /// The bins of the declaration `decl` of a range list or a set: its
    /// values that take part (19.5.7) and that its filter keeps (19.5.1.1),
    /// in one bin, in a bin for each value or distributed among a number of
    /// bins (19.5.1). What ignore and illegal bins take from them is taken
    /// once all bins have their values (19.5.5, 19.5.6).
    fn declared(
        &mut self,
        decl: u32,
        bin: &BinDescriptor,
        construction: &Construction<'_>,
        options: &PointOptions,
    ) -> Result<(), String> {
        let point = self.point;
        let subject = format!(
            "{} `{}` of coverpoint `{}`",
            bin.kind.keyword(),
            bin.name,
            point.name
        );
        let (items, warnings) = value_items(point, bin, construction.inputs, construction.sets)?;
        self.warnings
            .extend(warnings.into_iter().map(|text| Warning {
                subject: subject.clone(),
                text,
            }));
        let keeps = match &bin.values {
            BinValues::Ranges {
                filter: Some(BinFilter::Enumerate(filter)),
                ..
            } => Some(
                construction
                    .keeps
                    .get(*filter as usize)
                    .ok_or_else(|| format!("filter {filter} was not asked"))?
                    .as_slice(),
            ),
            _ => None,
        };
        if items.iter().any(|item| matches!(item, Item::Wild { .. })) {
            if bin.array != BinArray::Scalar || keeps.is_some() {
                return Err(format!(
                    "{subject} has a wildcard value of more than {MAX_WILDCARD_EXPANSION} ranges \
                     of values, which only a bin that is no array and has no `with` filter may \
                     have"
                ));
            }
            // The values of such a value are not known here.
            let removing = point.bins.iter().any(|other| {
                other.kind != BinKind::Bins
                    && matches!(other.values, BinValues::Ranges { .. } | BinValues::Set(_))
            });
            let text = match bin.kind {
                BinKind::Bins if removing || options.detect_overlap => Some(
                    "is matched as a pattern: `ignore_bins`, `illegal_bins` and \
                     option.detect_overlap do not see its values",
                ),
                BinKind::Bins => None,
                BinKind::Ignore | BinKind::Illegal => Some(
                    "is matched as a pattern: its values are in no bin when they are sampled, \
                     but still count as values of the bins they are taken from (IEEE 1800-2023 \
                     19.11.1)",
                ),
            };
            if let Some(text) = text {
                self.warnings.push(Warning {
                    subject: subject.clone(),
                    text: format!(
                        "a wildcard value of more than {MAX_WILDCARD_EXPANSION} ranges of values \
                         {text}"
                    ),
                });
            }
        }
        // A filter is applied before the values are distributed, or to
        // what each bin of an array got (19.5.1.1).
        let (items, after) = match keeps {
            Some(keeps) if bin.array == BinArray::Scalar || !construction.distribute_first => {
                (kept(&items, 0, keeps), None)
            }
            keeps => (items, keeps),
        };
        match &bin.array {
            BinArray::Scalar => {
                let id = BinId {
                    decl,
                    tag: NameTag::Scalar,
                };
                let index = self.bin(id, BinClass::Value, Some(bin))?;
                self.hold(index, items);
            }
            BinArray::Fixed(count) => {
                let count = match count {
                    Operand::Constant(constant) => constant,
                    Operand::Input(index) => construction
                        .inputs
                        .get(*index as usize)
                        .and_then(Option::as_ref)
                        .ok_or_else(|| format!("input {index} is not an integral input"))?,
                };
                let size = array_size(count).map_err(|reason| format!("{subject} {reason}"))?;
                // "B is defined as the total number of values divided by
                // the number of bins, rounded down, but not less than 1."
                let total: BigUint = items.iter().map(Item::count).sum();
                let per = (total / size).max(BigUint::from(1u8));
                let mut cursor = Cursor {
                    items: &items,
                    item: 0,
                    offset: BigUint::ZERO,
                    position: BigUint::ZERO,
                };
                for index in 0..size {
                    let id = BinId {
                        decl,
                        tag: NameTag::Index(index),
                    };
                    let bin_index = self.bin(id, BinClass::Value, Some(bin))?;
                    // "the last bin will include the remaining values".
                    let (values, position) = cursor.take((index + 1 != size).then_some(&per));
                    let values = match after {
                        Some(keeps) => {
                            kept(&values, u64::try_from(&position).unwrap_or(u64::MAX), keeps)
                        }
                        None => values,
                    };
                    self.hold(bin_index, values);
                }
            }
            BinArray::Open => {
                // A bin for each value, in the order the values first
                // appear: "each value is assigned one bin, including values
                // that are specified more than once".
                let mut seen = Seen::default();
                let mut fresh = Vec::new();
                let mut bins = BigUint::ZERO;
                for item in &items {
                    match item {
                        Item::Range { low, high } => {
                            for (low, high) in seen.fresh(low, high) {
                                let item = Item::Range { low, high };
                                bins += item.count();
                                fresh.push(item);
                            }
                        }
                        Item::Exact { value, mask } => {
                            if seen.exact.insert((value.clone(), mask.clone())) {
                                bins += 1u8;
                                fresh.push(item.clone());
                            }
                        }
                        Item::Wild { .. } => {}
                    }
                }
                if bin.wildcard && bins > BigUint::from(MAX_WILDCARD_EXPANSION) {
                    return Err(format!(
                        "{subject} has more than {MAX_WILDCARD_EXPANSION} bins"
                    ));
                }
                if bins > BigUint::from(MAX_BINS_PER_ITEM.saturating_sub(self.bins.len())) {
                    return Err(format!(
                        "coverpoint `{}` has more than {MAX_BINS_PER_ITEM} bins",
                        point.name
                    ));
                }
                // The values that a filter applied to the bins keeps.
                let surviving = after.map(|keeps| {
                    let mut seen = Seen::default();
                    for item in kept(&items, 0, keeps) {
                        match item {
                            Item::Range { low, high } => {
                                seen.fresh(&low, &high);
                            }
                            Item::Exact { value, mask } => {
                                seen.exact.insert((value, mask));
                            }
                            Item::Wild { .. } => {}
                        }
                    }
                    seen
                });
                for item in fresh {
                    match item {
                        Item::Range { low, high } => {
                            let mut key = low;
                            loop {
                                let id = BinId {
                                    decl,
                                    tag: NameTag::Value(ValueKey::Known(KeyValue::new(&key))),
                                };
                                let index = self.bin(id, BinClass::Value, Some(bin))?;
                                if surviving.as_ref().is_none_or(|kept| kept.holds(&key)) {
                                    self.values.intervals.push(Interval {
                                        low: key.clone(),
                                        high: key.clone(),
                                        bin: index,
                                    });
                                }
                                if key == high {
                                    break;
                                }
                                key += 1u8;
                            }
                        }
                        Item::Exact { value, mask } => {
                            let id = BinId {
                                decl,
                                tag: NameTag::Value(ValueKey::Unknown {
                                    value: KeyValue::new(&value),
                                    mask: KeyValue::new(&mask),
                                }),
                            };
                            let index = self.bin(id, BinClass::Value, Some(bin))?;
                            let pattern = (value, mask);
                            if surviving
                                .as_ref()
                                .is_none_or(|kept| kept.exact.contains(&pattern))
                            {
                                let (value, mask) = pattern;
                                self.values.exact.push(Exact {
                                    value,
                                    mask,
                                    bin: index,
                                });
                            }
                        }
                        Item::Wild { .. } => {}
                    }
                }
            }
        }
        Ok(())
    }
}

/// The value of `operand` from the constants and `inputs`.
pub(crate) fn operand_value<'a>(
    operand: &'a Operand,
    inputs: &'a [Option<Constant>],
) -> Result<&'a Constant, String> {
    match operand {
        Operand::Constant(constant) => Ok(constant),
        Operand::Input(index) => inputs
            .get(*index as usize)
            .and_then(Option::as_ref)
            .ok_or_else(|| format!("input {index} is not an integral input")),
    }
}

/// `range` with its operands evaluated from the constants and `inputs`.
pub(crate) fn evaluate<'a>(
    range: &'a RangeDescriptor,
    inputs: &'a [Option<Constant>],
) -> Result<RangeValue<'a>, String> {
    let operand = |operand: &'a Operand| operand_value(operand, inputs);
    let bound = |bound: &'a Bound| match bound {
        Bound::Open => Ok(None),
        Bound::Operand(value) => operand(value).map(Some),
    };
    Ok(match range {
        RangeDescriptor::Value(value) => RangeValue::Value(operand(value)?),
        RangeDescriptor::Range { low, high } => RangeValue::Range {
            low: bound(low)?,
            high: bound(high)?,
        },
    })
}
