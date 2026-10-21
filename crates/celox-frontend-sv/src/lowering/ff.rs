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

mod file_io;
mod fread;

use super::procedural::*;
use super::*;
use celox_design::{CounterKind, NbaSite, SPARSE_WORKING_REGION, WaitSite};
use celox_frontend_core::process::{ProcessKernelBuilder, ProcessKernelError};
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
    sparse_memories: HashSet<SourceVarId>,
    b: SIRBuilder<Addr>,
    /// Variables read from the working region.
    working: HashSet<SourceVarId>,
    /// Variables a nonblocking assignment writes.
    nonblocking: HashSet<SourceVarId>,
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
    /// The index of the process being lowered among the module's kernels.
    process: usize,
    /// The wait sites of the process, by local index.
    wait_sites: Vec<WaitSite<SourceVarId>>,
    /// The nonblocking assignment sites of the process, by local index.
    nba_sites: Vec<NbaSite<SourceVarId>>,
    host_request_sites: Vec<celox_design::HostRequestSite<SourceVarId>>,
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
            sparse_memories: HashSet::default(),
            b: SIRBuilder::new(),
            working: HashSet::default(),
            nonblocking: HashSet::default(),
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
            process: 0,
            wait_sites: Vec::new(),
            nba_sites: Vec::new(),
            host_request_sites: Vec::new(),
        }
    }

    pub fn with_sparse_memories(mut self, names: &HashSet<String>) -> Self {
        self.sparse_memories = names.iter().filter_map(|name| self.m.id(name)).collect();
        self
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
        } else {
            self.nonblocking.insert(id);
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
                sv::ir::Stmt::SystemTask { task, args, .. } => {
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
                        SystemFunction::ScopeRandomize
                        | SystemFunction::Urandom
                        | SystemFunction::UrandomRange => {
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
            } else if working_set.contains(id)
                || variables.get(id).is_some_and(SvVariable::is_scratch)
            {
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
        let expr = force_reads(&self.propagate(&expr), self.m);
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
        let node = self.expr_slt(
            &floating::condition(expr, self.m.variables, self.m.name_to_id),
            None,
            &mut arena,
        )?;
        let truth = slt_truth(&mut arena, node)?;
        let constant = slt_bool(&arena, &mut ConstCache::default(), truth);
        Ok((self.lower_slt(&arena, truth)?, constant))
    }

    /// Whether an expression is not logically false, as a one-bit register.
    fn eval_not_false(&mut self, expr: &sv::ir::Expr) -> Result<RegisterId, sv::AnalyzerError> {
        let mut arena = SLTNodeArena::new();
        let node = self.expr_slt(
            &floating::condition(expr, self.m.variables, self.m.name_to_id),
            None,
            &mut arena,
        )?;
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
                callee: sv::ir::Callee::System(sv::ir::SystemFunction::Fread),
                args,
                ..
            } => self.fread(args)?,
            Expr::Call {
                callee:
                    sv::ir::Callee::System(
                        function @ (sv::ir::SystemFunction::Ftell
                        | sv::ir::SystemFunction::Fseek
                        | sv::ir::SystemFunction::Rewind
                        | sv::ir::SystemFunction::Fgetc
                        | sv::ir::SystemFunction::Ungetc
                        | sv::ir::SystemFunction::Feof),
                    ),
                args,
                ..
            } => self.file_function(*function, args)?,
            Expr::Call {
                callee: sv::ir::Callee::System(sv::ir::SystemFunction::Fopen),
                args,
                ..
            } => self.fopen(args)?,
            Expr::Call {
                callee: sv::ir::Callee::System(sv::ir::SystemFunction::ScopeRandomize),
                args,
                ..
            } => {
                for arg in args.iter().flatten() {
                    let Expr::Ident(name) = arg else {
                        return Err(unsupported("scope randomize variable"));
                    };
                    let target = sv::ir::LValue::Ident(name.clone());
                    let width = self.lvalue_width(&target)?;
                    let mut words = Vec::with_capacity(width.div_ceil(32));
                    for offset in (0..width).step_by(32) {
                        words.push(Expr::Resize {
                            expr: Box::new(self.randomize_word()?),
                            width: (width - offset).min(32),
                            signed: false,
                        });
                    }
                    words.reverse();
                    self.assign(&target, &Expr::Concat(words))?;
                }
                Expr::Literal("32'sd1".into())
            }
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
                callee:
                    sv::ir::Callee::System(
                        function @ (sv::ir::SystemFunction::TestPlusargs
                        | sv::ir::SystemFunction::ValuePlusargs),
                    ),
                args,
                ..
            } => self.plusargs(*function, args)?,
            Expr::Call {
                callee: sv::ir::Callee::System(sv::ir::SystemFunction::DistUniform),
                args,
                ..
            } => self.dist_uniform(args)?,
            Expr::Call {
                callee: sv::ir::Callee::System(sv::ir::SystemFunction::Random),
                args,
                ..
            } => self.random(args.first().and_then(Option::as_ref))?,
            Expr::Call {
                callee:
                    sv::ir::Callee::System(
                        function @ (sv::ir::SystemFunction::Time | sv::ir::SystemFunction::Stime),
                    ),
                args,
                ..
            } => self.time(*function, args)?,
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
                self.m.variables.get_mut(&temp).unwrap().float = match r#type.kind() {
                    sv::ir::TypeKind::Real => Some(celox_sir::FloatPrecision::F64),
                    sv::ir::TypeKind::Shortreal => Some(celox_sir::FloatPrecision::F32),
                    _ => None,
                };
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

    fn vcd_task(
        &mut self,
        operation: sv::ir::VcdTask,
        args: &[sv::ir::SystemTaskArg<sv::ir::Expr>],
    ) -> Result<(), sv::AnalyzerError> {
        use celox_design::{HostRequestKind, HostRequestSite, HostRequestValue};
        use sv::ir::VcdTask;
        if self.kernel.is_none() {
            return Err(unsupported("VCD tasks require a timed process"));
        }
        let mut values = Vec::new();
        let mut strings = Vec::new();
        for (index, arg) in args.iter().enumerate() {
            if operation == VcdTask::Vars
                && index == 0
                && matches!(arg, sv::ir::SystemTaskArg::Empty)
            {
                continue;
            }
            if operation == VcdTask::Vars && index > 0 {
                let sv::ir::SystemTaskArg::Str(name) = arg else {
                    return Err(unsupported("$dumpvars module or variable selection"));
                };
                strings.push(name.clone());
                continue;
            }
            let (reg, signed, is_string) = match arg {
                sv::ir::SystemTaskArg::Expr(expr) => {
                    (self.eval(expr, None)?, self.expr_signed(expr), false)
                }
                sv::ir::SystemTaskArg::Str(text) if operation == VcdTask::File => {
                    let bytes = unescape(text).into_bytes();
                    let width = (bytes.len() * 8).max(8);
                    let value = bytes.iter().fold(BigUint::zero(), |value, byte| {
                        (value << 8u32) | BigUint::from(*byte)
                    });
                    let reg = self.b.alloc_bit(width, false);
                    self.b.emit(SIRInstruction::Imm(reg, SIRValue::new(value)));
                    (reg, false, true)
                }
                _ => {
                    return Err(unsupported(
                        "VCD argument must be an integral value or filename string",
                    ));
                }
            };
            let width = self.b.register(&reg).width();
            let is_4state = matches!(self.b.register(&reg), RegisterType::Logic { .. });
            let (id, _) = self.m.temp("vcd_argument", width, signed, is_4state);
            self.store(id, SIROffset::Static(0), width, reg);
            values.push(HostRequestValue {
                float_precision: None,
                id,
                width,
                signed,
                is_string,
            });
        }
        let local_site = self
            .host_request_sites
            .iter()
            .filter(|site| site.kind == HostRequestKind::Vcd)
            .count() as u32;
        self.host_request_sites.push(HostRequestSite {
            process: self.process,
            kind: HostRequestKind::Vcd,
            requires_timed_simulation: false,
            operation: operation as u32,
            args: values,
            results: Vec::new(),
            strings,
        });
        self.with_kernel(|kernel| kernel.host_request(HostRequestKind::Vcd, local_site))
            .map_err(|error| unsupported(error.to_string()))?;
        Ok(())
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

    /// A bijective 32-bit output permutation of the process RNG. Mixing
    /// high bits into low bits prevents the LCG's short low-bit periods from
    /// freezing bits when a wide variable consumes an even number of words.
    fn randomize_word(&mut self) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        use sv::ir::{BinaryOp, Expr};
        let mut word = self.urandom(None)?;
        for (shift, multiplier) in [(16, 0x7feb352du32), (15, 0x846ca68bu32)] {
            let mixed = Expr::Binary {
                left: Box::new(word.clone()),
                op: BinaryOp::BitXor,
                right: Box::new(Expr::Binary {
                    left: Box::new(word),
                    op: BinaryOp::Shr,
                    right: Box::new(Expr::Literal(shift.to_string())),
                }),
            };
            word = self.hidden(
                "randomize_mix",
                &Expr::Binary {
                    left: Box::new(mixed),
                    op: BinaryOp::Mul,
                    right: Box::new(Expr::Literal(format!("32'd{multiplier}"))),
                },
                false,
            )?;
        }
        let mixed = Expr::Binary {
            left: Box::new(word.clone()),
            op: BinaryOp::BitXor,
            right: Box::new(Expr::Binary {
                left: Box::new(word),
                op: BinaryOp::Shr,
                right: Box::new(Expr::Literal("16".into())),
            }),
        };
        self.hidden("randomize_word", &mixed, false)
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

    /// IEEE 1800-2023 20.3.1: `$time` is the simulation time, which counts
    /// the global time precision, in the time unit of the calling scope,
    /// rounded to an integer; halves round up. The analyzer passes the
    /// precision's units per time unit as the operand. `$stime` (20.3.2) is
    /// its low 32 bits.
    fn time(
        &mut self,
        function: sv::ir::SystemFunction,
        args: &[Option<sv::ir::Expr>],
    ) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        use sv::ir::{BinaryOp, Expr};
        let divisor = args
            .first()
            .and_then(Option::as_ref)
            .ok_or_else(|| unsupported(format!("operand of `{function}`")))?
            .clone();
        let binary = |left, op, right| Expr::Binary {
            left: Box::new(left),
            op,
            right: Box::new(right),
        };
        let time = Expr::Ident(self.m.time_slot());
        let time = if divisor == Expr::Literal("64'd1".into()) {
            time
        } else {
            let half = binary(
                divisor.clone(),
                BinaryOp::Shr,
                Expr::Literal("64'd1".into()),
            );
            let rounds_up = binary(
                binary(time.clone(), BinaryOp::Mod, divisor.clone()),
                BinaryOp::Ge,
                half,
            );
            binary(
                binary(time, BinaryOp::Div, divisor),
                BinaryOp::Add,
                Expr::Resize {
                    expr: Box::new(rounds_up),
                    width: 64,
                    signed: false,
                },
            )
        };
        Ok(match function {
            sv::ir::SystemFunction::Stime => Expr::Resize {
                expr: Box::new(time),
                width: 32,
                signed: false,
            },
            _ => time,
        })
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

    /// Annex N's uniform transform, represented as an exact rational. The
    /// largest product has 79 bits, so 96-bit temporaries avoid overflow.
    fn dist_uniform(
        &mut self,
        args: &[Option<sv::ir::Expr>],
    ) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        use sv::ir::{BinaryOp as B, Expr as E};
        let seed = args
            .first()
            .and_then(Option::as_ref)
            .ok_or_else(|| unsupported("seed of `$dist_uniform`"))?;
        let (lvalues, seed_read) = self.capture_system_actual(seed)?;
        let seed_width = self
            .m
            .comparison_width(&seed_read, &seed_read)
            .ok_or_else(|| unsupported("integral seed of `$dist_uniform`"))?;
        let seed_signed = self.expr_signed(seed);
        let original = self
            .host_hidden(
                "dist_original_seed",
                &seed_read,
                seed_width,
                seed_signed,
                true,
            )?
            .1;
        let mut bound = |index| -> Result<E, sv::AnalyzerError> {
            let value = self.hoist(
                args.get(index)
                    .and_then(Option::as_ref)
                    .ok_or_else(|| unsupported("bounds of `$dist_uniform`"))?,
            )?;
            let value = E::Resize {
                expr: Box::new(value),
                width: 32,
                signed: true,
            };
            self.hidden("dist_bound", &value, true)
        };
        let start = bound(1)?;
        let end = bound(2)?;
        let current = self.int_formal(original.clone());
        let current = self.hidden("dist_old_seed", &current, false)?;
        // The distribution's inout formal is int. Only the final copyout
        // writes the caller's actual, whose selector was captured once.
        let formal = self.hidden("dist_seed_formal", &current, true)?;
        let full = self.random(Some(&formal))?;
        let next = E::Binary {
            left: Box::new(E::Binary {
                left: Box::new(E::Mux {
                    condition: Box::new(E::Binary {
                        left: Box::new(current.clone()),
                        op: B::Eq,
                        right: Box::new(E::Literal("32'd0".into())),
                    }),
                    then_expr: Box::new(E::Literal("32'd259341593".into())),
                    else_expr: Box::new(current.clone()),
                }),
                op: B::Mul,
                right: Box::new(E::Literal("32'd69069".into())),
            }),
            op: B::Add,
            right: Box::new(E::Literal("32'd1".into())),
        };
        let next = self.hidden("dist_next_seed", &next, false)?;
        let lit = |n: i128| {
            if n < 0 {
                E::Unary {
                    op: sv::ir::UnaryOp::Minus,
                    expr: Box::new(E::Literal(format!("96'sd{}", -n))),
                }
            } else {
                E::Literal(format!("96'sd{n}"))
            }
        };
        let bin = |left, op, right| E::Binary {
            left: Box::new(left),
            op,
            right: Box::new(right),
        };
        let wide = |value| E::Resize {
            expr: Box::new(value),
            width: 96,
            signed: true,
        };
        let mux = |condition, then_expr, else_expr| E::Mux {
            condition: Box::new(condition),
            then_expr: Box::new(then_expr),
            else_expr: Box::new(else_expr),
        };
        let start = wide(start);
        let end = wide(end);
        let invalid = bin(start.clone(), B::Ge, end.clone());
        // If end is INT_MAX, Annex N shifts the lower bound down by one,
        // and adds one after the uniform transform.
        let shifted = bin(end.clone(), B::Eq, lit(i32::MAX as i128));
        let shifted_value = mux(shifted.clone(), lit(1), lit(0));
        let a = bin(start.clone(), B::Sub, shifted_value.clone());
        let span = bin(bin(end.clone(), B::Sub, start.clone()), B::Add, lit(1));
        // Extract the 23-bit mantissa before converting to signed wide
        // arithmetic. Widening the raw seed as signed would sign-extend its
        // high bit, and a logical shift would then retain those high ones.
        let m = wide(E::Resize {
            expr: Box::new(bin(next, B::Shr, E::Literal("32'd9".into()))),
            width: 32,
            signed: false,
        });
        let fraction = bin(
            bin(m.clone(), B::Mul, lit(8_388_608)),
            B::Add,
            bin(lit(8_388_608), B::Add, m),
        );
        let denominator = lit(1i128 << 46);
        let numerator = bin(
            bin(a, B::Mul, denominator.clone()),
            B::Add,
            bin(span, B::Mul, fraction),
        );
        let numerator = bin(
            numerator,
            B::Add,
            bin(shifted_value, B::Mul, denominator.clone()),
        );
        let numerator = self
            .host_hidden("dist_numerator", &numerator, 96, true, false)?
            .1;
        let negative_integral = bin(
            bin(numerator.clone(), B::Lt, lit(0)),
            B::LogicAnd,
            bin(
                bin(numerator.clone(), B::BitAnd, lit((1i128 << 46) - 1)),
                B::Eq,
                lit(0),
            ),
        );
        let value = bin(
            bin(numerator, B::Sar, lit(46)),
            B::Sub,
            mux(negative_integral, lit(1), lit(0)),
        );
        let value = self.host_hidden("dist_value", &value, 96, true, false)?.1;
        let value = mux(
            bin(value.clone(), B::Lt, start.clone()),
            start.clone(),
            mux(bin(value.clone(), B::Gt, end.clone()), end.clone(), value),
        );
        let is_full = bin(
            bin(start.clone(), B::Eq, lit(i32::MIN as i128)),
            B::LogicAnd,
            bin(end, B::Eq, lit(i32::MAX as i128)),
        );
        let value = mux(invalid.clone(), start, mux(is_full, wide(full), value));
        // A degenerate/reversed interval returns start without consuming seed.
        let next = E::Resize {
            expr: Box::new(formal),
            width: seed_width,
            signed: true,
        };
        let restored = mux(invalid, original, next);
        match lvalues.as_slice() {
            [lvalue] => self.assign(lvalue, &restored)?,
            parts => self.assign_concat(parts, &restored)?,
        }
        Ok(E::Resize {
            expr: Box::new(value),
            width: 32,
            signed: true,
        })
    }

    /// Capture an inout/output actual's reference before reading or writing
    /// it, including selectors changed by the call or by other selectors.
    fn capture_system_actual(
        &mut self,
        expr: &sv::ir::Expr,
    ) -> Result<(Vec<sv::ir::LValue>, sv::ir::Expr), sv::AnalyzerError> {
        use sv::ir::{Expr, LValue};
        let parts =
            lvalue_from_expr(expr).ok_or_else(|| unsupported("integral output/inout actual"))?;
        let signed = self.expr_signed(expr);
        let mut captured = Vec::new();
        for part in parts {
            let part = self.hoist_lvalue(&part)?;
            captured.push(self.freeze_lvalue(part)?);
        }
        let reads = captured
            .iter()
            .map(|part| match part {
                LValue::Ident(name) => Expr::Ident(name.clone()),
                LValue::Select { name, msb, lsb, .. } => Expr::Select {
                    expr: Box::new(Expr::Ident(name.clone())),
                    msb: msb.clone(),
                    lsb: lsb.clone(),
                    signed,
                },
            })
            .collect::<Vec<_>>();
        let read = if reads.len() == 1 {
            reads.into_iter().next().unwrap()
        } else {
            Expr::Concat(reads)
        };
        Ok((captured, read))
    }

    fn host_hidden(
        &mut self,
        purpose: &str,
        expr: &sv::ir::Expr,
        width: usize,
        signed: bool,
        four_state: bool,
    ) -> Result<(SourceVarId, sv::ir::Expr), sv::AnalyzerError> {
        let value = self.eval(expr, Some((width, signed)))?;
        let (id, name) = self.m.temp(purpose, width, signed, four_state);
        self.store(id, SIROffset::Static(0), width, value);
        Ok((id, sv::ir::Expr::Ident(name)))
    }

    fn plusargs(
        &mut self,
        function: sv::ir::SystemFunction,
        args: &[Option<sv::ir::Expr>],
    ) -> Result<sv::ir::Expr, sv::AnalyzerError> {
        use celox_design::{HostRequestKind, HostRequestSite, HostRequestValue};
        use sv::ir::Expr;
        if self.kernel.is_none() {
            return Err(unsupported(format!(
                "{} outside a timed process",
                function.name()
            )));
        }
        let query = self.hoist(
            args.first()
                .and_then(Option::as_ref)
                .ok_or_else(|| unsupported("plusargs query"))?,
        )?;
        let width = self
            .m
            .comparison_width(&query, &query)
            .ok_or_else(|| unsupported("packed plusargs query"))?;
        let (query_id, _) = self.host_hidden("plusargs_query", &query, width, false, true)?;
        let value_query = function == sv::ir::SystemFunction::ValuePlusargs;
        let mut results = Vec::new();
        let (ret_id, ret_name) = self.m.temp("plusargs_result", 32, true, false);
        results.push(HostRequestValue {
            float_precision: None,
            id: ret_id,
            width: 32,
            signed: true,
            is_string: false,
        });
        let target = if value_query {
            let expr = args
                .get(1)
                .and_then(Option::as_ref)
                .ok_or_else(|| unsupported("plusargs destination"))?;
            let (lvalues, captured) = self.capture_system_actual(expr)?;
            let width = self
                .m
                .comparison_width(expr, expr)
                .ok_or_else(|| unsupported("integral plusargs destination"))?;
            let (id, result) = self.host_hidden("plusargs_value", &captured, width, false, true)?;
            results.push(HostRequestValue {
                float_precision: None,
                id,
                width,
                signed: false,
                is_string: false,
            });
            Some((lvalues, result))
        } else {
            None
        };
        let site = self
            .host_request_sites
            .iter()
            .filter(|site| site.kind == HostRequestKind::Plusargs)
            .count() as u32;
        self.host_request_sites.push(HostRequestSite {
            process: 0,
            kind: HostRequestKind::Plusargs,
            requires_timed_simulation: false,
            operation: u32::from(value_query),
            args: vec![HostRequestValue {
                float_precision: None,
                id: query_id,
                width,
                signed: false,
                is_string: true,
            }],
            results,
            strings: Vec::new(),
        });
        self.with_kernel(|kernel| kernel.host_request(HostRequestKind::Plusargs, site))
            .map_err(kernel_error)?;
        if let Some((lvalues, result)) = target {
            match lvalues.as_slice() {
                [lvalue] => self.assign(lvalue, &result)?,
                parts => self.assign_concat(parts, &result)?,
            }
        }
        Ok(Expr::Ident(ret_name))
    }

    fn store(&mut self, id: SourceVarId, offset: SIROffset, width: usize, value: RegisterId) {
        let value = if let Some((enabled, forced)) = self.m.force_controls(id) {
            let condition = self.b.alloc_bit(1, false);
            self.b.emit(SIRInstruction::Load(
                condition,
                stable(enabled),
                SIROffset::Static(0),
                1,
            ));
            let replacement = self.b.alloc_reg(self.b.register(&value).clone());
            self.b.emit(SIRInstruction::Load(
                replacement,
                stable(forced),
                offset.clone(),
                width,
            ));
            let selected = self.b.alloc_reg(self.b.register(&value).clone());
            self.b
                .emit(SIRInstruction::Mux(selected, condition, replacement, value));
            selected
        } else {
            value
        };
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
        let owned_rhs = if self.m.calls(rhs) {
            self.hoist(rhs)?
        } else {
            rhs.clone()
        };
        let rhs = &owned_rhs;
        let width = self.lvalue_width(&lhs)?;
        let signed = self.expr_signed(rhs);
        let mut arena = SLTNodeArena::new();
        let from = floating::precision(rhs, self.m.variables, self.m.name_to_id);
        let target_var = self.m.id(lhs.name()).map(|id| self.m.var(id));
        let to = target_var.and_then(|v| v.float);
        let target_signed = target_var.is_some_and(|v| v.signed);
        let node = self.expr_slt(
            rhs,
            if from.is_some() || to.is_some() {
                None
            } else {
                Some((width, signed))
            },
            &mut arena,
        )?;
        let node = floating::convert(
            &mut arena,
            node,
            from,
            to,
            width,
            if from.is_none() {
                signed
            } else {
                target_signed
            },
            true,
        )
        .ok_or_else(|| unsupported("real assignment conversion"))?;
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
                if self.sparse_memories.contains(&id) {
                    let part = self.b.alloc_logic(packed_element_width);
                    self.b.emit(SIRInstruction::Slice(
                        part,
                        value,
                        inner_index * packed_element_width,
                        packed_element_width,
                    ));
                    self.when(valid, |this| {
                        this.store(id, offset, packed_element_width, part);
                        Ok(())
                    })?;
                    continue;
                }
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
        if self.sparse_memories.contains(&id) {
            return self.when(valid, |this| {
                this.store(id, offset, target_width, value);
                Ok(())
            });
        }
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
        if matches!(
            stmt,
            sv::ir::Stmt::Delay { .. } | sv::ir::Stmt::WaitEvent(_) | sv::ir::Stmt::Wait(_)
        ) && (self.kernel.is_none()
            || self.active_calls.iter().any(|name| {
                self.m
                    .subroutine(name)
                    .is_some_and(|subroutine| !subroutine.is_task)
            }))
        {
            return Err(unsupported("timed task call outside a suspendable process"));
        }
        match stmt {
            sv::ir::Stmt::Assign {
                lhs,
                rhs,
                nonblocking: true,
            } if self.kernel.is_some() => {
                self.nonblocking(std::slice::from_ref(lhs), rhs)?;
                Ok(true)
            }
            sv::ir::Stmt::AssignConcat {
                parts,
                rhs,
                nonblocking: true,
            } if self.kernel.is_some() => {
                self.nonblocking(parts, rhs)?;
                Ok(true)
            }
            sv::ir::Stmt::Assign { lhs, rhs, .. } => {
                self.assign(lhs, rhs)?;
                Ok(true)
            }
            sv::ir::Stmt::AssignConcat { parts, rhs, .. } => {
                self.assign_concat(parts, rhs)?;
                Ok(true)
            }
            sv::ir::Stmt::Delay {
                amount,
                ticks_per_unit,
            } => {
                self.delay(amount, *ticks_per_unit)?;
                Ok(true)
            }
            sv::ir::Stmt::WaitEvent(items) => {
                self.wait_event(items)?;
                Ok(true)
            }
            sv::ir::Stmt::Wait(condition) => {
                self.wait_level(condition)?;
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
            sv::ir::Stmt::SystemTask { task, args, site } => {
                self.system_task(*task, args, site.as_ref())
            }
        }
    }

    /// A system task statement: a runtime event, and for `$fatal` the end of
    /// simulation with an error.
    fn system_task(
        &mut self,
        task: sv::ir::SystemTask,
        args: &[sv::ir::SystemTaskArg<sv::ir::Expr>],
        site: Option<&sv::ir::SourceSpan>,
    ) -> Result<bool, sv::AnalyzerError> {
        if let sv::ir::SystemTask::Vcd(operation) = task {
            self.vcd_task(operation, args)?;
            return Ok(true);
        }
        if matches!(
            task,
            sv::ir::SystemTask::Fclose
                | sv::ir::SystemTask::Fflush
                | sv::ir::SystemTask::Fdisplay(_)
                | sv::ir::SystemTask::Fwrite(_)
                | sv::ir::SystemTask::Writemem(_)
        ) {
            return self.file_task(task, args);
        }
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
            let (id, words) = self
                .m
                .readmem(format, args, addresses[0], addresses[1], site)?;
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
        // IEEE 1800-2023 24.7: `$exit` outside a program block is ignored,
        // with a warning.
        if task == sv::ir::SystemTask::Exit {
            self.m
                .warn(crate::FrontendWarningKind::ExitOutsideProgram, site);
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
        let (template, values) = system_task_template(self.m, &kind, args);
        let mut regs = Vec::with_capacity(values.len());
        let mut arg_widths = Vec::with_capacity(values.len());
        let mut arg_signed = Vec::with_capacity(values.len());
        let mut arg_is_string = Vec::with_capacity(values.len());
        let mut arg_float_precision = Vec::with_capacity(values.len());
        for value in &values {
            match value {
                sv::ir::SystemTaskArg::Expr(expr) => {
                    let expr = if self.m.calls(expr) {
                        self.hoist(expr)?
                    } else {
                        expr.clone()
                    };
                    let signed = self.expr_signed(&expr);
                    arg_float_precision.push(floating::precision(
                        &expr,
                        self.m.variables,
                        self.m.name_to_id,
                    ));
                    let reg = self.eval(&expr, None)?;
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
                    arg_float_precision.push(None);
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
            arg_float_precision,
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
                let value = if self.kernel.is_some() {
                    // A count with an unknown bit, or a negative one, runs
                    // the body zero times (IEEE 1800-2023 12.7.2).
                    let count = if signed {
                        sv::ir::Expr::Mux {
                            condition: Box::new(sv::ir::Expr::Binary {
                                left: Box::new(count.clone()),
                                op: sv::ir::BinaryOp::Lt,
                                right: Box::new(sv::ir::Expr::Literal("0".to_string())),
                            }),
                            then_expr: Box::new(sv::ir::Expr::Literal("0".to_string())),
                            else_expr: Box::new(count.clone()),
                        }
                    } else {
                        count.clone()
                    };
                    let mut arena = SLTNodeArena::new();
                    let node = self.expr_slt(&count, None, &mut arena)?;
                    let original = self.lower_slt(&arena, node)?;
                    let node =
                        coerce_node_width(&mut arena, node, Some(64), signed).map_err(slt_error)?;
                    let value = self.lower_slt(&arena, node)?;
                    self.zero_if_unknown(value, original)
                } else {
                    self.eval(count, Some((64, signed)))?
                };
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
        // A kernel loop that may not suspend yields now and then, so the
        // runtime charges it against the time slot's budget instead of
        // spinning in the kernel.
        let header = if self.kernel.is_some() && !always_suspends(body) {
            self.yielding_back_edge(header)?
        } else {
            header
        };
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
        // Each inlined automatic task activation owns persistent storage, so
        // another process can enter the same task while this one is suspended.
        // Static formals/locals deliberately keep their module-wide addresses.
        let mut activation = Vec::new();
        if self.kernel.is_some() && subroutine.is_task {
            for name in &subroutine.automatic_locals {
                let original = self
                    .m
                    .id(name)
                    .ok_or_else(|| unsupported(format!("task local `{name}`")))?;
                let variable = self.m.var(original).clone();
                let (copy, copy_name) = self.m.temp(
                    "task_local",
                    variable.width,
                    variable.signed,
                    variable.is_4state,
                );
                self.m.variables.insert(
                    copy,
                    SvVariable {
                        path: vec![copy_name],
                        ..variable
                    },
                );
                if let Some(width) = self.element_widths.get(&stable(original)).copied() {
                    self.element_widths.insert(stable(copy), width);
                    self.element_widths.insert(working(copy), width);
                }
                self.m.name_to_id.insert(name.clone(), copy);
                activation.push((name.clone(), original));
            }
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
                let actual = if self.m.calls(&actual) {
                    self.hoist(&actual)?
                } else {
                    actual
                };
                let width = self.m.var(id).width;
                let signed = self.expr_signed(&actual);
                let from = floating::precision(&actual, self.m.variables, self.m.name_to_id);
                let to = self.m.var(id).float;
                let mut arena = SLTNodeArena::new();
                let node = self.expr_slt(
                    &actual,
                    if from.is_some() || to.is_some() {
                        None
                    } else {
                        Some((width, signed))
                    },
                    &mut arena,
                )?;
                let node = floating::convert(
                    &mut arena,
                    node,
                    from,
                    to,
                    width,
                    if from.is_none() {
                        signed
                    } else {
                        self.m.var(id).signed
                    },
                    true,
                )
                .ok_or_else(|| unsupported("real argument conversion"))?;
                let mut node =
                    coerce_node_width(&mut arena, node, Some(width), signed).map_err(slt_error)?;
                if !self.m.var(id).is_4state {
                    node = arena
                        .alloc(SLTNode::Unary(UnaryOp::ToTwoState, node))
                        .map_err(slt_error)?;
                }
                inputs.push((id, self.lower_slt(&arena, node)?));
            } else if self.kernel.is_none()
                || !subroutine.is_task
                || subroutine.automatic_locals.contains(&param.name)
            {
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
        for (name, original) in activation {
            self.m.name_to_id.insert(name, original);
        }
        Ok(subroutine.return_var.clone())
    }

    // -------------------------------------------------------------- process

    /// Lower a stateful combinational process. Its memory guard records the
    /// implicit sensitivity values, independently of internal settling calls.
    /// `explicit_targets` are hidden buffers consumed outside this process,
    /// such as child-port actuals. They retain their module-wide addresses.
    pub fn lower_comb_process(
        mut self,
        body: &[sv::ir::Stmt],
        sensitivity_body: &[sv::ir::Stmt],
        kind: sv::ir::CombProcessKind,
        allow_inferred_latches: bool,
        site: Option<&sv::ir::SourceSpan>,
        explicit_targets: &[SourceVarId],
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
        for &id in explicit_targets {
            let width = self.m.var(id).width;
            self.targets.push((id, BitAccess::new(0, width - 1)));
        }
        if self.exec_block(body)? {
            self.b.seal_block(SIRTerminator::Return);
        }
        let body_unit = seal_drained(std::mem::replace(&mut self.b, SIRBuilder::new()));
        // The analyzer promotes static locals whose entry value is observable
        // to module signals (lifetimes::keep_static_locals). Remaining hidden
        // locals are initialized per activation; they cannot retain old state.
        let mut targets: Vec<_> = self
            .targets
            .iter()
            .filter(|(id, _)| !self.m.is_hidden(*id) || explicit_targets.contains(id))
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
        if kind == sv::ir::CombProcessKind::AlwaysComb
            && super::comb_retention::retains_state(
                &body_unit,
                &targets,
                &targets
                    .iter()
                    .map(|target| (target.id, self.m.var(target.id).width))
                    .collect(),
            )
        {
            if !allow_inferred_latches {
                return Err(unsupported("latch inference inside always_comb"));
            }
            self.m.warn(crate::FrontendWarningKind::InferredLatch, site);
        }
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
        let mut sensitivity_counters = Vec::new();
        let mut counter_snapshots = Vec::new();
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
            // A source can change and return to its saved value inside one
            // initial activation. Count occurrences around stores before
            // optimization, rather than relying only on the settled value.
            let bits = celox_design::EVENT_COUNTER_WIDTH;
            let (counter, _) = self.m.temp("comb_occurrences", bits, false, false);
            let (saved_counter, _) = self.m.temp("comb_seen", bits, false, false);
            let current_counter = self.b.alloc_bit(bits, false);
            let previous_counter = self.b.alloc_bit(bits, false);
            self.b.emit(SIRInstruction::Load(
                current_counter,
                stable(counter),
                SIROffset::Static(0),
                bits,
            ));
            self.b.emit(SIRInstruction::Load(
                previous_counter,
                stable(saved_counter),
                SIROffset::Static(0),
                bits,
            ));
            let differs = self.b.alloc_bit(1, false);
            self.b.emit(SIRInstruction::Binary(
                differs,
                current_counter,
                BinaryOp::NeCase,
                previous_counter,
            ));
            let next = self.b.alloc_bit(1, false);
            self.b
                .emit(SIRInstruction::Binary(next, changed, BinaryOp::Or, differs));
            changed = next;
            self.store(saved_counter, SIROffset::Static(0), bits, current_counter);
            sensitivity_counters.push((*source, counter));
            counter_snapshots.push((counter, saved_counter));
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
        if !always_comb {
            // The event control is rearmed after its body, so occurrences
            // caused by this activation itself are already consumed.
            for &(counter, saved) in &counter_snapshots {
                let bits = celox_design::EVENT_COUNTER_WIDTH;
                let current = self.b.alloc_bit(bits, false);
                self.b.emit(SIRInstruction::Load(
                    current,
                    stable(counter),
                    SIROffset::Static(0),
                    bits,
                ));
                self.store(saved, SIROffset::Static(0), bits, current);
            }
        }
        self.jump(done);
        self.b.switch_to_block(done);
        let mut private_locals = HashMap::default();
        let unit = seal_builder(self.b).into_map_addr(&mut |addr| {
            let id = addr.var_id;
            if !self
                .m
                .variables
                .get(&id)
                .is_some_and(SvVariable::is_scratch)
                || self.m.created.contains(&id)
                || explicit_targets.contains(&id)
            {
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
                transparent_copy: false,
                target: LogicPathTarget::Process {
                    process: initialized,
                    sensitivity_counters: sensitivity_counters.clone(),
                    startup: (!always_comb).then(|| celox_slt::CombStartup {
                        initialized,
                        counters: counter_snapshots.clone(),
                        inputs: sources
                            .iter()
                            .cloned()
                            .zip(snapshots.iter().map(|&(saved, _, _)| saved))
                            .collect(),
                    }),
                    after_initials: matches!(
                        kind,
                        sv::ir::CombProcessKind::AlwaysComb | sv::ir::CombProcessKind::AlwaysLatch
                    ),
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
    ) -> Result<FfProcessUnits, sv::AnalyzerError> {
        let mut visited = HashSet::default();
        self.scan(body, &mut visited);
        if self
            .working
            .iter()
            .any(|id| self.sparse_memories.contains(id))
        {
            return Err(unsupported(
                "blocking writes to a shared edge-triggered memory",
            ));
        }
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
        // A variable the process writes both ways keeps its nonblocking
        // values apart: blocking reads see the blocking value, and the
        // nonblocking update lands after it (IEEE 1800-2023 10.4).
        let mut mixed: Vec<SourceVarId> = self
            .nonblocking
            .intersection(&self.working)
            .copied()
            .filter(|&id| !self.m.is_hidden(id) && self.m.var(id).array_dims.is_empty())
            .collect();
        mixed.sort_by_key(|id| id.0);
        let staged: Vec<NbaStage> = mixed
            .into_iter()
            .map(|id| {
                let (value, value_name) = self.m.shadow(id, "nba_value", false);
                let (mask, mask_name) = self.m.shadow(id, "nba_mask", true);
                NbaStage {
                    id,
                    name: self.m.var(id).path.join("."),
                    value,
                    value_name,
                    mask,
                    mask_name,
                }
            })
            .collect();
        let body = if staged.is_empty() {
            body.to_vec()
        } else {
            stage_nonblocking(body, &staged, self.m)
        };
        let ordinary_targets: Vec<_> = targets
            .iter()
            .copied()
            .filter(|target| !self.sparse_memories.contains(&target.id))
            .collect();
        emit_ff_seeds(&mut self.b, &ordinary_targets);
        for stage in &staged {
            let width = self.m.var(stage.mask).width;
            let zero = self.b.alloc_bit(width, false);
            self.b.emit(SIRInstruction::Imm(zero, SIRValue::new(0u8)));
            self.store(stage.mask, SIROffset::Static(0), width, zero);
        }
        if self.exec_block(&body)? {
            self.b.seal_block(SIRTerminator::Return);
        }
        let mut eval = seal_drained(self.b);
        if eval.blocks.values().flat_map(|block| &block.instructions).any(|instruction| {
            matches!(instruction, SIRInstruction::Load(_, address, ..)
                if address.region == WORKING_REGION && self.sparse_memories.contains(&address.var_id))
        }) {
            return Err(unsupported("read-modify-write destination in a shared FF memory"));
        }
        for block in eval.blocks.values_mut() {
            for instruction in &mut block.instructions {
                if let SIRInstruction::Store(address, ..) = instruction
                    && address.region == WORKING_REGION
                    && self.sparse_memories.contains(&address.var_id)
                {
                    address.region = SPARSE_WORKING_REGION;
                }
            }
        }
        let (active_targets, nba_targets): (Vec<_>, Vec<_>) = targets
            .iter()
            .copied()
            .partition(|target| self.working.contains(&target.id));
        let mut apply = SIRBuilder::new();
        emit_sparse_memory_commits(
            &mut apply,
            &targets,
            &self.sparse_memories,
            self.m.variables,
        );
        emit_nba_merges(&mut apply, &staged, self.m);
        let mut active = SIRBuilder::new();
        emit_ff_commits(&mut active, &active_targets);
        let mut nba = SIRBuilder::new();
        emit_sparse_memory_commits(
            &mut nba,
            &nba_targets,
            &self.sparse_memories,
            self.m.variables,
        );
        emit_nba_merges(&mut nba, &staged, self.m);
        Ok(FfProcessUnits {
            eval,
            apply: seal_builder(apply),
            active: seal_builder(active),
            nba: seal_builder(nba),
            targets,
        })
    }
}

/// Reuse the sparse next-state protocol used by the Veryl frontend: no whole
/// memory seed, and publication touches only the dirty ranges.
fn emit_sparse_memory_commits(
    builder: &mut SIRBuilder<Addr>,
    targets: &[VarAtomBase<SourceVarId>],
    sparse: &HashSet<SourceVarId>,
    variables: &HashMap<SourceVarId, SvVariable>,
) {
    let mut committed = HashSet::default();
    for target in targets {
        if sparse.contains(&target.id) {
            if !committed.insert(target.id) {
                continue;
            }
            // Sparse publication consumes this memory's complete dirty set,
            // including updates from other processes evaluated before commit.
            // Describe the full object, and publish it once even if this
            // process writes several disjoint static ranges.
            builder.emit(SIRInstruction::Commit(
                Addr {
                    region: SPARSE_WORKING_REGION,
                    var_id: target.id,
                },
                stable(target.id),
                SIROffset::Static(0),
                variables[&target.id].width,
                Vec::new(),
            ));
        } else {
            emit_ff_commits(builder, std::slice::from_ref(target));
        }
    }
}

/// The units of an edge-triggered process: its evaluation, the commit of
/// all its results, and that commit split into the blocking results (the
/// active region) and the nonblocking ones (the NBA region).
pub(super) struct FfProcessUnits {
    pub eval: ExecutionUnit<Addr>,
    pub apply: ExecutionUnit<Addr>,
    pub active: ExecutionUnit<Addr>,
    pub nba: ExecutionUnit<Addr>,
    pub targets: Vec<VarAtomBase<SourceVarId>>,
}

/// The staging of a variable a process writes both by blocking and by
/// nonblocking assignments: the nonblocking value and a mask of the bits
/// the nonblocking assignments wrote.
struct NbaStage {
    id: SourceVarId,
    name: String,
    value: SourceVarId,
    value_name: String,
    mask: SourceVarId,
    mask_name: String,
}

/// `stmts` with every nonblocking assignment to a staged variable writing
/// its staging value instead, and marking the written bits in its mask.
fn stage_nonblocking(
    stmts: &[sv::ir::Stmt],
    staged: &[NbaStage],
    pm: &ProcModule<'_>,
) -> Vec<sv::ir::Stmt> {
    let stage_of = |lvalue: &sv::ir::LValue| {
        staged
            .iter()
            .find(|stage| pm.id(lvalue.name()) == Some(stage.id))
    };
    let renamed = |lvalue: &sv::ir::LValue, name: &str| match lvalue {
        sv::ir::LValue::Ident(_) => sv::ir::LValue::Ident(name.to_string()),
        sv::ir::LValue::Select {
            msb,
            lsb,
            array_slice_width,
            array_slice_reversed,
            ..
        } => sv::ir::LValue::Select {
            name: name.to_string(),
            msb: msb.clone(),
            lsb: lsb.clone(),
            array_slice_width: array_slice_width.clone(),
            array_slice_reversed: *array_slice_reversed,
        },
    };
    let mark = |lvalue: &sv::ir::LValue, stage: &NbaStage| sv::ir::Stmt::Assign {
        lhs: renamed(lvalue, &stage.mask_name),
        rhs: sv::ir::Expr::Literal("'1".to_string()),
        nonblocking: true,
    };
    let mut out = Vec::with_capacity(stmts.len());
    for stmt in stmts {
        match stmt {
            sv::ir::Stmt::Assign {
                lhs,
                rhs,
                nonblocking: true,
            } if stage_of(lhs).is_some() => {
                let stage = stage_of(lhs).unwrap();
                out.push(sv::ir::Stmt::Assign {
                    lhs: renamed(lhs, &stage.value_name),
                    rhs: rhs.clone(),
                    nonblocking: true,
                });
                out.push(mark(lhs, stage));
            }
            sv::ir::Stmt::AssignConcat {
                parts,
                rhs,
                nonblocking: true,
            } if parts.iter().any(|part| stage_of(part).is_some()) => {
                out.push(sv::ir::Stmt::AssignConcat {
                    parts: parts
                        .iter()
                        .map(|part| match stage_of(part) {
                            Some(stage) => renamed(part, &stage.value_name),
                            None => part.clone(),
                        })
                        .collect(),
                    rhs: rhs.clone(),
                    nonblocking: true,
                });
                for part in parts {
                    if let Some(stage) = stage_of(part) {
                        out.push(mark(part, stage));
                    }
                }
            }
            sv::ir::Stmt::If {
                condition,
                then_body,
                else_body,
            } => out.push(sv::ir::Stmt::If {
                condition: condition.clone(),
                then_body: stage_nonblocking(then_body, staged, pm),
                else_body: stage_nonblocking(else_body, staged, pm),
            }),
            sv::ir::Stmt::Case {
                kind,
                selector,
                items,
                default,
            } => out.push(sv::ir::Stmt::Case {
                kind: *kind,
                selector: selector.clone(),
                items: items
                    .iter()
                    .map(|item| sv::ir::CaseItem {
                        labels: item.labels.clone(),
                        body: stage_nonblocking(&item.body, staged, pm),
                    })
                    .collect(),
                default: default
                    .as_ref()
                    .map(|body| stage_nonblocking(body, staged, pm)),
            }),
            sv::ir::Stmt::Loop {
                kind,
                init,
                condition,
                step,
                body,
            } => out.push(sv::ir::Stmt::Loop {
                kind: kind.clone(),
                init: init.clone(),
                condition: condition.clone(),
                step: step.clone(),
                body: stage_nonblocking(body, staged, pm),
            }),
            other => out.push(other.clone()),
        }
    }
    out
}

/// Publish the staged nonblocking values: each variable takes its staging
/// value in the bits the mask marks, after its blocking value was
/// committed.
fn emit_nba_merges(builder: &mut SIRBuilder<Addr>, staged: &[NbaStage], pm: &ProcModule<'_>) {
    for stage in staged {
        let _ = &stage.name;
        let variable = pm.var(stage.id);
        let width = variable.width;
        let current = if variable.is_4state {
            builder.alloc_logic(width)
        } else {
            builder.alloc_bit(width, false)
        };
        builder.emit(SIRInstruction::Load(
            current,
            stable(stage.id),
            SIROffset::Static(0),
            width,
        ));
        let value = if variable.is_4state {
            builder.alloc_logic(width)
        } else {
            builder.alloc_bit(width, false)
        };
        builder.emit(SIRInstruction::Load(
            value,
            working(stage.value),
            SIROffset::Static(0),
            width,
        ));
        let mask = builder.alloc_bit(width, false);
        builder.emit(SIRInstruction::Load(
            mask,
            working(stage.mask),
            SIROffset::Static(0),
            width,
        ));
        let keep = builder.alloc_bit(width, false);
        builder.emit(SIRInstruction::Unary(keep, UnaryOp::BitNot, mask));
        let kept = if variable.is_4state {
            builder.alloc_logic(width)
        } else {
            builder.alloc_bit(width, false)
        };
        builder.emit(SIRInstruction::Binary(kept, current, BinaryOp::And, keep));
        let taken = if variable.is_4state {
            builder.alloc_logic(width)
        } else {
            builder.alloc_bit(width, false)
        };
        builder.emit(SIRInstruction::Binary(taken, value, BinaryOp::And, mask));
        let merged = if variable.is_4state {
            builder.alloc_logic(width)
        } else {
            builder.alloc_bit(width, false)
        };
        builder.emit(SIRInstruction::Binary(merged, kept, BinaryOp::Or, taken));
        builder.emit(SIRInstruction::Store(
            stable(stage.id),
            SIROffset::Static(0),
            width,
            merged,
            Vec::new(),
            Vec::new(),
        ));
    }
}

impl Ff<'_, '_> {
    /// Lower an `initial` body into a process kernel that runs it from time
    /// zero, reading and writing stable state directly.
    pub fn lower_initial_kernel(
        mut self,
        body: &[sv::ir::Stmt],
        slots: ProcessSlots<SourceVarId>,
        process: usize,
    ) -> Result<KernelUnit, sv::AnalyzerError> {
        self.process = process;
        let mut kernel = ProcessKernelBuilder::new(slots);
        std::mem::swap(&mut self.b, kernel.builder());
        self.kernel = Some(kernel);
        self.exec_block(body)?;
        let mut kernel = self.kernel.take().expect("lowering a process kernel");
        std::mem::swap(&mut self.b, kernel.builder());
        let mut unit = kernel.build();
        prune_unreachable_blocks(&mut unit);
        Ok(KernelUnit {
            unit,
            wait_sites: self.wait_sites,
            nba_sites: self.nba_sites,
            host_request_sites: self.host_request_sites,
        })
    }

    /// A nonblocking assignment in a process (IEEE 1800-2023 10.4.2): the
    /// right-hand side is evaluated now, once, and each target part becomes
    /// an update the runtime queues for the NBA region.
    fn nonblocking(
        &mut self,
        parts: &[sv::ir::LValue],
        rhs: &sv::ir::Expr,
    ) -> Result<(), sv::AnalyzerError> {
        let mut targets = Vec::with_capacity(parts.len());
        for part in parts {
            targets.push(self.nba_target(part)?);
        }
        let total: usize = targets.iter().map(|(_, _, width)| width).sum();
        let four_state = targets.iter().any(|(id, _, _)| self.m.var(*id).is_4state);
        let (_, value_name) = self.m.temp("nba", total, false, four_state);
        self.assign(&sv::ir::LValue::Ident(value_name.clone()), rhs)?;
        // The parts, most significant first, take consecutive bits of the
        // value.
        let mut high = total;
        for (id, lsb, width) in targets {
            let low = high - width;
            let (staged, staged_name) =
                self.m
                    .temp("nba_part", width, false, self.m.var(id).is_4state);
            self.assign(
                &sv::ir::LValue::Ident(staged_name),
                &sv::ir::Expr::Select {
                    expr: Box::new(sv::ir::Expr::Ident(value_name.clone())),
                    msb: sv::ir::ConstExpr::Literal((high - 1).to_string()),
                    lsb: sv::ir::ConstExpr::Literal(low.to_string()),
                    signed: false,
                },
            )?;
            high = low;
            let site = NbaSite {
                process: self.process,
                target: id,
                lsb,
                width,
                value: staged,
                offset: None,
                pending: None,
            };
            let index = self.nba_sites.len() as u32;
            let staged_four_state = self.m.var(staged).is_4state;
            self.with_kernel(|kernel| {
                kernel.nonblocking(index, &site, |builder| {
                    let value = if staged_four_state {
                        builder.alloc_logic(width)
                    } else {
                        builder.alloc_bit(width, false)
                    };
                    builder.emit(SIRInstruction::Load(
                        value,
                        stable(staged),
                        SIROffset::Static(0),
                        width,
                    ));
                    Ok::<_, ProcessKernelError>((value, None))
                })
            })
            .map_err(kernel_error)?;
            self.nba_sites.push(site);
        }
        Ok(())
    }

    /// The variable and the constant bit range a nonblocking assignment in
    /// a process writes.
    fn nba_target(
        &self,
        lvalue: &sv::ir::LValue,
    ) -> Result<(SourceVarId, usize, usize), sv::AnalyzerError> {
        let id = self
            .m
            .id(lvalue.name())
            .ok_or_else(|| unsupported(format!("assignment target `{}`", lvalue.name())))?;
        let variable = self.m.var(id);
        if !variable.array_dims.is_empty() {
            return Err(unsupported(
                "nonblocking assignment to an unpacked array in a process",
            ));
        }
        match lvalue {
            sv::ir::LValue::Ident(_) => Ok((id, 0, variable.width)),
            sv::ir::LValue::Select { msb, lsb, .. } => {
                let constant = |expr| {
                    sv::typecheck::eval_const_expr_with_types(
                        expr,
                        self.m.constants,
                        self.m.parameter_types,
                    )
                };
                let (Some(msb), Some(lsb)) = (constant(msb), constant(lsb)) else {
                    return Err(unsupported(
                        "nonblocking assignment with a run-time position in a process",
                    ));
                };
                let (Some(msb), Some(lsb)) = (
                    packed_index_offset(variable, msb),
                    packed_index_offset(variable, lsb),
                ) else {
                    return Err(unsupported("nonblocking assignment outside its target"));
                };
                if msb < lsb {
                    return Err(unsupported(
                        "nonblocking assignment to a reversed part select in a process",
                    ));
                }
                Ok((id, lsb, msb - lsb + 1))
            }
        }
    }

    /// `#amount` (IEEE 1800-2023 9.4.1). The amount is self-determined and
    /// read as an unsigned 64-bit time: a negative value as its two's
    /// complement, and a value with an unknown bit as zero. `#0` resumes in
    /// the inactive region.
    fn delay(
        &mut self,
        amount: &sv::ir::Expr,
        ticks_per_unit: u64,
    ) -> Result<(), sv::AnalyzerError> {
        let signed = self.expr_signed(amount);
        let mut arena = SLTNodeArena::new();
        let node = self.expr_slt(amount, None, &mut arena)?;
        let original = self.lower_slt(&arena, node)?;
        let node = coerce_node_width(&mut arena, node, Some(PROCESS_DELAY_WIDTH), signed)
            .map_err(slt_error)?;
        let delay = self.lower_slt(&arena, node)?;
        let mut delay = self.zero_if_unknown(delay, original);
        if ticks_per_unit > 1 {
            // Check before multiplying so the 64-bit delay slot cannot wrap.
            let limit = self.b.alloc_bit(PROCESS_DELAY_WIDTH, false);
            self.b.emit(SIRInstruction::Imm(
                limit,
                SIRValue::new(u64::MAX / ticks_per_unit),
            ));
            let overflow = self.b.alloc_bit(1, false);
            self.b.emit(SIRInstruction::Binary(
                overflow,
                delay,
                BinaryOp::GtU,
                limit,
            ));
            let error = self.b.new_block();
            let scaled = self.b.new_block();
            self.b.seal_block(SIRTerminator::Branch {
                cond: overflow,
                true_block: (error, Vec::new()),
                false_block: (scaled, Vec::new()),
            });
            self.b.switch_to_block(error);
            let code = self
                .m
                .runtime_error("simulation delay overflows the simulation time range".to_string());
            self.b.seal_block(SIRTerminator::Error(code));
            self.b.switch_to_block(scaled);
            let factor = self.b.alloc_bit(PROCESS_DELAY_WIDTH, false);
            self.b
                .emit(SIRInstruction::Imm(factor, SIRValue::new(ticks_per_unit)));
            let product = self.b.alloc_bit(PROCESS_DELAY_WIDTH, false);
            self.b.emit(SIRInstruction::Binary(
                product,
                delay,
                BinaryOp::Mul,
                factor,
            ));
            delay = product;
        }
        self.with_kernel(|kernel| kernel.inactive_delay(delay))
            .map_err(kernel_error)
    }

    /// `value` as a two-state register that is zero when `source` has an
    /// unknown bit.
    fn zero_if_unknown(&mut self, value: RegisterId, source: RegisterId) -> RegisterId {
        let width = self.b.register(&value).width();
        if !matches!(self.b.register(&source), RegisterType::Logic { .. }) {
            return value;
        }
        // The reduction XOR of a value is unknown exactly when a bit is.
        let reduced = self.b.alloc_logic(1);
        self.b
            .emit(SIRInstruction::Unary(reduced, UnaryOp::Xor, source));
        let zero = self.b.alloc_bit(1, false);
        self.b.emit(SIRInstruction::Imm(zero, SIRValue::new(0u8)));
        let one = self.b.alloc_bit(1, false);
        self.b.emit(SIRInstruction::Imm(one, SIRValue::new(1u8)));
        let is_zero = self.b.alloc_bit(1, false);
        self.b.emit(SIRInstruction::Binary(
            is_zero,
            reduced,
            BinaryOp::EqCase,
            zero,
        ));
        let is_one = self.b.alloc_bit(1, false);
        self.b.emit(SIRInstruction::Binary(
            is_one,
            reduced,
            BinaryOp::EqCase,
            one,
        ));
        let known = self.b.alloc_bit(1, false);
        self.b
            .emit(SIRInstruction::Binary(known, is_zero, BinaryOp::Or, is_one));
        let value = match *self.b.register(&value) {
            RegisterType::Logic { width } => {
                let two_state = self.b.alloc_bit(width, false);
                self.b
                    .emit(SIRInstruction::Unary(two_state, UnaryOp::ToTwoState, value));
                two_state
            }
            _ => value,
        };
        let zero = self.b.alloc_bit(width, false);
        self.b.emit(SIRInstruction::Imm(zero, SIRValue::new(0u8)));
        let result = self.b.alloc_bit(width, false);
        self.b.emit(SIRInstruction::Mux(result, known, value, zero));
        result
    }

    /// `@(items)` (IEEE 1800-2023 9.4.2): the process waits for an
    /// occurrence of any item, which the runtime detects on the watched
    /// signals themselves, so nothing is left to check when it resumes.
    fn wait_event(&mut self, items: &[sv::ir::EventItem]) -> Result<(), sv::AnalyzerError> {
        let mut terms = Vec::new();
        for item in items {
            let id = match &item.expr {
                sv::ir::Expr::Ident(name) => self.m.id(name).filter(|&id| !self.m.is_hidden(id)),
                _ => None,
            }
            .ok_or_else(|| unsupported("event expression other than a variable"))?;
            match item.edge {
                sv::ir::EventEdge::Any => terms.push((id, CounterKind::Changes)),
                sv::ir::EventEdge::Pos => terms.push((id, CounterKind::Rises)),
                sv::ir::EventEdge::Neg => terms.push((id, CounterKind::Falls)),
                sv::ir::EventEdge::Both => {
                    terms.push((id, CounterKind::Rises));
                    terms.push((id, CounterKind::Falls));
                }
            }
        }
        let site = self.wait_site(terms);
        self.with_kernel(|kernel| -> Result<(), ProcessKernelError> {
            kernel.suspend_wait(site)?;
            let woken = kernel.builder().alloc_bit(1, false);
            kernel
                .builder()
                .emit(SIRInstruction::Imm(woken, SIRValue::new(1u8)));
            kernel.wake_if(woken);
            Ok(())
        })
        .map_err(kernel_error)
    }

    /// `wait (condition)` (IEEE 1800-2023 9.4.3): the process goes on at once
    /// if the condition holds, and otherwise checks it again whenever a
    /// variable it reads changes.
    fn wait_level(&mut self, condition: &sv::ir::Expr) -> Result<(), sv::AnalyzerError> {
        let mut names = HashSet::default();
        expr_idents(condition, &mut names);
        subroutine_reads(self.m, condition, &mut names);
        // Static task formals can change when another caller copies arguments
        // in, but hidden local storage has no observable change counters yet.
        // Do not compile a wait that would silently miss that wakeup, including
        // dependencies read indirectly by a function in the condition.
        if self.m.subroutines.values().any(|subroutine| {
            subroutine.is_task
                && subroutine.params.iter().any(|param| {
                    !subroutine.automatic_locals.contains(&param.name)
                        && names.iter().any(|name| {
                            self.m.id(name).is_some_and(|id| {
                                self.m.is_hidden(id) && Some(id) == self.m.id(&param.name)
                            })
                        })
                })
        }) {
            return Err(unsupported("wait condition reading a static task argument"));
        }
        let mut ids: Vec<_> = names
            .iter()
            .filter_map(|name| self.m.id(name))
            .filter(|&id| !self.m.is_hidden(id))
            .collect();
        ids.sort_unstable();
        ids.dedup();
        let site = self.wait_site(
            ids.into_iter()
                .map(|id| (id, CounterKind::Changes))
                .collect(),
        );
        // The condition is lowered once per check: before the process
        // suspends and at every resume.
        let mut error = None;
        let mut kernel = self.kernel.take().expect("lowering a process kernel");
        std::mem::swap(&mut self.b, kernel.builder());
        // The kernel is out of `self` while it builds the checks; reads go
        // to stable state all the same.
        let direct = std::mem::replace(&mut self.direct, true);
        let result = kernel.wait_level(site, |builder| {
            std::mem::swap(&mut self.b, builder);
            let truth = self.eval_truth(condition).map(|(truth, _)| truth);
            std::mem::swap(&mut self.b, builder);
            truth.unwrap_or_else(|e| {
                error = Some(e);
                let never = builder.alloc_bit(1, false);
                builder.emit(SIRInstruction::Imm(never, SIRValue::new(0u8)));
                never
            })
        });
        self.direct = direct;
        std::mem::swap(&mut self.b, kernel.builder());
        self.kernel = Some(kernel);
        result.map_err(kernel_error)?;
        error.map_or(Ok(()), Err)
    }

    /// Declare a wait site of the process with the given terms and return
    /// its local index.
    fn wait_site(&mut self, terms: Vec<(SourceVarId, CounterKind)>) -> u32 {
        let index = self.wait_sites.len() as u32;
        self.wait_sites.push(WaitSite {
            process: self.process,
            terms,
        });
        index
    }

    /// The back edge of a kernel loop that may not suspend: count the
    /// iteration, yield every 2^[`LOOP_YIELD_BITS`] of them, and continue at
    /// `header`. Returns the block the loop's back edges jump to.
    fn yielding_back_edge(&mut self, header: BlockId) -> Result<BlockId, sv::AnalyzerError> {
        let back = self.b.new_block();
        let (counter, _) = self.m.temp("loop", LOOP_YIELD_BITS, false, false);
        self.b.switch_to_block(back);
        let count = self.b.alloc_bit(LOOP_YIELD_BITS, false);
        self.b.emit(SIRInstruction::Load(
            count,
            stable(counter),
            SIROffset::Static(0),
            LOOP_YIELD_BITS,
        ));
        let one = self.b.alloc_bit(LOOP_YIELD_BITS, false);
        self.b.emit(SIRInstruction::Imm(one, SIRValue::new(1u8)));
        let next = self.b.alloc_bit(LOOP_YIELD_BITS, false);
        self.b
            .emit(SIRInstruction::Binary(next, count, BinaryOp::Add, one));
        self.store(counter, SIROffset::Static(0), LOOP_YIELD_BITS, next);
        let zero = self.b.alloc_bit(LOOP_YIELD_BITS, false);
        self.b.emit(SIRInstruction::Imm(zero, SIRValue::new(0u8)));
        let wrapped = self.b.alloc_bit(1, false);
        self.b
            .emit(SIRInstruction::Binary(wrapped, next, BinaryOp::Eq, zero));
        let yield_block = self.b.new_block();
        self.b.seal_block(SIRTerminator::Branch {
            cond: wrapped,
            true_block: (yield_block, Vec::new()),
            false_block: (header, Vec::new()),
        });
        self.b.switch_to_block(yield_block);
        self.with_kernel(ProcessKernelBuilder::yield_now)
            .map_err(kernel_error)?;
        self.jump(header);
        Ok(back)
    }
}

/// The kernel of a process and its wait and nonblocking assignment sites.
pub(super) struct KernelUnit {
    pub unit: ExecutionUnit<Addr>,
    pub wait_sites: Vec<WaitSite<SourceVarId>>,
    pub nba_sites: Vec<NbaSite<SourceVarId>>,
    pub host_request_sites: Vec<celox_design::HostRequestSite<SourceVarId>>,
}

/// Iterations, as a power of two, between the yields of a kernel loop that
/// may not suspend.
const LOOP_YIELD_BITS: usize = 16;

/// Whether every pass through `stmts` suspends the process.
fn always_suspends(stmts: &[sv::ir::Stmt]) -> bool {
    stmts.iter().any(|stmt| {
        matches!(
            stmt,
            sv::ir::Stmt::Delay { .. } | sv::ir::Stmt::WaitEvent(_)
        )
    })
}

/// The module variables the subroutines `expr` calls read, directly or
/// through further calls, besides their own formals and locals.
fn subroutine_reads(pm: &ProcModule<'_>, expr: &sv::ir::Expr, names: &mut HashSet<String>) {
    let mut pending = Vec::new();
    collect_call_names(expr, &mut pending);
    let mut visited = HashSet::default();
    while let Some(name) = pending.pop() {
        if !visited.insert(name.clone()) {
            continue;
        }
        let Some(subroutine) = pm.subroutine(&name) else {
            continue;
        };
        for param in &subroutine.params {
            if let Some(default) = &param.default {
                expr_idents(default, names);
            }
        }
        for stmt in &subroutine.body {
            let mut stmt = stmt.clone();
            stmt.visit_mut(
                &mut |expr| {
                    expr_idents(expr, names);
                    collect_call_names(expr, &mut pending);
                },
                &mut |_| {},
                &mut |_| {},
            );
            stmt.walk(&mut |stmt| {
                if let sv::ir::Stmt::Call { name, .. } = stmt {
                    pending.push(name.clone());
                }
            });
        }
    }
}

/// The names of the user subroutines `expr` calls.
fn collect_call_names(expr: &sv::ir::Expr, names: &mut Vec<String>) {
    let called = std::cell::RefCell::new(Vec::new());
    expr_calls(expr, &|callee| {
        if let Some(name) = callee.subroutine() {
            called.borrow_mut().push(name.to_string());
        }
        false
    });
    names.extend(called.into_inner());
}

fn kernel_error(error: ProcessKernelError) -> sv::AnalyzerError {
    unsupported(error.to_string())
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

fn force_reads(expr: &sv::ir::Expr, m: &ProcModule<'_>) -> sv::ir::Expr {
    if m.forceable.is_empty() {
        return expr.clone();
    }
    use sv::ir::Expr;
    let go = |expr: &Expr| force_reads(expr, m);
    match expr {
        Expr::Ident(name) => {
            if let Some(id) = m.id(name)
                && let Some((enabled, forced)) = m.force_controls(id)
            {
                Expr::Resize {
                    width: m.var(id).width,
                    signed: m.var(id).signed,
                    expr: Box::new(Expr::Mux {
                        condition: Box::new(Expr::Ident(m.var(enabled).path[0].clone())),
                        then_expr: Box::new(Expr::Ident(m.var(forced).path[0].clone())),
                        else_expr: Box::new(expr.clone()),
                    }),
                }
            } else {
                expr.clone()
            }
        }
        Expr::Literal(_) => expr.clone(),
        Expr::Select {
            expr,
            msb,
            lsb,
            signed,
        } => Expr::Select {
            expr: Box::new(go(expr)),
            msb: msb.clone(),
            lsb: lsb.clone(),
            signed: *signed,
        },
        Expr::Concat(parts) => Expr::Concat(parts.iter().map(go).collect()),
        Expr::RepeatConcat { count, parts } => Expr::RepeatConcat {
            count: count.clone(),
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
