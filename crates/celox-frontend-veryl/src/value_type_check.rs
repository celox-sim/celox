//! Rejects uses of `string` and floating-point values that Celox cannot
//! represent yet.
//!
//! Runtime strings exist only in the testbench VM: in RTL a non-constant
//! string occupies a one-bit slot. Floating-point values are stored as their
//! IEEE 754 bit patterns, so copies are exact, but every operation on them
//! would run as integer arithmetic. Constant expressions are folded by the
//! analyzer and stay accepted.
use veryl_analyzer::ir::{
    ArrayLiteralItem, AssignDestination, CasePattern, Component, Declaration, Expression, Factor,
    ForBound, ForRange, FunctionCall, Module, Op, Statement, SystemFunctionCall,
    SystemFunctionKind, SystemFunctionOutput, TbMethod, Type, TypeKind, VarIndex, VarKind,
    VarSelect,
};
use veryl_parser::token_range::TokenRange;

use crate::{
    HashSet,
    error::{LoweringPhase, ParserError},
};

const STRING_ISSUE: u32 = 1400;
const FLOAT_ISSUE: u32 = 1401;

pub(crate) fn check_module(module: &Module) -> Result<(), ParserError> {
    let mut checker = Checker {
        module,
        rtl: true,
        visited: HashSet::default(),
    };
    for declaration in &module.declarations {
        match declaration {
            Declaration::Comb(x) => {
                checker.rtl = true;
                checker.statements(&x.statements)?;
            }
            Declaration::Ff(x) => {
                checker.rtl = true;
                checker.statements(&x.statements)?;
            }
            Declaration::Initial(x) => {
                checker.rtl = false;
                checker.statements(&x.statements)?;
            }
            Declaration::Final(x) => {
                checker.rtl = false;
                checker.statements(&x.statements)?;
            }
            Declaration::Inst(x) => {
                checker.rtl = true;
                let child = match x.component.as_ref() {
                    Component::Module(child) => Some(child),
                    _ => None,
                };
                for input in &x.inputs {
                    let port = child.and_then(|child| child.variables.get(&input.id));
                    for expression in &input.exprs {
                        if let Some(port) = port {
                            checker.conversion(&port.r#type, expression)?;
                        }
                        checker.expression(expression)?;
                    }
                }
                for output in &x.outputs {
                    let port = child.and_then(|child| child.variables.get(&output.id));
                    for destination in &output.dst {
                        if let Some(port) = port {
                            checker.port_output(&port.r#type, destination)?;
                        }
                    }
                    checker.destinations(&output.dst)?;
                }
            }
            Declaration::External(_) | Declaration::Unsupported(_) | Declaration::Null => {}
        }
    }
    Ok(())
}

struct Checker<'a> {
    module: &'a Module,
    /// Whether statements run as RTL rather than in the testbench VM.
    rtl: bool,
    visited: HashSet<(veryl_analyzer::ir::VarId, bool)>,
}

fn float_kind(r#type: &Type) -> Option<&TypeKind> {
    r#type.kind.is_float().then_some(&r#type.kind)
}

fn float_operation(feature: &'static str, detail: String, token: &TokenRange) -> ParserError {
    ParserError::unsupported(
        FLOAT_ISSUE,
        LoweringPhase::SimulatorParser,
        feature,
        detail,
        Some(token),
    )
}

impl Checker<'_> {
    fn expression(&mut self, expression: &Expression) -> Result<(), ParserError> {
        let comptime = expression.comptime();
        // The analyzer folds constant expressions, so no operation remains.
        if comptime.is_const {
            return Ok(());
        }
        match expression {
            Expression::Term(factor) => self.factor(factor),
            Expression::Unary(_, inner, comptime) => {
                if float_kind(&inner.comptime().r#type).is_some()
                    || float_kind(&comptime.r#type).is_some()
                {
                    return Err(float_operation(
                        "floating-point operation",
                        "operations on non-constant f32/f64 values run as integer arithmetic"
                            .to_string(),
                        &comptime.token,
                    ));
                }
                self.expression(inner)
            }
            Expression::Binary(left, op, right, comptime) => {
                if *op == Op::As {
                    let source = float_kind(&left.comptime().r#type);
                    let target = float_kind(&comptime.r#type);
                    if source != target {
                        return Err(float_operation(
                            "floating-point conversion",
                            "casts between f32, f64 and integer types do not convert non-constant values"
                                .to_string(),
                            &comptime.token,
                        ));
                    }
                    return self.expression(left);
                }
                if float_kind(&left.comptime().r#type).is_some()
                    || float_kind(&right.comptime().r#type).is_some()
                    || float_kind(&comptime.r#type).is_some()
                {
                    return Err(float_operation(
                        "floating-point operation",
                        format!(
                            "operator `{op}` on non-constant f32/f64 values runs as integer arithmetic"
                        ),
                        &comptime.token,
                    ));
                }
                self.expression(left)?;
                self.expression(right)
            }
            Expression::Ternary(cond, left, right, comptime) => {
                self.condition(cond)?;
                if float_kind(&left.comptime().r#type) != float_kind(&right.comptime().r#type) {
                    return Err(float_operation(
                        "floating-point conversion",
                        "conditional arms of different f32/f64/integer types are not converted"
                            .to_string(),
                        &comptime.token,
                    ));
                }
                self.expression(cond)?;
                self.expression(left)?;
                self.expression(right)
            }
            Expression::Concatenation(items, _) => {
                for (value, repeat) in items {
                    self.integral(value, "concatenation")?;
                    self.expression(value)?;
                    if let Some(repeat) = repeat {
                        self.integral(repeat, "replication count")?;
                        self.expression(repeat)?;
                    }
                }
                Ok(())
            }
            Expression::ArrayLiteral(items, _) => {
                for item in items {
                    match item {
                        ArrayLiteralItem::Value(value, repeat) => {
                            self.expression(value)?;
                            if let Some(repeat) = repeat {
                                self.integral(repeat, "replication count")?;
                                self.expression(repeat)?;
                            }
                        }
                        ArrayLiteralItem::Defaul(value) => self.expression(value)?,
                    }
                }
                Ok(())
            }
            Expression::StructConstructor(_, fields, _) => {
                for (_, value) in fields {
                    self.expression(value)?;
                }
                Ok(())
            }
        }
    }

    fn factor(&mut self, factor: &Factor) -> Result<(), ParserError> {
        let comptime = factor.comptime();
        if self.rtl && !comptime.is_const && comptime.r#type.is_string() {
            return Err(ParserError::unsupported(
                STRING_ISSUE,
                LoweringPhase::SimulatorParser,
                "runtime string value",
                "non-constant string values are only supported in testbench code; RTL can use string literals, constants and parameters",
                Some(&comptime.token),
            ));
        }
        match factor {
            Factor::Variable(_, index, select, _) => self.select(index, select),
            Factor::HierVariable(x) => self.select(&x.index, &x.select),
            Factor::FunctionCall(call) => self.call(call),
            Factor::SystemFunctionCall(call) => self.system_call(call),
            Factor::Value(_) | Factor::Anonymous(_) | Factor::Unknown(_) => Ok(()),
        }
    }

    /// Rejects a non-constant float where an integral value is required.
    fn integral(&self, expression: &Expression, role: &str) -> Result<(), ParserError> {
        let comptime = expression.comptime();
        if !comptime.is_const && float_kind(&comptime.r#type).is_some() {
            return Err(float_operation(
                "floating-point operation",
                format!("non-constant f32/f64 values cannot be used as a {role}"),
                &comptime.token,
            ));
        }
        Ok(())
    }

    fn condition(&self, expression: &Expression) -> Result<(), ParserError> {
        self.integral(expression, "condition")
    }

    /// Rejects an implicit conversion between float and integer types.
    /// Even constant values are stored without conversion.
    fn conversion(&self, target: &Type, expression: &Expression) -> Result<(), ParserError> {
        let comptime = expression.comptime();
        if float_kind(target) != float_kind(&comptime.r#type) {
            return Err(float_operation(
                "floating-point conversion",
                format!(
                    "assigning a {} value to a {} target is not converted",
                    comptime.r#type.kind, target.kind
                ),
                &comptime.token,
            ));
        }
        Ok(())
    }

    fn port_output(&self, port: &Type, destination: &AssignDestination) -> Result<(), ParserError> {
        if float_kind(port) != float_kind(&destination.comptime.r#type) {
            return Err(float_operation(
                "floating-point conversion",
                format!(
                    "connecting a {} port to a {} destination is not converted",
                    port.kind, destination.comptime.r#type.kind
                ),
                &destination.token,
            ));
        }
        Ok(())
    }

    fn display_args<'e>(
        &mut self,
        args: impl IntoIterator<Item = &'e Expression>,
    ) -> Result<(), ParserError> {
        for arg in args {
            let comptime = arg.comptime();
            if float_kind(&comptime.r#type).is_some() {
                return Err(float_operation(
                    "floating-point formatting",
                    "f32/f64 values cannot be formatted yet".to_string(),
                    &comptime.token,
                ));
            }
            self.expression(arg)?;
        }
        Ok(())
    }

    fn call(&mut self, call: &FunctionCall) -> Result<(), ParserError> {
        let function = self.module.functions.get(&call.id);
        let formal = |path| {
            function.and_then(|function| {
                function
                    .args
                    .iter()
                    .flat_map(|arg| &arg.members)
                    .find(|(member, _, _)| member == path)
                    .map(|(_, comptime, _)| &comptime.r#type)
            })
        };
        for (path, expression) in &call.inputs {
            if let Some(formal) = formal(path) {
                self.conversion(formal, expression)?;
            }
            self.expression(expression)?;
        }
        for (path, destinations) in &call.outputs {
            for destination in destinations {
                if let Some(formal) = formal(path) {
                    self.port_output(formal, destination)?;
                }
            }
            self.destinations(destinations)?;
        }
        // Bodies of calls the analyzer folded never run.
        if !call.comptime.is_const
            && let Some(function) = function
            && self.visited.insert((call.id, self.rtl))
        {
            for body in &function.functions {
                self.statements(&body.statements)?;
            }
        }
        Ok(())
    }

    fn destinations(&mut self, destinations: &[AssignDestination]) -> Result<(), ParserError> {
        for destination in destinations {
            if self.rtl && destination.comptime.r#type.is_string() {
                return Err(ParserError::unsupported(
                    STRING_ISSUE,
                    LoweringPhase::SimulatorParser,
                    "runtime string value",
                    "string variables can only be assigned in testbench code",
                    Some(&destination.token),
                ));
            }
            self.select(&destination.index, &destination.select)?;
        }
        Ok(())
    }

    fn select(&mut self, index: &VarIndex, select: &VarSelect) -> Result<(), ParserError> {
        for expression in index.0.iter().chain(&select.0) {
            self.integral(expression, "index")?;
            self.expression(expression)?;
        }
        if let Some((_, expression)) = &select.1 {
            self.integral(expression, "index")?;
            self.expression(expression)?;
        }
        Ok(())
    }

    fn system_call(&mut self, call: &SystemFunctionCall) -> Result<(), ParserError> {
        match &call.kind {
            // Shape queries do not evaluate their operands.
            SystemFunctionKind::Bits(_) | SystemFunctionKind::Size(_, _) => Ok(()),
            SystemFunctionKind::Clog2(x)
            | SystemFunctionKind::Onehot(x)
            | SystemFunctionKind::Signed(x)
            | SystemFunctionKind::Unsigned(x) => {
                self.integral(&x.0, "system function argument")?;
                self.expression(&x.0)
            }
            SystemFunctionKind::Readmemh(input, output) => {
                self.expression(&input.0)?;
                match output {
                    SystemFunctionOutput::Local(destinations) => self.destinations(destinations),
                    SystemFunctionOutput::Hier(reference) => {
                        self.select(&reference.index, &reference.select)
                    }
                }
            }
            SystemFunctionKind::Display(args) | SystemFunctionKind::Write(args) => {
                self.display_args(args.iter().map(|arg| &arg.0))
            }
            SystemFunctionKind::Assert { cond, args, .. } => {
                self.condition(&cond.0)?;
                self.expression(&cond.0)?;
                self.display_args(args.iter().map(|arg| &arg.0))
            }
            SystemFunctionKind::Finish => Ok(()),
        }
    }

    fn statements(&mut self, statements: &[Statement]) -> Result<(), ParserError> {
        for statement in statements {
            match statement {
                Statement::Assign(x) => {
                    for destination in &x.dst {
                        self.conversion(&destination.comptime.r#type, &x.expr)?;
                    }
                    if let Some(destination) = &x.hier_dst {
                        self.conversion(&destination.comptime.r#type, &x.expr)?;
                        self.select(&destination.index, &destination.select)?;
                    }
                    self.expression(&x.expr)?;
                    // The testbench inlines constant string lets; RTL reads
                    // of them are rejected at the read.
                    let constant_let = x.expr.comptime().is_const
                        && x.dst.iter().all(|destination| {
                            self.module
                                .variables
                                .get(&destination.id)
                                .is_some_and(|variable| variable.kind == VarKind::Let)
                        });
                    if constant_let {
                        for destination in &x.dst {
                            self.select(&destination.index, &destination.select)?;
                        }
                    } else {
                        self.destinations(&x.dst)?;
                    }
                }
                Statement::FunctionCall(call) => self.call(call)?,
                Statement::SystemFunctionCall(call) => self.system_call(call)?,
                Statement::If(x) => {
                    self.condition(&x.cond)?;
                    self.expression(&x.cond)?;
                    self.statements(&x.true_side)?;
                    self.statements(&x.false_side)?;
                }
                Statement::IfReset(x) => {
                    self.statements(&x.true_side)?;
                    self.statements(&x.false_side)?;
                }
                Statement::Case(x) => {
                    self.integral(&x.case_target, "case expression")?;
                    self.expression(&x.case_target)?;
                    for arm in &x.arms {
                        for pattern in &arm.patterns {
                            match pattern {
                                CasePattern::Eq(value) => {
                                    self.integral(value, "case item")?;
                                    self.expression(value)?;
                                }
                                CasePattern::Range { lo, hi, .. } => {
                                    self.integral(lo, "case item")?;
                                    self.integral(hi, "case item")?;
                                    self.expression(lo)?;
                                    self.expression(hi)?;
                                }
                            }
                        }
                        self.statements(&arm.body)?;
                    }
                    self.statements(&x.default)?;
                }
                Statement::For(x) => {
                    let (ForRange::Forward { start, end, .. }
                    | ForRange::Reverse { start, end, .. }
                    | ForRange::Stepped { start, end, .. }) = &x.range;
                    for bound in [start, end] {
                        if let ForBound::Expression(value) = bound {
                            self.integral(value, "loop bound")?;
                            self.expression(value)?;
                        }
                    }
                    self.statements(&x.body)?;
                }
                Statement::TbMethodCall(x) => {
                    if let Some(destination) = &x.ret {
                        self.select(&destination.index, &destination.select)?;
                    }
                    match &x.method {
                        TbMethod::ClockNext { count, period } => {
                            for value in count.iter().chain(period.as_deref()) {
                                self.expression(value)?;
                            }
                        }
                        TbMethod::ResetAssert { duration, .. } => {
                            if let Some(value) = duration {
                                self.expression(value)?;
                            }
                        }
                        TbMethod::FileOpen { name, .. } => self.expression(&name.0)?,
                        TbMethod::FileWrite { args } => {
                            self.display_args(args.iter().map(|arg| &arg.0))?;
                        }
                        TbMethod::Component { args, .. } => {
                            for arg in args {
                                self.expression(&arg.0)?;
                            }
                        }
                        TbMethod::RandomSeed { value } => self.expression(value)?,
                        TbMethod::RandomGetRange { min, max, .. } => {
                            self.expression(min)?;
                            self.expression(max)?;
                        }
                        TbMethod::FileClose
                        | TbMethod::FileFlush
                        | TbMethod::RandomGet { .. }
                        | TbMethod::RandomGetSeed => {}
                    }
                }
                Statement::Break | Statement::Unsupported(_) | Statement::Null => {}
            }
        }
        Ok(())
    }
}
