//! What a construction makes of a covergroup type: the bins of each
//! coverpoint with the values they hold (IEEE 1800-2023 19.5).
//!
//! A shape is a function of the type's descriptor and of what a
//! construction hands over, so the instances that are built from equal
//! values share one.

use num_bigint::{BigInt, BigUint};
use serde::{Deserialize, Serialize};

use super::MAX_BINS_PER_ITEM;
use super::descriptor::{
    BinArray, BinKind, BinValues, Bound, Constant, Operand, PointDescriptor, PointType,
    RangeDescriptor, TypeDescriptor,
};
use super::value::{
    Exact, Interval, KeyValue, Matcher, Partition, bias, numeric, ones, value_of_key,
};

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

/// The bits of the two planes of `constant` in its width.
fn planes(constant: &Constant) -> (BigUint, BigUint) {
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
fn show(constant: &Constant) -> String {
    let (value, mask) = planes(constant);
    if mask.bits() == 0 {
        return numeric(&value, constant.ty.width, constant.ty.signed).to_string();
    }
    show_planes(&value, &mask, constant.ty.width)
}

/// The sized binary literal of the bits `value` with the x and z bits
/// `mask`.
fn show_planes(value: &BigUint, mask: &BigUint, width: u32) -> String {
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

/// The bins of the coverpoints of an instance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Shape {
    pub points: Vec<PointShape>,
    /// The number of hit counters an instance of the shape has.
    pub counters: u32,
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
    /// The bins of scalar declarations and the automatic bins.
    pub bins: Vec<BinShape>,
    pub matcher: Matcher,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct BinShape {
    pub id: BinId,
    pub kind: BinKind,
    /// Whether no value is associated with the bin, which then takes no
    /// part in coverage (19.11.1).
    pub empty: bool,
    /// The hit counter of the bin in an instance.
    pub counter: u32,
}

/// What names a bin of a coverpoint: the bins of two instances that are
/// equal here are one bin of the type (19.11.3).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct BinId {
    /// The bin declaration, or 0 for an automatic bin.
    pub decl: u32,
    pub tag: NameTag,
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
}

/// The name of bin `id` of coverpoint `point` (19.11.3).
pub(crate) fn bin_name(point: &PointDescriptor, id: &BinId) -> String {
    match &id.tag {
        NameTag::Scalar => point
            .bins
            .get(id.decl as usize)
            .map_or_else(String::new, |bin| bin.name.clone()),
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
    }
}

/// The options of a coverpoint that decide its bins.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub(crate) struct PointOptions {
    pub auto_bin_max: i32,
    pub detect_overlap: bool,
}

/// The key of value `value` of type `ty`.
fn key(ty: &PointType, value: &BigInt) -> BigUint {
    BigUint::try_from(value + BigInt::from(bias(ty.width, ty.signed))).unwrap_or_default()
}

/// Build the shape of an instance of `descriptor` from the integral
/// `inputs` of its construction, by input index, and the options of each
/// coverpoint. An error is a limit that the construction exceeds.
pub(crate) fn build(
    descriptor: &TypeDescriptor,
    inputs: &[Option<Constant>],
    options: &[PointOptions],
) -> Result<Shape, String> {
    let mut shape = Shape {
        points: Vec::with_capacity(descriptor.points.len()),
        counters: 0,
        warnings: Vec::new(),
    };
    for (point, options) in descriptor.points.iter().zip(options) {
        let mut builder = PointBuilder {
            point,
            bins: Vec::new(),
            intervals: Vec::new(),
            exact: Vec::new(),
            partition: None,
            counters: &mut shape.counters,
            warnings: &mut shape.warnings,
        };
        if point.automatic() {
            builder.automatic(options.auto_bin_max)?;
        }
        for (decl, bin) in point.bins.iter().enumerate() {
            match (bin.kind, &bin.values, bin.array) {
                (BinKind::Bins, BinValues::Ranges { ranges }, BinArray::Scalar) => {
                    builder.scalar(decl as u32, &bin.name, ranges, inputs)?;
                }
            }
        }
        let PointBuilder {
            bins,
            intervals,
            exact,
            partition,
            ..
        } = builder;
        let (matcher, overlaps) = Matcher::new(
            point.ty.width,
            intervals,
            exact,
            partition,
            options.detect_overlap,
        );
        for (first, second) in overlaps {
            let name = |bin: u32| bin_name(point, &bins[bin as usize].id);
            shape.warnings.push(Warning {
                subject: format!("coverpoint `{}`", point.name),
                text: format!(
                    "bins `{}` and `{}` overlap (option.detect_overlap)",
                    name(first),
                    name(second)
                ),
            });
        }
        shape.points.push(PointShape { bins, matcher });
    }
    Ok(shape)
}

struct PointBuilder<'a> {
    point: &'a PointDescriptor,
    bins: Vec<BinShape>,
    intervals: Vec<Interval>,
    exact: Vec<Exact>,
    partition: Option<Partition>,
    counters: &'a mut u32,
    warnings: &'a mut Vec<Warning>,
}

impl PointBuilder<'_> {
    fn bin(&mut self, id: BinId, empty: bool) -> u32 {
        let counter = *self.counters;
        *self.counters += 1;
        self.bins.push(BinShape {
            id,
            kind: BinKind::Bins,
            empty,
            counter,
        });
        self.bins.len() as u32 - 1
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
                // Automatic bins hold no value with x or z bits, so the bin
                // of such an enumerator has no value and takes no part in
                // coverage (19.11.1).
                let unknown = (&value.mask & ones(ty.width)).bits() != 0;
                let bin = self.bin(
                    BinId {
                        decl: 0,
                        tag: NameTag::Member(index as u32),
                    },
                    unknown,
                );
                if !unknown {
                    let key = (&value.value & ones(ty.width)) ^ bias(ty.width, ty.signed);
                    self.intervals.push(Interval {
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
                false,
            );
        }
        self.partition = Some(Partition {
            per,
            bins,
            first: 0,
        });
        Ok(())
    }

    /// The bin of the scalar declaration `decl`, of the values of `ranges`
    /// that take part (19.5.7).
    fn scalar(
        &mut self,
        decl: u32,
        name: &str,
        ranges: &[RangeDescriptor],
        inputs: &[Option<Constant>],
    ) -> Result<(), String> {
        let bin = self.bin(
            BinId {
                decl,
                tag: NameTag::Scalar,
            },
            true,
        );
        let point = self.point;
        let ty = &point.ty;
        for range in ranges {
            let resolved = resolve(ty, evaluate(range, inputs)?);
            self.warnings
                .extend(resolved.warnings.into_iter().map(|text| Warning {
                    subject: format!("bins `{name}` of coverpoint `{}`", point.name),
                    text,
                }));
            for item in resolved.items {
                self.bins[bin as usize].empty = false;
                match item {
                    ResolvedItem::Range { low, high } => self.intervals.push(Interval {
                        low: key(ty, &low),
                        high: key(ty, &high),
                        bin,
                    }),
                    ResolvedItem::Exact { value, mask } => {
                        self.exact.push(Exact { value, mask, bin })
                    }
                }
            }
        }
        Ok(())
    }
}

/// `range` with its operands evaluated from the constants and `inputs`.
fn evaluate<'a>(
    range: &'a RangeDescriptor,
    inputs: &'a [Option<Constant>],
) -> Result<RangeValue<'a>, String> {
    let operand = |operand: &'a Operand| match operand {
        Operand::Constant(constant) => Ok(constant),
        Operand::Input(index) => inputs
            .get(*index as usize)
            .and_then(Option::as_ref)
            .ok_or_else(|| format!("input {index} is not an integral input")),
    };
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
