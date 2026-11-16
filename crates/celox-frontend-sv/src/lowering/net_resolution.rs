//! Nets that several continuous assignments drive on the same bits.
//!
//! Each continuous assignment to a net is a driver of the bits it assigns,
//! and the net type resolves the values of the drivers of a bit (IEEE
//! 1800-2023 6.5, 6.6): Table 6-2 for `wire` and `tri`, Table 6-3 for `wand`
//! and `triand`, Table 6-4 for `wor` and `trior`. The scheduler wants one
//! definition per bit, so the drivers of such a net are joined into one
//! path per range of bits with the same drivers.
use super::procedural::slt_error;
use super::*;
use sv::ast::NetResolution;

/// The nets of a module that continuous assignments of different processes
/// drive on common bits, and the processes that drive them.
#[derive(Default)]
pub(super) struct ResolvedNets {
    nets: HashMap<String, NetResolution>,
    /// The continuous assignments that drive those nets.
    drivers: HashSet<usize>,
}

impl ResolvedNets {
    /// The nets of `module` whose drivers are joined. A net stays with the
    /// single-driver rule (and its "multiple net drivers" error) when a
    /// driver is not a plain continuous assignment, states a drive strength
    /// (10.3.4), or assigns a bit twice by itself. So does, in a two-state
    /// build, a net with an X or Z literal in a driver: the values of such
    /// a build cannot carry the high impedance that yields to another
    /// driver.
    pub(super) fn of(module: &sv::ir::Module, drivers: &ModuleDrivers, four_state: bool) -> Self {
        let mut resolved = Self::default();
        let processes = module.comb_processes();
        // A driver that repeats another one is not among `drivers`; one
        // with a drive strength still decides against resolving its net.
        let strong_repeats: HashSet<&str> = redundant_net_drivers(module)
            .into_iter()
            .filter(|&process| processes[process].has_drive_strength())
            .flat_map(|process| processes[process].body())
            .filter_map(|statement| match statement {
                sv::ir::Stmt::Assign { lhs, .. } => Some(lhs.name()),
                _ => None,
            })
            .collect();
        let nets = module
            .signals()
            .iter()
            .filter(|signal| {
                signal.is_net() && !signal.is_unresolved_net() && signal.hierarchical().is_none()
            })
            .map(|signal| (signal.name(), signal.net_resolution()))
            .chain(
                module
                    .ports()
                    .iter()
                    .filter(|port| port.is_net() && !port.is_unresolved_net())
                    .map(|port| (port.name(), port.net_resolution())),
            );
        for (name, resolution) in nets {
            if resolution == NetResolution::Single
                || resolved.nets.contains_key(name)
                || strong_repeats.contains(name)
            {
                continue;
            }
            let continuous = drivers.continuous(name);
            let overlap = |same: bool| {
                continuous.iter().enumerate().any(|(index, left)| {
                    continuous[index + 1..].iter().any(|right| {
                        (left.0 == right.0) == same && net_driver_ranges_overlap(left.1, right.1)
                    })
                })
            };
            if !overlap(false) || overlap(true) {
                continue;
            }
            let participants: HashSet<usize> = continuous.iter().map(|driver| driver.0).collect();
            if drivers
                .local(name)
                .iter()
                .any(|driver| !participants.contains(&driver.0))
                || participants.iter().any(|&process| {
                    let process = &processes[process];
                    process.has_drive_strength()
                        || (!four_state
                            && process.body().iter().any(|statement| match statement {
                                sv::ir::Stmt::Assign { rhs, .. }
                                | sv::ir::Stmt::AssignConcat { rhs, .. } => {
                                    needs_state_mode_rewrite(rhs, false)
                                }
                                _ => true,
                            }))
                })
            {
                continue;
            }
            resolved.nets.insert(name.to_string(), resolution);
            resolved.drivers.extend(participants);
        }
        resolved
    }

    /// Whether continuous assignment `process` drives a resolved net.
    pub(super) fn drives(&self, process: usize) -> bool {
        self.drivers.contains(&process)
    }

    /// The resolution of net `name`, if its drivers are joined.
    pub(super) fn net(&self, name: &str) -> Option<NetResolution> {
        self.nets.get(name).copied()
    }

    pub(super) fn names(&self) -> impl Iterator<Item = &str> {
        self.nets.keys().map(String::as_str)
    }
}

/// The error of a net whose drivers cannot be joined.
pub(super) fn multiple_drivers(name: &str) -> sv::AnalyzerError {
    sv::AnalyzerError::Unsupported(format!("multiple net drivers for `{name}`"))
}

/// Join the `drivers` of net `id` into one path per range of bits with the
/// same drivers. `planes` resolves four-state values; without it the drivers
/// hold no X or Z (a two-state build) and the tables reduce to a bitwise AND
/// or OR. `two_state` stores a four-state resolved value in a two-state net.
pub(super) fn join(
    arena: &mut SLTNodeArena<SourceVarId>,
    id: SourceVarId,
    resolution: NetResolution,
    drivers: Vec<LogicPath<SourceVarId>>,
    planes: bool,
    two_state: bool,
) -> Result<Vec<LogicPath<SourceVarId>>, sv::AnalyzerError> {
    let range = |path: &LogicPath<SourceVarId>| match &path.target {
        LogicPathTarget::Var(target) => target.access,
        _ => unreachable!("net drivers are variable paths"),
    };
    let mut cuts: Vec<usize> = drivers
        .iter()
        .flat_map(|path| [range(path).lsb, range(path).msb + 1])
        .collect();
    cuts.sort_unstable();
    cuts.dedup();
    let mut joined = Vec::new();
    for bounds in cuts.windows(2) {
        let (lsb, msb) = (bounds[0], bounds[1] - 1);
        let covering: Vec<_> = drivers
            .iter()
            .filter(|path| range(path).lsb <= lsb && msb <= range(path).msb)
            .collect();
        let Some(first) = covering.first() else {
            continue;
        };
        let mut values = Vec::with_capacity(covering.len());
        for path in &covering {
            let access = range(path);
            values.push(if access.lsb == lsb && access.msb == msb {
                path.expr
            } else {
                arena
                    .alloc(SLTNode::Slice {
                        expr: path.expr,
                        access: BitAccess::new(lsb - access.lsb, msb - access.lsb),
                    })
                    .map_err(slt_error)?
            });
        }
        let mut expr = match values.as_slice() {
            [value] => *value,
            _ if planes => resolve_planes(arena, resolution, &values, msb - lsb + 1)?,
            _ => {
                let op = if resolution == NetResolution::Or {
                    BinaryOp::Or
                } else {
                    BinaryOp::And
                };
                let mut resolved = values[0];
                for value in &values[1..] {
                    resolved = arena
                        .alloc(SLTNode::Binary(resolved, op, *value))
                        .map_err(slt_error)?;
                }
                resolved
            }
        };
        if two_state && planes {
            expr = arena
                .alloc(SLTNode::Unary(UnaryOp::ToTwoState, expr))
                .map_err(slt_error)?;
        }
        let mut path = LogicPath {
            target: LogicPathTarget::Var(VarAtomBase::new(id, lsb, msb)),
            transparent_copy: covering.len() == 1 && first.transparent_copy && expr == first.expr,
            sources: HashSet::default(),
            previous_sources: HashSet::default(),
            address_sources: HashSet::default(),
            local_inputs: Vec::new(),
            order_before: HashSet::default(),
            comb_capture_enable_sites: Vec::new(),
            comb_capture_enable_always: false,
            pre_lower_nodes: Vec::new(),
            expr,
        };
        for driver in covering {
            path.sources.extend(driver.sources.iter().copied());
            path.previous_sources
                .extend(driver.previous_sources.iter().copied());
            path.address_sources
                .extend(driver.address_sources.iter().copied());
        }
        joined.push(path);
    }
    Ok(joined)
}

/// Whether a driver path is one [`join`] can combine: a plain value of a
/// range of bits.
pub(super) fn joinable(path: &LogicPath<SourceVarId>) -> bool {
    matches!(path.target, LogicPathTarget::Var(_))
        && path.local_inputs.is_empty()
        && path.order_before.is_empty()
        && path.comb_capture_enable_sites.is_empty()
        && !path.comb_capture_enable_always
        && path.pre_lower_nodes.is_empty()
}

fn binary(
    arena: &mut SLTNodeArena<SourceVarId>,
    left: NodeId,
    op: BinaryOp,
    right: NodeId,
) -> Result<NodeId, sv::AnalyzerError> {
    arena
        .alloc(SLTNode::Binary(left, op, right))
        .map_err(slt_error)
}

fn complement(
    arena: &mut SLTNodeArena<SourceVarId>,
    node: NodeId,
) -> Result<NodeId, sv::AnalyzerError> {
    arena
        .alloc(SLTNode::Unary(UnaryOp::BitNot, node))
        .map_err(slt_error)
}

/// The resolved value of `values`, each `width` bits, on their value and
/// mask planes. A bit with mask 0 is known, mask 1 with value 1 is X and
/// mask 1 with value 0 is Z.
fn resolve_planes(
    arena: &mut SLTNodeArena<SourceVarId>,
    resolution: NetResolution,
    values: &[NodeId],
    width: usize,
) -> Result<NodeId, sv::AnalyzerError> {
    use BinaryOp::{And, Or, Xor};
    let padded = width.next_multiple_of(64);
    let mut planes = Vec::with_capacity(values.len());
    for value in values {
        let value = coerce_node_width(arena, *value, Some(padded), false).map_err(slt_error)?;
        let both = arena
            .alloc(SLTNode::Unary(UnaryOp::ToPlanes, value))
            .map_err(slt_error)?;
        let mut plane = |lsb| {
            arena
                .alloc(SLTNode::Slice {
                    expr: both,
                    access: BitAccess::new(lsb, lsb + padded - 1),
                })
                .map_err(slt_error)
        };
        planes.push((plane(0)?, plane(padded)?));
    }
    let (mut value, mut mask) = planes[0];
    for &(vb, mb) in &planes[1..] {
        let (va, ma) = (value, mask);
        let not_va = complement(arena, va)?;
        let not_vb = complement(arena, vb)?;
        // Z: mask without value.
        let za = binary(arena, ma, And, not_va)?;
        let zb = binary(arena, mb, And, not_vb)?;
        // Two drivers that are not Z conflict unless both are known and equal.
        let differ = binary(arena, va, Xor, vb)?;
        let unknown = binary(arena, ma, Or, mb)?;
        let unequal = binary(arena, unknown, Or, differ)?;
        let either_z = binary(arena, za, Or, zb)?;
        let both_drive = complement(arena, either_z)?;
        let conflict = binary(arena, both_drive, And, unequal)?;
        // A Z driver yields to the other one, whose mask the result takes.
        let from_b = binary(arena, za, And, mb)?;
        let from_a = binary(arena, zb, And, ma)?;
        let yielded = binary(arena, from_b, Or, from_a)?;
        mask = binary(arena, yielded, Or, conflict)?;
        // The value plane of a Z is 0, so `va | vb` is the other driver's
        // value when one is Z, and the common value of two equal ones.
        let either = binary(arena, va, Or, vb)?;
        value = binary(arena, either, Or, conflict)?;
        // A known 0 decides a wired AND (Table 6-3) and a known 1 a wired
        // OR (Table 6-4), whatever the other driver is.
        let decided = match resolution {
            NetResolution::And => {
                let known_a = binary(arena, ma, Or, va)?;
                let known_b = binary(arena, mb, Or, vb)?;
                let neither = binary(arena, known_a, And, known_b)?;
                Some((complement(arena, neither)?, false))
            }
            NetResolution::Or => {
                let not_ma = complement(arena, ma)?;
                let not_mb = complement(arena, mb)?;
                let one_a = binary(arena, not_ma, And, va)?;
                let one_b = binary(arena, not_mb, And, vb)?;
                Some((binary(arena, one_a, Or, one_b)?, true))
            }
            NetResolution::Wire | NetResolution::Single => None,
        };
        if let Some((decided, one)) = decided {
            let open = complement(arena, decided)?;
            mask = binary(arena, mask, And, open)?;
            value = if one {
                binary(arena, value, Or, decided)?
            } else {
                binary(arena, value, And, open)?
            };
        }
    }
    let both = arena
        .alloc(SLTNode::Concat(vec![(mask, padded), (value, padded)]))
        .map_err(slt_error)?;
    let resolved = arena
        .alloc(SLTNode::Unary(UnaryOp::FromPlanes, both))
        .map_err(slt_error)?;
    if padded == width {
        Ok(resolved)
    } else {
        arena
            .alloc(SLTNode::Slice {
                expr: resolved,
                access: BitAccess::new(0, width - 1),
            })
            .map_err(slt_error)
    }
}
