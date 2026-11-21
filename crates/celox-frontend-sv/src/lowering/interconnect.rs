//! Generic interconnect nets and ports (IEEE 1800-2023 6.6.8).
//!
//! An interconnect net has no type of its own: the nets and ports it
//! connects determine the type of the single simulated net they merge
//! into, and a net that is still an interconnect at the end of
//! elaboration is illegal (23.3.3.7.1). The analyzer only marks such
//! nets; this pass, which sees every specialized module of the design,
//! merges the types of all connected nets to a fixed point, rewrites
//! each interconnect variable to the merged type, and records the
//! merged driver resolution (Table 23-1) for driver validation and
//! driver resolution.
use super::*;
use sv::ast::NetResolution;

/// The data type of one net that an interconnect merges with.
#[derive(Clone, Debug, PartialEq)]
struct NetType {
    width: usize,
    signed: bool,
    float: Option<celox_sir::FloatPrecision>,
    is_4state: bool,
    value_kind: celox_design::ValueKind,
    type_kind: PortTypeKind,
    packed_ranges: Vec<(i128, i128)>,
}

impl NetType {
    fn of(variable: &SvVariable) -> Self {
        Self {
            width: variable.width,
            signed: variable.signed,
            float: variable.float,
            is_4state: variable.is_4state,
            value_kind: variable.value_kind,
            type_kind: variable.type_kind,
            packed_ranges: variable.packed_ranges.clone(),
        }
    }

    /// This type narrowed to `width` bits, as a select of a net carries.
    fn sliced(&self, width: usize) -> Self {
        Self {
            width,
            packed_ranges: if width > 1 {
                vec![(width as i128 - 1, 0)]
            } else {
                Vec::new()
            },
            ..self.clone()
        }
    }
}

/// The part of an interconnect variable one connection covers: the
/// whole variable, a flat bit range of a packed variable, or one
/// element of an unpacked array variable.
#[derive(Clone, Copy, Debug, PartialEq)]
enum RootRange {
    Bits(usize, usize),
    Element(usize),
}

/// One net that contributes its type to an interconnect, over the part
/// of the interconnect variable its connection covers, or the whole
/// variable when the range is `None`.
#[derive(Clone)]
struct Contribution {
    range: Option<RootRange>,
    ty: NetType,
    resolution: NetResolution,
    /// The number of elements of the contributing variable, so a whole
    /// typed array decomposes into its element types.
    elements: usize,
    /// The type is the merged summary of another interconnect, not an
    /// authored type: it states no per-element types of its own.
    summary: bool,
    /// The contribution is the variable's own authored implicit type
    /// (signing and packed dimensions, Syntax 6-2): its signing is a
    /// property of the net itself, not a vote of the connections.
    authored: bool,
}

type VarKey = (ModuleId, String);

/// A connection between two interconnects: each takes the other's
/// merged type, restricted to the connected ranges.
#[derive(Clone)]
struct Edge {
    a: VarKey,
    a_range: Option<RootRange>,
    b: VarKey,
    b_range: Option<RootRange>,
    /// The instance whose connection this is, as (module, position in
    /// its `instances`): port merging is per elaborated instance
    /// (23.3.3.7.1), so the two sides may merge differently for
    /// different instances of one specialization.
    instance: (ModuleId, usize),
}

/// The result of folding the contributions of one interconnect.
#[derive(Clone, PartialEq)]
struct Merged {
    ty: Option<NetType>,
    resolution: NetResolution,
    /// The width of one element, for an unpacked array variable.
    element_width: Option<usize>,
    /// The number of elements of the variable (1 when not an array).
    elements: usize,
    /// The merged type of each array element that connections typed
    /// distinctly (6.6.8: elements may resolve to different net types).
    element_types: HashMap<usize, NetType>,
    /// The merged driver resolution of each such element.
    element_resolutions: HashMap<usize, NetResolution>,
}

/// The net type that results from connecting an internal net of
/// resolution `internal` to an external net of resolution `external`
/// through a port (Table 23-1 and 23.3.3.7.1). An interconnect adopts
/// the other net's type; `Single` stands for the unresolved kinds
/// (`uwire`, pull and supply nets), which dominate the resolved kinds.
fn merge_resolution(internal: NetResolution, external: NetResolution) -> NetResolution {
    match (internal, external) {
        (NetResolution::Interconnect, other) | (other, NetResolution::Interconnect) => other,
        (NetResolution::Single, _) | (_, NetResolution::Single) => NetResolution::Single,
        (NetResolution::Wire, other) => other,
        (other, NetResolution::Wire) => other,
        (left, right) if left == right => left,
        // A `wand` and a `wor` meet in a `wor` (Table 23-1).
        _ => NetResolution::Or,
    }
}

/// Whether two attributes of overlapping contributions disagree.
/// Signing is not one: the merged net takes its signing from the
/// authored declaration, or from the connections (see `merge`).
fn type_differs(left: &NetType, right: &NetType) -> bool {
    left.float != right.float
        || left.is_4state != right.is_4state
        || left.value_kind != right.value_kind
        || left.type_kind != right.type_kind
}

/// The part of an interconnect variable a normalized contribution
/// covers.
#[derive(Clone, Copy, PartialEq)]
enum Part {
    Whole,
    Summary,
    Bits(usize, usize),
    Element(usize),
}

/// Fold the contributions of one interconnect into its merged type.
/// Contributions that overlap must agree on the type of the shared
/// bits; disjoint contributions may carry different types, which the
/// merged variable then transports as raw bits (6.6.8). `elements` is
/// the number of elements of an unpacked array variable (1 otherwise):
/// each element resolves on its own (its type and its driver
/// resolution), and the variable is laid out with the widest element.
/// A contribution that is only the summary of another interconnect
/// states no element types: it never conflicts with element types.
fn merge(
    name: &str,
    contributions: &[Contribution],
    elements: usize,
) -> Result<Merged, sv::AnalyzerError> {
    let different = || {
        sv::AnalyzerError::Unsupported(format!(
            "interconnect net `{name}` connects nets of different types"
        ))
    };
    let mut resolution = NetResolution::Interconnect;
    for contribution in contributions {
        resolution = merge_resolution(contribution.resolution, resolution);
    }
    // Normalize whole typed arrays into their elements.
    let mut parts: Vec<(Part, NetType, NetResolution, bool)> = Vec::new();
    for contribution in contributions {
        match contribution.range {
            Some(RootRange::Element(index)) => parts.push((
                Part::Element(index),
                contribution.ty.clone(),
                contribution.resolution,
                contribution.authored,
            )),
            Some(RootRange::Bits(lsb, msb)) => parts.push((
                Part::Bits(lsb, msb),
                contribution.ty.clone(),
                contribution.resolution,
                contribution.authored,
            )),
            None if contribution.summary => parts.push((
                Part::Summary,
                contribution.ty.clone(),
                contribution.resolution,
                contribution.authored,
            )),
            None if elements > 1
                && contribution.elements == elements
                && contribution.ty.width % elements == 0 =>
            {
                let element = contribution.ty.sliced(contribution.ty.width / elements);
                for index in 0..elements {
                    parts.push((
                        Part::Element(index),
                        element.clone(),
                        contribution.resolution,
                        contribution.authored,
                    ));
                }
            }
            None => parts.push((
                Part::Whole,
                contribution.ty.clone(),
                contribution.resolution,
                contribution.authored,
            )),
        }
    }
    for (index, (part, ty, _, authored)) in parts.iter().enumerate() {
        for (other_part, other, _, other_authored) in &parts[..index] {
            if *authored || *other_authored {
                continue;
            }
            let conflict = match (part, other_part) {
                (Part::Summary, _) | (_, Part::Summary) => false,
                (Part::Element(left), Part::Element(right)) => {
                    left == right && (type_differs(ty, other) || ty.width != other.width)
                }
                (Part::Element(_), Part::Whole) | (Part::Whole, Part::Element(_)) => {
                    type_differs(ty, other)
                }
                (Part::Whole, Part::Whole) => type_differs(ty, other),
                (Part::Bits(lsb, msb), Part::Bits(other_lsb, other_msb)) => {
                    lsb <= other_msb
                        && other_lsb <= msb
                        && (type_differs(ty, other)
                            || ((lsb, msb) == (other_lsb, other_msb) && ty.width != other.width))
                }
                (Part::Bits(..), Part::Element(_) | Part::Whole)
                | (Part::Element(_) | Part::Whole, Part::Bits(..)) => type_differs(ty, other),
            };
            if conflict {
                return Err(different());
            }
        }
    }
    let Some(first) = contributions.first() else {
        return Ok(Merged {
            ty: None,
            resolution,
            element_width: None,
            elements,
            element_types: HashMap::default(),
            element_resolutions: HashMap::default(),
        });
    };
    // The type and driver resolution of each distinctly typed element.
    let mut element_types: HashMap<usize, NetType> = HashMap::default();
    let mut element_resolutions: HashMap<usize, NetResolution> = HashMap::default();
    for (part, ty, part_resolution, _) in &parts {
        let Part::Element(index) = part else {
            continue;
        };
        element_types.entry(*index).or_insert_with(|| ty.clone());
        let entry = element_resolutions
            .entry(*index)
            .or_insert(NetResolution::Interconnect);
        *entry = merge_resolution(*part_resolution, *entry);
    }
    for (part, _, part_resolution, _) in &parts {
        if *part == Part::Whole {
            for index in 0..elements {
                let entry = element_resolutions
                    .entry(index)
                    .or_insert(NetResolution::Interconnect);
                *entry = merge_resolution(*part_resolution, *entry);
            }
        }
    }
    if let Some(authored) = contributions
        .iter()
        .find(|contribution| contribution.authored)
    {
        for ty in element_types.values_mut() {
            ty.signed = authored.ty.signed;
        }
    }
    element_resolutions.retain(|_, resolution| *resolution != NetResolution::Interconnect);
    let element_width = (elements > 1).then(|| {
        let from_elements = element_types.values().map(|ty| ty.width).max();
        let from_parts = parts
            .iter()
            .map(|(part, ty, _, _)| match part {
                Part::Element(_) => ty.width,
                Part::Bits(_, msb) => msb + 1,
                Part::Whole | Part::Summary => ty.width / elements,
            })
            .max();
        from_elements
            .into_iter()
            .chain(from_parts)
            .max()
            .unwrap_or(1)
    });
    let width = match element_width {
        Some(element) => {
            let flat = parts
                .iter()
                .filter_map(|(part, _, _, _)| match part {
                    Part::Bits(_, msb) => Some(msb + 1),
                    _ => None,
                })
                .max()
                .unwrap_or(0);
            (element * elements).max(flat)
        }
        None => parts
            .iter()
            .map(|(part, ty, _, _)| match part {
                Part::Bits(_, msb) => msb + 1,
                _ => ty.width,
            })
            .max()
            .unwrap_or(1),
    };
    let all = |pick: &dyn Fn(&NetType) -> bool| contributions.iter().all(|c| pick(&c.ty));
    // Only contributors that agree on a real precision make the merged
    // variable real; a mix of real and integral elements is transported
    // as raw bits, which port assignments pass through without conversion
    // (6.6.8: different elements may resolve to different net types).
    let float = if all(&|ty| ty.float == first.ty.float) {
        first.ty.float
    } else {
        None
    };
    let ty = NetType {
        width,
        // An authored signing is a property of the net itself; without
        // one, agreeing connections decide, and disagreeing ones take
        // the signing of a connection whose net type dominates.
        signed: contributions
            .iter()
            .find(|contribution| contribution.authored)
            .map(|authored| authored.ty.signed)
            .or_else(|| all(&|ty| ty.signed == first.ty.signed).then_some(first.ty.signed))
            .unwrap_or_else(|| {
                contributions
                    .iter()
                    .find(|contribution| contribution.resolution == resolution)
                    .is_some_and(|contribution| contribution.ty.signed)
            }),
        float,
        is_4state: contributions.iter().any(|c| c.ty.is_4state),
        value_kind: if all(&|ty| ty.value_kind == first.ty.value_kind) {
            first.ty.value_kind
        } else {
            celox_design::ValueKind::Bits
        },
        type_kind: if all(&|ty| ty.type_kind == first.ty.type_kind) {
            first.ty.type_kind
        } else {
            match float {
                Some(celox_sir::FloatPrecision::F64) => PortTypeKind::Real,
                Some(celox_sir::FloatPrecision::F32) => PortTypeKind::ShortReal,
                None => PortTypeKind::Other,
            }
        },
        packed_ranges: if contributions.len() == 1 && first.range.is_none() {
            first.ty.packed_ranges.clone()
        } else if let Some(element) = element_width {
            if element > 1 {
                vec![(element as i128 - 1, 0)]
            } else {
                Vec::new()
            }
        } else if width > 1 {
            vec![(width as i128 - 1, 0)]
        } else {
            Vec::new()
        },
    };
    Ok(Merged {
        ty: Some(ty),
        resolution,
        element_width,
        elements,
        element_types,
        element_resolutions,
    })
}

/// The parts of `module`'s variables that `expr` connects to, with the
/// root name of each.
fn actual_roots(expr: &sv::ir::Expr, module: &LoweredSvModule) -> Vec<(String, Option<RootRange>)> {
    match expr {
        sv::ir::Expr::Ident(name) => vec![(name.clone(), None)],
        sv::ir::Expr::Concat(parts) => parts
            .iter()
            .flat_map(|part| actual_roots(part, module))
            .collect(),
        sv::ir::Expr::Resize { expr, .. }
        | sv::ir::Expr::Unary {
            op: sv::ir::UnaryOp::ToTwoState,
            expr,
        } => actual_roots(expr, module),
        _ => output_lvalue_accesses(
            expr,
            &module.variables,
            &module.signal_names,
            &module.constants,
            &module.parameter_types,
        )
        .map(|accesses| {
            accesses
                .iter()
                .filter_map(|access| {
                    module.variables.get(&access.signal).map(|variable| {
                        let (lsb, msb) = (access.access.lsb, access.access.msb);
                        let elements: usize = variable.array_dims.iter().product::<usize>().max(1);
                        let range = if elements > 1 {
                            let element = variable.width / elements;
                            if element > 0 && msb - lsb + 1 == element && lsb % element == 0 {
                                RootRange::Element(lsb / element)
                            } else {
                                RootRange::Bits(lsb, msb)
                            }
                        } else {
                            RootRange::Bits(lsb, msb)
                        };
                        (variable.path.join("."), Some(range))
                    })
                })
                .collect()
        })
        .unwrap_or_default(),
    }
}

/// The type a connection to `range` of a variable carries, from the
/// type of the net on the other side of the connection.
fn connected_type(range: Option<RootRange>, other: &NetType) -> NetType {
    match range {
        Some(RootRange::Bits(lsb, msb)) => other.sliced(msb - lsb + 1),
        _ => other.clone(),
    }
}

/// Rewrite the selects of interconnect array variables in a port
/// connection expression after the variables were retyped: the
/// analyzer baked element selects as bit ranges of the placeholder
/// element width, so they are rescaled to the merged element width.
fn rescale_selects(
    expr: sv::ir::Expr,
    scales: &HashMap<String, (usize, usize)>,
    constants: &HashMap<String, i128>,
    parameter_types: &HashMap<String, (usize, bool)>,
) -> sv::ir::Expr {
    let map = |expr: sv::ir::Expr| rescale_selects(expr, scales, constants, parameter_types);
    match expr {
        sv::ir::Expr::Select {
            expr: inner,
            msb,
            lsb,
            signed,
        } => {
            if let sv::ir::Expr::Ident(name) = &*inner
                && let Some(&(old, new)) = scales.get(name)
                && old != new
            {
                let value = |expr: &sv::ir::ConstExpr| {
                    sv::typecheck::eval_const_expr_with_types(expr, constants, parameter_types)
                };
                if let (Some(msb), Some(lsb)) = (value(&msb), value(&lsb)) {
                    let (low, high) = (msb.min(lsb), msb.max(lsb));
                    let old = old as i128;
                    if high - low + 1 == old && low % old == 0 {
                        let low = low / old * new as i128;
                        return sv::ir::Expr::Select {
                            expr: inner,
                            msb: sv::ir::ConstExpr::Literal((low + new as i128 - 1).to_string()),
                            lsb: sv::ir::ConstExpr::Literal(low.to_string()),
                            signed,
                        };
                    }
                }
            }
            sv::ir::Expr::Select {
                expr: Box::new(map(*inner)),
                msb,
                lsb,
                signed,
            }
        }
        sv::ir::Expr::Concat(parts) => sv::ir::Expr::Concat(parts.into_iter().map(map).collect()),
        sv::ir::Expr::RepeatConcat { count, parts } => sv::ir::Expr::RepeatConcat {
            count,
            parts: parts.into_iter().map(map).collect(),
        },
        sv::ir::Expr::Resize {
            expr,
            width,
            signed,
        } => sv::ir::Expr::Resize {
            expr: Box::new(map(*expr)),
            width,
            signed,
        },
        sv::ir::Expr::Unary { op, expr } => sv::ir::Expr::Unary {
            op,
            expr: Box::new(map(*expr)),
        },
        sv::ir::Expr::Binary { left, op, right } => sv::ir::Expr::Binary {
            left: Box::new(map(*left)),
            op,
            right: Box::new(map(*right)),
        },
        sv::ir::Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => sv::ir::Expr::Mux {
            condition: Box::new(map(*condition)),
            then_expr: Box::new(map(*then_expr)),
            else_expr: Box::new(map(*else_expr)),
        },
        sv::ir::Expr::Call {
            callee,
            args,
            arg_order,
        } => sv::ir::Expr::Call {
            callee,
            args: args.into_iter().map(|arg| arg.map(map)).collect(),
            arg_order,
        },
        sv::ir::Expr::Inside { expr, items } => sv::ir::Expr::Inside {
            expr: Box::new(map(*expr)),
            items: items
                .into_iter()
                .map(|item| match item {
                    sv::ir::InsideItem::Value(value) => sv::ir::InsideItem::Value(map(value)),
                    sv::ir::InsideItem::Range { low, high } => sv::ir::InsideItem::Range {
                        low: map(low),
                        high: map(high),
                    },
                })
                .collect(),
        },
        other => other,
    }
}

/// A type an instance's connection contributes to an interconnect
/// formal of the instantiated module, kept apart from the formal's
/// internal contributions: each instance merges it with the internal
/// ones separately (23.3.3.7.1).
#[derive(Clone)]
struct InstanceExternal {
    parent_id: ModuleId,
    instance_index: usize,
    child_id: ModuleId,
    formal: String,
    contribution: Contribution,
}

/// One operand of a concatenation connected to a port: a fixed width
/// it consumes, or an interconnect variable that takes a slice of the
/// formal's type of its own width.
enum ConcatOperand {
    Fixed(usize, NetResolution),
    Net {
        key: VarKey,
        range: Option<RootRange>,
    },
}

/// A concatenation in a port connection that involves interconnects
/// (a net_lvalue of interconnect nets, 6.6.8): the operands share the
/// formal's type in declaration order, most significant first.
struct Concat {
    formal_ty: Option<NetType>,
    formal_resolution: NetResolution,
    formal_key: Option<VarKey>,
    operands: Vec<ConcatOperand>,
    /// The instance whose connection this is, as (module, position in
    /// its `instances`).
    instance: (ModuleId, usize),
}

/// The contributions the edges into `target` make, from the merged
/// types of their peers. `keep` selects the edges that belong to one
/// elaborated net (all of them, or one instance's).
fn edge_contributions(
    target: &VarKey,
    edges: &[Edge],
    merged: &HashMap<VarKey, Merged>,
    keep: &dyn Fn(&Edge) -> bool,
) -> Vec<Contribution> {
    let mut contributions = Vec::new();
    for edge in edges {
        if !keep(edge) {
            continue;
        }
        for (candidate, range, peer) in [
            (&edge.a, &edge.a_range, &edge.b),
            (&edge.b, &edge.b_range, &edge.a),
        ] {
            if candidate != target {
                continue;
            }
            let Some(peer_merged) = merged.get(peer) else {
                continue;
            };
            let Some(peer_ty) = &peer_merged.ty else {
                continue;
            };
            match range {
                None if !peer_merged.element_types.is_empty() => {
                    for (index, element_ty) in &peer_merged.element_types {
                        contributions.push(Contribution {
                            range: Some(RootRange::Element(*index)),
                            ty: element_ty.clone(),
                            resolution: peer_merged
                                .element_resolutions
                                .get(index)
                                .copied()
                                .unwrap_or(peer_merged.resolution),
                            elements: 1,
                            summary: false,
                            authored: false,
                        });
                    }
                }
                _ => {
                    let (ty, resolution, summary) = match range {
                        Some(RootRange::Element(index)) => {
                            if let Some(element_ty) = peer_merged.element_types.get(index) {
                                (
                                    element_ty.clone(),
                                    peer_merged
                                        .element_resolutions
                                        .get(index)
                                        .copied()
                                        .unwrap_or(peer_merged.resolution),
                                    false,
                                )
                            } else if let Some(element) = peer_merged.element_width {
                                (peer_ty.sliced(element), peer_merged.resolution, true)
                            } else {
                                (peer_ty.clone(), peer_merged.resolution, true)
                            }
                        }
                        Some(RootRange::Bits(lsb, msb)) => {
                            (peer_ty.sliced(msb - lsb + 1), peer_merged.resolution, true)
                        }
                        None => (peer_ty.clone(), peer_merged.resolution, true),
                    };
                    contributions.push(Contribution {
                        range: *range,
                        ty,
                        resolution,
                        elements: peer_merged.elements,
                        summary,
                        authored: false,
                    });
                }
            }
        }
    }
    contributions
}

/// The contributions one concatenation makes to `target`, once the
/// widths it needs are merged: to the formal, the concatenation of
/// its operands; to an operand, its slice of the formal's type. An
/// operand whose width nothing else determines takes the width the
/// formal leaves it, when it is the only one.
fn concat_contributions(
    concat: &Concat,
    target: &VarKey,
    merged: &HashMap<VarKey, Merged>,
) -> Result<Vec<Contribution>, sv::AnalyzerError> {
    let formal = match (&concat.formal_ty, &concat.formal_key) {
        (Some(ty), _) => Some((ty.clone(), concat.formal_resolution)),
        (None, Some(key)) => merged
            .get(key)
            .and_then(|merged| merged.ty.clone())
            .map(|ty| (ty, merged[key].resolution)),
        (None, None) => None,
    };
    let operand_width = |operand: &ConcatOperand| -> Option<usize> {
        match operand {
            ConcatOperand::Fixed(width, _) => Some(*width),
            ConcatOperand::Net { key, range } => match range {
                Some(RootRange::Bits(lsb, msb)) => Some(msb - lsb + 1),
                Some(RootRange::Element(_)) => {
                    merged.get(key).and_then(|merged| merged.element_width)
                }
                None => merged
                    .get(key)
                    .and_then(|merged| merged.ty.as_ref().map(|ty| ty.width)),
            },
        }
    };
    if concat.formal_key.as_ref() == Some(target) {
        let mut width = 0;
        let mut signed = true;
        let mut float = None;
        let mut floats_agree = true;
        let mut is_4state = false;
        let mut resolution = NetResolution::Interconnect;
        for operand in &concat.operands {
            match operand {
                ConcatOperand::Fixed(operand_width, operand_resolution) => {
                    width += operand_width;
                    signed = false;
                    floats_agree = false;
                    resolution = merge_resolution(*operand_resolution, resolution);
                }
                ConcatOperand::Net { key, .. } => {
                    let Some(operand_merged) = merged.get(key) else {
                        return Ok(Vec::new());
                    };
                    let Some(operand_ty) = &operand_merged.ty else {
                        return Ok(Vec::new());
                    };
                    let Some(operand_width) = operand_width(operand) else {
                        return Ok(Vec::new());
                    };
                    width += operand_width;
                    signed &= operand_ty.signed;
                    match float {
                        None => float = operand_ty.float,
                        Some(current) if Some(current) != operand_ty.float => {
                            floats_agree = false;
                        }
                        _ => {}
                    }
                    if operand_ty.float.is_none() {
                        floats_agree = false;
                    }
                    is_4state |= operand_ty.is_4state;
                    resolution = merge_resolution(operand_merged.resolution, resolution);
                }
            }
        }
        let ty = NetType {
            width,
            signed,
            float: if floats_agree { float } else { None },
            is_4state,
            value_kind: celox_design::ValueKind::Bits,
            type_kind: PortTypeKind::Other,
            packed_ranges: if width > 1 {
                vec![(width as i128 - 1, 0)]
            } else {
                Vec::new()
            },
        };
        return Ok(vec![Contribution {
            range: None,
            ty,
            resolution,
            elements: 1,
            summary: true,
            authored: false,
        }]);
    }
    let is_operand = concat.operands.iter().any(|operand| match operand {
        ConcatOperand::Net { key, .. } => key == target,
        ConcatOperand::Fixed(..) => false,
    });
    if !is_operand {
        return Ok(Vec::new());
    }
    let Some((formal_ty, formal_resolution)) = formal else {
        return Ok(Vec::new());
    };
    let mut known = 0;
    let mut unknown = 0;
    for operand in &concat.operands {
        match operand_width(operand) {
            Some(width) => known += width,
            None => unknown += 1,
        }
    }
    if unknown > 1 || (unknown == 1 && known >= formal_ty.width) {
        return Ok(Vec::new());
    }
    if unknown == 0 && known != formal_ty.width {
        return Err(sv::AnalyzerError::Unsupported(format!(
            "interconnect net `{}` connects a concatenation of {known} bits to a {}-bit port",
            target.1, formal_ty.width
        )));
    }
    let mut contributions = Vec::new();
    for operand in &concat.operands {
        let ConcatOperand::Net { key, range } = operand else {
            continue;
        };
        if key != target {
            continue;
        }
        let width = operand_width(operand).unwrap_or(formal_ty.width - known);
        contributions.push(Contribution {
            range: *range,
            ty: formal_ty.sliced(width),
            resolution: formal_resolution,
            elements: 1,
            summary: false,
            authored: false,
        });
    }
    Ok(contributions)
}

/// Give every instance of a specialization with interconnect
/// formals its own lowered module when more than one instance
/// elaborates it: port collapsing applies to each elaborated net
/// (23.3.3.7.1), and the merged type of a formal may differ between
/// instances, which one shared module cannot carry. The clone an
/// instance points to is named by the descriptor in its key.
/// Connection records are repointed or duplicated for the clones,
/// and each clone inherits the internal contributions of the
/// original module, so the fixed point merges each elaborated net
/// separately. Runs before the fixed point, and again for the
/// children a split leaves shared between clones.
fn split_shared_interconnect_specializations(
    module_ids: &mut HashMap<LoweredSvModuleKey, ModuleId>,
    modules: &mut HashMap<ModuleId, LoweredSvModule>,
    contributions: &mut HashMap<VarKey, Vec<Contribution>>,
    externals: &mut Vec<InstanceExternal>,
    edges: &mut Vec<Edge>,
    concats: &mut Vec<Concat>,
) {
    loop {
        // A specialization with interconnect formals that more than
        // one instance elaborates.
        let mut target: Option<(ModuleId, Vec<(ModuleId, usize)>)> = None;
        let mut children: Vec<ModuleId> = modules.keys().copied().collect();
        children.sort_by_key(|id| id.0);
        for child_id in children {
            if !modules[&child_id]
                .source
                .ports()
                .iter()
                .any(|port| port.is_interconnect())
            {
                continue;
            }
            let mut instances = Vec::new();
            for (&parent_id, parent) in modules.iter() {
                for (index, instance) in parent.instances.iter().enumerate() {
                    if module_ids.get(&LoweredSvModuleKey::instance_key(instance))
                        == Some(&child_id)
                    {
                        instances.push((parent_id, index));
                    }
                }
            }
            if instances.len() > 1 {
                target = Some((child_id, instances));
                break;
            }
        }
        let Some((child_id, instances)) = target else {
            break;
        };
        let base_key = module_ids
            .iter()
            .find(|(_, id)| **id == child_id)
            .map(|(key, _)| key.clone())
            .expect("specialization key");
        let first_free = modules.keys().map(|id| id.0).max().unwrap_or_default() + 1;
        // The first instance keeps the original module; each further
        // instance gets a clone under its own key.
        for (next_id, &(parent_id, index)) in (first_free..).zip(instances.iter().skip(1)) {
            let new_id = ModuleId(next_id);
            let clone = modules[&child_id].clone();
            modules.insert(new_id, clone);
            let descriptor = vec![(
                "$instance".to_string(),
                format!("{}:{}", parent_id.0, index),
            )];
            let mut new_key = base_key.clone();
            new_key.interconnect_descriptor = descriptor.clone();
            module_ids.insert(new_key, new_id);
            modules
                .get_mut(&parent_id)
                .expect("parent module")
                .instances[index]
                .interconnect_descriptor = descriptor;
            // The clone inherits the original's internal contributions.
            let names: Vec<String> = modules[&new_id]
                .interconnect_names
                .iter()
                .cloned()
                .collect();
            for name in names {
                if let Some(internal) = contributions.get(&(child_id, name.clone())).cloned() {
                    contributions.insert((new_id, name), internal);
                }
            }
            // Repoint this instance's records to the clone, and
            // duplicate the records of the clone's own instances.
            for external in externals.iter_mut() {
                if external.child_id == child_id
                    && external.parent_id == parent_id
                    && external.instance_index == index
                {
                    external.child_id = new_id;
                }
            }
            let mut duplicated_edges = Vec::new();
            for edge in edges.iter_mut() {
                if edge.instance == (parent_id, index) && edge.b.0 == child_id {
                    edge.b.0 = new_id;
                }
                if edge.instance.0 == child_id {
                    let mut edge = edge.clone();
                    edge.instance.0 = new_id;
                    if edge.a.0 == child_id {
                        edge.a.0 = new_id;
                    }
                    if edge.b.0 == child_id {
                        edge.b.0 = new_id;
                    }
                    duplicated_edges.push(edge);
                }
            }
            edges.extend(duplicated_edges);
            let duplicated_externals: Vec<InstanceExternal> = externals
                .iter()
                .filter(|external| external.parent_id == child_id)
                .map(|external| {
                    let mut external = external.clone();
                    external.parent_id = new_id;
                    external
                })
                .collect();
            externals.extend(duplicated_externals);
            let mut duplicated_concats = Vec::new();
            for concat in concats.iter_mut() {
                if concat.instance == (parent_id, index) {
                    if let Some(formal_key) = &mut concat.formal_key
                        && formal_key.0 == child_id
                    {
                        formal_key.0 = new_id;
                    }
                }
                if concat.instance.0 == child_id {
                    let mut concat = Concat {
                        formal_ty: concat.formal_ty.clone(),
                        formal_resolution: concat.formal_resolution,
                        formal_key: concat.formal_key.clone(),
                        operands: concat
                            .operands
                            .iter()
                            .map(|operand| match operand {
                                ConcatOperand::Fixed(width, resolution) => {
                                    ConcatOperand::Fixed(*width, *resolution)
                                }
                                ConcatOperand::Net { key, range } => ConcatOperand::Net {
                                    key: if key.0 == child_id {
                                        (new_id, key.1.clone())
                                    } else {
                                        key.clone()
                                    },
                                    range: *range,
                                },
                            })
                            .collect(),
                        instance: (new_id, concat.instance.1),
                    };
                    if let Some(formal_key) = &mut concat.formal_key
                        && formal_key.0 == child_id
                    {
                        formal_key.0 = new_id;
                    }
                    duplicated_concats.push(concat);
                }
            }
            concats.extend(duplicated_concats);
        }
    }
}

/// Resolve every interconnect of the design to the type of its
/// connections, rewriting the variables in place. See the module
/// documentation.
pub(super) fn resolve_interconnects(
    module_ids: &mut HashMap<LoweredSvModuleKey, ModuleId>,
    modules: &mut HashMap<ModuleId, LoweredSvModule>,
) -> Result<(), sv::AnalyzerError> {
    if modules
        .values()
        .all(|module| module.interconnect_names.is_empty())
    {
        return Ok(());
    }
    // The resolution each authored net or port of a module declares.
    let declared_resolution: HashMap<ModuleId, HashMap<String, NetResolution>> = modules
        .iter()
        .map(|(&id, module)| {
            (
                id,
                module
                    .source
                    .signals()
                    .iter()
                    .map(|signal| (signal.name().to_string(), signal.net_resolution()))
                    .chain(
                        module
                            .source
                            .ports()
                            .iter()
                            .map(|port| (port.name().to_string(), port.net_resolution())),
                    )
                    .collect(),
            )
        })
        .collect();
    let variable_of =
        |modules: &HashMap<ModuleId, LoweredSvModule>, key: &VarKey| -> Option<SvVariable> {
            let module = modules.get(&key.0)?;
            let id = module.signal_names.get(&key.1)?;
            module.variables.get(id).cloned()
        };
    // Static contributions come from connected nets that are not
    // interconnects, and from an authored packed shape; edges join two
    // interconnects whose types are found together below, and the
    // contributions of each instance to an interconnect formal are
    // kept apart for per-instance merging.
    let mut contributions: HashMap<VarKey, Vec<Contribution>> = HashMap::default();
    let mut edges: Vec<Edge> = Vec::new();
    let mut externals: Vec<InstanceExternal> = Vec::new();
    let mut concats: Vec<Concat> = Vec::new();
    for (&parent_id, parent) in modules.iter() {
        for key in &parent.interconnect_names {
            // An authored packed dimension or signing states part of the
            // type itself (6.6.8, Syntax 6-2); an undeclared shape does not.
            let Some(variable) = variable_of(modules, &(parent_id, key.clone())) else {
                continue;
            };
            let authored = parent
                .source
                .signals()
                .iter()
                .find(|signal| signal.name() == key)
                .map(|signal| signal.r#type())
                .or_else(|| {
                    parent
                        .source
                        .ports()
                        .iter()
                        .find(|port| port.name() == key)
                        .map(|port| port.r#type())
                })
                .is_some_and(|ty| !ty.packed_ranges().is_empty() || ty.is_signed());
            if authored {
                contributions
                    .entry((parent_id, key.clone()))
                    .or_default()
                    .push(Contribution {
                        range: None,
                        ty: NetType::of(&variable),
                        resolution: NetResolution::Interconnect,
                        elements: variable.array_dims.iter().product::<usize>().max(1),
                        summary: false,
                        authored: true,
                    });
            }
        }
        for (instance_index, instance) in parent.instances.iter().enumerate() {
            let child_key = LoweredSvModuleKey::instance_key(instance);
            let Some(&child_id) = module_ids.get(&child_key) else {
                continue;
            };
            let Some(child) = modules.get(&child_id) else {
                continue;
            };
            let Ok(authored_connections) = expand_structure_connections(
                &instance.port_connections,
                &parent.source,
                &child.source,
                None,
            ) else {
                continue;
            };
            let connections = match instance.array_element {
                Some((position, count)) => match array_element_connections(
                    &authored_connections,
                    child,
                    position,
                    count,
                    &parent.variables,
                    &parent.signal_names,
                    &parent.constants,
                    &parent.parameter_types,
                ) {
                    Ok(connections) => connections,
                    Err(_) => continue,
                },
                None => authored_connections.into_owned(),
            };
            for connection in &connections {
                let Some(formal_port) = child
                    .source
                    .ports()
                    .iter()
                    .find(|port| port.name() == connection.formal)
                else {
                    continue;
                };
                let Some(child_variable) = child
                    .signal_names
                    .get(&connection.formal)
                    .and_then(|id| child.variables.get(id))
                else {
                    continue;
                };
                let child_interconnect = formal_port.is_interconnect();
                let Some(actual) = connection.actual_expr.as_ref() else {
                    continue;
                };
                let mut mentioned = HashSet::default();
                procedural::expr_idents(actual, &mut mentioned);
                // A concatenation of interconnect nets is a net_lvalue
                // (6.6.8): its operands share the formal's type, in
                // order, and keep their own widths.
                let peeled = {
                    let mut peeled = actual;
                    loop {
                        match peeled {
                            sv::ir::Expr::Resize { expr, .. }
                            | sv::ir::Expr::Unary {
                                op: sv::ir::UnaryOp::ToTwoState,
                                expr,
                            } => peeled = expr,
                            _ => break peeled,
                        }
                    }
                };
                if let sv::ir::Expr::Concat(parts) = peeled {
                    let part_roots: Vec<Vec<(String, Option<RootRange>)>> = parts
                        .iter()
                        .map(|part| actual_roots(part, parent))
                        .collect();
                    let involves_interconnect = child_interconnect
                        || part_roots
                            .iter()
                            .flatten()
                            .any(|(root, _)| parent.interconnect_names.contains(root));
                    if involves_interconnect {
                        if formal_port.direction() == sv::ir::PortDirection::Input {
                            return Err(sv::AnalyzerError::Unsupported(format!(
                                "interconnect net `{}` used in an expression",
                                connection.formal
                            )));
                        }
                        let mut operands = Vec::new();
                        let mut operand_variables = Vec::new();
                        for roots in &part_roots {
                            if roots.len() != 1 {
                                return Err(sv::AnalyzerError::Unsupported(format!(
                                    "interconnect net `{}` used in an expression",
                                    connection.formal
                                )));
                            }
                            let (root, range) = &roots[0];
                            let Some(parent_variable) = parent
                                .signal_names
                                .get(root)
                                .and_then(|id| parent.variables.get(id))
                            else {
                                continue;
                            };
                            operand_variables.push(parent_variable.clone());
                            if parent.interconnect_names.contains(root) {
                                operands.push(ConcatOperand::Net {
                                    key: (parent_id, root.clone()),
                                    range: *range,
                                });
                            } else {
                                let width = match range {
                                    Some(RootRange::Bits(lsb, msb)) => msb - lsb + 1,
                                    Some(RootRange::Element(_)) => {
                                        let elements: usize = parent_variable
                                            .array_dims
                                            .iter()
                                            .product::<usize>()
                                            .max(1);
                                        parent_variable.width / elements
                                    }
                                    None => parent_variable.width,
                                };
                                operands.push(ConcatOperand::Fixed(
                                    width,
                                    declared_resolution[&parent_id]
                                        .get(root)
                                        .copied()
                                        .unwrap_or(NetResolution::Wire),
                                ));
                            }
                        }
                        if child_interconnect
                            && operands
                                .iter()
                                .all(|operand| matches!(operand, ConcatOperand::Fixed(..)))
                        {
                            // The formal takes the concatenation type.
                            let width: usize = operands
                                .iter()
                                .map(|operand| match operand {
                                    ConcatOperand::Fixed(width, _) => *width,
                                    ConcatOperand::Net { .. } => unreachable!(),
                                })
                                .sum();
                            let mut resolution = NetResolution::Interconnect;
                            for operand in &operands {
                                if let ConcatOperand::Fixed(_, operand_resolution) = operand {
                                    resolution = merge_resolution(*operand_resolution, resolution);
                                }
                            }
                            externals.push(InstanceExternal {
                                parent_id,
                                instance_index,
                                child_id,
                                formal: connection.formal.clone(),
                                contribution: Contribution {
                                    range: None,
                                    ty: NetType {
                                        width,
                                        signed: operand_variables
                                            .iter()
                                            .all(|variable| variable.signed),
                                        float: operand_variables
                                            .first()
                                            .and_then(|variable| variable.float)
                                            .filter(|float| {
                                                operand_variables.iter().all(|variable| {
                                                    variable.float.as_ref() == Some(float)
                                                })
                                            }),
                                        is_4state: operand_variables
                                            .iter()
                                            .any(|variable| variable.is_4state),
                                        value_kind: celox_design::ValueKind::Bits,
                                        type_kind: PortTypeKind::Other,
                                        packed_ranges: if width > 1 {
                                            vec![(width as i128 - 1, 0)]
                                        } else {
                                            Vec::new()
                                        },
                                    },
                                    resolution,
                                    elements: child_variable
                                        .array_dims
                                        .iter()
                                        .product::<usize>()
                                        .max(1),
                                    summary: false,
                                    authored: false,
                                },
                            });
                        } else if child_interconnect {
                            concats.push(Concat {
                                formal_ty: None,
                                formal_resolution: NetResolution::Interconnect,
                                formal_key: Some((child_id, connection.formal.clone())),
                                operands,
                                instance: (parent_id, instance_index),
                            });
                        } else {
                            concats.push(Concat {
                                formal_ty: Some(NetType::of(child_variable)),
                                formal_resolution: formal_port.net_resolution(),
                                formal_key: None,
                                operands,
                                instance: (parent_id, instance_index),
                            });
                        }
                        continue;
                    }
                }
                let roots = actual_roots(actual, parent);
                for name in &mentioned {
                    if parent.interconnect_names.contains(name)
                        && !roots.iter().any(|(root, _)| root == name)
                    {
                        return Err(sv::AnalyzerError::Unsupported(format!(
                            "interconnect net `{name}` used in an expression"
                        )));
                    }
                }
                for (root, range) in roots {
                    let Some(parent_variable) = parent
                        .signal_names
                        .get(&root)
                        .and_then(|id| parent.variables.get(id))
                    else {
                        continue;
                    };
                    let parent_interconnect = parent.interconnect_names.contains(&root);
                    if parent_interconnect && child_interconnect {
                        edges.push(Edge {
                            a: (parent_id, root.clone()),
                            a_range: range,
                            b: (child_id, connection.formal.clone()),
                            b_range: None,
                            instance: (parent_id, instance_index),
                        });
                    } else if parent_interconnect {
                        contributions
                            .entry((parent_id, root.clone()))
                            .or_default()
                            .push(Contribution {
                                range,
                                ty: connected_type(range, &NetType::of(child_variable)),
                                resolution: formal_port.net_resolution(),
                                elements: child_variable
                                    .array_dims
                                    .iter()
                                    .product::<usize>()
                                    .max(1),
                                summary: false,
                                authored: false,
                            });
                    } else if child_interconnect {
                        externals.push(InstanceExternal {
                            parent_id,
                            instance_index,
                            child_id,
                            formal: connection.formal.clone(),
                            contribution: Contribution {
                                range: None,
                                ty: connected_type(range, &NetType::of(parent_variable)),
                                resolution: declared_resolution[&parent_id]
                                    .get(&root)
                                    .copied()
                                    .unwrap_or(NetResolution::Wire),
                                elements: parent_variable
                                    .array_dims
                                    .iter()
                                    .product::<usize>()
                                    .max(1),
                                summary: false,
                                authored: false,
                            },
                        });
                    }
                }
            }
        }
    }
    split_shared_interconnect_specializations(
        module_ids,
        modules,
        &mut contributions,
        &mut externals,
        &mut edges,
        &mut concats,
    );
    // Each instance's contributions to an interconnect formal now
    // belong to the one lowered module that elaborates it.
    for external in externals {
        contributions
            .entry((external.child_id, external.formal.clone()))
            .or_default()
            .push(external.contribution);
    }
    // Fixed point: an interconnect connected to another interconnect
    // contributes the type it has merged so far, and takes its peer's.
    let elements_of = |modules: &HashMap<ModuleId, LoweredSvModule>, key: &VarKey| {
        variable_of(modules, key).map_or(1, |variable| {
            variable.array_dims.iter().product::<usize>().max(1)
        })
    };
    let mut merged: HashMap<VarKey, Merged> = HashMap::default();
    for (&id, module) in modules.iter() {
        for name in &module.interconnect_names {
            let key = (id, name.clone());
            merged.insert(
                key.clone(),
                merge(
                    name,
                    contributions.get(&key).map(Vec::as_slice).unwrap_or(&[]),
                    elements_of(modules, &key),
                )?,
            );
        }
    }
    for _ in 0..merged.len() + 1 {
        let mut changed = false;
        let targets: Vec<VarKey> = merged.keys().cloned().collect();
        for target in &targets {
            // Every edge into the target contributes the type its peer
            // has merged so far, over the range the connection covers.
            let mut all = contributions.get(target).cloned().unwrap_or_default();
            all.extend(edge_contributions(target, &edges, &merged, &|_| true));
            for concat in &concats {
                all.extend(concat_contributions(concat, target, &merged)?);
            }
            let next = merge(&target.1, &all, elements_of(modules, target))?;
            let current = merged.get(target).expect("interconnect merged");
            if *current != next {
                merged.insert(target.clone(), next);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    // Apply the merged types, and reject nets that no connection typed:
    // the simulated net may not remain an interconnect (23.3.3.7.1).
    for (&id, module) in modules.iter_mut() {
        let names: Vec<String> = module.interconnect_names.iter().cloned().collect();
        let mut scales: HashMap<String, (usize, usize)> = HashMap::default();
        for name in names {
            let Some(found) = merged.get(&(id, name.clone())) else {
                continue;
            };
            // A shaped declaration alone does not resolve the net:
            // the simulated net may not remain an interconnect at the
            // end of elaboration (23.3.3.7.1).
            if found.ty.is_none() || found.resolution == NetResolution::Interconnect {
                return Err(sv::AnalyzerError::Unsupported(format!(
                    "interconnect net `{name}` does not resolve to a net type"
                )));
            }
            let ty = found.ty.as_ref().expect("resolved type");
            let resolution = found.resolution;
            module
                .interconnect_resolutions
                .insert(name.clone(), resolution);
            if !found.element_resolutions.is_empty() {
                module
                    .interconnect_element_resolutions
                    .insert(name.clone(), found.element_resolutions.clone());
            }
            let Some(&var_id) = module.signal_names.get(&name) else {
                continue;
            };
            let Some(variable) = module.variables.get_mut(&var_id) else {
                continue;
            };
            let elements: usize = variable.array_dims.iter().product::<usize>().max(1);
            if elements > 1 {
                scales.insert(
                    name.clone(),
                    (variable.width / elements, ty.width / elements),
                );
            }
            variable.width = ty.width;
            variable.signed = ty.signed;
            variable.float = ty.float;
            variable.is_4state = ty.is_4state;
            variable.value_kind = ty.value_kind;
            variable.type_kind = ty.type_kind;
            variable.packed_ranges = if elements > 1 {
                let element = ty.width / elements;
                if element > 1 {
                    vec![(element as i128 - 1, 0)]
                } else {
                    Vec::new()
                }
            } else {
                ty.packed_ranges.clone()
            };
            let symbolic = variable.to_symbolic_variable();
            module.sim_module.variables.insert(var_id, symbolic);
            let full = (BigUint::from(1u8) << ty.width) - BigUint::from(1u8);
            for initial in module
                .sim_module
                .initial_memory_values
                .iter_mut()
                .chain(
                    module
                        .sim_module
                        .declaration_initial_memory_values
                        .iter_mut(),
                )
                .filter(|initial| initial.address == var_id)
            {
                if let InitialStateData::Packed {
                    mask, written_mask, ..
                } = &mut initial.data
                {
                    *mask = full.clone();
                    *written_mask = full.clone();
                }
            }
        }
        if !scales.is_empty() {
            for instance in &mut module.instances {
                for connection in &mut instance.port_connections {
                    if let Some(expr) = connection.actual_expr.take() {
                        connection.actual_expr = Some(rescale_selects(
                            expr,
                            &scales,
                            &module.constants,
                            &module.parameter_types,
                        ));
                    }
                }
            }
        }
    }
    Ok(())
}
