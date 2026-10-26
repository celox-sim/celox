//! Constant function calls (IEEE 1800-2023 13.4.3).
//!
//! A function called with constant arguments inside a constant expression,
//! such as a parameter initializer or a range bound, is evaluated during
//! elaboration. The functions of the module being analyzed are installed for
//! the duration of its analysis; the constant evaluator consults them for
//! calls that are not system functions. Bodies are interpreted from their
//! procedural statements.

use num_bigint::BigUint;
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
    argument_names: Vec<String>,
    argument_defaults: Vec<Option<Expr>>,
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

/// Constant evaluator state owned by one explicit analysis session.
#[derive(Default)]
pub(crate) struct State {
    functions: RefCell<ConstantFunctions>,
    revision: generate::Revision,
    depth: Cell<usize>,
    argument_names: RefCell<(
        Arc<ConstantFunctionTable>,
        Arc<HashMap<String, Vec<String>>>,
    )>,
}

pub(super) struct Installed {
    previous: ConstantFunctions,
    revision: u64,
    state: std::rc::Rc<AnalysisState>,
}

impl Drop for Installed {
    fn drop(&mut self) {
        let functions = &self.state.const_functions;
        replace(functions, std::mem::take(&mut self.previous));
        functions.revision.restore(self.revision);
    }
}

/// A numeric environment together with the functions visible at its call site.
pub(crate) struct Environment<'a> {
    pub(crate) constants: &'a dyn ConstantEnvironment,
    pub(crate) functions: &'a State,
}

impl ConstantEnvironment for Environment<'_> {
    fn get(&self, name: &str) -> Option<&i128> {
        self.constants.get(name)
    }
    fn function_state(&self) -> Option<&State> {
        Some(self.functions)
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
pub(super) fn install(
    state: &std::rc::Rc<AnalysisState>,
    functions: ConstantFunctions,
) -> Installed {
    let previous = installed(&state.const_functions);
    let revision = state.const_functions.revision.get();
    replace(&state.const_functions, functions);
    Installed {
        previous,
        revision,
        state: state.clone(),
    }
}

pub(super) fn replace(state: &State, functions: ConstantFunctions) {
    *state.functions.borrow_mut() = functions;
    state.revision.change();
}

pub(super) fn revision(state: &State) -> &generate::Revision {
    &state.revision
}

/// Why an installed function could not be converted for constant evaluation.
pub(super) fn installed(state: &State) -> ConstantFunctions {
    state.functions.borrow().clone()
}

pub(super) fn conversion_error(state: &State, name: &str) -> Option<AnalyzerError> {
    let functions = state.functions.borrow();
    functions
        .errors
        .by_name
        .get(name)
        .or(functions.errors.all.as_ref())
        .cloned()
}

pub(super) fn argument_names(state: &State) -> Arc<HashMap<String, Vec<String>>> {
    let functions = state.functions.borrow().functions.clone();
    let mut cached = state.argument_names.borrow_mut();
    if !Arc::ptr_eq(&cached.0, &functions) {
        let names = functions
            .iter()
            .map(|(name, function)| (name.clone(), function.argument_names.clone()))
            .collect();
        *cached = (functions, Arc::new(names));
    }
    cached.1.clone()
}

pub(super) fn is_constant_function(state: &State, name: &str) -> bool {
    state.functions.borrow().functions.contains_key(name)
}

/// The module-scope functions of a module, for constant evaluation. When
/// their bodies cannot be converted, there are none, and the reason is kept
/// for the calls of them.
impl ConstantFunctions {
    pub(crate) fn warnings(&self) -> Vec<super::warnings::AnalyzerWarning> {
        let mut warnings = Vec::new();
        for function in self.functions.values() {
            for statement in &function.body {
                statement.walk(&mut |statement| {
                    if let Stmt::SystemTask {
                        task: crate::system_functions::SystemTask::Exit,
                        site,
                        ..
                    } = statement
                    {
                        super::warnings::append(
                            &mut warnings,
                            [super::warnings::AnalyzerWarning::ExitOutsideProgram {
                                site: site.clone(),
                            }],
                        );
                    }
                });
            }
        }
        warnings.sort_by(|left, right| match (left, right) {
            (
                super::warnings::AnalyzerWarning::ExitOutsideProgram { site: left },
                super::warnings::AnalyzerWarning::ExitOutsideProgram { site: right },
            ) => left.cmp(right),
            (
                super::warnings::AnalyzerWarning::MixedTimescale,
                super::warnings::AnalyzerWarning::MixedTimescale,
            ) => std::cmp::Ordering::Equal,
            (super::warnings::AnalyzerWarning::MixedTimescale, _) => std::cmp::Ordering::Less,
            (_, super::warnings::AnalyzerWarning::MixedTimescale) => std::cmp::Ordering::Greater,
            // This collector emits only ExitOutsideProgram warnings.
            _ => std::cmp::Ordering::Equal,
        });
        warnings
    }

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

    pub(super) fn contains(&self, name: &str) -> bool {
        self.functions.contains_key(name) || self.errors.by_name.contains_key(name)
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
                        argument_names: function.argument_names.clone(),
                        argument_defaults: function
                            .argument_defaults
                            .iter()
                            .map(|default| {
                                default.clone().map(|expr| {
                                    let Stmt::Eval(expr) = stmt(Stmt::Eval(expr)) else {
                                        unreachable!("renaming preserves statement variants")
                                    };
                                    expr
                                })
                            })
                            .collect(),
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

    pub(super) fn declarations(&self) -> Vec<crate::ir::Subroutine> {
        self.functions
            .iter()
            .map(|(name, function)| crate::ir::Subroutine {
                name: name.clone(),
                sensitivity_body: Vec::new(),
                is_task: false,
                automatic_locals: Vec::new(),
                persistent_locals: Vec::new(),
                return_type: function.return_var.as_ref().map(|(_, ty)| ty.clone()),
                return_var: function.return_var.as_ref().map(|(name, _)| name.clone()),
                params: function
                    .params
                    .iter()
                    .enumerate()
                    .map(|(position, (name, ty))| crate::ir::SubroutineParam {
                        name: name.clone(),
                        source_name: function.argument_names[position].clone(),
                        direction: crate::procedural::ParamDirection::Input,
                        r#type: ty.clone(),
                        default: function.argument_defaults[position].clone().map(Into::into),
                    })
                    .collect(),
                body: function
                    .body
                    .iter()
                    .cloned()
                    .map(|statement| statement.map(&mut |expr| expr.into(), &mut |lhs| lhs.into()))
                    .collect(),
            })
            .collect()
    }

    /// The names of the locals of these functions.
    pub(super) fn local_names(&self) -> impl Iterator<Item = &String> {
        self.locals.keys()
    }
}

fn scope_flags(tree: &AnalysisContext<'_>) -> (bool, bool) {
    use celox_sv_syntax::{BodyItemKind, SubroutineKind};
    let native = tree.declarations();
    let mut pending = tree.body_membership().unwrap_or_default();
    let mut seen = HashSet::default();
    let (mut functions, mut controls) = (false, false);
    while let Some(id) = pending.pop() {
        if !seen.insert(id) {
            continue;
        }
        match &native.body_item(id).kind {
            BodyItemKind::Subroutine(id) => {
                functions |= native.subroutine(*id).kind == SubroutineKind::Function
            }
            BodyItemKind::Region(items)
            | BodyItemKind::Block { items, .. }
            | BodyItemKind::Unconverted { items, .. } => pending.extend(items),
            BodyItemKind::Unit(id) => pending.extend(&native.unit(*id).items),
            BodyItemKind::Conditional {
                then_body,
                otherwise,
                ..
            } => {
                controls = true;
                pending.push(*then_body);
                pending.extend(otherwise);
            }
            BodyItemKind::Case { arms, .. } => {
                controls = true;
                pending.extend(arms.iter().map(|arm| arm.body));
            }
            BodyItemKind::Loop { body, .. } => {
                controls = true;
                pending.push(*body);
            }
            _ => {}
        }
    }
    (functions, controls)
}

pub(super) fn module_constant_functions(
    tree: &AnalysisContext<'_>,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
    parameter_literals: &HashMap<String, Expr>,
) -> ConstantFunctions {
    symbolic_module_constant_functions(
        tree,
        const_env,
        type_aliases,
        parameter_literals,
        &HashSet::default(),
    )
}

pub(super) fn symbolic_module_constant_functions(
    tree: &AnalysisContext<'_>,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
    parameter_literals: &HashMap<String, Expr>,
    symbolic_parameters: &HashSet<String>,
) -> ConstantFunctions {
    let (has_functions, has_controls) = scope_flags(tree);
    if !has_functions {
        return ConstantFunctions::default();
    }
    let needs_bootstrap = !generate::root_subroutines_only(&tree.state) && has_controls;
    if !needs_bootstrap {
        return collect_module_constant_functions(
            tree,
            const_env,
            type_aliases,
            parameter_literals,
            symbolic_parameters,
        );
    }
    let mut roots = generate::with_root_subroutines(&tree.state, || {
        collect_module_constant_functions(
            tree,
            const_env,
            type_aliases,
            parameter_literals,
            symbolic_parameters,
        )
    });
    roots.extend_missing(&scope::imported(&tree.state).constant_functions);
    let _installed = install(&tree.state, roots);
    collect_module_constant_functions(
        tree,
        const_env,
        type_aliases,
        parameter_literals,
        symbolic_parameters,
    )
}

fn collect_module_constant_functions(
    tree: &AnalysisContext<'_>,
    const_env: &HashMap<String, i128>,
    type_aliases: &HashMap<String, Type>,
    parameter_literals: &HashMap<String, Expr>,
    symbolic_parameters: &HashSet<String>,
) -> ConstantFunctions {
    let (has_functions, _) = scope_flags(tree);
    if !has_functions {
        return ConstantFunctions::default();
    }
    // Native evaluation uses concrete types. Declaration dependency metadata
    // must also retain parameters appearing only in typedef dimensions.
    let symbolic_aliases;
    let type_aliases = if symbolic_parameters.is_empty() {
        type_aliases
    } else {
        let constants = const_env
            .iter()
            .filter(|(name, _)| !symbolic_parameters.contains(*name))
            .map(|(name, value)| (name.clone(), *value))
            .collect();
        symbolic_aliases = match type_aliases_from_scope_with_env(tree, &constants) {
            Ok(aliases) => aliases,
            Err(error) => return ConstantFunctions::unconverted(error),
        };
        &symbolic_aliases
    };
    let (subroutine_params, subroutine_shapes, subroutine_directions) =
        match procedural::subroutine_argument_names(tree, const_env, type_aliases) {
            Ok(names) => names,
            Err(error) => return ConstantFunctions::unconverted(error),
        };
    let mut dimensions = PackedDimensions::new(HashMap::default(), const_env, type_aliases);
    dimensions.subroutine_param_names = Arc::new(subroutine_params.clone());
    dimensions.subroutine_param_shapes = Arc::new(subroutine_shapes);
    dimensions.subroutine_directions = Arc::new(subroutine_directions.clone());
    let mut locals = Vec::new();
    let mut counter = 0usize;
    let mut state = procedural::BodyState {
        locals: &mut locals,
        counter: &mut counter,
        subroutine_params: &subroutine_params,
        subroutine_directions: &subroutine_directions,
        automatic: lifetimes::automatic_by_default(tree),
        statics: Vec::new(),
        static_initializers: Vec::new(),
        constant_evaluation: true,
    };
    let mut rejected = HashMap::default();
    let subroutines = match procedural::subroutines_with_symbolic_parameters(
        tree,
        const_env,
        &dimensions,
        parameter_literals,
        &mut state,
        Some(&mut rejected),
        symbolic_parameters,
    ) {
        Ok(subroutines) => subroutines,
        Err(error) => return ConstantFunctions::unconverted(error),
    };
    let readers = simulation_state_readers(&subroutines);
    let functions: ConstantFunctionTable = subroutines
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
                    argument_names: subroutine
                        .params
                        .iter()
                        .map(|param| param.source_name.clone())
                        .collect(),
                    argument_defaults: subroutine
                        .params
                        .iter()
                        .map(|param| param.default.clone())
                        .collect(),
                    params,
                    return_var: Some(return_var?),
                    body: subroutine.body,
                },
            ))
        })
        .collect();
    let mut functions = functions;
    for (name, error) in readers {
        functions.remove(&name);
        rejected.entry(name).or_insert(error);
    }
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

/// The functions among `functions` that are no constant functions because
/// they call a system function that reads the state of the simulation, such
/// as `$time`, in their bodies or argument defaults, or call a function that
/// does (IEEE 1800-2023 13.4.3), whether the call is reached or not. Each
/// comes with the error a constant call of it reports.
fn simulation_state_readers(functions: &[Subroutine]) -> HashMap<String, AnalyzerError> {
    let mut readers: HashMap<String, AnalyzerError> = HashMap::default();
    let mut callees: HashMap<&str, Vec<&str>> = HashMap::default();
    for function in functions.iter().filter(|function| !function.is_task) {
        let name = &function.name;
        let mut exprs: Vec<&Expr> = function
            .params
            .iter()
            .filter_map(|param| param.default.as_ref())
            .collect();
        let mut calls = Vec::new();
        let mut selectors = Vec::new();
        // Constant-folded branches still constrain whether a declaration is
        // a constant function. Void callees also matter for discarded calls.
        for stmt in function.body.iter().chain(&function.sensitivity_body) {
            // A constant function ignores its system task calls (13.4.3), so
            // their arguments call nothing.
            stmt.walk(&mut |stmt| {
                if let Stmt::Call { name, .. } = stmt {
                    calls.push(name.as_str());
                }
                let targets = match stmt {
                    Stmt::Assign { lhs, .. } => std::slice::from_ref(lhs),
                    Stmt::AssignConcat { parts, .. } => parts.as_slice(),
                    _ => &[],
                };
                for lhs in targets {
                    if let LValue::Select {
                        msb,
                        lsb,
                        array_slice_width,
                        ..
                    } = lhs
                    {
                        selectors.extend([msb, lsb]);
                        selectors.extend(array_slice_width);
                    }
                }
                if !matches!(stmt, Stmt::SystemTask { .. }) {
                    exprs.extend(lifetimes::stmt_exprs(stmt));
                }
            });
        }
        let mut callees_in_exprs = Vec::new();
        for expr in exprs {
            visit_expr_calls(expr, &mut |callee, _| callees_in_exprs.push(callee));
        }
        for selector in selectors {
            visit_const_calls(selector, &mut |callee, _| callees_in_exprs.push(callee));
        }
        let mut reads = None;
        for callee in callees_in_exprs {
            match callee {
                Callee::System(system) if system.reads_simulation_state() => {
                    reads.get_or_insert(*system);
                }
                Callee::System(_)
                | Callee::String(_)
                | Callee::Object(_)
                | Callee::Checked { .. } => {}
                Callee::Subroutine(callee) => calls.push(callee.as_str()),
            }
        }
        if let Some(system) = reads {
            readers.insert(
                name.clone(),
                AnalyzerError::InvalidSystemTfCall {
                    name: system.name().into(),
                    detail: format!("not permitted in constant function `{name}`"),
                },
            );
        }
        callees.insert(name, calls);
    }
    // The callers of a reader read the state too.
    loop {
        let found: Vec<(String, AnalyzerError)> = callees
            .iter()
            .filter(|(name, _)| !readers.contains_key(**name))
            .filter_map(|(name, calls)| {
                let error = calls.iter().find_map(|callee| readers.get(*callee))?;
                Some((name.to_string(), error.clone()))
            })
            .collect();
        if found.is_empty() {
            return readers;
        }
        readers.extend(found);
    }
}

/// Actual argument presence, including explicit holes in procedural calls.
#[derive(Clone, Copy)]
pub(in crate::ast) enum CallArguments<'a> {
    Expressions(&'a [Option<Expr>]),
    Constants(&'a [ConstExpr]),
}

impl CallArguments<'_> {
    pub(in crate::ast) fn is_present(self, index: usize) -> bool {
        match self {
            Self::Expressions(args) => args.get(index).is_some_and(Option::is_some),
            Self::Constants(args) => index < args.len(),
        }
    }
}

/// Calls in an expression, including the index expressions represented by
/// ConstExpr. Those indices can contain runtime calls in procedural code.
pub(in crate::ast) fn visit_expr_calls<'a>(
    expr: &'a Expr,
    f: &mut impl FnMut(&'a Callee, CallArguments<'a>),
) {
    match expr {
        Expr::Ident(_) | Expr::Literal(_) => {}
        Expr::Call { callee, args, .. } => {
            f(callee, CallArguments::Expressions(args));
            for arg in args.iter().flatten() {
                visit_expr_calls(arg, f);
            }
        }
        Expr::Select { expr, msb, lsb, .. } => {
            visit_expr_calls(expr, f);
            visit_const_calls(msb, f);
            visit_const_calls(lsb, f);
        }
        Expr::Resize { expr, .. } | Expr::Unary { expr, .. } => visit_expr_calls(expr, f),
        Expr::Concat(parts) => {
            for part in parts {
                visit_expr_calls(part, f);
            }
        }
        Expr::RepeatConcat { count, parts } => {
            visit_const_calls(count, f);
            for part in parts {
                visit_expr_calls(part, f);
            }
        }
        Expr::Binary { left, right, .. } => {
            visit_expr_calls(left, f);
            visit_expr_calls(right, f);
        }
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            visit_expr_calls(condition, f);
            visit_expr_calls(then_expr, f);
            visit_expr_calls(else_expr, f);
        }
        Expr::Inside { expr, items } => {
            visit_expr_calls(expr, f);
            for item in items {
                for expr in item.exprs() {
                    visit_expr_calls(expr, f);
                }
            }
        }
    }
}

pub(in crate::ast) fn visit_const_calls<'a>(
    expr: &'a ConstExpr,
    f: &mut impl FnMut(&'a Callee, CallArguments<'a>),
) {
    match expr {
        ConstExpr::Literal(_) | ConstExpr::Ident(_) => {}
        ConstExpr::Function { callee, args, .. } => {
            f(callee, CallArguments::Constants(args));
            for arg in args {
                visit_const_calls(arg, f);
            }
        }
        ConstExpr::Select { expr, bit } => {
            visit_const_calls(expr, f);
            visit_const_calls(bit, f);
        }
        ConstExpr::SelectRange { expr, msb, lsb, .. } => {
            visit_const_calls(expr, f);
            visit_const_calls(msb, f);
            visit_const_calls(lsb, f);
        }
        ConstExpr::Resize { expr, .. } | ConstExpr::Unary { expr, .. } => {
            visit_const_calls(expr, f)
        }
        ConstExpr::Binary { left, right, .. } => {
            visit_const_calls(left, f);
            visit_const_calls(right, f);
        }
        ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            visit_const_calls(condition, f);
            visit_const_calls(then_expr, f);
            visit_const_calls(else_expr, f);
        }
    }
}

/// Fill defaults in the declaration's bound scope; runtime IR keeps its holes.
pub(super) fn resolved_arguments(
    state: &State,
    callee: &Callee,
    args: &[Option<Expr>],
) -> Option<Vec<Expr>> {
    let functions = state.functions.borrow().functions.clone();
    let Some(function) = callee.subroutine().and_then(|name| functions.get(name)) else {
        return args.iter().cloned().collect();
    };
    if args.len() > function.params.len() {
        return None;
    }
    (0..function.params.len())
        .map(|position| {
            args.get(position)
                .cloned()
                .flatten()
                .or_else(|| function.argument_defaults[position].clone())
        })
        .collect()
}

/// Check arity and defaults without evaluating any actual argument.
pub(super) fn validate_argument_bindings(
    state: &State,
    name: &str,
    present: &[bool],
) -> Converted<()> {
    {
        let functions = state.functions.borrow();
        let functions = &functions.functions;
        let Some(function) = functions.get(name) else {
            // The raw syntax validator reports functions that cannot be used
            // in constant expressions, including conversion errors.
            return Ok(());
        };
        if present.len() > function.params.len() {
            return Err(unsupported(format!("too many arguments of `{name}`")));
        }
        for (position, default) in function.argument_defaults.iter().enumerate() {
            if !present.get(position).copied().unwrap_or(false) && default.is_none() {
                return Err(unsupported(format!(
                    "omitted argument {} of `{name}`",
                    position + 1
                )));
            }
        }
        Ok(())
    }
}

/// A named constant call can be folded only when its body and actuals evaluate.
pub(super) fn eval_expression_literal(
    state: &State,
    expr: &Expr,
    constants: &HashMap<String, i128>,
) -> Option<typecheck::IntegralLiteral> {
    let Expr::Call {
        callee: Callee::Subroutine(name),
        ..
    } = expr
    else {
        return None;
    };
    let env = Environment {
        constants,
        functions: state,
    };
    let constants: &dyn ConstantEnvironment = &env;
    let functions = state.functions.borrow().functions.clone();
    let (_, ty) = functions.get(name)?.return_var.as_ref()?;
    let width = ty
        .resolved_width()
        .or_else(|| typecheck::resolve_packed_width_in_env(ty.packed_ranges(), constants))?;
    let mut frame = Frame {
        constants,
        values: HashMap::default(),
        types: HashMap::default(),
        temporaries: 0,
        iterations: 0,
    };
    let value = frame.eval(expr)?;
    typecheck::parse_integral_literal(&format_typed_parameter_literal(
        value,
        width,
        ty.is_signed(),
    ))
}

/// The value of a call of an installed constant function.
pub(crate) fn eval_call(
    name: &str,
    args: &[crate::ir::ConstExpr],
    constants: &dyn ConstantEnvironment,
) -> Option<i128> {
    let state = constants.function_state()?;
    let functions = state.functions.borrow().functions.clone();
    let function = functions.get(name)?;
    let values = args
        .iter()
        .map(|arg| crate::typecheck::eval_const_expr_in_env(arg, constants))
        .collect::<Option<Vec<_>>>()?;
    let depth = state.depth.get();
    if depth >= MAX_DEPTH {
        return None;
    }
    struct DepthGuard<'a>(&'a Cell<usize>, usize);
    impl Drop for DepthGuard<'_> {
        fn drop(&mut self) {
            self.0.set(self.1);
        }
    }
    state.depth.set(depth + 1);
    let _depth = DepthGuard(&state.depth, depth);
    call(function, &values, constants)
}

/// Preserve the declared result type when freezing a caller's function value.
pub(super) fn eval_call_literal(
    state: &State,
    name: &str,
    args: &[crate::ir::ConstExpr],
    constants: &HashMap<String, i128>,
) -> Option<typecheck::IntegralLiteral> {
    let env = Environment {
        constants,
        functions: state,
    };
    let constants: &dyn ConstantEnvironment = &env;
    let functions = state.functions.borrow().functions.clone();
    let (_, ty) = functions.get(name)?.return_var.as_ref()?;
    let width = ty
        .resolved_width()
        .or_else(|| typecheck::resolve_packed_width_in_env(ty.packed_ranges(), constants))?;
    let value = eval_call(name, args, constants)?;
    typecheck::parse_integral_literal(&format_typed_parameter_literal(
        value,
        width,
        ty.is_signed(),
    ))
}

fn call(
    function: &ConstantFunction,
    args: &[i128],
    constants: &dyn ConstantEnvironment,
) -> Option<i128> {
    let mut frame = Frame {
        constants,
        values: HashMap::default(),
        types: HashMap::default(),
        temporaries: 0,
        iterations: 0,
    };
    let mut values = args.to_vec();
    for position in args.len()..function.params.len() {
        values.push(frame.eval(function.argument_defaults[position].as_ref()?)?);
    }
    call_in_frame(function, &values, frame)
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
    if width == 0 || width >= 128 {
        return value;
    }
    let mask = (1u128 << width) - 1;
    let truncated = (value as u128) & mask;
    if signed && truncated & (1u128 << (width - 1)) != 0 {
        (truncated | !mask) as i128
    } else {
        truncated as i128
    }
}

impl ConstantEnvironment for Frame<'_> {
    fn function_state(&self) -> Option<&State> {
        self.constants.function_state()
    }
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

    /// A bare unbased fill is assignment-sized, rather than a 32-bit value.
    /// Fill the interpreter's storage; `assign`/`set` narrows it to the target.
    fn assignment_value(&mut self, expr: &Expr) -> Option<i128> {
        if let Expr::Literal(text) = expr
            && text.trim_start().starts_with('\'')
            && let Some(text) = resize_unbased_fill_literal_for_cast(text, 128, false)
        {
            let literal = typecheck::parse_integral_literal(&text)?;
            return typecheck::integral_literal_as_i128(&literal, false);
        }
        self.eval(expr)
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
            Expr::Call { callee, args, .. } => ConstExpr::Function {
                callee: callee.clone(),
                args: resolved_arguments(self.constants.function_state()?, callee, args)?
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
            Expr::Concat(_) | Expr::RepeatConcat { .. } => {
                let literal = self.concat_literal(expr)?;
                if literal.width == 0 {
                    return None;
                }
                let value = crate::typecheck::integral_literal_as_i128(&literal, false)?;
                self.temporary(value, literal.width, false)
            }
        })
    }

    /// Fold concatenations with each operand's own width before the surrounding
    /// assignment can resize the value. This interpreter stores at most 128 bits.
    fn concat_literal(&mut self, expr: &Expr) -> Option<crate::typecheck::IntegralLiteral> {
        self.tick()?;
        let (parts, count) = match expr {
            Expr::Concat(parts) => (parts, 1),
            Expr::RepeatConcat { count, parts } => {
                let count = usize::try_from(self.eval_lowered(&count.clone().into())?).ok()?;
                (parts, count)
            }
            _ => {
                let lowered: crate::ir::ConstExpr = self.lower(expr)?.into();
                return crate::typecheck::eval_const_integral_literal_in_env(
                    &lowered,
                    self,
                    &|name| self.type_of(name),
                );
            }
        };
        let mut width = 0usize;
        let mut value = BigUint::default();
        let mut mask = BigUint::default();
        for part in parts {
            let literal = self.concat_literal(part)?;
            width = width.checked_add(literal.width)?;
            if width > 128 {
                return None;
            }
            value = (value << literal.width) | literal.value;
            mask = (mask << literal.width) | literal.mask;
        }
        let total = width.checked_mul(count)?;
        if total > 128 {
            return None;
        }
        let mut result_value = BigUint::default();
        let mut result_mask = BigUint::default();
        // A zero-width group remains empty for any count, without iterating.
        for _ in 0..if width == 0 { 0 } else { count } {
            result_value = (result_value << width) | &value;
            result_mask = (result_mask << width) | &mask;
        }
        Some(crate::typecheck::IntegralLiteral {
            width: total,
            signed: false,
            value: result_value,
            mask: result_mask,
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
                    let r#type = self
                        .constants
                        .function_state()?
                        .functions
                        .borrow()
                        .locals
                        .get(name)
                        .cloned()?;
                    self.declare(name, &r#type)?;
                }
                self.set(name, value);
            }
            LValue::Select { name, msb, lsb, .. } => {
                let msb = self.eval_lowered(&msb.clone().into())?;
                let lsb = self.eval_lowered(&lsb.clone().into())?;
                let (high, low) = (msb.max(lsb), msb.min(lsb));
                let width = u32::try_from(high.checked_sub(low)?).ok()?.checked_add(1)?;
                let low = u32::try_from(low).ok()?;
                if low.checked_add(width)? > 128 {
                    return None;
                }
                let mask = if width == 128 {
                    u128::MAX
                } else {
                    (1u128 << width) - 1
                } << low;
                let old = *self.get(name)? as u128;
                self.set(
                    name,
                    ((old & !mask) | (((value as u128) << low) & mask)) as i128,
                );
            }
        }
        Some(())
    }

    fn stmt(&mut self, stmt: &Stmt) -> Option<Flow> {
        match stmt {
            Stmt::Assign { lhs, rhs, .. } => {
                let value = self.assignment_value(rhs)?;
                self.assign(lhs, value)?;
            }
            Stmt::Local { name, init } => {
                let r#type = self
                    .constants
                    .function_state()?
                    .functions
                    .borrow()
                    .locals
                    .get(name)
                    .cloned()?;
                self.declare(name, &r#type)?;
                let value = match init {
                    Some(init) => self.assignment_value(init)?,
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
                    Some(value) => Some(self.assignment_value(value)?),
                    None => None,
                };
                return Some(Flow::Return(value));
            }
            // A discarded value must still be a constant.
            Stmt::Eval(expr) => {
                self.eval(expr)?;
            }
            // Messages have no effect on the value. Unknown and unsupported
            // system tasks are rejected before elaboration. `$exit` is
            // ignored outside a program block (IEEE 1800-2023 24.7). Its
            // diagnostic belongs to the declaration, not to evaluation.
            Stmt::SystemTask { .. } => {}
            // A function has no timing controls (IEEE 1800-2023 13.4).
            Stmt::Delay { .. } | Stmt::WaitEvent(_) | Stmt::WaitImplicit(_) | Stmt::Wait(_) => {
                return None;
            }
            Stmt::Fork { .. } | Stmt::AssignConcat { .. } | Stmt::Call { .. } => return None,
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
