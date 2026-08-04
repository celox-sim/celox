use std::collections::VecDeque;

use crate::{
    BuildConfig, HashMap, HashSet, LoweringPhase, ParserError, RegionedVarAddr,
    bitaccess::{celox_value_from_comptime_in_context, eval_constexpr},
    case::case_arm_condition_expr,
    resolve_total_width,
};
use bit_set::BitSet;
use celox_design::{
    BinaryOp, RuntimeErrorInfo, RuntimeEventKind, RuntimeEventSite, TriggerSet, UnaryOp,
    VarAtomBase, WORKING_REGION,
};
use celox_sir::{
    BlockId, RegisterId, RegisterType, SIRBuilder, SIRInstruction, SIROffset, SIRTerminator,
    SIRValue,
};
use num_bigint::{BigInt, BigUint, Sign};
use num_traits::{ToPrimitive, Zero};

use veryl_analyzer::ir::{
    AssertKind, CaseStatement, Expression, FfDeclaration, FfReset, ForBound, ForRange,
    ForStatement, IfResetStatement, IfStatement, Module, Op, Statement, SystemFunctionCall,
    SystemFunctionInput, SystemFunctionKind, TypeKind, VarId,
};
use veryl_analyzer::symbol::Affiliation;
use veryl_analyzer::value::Value;
use veryl_analyzer::value::byte_value_to_string;
use veryl_parser::token_range::TokenRange;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LoopBoundStatus {
    FitsLoopType,
    ExclusiveUpperSentinel,
    OutOfRange,
}

#[cfg(test)]
mod loop_bound_status_tests {
    use super::{FfParser, LoopBoundStatus};
    use veryl_analyzer::ir::{ForBound, Op};

    #[test]
    fn allows_exclusive_upper_sentinel() {
        assert_eq!(
            FfParser::loop_bound_status(&ForBound::Const(255), 8, false),
            Some(LoopBoundStatus::FitsLoopType)
        );
        assert_eq!(
            FfParser::loop_bound_status(&ForBound::Const(256), 8, false),
            Some(LoopBoundStatus::ExclusiveUpperSentinel)
        );
        assert_eq!(
            FfParser::loop_bound_status(&ForBound::Const(257), 8, false),
            Some(LoopBoundStatus::OutOfRange)
        );
    }

    #[test]
    fn bitwise_stall_checks_use_the_i32_loop_counter_width() {
        let Some(above_i32) = 1usize.checked_shl(32) else {
            return;
        };

        for (op, step, start, expected) in [
            (Op::BitXor, above_i32, Some(0), true),
            (Op::BitXor, above_i32 | 1, Some(0), false),
            (Op::BitOr, above_i32, Some(3), true),
            (Op::BitOr, above_i32 | 3, Some(3), true),
            (Op::BitOr, above_i32 | 4, Some(3), false),
            (Op::BitOr, above_i32 | 3, Some(above_i32 | 3), true),
        ] {
            assert_eq!(
                FfParser::step_can_stall(false, Some(op), step, start, 32),
                expected,
                "op={op:?}, step={step:#x}, start={start:?}",
            );
        }

        assert!(!FfParser::step_can_stall(
            false,
            Some(Op::BitXor),
            above_i32,
            Some(0),
            33,
        ));
    }
}

#[cfg(test)]
mod procedural_condition_tests {
    use super::FfParser;
    use veryl_analyzer::{
        ir::{Comptime, Expression, Factor, Shape, Type, TypeKind, ValueVariant},
        value::Value,
    };
    use veryl_parser::token_range::TokenRange;

    fn logic_constant(payload: u128, mask: u128, width: usize) -> Expression {
        let token = TokenRange::default();
        let mut ty = Type::new(TypeKind::Logic);
        ty.set_concrete_width(Shape::new(vec![Some(width)]));
        Expression::Term(Box::new(Factor::Value(Comptime {
            value: ValueVariant::Numeric(Value::from_u128(payload, mask, width, false)),
            r#type: ty,
            is_const: true,
            is_global: true,
            evaluated: true,
            token,
            ..Default::default()
        })))
    }

    #[test]
    fn constant_truth_ignores_unknown_bits_but_keeps_known_ones() {
        // Veryl encodes X=(payload 0, mask 1) and Z=(payload 1, mask 1).
        for (payload, mask, expected) in [
            (0x80, 0x04, true),
            (0x00, 0x04, false),
            (0x04, 0x04, false),
            (0x00, 0x00, false),
            (0x80, 0x00, true),
        ] {
            let condition = logic_constant(payload, mask, 8);
            assert_eq!(
                FfParser::get_constant_procedural_truth(&condition),
                Some(expected),
                "payload={payload:#x}, mask={mask:#x}",
            );
        }
    }
}

#[cfg(test)]
mod function_state_coercion_tests {
    use super::FfParser;
    use crate::BuildConfig;
    use veryl_analyzer::{
        Analyzer, Context, attribute_table,
        ir::{Component, Expression, Factor, Ir, Op, Statement, ValueVariant},
        symbol_table,
    };
    use veryl_metadata::Metadata;
    use veryl_parser::Parser;

    #[test]
    fn unpacked_array_element_assignment_uses_element_type() {
        symbol_table::clear();
        attribute_table::clear();

        let code = r#"
            module Top (
                clk: input clock,
                q: output logic
            ) {
                function observed () -> logic {
                    var values: logic<8>[2];
                    values[1] = 8'd0;
                    values[0] = 16'h0100;
                    $display("upper=%0d", values[1]);
                    return 1'b0;
                }

                always_ff (clk) {
                    q = observed();
                }
            }
        "#;
        let metadata = Metadata::create_default("prj").unwrap();
        let parsed = Parser::parse(code, &"").unwrap();
        let analyzer = Analyzer::new(&metadata);
        let mut context = Context::default();
        let mut ir = Ir::default();
        assert!(analyzer.analyze_pass1("prj", &parsed.veryl).is_empty());
        assert!(Analyzer::analyze_post_pass1().is_empty());
        assert!(
            analyzer
                .analyze_pass2(&parsed.veryl, &mut context, Some(&mut ir))
                .is_empty()
        );
        assert!(Analyzer::analyze_post_pass2(&ir).is_empty());

        let module = ir
            .components
            .into_iter()
            .find_map(|component| match component {
                Component::Module(module) => Some(module),
                _ => None,
            })
            .unwrap();
        let function_body = module
            .functions
            .values()
            .find_map(|function| function.get_function(&[]))
            .unwrap();
        let assignment = function_body
            .statements
            .iter()
            .find_map(|statement| match statement {
                Statement::Assign(assign)
                    if assign.expr.comptime().r#type.total_width() == Some(16) =>
                {
                    Some(assign)
                }
                _ => None,
            })
            .unwrap();

        let parser = FfParser::new(&module, BuildConfig::default());
        let coerced = parser
            .coerce_function_state_assignment(assignment.expr.clone(), &assignment.dst[0])
            .unwrap();
        let Expression::Binary(_, Op::As, target, comptime) = coerced else {
            panic!("unpacked array element assignment must insert an explicit cast");
        };
        assert_eq!(comptime.r#type.total_width(), Some(8));
        let Expression::Term(target) = target.as_ref() else {
            panic!("assignment cast must have a term target");
        };
        let Factor::Value(target) = target.as_ref() else {
            panic!("assignment cast must have a type target");
        };
        let ValueVariant::Type(target) = &target.value else {
            panic!("assignment cast target must be a type");
        };
        assert_eq!(target.total_width(), Some(8));
        assert!(target.array.is_empty());
    }
}

#[cfg(test)]
mod signed_div_rem_tests {
    use super::FfParser;
    use crate::BuildConfig;
    use celox_design::BinaryOp;
    use celox_sir::{SIRBuilder, SIRInstruction};
    use veryl_analyzer::{
        Analyzer, Context, attribute_table,
        ir::{Component, Declaration, Ir},
        symbol_table,
    };
    use veryl_metadata::Metadata;
    use veryl_parser::Parser;

    #[test]
    fn ff_parser_selects_explicit_div_rem_signedness_from_source_expressions() {
        symbol_table::clear();
        attribute_table::clear();

        let code = r#"
            module Top (
                clk: input clock,
                ua: input logic<8>,
                ub: input logic<8>,
                sa: input signed logic<8>,
                sb: input signed logic<8>,
                sc: input signed logic<8>,
                udiv: output logic<8>,
                urem: output logic<8>,
                sdiv: output signed logic<8>,
                srem: output signed logic<8>,
                nested: output signed logic<8>,
                mixed: output logic<8>
            ) {
                always_ff (clk) {
                    udiv = ua / ub;
                    urem = ua % ub;
                    sdiv = sa / sb;
                    srem = sa % sb;
                    nested = (sa / sb) / sc;
                    mixed = sa / ub;
                }
            }
        "#;
        let metadata = Metadata::create_default("prj").unwrap();
        let parsed = Parser::parse(code, &"").unwrap();
        let analyzer = Analyzer::new(&metadata);
        let mut context = Context::default();
        let mut ir = Ir::default();
        assert!(analyzer.analyze_pass1("prj", &parsed.veryl).is_empty());
        assert!(Analyzer::analyze_post_pass1().is_empty());
        assert!(
            analyzer
                .analyze_pass2(&parsed.veryl, &mut context, Some(&mut ir))
                .is_empty()
        );
        assert!(Analyzer::analyze_post_pass2(&ir).is_empty());

        let module = ir
            .components
            .into_iter()
            .find_map(|component| match component {
                Component::Module(module) => Some(module),
                _ => None,
            })
            .unwrap();
        let declarations = module
            .declarations
            .iter()
            .filter_map(|declaration| match declaration {
                Declaration::Ff(declaration) => Some(declaration.as_ref()),
                _ => None,
            })
            .collect::<Vec<_>>();
        let mut parser = FfParser::new(&module, BuildConfig::default());
        let mut builder = SIRBuilder::new();
        parser.parse_ff_group(&declarations, &mut builder).unwrap();
        let execution_unit = builder.flush_eu().unwrap();

        let mut div_u = 0;
        let mut div_s = 0;
        let mut rem_u = 0;
        let mut rem_s = 0;
        for op in execution_unit.blocks.values().flat_map(|block| {
            block
                .instructions
                .iter()
                .filter_map(|instruction| match instruction {
                    SIRInstruction::Binary(_, _, op, _) => Some(*op),
                    _ => None,
                })
        }) {
            match op {
                BinaryOp::DivU => div_u += 1,
                BinaryOp::DivS => div_s += 1,
                BinaryOp::RemU => rem_u += 1,
                BinaryOp::RemS => rem_s += 1,
                _ => {}
            }
        }

        assert_eq!((div_u, div_s, rem_u, rem_s), (2, 3, 1, 1));
    }
}

mod expression;
mod function_call;

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
    defined_ranges: HashMap<VarId, BitSet>,
    dynamic_defined_vars: HashSet<VarId>,
    dynamic_write_vars: HashSet<VarId>,
    sparse_write_vars: HashSet<VarId>,
    local_working_vars: HashSet<VarId>,
    local_let_values: HashMap<VarId, RegisterId>,
    loop_exit_blocks: Vec<BlockId>,
    reset: Option<FfReset>,
    function_arg_stack: Vec<HashMap<VarId, Expression>>,
    function_arg_value_stack: Vec<HashMap<VarId, RegisterId>>,
    function_expression_value_stack: Vec<HashMap<TokenRange, RegisterId>>,
    function_event_arg_state_stack: Vec<HashMap<TokenRange, HashMap<VarId, Expression>>>,
    // Maps an active array formal to its call-specific register snapshot and
    // the formal working region used for O(1) dynamic element loads.
    function_array_view_stack: Vec<HashMap<VarId, FunctionArrayView>>,
    function_array_view_enabled_stack: Vec<bool>,
    runtime_errors: HashMap<i64, RuntimeErrorInfo<VarId>>,
    runtime_event_sites: Vec<RuntimeEventSite>,
    next_runtime_error_code: i64,
    runtime_error_code_map: Option<HashMap<i64, i64>>,
    runtime_event_site_base: u32,
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
            defined_ranges: HashMap::default(),
            dynamic_defined_vars: HashSet::default(),
            dynamic_write_vars: HashSet::default(),
            sparse_write_vars: HashSet::default(),
            local_working_vars,
            local_let_values: HashMap::default(),
            loop_exit_blocks: Vec::new(),
            reset: None,
            function_arg_stack: Vec::new(),
            function_arg_value_stack: Vec::new(),
            function_expression_value_stack: Vec::new(),
            function_event_arg_state_stack: Vec::new(),
            function_array_view_stack: Vec::new(),
            function_array_view_enabled_stack: Vec::new(),
            runtime_errors: HashMap::default(),
            runtime_event_sites: Vec::new(),
            next_runtime_error_code: 2000,
            runtime_error_code_map: None,
            runtime_event_site_base: 0,
            config,
        }
    }

    pub fn with_relocated_runtime_ids(
        mut self,
        runtime_error_code_map: HashMap<i64, i64>,
        runtime_event_site_base: u32,
    ) -> Self {
        self.runtime_error_code_map = Some(runtime_error_code_map);
        self.runtime_event_site_base = runtime_event_site_base;
        self
    }

    pub fn with_sparse_write_vars(mut self, sparse_write_vars: HashSet<VarId>) -> Self {
        self.sparse_write_vars = sparse_write_vars;
        self
    }

    pub fn runtime_errors(&self) -> &HashMap<i64, RuntimeErrorInfo<VarId>> {
        &self.runtime_errors
    }

    pub fn runtime_event_sites(&self) -> &Vec<RuntimeEventSite> {
        &self.runtime_event_sites
    }

    fn runtime_error(&mut self, message: impl Into<String>, signals: Vec<VarId>) -> i64 {
        let local_code = self.next_runtime_error_code;
        self.next_runtime_error_code += 1;
        let code = self
            .runtime_error_code_map
            .as_ref()
            .and_then(|mapping| mapping.get(&local_code).copied())
            .unwrap_or(local_code);
        self.runtime_errors.insert(
            code,
            RuntimeErrorInfo {
                message: message.into(),
                signals,
            },
        );
        code
    }

    fn static_string_expr(expr: &Expression) -> Option<String> {
        if !expr.comptime().r#type.is_string() {
            return None;
        }
        let value = expr.comptime().get_value().ok()?;
        byte_value_to_string(value)
    }

    fn register_runtime_event_site(
        &mut self,
        kind: RuntimeEventKind,
        args: &[SystemFunctionInput],
    ) -> u32 {
        let (template, value_args) = if args
            .first()
            .and_then(|arg| Self::static_string_expr(&arg.0))
            .is_some()
        {
            (
                args.first()
                    .and_then(|arg| Self::static_string_expr(&arg.0)),
                &args[1..],
            )
        } else {
            (None, args)
        };
        let site = RuntimeEventSite {
            kind,
            template,
            arg_widths: value_args
                .iter()
                .map(|arg| self.get_expression_width(&arg.0))
                .collect(),
            arg_signed: value_args
                .iter()
                .map(|arg| arg.0.comptime().expr_context.signed)
                .collect(),
            arg_is_string: value_args
                .iter()
                .map(|arg| arg.0.comptime().r#type.is_string())
                .collect(),
        };
        let id = self
            .runtime_event_site_base
            .checked_add(self.runtime_event_sites.len() as u32)
            .expect("runtime event site identifier overflow");
        self.runtime_event_sites.push(site);
        id
    }

    fn parse_runtime_event_expression<A>(
        &mut self,
        expr: &Expression,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        let state = self
            .get_bound_function_event_arg_state(expr.token_range())
            .cloned();
        let has_state = state.is_some();
        if let Some(state) = state {
            self.function_arg_stack.push(state);
            self.function_array_view_stack.push(HashMap::default());
            self.function_array_view_enabled_stack.push(false);
        }
        let result =
            self.parse_expression(expr, targets, domain, convert, sources, ir_builder, None);
        if has_state {
            self.function_array_view_enabled_stack.pop();
            self.function_array_view_stack.pop();
            self.function_arg_stack.pop();
        }
        result
    }

    fn emit_runtime_event<A>(
        &mut self,
        site_id: u32,
        args: &[SystemFunctionInput],
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        let value_args = if args
            .first()
            .and_then(|arg| Self::static_string_expr(&arg.0))
            .is_some()
        {
            &args[1..]
        } else {
            args
        };
        let mut regs = Vec::new();
        for arg in value_args {
            self.parse_runtime_event_expression(
                &arg.0, targets, domain, convert, sources, ir_builder,
            )?;
            regs.push(self.stack.pop_back().unwrap());
        }
        ir_builder.emit(SIRInstruction::RuntimeEvent {
            site_id,
            args: regs,
        });
        Ok(())
    }

    fn prepare_effectful_runtime_event_args<A>(
        &mut self,
        args: &[SystemFunctionInput],
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<Vec<Option<RegisterId>>, ParserError> {
        let value_args = if args
            .first()
            .and_then(|arg| Self::static_string_expr(&arg.0))
            .is_some()
        {
            &args[1..]
        } else {
            args
        };
        let last_effectful_arg = value_args.iter().rposition(|arg| {
            expression::expression_has_side_effect(&arg.0)
                || self.expression_has_runtime_effect(&arg.0)
        });
        let Some(last_effectful_arg) = last_effectful_arg else {
            return Ok(vec![None; value_args.len()]);
        };

        // Evaluate only through the last effectful argument. Earlier pure reads
        // must be snapshotted before a later effect can change them, while pure
        // trailing arguments can stay lazy in the assertion failure block.
        value_args
            .iter()
            .enumerate()
            .map(|(index, arg)| {
                if index <= last_effectful_arg {
                    self.parse_runtime_event_expression(
                        &arg.0, targets, domain, convert, sources, ir_builder,
                    )?;
                    Ok(Some(self.stack.pop_back().unwrap()))
                } else {
                    Ok(None)
                }
            })
            .collect()
    }

    fn emit_runtime_event_with_prepared_args<A>(
        &mut self,
        site_id: u32,
        args: &[SystemFunctionInput],
        prepared: Vec<Option<RegisterId>>,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        let value_args = if args
            .first()
            .and_then(|arg| Self::static_string_expr(&arg.0))
            .is_some()
        {
            &args[1..]
        } else {
            args
        };
        debug_assert_eq!(value_args.len(), prepared.len());
        let mut regs = Vec::with_capacity(value_args.len());
        for (arg, prepared) in value_args.iter().zip(prepared) {
            if let Some(reg) = prepared {
                regs.push(reg);
            } else {
                self.parse_runtime_event_expression(
                    &arg.0, targets, domain, convert, sources, ir_builder,
                )?;
                regs.push(self.stack.pop_back().unwrap());
            }
        }
        ir_builder.emit(SIRInstruction::RuntimeEvent {
            site_id,
            args: regs,
        });
        Ok(())
    }

    fn parse_system_task_statement<A>(
        &mut self,
        call: &SystemFunctionCall,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<ControlFlow, ParserError> {
        match &call.kind {
            SystemFunctionKind::Display(args) | SystemFunctionKind::Write(args) => {
                let site_id = self.register_runtime_event_site(RuntimeEventKind::Display, args);
                self.emit_runtime_event(
                    site_id, args, targets, domain, convert, sources, ir_builder,
                )?;
                Ok(ControlFlow::Continue)
            }
            SystemFunctionKind::Assert { kind, cond, args } => {
                self.parse_runtime_event_expression(
                    &cond.0, targets, domain, convert, sources, ir_builder,
                )?;
                let cond_reg = self.stack.pop_back().unwrap();
                let cond_reg = self.lower_procedural_condition(cond_reg, ir_builder);
                let pass_bb = ir_builder.new_block();
                let fail_bb = ir_builder.new_block();
                let event_kind = match kind {
                    AssertKind::Fatal => RuntimeEventKind::AssertFatal,
                    AssertKind::Continue => RuntimeEventKind::AssertContinue,
                };
                let site_id = self.register_runtime_event_site(event_kind, args);
                // Assertion message arguments are observationally eager: any
                // caller-visible effect must happen even when the assertion
                // passes. Keep pure arguments in the failure block so the
                // common passing path remains lazy.
                let prepared_args = self.prepare_effectful_runtime_event_args(
                    args, targets, domain, convert, sources, ir_builder,
                )?;
                ir_builder.seal_block(SIRTerminator::Branch {
                    cond: cond_reg,
                    true_block: (pass_bb, vec![]),
                    false_block: (fail_bb, vec![]),
                });
                ir_builder.switch_to_block(fail_bb);
                self.emit_runtime_event_with_prepared_args(
                    site_id,
                    args,
                    prepared_args,
                    targets,
                    domain,
                    convert,
                    sources,
                    ir_builder,
                )?;
                match kind {
                    AssertKind::Fatal => {
                        let message = if let Some(template) = args
                            .first()
                            .and_then(|arg| Self::static_string_expr(&arg.0))
                        {
                            template
                        } else {
                            "assertion failed".to_string()
                        };
                        let code = self.runtime_error(message, Vec::new());
                        ir_builder.seal_block(SIRTerminator::Error(code));
                    }
                    AssertKind::Continue => {
                        ir_builder.seal_block(SIRTerminator::Jump(pass_bb, vec![]));
                    }
                }
                ir_builder.switch_to_block(pass_bb);
                Ok(ControlFlow::Continue)
            }
            _ => Err(ParserError::unsupported(
                66,
                LoweringPhase::FfLowering,
                "system function call",
                format!("{call}"),
                Some(&call.comptime.token),
            )),
        }
    }

    fn get_constant_value(&self, expr: &Expression) -> Option<u64> {
        eval_constexpr(expr)?.to_u64()
    }

    fn get_constant_procedural_truth(expr: &Expression) -> Option<bool> {
        let comptime = expr.comptime();
        let is_value = matches!(expr, Expression::Term(factor) if matches!(factor.as_ref(), veryl_analyzer::ir::Factor::Value(_)));
        if !(comptime.is_const || is_value && comptime.evaluated) {
            return None;
        }

        if let Some((value, mask, width, _)) = celox_value_from_comptime_in_context(comptime, None)
        {
            let width_mask = (BigUint::from(1u8) << width) - BigUint::from(1u8);
            let known = &width_mask ^ (&mask & &width_mask);
            return Some(!(value & known).is_zero());
        }

        comptime
            .r#type
            .is_2state()
            .then(|| eval_constexpr(expr).map(|value| !value.is_zero()))
            .flatten()
    }

    fn lower_procedural_condition<A>(
        &self,
        condition: RegisterId,
        ir_builder: &mut SIRBuilder<A>,
    ) -> RegisterId {
        if matches!(
            ir_builder.register(&condition),
            RegisterType::Bit {
                width: 1,
                signed: false
            }
        ) {
            return condition;
        }

        if matches!(
            ir_builder.register(&condition),
            RegisterType::Logic { width: 1 }
        ) {
            let known_truth = ir_builder.alloc_bit(1, false);
            ir_builder.emit(SIRInstruction::Unary(
                known_truth,
                UnaryOp::ToTwoState,
                condition,
            ));
            return known_truth;
        }

        let source_is_two_state =
            matches!(ir_builder.register(&condition), RegisterType::Bit { .. });
        let truth = if source_is_two_state {
            ir_builder.alloc_bit(1, false)
        } else {
            ir_builder.alloc_logic(1)
        };
        ir_builder.emit(SIRInstruction::Unary(truth, UnaryOp::Or, condition));
        if source_is_two_state {
            truth
        } else {
            let known_truth = ir_builder.alloc_bit(1, false);
            ir_builder.emit(SIRInstruction::Unary(
                known_truth,
                UnaryOp::ToTwoState,
                truth,
            ));
            known_truth
        }
    }

    fn cast_reg_width_ext<A>(
        &self,
        ir_builder: &mut SIRBuilder<A>,
        reg: RegisterId,
        target_width: usize,
        signed: bool,
    ) -> RegisterId {
        let src_type = ir_builder.register(&reg).clone();
        let src_width = src_type.width();
        let alloc_like_source = |builder: &mut SIRBuilder<A>, width, signed| match &src_type {
            RegisterType::Logic { .. } => builder.alloc_logic(width),
            RegisterType::Bit { .. } => builder.alloc_bit(width, signed),
        };
        if src_width == target_width {
            reg
        } else if src_width < target_width {
            let dest = alloc_like_source(ir_builder, target_width, signed);
            if signed {
                let sign = alloc_like_source(ir_builder, 1, false);
                ir_builder.emit(SIRInstruction::Slice(sign, reg, src_width - 1, 1));
                let pad_width = target_width - src_width;
                let pad = if pad_width == 1 {
                    sign
                } else {
                    let ext = alloc_like_source(ir_builder, pad_width, true);
                    ir_builder.emit(SIRInstruction::Concat(
                        ext,
                        std::iter::repeat_n(sign, pad_width).collect(),
                    ));
                    ext
                };
                ir_builder.emit(SIRInstruction::Concat(dest, vec![pad, reg]));
            } else {
                ir_builder.emit(SIRInstruction::Unary(dest, UnaryOp::Ident, reg));
            }
            dest
        } else {
            let mask_val = (BigUint::from(1u64) << target_width) - BigUint::from(1u64);
            let mask = ir_builder.alloc_bit(target_width, false);
            ir_builder.emit(SIRInstruction::Imm(mask, SIRValue::new(mask_val)));
            let dest = alloc_like_source(ir_builder, target_width, signed);
            ir_builder.emit(SIRInstruction::Binary(dest, reg, BinaryOp::And, mask));
            dest
        }
    }

    fn get_expression_width(&self, expr: &Expression) -> usize {
        crate::context_width::get_expr_width(expr)
            .or_else(|| expr.comptime().r#type.total_width())
            .unwrap_or(64)
    }

    // expression / function-call lowering is split into submodules:
    // - parser/ff/expression.rs
    // - parser/ff/function_call.rs
    fn parse_statement_list<A>(
        &mut self,
        stmts: &[Statement],
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<ControlFlow, ParserError> {
        for stmt in stmts {
            let flow = self.parse_statement(stmt, targets, domain, convert, sources, ir_builder)?;
            if matches!(flow, ControlFlow::Break) {
                return Ok(ControlFlow::Break);
            }
        }
        Ok(ControlFlow::Continue)
    }

    fn parse_statement_refs<A>(
        &mut self,
        stmts: &[&Statement],
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<ControlFlow, ParserError> {
        for stmt in stmts {
            let flow = self.parse_statement(stmt, targets, domain, convert, sources, ir_builder)?;
            if matches!(flow, ControlFlow::Break) {
                return Ok(ControlFlow::Break);
            }
        }
        Ok(ControlFlow::Continue)
    }

    fn parse_if_statement<A>(
        &mut self,
        stmt: &IfStatement,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<ControlFlow, ParserError> {
        // Constant folding: if condition is compile-time constant, inline the appropriate side
        if let Some(cond_is_true) = Self::get_constant_procedural_truth(&stmt.cond) {
            let side = if cond_is_true {
                &stmt.true_side
            } else {
                &stmt.false_side
            };
            return self.parse_statement_list(side, targets, domain, convert, sources, ir_builder);
        }

        // 1. Evaluate condition expression
        self.parse_expression(
            &stmt.cond, targets, domain, convert, sources, ir_builder, None,
        )?;
        let cond_reg = self.stack.pop_back().unwrap();
        let cond_reg = self.lower_procedural_condition(cond_reg, ir_builder);

        let then_bb = ir_builder.new_block();
        let else_bb = ir_builder.new_block();
        let merge_bb = ir_builder.new_block();

        // --- Create snapshot ---
        // Save both static (BitSet) and dynamic (HashSet) states
        let pre_if_defined = self.defined_ranges.clone();
        let pre_if_dynamic = self.dynamic_defined_vars.clone(); // 【追加】

        // 2. Terminate current block with Branch
        ir_builder.seal_block(SIRTerminator::Branch {
            cond: cond_reg,
            true_block: (then_bb, vec![]),
            false_block: (else_bb, vec![]),
        });

        // 3. Then Path
        ir_builder.switch_to_block(then_bb);
        let then_flow = self.parse_statement_list(
            &stmt.true_side,
            targets,
            domain,
            convert,
            sources,
            ir_builder,
        )?;
        // Collect state at the end of Then, and restore state at the beginning
        let then_defined = std::mem::replace(&mut self.defined_ranges, pre_if_defined.clone());
        let then_dynamic = std::mem::replace(&mut self.dynamic_defined_vars, pre_if_dynamic); // 【追加】

        if matches!(then_flow, ControlFlow::Continue) {
            ir_builder.seal_block(SIRTerminator::Jump(merge_bb, vec![]));
        }

        // 4. Else Path
        ir_builder.switch_to_block(else_bb);
        let else_flow = self.parse_statement_list(
            &stmt.false_side,
            targets,
            domain,
            convert,
            sources,
            ir_builder,
        )?;
        // Collect state at the end of Else
        let else_defined = std::mem::take(&mut self.defined_ranges);
        let else_dynamic = std::mem::take(&mut self.dynamic_defined_vars); // 【追加】

        if matches!(else_flow, ControlFlow::Continue) {
            ir_builder.seal_block(SIRTerminator::Jump(merge_bb, vec![]));
        }

        // 5. Merge logic
        match (then_flow, else_flow) {
            (ControlFlow::Continue, ControlFlow::Continue) => {
                self.defined_ranges = self.intersect_defined_states(then_defined, else_defined);
                self.dynamic_defined_vars = self.intersect_dynamic_vars(then_dynamic, else_dynamic);
                ir_builder.switch_to_block(merge_bb);
                Ok(ControlFlow::Continue)
            }
            (ControlFlow::Continue, ControlFlow::Break) => {
                self.defined_ranges = then_defined;
                self.dynamic_defined_vars = then_dynamic;
                ir_builder.switch_to_block(merge_bb);
                Ok(ControlFlow::Continue)
            }
            (ControlFlow::Break, ControlFlow::Continue) => {
                self.defined_ranges = else_defined;
                self.dynamic_defined_vars = else_dynamic;
                ir_builder.switch_to_block(merge_bb);
                Ok(ControlFlow::Continue)
            }
            (ControlFlow::Break, ControlFlow::Break) => Ok(ControlFlow::Break),
        }
    }

    fn parse_case_statement<A>(
        &mut self,
        stmt: &CaseStatement,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<ControlFlow, ParserError> {
        self.parse_case_arm(stmt, 0, targets, domain, convert, sources, ir_builder)
    }

    fn parse_case_arm<A>(
        &mut self,
        stmt: &CaseStatement,
        arm_index: usize,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<ControlFlow, ParserError> {
        let Some(arm) = stmt.arms.get(arm_index) else {
            return self.parse_statement_list(
                &stmt.default,
                targets,
                domain,
                convert,
                sources,
                ir_builder,
            );
        };

        let cond = case_arm_condition_expr(&stmt.case_target, &arm.patterns);
        if let Some(cond_is_true) = Self::get_constant_procedural_truth(&cond) {
            return if cond_is_true {
                self.parse_statement_list(&arm.body, targets, domain, convert, sources, ir_builder)
            } else {
                self.parse_case_arm(
                    stmt,
                    arm_index + 1,
                    targets,
                    domain,
                    convert,
                    sources,
                    ir_builder,
                )
            };
        }

        self.parse_expression(&cond, targets, domain, convert, sources, ir_builder, None)?;
        let cond_reg = self.stack.pop_back().unwrap();
        let cond_reg = self.lower_procedural_condition(cond_reg, ir_builder);

        let then_bb = ir_builder.new_block();
        let else_bb = ir_builder.new_block();
        let merge_bb = ir_builder.new_block();

        let pre_case_defined = self.defined_ranges.clone();
        let pre_case_dynamic = self.dynamic_defined_vars.clone();

        ir_builder.seal_block(SIRTerminator::Branch {
            cond: cond_reg,
            true_block: (then_bb, vec![]),
            false_block: (else_bb, vec![]),
        });

        ir_builder.switch_to_block(then_bb);
        let then_flow =
            self.parse_statement_list(&arm.body, targets, domain, convert, sources, ir_builder)?;
        let then_defined = std::mem::replace(&mut self.defined_ranges, pre_case_defined.clone());
        let then_dynamic = std::mem::replace(&mut self.dynamic_defined_vars, pre_case_dynamic);
        if matches!(then_flow, ControlFlow::Continue) {
            ir_builder.seal_block(SIRTerminator::Jump(merge_bb, vec![]));
        }

        ir_builder.switch_to_block(else_bb);
        let else_flow = self.parse_case_arm(
            stmt,
            arm_index + 1,
            targets,
            domain,
            convert,
            sources,
            ir_builder,
        )?;
        let else_defined = std::mem::take(&mut self.defined_ranges);
        let else_dynamic = std::mem::take(&mut self.dynamic_defined_vars);
        if matches!(else_flow, ControlFlow::Continue) {
            ir_builder.seal_block(SIRTerminator::Jump(merge_bb, vec![]));
        }

        match (then_flow, else_flow) {
            (ControlFlow::Continue, ControlFlow::Continue) => {
                self.defined_ranges = self.intersect_defined_states(then_defined, else_defined);
                self.dynamic_defined_vars = self.intersect_dynamic_vars(then_dynamic, else_dynamic);
                ir_builder.switch_to_block(merge_bb);
                Ok(ControlFlow::Continue)
            }
            (ControlFlow::Continue, ControlFlow::Break) => {
                self.defined_ranges = then_defined;
                self.dynamic_defined_vars = then_dynamic;
                ir_builder.switch_to_block(merge_bb);
                Ok(ControlFlow::Continue)
            }
            (ControlFlow::Break, ControlFlow::Continue) => {
                self.defined_ranges = else_defined;
                self.dynamic_defined_vars = else_dynamic;
                ir_builder.switch_to_block(merge_bb);
                Ok(ControlFlow::Continue)
            }
            (ControlFlow::Break, ControlFlow::Break) => Ok(ControlFlow::Break),
        }
    }

    /// Helper to take intersection of dynamic defined variables
    fn intersect_dynamic_vars(
        &self,
        mut left: HashSet<VarId>,
        right: HashSet<VarId>,
    ) -> HashSet<VarId> {
        left.retain(|var_id| right.contains(var_id));
        left
    }

    /// Helper to take intersection of defined states of two paths
    fn intersect_defined_states(
        &self,
        mut left: HashMap<VarId, BitSet>,
        right: HashMap<VarId, BitSet>,
    ) -> HashMap<VarId, BitSet> {
        let mut result = HashMap::default();

        // Take bitwise AND only for variables existing in both
        for (var_id, left_bits) in left.drain() {
            if let Some(right_bits) = right.get(&var_id) {
                // If the result of AND is not empty, keep it as "defined" after merging
                if left_bits.intersection(right_bits).next().is_some() {
                    result.insert(var_id, left_bits);
                }
            }
        }
        result
    }
    fn parse_statement<A>(
        &mut self,
        stmt: &Statement,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<ControlFlow, ParserError> {
        match stmt {
            Statement::Assign(assign_statement) => {
                self.parse_assign_statement(
                    assign_statement,
                    targets,
                    domain,
                    convert,
                    sources,
                    ir_builder,
                )?;
            }
            Statement::If(stmt) => {
                return self
                    .parse_if_statement(stmt, targets, domain, convert, sources, ir_builder);
            }
            Statement::Case(stmt) => {
                return self
                    .parse_case_statement(stmt, targets, domain, convert, sources, ir_builder);
            }
            Statement::IfReset(stmt) => {
                return self
                    .parse_if_reset_statement(stmt, targets, domain, convert, sources, ir_builder);
            }
            Statement::Null => {}
            Statement::SystemFunctionCall(call) => {
                return self.parse_system_task_statement(
                    call, targets, domain, convert, sources, ir_builder,
                );
            }
            Statement::FunctionCall(call) => {
                self.parse_function_call_statement(
                    call, targets, domain, convert, sources, ir_builder,
                )?;
            }
            Statement::For(f) => {
                self.parse_for_statement(f, targets, domain, convert, sources, ir_builder)?;
            }
            Statement::Break => {
                let Some(exit_bb) = self.loop_exit_blocks.last().copied() else {
                    return Err(ParserError::illegal_context(
                        "statement in always_ff",
                        "break outside loop".to_string(),
                        None,
                    ));
                };
                ir_builder.seal_block(SIRTerminator::Jump(exit_bb, vec![]));
                return Ok(ControlFlow::Break);
            }
            Statement::TbMethodCall(_) | Statement::Unsupported(_) => {
                return Err(ParserError::illegal_context(
                    "statement in always_ff",
                    format!("{stmt}"),
                    None,
                ));
            }
        }
        Ok(ControlFlow::Continue)
    }

    fn loop_bound_status(bound: &ForBound, width: usize, signed: bool) -> Option<LoopBoundStatus> {
        let value = match bound {
            ForBound::Const(v) => BigInt::from(*v),
            ForBound::Expression(expr) => {
                if !expr.comptime().is_const {
                    return None;
                }
                let value = expr.comptime().get_value().ok()?;
                match value {
                    Value::U64(v) => {
                        if v.signed {
                            BigInt::from(v.to_i64()?)
                        } else {
                            BigInt::from(v.to_u64()?)
                        }
                    }
                    Value::BigUint(v) => {
                        if v.signed {
                            v.to_bigint()?
                        } else {
                            BigInt::from_biguint(Sign::Plus, (*v.payload).clone())
                        }
                    }
                }
            }
        };

        if signed {
            let max = (BigInt::from(1u8) << (width.saturating_sub(1))) - BigInt::from(1u8);
            let min = -(BigInt::from(1u8) << (width.saturating_sub(1)));
            Some(if value >= min && value <= max {
                LoopBoundStatus::FitsLoopType
            } else if value == max + BigInt::from(1u8) {
                LoopBoundStatus::ExclusiveUpperSentinel
            } else {
                LoopBoundStatus::OutOfRange
            })
        } else {
            let max = (BigUint::from(1u8) << width) - BigUint::from(1u8);
            let max = BigInt::from_biguint(Sign::Plus, max);
            Some(if value.sign() != Sign::Minus && value <= max {
                LoopBoundStatus::FitsLoopType
            } else if value == max + BigInt::from(1u8) {
                LoopBoundStatus::ExclusiveUpperSentinel
            } else {
                LoopBoundStatus::OutOfRange
            })
        }
    }

    fn loop_bound_width(&self, bound: &ForBound, signed: bool) -> Option<usize> {
        match bound {
            ForBound::Const(v) => {
                let value = BigInt::from(*v);
                Some(if signed {
                    if value.sign() == Sign::Minus {
                        let magnitude = (-value - BigInt::from(1u8)).to_biguint()?;
                        magnitude.bits() as usize + 1
                    } else {
                        value.to_biguint()?.bits() as usize + 1
                    }
                } else {
                    let magnitude = value.to_biguint()?;
                    (magnitude.bits() as usize).max(1)
                })
            }
            ForBound::Expression(expr) => {
                let comptime_width = expr.comptime().r#type.total_width();
                let context_width = Some(expr.comptime().expr_context.width).filter(|w| *w > 0);
                Some(
                    comptime_width
                        .into_iter()
                        .chain(context_width)
                        .chain(std::iter::once(self.get_expression_width(expr)))
                        .max()
                        .unwrap_or(1),
                )
            }
        }
    }

    fn step_math_width(base_width: usize, stepped_op: Option<Op>, step: usize) -> usize {
        match stepped_op {
            Some(Op::Mul) => {
                let step_bits = (usize::BITS as usize - step.leading_zeros() as usize).max(1);
                base_width.saturating_add(step_bits)
            }
            Some(Op::LogicShiftL | Op::ArithShiftL) => base_width.saturating_add(step.max(1)),
            Some(Op::BitOr | Op::BitXor) => base_width,
            Some(Op::Add) | None => {
                if step <= 1 {
                    return base_width;
                }
                let step_bits = (usize::BITS as usize - step.leading_zeros() as usize).max(1);
                base_width.saturating_add(step_bits)
            }
            Some(_) => base_width,
        }
    }

    fn truncate_usize_to_width(value: usize, width: usize) -> usize {
        if width >= usize::BITS as usize {
            value
        } else if width == 0 {
            0
        } else {
            value & ((1usize << width) - 1)
        }
    }

    fn emit_loop_value_fits<A>(
        ir_builder: &mut SIRBuilder<A>,
        value_reg: RegisterId,
        compare_width: usize,
        loop_width: usize,
        loop_signed: bool,
        allow_exclusive_upper_sentinel: bool,
    ) -> RegisterId {
        debug_assert!(compare_width >= loop_width);
        let one = BigUint::from(1u8);
        if loop_signed {
            let min_payload = (&one << compare_width) - (&one << (loop_width - 1));
            let max_payload = (&one << (loop_width - 1)) - &one;

            let min_reg = ir_builder.alloc_bit(compare_width, true);
            ir_builder.emit(SIRInstruction::Imm(min_reg, SIRValue::new(min_payload)));
            let max_reg = ir_builder.alloc_bit(compare_width, true);
            ir_builder.emit(SIRInstruction::Imm(
                max_reg,
                SIRValue::new(max_payload.clone()),
            ));

            let ge_min = ir_builder.alloc_bit(1, false);
            ir_builder.emit(SIRInstruction::Binary(
                ge_min,
                value_reg,
                BinaryOp::GeS,
                min_reg,
            ));
            let le_max = ir_builder.alloc_bit(1, false);
            ir_builder.emit(SIRInstruction::Binary(
                le_max,
                value_reg,
                BinaryOp::LeS,
                max_reg,
            ));
            let fits_reg = ir_builder.alloc_bit(1, false);
            ir_builder.emit(SIRInstruction::Binary(
                fits_reg,
                ge_min,
                BinaryOp::LogicAnd,
                le_max,
            ));

            if allow_exclusive_upper_sentinel {
                let sentinel_reg = ir_builder.alloc_bit(compare_width, true);
                ir_builder.emit(SIRInstruction::Imm(
                    sentinel_reg,
                    SIRValue::new(max_payload + &one),
                ));
                let is_sentinel = ir_builder.alloc_bit(1, false);
                ir_builder.emit(SIRInstruction::Binary(
                    is_sentinel,
                    value_reg,
                    BinaryOp::Eq,
                    sentinel_reg,
                ));
                let allowed_reg = ir_builder.alloc_bit(1, false);
                ir_builder.emit(SIRInstruction::Binary(
                    allowed_reg,
                    fits_reg,
                    BinaryOp::LogicOr,
                    is_sentinel,
                ));
                allowed_reg
            } else {
                fits_reg
            }
        } else {
            let max_payload = (&one << loop_width) - &one;
            let max_reg = ir_builder.alloc_bit(compare_width, false);
            ir_builder.emit(SIRInstruction::Imm(
                max_reg,
                SIRValue::new(max_payload.clone()),
            ));
            let fits_reg = ir_builder.alloc_bit(1, false);
            ir_builder.emit(SIRInstruction::Binary(
                fits_reg,
                value_reg,
                BinaryOp::LeU,
                max_reg,
            ));

            if allow_exclusive_upper_sentinel {
                let sentinel_reg = ir_builder.alloc_bit(compare_width, false);
                ir_builder.emit(SIRInstruction::Imm(
                    sentinel_reg,
                    SIRValue::new(max_payload + &one),
                ));
                let is_sentinel = ir_builder.alloc_bit(1, false);
                ir_builder.emit(SIRInstruction::Binary(
                    is_sentinel,
                    value_reg,
                    BinaryOp::Eq,
                    sentinel_reg,
                ));
                let allowed_reg = ir_builder.alloc_bit(1, false);
                ir_builder.emit(SIRInstruction::Binary(
                    allowed_reg,
                    fits_reg,
                    BinaryOp::LogicOr,
                    is_sentinel,
                ));
                allowed_reg
            } else {
                fits_reg
            }
        }
    }

    fn parse_for_bound<A>(
        &mut self,
        bound: &ForBound,
        canonical_width: usize,
        width: usize,
        signed: bool,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<RegisterId, ParserError> {
        match bound {
            ForBound::Const(v) => {
                let reg = ir_builder.alloc_bit(width, signed);
                ir_builder.emit(SIRInstruction::Imm(reg, SIRValue::new(*v as u64)));
                Ok(reg)
            }
            ForBound::Expression(expr) => {
                self.parse_expression(expr, targets, domain, convert, sources, ir_builder, None)?;
                let reg = self.stack.pop_back().unwrap();
                let source_signed = expr.comptime().r#type.signed;
                let extend_signed = source_signed && signed;
                let canonical =
                    self.cast_reg_width_ext(ir_builder, reg, canonical_width, extend_signed);
                let canonical = match ir_builder.register(&canonical) {
                    RegisterType::Bit {
                        width: reg_width,
                        signed: reg_signed,
                    } if *reg_width == canonical_width && *reg_signed == signed => canonical,
                    _ => {
                        let bit_reg = ir_builder.alloc_bit(canonical_width, signed);
                        ir_builder.emit(SIRInstruction::Unary(bit_reg, UnaryOp::Ident, canonical));
                        bit_reg
                    }
                };
                let widened = self.cast_reg_width_ext(ir_builder, canonical, width, signed);
                match ir_builder.register(&widened) {
                    RegisterType::Bit {
                        width: reg_width,
                        signed: reg_signed,
                    } if *reg_width == width && *reg_signed == signed => Ok(widened),
                    _ => {
                        let bit_reg = ir_builder.alloc_bit(width, signed);
                        ir_builder.emit(SIRInstruction::Unary(bit_reg, UnaryOp::Ident, widened));
                        Ok(bit_reg)
                    }
                }
            }
        }
    }

    fn parse_for_statement<A>(
        &mut self,
        stmt: &ForStatement,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        let base_loop_width =
            resolve_total_width(self.module, &self.module.variables[&stmt.var_id])?;
        let loop_signed = stmt.var_type.signed;

        let (start_bound, end_bound, inclusive, step, reverse, stepped_op, start_const, end_const) =
            match &stmt.range {
                ForRange::Forward {
                    start,
                    end,
                    inclusive,
                    step,
                } => (
                    start,
                    end,
                    *inclusive,
                    *step,
                    false,
                    None,
                    Self::bound_const_value(start),
                    Self::bound_const_value(end),
                ),
                ForRange::Reverse {
                    start,
                    end,
                    inclusive,
                    step,
                } => (
                    start,
                    end,
                    *inclusive,
                    *step,
                    true,
                    None,
                    Self::bound_const_value(start),
                    Self::bound_const_value(end),
                ),
                ForRange::Stepped {
                    start,
                    end,
                    inclusive,
                    step,
                    op,
                } => (
                    start,
                    end,
                    *inclusive,
                    *step,
                    false,
                    Some(*op),
                    Self::bound_const_value(start),
                    Self::bound_const_value(end),
                ),
            };
        let loop_var_name = veryl_parser::resource_table::get_str_value(stmt.var_name)
            .unwrap_or_else(|| "<unknown>".to_string());
        let non_progress_message =
            format!("Non-progressing for loop in always_ff (loop variable `{loop_var_name}`)");
        let range_message = format!(
            "For loop value exceeds loop variable range in always_ff (loop variable `{loop_var_name}`)"
        );
        let loop_width = base_loop_width.max(1);

        let const_empty = Self::const_range_is_empty(reverse, start_const, end_const, inclusive);
        let const_singleton =
            Self::const_range_is_singleton(reverse, start_const, end_const, inclusive);
        if start_const.is_some()
            && end_const.is_some()
            && Self::step_can_stall(reverse, stepped_op, step, start_const, loop_width)
            && !const_empty
            && !const_singleton
        {
            return Err(ParserError::illegal_context(
                "non-progressing for loop in always_ff",
                format!("{:?}", stmt.var_name),
                Some(&stmt.token),
            ));
        }

        let start_bound_width = self
            .loop_bound_width(start_bound, loop_signed)
            .unwrap_or(loop_width);
        let end_bound_width = self
            .loop_bound_width(end_bound, loop_signed)
            .unwrap_or(loop_width);
        let start_status = Self::loop_bound_status(start_bound, loop_width, loop_signed);
        let end_status = Self::loop_bound_status(end_bound, loop_width, loop_signed);
        let uses_exclusive_end_sentinel = !inclusive;
        // Veryl now models loop variables as i32. Reject statically invalid
        // bounds, but keep supporting the exclusive upper sentinel used for
        // full-range iteration such as `0..256` on an 8-bit loop variable.
        if matches!(
            start_status,
            Some(LoopBoundStatus::OutOfRange | LoopBoundStatus::ExclusiveUpperSentinel)
        ) || matches!(end_status, Some(LoopBoundStatus::OutOfRange))
            || (inclusive && end_status == Some(LoopBoundStatus::ExclusiveUpperSentinel))
        {
            return Err(ParserError::illegal_context(
                "for loop bound exceeding i32 loop variable",
                format!("{:?}", stmt.var_name),
                Some(&stmt.token),
            ));
        }
        let counter_width = loop_width.max(1);
        let bound_width = counter_width.max(start_bound_width).max(end_bound_width);
        let widen_inclusive = inclusive && !loop_signed;
        let compare_width = if widen_inclusive {
            bound_width.saturating_add(1)
        } else if uses_exclusive_end_sentinel {
            bound_width.max(counter_width.saturating_add(1))
        } else {
            bound_width
        };
        let math_width = if reverse {
            Self::step_math_width(compare_width, Some(Op::Add), step)
        } else {
            Self::step_math_width(compare_width, stepped_op, step)
        };

        let start_reg = self.parse_for_bound(
            start_bound,
            compare_width,
            compare_width,
            loop_signed,
            targets,
            domain,
            convert,
            sources,
            ir_builder,
        )?;
        let end_reg = self.parse_for_bound(
            end_bound,
            compare_width,
            compare_width,
            loop_signed,
            targets,
            domain,
            convert,
            sources,
            ir_builder,
        )?;

        let one_reg = ir_builder.alloc_bit(compare_width, loop_signed);
        ir_builder.emit(SIRInstruction::Imm(one_reg, SIRValue::new(1u64)));
        let end_limit = if widen_inclusive {
            let reg = ir_builder.alloc_bit(compare_width, loop_signed);
            ir_builder.emit(SIRInstruction::Binary(reg, end_reg, BinaryOp::Add, one_reg));
            reg
        } else {
            end_reg
        };

        let init_reg = if reverse && !inclusive {
            let raw = ir_builder.alloc_bit(compare_width, loop_signed);
            ir_builder.emit(SIRInstruction::Binary(raw, end_reg, BinaryOp::Sub, one_reg));
            let visible = self.cast_reg_width_ext(ir_builder, raw, loop_width, loop_signed);
            self.cast_reg_width_ext(ir_builder, visible, compare_width, loop_signed)
        } else if reverse {
            end_reg
        } else {
            start_reg
        };

        let header_counter = ir_builder.alloc_bit(compare_width, loop_signed);
        let fitcheck_counter = ir_builder.alloc_bit(compare_width, loop_signed);
        let body_counter = ir_builder.alloc_bit(compare_width, loop_signed);
        let needs_range_check = compare_width != loop_width;
        let precheck_bb = (!reverse && needs_range_check).then(|| ir_builder.new_block());
        let reverse_precheck_bb = (reverse && needs_range_check).then(|| ir_builder.new_block());
        let empty_exit_counter = ir_builder.alloc_bit(compare_width, loop_signed);
        let empty_exit_check_bb =
            needs_range_check.then(|| ir_builder.new_block_with(vec![empty_exit_counter]));
        let header_bb = ir_builder.new_block_with(vec![header_counter]);
        let fitcheck_bb = ir_builder.new_block_with(vec![fitcheck_counter]);
        let body_bb = ir_builder.new_block_with(vec![body_counter]);
        let range_error_bb = needs_range_check.then(|| ir_builder.new_block());
        let exit_bb = ir_builder.new_block();
        if let Some(precheck_bb) = precheck_bb {
            ir_builder.seal_block(SIRTerminator::Jump(precheck_bb, vec![]));
        } else if let Some(reverse_precheck_bb) = reverse_precheck_bb {
            ir_builder.seal_block(SIRTerminator::Jump(reverse_precheck_bb, vec![]));
        } else {
            ir_builder.seal_block(SIRTerminator::Jump(header_bb, vec![init_reg]));
        }

        let pre_loop_defined = self.defined_ranges.clone();
        let pre_loop_dynamic = self.dynamic_defined_vars.clone();

        if let (Some(precheck_bb), Some(empty_exit_check_bb), Some(range_error_bb)) =
            (precheck_bb, empty_exit_check_bb, range_error_bb)
        {
            ir_builder.switch_to_block(precheck_bb);
            let end_allowed_reg = if compare_width > 64 {
                Self::emit_loop_value_fits(
                    ir_builder,
                    end_reg,
                    compare_width,
                    loop_width,
                    loop_signed,
                    !inclusive,
                )
            } else {
                let end_visible =
                    self.cast_reg_width_ext(ir_builder, end_reg, loop_width, loop_signed);
                let end_roundtrip =
                    self.cast_reg_width_ext(ir_builder, end_visible, compare_width, loop_signed);
                let end_fits_reg = ir_builder.alloc_bit(1, false);
                ir_builder.emit(SIRInstruction::Binary(
                    end_fits_reg,
                    end_reg,
                    BinaryOp::Eq,
                    end_roundtrip,
                ));
                if !inclusive {
                    let sentinel_reg = ir_builder.alloc_bit(compare_width, loop_signed);
                    let sentinel_value = if loop_signed {
                        1u64 << (loop_width - 1)
                    } else {
                        1u64 << loop_width
                    };
                    ir_builder.emit(SIRInstruction::Imm(
                        sentinel_reg,
                        SIRValue::new(sentinel_value),
                    ));
                    let end_is_sentinel_reg = ir_builder.alloc_bit(1, false);
                    ir_builder.emit(SIRInstruction::Binary(
                        end_is_sentinel_reg,
                        end_reg,
                        BinaryOp::Eq,
                        sentinel_reg,
                    ));
                    let allowed_reg = ir_builder.alloc_bit(1, false);
                    ir_builder.emit(SIRInstruction::Binary(
                        allowed_reg,
                        end_fits_reg,
                        BinaryOp::LogicOr,
                        end_is_sentinel_reg,
                    ));
                    allowed_reg
                } else {
                    end_fits_reg
                }
            };
            let precheck_pass_bb = ir_builder.new_block();
            ir_builder.seal_block(SIRTerminator::Branch {
                cond: end_allowed_reg,
                true_block: (precheck_pass_bb, vec![]),
                false_block: (range_error_bb, vec![]),
            });
            ir_builder.switch_to_block(precheck_pass_bb);
            let cond_reg = ir_builder.alloc_bit(1, false);
            ir_builder.emit(SIRInstruction::Binary(
                cond_reg,
                start_reg,
                if loop_signed {
                    if inclusive {
                        BinaryOp::LeS
                    } else {
                        BinaryOp::LtS
                    }
                } else {
                    BinaryOp::LtU
                },
                end_limit,
            ));
            // Keep the initial widened-range decision in the precheck block so
            // we do not reuse the same block-param SSA value across multiple blocks.
            ir_builder.seal_block(SIRTerminator::Branch {
                cond: cond_reg,
                true_block: (fitcheck_bb, vec![start_reg]),
                false_block: (empty_exit_check_bb, vec![start_reg]),
            });
        }

        if let (Some(reverse_precheck_bb), Some(range_error_bb)) =
            (reverse_precheck_bb, range_error_bb)
        {
            ir_builder.switch_to_block(reverse_precheck_bb);
            let start_fits = Self::emit_loop_value_fits(
                ir_builder,
                start_reg,
                compare_width,
                loop_width,
                loop_signed,
                false,
            );
            let end_fits = Self::emit_loop_value_fits(
                ir_builder,
                end_reg,
                compare_width,
                loop_width,
                loop_signed,
                !inclusive,
            );
            let bounds_fit = ir_builder.alloc_bit(1, false);
            ir_builder.emit(SIRInstruction::Binary(
                bounds_fit,
                start_fits,
                BinaryOp::LogicAnd,
                end_fits,
            ));
            ir_builder.seal_block(SIRTerminator::Branch {
                cond: bounds_fit,
                true_block: (header_bb, vec![init_reg]),
                false_block: (range_error_bb, vec![]),
            });
        }

        ir_builder.switch_to_block(header_bb);
        if reverse {
            let in_range = ir_builder.alloc_bit(1, false);
            ir_builder.emit(SIRInstruction::Binary(
                in_range,
                header_counter,
                if loop_signed {
                    BinaryOp::GeS
                } else {
                    BinaryOp::GeU
                },
                start_reg,
            ));
            ir_builder.seal_block(SIRTerminator::Branch {
                cond: in_range,
                true_block: (fitcheck_bb, vec![header_counter]),
                false_block: empty_exit_check_bb
                    .map_or((exit_bb, vec![]), |block| (block, vec![header_counter])),
            });
        } else {
            let cond_reg = ir_builder.alloc_bit(1, false);
            ir_builder.emit(SIRInstruction::Binary(
                cond_reg,
                header_counter,
                if loop_signed {
                    if inclusive {
                        BinaryOp::LeS
                    } else {
                        BinaryOp::LtS
                    }
                } else {
                    BinaryOp::LtU
                },
                end_limit,
            ));
            ir_builder.seal_block(SIRTerminator::Branch {
                cond: cond_reg,
                true_block: (fitcheck_bb, vec![header_counter]),
                false_block: (exit_bb, vec![]),
            });
        }

        if let (Some(empty_exit_check_bb), Some(range_error_bb)) =
            (empty_exit_check_bb, range_error_bb)
        {
            ir_builder.switch_to_block(empty_exit_check_bb);
            let empty_fits_reg = if compare_width > 64 {
                Self::emit_loop_value_fits(
                    ir_builder,
                    empty_exit_counter,
                    compare_width,
                    loop_width,
                    loop_signed,
                    false,
                )
            } else {
                let empty_visible = self.cast_reg_width_ext(
                    ir_builder,
                    empty_exit_counter,
                    loop_width,
                    loop_signed,
                );
                let empty_roundtrip =
                    self.cast_reg_width_ext(ir_builder, empty_visible, compare_width, loop_signed);
                let empty_fits_reg = ir_builder.alloc_bit(1, false);
                ir_builder.emit(SIRInstruction::Binary(
                    empty_fits_reg,
                    empty_exit_counter,
                    BinaryOp::Eq,
                    empty_roundtrip,
                ));
                empty_fits_reg
            };
            let empty_start_fits_reg = if compare_width > 64 {
                Self::emit_loop_value_fits(
                    ir_builder,
                    start_reg,
                    compare_width,
                    loop_width,
                    loop_signed,
                    false,
                )
            } else {
                let empty_start_visible =
                    self.cast_reg_width_ext(ir_builder, start_reg, loop_width, loop_signed);
                let empty_start_roundtrip = self.cast_reg_width_ext(
                    ir_builder,
                    empty_start_visible,
                    compare_width,
                    loop_signed,
                );
                let empty_start_fits_reg = ir_builder.alloc_bit(1, false);
                ir_builder.emit(SIRInstruction::Binary(
                    empty_start_fits_reg,
                    start_reg,
                    BinaryOp::Eq,
                    empty_start_roundtrip,
                ));
                empty_start_fits_reg
            };
            let empty_allowed_reg = ir_builder.alloc_bit(1, false);
            ir_builder.emit(SIRInstruction::Binary(
                empty_allowed_reg,
                empty_fits_reg,
                BinaryOp::LogicAnd,
                empty_start_fits_reg,
            ));
            ir_builder.seal_block(SIRTerminator::Branch {
                cond: empty_allowed_reg,
                true_block: (exit_bb, vec![]),
                false_block: (range_error_bb, vec![]),
            });
        }

        ir_builder.switch_to_block(fitcheck_bb);
        let fitcheck_visible_reg =
            self.cast_reg_width_ext(ir_builder, fitcheck_counter, loop_width, loop_signed);
        // Publish the loop variable before entering the body block so the
        // body itself stays a single widened block for native codegen.
        ir_builder.emit(SIRInstruction::Store(
            convert(stmt.var_id, domain.region()),
            SIROffset::Static(0),
            loop_width,
            fitcheck_visible_reg,
            Vec::new(),
            Vec::new(),
        ));
        if let Some(range_error_bb) = range_error_bb {
            let fits_loop_reg = if compare_width > 64 {
                Self::emit_loop_value_fits(
                    ir_builder,
                    fitcheck_counter,
                    compare_width,
                    loop_width,
                    loop_signed,
                    false,
                )
            } else {
                let visible_roundtrip = self.cast_reg_width_ext(
                    ir_builder,
                    fitcheck_visible_reg,
                    compare_width,
                    loop_signed,
                );
                let fits_loop_reg = ir_builder.alloc_bit(1, false);
                ir_builder.emit(SIRInstruction::Binary(
                    fits_loop_reg,
                    fitcheck_counter,
                    BinaryOp::Eq,
                    visible_roundtrip,
                ));
                fits_loop_reg
            };
            ir_builder.seal_block(SIRTerminator::Branch {
                cond: fits_loop_reg,
                true_block: (body_bb, vec![fitcheck_counter]),
                false_block: (range_error_bb, vec![]),
            });
        } else {
            ir_builder.seal_block(SIRTerminator::Jump(body_bb, vec![fitcheck_counter]));
        }
        if let Some(range_error_bb) = range_error_bb {
            ir_builder.switch_to_block(range_error_bb);
            let error_code = self.runtime_error(range_message, vec![stmt.var_id]);
            ir_builder.seal_block(SIRTerminator::Error(error_code));
        }
        ir_builder.switch_to_block(body_bb);
        self.local_working_vars.insert(stmt.var_id);

        let mut local_defined = self.defined_ranges.clone();
        local_defined.insert(stmt.var_id, (0..loop_width).collect());
        let prev_defined = std::mem::replace(&mut self.defined_ranges, local_defined);
        let mut local_dynamic = self.dynamic_defined_vars.clone();
        local_dynamic.insert(stmt.var_id);
        let prev_dynamic = std::mem::replace(&mut self.dynamic_defined_vars, local_dynamic);

        self.loop_exit_blocks.push(exit_bb);
        let body_flow =
            self.parse_statement_list(&stmt.body, targets, domain, convert, sources, ir_builder)?;
        self.loop_exit_blocks.pop();

        self.defined_ranges = prev_defined;
        self.dynamic_defined_vars = prev_dynamic;

        if matches!(body_flow, ControlFlow::Break) {
            self.local_working_vars.remove(&stmt.var_id);
            ir_builder.switch_to_block(exit_bb);
            self.defined_ranges = pre_loop_defined;
            self.dynamic_defined_vars = pre_loop_dynamic;
            return Ok(());
        }

        if !reverse {
            let step_width = if matches!(stepped_op, Some(Op::BitOr | Op::BitXor)) {
                loop_width
            } else {
                math_width
            };
            let current_step =
                self.cast_reg_width_ext(ir_builder, body_counter, step_width, loop_signed);
            let step_reg = ir_builder.alloc_bit(step_width, loop_signed);
            let step_value = Self::truncate_usize_to_width(step, step_width);
            ir_builder.emit(SIRInstruction::Imm(
                step_reg,
                SIRValue::new(step_value as u64),
            ));
            let next_step = ir_builder.alloc_bit(step_width, loop_signed);
            let op = match stepped_op {
                Some(Op::Mul) => BinaryOp::Mul,
                Some(Op::LogicShiftL | Op::ArithShiftL) => BinaryOp::Shl,
                Some(Op::BitOr) => BinaryOp::Or,
                Some(Op::BitXor) => BinaryOp::Xor,
                Some(Op::Add) | None => BinaryOp::Add,
                Some(other) => {
                    self.local_working_vars.remove(&stmt.var_id);
                    return Err(ParserError::illegal_context(
                        "for loop step operator in always_ff",
                        format!("{other:?}"),
                        Some(&stmt.token),
                    ));
                }
            };
            ir_builder.emit(SIRInstruction::Binary(
                next_step,
                current_step,
                op,
                step_reg,
            ));
            let current_math =
                self.cast_reg_width_ext(ir_builder, current_step, math_width, loop_signed);
            // Compound assignment updates the fixed-width loop variable
            // before the next comparison. Truncate to that visible width so
            // overflow cannot be hidden by the widened comparison path.
            let next_visible =
                self.cast_reg_width_ext(ir_builder, next_step, loop_width, loop_signed);
            let next_reg =
                self.cast_reg_width_ext(ir_builder, next_visible, math_width, loop_signed);
            let progress_reg = ir_builder.alloc_bit(1, false);
            ir_builder.emit(SIRInstruction::Binary(
                progress_reg,
                next_reg,
                BinaryOp::Ne,
                current_math,
            ));
            let stall_bb = ir_builder.new_block();
            let continue_bb = ir_builder.new_block();
            ir_builder.seal_block(SIRTerminator::Branch {
                cond: progress_reg,
                true_block: (continue_bb, vec![]),
                false_block: (stall_bb, vec![]),
            });
            ir_builder.switch_to_block(continue_bb);
            let increasing_reg = ir_builder.alloc_bit(1, false);
            ir_builder.emit(SIRInstruction::Binary(
                increasing_reg,
                next_reg,
                if loop_signed {
                    BinaryOp::GtS
                } else {
                    BinaryOp::GtU
                },
                current_math,
            ));
            let range_check_bb = ir_builder.new_block();
            ir_builder.seal_block(SIRTerminator::Branch {
                cond: increasing_reg,
                true_block: (range_check_bb, vec![]),
                false_block: (stall_bb, vec![]),
            });
            ir_builder.switch_to_block(range_check_bb);
            let end_reg = self.cast_reg_width_ext(ir_builder, end_limit, math_width, loop_signed);
            let in_range_reg = ir_builder.alloc_bit(1, false);
            ir_builder.emit(SIRInstruction::Binary(
                in_range_reg,
                next_reg,
                if loop_signed {
                    if inclusive {
                        BinaryOp::LeS
                    } else {
                        BinaryOp::LtS
                    }
                } else {
                    BinaryOp::LtU
                },
                end_reg,
            ));
            let next_counter =
                self.cast_reg_width_ext(ir_builder, next_reg, compare_width, loop_signed);
            ir_builder.seal_block(SIRTerminator::Branch {
                cond: in_range_reg,
                true_block: (header_bb, vec![next_counter]),
                false_block: (exit_bb, vec![]),
            });
            ir_builder.switch_to_block(stall_bb);
            let error_code = self.runtime_error(non_progress_message, vec![stmt.var_id]);
            ir_builder.seal_block(SIRTerminator::Error(error_code));
        } else {
            let current_math =
                self.cast_reg_width_ext(ir_builder, body_counter, math_width, loop_signed);
            let start_math =
                self.cast_reg_width_ext(ir_builder, start_reg, math_width, loop_signed);
            let step_reg = ir_builder.alloc_bit(math_width, loop_signed);
            ir_builder.emit(SIRInstruction::Imm(step_reg, SIRValue::new(step as u64)));
            let next_raw = ir_builder.alloc_bit(math_width, loop_signed);
            ir_builder.emit(SIRInstruction::Binary(
                next_raw,
                current_math,
                BinaryOp::Sub,
                step_reg,
            ));
            let next_visible =
                self.cast_reg_width_ext(ir_builder, next_raw, loop_width, loop_signed);
            let next_reg =
                self.cast_reg_width_ext(ir_builder, next_visible, math_width, loop_signed);
            let decreasing_reg = ir_builder.alloc_bit(1, false);
            ir_builder.emit(SIRInstruction::Binary(
                decreasing_reg,
                next_reg,
                if loop_signed {
                    BinaryOp::LtS
                } else {
                    BinaryOp::LtU
                },
                current_math,
            ));
            let range_check_bb = ir_builder.new_block();
            let stall_bb = ir_builder.new_block();
            ir_builder.seal_block(SIRTerminator::Branch {
                cond: decreasing_reg,
                true_block: (range_check_bb, vec![]),
                false_block: (stall_bb, vec![]),
            });
            ir_builder.switch_to_block(range_check_bb);
            let in_range_reg = ir_builder.alloc_bit(1, false);
            ir_builder.emit(SIRInstruction::Binary(
                in_range_reg,
                next_reg,
                if loop_signed {
                    BinaryOp::GeS
                } else {
                    BinaryOp::GeU
                },
                start_math,
            ));
            let next_counter =
                self.cast_reg_width_ext(ir_builder, next_reg, compare_width, loop_signed);
            ir_builder.seal_block(SIRTerminator::Branch {
                cond: in_range_reg,
                true_block: (header_bb, vec![next_counter]),
                false_block: (exit_bb, vec![]),
            });
            ir_builder.switch_to_block(stall_bb);
            let error_code = self.runtime_error(non_progress_message, vec![stmt.var_id]);
            ir_builder.seal_block(SIRTerminator::Error(error_code));
        }

        self.local_working_vars.remove(&stmt.var_id);
        ir_builder.switch_to_block(exit_bb);
        self.defined_ranges = pre_loop_defined;
        self.dynamic_defined_vars = pre_loop_dynamic;
        Ok(())
    }

    fn parse_if_reset_statement<A>(
        &mut self,
        stmt: &IfResetStatement,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<ControlFlow, ParserError> {
        let true_side: Vec<&Statement> = stmt.true_side.iter().collect();
        let false_side: Vec<&Statement> = stmt.false_side.iter().collect();
        self.parse_if_reset_internal(
            &true_side,
            &false_side,
            targets,
            domain,
            convert,
            sources,
            ir_builder,
        )
    }

    fn parse_if_reset_internal<A>(
        &mut self,
        true_side: &[&Statement],
        false_side: &[&Statement],
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<ControlFlow, ParserError> {
        // 1. Load reset signal (used as condition expression)
        let (reset_id, reset_index, reset_select, is_low) = {
            let reset = self
                .reset
                .as_ref()
                .expect("if_reset used without reset signal in FfDeclaration");
            let var = &self.module.variables[&reset.id];
            let is_low = match var.r#type.kind {
                TypeKind::ResetAsyncLow | TypeKind::ResetSyncLow => true,
                TypeKind::Reset => matches!(
                    self.config.reset_type,
                    veryl_metadata::ResetType::AsyncLow | veryl_metadata::ResetType::SyncLow
                ),
                _ => false,
            };
            (reset.id, reset.index.clone(), reset.select.clone(), is_low)
        };

        self.op_load(
            reset_id,
            &reset_index,
            &reset_select,
            domain,
            convert,
            sources,
            ir_builder,
        )?;
        let mut cond_reg = self.stack.pop_back().unwrap();

        // 1.1 Handle reset polarity (Invert if Low-Active)
        if is_low {
            let inverted_reg = ir_builder.alloc_logic(1);
            ir_builder.emit(SIRInstruction::Unary(
                inverted_reg,
                UnaryOp::LogicNot,
                cond_reg,
            ));
            cond_reg = inverted_reg;
        }
        cond_reg = self.lower_procedural_condition(cond_reg, ir_builder);

        let then_bb = ir_builder.new_block();
        let else_bb = ir_builder.new_block();
        let merge_bb = ir_builder.new_block();

        // --- Create snapshot ---
        let pre_if_defined = self.defined_ranges.clone();
        let pre_if_dynamic = self.dynamic_defined_vars.clone();

        // 2. Terminate current block with Branch
        ir_builder.seal_block(SIRTerminator::Branch {
            cond: cond_reg,
            true_block: (then_bb, vec![]),
            false_block: (else_bb, vec![]),
        });

        // 3. Then Path (Reset active)
        ir_builder.switch_to_block(then_bb);
        let then_flow =
            self.parse_statement_refs(true_side, targets, domain, convert, sources, ir_builder)?;
        let then_defined = std::mem::replace(&mut self.defined_ranges, pre_if_defined.clone());
        let then_dynamic = std::mem::replace(&mut self.dynamic_defined_vars, pre_if_dynamic);
        if matches!(then_flow, ControlFlow::Continue) {
            ir_builder.seal_block(SIRTerminator::Jump(merge_bb, vec![]));
        }

        // 4. Else Path (Normal operation)
        ir_builder.switch_to_block(else_bb);
        let else_flow =
            self.parse_statement_refs(false_side, targets, domain, convert, sources, ir_builder)?;
        let else_defined = std::mem::take(&mut self.defined_ranges);
        let else_dynamic = std::mem::take(&mut self.dynamic_defined_vars);
        if matches!(else_flow, ControlFlow::Continue) {
            ir_builder.seal_block(SIRTerminator::Jump(merge_bb, vec![]));
        }

        // 5. Merge logic (Intersection of defined states of both paths)
        match (then_flow, else_flow) {
            (ControlFlow::Continue, ControlFlow::Continue) => {
                self.defined_ranges = self.intersect_defined_states(then_defined, else_defined);
                self.dynamic_defined_vars = self.intersect_dynamic_vars(then_dynamic, else_dynamic);
                ir_builder.switch_to_block(merge_bb);
                Ok(ControlFlow::Continue)
            }
            (ControlFlow::Continue, ControlFlow::Break) => {
                self.defined_ranges = then_defined;
                self.dynamic_defined_vars = then_dynamic;
                ir_builder.switch_to_block(merge_bb);
                Ok(ControlFlow::Continue)
            }
            (ControlFlow::Break, ControlFlow::Continue) => {
                self.defined_ranges = else_defined;
                self.dynamic_defined_vars = else_dynamic;
                ir_builder.switch_to_block(merge_bb);
                Ok(ControlFlow::Continue)
            }
            (ControlFlow::Break, ControlFlow::Break) => Ok(ControlFlow::Break),
        }
    }

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

        self.defined_ranges.clear();
        self.dynamic_defined_vars.clear();
        self.dynamic_write_vars.clear();
        self.local_let_values.clear();
        self.reset = decls[0].reset.clone();

        let mut targets = Vec::new();
        let mut sources = Vec::new();

        let mut all_true_sides = Vec::new();
        let mut all_false_sides = Vec::new();
        let mut other_statements = Vec::new();

        for decl in decls {
            for stmt in &decl.statements {
                if let Statement::IfReset(if_reset) = stmt {
                    all_true_sides.extend(if_reset.true_side.iter().collect::<Vec<_>>());
                    all_false_sides.extend(if_reset.false_side.iter().collect::<Vec<_>>());
                } else {
                    other_statements.push(stmt);
                }
            }
        }

        for stmt in other_statements {
            self.parse_statement(
                stmt,
                &mut targets,
                &Domain::Ff,
                convert,
                &mut sources,
                ir_builder,
            )?;
        }

        if !all_true_sides.is_empty() || !all_false_sides.is_empty() {
            self.parse_if_reset_internal(
                &all_true_sides,
                &all_false_sides,
                &mut targets,
                &Domain::Ff,
                convert,
                &mut sources,
                ir_builder,
            )?;
        }

        Ok(FfGroupParseResult {
            targets,
            sources,
            dynamic_write_vars: self.dynamic_write_vars.clone(),
        })
    }

    fn bound_const_value(bound: &ForBound) -> Option<usize> {
        match bound {
            ForBound::Const(v) => Some(*v),
            ForBound::Expression(expr) => eval_constexpr(expr)?.to_usize(),
        }
    }

    fn step_can_stall(
        reverse: bool,
        stepped_op: Option<Op>,
        step: usize,
        start_const: Option<usize>,
        loop_width: usize,
    ) -> bool {
        if reverse {
            return step == 0;
        }
        let bitwise_step = Self::truncate_usize_to_width(step, loop_width);
        match stepped_op {
            Some(Op::Mul) => step == 0 || step == 1 || start_const == Some(0),
            Some(Op::LogicShiftL | Op::ArithShiftL) => step == 0 || start_const == Some(0),
            Some(Op::BitOr) => start_const
                .map(|start| Self::truncate_usize_to_width(start, loop_width))
                .is_some_and(|start| (start | bitwise_step) == start),
            Some(Op::BitXor) => bitwise_step == 0,
            Some(Op::Add) | None => step == 0,
            Some(_) => false,
        }
    }

    fn const_range_is_empty(
        reverse: bool,
        start_const: Option<usize>,
        end_const: Option<usize>,
        inclusive: bool,
    ) -> bool {
        let (Some(start), Some(end)) = (start_const, end_const) else {
            return false;
        };
        if reverse {
            if inclusive { end < start } else { end <= start }
        } else if inclusive {
            start > end
        } else {
            start >= end
        }
    }

    fn const_range_is_singleton(
        _reverse: bool,
        start_const: Option<usize>,
        end_const: Option<usize>,
        inclusive: bool,
    ) -> bool {
        let (Some(start), Some(end)) = (start_const, end_const) else {
            return false;
        };
        if !inclusive {
            return false;
        }
        start == end
    }
}
