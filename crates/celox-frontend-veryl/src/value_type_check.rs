//! Validates unsupported string and floating-point contexts.
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
                            if output.dst.len() != 1
                                && float_kind(&port.r#type)
                                    != float_kind(&destination.comptime.r#type)
                            {
                                return Err(float_operation("floating-point conversion", "concatenated floating-point output conversions are not supported".to_string(), &destination.token));
                            }
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
        enum Pending<'a> {
            Expression(&'a Expression),
            Integral(&'a Expression, &'static str),
        }
        let mut pending = vec![Pending::Expression(expression)];
        while let Some(item) = pending.pop() {
            let expression = match item {
                Pending::Expression(expression) => expression,
                Pending::Integral(expression, role) => {
                    self.integral(expression, role)?;
                    continue;
                }
            };
            let comptime = expression.comptime();
            // The analyzer folds constant expressions, so no operation remains.
            if comptime.is_const {
                continue;
            }
            match expression {
                Expression::Term(factor) => self.factor(factor)?,
                Expression::Unary(op, inner, comptime) => {
                    if float_kind(crate::float::type_of(inner)).is_some()
                        && !matches!(op, Op::Add | Op::Sub)
                    {
                        return Err(float_operation(
                            "floating-point operation",
                            format!("operator `{op}` is not supported on floating-point values"),
                            &comptime.token,
                        ));
                    }
                    pending.push(Pending::Expression(inner));
                }
                Expression::Binary(left, op, right, comptime) => {
                    if *op == Op::As {
                        self.conversion(&comptime.r#type, left)?;
                        pending.push(Pending::Expression(left));
                        continue;
                    }
                    if (float_kind(crate::float::type_of(left)).is_some()
                        || float_kind(crate::float::type_of(right)).is_some())
                        && crate::float::binary(*op).is_none()
                    {
                        return Err(float_operation(
                            "floating-point operation",
                            format!("operator `{op}` is not supported on floating-point values"),
                            &comptime.token,
                        ));
                    }
                    if float_kind(crate::float::type_of(left)).is_some() {
                        self.conversion(crate::float::type_of(left), right)?;
                    } else if float_kind(crate::float::type_of(right)).is_some() {
                        self.conversion(crate::float::type_of(right), left)?;
                    }
                    pending.push(Pending::Expression(right));
                    pending.push(Pending::Expression(left));
                }
                Expression::Ternary(cond, left, right, _) => {
                    self.condition(cond)?;
                    self.conversion(crate::float::type_of(expression), left)?;
                    self.conversion(crate::float::type_of(expression), right)?;
                    pending.push(Pending::Expression(right));
                    pending.push(Pending::Expression(left));
                    pending.push(Pending::Expression(cond));
                }
                Expression::Concatenation(items, _) => {
                    for (value, repeat) in items.iter().rev() {
                        if let Some(repeat) = repeat {
                            pending.push(Pending::Expression(repeat));
                            pending.push(Pending::Integral(repeat, "replication count"));
                        }
                        pending.push(Pending::Expression(value));
                        pending.push(Pending::Integral(value, "concatenation"));
                    }
                }
                Expression::ArrayLiteral(items, _) => {
                    for item in items.iter().rev() {
                        match item {
                            ArrayLiteralItem::Value(value, repeat) => {
                                if let Some(repeat) = repeat {
                                    pending.push(Pending::Expression(repeat));
                                    pending.push(Pending::Integral(repeat, "replication count"));
                                }
                                pending.push(Pending::Expression(value));
                            }
                            ArrayLiteralItem::Defaul(value) => {
                                pending.push(Pending::Expression(value))
                            }
                        }
                    }
                }
                Expression::StructConstructor(_, fields, _) => {
                    pending.extend(
                        fields
                            .iter()
                            .rev()
                            .map(|(_, value)| Pending::Expression(value)),
                    );
                }
            }
        }
        Ok(())
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
        if !comptime.is_const && float_kind(crate::float::type_of(expression)).is_some() {
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

    fn conversion(&self, target: &Type, expression: &Expression) -> Result<(), ParserError> {
        let source = crate::float::type_of(expression);
        if float_kind(target) != float_kind(source)
            && (float_kind(target).is_some() || float_kind(source).is_some())
        {
            if !target.array.is_empty() || !source.array.is_empty() {
                return Err(float_operation(
                    "floating-point conversion",
                    "array floating-point conversions are not supported".to_string(),
                    &expression.comptime().token,
                ));
            }
            let integer = if float_kind(target).is_none() {
                target
            } else {
                source
            };
            if float_kind(integer).is_none() && integer.total_width().unwrap_or(0) > 64 {
                return Err(float_operation(
                    "floating-point conversion",
                    "integer conversions wider than 64 bits are not supported".to_string(),
                    &expression.comptime().token,
                ));
            }
        }
        Ok(())
    }

    fn port_output(&self, port: &Type, destination: &AssignDestination) -> Result<(), ParserError> {
        let target = &destination.comptime.r#type;
        if float_kind(port) != float_kind(target) {
            let integer = if float_kind(port).is_none() {
                port
            } else {
                target
            };
            if !port.array.is_empty()
                || !target.array.is_empty()
                || (float_kind(integer).is_none() && integer.total_width().unwrap_or(0) > 64)
            {
                return Err(float_operation(
                    "floating-point conversion",
                    "output conversions require scalar float or integer types up to 64 bits"
                        .to_string(),
                    &destination.token,
                ));
            }
        }
        Ok(())
    }

    fn display_args<'e>(
        &mut self,
        args: impl IntoIterator<Item = &'e Expression>,
    ) -> Result<(), ParserError> {
        for arg in args {
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
                    if destinations.len() != 1
                        && float_kind(formal) != float_kind(&destination.comptime.r#type)
                    {
                        return Err(float_operation(
                            "floating-point conversion",
                            "concatenated floating-point function outputs are not supported"
                                .to_string(),
                            &destination.token,
                        ));
                    }
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
        // Else-if chains and generated decoders can be thousands of nodes
        // deep. Keep traversal state on the heap, preserving source-order
        // diagnostics (including each case arm's patterns before its body).
        enum Pending<'a> {
            Statements(&'a [Statement]),
            Patterns(&'a [CasePattern]),
        }
        let mut pending = vec![Pending::Statements(statements)];
        while let Some(item) = pending.pop() {
            let statements = match item {
                Pending::Statements(statements) => statements,
                Pending::Patterns(patterns) => {
                    for pattern in patterns {
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
                    continue;
                }
            };
            let Some((statement, rest)) = statements.split_first() else {
                continue;
            };
            if !rest.is_empty() {
                pending.push(Pending::Statements(rest));
            }
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
                    pending.push(Pending::Statements(&x.false_side));
                    pending.push(Pending::Statements(&x.true_side));
                }
                Statement::IfReset(x) => {
                    pending.push(Pending::Statements(&x.false_side));
                    pending.push(Pending::Statements(&x.true_side));
                }
                Statement::Case(x) => {
                    self.integral(&x.case_target, "case expression")?;
                    self.expression(&x.case_target)?;
                    pending.push(Pending::Statements(&x.default));
                    for arm in x.arms.iter().rev() {
                        pending.push(Pending::Statements(&arm.body));
                        pending.push(Pending::Patterns(&arm.patterns));
                    }
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
                    pending.push(Pending::Statements(&x.body));
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
