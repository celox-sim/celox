use std::collections::VecDeque;

use crate::context_width::{
    ValueContext, binary_semantics, cast_semantics, expression_signed, resolve_binary_op,
    system_function_dimension, system_function_type_size,
};
use crate::{
    BuildConfig, HashMap, HashSet, ParserError, RegionedVarAddr,
    bitaccess::{
        PartSelectGeometry, build_dynamic_partial_assign_expr, build_partial_assign_expr,
        celox_value_from_comptime_in_context, eval_constexpr, eval_var_select, get_access_width,
        is_static_access,
    },
    case::case_arm_condition_expr,
    function_call_arg, resolve_total_width,
};
use bit_set::BitSet;
use celox_design::{
    BinaryOp, BitAccess, DisplaySizing, RuntimeErrorInfo, RuntimeEventKind, RuntimeEventSite,
    SPARSE_WORKING_REGION, STABLE_REGION, Severity, TriggerSet, UnaryOp, VarAtomBase,
    WORKING_REGION,
};
use celox_sir::{
    BlockId, RegisterId, RegisterType, SIRBuilder, SIRInstruction, SIROffset, SIRTerminator,
    SIRValue,
};
use num_bigint::{BigInt, BigUint, Sign};
use num_traits::{ToPrimitive, Zero};

use veryl_analyzer::ir::{
    ArrayLiteralItem, AssertKind, AssignDestination, AssignStatement, CasePattern, CaseStatement,
    Comptime, Expression, Factor, FfDeclaration, FfReset, ForBound, ForRange, ForStatement,
    IfResetStatement, IfStatement, Module, Op, Shape, Statement, SystemFunctionCall,
    SystemFunctionInput, SystemFunctionKind, Type, TypeKind, ValueVariant, VarId, VarIndex,
    VarKind, VarSelect, VarSelectOp,
};
use veryl_analyzer::symbol::Affiliation;
use veryl_analyzer::value::Value;
use veryl_analyzer::value::byte_value_to_string;
use veryl_parser::token_range::TokenRange;

#[cfg(test)]
mod loop_bound_status_tests;

#[cfg(test)]
mod procedural_condition_tests;

#[cfg(test)]
mod function_state_coercion_tests;

#[cfg(test)]
mod signed_div_rem_tests;

mod access;
mod aggregate;
mod array_literal;
mod array_view;
mod array_view_usage;
mod branch;
mod effect_analysis;
mod expression;
mod for_loop;
mod function_args;
mod function_body;
mod function_call;
mod function_effects;
mod function_path;
mod function_result;
mod function_state;
mod guard;
mod inline_call;
mod operator;
mod read_write;
mod reset;
mod statement;
mod system_function;
mod system_task;
mod width;
use access::*;
use array_view::*;
use branch::*;
pub(crate) use expression::expression_has_side_effect;
#[cfg(test)]
use for_loop::LoopBoundStatus;
use function_call::*;
use function_path::*;
use guard::*;
use system_task::*;

pub enum Domain {
    Ff, // TODO: add clock
}
impl Domain {
    pub fn region(&self) -> u32 {
        match self {
            Domain::Ff => WORKING_REGION,
        }
    }
}

pub struct FfGroupParseResult<A = RegionedVarAddr> {
    pub targets: Vec<VarAtomBase<A>>,
    pub sources: Vec<VarAtomBase<A>>,
    pub dynamic_write_vars: HashSet<VarId>,
}

#[derive(Clone)]
struct FunctionArrayView {
    backing_var_id: VarId,
    // Registers preserve this invocation's values if a nested invocation
    // temporarily reuses the same formal working region.
    elements: Vec<RegisterId>,
    // Aliased forwarding views do not write their backing and therefore do
    // not require the caller's snapshot to be restored.
    owns_backing: bool,
    // Lazily evaluated literal items are keyed by their structural path in
    // the bound array literal so the cache survives cloned bindings.
    cached_literal_items: HashMap<Vec<usize>, FunctionArrayLiteralItemCache>,
    // Branch-local lazy initialization is represented explicitly at control
    // flow joins. `None` means every path reaching the current block has a
    // valid backing view; `Some` guards the carried element snapshot.
    initialized: Option<RegisterId>,
}

#[derive(Clone)]
struct FunctionArrayLiteralItemCache {
    elements: Vec<RegisterId>,
    // `None` means the item is available on every path reaching the current
    // block. `Some` guards a cache populated on only some predecessors.
    initialized: Option<RegisterId>,
}

impl<A> Default for FfGroupParseResult<A> {
    fn default() -> Self {
        Self {
            targets: Vec::new(),
            sources: Vec::new(),
            dynamic_write_vars: HashSet::default(),
        }
    }
}

pub struct FfParser<'a> {
    module: &'a Module,
    stack: VecDeque<RegisterId>,
    defined: Definedness,
    dynamic_write_vars: HashSet<VarId>,
    placement: WritePlacement,
    local_working_vars: HashSet<VarId>,
    // Locals and formals of functions whose bodies are lowered inline. They
    // use blocking working-region storage that is private to the call and is
    // neither seeded from nor committed to stable state.
    inline_function_locals: HashSet<VarId>,
    local_let_values: HashMap<VarId, RegisterId>,
    loop_exit_blocks: Vec<BlockId>,
    reset: Option<FfReset>,
    frames: CallFrames,
    runtime: RuntimeSites,
    config: BuildConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ControlFlow {
    Continue,
    Break,
}

impl<'a> FfParser<'a> {
    pub fn new(module: &'a Module, config: BuildConfig) -> Self {
        let local_working_vars = module
            .variables
            .iter()
            .filter_map(|(id, variable)| {
                (variable.affiliation == Affiliation::AlwaysFf
                    && variable.kind != veryl_analyzer::ir::VarKind::Let)
                    .then_some(*id)
            })
            .collect();
        Self {
            module,
            stack: VecDeque::new(),
            defined: Definedness {
                defined_ranges: HashMap::default(),
                dynamic_defined_vars: HashSet::default(),
            },
            dynamic_write_vars: HashSet::default(),
            placement: WritePlacement {
                sparse_write_vars: HashSet::default(),
                direct_static_write_ranges: HashMap::default(),
                direct_dynamic_write_vars: HashSet::default(),
            },
            local_working_vars,
            inline_function_locals: HashSet::default(),
            local_let_values: HashMap::default(),
            loop_exit_blocks: Vec::new(),
            reset: None,
            frames: CallFrames {
                function_arg_stack: Vec::new(),
                function_arg_value_stack: Vec::new(),
                function_expression_value_stack: Vec::new(),
                function_event_arg_state_stack: Vec::new(),
                function_array_view_stack: Vec::new(),
                function_array_view_enabled_stack: Vec::new(),
            },
            runtime: RuntimeSites {
                runtime_errors: HashMap::default(),
                runtime_event_sites: Vec::new(),
                memory_file_bases: Vec::new(),
                next_runtime_error_code: 2000,
                runtime_error_code_map: None,
                runtime_event_site_base: 0,
            },
            config,
        }
    }

    pub fn with_relocated_runtime_ids(
        mut self,
        runtime_error_code_map: HashMap<i64, i64>,
        runtime_event_site_base: u32,
    ) -> Self {
        self.runtime.runtime_error_code_map = Some(runtime_error_code_map);
        self.runtime.runtime_event_site_base = runtime_event_site_base;
        self
    }

    pub fn with_direct_write_ranges(
        mut self,
        static_ranges: HashMap<VarId, Vec<celox_design::BitAccess>>,
        dynamic_vars: HashSet<VarId>,
    ) -> Self {
        self.placement.direct_static_write_ranges = static_ranges;
        self.placement.direct_dynamic_write_vars = dynamic_vars;
        self
    }

    pub fn with_sparse_write_vars(mut self, sparse_write_vars: HashSet<VarId>) -> Self {
        self.placement.sparse_write_vars = sparse_write_vars;
        self
    }

    pub fn runtime_errors(&self) -> &HashMap<i64, RuntimeErrorInfo<VarId>> {
        &self.runtime.runtime_errors
    }

    pub fn runtime_event_sites(&self) -> &Vec<RuntimeEventSite> {
        &self.runtime.runtime_event_sites
    }

    /// The directories the memory file loads of the blocks look relative
    /// names up in.
    pub fn memory_file_bases(&self) -> &[celox_design::MemoryFileBase] {
        &self.runtime.memory_file_bases
    }

    // expression / function-call lowering is split into submodules:
    // - parser/ff/expression.rs
    // - parser/ff/function_call.rs
    pub fn detect_trigger_set(&self, decl: &FfDeclaration) -> TriggerSet<VarId> {
        let mut trigger_set = TriggerSet {
            clock: decl.clock.id,
            resets: Vec::new(),
        };

        if let Some(reset) = &decl.reset {
            let var = &self.module.variables[&reset.id];
            let is_async = match var.r#type.kind {
                TypeKind::ResetAsyncHigh | TypeKind::ResetAsyncLow => true,
                TypeKind::Reset => matches!(
                    self.config.reset_type,
                    veryl_metadata::ResetType::AsyncHigh | veryl_metadata::ResetType::AsyncLow
                ),
                _ => false,
            };
            if is_async {
                trigger_set.resets.push(reset.id);
            }
        }
        trigger_set
    }

    pub fn parse_ff_group(
        &mut self,
        decls: &[&FfDeclaration],
        ir_builder: &mut SIRBuilder<RegionedVarAddr>,
    ) -> Result<FfGroupParseResult, ParserError> {
        self.parse_ff_group_into(
            decls,
            &|var_id, region| RegionedVarAddr { var_id, region },
            ir_builder,
        )
    }

    pub fn parse_ff_group_into<A>(
        &mut self,
        decls: &[&FfDeclaration],
        convert: &impl Fn(VarId, u32) -> A,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<FfGroupParseResult<A>, ParserError> {
        if decls.is_empty() {
            return Ok(FfGroupParseResult::default());
        }

        self.defined.defined_ranges.clear();
        self.defined.dynamic_defined_vars.clear();
        self.dynamic_write_vars.clear();
        self.local_let_values.clear();
        let mut targets = Vec::new();
        let mut sources = Vec::new();

        // Reset-only declarations can share one branch, which keeps large
        // unrolled reset groups compact. If a declaration has neighboring
        // statements, however, moving `if_reset` changes the source-order
        // priority of overlapping nonblocking assignments.
        let reset_only = decls
            .iter()
            .all(|decl| matches!(decl.statements.as_slice(), [Statement::IfReset(_)]));
        if reset_only {
            self.reset = decls[0].reset.clone();
            let mut all_true_sides = Vec::new();
            let mut all_false_sides = Vec::new();
            for decl in decls {
                let Statement::IfReset(if_reset) = &decl.statements[0] else {
                    unreachable!("reset-only declarations contain one if_reset statement");
                };
                all_true_sides.extend(if_reset.true_side.iter());
                all_false_sides.extend(if_reset.false_side.iter());
            }
            self.parse_if_reset_internal(
                &all_true_sides,
                &all_false_sides,
                &mut targets,
                &Domain::Ff,
                convert,
                &mut sources,
                ir_builder,
            )?;
        } else {
            for decl in decls {
                self.reset = decl.reset.clone();
                for stmt in &decl.statements {
                    self.parse_statement(
                        stmt,
                        &mut targets,
                        &Domain::Ff,
                        convert,
                        &mut sources,
                        ir_builder,
                    )?;
                }
            }
        }

        Ok(FfGroupParseResult {
            targets,
            sources,
            dynamic_write_vars: self.dynamic_write_vars.clone(),
        })
    }
}
