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
use celox_heap::coverage::OptionId;
use celox_sv_syntax::{
    BinsKind, BinsValue, CoverageItem, CoverageOption, CoverageRangeKind, CovergroupDeclaration,
    CovergroupKind, Coverpoint, CoverpointItem, ExprId, ExprKind, TokenRange, TypeId,
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
}

impl Level {
    fn noun(self) -> &'static str {
        match self {
            Self::Covergroup => "a covergroup",
            Self::Coverpoint => "a coverpoint",
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

/// A member of `option` (Table 19-1): its name, whether a covergroup and a
/// coverpoint have it (Table 19-2), and what a definition that assigns it
/// compiles to, if the analysis supports the member.
const OPTIONS: [(&str, bool, bool, Option<(OptionId, OptionType)>); 11] = [
    (
        "name",
        true,
        false,
        Some((OptionId::Name, OptionType::Text)),
    ),
    (
        "weight",
        true,
        true,
        Some((OptionId::Weight, OptionType::Int)),
    ),
    ("goal", true, true, Some((OptionId::Goal, OptionType::Int))),
    (
        "comment",
        true,
        true,
        Some((OptionId::Comment, OptionType::Text)),
    ),
    (
        "at_least",
        true,
        true,
        Some((OptionId::AtLeast, OptionType::Int)),
    ),
    (
        "auto_bin_max",
        true,
        true,
        Some((OptionId::AutoBinMax, OptionType::Int)),
    ),
    ("cross_num_print_missing", true, false, None),
    ("cross_retain_auto_bins", true, false, None),
    ("detect_overlap", true, true, None),
    (
        "per_instance",
        true,
        false,
        Some((OptionId::PerInstance, OptionType::Bit)),
    ),
    ("get_inst_coverage", true, false, None),
];

/// A member of `type_option` (Table 19-3): its name and whether a
/// covergroup and a coverpoint have it (19.10).
const TYPE_OPTIONS: [(&str, bool, bool); 7] = [
    ("weight", true, true),
    ("goal", true, true),
    ("comment", true, true),
    ("strobe", true, false),
    ("merge_instances", true, false),
    ("distribute_first", true, false),
    ("real_interval", true, true),
];

/// What the definition-level assignment `option` at `level` compiles to:
/// the member of the heap and its type. An error is what the standard
/// excludes, a member that does not exist or that the level has not
/// (19.7, 19.10); `Ok(Err(_))` is the rejection of a member, or of a
/// `type_option`, that the analysis does not support.
pub(in crate::ast) fn option_member(
    option: &CoverageOption,
    level: Level,
    display: &str,
) -> Converted<Result<(OptionId, OptionType), AnalyzerError>> {
    let member = instances::normalize_identifier(&option.member.spelling);
    let structure = if option.type_option {
        "type_option"
    } else {
        "option"
    };
    let found = if option.type_option {
        TYPE_OPTIONS
            .iter()
            .find(|(name, ..)| *name == member)
            .map(|&(_, covergroup, coverpoint)| (covergroup, coverpoint, None))
    } else {
        OPTIONS
            .iter()
            .find(|(name, ..)| *name == member)
            .map(|&(_, covergroup, coverpoint, compiled)| (covergroup, coverpoint, compiled))
    };
    let Some((covergroup, coverpoint, compiled)) = found else {
        return Err(invalid(format!(
            "`{structure}` has no member `{member}`, which covergroup `{display}` assigns (IEEE 1800-2023 19.10)"
        )));
    };
    let has = match level {
        Level::Covergroup => covergroup,
        Level::Coverpoint => coverpoint,
    };
    if !has {
        return Err(invalid(format!(
            "`{structure}.{member}` is assigned in {} of covergroup `{display}`, which has no such member (IEEE 1800-2023 19.10)",
            level.noun()
        )));
    }
    Ok(match compiled {
        Some(compiled) if !option.type_option => Ok(compiled),
        _ if option.type_option => Err(unsupported(
            "covergroup type_option (IEEE 1800-2023 19.7.1)",
        )),
        _ => Err(unsupported(format!(
            "covergroup option `{member}` (IEEE 1800-2023 19.7)"
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

/// The operand expressions of the range list of bin value `value`: those of
/// `bins b = { ... }` without a filter, which is what the analysis compiles.
pub(in crate::ast) fn range_operands(value: &BinsValue) -> Vec<ExprId> {
    let BinsValue::Ranges { ranges, with: None } = value else {
        return Vec::new();
    };
    ranges
        .iter()
        .flat_map(|range| match &range.kind {
            CoverageRangeKind::Value(value) => vec![*value],
            CoverageRangeKind::Range { low, high } => low.iter().chain(high).copied().collect(),
            CoverageRangeKind::Tolerance { .. } => Vec::new(),
        })
        .collect()
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
    pub types: Vec<TypeId>,
}

impl Bound {
    pub(crate) fn of(declaration: &CovergroupDeclaration) -> Self {
        let mut bound = Self::default();
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
                                bound.constructed.extend(range_operands(&bins.value));
                            }
                        }
                    }
                }
                CoverageItem::Cross(_) => {}
            }
        }
        bound
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
/// definition assigns twice (19.7, 19.10), two items of one name (19.5),
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
    let no_sample_formal = |root: ExprId, what: &str| -> Converted<()> {
        if sample_formals.is_empty() {
            return Ok(());
        }
        match variable_names(native, root)
            .into_iter()
            .find(|(name, _)| sample_formals.contains(name))
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
    for item in &declaration.items {
        match item {
            CoverageItem::Option(option) => {
                let place = format!("covergroup `{display}`");
                if let Some(error) =
                    assigned(&mut group_options, option, Level::Covergroup, &place)?
                {
                    reject(error);
                }
                no_sample_formal(option.value, "an option value")?;
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
                for item in &coverpoint.items {
                    match item {
                        CoverpointItem::Option(option) => {
                            if let Some(error) =
                                assigned(&mut options, option, Level::Coverpoint, &place)?
                            {
                                reject(error);
                            }
                            no_sample_formal(option.value, "an option value")?;
                        }
                        CoverpointItem::Bins(bins) => {
                            for operand in range_operands(&bins.value) {
                                no_sample_formal(operand, "a bin")?;
                            }
                            if let Some(construct) = unsupported_bins(bins) {
                                reject(unsupported(construct));
                            }
                        }
                    }
                }
            }
            CoverageItem::Cross(cross) => {
                if let Some(label) = &cross.label {
                    let name = instances::normalize_identifier(&label.spelling);
                    if names.contains(&name) {
                        return Err(invalid(format!(
                            "covergroup `{display}` has two items named `{name}` (IEEE 1800-2023 19.5)"
                        )));
                    }
                    names.push(name);
                }
                reject(unsupported("covergroup cross (IEEE 1800-2023 19.6)"));
            }
        }
    }
    Ok(rejection)
}

/// What of bin declaration `bins` the analysis does not support: anything
/// but `bins name = { values and ranges }`.
fn unsupported_bins(bins: &celox_sv_syntax::Bins) -> Option<&'static str> {
    if bins.wildcard {
        return Some("covergroup wildcard bins (IEEE 1800-2023 19.5.4)");
    }
    match bins.kind {
        BinsKind::Bins => {}
        BinsKind::IgnoreBins => return Some("covergroup ignore_bins (IEEE 1800-2023 19.5.5)"),
        BinsKind::IllegalBins => return Some("covergroup illegal_bins (IEEE 1800-2023 19.5.6)"),
    }
    if bins.array.is_some() {
        return Some("covergroup bin array (IEEE 1800-2023 19.5.1)");
    }
    match &bins.value {
        BinsValue::Ranges { ranges, with } => {
            if with.is_some() {
                return Some("covergroup bins with a filter (IEEE 1800-2023 19.5.1.1)");
            }
            if ranges
                .iter()
                .any(|range| matches!(range.kind, CoverageRangeKind::Tolerance { .. }))
            {
                return Some(
                    "covergroup bin range with a tolerance (IEEE 1800-2023 19.5.1, 11.4.13)",
                );
            }
        }
        BinsValue::CoverpointWith { .. } => {
            return Some("covergroup bins with a filter (IEEE 1800-2023 19.5.1.1)");
        }
        BinsValue::Set(_) => {
            return Some("covergroup bins of a set expression (IEEE 1800-2023 19.5.1.2)");
        }
        BinsValue::Transitions(_) | BinsValue::DefaultSequence => {
            return Some("covergroup transition bins (IEEE 1800-2023 19.5.2)");
        }
        BinsValue::Default => return Some("covergroup default bins (IEEE 1800-2023 19.5)"),
    }
    if bins.iff.is_some() {
        return Some("covergroup bins with a guard (IEEE 1800-2023 19.5.1)");
    }
    None
}
