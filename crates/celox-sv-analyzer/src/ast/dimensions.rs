//! Packed/unpacked dimension metadata and size-query type discovery.

use super::*;
mod discovery;
pub(super) use discovery::State;
pub(super) use discovery::all_class_signatures;
mod query_binding;
pub(super) use discovery::query_operand_dimensions_id;
mod queries;
pub(super) use query_binding::{
    query_context_for_operand, query_value, shape as operand_shape, validate_data_operand,
};

/// Imported interface member shapes remain visible during native discovery.
fn extend_interface_dimensions(state: &AnalysisState, dimensions: &mut PackedDimensions) {
    let imported = scope::imported(state);
    if imported.interface_members.is_empty() {
        return;
    }
    for (name, ty) in imported
        .ports
        .iter()
        .map(|port| (port.name(), port.r#type()))
        .chain(
            imported
                .signals
                .iter()
                .map(|signal| (signal.name(), signal.r#type())),
        )
    {
        if dimensions.get(name).is_none() {
            dimensions.insert(name.to_string(), procedural::dimensions_from_type(ty));
        }
    }
}

pub(super) fn bound_bits_type(
    state: &AnalysisState,
    expression: &Expr,
    dimensions: &PackedDimensions,
) -> Option<ExprType> {
    bound_query_type(
        expression,
        expr_static_width(state, expression, dimensions)?,
        dimensions,
        &dimensions.const_env,
    )
}

fn bound_query_type(
    expression: &Expr,
    width: usize,
    packed_dimensions: &PackedDimensions,
    const_env: &HashMap<String, i128>,
) -> Option<ExprType> {
    let signed = expression_signedness(expression, packed_dimensions, const_env)?;
    Some(ExprType {
        width: width.max(1),
        signed,
    })
}

/// Resolve signedness recursively from lexical declarations and parameter types.
pub(super) fn expression_signedness(
    expression: &Expr,
    dimensions: &PackedDimensions,
    const_env: &HashMap<String, i128>,
) -> Option<bool> {
    expr_signedness_with_return_types(
        expression,
        &SizeQuerySignedness {
            dimensions,
            const_env,
        },
        &dimensions.functions,
        &dimensions.function_return_types,
    )
}

/// The same precedence as the former eagerly collected signedness table,
/// looking up only the identifiers used by the query operand.
struct SizeQuerySignedness<'a> {
    dimensions: &'a PackedDimensions,
    const_env: &'a HashMap<String, i128>,
}

impl Signedness for SizeQuerySignedness<'_> {
    fn signedness(&self, name: &str) -> Option<bool> {
        if let Some(variable) = self.dimensions.get(name) {
            return Some(variable.signed);
        }
        if let Some(signed) = self.const_env.get(&variable_signed_marker(name)) {
            return Some(*signed != 0);
        }
        parameter_type_from_const_env(self.const_env, name)
            .map(|ty| ty.signed)
            .or_else(|| self.const_env.contains_key(name).then_some(true))
    }
}

/// Locals shadow every constant projection of the enclosing binding. Retain
/// their declared shapes while checking unevaluated output/inout actuals.
pub(super) fn extend_query_local_dimensions(
    context: &mut PackedDimensions,
    locals: impl IntoIterator<Item = (String, VariableDimensions)>,
) {
    for (name, ty) in locals {
        for key in parameters::binding_keys(&name, &context.const_env, ty.packed.len()) {
            context.const_env.remove(&key);
        }
        context.parameter_values.remove(&name);
        context.const_env.insert(
            variable_dimensions_marker(&name),
            (ty.unpacked.len() + ty.packed.len()) as i128,
        );
        context.const_env.insert(
            variable_unpacked_dimensions_marker(&name),
            ty.unpacked.len() as i128,
        );
        context.insert(name, ty);
    }
}

pub(super) fn packed_dimensions_from_ports_and_signals(
    ports: &[Port],
    signals: &[Signal],
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
) -> PackedDimensions {
    let mut dimensions = HashMap::default();
    for port in ports {
        dimensions.insert(
            port.name().to_string(),
            VariableDimensions {
                container: port.r#type().container.clone(),
                randomizable: port.r#type().container.is_none()
                    && (!port.r#type().is_enum || port.r#type().enum_values.is_some())
                    && port.randomizable,
                enum_values: port.r#type().enum_values.clone(),
                real: matches!(port.r#type().kind(), TypeKind::Real | TypeKind::Shortreal),
                string: matches!(port.r#type().kind(), TypeKind::String),
                class: port.r#type().class().map(str::to_owned),
                packed: signal_packed_dimension_widths(port.r#type().packed_ranges()),
                unpacked: unpacked_dimension_widths(port.r#type().unpacked_ranges()),
                signed: port.r#type().is_signed(),
                is_2state: matches!(
                    port.r#type().kind(),
                    TypeKind::Bit
                        | TypeKind::Real
                        | TypeKind::Shortreal
                        | TypeKind::String
                        | TypeKind::Object
                        | TypeKind::Process
                ),
                members: port.r#type().members.clone(),
                signed_element_depth: port.r#type().signed_element_depth,
            },
        );
    }
    for signal in signals {
        dimensions.insert(
            signal.name().to_string(),
            VariableDimensions {
                container: signal.r#type().container.clone(),
                randomizable: signal.r#type().container.is_none()
                    && (!signal.r#type().is_enum || signal.r#type().enum_values.is_some())
                    && !signal.is_net()
                    && !signal.is_const,
                enum_values: signal.r#type().enum_values.clone(),
                real: matches!(signal.r#type().kind(), TypeKind::Real | TypeKind::Shortreal),
                string: matches!(signal.r#type().kind(), TypeKind::String),
                class: signal.r#type().class().map(str::to_owned),
                packed: signal_packed_dimension_widths(signal.r#type().packed_ranges()),
                unpacked: unpacked_dimension_widths(signal.r#type().unpacked_ranges()),
                signed: signal.r#type().is_signed(),
                is_2state: matches!(
                    signal.r#type().kind(),
                    TypeKind::Bit
                        | TypeKind::Real
                        | TypeKind::Shortreal
                        | TypeKind::String
                        | TypeKind::Object
                        | TypeKind::Process
                ),
                members: signal.r#type().members.clone(),
                signed_element_depth: signal.r#type().signed_element_depth,
            },
        );
    }
    PackedDimensions::new(dimensions, const_env, type_aliases)
}

pub(super) fn signal_packed_dimension_widths(ranges: &[PackedRange]) -> Vec<PackedDimension> {
    ranges
        .iter()
        .map(|range| {
            let left = range.left().clone();
            let right = range.right().clone();
            let width = |high: ConstExpr, low: ConstExpr| ConstExpr::Binary {
                left: Box::new(ConstExpr::Binary {
                    left: Box::new(high),
                    op: BinaryOp::Sub,
                    right: Box::new(low),
                }),
                op: BinaryOp::Add,
                right: Box::new(ConstExpr::Literal("1".to_string())),
            };
            let width = ConstExpr::Mux {
                condition: Box::new(ConstExpr::Binary {
                    left: Box::new(left.clone()),
                    op: BinaryOp::Ge,
                    right: Box::new(right.clone()),
                }),
                then_expr: Box::new(width(left.clone(), right.clone())),
                else_expr: Box::new(width(right.clone(), left.clone())),
            };
            PackedDimension {
                left,
                right,
                width,
                normalize_single: false,
            }
        })
        .collect()
}

pub(super) fn unpacked_dimension_widths(ranges: &[UnpackedRange]) -> Vec<UnpackedDimension> {
    ranges
        .iter()
        .map(|range| {
            let left = range.left().clone();
            let right = range.right().clone();
            let width = ConstExpr::Mux {
                condition: Box::new(ConstExpr::Binary {
                    left: Box::new(left.clone()),
                    op: BinaryOp::Ge,
                    right: Box::new(right.clone()),
                }),
                then_expr: Box::new(ConstExpr::Binary {
                    left: Box::new(left.clone()),
                    op: BinaryOp::Sub,
                    right: Box::new(right.clone()),
                }),
                else_expr: Box::new(ConstExpr::Binary {
                    left: Box::new(right.clone()),
                    op: BinaryOp::Sub,
                    right: Box::new(left.clone()),
                }),
            };
            UnpackedDimension {
                left,
                right,
                width: add_expr(width, ConstExpr::Literal("1".to_string())),
            }
        })
        .collect()
}

pub(super) fn function_packed_dimension_widths(ranges: &[PackedRange]) -> Vec<PackedDimension> {
    signal_packed_dimension_widths(ranges)
        .into_iter()
        .map(|mut dimension| {
            dimension.normalize_single = true;
            dimension
        })
        .collect()
}

pub(super) fn parameter_marker(name: &str) -> String {
    format!("__parameter::{name}")
}

pub(super) fn local_parameter_marker(name: &str) -> String {
    format!("__parameter::local::{name}")
}

pub(super) fn enum_marker(name: &str) -> String {
    format!("__enum::{name}")
}

pub(super) fn parameter_width_marker(name: &str) -> String {
    format!("__parameter::width::{name}")
}

pub(super) fn parameter_signed_marker(name: &str) -> String {
    format!("__parameter::signed::{name}")
}

pub(super) fn parameter_rank_marker(name: &str) -> String {
    format!("__parameter::rank::{name}")
}

pub(super) fn parameter_dimensions_marker(name: &str) -> String {
    format!("__parameter::dimensions::{name}")
}

pub(super) fn parameter_dimension_marker(name: &str, index: usize, bound: &str) -> String {
    format!("__parameter::dimension::{index}::{bound}::{name}")
}

pub(super) fn parameter_signed_element_marker(name: &str) -> String {
    format!("__parameter::signed_element::{name}")
}

pub(super) fn variable_bits_marker(name: &str) -> String {
    format!("__variable::bits::{name}")
}

pub(super) fn variable_size_marker(name: &str) -> String {
    format!("__variable::size::{name}")
}

pub(super) fn variable_signed_marker(name: &str) -> String {
    format!("__variable::signed::{name}")
}

pub(super) fn variable_dimensions_marker(name: &str) -> String {
    format!("__variable::dimensions::{name}")
}

pub(super) fn variable_unpacked_dimensions_marker(name: &str) -> String {
    format!("__variable::unpacked_dimensions::{name}")
}

pub(super) fn variable_dimension_marker(name: &str, index: usize, bound: &str) -> String {
    format!("__variable::dimension::{index}::{bound}::{name}")
}

/// Preliminary module discovery cannot see unqualified generate-local names.
/// Restore their declared ranges before lowering selections, so an unpacked
/// index removes an array dimension rather than selecting one packed bit.
fn extend_variable_dimensions_from_const_env(
    context: &mut PackedDimensions,
    const_env: &HashMap<String, i128>,
) {
    for (key, count) in const_env {
        let Some(name) = key.strip_prefix("__variable::dimensions::") else {
            continue;
        };
        let Some(unpacked) = const_env.get(&variable_unpacked_dimensions_marker(name)) else {
            continue;
        };
        let ranges = (0..*count)
            .map(|index| {
                let index = usize::try_from(index).ok()?;
                let left = *const_env.get(&variable_dimension_marker(name, index, "left"))?;
                let right = *const_env.get(&variable_dimension_marker(name, index, "right"))?;
                Some(PackedRange::new(
                    ConstExpr::Literal(left.to_string()),
                    ConstExpr::Literal(right.to_string()),
                ))
            })
            .collect::<Option<Vec<_>>>();
        let Some(mut ranges) = ranges else { continue };
        let Ok(unpacked) = usize::try_from(*unpacked) else {
            continue;
        };
        if unpacked > ranges.len() {
            continue;
        }
        let packed_ranges = ranges.split_off(unpacked);
        let ty = Type {
            container: None,
            is_enum: false,
            enum_values: None,
            kind: TypeKind::Logic,
            is_signed: variable_type_is_signed(const_env, name),
            packed_ranges,
            unpacked_ranges: ranges
                .into_iter()
                .map(|range| UnpackedRange::new(range.left, range.right))
                .collect(),
            members: Vec::new(),
            signed_element_depth: None,
            class: None,
            nominal: Nominal::default(),
        };
        let mut shape = procedural::dimensions_from_type(&ty);
        if let Some(existing) = context.get(name) {
            shape.container = existing.container.clone();
            shape.randomizable = existing.randomizable;
            shape.enum_values = existing.enum_values.clone();
            shape.members = existing.members.clone();
            shape.is_2state = existing.is_2state;
            shape.signed_element_depth = existing.signed_element_depth;
        }
        context.insert(name.to_string(), shape);
    }
}

pub(super) fn variable_size_function_width(
    const_env: &HashMap<String, i128>,
    name: &str,
    first_dimension_only: bool,
) -> Option<usize> {
    let marker = if first_dimension_only {
        variable_size_marker(name)
    } else {
        variable_bits_marker(name)
    };
    usize::try_from(*const_env.get(&marker)?).ok()
}

fn variable_type_is_signed(const_env: &HashMap<String, i128>, name: &str) -> bool {
    const_env
        .get(&variable_signed_marker(name))
        .is_some_and(|signed| *signed != 0)
}

pub(super) fn extend_const_env_with_variable_types<'a>(
    state: &AnalysisState,
    const_env: &mut HashMap<String, i128>,
    variables: impl Iterator<Item = (&'a str, &'a Type)>,
) {
    for (name, r#type) in variables {
        const_env.insert(
            variable_dimensions_marker(name),
            (r#type.unpacked_ranges().len() + r#type.packed_ranges().len()) as i128,
        );
        const_env.insert(
            variable_unpacked_dimensions_marker(name),
            r#type.unpacked_ranges().len() as i128,
        );
        for (index, (left, right)) in r#type
            .unpacked_ranges()
            .iter()
            .map(|range| (range.left(), range.right()))
            .chain(
                r#type
                    .packed_ranges()
                    .iter()
                    .map(|range| (range.left(), range.right())),
            )
            .enumerate()
        {
            for (bound, value) in [("left", left), ("right", right)] {
                let key = variable_dimension_marker(name, index, bound);
                if let Some(value) = eval_ast_const_expr(state, value, const_env) {
                    const_env.insert(key, value);
                } else {
                    const_env.remove(&key);
                }
            }
        }
        let dimension_width = |range: &PackedRange| {
            let left = eval_ast_const_expr(state, range.left(), const_env)?;
            let right = eval_ast_const_expr(state, range.right(), const_env)?;
            usize::try_from(left.abs_diff(right)).ok()?.checked_add(1)
        };
        let unpacked_widths = r#type
            .unpacked_ranges()
            .iter()
            .map(|range| {
                let left = eval_ast_const_expr(state, range.left(), const_env)?;
                let right = eval_ast_const_expr(state, range.right(), const_env)?;
                usize::try_from(left.abs_diff(right)).ok()?.checked_add(1)
            })
            .collect::<Option<Vec<_>>>();
        let packed_widths = r#type
            .packed_ranges()
            .iter()
            .map(dimension_width)
            .collect::<Option<Vec<_>>>();
        let (Some(unpacked_widths), Some(packed_widths)) = (unpacked_widths, packed_widths) else {
            continue;
        };
        let bits = unpacked_widths
            .iter()
            .chain(&packed_widths)
            .try_fold(1usize, |width, dimension| width.checked_mul(*dimension));
        let size = unpacked_widths
            .first()
            .or_else(|| packed_widths.first())
            .copied()
            .unwrap_or(1);
        if let Some(bits) = bits.and_then(|bits| i128::try_from(bits).ok()) {
            const_env.insert(variable_bits_marker(name), bits);
        }
        if let Ok(size) = i128::try_from(size) {
            const_env.insert(variable_size_marker(name), size);
        }
        const_env.insert(variable_signed_marker(name), r#type.is_signed() as i128);
    }
}

pub(super) fn insert_parameter_type_markers(
    const_env: &mut HashMap<String, i128>,
    name: &str,
    r#type: ExprType,
) {
    if let Ok(width) = i128::try_from(r#type.width) {
        const_env.insert(parameter_width_marker(name), width);
        const_env.insert(parameter_signed_marker(name), r#type.signed as i128);
    }
}

pub(super) fn parameter_types_from_const_env(
    const_env: &HashMap<String, i128>,
) -> HashMap<String, ExprType> {
    #[cfg(test)]
    PARAMETER_TYPE_SCAN_ENTRIES.with(|count| count.set(count.get() + const_env.len()));
    const PREFIX: &str = "__parameter::width::";
    const_env
        .iter()
        .filter_map(|(marker, width)| {
            let name = marker.strip_prefix(PREFIX)?;
            let width = usize::try_from(*width).ok()?;
            let signed = const_env
                .get(&parameter_signed_marker(name))
                .is_some_and(|signed| *signed != 0);
            Some((name.to_string(), ExprType { width, signed }))
        })
        .collect()
}

/// Read one parameter's type without reconstructing the whole environment.
pub(super) fn parameter_type_from_const_env(
    const_env: &HashMap<String, i128>,
    name: &str,
) -> Option<ExprType> {
    parameter_type_from_lookup(name, &|marker| const_env.get(marker).copied())
}

pub(super) fn parameter_type_from_lookup(
    name: &str,
    lookup: &impl Fn(&str) -> Option<i128>,
) -> Option<ExprType> {
    let width = usize::try_from(lookup(&parameter_width_marker(name))?).ok()?;
    let signed = lookup(&parameter_signed_marker(name)).is_some_and(|signed| signed != 0);
    Some(ExprType { width, signed })
}

#[cfg(test)]
thread_local! {
    static SYNTAX_TYPE_DISCOVERIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static PARAMETER_TYPE_SCAN_ENTRIES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    pub(super) static CLASS_SIGNATURE_BINDINGS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
mod tests;

pub(super) fn parameter_packed_dimensions(parameters: &[Parameter]) -> VariablePackedDimensions {
    parameters
        .iter()
        .filter(|parameter| !parameter.packed_ranges.is_empty())
        .map(|parameter| {
            (
                parameter.name.clone(),
                VariableDimensions {
                    container: None,
                    randomizable: true,
                    enum_values: None,
                    real: false,
                    string: false,
                    class: None,
                    packed: function_packed_dimension_widths(&parameter.packed_ranges),
                    unpacked: Vec::new(),
                    signed: parameter.declared_signed.unwrap_or(false),
                    is_2state: parameter.declared_is_2state,
                    members: parameter.members.clone(),
                    signed_element_depth: parameter.signed_element_depth,
                },
            )
        })
        .collect()
}
