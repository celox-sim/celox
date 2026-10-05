//! Imperative SIR for sequential processes.
//!
//! An `always_ff` body becomes straight SIR control flow: branches for `if`
//! and `case`, loop blocks for loops whose trip count is not known, and
//! inlined subroutine bodies. Nonblocking targets are read from the stable
//! region and written to the working region, so every process of a trigger
//! observes the pre-edge state; blocking targets and procedural locals are
//! both read and written in the working region (IEEE 1800-2023 10.4).
//! Expressions are lowered through SLT, which keeps their width and
//! four-state semantics identical to the combinational lowering.

use super::procedural::*;
use super::*;
use celox_sir::RegisterId;
use celox_slt::SLTToSIRLowerer;
use num_traits::{ToPrimitive, Zero};

type Addr = RegionedVarAddr;

struct LoopBlocks {
    break_block: BlockId,
    continue_block: BlockId,
}

struct FunctionBlocks {
    return_block: BlockId,
    return_var: Option<String>,
    loop_depth: usize,
}

pub(super) struct Ff<'p, 'a> {
    pub m: &'p mut ProcModule<'a>,
    b: SIRBuilder<Addr>,
    /// Variables read from the working region.
    working: HashSet<SourceVarId>,
    /// The bits whose working value is the process result.
    targets: Vec<(SourceVarId, BitAccess)>,
    loops: Vec<LoopBlocks>,
    functions: Vec<FunctionBlocks>,
    /// Loop variables of unrolled loops and their literal values.
    overlay: HashMap<String, (String, i128)>,
    active_calls: Vec<String>,
    element_widths: HashMap<Addr, usize>,
    unrolled: usize,
    /// Read-modify-write views of a variable's working value: alias -> var.
    aliases: HashMap<SourceVarId, SourceVarId>,
    next_alias: u32,
}

fn stable(var_id: SourceVarId) -> Addr {
    RegionedVarAddrBase {
        region: STABLE_REGION,
        var_id,
    }
}

fn working(var_id: SourceVarId) -> Addr {
    RegionedVarAddrBase {
        region: WORKING_REGION,
        var_id,
    }
}

impl<'p, 'a> Ff<'p, 'a> {
    pub fn new(m: &'p mut ProcModule<'a>) -> Self {
        let mut element_widths = HashMap::default();
        for (&id, variable) in m.variables.iter() {
            if let Some(width) = unpacked_element_width(variable) {
                element_widths.insert(stable(id), width);
                element_widths.insert(working(id), width);
            }
        }
        Self {
            m,
            b: SIRBuilder::new(),
            working: HashSet::default(),
            targets: Vec::new(),
            loops: Vec::new(),
            functions: Vec::new(),
            overlay: HashMap::default(),
            active_calls: Vec::new(),
            element_widths,
            unrolled: 0,
            aliases: HashMap::default(),
            next_alias: u32::MAX,
        }
    }

    /// Record a written lvalue: its constant bit range, or the whole variable.
    fn record_target(&mut self, lvalue: &sv::ir::LValue, blocking: bool) {
        let Some(id) = self.m.id(lvalue.name()) else {
            return;
        };
        if blocking {
            self.working.insert(id);
        }
        if self.m.is_hidden(id) {
            return;
        }
        let variable = self.m.var(id);
        let whole = BitAccess::new(0, variable.width - 1);
        let access = match lvalue {
            sv::ir::LValue::Ident(_) => whole,
            sv::ir::LValue::Select { msb, lsb, .. } => sv::typecheck::eval_const_expr_with_types(
                msb,
                self.m.constants,
                self.m.parameter_types,
            )
            .zip(sv::typecheck::eval_const_expr_with_types(
                lsb,
                self.m.constants,
                self.m.parameter_types,
            ))
            .and_then(|(msb, lsb)| {
                Some((
                    packed_index_offset(variable, msb)?,
                    packed_index_offset(variable, lsb)?,
                ))
            })
            .map_or(whole, |(msb, lsb)| {
                BitAccess::new(msb.min(lsb), msb.max(lsb))
            }),
        };
        self.targets.push((id, access));
    }

    /// Collect the variables a body writes, following calls into subroutines.
    fn scan(&mut self, stmts: &[sv::ir::Stmt], visited: &mut HashSet<String>) {
        let mut writes: Vec<(sv::ir::LValue, bool)> = Vec::new();
        for stmt in stmts {
            stmt.walk(&mut |stmt| match stmt {
                sv::ir::Stmt::Assign {
                    lhs, nonblocking, ..
                } => writes.push((lhs.clone(), !nonblocking)),
                sv::ir::Stmt::AssignConcat {
                    parts, nonblocking, ..
                } => {
                    writes.extend(parts.iter().map(|part| (part.clone(), !nonblocking)));
                }
                // `$readmemh` writes its destination immediately.
                sv::ir::Stmt::SystemTask { name, args } => {
                    if let Some(destination) = readmem_destination(name, args) {
                        writes.push((sv::ir::LValue::Ident(destination.to_string()), true));
                    }
                }
                _ => {}
            });
        }
        for (lvalue, blocking) in writes {
            self.record_target(&lvalue, blocking);
        }
        // Calls: output arguments and the subroutine bodies.
        let mut calls = Vec::new();
        for stmt in stmts {
            stmt.walk(&mut |stmt| {
                let mut exprs = Vec::new();
                match stmt {
                    sv::ir::Stmt::Call { name, args } => {
                        calls.push((name.clone(), args.clone()));
                        exprs.extend(args.iter().flatten().cloned());
                    }
                    sv::ir::Stmt::Assign { rhs, .. } | sv::ir::Stmt::AssignConcat { rhs, .. } => {
                        exprs.push(rhs.clone())
                    }
                    sv::ir::Stmt::If { condition, .. } => exprs.push(condition.clone()),
                    sv::ir::Stmt::Case {
                        selector, items, ..
                    } => {
                        exprs.push(selector.clone());
                        for item in items {
                            for label in &item.labels {
                                match label {
                                    sv::ir::CaseLabel::Value(value) => exprs.push(value.clone()),
                                    sv::ir::CaseLabel::Range { low, high } => {
                                        exprs.push(low.clone());
                                        exprs.push(high.clone());
                                    }
                                }
                            }
                        }
                    }
                    sv::ir::Stmt::Loop { condition, .. } => exprs.extend(condition.clone()),
                    sv::ir::Stmt::Return(Some(value)) => exprs.push(value.clone()),
                    sv::ir::Stmt::Local {
                        init: Some(init), ..
                    } => exprs.push(init.clone()),
                    _ => {}
                }
                for expr in exprs {
                    collect_calls(&expr, &mut calls);
                }
            });
        }
        for (name, args) in calls {
            let Some(subroutine) = self.m.subroutine(&name).cloned() else {
                continue;
            };
            for (param, arg) in subroutine.params.iter().zip(args.iter()) {
                if param.direction.is_written()
                    && let Some(lvalues) = arg.as_ref().and_then(lvalue_from_expr)
                {
                    for lvalue in lvalues {
                        self.record_target(&lvalue, true);
                    }
                }
            }
            if visited.insert(name) {
                self.scan(&subroutine.body, visited);
            }
        }
    }

    // ----------------------------------------------------------- expressions

    fn lower_slt(
        &mut self,
        arena: &SLTNodeArena<SourceVarId>,
        node: NodeId,
    ) -> Result<RegisterId, sv::AnalyzerError> {
        let mut mapped_arena = SLTNodeArena::<Addr>::new();
        let mut cache = HashMap::default();
        let aliases = &self.aliases;
        let working_set = &self.working;
        let variables = &*self.m.variables;
        let map = |id: &SourceVarId| -> Addr {
            if let Some(var) = aliases.get(id) {
                working(*var)
            } else if working_set.contains(id) || variables.get(id).is_some_and(|var| var.hidden) {
                working(*id)
            } else {
                stable(*id)
            }
        };
        let mapped = arena
            .get(node)
            .map_addr(node, arena, &mut mapped_arena, &mut cache, &map)
            .map_err(slt_error)?;
        let lowerer = SLTToSIRLowerer::new(self.m.four_state)
            .with_unpacked_input_types(&mapped_arena, &self.element_widths);
        let mut lower_cache = HashMap::default();
        Ok(lowerer.lower(&mut self.b, mapped, &mapped_arena, &mut lower_cache))
    }

    fn propagate(&self, expr: &sv::ir::Expr) -> sv::ir::Expr {
        if self.overlay.is_empty() {
            return expr.clone();
        }
        substitute_overlay(expr, &self.overlay)
    }

    fn propagate_lvalue(&self, lvalue: &sv::ir::LValue) -> sv::ir::LValue {
        match lvalue {
            sv::ir::LValue::Ident(_) => lvalue.clone(),
            sv::ir::LValue::Select {
                name,
                msb,
                lsb,
                array_slice_width,
                array_slice_reversed,
            } => sv::ir::LValue::Select {
                name: name.clone(),
                msb: substitute_overlay_const(msb, &self.overlay),
                lsb: substitute_overlay_const(lsb, &self.overlay),
                array_slice_width: array_slice_width.clone(),
                array_slice_reversed: *array_slice_reversed,
            },
        }
    }

    /// Lower an expression to SLT with its calls already executed.
    fn expr_slt(
        &mut self,
        expr: &sv::ir::Expr,
        context: Option<(usize, bool)>,
        arena: &mut SLTNodeArena<SourceVarId>,
    ) -> Result<NodeId, sv::AnalyzerError> {
        let expr = expr_for_state_mode(expr, self.m.four_state);
        let expr = if expr_calls(&expr, &self.m.subroutines) {
            self.hoist(&expr)?
        } else {
            expr
        };
        let expr = self.propagate(&expr);
        if let (sv::ir::Expr::Literal(literal), Some((width, _))) = (&expr, context)
            && let Some(fill) = unbased_fill_literal(literal)
        {
            return lower_unbased_fill_literal_slt(arena, fill, width)
                .ok_or_else(|| unsupported(format!("expression `{literal}`")));
        }
        let (node, _) = lower_expr_with_context(
            &expr,
            self.m.variables,
            self.m.name_to_id,
            self.m.constants,
            self.m.parameter_types,
            arena,
            context.map(|(width, _)| width),
            context.map(|(_, signed)| signed),
        )
        .ok_or_else(|| unsupported("always_ff expression"))?;
        Ok(node)
    }

    fn eval(
        &mut self,
        expr: &sv::ir::Expr,
        context: Option<(usize, bool)>,
    ) -> Result<RegisterId, sv::AnalyzerError> {
        let mut arena = SLTNodeArena::new();
        let mut node = self.expr_slt(expr, context, &mut arena)?;
        if let Some((width, signed)) = context {
            node = coerce_node_width(&mut arena, node, Some(width), signed).map_err(slt_error)?;
        }
        self.lower_slt(&arena, node)
    }

    /// The truth of a procedural condition as a one-bit register.
    fn eval_truth(
        &mut self,
        expr: &sv::ir::Expr,
    ) -> Result<(RegisterId, Option<bool>), sv::AnalyzerError> {
        let mut arena = SLTNodeArena::new();
        let node = self.expr_slt(expr, None, &mut arena)?;
        let truth = slt_truth(&mut arena, node)?;
        let constant = slt_bool(&arena, truth);
        Ok((self.lower_slt(&arena, truth)?, constant))
    }

    /// The constant value of an expression, if it has one.
    fn eval_const(
        &mut self,
        expr: &sv::ir::Expr,
    ) -> Result<Option<(BigUint, usize)>, sv::AnalyzerError> {
        if expr_calls(expr, &self.m.subroutines) {
            return Ok(None);
        }
        let expr = self.propagate(&expr_for_state_mode(expr, self.m.four_state));
        let mut arena = SLTNodeArena::new();
        let Some((node, _)) = lower_expr_with_context(
            &expr,
            self.m.variables,
            self.m.name_to_id,
            self.m.constants,
            self.m.parameter_types,
            &mut arena,
            None,
            None,
        ) else {
            return Ok(None);
        };
        Ok(slt_const(&arena, node))
    }

    fn expr_signed(&self, expr: &sv::ir::Expr) -> bool {
        self.m.expr_signed(expr)
    }

    /// Execute the user subroutine calls of an expression and replace each by
    /// a hidden variable holding its result.
    fn hoist(&mut self, expr: &sv::ir::Expr) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        use sv::ir::Expr;
        if !expr_calls(expr, &self.m.subroutines) {
            return Ok(expr.clone());
        }
        Ok(match expr {
            Expr::Call { name, args } if self.m.subroutines.contains_key(name) => {
                let args: Vec<Option<Expr>> = args.iter().cloned().map(Some).collect();
                let subroutine = self
                    .m
                    .subroutine(name)
                    .cloned()
                    .ok_or_else(|| unsupported(format!("function `{name}`")))?;
                let Some(r#type) = subroutine.return_type.clone() else {
                    return Err(unsupported(format!(
                        "void function `{name}` used as a value"
                    )));
                };
                let (width, signed, is_4state) = self.m.type_shape(&r#type)?;
                let result = self.call(name, &args)?;
                let (temp, temp_name) = self.m.temp("call", width, signed, is_4state);
                let Some(result) = result else {
                    return Err(unsupported(format!(
                        "void function `{name}` used as a value"
                    )));
                };
                let value = self.eval(&Expr::Ident(result), Some((width, signed)))?;
                self.store(temp, SIROffset::Static(0), width, value);
                Expr::Ident(temp_name)
            }
            Expr::Binary { left, op, right }
                if matches!(op, sv::ir::BinaryOp::LogicAnd | sv::ir::BinaryOp::LogicOr)
                    && expr_calls(right, &self.m.subroutines) =>
            {
                let left = self.hoist(left)?;
                let (truth, _) = self.eval_truth(&left)?;
                let taken = self.b.new_block();
                let join = self.b.new_block();
                let (true_block, false_block) = if *op == sv::ir::BinaryOp::LogicAnd {
                    (taken, join)
                } else {
                    (join, taken)
                };
                self.b.seal_block(SIRTerminator::Branch {
                    cond: truth,
                    true_block: (true_block, Vec::new()),
                    false_block: (false_block, Vec::new()),
                });
                self.b.switch_to_block(taken);
                let right = self.hoist(right)?;
                self.b.seal_block(SIRTerminator::Jump(join, Vec::new()));
                self.b.switch_to_block(join);
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
            } if expr_calls(then_expr, &self.m.subroutines)
                || expr_calls(else_expr, &self.m.subroutines) =>
            {
                let condition = self.hoist(condition)?;
                let (truth, _) = self.eval_truth(&condition)?;
                let then_block = self.b.new_block();
                let else_block = self.b.new_block();
                let join = self.b.new_block();
                self.b.seal_block(SIRTerminator::Branch {
                    cond: truth,
                    true_block: (then_block, Vec::new()),
                    false_block: (else_block, Vec::new()),
                });
                self.b.switch_to_block(then_block);
                let then_expr = self.hoist(then_expr)?;
                self.b.seal_block(SIRTerminator::Jump(join, Vec::new()));
                self.b.switch_to_block(else_block);
                let else_expr = self.hoist(else_expr)?;
                self.b.seal_block(SIRTerminator::Jump(join, Vec::new()));
                self.b.switch_to_block(join);
                Expr::Mux {
                    condition: Box::new(condition),
                    then_expr: Box::new(then_expr),
                    else_expr: Box::new(else_expr),
                }
            }
            Expr::Call { name, args } => Expr::Call {
                name: name.clone(),
                args: args
                    .iter()
                    .map(|arg| self.hoist(arg))
                    .collect::<Result<_, _>>()?,
            },
            Expr::Select {
                expr,
                msb,
                lsb,
                signed,
            } => Expr::Select {
                expr: Box::new(self.hoist(expr)?),
                msb: msb.clone(),
                lsb: lsb.clone(),
                signed: *signed,
            },
            Expr::Concat(parts) => Expr::Concat(
                parts
                    .iter()
                    .map(|part| self.hoist(part))
                    .collect::<Result<_, _>>()?,
            ),
            Expr::RepeatConcat { count, parts } => Expr::RepeatConcat {
                count: count.clone(),
                parts: parts
                    .iter()
                    .map(|part| self.hoist(part))
                    .collect::<Result<_, _>>()?,
            },
            Expr::Resize {
                expr,
                width,
                signed,
            } => Expr::Resize {
                expr: Box::new(self.hoist(expr)?),
                width: *width,
                signed: *signed,
            },
            Expr::Unary { op, expr } => Expr::Unary {
                op: *op,
                expr: Box::new(self.hoist(expr)?),
            },
            Expr::Binary { left, op, right } => Expr::Binary {
                left: Box::new(self.hoist(left)?),
                op: *op,
                right: Box::new(self.hoist(right)?),
            },
            Expr::Mux {
                condition,
                then_expr,
                else_expr,
            } => Expr::Mux {
                condition: Box::new(self.hoist(condition)?),
                then_expr: then_expr.clone(),
                else_expr: else_expr.clone(),
            },
            Expr::Inside { expr, items } => Expr::Inside {
                expr: Box::new(self.hoist(expr)?),
                items: items
                    .iter()
                    .map(|item| -> Result<sv::ir::InsideItem, sv::AnalyzerError> {
                        Ok(match item {
                            sv::ir::InsideItem::Value(value) => {
                                sv::ir::InsideItem::Value(self.hoist(value)?)
                            }
                            sv::ir::InsideItem::Range { low, high } => sv::ir::InsideItem::Range {
                                low: self.hoist(low)?,
                                high: self.hoist(high)?,
                            },
                        })
                    })
                    .collect::<Result<_, _>>()?,
            },
            Expr::Ident(_) | Expr::Literal(_) => expr.clone(),
        })
    }

    // ---------------------------------------------------------------- stores

    fn store(&mut self, id: SourceVarId, offset: SIROffset, width: usize, value: RegisterId) {
        self.b.emit(SIRInstruction::Store(
            working(id),
            offset,
            width,
            value,
            Vec::new(),
            Vec::new(),
        ));
    }

    fn alias(&mut self, id: SourceVarId) -> SourceVarId {
        let alias = SourceVarId(self.next_alias);
        self.next_alias -= 1;
        self.aliases.insert(alias, id);
        alias
    }

    fn default_reg(&mut self, id: SourceVarId) -> RegisterId {
        let var = self.m.var(id);
        let width = var.width;
        let unknown = var.is_4state && self.m.four_state;
        let reg = if unknown {
            self.b.alloc_logic(width)
        } else {
            self.b.alloc_bit(width, false)
        };
        let mask = if unknown {
            (BigUint::from(1u8) << width) - BigUint::from(1u8)
        } else {
            BigUint::zero()
        };
        self.b.emit(SIRInstruction::Imm(
            reg,
            SIRValue::new_four_state(BigUint::zero(), mask),
        ));
        reg
    }

    fn init_default(&mut self, id: SourceVarId) {
        let width = self.m.var(id).width;
        let value = self.default_reg(id);
        let offset = sv_memory_offset(self.m.var(id), 0, width);
        self.store(id, offset, width, value);
    }

    fn assign(
        &mut self,
        lhs: &sv::ir::LValue,
        rhs: &sv::ir::Expr,
    ) -> Result<(), sv::AnalyzerError> {
        let lhs = self.propagate_lvalue(lhs);
        let width = self.lvalue_width(&lhs)?;
        let signed = self.expr_signed(rhs);
        let mut arena = SLTNodeArena::new();
        let node = self.expr_slt(rhs, Some((width, signed)), &mut arena)?;
        self.write(&lhs, &mut arena, node, signed, rhs)
    }

    fn lvalue_width(&self, lvalue: &sv::ir::LValue) -> Result<usize, sv::AnalyzerError> {
        let id = self
            .m
            .id(lvalue.name())
            .ok_or_else(|| unsupported(format!("assignment target `{}`", lvalue.name())))?;
        let variable = self.m.var(id);
        let constants = self.m.constants;
        let parameter_types = self.m.parameter_types;
        match lvalue {
            sv::ir::LValue::Ident(_) => Ok(variable.width),
            sv::ir::LValue::Select { msb, lsb, .. } => {
                if let (Some(msb), Some(lsb)) = (
                    sv::typecheck::eval_const_expr_with_types(msb, constants, parameter_types),
                    sv::typecheck::eval_const_expr_with_types(lsb, constants, parameter_types),
                ) {
                    return Ok(usize::try_from(msb.abs_diff(lsb)).unwrap_or(0) + 1);
                }
                if let Some((_, _, _, access)) = dynamic_array_element_lvalue(
                    lvalue,
                    self.m.variables,
                    self.m.name_to_id,
                    constants,
                    parameter_types,
                ) {
                    return Ok(access.msb - access.lsb + 1);
                }
                if let Some(write) = dynamic_packed_write(
                    lvalue,
                    self.m.variables,
                    self.m.name_to_id,
                    constants,
                    parameter_types,
                ) {
                    return Ok(write.select_width);
                }
                Err(unsupported(format!(
                    "assignment target `{}`",
                    lvalue.name()
                )))
            }
        }
    }

    /// Store the SLT value `node` into `lhs`.
    fn write(
        &mut self,
        lhs: &sv::ir::LValue,
        arena: &mut SLTNodeArena<SourceVarId>,
        node: NodeId,
        signed: bool,
        rhs: &sv::ir::Expr,
    ) -> Result<(), sv::AnalyzerError> {
        let id = self
            .m
            .id(lhs.name())
            .ok_or_else(|| unsupported(format!("assignment target `{}`", lhs.name())))?;
        let width = self.lvalue_width(lhs)?;
        let mut node = coerce_node_width(arena, node, Some(width), signed).map_err(slt_error)?;
        let (var_width, is_4state) = {
            let var = self.m.var(id);
            (var.width, var.is_4state)
        };
        if !is_4state || (!self.m.four_state && expr_is_unknown_literal(rhs)) {
            node = arena
                .alloc(SLTNode::Unary(UnaryOp::ToTwoState, node))
                .map_err(slt_error)?;
        }
        let constants = self.m.constants;
        let parameter_types = self.m.parameter_types;
        match lhs {
            sv::ir::LValue::Ident(_) => {
                let value = self.lower_slt(arena, node)?;
                let offset = sv_memory_offset(self.m.var(id), 0, var_width);
                self.store(id, offset, var_width, value);
                Ok(())
            }
            sv::ir::LValue::Select { msb, lsb, .. } => {
                if let (Some(msb), Some(lsb)) = (
                    sv::typecheck::eval_const_expr_with_types(msb, constants, parameter_types),
                    sv::typecheck::eval_const_expr_with_types(lsb, constants, parameter_types),
                ) {
                    let variable = self.m.var(id);
                    let (Some(msb), Some(lsb)) = (
                        packed_index_offset(variable, msb),
                        packed_index_offset(variable, lsb),
                    ) else {
                        return Ok(());
                    };
                    let low = msb.min(lsb);
                    let node = permute_reversed_lvalue_rhs_slt(
                        lhs,
                        node,
                        width,
                        constants,
                        parameter_types,
                        arena,
                    )
                    .ok_or_else(|| unsupported("assignment lvalue order"))?;
                    let value = self.lower_slt(arena, node)?;
                    let offset = sv_memory_offset(self.m.var(id), low, width);
                    self.store(id, offset, width, value);
                    return Ok(());
                }
                if let Some((_, element_width, offset, access)) = dynamic_array_element_lvalue(
                    lhs,
                    self.m.variables,
                    self.m.name_to_id,
                    constants,
                    parameter_types,
                ) {
                    return self.write_element(id, element_width, &offset, access, arena, node);
                }
                if let Some(write) = dynamic_packed_write(
                    lhs,
                    self.m.variables,
                    self.m.name_to_id,
                    constants,
                    parameter_types,
                ) {
                    let (up, _) = lower_expr_with_context(
                        &self.propagate(&write.up),
                        self.m.variables,
                        self.m.name_to_id,
                        constants,
                        parameter_types,
                        arena,
                        None,
                        Some(false),
                    )
                    .ok_or_else(|| unsupported("dynamic assignment position"))?;
                    let (down, _) = lower_expr_with_context(
                        &self.propagate(&write.down),
                        self.m.variables,
                        self.m.name_to_id,
                        constants,
                        parameter_types,
                        arena,
                        None,
                        Some(false),
                    )
                    .ok_or_else(|| unsupported("dynamic assignment position"))?;
                    let alias = self.alias(id);
                    let current = arena
                        .alloc(SLTNode::Input {
                            variable: alias,
                            signed: false,
                            index: Vec::new(),
                            access: BitAccess::new(0, var_width - 1),
                        })
                        .map_err(slt_error)?;
                    let wide = var_width
                        .max(celox_slt::get_width(up, arena))
                        .max(celox_slt::get_width(down, arena));
                    let mask = slt_constant(
                        arena,
                        (BigUint::from(1u8) << width) - BigUint::from(1u8),
                        wide,
                        false,
                    )?;
                    let coerce = |arena: &mut SLTNodeArena<SourceVarId>, node| {
                        coerce_node_width(arena, node, Some(wide), false).map_err(slt_error)
                    };
                    let value = coerce(arena, node)?;
                    let current = coerce(arena, current)?;
                    let up = coerce(arena, up)?;
                    let down = coerce(arena, down)?;
                    let place = |arena: &mut SLTNodeArena<SourceVarId>,
                                 value|
                     -> Result<NodeId, sv::AnalyzerError> {
                        let raised = arena
                            .alloc(SLTNode::Binary(value, BinaryOp::Shl, up))
                            .map_err(slt_error)?;
                        arena
                            .alloc(SLTNode::Binary(raised, BinaryOp::Shr, down))
                            .map_err(slt_error)
                    };
                    let placed_mask = place(arena, mask)?;
                    let placed_value = place(arena, value)?;
                    let placed_value = arena
                        .alloc(SLTNode::Binary(placed_value, BinaryOp::And, placed_mask))
                        .map_err(slt_error)?;
                    let keep = arena
                        .alloc(SLTNode::Unary(UnaryOp::BitNot, placed_mask))
                        .map_err(slt_error)?;
                    let kept = arena
                        .alloc(SLTNode::Binary(current, BinaryOp::And, keep))
                        .map_err(slt_error)?;
                    let mut updated = arena
                        .alloc(SLTNode::Binary(kept, BinaryOp::Or, placed_value))
                        .map_err(slt_error)?;
                    if self.m.four_state {
                        // A write at an unknown position is ignored (IEEE 1800-2023 11.5.1).
                        let mut known = None;
                        for position in [up, down] {
                            let two_state = arena
                                .alloc(SLTNode::Unary(UnaryOp::ToTwoState, position))
                                .map_err(slt_error)?;
                            let same = arena
                                .alloc(SLTNode::Binary(position, BinaryOp::EqCase, two_state))
                                .map_err(slt_error)?;
                            known = Some(match known {
                                None => same,
                                Some(other) => slt_and(arena, other, same)?,
                            });
                        }
                        if let Some(known) = known {
                            updated = arena
                                .alloc(SLTNode::Mux {
                                    cond: known,
                                    then_expr: updated,
                                    else_expr: current,
                                })
                                .map_err(slt_error)?;
                        }
                    }
                    let updated = if wide == var_width {
                        updated
                    } else {
                        arena
                            .alloc(SLTNode::Slice {
                                expr: updated,
                                access: BitAccess::new(0, var_width - 1),
                            })
                            .map_err(slt_error)?
                    };
                    let value = self.lower_slt(arena, updated)?;
                    self.store(id, SIROffset::Static(0), var_width, value);
                    return Ok(());
                }
                Err(unsupported(format!("assignment target `{}`", lhs.name())))
            }
        }
    }

    /// Write one element (or a part of it) of an unpacked array at a run-time
    /// index. An unknown or out-of-range index writes nothing.
    fn write_element(
        &mut self,
        id: SourceVarId,
        element_width: usize,
        offset: &sv::ir::ConstExpr,
        access: BitAccess,
        arena: &mut SLTNodeArena<SourceVarId>,
        node: NodeId,
    ) -> Result<(), sv::AnalyzerError> {
        let target_width = access.msb - access.lsb + 1;
        let offset =
            expr_from_const_expr(offset).ok_or_else(|| unsupported("dynamic assignment index"))?;
        let offset = self.propagate(&offset);
        let (bit_offset, _) = lower_expr_with_context(
            &offset,
            self.m.variables,
            self.m.name_to_id,
            self.m.constants,
            self.m.parameter_types,
            arena,
            None,
            None,
        )
        .ok_or_else(|| unsupported("dynamic assignment index"))?;
        let bit_offset =
            coerce_node_width(arena, bit_offset, Some(64), false).map_err(slt_error)?;
        let stride = slt_constant(arena, BigUint::from(element_width), 64, false)?;
        let index = arena
            .alloc(SLTNode::Binary(bit_offset, BinaryOp::DivU, stride))
            .map_err(slt_error)?;
        let element_count = self.m.var(id).width / element_width.max(1);
        let (index, valid) = dynamic_array_index_guard_slt(arena, index, element_count)
            .ok_or_else(|| unsupported("dynamic assignment index"))?;
        let packed_element_width = unpacked_element_width(self.m.var(id))
            .ok_or_else(|| unsupported("dynamic assignment"))?;
        let index = self.lower_slt(arena, index)?;
        let valid = self.lower_slt(arena, valid)?;
        let value = self.lower_slt(arena, node)?;
        if element_width != packed_element_width {
            // A whole multi-dimensional element: one store per inner element.
            if access.lsb != 0 || target_width != element_width {
                return Err(unsupported(
                    "partial write of a multi-dimensional array element",
                ));
            }
            let inner_count = element_width / packed_element_width;
            let inner = self.b.alloc_bit(64, false);
            self.b.emit(SIRInstruction::Imm(
                inner,
                SIRValue::new(inner_count as u64),
            ));
            let scaled = self.b.alloc_bit(64, false);
            self.b
                .emit(SIRInstruction::Binary(scaled, index, BinaryOp::Mul, inner));
            for inner_index in 0..inner_count {
                let element_index = if inner_index == 0 {
                    scaled
                } else {
                    let step = self.b.alloc_bit(64, false);
                    self.b
                        .emit(SIRInstruction::Imm(step, SIRValue::new(inner_index as u64)));
                    let element_index = self.b.alloc_bit(64, false);
                    self.b.emit(SIRInstruction::Binary(
                        element_index,
                        scaled,
                        BinaryOp::Add,
                        step,
                    ));
                    element_index
                };
                let offset = SIROffset::Element {
                    index: element_index,
                    element_width: packed_element_width,
                    bit_offset: 0,
                    dynamic_bit_offset: None,
                };
                let old = self.b.alloc_logic(packed_element_width);
                self.b.emit(SIRInstruction::Load(
                    old,
                    working(id),
                    offset.clone(),
                    packed_element_width,
                ));
                let part = self.b.alloc_logic(packed_element_width);
                self.b.emit(SIRInstruction::Slice(
                    part,
                    value,
                    inner_index * packed_element_width,
                    packed_element_width,
                ));
                let stored = self.b.alloc_logic(packed_element_width);
                self.b.emit(SIRInstruction::Mux(stored, valid, part, old));
                self.store(id, offset, packed_element_width, stored);
            }
            return Ok(());
        }
        let offset = SIROffset::Element {
            index,
            element_width,
            bit_offset: access.lsb,
            dynamic_bit_offset: None,
        };
        let old = self.b.alloc_logic(target_width);
        self.b.emit(SIRInstruction::Load(
            old,
            working(id),
            offset.clone(),
            target_width,
        ));
        let stored = self.b.alloc_logic(target_width);
        self.b.emit(SIRInstruction::Mux(stored, valid, value, old));
        self.store(id, offset, target_width, stored);
        Ok(())
    }

    fn assign_concat(
        &mut self,
        parts: &[sv::ir::LValue],
        rhs: &sv::ir::Expr,
    ) -> Result<(), sv::AnalyzerError> {
        let parts: Vec<_> = parts
            .iter()
            .map(|part| self.propagate_lvalue(part))
            .collect();
        let widths = parts
            .iter()
            .map(|part| self.lvalue_width(part))
            .collect::<Result<Vec<_>, _>>()?;
        let total: usize = widths.iter().sum();
        let signed = self.expr_signed(rhs);
        let mut arena = SLTNodeArena::new();
        let node = self.expr_slt(rhs, Some((total, signed)), &mut arena)?;
        let node = coerce_node_width(&mut arena, node, Some(total), signed).map_err(slt_error)?;
        // Capture the value before any part is written.
        let (temp, temp_name) = self.m.temp("concat", total, false, true);
        let value = self.lower_slt(&arena, node)?;
        self.store(temp, SIROffset::Static(0), total, value);
        let mut lsb = total;
        for (part, width) in parts.iter().zip(widths) {
            lsb -= width;
            let slice = sv::ir::Expr::Select {
                expr: Box::new(sv::ir::Expr::Ident(temp_name.clone())),
                msb: sv::ir::ConstExpr::Literal((lsb + width - 1).to_string()),
                lsb: sv::ir::ConstExpr::Literal(lsb.to_string()),
                signed: false,
            };
            self.assign(part, &slice)?;
        }
        Ok(())
    }

    // --------------------------------------------------------------- control

    /// Lower a statement list; `false` when control cannot fall off its end.
    fn exec_block(&mut self, stmts: &[sv::ir::Stmt]) -> Result<bool, sv::AnalyzerError> {
        for stmt in stmts {
            if !self.exec(stmt)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn jump(&mut self, target: BlockId) {
        self.b.seal_block(SIRTerminator::Jump(target, Vec::new()));
    }

    fn exec(&mut self, stmt: &sv::ir::Stmt) -> Result<bool, sv::AnalyzerError> {
        match stmt {
            sv::ir::Stmt::Assign { lhs, rhs, .. } => {
                self.assign(lhs, rhs)?;
                Ok(true)
            }
            sv::ir::Stmt::AssignConcat { parts, rhs, .. } => {
                self.assign_concat(parts, rhs)?;
                Ok(true)
            }
            sv::ir::Stmt::Local { name, init } => {
                let id = self
                    .m
                    .id(name)
                    .ok_or_else(|| unsupported(format!("local `{name}`")))?;
                match init {
                    Some(init) => self.assign(&sv::ir::LValue::Ident(name.clone()), init)?,
                    None => self.init_default(id),
                }
                Ok(true)
            }
            sv::ir::Stmt::If {
                condition,
                then_body,
                else_body,
            } => {
                let (truth, constant) = self.eval_truth(condition)?;
                match constant {
                    Some(true) => return self.exec_block(then_body),
                    Some(false) => return self.exec_block(else_body),
                    None => {}
                }
                let then_block = self.b.new_block();
                let else_block = self.b.new_block();
                let join = self.b.new_block();
                self.b.seal_block(SIRTerminator::Branch {
                    cond: truth,
                    true_block: (then_block, Vec::new()),
                    false_block: (else_block, Vec::new()),
                });
                self.b.switch_to_block(then_block);
                let then_falls = self.exec_block(then_body)?;
                if then_falls {
                    self.jump(join);
                }
                self.b.switch_to_block(else_block);
                let else_falls = self.exec_block(else_body)?;
                if else_falls {
                    self.jump(join);
                }
                self.b.switch_to_block(join);
                if !then_falls && !else_falls {
                    // The join block is unreachable; give it a terminator.
                    self.b.seal_block(SIRTerminator::Return);
                    return Ok(false);
                }
                Ok(true)
            }
            sv::ir::Stmt::Case {
                kind,
                selector,
                items,
                default,
            } => self.exec_case(*kind, selector, items, default.as_deref()),
            sv::ir::Stmt::Loop {
                kind,
                init,
                condition,
                step,
                body,
            } => self.exec_loop(kind, init, condition.as_ref(), step, body),
            sv::ir::Stmt::Break | sv::ir::Stmt::Continue => {
                let blocks = self
                    .loops
                    .last()
                    .ok_or_else(|| unsupported("break or continue outside a loop"))?;
                if self
                    .functions
                    .last()
                    .is_some_and(|function| function.loop_depth == self.loops.len())
                {
                    return Err(unsupported("break or continue outside a loop"));
                }
                let target = if matches!(stmt, sv::ir::Stmt::Break) {
                    blocks.break_block
                } else {
                    blocks.continue_block
                };
                self.jump(target);
                Ok(false)
            }
            sv::ir::Stmt::Return(value) => {
                let function = self
                    .functions
                    .last()
                    .ok_or_else(|| unsupported("return outside a subroutine"))?;
                let (return_block, return_var) =
                    (function.return_block, function.return_var.clone());
                if let (Some(value), Some(return_var)) = (value, return_var) {
                    self.assign(&sv::ir::LValue::Ident(return_var), value)?;
                }
                self.jump(return_block);
                Ok(false)
            }
            sv::ir::Stmt::Call { name, args } => {
                if !self.m.subroutines.contains_key(name) {
                    return Err(unsupported(format!("call of `{name}`")));
                }
                self.call(name, args)?;
                Ok(true)
            }
            sv::ir::Stmt::SystemTask { name, args } => self.system_task(name, args),
        }
    }

    /// A system task statement: a runtime event, and for `$fatal` the end of
    /// simulation with an error.
    fn system_task(
        &mut self,
        name: &str,
        args: &[sv::ir::SystemTaskArg<sv::ir::Expr>],
    ) -> Result<bool, sv::AnalyzerError> {
        if readmem_destination(name, args).is_some() {
            let mut addresses = [None, None];
            for (slot, arg) in addresses.iter_mut().zip(args.iter().skip(2)) {
                let sv::ir::SystemTaskArg::Expr(expr) = arg else {
                    continue;
                };
                let signed = self.expr_signed(expr);
                let (value, width) = self
                    .eval_const(expr)?
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
            let (id, words) = self.m.readmem(name, args, addresses[0], addresses[1])?;
            for word in words {
                let width = word.access.msb - word.access.lsb + 1;
                let reg = if word.mask.is_zero() {
                    self.b.alloc_bit(width, false)
                } else {
                    self.b.alloc_logic(width)
                };
                self.b.emit(SIRInstruction::Imm(
                    reg,
                    SIRValue::new_four_state(word.value, word.mask),
                ));
                let offset = sv_memory_offset(self.m.var(id), word.access.lsb, width);
                self.store(id, offset, width, reg);
            }
            return Ok(true);
        }
        let kind =
            system_task_kind(name).ok_or_else(|| unsupported(format!("system task `{name}`")))?;
        let (template, values) = system_task_template(&kind, args);
        let mut regs = Vec::with_capacity(values.len());
        let mut arg_widths = Vec::with_capacity(values.len());
        let mut arg_signed = Vec::with_capacity(values.len());
        let mut arg_is_string = Vec::with_capacity(values.len());
        for value in &values {
            match value {
                sv::ir::SystemTaskArg::Expr(expr) => {
                    let signed = self.expr_signed(expr);
                    let reg = self.eval(expr, None)?;
                    arg_widths.push(self.b.register(&reg).width());
                    arg_signed.push(signed);
                    arg_is_string.push(false);
                    regs.push(reg);
                }
                sv::ir::SystemTaskArg::Str(text) => {
                    let bytes = unescape(text).into_bytes();
                    let width = (bytes.len() * 8).max(8);
                    let value = bytes.iter().fold(BigUint::zero(), |value, byte| {
                        (value << 8u32) | BigUint::from(*byte)
                    });
                    let reg = self.b.alloc_bit(width, false);
                    self.b.emit(SIRInstruction::Imm(reg, SIRValue::new(value)));
                    arg_widths.push(width);
                    arg_signed.push(false);
                    arg_is_string.push(true);
                    regs.push(reg);
                }
                sv::ir::SystemTaskArg::Empty => {
                    return Err(unsupported(format!("empty argument of `{name}`")));
                }
            }
        }
        let event_kind = match &kind {
            SystemTaskKind::Print(kind) => *kind,
            SystemTaskKind::Finish => RuntimeEventKind::Finish,
            SystemTaskKind::Message => RuntimeEventKind::AssertContinue,
            SystemTaskKind::Fatal => RuntimeEventKind::AssertFatal,
        };
        let site_id = self.m.event_site(RuntimeEventSite {
            kind: event_kind,
            template: template.clone(),
            scope: None,
            arg_widths,
            arg_signed,
            arg_is_string,
        });
        self.b.emit(SIRInstruction::RuntimeEvent {
            site_id,
            args: regs,
        });
        if matches!(kind, SystemTaskKind::Fatal) {
            let code = self
                .m
                .runtime_error(template.unwrap_or_else(|| "$fatal".to_string()));
            self.b.seal_block(SIRTerminator::Error(code));
            return Ok(false);
        }
        Ok(true)
    }

    fn exec_case(
        &mut self,
        kind: sv::ir::CaseKind,
        selector: &sv::ir::Expr,
        items: &[sv::ir::CaseItem],
        default: Option<&[sv::ir::Stmt]>,
    ) -> Result<bool, sv::AnalyzerError> {
        // Evaluate the selector once.
        let selector = if expr_calls(selector, &self.m.subroutines) {
            self.hoist(selector)?
        } else {
            selector.clone()
        };
        let join = self.b.new_block();
        let mut any_falls = false;
        for item in items {
            let mut terms = Vec::new();
            for label in &item.labels {
                terms.push(match (kind, label) {
                    (sv::ir::CaseKind::Inside, label) => sv::ir::Expr::Inside {
                        expr: Box::new(selector.clone()),
                        items: vec![match label {
                            sv::ir::CaseLabel::Value(value) => {
                                sv::ir::InsideItem::Value(value.clone())
                            }
                            sv::ir::CaseLabel::Range { low, high } => sv::ir::InsideItem::Range {
                                low: low.clone(),
                                high: high.clone(),
                            },
                        }],
                    },
                    (_, sv::ir::CaseLabel::Value(value)) => sv::ir::Expr::Binary {
                        left: Box::new(selector.clone()),
                        op: if kind == sv::ir::CaseKind::Exact {
                            sv::ir::BinaryOp::EqCase
                        } else {
                            sv::ir::BinaryOp::EqWildcard
                        },
                        right: Box::new(value.clone()),
                    },
                    (_, sv::ir::CaseLabel::Range { .. }) => {
                        return Err(unsupported("range label outside case inside"));
                    }
                });
            }
            let condition = terms
                .into_iter()
                .reduce(|left, right| sv::ir::Expr::Binary {
                    left: Box::new(left),
                    op: sv::ir::BinaryOp::LogicOr,
                    right: Box::new(right),
                })
                .unwrap_or_else(|| sv::ir::Expr::Literal("1'b0".to_string()));
            let (truth, constant) = self.eval_truth(&condition)?;
            match constant {
                Some(false) => continue,
                Some(true) => {
                    let falls = self.exec_block(&item.body)?;
                    if falls {
                        self.jump(join);
                    } else {
                        // Control left through a jump; continue in a dead block.
                    }
                    self.b.switch_to_block(join);
                    if !falls && !any_falls {
                        self.b.seal_block(SIRTerminator::Return);
                        return Ok(false);
                    }
                    return Ok(true);
                }
                None => {}
            }
            let item_block = self.b.new_block();
            let next = self.b.new_block();
            self.b.seal_block(SIRTerminator::Branch {
                cond: truth,
                true_block: (item_block, Vec::new()),
                false_block: (next, Vec::new()),
            });
            self.b.switch_to_block(item_block);
            if self.exec_block(&item.body)? {
                any_falls = true;
                self.jump(join);
            }
            self.b.switch_to_block(next);
        }
        let default_falls = match default {
            Some(default) => self.exec_block(default)?,
            None => true,
        };
        if default_falls {
            any_falls = true;
            self.jump(join);
        }
        self.b.switch_to_block(join);
        if !any_falls {
            self.b.seal_block(SIRTerminator::Return);
            return Ok(false);
        }
        Ok(true)
    }

    fn exec_loop(
        &mut self,
        kind: &sv::ir::LoopKind<sv::ir::Expr>,
        init: &[sv::ir::Stmt],
        condition: Option<&sv::ir::Expr>,
        step: &[sv::ir::Stmt],
        body: &[sv::ir::Stmt],
    ) -> Result<bool, sv::AnalyzerError> {
        if let sv::ir::LoopKind::For = kind
            && let Some(canonical) = canonical_for_loop(init, condition, step)
            && self
                .m
                .id(&canonical.var)
                .is_some_and(|id| self.m.is_hidden(id))
            && let Some(values) = self.unrolled_values(&canonical, condition, step, body)?
        {
            return self.unroll(&canonical, &values, body);
        }
        self.runtime_loop(kind, init, condition, step, body)
    }

    /// The loop-variable values of a counted loop with constant bounds, when
    /// it is small enough to unroll.
    fn unrolled_values(
        &mut self,
        canonical: &CanonicalLoop,
        condition: Option<&sv::ir::Expr>,
        step: &[sv::ir::Stmt],
        body: &[sv::ir::Stmt],
    ) -> Result<Option<Vec<(String, i128)>>, sv::AnalyzerError> {
        let mut written = HashSet::default();
        written_names(body, &mut written);
        if written.contains(&canonical.var) {
            return Ok(None);
        }
        let id = self.m.id(&canonical.var).expect("loop variable");
        let (width, signed) = (self.m.var(id).width, self.m.var(id).signed);
        let Some((start, _)) = self.eval_const(&canonical.start)? else {
            return Ok(None);
        };
        let Some(condition) = condition else {
            return Ok(None);
        };
        let [sv::ir::Stmt::Assign { rhs: step_rhs, .. }] = step else {
            return Ok(None);
        };
        let mask = (BigUint::from(1u8) << width) - BigUint::from(1u8);
        let mut value = start & &mask;
        let mut values = Vec::new();
        let saved = self.overlay.remove(&canonical.var);
        let result = loop {
            let numeric = if signed && value.bit(width as u64 - 1) {
                (num_bigint::BigInt::from(value.clone()) - (num_bigint::BigInt::from(1u8) << width))
                    .to_i128()
            } else {
                value.to_i128()
            };
            let Some(numeric) = numeric else { break None };
            let literal = (typed_literal(&value, width, signed), numeric);
            self.overlay.insert(canonical.var.clone(), literal.clone());
            let Some((proceed, _)) = self.eval_const(condition)? else {
                break None;
            };
            if proceed.is_zero() {
                break Some(values);
            }
            values.push(literal);
            if values.len() + self.unrolled > MAX_UNROLLED_ITERATIONS {
                break None;
            }
            let Some((next, _)) = self.eval_const(step_rhs)? else {
                break None;
            };
            value = next & &mask;
        };
        self.overlay.remove(&canonical.var);
        if let Some(saved) = saved {
            self.overlay.insert(canonical.var.clone(), saved);
        }
        Ok(result)
    }

    fn unroll(
        &mut self,
        canonical: &CanonicalLoop,
        values: &[(String, i128)],
        body: &[sv::ir::Stmt],
    ) -> Result<bool, sv::AnalyzerError> {
        self.unrolled += values.len();
        let exit = self.b.new_block();
        let saved = self.overlay.remove(&canonical.var);
        let mut reachable = true;
        for literal in values {
            let next = self.b.new_block();
            self.overlay.insert(canonical.var.clone(), literal.clone());
            self.loops.push(LoopBlocks {
                break_block: exit,
                continue_block: next,
            });
            let falls = self.exec_block(body);
            self.loops.pop();
            if falls? {
                self.jump(next);
            }
            self.b.switch_to_block(next);
            reachable = true;
        }
        self.overlay.remove(&canonical.var);
        if let Some(saved) = saved {
            self.overlay.insert(canonical.var.clone(), saved);
        }
        let _ = reachable;
        self.jump(exit);
        self.b.switch_to_block(exit);
        // The loop variable keeps no value after the loop: it is scoped to it.
        Ok(true)
    }

    fn runtime_loop(
        &mut self,
        kind: &sv::ir::LoopKind<sv::ir::Expr>,
        init: &[sv::ir::Stmt],
        condition: Option<&sv::ir::Expr>,
        step: &[sv::ir::Stmt],
        body: &[sv::ir::Stmt],
    ) -> Result<bool, sv::AnalyzerError> {
        if !self.exec_block(init)? {
            return Ok(false);
        }
        // A `repeat` count is evaluated once, into a hidden counter.
        let counter = match kind {
            sv::ir::LoopKind::Repeat(count) => {
                let signed = self.expr_signed(count);
                let (id, name) = self.m.temp("repeat", 64, false, false);
                let value = self.eval(count, Some((64, signed)))?;
                self.store(id, SIROffset::Static(0), 64, value);
                Some(name)
            }
            _ => None,
        };
        let header = self.b.new_block();
        let body_block = self.b.new_block();
        let latch = self.b.new_block();
        let exit = self.b.new_block();
        if matches!(kind, sv::ir::LoopKind::DoWhile) {
            self.jump(body_block);
        } else {
            self.jump(header);
        }
        // Header: the loop condition.
        self.b.switch_to_block(header);
        let test = match (kind, &counter) {
            (_, Some(counter)) => Some(sv::ir::Expr::Binary {
                left: Box::new(sv::ir::Expr::Ident(counter.clone())),
                op: sv::ir::BinaryOp::Ne,
                right: Box::new(sv::ir::Expr::Literal("64'd0".to_string())),
            }),
            (sv::ir::LoopKind::Forever, _) => None,
            _ => condition.cloned(),
        };
        match test {
            Some(test) => {
                let (truth, _) = self.eval_truth(&test)?;
                self.b.seal_block(SIRTerminator::Branch {
                    cond: truth,
                    true_block: (body_block, Vec::new()),
                    false_block: (exit, Vec::new()),
                });
            }
            None => self.jump(body_block),
        }
        // Body.
        self.b.switch_to_block(body_block);
        if let Some(counter) = &counter {
            let decrement = sv::ir::Expr::Binary {
                left: Box::new(sv::ir::Expr::Ident(counter.clone())),
                op: sv::ir::BinaryOp::Sub,
                right: Box::new(sv::ir::Expr::Literal("64'd1".to_string())),
            };
            self.assign(&sv::ir::LValue::Ident(counter.clone()), &decrement)?;
        }
        self.loops.push(LoopBlocks {
            break_block: exit,
            continue_block: latch,
        });
        let falls = self.exec_block(body);
        self.loops.pop();
        if falls? {
            self.jump(latch);
        }
        // Latch: the step, then the condition again. A counted loop whose
        // step leaves its variable unchanged would never end; it stops the
        // simulation with an error instead.
        self.b.switch_to_block(latch);
        let progress = match kind {
            sv::ir::LoopKind::For => canonical_for_loop(init, condition, step)
                .and_then(|canonical| Some((canonical.var.clone(), self.m.id(&canonical.var)?))),
            _ => None,
        };
        let previous = match &progress {
            Some((var, id)) => {
                let (width, signed) = (self.m.var(*id).width, self.m.var(*id).signed);
                let (temp, temp_name) = self.m.temp("progress", width, signed, false);
                let value = self.eval(&sv::ir::Expr::Ident(var.clone()), Some((width, signed)))?;
                self.store(temp, SIROffset::Static(0), width, value);
                Some(temp_name)
            }
            None => None,
        };
        if self.exec_block(step)? {
            if let (Some((var, id)), Some(previous)) = (&progress, previous) {
                let stalled = sv::ir::Expr::Binary {
                    left: Box::new(sv::ir::Expr::Ident(var.clone())),
                    op: sv::ir::BinaryOp::EqCase,
                    right: Box::new(sv::ir::Expr::Ident(previous)),
                };
                let (stalled, _) = self.eval_truth(&stalled)?;
                let error_block = self.b.new_block();
                self.b.seal_block(SIRTerminator::Branch {
                    cond: stalled,
                    true_block: (error_block, Vec::new()),
                    false_block: (header, Vec::new()),
                });
                self.b.switch_to_block(error_block);
                let source = self.m.var(*id).path[0].clone();
                let code = self.m.runtime_error(format!(
                    "Non-progressing for loop in always_ff (loop variable `{source}`)"
                ));
                if let Some(info) = self.m.runtime_errors.get_mut(&code) {
                    info.signals.push(*id);
                }
                self.b.seal_block(SIRTerminator::Error(code));
            } else {
                self.jump(header);
            }
        }
        self.b.switch_to_block(exit);
        Ok(true)
    }

    /// Inline a call. Returns the name of the variable holding the result.
    fn call(
        &mut self,
        name: &str,
        args: &[Option<sv::ir::Expr>],
    ) -> Result<Option<String>, sv::AnalyzerError> {
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
        // Evaluate every input before any formal is written: an argument may
        // read a formal of an enclosing call of another subroutine.
        let mut outputs = Vec::new();
        let mut inputs = Vec::new();
        for (position, param) in subroutine.params.iter().enumerate() {
            let id = self
                .m
                .id(&param.name)
                .ok_or_else(|| unsupported(format!("argument `{}`", param.source_name)))?;
            let actual = args
                .get(position)
                .cloned()
                .flatten()
                .or_else(|| param.default.clone());
            if param.direction.is_written() {
                let lvalues = actual.as_ref().and_then(lvalue_from_expr).ok_or_else(|| {
                    unsupported(format!(
                        "output argument `{}` of `{name}`",
                        param.source_name
                    ))
                })?;
                outputs.push((param.name.clone(), id, lvalues));
            }
            if param.direction.is_read() {
                let actual = actual.ok_or_else(|| {
                    unsupported(format!(
                        "missing argument `{}` of `{name}`",
                        param.source_name
                    ))
                })?;
                let width = self.m.var(id).width;
                let signed = self.expr_signed(&actual);
                let mut arena = SLTNodeArena::new();
                let node = self.expr_slt(&actual, Some((width, signed)), &mut arena)?;
                let mut node =
                    coerce_node_width(&mut arena, node, Some(width), signed).map_err(slt_error)?;
                if !self.m.var(id).is_4state {
                    node = arena
                        .alloc(SLTNode::Unary(UnaryOp::ToTwoState, node))
                        .map_err(slt_error)?;
                }
                inputs.push((id, self.lower_slt(&arena, node)?));
            } else {
                let value = self.default_reg(id);
                inputs.push((id, value));
            }
        }
        for (id, value) in inputs {
            let width = self.m.var(id).width;
            let offset = sv_memory_offset(self.m.var(id), 0, width);
            self.store(id, offset, width, value);
        }
        if let Some(return_var) = &subroutine.return_var {
            let id = self
                .m
                .id(return_var)
                .ok_or_else(|| unsupported(format!("result of `{name}`")))?;
            self.init_default(id);
        }
        let return_block = self.b.new_block();
        self.functions.push(FunctionBlocks {
            return_block,
            return_var: subroutine.return_var.clone(),
            loop_depth: self.loops.len(),
        });
        self.active_calls.push(name.to_string());
        let falls = self.exec_block(&subroutine.body);
        self.active_calls.pop();
        self.functions.pop();
        if falls? {
            self.jump(return_block);
        }
        self.b.switch_to_block(return_block);
        for (formal, id, lvalues) in outputs {
            let signed = self.m.var(id).signed;
            if let [lvalue] = lvalues.as_slice() {
                self.assign(lvalue, &sv::ir::Expr::Ident(formal))?;
            } else {
                let _ = signed;
                let parts = lvalues;
                self.assign_concat(&parts, &sv::ir::Expr::Ident(formal))?;
            }
        }
        Ok(subroutine.return_var.clone())
    }

    // -------------------------------------------------------------- process

    /// Lower one `always_ff` body into its evaluation and commit units.
    pub fn lower_process(
        mut self,
        body: &[sv::ir::Stmt],
    ) -> Result<
        (
            ExecutionUnit<Addr>,
            ExecutionUnit<Addr>,
            Vec<VarAtomBase<SourceVarId>>,
        ),
        sv::AnalyzerError,
    > {
        let mut visited = HashSet::default();
        self.scan(body, &mut visited);
        // Merge the written ranges of each variable.
        let mut ranges: HashMap<SourceVarId, Vec<BitAccess>> = HashMap::default();
        for (id, access) in std::mem::take(&mut self.targets) {
            ranges.entry(id).or_default().push(access);
        }
        let mut targets = Vec::new();
        let mut ids: Vec<_> = ranges.keys().copied().collect();
        ids.sort_by_key(|id| id.0);
        for id in ids {
            let mut accesses = ranges.remove(&id).unwrap_or_default();
            accesses.sort_by_key(|access| access.lsb);
            let mut merged: Vec<BitAccess> = Vec::new();
            for access in accesses {
                match merged.last_mut() {
                    Some(last) if access.lsb <= last.msb + 1 => last.msb = last.msb.max(access.msb),
                    _ => merged.push(access),
                }
            }
            targets.extend(
                merged
                    .into_iter()
                    .map(|access| VarAtomBase::new(id, access.lsb, access.msb)),
            );
        }
        emit_ff_seeds(&mut self.b, &targets);
        if self.exec_block(body)? {
            self.b.seal_block(SIRTerminator::Return);
        }
        let eval = seal_drained(self.b);
        let mut apply = SIRBuilder::new();
        emit_ff_commits(&mut apply, &targets);
        Ok((eval, seal_builder(apply), targets))
    }
}

fn seal_drained(mut builder: SIRBuilder<Addr>) -> ExecutionUnit<Addr> {
    let (blocks, register_map, _) = builder.drain();
    let mut unit = ExecutionUnit {
        entry_block_id: BlockId(0),
        blocks,
        register_map,
    };
    // Code after a jump (`break`, `return`, `$fatal`) leaves blocks that
    // control never enters.
    prune_unreachable_blocks(&mut unit);
    unit
}

/// Drop the blocks control never enters.
pub(super) fn prune_unreachable_blocks<A>(unit: &mut ExecutionUnit<A>) {
    let mut reachable = HashSet::default();
    let mut work = vec![unit.entry_block_id];
    while let Some(block) = work.pop() {
        if !reachable.insert(block) {
            continue;
        }
        match &unit.blocks[&block].terminator {
            SIRTerminator::Jump(target, _) => work.push(*target),
            SIRTerminator::Branch {
                true_block,
                false_block,
                ..
            } => {
                work.push(true_block.0);
                work.push(false_block.0);
            }
            SIRTerminator::Switch { cases, default, .. } => {
                work.extend(cases.iter().map(|case| case.target));
                work.push(*default);
            }
            SIRTerminator::Return | SIRTerminator::Error(_) => {}
        }
    }
    unit.blocks.retain(|id, _| reachable.contains(id));
}

fn collect_calls(expr: &sv::ir::Expr, calls: &mut Vec<(String, Vec<Option<sv::ir::Expr>>)>) {
    match expr {
        sv::ir::Expr::Call { name, args } => {
            calls.push((name.clone(), args.iter().cloned().map(Some).collect()));
            for arg in args {
                collect_calls(arg, calls);
            }
        }
        sv::ir::Expr::Ident(_) | sv::ir::Expr::Literal(_) => {}
        sv::ir::Expr::Select { expr, .. }
        | sv::ir::Expr::Resize { expr, .. }
        | sv::ir::Expr::Unary { expr, .. } => collect_calls(expr, calls),
        sv::ir::Expr::Concat(parts) | sv::ir::Expr::RepeatConcat { parts, .. } => {
            parts.iter().for_each(|part| collect_calls(part, calls))
        }
        sv::ir::Expr::Binary { left, right, .. } => {
            collect_calls(left, calls);
            collect_calls(right, calls);
        }
        sv::ir::Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            collect_calls(condition, calls);
            collect_calls(then_expr, calls);
            collect_calls(else_expr, calls);
        }
        sv::ir::Expr::Inside { expr, items } => {
            collect_calls(expr, calls);
            for item in items {
                item.exprs()
                    .into_iter()
                    .for_each(|operand| collect_calls(operand, calls));
            }
        }
    }
}

fn substitute_overlay_const(
    expr: &sv::ir::ConstExpr,
    overlay: &HashMap<String, (String, i128)>,
) -> sv::ir::ConstExpr {
    use sv::ir::ConstExpr;
    let go = |expr: &ConstExpr| substitute_overlay_const(expr, overlay);
    match expr {
        ConstExpr::Ident(name) => match overlay.get(name) {
            Some((_, value)) => ConstExpr::Literal(value.to_string()),
            None => expr.clone(),
        },
        ConstExpr::Literal(_) => expr.clone(),
        ConstExpr::Select { expr, bit } => ConstExpr::Select {
            expr: Box::new(go(expr)),
            bit: Box::new(go(bit)),
        },
        ConstExpr::Function { name, args } => ConstExpr::Function {
            name: name.clone(),
            args: args.iter().map(go).collect(),
        },
        ConstExpr::Unary { op, expr } => ConstExpr::Unary {
            op: *op,
            expr: Box::new(go(expr)),
        },
        ConstExpr::Binary { left, op, right } => ConstExpr::Binary {
            left: Box::new(go(left)),
            op: *op,
            right: Box::new(go(right)),
        },
        ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => ConstExpr::Mux {
            condition: Box::new(go(condition)),
            then_expr: Box::new(go(then_expr)),
            else_expr: Box::new(go(else_expr)),
        },
    }
}

fn substitute_overlay(
    expr: &sv::ir::Expr,
    overlay: &HashMap<String, (String, i128)>,
) -> sv::ir::Expr {
    use sv::ir::Expr;
    let go = |expr: &Expr| substitute_overlay(expr, overlay);
    match expr {
        Expr::Ident(name) => match overlay.get(name) {
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
            msb: substitute_overlay_const(msb, overlay),
            lsb: substitute_overlay_const(lsb, overlay),
            signed: *signed,
        },
        Expr::Concat(parts) => Expr::Concat(parts.iter().map(go).collect()),
        Expr::RepeatConcat { count, parts } => Expr::RepeatConcat {
            count: substitute_overlay_const(count, overlay),
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
