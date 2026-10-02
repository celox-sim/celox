//! Portability warnings for expressions with unspecified side-effect ordering.
//!
//! Copy-out consists of blocking assignments (IEEE 1800-2023 4.9.7), but 13.5
//! does not order those assignments between formals. This is a portability
//! warning, not a rejection or a change to Celox's chosen execution order.
//! Runtime indices are deliberately not treated as proven aliases.
use veryl_analyzer::ir::{
    ArrayLiteralItem, AssignDestination, CasePattern, Component, Declaration, Expression, Factor,
    ForBound, ForRange, FunctionCall, Ir, Module, Statement, SystemFunctionCall,
    SystemFunctionKind, TbMethod, VarIndex, VarSelect,
};

use veryl_parser::{
    token_range::TokenRange,
    veryl_grammar_trait::{self as ast, Veryl},
    veryl_walker::VerylWalker,
};

use crate::{
    FrontendDiagnostic, HashSet,
    bitaccess::{eval_var_select, is_static_access},
};

pub fn check_function_output_aliases(ir: &Ir) -> Vec<FrontendDiagnostic> {
    check(ir, Check::OutputAliases, &[], &HashSet::default())
}

/// IEEE 1800-2023 10.9.1 leaves evaluation counts undefined for effectful
/// default/type keys and array assignment pattern replications. Veryl exposes
/// default and repeat items; ordinary concatenation repeats are not covered.
pub fn check_array_literal_side_effects<'a>(
    ir: &Ir,
    asts: impl IntoIterator<Item = &'a Veryl>,
    defines: &HashSet<veryl_parser::resource_table::StrId>,
) -> Vec<FrontendDiagnostic> {
    // Pass 2 expands assignment literals into individual element assignments.
    // Retain their syntax provenance so expanded default/repeat items remain
    // distinguishable from explicit elements and concatenations.
    let mut sources = ArrayItemSources::default();
    for ast in asts {
        sources.veryl(ast);
    }
    check(ir, Check::ArrayLiteralSideEffects, &sources.0, defines)
}

struct ArrayItemSource {
    token: TokenRange,
    value: TokenRange,
    expression: ast::Expression,
    kind: &'static str,
}

#[derive(Default)]
struct ArrayItemSources(Vec<ArrayItemSource>);

impl VerylWalker for ArrayItemSources {
    fn identifier_factor(&mut self, factor: &ast::IdentifierFactor) {
        walk_evaluated_identifier(self, factor);
    }

    fn array_literal_item(&mut self, item: &ast::ArrayLiteralItem) {
        let (value, repeat, kind) = match item.array_literal_item_group.as_ref() {
            ast::ArrayLiteralItemGroup::ExpressionArrayLiteralItemOpt(x) => (
                x.expression.as_ref(),
                x.array_literal_item_opt
                    .as_ref()
                    .map(|x| x.expression.as_ref()),
                x.array_literal_item_opt.as_ref().map(|_| "repeat"),
            ),
            ast::ArrayLiteralItemGroup::DefaulColonExpression(x) => {
                (x.expression.as_ref(), None, Some("default"))
            }
        };
        if let Some(kind) = kind {
            // The parser's Expression -> TokenRange conversion can stop at
            // a function name, excluding arguments. Walk the tokens to retain
            // the complete expression span used by analyzed function calls.
            let mut range = ExpressionRange(value.into());
            range.expression(value);
            let mut token: TokenRange = item.into();
            if token.end.pos < range.0.end.pos {
                token.end = range.0.end;
            }
            self.0.push(ArrayItemSource {
                token,
                value: range.0,
                expression: value.clone(),
                kind,
            });
        }
        self.expression(value);
        if let Some(repeat) = repeat {
            self.expression(repeat);
        }
    }
}

// Use the same unevaluated context when collecting source-only items and
// examining their calls as when walking analyzed IR.
fn walk_evaluated_identifier(walker: &mut impl VerylWalker, factor: &ast::IdentifierFactor) {
    if let Ok(resolved) =
        veryl_analyzer::symbol_table::resolve(factor.expression_identifier.as_ref())
        && matches!(
            resolved.found.kind,
            veryl_analyzer::symbol::SymbolKind::SystemFunction(_)
        )
        && matches!(
            resolved.found.token.text.to_string().as_str(),
            "$bits" | "$size" | "bits" | "size"
        )
    {
        return;
    }
    walker.expression_identifier(&factor.expression_identifier);
    if let Some(opt) = &factor.identifier_factor_opt {
        match opt.identifier_factor_opt_group.as_ref() {
            ast::IdentifierFactorOptGroup::FunctionCall(x) => {
                walker.function_call(&x.function_call)
            }
            ast::IdentifierFactorOptGroup::StructConstructor(x) => {
                walker.struct_constructor(&x.struct_constructor)
            }
        }
    }
}

struct SourceEffects<'a> {
    module: &'a Module,
    defines: &'a HashSet<veryl_parser::resource_table::StrId>,
    observable: bool,
}

impl VerylWalker for SourceEffects<'_> {
    fn identifier_factor(&mut self, factor: &ast::IdentifierFactor) {
        if let Some(opt) = &factor.identifier_factor_opt
            && let ast::IdentifierFactorOptGroup::FunctionCall(call) =
                opt.identifier_factor_opt_group.as_ref()
            && let Ok(resolved) =
                veryl_analyzer::symbol_table::resolve(factor.expression_identifier.as_ref())
            && let veryl_analyzer::symbol::SymbolKind::Function(function) = &resolved.found.kind
        {
            self.observable |= function.has_side_effect_in(self.defines)
                || source_call_has_copyout(function, &call.function_call, self.defines)
                || self.module.functions.values().any(|body| {
                    body.path.sig.symbol == resolved.found.id
                        && crate::dynamic_for_check::function_has_observable_effect(
                            body,
                            self.module,
                        )
                });
        }
        walk_evaluated_identifier(self, factor);
    }
}

/// A written formal is observable only when its actual has a destination.
/// Named arguments bind by name, independently of their source order.
fn source_call_has_copyout(
    function: &veryl_analyzer::symbol::FunctionProperty,
    call: &ast::FunctionCall,
    defines: &HashSet<veryl_parser::resource_table::StrId>,
) -> bool {
    let Some(args) = &call.function_call_opt else {
        return false;
    };
    let items: Vec<_> = args.argument_list.as_ref().into();
    let written = function.written_output_paths(defines);
    items.iter().enumerate().any(|(index, item)| {
        let (name, actual) = if let Some(named) = &item.argument_item_opt {
            (
                item.argument_expression
                    .expression
                    .unwrap_identifier()
                    .map(|id| id.identifier().token.text),
                named.expression.as_ref(),
            )
        } else {
            (
                function.ports.get(index).map(|port| port.name()),
                item.argument_expression.expression.as_ref(),
            )
        };
        let discarded = actual.unwrap_identifier().is_some_and(|id| {
            veryl_parser::veryl_token::is_anonymous_token(&id.identifier().token)
        });
        !discarded
            && name.is_some_and(|name| {
                written
                    .iter()
                    .any(|path| path.paths.first().is_some_and(|root| root.base() == name))
            })
    })
}

fn contains(outer: &TokenRange, inner: &TokenRange) -> bool {
    outer.beg.source == inner.beg.source
        && outer.beg.pos <= inner.beg.pos
        && inner.end.pos <= outer.end.pos
}

struct ExpressionRange(TokenRange);

impl VerylWalker for ExpressionRange {
    fn veryl_token(&mut self, token: &veryl_parser::veryl_token::VerylToken) {
        if self.0.end.pos < token.token.pos {
            self.0.end = token.token;
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Check {
    OutputAliases,
    ArrayLiteralSideEffects,
}

fn check(
    ir: &Ir,
    kind: Check,
    sources: &[ArrayItemSource],
    defines: &HashSet<veryl_parser::resource_table::StrId>,
) -> Vec<FrontendDiagnostic> {
    let mut diagnostics = Vec::new();
    let mut seen = HashSet::default();
    let mut visited = HashSet::default();
    let mut modules: Vec<_> = ir
        .components
        .iter()
        .filter_map(|component| match component {
            Component::Module(module) => Some(module),
            _ => None,
        })
        .collect();
    while let Some(module) = modules.pop() {
        if !visited.insert(std::ptr::from_ref(module)) {
            continue;
        }
        let mut checker = Checker {
            kind,
            sources,
            defines,
            represented: HashSet::default(),
            retained_ranges: Vec::new(),
            module,
            diagnostics: &mut diagnostics,
            seen: &mut seen,
        };
        for declaration in &module.declarations {
            match declaration {
                Declaration::Comb(x) => checker.statements(&x.statements),
                Declaration::Ff(x) => checker.statements(&x.statements),
                Declaration::Initial(x) => checker.statements(&x.statements),
                Declaration::Final(x) => checker.statements(&x.statements),
                Declaration::Inst(x) => {
                    for input in &x.inputs {
                        for expression in &input.exprs {
                            checker.expression(expression);
                        }
                    }
                    for output in &x.outputs {
                        checker.destinations(&output.dst);
                    }
                    if let Component::Module(child) = x.component.as_ref() {
                        modules.push(child);
                    }
                }
                Declaration::External(_) | Declaration::Unsupported(_) | Declaration::Null => {}
            }
        }
        for function in module.functions.values() {
            for body in &function.functions {
                checker.statements(&body.statements);
            }
        }
        checker.eliminated_items();
    }
    diagnostics
}

struct Checker<'a, 'b> {
    kind: Check,
    sources: &'a [ArrayItemSource],
    defines: &'a HashSet<veryl_parser::resource_table::StrId>,
    represented: HashSet<usize>,
    retained_ranges: Vec<TokenRange>,
    module: &'a Module,
    diagnostics: &'b mut Vec<FrontendDiagnostic>,
    seen: &'b mut HashSet<(String, usize, usize)>,
}

impl Checker<'_, '_> {
    fn call(&mut self, call: &FunctionCall) {
        let outputs: Vec<_> = call
            .outputs
            .iter()
            .filter(|_| self.kind == Check::OutputAliases)
            .collect();
        'pairs: for (i, (left_arg, left)) in outputs.iter().enumerate() {
            for (right_arg, right) in outputs.iter().skip(i + 1) {
                // Flattened struct members of one formal belong to a single
                // copy-out; only compare different source-level arguments.
                if let Some(function) = self.module.functions.get(&call.id) {
                    let formal = |path| {
                        function
                            .args
                            .iter()
                            .find(|arg| arg.members.iter().any(|(member, _, _)| member == path))
                    };
                    if let (Some(a), Some(b)) = (formal(left_arg), formal(right_arg))
                        && a.name == b.name
                    {
                        continue;
                    }
                }
                for a in left.iter() {
                    for b in right.iter() {
                        if a.id != b.id
                            || !is_static_access(&a.index, &a.select)
                            || !is_static_access(&b.index, &b.select)
                        {
                            continue;
                        }
                        let (Ok(a_bits), Ok(b_bits)) = (
                            eval_var_select(self.module, a.id, &a.index, &a.select),
                            eval_var_select(self.module, b.id, &b.index, &b.select),
                        ) else {
                            continue;
                        };
                        let lsb = a_bits.lsb.max(b_bits.lsb);
                        let msb = a_bits.msb.min(b_bits.msb);
                        if lsb <= msb {
                            let token = &call.comptime.token;
                            let key = (
                                token.beg.source.to_string(),
                                token.beg.pos as usize,
                                token.end.pos as usize,
                            );
                            if self.seen.insert(key) {
                                self.diagnostics.push(
                                    FrontendDiagnostic::unspecified_output_copy_order(
                                        token,
                                        format!(
                                            "outputs `{left_arg}` and `{right_arg}` overlap on `{}[{msb}:{lsb}]`; the final value can depend on copy-out order",
                                            a.path
                                        ),
                                    ),
                                );
                            }
                            break 'pairs;
                        }
                    }
                }
            }
        }
        // Nested calls still need their own diagnostic, even if the enclosing
        // call already warned. Bodies are visited once at their declarations.
        for expression in call.inputs.values() {
            self.expression(expression);
        }
        for destinations in call.outputs.values() {
            self.destinations(destinations);
        }
    }

    fn destinations(&mut self, destinations: &[AssignDestination]) {
        for destination in destinations {
            self.select(&destination.index, &destination.select);
        }
    }

    fn select(&mut self, index: &VarIndex, select: &VarSelect) {
        for expression in index.0.iter().chain(&select.0) {
            self.expression(expression);
        }
        if let Some((_, expression)) = &select.1 {
            self.expression(expression);
        }
    }

    fn expression(&mut self, expression: &Expression) {
        self.retain_range(expression.comptime().token);
        self.array_item(expression);
        match expression {
            Expression::Term(factor) => match factor.as_ref() {
                Factor::FunctionCall(call) => self.call(call),
                Factor::SystemFunctionCall(call) => self.system_call(call),
                Factor::Variable(_, index, select, _) => self.select(index, select),
                Factor::HierVariable(x) => self.select(&x.index, &x.select),
                Factor::Value(_) | Factor::Anonymous(_) | Factor::Unknown(_) => {}
            },
            Expression::Unary(_, inner, _) => self.expression(inner),
            Expression::Binary(left, _, right, _) => {
                self.expression(left);
                self.expression(right);
            }
            Expression::Ternary(cond, left, right, _) => {
                self.expression(cond);
                self.expression(left);
                self.expression(right);
            }
            Expression::Concatenation(items, _) => {
                for (value, repeat) in items {
                    self.expression(value);
                    if let Some(repeat) = repeat {
                        self.expression(repeat);
                    }
                }
            }
            Expression::ArrayLiteral(items, _) => {
                for item in items {
                    match item {
                        ArrayLiteralItem::Value(value, repeat) => {
                            self.expression(value);
                            if let Some(repeat) = repeat {
                                self.expression(repeat);
                            }
                        }
                        ArrayLiteralItem::Defaul(value) => {
                            self.expression(value);
                        }
                    }
                }
            }
            Expression::StructConstructor(_, fields, _) => {
                for (_, value) in fields {
                    self.expression(value);
                }
            }
        }
    }

    fn array_item(&mut self, value: &Expression) {
        if self.kind != Check::ArrayLiteralSideEffects {
            return;
        }
        let token = &value.comptime().token;
        let matches: Vec<_> = self
            .sources
            .iter()
            .enumerate()
            .filter(|(_, source)| contains(&source.value, token))
            .map(|(index, source)| {
                self.represented.insert(index);
                source
            })
            .collect();
        if matches.is_empty()
            || !crate::dynamic_for_check::expression_has_observable_effect(value, self.module)
        {
            return;
        }
        for source in matches {
            self.warn_array_item(source);
        }
    }

    fn retain_range(&mut self, token: TokenRange) {
        if self.kind == Check::ArrayLiteralSideEffects {
            self.retained_ranges.push(token);
        }
    }

    fn eliminated_items(&mut self) {
        if self.kind != Check::ArrayLiteralSideEffects {
            return;
        }
        for (index, source) in self.sources.iter().enumerate() {
            if self.represented.contains(&index)
                || !self
                    .retained_ranges
                    .iter()
                    .any(|range| contains(range, &source.token))
            {
                continue;
            }
            // A source item must belong to a retained assignment or expression:
            // lexical module/function containment also includes inactive
            // conditional-compilation and generate branches.
            // An unused default can disappear entirely during array expansion.
            // Resolve its source calls against analyzer effect summaries and
            // any available specialized bodies instead of requiring surviving IR.
            let mut effects = SourceEffects {
                module: self.module,
                defines: self.defines,
                observable: false,
            };
            effects.expression(&source.expression);
            if effects.observable {
                self.warn_array_item(source);
            }
        }
    }

    fn warn_array_item(&mut self, source: &ArrayItemSource) {
        let token = &source.token;
        let key = (
            token.beg.source.to_string(),
            token.beg.pos as usize,
            token.end.pos as usize,
        );
        if self.seen.insert(key) {
            self.diagnostics.push(FrontendDiagnostic::undefined_array_literal_evaluation_count(
                token,
                format!("`{}` item has side effects; IEEE 1800-2023 §10.9.1 leaves its evaluation count undefined in the emitted SystemVerilog assignment pattern", source.kind),
            ));
        }
    }

    fn system_call(&mut self, call: &SystemFunctionCall) {
        match &call.kind {
            // Do not diagnose calls nested in unevaluated shape operands.
            SystemFunctionKind::Bits(_) | SystemFunctionKind::Size(_) => {}
            SystemFunctionKind::Clog2(x)
            | SystemFunctionKind::Onehot(x)
            | SystemFunctionKind::Signed(x)
            | SystemFunctionKind::Unsigned(x) => self.expression(&x.0),
            SystemFunctionKind::Readmemh(input, output) => {
                self.expression(&input.0);
                self.destinations(&output.0);
            }
            SystemFunctionKind::Display(args) | SystemFunctionKind::Write(args) => {
                for arg in args {
                    self.expression(&arg.0);
                }
            }
            SystemFunctionKind::Assert { cond, args, .. } => {
                self.expression(&cond.0);
                for arg in args {
                    self.expression(&arg.0);
                }
            }
            SystemFunctionKind::Finish => {}
        }
    }

    fn statements(&mut self, statements: &[Statement]) {
        for statement in statements {
            match statement {
                Statement::Assign(x) => {
                    // Array expansion preserves the original assignment span
                    // even when it drops an unused default expression.
                    self.retain_range(x.token);
                    self.expression(&x.expr);
                    self.destinations(&x.dst);
                }
                Statement::FunctionCall(call) => self.call(call),
                Statement::SystemFunctionCall(call) => self.system_call(call),
                Statement::If(x) => {
                    self.expression(&x.cond);
                    self.statements(&x.true_side);
                    self.statements(&x.false_side);
                }
                Statement::IfReset(x) => {
                    self.statements(&x.true_side);
                    self.statements(&x.false_side);
                }
                Statement::Case(x) => {
                    self.expression(&x.case_target);
                    for arm in &x.arms {
                        for pattern in &arm.patterns {
                            match pattern {
                                CasePattern::Eq(value) => self.expression(value),
                                CasePattern::Range { lo, hi, .. } => {
                                    self.expression(lo);
                                    self.expression(hi);
                                }
                            }
                        }
                        self.statements(&arm.body);
                    }
                    self.statements(&x.default);
                }
                Statement::For(x) => {
                    let (ForRange::Forward { start, end, .. }
                    | ForRange::Reverse { start, end, .. }
                    | ForRange::Stepped { start, end, .. }) = &x.range;
                    for bound in [start, end] {
                        if let ForBound::Expression(value) = bound {
                            self.expression(value);
                        }
                    }
                    self.statements(&x.body);
                }
                Statement::TbMethodCall(x) => {
                    if let Some(dst) = &x.ret {
                        self.select(&dst.index, &dst.select);
                    }
                    match &x.method {
                        TbMethod::ClockNext { count, period } => {
                            for value in count.iter().chain(period.as_deref()) {
                                self.expression(value);
                            }
                        }
                        TbMethod::ResetAssert { duration, .. } => {
                            if let Some(value) = duration {
                                self.expression(value);
                            }
                        }
                        TbMethod::FileOpen { name, .. } => self.expression(&name.0),
                        TbMethod::FileWrite { args } | TbMethod::Component { args, .. } => {
                            for arg in args {
                                self.expression(&arg.0);
                            }
                        }
                        TbMethod::RandomSeed { value } => self.expression(value),
                        TbMethod::RandomGetRange { min, max, .. } => {
                            self.expression(min);
                            self.expression(max);
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
    }
}
