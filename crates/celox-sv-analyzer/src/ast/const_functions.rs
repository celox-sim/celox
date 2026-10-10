//! Constant function calls (IEEE 1800-2023 13.4.3).
//!
//! A function called with constant arguments inside a constant expression,
//! such as a parameter initializer or a range bound, is evaluated during
//! elaboration. The functions of the module being analyzed are installed for
//! the duration of its analysis; the constant evaluator consults them for
//! calls that are not system functions. Bodies are interpreted from their
//! procedural statements.

use std::cell::{Cell, RefCell};

use super::*;
use crate::procedural::{CaseKind, CaseLabel, LoopKind};
use crate::typecheck::ConstantEnvironment;

/// Bounds on one evaluation, so a non-terminating constant function fails
/// instead of hanging the analyzer.
const MAX_ITERATIONS: usize = 1_000_000;
const MAX_DEPTH: usize = 64;

/// A function whose calls in constant expressions are evaluated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ConstantFunction {
    params: Vec<(String, crate::ir::Type)>,
    return_var: Option<(String, crate::ir::Type)>,
    body: Vec<Stmt>,
}

pub(super) type ConstantFunctionTable = HashMap<String, ConstantFunction>;

/// Why functions are not constant functions.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct FunctionErrors {
    /// Every function of the module, when their declarations could not be
    /// converted at all.
    all: Option<AnalyzerError>,
    by_name: HashMap<String, AnalyzerError>,
}

thread_local! {
    static FUNCTIONS: RefCell<Arc<ConstantFunctionTable>> = RefCell::new(Arc::default());
    static LOCALS: RefCell<Arc<HashMap<String, crate::ir::Type>>> = RefCell::new(Arc::default());
    static ERRORS: RefCell<Arc<FunctionErrors>> = RefCell::new(Arc::default());
    static DEPTH: Cell<usize> = const { Cell::new(0) };
}

/// Restores the previously installed functions when dropped.
pub(super) struct Installed {
    previous: ConstantFunctions,
}

impl Drop for Installed {
    fn drop(&mut self) {
        replace(std::mem::take(&mut self.previous));
    }
}

/// The functions of a module and the types of their locals.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct ConstantFunctions {
    functions: Arc<ConstantFunctionTable>,
    locals: Arc<HashMap<String, crate::ir::Type>>,
    /// Why the module's functions, or one of them by name, could not be
    /// converted: a call of such a function reports this.
    errors: Arc<FunctionErrors>,
}

/// Make `functions` the constant functions until the guard is dropped.
pub(super) fn install(functions: ConstantFunctions) -> Installed {
    let previous = installed();
    replace(functions);
    Installed { previous }
}

/// Replace the installed functions; the guard of the enclosing [`install`]
/// still restores the ones before it.
pub(super) fn replace(functions: ConstantFunctions) {
    FUNCTIONS.with(|current| *current.borrow_mut() = functions.functions);
    LOCALS.with(|current| *current.borrow_mut() = functions.locals);
    ERRORS.with(|current| *current.borrow_mut() = functions.errors);
}

pub(super) fn installed() -> ConstantFunctions {
    ConstantFunctions {
        functions: FUNCTIONS.with(|current| current.borrow().clone()),
        locals: LOCALS.with(|current| current.borrow().clone()),
        errors: ERRORS.with(|current| current.borrow().clone()),
    }
}

/// Why the function `name` of the installed module could not be converted,
/// if it could not.
pub(super) fn conversion_error(name: &str) -> Option<AnalyzerError> {
    ERRORS.with(|current| {
        let errors = current.borrow();
        errors.by_name.get(name).or(errors.all.as_ref()).cloned()
    })
}

pub(super) fn is_constant_function(name: &str) -> bool {
    FUNCTIONS.with(|current| current.borrow().contains_key(name))
}

/// The module-scope functions of a module, for constant evaluation. When
/// their bodies cannot be converted, there are none, and the reason is kept
/// for the calls of them.
impl ConstantFunctions {
    fn unconverted(error: AnalyzerError) -> Self {
        Self {
            errors: Arc::new(FunctionErrors {
                all: Some(error),
                by_name: HashMap::default(),
            }),
            ..Self::default()
        }
    }
}

impl ConstantFunctions {
    /// Add the functions of `other` that are not known already.
    pub(super) fn extend_missing(&mut self, other: &ConstantFunctions) {
        let functions = Arc::make_mut(&mut self.functions);
        for (name, function) in other.functions.iter() {
            functions
                .entry(name.clone())
                .or_insert_with(|| function.clone());
        }
        let locals = Arc::make_mut(&mut self.locals);
        for (name, r#type) in other.locals.iter() {
            locals.entry(name.clone()).or_insert_with(|| r#type.clone());
        }
        let errors = Arc::make_mut(&mut self.errors);
        if errors.all.is_none() {
            errors.all.clone_from(&other.errors.all);
        }
        for (name, error) in &other.errors.by_name {
            errors
                .by_name
                .entry(name.clone())
                .or_insert_with(|| error.clone());
        }
    }

    /// Keep the functions and locals whose names satisfy `keep`.
    pub(super) fn retain(&mut self, keep: impl Fn(&str) -> bool) {
        Arc::make_mut(&mut self.functions).retain(|name, _| keep(name));
        Arc::make_mut(&mut self.locals).retain(|name, _| keep(name));
        Arc::make_mut(&mut self.errors)
            .by_name
            .retain(|name, _| keep(name));
    }

    /// Make the function `target` callable as `name` too.
    pub(super) fn alias(&mut self, name: &str, target: &str) {
        if let Some(function) = self.functions.get(target).cloned() {
            Arc::make_mut(&mut self.functions).insert(name.to_string(), function);
        }
        if let Some(error) = self.errors.by_name.get(target).cloned() {
            Arc::make_mut(&mut self.errors)
                .by_name
                .insert(name.to_string(), error);
        }
    }

    /// These functions with their names, parameter and local names, and
    /// bodies renamed.
    pub(super) fn renamed(
        &self,
        name: &mut impl FnMut(&str) -> String,
        stmt: &mut impl FnMut(Stmt) -> Stmt,
    ) -> Self {
        let functions = self
            .functions
            .iter()
            .map(|(function_name, function)| {
                (
                    name(function_name),
                    ConstantFunction {
                        params: function
                            .params
                            .iter()
                            .map(|(param, r#type)| (name(param), r#type.clone()))
                            .collect(),
                        return_var: function
                            .return_var
                            .as_ref()
                            .map(|(var, r#type)| (name(var), r#type.clone())),
                        body: function.body.iter().cloned().map(&mut *stmt).collect(),
                    },
                )
            })
            .collect();
        let locals = self
            .locals
            .iter()
            .map(|(local, r#type)| (name(local), r#type.clone()))
            .collect();
        Self {
            functions: Arc::new(functions),
            locals: Arc::new(locals),
            errors: Arc::new(FunctionErrors {
                all: self.errors.all.clone(),
                by_name: self
                    .errors
                    .by_name
                    .iter()
                    .map(|(function, error)| (name(function), error.clone()))
                    .collect(),
            }),
        }
    }

    /// The names of the locals of these functions.
    pub(super) fn local_names(&self) -> impl Iterator<Item = &String> {
        self.locals.keys()
    }
}

pub(super) fn module_constant_functions(
    node: RefNode<'_>,
    tree: &SyntaxTree,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
    parameter_literals: &HashMap<String, Expr>,
) -> ConstantFunctions {
    let has_functions = node
        .clone()
        .into_iter()
        .any(|child| matches!(child, RefNode::FunctionDeclaration(_)));
    if !has_functions {
        return ConstantFunctions::default();
    }
    let (subroutine_params, subroutine_shapes) =
        match procedural::subroutine_argument_names(node.clone(), tree, const_env, type_aliases) {
            Ok(names) => names,
            Err(error) => return ConstantFunctions::unconverted(error),
        };
    let mut dimensions = PackedDimensions::new(HashMap::default(), const_env, type_aliases);
    dimensions.subroutine_param_shapes = Arc::new(subroutine_shapes);
    let mut locals = Vec::new();
    let mut counter = 0usize;
    let mut state = procedural::BodyState {
        locals: &mut locals,
        counter: &mut counter,
        subroutine_params: &subroutine_params,
    };
    let mut rejected = HashMap::default();
    let subroutines = match procedural::subroutines_from_module_node_with(
        node,
        tree,
        const_env,
        &dimensions,
        parameter_literals,
        &mut state,
        Some(&mut rejected),
    ) {
        Ok(subroutines) => subroutines,
        Err(error) => return ConstantFunctions::unconverted(error),
    };
    let functions = subroutines
        .into_iter()
        .filter(|subroutine| !subroutine.is_task)
        .filter_map(|subroutine| {
            let return_var = subroutine
                .return_var
                .clone()
                .zip(subroutine.return_type.clone());
            let params = subroutine
                .params
                .iter()
                .map(|param| (param.name.clone(), param.r#type.clone()))
                .collect();
            Some((
                subroutine.name.clone(),
                ConstantFunction {
                    params,
                    return_var: Some(return_var?),
                    body: subroutine.body,
                },
            ))
        })
        .collect();
    ConstantFunctions {
        errors: Arc::new(FunctionErrors {
            all: None,
            by_name: rejected,
        }),
        functions: Arc::new(functions),
        locals: Arc::new(
            locals
                .into_iter()
                .map(|local| (local.name, local.r#type))
                .collect(),
        ),
    }
}

/// The value of a call of an installed constant function.
pub(crate) fn eval_call(
    name: &str,
    args: &[crate::ir::ConstExpr],
    constants: &dyn ConstantEnvironment,
) -> Option<i128> {
    let functions = FUNCTIONS.with(|current| current.borrow().clone());
    let function = functions.get(name)?;
    let values = args
        .iter()
        .map(|arg| crate::typecheck::eval_const_expr_in_env(arg, constants))
        .collect::<Option<Vec<_>>>()?;
    let depth = DEPTH.with(Cell::get);
    if depth >= MAX_DEPTH {
        return None;
    }
    DEPTH.with(|current| current.set(depth + 1));
    let result = call(function, &values, constants);
    DEPTH.with(|current| current.set(depth));
    result
}

fn call(
    function: &ConstantFunction,
    args: &[i128],
    constants: &dyn ConstantEnvironment,
) -> Option<i128> {
    let frame = Frame {
        constants,
        values: HashMap::default(),
        types: HashMap::default(),
        temporaries: 0,
        iterations: 0,
    };
    call_in_frame(function, args, frame)
}

fn call_in_frame(function: &ConstantFunction, args: &[i128], mut frame: Frame<'_>) -> Option<i128> {
    if args.len() != function.params.len() {
        return None;
    }
    #[cfg(test)]
    FUNCTION_CALLS.with(|count| count.set(count.get() + 1));
    for ((name, r#type), value) in function.params.iter().zip(args) {
        frame.declare(name, r#type)?;
        frame.set(name, *value);
    }
    let (return_name, return_type) = function.return_var.as_ref()?;
    frame.declare(return_name, return_type)?;
    frame.set(return_name, 0);
    match frame.block(&function.body)? {
        Flow::Return(Some(value)) => {
            frame.set(return_name, value);
        }
        Flow::Next | Flow::Return(None) => {}
        Flow::Break | Flow::Continue => return None,
    }
    frame.values.get(return_name).copied()
}

enum Flow {
    Next,
    Break,
    Continue,
    Return(Option<i128>),
}

/// Calls borrow their caller's values; only parameters, locals, the return
/// variable, and typed temporaries are owned here. Nested calls borrow this
/// overlay, preserving the former copied environment's lookup precedence.
struct Frame<'a> {
    constants: &'a dyn ConstantEnvironment,
    values: HashMap<String, i128>,
    types: HashMap<String, (usize, bool)>,
    temporaries: usize,
    iterations: usize,
}

/// `value` as a `width`-bit value of the given signedness.
fn fit(value: i128, width: usize, signed: bool) -> i128 {
    if width == 0 || width >= 127 {
        return value;
    }
    let modulus = 1i128 << width;
    let truncated = value.rem_euclid(modulus);
    if signed && truncated >= modulus / 2 {
        truncated - modulus
    } else {
        truncated
    }
}

impl ConstantEnvironment for Frame<'_> {
    fn get(&self, name: &str) -> Option<&i128> {
        self.values.get(name).or_else(|| self.constants.get(name))
    }
}

impl Frame<'_> {
    fn type_of(&self, name: &str) -> Option<(usize, bool)> {
        self.types.get(name).copied().or_else(|| {
            dimensions::parameter_type_from_lookup(name, &|marker| {
                self.constants.get(marker).copied()
            })
            .map(|ty| (ty.width, ty.signed))
        })
    }

    fn eval_lowered(&self, expr: &crate::ir::ConstExpr) -> Option<i128> {
        crate::typecheck::eval_const_expr_with_types_in_env(expr, self, &|name| self.type_of(name))
    }

    fn declare(&mut self, name: &str, r#type: &crate::ir::Type) -> Option<()> {
        if !r#type.unpacked_ranges().is_empty() {
            return None;
        }
        let width = r#type
            .resolved_width()
            .or_else(|| {
                crate::typecheck::resolve_packed_width_in_env(r#type.packed_ranges(), self)
            })?
            .max(1);
        self.types
            .insert(name.to_string(), (width, r#type.is_signed()));
        Some(())
    }

    fn set(&mut self, name: &str, value: i128) {
        let value = match self.type_of(name) {
            Some((width, signed)) => fit(value, width, signed),
            None => value,
        };
        self.values.insert(name.to_string(), value);
    }

    fn tick(&mut self) -> Option<()> {
        self.iterations += 1;
        (self.iterations <= MAX_ITERATIONS).then_some(())
    }

    fn eval(&mut self, expr: &Expr) -> Option<i128> {
        let lowered: crate::ir::ConstExpr = self.lower(expr)?.into();
        // The literal evaluator propagates expression widths; the plain one
        // covers what it does not model.
        if let Some(literal) =
            crate::typecheck::eval_const_integral_literal_in_env(&lowered, self, &|name| {
                self.type_of(name)
            })
        {
            return crate::typecheck::integral_literal_as_i128(&literal, literal.signed);
        }
        self.eval_lowered(&lowered)
    }

    fn truth(&mut self, expr: &Expr) -> Option<bool> {
        Some(self.eval(expr)? != 0)
    }

    /// A temporary holding `value` with the given type.
    fn temporary(&mut self, value: i128, width: usize, signed: bool) -> ConstExpr {
        let name = format!("\0constant_function_temporary{}", self.temporaries);
        self.temporaries += 1;
        self.types.insert(name.clone(), (width.max(1), signed));
        self.set(&name, value);
        ConstExpr::Ident(name)
    }

    /// `expr` as a constant expression, with the parts the constant
    /// evaluator does not model replaced by typed temporaries.
    fn lower(&mut self, expr: &Expr) -> Option<ConstExpr> {
        Some(match expr {
            Expr::Ident(name) => ConstExpr::Ident(name.clone()),
            Expr::Literal(value) => ConstExpr::Literal(value.clone()),
            Expr::Unary { op, expr } => ConstExpr::Unary {
                op: *op,
                expr: Box::new(self.lower(expr)?),
            },
            Expr::Binary { left, op, right } => ConstExpr::Binary {
                left: Box::new(self.lower(left)?),
                op: *op,
                right: Box::new(self.lower(right)?),
            },
            Expr::Mux {
                condition,
                then_expr,
                else_expr,
            } => {
                // Only the selected arm is evaluated.
                if self.truth(condition)? {
                    self.lower(then_expr)?
                } else {
                    self.lower(else_expr)?
                }
            }
            Expr::Call { name, args } => ConstExpr::Function {
                name: name.clone(),
                args: args
                    .iter()
                    .map(|arg| self.lower(arg))
                    .collect::<Option<_>>()?,
                site: None,
            },
            Expr::Select { expr, msb, lsb, .. } => {
                let value = self.eval(expr)?;
                let msb = self.eval_lowered(&msb.clone().into())?;
                let lsb = self.eval_lowered(&lsb.clone().into())?;
                let (high, low) = (msb.max(lsb), msb.min(lsb));
                let width = usize::try_from(high - low).ok()? + 1;
                let shifted = value.checked_shr(u32::try_from(low).ok()?)?;
                self.temporary(fit(shifted, width, false), width, false)
            }
            Expr::Resize {
                expr,
                width,
                signed,
            } => {
                let value = self.eval(expr)?;
                self.temporary(fit(value, *width, *signed), *width, *signed)
            }
            Expr::Inside { expr, items } => {
                let mut found = false;
                for item in items {
                    found |= match item {
                        InsideItem::Value(item) => self.truth(&Expr::Binary {
                            left: expr.clone(),
                            op: BinaryOp::EqWildcard,
                            right: Box::new(item.clone()),
                        })?,
                        InsideItem::Range { low, high } => {
                            self.truth(&Expr::Binary {
                                left: Box::new(low.clone()),
                                op: BinaryOp::Le,
                                right: expr.clone(),
                            })? && self.truth(&Expr::Binary {
                                left: expr.clone(),
                                op: BinaryOp::Le,
                                right: Box::new(high.clone()),
                            })?
                        }
                    };
                }
                ConstExpr::Literal(format!("1'b{}", u8::from(found)))
            }
            Expr::Concat(_) | Expr::RepeatConcat { .. } => return None,
        })
    }

    fn block(&mut self, stmts: &[Stmt]) -> Option<Flow> {
        for stmt in stmts {
            match self.stmt(stmt)? {
                Flow::Next => {}
                flow => return Some(flow),
            }
        }
        Some(Flow::Next)
    }

    fn assign(&mut self, lhs: &LValue, value: i128) -> Option<()> {
        match lhs {
            LValue::Ident(name) => {
                if self.type_of(name).is_none() {
                    let r#type = LOCALS.with(|locals| locals.borrow().get(name).cloned())?;
                    self.declare(name, &r#type)?;
                }
                self.set(name, value);
            }
            LValue::Select { name, msb, lsb, .. } => {
                let msb = self.eval_lowered(&msb.clone().into())?;
                let lsb = self.eval_lowered(&lsb.clone().into())?;
                let (high, low) = (msb.max(lsb), msb.min(lsb));
                let width = u32::try_from(high - low).ok()? + 1;
                let low = u32::try_from(low).ok()?;
                if low + width >= 127 {
                    return None;
                }
                let mask = ((1i128 << width) - 1) << low;
                let old = *self.get(name)?;
                self.set(name, (old & !mask) | ((value << low) & mask));
            }
        }
        Some(())
    }

    fn stmt(&mut self, stmt: &Stmt) -> Option<Flow> {
        match stmt {
            Stmt::Assign { lhs, rhs, .. } => {
                let value = self.eval(rhs)?;
                self.assign(lhs, value)?;
            }
            Stmt::Local { name, init } => {
                let r#type = LOCALS.with(|locals| locals.borrow().get(name).cloned())?;
                self.declare(name, &r#type)?;
                let value = match init {
                    Some(init) => self.eval(init)?,
                    None => 0,
                };
                self.set(name, value);
            }
            Stmt::If {
                condition,
                then_body,
                else_body,
            } => {
                return if self.truth(condition)? {
                    self.block(then_body)
                } else {
                    self.block(else_body)
                };
            }
            Stmt::Case {
                kind,
                selector,
                items,
                default,
            } => {
                if matches!(kind, CaseKind::Z | CaseKind::X) {
                    return None;
                }
                // Each label compares as an equality with the selector, at
                // their common width (IEEE 1800-2023 12.5).
                for item in items {
                    for label in &item.labels {
                        let comparison = match (kind, label) {
                            (CaseKind::Inside, label) => Expr::Inside {
                                expr: Box::new(selector.clone()),
                                items: vec![match label {
                                    CaseLabel::Value(value) => InsideItem::Value(value.clone()),
                                    CaseLabel::Range { low, high } => InsideItem::Range {
                                        low: low.clone(),
                                        high: high.clone(),
                                    },
                                }],
                            },
                            (_, CaseLabel::Value(value)) => Expr::Binary {
                                left: Box::new(selector.clone()),
                                op: BinaryOp::Eq,
                                right: Box::new(value.clone()),
                            },
                            (_, CaseLabel::Range { .. }) => return None,
                        };
                        let matched = self.truth(&comparison)?;
                        if matched {
                            return self.block(&item.body);
                        }
                    }
                }
                if let Some(default) = default {
                    return self.block(default);
                }
            }
            Stmt::Loop {
                kind,
                init,
                condition,
                step,
                body,
            } => {
                if !matches!(self.block(init)?, Flow::Next) {
                    return None;
                }
                let mut remaining = match kind {
                    LoopKind::Repeat(count) => Some(self.eval(count)?),
                    _ => None,
                };
                let mut first = true;
                loop {
                    self.tick()?;
                    let check = !(first && matches!(kind, LoopKind::DoWhile));
                    first = false;
                    if let Some(remaining) = &mut remaining {
                        if *remaining <= 0 {
                            break;
                        }
                        *remaining -= 1;
                    }
                    if check
                        && let Some(condition) = condition
                        && !self.truth(condition)?
                    {
                        break;
                    }
                    match self.block(body)? {
                        Flow::Next | Flow::Continue => {}
                        Flow::Break => break,
                        Flow::Return(value) => return Some(Flow::Return(value)),
                    }
                    if !matches!(self.block(step)?, Flow::Next) {
                        return None;
                    }
                }
            }
            Stmt::Break => return Some(Flow::Break),
            Stmt::Continue => return Some(Flow::Continue),
            Stmt::Return(value) => {
                let value = match value {
                    Some(value) => Some(self.eval(value)?),
                    None => None,
                };
                return Some(Flow::Return(value));
            }
            // A discarded value must still be a constant.
            Stmt::Eval(expr) => {
                self.eval(expr)?;
            }
            // Messages have no effect on the value. Unknown and unsupported
            // system tasks are rejected before elaboration.
            Stmt::SystemTask { .. } => {}
            Stmt::AssignConcat { .. } | Stmt::Call { .. } => return None,
        }
        Some(Flow::Next)
    }
}

#[cfg(test)]
thread_local! {
    static FUNCTION_CALLS: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
mod tests;
