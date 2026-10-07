//! Symbolic execution of combinational processes into SLT.
//!
//! A process is executed statement by statement over a [`SymbolicStore`]
//! that maps each written variable to the SLT value it holds at the current
//! point. Branches fork the store and merge it with multiplexers; `break`,
//! `continue`, and `return` set hidden flag variables under which the rest of
//! a block runs. Loops whose iteration count is known are unrolled; counted
//! loops with run-time bounds become [`SLTNode::ForFold`] folds. Subroutine
//! calls are inlined: their arguments and locals are hidden variables.

use super::procedural::*;
use super::*;
use celox_slt::{RangeStore, SLTForFoldResult, SLTForUpdate, SLTLoopBound, SymbolicStore};
use num_traits::{ToPrimitive, Zero};

type Sources = HashSet<VarAtomBase<SourceVarId>>;
type Store = SymbolicStore<SourceVarId, NodeId>;
type Value = (NodeId, Sources);

#[derive(Clone)]
enum Frame {
    Loop {
        break_flag: SourceVarId,
        continue_flag: SourceVarId,
    },
    Function {
        return_flag: SourceVarId,
        return_var: Option<String>,
    },
}

pub(super) struct Comb<'p, 'a> {
    pub m: &'p mut ProcModule<'a>,
    pub arena: &'p mut SLTNodeArena<SourceVarId>,
    active_calls: Vec<String>,
    unrolled: usize,
    /// Whether nonblocking assignments are allowed (they are not in
    /// combinational processes).
    allow_nonblocking: bool,
    /// Branch conditions known on the current path, with their values.
    facts: Vec<(NodeId, bool, Sources)>,
    /// Other conditions that hold on the current path: no jump taken yet,
    /// an unrolled iteration running, a short-circuit operand evaluated.
    guards: Vec<Value>,
    /// Combinational runtime events, their sites, and the inputs their
    /// operands read.
    pub observers: Vec<CombObserver<SourceVarId>>,
    pub sites: Vec<RuntimeEventSite>,
    effect_sensitivity: Sources,
    /// The depth of run-time loop folds being executed.
    folding: usize,
    /// Whether the process is a continuous assignment, which drives fixed bits.
    pub continuous: bool,
    /// Whether the process is an `initial` block, which defines initial state.
    pub initial: bool,
}

fn range_error(error: impl std::fmt::Display) -> sv::AnalyzerError {
    unsupported(format!("procedural lowering: {error}"))
}

fn full(width: usize) -> BitAccess {
    BitAccess::new(0, width.saturating_sub(1))
}

impl<'p, 'a> Comb<'p, 'a> {
    pub fn new(m: &'p mut ProcModule<'a>, arena: &'p mut SLTNodeArena<SourceVarId>) -> Self {
        Self {
            m,
            arena,
            active_calls: Vec::new(),
            unrolled: 0,
            allow_nonblocking: false,
            facts: Vec::new(),
            guards: Vec::new(),
            observers: Vec::new(),
            sites: Vec::new(),
            effect_sensitivity: Sources::default(),
            folding: 0,
            continuous: false,
            initial: false,
        }
    }

    fn alloc(&mut self, node: SLTNode<SourceVarId>) -> Result<NodeId, sv::AnalyzerError> {
        self.arena.alloc(node).map_err(slt_error)
    }

    fn width(&self, node: NodeId) -> usize {
        celox_slt::get_width(node, self.arena)
    }

    fn constant(&mut self, value: u64, width: usize) -> Result<NodeId, sv::AnalyzerError> {
        slt_constant(self.arena, BigUint::from(value), width, false)
    }

    fn slice(&mut self, node: NodeId, access: BitAccess) -> Result<NodeId, sv::AnalyzerError> {
        let width = self.width(node);
        if access.lsb == 0 && access.msb + 1 == width {
            return Ok(node);
        }
        self.alloc(SLTNode::Slice { expr: node, access })
    }

    fn concat_lsb_first(
        &mut self,
        parts: Vec<(NodeId, usize)>,
    ) -> Result<NodeId, sv::AnalyzerError> {
        if parts.len() == 1 {
            return Ok(parts[0].0);
        }
        self.alloc(SLTNode::Concat(parts.into_iter().rev().collect()))
    }

    // ---------------------------------------------------------------- store

    /// The value of bits `access` of variable `id` at this point.
    fn read(
        &mut self,
        store: &Store,
        id: SourceVarId,
        access: BitAccess,
    ) -> Result<Value, sv::AnalyzerError> {
        let signed =
            self.m.var(id).signed && access.lsb == 0 && access.msb + 1 == self.m.var(id).width;
        let Some(range) = store.get(&id) else {
            let node = self.alloc(SLTNode::Input {
                variable: id,
                signed,
                index: Vec::new(),
                access,
            })?;
            return Ok((
                node,
                [VarAtomBase::new(id, access.lsb, access.msb)]
                    .into_iter()
                    .collect(),
            ));
        };
        let parts = range.get_parts(access).map_err(range_error)?;
        let mut nodes = Vec::with_capacity(parts.len());
        let mut sources = Sources::default();
        let mut lsb = access.lsb;
        for (value, relative) in parts {
            let part_width = relative.msb - relative.lsb + 1;
            let node = match value {
                Some((node, part_sources)) => {
                    sources.extend(part_sources.iter().copied());
                    self.slice(node, relative)?
                }
                None => {
                    let part = BitAccess::new(lsb, lsb + part_width - 1);
                    sources.insert(VarAtomBase::new(id, part.lsb, part.msb));
                    self.alloc(SLTNode::Input {
                        variable: id,
                        signed: signed && part_width == access.msb - access.lsb + 1,
                        index: Vec::new(),
                        access: part,
                    })?
                }
            };
            nodes.push((node, part_width));
            lsb += part_width;
        }
        Ok((self.concat_lsb_first(nodes)?, sources))
    }

    fn write(
        &mut self,
        store: &mut Store,
        id: SourceVarId,
        access: BitAccess,
        value: Value,
    ) -> Result<(), sv::AnalyzerError> {
        let width = self.m.var(id).width;
        debug_assert_eq!(self.width(value.0), access.msb - access.lsb + 1);
        store
            .entry(id)
            .or_insert_with(|| RangeStore::new(None, width))
            .update(access, Some(value))
            .map_err(range_error)
    }

    fn set_flag(
        &mut self,
        store: &mut Store,
        id: SourceVarId,
        value: bool,
    ) -> Result<(), sv::AnalyzerError> {
        let node = self.constant(u64::from(value), 1)?;
        self.write(store, id, full(1), (node, Sources::default()))
    }

    /// The default value of a variable: all X for four-state, zero otherwise.
    fn default_value(&mut self, id: SourceVarId) -> Result<NodeId, sv::AnalyzerError> {
        let var = self.m.var(id);
        let width = var.width;
        if var.is_4state && self.m.four_state {
            slt_unknown(self.arena, width)
        } else {
            self.constant(0, width)
        }
    }

    fn flag(&mut self, purpose: &str) -> SourceVarId {
        self.m.temp(purpose, 1, false, false).0
    }

    // ---------------------------------------------------------------- merge

    /// Merge two branch versions: `then_store` where `condition` holds.
    fn merge(
        &mut self,
        condition: &Value,
        then_store: &Store,
        else_store: &Store,
    ) -> Result<Store, sv::AnalyzerError> {
        if let Some(value) = slt_bool(self.arena, condition.0) {
            return Ok(if value {
                then_store.fork()
            } else {
                else_store.fork()
            });
        }
        let mut merged = then_store.fork();
        let keys: Vec<SourceVarId> = then_store.differing_keys(else_store).copied().collect();
        for id in keys {
            // A variable written on only one side keeps its entry value on
            // the other.
            let width = self.m.var(id).width;
            let unwritten = RangeStore::new(None, width);
            let then_range = then_store.get(&id).unwrap_or(&unwritten);
            let else_range = else_store.get(&id).unwrap_or(&unwritten);
            if then_range == else_range {
                continue;
            }
            let parts = then_range.aligned_parts(else_range).map_err(range_error)?;
            let parts: Vec<_> = parts
                .into_iter()
                .map(
                    |(access, (then_value, then_access), (else_value, else_access))| {
                        (
                            access,
                            then_value.clone(),
                            then_access,
                            else_value.clone(),
                            else_access,
                        )
                    },
                )
                .collect();
            let mut range = RangeStore {
                ranges: std::collections::BTreeMap::new(),
            };
            for (access, then_value, then_access, else_value, else_access) in parts {
                let value = if then_value.is_none() && else_value.is_none() {
                    None
                } else {
                    let (then_node, then_sources) =
                        self.materialize(id, access, then_value, then_access)?;
                    let (else_node, else_sources) =
                        self.materialize(id, access, else_value, else_access)?;
                    // Earlier merges on the same (or the complementary)
                    // condition are decided in each arm.
                    let (then_node, then_sources) =
                        self.simplify(then_node, then_sources, condition.0, true)?;
                    let (else_node, else_sources) =
                        self.simplify(else_node, else_sources, condition.0, false)?;
                    let mut sources = then_sources;
                    sources.extend(else_sources);
                    if then_node == else_node {
                        Some((then_node, sources))
                    } else {
                        sources.extend(condition.1.iter().copied());
                        let node = self.alloc(SLTNode::Mux {
                            cond: condition.0,
                            then_expr: then_node,
                            else_expr: else_node,
                        })?;
                        Some((node, sources))
                    }
                };
                range
                    .ranges
                    .insert(access.lsb, (value, access.msb - access.lsb + 1, access.lsb));
            }
            merged.insert(id, range);
        }
        Ok(merged)
    }

    /// Resolve the multiplexers of `node` whose condition is known when
    /// `condition` has `value`.
    fn simplify(
        &mut self,
        node: NodeId,
        sources: Sources,
        condition: NodeId,
        value: bool,
    ) -> Result<Value, sv::AnalyzerError> {
        let simplified = self.simplify_node(node, condition, value)?;
        if simplified == node {
            return Ok((node, sources));
        }
        match self.node_sources(simplified) {
            Some(sources) => Ok((simplified, sources)),
            None => Ok((node, sources)),
        }
    }

    fn simplify_node(
        &mut self,
        node: NodeId,
        condition: NodeId,
        value: bool,
    ) -> Result<NodeId, sv::AnalyzerError> {
        match self.arena.get(node).clone() {
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => {
                let known = if cond == condition {
                    Some(value)
                } else if self.complementary(cond, condition) {
                    Some(!value)
                } else {
                    None
                };
                match known {
                    Some(true) => self.simplify_node(then_expr, condition, value),
                    Some(false) => self.simplify_node(else_expr, condition, value),
                    None => Ok(node),
                }
            }
            SLTNode::Slice { expr, access } => {
                let inner = self.simplify_node(expr, condition, value)?;
                if inner == expr {
                    Ok(node)
                } else {
                    self.slice(inner, access)
                }
            }
            _ => Ok(node),
        }
    }

    /// The module-state reads of a tree, or `None` when it contains a fold.
    fn node_sources(&self, node: NodeId) -> Option<Sources> {
        let mut sources = Sources::default();
        let mut stack = vec![node];
        let mut seen = HashSet::default();
        while let Some(node) = stack.pop() {
            if !seen.insert(node) {
                continue;
            }
            match self.arena.get(node) {
                SLTNode::Input {
                    variable,
                    index,
                    access,
                    ..
                } => {
                    if index.is_empty() {
                        sources.insert(VarAtomBase::new(*variable, access.lsb, access.msb));
                    } else {
                        let width = self.m.var(*variable).width;
                        sources.insert(VarAtomBase::new(*variable, 0, width - 1));
                        stack.extend(index.iter().map(|entry| entry.node));
                    }
                }
                SLTNode::Constant(..) => {}
                SLTNode::Binary(left, _, right) => stack.extend([*left, *right]),
                SLTNode::Unary(_, inner)
                | SLTNode::Slice { expr: inner, .. }
                | SLTNode::Capture { expr: inner, .. } => stack.push(*inner),
                SLTNode::Mux {
                    cond,
                    then_expr,
                    else_expr,
                } => stack.extend([*cond, *then_expr, *else_expr]),
                SLTNode::Concat(parts) => stack.extend(parts.iter().map(|(part, _)| *part)),
                SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. } => return None,
            }
        }
        Some(sources)
    }

    fn materialize(
        &mut self,
        id: SourceVarId,
        absolute: BitAccess,
        value: Option<Value>,
        relative: BitAccess,
    ) -> Result<Value, sv::AnalyzerError> {
        match value {
            Some((node, sources)) => Ok((self.slice(node, relative)?, sources)),
            None => {
                let node = self.alloc(SLTNode::Input {
                    variable: id,
                    signed: false,
                    index: Vec::new(),
                    access: absolute,
                })?;
                Ok((
                    node,
                    [VarAtomBase::new(id, absolute.lsb, absolute.msb)]
                        .into_iter()
                        .collect(),
                ))
            }
        }
    }

    // ----------------------------------------------------------- expressions

    /// Replace each local whose value is a known constant by a literal, so
    /// selects and loop conditions over it fold.
    fn propagate(&mut self, store: &Store, expr: &sv::ir::Expr) -> sv::ir::Expr {
        let mut names = HashSet::default();
        expr_idents(expr, &mut names);
        let mut literals: HashMap<String, (String, i128)> = HashMap::default();
        for name in names {
            let Some(id) = self.m.id(&name) else { continue };
            let Some(range) = store.get(&id) else {
                continue;
            };
            let var = self.m.var(id);
            let (width, signed) = (var.width, var.signed);
            let Ok(parts) = range.get_parts(full(width)) else {
                continue;
            };
            let [(Some((node, _)), relative)] = parts.as_slice() else {
                continue;
            };
            if relative.lsb != 0 || relative.msb + 1 != width || self.width(*node) != width {
                continue;
            }
            let Some((value, _)) = slt_const(self.arena, *node) else {
                continue;
            };
            let numeric = if signed && width > 0 && value.bit(width as u64 - 1) {
                (num_bigint::BigInt::from(value.clone()) - (num_bigint::BigInt::from(1u8) << width))
                    .to_i128()
            } else {
                value.to_i128()
            };
            let Some(numeric) = numeric else { continue };
            literals.insert(name, (typed_literal(&value, width, signed), numeric));
        }
        if literals.is_empty() {
            return expr.clone();
        }
        substitute_literals(expr, &literals)
    }

    fn propagate_lvalue(&mut self, store: &Store, lvalue: &sv::ir::LValue) -> sv::ir::LValue {
        match lvalue {
            sv::ir::LValue::Ident(_) => lvalue.clone(),
            sv::ir::LValue::Select {
                name,
                msb,
                lsb,
                array_slice_width,
                array_slice_reversed,
            } => {
                let probe = sv::ir::Expr::Select {
                    expr: Box::new(sv::ir::Expr::Literal("0".to_string())),
                    msb: msb.clone(),
                    lsb: lsb.clone(),
                    signed: false,
                };
                let sv::ir::Expr::Select { msb, lsb, .. } = self.propagate(store, &probe) else {
                    unreachable!()
                };
                sv::ir::LValue::Select {
                    name: name.clone(),
                    msb,
                    lsb,
                    array_slice_width: array_slice_width.clone(),
                    array_slice_reversed: *array_slice_reversed,
                }
            }
        }
    }

    /// Evaluate an expression in the current store.
    fn eval(
        &mut self,
        store: &mut Store,
        frames: &[Frame],
        expr: &sv::ir::Expr,
        context: Option<(usize, bool)>,
    ) -> Result<Value, sv::AnalyzerError> {
        let expr = expr_for_state_mode(expr, self.m.four_state);
        let expr = if self.m.calls(&expr) {
            self.hoist(store, frames, &expr)?
        } else {
            expr
        };
        let expr = self.propagate(store, &expr);
        let (node, _) = if let (sv::ir::Expr::Literal(literal), Some((width, _))) = (&expr, context)
            && let Some(fill) = unbased_fill_literal(literal)
        {
            (
                lower_unbased_fill_literal_slt(self.arena, fill, width)
                    .ok_or_else(|| unsupported(format!("expression `{literal}`")))?,
                Sources::default(),
            )
        } else {
            lower_expr_with_context(
                &expr,
                self.m.variables,
                self.m.name_to_id,
                self.m.constants,
                self.m.parameter_types,
                self.arena,
                context.map(|(width, _)| width),
                context.map(|(_, signed)| signed),
            )
            .ok_or_else(|| unsupported("combinational expression"))?
        };
        let mut memo = HashMap::default();
        self.resolve(store, node, &mut memo)
    }

    fn expr_signed(&self, expr: &sv::ir::Expr) -> bool {
        self.m.expr_signed(expr)
    }

    /// Rewrite the module-state reads of a freshly lowered tree to the values
    /// the store holds.
    fn resolve(
        &mut self,
        store: &Store,
        node: NodeId,
        memo: &mut HashMap<NodeId, Value>,
    ) -> Result<Value, sv::AnalyzerError> {
        if let Some(value) = memo.get(&node) {
            return Ok(value.clone());
        }
        let value = match self.arena.get(node).clone() {
            SLTNode::Input {
                variable,
                signed,
                index,
                access,
            } => {
                if index.is_empty() {
                    if store.get(&variable).is_some_and(|range| {
                        range
                            .get_parts_ref(access)
                            .is_ok_and(|parts| parts.iter().any(|(value, _)| value.is_some()))
                    }) {
                        let (value, sources) = self.read(store, variable, access)?;
                        let _ = signed;
                        (value, sources)
                    } else {
                        (
                            node,
                            [VarAtomBase::new(variable, access.lsb, access.msb)]
                                .into_iter()
                                .collect(),
                        )
                    }
                } else {
                    let mut sources = Sources::default();
                    let mut resolved_index = Vec::with_capacity(index.len());
                    for entry in &index {
                        let (index_node, index_sources) = self.resolve(store, entry.node, memo)?;
                        sources.extend(index_sources);
                        resolved_index.push(SLTIndex {
                            node: index_node,
                            stride: entry.stride,
                            kind: entry.kind,
                        });
                    }
                    let var_width = self.m.var(variable).width;
                    let written = store.get(&variable).is_some_and(|range| {
                        range.ranges.values().any(|(value, _, _)| value.is_some())
                    });
                    if written {
                        // Read the selected bits out of the current value.
                        let (whole, whole_sources) = self.read(store, variable, full(var_width))?;
                        sources.extend(whole_sources);
                        let offset_width = 64;
                        let mut offset = self.constant(access.lsb as u64, offset_width)?;
                        for entry in &resolved_index {
                            let index_node = coerce_node_width(
                                self.arena,
                                entry.node,
                                Some(offset_width),
                                false,
                            )
                            .map_err(slt_error)?;
                            let stride = self.constant(entry.stride as u64, offset_width)?;
                            let scaled =
                                self.alloc(SLTNode::Binary(index_node, BinaryOp::Mul, stride))?;
                            offset = self.alloc(SLTNode::Binary(offset, BinaryOp::Add, scaled))?;
                        }
                        let offset = coerce_node_width(
                            self.arena,
                            offset,
                            Some(var_width.max(offset_width)),
                            false,
                        )
                        .map_err(slt_error)?;
                        let whole = coerce_node_width(
                            self.arena,
                            whole,
                            Some(var_width.max(offset_width)),
                            false,
                        )
                        .map_err(slt_error)?;
                        let shifted = self.alloc(SLTNode::Binary(whole, BinaryOp::Shr, offset))?;
                        let value =
                            self.slice(shifted, BitAccess::new(0, access.msb - access.lsb))?;
                        (value, sources)
                    } else {
                        sources.insert(VarAtomBase::new(variable, 0, var_width.saturating_sub(1)));
                        let rebuilt = if resolved_index
                            .iter()
                            .zip(&index)
                            .all(|(new, old)| new.node == old.node)
                        {
                            node
                        } else {
                            self.alloc(SLTNode::Input {
                                variable,
                                signed,
                                index: resolved_index,
                                access,
                            })?
                        };
                        (rebuilt, sources)
                    }
                }
            }
            SLTNode::Constant(..) => (node, Sources::default()),
            SLTNode::Binary(left, op, right) => {
                let (l, mut sources) = self.resolve(store, left, memo)?;
                let (r, r_sources) = self.resolve(store, right, memo)?;
                sources.extend(r_sources);
                let rebuilt = if l == left && r == right {
                    node
                } else {
                    self.alloc(SLTNode::Binary(l, op, r))?
                };
                (rebuilt, sources)
            }
            SLTNode::Unary(op, inner) => {
                let (i, sources) = self.resolve(store, inner, memo)?;
                let rebuilt = if i == inner {
                    node
                } else {
                    self.alloc(SLTNode::Unary(op, i))?
                };
                (rebuilt, sources)
            }
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => {
                let (c, mut sources) = self.resolve(store, cond, memo)?;
                let (t, t_sources) = self.resolve(store, then_expr, memo)?;
                let (e, e_sources) = self.resolve(store, else_expr, memo)?;
                sources.extend(t_sources);
                sources.extend(e_sources);
                let rebuilt = if c == cond && t == then_expr && e == else_expr {
                    node
                } else {
                    self.alloc(SLTNode::Mux {
                        cond: c,
                        then_expr: t,
                        else_expr: e,
                    })?
                };
                (rebuilt, sources)
            }
            SLTNode::Concat(parts) => {
                let mut sources = Sources::default();
                let mut changed = false;
                let mut resolved = Vec::with_capacity(parts.len());
                for (part, width) in &parts {
                    let (p, part_sources) = self.resolve(store, *part, memo)?;
                    changed |= p != *part;
                    sources.extend(part_sources);
                    resolved.push((p, *width));
                }
                let rebuilt = if changed {
                    self.alloc(SLTNode::Concat(resolved))?
                } else {
                    node
                };
                (rebuilt, sources)
            }
            SLTNode::Slice { expr, access } => {
                // A constant select of a variable reads only the selected bits.
                if let SLTNode::Input {
                    variable,
                    index,
                    access: input_access,
                    ..
                } = self.arena.get(expr).clone()
                    && index.is_empty()
                    && access.msb + input_access.lsb <= input_access.msb
                {
                    let absolute = BitAccess::new(
                        input_access.lsb + access.lsb,
                        input_access.lsb + access.msb,
                    );
                    let value = self.read(store, variable, absolute)?;
                    memo.insert(node, value.clone());
                    return Ok(value);
                }
                let (e, sources) = self.resolve(store, expr, memo)?;
                let rebuilt = if e == expr {
                    node
                } else {
                    self.alloc(SLTNode::Slice { expr: e, access })?
                };
                (rebuilt, sources)
            }
            SLTNode::Capture { expr, key } => {
                let (e, sources) = self.resolve(store, expr, memo)?;
                let rebuilt = if e == expr {
                    node
                } else {
                    self.alloc(SLTNode::Capture { expr: e, key })?
                };
                (rebuilt, sources)
            }
            SLTNode::ForFold { .. } | SLTNode::ForFoldGroup { .. } => {
                return Err(unsupported("loop fold in an expression"));
            }
        };
        memo.insert(node, value.clone());
        Ok(value)
    }

    /// Execute the user subroutine calls of an expression, left to right, and
    /// replace each by a hidden variable holding its result. Calls in an
    /// operand that short-circuit evaluation may skip run only when it is
    /// evaluated (IEEE 1800-2023 11.4.7, 11.4.11).
    fn hoist(
        &mut self,
        store: &mut Store,
        frames: &[Frame],
        expr: &sv::ir::Expr,
    ) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        use sv::ir::Expr;
        if !self.m.calls(expr) {
            return Ok(expr.clone());
        }
        Ok(match expr {
            Expr::Call { name, .. } if self.m.dpi_imports.contains_key(name) => {
                return Err(unsupported(format!(
                    "DPI-C function `{name}` called in combinational logic"
                )));
            }
            Expr::Call { name, args } if self.m.subroutines.contains_key(name) => {
                let args: Vec<Option<Expr>> = args.iter().cloned().map(Some).collect();
                let result = self.call(store, frames, name, &args)?;
                let Some((node, sources)) = result else {
                    return Err(unsupported(format!(
                        "void function `{name}` used as a value"
                    )));
                };
                let subroutine = self
                    .m
                    .subroutine(name)
                    .cloned()
                    .ok_or_else(|| unsupported(format!("function `{name}`")))?;
                let (width, signed, is_4state) = match &subroutine.return_type {
                    Some(r#type) => self.m.type_shape(r#type)?,
                    None => (1, false, false),
                };
                let (temp, temp_name) = self.m.temp("call", width, signed, is_4state);
                let node =
                    coerce_node_width(self.arena, node, Some(width), signed).map_err(slt_error)?;
                self.write(store, temp, full(width), (node, sources))?;
                Expr::Ident(temp_name)
            }
            Expr::Call { name, args } => Expr::Call {
                name: name.clone(),
                args: args
                    .iter()
                    .map(|arg| self.hoist(store, frames, arg))
                    .collect::<Result<_, _>>()?,
            },
            Expr::Binary { left, op, right }
                if matches!(op, sv::ir::BinaryOp::LogicAnd | sv::ir::BinaryOp::LogicOr)
                    && self.m.calls(right) =>
            {
                let left = self.hoist(store, frames, left)?;
                let left_value = self.eval(store, frames, &left, None)?;
                let truth = slt_truth(self.arena, left_value.0)?;
                let guard = if *op == sv::ir::BinaryOp::LogicAnd {
                    truth
                } else {
                    slt_not(self.arena, truth)?
                };
                let guard = (guard, left_value.1);
                let mut taken = store.fork();
                self.guards.push(guard.clone());
                let right = self.hoist(&mut taken, frames, right);
                self.guards.pop();
                let right = right?;
                *store = self.merge(&guard, &taken, store)?;
                Expr::Binary {
                    left: Box::new(left),
                    op: *op,
                    right: Box::new(right),
                }
            }
            Expr::Mux {
                condition,
                then_expr,
                else_expr,
            } if self.m.calls(then_expr) || self.m.calls(else_expr) => {
                let condition = self.hoist(store, frames, condition)?;
                let condition_value = self.eval(store, frames, &condition, None)?;
                let truth = slt_truth(self.arena, condition_value.0)?;
                // An unknown condition evaluates both arms.
                let unknown = if self.m.four_state {
                    let known =
                        self.alloc(SLTNode::Unary(UnaryOp::ToTwoState, condition_value.0))?;
                    let is_known =
                        self.alloc(SLTNode::Binary(condition_value.0, BinaryOp::EqCase, known))?;
                    Some(slt_not(self.arena, is_known)?)
                } else {
                    None
                };
                let then_guard = match unknown {
                    Some(unknown) => slt_or(self.arena, truth, unknown)?,
                    None => truth,
                };
                let not_truth = slt_not(self.arena, truth)?;
                let else_guard = match unknown {
                    Some(unknown) => slt_or(self.arena, not_truth, unknown)?,
                    None => not_truth,
                };
                let mut then_store = store.fork();
                self.guards.push((then_guard, condition_value.1.clone()));
                let then_expr = self.hoist(&mut then_store, frames, then_expr);
                self.guards.pop();
                let then_expr = then_expr?;
                *store =
                    self.merge(&(then_guard, condition_value.1.clone()), &then_store, store)?;
                let mut else_store = store.fork();
                self.guards.push((else_guard, condition_value.1.clone()));
                let else_expr = self.hoist(&mut else_store, frames, else_expr);
                self.guards.pop();
                let else_expr = else_expr?;
                *store = self.merge(&(else_guard, condition_value.1), &else_store, store)?;
                Expr::Mux {
                    condition: Box::new(condition),
                    then_expr: Box::new(then_expr),
                    else_expr: Box::new(else_expr),
                }
            }
            Expr::Select {
                expr,
                msb,
                lsb,
                signed,
            } => Expr::Select {
                expr: Box::new(self.hoist(store, frames, expr)?),
                msb: msb.clone(),
                lsb: lsb.clone(),
                signed: *signed,
            },
            Expr::Concat(parts) => Expr::Concat(
                parts
                    .iter()
                    .map(|part| self.hoist(store, frames, part))
                    .collect::<Result<_, _>>()?,
            ),
            Expr::RepeatConcat { count, parts } => Expr::RepeatConcat {
                count: count.clone(),
                parts: parts
                    .iter()
                    .map(|part| self.hoist(store, frames, part))
                    .collect::<Result<_, _>>()?,
            },
            Expr::Resize {
                expr,
                width,
                signed,
            } => Expr::Resize {
                expr: Box::new(self.hoist(store, frames, expr)?),
                width: *width,
                signed: *signed,
            },
            Expr::Unary { op, expr } => Expr::Unary {
                op: *op,
                expr: Box::new(self.hoist(store, frames, expr)?),
            },
            Expr::Binary { left, op, right } => Expr::Binary {
                left: Box::new(self.hoist(store, frames, left)?),
                op: *op,
                right: Box::new(self.hoist(store, frames, right)?),
            },
            Expr::Mux {
                condition,
                then_expr,
                else_expr,
            } => Expr::Mux {
                condition: Box::new(self.hoist(store, frames, condition)?),
                then_expr: then_expr.clone(),
                else_expr: else_expr.clone(),
            },
            Expr::Inside { expr, items } => Expr::Inside {
                expr: Box::new(self.hoist(store, frames, expr)?),
                items: items
                    .iter()
                    .map(|item| -> Result<sv::ir::InsideItem, sv::AnalyzerError> {
                        Ok(match item {
                            sv::ir::InsideItem::Value(value) => {
                                sv::ir::InsideItem::Value(self.hoist(store, frames, value)?)
                            }
                            sv::ir::InsideItem::Range { low, high } => sv::ir::InsideItem::Range {
                                low: self.hoist(store, frames, low)?,
                                high: self.hoist(store, frames, high)?,
                            },
                        })
                    })
                    .collect::<Result<_, _>>()?,
            },
            Expr::Ident(_) | Expr::Literal(_) => expr.clone(),
        })
    }

    // ----------------------------------------------------------- assignments

    fn target_width(
        &mut self,
        store: &Store,
        lvalue: &sv::ir::LValue,
    ) -> Result<usize, sv::AnalyzerError> {
        Ok(match self.lvalue_target(store, lvalue)? {
            Target::Static { access, .. } => access.msb - access.lsb + 1,
            Target::Dynamic { width, .. } => width,
        })
    }

    fn lvalue_target(
        &mut self,
        store: &Store,
        lvalue: &sv::ir::LValue,
    ) -> Result<Target, sv::AnalyzerError> {
        let lvalue = self.propagate_lvalue(store, lvalue);
        let id = self
            .m
            .id(lvalue.name())
            .ok_or_else(|| unsupported(format!("assignment target `{}`", lvalue.name())))?;
        let var_width = self.m.var(id).width;
        let constants = self.m.constants;
        let parameter_types = self.m.parameter_types;
        match &lvalue {
            sv::ir::LValue::Ident(_) => Ok(Target::Static {
                id,
                access: full(var_width),
                lvalue,
            }),
            sv::ir::LValue::Select { msb, lsb, .. } => {
                let msb_value =
                    sv::typecheck::eval_const_expr_with_types(msb, constants, parameter_types);
                let lsb_value =
                    sv::typecheck::eval_const_expr_with_types(lsb, constants, parameter_types);
                if let (Some(msb), Some(lsb)) = (msb_value, lsb_value) {
                    let variable = self.m.var(id);
                    let (Some(msb), Some(lsb)) = (
                        packed_index_offset(variable, msb),
                        packed_index_offset(variable, lsb),
                    ) else {
                        // A constant select entirely outside the vector writes nothing.
                        return Ok(Target::Dynamic {
                            id,
                            offset: Offset::Nothing,
                            width: usize::try_from(msb.abs_diff(lsb)).unwrap_or(0) + 1,
                        });
                    };
                    return Ok(Target::Static {
                        id,
                        access: BitAccess::new(lsb.min(msb), lsb.max(msb)),
                        lvalue: lvalue.clone(),
                    });
                }
                if let Some((_, element_width, offset, access)) = dynamic_array_element_lvalue(
                    &lvalue,
                    self.m.variables,
                    self.m.name_to_id,
                    constants,
                    parameter_types,
                ) {
                    let element_count = var_width / element_width.max(1);
                    return Ok(Target::Dynamic {
                        id,
                        offset: Offset::Element {
                            offset,
                            element_width,
                            element_count,
                            access,
                        },
                        width: access.msb - access.lsb + 1,
                    });
                }
                if let Some(write) = dynamic_packed_write(
                    &lvalue,
                    self.m.variables,
                    self.m.name_to_id,
                    constants,
                    parameter_types,
                ) {
                    return Ok(Target::Dynamic {
                        id,
                        offset: Offset::Packed {
                            up: write.up,
                            down: write.down,
                        },
                        width: write.select_width,
                    });
                }
                Err(unsupported(format!(
                    "assignment target `{}`",
                    lvalue.name()
                )))
            }
        }
    }

    /// Assign `rhs` to `lhs`.
    fn assign(
        &mut self,
        store: &mut Store,
        frames: &[Frame],
        lhs: &sv::ir::LValue,
        rhs: &sv::ir::Expr,
    ) -> Result<(), sv::AnalyzerError> {
        let target = self.lvalue_target(store, lhs)?;
        let width = match &target {
            Target::Static { access, .. } => access.msb - access.lsb + 1,
            Target::Dynamic { width, .. } => *width,
        };
        if self.continuous && matches!(target, Target::Dynamic { .. }) {
            return Err(unsupported(format!(
                "combinational assignment target `{}` at a run-time position",
                lhs.name()
            )));
        }
        let signed = self.expr_signed(rhs);
        let value = self
            .eval(store, frames, rhs, Some((width, signed)))
            .map_err(|error| match error {
                sv::AnalyzerError::Unsupported(construct)
                    if construct == "combinational expression" =>
                {
                    unsupported(format!(
                        "combinational expression assigned to `{}`",
                        lhs.name()
                    ))
                }
                error => error,
            })?;
        self.store_value(store, frames, target, value, signed, rhs)
    }

    fn store_value(
        &mut self,
        store: &mut Store,
        frames: &[Frame],
        target: Target,
        value: Value,
        signed: bool,
        rhs: &sv::ir::Expr,
    ) -> Result<(), sv::AnalyzerError> {
        let (id, width) = match &target {
            Target::Static { id, access, .. } => (*id, access.msb - access.lsb + 1),
            Target::Dynamic { id, width, .. } => (*id, *width),
        };
        let (mut node, mut sources) = value;
        node = coerce_node_width(self.arena, node, Some(width), signed).map_err(slt_error)?;
        let target_two_state = !self.m.var(id).is_4state;
        if target_two_state || (!self.m.four_state && expr_is_unknown_literal(rhs)) {
            node = self.alloc(SLTNode::Unary(UnaryOp::ToTwoState, node))?;
        }
        match target {
            Target::Static { id, access, lvalue } => {
                let node = permute_reversed_lvalue_rhs_slt(
                    &lvalue,
                    node,
                    width,
                    self.m.constants,
                    self.m.parameter_types,
                    self.arena,
                )
                .ok_or_else(|| unsupported("assignment lvalue order"))?;
                self.write(store, id, access, (node, sources))
            }
            Target::Dynamic { id, offset, width } => {
                let var_width = self.m.var(id).width;
                let (current, current_sources) = self.read(store, id, full(var_width))?;
                // A partial dynamic update keeps the other bits; they are not a
                // self-dependency of the variable.
                sources.extend(current_sources.into_iter().filter(|source| source.id != id));
                let (up, down) = match offset {
                    Offset::Nothing => return Ok(()),
                    Offset::Element {
                        offset,
                        element_width,
                        element_count,
                        access,
                    } => {
                        let offset_expr = expr_from_const_expr(&offset)
                            .ok_or_else(|| unsupported("dynamic assignment index"))?;
                        let (bit_offset, offset_sources) =
                            self.eval(store, frames, &offset_expr, None)?;
                        sources.extend(offset_sources);
                        let wide = 64.max(var_width);
                        let bit_offset = coerce_node_width(self.arena, bit_offset, Some(64), false)
                            .map_err(slt_error)?;
                        // The element index; an unknown or out-of-range one writes nothing.
                        let stride = self.constant(element_width as u64, 64)?;
                        let index =
                            self.alloc(SLTNode::Binary(bit_offset, BinaryOp::DivU, stride))?;
                        let (_, valid) =
                            dynamic_array_index_guard_slt(self.arena, index, element_count)
                                .ok_or_else(|| unsupported("dynamic assignment index"))?;
                        let element_base =
                            self.alloc(SLTNode::Binary(index, BinaryOp::Mul, stride))?;
                        let lsb = self.constant(access.lsb as u64, 64)?;
                        let position =
                            self.alloc(SLTNode::Binary(element_base, BinaryOp::Add, lsb))?;
                        let outside = self.constant(var_width as u64, 64)?;
                        let position = self.alloc(SLTNode::Mux {
                            cond: valid,
                            then_expr: position,
                            else_expr: outside,
                        })?;
                        let position = coerce_node_width(self.arena, position, Some(wide), false)
                            .map_err(slt_error)?;
                        let zero = self.constant(0, wide)?;
                        (position, zero)
                    }
                    Offset::Packed { up, down } => {
                        let (up, up_sources) = self.eval(store, frames, &up, None)?;
                        let (down, down_sources) = self.eval(store, frames, &down, None)?;
                        sources.extend(up_sources);
                        sources.extend(down_sources);
                        (up, down)
                    }
                };
                let wide = var_width.max(self.width(up)).max(self.width(down));
                let mask_value = (BigUint::from(1u8) << width) - BigUint::from(1u8);
                let mask = slt_constant(self.arena, mask_value, wide, false)?;
                let value =
                    coerce_node_width(self.arena, node, Some(wide), false).map_err(slt_error)?;
                let current =
                    coerce_node_width(self.arena, current, Some(wide), false).map_err(slt_error)?;
                let up = coerce_node_width(self.arena, up, Some(wide), false).map_err(slt_error)?;
                let down =
                    coerce_node_width(self.arena, down, Some(wide), false).map_err(slt_error)?;
                let place = |this: &mut Self, value| -> Result<NodeId, sv::AnalyzerError> {
                    let raised = this.alloc(SLTNode::Binary(value, BinaryOp::Shl, up))?;
                    this.alloc(SLTNode::Binary(raised, BinaryOp::Shr, down))
                };
                let placed_mask = place(self, mask)?;
                let placed_value = place(self, value)?;
                let placed_value =
                    self.alloc(SLTNode::Binary(placed_value, BinaryOp::And, placed_mask))?;
                let keep = self.alloc(SLTNode::Unary(UnaryOp::BitNot, placed_mask))?;
                let kept = self.alloc(SLTNode::Binary(current, BinaryOp::And, keep))?;
                let mut updated = self.alloc(SLTNode::Binary(kept, BinaryOp::Or, placed_value))?;
                if self.m.four_state {
                    // A write at an unknown position is ignored (IEEE 1800-2023 11.5.1).
                    let mut known = None;
                    for position in [up, down] {
                        let two_state =
                            self.alloc(SLTNode::Unary(UnaryOp::ToTwoState, position))?;
                        let same =
                            self.alloc(SLTNode::Binary(position, BinaryOp::EqCase, two_state))?;
                        known = Some(match known {
                            None => same,
                            Some(other) => slt_and(self.arena, other, same)?,
                        });
                    }
                    if let Some(known) = known
                        && slt_bool(self.arena, known) != Some(true)
                    {
                        updated = self.alloc(SLTNode::Mux {
                            cond: known,
                            then_expr: updated,
                            else_expr: current,
                        })?;
                    }
                }
                let updated = self.slice(updated, full(var_width))?;
                self.write(store, id, full(var_width), (updated, sources))
            }
        }
    }

    fn assign_concat(
        &mut self,
        store: &mut Store,
        frames: &[Frame],
        parts: &[sv::ir::LValue],
        rhs: &sv::ir::Expr,
    ) -> Result<(), sv::AnalyzerError> {
        let mut widths = Vec::with_capacity(parts.len());
        for part in parts {
            widths.push(self.target_width(store, part)?);
        }
        let total: usize = widths.iter().sum();
        let signed = self.expr_signed(rhs);
        let (node, sources) = self.eval(store, frames, rhs, Some((total, signed)))?;
        let node = coerce_node_width(self.arena, node, Some(total), signed).map_err(slt_error)?;
        let mut lsb = total;
        for (part, width) in parts.iter().zip(widths) {
            lsb -= width;
            let slice = self.slice(node, BitAccess::new(lsb, lsb + width - 1))?;
            let target = self.lvalue_target(store, part)?;
            self.store_value(store, frames, target, (slice, sources.clone()), false, rhs)?;
        }
        Ok(())
    }

    /// Write `value` (already evaluated) to an output-argument lvalue.
    fn write_back(
        &mut self,
        store: &mut Store,
        frames: &[Frame],
        lvalues: &[sv::ir::LValue],
        value: Value,
        value_signed: bool,
    ) -> Result<(), sv::AnalyzerError> {
        let mut widths = Vec::with_capacity(lvalues.len());
        for part in lvalues {
            widths.push(self.target_width(store, part)?);
        }
        let total: usize = widths.iter().sum();
        let node =
            coerce_node_width(self.arena, value.0, Some(total), value_signed).map_err(slt_error)?;
        let mut lsb = total;
        let placeholder = sv::ir::Expr::Literal("0".to_string());
        for (part, width) in lvalues.iter().zip(widths) {
            lsb -= width;
            let slice = self.slice(node, BitAccess::new(lsb, lsb + width - 1))?;
            let target = self.lvalue_target(store, part)?;
            self.store_value(
                store,
                frames,
                target,
                (slice, value.1.clone()),
                value_signed,
                &placeholder,
            )?;
        }
        Ok(())
    }

    // ------------------------------------------------------------- control

    /// `None` when every jump flag of the innermost loop and function is known
    /// clear; otherwise the condition that execution is still active.
    fn active(
        &mut self,
        store: &Store,
        frames: &[Frame],
    ) -> Result<Option<Value>, sv::AnalyzerError> {
        let mut flags = Vec::new();
        let mut seen_loop = false;
        for frame in frames.iter().rev() {
            match frame {
                Frame::Loop {
                    break_flag,
                    continue_flag,
                } => {
                    if !seen_loop {
                        flags.push(*break_flag);
                        flags.push(*continue_flag);
                        seen_loop = true;
                    }
                }
                Frame::Function { return_flag, .. } => {
                    flags.push(*return_flag);
                    break;
                }
            }
        }
        let mut any: Option<Value> = None;
        for flag in flags {
            if store.get(&flag).is_none() {
                continue;
            }
            let (node, sources) = self.read(store, flag, full(1))?;
            if slt_bool(self.arena, node) == Some(false) {
                continue;
            }
            any = Some(match any {
                None => (node, sources),
                Some((other, mut other_sources)) => {
                    other_sources.extend(sources);
                    (slt_or(self.arena, other, node)?, other_sources)
                }
            });
        }
        match any {
            None => Ok(None),
            Some((node, sources)) => Ok(Some((slt_not(self.arena, node)?, sources))),
        }
    }

    fn exec_block(
        &mut self,
        mut store: Store,
        frames: &[Frame],
        stmts: &[sv::ir::Stmt],
    ) -> Result<Store, sv::AnalyzerError> {
        for (index, stmt) in stmts.iter().enumerate() {
            store = self.exec(store, frames, stmt)?;
            if index + 1 < stmts.len() && stmt_may_jump(stmt, false) {
                if let Some(active) = self.active(&store, frames)? {
                    return match slt_bool(self.arena, active.0) {
                        Some(false) => Ok(store),
                        Some(true) => self.exec_block(store, frames, &stmts[index + 1..]),
                        None => {
                            self.guards.push(active.clone());
                            let rest = self.exec_block(store.fork(), frames, &stmts[index + 1..]);
                            self.guards.pop();
                            self.merge(&active, &rest?, &store)
                        }
                    };
                }
            }
        }
        Ok(store)
    }

    fn exec(
        &mut self,
        mut store: Store,
        frames: &[Frame],
        stmt: &sv::ir::Stmt,
    ) -> Result<Store, sv::AnalyzerError> {
        match stmt {
            sv::ir::Stmt::Assign {
                lhs,
                rhs,
                nonblocking,
            } => {
                if *nonblocking && !self.allow_nonblocking {
                    return Err(unsupported("nonblocking assignment inside always_comb"));
                }
                self.assign(&mut store, frames, lhs, rhs)?;
                Ok(store)
            }
            sv::ir::Stmt::AssignConcat {
                parts,
                rhs,
                nonblocking,
            } => {
                if *nonblocking && !self.allow_nonblocking {
                    return Err(unsupported("nonblocking assignment inside always_comb"));
                }
                self.assign_concat(&mut store, frames, parts, rhs)?;
                Ok(store)
            }
            sv::ir::Stmt::Local { name, init } => {
                let id = self
                    .m
                    .id(name)
                    .ok_or_else(|| unsupported(format!("local `{name}`")))?;
                let width = self.m.var(id).width;
                match init {
                    Some(init) => {
                        self.assign(
                            &mut store,
                            frames,
                            &sv::ir::LValue::Ident(name.clone()),
                            init,
                        )?;
                    }
                    None => {
                        let node = self.default_value(id)?;
                        self.write(&mut store, id, full(width), (node, Sources::default()))?;
                    }
                }
                Ok(store)
            }
            sv::ir::Stmt::If {
                condition,
                then_body,
                else_body,
            } => {
                let (node, sources) = self.eval(&mut store, frames, condition, None)?;
                let truth = slt_truth(self.arena, node)?;
                match slt_bool(self.arena, truth).or_else(|| self.known(truth)) {
                    Some(true) => self.exec_block(store, frames, then_body),
                    Some(false) => self.exec_block(store, frames, else_body),
                    None => {
                        self.facts.push((truth, true, sources.clone()));
                        let then_store = self.exec_block(store.fork(), frames, then_body);
                        self.facts.pop();
                        let then_store = then_store?;
                        self.facts.push((truth, false, sources.clone()));
                        let else_store = self.exec_block(store, frames, else_body);
                        self.facts.pop();
                        let else_store = else_store?;
                        self.merge(&(truth, sources), &then_store, &else_store)
                    }
                }
            }
            sv::ir::Stmt::Case {
                kind,
                selector,
                items,
                default,
            } => self.exec_case(store, frames, *kind, selector, items, default.as_deref()),
            sv::ir::Stmt::Loop {
                kind,
                init,
                condition,
                step,
                body,
            } => self.exec_loop(store, frames, kind, init, condition.as_ref(), step, body),
            sv::ir::Stmt::Break | sv::ir::Stmt::Continue => {
                let flag = frames
                    .iter()
                    .rev()
                    .take_while(|frame| matches!(frame, Frame::Loop { .. }))
                    .find_map(|frame| match frame {
                        Frame::Loop {
                            break_flag,
                            continue_flag,
                        } => Some(if matches!(stmt, sv::ir::Stmt::Break) {
                            *break_flag
                        } else {
                            *continue_flag
                        }),
                        Frame::Function { .. } => None,
                    })
                    .ok_or_else(|| unsupported("break or continue outside a loop"))?;
                self.set_flag(&mut store, flag, true)?;
                Ok(store)
            }
            sv::ir::Stmt::Return(value) => {
                let (return_flag, return_var) = frames
                    .iter()
                    .rev()
                    .find_map(|frame| match frame {
                        Frame::Function {
                            return_flag,
                            return_var,
                        } => Some((*return_flag, return_var.clone())),
                        Frame::Loop { .. } => None,
                    })
                    .ok_or_else(|| unsupported("return outside a subroutine"))?;
                if let (Some(value), Some(return_var)) = (value, return_var) {
                    self.assign(
                        &mut store,
                        frames,
                        &sv::ir::LValue::Ident(return_var),
                        value,
                    )?;
                }
                self.set_flag(&mut store, return_flag, true)?;
                Ok(store)
            }
            sv::ir::Stmt::Call { name, args } => {
                if self.m.dpi_imports.contains_key(name) {
                    return Err(unsupported(format!(
                        "DPI-C function `{name}` called in combinational logic"
                    )));
                }
                if !self.m.subroutines.contains_key(name) {
                    return Err(unsupported(format!("call of `{name}`")));
                }
                self.call(&mut store, frames, name, args)?;
                Ok(store)
            }
            sv::ir::Stmt::Eval(expr) => {
                self.eval(&mut store, frames, expr, None)?;
                Ok(store)
            }
            sv::ir::Stmt::SystemTask { name, args }
                if name == "$readmemh" || name == "$readmemb" =>
            {
                self.readmem(&mut store, frames, name, args)?;
                Ok(store)
            }
            sv::ir::Stmt::SystemTask { name, .. } if self.initial => Err(unsupported(format!(
                "system task `{name}` inside an initial block"
            ))),
            sv::ir::Stmt::SystemTask { name, args } => {
                self.system_task(&mut store, frames, name, args)?;
                Ok(store)
            }
        }
    }

    fn exec_case(
        &mut self,
        mut store: Store,
        frames: &[Frame],
        kind: sv::ir::CaseKind,
        selector: &sv::ir::Expr,
        items: &[sv::ir::CaseItem],
        default: Option<&[sv::ir::Stmt]>,
    ) -> Result<Store, sv::AnalyzerError> {
        // Evaluate the selector once; items compare against its value.
        let selector = if self.m.calls(selector) {
            self.hoist(&mut store, frames, selector)?
        } else {
            selector.clone()
        };
        let conditions = self.case_conditions(&mut store, frames, kind, &selector, items)?;
        let coverage = self.case_coverage(&mut store, frames, kind, &selector, items)?;
        self.exec_case_chain(store, frames, &conditions, items, default, &coverage)
    }

    /// The match condition of each case item. The selector and the labels are
    /// each evaluated self-determined, then extended to the widest of them:
    /// with their sign only when every one of them is signed (IEEE 1800-2023
    /// 12.5).
    fn case_conditions(
        &mut self,
        store: &mut Store,
        frames: &[Frame],
        kind: sv::ir::CaseKind,
        selector: &sv::ir::Expr,
        items: &[sv::ir::CaseItem],
    ) -> Result<Vec<Value>, sv::AnalyzerError> {
        let mut conditions = Vec::with_capacity(items.len());
        if kind == sv::ir::CaseKind::Inside {
            for item in items {
                let condition = sv::ir::Expr::Inside {
                    expr: Box::new(selector.clone()),
                    items: item
                        .labels
                        .iter()
                        .map(|label| match label {
                            sv::ir::CaseLabel::Value(value) => {
                                sv::ir::InsideItem::Value(value.clone())
                            }
                            sv::ir::CaseLabel::Range { low, high } => sv::ir::InsideItem::Range {
                                low: low.clone(),
                                high: high.clone(),
                            },
                        })
                        .collect(),
                };
                let (node, sources) = self.eval(store, frames, &condition, None)?;
                let truth = slt_truth(self.arena, node)?;
                conditions.push((truth, sources));
            }
            return Ok(conditions);
        }
        let selector_signed = self.expr_signed(selector);
        let (selector_node, selector_sources) = self.eval(store, frames, selector, None)?;
        let mut labels = Vec::with_capacity(items.len());
        let mut width = self.width(selector_node);
        let mut all_signed = selector_signed;
        for item in items {
            let mut item_labels = Vec::with_capacity(item.labels.len());
            for label in &item.labels {
                let sv::ir::CaseLabel::Value(value) = label else {
                    return Err(unsupported("range label outside case inside"));
                };
                all_signed &= self.expr_signed(value);
                let (node, sources) = self.eval(store, frames, value, None)?;
                width = width.max(self.width(node));
                item_labels.push((node, sources));
            }
            labels.push(item_labels);
        }
        // The selector and the labels are evaluated at their common width
        // and signedness (IEEE 1800-2023 12.5), like the operands of an
        // equality: an unsigned operand also turns the others' operands
        // unsigned (11.8.2).
        let selector_node = if self.width(selector_node) < width || selector_signed != all_signed {
            self.eval(store, frames, selector, Some((width, all_signed)))?
                .0
        } else {
            selector_node
        };
        let selector_node = coerce_node_width(self.arena, selector_node, Some(width), all_signed)
            .map_err(slt_error)?;
        let mut item_values = items.iter().map(|item| item.labels.iter());
        for item_labels in &mut labels {
            let values = item_values.next().into_iter().flatten();
            for ((node, _), label) in item_labels.iter_mut().zip(values) {
                if let sv::ir::CaseLabel::Value(value) = label
                    && (self.width(*node) < width || self.expr_signed(value) != all_signed)
                {
                    *node = self
                        .eval(store, frames, value, Some((width, all_signed)))?
                        .0;
                }
            }
        }
        let op = if kind == sv::ir::CaseKind::Exact {
            BinaryOp::EqCase
        } else {
            BinaryOp::EqWildcard
        };
        for item_labels in labels {
            let mut condition: Option<Value> = None;
            for (node, mut sources) in item_labels {
                let node = coerce_node_width(self.arena, node, Some(width), all_signed)
                    .map_err(slt_error)?;
                let matched = self.alloc(SLTNode::Binary(selector_node, op, node))?;
                let matched = slt_truth(self.arena, matched)?;
                sources.extend(selector_sources.iter().copied());
                condition = Some(match condition {
                    None => (matched, sources),
                    Some((other, mut other_sources)) => {
                        other_sources.extend(sources);
                        (slt_or(self.arena, other, matched)?, other_sources)
                    }
                });
            }
            let condition = match condition {
                Some(condition) => condition,
                None => (self.constant(0, 1)?, Sources::default()),
            };
            conditions.push(condition);
        }
        Ok(conditions)
    }

    fn exec_case_chain(
        &mut self,
        store: Store,
        frames: &[Frame],
        conditions: &[Value],
        items: &[sv::ir::CaseItem],
        default: Option<&[sv::ir::Stmt]>,
        coverage: &[bool],
    ) -> Result<Store, sv::AnalyzerError> {
        let Some(((condition, rest_conditions), (item, rest_items))) =
            conditions.split_first().zip(items.split_first())
        else {
            return match default {
                Some(default) => self.exec_block(store, frames, default),
                None => Ok(store),
            };
        };
        let (covered, rest_coverage) = coverage
            .split_first()
            .map_or((false, &[][..]), |(covered, rest)| (*covered, rest));
        match slt_bool(self.arena, condition.0).or_else(|| self.known(condition.0)) {
            Some(true) => self.exec_block(store, frames, &item.body),
            Some(false) => self.exec_case_chain(
                store,
                frames,
                rest_conditions,
                rest_items,
                default,
                rest_coverage,
            ),
            None => {
                // Once the items so far cover every two-state selector value,
                // this item is taken whenever no earlier one is; the rest of
                // the case is unreachable.
                if covered {
                    return self.exec_block(store, frames, &item.body);
                }
                self.facts.push((condition.0, true, condition.1.clone()));
                let then_store = self.exec_block(store.fork(), frames, &item.body);
                self.facts.pop();
                let then_store = then_store?;
                self.facts.push((condition.0, false, condition.1.clone()));
                let else_store = self.exec_case_chain(
                    store,
                    frames,
                    rest_conditions,
                    rest_items,
                    default,
                    rest_coverage,
                );
                self.facts.pop();
                let else_store = else_store?;
                self.merge(condition, &then_store, &else_store)
            }
        }
    }

    /// The value a condition is known to have on the current path, from an
    /// enclosing branch on the same or the complementary two-state condition.
    fn known(&self, truth: NodeId) -> Option<bool> {
        self.facts.iter().rev().find_map(|&(fact, value, _)| {
            if fact == truth {
                Some(value)
            } else if self.complementary(fact, truth) {
                Some(!value)
            } else {
                None
            }
        })
    }

    /// The boolean a truth node tests, without the truth wrappers.
    fn truth_core(&self, node: NodeId) -> NodeId {
        let mut node = node;
        loop {
            match self.arena.get(node) {
                SLTNode::Unary(UnaryOp::ToTwoState | UnaryOp::Ident, inner) => node = *inner,
                SLTNode::Unary(UnaryOp::Or, inner) if self.width(*inner) == 1 => node = *inner,
                _ => return node,
            }
        }
    }

    /// Whether two truth nodes are complements for every two-state input.
    fn complementary(&self, left: NodeId, right: NodeId) -> bool {
        let left = self.truth_core(left);
        let right = self.truth_core(right);
        if !self.two_state(left) || !self.two_state(right) {
            return false;
        }
        let negates = |a: NodeId, b: NodeId| match self.arena.get(b) {
            SLTNode::Unary(UnaryOp::LogicNot, inner) => self.truth_core(*inner) == a,
            SLTNode::Unary(UnaryOp::BitNot, inner) => {
                self.width(*inner) == 1 && self.truth_core(*inner) == a
            }
            _ => false,
        };
        if negates(left, right) || negates(right, left) {
            return true;
        }
        match (self.arena.get(left), self.arena.get(right)) {
            (SLTNode::Binary(a, op_a, b), SLTNode::Binary(c, op_b, d)) => {
                a == c
                    && b == d
                    && matches!(
                        (op_a, op_b),
                        (BinaryOp::Eq, BinaryOp::Ne)
                            | (BinaryOp::Ne, BinaryOp::Eq)
                            | (BinaryOp::EqCase, BinaryOp::NeCase)
                            | (BinaryOp::NeCase, BinaryOp::EqCase)
                    )
            }
            _ => false,
        }
    }

    /// Whether a value never holds unknown bits.
    fn two_state(&self, node: NodeId) -> bool {
        if !self.m.four_state {
            return true;
        }
        match self.arena.get(node) {
            SLTNode::Input {
                variable, index, ..
            } => {
                index.is_empty()
                    && self
                        .m
                        .variables
                        .get(variable)
                        .is_some_and(|var| !var.is_4state)
            }
            SLTNode::Constant(_, mask, _, _) => mask.is_zero(),
            SLTNode::Unary(UnaryOp::ToTwoState, _) => true,
            SLTNode::Unary(_, inner)
            | SLTNode::Slice { expr: inner, .. }
            | SLTNode::Capture { expr: inner, .. } => self.two_state(*inner),
            SLTNode::Binary(left, op, right) => {
                !matches!(
                    op,
                    BinaryOp::DivU | BinaryOp::DivS | BinaryOp::RemU | BinaryOp::RemS
                ) && self.two_state(*left)
                    && self.two_state(*right)
            }
            SLTNode::Mux {
                cond,
                then_expr,
                else_expr,
            } => self.two_state(*cond) && self.two_state(*then_expr) && self.two_state(*else_expr),
            SLTNode::Concat(parts) => parts.iter().all(|(part, _)| self.two_state(*part)),
            _ => false,
        }
    }

    /// For each item, whether it and the items before it cover every
    /// two-state value of the selector with constant labels.
    fn case_coverage(
        &mut self,
        store: &mut Store,
        frames: &[Frame],
        kind: sv::ir::CaseKind,
        selector: &sv::ir::Expr,
        items: &[sv::ir::CaseItem],
    ) -> Result<Vec<bool>, sv::AnalyzerError> {
        let none = vec![false; items.len()];
        let mut probe = store.fork();
        let (selector_node, _) = self.eval(&mut probe, frames, selector, None)?;
        let selector_width = self.width(selector_node);
        // A four-state selector also takes X and Z bits; only exact
        // comparisons of a narrow selector are enumerated.
        let four_state_selector = !self.two_state(selector_node);
        if four_state_selector && (kind != sv::ir::CaseKind::Exact || selector_width > 6) {
            return Ok(none);
        }
        let selector_signed = self.expr_signed(selector);
        let mut item_labels = Vec::with_capacity(items.len());
        for item in items {
            let mut labels = Vec::new();
            for label in &item.labels {
                let mut constant = |this: &mut Self,
                                    expr: &sv::ir::Expr|
                 -> Result<
                    Option<(BigUint, BigUint, usize, bool)>,
                    sv::AnalyzerError,
                > {
                    let signed = this.expr_signed(expr);
                    let (node, _) = this.eval(&mut probe, frames, expr, None)?;
                    Ok(slt_const4(this.arena, node)
                        .map(|(value, mask, width)| (value, mask, width, signed)))
                };
                match label {
                    sv::ir::CaseLabel::Value(value) => match constant(self, value)? {
                        Some(label) => labels.push(Label::Value(label)),
                        None => return Ok(none),
                    },
                    sv::ir::CaseLabel::Range { low, high } => {
                        match (constant(self, low)?, constant(self, high)?) {
                            (Some(low), Some(high)) if low.1.is_zero() && high.1.is_zero() => {
                                labels.push(Label::Range(low.0, high.0))
                            }
                            _ => return Ok(none),
                        }
                    }
                }
            }
            item_labels.push(labels);
        }
        let all_signed = selector_signed
            && item_labels
                .iter()
                .flatten()
                .all(|label| matches!(label, Label::Value((_, _, _, true))));
        let width = item_labels
            .iter()
            .flatten()
            .map(|label| match label {
                Label::Value((_, _, width, _)) => *width,
                Label::Range(..) => 0,
            })
            .fold(selector_width, usize::max);
        let width_mask = (BigUint::from(1u8) << width) - BigUint::from(1u8);
        let extend = |value: &BigUint, from: usize| -> BigUint {
            if all_signed && from > 0 && value.bit(from as u64 - 1) {
                value | (&width_mask ^ ((BigUint::from(1u8) << from) - BigUint::from(1u8)))
            } else {
                value.clone()
            }
        };
        // A selector value is `(payload, mask)`.
        let matches = |label: &Label, (value, value_mask): &(BigUint, BigUint)| match label {
            Label::Value((label_value, mask, label_width, _)) => {
                let label_value = extend(label_value, *label_width);
                let mask = extend(mask, *label_width);
                if kind == sv::ir::CaseKind::Exact {
                    *value_mask == mask && *value == label_value
                } else {
                    let care = &width_mask ^ (&mask & &width_mask);
                    value_mask.is_zero() && (value & &care) == (&label_value & &care)
                }
            }
            Label::Range(low, high) => value_mask.is_zero() && value >= low && value <= high,
        };
        let mut coverage = Vec::with_capacity(items.len());
        if selector_width > 16 {
            // Too many values to enumerate; only an all-wildcard label covers them.
            let mut covered = false;
            for labels in &item_labels {
                covered |= kind != sv::ir::CaseKind::Exact
                    && labels.iter().any(|label| {
                        matches!(label, Label::Value((_, mask, label_width, _))
                            if *label_width >= width && *mask == (BigUint::from(1u8) << *label_width) - BigUint::from(1u8))
                    });
                coverage.push(covered);
            }
            return Ok(coverage);
        }
        let mut remaining: Vec<(BigUint, BigUint)> = if four_state_selector {
            // Each bit is 0, 1, Z (payload 0, mask 1), or X (payload 1, mask 1).
            (0u64..(1u64 << (2 * selector_width)))
                .map(|raw| {
                    let mut value = BigUint::zero();
                    let mut mask = BigUint::zero();
                    for bit in 0..selector_width {
                        let digit = (raw >> (2 * bit)) & 3;
                        if digit & 1 != 0 {
                            value |= BigUint::from(1u8) << bit;
                        }
                        if digit & 2 != 0 {
                            mask |= BigUint::from(1u8) << bit;
                        }
                    }
                    (
                        extend(&value, selector_width),
                        extend(&mask, selector_width),
                    )
                })
                .collect()
        } else {
            (0u64..(1u64 << selector_width))
                .map(|raw| (extend(&BigUint::from(raw), selector_width), BigUint::zero()))
                .collect()
        };
        for labels in &item_labels {
            remaining.retain(|value| !labels.iter().any(|label| matches(label, value)));
            coverage.push(remaining.is_empty());
        }
        Ok(coverage)
    }

    // ---------------------------------------------------------------- loops

    #[allow(clippy::too_many_arguments)]
    fn exec_loop(
        &mut self,
        mut store: Store,
        frames: &[Frame],
        kind: &sv::ir::LoopKind<sv::ir::Expr>,
        init: &[sv::ir::Stmt],
        condition: Option<&sv::ir::Expr>,
        step: &[sv::ir::Stmt],
        body: &[sv::ir::Stmt],
    ) -> Result<Store, sv::AnalyzerError> {
        let break_flag = self.flag("break");
        let continue_flag = self.flag("continue");
        let mut loop_frames = frames.to_vec();
        loop_frames.push(Frame::Loop {
            break_flag,
            continue_flag,
        });

        if let sv::ir::LoopKind::For = kind
            && let Some(canonical) = canonical_for_loop(init, condition, step)
            && self.loop_needs_fold(&store, frames, &canonical, body)?
        {
            self.folding += 1;
            let folded = self.fold_loop(
                store,
                frames,
                &loop_frames,
                &canonical,
                body,
                break_flag,
                continue_flag,
            );
            self.folding -= 1;
            return folded;
        }

        store = self.exec_block(store, frames, init)?;
        self.set_flag(&mut store, break_flag, false)?;
        let mut remaining_repeat = match kind {
            sv::ir::LoopKind::Repeat(count) => {
                let (node, _) = self.eval(&mut store, frames, count, None)?;
                let (value, _) = slt_const(self.arena, node)
                    .ok_or_else(|| unsupported("repeat count that depends on run-time values"))?;
                Some(
                    value
                        .to_usize()
                        .ok_or_else(|| unsupported("repeat count"))?,
                )
            }
            _ => None,
        };
        let mut first = true;
        let mut symbolic_iterations = 0usize;
        // Whether every variable the step writes is local to the loop.
        let step_is_local = {
            let mut declared = HashSet::default();
            for stmt in init {
                if let sv::ir::Stmt::Local { name, .. } = stmt {
                    declared.insert(name.clone());
                }
            }
            let mut stepped = HashSet::default();
            written_names(step, &mut stepped);
            !stepped.is_empty() && stepped.iter().all(|name| declared.contains(name))
        };
        loop {
            // The loop condition (not for the first pass of a do-while).
            let proceed = match (kind, condition) {
                (sv::ir::LoopKind::DoWhile, _) if first => None,
                (sv::ir::LoopKind::Repeat(_), _) => {
                    let remaining = remaining_repeat.as_mut().expect("repeat count");
                    if *remaining == 0 {
                        break;
                    }
                    *remaining -= 1;
                    None
                }
                (sv::ir::LoopKind::Forever, _) => None,
                (_, Some(condition)) => {
                    let (node, sources) = self.eval(&mut store, frames, condition, None)?;
                    let truth = slt_truth(self.arena, node)?;
                    match slt_bool(self.arena, truth) {
                        Some(false) => break,
                        Some(true) => None,
                        None => Some((truth, sources)),
                    }
                }
                (_, None) => None,
            };
            first = false;
            self.unrolled += 1;
            if self.unrolled > MAX_UNROLLED_ITERATIONS {
                return Err(unsupported("procedural loop unroll limit exceeded"));
            }
            // A run-time condition, or a `break` taken on some path, guards
            // the iteration.
            let broken = self.read(&store, break_flag, full(1))?;
            let broken_value = slt_bool(self.arena, broken.0);
            if broken_value == Some(true) {
                break;
            }
            let mut guard = proceed;
            if broken_value.is_none() {
                let active = (slt_not(self.arena, broken.0)?, broken.1);
                guard = Some(match guard {
                    None => active,
                    Some((node, mut sources)) => {
                        sources.extend(active.1);
                        (slt_and(self.arena, node, active.0)?, sources)
                    }
                });
            }
            if guard.is_some() {
                symbolic_iterations += 1;
                if symbolic_iterations > MAX_SYMBOLIC_ITERATIONS {
                    return Err(unsupported(
                        "loop condition that depends on run-time values",
                    ));
                }
            }
            let mut iteration = store.fork();
            self.set_flag(&mut iteration, continue_flag, false)?;
            if let Some(guard) = &guard {
                self.guards.push(guard.clone());
            }
            let executed = self.exec_block(iteration, &loop_frames, body);
            if guard.is_some() {
                self.guards.pop();
            }
            iteration = executed?;
            // `continue` ends only this iteration; the step still runs.
            self.set_flag(&mut iteration, continue_flag, false)?;
            store = match &guard {
                None => iteration,
                Some(guard) => self.merge(guard, &iteration, &store)?,
            };
            // The step of loop-scoped control variables runs unconditionally:
            // once the loop has stopped on a path, their values are no longer
            // observable there, and keeping them constant keeps the loop
            // bounded. Other steps run only where the loop is still active.
            if step_is_local {
                store = self.exec_block(store, frames, step)?;
            } else {
                let broken_after = self.read(&store, break_flag, full(1))?;
                let mut step_guard = (slt_not(self.arena, broken_after.0)?, broken_after.1);
                if let Some(guard) = &guard {
                    let mut sources = step_guard.1;
                    sources.extend(guard.1.iter().copied());
                    step_guard = (slt_and(self.arena, step_guard.0, guard.0)?, sources);
                }
                store = match slt_bool(self.arena, step_guard.0) {
                    Some(true) => self.exec_block(store, frames, step)?,
                    Some(false) => store,
                    None => {
                        let stepped = self.exec_block(store.fork(), frames, step)?;
                        self.merge(&step_guard, &stepped, &store)?
                    }
                };
            }
            // A `return` inside the loop ends it as well.
            if let Some(active) = self.active_return(&store, frames)? {
                match slt_bool(self.arena, active.0) {
                    Some(false) => break,
                    Some(true) => {}
                    None => {
                        // Later iterations run only while no return was taken.
                        let (node, sources) = self.read(&store, break_flag, full(1))?;
                        let returned = slt_not(self.arena, active.0)?;
                        let node = slt_or(self.arena, node, returned)?;
                        let mut sources = sources;
                        sources.extend(active.1);
                        self.write(&mut store, break_flag, full(1), (node, sources))?;
                    }
                }
            }
        }
        Ok(store)
    }

    /// The active condition of the innermost function's `return` flag.
    fn active_return(
        &mut self,
        store: &Store,
        frames: &[Frame],
    ) -> Result<Option<Value>, sv::AnalyzerError> {
        let Some(return_flag) = frames.iter().rev().find_map(|frame| match frame {
            Frame::Function { return_flag, .. } => Some(*return_flag),
            Frame::Loop { .. } => None,
        }) else {
            return Ok(None);
        };
        if store.get(&return_flag).is_none() {
            return Ok(None);
        }
        let (node, sources) = self.read(store, return_flag, full(1))?;
        if slt_bool(self.arena, node) == Some(false) {
            return Ok(None);
        }
        Ok(Some((slt_not(self.arena, node)?, sources)))
    }

    /// Whether a counted loop must be folded: its bounds depend on run-time
    /// values, or it runs more iterations than may be unrolled.
    fn loop_needs_fold(
        &mut self,
        store: &Store,
        frames: &[Frame],
        canonical: &CanonicalLoop,
        body: &[sv::ir::Stmt],
    ) -> Result<bool, sv::AnalyzerError> {
        let mut written = HashSet::default();
        written_names(body, &mut written);
        if written.contains(&canonical.var) {
            return Ok(false);
        }
        let mut probe = store.fork();
        let (start, _) = self.eval(&mut probe, frames, &canonical.start, None)?;
        let (end, _) = self.eval(&mut probe, frames, &canonical.end, None)?;
        let (Some((start, _)), Some((end, _))) =
            (slt_const(self.arena, start), slt_const(self.arena, end))
        else {
            return Ok(true);
        };
        // Constant bounds: estimate the trip count of an additive loop.
        let start = start.to_f64().unwrap_or(f64::MAX);
        let end = end.to_f64().unwrap_or(f64::MAX);
        let trips = if canonical.step_op == celox_slt::SLTStepOp::Add {
            ((end - start).abs() / canonical.step.max(1) as f64).ceil()
        } else {
            0.0
        };
        Ok(trips > (MAX_UNROLLED_ITERATIONS - self.unrolled.min(MAX_UNROLLED_ITERATIONS)) as f64)
    }

    #[allow(clippy::too_many_arguments)]
    fn fold_loop(
        &mut self,
        mut store: Store,
        frames: &[Frame],
        loop_frames: &[Frame],
        canonical: &CanonicalLoop,
        body: &[sv::ir::Stmt],
        break_flag: SourceVarId,
        continue_flag: SourceVarId,
    ) -> Result<Store, sv::AnalyzerError> {
        let loop_var = self
            .m
            .id(&canonical.var)
            .ok_or_else(|| unsupported(format!("loop variable `{}`", canonical.var)))?;
        let (loop_width, loop_signed) = {
            let var = self.m.var(loop_var);
            (var.width, var.signed)
        };
        let (start, start_sources) = self.eval(
            &mut store,
            frames,
            &canonical.start,
            Some((loop_width, loop_signed)),
        )?;
        let start = coerce_node_width(self.arena, start, Some(loop_width), loop_signed)
            .map_err(slt_error)?;
        let end_signed = self.expr_signed(&canonical.end);
        let (mut end, end_sources) = self.eval(&mut store, frames, &canonical.end, None)?;
        let mut inclusive = matches!(
            canonical.compare,
            sv::ir::BinaryOp::Le | sv::ir::BinaryOp::Ge
        );
        if canonical.compare == sv::ir::BinaryOp::Gt {
            // `v > end` is `v >= end + 1`.
            let width = self.width(end) + 1;
            let wide =
                coerce_node_width(self.arena, end, Some(width), end_signed).map_err(slt_error)?;
            let one = slt_constant(self.arena, BigUint::from(1u8), width, end_signed)?;
            end = self.alloc(SLTNode::Binary(wide, BinaryOp::Add, one))?;
            inclusive = true;
        }
        let (start_bound, end_bound) = if canonical.decreasing {
            (
                SLTLoopBound::TypedExpr {
                    node: end,
                    signed: end_signed,
                },
                SLTLoopBound::TypedExpr {
                    node: start,
                    signed: loop_signed,
                },
            )
        } else {
            (
                SLTLoopBound::TypedExpr {
                    node: start,
                    signed: loop_signed,
                },
                SLTLoopBound::TypedExpr {
                    node: end,
                    signed: end_signed,
                },
            )
        };
        // The reverse fold counts down from the start value itself.
        let inclusive = if canonical.decreasing {
            true
        } else {
            inclusive
        };

        // Find the state the body writes, then run it with that state read
        // from the loop-carried values.
        let mut probe = store.fork();
        let loop_input = self.alloc(SLTNode::Input {
            variable: loop_var,
            signed: loop_signed,
            index: Vec::new(),
            access: full(loop_width),
        })?;
        probe.remove(&loop_var);
        let _ = loop_input;
        self.set_flag(&mut probe, break_flag, false)?;
        self.set_flag(&mut probe, continue_flag, false)?;
        let saved_unrolled = self.unrolled;
        let first_temp = self.m.next_id;
        let probe_after = self.exec_block(probe.fork(), loop_frames, body)?;
        self.unrolled = saved_unrolled;
        let is_state = |id: &SourceVarId| {
            *id != loop_var && *id != break_flag && *id != continue_flag && id.0 < first_temp.0
        };
        let mut carried: Vec<SourceVarId> = probe_after
            .differing_keys(&probe)
            .copied()
            .filter(is_state)
            .collect();
        for id in probe_after.keys() {
            if probe.get(id).is_none() && is_state(id) && !carried.contains(id) {
                carried.push(*id);
            }
        }
        carried.sort_by_key(|id| id.0);

        let mut iteration = store.fork();
        iteration.remove(&loop_var);
        for id in &carried {
            iteration.remove(id);
        }
        self.set_flag(&mut iteration, break_flag, false)?;
        self.set_flag(&mut iteration, continue_flag, false)?;
        let after = self.exec_block(iteration, loop_frames, body)?;

        let mut initials = Vec::with_capacity(carried.len());
        let mut updates = Vec::with_capacity(carried.len());
        let mut initial_sources = Sources::default();
        let mut update_sources = Sources::default();
        for id in &carried {
            let width = self.m.var(*id).width;
            let target = VarAtomBase::new(*id, 0, width - 1);
            let (initial, sources) = self.read(&store, *id, full(width))?;
            initial_sources.extend(sources);
            let (update, sources) = self.read(&after, *id, full(width))?;
            update_sources.extend(sources);
            initials.push(SLTForUpdate {
                target,
                expr: initial,
            });
            updates.push(SLTForUpdate {
                target,
                expr: update,
            });
        }
        // The fold stops after an iteration that breaks or returns.
        let (broke, broke_sources) = self.read(&after, break_flag, full(1))?;
        let mut stop = broke;
        let mut stop_sources = broke_sources;
        if let Some(Frame::Function { return_flag, .. }) = frames
            .iter()
            .rev()
            .find(|frame| matches!(frame, Frame::Function { .. }))
        {
            if after.get(return_flag).is_some() {
                let (returned, sources) = self.read(&after, *return_flag, full(1))?;
                stop = slt_or(self.arena, stop, returned)?;
                stop_sources.extend(sources);
            }
        }
        let continue_cond = slt_not(self.arena, stop)?;

        let bound: HashSet<SourceVarId> = carried.iter().copied().chain([loop_var]).collect();
        let mut sources: Sources = start_sources;
        sources.extend(end_sources);
        sources.extend(initial_sources);
        sources.extend(
            update_sources
                .into_iter()
                .filter(|source| !bound.contains(&source.id)),
        );
        sources.extend(
            stop_sources
                .into_iter()
                .filter(|source| !bound.contains(&source.id)),
        );

        if carried.is_empty() {
            return Ok(store);
        }
        for id in &carried {
            let width = self.m.var(*id).width;
            let target = VarAtomBase::new(*id, 0, width - 1);
            let node = self.alloc(SLTNode::ForFold {
                loop_var,
                loop_width,
                loop_signed,
                start: start_bound.clone(),
                end: end_bound.clone(),
                inclusive,
                step: canonical.step,
                step_op: canonical.step_op,
                reverse: canonical.decreasing,
                result: SLTForFoldResult::State(target),
                initials: initials.clone(),
                updates: updates.clone(),
                effects: Vec::new(),
                continue_cond,
            })?;
            let own: Sources = sources
                .iter()
                .copied()
                .filter(|source| source.id != *id)
                .collect();
            self.write(&mut store, *id, full(width), (node, own))?;
        }
        Ok(store)
    }

    // ---------------------------------------------------------- subroutines

    /// Inline a call of `name`. Returns the function result, if any.
    fn call(
        &mut self,
        store: &mut Store,
        frames: &[Frame],
        name: &str,
        args: &[Option<sv::ir::Expr>],
    ) -> Result<Option<Value>, sv::AnalyzerError> {
        let subroutine = self
            .m
            .subroutine(name)
            .cloned()
            .ok_or_else(|| unsupported(format!("call of `{name}`")))?;
        if self.active_calls.iter().any(|active| active == name) {
            return Err(unsupported(format!("recursive call of `{name}`")));
        }
        if self.active_calls.len() >= MAX_CALL_DEPTH {
            return Err(unsupported("subroutine call depth limit exceeded"));
        }
        if args.len() > subroutine.params.len() {
            return Err(unsupported(format!(
                "function call `{name}` with too many arguments"
            )));
        }
        // Copy the inputs in, left to right, in the caller's scope.
        let mut outputs = Vec::new();
        let mut copied = Vec::new();
        for (position, param) in subroutine.params.iter().enumerate() {
            let id = self
                .m
                .id(&param.name)
                .ok_or_else(|| unsupported(format!("argument `{}`", param.source_name)))?;
            let width = self.m.var(id).width;
            let actual = args
                .get(position)
                .cloned()
                .flatten()
                .or_else(|| param.default.clone());
            if param.direction.is_written() {
                let actual = actual.as_ref().and_then(lvalue_from_expr).ok_or_else(|| {
                    unsupported(format!(
                        "output argument `{}` of `{name}`",
                        param.source_name
                    ))
                })?;
                outputs.push((id, actual));
            }
            if param.direction.is_read() {
                let actual = actual.ok_or_else(|| {
                    unsupported(format!(
                        "missing argument `{}` of `{name}`",
                        param.source_name
                    ))
                })?;
                let signed = self.expr_signed(&actual);
                let (node, sources) = self.eval(store, frames, &actual, Some((width, signed)))?;
                let node =
                    coerce_node_width(self.arena, node, Some(width), signed).map_err(slt_error)?;
                let node = if self.m.var(id).is_4state {
                    node
                } else {
                    self.alloc(SLTNode::Unary(UnaryOp::ToTwoState, node))?
                };
                copied.push((id, (node, sources)));
            } else {
                let node = self.default_value(id)?;
                copied.push((id, (node, Sources::default())));
            }
        }
        for (id, value) in copied {
            let width = self.m.var(id).width;
            self.write(store, id, full(width), value)?;
        }
        let return_var = match &subroutine.return_var {
            Some(name) => {
                let id = self
                    .m
                    .id(name)
                    .ok_or_else(|| unsupported(format!("result of `{}`", subroutine.name)))?;
                let node = self.default_value(id)?;
                let width = self.m.var(id).width;
                self.write(store, id, full(width), (node, Sources::default()))?;
                Some(id)
            }
            None => None,
        };
        let return_flag = self.flag("return");
        self.set_flag(store, return_flag, false)?;
        let mut call_frames = frames.to_vec();
        call_frames.push(Frame::Function {
            return_flag,
            return_var: subroutine.return_var.clone(),
        });
        self.active_calls.push(name.to_string());
        let result = self.exec_block(store.fork(), &call_frames, &subroutine.body);
        self.active_calls.pop();
        *store = result?;
        // Copy the outputs back.
        for (id, lvalues) in outputs {
            let width = self.m.var(id).width;
            let signed = self.m.var(id).signed;
            let value = self.read(store, id, full(width))?;
            self.write_back(store, frames, &lvalues, value, signed)?;
        }
        match return_var {
            Some(id) => {
                let width = self.m.var(id).width;
                Ok(Some(self.read(store, id, full(width))?))
            }
            None => Ok(None),
        }
    }

    // -------------------------------------------------------- system tasks

    /// `$readmemh`/`$readmemb`: the words of the file, written as constants.
    fn readmem(
        &mut self,
        store: &mut Store,
        frames: &[Frame],
        name: &str,
        args: &[sv::ir::SystemTaskArg<sv::ir::Expr>],
    ) -> Result<(), sv::AnalyzerError> {
        let mut addresses = [None, None];
        for (slot, arg) in addresses.iter_mut().zip(args.iter().skip(2)) {
            let sv::ir::SystemTaskArg::Expr(expr) = arg else {
                continue;
            };
            let (node, _) = self.eval(store, frames, expr, None)?;
            let signed = self.expr_signed(expr);
            let (value, width) = slt_const(self.arena, node)
                .ok_or_else(|| unsupported(format!("`{name}` address that is not constant")))?;
            let value = num_bigint::BigInt::from(value);
            let value = if signed && width > 0 && value.bit(width as u64 - 1) {
                value - (num_bigint::BigInt::from(1u8) << width)
            } else {
                value
            };
            *slot = Some(
                value
                    .to_i128()
                    .ok_or_else(|| unsupported(format!("`{name}` address")))?,
            );
        }
        if self.path_condition()?.is_some() {
            return Err(unsupported(format!("`{name}` under a run-time condition")));
        }
        let (id, words) = self.m.readmem(name, args, addresses[0], addresses[1])?;
        for word in words {
            let width = word.access.msb - word.access.lsb + 1;
            let node = self.alloc(SLTNode::Constant(word.value, word.mask, width, false))?;
            self.write(store, id, word.access, (node, Sources::default()))?;
        }
        Ok(())
    }

    /// The condition under which the current statement runs, if it does not
    /// run unconditionally.
    fn path_condition(&mut self) -> Result<Option<Value>, sv::AnalyzerError> {
        let mut terms: Vec<Value> = Vec::new();
        for (truth, value, sources) in self.facts.clone() {
            let node = if value {
                truth
            } else {
                slt_not(self.arena, truth)?
            };
            terms.push((node, sources));
        }
        terms.extend(self.guards.iter().cloned());
        let mut result: Option<Value> = None;
        for (node, sources) in terms {
            if slt_bool(self.arena, node) == Some(true) {
                continue;
            }
            result = Some(match result {
                None => (node, sources),
                Some((other, mut other_sources)) => {
                    other_sources.extend(sources);
                    (slt_and(self.arena, other, node)?, other_sources)
                }
            });
        }
        Ok(result)
    }

    fn capture(&mut self, expr: NodeId) -> Result<NodeId, sv::AnalyzerError> {
        let key = self.m.capture_key();
        self.alloc(SLTNode::Capture { expr, key })
    }

    /// A display, message, or `$finish` in a combinational process: an
    /// observer that emits the event whenever the process runs with the
    /// statement on its path (IEEE 1800-2023 9.2.2.2).
    fn system_task(
        &mut self,
        store: &mut Store,
        frames: &[Frame],
        name: &str,
        args: &[sv::ir::SystemTaskArg<sv::ir::Expr>],
    ) -> Result<(), sv::AnalyzerError> {
        let kind =
            system_task_kind(name).ok_or_else(|| unsupported(format!("system task `{name}`")))?;
        if self.folding > 0 {
            return Err(unsupported(format!(
                "system task `{name}` inside a combinational loop with a run-time bound"
            )));
        }
        let (template, values) = system_task_template(&kind, args);
        let observer_store = store.fork();
        let mut observed = Sources::default();
        let mut captured = Vec::with_capacity(values.len());
        let mut arg_widths = Vec::with_capacity(values.len());
        let mut arg_signed = Vec::with_capacity(values.len());
        let mut arg_is_string = Vec::with_capacity(values.len());
        for value in &values {
            let node = match value {
                sv::ir::SystemTaskArg::Expr(expr) => {
                    let signed = self.expr_signed(expr);
                    let (node, sources) = self.eval(store, frames, expr, None)?;
                    arg_signed.push(signed);
                    arg_is_string.push(false);
                    observed.extend(sources);
                    node
                }
                sv::ir::SystemTaskArg::Str(text) => {
                    let bytes = unescape(text).into_bytes();
                    let width = (bytes.len() * 8).max(8);
                    let value = bytes.iter().fold(BigUint::zero(), |value, byte| {
                        (value << 8u32) | BigUint::from(*byte)
                    });
                    arg_signed.push(false);
                    arg_is_string.push(true);
                    slt_constant(self.arena, value, width, false)?
                }
                sv::ir::SystemTaskArg::Empty => {
                    return Err(unsupported(format!("empty argument of `{name}`")));
                }
            };
            arg_widths.push(self.width(node));
            captured.push(self.capture(node)?);
        }
        let event_kind = match &kind {
            SystemTaskKind::Print(kind) => *kind,
            SystemTaskKind::Finish => RuntimeEventKind::Finish,
            SystemTaskKind::Message => RuntimeEventKind::AssertContinue,
            SystemTaskKind::Fatal => RuntimeEventKind::AssertFatal,
        };
        let condition = self.path_condition()?;
        let guard = match (&kind, condition) {
            (SystemTaskKind::Print(_) | SystemTaskKind::Finish, None) => None,
            (SystemTaskKind::Print(_) | SystemTaskKind::Finish, Some((node, sources))) => {
                observed.extend(sources);
                Some(node)
            }
            // Assertion sites fire when their guard is false.
            (SystemTaskKind::Message | SystemTaskKind::Fatal, None) => Some(self.constant(0, 1)?),
            (SystemTaskKind::Message | SystemTaskKind::Fatal, Some((node, sources))) => {
                observed.extend(sources);
                Some(slt_not(self.arena, node)?)
            }
        };
        let guard = guard.map(|guard| self.capture(guard)).transpose()?;
        self.effect_sensitivity.extend(observed.iter().copied());
        let site_id = self.sites.len() as u32;
        self.sites.push(RuntimeEventSite {
            kind: event_kind,
            template,
            scope: None,
            arg_widths,
            arg_signed,
            arg_is_string,
        });
        let mut preceding_writes = Vec::new();
        for (id, range) in store.iter() {
            if self.m.is_hidden(*id) {
                continue;
            }
            for (&lsb, (value, width, _)) in &range.ranges {
                if value.is_some() {
                    preceding_writes.push(VarAtomBase::new(*id, lsb, lsb + width - 1));
                }
            }
        }
        let mut observed_ids: Vec<SourceVarId> = observed.iter().map(|atom| atom.id).collect();
        observed_ids.sort_by_key(|id| id.0);
        observed_ids.dedup();
        let mut local_inputs = Vec::new();
        for id in observed_ids {
            let Some(range) = observer_store.get(&id) else {
                continue;
            };
            if !range.ranges.values().any(|(value, _, _)| value.is_some()) {
                continue;
            }
            let width = self.m.var(id).width;
            let (node, _) = self.read(&observer_store, id, full(width))?;
            local_inputs.push((id, node));
        }
        self.observers.push(CombObserver {
            site_id,
            activation_group: 0,
            guard,
            args: captured,
            loop_runner: None,
            sensitivity: Vec::new(),
            local_inputs,
            observed_inputs: observed
                .into_iter()
                .filter(|atom| !self.m.is_hidden(atom.id))
                .collect(),
            position_inputs: Vec::new(),
            preceding_writes,
            written_before: Vec::new(),
            written_input_atoms: Vec::new(),
            written_inputs: Vec::new(),
            captured_in_loop: false,
        });
        Ok(())
    }

    // -------------------------------------------------------------- process

    /// Execute an `initial` block and return the initial state it defines.
    /// Its writes must have constant values.
    pub fn lower_initial(
        &mut self,
        body: &[sv::ir::Stmt],
    ) -> Result<Vec<InitialStateValue<SourceVarId>>, sv::AnalyzerError> {
        self.initial = true;
        self.allow_nonblocking = true;
        let store = self.exec_block(Store::default(), &[], body)?;
        let mut ids: Vec<SourceVarId> = store
            .keys()
            .copied()
            .filter(|id| !self.m.is_hidden(*id))
            .collect();
        ids.sort_by_key(|id| id.0);
        let mut values = Vec::new();
        for id in ids {
            let Some(range) = store.get(&id) else {
                continue;
            };
            let mut runs = Vec::new();
            for (&lsb, (value, width, origin)) in &range.ranges {
                let Some((node, _)) = value else { continue };
                let (value, mask, _) = slt_const4(self.arena, *node).ok_or_else(|| {
                    unsupported(format!(
                        "initial block value of `{}` that depends on design state",
                        self.m.var(id).path.join(".")
                    ))
                })?;
                let shift = lsb - origin;
                let keep = (BigUint::from(1u8) << *width) - BigUint::from(1u8);
                let value = (value >> shift) & &keep;
                let mask = (mask >> shift) & &keep;
                runs.push(celox_design::InitialStateWriteRun {
                    bit_offset: lsb,
                    bit_width: *width,
                    value_bytes: celox_frontend_core::memory_file::biguint_to_fixed_le_bytes(
                        &value, *width,
                    ),
                    mask_bytes: celox_frontend_core::memory_file::biguint_to_fixed_le_bytes(
                        &mask, *width,
                    ),
                });
            }
            if !runs.is_empty() {
                values.push(InitialStateValue {
                    address: id,
                    data: InitialStateData::Writes(runs),
                });
            }
        }
        Ok(values)
    }

    /// Execute one combinational process and return the logic it defines.
    pub fn lower_process(
        &mut self,
        body: &[sv::ir::Stmt],
    ) -> Result<Vec<LogicPath<SourceVarId>>, sv::AnalyzerError> {
        let store = self.exec_block(Store::default(), &[], body)?;
        // Every bit a process may write must be defined on every path;
        // otherwise it would hold its previous value (a latch).
        let mut visited = HashSet::default();
        let mut static_writes = Vec::new();
        self.static_writes(body, &mut visited, &mut static_writes);
        for (id, access) in static_writes {
            let defined = store.get(&id).is_some_and(|range| {
                range
                    .get_parts_ref(access)
                    .is_ok_and(|parts| parts.iter().all(|(value, _)| value.is_some()))
            });
            if !defined {
                return Err(unsupported("latch inference inside always_comb"));
            }
        }
        let mut written: Vec<SourceVarId> = store
            .keys()
            .copied()
            .filter(|id| !self.m.is_hidden(*id))
            .collect();
        written.sort_by_key(|id| id.0);
        let mut written_atoms = Vec::new();
        for id in &written {
            if let Some(range) = store.get(id) {
                for (&lsb, (value, width, _)) in &range.ranges {
                    if value.is_some() {
                        written_atoms.push(VarAtomBase::new(*id, lsb, lsb + width - 1));
                    }
                }
            }
        }
        let mut paths = Vec::new();
        for id in written {
            let Some(range) = store.get(&id) else {
                continue;
            };
            let ranges: Vec<_> = range
                .ranges
                .iter()
                .map(|(&lsb, (value, width, origin))| (lsb, value.clone(), *width, *origin))
                .collect();
            for (lsb, value, width, origin) in ranges {
                let Some((expr, sources)) = value else {
                    continue;
                };
                let msb = lsb + width - 1;
                let relative = BitAccess::new(lsb - origin, msb - origin);
                let expr = self.slice(expr, relative)?;
                let target = BitAccess::new(lsb, msb);
                if sources
                    .iter()
                    .any(|source| source.id == id && source.access.overlaps(&target))
                {
                    return Err(unsupported("latch inference inside always_comb"));
                }
                let previous_sources = sources
                    .iter()
                    .copied()
                    .filter(|source| source.id != id || !source.access.overlaps(&target))
                    .filter(|source| {
                        written_atoms.iter().any(|written| {
                            written.id == source.id && written.access.overlaps(&source.access)
                        })
                    })
                    .collect();
                paths.push(LogicPath {
                    target: LogicPathTarget::Var(VarAtomBase::new(id, lsb, msb)),
                    sources: sources
                        .into_iter()
                        .filter(|source| !self.m.is_hidden(source.id))
                        .collect(),
                    previous_sources,
                    address_sources: HashSet::default(),
                    local_inputs: Vec::new(),
                    order_before: HashSet::default(),
                    comb_capture_enable_sites: Vec::new(),
                    comb_capture_enable_always: false,
                    pre_lower_nodes: Vec::new(),
                    expr,
                });
            }
        }
        if !self.observers.is_empty() {
            let mut sensitivity: Sources = std::mem::take(&mut self.effect_sensitivity);
            for path in &paths {
                sensitivity.extend(path.sources.iter().copied());
            }
            let sensitivity: Vec<_> = subtract_written_sensitivity(sensitivity, &written_atoms)
                .into_iter()
                .filter(|atom| !self.m.is_hidden(atom.id))
                .collect();
            for observer in &mut self.observers {
                observer.sensitivity = sensitivity.clone();
                observer.written_input_atoms = observer
                    .observed_inputs
                    .iter()
                    .chain(observer.position_inputs.iter())
                    .copied()
                    .filter(|atom| {
                        written_atoms.iter().any(|written| {
                            written.id == atom.id && written.access.overlaps(&atom.access)
                        })
                    })
                    .collect();
                let mut written_inputs: Vec<_> = observer
                    .written_input_atoms
                    .iter()
                    .map(|atom| atom.id)
                    .collect();
                written_inputs.sort_by_key(|id| id.0);
                written_inputs.dedup();
                observer.written_inputs = written_inputs;
            }
        }
        Ok(paths)
    }
}

/// `atoms` without the bits in `written`.
fn subtract_written_sensitivity(atoms: Sources, written: &[VarAtomBase<SourceVarId>]) -> Sources {
    let mut result = Sources::default();
    for atom in atoms {
        let mut ranges = vec![(atom.access.lsb, atom.access.msb)];
        for written in written.iter().filter(|written| written.id == atom.id) {
            ranges = ranges
                .into_iter()
                .flat_map(|(lsb, msb)| {
                    if written.access.msb < lsb || written.access.lsb > msb {
                        return vec![(lsb, msb)];
                    }
                    let mut kept = Vec::new();
                    if lsb < written.access.lsb {
                        kept.push((lsb, written.access.lsb - 1));
                    }
                    if written.access.msb < msb {
                        kept.push((written.access.msb + 1, msb));
                    }
                    kept
                })
                .collect();
        }
        for (lsb, msb) in ranges {
            result.insert(VarAtomBase::new(atom.id, lsb, msb));
        }
    }
    result
}

impl Comb<'_, '_> {
    /// The module variables (and constant bit ranges) a statement list may
    /// write, following subroutine calls.
    fn static_writes(
        &self,
        stmts: &[sv::ir::Stmt],
        visited: &mut HashSet<String>,
        writes: &mut Vec<(SourceVarId, BitAccess)>,
    ) {
        let mut calls = Vec::new();
        let record =
            |this: &Self, lvalue: &sv::ir::LValue, writes: &mut Vec<(SourceVarId, BitAccess)>| {
                let Some(id) = this.m.id(lvalue.name()) else {
                    return;
                };
                if this.m.is_hidden(id) {
                    return;
                }
                let variable = this.m.var(id);
                let access = match lvalue {
                    sv::ir::LValue::Ident(_) => full(variable.width),
                    sv::ir::LValue::Select { msb, lsb, .. } => {
                        let bounds = sv::typecheck::eval_const_expr_with_types(
                            msb,
                            this.m.constants,
                            this.m.parameter_types,
                        )
                        .zip(sv::typecheck::eval_const_expr_with_types(
                            lsb,
                            this.m.constants,
                            this.m.parameter_types,
                        ))
                        .and_then(|(msb, lsb)| {
                            Some((
                                packed_index_offset(variable, msb)?,
                                packed_index_offset(variable, lsb)?,
                            ))
                        });
                        match bounds {
                            Some((msb, lsb)) => BitAccess::new(msb.min(lsb), msb.max(lsb)),
                            // A run-time select may write any bit; the written
                            // value keeps the others, so it defines the whole vector.
                            None => full(variable.width),
                        }
                    }
                };
                writes.push((id, access));
            };
        for stmt in stmts {
            stmt.walk(&mut |stmt| match stmt {
                sv::ir::Stmt::Assign { lhs, .. } => record(self, lhs, writes),
                sv::ir::Stmt::AssignConcat { parts, .. } => {
                    for part in parts {
                        record(self, part, writes);
                    }
                }
                sv::ir::Stmt::Call { name, args } => calls.push((name.clone(), args.clone())),
                _ => {}
            });
        }
        for (name, args) in calls {
            let Some(subroutine) = self.m.subroutine(&name).cloned() else {
                continue;
            };
            for (param, arg) in subroutine.params.iter().zip(&args) {
                if param.direction.is_written()
                    && let Some(lvalues) = arg.as_ref().and_then(lvalue_from_expr)
                {
                    for lvalue in &lvalues {
                        record(self, lvalue, writes);
                    }
                }
            }
            if visited.insert(name) {
                self.static_writes(&subroutine.body, visited, writes);
            }
        }
    }
}

/// Iterations unrolled under a run-time loop condition before the loop is
/// considered unbounded.
const MAX_SYMBOLIC_ITERATIONS: usize = 1024;

enum Label {
    Value((BigUint, BigUint, usize, bool)),
    Range(BigUint, BigUint),
}

enum Target {
    Static {
        id: SourceVarId,
        access: BitAccess,
        lvalue: sv::ir::LValue,
    },
    Dynamic {
        id: SourceVarId,
        offset: Offset,
        width: usize,
    },
}

enum Offset {
    /// A constant select outside the vector.
    Nothing,
    /// An element of an unpacked array at a run-time index.
    Element {
        offset: sv::ir::ConstExpr,
        element_width: usize,
        element_count: usize,
        access: BitAccess,
    },
    /// A run-time position in a packed vector; see [`RuntimePosition`].
    Packed {
        up: sv::ir::Expr,
        down: sv::ir::Expr,
    },
}

/// Replace identifiers by literals, in expressions and select bounds.
fn substitute_literals(
    expr: &sv::ir::Expr,
    literals: &HashMap<String, (String, i128)>,
) -> sv::ir::Expr {
    fn constant(
        expr: &sv::ir::ConstExpr,
        literals: &HashMap<String, (String, i128)>,
    ) -> sv::ir::ConstExpr {
        use sv::ir::ConstExpr;
        match expr {
            ConstExpr::Ident(name) => match literals.get(name) {
                Some((_, value)) => ConstExpr::Literal(value.to_string()),
                None => expr.clone(),
            },
            ConstExpr::Literal(_) => expr.clone(),
            ConstExpr::Select { expr, bit } => ConstExpr::Select {
                expr: Box::new(constant(expr, literals)),
                bit: Box::new(constant(bit, literals)),
            },
            ConstExpr::Function { name, args } => ConstExpr::Function {
                name: name.clone(),
                args: args.iter().map(|arg| constant(arg, literals)).collect(),
            },
            ConstExpr::Unary { op, expr } => ConstExpr::Unary {
                op: *op,
                expr: Box::new(constant(expr, literals)),
            },
            ConstExpr::Binary { left, op, right } => ConstExpr::Binary {
                left: Box::new(constant(left, literals)),
                op: *op,
                right: Box::new(constant(right, literals)),
            },
            ConstExpr::Mux {
                condition,
                then_expr,
                else_expr,
            } => ConstExpr::Mux {
                condition: Box::new(constant(condition, literals)),
                then_expr: Box::new(constant(then_expr, literals)),
                else_expr: Box::new(constant(else_expr, literals)),
            },
        }
    }
    use sv::ir::Expr;
    let go = |expr: &Expr| substitute_literals(expr, literals);
    match expr {
        Expr::Ident(name) => match literals.get(name) {
            Some((literal, _)) => Expr::Literal(literal.clone()),
            None => expr.clone(),
        },
        Expr::Literal(_) => expr.clone(),
        Expr::Select {
            expr,
            msb,
            lsb,
            signed,
        } => Expr::Select {
            expr: Box::new(go(expr)),
            msb: constant(msb, literals),
            lsb: constant(lsb, literals),
            signed: *signed,
        },
        Expr::Concat(parts) => Expr::Concat(parts.iter().map(go).collect()),
        Expr::RepeatConcat { count, parts } => Expr::RepeatConcat {
            count: constant(count, literals),
            parts: parts.iter().map(go).collect(),
        },
        Expr::Resize {
            expr,
            width,
            signed,
        } => Expr::Resize {
            expr: Box::new(go(expr)),
            width: *width,
            signed: *signed,
        },
        Expr::Unary { op, expr } => Expr::Unary {
            op: *op,
            expr: Box::new(go(expr)),
        },
        Expr::Binary { left, op, right } => Expr::Binary {
            left: Box::new(go(left)),
            op: *op,
            right: Box::new(go(right)),
        },
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => Expr::Mux {
            condition: Box::new(go(condition)),
            then_expr: Box::new(go(then_expr)),
            else_expr: Box::new(go(else_expr)),
        },
        Expr::Call { name, args } => Expr::Call {
            name: name.clone(),
            args: args.iter().map(go).collect(),
        },
        Expr::Inside { expr, items } => Expr::Inside {
            expr: Box::new(go(expr)),
            items: items
                .iter()
                .map(|item| item.map(&mut |operand| go(operand)))
                .collect(),
        },
    }
}
