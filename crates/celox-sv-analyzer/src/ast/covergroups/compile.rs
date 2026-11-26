//! What the items of a covergroup declaration are to the analysis (IEEE
//! 1800-2023 19.5, 19.7): which of them it compiles into the descriptor of
//! the type and the subroutines (see `procedural::covergroup_subroutines`),
//! which it rejects where the type is used, and which the standard makes
//! illegal.
//!
//! Everything here reads the declaration alone. What needs the names of the
//! scope, the types of the coverpoints and the values of the bins, is found
//! when the subroutines are made.

use super::*;
use celox_heap::coverage::{OptionId, TypeOptionId};
use celox_sv_syntax::{
    Bins, BinsArray, BinsKind, BinsValue, CoverageItem, CoverageOption, CoverageRange,
    CoverageRangeKind, CovergroupDeclaration, CovergroupKind, Coverpoint, CoverpointItem, Cross,
    CrossBins, CrossItem, ExprId, ExprKind, SelectId, SelectKind, TokenRange, TypeId,
};

fn invalid(text: impl Into<String>) -> AnalyzerError {
    AnalyzerError::InvalidCovergroup(text.into())
}

/// Which of a covergroup, a coverpoint and a cross has an option member
/// (IEEE 1800-2023 19.10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ast) enum Level {
    Covergroup,
    Coverpoint,
    Cross,
}

impl Level {
    fn noun(self) -> &'static str {
        match self {
            Self::Covergroup => "a covergroup",
            Self::Coverpoint => "a coverpoint",
            Self::Cross => "a cross",
        }
    }
}

/// The type of an option member (19.10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ast) enum OptionType {
    Int,
    Bit,
    Text,
}

/// A member of `option` or `type_option` as the heap keeps it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::ast) enum Member {
    Option(OptionId),
    Type(TypeOptionId),
    /// `type_option.strobe`, which the heap does not keep: it has no effect
    /// on a type without a clocking event (19.3), and a type with one that
    /// sets it is rejected.
    Strobe,
}

/// A member of `option` (Table 19-1): its name, whether a covergroup, a
/// coverpoint and a cross have it (19.10), what it is to the heap, and
/// whether it "can only be set in the covergroup definition" (19.7).
#[allow(clippy::type_complexity)]
const OPTIONS: [(&str, [bool; 3], (OptionId, OptionType), bool); 11] = [
    (
        "name",
        [true, false, false],
        (OptionId::Name, OptionType::Text),
        false,
    ),
    (
        "weight",
        [true, true, true],
        (OptionId::Weight, OptionType::Int),
        false,
    ),
    (
        "goal",
        [true, true, true],
        (OptionId::Goal, OptionType::Int),
        false,
    ),
    (
        "comment",
        [true, true, true],
        (OptionId::Comment, OptionType::Text),
        false,
    ),
    (
        "at_least",
        [true, true, true],
        (OptionId::AtLeast, OptionType::Int),
        false,
    ),
    (
        "auto_bin_max",
        [true, true, false],
        (OptionId::AutoBinMax, OptionType::Int),
        true,
    ),
    (
        "cross_num_print_missing",
        [true, false, true],
        (OptionId::CrossNumPrintMissing, OptionType::Int),
        false,
    ),
    (
        "cross_retain_auto_bins",
        [true, false, true],
        (OptionId::CrossRetainAutoBins, OptionType::Bit),
        true,
    ),
    (
        "detect_overlap",
        [true, true, false],
        (OptionId::DetectOverlap, OptionType::Bit),
        true,
    ),
    (
        "per_instance",
        [true, false, false],
        (OptionId::PerInstance, OptionType::Bit),
        true,
    ),
    (
        "get_inst_coverage",
        [true, false, false],
        (OptionId::GetInstCoverage, OptionType::Bit),
        true,
    ),
];

/// A member of `type_option` (Table 19-3): its name, whether a covergroup,
/// a coverpoint and a cross have it (19.10), what it is to the heap if the
/// analysis supports it, and whether it "can only be set in the covergroup
/// definition" (19.7.1).
#[allow(clippy::type_complexity)]
const TYPE_OPTIONS: [(&str, [bool; 3], Option<(Member, OptionType)>, bool); 7] = [
    (
        "weight",
        [true, true, true],
        Some((Member::Type(TypeOptionId::Weight), OptionType::Int)),
        false,
    ),
    (
        "goal",
        [true, true, true],
        Some((Member::Type(TypeOptionId::Goal), OptionType::Int)),
        false,
    ),
    (
        "comment",
        [true, true, true],
        Some((Member::Type(TypeOptionId::Comment), OptionType::Text)),
        false,
    ),
    (
        "strobe",
        [true, false, false],
        Some((Member::Strobe, OptionType::Bit)),
        true,
    ),
    (
        "merge_instances",
        [true, false, false],
        Some((Member::Type(TypeOptionId::MergeInstances), OptionType::Bit)),
        false,
    ),
    (
        "distribute_first",
        [true, false, false],
        Some((Member::Type(TypeOptionId::DistributeFirst), OptionType::Bit)),
        false,
    ),
    ("real_interval", [true, true, false], None, true),
];

/// Member `member` of `type_option` if `type_option`, else of `option`, at
/// `level` of covergroup `display`, which the definition assigns if
/// `defined`, else a procedure uses: what it is to the heap and whether it
/// can be set only in the definition. An error is a member that does not
/// exist or that the level has not (19.10); `Ok(Err(_))` is the rejection
/// of a member the analysis does not support.
#[allow(clippy::type_complexity)]
fn lookup(
    type_option: bool,
    member: &str,
    level: Level,
    display: &str,
    defined: bool,
) -> Converted<Result<((Member, OptionType), bool), AnalyzerError>> {
    let structure = if type_option { "type_option" } else { "option" };
    let found = if type_option {
        TYPE_OPTIONS
            .iter()
            .find(|(name, ..)| *name == member)
            .map(|&(_, levels, compiled, definition)| (levels, compiled, definition))
    } else {
        OPTIONS.iter().find(|(name, ..)| *name == member).map(
            |&(_, levels, (id, kind), definition)| {
                (levels, Some((Member::Option(id), kind)), definition)
            },
        )
    };
    let Some((levels, compiled, definition)) = found else {
        return Err(invalid(if defined {
            format!(
                "`{structure}` has no member `{member}`, which covergroup `{display}` assigns (IEEE 1800-2023 19.10)"
            )
        } else {
            format!(
                "`{structure}` of covergroup `{display}` has no member `{member}` (IEEE 1800-2023 19.10)"
            )
        }));
    };
    let has = levels[match level {
        Level::Covergroup => 0,
        Level::Coverpoint => 1,
        Level::Cross => 2,
    }];
    if !has {
        return Err(invalid(format!(
            "`{structure}.{member}` is {} {} of covergroup `{display}`, which has no such member (IEEE 1800-2023 19.10)",
            if defined { "assigned in" } else { "used on" },
            level.noun()
        )));
    }
    Ok(match compiled {
        Some(compiled) => Ok((compiled, definition)),
        None => Err(unsupported(format!(
            "covergroup {structure}.{member} (IEEE 1800-2023 19.7.1)"
        ))),
    })
}

/// What the definition-level assignment `option` at `level` compiles to:
/// the member of the heap and its type. An error is what the standard
/// excludes, a member that does not exist or that the level has not
/// (19.7, 19.10); `Ok(Err(_))` is the rejection of a member that the
/// analysis does not support.
pub(in crate::ast) fn option_member(
    option: &CoverageOption,
    level: Level,
    display: &str,
) -> Converted<Result<(Member, OptionType), AnalyzerError>> {
    let member = instances::normalize_identifier(&option.member.spelling);
    Ok(lookup(option.type_option, &member, level, display, true)?.map(|(compiled, _)| compiled))
}

/// What a use of member `member` of `type_option` if `type_option`, else of
/// `option`, of `level` of covergroup `display` in a procedure is: a write
/// if `write` (19.7, 19.7.1). The members that "can only be set in the
/// covergroup definition" are errors to write; `Ok(Err(_))` is the
/// rejection of what the analysis does not support, a member the heap does
/// not keep.
pub(in crate::ast) fn procedural_member(
    type_option: bool,
    member: &str,
    level: Level,
    display: &str,
    write: bool,
) -> Converted<Result<(Member, OptionType), AnalyzerError>> {
    let structure = if type_option { "type_option" } else { "option" };
    let found = lookup(type_option, member, level, display, false)?;
    let definition = match &found {
        Ok((_, definition)) => *definition,
        // `real_interval` can be set only in the definition, as `strobe`.
        Err(_) => true,
    };
    if write && definition {
        return Err(invalid(format!(
            "`{structure}.{member}` of {} of covergroup `{display}` is assigned in a procedure: it can be set only in the covergroup definition (IEEE 1800-2023 {})",
            level.noun(),
            if type_option { "19.7.1" } else { "19.7" }
        )));
    }
    Ok(match found {
        Ok((compiled, _)) if !matches!(compiled.0, Member::Strobe) => Ok(compiled),
        _ => Err(unsupported(format!(
            "covergroup {structure}.{member} read in a procedure (IEEE 1800-2023 19.7.1)"
        ))),
    })
}

/// The name of coverpoint `coverpoint`, item `item` of its covergroup, and
/// whether the analysis made it up: the label, else the variable the
/// coverpoint is of, else a name that only reports show (19.5).
pub(in crate::ast) fn point_name(
    native: &Declarations,
    coverpoint: &Coverpoint,
    item: usize,
) -> (String, bool) {
    if let Some(label) = &coverpoint.label {
        return (instances::normalize_identifier(&label.spelling), false);
    }
    match &native
        .expression(expression_binding::ungroup(coverpoint.expression, native))
        .kind
    {
        ExprKind::Name(name) => (instances::normalize_identifier(&name.spelling), false),
        _ => (format!("$point{item}"), true),
    }
}

/// The coverpoints of `declaration` in declaration order: coverpoint `k` is
/// item `k + 1` of the type.
pub(in crate::ast) fn coverpoints(
    declaration: &CovergroupDeclaration,
) -> impl Iterator<Item = &Coverpoint> {
    declaration.items.iter().filter_map(|item| match item {
        CoverageItem::Coverpoint(coverpoint) => Some(coverpoint),
        _ => None,
    })
}

/// The crosses of `declaration` in declaration order.
pub(in crate::ast) fn crosses(declaration: &CovergroupDeclaration) -> impl Iterator<Item = &Cross> {
    declaration.items.iter().filter_map(|item| match item {
        CoverageItem::Cross(cross) => Some(cross),
        _ => None,
    })
}

/// The identifier cross item `item` spells (19.6).
pub(in crate::ast) fn cross_item_name(native: &Declarations, item: ExprId) -> String {
    match &native.expression(item).kind {
        ExprKind::Name(name) => instances::normalize_identifier(&name.spelling),
        // The parser makes a name of each.
        _ => String::new(),
    }
}

/// The name of cross `cross`, the `position`-th of its covergroup from 1:
/// its label, else one that only reports show, as an unlabelled coverpoint
/// of an expression has (19.5, 19.6).
pub(in crate::ast) fn cross_name(cross: &Cross, position: usize) -> String {
    match &cross.label {
        Some(label) => instances::normalize_identifier(&label.spelling),
        None => format!("$cross{position}"),
    }
}

/// The coverpoints of a covergroup as the analysis numbers them, and what
/// each cross crosses (19.6): the coverpoints it declares, in declaration
/// order, then a coverpoint for each variable that a cross crosses and
/// that is no coverpoint, "as if it had been created by the statement
/// `coverpoint V;`", once for each variable, in the order the crosses
/// first name them. Item `k + 1` of the type is coverpoint `k`; the crosses
/// follow them.
pub(in crate::ast) struct Layout<'x> {
    pub declared: Vec<&'x Coverpoint>,
    /// The variable of each implicit coverpoint, which is its name, with the
    /// cross item that first names it.
    pub implicit: Vec<(String, ExprId)>,
    /// The names of the coverpoints, declared and implicit: those that
    /// reports alone show are `None` (19.5).
    pub names: Vec<Option<String>>,
    /// The crosses with the coverpoints of their items, by index.
    pub crosses: Vec<(&'x Cross, Vec<u32>)>,
}

impl<'x> Layout<'x> {
    pub(in crate::ast) fn of(
        native: &Declarations,
        declaration: &'x CovergroupDeclaration,
    ) -> Self {
        let declared: Vec<&Coverpoint> = coverpoints(declaration).collect();
        let mut names: Vec<Option<String>> = declared
            .iter()
            .enumerate()
            .map(|(index, coverpoint)| {
                let (name, generated) = point_name(native, coverpoint, index + 1);
                (!generated).then_some(name)
            })
            .collect();
        let mut implicit = Vec::new();
        let mut crossed = Vec::new();
        for cross in crosses(declaration) {
            let mut points = Vec::new();
            for &item in &cross.items {
                let name = cross_item_name(native, item);
                let index = match names
                    .iter()
                    .position(|known| known.as_deref() == Some(name.as_str()))
                {
                    Some(index) => index,
                    None => {
                        names.push(Some(name.clone()));
                        implicit.push((name, item));
                        names.len() - 1
                    }
                };
                points.push(index as u32);
            }
            crossed.push((cross, points));
        }
        Self {
            declared,
            implicit,
            names,
            crosses: crossed,
        }
    }

    /// The index of the coverpoint named `name`, if one is.
    pub(in crate::ast) fn point(&self, name: &str) -> Option<usize> {
        self.names
            .iter()
            .position(|known| known.as_deref() == Some(name))
    }
}

/// A select expression of a cross bin (19.6.1) without its parentheses,
/// with the chains of `&&` and of `||` each as one node, as
/// `celox_heap::coverage::Select` has them.
pub(in crate::ast) enum SelectTree<'x> {
    /// A `binsof` condition, or a cross identifier or cross set expression.
    Leaf(&'x SelectKind),
    And(Vec<SelectTree<'x>>),
    Or(Vec<SelectTree<'x>>),
}

/// The select expression `id` as a [`SelectTree`]. An error is what the
/// analysis does not support: a `with` selection and a `matches` count
/// (19.6.1.2), and `&&` and `||` that alternate more than
/// `celox_heap::coverage::MAX_SELECT_DEPTH` times.
pub(in crate::ast) fn select_tree(
    native: &Declarations,
    id: SelectId,
) -> Result<SelectTree<'_>, AnalyzerError> {
    select_node(native, id, 0)
}

/// The text of the rejection of a `with` selection or a `matches` count.
const WITH_OR_MATCHES: &str =
    "covergroup cross with or matches selection (IEEE 1800-2023 19.6.1.2)";

fn select_node(
    native: &Declarations,
    mut id: SelectId,
    depth: usize,
) -> Result<SelectTree<'_>, AnalyzerError> {
    // Parentheses group and mean nothing else.
    while let SelectKind::Group(inner) = &native.coverage_select(id).kind {
        id = *inner;
    }
    let kind = &native.coverage_select(id).kind;
    let and = match kind {
        SelectKind::And { .. } => true,
        SelectKind::Or { .. } => false,
        SelectKind::With { .. }
        | SelectKind::Expression {
            matches: Some(_), ..
        } => return Err(unsupported(WITH_OR_MATCHES)),
        _ => return Ok(SelectTree::Leaf(kind)),
    };
    if depth >= celox_heap::coverage::MAX_SELECT_DEPTH {
        return Err(unsupported(format!(
            "covergroup cross selection nested deeper than {} (IEEE 1800-2023 19.6.1)",
            celox_heap::coverage::MAX_SELECT_DEPTH
        )));
    }
    // The operands of the chain, the leftmost first.
    let mut operands = Vec::new();
    let mut pending = vec![id];
    while let Some(mut next) = pending.pop() {
        while let SelectKind::Group(inner) = &native.coverage_select(next).kind {
            next = *inner;
        }
        match &native.coverage_select(next).kind {
            SelectKind::And { left, right } if and => pending.extend([*right, *left]),
            SelectKind::Or { left, right } if !and => pending.extend([*right, *left]),
            _ => operands.push(select_node(native, next, depth + 1)?),
        }
    }
    Ok(if and {
        SelectTree::And(operands)
    } else {
        SelectTree::Or(operands)
    })
}

/// The name `item` that a `with` expression gives the value it is asked
/// about (IEEE 1800-2023 19.5.1.1).
pub(in crate::ast) const ITEM: &str = "item";

/// The `with` expression of bin declaration `bins`, if it has one
/// (19.5.1.1).
pub(in crate::ast) fn filter(bins: &Bins) -> Option<ExprId> {
    match &bins.value {
        BinsValue::Ranges { with, .. } => *with,
        BinsValue::CoverpointWith { with, .. } => Some(*with),
        _ => None,
    }
}

/// The expressions of bin declaration `bins` that a construction evaluates
/// (19.5.1, 19.5.1.1, 19.5.1.2): the size of the array, the values and
/// bounds of the range list, the `with` expression and the set expression.
/// The guard of the bin is none of them: a sample evaluates it.
pub(in crate::ast) fn construction_operands(bins: &Bins) -> Vec<ExprId> {
    let mut operands = Vec::new();
    if let Some(BinsArray::Sized(size)) = &bins.array {
        operands.push(*size);
    }
    match &bins.value {
        BinsValue::Ranges { ranges, with } => {
            operands.extend(range_operands(ranges));
            operands.extend(with);
        }
        BinsValue::CoverpointWith { with, .. } => operands.push(*with),
        BinsValue::Set(set) => operands.push(*set),
        BinsValue::Transitions(_) | BinsValue::DefaultSequence | BinsValue::Default => {}
    }
    operands
}

/// The operands of the elements of a range list (19.5.1, 19.6.1).
pub(in crate::ast) fn range_operands(ranges: &[CoverageRange]) -> Vec<ExprId> {
    let mut operands = Vec::new();
    for range in ranges {
        match &range.kind {
            CoverageRangeKind::Value(value) => operands.push(*value),
            CoverageRangeKind::Range { low, high } => operands.extend(low.iter().chain(high)),
            CoverageRangeKind::Tolerance {
                center, tolerance, ..
            } => operands.extend([*center, *tolerance]),
        }
    }
    operands
}

/// The operands of the `intersect` lists of select expression `select`
/// (19.6.1), which a construction evaluates.
fn intersect_operands(native: &Declarations, select: SelectId) -> Vec<ExprId> {
    let mut operands = Vec::new();
    let mut pending = vec![select];
    while let Some(id) = pending.pop() {
        match &native.coverage_select(id).kind {
            SelectKind::Binsof {
                intersect: Some(ranges),
                ..
            } => operands.extend(range_operands(ranges)),
            SelectKind::Binsof { .. } | SelectKind::Expression { .. } => {}
            SelectKind::And { left, right } | SelectKind::Or { left, right } => {
                pending.extend([*left, *right]);
            }
            SelectKind::Group(inner) => pending.push(*inner),
            SelectKind::With { select, .. } => pending.push(*select),
        }
    }
    operands
}

/// The expressions and the types after the header of `declaration` that the
/// analysis binds: the coverpoint expressions with their guards and types,
/// which a sample evaluates, and the option values and bin operands, which
/// a construction evaluates.
#[derive(Debug, Default)]
pub(crate) struct Bound {
    /// Evaluated at each sample: these read the arguments of the covergroup
    /// and of its sample method (19.8.1).
    pub sampled: Vec<ExprId>,
    /// Evaluated at `new`: these read the arguments of the covergroup.
    pub constructed: Vec<ExprId>,
    /// Those of `constructed` that are the `with` expressions of bins
    /// (19.5.1.1), in which `item` names the value the filter is asked
    /// about and nothing of the scope.
    pub filters: Vec<ExprId>,
    pub types: Vec<TypeId>,
}

impl Bound {
    pub(crate) fn of(native: &Declarations, declaration: &CovergroupDeclaration) -> Self {
        let mut bound = Self::default();
        // The variables of the implicit coverpoints (19.6).
        let layout = Layout::of(native, declaration);
        bound
            .sampled
            .extend(layout.implicit.iter().map(|(_, item)| *item));
        for item in &declaration.items {
            match item {
                CoverageItem::Option(option) => bound.constructed.push(option.value),
                CoverageItem::Coverpoint(coverpoint) => {
                    bound.sampled.push(coverpoint.expression);
                    bound.sampled.extend(coverpoint.iff);
                    bound.types.extend(coverpoint.data_type);
                    for item in &coverpoint.items {
                        match item {
                            CoverpointItem::Option(option) => bound.constructed.push(option.value),
                            CoverpointItem::Bins(bins) => {
                                bound.constructed.extend(construction_operands(bins));
                                bound.filters.extend(filter(bins));
                                bound.sampled.extend(bins.iff);
                            }
                        }
                    }
                }
                CoverageItem::Cross(cross) => {
                    bound.sampled.extend(cross.iff);
                    for item in &cross.body_items {
                        match item {
                            CrossItem::Option(option) => bound.constructed.push(option.value),
                            CrossItem::Bins(bins) => {
                                bound.sampled.extend(bins.iff);
                                bound
                                    .constructed
                                    .extend(intersect_operands(native, bins.select));
                            }
                            CrossItem::Function(_) => {}
                        }
                    }
                }
            }
        }
        bound
    }

    /// Whether the expression at `tokens` is `name` as the value a `with`
    /// expression is asked about: `item` in one of them (19.5.1.1).
    pub(crate) fn names_item(&self, native: &Declarations, name: &str, tokens: TokenRange) -> bool {
        name == ITEM
            && self.filters.iter().any(|filter| {
                let filter = native.expression(*filter).tokens;
                filter.start <= tokens.start && tokens.start < filter.end
            })
    }

    /// The token ranges of the expressions and types.
    pub(crate) fn ranges(&self, native: &Declarations) -> Vec<TokenRange> {
        self.sampled
            .iter()
            .chain(&self.constructed)
            .map(|id| native.expression(*id).tokens)
            .chain(self.types.iter().map(|id| native.data_type(*id).tokens))
            .collect()
    }
}

/// The names expression `root` uses as variables, each with its node: every
/// name in it but the function a call names, the key of an assignment
/// pattern item and the scope of `scope::name`.
pub(in crate::ast) fn variable_names(native: &Declarations, root: ExprId) -> Vec<(String, ExprId)> {
    let mut names = Vec::new();
    let mut pending = vec![root];
    while let Some(id) = pending.pop() {
        let kind = &native.expression(id).kind;
        // The nodes of this one that name no variable.
        let mut skipped: Vec<ExprId> = Vec::new();
        match kind {
            ExprKind::Name(name) => {
                names.push((instances::normalize_identifier(&name.spelling), id));
            }
            ExprKind::Call { expression, .. } | ExprKind::Parameterized { expression, .. } => {
                skipped.push(*expression);
            }
            ExprKind::Member {
                expression,
                separator,
                ..
            } if separator == "::" => skipped.push(*expression),
            ExprKind::Pattern { items, .. } => {
                skipped.extend(items.iter().filter_map(|item| item.key));
            }
            _ => {}
        }
        kind.map_expressions(|child| {
            if !skipped.contains(&child)
                || !matches!(native.expression(child).kind, ExprKind::Name(_))
            {
                pending.push(child);
            }
            child
        });
    }
    names
}

/// What the items of covergroup `declaration`, shown as `display`, are to
/// the analysis. An error is what the standard makes illegal: an option
/// member that does not exist, that its level has not or that one
/// definition assigns twice (19.7, 19.10), two items of one name and two
/// bins of one name in a coverpoint (19.5),
/// and an argument of the sample method in an option value or a bin
/// (19.8.1), whose names are `sample_formals`. `Ok(Some(_))` is the
/// rejection of the first item the analysis does not support, which the
/// uses of the type report.
pub(super) fn check(
    native: &Declarations,
    declaration: &CovergroupDeclaration,
    display: &str,
    sample_formals: &[String],
) -> Converted<Option<AnalyzerError>> {
    if matches!(declaration.kind, CovergroupKind::Extends) {
        return Ok(None);
    }
    let mut rejection: Option<AnalyzerError> = None;
    let mut reject = |error: AnalyzerError| {
        rejection.get_or_insert(error);
    };
    // The members a definition has assigned, of `option` and `type_option`.
    let assigned = |seen: &mut Vec<(bool, String)>,
                    option: &CoverageOption,
                    level: Level,
                    place: &str|
     -> Converted<Option<AnalyzerError>> {
        let compiled = option_member(option, level, display)?;
        let member = instances::normalize_identifier(&option.member.spelling);
        let key = (option.type_option, member);
        if seen.contains(&key) {
            return Err(invalid(format!(
                "`{}.{}` is assigned twice in {place} (IEEE 1800-2023 19.7)",
                if key.0 { "type_option" } else { "option" },
                key.1
            )));
        }
        seen.push(key);
        Ok(compiled.err())
    };
    // In a `with` expression (`filter`), `item` is the value the filter is
    // asked about (19.5.1.1), not an argument of that name.
    let no_sample_formal = |root: ExprId, what: &str, in_filter: bool| -> Converted<()> {
        if sample_formals.is_empty() {
            return Ok(());
        }
        match variable_names(native, root)
            .into_iter()
            .find(|(name, _)| sample_formals.contains(name) && !(in_filter && name == ITEM))
        {
            Some((name, _)) => Err(invalid(format!(
                "argument `{name}` of the sample method of covergroup `{display}` is used in {what}: it is visible in coverpoint expressions and their guards only (IEEE 1800-2023 19.8.1)"
            ))),
            None => Ok(()),
        }
    };
    let mut group_options = Vec::new();
    let mut names: Vec<String> = Vec::new();
    let mut points = 0;
    let layout = Layout::of(native, declaration);
    let cross_labels: Vec<String> = crosses(declaration)
        .filter_map(|cross| cross.label.as_ref())
        .map(|label| instances::normalize_identifier(&label.spelling))
        .collect();
    let mut crossed = 0;
    for item in &declaration.items {
        match item {
            CoverageItem::Option(option) => {
                let place = format!("covergroup `{display}`");
                if let Some(error) =
                    assigned(&mut group_options, option, Level::Covergroup, &place)?
                {
                    reject(error);
                }
                no_sample_formal(option.value, "an option value", false)?;
            }
            CoverageItem::Coverpoint(coverpoint) => {
                points += 1;
                let (name, generated) = point_name(native, coverpoint, points);
                if !generated {
                    if names.contains(&name) {
                        return Err(invalid(format!(
                            "covergroup `{display}` has two items named `{name}` (IEEE 1800-2023 19.5)"
                        )));
                    }
                    names.push(name.clone());
                }
                let place = format!("coverpoint `{name}` of covergroup `{display}`");
                let mut options = Vec::new();
                // A coverpoint is a scope (19.5), which its bins are named
                // in.
                let mut bin_names: Vec<String> = Vec::new();
                for item in &coverpoint.items {
                    match item {
                        CoverpointItem::Option(option) => {
                            if let Some(error) =
                                assigned(&mut options, option, Level::Coverpoint, &place)?
                            {
                                reject(error);
                            }
                            no_sample_formal(option.value, "an option value", false)?;
                        }
                        CoverpointItem::Bins(bins) => {
                            let bin = instances::normalize_identifier(&bins.name.spelling);
                            if bin_names.contains(&bin) {
                                return Err(invalid(format!(
                                    "{place} has two bins named `{bin}` (IEEE 1800-2023 19.5)"
                                )));
                            }
                            bin_names.push(bin);
                            let with = filter(bins);
                            for operand in construction_operands(bins) {
                                no_sample_formal(operand, "a bin", Some(operand) == with)?;
                            }
                            check_bins(bins, &name, &place)?;
                            if let Some(construct) = unsupported_bins(bins) {
                                reject(unsupported(construct));
                            }
                        }
                    }
                }
            }
            CoverageItem::Cross(cross) => {
                crossed += 1;
                let name = cross_name(cross, crossed);
                if cross.label.is_some() {
                    if names.contains(&name) {
                        return Err(invalid(format!(
                            "covergroup `{display}` has two items named `{name}` (IEEE 1800-2023 19.5)"
                        )));
                    }
                    names.push(name.clone());
                }
                let place = format!("cross `{name}` of covergroup `{display}`");
                let points = &layout.crosses[crossed - 1].1;
                for (position, &item) in cross.items.iter().enumerate() {
                    let item = cross_item_name(native, item);
                    // A cross name is visible only after a covergroup
                    // variable or `::` (19.6), so `item` is a variable, whose
                    // implicit coverpoint is an item of the name of a cross.
                    if cross_labels.contains(&item) {
                        return Err(invalid(format!(
                            "covergroup `{display}` has two items named `{item}`: the coverpoint of the variable that {place} crosses and a cross (IEEE 1800-2023 19.5, 19.6)"
                        )));
                    }
                    if points[..position].contains(&points[position]) {
                        reject(unsupported(
                            "covergroup cross that lists an item twice (IEEE 1800-2023 19.6)",
                        ));
                    }
                }
                let mut options = Vec::new();
                // The label of a cross "creates a hierarchical scope for the
                // bins defined within the cross" (19.6).
                let mut bin_names: Vec<String> = Vec::new();
                for item in &cross.body_items {
                    match item {
                        CrossItem::Option(option) => {
                            if let Some(error) =
                                assigned(&mut options, option, Level::Cross, &place)?
                            {
                                reject(error);
                            }
                            no_sample_formal(option.value, "an option value", false)?;
                        }
                        CrossItem::Function(_) => reject(unsupported(
                            "covergroup function declared in a cross body (IEEE 1800-2023 19.6, 19.6.1.4)",
                        )),
                        CrossItem::Bins(bins) => {
                            let bin = instances::normalize_identifier(&bins.name.spelling);
                            if bin_names.contains(&bin) {
                                return Err(invalid(format!(
                                    "{place} has two bins named `{bin}` (IEEE 1800-2023 19.6)"
                                )));
                            }
                            bin_names.push(bin);
                            for operand in intersect_operands(native, bins.select) {
                                no_sample_formal(operand, "a cross bin", false)?;
                            }
                            if let Some(error) = check_select(
                                native,
                                &layout,
                                cross,
                                points,
                                bins,
                                &cross_labels,
                                &place,
                            )? {
                                reject(error);
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(rejection)
}

/// What the standard excludes of the select expression of bin declaration
/// `bins` of `cross`, which crosses the coverpoints `points` of `layout`
/// and which `place` names, as the declaration alone shows: a `binsof` of
/// what is no coverpoint, of a bin that its coverpoint does not declare
/// (19.6.1), and the identifier of another cross (19.6.1.2), the labels of
/// the crosses of the covergroup being `labels`. `Ok(Some(_))` is the
/// rejection of what the analysis does not support.
fn check_select(
    native: &Declarations,
    layout: &Layout<'_>,
    cross: &Cross,
    points: &[u32],
    bins: &CrossBins,
    labels: &[String],
    place: &str,
) -> Converted<Option<AnalyzerError>> {
    let bin = instances::normalize_identifier(&bins.name.spelling);
    let tree = match select_tree(native, bins.select) {
        Ok(tree) => tree,
        Err(error) => return Ok(Some(error)),
    };
    let mut pending = vec![&tree];
    while let Some(tree) = pending.pop() {
        let kind = match tree {
            SelectTree::And(parts) | SelectTree::Or(parts) => {
                pending.extend(parts);
                continue;
            }
            SelectTree::Leaf(kind) => kind,
        };
        match kind {
            SelectKind::Binsof {
                target, bin: of, ..
            } => {
                let target = instances::normalize_identifier(&target.spelling);
                // "The binsof construct yields the bins of its expression,
                // which can be either a coverage point (explicitly defined
                // or implicitly defined for a single variable) or a
                // coverage point bin" (19.6.1).
                let Some(point) = layout.point(&target) else {
                    return Err(invalid(format!(
                        "bin `{bin}` of {place} selects the bins of `{target}`, which is no coverpoint of the covergroup and no variable that it crosses (IEEE 1800-2023 19.6.1)"
                    )));
                };
                if let Some(of) = of {
                    let of = instances::normalize_identifier(&of.spelling);
                    let declares = layout.declared.get(point).is_some_and(|coverpoint| {
                        coverpoint.items.iter().any(|item| {
                            matches!(item, CoverpointItem::Bins(declared)
                                if instances::normalize_identifier(&declared.name.spelling) == of)
                        })
                    });
                    if !declares {
                        return Err(invalid(format!(
                            "bin `{bin}` of {place} selects `{target}.{of}`, which is no bin that coverpoint `{target}` declares (IEEE 1800-2023 19.6.1)"
                        )));
                    }
                }
                if !points.contains(&(point as u32)) {
                    return Ok(Some(unsupported(
                        "covergroup cross selection of the bins of a coverpoint that the cross does not cross (IEEE 1800-2023 19.6.1)",
                    )));
                }
            }
            SelectKind::Expression { value, .. } => {
                let named = match &native.expression(*value).kind {
                    ExprKind::Name(name) => Some(instances::normalize_identifier(&name.spelling)),
                    _ => None,
                };
                let own = cross
                    .label
                    .as_ref()
                    .map(|label| instances::normalize_identifier(&label.spelling));
                match named {
                    // "When a cross_identifier is used as a
                    // select_expression, it selects all possible bin
                    // tuples" (19.6.1.2).
                    Some(named) if Some(&named) == own.as_ref() => {}
                    Some(named) if labels.contains(&named) => {
                        return Err(invalid(format!(
                            "bin `{bin}` of {place} selects the bin tuples of cross `{named}`: only the cross_identifier of the enclosing cross may be used (IEEE 1800-2023 19.6.1.2)"
                        )));
                    }
                    _ => {
                        return Ok(Some(unsupported(
                            "covergroup cross set expression (IEEE 1800-2023 19.6.1.3, 19.6.1.4)",
                        )));
                    }
                }
            }
            _ => {}
        }
    }
    Ok(None)
}

/// What the standard excludes of bin declaration `bins` of the coverpoint
/// named `point`, which `place` names, as the declaration alone shows: an
/// ignored `default` bin (19.5), and the name of another coverpoint before
/// `with` (19.5.1.1).
fn check_bins(bins: &Bins, point: &str, place: &str) -> Converted<()> {
    let name = instances::normalize_identifier(&bins.name.spelling);
    match &bins.value {
        BinsValue::Default | BinsValue::DefaultSequence if bins.kind == BinsKind::IgnoreBins => {
            Err(invalid(format!(
                "`ignore_bins {name}` of {place} is a default bin, which cannot be ignored (IEEE 1800-2023 19.5)"
            )))
        }
        BinsValue::CoverpointWith { coverpoint, .. }
            if instances::normalize_identifier(&coverpoint.spelling) != point =>
        {
            Err(invalid(format!(
                "bin `{name}` of {place} takes the values of `{}`: only the name of its own coverpoint stands for all its values (IEEE 1800-2023 19.5.1.1)",
                instances::normalize_identifier(&coverpoint.spelling)
            )))
        }
        _ => Ok(()),
    }
}

/// What of bin declaration `bins` the analysis does not support: bins of
/// transitions, and a `default` bin array with a size, whose values the
/// standard does not distribute.
fn unsupported_bins(bins: &Bins) -> Option<&'static str> {
    match &bins.value {
        BinsValue::Transitions(_) | BinsValue::DefaultSequence => {
            Some("covergroup transition bins (IEEE 1800-2023 19.5.2)")
        }
        BinsValue::Default if matches!(bins.array, Some(BinsArray::Sized(_))) => {
            Some("covergroup default bin array with a size (IEEE 1800-2023 19.5)")
        }
        _ => None,
    }
}
