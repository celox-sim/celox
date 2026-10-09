//! The `initial` blocks of a `#[test]` module as resumable process kernels.
//!
//! Each block becomes one kernel over the elaborated instance's stable
//! state. Expressions and assignments go through [`FfParser`] in its
//! process domain; control flow, clock and reset waits, assertions and the
//! `$tb` methods are lowered here. A loop keeps its counter and bounds in
//! state, so a wait inside its body can suspend the kernel. Random numbers
//! and component methods are host requests served through scratch state.
//! A hierarchical reference becomes a synthetic variable of the module that
//! addresses the child instance's state.

use celox_design::{
    AbsoluteAddrBase, EventLocation, HostRequest, HostValue, PROCESS_CLOCK_WIDTH,
    PROCESS_DELAY_WIDTH, PROCESS_RELEASE_WIDTH, PROCESS_STATUS_WIDTH, PortTypeKind, ProcessClock,
    ProcessRelease, ProcessSlots, RegionedAbsoluteAddrBase, RuntimeErrorInfo, RuntimeEventKind,
    STABLE_REGION, VarAtomBase, VariableMetadata,
};
use celox_frontend_core::process::{PROCESS_RESUME_WIDTH, ProcessKernelBuilder};
use celox_sir::{
    BasicBlock, BinaryOp, BlockId, ExecutionUnit, RegisterId, RegisterType, SIRInstruction,
    SIROffset, SIRTerminator, SIRValue, UnaryOp,
};
use num_bigint::BigUint;
use veryl_analyzer::ir::{
    AssertKind, AssignDestination, AssignStatement, Expression, ForBound, ForRange, Function,
    FunctionBody, HierVarRef, Module, Op, Statement, SystemFunctionCall, SystemFunctionInput,
    SystemFunctionKind, SystemFunctionOutput, TbMethod, TbMethodCall, VarId, VarIndex, VarSelect,
};
use veryl_analyzer::symbol::Affiliation;
use veryl_parser::resource_table::{self, StrId};

use crate::{
    BuildConfig, FrontendLookup, HashMap, HashSet, ParserError, SourceVarId, VariableKind,
    VerylTestbenchSource,
    lowering::{
        case::case_arm_condition_expr,
        context_width::expression_signed,
        ff::{Domain, FfParser},
        types::resolve_total_width,
    },
    symbolic::artifact::SymbolicVariable,
};
use celox_frontend_core::ScheduledRtl;

type SourceAddr = AbsoluteAddrBase<SourceVarId>;
type RegionedSourceAddr = RegionedAbsoluteAddrBase<SourceVarId>;

/// A hidden state object one block needs, in the order the lowering
/// consumes them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScratchNeed {
    pub purpose: &'static str,
    pub width: usize,
}

/// The hidden state of one `initial` block.
#[derive(Clone, Debug)]
pub struct BlockStorage {
    pub resume: SourceVarId,
    pub status: SourceVarId,
    pub delay: SourceVarId,
    pub clock: SourceVarId,
    pub release: SourceVarId,
    pub scratch: Vec<(ScratchNeed, SourceVarId)>,
    /// The block's own copy of each function-local variable, so a call
    /// that waits keeps its arguments and locals while another block calls
    /// the function.
    pub private: HashMap<VarId, SourceVarId>,
}

/// The hidden state of a module's `initial` blocks, in declaration order.
#[derive(Clone, Debug, Default)]
pub struct ModuleProcessStorage {
    pub blocks: Vec<BlockStorage>,
}

/// Declare the hidden state the module's `initial` blocks need as
/// variables after `variables`, and describe it.
pub(crate) fn declare_storage(
    ir: &Module,
    ids: &HashMap<VarId, SourceVarId>,
    variables: &mut HashMap<SourceVarId, SymbolicVariable>,
) -> Result<ModuleProcessStorage, ParserError> {
    let mut function_locals: Vec<VarId> = ir
        .variables
        .iter()
        .filter(|(_, variable)| variable.affiliation == Affiliation::Function)
        .map(|(id, _)| *id)
        .collect();
    function_locals.sort_unstable();
    let templates = function_locals
        .iter()
        .map(|&local| (local, variables[&ids[&local]].clone()))
        .collect::<Vec<_>>();
    let mut next = variables.len() as u32;
    let mut declare = |path: Vec<String>, template: Option<&SymbolicVariable>, width: usize| {
        let id = SourceVarId(next);
        next += 1;
        let variable = match template {
            Some(template) => SymbolicVariable {
                path,
                source: None,
                module_affiliated: false,
                ..template.clone()
            },
            None => SymbolicVariable {
                path,
                kind: VariableKind::Variable,
                signed: false,
                metadata: VariableMetadata {
                    width,
                    is_4state: false,
                    kind: celox_design::DomainKind::Other,
                    type_kind: PortTypeKind::Bit,
                    array_dims: Vec::new(),
                },
                packed_dims: vec![width],
                source: None,
                // Hidden state is not a signal of the design.
                module_affiliated: false,
            },
        };
        variables.insert(id, variable);
        id
    };
    let mut blocks = Vec::new();
    for (index, statements) in initial_blocks(ir).enumerate() {
        let name = |slot: &str| vec![format!("$initial[{index}]"), slot.to_string()];
        let needs = scratch_needs(ir, statements)?;
        let scratch = needs
            .into_iter()
            .enumerate()
            .map(|(slot, need)| {
                let id = declare(name(&format!("{}@{slot}", need.purpose)), None, need.width);
                (need, id)
            })
            .collect();
        let private = templates
            .iter()
            .map(|(local, template)| {
                let path = name(&format!("{}@private", template.path.join(".")));
                (*local, declare(path, Some(template), 0))
            })
            .collect();
        blocks.push(BlockStorage {
            resume: declare(name("resume"), None, PROCESS_RESUME_WIDTH),
            status: declare(name("status"), None, PROCESS_STATUS_WIDTH),
            delay: declare(name("delay"), None, PROCESS_DELAY_WIDTH),
            clock: declare(name("clock"), None, PROCESS_CLOCK_WIDTH),
            release: declare(name("release"), None, PROCESS_RELEASE_WIDTH),
            scratch,
            private,
        });
    }
    Ok(ModuleProcessStorage { blocks })
}

fn initial_blocks(ir: &Module) -> impl Iterator<Item = &[Statement]> {
    ir.declarations
        .iter()
        .filter_map(|declaration| match declaration {
            veryl_analyzer::ir::Declaration::Initial(initial) => {
                Some(initial.statements.as_slice())
            }
            _ => None,
        })
}

/// Whether a range with constant bounds has no value.
fn constant_empty_range(range: &ForRange) -> bool {
    let constant = |bound: &ForBound| match bound {
        ForBound::Const(value, _) => Some(*value),
        ForBound::Expression(_) => None,
    };
    match range {
        ForRange::Forward {
            start,
            end,
            inclusive,
            ..
        }
        | ForRange::Stepped {
            start,
            end,
            inclusive,
            ..
        } => match (constant(start), constant(end)) {
            (Some(start), Some(end)) => {
                if *inclusive {
                    start > end
                } else {
                    start >= end
                }
            }
            _ => false,
        },
        ForRange::Reverse {
            start,
            end,
            inclusive,
            ..
        } => match (constant(start), constant(end)) {
            (Some(start), Some(end)) => {
                if *inclusive {
                    end < start
                } else {
                    end <= start
                }
            }
            _ => false,
        },
    }
}

/// How a `for` loop compares and steps, following the testbench
/// interpreter: the counter is kept at its own width, bounds compare in
/// the counter's signedness only when the bound is signed too, and a
/// step that does not move the counter is an error.
struct LoopShape<'s> {
    start: &'s ForBound,
    end: &'s ForBound,
    inclusive: bool,
    reverse: bool,
    step: usize,
    op: Option<Op>,
    var_width: usize,
    var_signed: bool,
    /// The width the bounds and the counter compare in.
    width: usize,
    /// Whether the comparison with the bound the loop runs towards is signed.
    cmp_signed: bool,
    start_signed: bool,
    end_signed: bool,
    /// Whether an unsigned bound may hold a value above 64 bits, which the
    /// interpreter only runs as a single iteration.
    start_wide: bool,
    end_wide: bool,
}

impl LoopShape<'_> {
    fn wide_capable(&self) -> bool {
        self.start_wide || self.end_wide
    }
}

fn loop_shape<'s>(
    ir: &Module,
    stmt: &'s veryl_analyzer::ir::ForStatement,
) -> Result<LoopShape<'s>, ParserError> {
    let var_width = resolve_total_width(ir, &ir.variables[&stmt.var_id])?.max(1);
    let var_signed = stmt.var_type.signed;
    let bound_width = |bound: &ForBound| match bound {
        ForBound::Const(..) => 64,
        ForBound::Expression(expr) => expr.comptime().r#type.total_width().unwrap_or(64),
    };
    let bound_signed = |bound: &ForBound| match bound {
        ForBound::Const(_, signed) => *signed,
        ForBound::Expression(expr) => expression_signed(expr),
    };
    let (start, end, inclusive, step, reverse, op) = match &stmt.range {
        ForRange::Forward {
            start,
            end,
            inclusive,
            step,
        } => (start, end, *inclusive, *step, false, None),
        ForRange::Reverse {
            start,
            end,
            inclusive,
            step,
        } => (start, end, *inclusive, *step, true, None),
        ForRange::Stepped {
            start,
            end,
            inclusive,
            step,
            op,
        } => (start, end, *inclusive, *step, false, Some(*op)),
    };
    let start_signed = bound_signed(start);
    let end_signed = bound_signed(end);
    let continuation_signed = if reverse { start_signed } else { end_signed };
    Ok(LoopShape {
        start,
        end,
        inclusive,
        reverse,
        step,
        op,
        var_width,
        var_signed,
        width: var_width
            .max(bound_width(start))
            .max(bound_width(end))
            .max(64)
            + 1,
        cmp_signed: var_signed && continuation_signed,
        start_signed,
        end_signed,
        start_wide: !start_signed && bound_width(start) > 64,
        end_wide: !end_signed && bound_width(end) > 64,
    })
}

/// The width of a value passed to a component method: the width its
/// context gives it, else the width the expression evaluates in.
fn argument_width(input: &SystemFunctionInput) -> usize {
    let expr = &input.0;
    let context = expr.comptime().expr_context.width;
    if context > 0 {
        return context;
    }
    crate::context_width::get_expr_width(expr)
        .or_else(|| expr.comptime().r#type.total_width())
        .or_else(|| expr.comptime().get_value().ok().map(|value| value.width()))
        .unwrap_or(64)
        .max(1)
}

fn function_body(
    functions: &HashMap<VarId, Function>,
    call: &veryl_analyzer::ir::FunctionCall,
) -> Option<FunctionBody> {
    let function = functions.get(&call.id)?;
    match &call.index {
        Some(index) => function.get_function(index),
        None => function.get_function(&[]),
    }
}

/// The scratch state the statements need, in lowering order.
fn scratch_needs(ir: &Module, statements: &[Statement]) -> Result<Vec<ScratchNeed>, ParserError> {
    let mut needs = Vec::new();
    let mut active = HashSet::default();
    collect_scratch_needs(ir, statements, &mut active, &mut needs)?;
    Ok(needs)
}

fn collect_scratch_needs(
    ir: &Module,
    statements: &[Statement],
    active: &mut HashSet<VarId>,
    needs: &mut Vec<ScratchNeed>,
) -> Result<(), ParserError> {
    let need = |needs: &mut Vec<ScratchNeed>, purpose: &'static str, width: usize| {
        needs.push(ScratchNeed { purpose, width });
    };
    for statement in statements {
        match statement {
            Statement::If(stmt) => {
                collect_scratch_needs(ir, &stmt.true_side, active, needs)?;
                collect_scratch_needs(ir, &stmt.false_side, active, needs)?;
            }
            Statement::IfReset(stmt) => {
                collect_scratch_needs(ir, &stmt.true_side, active, needs)?;
                collect_scratch_needs(ir, &stmt.false_side, active, needs)?;
            }
            Statement::Case(stmt) => {
                for arm in &stmt.arms {
                    collect_scratch_needs(ir, &arm.body, active, needs)?;
                }
                collect_scratch_needs(ir, &stmt.default, active, needs)?;
            }
            Statement::For(stmt) => {
                let shape = loop_shape(ir, stmt)?;
                need(needs, "for_start", shape.width);
                need(needs, "for_end", shape.width);
                if shape.wide_capable() {
                    need(needs, "for_single", 1);
                }
                collect_scratch_needs(ir, &stmt.body, active, needs)?;
            }
            Statement::FunctionCall(call) => {
                if active.insert(call.id)
                    && let Some(body) = function_body(&ir.functions, call)
                {
                    collect_scratch_needs(ir, &body.statements, active, needs)?;
                    active.remove(&call.id);
                }
            }
            Statement::TbMethodCall(tb) => match &tb.method {
                TbMethod::RandomSeed { .. } => need(needs, "random_seed", 64),
                TbMethod::RandomGet { width, .. } => {
                    need(needs, "random_value", (*width as usize).max(1));
                }
                TbMethod::RandomGetRange { width, .. } => {
                    need(needs, "random_min", 64);
                    need(needs, "random_max", 64);
                    need(needs, "random_value", (*width as usize).max(1));
                }
                TbMethod::RandomGetSeed => need(needs, "random_seed", 64),
                TbMethod::Component { args, .. } => {
                    for arg in args {
                        need(needs, "component_arg", argument_width(arg));
                    }
                    if tb.ret.is_some() {
                        need(
                            needs,
                            "component_result",
                            tb.ret_width.map_or(64, |w| w as usize).max(1),
                        );
                    }
                }
                TbMethod::ClockNext { .. }
                | TbMethod::ResetAssert { .. }
                | TbMethod::FileOpen { .. }
                | TbMethod::FileWrite { .. }
                | TbMethod::FileClose
                | TbMethod::FileFlush => {}
            },
            Statement::Assign(_)
            | Statement::SystemFunctionCall(_)
            | Statement::Break
            | Statement::Unsupported(_)
            | Statement::Null => {}
        }
    }
    Ok(())
}

/// Compile the `initial` blocks of every instance in `testbench` into
/// kernels appended to `scheduled`.
pub(crate) fn lower_testbench_kernels(
    scheduled: &mut ScheduledRtl,
    testbench: &VerylTestbenchSource,
    module_ir: &HashMap<celox_design::ModuleId, &Module>,
    storage: &HashMap<celox_design::ModuleId, ModuleProcessStorage>,
    config: &BuildConfig,
) -> Result<(), ParserError> {
    for source in testbench.sources() {
        if source.initial_statements.is_none() {
            continue;
        }
        let instance = source.base_instance(&scheduled.frontend_lookup);
        let module_id = scheduled.frontend_lookup.instance_module[&instance];
        let ir = module_ir[&module_id];
        let storage = storage.get(&module_id).ok_or_else(|| {
            ParserError::illegal_context(
                "testbench process lowering",
                "the module's process storage was not declared",
                None,
            )
        })?;
        for (index, statements) in source.initial_blocks().into_iter().enumerate() {
            let block = storage.blocks.get(index).ok_or_else(|| {
                ParserError::illegal_context(
                    "testbench process lowering",
                    format!("initial block {index} has no process storage"),
                    None,
                )
            })?;
            lower_block(
                scheduled, testbench, source, module_ir, instance, module_id, ir, block,
                statements, config,
            )?;
        }
    }
    Ok(())
}

enum Flow {
    /// The block is open; the next statement follows.
    Continue,
    /// Control left the block.
    Ended,
}

struct BlockLowering<'a> {
    parser: FfParser<'a>,
    kernel: ProcessKernelBuilder<RegionedSourceAddr>,
    ir: &'a Module,
    ids: &'a HashMap<VarId, SourceVarId>,
    lookup: &'a FrontendLookup,
    instance: celox_design::InstanceId,
    module_id: celox_design::ModuleId,
    hierarchical: HashMap<VarId, SourceAddr>,
    private: HashMap<VarId, SourceVarId>,
    scratch: std::vec::IntoIter<(ScratchNeed, SourceVarId)>,
    clocks: Vec<(StrId, ProcessClock<SourceAddr>)>,
    releases: Vec<ProcessRelease<SourceAddr>>,
    host_requests: Vec<HostRequest<SourceAddr>>,
    clock_periods: &'a HashMap<StrId, u64>,
    resource_prefix: String,
    active_functions: HashSet<VarId>,
    site_base: u32,
    targets: Vec<VarAtomBase<RegionedSourceAddr>>,
    sources: Vec<VarAtomBase<RegionedSourceAddr>>,
    /// The process stored to design state since the combinational logic
    /// was last settled, so a statement that reads design state settles
    /// it first.
    dirty: bool,
}

#[allow(clippy::too_many_arguments)]
fn lower_block(
    scheduled: &mut ScheduledRtl,
    testbench: &VerylTestbenchSource,
    source: &VerylTestbenchSource,
    module_ir: &HashMap<celox_design::ModuleId, &Module>,
    instance: celox_design::InstanceId,
    module_id: celox_design::ModuleId,
    ir: &Module,
    storage: &BlockStorage,
    statements: &[Statement],
    config: &BuildConfig,
) -> Result<(), ParserError> {
    let lookup = &scheduled.frontend_lookup;
    let ids = &testbench.id_map.module_variables[&module_id];
    // Hierarchical references become synthetic variables of a copy of the
    // module, so the expression lowering treats them like local ones.
    let mut module = ir.clone();
    let mut hierarchical_vars = HashMap::default();
    let mut hierarchical = HashMap::default();
    let mut next_synthetic = 0x4000_0000u32;
    let mut references = Vec::new();
    collect_hierarchical_references(
        statements,
        &ir.functions,
        &mut HashSet::default(),
        &mut references,
    );
    for reference in references {
        let key = (reference.inst_path.clone(), reference.var_path.clone());
        if hierarchical_vars.contains_key(&key) {
            continue;
        }
        let resolved = resolve_hierarchical(lookup, module_ir, testbench, instance, &reference)?;
        let id = VarId::from_raw(next_synthetic);
        next_synthetic += 1;
        let mut variable = resolved.variable;
        variable.id = id;
        module.variables.insert(id, variable);
        hierarchical_vars.insert(key, id);
        hierarchical.insert(id, resolved.target);
    }
    let site_base = scheduled.runtime_schema.runtime_event_sites.len() as u32;
    let parser = FfParser::new(&module, *config)
        .with_hierarchical_vars(hierarchical_vars)
        .with_runtime_event_site_base(site_base);
    let stable = |var_id: SourceVarId| RegionedSourceAddr {
        region: STABLE_REGION,
        instance_id: instance,
        var_id,
    };
    let kernel = ProcessKernelBuilder::with_addresses(ProcessSlots::new(
        stable(storage.resume),
        stable(storage.status),
        stable(storage.delay),
        stable(storage.clock),
        stable(storage.release),
    ));
    let resource_prefix = lookup
        .instance_ids
        .iter()
        .find_map(|(path, &id)| {
            (id == instance).then(|| lookup.instance_path_segments(path).join("."))
        })
        .unwrap_or_default();
    let mut lowering = BlockLowering {
        parser,
        kernel,
        ir: &module,
        ids,
        lookup,
        instance,
        module_id,
        hierarchical,
        private: storage.private.clone(),
        scratch: storage.scratch.clone().into_iter(),
        clocks: Vec::new(),
        releases: Vec::new(),
        host_requests: Vec::new(),
        clock_periods: &source.clock_periods,
        resource_prefix,
        active_functions: HashSet::default(),
        site_base,
        targets: Vec::new(),
        sources: Vec::new(),
        dirty: false,
    };
    lowering.declare_clocks(statements);
    lowering.lower_statements(statements)?;
    let BlockLowering {
        parser,
        kernel,
        clocks,
        releases,
        host_requests,
        ..
    } = lowering;
    let mut unit = kernel.build();
    prune_unreachable_blocks(&mut unit);

    // Runtime errors: the parser numbers them locally from 2000.
    let next_code = scheduled
        .runtime_schema
        .runtime_errors
        .keys()
        .copied()
        .max()
        .map_or(1, |code| code + 1);
    let mut codes = HashMap::default();
    let mut errors = parser.runtime_errors().iter().collect::<Vec<_>>();
    errors.sort_by_key(|(code, _)| **code);
    for (offset, (&local, info)) in errors.into_iter().enumerate() {
        let global = next_code + offset as i64;
        codes.insert(local, global);
        scheduled.runtime_schema.runtime_errors.insert(
            global,
            RuntimeErrorInfo {
                message: info.message.clone(),
                signals: info
                    .signals
                    .iter()
                    .filter_map(|var| {
                        let var_id = *ids.get(var)?;
                        lookup.state_address(&SourceAddr {
                            instance_id: instance,
                            var_id,
                        })
                    })
                    .collect(),
            },
        );
    }
    for site in parser.runtime_event_sites() {
        scheduled
            .runtime_schema
            .runtime_event_sites
            .push(site.clone());
    }
    let to_state = |address: &SourceAddr| {
        lookup.state_address(address).ok_or_else(|| {
            ParserError::illegal_context(
                "testbench process lowering",
                format!("{address:?} has no state object"),
                None,
            )
        })
    };
    let unit = map_unit(&unit, &codes, |address| {
        Ok(RegionedAbsoluteAddrBase {
            region: address.region,
            instance_id: to_state(&SourceAddr {
                instance_id: address.instance_id,
                var_id: address.var_id,
            })?
            .instance_id,
            var_id: to_state(&SourceAddr {
                instance_id: address.instance_id,
                var_id: address.var_id,
            })?
            .var_id,
        })
    })?;
    let slots = ProcessSlots {
        resume: stable(storage.resume).into_absolute(),
        status: stable(storage.status).into_absolute(),
        delay: stable(storage.delay).into_absolute(),
        clock: stable(storage.clock).into_absolute(),
        release: stable(storage.release).into_absolute(),
        clocks: clocks.into_iter().map(|(_, clock)| clock).collect(),
        releases,
        host_requests,
    };
    let mut failure = None;
    let slots = slots.map(|address| match to_state(&address) {
        Ok(state) => state,
        Err(error) => {
            failure.get_or_insert(error);
            celox_design::StateAddr {
                instance_id: address.instance_id,
                var_id: celox_design::StateObjectId::from_raw(0),
            }
        }
    });
    if let Some(error) = failure {
        return Err(error);
    }
    scheduled.sir.processes.push(unit);
    scheduled.runtime_schema.processes.push(slots);
    Ok(())
}

trait IntoAbsolute {
    fn into_absolute(self) -> SourceAddr;
}

impl IntoAbsolute for RegionedSourceAddr {
    fn into_absolute(self) -> SourceAddr {
        SourceAddr {
            instance_id: self.instance_id,
            var_id: self.var_id,
        }
    }
}

struct ResolvedHierarchical {
    variable: veryl_analyzer::ir::Variable,
    target: SourceAddr,
}

/// The child instance's variable a hierarchical reference names.
fn resolve_hierarchical(
    lookup: &FrontendLookup,
    module_ir: &HashMap<celox_design::ModuleId, &Module>,
    testbench: &VerylTestbenchSource,
    base_instance: celox_design::InstanceId,
    reference: &HierVarRef,
) -> Result<ResolvedHierarchical, ParserError> {
    let (state, _) =
        crate::testbench::resolve_hierarchical_reference_from(lookup, base_instance, reference)?;
    let source = lookup.state_to_source.get(&state).ok_or_else(|| {
        ParserError::illegal_context(
            "hierarchical variable reference",
            format!("`{}` has no source variable", reference.var_path),
            Some(&reference.comptime.token),
        )
    })?;
    let module_id = lookup.instance_module[&source.instance_id];
    let ids = &testbench.id_map.module_variables[&module_id];
    let ir = module_ir[&module_id];
    let variable = ir
        .variables
        .iter()
        .find(|(id, _)| ids.get(id) == Some(&source.var_id))
        .map(|(_, variable)| variable.clone())
        .ok_or_else(|| {
            ParserError::illegal_context(
                "hierarchical variable reference",
                format!("`{}` is not a variable of the instance", reference.var_path),
                Some(&reference.comptime.token),
            )
        })?;
    Ok(ResolvedHierarchical {
        variable,
        target: *source,
    })
}

/// Every hierarchical reference the statements make, reads and writes.
fn collect_hierarchical_references(
    statements: &[Statement],
    functions: &HashMap<VarId, Function>,
    active: &mut HashSet<VarId>,
    out: &mut Vec<HierVarRef>,
) {
    let mut reads = Vec::new();
    crate::testbench::collect_statement_reads(
        statements,
        functions,
        &mut HashSet::default(),
        &mut reads,
    );
    for read in reads {
        if let crate::testbench::TestbenchRead::Hierarchical(reference) = read {
            out.push(*reference);
        }
    }
    for statement in statements {
        match statement {
            Statement::Assign(assign) => {
                if let Some(destination) = &assign.hier_dst {
                    out.push(crate::testbench::hierarchical_destination_reference(
                        destination,
                    ));
                }
            }
            Statement::SystemFunctionCall(call) => {
                if let SystemFunctionKind::Readmemh(_, SystemFunctionOutput::Hier(reference)) =
                    &call.kind
                {
                    out.push((**reference).clone());
                }
            }
            Statement::If(stmt) => {
                collect_hierarchical_references(&stmt.true_side, functions, active, out);
                collect_hierarchical_references(&stmt.false_side, functions, active, out);
            }
            Statement::IfReset(stmt) => {
                collect_hierarchical_references(&stmt.true_side, functions, active, out);
                collect_hierarchical_references(&stmt.false_side, functions, active, out);
            }
            Statement::Case(stmt) => {
                for arm in &stmt.arms {
                    collect_hierarchical_references(&arm.body, functions, active, out);
                }
                collect_hierarchical_references(&stmt.default, functions, active, out);
            }
            Statement::For(stmt) => {
                collect_hierarchical_references(&stmt.body, functions, active, out);
            }
            Statement::FunctionCall(call) => {
                if active.insert(call.id)
                    && let Some(body) = function_body(functions, call)
                {
                    collect_hierarchical_references(&body.statements, functions, active, out);
                    active.remove(&call.id);
                }
            }
            Statement::TbMethodCall(_)
            | Statement::Break
            | Statement::Unsupported(_)
            | Statement::Null => {}
        }
    }
}

/// The address of a module variable, or of the child state a synthetic
/// variable stands for. A process reads and writes stable state only.
fn convert_fn<'b>(
    instance: celox_design::InstanceId,
    ids: &'b HashMap<VarId, SourceVarId>,
    hierarchical: &'b HashMap<VarId, SourceAddr>,
    private: &'b HashMap<VarId, SourceVarId>,
) -> impl Fn(VarId, u32) -> RegionedSourceAddr + 'b {
    move |var_id, _region| {
        let target = hierarchical
            .get(&var_id)
            .copied()
            .unwrap_or_else(|| SourceAddr {
                instance_id: instance,
                var_id: private
                    .get(&var_id)
                    .copied()
                    .unwrap_or_else(|| ids[&var_id]),
            });
        RegionedSourceAddr {
            region: STABLE_REGION,
            instance_id: target.instance_id,
            var_id: target.var_id,
        }
    }
}

/// The parser, kernel and the operand stacks with the address mapping.
macro_rules! parts {
    ($self:ident) => {{
        let BlockLowering {
            parser,
            kernel,
            targets,
            sources,
            ids,
            hierarchical,
            private,
            instance,
            ..
        } = $self;
        let convert = convert_fn(*instance, ids, hierarchical, private);
        (parser, kernel, targets, sources, convert)
    }};
}

impl<'a> BlockLowering<'a> {
    fn scratch_addr(&self, id: SourceVarId) -> RegionedSourceAddr {
        RegionedSourceAddr {
            region: STABLE_REGION,
            instance_id: self.instance,
            var_id: id,
        }
    }

    fn take_scratch(&mut self, purpose: &'static str) -> Result<(usize, SourceVarId), ParserError> {
        match self.scratch.next() {
            Some((need, id)) if need.purpose == purpose => Ok((need.width, id)),
            other => Err(ParserError::internal(
                "testbench process lowering",
                format!("scratch for {purpose} expected, found {other:?}"),
                None,
            )),
        }
    }

    fn error(
        &self,
        what: &'static str,
        detail: impl Into<String>,
        token: Option<&veryl_parser::token_range::TokenRange>,
    ) -> ParserError {
        ParserError::illegal_context(what, detail.into(), token)
    }

    /// The module variable named by a `$tb` instance.
    fn instance_var(&self, inst: StrId) -> Result<VarId, ParserError> {
        self.ir
            .variables
            .iter()
            .find(|(_, variable)| variable.path.0.as_slice() == [inst])
            .map(|(id, _)| *id)
            .ok_or_else(|| {
                self.error(
                    "testbench method call",
                    format!("`{}` is not a variable of the module", source_name(inst)),
                    None,
                )
            })
    }

    fn resource_name(&self, name: StrId) -> String {
        let local = source_name(name);
        if self.resource_prefix.is_empty() {
            local
        } else {
            format!("{}.{local}", self.resource_prefix)
        }
    }

    /// The clocks the block waits on, in first-use order, with their
    /// periods.
    fn declare_clocks(&mut self, statements: &[Statement]) {
        let mut active = HashSet::default();
        self.scan_clocks(statements, &mut active);
    }

    fn scan_clocks(&mut self, statements: &[Statement], active: &mut HashSet<VarId>) {
        for statement in statements {
            match statement {
                Statement::TbMethodCall(tb) => match &tb.method {
                    TbMethod::ClockNext { period, .. } => {
                        let period = self
                            .clock_periods
                            .get(&tb.inst)
                            .copied()
                            .or_else(|| period.as_deref().and_then(try_eval_const))
                            .unwrap_or(2)
                            .max(2);
                        self.declare_clock(tb.inst, period);
                    }
                    TbMethod::ResetAssert { clock, .. } => {
                        let period = self.clock_periods.get(clock).copied().unwrap_or(2).max(2);
                        self.declare_clock(*clock, period);
                    }
                    _ => {}
                },
                Statement::If(stmt) => {
                    self.scan_clocks(&stmt.true_side, active);
                    self.scan_clocks(&stmt.false_side, active);
                }
                Statement::IfReset(stmt) => {
                    self.scan_clocks(&stmt.true_side, active);
                    self.scan_clocks(&stmt.false_side, active);
                }
                Statement::Case(stmt) => {
                    for arm in &stmt.arms {
                        self.scan_clocks(&arm.body, active);
                    }
                    self.scan_clocks(&stmt.default, active);
                }
                Statement::For(stmt) => self.scan_clocks(&stmt.body, active),
                Statement::FunctionCall(call) => {
                    if active.insert(call.id)
                        && let Some(body) = function_body(&self.ir.functions, call)
                    {
                        self.scan_clocks(&body.statements, active);
                        active.remove(&call.id);
                    }
                }
                _ => {}
            }
        }
    }

    fn declare_clock(&mut self, inst: StrId, period: u64) {
        if self.clocks.iter().any(|(name, _)| *name == inst) {
            return;
        }
        let Ok(var) = self.instance_var(inst) else {
            return;
        };
        let signal = SourceAddr {
            instance_id: self.instance,
            var_id: self.ids[&var],
        };
        self.clocks.push((inst, ProcessClock { signal, period }));
        let index = self.kernel.add_clock();
        debug_assert_eq!(index as usize + 1, self.clocks.len());
    }

    fn clock_index(&self, inst: StrId) -> Result<u32, ParserError> {
        self.clocks
            .iter()
            .position(|(name, _)| *name == inst)
            .map(|index| index as u32)
            .ok_or_else(|| {
                self.error(
                    "testbench method call",
                    format!("`{}` is not a clock of the module", source_name(inst)),
                    None,
                )
            })
    }

    // ------------------------------------------------------------ helpers

    fn expression(&mut self, expr: &Expression) -> Result<RegisterId, ParserError> {
        let (parser, kernel, targets, sources, convert) = parts!(self);
        parser.parse_expression(
            expr,
            targets,
            &Domain::Process,
            &convert,
            sources,
            kernel.builder(),
            None,
        )?;
        Ok(parser.pop_value())
    }

    fn condition(&mut self, expr: &Expression) -> Result<RegisterId, ParserError> {
        let value = self.expression(expr)?;
        Ok(self
            .parser
            .lower_procedural_condition(value, self.kernel.builder()))
    }

    /// `value` as an unsigned two-state register of `width` bits.
    fn unsigned_bits(&mut self, value: RegisterId, width: usize) -> RegisterId {
        let builder = self.kernel.builder();
        let value = match *builder.register(&value) {
            RegisterType::Logic { width } => {
                let two_state = builder.alloc_bit(width, false);
                builder.emit(SIRInstruction::Unary(two_state, UnaryOp::ToTwoState, value));
                two_state
            }
            RegisterType::Bit { .. } => value,
        };
        let signed = matches!(
            builder.register(&value),
            RegisterType::Bit { signed: true, .. }
        );
        let cast = self
            .parser
            .cast_reg_width_ext(self.kernel.builder(), value, width, signed);
        let builder = self.kernel.builder();
        match *builder.register(&cast) {
            RegisterType::Bit {
                width: w,
                signed: false,
            } if w == width => cast,
            _ => {
                let unsigned = builder.alloc_bit(width, false);
                builder.emit(SIRInstruction::Unary(unsigned, UnaryOp::Ident, cast));
                unsigned
            }
        }
    }

    fn constant(&mut self, value: u64, width: usize) -> RegisterId {
        let builder = self.kernel.builder();
        let reg = builder.alloc_bit(width, false);
        builder.emit(SIRInstruction::Imm(reg, SIRValue::new(value)));
        reg
    }

    fn store_scratch(&mut self, id: SourceVarId, width: usize, value: RegisterId) {
        let address = self.scratch_addr(id);
        self.kernel.builder().emit(SIRInstruction::Store(
            address,
            SIROffset::Static(0),
            width,
            value,
            Vec::new(),
            Vec::new(),
        ));
    }

    fn load_scratch(&mut self, id: SourceVarId, width: usize, signed: bool) -> RegisterId {
        let address = self.scratch_addr(id);
        let builder = self.kernel.builder();
        let reg = builder.alloc_bit(width, signed);
        builder.emit(SIRInstruction::Load(
            reg,
            address,
            SIROffset::Static(0),
            width,
        ));
        reg
    }

    fn destination(
        &self,
        var_id: VarId,
        token: veryl_parser::token_range::TokenRange,
    ) -> AssignDestination {
        AssignDestination {
            id: var_id,
            path: self.ir.variables[&var_id].path.clone(),
            index: VarIndex::default(),
            select: VarSelect::default(),
            comptime: Default::default(),
            token,
        }
    }

    fn store(
        &mut self,
        destination: &AssignDestination,
        value: RegisterId,
    ) -> Result<(), ParserError> {
        if !self.private.contains_key(&destination.id) {
            self.dirty = true;
        }
        let (parser, kernel, targets, sources, convert) = parts!(self);
        parser.push_value(value);
        parser.op_store(
            destination,
            targets,
            &Domain::Process,
            &convert,
            sources,
            kernel.builder(),
        )
    }

    /// Settle the combinational logic before a read of design state that
    /// follows a store, as the statements of a testbench are sequential.
    fn settle_if_dirty(&mut self) -> Result<(), ParserError> {
        if !self.dirty {
            return Ok(());
        }
        self.kernel
            .settle()
            .map_err(|error| self.error("testbench process lowering", error.to_string(), None))?;
        self.suspended();
        Ok(())
    }

    fn load_var(&mut self, var_id: VarId) -> Result<RegisterId, ParserError> {
        let (parser, kernel, _targets, sources, convert) = parts!(self);
        parser.op_load(
            var_id,
            &VarIndex::default(),
            &VarSelect::default(),
            &Domain::Process,
            &convert,
            sources,
            kernel.builder(),
        )?;
        Ok(parser.pop_value())
    }

    /// The process gave control to the runtime: registers and the settled
    /// state are fresh when it resumes.
    fn suspended(&mut self) {
        self.parser.clear_register_cache();
        self.dirty = false;
    }

    fn host_scratch(
        &mut self,
        purpose: &'static str,
        signed: bool,
        is_string: bool,
    ) -> Result<(HostValue<SourceAddr>, SourceVarId), ParserError> {
        let (width, id) = self.take_scratch(purpose)?;
        Ok((
            HostValue {
                signal: SourceAddr {
                    instance_id: self.instance,
                    var_id: id,
                },
                width,
                signed,
                is_string,
            },
            id,
        ))
    }

    fn host_request(&mut self, request: HostRequest<SourceAddr>) -> Result<(), ParserError> {
        self.host_requests.push(request);
        let index = self.kernel.add_host_request();
        debug_assert_eq!(index as usize + 1, self.host_requests.len());
        self.kernel
            .host(index)
            .map_err(|error| self.error("testbench process lowering", error.to_string(), None))?;
        self.suspended();
        Ok(())
    }

    // --------------------------------------------------------- statements

    fn lower_statements(&mut self, statements: &[Statement]) -> Result<Flow, ParserError> {
        for statement in statements {
            if let Flow::Ended = self.lower_statement(statement)? {
                return Ok(Flow::Ended);
            }
        }
        Ok(Flow::Continue)
    }

    fn lower_statement(&mut self, statement: &Statement) -> Result<Flow, ParserError> {
        let reads = match statement {
            Statement::Null | Statement::Break => false,
            Statement::SystemFunctionCall(call) => !matches!(call.kind, SystemFunctionKind::Finish),
            _ => true,
        };
        if reads {
            self.settle_if_dirty()?;
        }
        match statement {
            Statement::Assign(assign) => {
                self.lower_assign(assign)?;
                self.dirty = true;
                Ok(Flow::Continue)
            }
            Statement::If(stmt) => {
                // A constant condition keeps the other side out of the
                // kernel, as the interpreter never ran it.
                match FfParser::get_constant_procedural_truth(&stmt.cond) {
                    Some(true) => self.lower_statements(&stmt.true_side),
                    Some(false) => self.lower_statements(&stmt.false_side),
                    None => {
                        let condition = self.condition(&stmt.cond)?;
                        self.branch(condition, &stmt.true_side, &stmt.false_side)
                    }
                }
            }
            Statement::IfReset(stmt) => Err(self.error(
                "statement in an initial block",
                "if_reset",
                Some(&stmt.token),
            )),
            Statement::Case(stmt) => self.lower_case(stmt),
            Statement::For(stmt) => {
                if constant_empty_range(&stmt.range) {
                    // The body never runs; its `$readmemh` files need not exist.
                    return Ok(Flow::Continue);
                }
                self.lower_for(stmt)
            }
            Statement::Break => {
                let Some(exit) = self.parser.loop_exit_blocks_mut().last().copied() else {
                    return Err(self.error(
                        "statement in an initial block",
                        "break outside loop",
                        None,
                    ));
                };
                self.kernel
                    .builder()
                    .seal_block(SIRTerminator::Jump(exit, Vec::new()));
                Ok(Flow::Ended)
            }
            Statement::SystemFunctionCall(call) => self.lower_system_function(call),
            Statement::FunctionCall(call) => self.lower_function_call(call),
            Statement::TbMethodCall(tb) => self.lower_tb_method(tb),
            Statement::Null => Ok(Flow::Continue),
            Statement::Unsupported(token) => Err(self.error(
                "statement in an initial block",
                "unsupported statement",
                Some(token),
            )),
        }
    }

    /// Branch on `condition` into two bodies that rejoin.
    fn branch(
        &mut self,
        condition: RegisterId,
        true_side: &[Statement],
        false_side: &[Statement],
    ) -> Result<Flow, ParserError> {
        let builder = self.kernel.builder();
        let then_block = builder.new_block();
        let else_block = builder.new_block();
        let join = builder.new_block();
        builder.seal_block(SIRTerminator::Branch {
            cond: condition,
            true_block: (then_block, Vec::new()),
            false_block: (else_block, Vec::new()),
        });
        let mut joined = false;
        for (block, body) in [(then_block, true_side), (else_block, false_side)] {
            self.kernel.builder().switch_to_block(block);
            if let Flow::Continue = self.lower_statements(body)? {
                self.kernel
                    .builder()
                    .seal_block(SIRTerminator::Jump(join, Vec::new()));
                joined = true;
            }
        }
        self.kernel.builder().switch_to_block(join);
        if joined {
            Ok(Flow::Continue)
        } else {
            self.kernel.builder().seal_block(SIRTerminator::Return);
            Ok(Flow::Ended)
        }
    }

    fn lower_case(
        &mut self,
        stmt: &veryl_analyzer::ir::CaseStatement,
    ) -> Result<Flow, ParserError> {
        self.lower_case_arm(stmt, 0)
    }

    fn lower_case_arm(
        &mut self,
        stmt: &veryl_analyzer::ir::CaseStatement,
        arm: usize,
    ) -> Result<Flow, ParserError> {
        let Some(case_arm) = stmt.arms.get(arm) else {
            return self.lower_statements(&stmt.default);
        };
        let condition_expr = case_arm_condition_expr(&stmt.case_target, &case_arm.patterns);
        let condition = self.condition(&condition_expr)?;
        let builder = self.kernel.builder();
        let then_block = builder.new_block();
        let else_block = builder.new_block();
        let join = builder.new_block();
        builder.seal_block(SIRTerminator::Branch {
            cond: condition,
            true_block: (then_block, Vec::new()),
            false_block: (else_block, Vec::new()),
        });
        let mut joined = false;
        self.kernel.builder().switch_to_block(then_block);
        if let Flow::Continue = self.lower_statements(&case_arm.body)? {
            self.kernel
                .builder()
                .seal_block(SIRTerminator::Jump(join, Vec::new()));
            joined = true;
        }
        self.kernel.builder().switch_to_block(else_block);
        if let Flow::Continue = self.lower_case_arm(stmt, arm + 1)? {
            self.kernel
                .builder()
                .seal_block(SIRTerminator::Jump(join, Vec::new()));
            joined = true;
        }
        self.kernel.builder().switch_to_block(join);
        if joined {
            Ok(Flow::Continue)
        } else {
            self.kernel.builder().seal_block(SIRTerminator::Return);
            Ok(Flow::Ended)
        }
    }

    fn lower_assign(&mut self, assign: &AssignStatement) -> Result<(), ParserError> {
        // A constant index or part select outside the destination is an
        // error of the testbench, as it was for the interpreter.
        for destination in &assign.dst {
            if let Some(&var_id) = self.ids.get(&destination.id)
                && let Some(info) = self.lookup.module_variables[&self.module_id].get(&var_id)
            {
                crate::testbench::ExprCompiler::validate_target_bounds_parts(
                    info,
                    &destination.index,
                    &destination.select,
                )
                .map_err(|error| match error {
                    ParserError::IllegalContext {
                        feature, detail, ..
                    } => ParserError::illegal_context(feature, detail, Some(&destination.token)),
                    other => other,
                })?;
            }
        }
        let rewritten;
        let assign = if let Some(destination) = &assign.hier_dst {
            let reference = crate::testbench::hierarchical_destination_reference(destination);
            let key = (reference.inst_path.clone(), reference.var_path.clone());
            let id = *self.parser.hierarchical_var(&key).ok_or_else(|| {
                self.error(
                    "hierarchical assignment",
                    format!("`{}` was not resolved", destination.var_path),
                    Some(&destination.token),
                )
            })?;
            rewritten = AssignStatement {
                dst: vec![AssignDestination {
                    id,
                    path: destination.var_path.clone(),
                    index: destination.index.clone(),
                    select: destination.select.clone(),
                    comptime: destination.comptime.clone(),
                    token: destination.token,
                }],
                hier_dst: None,
                width: assign.width,
                expr: assign.expr.clone(),
                token: assign.token,
            };
            &rewritten
        } else {
            assign
        };
        let (parser, kernel, targets, sources, convert) = parts!(self);
        parser.parse_assign_statement(
            assign,
            targets,
            &Domain::Process,
            &convert,
            sources,
            kernel.builder(),
        )
    }

    /// A `for` loop whose counter and bounds live in state, so its body
    /// may suspend. See [`LoopShape`] for the semantics.
    fn lower_for(&mut self, stmt: &veryl_analyzer::ir::ForStatement) -> Result<Flow, ParserError> {
        let shape = loop_shape(self.ir, stmt)?;
        let LoopShape {
            inclusive,
            reverse,
            step,
            op,
            var_width,
            var_signed,
            width,
            cmp_signed,
            ..
        } = shape;
        let (_, start_slot) = self.take_scratch("for_start")?;
        let (_, end_slot) = self.take_scratch("for_end")?;
        let single_slot = if shape.wide_capable() {
            Some(self.take_scratch("for_single")?.1)
        } else {
            None
        };
        let start_reg = self.for_bound(shape.start, width, cmp_signed)?;
        let end_reg = self.for_bound(shape.end, width, cmp_signed)?;
        self.store_scratch(start_slot, width, start_reg);
        self.store_scratch(end_slot, width, end_reg);
        let var = self.destination(stmt.var_id, stmt.token);
        let stall_message = format!(
            "non-progressing stepped for loop (loop variable `{}`)",
            source_name(stmt.var_name)
        );

        let builder = self.kernel.builder();
        let header = builder.new_block();
        let body = builder.new_block();
        let latch = builder.new_block();
        let exit = builder.new_block();
        let normal = builder.new_block();

        // A bound above 64 bits runs the body once when the range is that
        // one value, nothing when it is empty, and fails otherwise.
        if let Some(single_slot) = single_slot {
            let mut wide = None;
            for (reg, is_wide) in [(start_reg, shape.start_wide), (end_reg, shape.end_wide)] {
                if !is_wide {
                    continue;
                }
                let builder = self.kernel.builder();
                let shift = builder.alloc_bit(width, false);
                builder.emit(SIRInstruction::Imm(shift, SIRValue::new(64u64)));
                let high = builder.alloc_bit(width, cmp_signed);
                builder.emit(SIRInstruction::Binary(high, reg, BinaryOp::Shr, shift));
                let zero = builder.alloc_bit(width, cmp_signed);
                builder.emit(SIRInstruction::Imm(zero, SIRValue::new(0u64)));
                let nonzero = builder.alloc_bit(1, false);
                builder.emit(SIRInstruction::Binary(nonzero, high, BinaryOp::Ne, zero));
                wide = Some(match wide {
                    None => nonzero,
                    Some(previous) => {
                        let either = builder.alloc_bit(1, false);
                        builder.emit(SIRInstruction::Binary(
                            either,
                            previous,
                            BinaryOp::Or,
                            nonzero,
                        ));
                        either
                    }
                });
            }
            let wide = wide.expect("a wide bound");
            let builder = self.kernel.builder();
            let wide_block = builder.new_block();
            let single_block = builder.new_block();
            let fail_block = builder.new_block();
            let decide_block = builder.new_block();
            builder.seal_block(SIRTerminator::Branch {
                cond: wide,
                true_block: (wide_block, Vec::new()),
                false_block: (normal, Vec::new()),
            });
            builder.switch_to_block(wide_block);
            let empty = builder.alloc_bit(1, false);
            let (lhs, op_empty, rhs) = match (reverse, inclusive, cmp_signed) {
                (false, true, true) => (start_reg, BinaryOp::GtS, end_reg),
                (false, true, false) => (start_reg, BinaryOp::GtU, end_reg),
                (false, false, true) => (start_reg, BinaryOp::GeS, end_reg),
                (false, false, false) => (start_reg, BinaryOp::GeU, end_reg),
                (true, true, true) => (end_reg, BinaryOp::LtS, start_reg),
                (true, true, false) => (end_reg, BinaryOp::LtU, start_reg),
                (true, false, true) => (end_reg, BinaryOp::LeS, start_reg),
                (true, false, false) => (end_reg, BinaryOp::LeU, start_reg),
            };
            builder.emit(SIRInstruction::Binary(empty, lhs, op_empty, rhs));
            builder.seal_block(SIRTerminator::Branch {
                cond: empty,
                true_block: (exit, Vec::new()),
                false_block: (decide_block, Vec::new()),
            });
            builder.switch_to_block(decide_block);
            let single = if inclusive {
                let same = builder.alloc_bit(1, false);
                builder.emit(SIRInstruction::Binary(
                    same,
                    start_reg,
                    BinaryOp::Eq,
                    end_reg,
                ));
                same
            } else {
                let never = builder.alloc_bit(1, false);
                builder.emit(SIRInstruction::Imm(never, SIRValue::new(0u64)));
                never
            };
            builder.seal_block(SIRTerminator::Branch {
                cond: single,
                true_block: (single_block, Vec::new()),
                false_block: (fail_block, Vec::new()),
            });
            builder.switch_to_block(fail_block);
            let code = self.parser.runtime_error(
                format!(
                    "dynamic for-loop bound exceeds host {}",
                    if var_signed { "i128" } else { "usize" }
                ),
                vec![stmt.var_id],
            );
            self.kernel.builder().seal_block(SIRTerminator::Error(code));
            self.kernel.builder().switch_to_block(single_block);
            let one = self.constant(1, 1);
            self.store_scratch(single_slot, 1, one);
            let counter = self.parser.cast_reg_width_ext(
                self.kernel.builder(),
                start_reg,
                var_width,
                cmp_signed,
            );
            self.store(&var, counter)?;
            self.parser.clear_register_cache();
            self.kernel
                .builder()
                .seal_block(SIRTerminator::Jump(body, Vec::new()));
            self.kernel.builder().switch_to_block(normal);
            let zero = self.constant(0, 1);
            self.store_scratch(single_slot, 1, zero);
        } else {
            self.kernel
                .builder()
                .seal_block(SIRTerminator::Jump(normal, Vec::new()));
            self.kernel.builder().switch_to_block(normal);
        }

        // A signed counter cannot reach an unsigned bound above its maximum.
        if var_signed && var_width <= 64 {
            let max = (1u64 << (var_width - 1)) - 1;
            for (reg, bound_signed, is_end) in [
                (start_reg, shape.start_signed, false),
                (end_reg, shape.end_signed, true),
            ] {
                if bound_signed {
                    continue;
                }
                let builder = self.kernel.builder();
                let limit = builder.alloc_bit(width, cmp_signed);
                builder.emit(SIRInstruction::Imm(limit, SIRValue::new(max)));
                let above = builder.alloc_bit(1, false);
                builder.emit(SIRInstruction::Binary(above, reg, BinaryOp::GtU, limit));
                let out_of_range = if is_end && !inclusive {
                    // An exclusive end one above the maximum is reachable.
                    let next = builder.alloc_bit(width, cmp_signed);
                    builder.emit(SIRInstruction::Imm(next, SIRValue::new(max + 1)));
                    let allowed = builder.alloc_bit(1, false);
                    builder.emit(SIRInstruction::Binary(allowed, reg, BinaryOp::Eq, next));
                    let not_allowed = builder.alloc_bit(1, false);
                    builder.emit(SIRInstruction::Unary(
                        not_allowed,
                        UnaryOp::LogicNot,
                        allowed,
                    ));
                    let out = builder.alloc_bit(1, false);
                    builder.emit(SIRInstruction::Binary(
                        out,
                        above,
                        BinaryOp::And,
                        not_allowed,
                    ));
                    out
                } else {
                    above
                };
                let fail_block = builder.new_block();
                let continue_block = builder.new_block();
                builder.seal_block(SIRTerminator::Branch {
                    cond: out_of_range,
                    true_block: (fail_block, Vec::new()),
                    false_block: (continue_block, Vec::new()),
                });
                builder.switch_to_block(fail_block);
                let code = self
                    .parser
                    .runtime_error(stall_message.clone(), vec![stmt.var_id]);
                self.kernel.builder().seal_block(SIRTerminator::Error(code));
                self.kernel.builder().switch_to_block(continue_block);
            }
        }

        // The counter starts at the first value of the range.
        let initial = if reverse {
            if inclusive {
                end_reg
            } else {
                let builder = self.kernel.builder();
                let one = builder.alloc_bit(width, cmp_signed);
                builder.emit(SIRInstruction::Imm(one, SIRValue::new(1u64)));
                let first = builder.alloc_bit(width, cmp_signed);
                builder.emit(SIRInstruction::Binary(first, end_reg, BinaryOp::Sub, one));
                first
            }
        } else {
            start_reg
        };
        let initial =
            self.parser
                .cast_reg_width_ext(self.kernel.builder(), initial, var_width, cmp_signed);
        self.store(&var, initial)?;
        self.parser.clear_register_cache();
        self.kernel
            .builder()
            .seal_block(SIRTerminator::Jump(header, Vec::new()));

        // Header: compare the counter with the bound it runs towards.
        self.kernel.builder().switch_to_block(header);
        let current = self.load_var(stmt.var_id)?;
        let current = self.extend_for_compare(current, width, cmp_signed);
        let bound = if reverse {
            self.load_scratch(start_slot, width, cmp_signed)
        } else {
            self.load_scratch(end_slot, width, cmp_signed)
        };
        let builder = self.kernel.builder();
        let in_range = builder.alloc_bit(1, false);
        let op_in_range = match (reverse, inclusive, cmp_signed) {
            (true, _, true) => BinaryOp::GeS,
            (true, _, false) => BinaryOp::GeU,
            (false, true, true) => BinaryOp::LeS,
            (false, true, false) => BinaryOp::LeU,
            (false, false, true) => BinaryOp::LtS,
            (false, false, false) => BinaryOp::LtU,
        };
        builder.emit(SIRInstruction::Binary(
            in_range,
            current,
            op_in_range,
            bound,
        ));
        builder.seal_block(SIRTerminator::Branch {
            cond: in_range,
            true_block: (body, Vec::new()),
            false_block: (exit, Vec::new()),
        });

        // Body.
        self.kernel.builder().switch_to_block(body);
        self.parser.clear_register_cache();
        self.parser.loop_exit_blocks_mut().push(exit);
        let flow = self.lower_statements(&stmt.body);
        self.parser.loop_exit_blocks_mut().pop();
        if let Flow::Continue = flow? {
            self.kernel
                .builder()
                .seal_block(SIRTerminator::Jump(latch, Vec::new()));
        }

        // Latch: the step at the counter's width; a counter that does not
        // move towards the bound is an error.
        self.kernel.builder().switch_to_block(latch);
        self.parser.clear_register_cache();
        let current = self.load_var(stmt.var_id)?;
        let builder = self.kernel.builder();
        let step_bits = if var_width < 64 {
            (step as u64) & ((1u64 << var_width) - 1)
        } else {
            step as u64
        };
        let step_reg = builder.alloc_bit(var_width, var_signed);
        builder.emit(SIRInstruction::Imm(step_reg, SIRValue::new(step_bits)));
        let next = builder.alloc_bit(var_width, var_signed);
        let binary = match op {
            None if reverse => Some(BinaryOp::Sub),
            None => Some(BinaryOp::Add),
            Some(Op::Add) => Some(BinaryOp::Add),
            Some(Op::Sub) => Some(BinaryOp::Sub),
            Some(Op::Mul) => Some(BinaryOp::Mul),
            Some(Op::LogicShiftL | Op::ArithShiftL) => {
                if step >= var_width {
                    None
                } else {
                    Some(BinaryOp::Shl)
                }
            }
            Some(Op::BitOr) => Some(BinaryOp::Or),
            Some(Op::BitXor) => Some(BinaryOp::Xor),
            Some(Op::BitAnd) => Some(BinaryOp::And),
            Some(other) => {
                return Err(self.error(
                    "for loop step operator in an initial block",
                    format!("{other:?}"),
                    Some(&stmt.token),
                ));
            }
        };
        match binary {
            Some(binary) => builder.emit(SIRInstruction::Binary(next, current, binary, step_reg)),
            None => builder.emit(SIRInstruction::Imm(next, SIRValue::new(0u64))),
        }
        let stalled = builder.alloc_bit(1, false);
        let op_stalled = match (reverse, var_signed) {
            (false, true) => BinaryOp::LeS,
            (false, false) => BinaryOp::LeU,
            (true, true) => BinaryOp::GeS,
            (true, false) => BinaryOp::GeU,
        };
        builder.emit(SIRInstruction::Binary(stalled, next, op_stalled, current));
        let progress = builder.new_block();
        let stall = builder.new_block();
        builder.seal_block(SIRTerminator::Branch {
            cond: stalled,
            true_block: (stall, Vec::new()),
            false_block: (progress, Vec::new()),
        });
        builder.switch_to_block(stall);
        let code = self.parser.runtime_error(stall_message, vec![stmt.var_id]);
        self.kernel.builder().seal_block(SIRTerminator::Error(code));
        self.kernel.builder().switch_to_block(progress);
        self.store(&var, next)?;
        self.parser.clear_register_cache();
        match single_slot {
            Some(single_slot) => {
                let single = self.load_scratch(single_slot, 1, false);
                self.kernel.builder().seal_block(SIRTerminator::Branch {
                    cond: single,
                    true_block: (exit, Vec::new()),
                    false_block: (header, Vec::new()),
                });
            }
            None => {
                self.kernel
                    .builder()
                    .seal_block(SIRTerminator::Jump(header, Vec::new()));
            }
        }
        self.kernel.builder().switch_to_block(exit);
        self.parser.clear_register_cache();
        Ok(Flow::Continue)
    }

    /// `value` at `width` bits, sign-extended when `signed`.
    fn extend_for_compare(&mut self, value: RegisterId, width: usize, signed: bool) -> RegisterId {
        let value = self
            .parser
            .cast_reg_width_ext(self.kernel.builder(), value, width, signed);
        let builder = self.kernel.builder();
        match *builder.register(&value) {
            RegisterType::Bit {
                width: w,
                signed: s,
            } if w == width && s == signed => value,
            _ => {
                let typed = builder.alloc_bit(width, signed);
                builder.emit(SIRInstruction::Unary(typed, UnaryOp::Ident, value));
                typed
            }
        }
    }

    fn for_bound(
        &mut self,
        bound: &ForBound,
        width: usize,
        signed: bool,
    ) -> Result<RegisterId, ParserError> {
        let (parser, kernel, targets, sources, convert) = parts!(self);
        parser.parse_for_bound(
            bound,
            width,
            width,
            signed,
            targets,
            &Domain::Process,
            &convert,
            sources,
            kernel.builder(),
        )
    }

    fn lower_system_function(&mut self, call: &SystemFunctionCall) -> Result<Flow, ParserError> {
        match &call.kind {
            SystemFunctionKind::Finish => {
                self.kernel.finish();
                Ok(Flow::Ended)
            }
            SystemFunctionKind::Assert { kind, cond, args } => {
                self.lower_assert(call, *kind, cond, args)
            }
            SystemFunctionKind::Readmemh(_, SystemFunctionOutput::Local(_)) => {
                self.dirty = true;
                self.system_task(call)
            }
            SystemFunctionKind::Readmemh(filename, SystemFunctionOutput::Hier(reference)) => {
                let key = (reference.inst_path.clone(), reference.var_path.clone());
                let id = *self.parser.hierarchical_var(&key).ok_or_else(|| {
                    self.error(
                        "$readmemh destination",
                        format!("`{}` was not resolved", reference.var_path),
                        Some(&reference.comptime.token),
                    )
                })?;
                let local = SystemFunctionCall {
                    kind: SystemFunctionKind::Readmemh(
                        filename.clone(),
                        SystemFunctionOutput::Local(vec![AssignDestination {
                            id,
                            path: reference.var_path.clone(),
                            index: reference.index.clone(),
                            select: reference.select.clone(),
                            comptime: reference.comptime.clone(),
                            token: reference.comptime.token,
                        }]),
                    ),
                    comptime: call.comptime.clone(),
                };
                self.dirty = true;
                self.system_task(&local)
            }
            SystemFunctionKind::Display(_) | SystemFunctionKind::Write(_) => self.system_task(call),
            SystemFunctionKind::Bits(_)
            | SystemFunctionKind::Size(..)
            | SystemFunctionKind::Clog2(_)
            | SystemFunctionKind::Onehot(_)
            | SystemFunctionKind::Signed(_)
            | SystemFunctionKind::Unsigned(_) => Ok(Flow::Continue),
        }
    }

    fn system_task(&mut self, call: &SystemFunctionCall) -> Result<Flow, ParserError> {
        let (parser, kernel, targets, sources, convert) = parts!(self);
        parser.parse_system_task_statement(
            call,
            targets,
            &Domain::Process,
            &convert,
            sources,
            kernel.builder(),
        )?;
        Ok(Flow::Continue)
    }

    fn lower_assert(
        &mut self,
        call: &SystemFunctionCall,
        kind: AssertKind,
        cond: &SystemFunctionInput,
        args: &[SystemFunctionInput],
    ) -> Result<Flow, ParserError> {
        let condition = self.condition(&cond.0)?;
        let location = event_location(&call.comptime.token);
        let pass_site = self
            .parser
            .register_runtime_event_site(RuntimeEventKind::AssertPass, args);
        let fail_kind = match kind {
            AssertKind::Fatal => RuntimeEventKind::AssertFatal,
            AssertKind::Continue => RuntimeEventKind::AssertContinue,
        };
        let fail_site = self.parser.register_runtime_event_site(fail_kind, args);
        for site in [pass_site, fail_site] {
            let index = (site - self.site_base) as usize;
            self.parser.runtime_event_sites_mut()[index].location = location.clone();
        }
        let prepared = {
            let (parser, kernel, targets, sources, convert) = parts!(self);
            parser.prepare_effectful_runtime_event_args(
                args,
                targets,
                &Domain::Process,
                &convert,
                sources,
                kernel.builder(),
            )?
        };
        let builder = self.kernel.builder();
        let pass_block = builder.new_block();
        let fail_block = builder.new_block();
        let join = builder.new_block();
        builder.seal_block(SIRTerminator::Branch {
            cond: condition,
            true_block: (pass_block, Vec::new()),
            false_block: (fail_block, Vec::new()),
        });
        for (block, site) in [(pass_block, pass_site), (fail_block, fail_site)] {
            self.kernel.builder().switch_to_block(block);
            let (parser, kernel, targets, sources, convert) = parts!(self);
            parser.emit_runtime_event_with_prepared_args(
                site,
                args,
                prepared.clone(),
                targets,
                &Domain::Process,
                &convert,
                sources,
                kernel.builder(),
            )?;
            if site == fail_site && matches!(kind, AssertKind::Fatal) {
                let message = args
                    .first()
                    .and_then(|arg| FfParser::static_string_expr(&arg.0))
                    .unwrap_or_else(|| "assertion failed".to_string());
                let code = self.parser.runtime_error(message, Vec::new());
                self.kernel.builder().seal_block(SIRTerminator::Error(code));
            } else {
                self.kernel
                    .builder()
                    .seal_block(SIRTerminator::Jump(join, Vec::new()));
            }
        }
        self.kernel.builder().switch_to_block(join);
        Ok(Flow::Continue)
    }

    /// Inline a function call statement: bind its inputs, run its body, and
    /// copy its outputs back.
    fn lower_function_call(
        &mut self,
        call: &veryl_analyzer::ir::FunctionCall,
    ) -> Result<Flow, ParserError> {
        let Some(body) = function_body(&self.ir.functions, call) else {
            return Err(self.error(
                "function call in an initial block",
                format!("{call}"),
                Some(&call.comptime.token),
            ));
        };
        if !self.active_functions.insert(call.id) {
            return Err(self.error(
                "function call in an initial block",
                "recursive call",
                Some(&call.comptime.token),
            ));
        }
        for (path, expr) in &call.inputs {
            if let Some(&formal) = body.arg_map.get(path) {
                let value = self.expression(expr)?;
                let destination = self.destination(formal, call.comptime.token);
                self.store(&destination, value)?;
            }
        }
        let flow = self.lower_statements(&body.statements)?;
        self.active_functions.remove(&call.id);
        if let Flow::Ended = flow {
            return Ok(Flow::Ended);
        }
        for (path, destinations) in &call.outputs {
            if let Some(&formal) = body.arg_map.get(path) {
                for destination in destinations {
                    let value = self.load_var(formal)?;
                    self.store(destination, value)?;
                }
            }
        }
        Ok(Flow::Continue)
    }

    fn lower_tb_method(&mut self, tb: &TbMethodCall) -> Result<Flow, ParserError> {
        match &tb.method {
            TbMethod::ClockNext { count, .. } => {
                let clock = self.clock_index(tb.inst)?;
                let count = match count {
                    Some(expr) => {
                        let value = self.expression(expr)?;
                        self.unsigned_bits(value, PROCESS_DELAY_WIDTH)
                    }
                    None => self.constant(1, PROCESS_DELAY_WIDTH),
                };
                self.kernel
                    .wait_clock(clock, count, None)
                    .map_err(|error| {
                        self.error("testbench process lowering", error.to_string(), None)
                    })?;
                self.suspended();
                Ok(Flow::Continue)
            }
            TbMethod::ResetAssert { clock, duration } => {
                let clock_index = self.clock_index(*clock)?;
                let reset = self.instance_var(tb.inst)?;
                let reset_source = SourceAddr {
                    instance_id: self.instance,
                    var_id: self.ids[&reset],
                };
                let type_kind = self.lookup.module_variables[&self.module_id][&reset_source.var_id]
                    .metadata
                    .type_kind;
                let (assert_value, deassert_value) = match type_kind {
                    PortTypeKind::ResetAsyncHigh | PortTypeKind::ResetSyncHigh => (1, 0),
                    _ => (0, 1),
                };
                let value = self.constant(assert_value, 1);
                let destination = self.destination(
                    reset,
                    tb.ret.as_ref().map_or(self.ir.token, |ret| ret.token),
                );
                self.store(&destination, value)?;
                let duration = match duration {
                    Some(expr) => {
                        let value = self.expression(expr)?;
                        let value = self.unsigned_bits(value, PROCESS_DELAY_WIDTH);
                        // A reset holds for at least one cycle.
                        let builder = self.kernel.builder();
                        let zero = builder.alloc_bit(PROCESS_DELAY_WIDTH, false);
                        builder.emit(SIRInstruction::Imm(zero, SIRValue::new(0u64)));
                        let one = builder.alloc_bit(PROCESS_DELAY_WIDTH, false);
                        builder.emit(SIRInstruction::Imm(one, SIRValue::new(1u64)));
                        let is_zero = builder.alloc_bit(1, false);
                        builder.emit(SIRInstruction::Binary(is_zero, value, BinaryOp::Eq, zero));
                        let held = builder.alloc_bit(PROCESS_DELAY_WIDTH, false);
                        builder.emit(SIRInstruction::Mux(held, is_zero, one, value));
                        held
                    }
                    None => self.constant(3, PROCESS_DELAY_WIDTH),
                };
                let release = match self
                    .releases
                    .iter()
                    .position(|release| release.signal == reset_source)
                {
                    Some(index) => index as u32,
                    None => {
                        let clock_var = self.instance_var(*clock)?;
                        self.releases.push(ProcessRelease {
                            signal: reset_source,
                            value: deassert_value,
                            clock: Some(SourceAddr {
                                instance_id: self.instance,
                                var_id: self.ids[&clock_var],
                            }),
                        });
                        let index = self.kernel.add_release();
                        debug_assert_eq!(index as usize + 1, self.releases.len());
                        index
                    }
                };
                self.kernel
                    .wait_clock(clock_index, duration, Some(release))
                    .map_err(|error| {
                        self.error("testbench process lowering", error.to_string(), None)
                    })?;
                self.suspended();
                Ok(Flow::Continue)
            }
            TbMethod::RandomSeed { value } => {
                let seed = self.expression(value)?;
                let seed = self.unsigned_bits(seed, 64);
                let (host_value, slot) = self.host_scratch("random_seed", false, false)?;
                self.store_scratch(slot, 64, seed);
                self.host_request(HostRequest::RandomSeed {
                    handle: self.resource_name(tb.inst),
                    value: host_value,
                })?;
                Ok(Flow::Continue)
            }
            TbMethod::RandomGet { width, signed } => {
                let (result, slot) = self.host_scratch("random_value", *signed, false)?;
                let result_width = result.width;
                self.host_request(HostRequest::RandomGet {
                    handle: self.resource_name(tb.inst),
                    result,
                })?;
                if let Some(ret) = tb.ret.as_deref() {
                    let value = self.load_scratch(slot, result_width, *signed);
                    let _ = width;
                    self.store(ret, value)?;
                }
                Ok(Flow::Continue)
            }
            TbMethod::RandomGetRange {
                min,
                max,
                width: _,
                signed,
            } => {
                let min_reg = self.expression(min)?;
                let min_reg = self.unsigned_bits(min_reg, 64);
                let max_reg = self.expression(max)?;
                let max_reg = self.unsigned_bits(max_reg, 64);
                let (min_value, min_slot) = self.host_scratch("random_min", *signed, false)?;
                let (max_value, max_slot) = self.host_scratch("random_max", *signed, false)?;
                let (result, slot) = self.host_scratch("random_value", *signed, false)?;
                let result_width = result.width;
                self.store_scratch(min_slot, 64, min_reg);
                self.store_scratch(max_slot, 64, max_reg);
                self.host_request(HostRequest::RandomGetRange {
                    handle: self.resource_name(tb.inst),
                    min: min_value,
                    max: max_value,
                    result,
                })?;
                if let Some(ret) = tb.ret.as_deref() {
                    let value = self.load_scratch(slot, result_width, *signed);
                    self.store(ret, value)?;
                }
                Ok(Flow::Continue)
            }
            TbMethod::RandomGetSeed => {
                let (result, slot) = self.host_scratch("random_seed", false, false)?;
                self.host_request(HostRequest::RandomGetSeed {
                    handle: self.resource_name(tb.inst),
                    result,
                })?;
                if let Some(ret) = tb.ret.as_deref() {
                    let value = self.load_scratch(slot, 64, false);
                    self.store(ret, value)?;
                }
                Ok(Flow::Continue)
            }
            TbMethod::Component { method, args } => {
                let mut host_args = Vec::new();
                for arg in args {
                    let is_string = arg.0.comptime().r#type.is_string();
                    let signed = expression_signed(&arg.0);
                    let (value, slot) = self.host_scratch("component_arg", signed, is_string)?;
                    let width = value.width;
                    let reg = if let Some(text) = FfParser::static_string_expr(&arg.0) {
                        let builder = self.kernel.builder();
                        let reg = builder.alloc_bit(width, false);
                        builder.emit(SIRInstruction::Imm(
                            reg,
                            SIRValue::new(BigUint::from_bytes_be(text.as_bytes())),
                        ));
                        reg
                    } else {
                        let reg = self.expression(&arg.0)?;
                        self.unsigned_bits(reg, width)
                    };
                    self.store_scratch(slot, width, reg);
                    host_args.push(value);
                }
                let result = match tb.ret.as_deref() {
                    Some(_) => {
                        let (value, slot) =
                            self.host_scratch("component_result", tb.ret_signed, false)?;
                        Some((value, slot))
                    }
                    None => None,
                };
                self.host_request(HostRequest::Component {
                    instance: self.resource_name(tb.inst),
                    method: source_name(*method),
                    args: host_args,
                    result: result.as_ref().map(|(value, _)| value.clone()),
                    declared_width: tb.ret_width.map(|w| w as usize),
                    strict: tb.ret_strict,
                })?;
                if let (Some(ret), Some((value, slot))) = (tb.ret.as_deref(), result) {
                    let reg = self.load_scratch(slot, value.width, tb.ret_signed);
                    self.store(ret, reg)?;
                }
                Ok(Flow::Continue)
            }
            TbMethod::FileOpen { .. }
            | TbMethod::FileWrite { .. }
            | TbMethod::FileClose
            | TbMethod::FileFlush => Ok(Flow::Continue),
        }
    }
}

fn source_name(id: StrId) -> String {
    resource_table::get_str_value(id).unwrap_or_else(|| format!("{id}"))
}

fn try_eval_const(expr: &Expression) -> Option<u64> {
    if !expr.comptime().is_const {
        return None;
    }
    expr.comptime()
        .get_value()
        .ok()
        .map(|value| value.payload_u64())
}

fn event_location(token: &veryl_parser::token_range::TokenRange) -> Option<EventLocation> {
    let begin = &token.beg;
    let file = begin
        .source
        .get_path()
        .and_then(resource_table::get_path_value)?;
    Some(EventLocation {
        file: file.to_string_lossy().into_owned(),
        line: begin.line,
        column: begin.column,
    })
}

/// Drop the blocks control never enters.
fn prune_unreachable_blocks<A>(unit: &mut ExecutionUnit<A>) {
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

/// Map the addresses and runtime error codes of a unit.
fn map_unit<A, B>(
    unit: &ExecutionUnit<A>,
    codes: &HashMap<i64, i64>,
    mut map: impl FnMut(&A) -> Result<B, ParserError>,
) -> Result<ExecutionUnit<B>, ParserError> {
    let mut blocks = HashMap::default();
    let mut ids: Vec<&BlockId> = unit.blocks.keys().collect();
    ids.sort();
    for id in ids {
        let block = &unit.blocks[id];
        let mut instructions = Vec::with_capacity(block.instructions.len());
        for instruction in &block.instructions {
            // Resolve every address first, then rebuild with the results.
            let mut resolved = Vec::new();
            let mut failure = None;
            instruction.map_addr(|address| match map(address) {
                Ok(address) => resolved.push(address),
                Err(error) => {
                    failure.get_or_insert(error);
                }
            });
            if let Some(error) = failure {
                return Err(error);
            }
            let mut resolved = resolved.into_iter();
            instructions.push(
                instruction
                    .map_addr(|_| resolved.next().expect("one resolved address per address")),
            );
        }
        let terminator = match &block.terminator {
            SIRTerminator::Error(code) => {
                SIRTerminator::Error(codes.get(code).copied().unwrap_or(*code))
            }
            other => other.clone(),
        };
        blocks.insert(
            *id,
            BasicBlock {
                id: block.id,
                params: block.params.clone(),
                instructions,
                terminator,
            },
        );
    }
    Ok(ExecutionUnit {
        entry_block_id: unit.entry_block_id,
        blocks,
        register_map: unit.register_map.clone(),
    })
}
