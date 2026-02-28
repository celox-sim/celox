use std::collections::VecDeque;

use crate::ir::{
    RegisterId, SIRBuilder, SIRInstruction, SIRTerminator, TriggerSet, UnaryOp, VarAtomBase,
    WORKING_REGION,
};
use crate::{
    HashMap, HashSet,
    parser::{ParserError, bitaccess::eval_var_select},
};
use bit_set::BitSet;
use num_traits::ToPrimitive;

use veryl_analyzer::ir::{
    Expression, Factor, FfDeclaration, FfReset, IfResetStatement, IfStatement, Module, Op,
    Statement, TypeKind, ValueVariant, VarId,
};

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

pub struct FfParser<'a> {
    module: &'a Module,
    stack: VecDeque<RegisterId>,
    defined_ranges: HashMap<VarId, BitSet>,
    dynamic_defined_vars: HashSet<VarId>,
    reset: Option<FfReset>,
    function_arg_stack: Vec<HashMap<VarId, Expression>>,
}

impl<'a> FfParser<'a> {
    pub fn new(module: &'a Module) -> Self {
        Self {
            module,
            stack: VecDeque::new(),
            defined_ranges: HashMap::default(),
            dynamic_defined_vars: HashSet::default(),
            reset: None,
            function_arg_stack: Vec::new(),
        }
    }

    fn get_constant_value(&self, expr: &Expression) -> Option<u64> {
        if let Expression::Term(factor) = expr
            && let Factor::Value(comptime, _) = factor.as_ref()
            && let Ok(val) = comptime.get_value()
        {
            return val.payload().to_u64();
        }
        None
    }

    fn get_cast_target_info(&self, expr: &Expression) -> Option<(usize, bool, bool)> {
        let Expression::Term(factor) = expr else {
            return None;
        };
        let Factor::Value(comptime, _) = factor.as_ref() else {
            return None;
        };
        let ValueVariant::Type(ty) = &comptime.value else {
            return None;
        };

        let width = ty.total_width()?;
        let signed = ty.signed;
        let is_2state = ty.is_2state();
        Some((width, signed, is_2state))
    }

    fn get_expression_width(&self, expr: &Expression) -> usize {
        match expr {
            Expression::Binary(left, op, right) => {
                let lw = self.get_expression_width(left);
                let rw = self.get_expression_width(right);
                match op {
                    Op::Eq
                    | Op::Ne
                    | Op::Less
                    | Op::LessEq
                    | Op::Greater
                    | Op::GreaterEq
                    | Op::LogicAnd
                    | Op::LogicOr
                    | Op::LogicNot => 1,
                    _ => lw.max(rw),
                }
            }
            Expression::Unary(op, expr) => match op {
                Op::LogicNot
                | Op::BitAnd
                | Op::BitOr
                | Op::BitXor
                | Op::BitXnor
                | Op::BitNand
                | Op::BitNor => 1,
                _ => self.get_expression_width(expr),
            },
            Expression::Term(factor) => self.get_factor_width(factor),
            Expression::Ternary(_, then, els) => self
                .get_expression_width(then)
                .max(self.get_expression_width(els)),
            Expression::Concatenation(exprs) => {
                let mut total = 0;
                for (expr, replication) in exprs {
                    let w = self.get_expression_width(expr);
                    let rep = if let Some(rep_expr) = replication {
                        self.get_constant_value(rep_expr).unwrap_or(1) as usize
                    } else {
                        1
                    };
                    total += w * rep;
                }
                total
            }
            _ => 64,
        }
    }

    fn get_factor_width(&self, factor: &Factor) -> usize {
        match factor {
            Factor::Variable(var_id, index, select, _, _) => {
                let access = eval_var_select(self.module, *var_id, index, select);
                access.msb - access.lsb + 1
            }
            Factor::Value(comptime, _) => {
                if let Ok(v) = comptime.get_value() {
                    v.width()
                } else {
                    64
                }
            }
            Factor::FunctionCall(call, _) => call
                .ret
                .as_ref()
                .and_then(|x| x.r#type.total_width())
                .unwrap_or(64),
            _ => 64,
        }
    }

    // expression / function-call lowering is split into submodules:
    // - parser/ff/expression.rs
    // - parser/ff/function_call.rs
    fn parse_if_statement<A>(
        &mut self,
        stmt: &IfStatement,
        targets: &mut Vec<VarAtomBase<A>>,
        domain: &Domain,
        convert: &impl Fn(VarId, u32) -> A,
        sources: &mut Vec<VarAtomBase<A>>,
        ir_builder: &mut SIRBuilder<A>,
    ) -> Result<(), ParserError> {
        // 1. Evaluate condition expression
        self.parse_expression(
            &stmt.cond, targets, domain, convert, sources, ir_builder, None,
        )?;
        let cond_reg = self.stack.pop_back().unwrap();

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
        for stmt in &stmt.true_side {
            self.parse_statement(stmt, targets, domain, convert, sources, ir_builder)?;
        }
        // Collect state at the end of Then, and restore state at the beginning
        let then_defined = std::mem::replace(&mut self.defined_ranges, pre_if_defined.clone());
        let then_dynamic = std::mem::replace(&mut self.dynamic_defined_vars, pre_if_dynamic); // 【追加】

        ir_builder.seal_block(SIRTerminator::Jump(merge_bb, vec![]));

        // 4. Else Path
        ir_builder.switch_to_block(else_bb);
        for stmt in &stmt.false_side {
            self.parse_statement(stmt, targets, domain, convert, sources, ir_builder)?;
        }
        // Collect state at the end of Else
        let else_defined = std::mem::take(&mut self.defined_ranges);
        let else_dynamic = std::mem::take(&mut self.dynamic_defined_vars); // 【追加】

        ir_builder.seal_block(SIRTerminator::Jump(merge_bb, vec![]));

        // 5. Merge logic
        // Take intersection of static and dynamic states respectively
        self.defined_ranges = self.intersect_defined_states(then_defined, else_defined);
        self.dynamic_defined_vars = self.intersect_dynamic_vars(then_dynamic, else_dynamic); // 【追加】

        // 6. Set merge point as "next parse target"
        ir_builder.switch_to_block(merge_bb);
        Ok(())
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
    ) -> Result<(), ParserError> {
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
                self.parse_if_statement(stmt, targets, domain, convert, sources, ir_builder)?;
            }
            Statement::IfReset(stmt) => {
                self.parse_if_reset_statement(stmt, targets, domain, convert, sources, ir_builder)?;
            }
            Statement::Null => {}
            Statement::SystemFunctionCall(call) => {
                return Err(ParserError::UnsupportedFFLowering {
                    feature: "system function call",
                    detail: format!("{call}"),
                });
            }
            Statement::FunctionCall(call) => {
                self.parse_function_call_statement(
                    call, targets, domain, convert, sources, ir_builder,
                )?;
            }
        }
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
    ) -> Result<(), ParserError> {
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
    ) -> Result<(), ParserError> {
        // 1. Load reset signal (used as condition expression)
        let (reset_id, reset_index, reset_select, is_low) = {
            let reset = self
                .reset
                .as_ref()
                .expect("if_reset used without reset signal in FfDeclaration");
            let var = &self.module.variables[&reset.id];
            let is_low = matches!(
                var.r#type.kind,
                TypeKind::ResetAsyncLow | TypeKind::ResetSyncLow
            );
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
            let inverted_reg = ir_builder.alloc_bit(1, false);
            ir_builder.emit(SIRInstruction::Unary(
                inverted_reg,
                UnaryOp::LogicNot,
                cond_reg,
            ));
            cond_reg = inverted_reg;
        }

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
        for s in true_side {
            self.parse_statement(s, targets, domain, convert, sources, ir_builder)?;
        }
        let then_defined = std::mem::replace(&mut self.defined_ranges, pre_if_defined.clone());
        let then_dynamic = std::mem::replace(&mut self.dynamic_defined_vars, pre_if_dynamic);
        ir_builder.seal_block(SIRTerminator::Jump(merge_bb, vec![]));

        // 4. Else Path (Normal operation)
        ir_builder.switch_to_block(else_bb);
        for s in false_side {
            self.parse_statement(s, targets, domain, convert, sources, ir_builder)?;
        }
        let else_defined = std::mem::take(&mut self.defined_ranges);
        let else_dynamic = std::mem::take(&mut self.dynamic_defined_vars);
        ir_builder.seal_block(SIRTerminator::Jump(merge_bb, vec![]));

        // 5. Merge logic (Intersection of defined states of both paths)
        self.defined_ranges = self.intersect_defined_states(then_defined, else_defined);
        self.dynamic_defined_vars = self.intersect_dynamic_vars(then_dynamic, else_dynamic);

        // 6. Set merge point as "next parse target"
        ir_builder.switch_to_block(merge_bb);
        Ok(())
    }

    pub fn detect_trigger_set(&self, decl: &FfDeclaration) -> TriggerSet<VarId> {
        let mut trigger_set = TriggerSet {
            clock: decl.clock.id,
            resets: Vec::new(),
        };

        if let Some(reset) = &decl.reset {
            let var = &self.module.variables[&reset.id];
            if matches!(
                var.r#type.kind,
                TypeKind::ResetAsyncHigh | TypeKind::ResetAsyncLow
            ) {
                trigger_set.resets.push(reset.id);
            }
        }
        trigger_set
    }

    pub fn parse_ff_group(
        &mut self,
        decls: &[&FfDeclaration],
        ir_builder: &mut SIRBuilder<crate::ir::RegionedVarAddr>,
    ) -> Result<(), ParserError> {
        if decls.is_empty() {
            return Ok(());
        }

        self.defined_ranges.clear();
        self.dynamic_defined_vars.clear();
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
                &|x, region| crate::ir::RegionedVarAddr { var_id: x, region },
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
                &|x, region| crate::ir::RegionedVarAddr { var_id: x, region },
                &mut sources,
                ir_builder,
            )?;
        }

        Ok(())
    }

    /// Returns the set of variables written by this FF group (deduplicated).
    /// Used by the caller to generate Commit instructions.
    pub fn collect_written_vars(decls: &[&FfDeclaration]) -> impl Iterator<Item = VarId> {
        let mut seen = crate::HashSet::default();
        decls
            .iter()
            .flat_map(|d| d.statements.iter())
            .flat_map(|s| Self::collect_assigned_var_ids(s))
            .filter(move |id| seen.insert(*id))
            .collect::<Vec<_>>()
            .into_iter()
    }

    fn collect_expr_output_var_ids(expr: &Expression) -> Vec<VarId> {
        match expr {
            Expression::Term(factor) => {
                if let Factor::FunctionCall(call, _) = factor.as_ref() {
                    call.outputs
                        .values()
                        .flat_map(|dsts| dsts.iter().map(|d| d.id))
                        .collect()
                } else {
                    vec![]
                }
            }
            Expression::Binary(lhs, _, rhs) => {
                let mut v = Self::collect_expr_output_var_ids(lhs);
                v.extend(Self::collect_expr_output_var_ids(rhs));
                v
            }
            Expression::Unary(_, inner) => Self::collect_expr_output_var_ids(inner),
            Expression::Ternary(cond, then_e, else_e) => {
                let mut v = Self::collect_expr_output_var_ids(cond);
                v.extend(Self::collect_expr_output_var_ids(then_e));
                v.extend(Self::collect_expr_output_var_ids(else_e));
                v
            }
            _ => vec![],
        }
    }

    fn collect_assigned_var_ids(stmt: &Statement) -> Vec<VarId> {
        match stmt {
            Statement::Assign(a) => {
                let mut ids: Vec<VarId> = a.dst.iter().map(|d| d.id).collect();
                // Also collect output args of any FunctionCall embedded in the RHS expression
                ids.extend(Self::collect_expr_output_var_ids(&a.expr));
                ids
            }
            Statement::If(s) => s
                .true_side
                .iter()
                .chain(s.false_side.iter())
                .flat_map(Self::collect_assigned_var_ids)
                .collect(),
            Statement::IfReset(s) => s
                .true_side
                .iter()
                .chain(s.false_side.iter())
                .flat_map(Self::collect_assigned_var_ids)
                .collect(),
            Statement::FunctionCall(call) => call
                .outputs
                .values()
                .flat_map(|dsts| dsts.iter().map(|d| d.id))
                .collect(),
            _ => vec![],
        }
    }
}
