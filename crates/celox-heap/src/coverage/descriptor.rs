//! The description of a covergroup type (IEEE 1800-2023 19.3): what the
//! analyzer knows of it at elaboration, which compiled code registers once
//! and every instance of the type is built from.
//!
//! A descriptor holds no value that a construction computes: such a value is
//! an [`Operand::Input`], which the constructing code hands over before it
//! builds the instance. The analyzer makes descriptors with the `new`
//! functions here and then assigns the public fields it needs, so that a
//! field added later needs no change there.

use num_bigint::BigUint;
use serde::{Deserialize, Serialize};

/// A covergroup type.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TypeDescriptor {
    /// The name reports and messages show, such as `Top.cg` or `p::cg`.
    pub name: String,
    /// Whether the covergroup is embedded in a class (19.4): a shallow copy
    /// of the object that holds an instance gets `null` instead (8.12).
    pub embedded: bool,
    /// The values a construction hands over, by input index.
    pub inputs: Vec<InputDescriptor>,
    /// The integral formals that sampling reads, by formal slot: an instance
    /// keeps their values.
    pub formals: Vec<ValueType>,
    /// The coverpoints in declaration order. Coverpoint `k` is item `k + 1`;
    /// item 0 is the covergroup.
    pub points: Vec<PointDescriptor>,
    /// The `option.member = expression` assignments of the definition
    /// (19.7), in the order a construction applies them.
    pub options: Vec<OptionInit>,
    /// The type options of the covergroup (19.7.1).
    pub type_options: GroupTypeOptions,
    /// The element type of each array a construction hands over as the
    /// values of a bin (19.5.1.2), by set index.
    pub sets: Vec<ValueType>,
    /// The `with` filters that a construction evaluates for each value of a
    /// bin (19.5.1.1), by filter index.
    pub filters: Vec<FilterDescriptor>,
}

impl TypeDescriptor {
    /// A covergroup type of `name` without formals or items, with the
    /// default type options.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            embedded: false,
            inputs: Vec::new(),
            formals: Vec::new(),
            points: Vec::new(),
            options: Vec::new(),
            type_options: GroupTypeOptions::default(),
            sets: Vec::new(),
            filters: Vec::new(),
        }
    }

    /// What makes the descriptor unusable, which only a wrong producer
    /// causes: the operations index by what this accepts.
    pub(crate) fn validate(&self) -> Result<(), String> {
        let operand = |operand: &Operand| match operand {
            Operand::Constant(constant) if constant.ty.width == 0 => {
                Err("a constant of no bits".to_string())
            }
            Operand::Constant(_) => Ok(()),
            Operand::Input(index) => match self.inputs.get(*index as usize) {
                Some(InputDescriptor::Integral(ty)) if ty.width != 0 => Ok(()),
                _ => Err(format!("input {index} is not an integral input")),
            },
        };
        if self.formals.iter().any(|formal| formal.width == 0) {
            return Err("a formal of no bits".into());
        }
        if self.sets.iter().any(|set| set.width == 0) {
            return Err("a set of elements of no bits".into());
        }
        if self.type_options.weight < 0 {
            return Err("a negative type weight".into());
        }
        // The bin that asks each filter.
        let mut asked = vec![false; self.filters.len()];
        for point in &self.points {
            if point.ty.width == 0 {
                return Err(format!("coverpoint `{}` of no bits", point.name));
            }
            if point.type_options.weight < 0 {
                return Err(format!("a negative type weight of `{}`", point.name));
            }
            let predicate = |bit: u32| {
                if bit < point.predicates {
                    Ok(())
                } else {
                    Err(format!(
                        "coverpoint `{}` has no predicate bit {bit}",
                        point.name
                    ))
                }
            };
            for bin in &point.bins {
                let wrong = |what: &str| {
                    Err(format!(
                        "bin `{}` of coverpoint `{}` {what}",
                        bin.name, point.name
                    ))
                };
                if let Some(bit) = bin.iff {
                    predicate(bit)?;
                }
                if let BinArray::Fixed(count) = &bin.array {
                    operand(count)?;
                }
                match &bin.values {
                    BinValues::Ranges { ranges, filter } => {
                        for range in ranges {
                            match range {
                                RangeDescriptor::Value(value) => operand(value)?,
                                RangeDescriptor::Range { low, high } => {
                                    for bound in [low, high] {
                                        if let Bound::Operand(bound) = bound {
                                            operand(bound)?;
                                        }
                                    }
                                }
                            }
                        }
                        match filter {
                            None => {}
                            Some(BinFilter::Predicate(bit)) => {
                                predicate(*bit)?;
                                if bin.kind != BinKind::Bins || bin.array != BinArray::Scalar {
                                    return wrong(
                                        "is filtered at each sample but no scalar `bins`",
                                    );
                                }
                            }
                            Some(BinFilter::Enumerate(index)) => {
                                let Some(filter) = self.filters.get(*index as usize) else {
                                    return wrong(&format!("has filter {index}, which is none"));
                                };
                                let item = ValueType {
                                    width: point.ty.width,
                                    signed: point.ty.signed,
                                };
                                if filter.components != [item] {
                                    return wrong(&format!(
                                        "has filter {index}, which is not of its values"
                                    ));
                                }
                                if std::mem::replace(&mut asked[*index as usize], true) {
                                    return wrong(&format!(
                                        "has filter {index}, which another bin has"
                                    ));
                                }
                            }
                        }
                    }
                    BinValues::Set(index) => {
                        if *index as usize >= self.sets.len() {
                            return wrong(&format!("has set {index}, which is none"));
                        }
                    }
                    BinValues::Default => {
                        // "A default bin cannot be explicitly ignored" (19.5),
                        // and the values of one with a size have no
                        // distribution.
                        if bin.kind == BinKind::Ignore {
                            return wrong("is an ignored `default` bin");
                        }
                        if matches!(bin.array, BinArray::Fixed(_)) {
                            return wrong("is a `default` bin array with a size");
                        }
                        if bin.wildcard {
                            return wrong("is a wildcard `default` bin");
                        }
                    }
                }
            }
        }
        if let Some(index) = asked.iter().position(|asked| !asked) {
            return Err(format!("filter {index} is of no bin"));
        }
        for init in &self.options {
            if init.item as usize > self.points.len() {
                return Err(format!("an option of item {}, which it has not", init.item));
            }
            if !init.option.applies_to(init.item) {
                return Err(format!(
                    "item {} has no option `{}`",
                    init.item,
                    init.option.name()
                ));
            }
            match (&init.value, init.option.kind()) {
                (OptionValue::String(_), OptionKind::Text) => {}
                (OptionValue::StringInput(index), OptionKind::Text) => {
                    if !matches!(
                        self.inputs.get(*index as usize),
                        Some(InputDescriptor::String)
                    ) {
                        return Err(format!("input {index} is not a string input"));
                    }
                }
                (OptionValue::Operand(value), OptionKind::Int | OptionKind::Bit) => operand(value)?,
                _ => {
                    return Err(format!(
                        "a value of another type for option `{}`",
                        init.option.name()
                    ));
                }
            }
        }
        Ok(())
    }
}

/// A value a construction hands over.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum InputDescriptor {
    /// An integral value of this type, in 64-bit words of its two planes.
    Integral(ValueType),
    /// A string, whose bytes the instance copies: the value of an option,
    /// or else a value that the `with` filters of the type read, which
    /// decides the bins of the construction with the integral inputs.
    String,
}

/// The width in bits and the signedness of an integral value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ValueType {
    pub width: u32,
    pub signed: bool,
}

/// A value of a definition that a construction needs.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Operand {
    /// A value known at elaboration.
    Constant(Constant),
    /// The integral input of this index.
    Input(u32),
}

/// An integral value with its type. A bit that is set in `mask` is x where
/// `value` has it set and z where not, the encoding of state memory.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Constant {
    pub ty: ValueType,
    pub value: BigUint,
    pub mask: BigUint,
}

/// A coverpoint (19.5).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PointDescriptor {
    pub name: String,
    /// Whether the coverpoint has no label and the name is made up, which
    /// only reports show (19.5).
    pub generated_name: bool,
    pub ty: PointType,
    /// The bins in declaration order.
    pub bins: Vec<BinDescriptor>,
    /// The type options of the coverpoint (19.7.1).
    pub type_options: ItemTypeOptions,
    /// The number of predicate bits a sample hands over for the
    /// coverpoint: the guards of its bins and the filters that are
    /// evaluated at each sample.
    pub predicates: u32,
}

impl PointDescriptor {
    /// A coverpoint of `name` and `ty` without bins of its own, with the
    /// default type options.
    pub fn new(name: impl Into<String>, ty: PointType) -> Self {
        Self {
            name: name.into(),
            generated_name: false,
            ty,
            bins: Vec::new(),
            type_options: ItemTypeOptions::default(),
            predicates: 0,
        }
    }

    /// Whether the coverpoint gets automatic bins (19.5.3): it "does not
    /// define any bins except ignored or illegal bins". A `default` bin of
    /// `bins` is a bin it defines.
    pub fn automatic(&self) -> bool {
        !self.bins.iter().any(|bin| bin.kind == BinKind::Bins)
    }
}

/// The effective type of a coverpoint expression (19.5.7).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PointType {
    pub width: u32,
    pub signed: bool,
    /// Whether a value of the type may hold x or z bits (19.5.4).
    pub four_state: bool,
    /// The enumerators of an enumeration with their values: its automatic
    /// bins are one for each (19.5.3). Empty for any other type.
    pub members: Vec<(String, Constant)>,
}

/// A bin declaration of a coverpoint (19.5.1).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BinDescriptor {
    pub name: String,
    pub kind: BinKind,
    pub values: BinValues,
    pub array: BinArray,
    /// Whether the declaration is `wildcard`: an x or z bit of a value
    /// stands for both 0 and 1 (19.5.4).
    pub wildcard: bool,
    /// The predicate bit of the `iff` guard of the declaration (19.5.1): a
    /// sample with the bit clear does not count in its bins.
    pub iff: Option<u32>,
}

impl BinDescriptor {
    /// The scalar bin `bins name = values` without a guard.
    pub fn new(name: impl Into<String>, values: BinValues) -> Self {
        Self {
            name: name.into(),
            kind: BinKind::Bins,
            values,
            array: BinArray::Scalar,
            wildcard: false,
            iff: None,
        }
    }
}

/// The keyword of a bin declaration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BinKind {
    /// `bins`: a bin that counts towards coverage.
    Bins,
    /// `ignore_bins`: its values are values of no `bins` bin (19.5.5).
    Ignore,
    /// `illegal_bins`: its values are values of no `bins` bin, and a sample
    /// of one is an error (19.5.6).
    Illegal,
}

impl BinKind {
    /// The keyword of the declaration.
    pub const fn keyword(self) -> &'static str {
        match self {
            Self::Bins => "bins",
            Self::Ignore => "ignore_bins",
            Self::Illegal => "illegal_bins",
        }
    }
}

/// What a bin collects.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BinValues {
    /// The values of a range list (19.5.1), each resolved against the type
    /// of the coverpoint (19.5.7), and of them those a `with` filter keeps
    /// (19.5.1.1).
    Ranges {
        ranges: Vec<RangeDescriptor>,
        filter: Option<BinFilter>,
    },
    /// The elements of the array of this set index (19.5.1.2), each
    /// resolved as a single value is.
    Set(u32),
    /// `default`: the values that are in no other bin of the coverpoint
    /// (19.5). Such a bin does not count towards coverage.
    Default,
}

impl BinValues {
    /// The values of the range list `ranges`, unfiltered.
    pub fn ranges(ranges: Vec<RangeDescriptor>) -> Self {
        Self::Ranges {
            ranges,
            filter: None,
        }
    }
}

/// How the `with` filter of a bin is applied (19.5.1.1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BinFilter {
    /// The construction evaluates the filter of this index for each value:
    /// the bin has the values it keeps.
    Enumerate(u32),
    /// The sample evaluates the expression for the sampled value into the
    /// predicate bit of this index: the bin has every value of its range
    /// list, and a sample with the bit clear is none of the bin. For a
    /// scalar `bins` of more values than a construction enumerates.
    Predicate(u32),
}

/// A `with` filter that a construction evaluates.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FilterDescriptor {
    /// The types of the values a candidate is of: for the filter of a
    /// coverpoint bin the type of the coverpoint, as `item` has it.
    pub components: Vec<ValueType>,
    /// Whether what the filter keeps is decided by what a construction
    /// hands over: its integral inputs, the string inputs that no option
    /// takes, and its sets. A construction that hands over what an earlier
    /// one of the type did then takes its bins and is not asked (19.5.1.1
    /// allows "caching of previously calculated results"). With a filter
    /// that is not, such as one that calls a method of an object, every
    /// construction of the type is asked about every filter and has bins
    /// of its own.
    pub shared: bool,
}

impl FilterDescriptor {
    /// A filter whose values are of the types `components` and that keeps
    /// what the values a construction hands over decide.
    pub fn new(components: Vec<ValueType>) -> Self {
        Self {
            components,
            shared: true,
        }
    }
}

/// How many bins a declaration makes.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BinArray {
    /// One bin for all the values.
    Scalar,
    /// `name[]`: a bin for each value (19.5.1), or for a `default` bin a bin
    /// for each value that is sampled.
    Open,
    /// `name[count]`: this many bins, among which the values are
    /// distributed (19.5.1).
    Fixed(Operand),
}

/// An element of a range list.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RangeDescriptor {
    Value(Operand),
    Range { low: Bound, high: Bound },
}

/// A bound of a range.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Bound {
    /// `$`: the lowest or highest value of the coverpoint type.
    Open,
    Operand(Operand),
}

/// An `option.member = expression` of a covergroup definition (19.7).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct OptionInit {
    /// The item whose option it is: 0 for the covergroup.
    pub item: u32,
    pub option: OptionId,
    pub value: OptionValue,
}

/// The value of an [`OptionInit`].
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OptionValue {
    /// The value of an `int` or `bit` member.
    Operand(Operand),
    /// The string input of this index, for a `string` member.
    StringInput(u32),
    /// A string known at elaboration.
    String(String),
}

/// The type options of a covergroup (19.7.1, Table 19-3).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct GroupTypeOptions {
    pub weight: i32,
    pub goal: i32,
    pub comment: String,
    pub merge_instances: bool,
    pub distribute_first: bool,
}

impl Default for GroupTypeOptions {
    fn default() -> Self {
        Self {
            weight: 1,
            goal: 100,
            comment: String::new(),
            merge_instances: false,
            distribute_first: false,
        }
    }
}

/// The type options of a coverpoint (19.7.1, Table 19-4).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ItemTypeOptions {
    pub weight: i32,
    pub goal: i32,
    pub comment: String,
}

impl Default for ItemTypeOptions {
    fn default() -> Self {
        Self {
            weight: 1,
            goal: 100,
            comment: String::new(),
        }
    }
}

/// The type of an option member (19.10).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OptionKind {
    Int,
    Bit,
    Text,
}

/// A member of `option` (19.7, Table 19-1).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OptionId {
    Name,
    Weight,
    Goal,
    Comment,
    AtLeast,
    AutoBinMax,
    DetectOverlap,
    PerInstance,
    GetInstCoverage,
}

impl OptionId {
    const ALL: [Self; 9] = [
        Self::Name,
        Self::Weight,
        Self::Goal,
        Self::Comment,
        Self::AtLeast,
        Self::AutoBinMax,
        Self::DetectOverlap,
        Self::PerInstance,
        Self::GetInstCoverage,
    ];

    /// The number of the member in an operation code.
    pub const fn code(self) -> u32 {
        match self {
            Self::Name => 0,
            Self::Weight => 1,
            Self::Goal => 2,
            Self::Comment => 3,
            Self::AtLeast => 4,
            Self::AutoBinMax => 5,
            Self::DetectOverlap => 6,
            Self::PerInstance => 7,
            Self::GetInstCoverage => 8,
        }
    }

    /// The member of number `code`.
    pub fn from_code(code: u32) -> Option<Self> {
        Self::ALL.into_iter().find(|option| option.code() == code)
    }

    /// The member as source code names it.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Weight => "weight",
            Self::Goal => "goal",
            Self::Comment => "comment",
            Self::AtLeast => "at_least",
            Self::AutoBinMax => "auto_bin_max",
            Self::DetectOverlap => "detect_overlap",
            Self::PerInstance => "per_instance",
            Self::GetInstCoverage => "get_inst_coverage",
        }
    }

    pub(crate) const fn kind(self) -> OptionKind {
        match self {
            Self::Name | Self::Comment => OptionKind::Text,
            Self::Weight | Self::Goal | Self::AtLeast | Self::AutoBinMax => OptionKind::Int,
            Self::DetectOverlap | Self::PerInstance | Self::GetInstCoverage => OptionKind::Bit,
        }
    }

    /// Whether `item` has the member (19.10): a coverpoint has those of
    /// its own `option` structure only.
    pub(crate) const fn applies_to(self, item: u32) -> bool {
        item == 0
            || matches!(
                self,
                Self::Weight
                    | Self::Goal
                    | Self::Comment
                    | Self::AtLeast
                    | Self::AutoBinMax
                    | Self::DetectOverlap
            )
    }
}

/// A member of `type_option` (19.7.1, Table 19-3).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TypeOptionId {
    Weight,
    Goal,
    Comment,
    MergeInstances,
    DistributeFirst,
}

impl TypeOptionId {
    const ALL: [Self; 5] = [
        Self::Weight,
        Self::Goal,
        Self::Comment,
        Self::MergeInstances,
        Self::DistributeFirst,
    ];

    /// The number of the member in an operation code.
    pub const fn code(self) -> u32 {
        match self {
            Self::Weight => 0,
            Self::Goal => 1,
            Self::Comment => 2,
            Self::MergeInstances => 3,
            Self::DistributeFirst => 4,
        }
    }

    /// The member of number `code`.
    pub fn from_code(code: u32) -> Option<Self> {
        Self::ALL.into_iter().find(|option| option.code() == code)
    }

    /// The member as source code names it.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Weight => "weight",
            Self::Goal => "goal",
            Self::Comment => "comment",
            Self::MergeInstances => "merge_instances",
            Self::DistributeFirst => "distribute_first",
        }
    }

    pub(crate) const fn kind(self) -> OptionKind {
        match self {
            Self::Comment => OptionKind::Text,
            Self::Weight | Self::Goal => OptionKind::Int,
            Self::MergeInstances | Self::DistributeFirst => OptionKind::Bit,
        }
    }
}
