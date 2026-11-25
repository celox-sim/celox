//! Covergroup coverage (IEEE 1800-2023 clause 19).
//!
//! The heap keeps what covergroups collect outside its objects, in three
//! layers. A [`TypeDescriptor`] is what the analyzer knows of a covergroup
//! type; compiled code registers it once under the type's key. A shape is
//! what one construction makes of it, the bins of each coverpoint with their
//! values, which instances built from equal values share. An instance has
//! the hit counters, the options and the collection state of one `new`.
//!
//! An instance is named by the handle of the object that `new` returns, and
//! outlives it: coverage is cumulative (19.11), so a collection only marks
//! the instance as no longer reachable and its hits stay in the type's
//! coverage and in the report. Nothing here holds a handle other than that
//! name; names and comments are copied bytes.
//!
//! Compiled code reaches all of it through one heap operation,
//! [`COVERAGE`], whose parameter selects one of the operations of [`op`].
//! None of them reads or writes state memory or draws a random number. A
//! sample is the calls of [`op::POINTS`], [`op::PRESENT`] and [`op::POINT`]
//! for the values of the coverpoints, which the heap only notes, and then
//! [`op::SAMPLE_END`], which finds the bins and counts: the code evaluates
//! every expression of the sample before the first of these calls, so no
//! user code runs between them.

use std::sync::Arc;

use fxhash::FxHashMap;
use num_bigint::BigUint;
use serde::{Deserialize, Serialize};

use crate::{Fault, Handle, Heap, NULL};

mod compute;
mod descriptor;
mod report;
mod shape;
#[cfg(test)]
mod tests;
mod value;

pub use descriptor::{
    BinArray, BinDescriptor, BinKind, BinValues, Bound, Constant, GroupTypeOptions,
    InputDescriptor, ItemTypeOptions, Operand, OptionId, OptionInit, OptionValue, PointDescriptor,
    PointType, RangeDescriptor, TypeDescriptor, TypeOptionId, ValueType,
};
pub use report::{
    BinReport, BinReportKind, CoverItemKind, CoverItemReport, CoverageReport,
    CovergroupInstanceReport, CovergroupReport,
};
pub use shape::{RangeValue, Resolved, ResolvedItem, resolve};

use descriptor::OptionKind;
use shape::{PointOptions, PointShape, Shape};
use value::{Matcher, bias, low_ones, ones};

/// The heap operation of every coverage operation: the low byte of the
/// parameter selects one of [`op`], and the two 24-bit fields above it are
/// its immediate operands `x` and `y` (see [`code`]).
pub const COVERAGE: u64 = 176;

/// The most bins a coverpoint of an instance has. A construction that needs
/// more is a [`Fault::Coverage`].
pub const MAX_BINS_PER_ITEM: usize = 1 << 20;

/// The coverage operations. `key` is the key of a covergroup type (see
/// [`type_key`]), `object` the handle of an instance, `item` an item of the
/// type: 0 for the covergroup, `k + 1` for coverpoint `k`. An integral value
/// is passed in 64-bit words of its value and mask planes, zero extended.
///
/// An operation on a key that is not registered, or on an item the type does
/// not have, is a [`Fault::Coverage`], and one on a handle that names no
/// instance a [`Fault::NullDereference`], except
/// [`SAMPLING`](crate::coverage::op::SAMPLING). The calls that only note a
/// value of a sample look nothing up: what is wrong with them is the fault of
/// the [`SAMPLE_END`](crate::coverage::op::SAMPLE_END) that counts it.
pub mod op {
    /// `registered(key)`: whether the type is registered.
    pub const REGISTERED: u64 = 0;
    /// `register(key)`: register the type whose descriptor the bytes that
    /// [`REGISTER_BYTES`] collected for the key are as JSON. A type that is
    /// registered stays as it is.
    pub const REGISTER: u64 = 1;
    /// `new(key, size, class)`: the handle of a new object of `class` with
    /// `size` zeroed bytes whose random state is not seeded, as a new
    /// instance of the type.
    pub const NEW: u64 = 2;
    /// `input(object, value, mask)`: word `y` of integral input `x` of the
    /// construction.
    pub const INPUT: u64 = 3;
    /// `input_string(object, string)`: string input `x` of the construction,
    /// whose bytes are copied.
    pub const INPUT_STRING: u64 = 4;
    /// `build(object)`: make the bins of the instance from its inputs: the
    /// number of warnings, whose text [`MESSAGE`] gives.
    pub const BUILD: u64 = 8;
    /// `sampling(object)`: whether the instance is built and collecting.
    /// `null` and a handle of no instance are not, without a fault.
    pub const SAMPLING: u64 = 9;
    /// `point(object, value, mask)`: word `y` of the value of item `x` in
    /// the sample in progress.
    pub const POINT: u64 = 10;
    /// `sample_end(object)`: count the sample in progress in the bins of
    /// the instance: the number of illegal bins hit, whose text
    /// [`MESSAGE`] gives. A value of an item that is not a coverpoint of the
    /// type, a word its value does not have and the values of a [`POINTS`]
    /// call that do not fit a word are a fault, unless the instance is not
    /// collecting.
    pub const SAMPLE_END: u64 = 12;
    /// `inst_coverage(object, item)`: what `get_inst_coverage` of the item
    /// gives (19.8): with `x` 0 the coverage as the bits of an `f64`, with 1
    /// the number of covered bins and with 2 the number of bins, neither
    /// above the largest `int`.
    pub const INST_COVERAGE: u64 = 13;
    /// `type_coverage(key, item)`: what `get_coverage` of the item gives, as
    /// [`INST_COVERAGE`] does.
    pub const TYPE_COVERAGE: u64 = 14;
    /// `overall()`: what `$get_coverage` gives (19.9), as the bits of an
    /// `f64`.
    pub const OVERALL: u64 = 15;
    /// `collect(object)`: start (`y` 1) or stop (`y` 0) collecting for item
    /// `x` (19.8).
    pub const COLLECT: u64 = 16;
    /// `set_option(object, value)`: assign member `y` (see
    /// [`OptionId::code`](super::OptionId::code)) of `option` of item `x`:
    /// an `int`, a bit, or the handle of a string, whose bytes are copied.
    pub const SET_OPTION: u64 = 17;
    /// `get_option(object)`: member `y` of `option` of item `x`: an `int`
    /// sign extended, a bit, or the handle of a string.
    pub const GET_OPTION: u64 = 18;
    /// `set_type_option(key, value)`: assign member `y` (see
    /// [`TypeOptionId::code`](super::TypeOptionId::code)) of `type_option`
    /// of item `x`, as [`SET_OPTION`] takes a value.
    pub const SET_TYPE_OPTION: u64 = 19;
    /// `get_type_option(key)`: member `y` of `type_option` of item `x`, as
    /// [`GET_OPTION`] gives a value.
    pub const GET_TYPE_OPTION: u64 = 20;
    /// `register_bytes(key, word)`: append the low `x` bytes of `word`,
    /// most significant first, to the descriptor being collected for the
    /// key. `x` is at most 8.
    pub const REGISTER_BYTES: u64 = 21;
    /// `message(object)`: the handle of the string of the warnings of the
    /// last [`BUILD`] or the illegal bins of the last [`SAMPLE_END`],
    /// joined with `"; "`, which it then forgets.
    pub const MESSAGE: u64 = 22;
    /// `formal_set(object, value, mask)`: word `y` of the value of formal
    /// `x` that the instance keeps for its samples.
    pub const FORMAL_SET: u64 = 23;
    /// `formal(object)`: a word of the value of formal `x`: `y` is twice
    /// the word index, plus 1 for the mask plane.
    pub const FORMAL: u64 = 24;
    /// `points(object, values, masks)`: the values of the `y` items from
    /// item `x` on in the sample in progress, each in the width of its
    /// coverpoint, the first item in the low bits.
    pub const POINTS: u64 = 25;
    /// `present(object, bits)`: which items of the next [`POINTS`] call
    /// are sampled, one bit each from bit 0 on. All of them are without
    /// this call.
    pub const PRESENT: u64 = 26;
}

/// The opcode word of coverage operation `sub` with the immediate operands
/// `x` and `y`, of 24 bits each.
pub const fn code(sub: u64, x: u64, y: u64) -> u64 {
    crate::ops::encode(COVERAGE, sub | (x & FIELD) << 8 | (y & FIELD) << 32)
}

const FIELD: u64 = 0xff_ffff;

/// The key of the covergroup type of reserved name `name`: its 64-bit
/// FNV-1a hash.
pub const fn type_key(name: &str) -> u64 {
    let bytes = name.as_bytes();
    let mut hash = 0xcbf2_9ce4_8422_2325;
    let mut index = 0;
    while index < bytes.len() {
        hash = (hash ^ bytes[index] as u64).wrapping_mul(0x0100_0000_01b3);
        index += 1;
    }
    hash
}

/// The covergroup types and instances of a heap.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Registry {
    /// In registration order.
    types: Vec<TypeEntry>,
    by_key: FxHashMap<u64, u32>,
    /// In creation order.
    instances: Vec<Instance>,
    by_handle: FxHashMap<Handle, u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct TypeEntry {
    key: u64,
    descriptor: Arc<TypeDescriptor>,
    /// The type options, which code may assign at any time (19.7.1).
    options: TypeOptions,
    shapes: Vec<Arc<Shape>>,
    /// The shape of each construction that was built, by what decided it.
    shape_index: FxHashMap<ShapeInputs, u32>,
    /// The instances of the type, in creation order.
    instances: Vec<u32>,
    /// Where the words of each integral input start in the words of a
    /// construction, and where the last ends.
    input_words: Vec<u32>,
    /// The same for the formals an instance keeps.
    formal_words: Vec<u32>,
}

/// Everything that building a shape reads.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
struct ShapeInputs {
    /// The words of the integral inputs.
    words: Vec<u64>,
    points: Vec<PointOptions>,
    distribute_first: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct TypeOptions {
    weight: i32,
    goal: i32,
    comment: Vec<u8>,
    merge_instances: bool,
    distribute_first: bool,
    points: Vec<ItemTypeValues>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct ItemTypeValues {
    weight: i32,
    goal: i32,
    comment: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Instance {
    handle: Handle,
    ty: u32,
    /// The number of the instance among those of its type.
    ordinal: u32,
    /// The shape of the type the instance has once it is built.
    shape: Option<u32>,
    /// The inputs of the construction, until it is built.
    building: Option<Box<Building>>,
    /// Whether the object of the handle was not collected yet.
    live: bool,
    /// Whether the covergroup and each coverpoint collect (19.8): a
    /// coverpoint is sampled when both do.
    collecting: bool,
    item_collecting: Vec<bool>,
    options: InstanceOptions,
    /// The words of the planes of the formals that sampling reads.
    formals: Vec<u64>,
    /// The hits of each bin, by the counter of the bin in the shape.
    counts: Vec<u64>,
    /// The samples taken while the covergroup was collecting.
    samples: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Building {
    /// A value word and a mask word for each word of each integral input.
    words: Vec<u64>,
    /// The string inputs by input index.
    strings: Vec<Vec<u8>>,
}

/// The instance options (19.7, Table 19-1).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct InstanceOptions {
    name: Vec<u8>,
    weight: i32,
    goal: i32,
    comment: Vec<u8>,
    at_least: i32,
    auto_bin_max: i32,
    detect_overlap: bool,
    per_instance: bool,
    get_inst_coverage: bool,
    points: Vec<ItemOptions>,
}

/// The options of a coverpoint of an instance. Those the covergroup's are
/// the default of (Table 19-2) have a value once they are assigned; weight,
/// goal and comment are the coverpoint's own.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct ItemOptions {
    weight: i32,
    goal: i32,
    comment: Vec<u8>,
    at_least: Option<i32>,
    auto_bin_max: Option<i32>,
    detect_overlap: Option<bool>,
}

/// The value of an option member.
#[derive(Clone, Debug, PartialEq, Eq)]
enum OptionArg {
    Int(i32),
    Bit(bool),
    Text(Vec<u8>),
}

/// `weight` as the value of a weight, which "shall be a non-negative
/// integral value" (Table 19-1, Table 19-3).
fn weight_value(weight: i32) -> Result<i32, String> {
    if weight < 0 {
        Err(format!("the negative weight {weight}"))
    } else {
        Ok(weight)
    }
}

impl InstanceOptions {
    fn new(name: Vec<u8>, points: usize) -> Self {
        Self {
            name,
            weight: 1,
            goal: 100,
            comment: Vec::new(),
            at_least: 1,
            auto_bin_max: 64,
            detect_overlap: false,
            per_instance: false,
            get_inst_coverage: false,
            points: vec![
                ItemOptions {
                    weight: 1,
                    goal: 100,
                    comment: Vec::new(),
                    at_least: None,
                    auto_bin_max: None,
                    detect_overlap: None,
                };
                points
            ],
        }
    }

    /// The least hits a bin of coverpoint `point` is covered with.
    fn at_least(&self, point: usize) -> i32 {
        self.points[point].at_least.unwrap_or(self.at_least)
    }

    fn point_options(&self, point: usize) -> PointOptions {
        let options = &self.points[point];
        PointOptions {
            auto_bin_max: options.auto_bin_max.unwrap_or(self.auto_bin_max),
            detect_overlap: options.detect_overlap.unwrap_or(self.detect_overlap),
        }
    }

    fn set(&mut self, item: u32, option: OptionId, value: OptionArg) -> Result<(), String> {
        let wrong = || format!("item {item} has no option `{}`", option.name());
        if item == 0 {
            match (option, value) {
                (OptionId::Name, OptionArg::Text(value)) => self.name = value,
                (OptionId::Weight, OptionArg::Int(value)) => self.weight = weight_value(value)?,
                (OptionId::Goal, OptionArg::Int(value)) => self.goal = value,
                (OptionId::Comment, OptionArg::Text(value)) => self.comment = value,
                (OptionId::AtLeast, OptionArg::Int(value)) => self.at_least = value,
                (OptionId::AutoBinMax, OptionArg::Int(value)) => self.auto_bin_max = value,
                (OptionId::DetectOverlap, OptionArg::Bit(value)) => self.detect_overlap = value,
                (OptionId::PerInstance, OptionArg::Bit(value)) => self.per_instance = value,
                (OptionId::GetInstCoverage, OptionArg::Bit(value)) => {
                    self.get_inst_coverage = value
                }
                _ => return Err(wrong()),
            }
            return Ok(());
        }
        let point = self.points.get_mut(item as usize - 1).ok_or_else(wrong)?;
        match (option, value) {
            (OptionId::Weight, OptionArg::Int(value)) => point.weight = weight_value(value)?,
            (OptionId::Goal, OptionArg::Int(value)) => point.goal = value,
            (OptionId::Comment, OptionArg::Text(value)) => point.comment = value,
            (OptionId::AtLeast, OptionArg::Int(value)) => point.at_least = Some(value),
            (OptionId::AutoBinMax, OptionArg::Int(value)) => point.auto_bin_max = Some(value),
            (OptionId::DetectOverlap, OptionArg::Bit(value)) => point.detect_overlap = Some(value),
            _ => return Err(wrong()),
        }
        Ok(())
    }

    fn get(&self, item: u32, option: OptionId) -> Result<OptionArg, String> {
        let wrong = || format!("item {item} has no option `{}`", option.name());
        if item == 0 {
            return Ok(match option {
                OptionId::Name => OptionArg::Text(self.name.clone()),
                OptionId::Weight => OptionArg::Int(self.weight),
                OptionId::Goal => OptionArg::Int(self.goal),
                OptionId::Comment => OptionArg::Text(self.comment.clone()),
                OptionId::AtLeast => OptionArg::Int(self.at_least),
                OptionId::AutoBinMax => OptionArg::Int(self.auto_bin_max),
                OptionId::DetectOverlap => OptionArg::Bit(self.detect_overlap),
                OptionId::PerInstance => OptionArg::Bit(self.per_instance),
                OptionId::GetInstCoverage => OptionArg::Bit(self.get_inst_coverage),
            });
        }
        let index = item as usize - 1;
        let point = self.points.get(index).ok_or_else(wrong)?;
        Ok(match option {
            OptionId::Weight => OptionArg::Int(point.weight),
            OptionId::Goal => OptionArg::Int(point.goal),
            OptionId::Comment => OptionArg::Text(point.comment.clone()),
            OptionId::AtLeast => OptionArg::Int(self.at_least(index)),
            OptionId::AutoBinMax => OptionArg::Int(self.point_options(index).auto_bin_max),
            OptionId::DetectOverlap => OptionArg::Bit(self.point_options(index).detect_overlap),
            OptionId::Name | OptionId::PerInstance | OptionId::GetInstCoverage => {
                return Err(wrong());
            }
        })
    }
}

impl TypeOptions {
    fn new(descriptor: &TypeDescriptor) -> Self {
        let options = &descriptor.type_options;
        Self {
            weight: options.weight,
            goal: options.goal,
            comment: options.comment.clone().into_bytes(),
            merge_instances: options.merge_instances,
            distribute_first: options.distribute_first,
            points: descriptor
                .points
                .iter()
                .map(|point| ItemTypeValues {
                    weight: point.type_options.weight,
                    goal: point.type_options.goal,
                    comment: point.type_options.comment.clone().into_bytes(),
                })
                .collect(),
        }
    }

    fn set(&mut self, item: u32, option: TypeOptionId, value: OptionArg) -> Result<(), String> {
        let wrong = || format!("item {item} has no type option `{}`", option.name());
        if item == 0 {
            match (option, value) {
                (TypeOptionId::Weight, OptionArg::Int(value)) => self.weight = weight_value(value)?,
                (TypeOptionId::Goal, OptionArg::Int(value)) => self.goal = value,
                (TypeOptionId::Comment, OptionArg::Text(value)) => self.comment = value,
                (TypeOptionId::MergeInstances, OptionArg::Bit(value)) => {
                    self.merge_instances = value
                }
                (TypeOptionId::DistributeFirst, OptionArg::Bit(value)) => {
                    self.distribute_first = value
                }
                _ => return Err(wrong()),
            }
            return Ok(());
        }
        let point = self.points.get_mut(item as usize - 1).ok_or_else(wrong)?;
        match (option, value) {
            (TypeOptionId::Weight, OptionArg::Int(value)) => point.weight = weight_value(value)?,
            (TypeOptionId::Goal, OptionArg::Int(value)) => point.goal = value,
            (TypeOptionId::Comment, OptionArg::Text(value)) => point.comment = value,
            _ => return Err(wrong()),
        }
        Ok(())
    }

    fn get(&self, item: u32, option: TypeOptionId) -> Result<OptionArg, String> {
        let wrong = || format!("item {item} has no type option `{}`", option.name());
        if item == 0 {
            return Ok(match option {
                TypeOptionId::Weight => OptionArg::Int(self.weight),
                TypeOptionId::Goal => OptionArg::Int(self.goal),
                TypeOptionId::Comment => OptionArg::Text(self.comment.clone()),
                TypeOptionId::MergeInstances => OptionArg::Bit(self.merge_instances),
                TypeOptionId::DistributeFirst => OptionArg::Bit(self.distribute_first),
            });
        }
        let point = self.points.get(item as usize - 1).ok_or_else(wrong)?;
        Ok(match option {
            TypeOptionId::Weight => OptionArg::Int(point.weight),
            TypeOptionId::Goal => OptionArg::Int(point.goal),
            TypeOptionId::Comment => OptionArg::Text(point.comment.clone()),
            TypeOptionId::MergeInstances | TypeOptionId::DistributeFirst => return Err(wrong()),
        })
    }
}

/// Where the words of each of `widths` start when every value takes a value
/// word and a mask word for each 64 bits, and where the last ends. `None`
/// is a value that has no words.
fn word_starts(widths: impl Iterator<Item = Option<u32>>) -> Vec<u32> {
    let mut starts = vec![0];
    let mut next = 0;
    for width in widths {
        next += width.map_or(0, |width| width.div_ceil(64) * 2);
        starts.push(next);
    }
    starts
}

/// The low 32 bits of `constant` as an `int` variable holds it once it is
/// assigned: extended by its sign where it is narrower, and with 0 for an x
/// or z bit (IEEE 1800-2023 6.11.2).
fn int_bits(constant: &Constant) -> u32 {
    let width = constant.ty.width;
    let known = &constant.value & (ones(width) ^ (&constant.mask & ones(width)));
    let mut word = known.iter_u32_digits().next().unwrap_or(0);
    if constant.ty.signed && (1..32).contains(&width) && word >> (width - 1) & 1 != 0 {
        word |= u32::MAX << width;
    }
    word
}

/// The value of 64-bit words `words`, least significant first.
fn big(words: impl Iterator<Item = u64>) -> BigUint {
    BigUint::new(
        words
            .flat_map(|word| [word as u32, (word >> 32) as u32])
            .collect(),
    )
}

/// What the commit calls of the sample in progress noted, the descriptor
/// bytes being collected and the text a build or sample left for
/// [`op::MESSAGE`]: nothing of it outlives the function that makes the
/// calls, so it is no part of a heap's contents.
#[derive(Clone, Debug, Default)]
pub(crate) struct Scratch {
    sampled: Vec<Sampled>,
    /// The bits of [`op::PRESENT`] for the next [`op::POINTS`].
    present: Option<u64>,
    descriptors: FxHashMap<u64, Vec<u8>>,
    messages: Vec<String>,
}

#[derive(Clone, Copy, Debug)]
enum Sampled {
    /// A word of the value of an item.
    Word {
        item: u32,
        word: u32,
        value: u64,
        mask: u64,
    },
    /// The values of `count` items from `first` on.
    Pack {
        first: u32,
        count: u32,
        values: u64,
        masks: u64,
        present: u64,
    },
}

impl Registry {
    /// Whether any instance was created.
    pub(crate) fn has_instances(&self) -> bool {
        !self.instances.is_empty()
    }

    /// Whether `handle` names an instance of an embedded covergroup (19.4).
    pub(crate) fn embedded(&self, handle: Handle) -> bool {
        self.by_handle.get(&handle).is_some_and(|&index| {
            let instance = &self.instances[index as usize];
            self.types[instance.ty as usize].descriptor.embedded
        })
    }

    /// Note which instances no handle reaches any more: those `reachable`
    /// does not hold of. They keep their hits.
    pub(crate) fn release(&mut self, reachable: impl Fn(Handle) -> bool) {
        for instance in &mut self.instances {
            if instance.live && !reachable(instance.handle) {
                instance.live = false;
            }
        }
    }

    /// Bytes of hit counters, names and comments.
    pub(crate) fn content_size(&self) -> usize {
        let types: usize = self
            .types
            .iter()
            .map(|entry| {
                entry.options.comment.len()
                    + entry
                        .options
                        .points
                        .iter()
                        .map(|point| point.comment.len())
                        .sum::<usize>()
            })
            .sum();
        let instances: usize = self
            .instances
            .iter()
            .map(|instance| {
                let options = &instance.options;
                instance.counts.len() * 8
                    + options.name.len()
                    + options.comment.len()
                    + options
                        .points
                        .iter()
                        .map(|point| point.comment.len())
                        .sum::<usize>()
            })
            .sum();
        types + instances
    }

    fn register(&mut self, key: u64, descriptor: Arc<TypeDescriptor>) {
        let index = self.types.len() as u32;
        self.types.push(TypeEntry {
            key,
            options: TypeOptions::new(&descriptor),
            shapes: Vec::new(),
            shape_index: FxHashMap::default(),
            instances: Vec::new(),
            input_words: word_starts(descriptor.inputs.iter().map(|input| match input {
                InputDescriptor::Integral(ty) => Some(ty.width),
                InputDescriptor::String => None,
            })),
            formal_words: word_starts(descriptor.formals.iter().map(|ty| Some(ty.width))),
            descriptor,
        });
        self.by_key.insert(key, index);
    }

    fn add_instance(&mut self, ty: usize, handle: Handle) {
        let entry = &mut self.types[ty];
        let descriptor = &entry.descriptor;
        let ordinal = entry.instances.len() as u32;
        let index = self.instances.len() as u32;
        entry.instances.push(index);
        self.instances.push(Instance {
            handle,
            ty: ty as u32,
            ordinal,
            shape: None,
            building: Some(Box::new(Building {
                words: vec![0; entry.input_words.last().copied().unwrap_or(0) as usize],
                strings: vec![Vec::new(); descriptor.inputs.len()],
            })),
            live: true,
            collecting: true,
            item_collecting: vec![true; descriptor.points.len()],
            // The name of an instance is the tool's choice until it is
            // assigned (Table 19-1).
            options: InstanceOptions::new(
                format!("{}#{ordinal}", descriptor.name).into_bytes(),
                descriptor.points.len(),
            ),
            formals: vec![0; entry.formal_words.last().copied().unwrap_or(0) as usize],
            counts: Vec::new(),
            samples: 0,
        });
        self.by_handle.insert(handle, index);
    }

    /// Where word `word` of plane pair `slot` is in words laid out by
    /// `starts`.
    fn word_index(starts: &[u32], slot: u32, word: u32) -> Option<usize> {
        let start = *starts.get(slot as usize)?;
        let end = *starts.get(slot as usize + 1)?;
        let index = start.checked_add(word.checked_mul(2)?)?;
        (index < end).then_some(index as usize)
    }

    fn input(
        &mut self,
        instance: usize,
        input: u32,
        word: u32,
        value: u64,
        mask: u64,
    ) -> Result<(), String> {
        let instance = &mut self.instances[instance];
        let entry = &self.types[instance.ty as usize];
        let building = instance
            .building
            .as_mut()
            .ok_or("an input of an instance that is built already")?;
        let index = Self::word_index(&entry.input_words, input, word)
            .ok_or_else(|| format!("input {input} has no word {word}"))?;
        building.words[index] = value;
        building.words[index + 1] = mask;
        Ok(())
    }

    fn input_string(&mut self, instance: usize, input: u32, bytes: Vec<u8>) -> Result<(), String> {
        let instance = &mut self.instances[instance];
        let entry = &self.types[instance.ty as usize];
        let building = instance
            .building
            .as_mut()
            .ok_or("an input of an instance that is built already")?;
        match entry.descriptor.inputs.get(input as usize) {
            Some(InputDescriptor::String) => {
                building.strings[input as usize] = bytes;
                Ok(())
            }
            _ => Err(format!("input {input} is not a string input")),
        }
    }

    /// Build instance `instance` from its inputs: apply the option
    /// assignments of the definition in their order, then take the shape of
    /// an earlier construction that read the same values or make one. The
    /// result is the warnings of the shape for this instance.
    fn build(&mut self, instance: usize) -> Result<Vec<String>, String> {
        let Self {
            types, instances, ..
        } = self;
        let instance = &mut instances[instance];
        let entry = &mut types[instance.ty as usize];
        let descriptor = Arc::clone(&entry.descriptor);
        let mut building = instance
            .building
            .take()
            .ok_or("an instance that is built twice")?;
        let integral = |index: usize| match &descriptor.inputs[index] {
            InputDescriptor::Integral(ty) => Some(*ty),
            InputDescriptor::String => None,
        };
        // Bits above the width of an input are not part of its value.
        for index in 0..descriptor.inputs.len() {
            let (start, end) = (
                entry.input_words[index] as usize,
                entry.input_words[index + 1] as usize,
            );
            if let Some(ty) = integral(index)
                && end > start
            {
                let top = low_ones(ty.width - (ty.width - 1) / 64 * 64);
                building.words[end - 2] &= top;
                building.words[end - 1] &= top;
            }
        }
        let input_constant = |building: &Building, index: usize| {
            let ty = integral(index)?;
            let words = &building.words
                [entry.input_words[index] as usize..entry.input_words[index + 1] as usize];
            Some(Constant {
                ty,
                value: big(words.iter().copied().step_by(2)),
                mask: big(words.iter().copied().skip(1).step_by(2)),
            })
        };
        for init in &descriptor.options {
            let value = match &init.value {
                OptionValue::String(text) => OptionArg::Text(text.clone().into_bytes()),
                OptionValue::StringInput(index) => OptionArg::Text(
                    building
                        .strings
                        .get(*index as usize)
                        .cloned()
                        .unwrap_or_default(),
                ),
                OptionValue::Operand(operand) => {
                    let input;
                    let constant = match operand {
                        Operand::Constant(constant) => constant,
                        Operand::Input(index) => {
                            input = input_constant(&building, *index as usize)
                                .ok_or_else(|| format!("input {index} is not integral"))?;
                            &input
                        }
                    };
                    let word = int_bits(constant);
                    match init.option.kind() {
                        OptionKind::Bit => OptionArg::Bit(word & 1 != 0),
                        OptionKind::Int | OptionKind::Text => OptionArg::Int(word as i32),
                    }
                }
            };
            instance.options.set(init.item, init.option, value)?;
        }
        let key = ShapeInputs {
            words: building.words.clone(),
            points: (0..descriptor.points.len())
                .map(|point| instance.options.point_options(point))
                .collect(),
            distribute_first: entry.options.distribute_first,
        };
        let shape = match entry.shape_index.get(&key) {
            Some(&shape) => shape,
            None => {
                let inputs: Vec<Option<Constant>> = (0..descriptor.inputs.len())
                    .map(|index| input_constant(&building, index))
                    .collect();
                let shape = entry.shapes.len() as u32;
                entry
                    .shapes
                    .push(Arc::new(shape::build(&descriptor, &inputs, &key.points)?));
                entry.shape_index.insert(key, shape);
                shape
            }
        };
        instance.shape = Some(shape);
        let shape = &entry.shapes[shape as usize];
        instance.counts = vec![0; shape.counters as usize];
        let name = String::from_utf8_lossy(&instance.options.name);
        Ok(shape
            .warnings
            .iter()
            .map(|warning| {
                format!(
                    "{} of covergroup `{}` (instance `{name}`): {}",
                    warning.subject, descriptor.name, warning.text
                )
            })
            .collect())
    }

    /// Count the sample that `sampled` holds in the bins of instance
    /// `instance`, unless it is not built or not collecting. A value of an
    /// item that is not a coverpoint of the type, a word its value does not
    /// have and a pack that does not fit a word are errors, which only wrong
    /// code causes: counting stops there and the sample is not one of the
    /// instance.
    fn sample(&mut self, instance: usize, sampled: &[Sampled]) -> Result<(), String> {
        let Self {
            types, instances, ..
        } = self;
        let instance = &mut instances[instance];
        let entry = &types[instance.ty as usize];
        let Some(shape) = instance.shape.filter(|_| instance.collecting) else {
            return Ok(());
        };
        let shape = &entry.shapes[shape as usize];
        let points = &entry.descriptor.points;
        // The coverpoint that item `item` is.
        let coverpoint = |item: u32| {
            (item as usize)
                .checked_sub(1)
                .filter(|&index| index < points.len())
                .ok_or_else(|| format!("item {item} is not a coverpoint"))
        };
        let mut at = 0;
        while let Some(&first) = sampled.get(at) {
            match first {
                Sampled::Pack {
                    first,
                    count,
                    values,
                    masks,
                    present,
                } => {
                    // A coverpoint has a bit at least, so an item of a pack
                    // that fits a word has a bit of `present`.
                    let mut offset = 0;
                    for index in 0..count {
                        let point = coverpoint(first + index)?;
                        let ty = &points[point].ty;
                        if ty.width > 64 - offset {
                            return Err(format!(
                                "the values of the {count} items from item {first} on do not fit \
                                 a word"
                            ));
                        }
                        if present >> index & 1 != 0 && instance.item_collecting[point] {
                            count_value(
                                ty,
                                &shape.points[point],
                                &[values >> offset],
                                &[masks >> offset],
                                &mut instance.counts,
                            );
                        }
                        offset += ty.width;
                    }
                    at += 1;
                }
                Sampled::Word { item, .. } => {
                    let words = sampled[at..].iter().map_while(|sampled| match *sampled {
                        Sampled::Word {
                            item: other,
                            word,
                            value,
                            mask,
                        } if other == item => Some((word as usize, value, mask)),
                        _ => None,
                    });
                    let count = words.clone().count();
                    let point = coverpoint(item)?;
                    let ty = &points[point].ty;
                    let size = ty.width.div_ceil(64) as usize;
                    if let Some((word, ..)) = words.clone().find(|&(word, ..)| word >= size) {
                        return Err(format!("the value of item {item} has no word {word}"));
                    }
                    if instance.item_collecting[point] {
                        let bins = &shape.points[point];
                        if size <= 1 {
                            let (mut value, mut mask) = (0, 0);
                            for (_, value_word, mask_word) in words {
                                (value, mask) = (value_word, mask_word);
                            }
                            count_value(ty, bins, &[value], &[mask], &mut instance.counts);
                        } else {
                            let (mut value, mut mask) = (vec![0; size], vec![0; size]);
                            for (word, value_word, mask_word) in words {
                                value[word] = value_word;
                                mask[word] = mask_word;
                            }
                            count_value(ty, bins, &value, &mask, &mut instance.counts);
                        }
                    }
                    at += count;
                }
            }
        }
        instance.samples += 1;
        Ok(())
    }
}

/// Count the value of the words `value` with the x and z bits `mask`, least
/// significant word first, in the bins of a coverpoint of type `ty`. A
/// value with an x or z bit is only in a bin that holds exactly it
/// (19.5.4), so in no automatic bin (19.5.3).
fn count_value(
    ty: &PointType,
    point: &PointShape,
    value: &[u64],
    mask: &[u64],
    counts: &mut [u64],
) {
    let hit = |bin: u32| counts[point.bins[bin as usize].counter as usize] += 1;
    match &point.matcher {
        Matcher::Narrow(matcher) => {
            let ones = low_ones(ty.width);
            let value = value.first().copied().unwrap_or(0) & ones;
            let mask = mask.first().copied().unwrap_or(0) & ones;
            if mask != 0 {
                matcher.exact_hits(&value, &mask, hit);
            } else {
                let sign = if ty.signed { 1 << (ty.width - 1) } else { 0 };
                matcher.hits(&(value ^ sign), hit);
            }
        }
        Matcher::Wide(matcher) => {
            let ones = ones(ty.width);
            let value = big(value.iter().copied()) & &ones;
            let mask = big(mask.iter().copied()) & &ones;
            if mask.bits() != 0 {
                matcher.exact_hits(&value, &mask, hit);
            } else {
                matcher.hits(&(value ^ bias(ty.width, ty.signed)), hit);
            }
        }
    }
}

/// The number that the operations give for covered and total bins: an
/// `int` (19.8).
fn saturate(count: u64) -> u64 {
    count.min(i32::MAX as u64)
}

impl Heap {
    /// The instance `object` names, or a null dereference.
    fn cover_instance(&mut self, object: Handle) -> Option<usize> {
        let index = self.coverage.by_handle.get(&object).copied();
        if index.is_none() {
            self.fault.get_or_insert(Fault::NullDereference);
        }
        index.map(|index| index as usize)
    }

    /// The registered type of `key`, or a fault.
    fn cover_type(&mut self, key: u64) -> Option<usize> {
        let index = self.coverage.by_key.get(&key).copied();
        if index.is_none() {
            self.cover_fault(format!("no type is registered as {key:#x}"));
        }
        index.map(|index| index as usize)
    }

    fn cover_fault(&mut self, reason: impl Into<String>) {
        self.fault
            .get_or_insert_with(|| Fault::Coverage(reason.into()));
    }

    /// Record the error of an operation on instance `instance`, if any.
    fn cover_check(&mut self, instance: usize, result: Result<(), String>) {
        if let Err(reason) = result {
            let entry = &self.coverage.types[self.coverage.instances[instance].ty as usize];
            let reason = format!("type `{}`: {reason}", entry.descriptor.name);
            self.cover_fault(reason);
        }
    }

    /// Register the covergroup type `descriptor` under `key` (see
    /// [`type_key`]), unless a type is registered under it. A descriptor
    /// that names an input or an option it does not have is a
    /// [`Fault::Coverage`].
    pub fn register_cover_type(&mut self, key: u64, descriptor: Arc<TypeDescriptor>) {
        if self.coverage.by_key.contains_key(&key) {
            return;
        }
        match descriptor.validate() {
            Ok(()) => self.coverage.register(key, descriptor),
            Err(reason) => self.cover_fault(format!(
                "descriptor of type `{}`: {reason}",
                descriptor.name
            )),
        }
    }

    /// A new instance of the type of `key`: the handle of a new object of
    /// `class` with `size` zeroed bytes. Its random state is not seeded,
    /// so constructing a covergroup draws no random number (IEEE 1800-2023
    /// 18.14.1). The instance takes its inputs next and then is built.
    pub fn new_cover_instance(&mut self, key: u64, size: usize, class: u32) -> Handle {
        let Some(ty) = self.cover_type(key) else {
            return NULL;
        };
        let handle = self.new_object(class, size);
        self.coverage.add_instance(ty, handle);
        handle
    }

    /// Give the construction of instance `object` word `word` of the planes
    /// of its integral input `input`.
    pub fn cover_input(&mut self, object: Handle, input: u32, word: u32, value: u64, mask: u64) {
        if let Some(instance) = self.cover_instance(object) {
            let result = self.coverage.input(instance, input, word, value, mask);
            self.cover_check(instance, result);
        }
    }

    /// Build instance `object` from its inputs (see [`op::BUILD`]): the
    /// number of warnings.
    pub fn cover_build(&mut self, object: Handle) -> u64 {
        self.coverage_scratch.messages.clear();
        let Some(instance) = self.cover_instance(object) else {
            return 0;
        };
        match self.coverage.build(instance) {
            Ok(messages) => {
                self.coverage_scratch.messages = messages;
                self.coverage_scratch.messages.len() as u64
            }
            Err(reason) => {
                self.cover_check(instance, Err(reason));
                0
            }
        }
    }

    /// Note word `word` of the planes of the value of item `item` in the
    /// sample in progress.
    pub fn cover_point(&mut self, item: u32, word: u32, value: u64, mask: u64) {
        self.coverage_scratch.sampled.push(Sampled::Word {
            item,
            word,
            value,
            mask,
        });
    }

    /// Count the sample in progress in the bins of instance `object` (see
    /// [`op::SAMPLE_END`]): the number of illegal bins hit. A sample with a
    /// value of an item that is not a coverpoint of the type is a
    /// [`Fault::Coverage`].
    pub fn cover_sample_end(&mut self, object: Handle) -> u64 {
        let mut sampled = std::mem::take(&mut self.coverage_scratch.sampled);
        self.coverage_scratch.present = None;
        self.coverage_scratch.messages.clear();
        if let Some(instance) = self.cover_instance(object) {
            let result = self.coverage.sample(instance, &sampled);
            self.cover_check(instance, result);
        }
        // The next sample notes its values in the same allocation.
        sampled.clear();
        self.coverage_scratch.sampled = sampled;
        0
    }

    /// What `get_inst_coverage` of item `item` of instance `object` gives
    /// (IEEE 1800-2023 19.8, 19.11): the coverage in percent, the number
    /// of covered bins and the number of bins.
    pub fn inst_coverage(&mut self, object: Handle, item: u32) -> (f64, u64, u64) {
        let Some(instance) = self.cover_instance(object) else {
            return (0.0, 0, 0);
        };
        let ty = self.coverage.instances[instance].ty as usize;
        if !self.cover_item(ty, item) {
            return (0.0, 0, 0);
        }
        let score = compute::instance(&self.coverage, instance, item);
        (score.coverage, score.parts.covered, score.parts.total)
    }

    /// What `get_coverage` of item `item` of the type of `key` gives (IEEE
    /// 1800-2023 19.8, 19.11), as [`Heap::inst_coverage`] does.
    pub fn type_coverage(&mut self, key: u64, item: u32) -> (f64, u64, u64) {
        let Some(ty) = self.cover_type(key).filter(|&ty| self.cover_item(ty, item)) else {
            return (0.0, 0, 0);
        };
        let score = compute::of_type(&self.coverage, ty, item);
        (score.coverage, score.parts.covered, score.parts.total)
    }

    /// Whether type `ty` has item `item`, or a fault.
    fn cover_item(&mut self, ty: usize, item: u32) -> bool {
        let descriptor = &self.coverage.types[ty].descriptor;
        let has = item as usize <= descriptor.points.len();
        if !has {
            let reason = format!("type `{}`: no item {item}", descriptor.name);
            self.cover_fault(reason);
        }
        has
    }

    /// What `$get_coverage` gives (IEEE 1800-2023 19.9): the coverage of
    /// all covergroup types in percent.
    pub fn overall_coverage(&self) -> f64 {
        compute::overall(&self.coverage)
    }

    /// Assign member `option` of `option` of item `item` of instance
    /// `object` (IEEE 1800-2023 19.7): `value` is an `int`, a bit, or the
    /// handle of a string. A member the item does not have and a negative
    /// weight are a [`Fault::Coverage`].
    pub fn set_cover_option(&mut self, object: Handle, item: u32, option: OptionId, value: u64) {
        if let Some(instance) = self.cover_instance(object) {
            let value = self.cover_option_value(option.kind(), value);
            let result = self.coverage.instances[instance]
                .options
                .set(item, option, value);
            self.cover_check(instance, result);
        }
    }

    /// Member `option` of `option` of item `item` of instance `object`: an
    /// `int` sign extended, a bit, or the handle of a string. A member that
    /// the covergroup's is the default of reads as the covergroup's until
    /// it is assigned (IEEE 1800-2023 Table 19-2).
    pub fn cover_option(&mut self, object: Handle, item: u32, option: OptionId) -> u64 {
        let Some(instance) = self.cover_instance(object) else {
            return 0;
        };
        match self.coverage.instances[instance].options.get(item, option) {
            Ok(value) => self.cover_option_result(value),
            Err(reason) => {
                self.cover_check(instance, Err(reason));
                0
            }
        }
    }

    fn cover_option_value(&self, kind: OptionKind, value: u64) -> OptionArg {
        match kind {
            OptionKind::Int => OptionArg::Int(value as u32 as i32),
            OptionKind::Bit => OptionArg::Bit(value & 1 != 0),
            OptionKind::Text => OptionArg::Text(self.string(value as Handle).to_vec()),
        }
    }

    fn cover_option_result(&mut self, value: OptionArg) -> u64 {
        match value {
            OptionArg::Int(value) => i64::from(value) as u64,
            OptionArg::Bit(value) => value.into(),
            OptionArg::Text(value) => self.intern(&value).into(),
        }
    }

    fn set_cover_type_option(&mut self, key: u64, item: u32, option: u32, value: u64) {
        let Some(ty) = self.cover_type(key) else {
            return;
        };
        let result = match TypeOptionId::from_code(option) {
            Some(option) => {
                let value = self.cover_option_value(option.kind(), value);
                self.coverage.types[ty].options.set(item, option, value)
            }
            None => Err(format!("no type option of number {option}")),
        };
        if let Err(reason) = result {
            let reason = format!(
                "type `{}`: {reason}",
                self.coverage.types[ty].descriptor.name
            );
            self.cover_fault(reason);
        }
    }

    fn cover_type_option(&mut self, key: u64, item: u32, option: u32) -> u64 {
        let Some(ty) = self.cover_type(key) else {
            return 0;
        };
        let result = match TypeOptionId::from_code(option) {
            Some(option) => self.coverage.types[ty].options.get(item, option),
            None => Err(format!("no type option of number {option}")),
        };
        match result {
            Ok(value) => self.cover_option_result(value),
            Err(reason) => {
                let reason = format!(
                    "type `{}`: {reason}",
                    self.coverage.types[ty].descriptor.name
                );
                self.cover_fault(reason);
                0
            }
        }
    }

    /// Perform the coverage operation that `parameter` selects on `a`, `b`
    /// and `c` (see [`op`] and [`code`]).
    pub fn coverage_call(&mut self, parameter: u64, a: u64, b: u64, c: u64) -> u64 {
        let x = (parameter >> 8 & FIELD) as u32;
        let y = (parameter >> 32 & FIELD) as u32;
        let object = a as Handle;
        let result = |(coverage, covered, total): (f64, u64, u64)| match x {
            0 => coverage.to_bits(),
            1 => saturate(covered),
            _ => saturate(total),
        };
        match parameter & 0xff {
            op::REGISTERED => self.coverage.by_key.contains_key(&a).into(),
            op::REGISTER_BYTES => {
                let count = (x as usize).min(8);
                self.coverage_scratch
                    .descriptors
                    .entry(a)
                    .or_default()
                    .extend_from_slice(&b.to_be_bytes()[8 - count..]);
                0
            }
            op::REGISTER => {
                let bytes = self
                    .coverage_scratch
                    .descriptors
                    .remove(&a)
                    .unwrap_or_default();
                if !self.coverage.by_key.contains_key(&a) {
                    match serde_json::from_slice::<TypeDescriptor>(&bytes) {
                        Ok(descriptor) => self.register_cover_type(a, Arc::new(descriptor)),
                        Err(error) => self.cover_fault(format!("descriptor of a type: {error}")),
                    }
                }
                0
            }
            op::NEW => self.new_cover_instance(a, b as usize, c as u32).into(),
            op::INPUT => {
                self.cover_input(object, x, y, b, c);
                0
            }
            op::INPUT_STRING => {
                if let Some(instance) = self.cover_instance(object) {
                    let bytes = self.string(b as Handle).to_vec();
                    let result = self.coverage.input_string(instance, x, bytes);
                    self.cover_check(instance, result);
                }
                0
            }
            op::BUILD => self.cover_build(object),
            op::SAMPLING => self
                .coverage
                .by_handle
                .get(&object)
                .is_some_and(|&instance| {
                    let instance = &self.coverage.instances[instance as usize];
                    instance.shape.is_some() && instance.collecting
                })
                .into(),
            op::POINT => {
                self.cover_point(x, y, b, c);
                0
            }
            op::POINTS => {
                let present = self.coverage_scratch.present.take().unwrap_or(u64::MAX);
                self.coverage_scratch.sampled.push(Sampled::Pack {
                    first: x,
                    count: y,
                    values: b,
                    masks: c,
                    present,
                });
                0
            }
            op::PRESENT => {
                self.coverage_scratch.present = Some(b);
                0
            }
            op::SAMPLE_END => self.cover_sample_end(object),
            op::INST_COVERAGE => result(self.inst_coverage(object, b as u32)),
            op::TYPE_COVERAGE => result(self.type_coverage(a, b as u32)),
            op::OVERALL => self.overall_coverage().to_bits(),
            op::COLLECT => {
                if let Some(index) = self.cover_instance(object) {
                    let instance = &mut self.coverage.instances[index];
                    let collecting = match x {
                        0 => Some(&mut instance.collecting),
                        item => instance.item_collecting.get_mut(item as usize - 1),
                    };
                    match collecting {
                        Some(collecting) => *collecting = y != 0,
                        None => self.cover_check(index, Err(format!("no item {x}"))),
                    }
                }
                0
            }
            op::SET_OPTION => {
                match OptionId::from_code(y) {
                    Some(option) => self.set_cover_option(object, x, option, b),
                    None => self.cover_fault(format!("no option of number {y}")),
                }
                0
            }
            op::GET_OPTION => match OptionId::from_code(y) {
                Some(option) => self.cover_option(object, x, option),
                None => {
                    self.cover_fault(format!("no option of number {y}"));
                    0
                }
            },
            op::SET_TYPE_OPTION => {
                self.set_cover_type_option(a, x, y, b);
                0
            }
            op::GET_TYPE_OPTION => self.cover_type_option(a, x, y),
            op::MESSAGE => {
                let text = std::mem::take(&mut self.coverage_scratch.messages).join("; ");
                self.intern(text.as_bytes()).into()
            }
            op::FORMAL_SET => {
                if let Some(instance) = self.cover_instance(object) {
                    let instance = &mut self.coverage.instances[instance];
                    let starts = &self.coverage.types[instance.ty as usize].formal_words;
                    match Registry::word_index(starts, x, y) {
                        Some(index) => {
                            instance.formals[index] = b;
                            instance.formals[index + 1] = c;
                        }
                        None => self.cover_fault(format!("formal {x} has no word {y}")),
                    }
                }
                0
            }
            op::FORMAL => self.cover_instance(object).map_or(0, |instance| {
                let instance = &self.coverage.instances[instance];
                let starts = &self.coverage.types[instance.ty as usize].formal_words;
                match Registry::word_index(starts, x, y / 2) {
                    Some(index) => instance.formals[index + (y & 1) as usize],
                    None => {
                        self.cover_fault(format!("formal {x} has no word {}", y / 2));
                        0
                    }
                }
            }),
            other => panic!("unknown coverage operation {other}"),
        }
    }

    /// The coverage of every covergroup type that has an instance, with
    /// the bins of the types and of the instances and their hits.
    pub fn coverage_report(&self) -> CoverageReport {
        report::report(&self.coverage)
    }

    /// Whether a covergroup was constructed: the heap keeps coverage, which
    /// only a copy of the heap itself records.
    pub fn has_coverage(&self) -> bool {
        self.coverage.has_instances()
    }
}
