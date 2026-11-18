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
        if self.type_options.weight < 0 {
            return Err("a negative type weight".into());
        }
        for point in &self.points {
            if point.ty.width == 0 {
                return Err(format!("coverpoint `{}` of no bits", point.name));
            }
            if point.type_options.weight < 0 {
                return Err(format!("a negative type weight of `{}`", point.name));
            }
            for bin in &point.bins {
                let BinValues::Ranges { ranges } = &bin.values;
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
            }
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
    /// A string, whose bytes the instance copies.
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
        }
    }

    /// Whether the coverpoint gets automatic bins (19.5.3): it "does not
    /// define any bins except ignored or illegal bins".
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
}

impl BinDescriptor {
    /// The scalar bin `bins name = values`.
    pub fn new(name: impl Into<String>, values: BinValues) -> Self {
        Self {
            name: name.into(),
            kind: BinKind::Bins,
            values,
            array: BinArray::Scalar,
        }
    }
}

/// The keyword of a bin declaration.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BinKind {
    /// `bins`: a bin that counts towards coverage.
    Bins,
}

/// What a bin collects.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BinValues {
    /// The values of a range list (19.5.1), each resolved against the type
    /// of the coverpoint (19.5.7).
    Ranges { ranges: Vec<RangeDescriptor> },
}

impl BinValues {
    /// The values of the range list `ranges`.
    pub fn ranges(ranges: Vec<RangeDescriptor>) -> Self {
        Self::Ranges { ranges }
    }
}

/// How many bins a declaration makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BinArray {
    /// One bin for all the values.
    Scalar,
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
