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
use celox_frontend_core::process::ProcessKernelBuilder;
use celox_sir::{RegisterId, RegisterType};
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
    /// One persistent RNG of `$urandom` and `$urandom_range` shared by every
    /// call from this process, including calls in inlined subroutines. Bit
    /// 32 distinguishes an unseeded state.
    random: Option<(SourceVarId, String)>,
    random_seed: u32,
    /// The seed of this process's `$random` calls without a seed argument.
    dist_seed: Option<(SourceVarId, String)>,
    /// The kernel of a process being lowered. Its builder is `b` while the
    /// body is lowered, and the process reads and writes stable state
    /// directly.
    kernel: Option<ProcessKernelBuilder>,
    direct: bool,
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
    pub fn new(m: &'p mut ProcModule<'a>, random_seed: u32) -> Self {
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
            random: None,
            random_seed,
            dist_seed: None,
            kernel: None,
            direct: false,
        }
    }

    /// Where a write of `id`, and a read of a value the process wrote, go:
    /// the working region, or stable state in a process kernel.
    fn target(&self, id: SourceVarId) -> Addr {
        if self.kernel.is_some() || self.direct {
            stable(id)
        } else {
            working(id)
        }
    }

    /// Run `f` on the kernel with its builder in place.
    fn with_kernel<T>(&mut self, f: impl FnOnce(&mut ProcessKernelBuilder) -> T) -> T {
        let mut kernel = self.kernel.take().expect("lowering a process kernel");
        std::mem::swap(&mut self.b, kernel.builder());
        let result = f(&mut kernel);
        std::mem::swap(&mut self.b, kernel.builder());
        self.kernel = Some(kernel);
        result
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
        // A run-time bit select within one array element writes only that
        // element (see `write`); a run-time element may be any of them.
        let element_write = dynamic_array_element_lvalue(
            lvalue,
            self.m.variables,
            self.m.name_to_id,
            self.m.constants,
            self.m.parameter_types,
        )
        .is_some();
        let whole = (!element_write)
            .then(|| {
                dynamic_packed_write(
                    lvalue,
                    self.m.variables,
                    self.m.name_to_id,
                    self.m.constants,
                    self.m.parameter_types,
                )
            })
            .flatten()
            .and_then(|write| write.window)
            .unwrap_or(BitAccess::new(0, variable.width - 1));
        let access = match lvalue {
            sv::ir::LValue::Ident(_) => BitAccess::new(0, variable.width - 1),
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
                sv::ir::Stmt::SystemTask { task, args } => {
                    if let Some(destination) = readmem_destination(*task, args) {
                        writes.push((sv::ir::LValue::Ident(destination.to_string()), true));
                    }
                }
                _ => {}
            });
        }
        for (lvalue, blocking) in writes {
            self.record_target(&lvalue, blocking);
        }
        // Calls: output arguments and the subroutine bodies, including the
        // calls in the select positions of assignment targets.
        let mut calls = Vec::new();
        for stmt in stmts {
            stmt.walk(&mut |stmt| stmt_calls(stmt, &mut calls));
        }
        while let Some((callee, args)) = calls.pop() {
            let name = match callee {
                sv::ir::Callee::System(function) => {
                    use sv::ir::SystemFunction;
                    match function {
                        SystemFunction::Urandom | SystemFunction::UrandomRange => {
                            self.urandom_state();
                        }
                        SystemFunction::Random if args.is_empty() => {
                            self.dist_seed();
                        }
                        _ => {}
                    }
                    for lvalue in written_system_arguments(function, &args) {
                        self.record_target(&lvalue, true);
                    }
                    continue;
                }
                sv::ir::Callee::Subroutine(name) => name,
            };
            let Some(subroutine) = self.m.subroutine(&name).cloned() else {
                continue;
            };
            default_calls(&subroutine, &args, &mut calls);
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
        let direct = self.kernel.is_some() || self.direct;
        let map = |id: &SourceVarId| -> Addr {
            if direct {
                stable(*aliases.get(id).unwrap_or(id))
            } else if let Some(var) = aliases.get(id) {
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
        let expr = if self.m.calls(&expr) {
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
        let constant = slt_bool(&arena, &mut ConstCache::default(), truth);
        Ok((self.lower_slt(&arena, truth)?, constant))
    }

    /// Whether an expression is not logically false, as a one-bit register.
    fn eval_not_false(&mut self, expr: &sv::ir::Expr) -> Result<RegisterId, sv::AnalyzerError> {
        let mut arena = SLTNodeArena::new();
        let node = self.expr_slt(expr, None, &mut arena)?;
        let not_false = slt_not_false(&mut arena, node)?;
        self.lower_slt(&arena, not_false)
    }

    /// The constant value of an expression, if it has one.
    fn eval_const(
        &mut self,
        expr: &sv::ir::Expr,
    ) -> Result<Option<(BigUint, usize)>, sv::AnalyzerError> {
        if self.m.calls(expr) {
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
        Ok(slt_const(&arena, &mut ConstCache::default(), node))
    }

    fn expr_signed(&self, expr: &sv::ir::Expr) -> bool {
        self.m.expr_signed(expr)
    }

    /// Run `f` where the right operand of `&&` or `||` is evaluated: it is
    /// skipped only when the left one is known false (`&&`) or known true
    /// (`||`) (IEEE 1800-2023 11.4.7), so an unknown left operand evaluates
    /// it.
    fn short_circuit<T>(
        &mut self,
        op: sv::ir::BinaryOp,
        left: &sv::ir::Expr,
        f: impl FnOnce(&mut Self) -> Result<T, sv::AnalyzerError>,
    ) -> Result<T, sv::AnalyzerError> {
        let taken = self.b.new_block();
        let join = self.b.new_block();
        let (cond, true_block, false_block) = if op == sv::ir::BinaryOp::LogicAnd {
            (self.eval_not_false(left)?, taken, join)
        } else {
            (self.eval_truth(left)?.0, join, taken)
        };
        self.b.seal_block(SIRTerminator::Branch {
            cond,
            true_block: (true_block, Vec::new()),
            false_block: (false_block, Vec::new()),
        });
        self.b.switch_to_block(taken);
        let result = f(self)?;
        self.b.seal_block(SIRTerminator::Jump(join, Vec::new()));
        self.b.switch_to_block(join);
        Ok(result)
    }

    /// Run `f` in a block entered only where `cond` holds.
    fn when<T>(
        &mut self,
        cond: RegisterId,
        f: impl FnOnce(&mut Self) -> Result<T, sv::AnalyzerError>,
    ) -> Result<T, sv::AnalyzerError> {
        let taken = self.b.new_block();
        let join = self.b.new_block();
        self.b.seal_block(SIRTerminator::Branch {
            cond,
            true_block: (taken, Vec::new()),
            false_block: (join, Vec::new()),
        });
        self.b.switch_to_block(taken);
        let result = f(self)?;
        self.b.seal_block(SIRTerminator::Jump(join, Vec::new()));
        self.b.switch_to_block(join);
        Ok(result)
    }

    /// Run `then_f` and `else_f` where the arms of a conditional operator
    /// are evaluated; an ambiguous condition evaluates both (IEEE 1800-2023
    /// 11.4.11).
    fn mux_arms<A, B>(
        &mut self,
        condition: &sv::ir::Expr,
        then_f: impl FnOnce(&mut Self) -> Result<A, sv::AnalyzerError>,
        else_f: impl FnOnce(&mut Self) -> Result<B, sv::AnalyzerError>,
    ) -> Result<(A, B), sv::AnalyzerError> {
        if self.m.four_state {
            let mut arena = SLTNodeArena::new();
            let mut consts = ConstCache::default();
            let node = self.expr_slt(condition, None, &mut arena)?;
            let truth = slt_truth(&mut arena, node)?;
            let unknown = slt_truth_unknown(&mut arena, &mut consts, node)?;
            let then_cond = slt_or(&mut arena, &mut consts, truth, unknown)?;
            let not_truth = slt_not(&mut arena, &mut consts, truth)?;
            let else_cond = slt_or(&mut arena, &mut consts, not_truth, unknown)?;
            let then_cond = self.lower_slt(&arena, then_cond)?;
            let else_cond = self.lower_slt(&arena, else_cond)?;
            let then_result = self.when(then_cond, then_f)?;
            let else_result = self.when(else_cond, else_f)?;
            return Ok((then_result, else_result));
        }
        let (truth, _) = self.eval_truth(condition)?;
        let then_block = self.b.new_block();
        let else_block = self.b.new_block();
        let join = self.b.new_block();
        self.b.seal_block(SIRTerminator::Branch {
            cond: truth,
            true_block: (then_block, Vec::new()),
            false_block: (else_block, Vec::new()),
        });
        self.b.switch_to_block(then_block);
        let then_result = then_f(self)?;
        self.b.seal_block(SIRTerminator::Jump(join, Vec::new()));
        self.b.switch_to_block(else_block);
        let else_result = else_f(self)?;
        self.b.seal_block(SIRTerminator::Jump(join, Vec::new()));
        self.b.switch_to_block(join);
        Ok((then_result, else_result))
    }

    /// Evaluate the subroutine calls of a select position, left to right,
    /// and refer to their results. A flattened select repeats an index in
    /// its bounds and range checks; the copies of one call share its site,
    /// so the call is evaluated once.
    fn hoist_const(
        &mut self,
        expr: &sv::ir::ConstExpr,
        calls: &mut Vec<(sv::ir::ConstExpr, sv::ir::ConstExpr)>,
    ) -> Result<sv::ir::ConstExpr, sv::AnalyzerError> {
        use sv::ir::ConstExpr;
        if !self.m.const_calls(expr) {
            return Ok(expr.clone());
        }
        let operand = |expr: &ConstExpr| {
            expr_from_const_expr(expr).ok_or_else(|| unsupported("operand in a select"))
        };
        Ok(match expr {
            ConstExpr::Function { callee, args, .. } if self.m.is_callee(callee) => {
                if let Some((_, result)) = calls.iter().find(|(call, _)| call == expr) {
                    return Ok(result.clone());
                }
                let mut lowered = Vec::with_capacity(args.len());
                for arg in args {
                    let arg = self.hoist_const(arg, calls)?;
                    lowered.push(expr_from_const_expr(&arg).ok_or_else(|| {
                        unsupported(format!("argument of `{callee}` in a select"))
                    })?);
                }
                let call = sv::ir::Expr::Call {
                    arg_order: Vec::new(),
                    callee: callee.clone(),
                    args: lowered.into_iter().map(Some).collect(),
                };
                let sv::ir::Expr::Ident(result) = self.hoist(&call)? else {
                    return Err(unsupported(format!("call of `{callee}` in a select")));
                };
                let result = ConstExpr::Ident(result);
                calls.push((expr.clone(), result.clone()));
                result
            }
            ConstExpr::Function { callee, args, site } => ConstExpr::Function {
                callee: callee.clone(),
                args: args
                    .iter()
                    .map(|arg| self.hoist_const(arg, calls))
                    .collect::<Result<_, _>>()?,
                site: *site,
            },
            ConstExpr::SelectRange {
                expr,
                msb,
                lsb,
                signed,
            } => ConstExpr::SelectRange {
                expr: Box::new(self.hoist_const(expr, calls)?),
                msb: Box::new(self.hoist_const(msb, calls)?),
                lsb: Box::new(self.hoist_const(lsb, calls)?),
                signed: *signed,
            },
            ConstExpr::Select { expr, bit } => ConstExpr::Select {
                expr: Box::new(self.hoist_const(expr, calls)?),
                bit: Box::new(self.hoist_const(bit, calls)?),
            },
            ConstExpr::Resize {
                expr,
                width,
                signed,
            } => ConstExpr::Resize {
                expr: Box::new(self.hoist_const(expr, calls)?),
                width: *width,
                signed: *signed,
            },
            ConstExpr::Unary { op, expr } => ConstExpr::Unary {
                op: *op,
                expr: Box::new(self.hoist_const(expr, calls)?),
            },
            ConstExpr::Binary { left, op, right }
                if matches!(op, sv::ir::BinaryOp::LogicAnd | sv::ir::BinaryOp::LogicOr)
                    && self.m.const_calls(right) =>
            {
                let left = self.hoist_const(left, calls)?;
                let right = self
                    .short_circuit(*op, &operand(&left)?, |this| this.hoist_const(right, calls))?;
                ConstExpr::Binary {
                    left: Box::new(left),
                    op: *op,
                    right: Box::new(right),
                }
            }
            ConstExpr::Binary { left, op, right } => ConstExpr::Binary {
                left: Box::new(self.hoist_const(left, calls)?),
                op: *op,
                right: Box::new(self.hoist_const(right, calls)?),
            },
            ConstExpr::Mux {
                condition,
                then_expr,
                else_expr,
            } => {
                let condition = self.hoist_const(condition, calls)?;
                let (then_expr, else_expr) =
                    if self.m.const_calls(then_expr) || self.m.const_calls(else_expr) {
                        // A part of both arms is a copy the flattening of a
                        // select made, which runs whichever arm is taken.
                        for shared in self.m.shared_calls(then_expr, else_expr) {
                            self.hoist_const(shared, calls)?;
                        }
                        // `calls` is shared by both arms, which run one after
                        // the other.
                        let calls = std::cell::RefCell::new(calls);
                        self.mux_arms(
                            &operand(&condition)?,
                            |this| this.hoist_const(then_expr, &mut calls.borrow_mut()),
                            |this| this.hoist_const(else_expr, &mut calls.borrow_mut()),
                        )?
                    } else {
                        ((**then_expr).clone(), (**else_expr).clone())
                    };
                ConstExpr::Mux {
                    condition: Box::new(condition),
                    then_expr: Box::new(then_expr),
                    else_expr: Box::new(else_expr),
                }
            }
            ConstExpr::Literal(_) | ConstExpr::Ident(_) => expr.clone(),
        })
    }

    /// An assignment target whose select positions refer to the results of
    /// their subroutine calls, evaluated now.
    fn hoist_lvalue(
        &mut self,
        lvalue: &sv::ir::LValue,
    ) -> Result<sv::ir::LValue, sv::AnalyzerError> {
        if !self.m.lvalue_calls(lvalue) {
            return Ok(lvalue.clone());
        }
        let mut lvalue = lvalue.clone();
        if let sv::ir::LValue::Select { msb, lsb, .. } = &mut lvalue {
            let mut calls = Vec::new();
            *lsb = self.hoist_const(lsb, &mut calls)?;
            *msb = self.hoist_const(msb, &mut calls)?;
        }
        Ok(lvalue)
    }

    /// `lvalue` with the variables its select positions read replaced by
    /// copies of their current values, so a later write to one does not move
    /// the target.
    fn freeze_lvalue(
        &mut self,
        mut lvalue: sv::ir::LValue,
    ) -> Result<sv::ir::LValue, sv::AnalyzerError> {
        let sv::ir::LValue::Select { msb, lsb, .. } = &mut lvalue else {
            return Ok(lvalue);
        };
        let mut copies = HashMap::default();
        *lsb = self.freeze_const(lsb, &mut copies)?;
        *msb = self.freeze_const(msb, &mut copies)?;
        Ok(lvalue)
    }

    /// `expr` with each variable it reads, and each bit it reads from an
    /// unpacked array, replaced by a copy of its current value.
    fn freeze_const(
        &mut self,
        expr: &sv::ir::ConstExpr,
        copies: &mut HashMap<sv::ir::ConstExpr, sv::ir::ConstExpr>,
    ) -> Result<sv::ir::ConstExpr, sv::AnalyzerError> {
        use sv::ir::ConstExpr;
        if let Some(copy) = copies.get(expr) {
            return Ok(copy.clone());
        }
        let array = |this: &Self, name: &str| {
            this.m
                .id(name)
                .map(|id| !this.m.var(id).array_dims.is_empty())
        };
        let frozen = match expr {
            ConstExpr::Ident(name) if array(self, name) == Some(false) => {
                let id = self.m.id(name).expect("a variable");
                let variable = self.m.var(id);
                let (width, signed, is_4state) =
                    (variable.width, variable.signed, variable.is_4state);
                let (temp, temp_name) = self.m.temp("position", width, signed, is_4state);
                let value = self.eval(&sv::ir::Expr::Ident(name.clone()), Some((width, signed)))?;
                self.store(temp, SIROffset::Static(0), width, value);
                ConstExpr::Ident(temp_name)
            }
            ConstExpr::SelectRange {
                expr: base,
                msb,
                lsb,
                signed,
            } if matches!(&**base, ConstExpr::Ident(name) if self.m.id(name).is_some()) => {
                let ConstExpr::Ident(name) = &**base else {
                    unreachable!()
                };
                let msb = self.freeze_const(msb, copies)?;
                let lsb = self.freeze_const(lsb, copies)?;
                let is_4state = self.m.var(self.m.id(name).expect("a variable")).is_4state;
                let read = sv::ir::Expr::Select {
                    expr: Box::new(sv::ir::Expr::Ident(name.clone())),
                    msb,
                    lsb,
                    signed: *signed,
                };
                let value = self.eval(&read, None)?;
                let width = self.b.register(&value).width();
                let (temp, temp_name) = self.m.temp("position", width, *signed, is_4state);
                self.store(temp, SIROffset::Static(0), width, value);
                ConstExpr::Ident(temp_name)
            }
            ConstExpr::Select { expr: base, bit } if matches!(&**base, ConstExpr::Ident(name) if array(self, name) == Some(true)) =>
            {
                let ConstExpr::Ident(name) = &**base else {
                    unreachable!()
                };
                let bit = self.freeze_const(bit, copies)?;
                let is_4state = self.m.var(self.m.id(name).expect("a variable")).is_4state;
                let read = sv::ir::Expr::Select {
                    expr: Box::new(sv::ir::Expr::Ident(name.clone())),
                    msb: bit.clone(),
                    lsb: bit,
                    signed: false,
                };
                let value = self.eval(&read, Some((1, false)))?;
                let (temp, temp_name) = self.m.temp("position", 1, false, is_4state);
                self.store(temp, SIROffset::Static(0), 1, value);
                ConstExpr::Ident(temp_name)
            }
            ConstExpr::Ident(_) | ConstExpr::Literal(_) => return Ok(expr.clone()),
            ConstExpr::SelectRange {
                expr,
                msb,
                lsb,
                signed,
            } => ConstExpr::SelectRange {
                expr: Box::new(self.freeze_const(expr, copies)?),
                msb: Box::new(self.freeze_const(msb, copies)?),
                lsb: Box::new(self.freeze_const(lsb, copies)?),
                signed: *signed,
            },
            ConstExpr::Select { expr, bit } => ConstExpr::Select {
                expr: Box::new(self.freeze_const(expr, copies)?),
                bit: Box::new(self.freeze_const(bit, copies)?),
            },
            ConstExpr::Function { callee, args, site } => ConstExpr::Function {
                callee: callee.clone(),
                args: args
                    .iter()
                    .map(|arg| self.freeze_const(arg, copies))
                    .collect::<Result<_, _>>()?,
                site: *site,
            },
            ConstExpr::Resize {
                expr,
                width,
                signed,
            } => ConstExpr::Resize {
                expr: Box::new(self.freeze_const(expr, copies)?),
                width: *width,
                signed: *signed,
            },
            ConstExpr::Unary { op, expr } => ConstExpr::Unary {
                op: *op,
                expr: Box::new(self.freeze_const(expr, copies)?),
            },
            ConstExpr::Binary { left, op, right } => ConstExpr::Binary {
                left: Box::new(self.freeze_const(left, copies)?),
                op: *op,
                right: Box::new(self.freeze_const(right, copies)?),
            },
            ConstExpr::Mux {
                condition,
                then_expr,
                else_expr,
            } => ConstExpr::Mux {
                condition: Box::new(self.freeze_const(condition, copies)?),
                then_expr: Box::new(self.freeze_const(then_expr, copies)?),
                else_expr: Box::new(self.freeze_const(else_expr, copies)?),
            },
        };
        copies.insert(expr.clone(), frozen.clone());
        Ok(frozen)
    }

    /// Execute the user subroutine calls of an expression and replace each by
    /// a hidden variable holding its result.
    fn hoist(&mut self, expr: &sv::ir::Expr) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        use sv::ir::Expr;
        if !self.m.calls(expr) {
            return Ok(expr.clone());
        }
        Ok(match expr {
            Expr::Call {
                callee: sv::ir::Callee::System(sv::ir::SystemFunction::Urandom),
                args,
                ..
            } => {
                let seed = match args.first().and_then(Option::as_ref) {
                    Some(seed) => Some(self.hoist(seed)?),
                    None => None,
                };
                self.urandom(seed)?
            }
            Expr::Call {
                callee: sv::ir::Callee::System(sv::ir::SystemFunction::UrandomRange),
                args,
                ..
            } => self.urandom_range(args)?,
            Expr::Call {
                callee: sv::ir::Callee::System(sv::ir::SystemFunction::Random),
                args,
                ..
            } => self.random(args.first().and_then(Option::as_ref))?,
            Expr::Call {
                callee: sv::ir::Callee::Subroutine(name),
                args,
                arg_order,
            } if self.m.dpi_imports.contains_key(name) => {
                let import = self.m.dpi_imports[name].clone();
                let Some(r#type) = import.return_type() else {
                    return Err(unsupported(format!(
                        "void DPI-C function `{name}` used as a value"
                    )));
                };
                let args = args.clone();
                let result = self
                    .dpi_call(&import, &args, arg_order)?
                    .expect("a non-void DPI-C import defines a result");
                let (temp, temp_name) = self.m.temp(
                    "dpi",
                    r#type.width(),
                    r#type.is_signed(),
                    r#type.is_4state(),
                );
                self.store(temp, SIROffset::Static(0), r#type.width(), result);
                Expr::Ident(temp_name)
            }
            Expr::Call {
                callee: sv::ir::Callee::Subroutine(name),
                args,
                arg_order,
            } if self.m.subroutines.contains_key(name) => {
                let args = args.clone();
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
                let result = self.call(name, &args, arg_order)?;
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
                    && self.m.calls(right) =>
            {
                let left = self.hoist(left)?;
                let right = self.short_circuit(*op, &left, |this| this.hoist(right))?;
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
                let condition = self.hoist(condition)?;
                let (then_expr, else_expr) = self.mux_arms(
                    &condition,
                    |this| this.hoist(then_expr),
                    |this| this.hoist(else_expr),
                )?;
                Expr::Mux {
                    condition: Box::new(condition),
                    then_expr: Box::new(then_expr),
                    else_expr: Box::new(else_expr),
                }
            }
            Expr::Call {
                callee,
                args,
                arg_order,
            } => Expr::Call {
                arg_order: arg_order.clone(),
                callee: callee.clone(),
                args: args
                    .iter()
                    .map(|arg| arg.as_ref().map(|arg| self.hoist(arg)).transpose())
                    .collect::<Result<_, _>>()?,
            },
            Expr::Select {
                expr,
                msb,
                lsb,
                signed,
            } => {
                let expr = self.hoist(expr)?;
                let mut calls = Vec::new();
                let lsb = self.hoist_const(lsb, &mut calls)?;
                let msb = self.hoist_const(msb, &mut calls)?;
                Expr::Select {
                    expr: Box::new(expr),
                    msb,
                    lsb,
                    signed: *signed,
                }
            }
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

    /// Call a DPI-C import, returning the register holding its result.
    ///
    /// Each argument is converted to its C type: a two-state integer, whose
    /// unknown bits read as zero, or a `logic`, which keeps its unknown bit
    /// in four-state simulation (IEEE 1800-2023 35.5.6).
    fn dpi_call(
        &mut self,
        import: &sv::ir::DpiImport,
        args: &[Option<sv::ir::Expr>],
        arg_order: &[usize],
    ) -> Result<Option<RegisterId>, sv::AnalyzerError> {
        let name = import.name();
        if args.len() != import.arguments().len() {
            return Err(unsupported(format!(
                "call of DPI-C function `{name}` with {} arguments; it takes {}",
                args.len(),
                import.arguments().len()
            )));
        }
        let mut registers = vec![None; args.len()];
        let positions: Vec<_> = if arg_order.is_empty() {
            (0..args.len()).collect()
        } else {
            arg_order.to_vec()
        };
        // Evaluate actuals in source order, then pass registers in C formal order.
        for position in positions {
            let argument = import.arguments().get(position).ok_or_else(|| {
                unsupported(format!("argument position of DPI-C function `{name}`"))
            })?;
            let arg = &args[position];
            let Some(arg) = arg else {
                return Err(unsupported(format!(
                    "omitted argument of DPI-C function `{name}`"
                )));
            };
            let r#type = argument.r#type();
            let arg = self.hoist(arg)?;
            let value = self.eval(&arg, Some((r#type.width(), r#type.is_signed())))?;
            registers[position] = Some(self.dpi_register(value, r#type));
        }
        let registers = registers
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| unsupported(format!("missing argument of DPI-C function `{name}`")))?;
        let func = self.m.extern_function(import);
        let dst = import
            .return_type()
            .map(|r#type| self.dpi_result_register(r#type));
        self.b.emit(SIRInstruction::ExternCall {
            dst,
            func,
            args: registers,
        });
        self.m.extern_calls += 1;
        Ok(dst)
    }

    /// `value`, of the argument's width, in the register type passed as its
    /// C type.
    fn dpi_register(&mut self, value: RegisterId, r#type: sv::ir::DpiType) -> RegisterId {
        let register = self.dpi_result_register(r#type);
        let op = if matches!(self.b.register(&register), RegisterType::Logic { .. }) {
            UnaryOp::Ident
        } else {
            UnaryOp::ToTwoState
        };
        self.b.emit(SIRInstruction::Unary(register, op, value));
        register
    }

    /// A register of the type that holds a DPI-C value of type `r#type`.
    fn dpi_result_register(&mut self, r#type: sv::ir::DpiType) -> RegisterId {
        if r#type.is_4state() && self.m.four_state {
            self.b.alloc_logic(1)
        } else {
            self.b.alloc_bit(r#type.width(), r#type.is_signed())
        }
    }

    // ---------------------------------------------------------------- stores

    /// The state of this process's `$urandom` stream.
    fn urandom_state(&mut self) -> (SourceVarId, String) {
        if self.random.is_none() {
            let (id, name) = self.m.temp("urandom_state", 33, false, false);
            self.working.insert(id);
            self.targets.push((id, BitAccess::new(0, 32)));
            self.random = Some((id, name));
        }
        self.random.clone().expect("a process RNG")
    }

    /// The seed of this process's `$random` calls without a seed argument.
    /// It starts at 0, as the seed variable of `$random(seed)` typically
    /// does, so a design whose one process calls `$random` gets the same
    /// numbers as other simulators that use one seed for the whole design.
    fn dist_seed(&mut self) -> (SourceVarId, String) {
        if self.dist_seed.is_none() {
            let (id, name) = self.m.temp("random_seed", 32, false, false);
            self.working.insert(id);
            self.targets.push((id, BitAccess::new(0, 31)));
            self.dist_seed = Some((id, name));
        }
        self.dist_seed.clone().expect("a process seed")
    }

    /// Evaluate `expr` into a new hidden variable.
    fn hidden(
        &mut self,
        purpose: &str,
        expr: &sv::ir::Expr,
        signed: bool,
    ) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        let value = self.eval(expr, Some((32, signed)))?;
        let (id, name) = self.m.temp(purpose, 32, signed, false);
        self.store(id, SIROffset::Static(0), 32, value);
        Ok(sv::ir::Expr::Ident(name))
    }

    /// `expr` converted to a 32-bit `int` or `int unsigned` formal.
    fn int_formal(&self, expr: sv::ir::Expr) -> sv::ir::Expr {
        let signed = self.expr_signed(&expr);
        sv::ir::Expr::Resize {
            expr: Box::new(expr),
            width: 32,
            signed,
        }
    }

    /// IEEE 1800-2023 18.13.1/18.14: a deterministic, process-local stream.
    /// This full-period 32-bit LCG is implementation-defined; tests compare
    /// repeatability rather than assuming another simulator's sequence.
    fn urandom(&mut self, seed: Option<sv::ir::Expr>) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        use sv::ir::{BinaryOp, Expr};
        let (id, name) = self.urandom_state();
        let initial = match seed {
            Some(seed) => seed,
            None => Expr::Mux {
                condition: Box::new(Expr::Binary {
                    left: Box::new(Expr::Ident(name.clone())),
                    op: BinaryOp::Eq,
                    right: Box::new(Expr::Literal("33'd0".into())),
                }),
                then_expr: Box::new(Expr::Literal(format!("32'd{}", self.random_seed))),
                else_expr: Box::new(Expr::Ident(name.clone())),
            },
        };
        // The int seed formal preserves a narrow signed operand's sign bits
        // before its 32-bit representation becomes unsigned RNG state.
        let next = Expr::Resize {
            width: 32,
            signed: false,
            expr: Box::new(Expr::Binary {
                left: Box::new(Expr::Binary {
                    left: Box::new(self.int_formal(initial)),
                    op: BinaryOp::Mul,
                    right: Box::new(Expr::Literal("32'd1664525".into())),
                }),
                op: BinaryOp::Add,
                right: Box::new(Expr::Literal("32'd1013904223".into())),
            }),
        };
        let result = self.hidden("urandom_result", &next, false)?;
        let state = Expr::Concat(vec![Expr::Literal("1'b1".into()), result.clone()]);
        let state = self.eval(&state, Some((33, false)))?;
        self.store(id, SIROffset::Static(0), 33, state);
        Ok(result)
    }

    /// IEEE 1800-2023 18.13.2: a number of the `$urandom` stream between the
    /// two bounds, in either order, inclusive.
    fn urandom_range(
        &mut self,
        args: &[Option<sv::ir::Expr>],
    ) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        use sv::ir::{BinaryOp, Expr};
        let bound = |this: &mut Self, arg: Option<&Expr>| match arg {
            Some(arg) => {
                let arg = this.hoist(arg)?;
                let arg = this.int_formal(arg);
                this.hidden("urandom_range_bound", &arg, false)
            }
            None => Ok(Expr::Literal("32'd0".into())),
        };
        let max = bound(self, args.first().and_then(Option::as_ref))?;
        let min = bound(self, args.get(1).and_then(Option::as_ref))?;
        let value = self.urandom(None)?;
        let widen = |expr: Expr, width| Expr::Resize {
            expr: Box::new(expr),
            width,
            signed: false,
        };
        let binary = |left, op, right| Expr::Binary {
            left: Box::new(left),
            op,
            right: Box::new(right),
        };
        let reversed = binary(max.clone(), BinaryOp::Lt, min.clone());
        let pick = |when_reversed: &Expr, otherwise: &Expr| Expr::Mux {
            condition: Box::new(reversed.clone()),
            then_expr: Box::new(when_reversed.clone()),
            else_expr: Box::new(otherwise.clone()),
        };
        let (high, low) = (pick(&min, &max), pick(&max, &min));
        // The span is 2**32 for the full range, so it takes 33 bits. Scaling
        // takes the offset from the high bits of the number: the low bits of
        // the LCG have short periods.
        let span = binary(
            binary(widen(high, 33), BinaryOp::Sub, widen(low.clone(), 33)),
            BinaryOp::Add,
            Expr::Literal("33'd1".into()),
        );
        let offset = binary(
            binary(widen(value, 65), BinaryOp::Mul, widen(span, 65)),
            BinaryOp::Shr,
            Expr::Literal("65'd32".into()),
        );
        let result = Expr::Resize {
            expr: Box::new(binary(widen(low, 65), BinaryOp::Add, offset)),
            width: 32,
            signed: false,
        };
        self.hidden("urandom_range_result", &result, false)
    }

    /// IEEE 1800-2023 20.14.1 and Annex N: `$random` is
    /// `rtl_dist_uniform(seed, LONG_MIN, LONG_MAX)` with a 32-bit `long`.
    ///
    /// Annex N computes the number in floating point from bits 31:9 of the
    /// next seed, `m`; the result is exactly `512 * (m + 1) + m / 16384 -
    /// 2**31`, one less where that is negative and `m` is a multiple of
    /// 16384 (Annex N takes `r - 1` of a negative integral `r`), and
    /// `LONG_MIN` where `m` is all ones and the conversion overflows, as the
    /// reference code returns on x86 and Icarus Verilog does.
    fn random(&mut self, seed: Option<&sv::ir::Expr>) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        use sv::ir::{BinaryOp, Expr};
        let (current, store_back) = match seed {
            Some(seed) => {
                let lvalues = lvalue_from_expr(seed)
                    .ok_or_else(|| unsupported("seed variable of `$random`"))?;
                (self.hoist(seed)?, Some(lvalues))
            }
            None => (Expr::Ident(self.dist_seed().1), None),
        };
        let literal = |value: u32| Expr::Literal(format!("32'd{value}"));
        let binary = |left, op, right| Expr::Binary {
            left: Box::new(left),
            op,
            right: Box::new(right),
        };
        let current = self.int_formal(current);
        let current = self.hidden("random_seed", &current, false)?;
        let seeded = Expr::Mux {
            condition: Box::new(binary(current.clone(), BinaryOp::Eq, literal(0))),
            then_expr: Box::new(literal(259_341_593)),
            else_expr: Box::new(current),
        };
        let next = binary(
            binary(seeded, BinaryOp::Mul, literal(69_069)),
            BinaryOp::Add,
            literal(1),
        );
        let next = self.hidden("random_seed", &next, false)?;
        // `{m, 9'b0} + 512 + m / 16384`, which wraps only for the all-ones `m`.
        let scaled = binary(
            binary(
                binary(next.clone(), BinaryOp::BitAnd, literal(0xffff_fe00)),
                BinaryOp::Add,
                literal(512),
            ),
            BinaryOp::Add,
            binary(next.clone(), BinaryOp::Shr, literal(23)),
        );
        let scaled = self.hidden("random_scaled", &scaled, false)?;
        let integral_negative = binary(
            binary(scaled.clone(), BinaryOp::Lt, literal(0x8000_0000)),
            BinaryOp::LogicAnd,
            binary(
                binary(next.clone(), BinaryOp::BitAnd, literal(0x007f_fe00)),
                BinaryOp::Eq,
                literal(0),
            ),
        );
        let value = Expr::Mux {
            condition: Box::new(binary(
                binary(next.clone(), BinaryOp::Shr, literal(9)),
                BinaryOp::Eq,
                literal(0x007f_ffff),
            )),
            then_expr: Box::new(literal(0x8000_0000)),
            else_expr: Box::new(binary(
                binary(scaled, BinaryOp::BitXor, literal(0x8000_0000)),
                BinaryOp::Sub,
                Expr::Mux {
                    condition: Box::new(integral_negative),
                    then_expr: Box::new(literal(1)),
                    else_expr: Box::new(literal(0)),
                },
            )),
        };
        let value = Expr::Resize {
            expr: Box::new(value),
            width: 32,
            signed: true,
        };
        let result = self.hidden("random_result", &value, true)?;
        match store_back {
            Some(lvalues) => match lvalues.as_slice() {
                [lvalue] => self.assign(lvalue, &next)?,
                parts => self.assign_concat(parts, &next)?,
            },
            None => {
                let (id, _) = self.dist_seed();
                let next = self.eval(&next, Some((32, false)))?;
                self.store(id, SIROffset::Static(0), 32, next);
            }
        }
        Ok(result)
    }

    fn store(&mut self, id: SourceVarId, offset: SIROffset, width: usize, value: RegisterId) {
        let target = self.target(id);
        self.b.emit(SIRInstruction::Store(
            target,
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
        let lhs = self.hoist_lvalue(&lhs)?;
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
                    // A select within one array element reads and writes only
                    // that element, which is all its process commits.
                    let window = write
                        .window
                        .unwrap_or_else(|| BitAccess::new(0, var_width - 1));
                    let alias = self.alias(id);
                    let current = arena
                        .alloc(SLTNode::Input {
                            variable: alias,
                            signed: false,
                            index: Vec::new(),
                            access: window,
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
                    let current = if window.lsb == 0 {
                        current
                    } else {
                        let bottom = slt_constant(arena, BigUint::from(window.lsb), wide, false)?;
                        arena
                            .alloc(SLTNode::Binary(current, BinaryOp::Shl, bottom))
                            .map_err(slt_error)?
                    };
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
                                Some(other) => {
                                    slt_and(arena, &mut ConstCache::default(), other, same)?
                                }
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
                    let window_width = window.msb - window.lsb + 1;
                    let updated = if window.lsb == 0 && window_width == wide {
                        updated
                    } else {
                        arena
                            .alloc(SLTNode::Slice {
                                expr: updated,
                                access: window,
                            })
                            .map_err(slt_error)?
                    };
                    let value = self.lower_slt(arena, updated)?;
                    let offset = sv_memory_offset(self.m.var(id), window.lsb, window_width);
                    self.store(id, offset, window_width, value);
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
                let target = self.target(id);
                self.b.emit(SIRInstruction::Load(
                    old,
                    target,
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
        let target = self.target(id);
        self.b.emit(SIRInstruction::Load(
            old,
            target,
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
        // The positions of the parts are evaluated before any part is written.
        let mut evaluated = Vec::with_capacity(parts.len());
        for part in parts {
            let part = self.propagate_lvalue(part);
            let part = self.hoist_lvalue(&part)?;
            evaluated.push(if parts.len() > 1 {
                self.freeze_lvalue(part)?
            } else {
                part
            });
        }
        let parts = evaluated;
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
            sv::ir::Stmt::Call {
                name,
                args,
                arg_order,
            } => {
                if let Some(import) = self.m.dpi_imports.get(name).cloned() {
                    self.dpi_call(&import, args, arg_order)?;
                    return Ok(true);
                }
                if !self.m.subroutines.contains_key(name) {
                    return Err(unsupported(format!("call of `{name}`")));
                }
                self.call(name, args, arg_order)?;
                Ok(true)
            }
            sv::ir::Stmt::Eval(expr) => {
                self.eval(expr, None)?;
                Ok(true)
            }
            sv::ir::Stmt::SystemTask { task, args } => self.system_task(*task, args),
        }
    }

    /// A system task statement: a runtime event, and for `$fatal` the end of
    /// simulation with an error.
    fn system_task(
        &mut self,
        task: sv::ir::SystemTask,
        args: &[sv::ir::SystemTaskArg<sv::ir::Expr>],
    ) -> Result<bool, sv::AnalyzerError> {
        if let sv::ir::SystemTask::Readmem(format) = task {
            let mut addresses = [None, None];
            for (slot, arg) in addresses.iter_mut().zip(args.iter().skip(2)) {
                let sv::ir::SystemTaskArg::Expr(expr) = arg else {
                    continue;
                };
                let signed = self.expr_signed(expr);
                let (value, width) = self
                    .eval_const(expr)?
                    .ok_or_else(|| unsupported(format!("`{task}` address that is not constant")))?;
                let value = num_bigint::BigInt::from(value);
                let value = if signed && width > 0 && value.bit(width as u64 - 1) {
                    value - (num_bigint::BigInt::from(1u8) << width)
                } else {
                    value
                };
                *slot = Some(
                    value
                        .to_i128()
                        .ok_or_else(|| unsupported(format!("`{task}` address")))?,
                );
            }
            let (id, words) = self.m.readmem(format, args, addresses[0], addresses[1])?;
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
        // IEEE 1800-2023 24.7: `$exit` outside a program block is ignored.
        if task == sv::ir::SystemTask::Exit {
            return Ok(true);
        }
        let kind =
            system_task_kind(task).ok_or_else(|| unsupported(format!("system task `{task}`")))?;
        // A process ends the simulation itself; the rest of its body does not
        // run. A stop suspends the process with the simulation, which then
        // continues after the call.
        match kind {
            SystemTaskKind::Control(RuntimeEventKind::Finish) if self.kernel.is_some() => {
                self.with_kernel(ProcessKernelBuilder::finish);
                return Ok(false);
            }
            SystemTaskKind::Control(RuntimeEventKind::Stop) if self.kernel.is_some() => {
                self.with_kernel(ProcessKernelBuilder::stop)
                    .map_err(|error| unsupported(error.to_string()))?;
                return Ok(true);
            }
            _ => {}
        }
        let assertion = if matches!(kind, SystemTaskKind::Assertion(_)) {
            let Some(sv::ir::SystemTaskArg::Expr(condition)) = args.first() else {
                return Err(unsupported(format!("condition of `{task}`")));
            };
            Some(self.eval_truth(condition)?)
        } else {
            None
        };
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
                    return Err(unsupported(format!("empty argument of `{task}`")));
                }
            }
        }
        // Argument effects happen before branching, and the condition register
        // retains the value it had before those effects.
        let resume = match assertion {
            Some((_, Some(true))) => return Ok(true),
            Some((truth, None)) => {
                let resume = self.b.new_block();
                let fail = self.b.new_block();
                self.b.seal_block(SIRTerminator::Branch {
                    cond: truth,
                    true_block: (resume, Vec::new()),
                    false_block: (fail, Vec::new()),
                });
                self.b.switch_to_block(fail);
                Some(resume)
            }
            _ => None,
        };
        let event_kind = match &kind {
            SystemTaskKind::Print(kind, _) => *kind,
            SystemTaskKind::Control(kind) => *kind,
            SystemTaskKind::Message => RuntimeEventKind::AssertContinue,
            SystemTaskKind::Fatal => RuntimeEventKind::AssertFatal,
            SystemTaskKind::Assertion(kind) => *kind,
        };
        let site_id = self.m.event_site(RuntimeEventSite {
            kind: event_kind,
            template: template.clone(),
            sizing: DisplaySizing::Ieee,
            scope: None,
            arg_widths,
            arg_signed,
            arg_is_string,
        });
        self.b.emit(SIRInstruction::RuntimeEvent {
            site_id,
            args: regs,
        });
        if kind.is_fatal() {
            let code = self
                .m
                .runtime_error(template.unwrap_or_else(|| sv::ir::SystemTask::Fatal.to_string()));
            self.b.seal_block(SIRTerminator::Error(code));
            if resume.is_none() {
                return Ok(false);
            }
        } else if let Some(resume) = resume {
            self.jump(resume);
        }
        if let Some(resume) = resume {
            self.b.switch_to_block(resume);
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
        let selector = if self.m.calls(selector) {
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
                    (sv::ir::CaseKind::X, sv::ir::CaseLabel::Value(value)) => {
                        sv::ir::Expr::Binary {
                            left: Box::new(sv::ir::Expr::Binary {
                                left: Box::new(selector.clone()),
                                op: sv::ir::BinaryOp::EqWildcard,
                                right: Box::new(value.clone()),
                            }),
                            op: sv::ir::BinaryOp::NeCase,
                            right: Box::new(sv::ir::Expr::Literal("1'b0".to_string())),
                        }
                    }
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
            && let Some((values, last)) = self.unrolled_values(&canonical, condition, step, body)?
        {
            return self.unroll(&canonical, &values, last, body);
        }
        self.runtime_loop(kind, init, condition, step, body)
    }

    /// The loop-variable values of a counted loop with constant bounds, when
    /// it is small enough to unroll, and the value that ends it.
    fn unrolled_values(
        &mut self,
        canonical: &CanonicalLoop,
        condition: Option<&sv::ir::Expr>,
        step: &[sv::ir::Stmt],
        body: &[sv::ir::Stmt],
    ) -> Result<Option<(Vec<(String, i128)>, i128)>, sv::AnalyzerError> {
        let mut written = HashSet::default();
        written_names(body, &mut written);
        if written.contains(&canonical.var) {
            return Ok(None);
        }
        let id = self.m.id(&canonical.var).expect("loop variable");
        let (width, signed) = (self.m.var(id).width, self.m.var(id).signed);
        let start_signed = self.expr_signed(&canonical.start);
        let Some((mut start, start_width)) = self.eval_const(&canonical.start)? else {
            return Ok(None);
        };
        // The initializer widens to the loop variable by its own signedness.
        if start_signed
            && start_width > 0
            && start_width < width
            && start.bit(start_width as u64 - 1)
        {
            start |= ((BigUint::from(1u8) << width) - BigUint::from(1u8))
                ^ ((BigUint::from(1u8) << start_width) - BigUint::from(1u8));
        }
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
                break Some((values, numeric));
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
        last: i128,
        body: &[sv::ir::Stmt],
    ) -> Result<bool, sv::AnalyzerError> {
        self.unrolled += values.len();
        // The loop variable may be declared outside the loop, which then reads
        // its value: the one `break` left, or the one that ended the loop.
        let id = self.m.id(&canonical.var).expect("loop variable");
        let exit = self.b.new_block();
        let saved = self.overlay.remove(&canonical.var);
        let mut reachable = true;
        for literal in values {
            let next = self.b.new_block();
            self.store_constant(id, literal.1);
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
        self.store_constant(id, last);
        self.jump(exit);
        self.b.switch_to_block(exit);
        Ok(true)
    }

    /// Store `value`, truncated to the variable's width, into `id`.
    fn store_constant(&mut self, id: SourceVarId, value: i128) {
        let width = self.m.var(id).width;
        let modulus = num_bigint::BigInt::from(1u8) << width;
        let value = ((num_bigint::BigInt::from(value) % &modulus) + &modulus) % &modulus;
        let register = self.b.alloc_bit(width, false);
        self.b.emit(SIRInstruction::Imm(
            register,
            SIRValue::new(value.to_biguint().expect("a non-negative value")),
        ));
        let offset = sv_memory_offset(self.m.var(id), 0, width);
        self.store(id, offset, width, register);
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
        arg_order: &[usize],
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
        let positions: Vec<_> = if arg_order.is_empty() {
            (0..subroutine.params.len()).collect()
        } else {
            arg_order.to_vec()
        };
        for position in positions {
            let param = subroutine
                .params
                .get(position)
                .ok_or_else(|| unsupported(format!("argument position of `{name}`")))?;
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

    /// Lower a stateful combinational process. Its memory guard records the
    /// implicit sensitivity values, independently of internal settling calls.
    pub fn lower_comb_process(
        mut self,
        body: &[sv::ir::Stmt],
        sensitivity_body: &[sv::ir::Stmt],
        kind: sv::ir::CombProcessKind,
        arena: &mut SLTNodeArena<SourceVarId>,
    ) -> Result<Vec<LogicPath<SourceVarId>>, sv::AnalyzerError> {
        let always_comb = kind != sv::ir::CombProcessKind::AlwaysStar;
        self.direct = true;
        let mut reads = self.m.implicit_reads(body, always_comb);
        reads.extend(self.m.implicit_reads(sensitivity_body, always_comb));
        for stmt in body {
            let mut nonblocking = false;
            stmt.walk(&mut |stmt| {
                nonblocking |= matches!(
                    stmt,
                    sv::ir::Stmt::Assign {
                        nonblocking: true,
                        ..
                    } | sv::ir::Stmt::AssignConcat {
                        nonblocking: true,
                        ..
                    }
                )
            });
            if nonblocking {
                return Err(unsupported("nonblocking assignment inside always_comb"));
            }
        }
        self.scan(body, &mut HashSet::default());
        if self.exec_block(body)? {
            self.b.seal_block(SIRTerminator::Return);
        }
        let body_unit = seal_drained(std::mem::replace(&mut self.b, SIRBuilder::new()));
        let mut targets: Vec<_> = self
            .targets
            .iter()
            .filter(|(id, _)| !self.m.is_hidden(*id))
            .map(|(id, access)| VarAtomBase::new(*id, access.lsb, access.msb))
            .collect();
        targets.sort_by_key(|atom| (atom.id.0, atom.access.lsb, atom.access.msb));
        let mut merged: Vec<VarAtomBase<SourceVarId>> = Vec::new();
        for target in targets {
            match merged.last_mut() {
                Some(last) if last.id == target.id && target.access.lsb <= last.access.msb + 1 => {
                    last.access.msb = last.access.msb.max(target.access.msb)
                }
                _ => merged.push(target),
            }
        }
        let mut targets = merged;
        let dependencies = super::comb::subtract_written_sensitivity(reads.clone(), &targets);
        let mut sources: Vec<_> = if always_comb {
            dependencies.clone()
        } else {
            reads
        }
        .into_iter()
        .collect();
        sources.sort_by_key(|atom| (atom.id.0, atom.access.lsb, atom.access.msb));
        let (initialized, _) = self.m.temp("comb_initialized", 1, false, false);
        let init = self.b.alloc_bit(1, false);
        self.b.emit(SIRInstruction::Load(
            init,
            stable(initialized),
            SIROffset::Static(0),
            1,
        ));
        let mut changed = self.b.alloc_bit(1, false);
        self.b
            .emit(SIRInstruction::Imm(changed, SIRValue::new(0u8)));
        let mut snapshots = Vec::new();
        for source in &sources {
            let width = source.access.msb - source.access.lsb + 1;
            let (saved, _) = self.m.temp(
                "comb_input",
                width,
                false,
                self.m.four_state && self.m.var(source.id).is_4state,
            );
            let current = self.b.alloc_logic(width);
            let previous = self.b.alloc_logic(width);
            let offset = sv_memory_offset(self.m.var(source.id), source.access.lsb, width);
            self.b.emit(SIRInstruction::Load(
                current,
                stable(source.id),
                offset,
                width,
            ));
            self.b.emit(SIRInstruction::Load(
                previous,
                stable(saved),
                SIROffset::Static(0),
                width,
            ));
            let differs = self.b.alloc_bit(1, false);
            self.b.emit(SIRInstruction::Binary(
                differs,
                current,
                BinaryOp::NeCase,
                previous,
            ));
            let next = self.b.alloc_bit(1, false);
            self.b
                .emit(SIRInstruction::Binary(next, changed, BinaryOp::Or, differs));
            changed = next;
            snapshots.push((saved, width, current));
        }
        let run = self.b.alloc_bit(1, false);
        if always_comb {
            let first = self.b.alloc_bit(1, false);
            self.b
                .emit(SIRInstruction::Unary(first, UnaryOp::LogicNot, init));
            self.b
                .emit(SIRInstruction::Binary(run, first, BinaryOp::Or, changed));
        } else {
            self.b
                .emit(SIRInstruction::Binary(run, init, BinaryOp::And, changed));
        }
        // Snapshot before executing: writes in the body do not retrigger it.
        for &(saved, width, current) in &snapshots {
            self.store(saved, SIROffset::Static(0), width, current);
        }
        let one = self.b.alloc_bit(1, false);
        self.b.emit(SIRInstruction::Imm(one, SIRValue::new(1u8)));
        self.store(initialized, SIROffset::Static(0), 1, one);
        let execute = self.b.new_block();
        let done = self.b.new_block();
        self.b.seal_block(SIRTerminator::Branch {
            cond: run,
            true_block: (execute, vec![]),
            false_block: (done, vec![]),
        });
        self.b.switch_to_block(execute);
        self.b.inline_execution_unit(&body_unit);
        if !always_comb {
            // @* is not waiting during its body. Its own writes cannot wake
            // the same activation, including when another output is published.
            for (source, &(saved, width, _)) in sources.iter().zip(&snapshots) {
                let current = self.b.alloc_logic(width);
                let offset = sv_memory_offset(self.m.var(source.id), source.access.lsb, width);
                self.b.emit(SIRInstruction::Load(
                    current,
                    stable(source.id),
                    offset,
                    width,
                ));
                self.store(saved, SIROffset::Static(0), width, current);
            }
        }
        self.jump(done);
        self.b.switch_to_block(done);
        let mut private_locals = HashMap::default();
        let unit = seal_builder(self.b).into_map_addr(&mut |addr| {
            let id = addr.var_id;
            if !self.m.is_hidden(id) || self.m.created.contains(&id) {
                return id;
            }
            *private_locals.entry(id).or_insert_with(|| {
                let variable = self.m.var(id).clone();
                let (copy, name) = self.m.temp(
                    "comb_local",
                    variable.width,
                    variable.signed,
                    variable.is_4state,
                );
                self.m.variables.insert(
                    copy,
                    SvVariable {
                        path: vec![name],
                        ..variable
                    },
                );
                copy
            })
        });
        if targets.is_empty() {
            targets.push(VarAtomBase::new(initialized, 0, 0));
        }
        let mut paths = Vec::new();
        for target in targets {
            let expr = arena
                .alloc(SLTNode::Input {
                    variable: target.id,
                    signed: false,
                    index: Vec::new(),
                    access: target.access,
                })
                .map_err(slt_error)?;
            paths.push(LogicPath {
                target: LogicPathTarget::Process {
                    process: initialized,
                    after_initials: kind == sv::ir::CombProcessKind::AlwaysComb,
                    target,
                    unit: Box::new(unit.clone()),
                },
                sources: dependencies.clone(),
                previous_sources: HashSet::default(),
                address_sources: HashSet::default(),
                local_inputs: Vec::new(),
                order_before: HashSet::default(),
                comb_capture_enable_sites: Vec::new(),
                comb_capture_enable_always: false,
                pre_lower_nodes: Vec::new(),
                expr,
            });
        }
        Ok(paths)
    }

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

impl Ff<'_, '_> {
    /// Lower an `initial` body into a process kernel that runs it from time
    /// zero, reading and writing stable state directly.
    pub fn lower_initial_kernel(
        mut self,
        body: &[sv::ir::Stmt],
        slots: ProcessSlots<SourceVarId>,
    ) -> Result<ExecutionUnit<Addr>, sv::AnalyzerError> {
        // A nonblocking update would take effect in a later region of the
        // time step, after the process has run on.
        for stmt in body {
            let mut nonblocking = false;
            stmt.walk(&mut |stmt| {
                nonblocking |= matches!(
                    stmt,
                    sv::ir::Stmt::Assign {
                        nonblocking: true,
                        ..
                    } | sv::ir::Stmt::AssignConcat {
                        nonblocking: true,
                        ..
                    }
                );
            });
            if nonblocking {
                return Err(unsupported(
                    "nonblocking assignment in an initial block that runs as a process",
                ));
            }
        }
        let mut kernel = ProcessKernelBuilder::new(slots);
        std::mem::swap(&mut self.b, kernel.builder());
        self.kernel = Some(kernel);
        self.exec_block(body)?;
        let mut kernel = self.kernel.take().expect("lowering a process kernel");
        std::mem::swap(&mut self.b, kernel.builder());
        let mut unit = kernel.build();
        prune_unreachable_blocks(&mut unit);
        Ok(unit)
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
        ConstExpr::SelectRange {
            expr,
            msb,
            lsb,
            signed,
        } => ConstExpr::SelectRange {
            expr: Box::new(
                if matches!(&**expr, ConstExpr::Ident(_))
                    && (const_expr_references_identifier(msb)
                        || const_expr_references_identifier(lsb))
                {
                    (**expr).clone()
                } else {
                    go(expr)
                },
            ),
            msb: Box::new(go(msb)),
            lsb: Box::new(go(lsb)),
            signed: *signed,
        },
        ConstExpr::Select { expr, bit } => ConstExpr::Select {
            expr: Box::new(go(expr)),
            bit: Box::new(go(bit)),
        },
        ConstExpr::Function { callee, args, site } => ConstExpr::Function {
            callee: callee.clone(),
            site: *site,
            args: args.iter().map(go).collect(),
        },
        ConstExpr::Resize {
            expr,
            width,
            signed,
        } => ConstExpr::Resize {
            expr: Box::new(go(expr)),
            width: *width,
            signed: *signed,
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
        Expr::Call {
            callee,
            args,
            arg_order,
        } => Expr::Call {
            arg_order: arg_order.clone(),
            callee: callee.clone(),
            args: args.iter().map(|arg| arg.as_ref().map(go)).collect(),
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
