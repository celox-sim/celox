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
    /// The crosses in declaration order (19.6). Cross `x` is the item after
    /// the coverpoints: item `points.len() + 1 + x`.
    pub crosses: Vec<CrossDescriptor>,
}

/// What an item of a covergroup type is: its `option` and `type_option`
/// have the members of that level (19.10).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Level {
    Group,
    Point,
    Cross,
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
            crosses: Vec::new(),
        }
    }

    /// The number of items but the covergroup: the coverpoints and the
    /// crosses.
    pub(crate) fn items(&self) -> usize {
        self.points.len() + self.crosses.len()
    }

    /// What item `item` is, if the type has it.
    pub(crate) fn level(&self, item: u32) -> Option<Level> {
        let item = item as usize;
        if item == 0 {
            Some(Level::Group)
        } else if item <= self.points.len() {
            Some(Level::Point)
        } else if item <= self.items() {
            Some(Level::Cross)
        } else {
            None
        }
    }

    fn validate_operand(&self, operand: &Operand) -> Result<(), String> {
        match operand {
            Operand::Constant(constant) if constant.ty.width == 0 => {
                Err("a constant of no bits".to_string())
            }
            Operand::Constant(_) => Ok(()),
            Operand::Input(index) => match self.inputs.get(*index as usize) {
                Some(InputDescriptor::Integral(ty)) if ty.width != 0 => Ok(()),
                _ => Err(format!("input {index} is not an integral input")),
            },
        }
    }

    fn validate_ranges(&self, ranges: &[RangeDescriptor]) -> Result<(), String> {
        for range in ranges {
            match range {
                RangeDescriptor::Value(value) => self.validate_operand(value)?,
                RangeDescriptor::Range { low, high } => {
                    for bound in [low, high] {
                        if let Bound::Operand(bound) = bound {
                            self.validate_operand(bound)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// What is wrong with the transitions `sequences` of bin declaration
    /// `bin` (19.5.2), as [`TypeDescriptor::validate`] tells it after the
    /// name of the bin.
    fn validate_transitions(
        &self,
        bin: &BinDescriptor,
        sequences: &[Vec<TransitionStep>],
    ) -> Result<(), String> {
        if sequences.is_empty() {
            return Err("has no transition".into());
        }
        // `[N]` is no form of a transition bin (Syntax 19-3).
        if matches!(bin.array, BinArray::Fixed(_)) {
            return Err("is a transition bin array with a size".into());
        }
        for steps in sequences {
            let Some(first) = steps.first() else {
                return Err("has a transition of no steps".into());
            };
            // "Transition bin specifications of length 0 shall be illegal"
            // (19.5.2): one value range without a repetition, or with one
            // that is 1, also as the low count of a range, which stands for
            // its counts ("3 [* 3:5]" is "the same as" its three
            // sequences). Repetition counts that a construction hands over
            // are checked when it builds the bins.
            if steps.len() == 1 && first.repeat.low() == Some(1) {
                return Err("has a transition of length 0".into());
            }
            for step in steps {
                if step.values.is_empty() {
                    return Err("has a transition step of no values".into());
                }
                self.validate_ranges(&step.values)?;
                let (low, high) = match &step.repeat {
                    Repeat::Once => continue,
                    Repeat::Consecutive(low, high) => (low, high),
                    Repeat::Goto(low, high) | Repeat::NonConsecutive(low, high) => {
                        // Such a sequence is "of unbounded or undetermined
                        // varying length" (19.5.2, 19.5.5, 19.5.6).
                        if bin.array != BinArray::Scalar {
                            return Err("is a transition bin array with a goto or nonconsecutive \
                                 repetition"
                                .into());
                        }
                        if bin.kind != BinKind::Bins {
                            return Err("is an ignored or illegal transition with a goto or \
                                 nonconsecutive repetition"
                                .into());
                        }
                        (low, high)
                    }
                };
                self.validate_operand(low)?;
                self.validate_operand(high)?;
            }
        }
        Ok(())
    }

    /// What is wrong with cross `cross`, as [`TypeDescriptor::validate`]
    /// tells it.
    fn validate_cross(&self, cross: &CrossDescriptor) -> Result<(), String> {
        if cross.points.len() < 2 {
            return Err(format!(
                "cross `{}` of fewer than two coverpoints",
                cross.name
            ));
        }
        if let Some(&point) = cross
            .points
            .iter()
            .find(|&&point| point as usize >= self.points.len())
        {
            return Err(format!(
                "cross `{}` of coverpoint {point}, which is none",
                cross.name
            ));
        }
        if cross.type_options.weight < 0 {
            return Err(format!("a negative type weight of `{}`", cross.name));
        }
        let predicate = |bit: u32| {
            if bit < cross.predicates {
                Ok(())
            } else {
                Err(format!("cross `{}` has no predicate bit {bit}", cross.name))
            }
        };
        if let Some(bit) = cross.iff {
            predicate(bit)?;
        }
        for bin in &cross.bins {
            let wrong = |what: &str| {
                Err(format!(
                    "bin `{}` of cross `{}` {what}",
                    bin.name, cross.name
                ))
            };
            if let Some(bit) = bin.iff {
                predicate(bit)?;
            }
            // The terms that are left to look at, with how deep they are.
            let mut terms = vec![(&bin.select, 0)];
            while let Some((select, depth)) = terms.pop() {
                match select {
                    Select::BinsOf {
                        position,
                        bin: declaration,
                        intersect,
                        negate: _,
                    } => {
                        let Some(&point) = cross.points.get(*position as usize) else {
                            return wrong(&format!(
                                "selects bins of cross item {position}, which is none"
                            ));
                        };
                        let point = &self.points[point as usize];
                        if let Some(declaration) = declaration
                            && *declaration as usize >= point.bins.len()
                        {
                            return wrong(&format!(
                                "selects bin {declaration} of coverpoint `{}`, which is none",
                                point.name
                            ));
                        }
                        if let Some(ranges) = intersect {
                            self.validate_ranges(ranges)?;
                        }
                    }
                    Select::And(parts) | Select::Or(parts) => {
                        if parts.is_empty() {
                            return wrong("combines no selections");
                        }
                        if depth == MAX_SELECT_DEPTH {
                            return wrong(&format!(
                                "has a selection nested deeper than {MAX_SELECT_DEPTH}"
                            ));
                        }
                        terms.extend(parts.iter().map(|part| (part, depth + 1)));
                    }
                }
            }
        }
        Ok(())
    }

    /// What makes the descriptor unusable, which only a wrong producer
    /// causes: the operations index by what this accepts.
    pub(crate) fn validate(&self) -> Result<(), String> {
        let operand = |operand: &Operand| self.validate_operand(operand);
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
                        self.validate_ranges(ranges)?;
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
                    BinValues::Transitions(sequences) => {
                        self.validate_transitions(bin, sequences)
                            .or_else(|what| wrong(&what))?;
                    }
                    BinValues::DefaultSequence => {
                        // "A default sequence specification shall not accept
                        // multiple transition bins" (19.5.2), and a default
                        // bin cannot be ignored (19.5).
                        if bin.kind == BinKind::Ignore {
                            return wrong("is an ignored `default sequence` bin");
                        }
                        if bin.array != BinArray::Scalar {
                            return wrong("is a `default sequence` bin array");
                        }
                        if bin.wildcard {
                            return wrong("is a wildcard `default sequence` bin");
                        }
                    }
                }
            }
        }
        if let Some(index) = asked.iter().position(|asked| !asked) {
            return Err(format!("filter {index} is of no bin"));
        }
        for cross in &self.crosses {
            self.validate_cross(cross)?;
        }
        for init in &self.options {
            let Some(level) = self.level(init.item) else {
                return Err(format!("an option of item {}, which it has not", init.item));
            };
            if !init.option.applies_to(level) {
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
    /// A `trans_list` (19.5.2): each element is a `trans_set`, the steps it
    /// lists with `=>`. A sample counts in the bin when the values the
    /// coverpoint was sampled with last match one of them; with
    /// [`BinArray::Open`] each sequence of values that one of them stands
    /// for is a bin of its own.
    Transitions(Vec<Vec<TransitionStep>>),
    /// `default sequence` (19.5.2): the samples at which no other
    /// transition bin of the coverpoint counts and none of the transitions
    /// that were under way goes on. Such a bin does not count towards
    /// coverage.
    DefaultSequence,
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

/// A `trans_range_list` (19.5.2): the values of one step of a transition
/// and how often they are repeated.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TransitionStep {
    /// The range list of the step, resolved as the values of a bin are
    /// (19.5.7), and for a `wildcard` declaration with x and z bits that
    /// stand for 0 and 1 (19.5.4).
    pub values: Vec<RangeDescriptor>,
    pub repeat: Repeat,
}

impl TransitionStep {
    /// The step of the values `values` without a repetition.
    pub fn new(values: Vec<RangeDescriptor>) -> Self {
        Self {
            values,
            repeat: Repeat::Once,
        }
    }
}

/// The repetition of a step of a transition (19.5.2). The counts are the
/// low and the high bound of `repeat_range`; `[* n]` is `Consecutive(n,
/// n)`. A construction evaluates them: each is a value of at least 1 and
/// without x or z bits, and the high bound is not below the low one.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Repeat {
    /// No repetition: one sample of the values.
    Once,
    /// `[* low:high]`: that many samples of the values in a row.
    Consecutive(Operand, Operand),
    /// `[-> low:high]`: that many samples of the values, with any samples
    /// of other values before each; the next step follows the last of them
    /// at once.
    Goto(Operand, Operand),
    /// `[= low:high]`: as `Goto`, but the next step may follow after more
    /// samples of other values.
    NonConsecutive(Operand, Operand),
}

impl Repeat {
    /// The low count where the descriptor tells it: 1 for `Once`, and a
    /// constant count of at most `u32::MAX`.
    fn low(&self) -> Option<u32> {
        match self {
            Self::Once => Some(1),
            Self::Consecutive(low, _) | Self::Goto(low, _) | Self::NonConsecutive(low, _) => {
                match low {
                    Operand::Constant(constant) => {
                        let ones = super::value::ones(constant.ty.width);
                        ((&constant.mask & &ones).bits() == 0)
                            .then(|| u32::try_from(&constant.value & &ones).ok())
                            .flatten()
                    }
                    Operand::Input(_) => None,
                }
            }
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

/// The deepest that the `&&` and `||` of a [`Select`] nest.
pub const MAX_SELECT_DEPTH: usize = 32;

/// A cross of coverpoints (19.6).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CrossDescriptor {
    /// The name of the cross, as reports and messages show it.
    pub name: String,
    /// The coverpoints that are crossed, two at least, by their index among
    /// [`TypeDescriptor::points`] and in the order the cross lists them. A
    /// variable that a cross lists is the coverpoint of that variable
    /// (19.6).
    pub points: Vec<u32>,
    /// The predicate bit of the `iff` guard of the cross: a sample with the
    /// bit clear is none of the cross.
    pub iff: Option<u32>,
    /// The bins the cross declares, in declaration order (19.6.1). The
    /// cross products that are in none of them are its automatic bins.
    pub bins: Vec<CrossBinDescriptor>,
    /// The type options of the cross (19.7.1).
    pub type_options: ItemTypeOptions,
    /// The number of predicate bits a sample hands over for the cross: its
    /// guard and the guards of its bins.
    pub predicates: u32,
}

impl CrossDescriptor {
    /// The cross `name` of the coverpoints `points` without a guard or bins
    /// of its own, with the default type options.
    pub fn new(name: impl Into<String>, points: Vec<u32>) -> Self {
        Self {
            name: name.into(),
            points,
            iff: None,
            bins: Vec::new(),
            type_options: ItemTypeOptions::default(),
            predicates: 0,
        }
    }
}

/// A bin declaration of a cross (19.6.1): a name and a count for the cross
/// products that `select` selects.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CrossBinDescriptor {
    /// The bin identifier, as reports and messages show it.
    pub name: String,
    /// `bins`, or `ignore_bins` and `illegal_bins`, whose cross products
    /// are those of no other bin of the cross (19.6.2, 19.6.3).
    pub kind: BinKind,
    /// The select expression: the cross products of the bin.
    pub select: Select,
    /// The predicate bit of the `iff` guard of the declaration: a sample
    /// with the bit clear does not count in the bin.
    pub iff: Option<u32>,
}

impl CrossBinDescriptor {
    /// `bins name = select` without a guard.
    pub fn new(name: impl Into<String>, select: Select) -> Self {
        Self {
            name: name.into(),
            kind: BinKind::Bins,
            select,
            iff: None,
        }
    }
}

/// A select expression of a cross bin (19.6.1): the cross products it
/// selects.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Select {
    /// `binsof(item)`, `binsof(item.bin)`, either with `intersect {…}`, and
    /// the negation of each with `!`: the cross products whose bin of one
    /// of the crossed coverpoints is a selected one.
    BinsOf {
        /// The coverpoint, by its position in [`CrossDescriptor::points`].
        position: u32,
        /// The bin declaration of the coverpoint whose bins are selected,
        /// by its index among [`PointDescriptor::bins`]; every bin of the
        /// coverpoint without one.
        bin: Option<u32>,
        /// Of those bins only the ones that hold a value of this range
        /// list, which is resolved as that of a bin is (19.5.7).
        intersect: Option<Vec<RangeDescriptor>>,
        /// `!`: the bins of the coverpoint that are not selected so.
        negate: bool,
    },
    /// `&&`: the cross products that every term selects. A chain of `&&`
    /// is one node.
    And(Vec<Select>),
    /// `||`: the cross products that a term selects.
    Or(Vec<Select>),
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

/// The type options of a coverpoint or cross (19.7.1, Table 19-4).
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

/// The type of an option member (19.10): what [`op::SET_OPTION`] and
/// [`op::SET_TYPE_OPTION`] take as the value and [`op::GET_OPTION`] and
/// [`op::GET_TYPE_OPTION`] give.
///
/// [`op::SET_OPTION`]: super::op::SET_OPTION
/// [`op::SET_TYPE_OPTION`]: super::op::SET_TYPE_OPTION
/// [`op::GET_OPTION`]: super::op::GET_OPTION
/// [`op::GET_TYPE_OPTION`]: super::op::GET_TYPE_OPTION
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OptionKind {
    /// An `int`, sign extended.
    Int,
    /// A `bit`.
    Bit,
    /// A `string`, as the handle of a heap string.
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
    /// `cross_num_print_missing`: how many uncovered automatic bins of a
    /// cross a report lists.
    CrossNumPrintMissing,
    /// `cross_retain_auto_bins`: whether a cross keeps automatic bins beside
    /// the bins it declares.
    CrossRetainAutoBins,
}

impl OptionId {
    const ALL: [Self; 11] = [
        Self::Name,
        Self::Weight,
        Self::Goal,
        Self::Comment,
        Self::AtLeast,
        Self::AutoBinMax,
        Self::DetectOverlap,
        Self::PerInstance,
        Self::GetInstCoverage,
        Self::CrossNumPrintMissing,
        Self::CrossRetainAutoBins,
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
            Self::CrossNumPrintMissing => 9,
            Self::CrossRetainAutoBins => 10,
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
            Self::CrossNumPrintMissing => "cross_num_print_missing",
            Self::CrossRetainAutoBins => "cross_retain_auto_bins",
        }
    }

    /// The type of the member (19.10).
    pub const fn kind(self) -> OptionKind {
        match self {
            Self::Name | Self::Comment => OptionKind::Text,
            Self::Weight
            | Self::Goal
            | Self::AtLeast
            | Self::AutoBinMax
            | Self::CrossNumPrintMissing => OptionKind::Int,
            Self::DetectOverlap
            | Self::PerInstance
            | Self::GetInstCoverage
            | Self::CrossRetainAutoBins => OptionKind::Bit,
        }
    }

    /// Whether an item of `level` has the member (19.10): a coverpoint and
    /// a cross have those of their own `option` structure only.
    pub(crate) const fn applies_to(self, level: Level) -> bool {
        match level {
            Level::Group => true,
            Level::Point => matches!(
                self,
                Self::Weight
                    | Self::Goal
                    | Self::Comment
                    | Self::AtLeast
                    | Self::AutoBinMax
                    | Self::DetectOverlap
            ),
            Level::Cross => matches!(
                self,
                Self::Weight
                    | Self::Goal
                    | Self::Comment
                    | Self::AtLeast
                    | Self::CrossNumPrintMissing
                    | Self::CrossRetainAutoBins
            ),
        }
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

    /// The type of the member (19.10).
    pub const fn kind(self) -> OptionKind {
        match self {
            Self::Comment => OptionKind::Text,
            Self::Weight | Self::Goal => OptionKind::Int,
            Self::MergeInstances | Self::DistributeFirst => OptionKind::Bit,
        }
    }
}
