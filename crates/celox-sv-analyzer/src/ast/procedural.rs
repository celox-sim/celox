//! Statement-structured procedural bodies.
//!
//! `always`, `initial`, and subroutine bodies are converted statement by
//! statement into [`Stmt`] trees. Variables declared inside a body are
//! lexically scoped (IEEE 1800-2023 6.21): each receives a module-unique name
//! (`name@n`) and is collected as a [`LocalVariable`], so a local may shadow a
//! module signal or share its name with a local of another process.

use super::*;
mod assertions;
mod assign_statements;
mod call_statements;
mod class_methods;
mod control;
mod display_formats;
mod effects;
mod final_procedures;
mod foreach;
mod initial_processes;
mod intra_assignment;
mod leaf_statements;
mod locals;
mod loops;
mod ordering;
use foreach::ForeachLoop;
pub(super) mod dpi_reports;
mod places;
pub(super) mod randomization;
pub(super) mod references;
pub(crate) mod static_references;
mod subroutines;
pub(super) mod timed_tasks;
mod timing;
use crate::procedural::{
    CaseItemBase, CaseKind, CaseLabel, EventEdge, EventItemBase, LoopKind, ParamDirection,
    SystemTaskArg,
};
use assertions::{assertion_failure, attribute_reports, deferred_action};
pub(super) use class_methods::*;
use display_formats::*;
pub(super) use initial_processes::*;
pub(super) use places::*;
pub(super) use subroutines::*;

/// One lexical scope: the locals it declares and the bindings they shadow.
#[derive(Clone)]
struct Scope {
    entries: Vec<ScopeEntry>,
    /// The types that the typedefs of the scope shadow, in declaration
    /// order.
    shadowed_types: Vec<(String, Option<Type>)>,
}

#[derive(Clone)]
struct ScopeEntry {
    source: String,
    unique: Option<String>,
    shadowed_dimensions: Option<VariableDimensions>,
    shadowed_constant_type: Option<semantic_types::ConstantType>,
    shadowed_constants: Vec<(String, Option<i128>)>,
    shadowed_literal: Option<String>,
    shadowed_parameter_value: Option<Expr>,
    shadowed_signedness: Option<bool>,
    /// Whether the name was one of `class_static_members` before this
    /// entry bound it: restored when the scope ends; `None` when the entry
    /// left the set as it was.
    shadowed_static_member: Option<bool>,
}

/// Per-module state shared by every body converted in that module.
pub(super) struct BodyState<'a> {
    pub locals: &'a mut Vec<LocalVariable>,
    pub counter: &'a mut usize,
    /// Positional argument names of each subroutine, for named arguments.
    pub subroutine_params: &'a HashMap<String, Vec<String>>,
    /// The argument directions of each subroutine.
    pub subroutine_directions: &'a lifetimes::Directions,
    /// Whether the variables of the scope's subroutines and blocks are
    /// automatic by default (IEEE 1800-2023 6.21).
    pub automatic: bool,
    /// The locals declared with a static lifetime: their unique names and
    /// types.
    pub statics: Vec<(String, Type)>,
    /// The statics declared with the `static` keyword.
    pub explicit_statics: HashSet<String>,
    /// The variables with a static lifetime of each subroutine lowered so
    /// far, by its owner name (see `BodyBuilder::set_owner`): by the names
    /// they are declared with, their unique names and types, which a
    /// hierarchical name `f.v` reaches (IEEE 1800-2023 23.6, see
    /// `static_references`).
    pub subroutine_statics: HashMap<String, HashMap<String, (String, Type)>>,
    /// Function static declarations initialized once at time zero.
    pub static_initializers: Vec<Stmt>,
    /// Constant calls get a fresh frame, even for static declarations (13.4.3).
    pub constant_evaluation: bool,
}

/// The methods of the classes of a scope that a pass over its subroutines
/// lowers.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum ClassMethods {
    /// Every method, with the dispatchers, constructors and helpers of each
    /// class, to execute them.
    All,
    /// The static functions: a constant function call has no object to call
    /// another method on (IEEE 1800-2023 8.10, 13.4.3).
    StaticFunctions,
    /// None: the pass describes the subroutines of the scope itself.
    None,
}

/// What a pass over the classes of a package lowered, which the next
/// analysis of the package takes over for the classes that cannot have
/// changed: a package is analyzed again for each scope that asks for
/// specializations of its classes (IEEE 1800-2023 8.25).
#[derive(Debug, Default)]
pub(in crate::ast) struct Lowered {
    /// By the names of the classes.
    classes: HashMap<String, std::rc::Rc<LoweredClass>>,
    /// The classes the pass saw, of the package and of its imports.
    known: HashSet<String>,
    /// The number after those of the locals the pass named.
    pub counter: usize,
}

#[cfg(test)]
thread_local! {
    /// Classes whose methods a pass over the classes of a scope lowered,
    /// rather than took over from the last analysis.
    pub(in crate::ast) static CLASS_LOWERINGS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Lower every class in every analysis, to compare with the analyses
    /// that take classes over.
    pub(in crate::ast) static LOWER_EVERY_CLASS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether a test asked for [`LOWER_EVERY_CLASS`].
fn lower_every_class() -> bool {
    #[cfg(test)]
    return LOWER_EVERY_CLASS.with(std::cell::Cell::get);
    #[cfg(not(test))]
    false
}

/// What lowering the methods of one class made.
struct LoweredClass {
    methods: Converted<Vec<Subroutine>>,
    locals: Vec<LocalVariable>,
    statics: Vec<(String, Type)>,
    explicit_statics: Vec<String>,
    static_initializers: Vec<Stmt>,
    inline_randomizations: Vec<classes::constraints::Inline>,
}

impl std::fmt::Debug for LoweredClass {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LoweredClass")
            .field("methods", &self.methods.as_ref().map(Vec::len))
            .finish_non_exhaustive()
    }
}

/// Where the records of a pass stand before a class is lowered.
struct Mark {
    locals: usize,
    statics: usize,
    explicit_statics: HashSet<String>,
    static_initializers: usize,
    inline_randomizations: usize,
}

impl Mark {
    fn of(session: &Session, state: &BodyState<'_>) -> Self {
        Self {
            locals: state.locals.len(),
            statics: state.statics.len(),
            explicit_statics: state.explicit_statics.clone(),
            static_initializers: state.static_initializers.len(),
            inline_randomizations: session.inline_randomizations.len(),
        }
    }
}

impl LoweredClass {
    /// What lowering a class to `methods` recorded since `mark`.
    fn since(
        mark: Mark,
        methods: Converted<Vec<Subroutine>>,
        session: &Session,
        state: &BodyState<'_>,
    ) -> Self {
        Self {
            methods,
            locals: state.locals[mark.locals..].to_vec(),
            statics: state.statics[mark.statics..].to_vec(),
            explicit_statics: state
                .explicit_statics
                .difference(&mark.explicit_statics)
                .cloned()
                .collect(),
            static_initializers: state.static_initializers[mark.static_initializers..].to_vec(),
            inline_randomizations: session.inline_randomizations[mark.inline_randomizations..]
                .to_vec(),
        }
    }

    /// Record again what lowering the class recorded.
    fn replay(&self, session: &mut Session, state: &mut BodyState<'_>) {
        state.locals.extend(self.locals.iter().cloned());
        state.statics.extend(self.statics.iter().cloned());
        state
            .explicit_statics
            .extend(self.explicit_statics.iter().cloned());
        state
            .static_initializers
            .extend(self.static_initializers.iter().cloned());
        session
            .inline_randomizations
            .extend(self.inline_randomizations.iter().cloned());
    }
}

/// The package whose classes a pass over `tree` lowers for every scope
/// that uses them, when it is one: the first analysis of a package that
/// other scopes may ask for specializations.
fn reused_package(
    tree: &AnalysisContext<'_>,
    session: &Session,
    state: &BodyState<'_>,
    symbolic_parameters: &HashSet<String>,
    class_methods: ClassMethods,
) -> Option<String> {
    (class_methods == ClassMethods::All
        && !state.constant_evaluation
        && symbolic_parameters.is_empty()
        && !lower_every_class())
    .then_some(tree.package.as_deref())
    .flatten()
    .filter(|package| session.foreign.owns(package))
    .map(str::to_owned)
}

/// The number the locals of the scope of `tree` are named from: after
/// those of the classes its analysis may take over from the last one.
pub(super) fn first_local(tree: &AnalysisContext<'_>, session: &Session) -> usize {
    tree.package
        .as_deref()
        .and_then(|package| session.foreign.lowered(package))
        .map_or(0, |lowered| lowered.counter)
}

/// The classes that a class which `previous` did not know is or extends:
/// their dispatchers, casts and randomization are over classes that pass
/// did not see (IEEE 1800-2023 8.20), so they are lowered again. None when
/// a class cannot be made, whose bases are not known then.
fn outdated_classes(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    const_env: &ConstEnv,
    type_aliases: &TypeAliases,
    previous: &Lowered,
    known: &HashSet<String>,
) -> Option<HashSet<String>> {
    let mut outdated = HashSet::default();
    let mut pending = known
        .iter()
        .filter(|name| !previous.known.contains(*name))
        .cloned()
        .collect::<Vec<_>>();
    while let Some(name) = pending.pop() {
        if !outdated.insert(name.clone()) {
            continue;
        }
        let class = classes::class(tree, session, &name, const_env, type_aliases).ok()?;
        pending.extend(class.base.iter().chain(&class.interfaces).cloned());
    }
    Some(outdated)
}

/// The statements and condition of a `for` loop header. A condition with
/// increment or decrement operators is the `prologue` of each iteration
/// instead (see [`BodyBuilder::loop_condition`]).
struct ForHeader {
    init: Vec<Stmt>,
    condition: Option<Expr>,
    prologue: Vec<Stmt>,
    steps: Vec<Stmt>,
}

/// Converts the statements of one procedural body.
pub(super) struct BodyBuilder<'s, 't, 'a> {
    tree: &'t AnalysisContext<'t>,
    pub(super) session: &'s mut Session,
    dims: PackedDimensions,
    names: Names,
    state: &'s mut BodyState<'a>,
    type_aliases: TypeAliases,
    /// The kind of body, for the system tasks it may call.
    body: system_functions::Body,
    /// Whether the body is a subroutine's, whatever `body` a detached
    /// fork has made of it while its branches are converted.
    subroutine: bool,
    /// Whether the body's variables are automatic by default.
    automatic: bool,
    task_timing: bool,
    /// While a delayed nonblocking assignment's delay control is being
    /// converted: that delay schedules an update without suspending, so a
    /// delay control is allowed in an `always` body for it (IEEE
    /// 1800-2023 9.4.5).
    delayed_nonblocking: bool,
    /// The shape of the function's result, the target of its return values:
    /// patterns take their shape from it (10.9) and streams align to it
    /// (11.4.14).
    return_shape: Option<VariableDimensions>,
    /// The formals of the subroutine that are references (`ref`), which
    /// name the actual's storage rather than a local of their own (IEEE
    /// 1800-2023 13.5.2).
    reference_formals: HashSet<String>,
    /// Whether the function's result is a chandle, `None` without a result.
    return_chandle: Option<bool>,
    pub(super) sensitivity_body: Vec<Stmt>,
    /// The name of the subroutine whose body this is, which the names of
    /// its variables carry (see [`Self::declare_static`], [`Self::declare`]).
    owner: Option<String>,
    /// The variables of the subroutine [`Self::declare`] named so far.
    owned_locals: usize,
    /// The named blocks enclosing the current statement that a `disable`
    /// can leave (9.6.2): their labels and block ids, innermost last, and
    /// whether a `disable` in the body has named the block. Only such a
    /// block stays a block in the statement tree.
    pub(super) named_blocks: Vec<(String, u64, bool)>,
    /// The named fork-branch blocks a `disable` in a sibling branch can
    /// kill (9.6.2): their labels, the hidden variables holding each
    /// branch's process handle and whether it is armed, and whether a
    /// `disable` has named the branch. Only such a branch takes a handle.
    pub(super) fork_blocks: Vec<(String, String, String, bool)>,
}

/// The names a body declares: its scopes of locals, innermost last, and
/// the values of its local parameters.
pub(super) struct Names {
    scopes: Vec<Scope>,
    /// The values of the local parameters declared in the body.
    local_constants: HashMap<String, String>,
}

impl Names {
    fn lookup(&self, source: &str) -> Option<&str> {
        for scope in self.scopes.iter().rev() {
            if let Some(entry) = scope
                .entries
                .iter()
                .rev()
                .find(|entry| entry.source == source)
            {
                // A parameter in an inner block also hides an outer variable.
                return entry.unique.as_deref();
            }
        }
        None
    }

    fn rename_name(&self, name: &mut String) {
        if let Some(unique) = self.lookup(name) {
            *name = unique.to_string();
        }
    }

    /// The literal value of a local parameter that `name` refers to.
    fn local_constant(&self, name: &str) -> Option<&String> {
        if self.lookup(name).is_some() {
            return None;
        }
        self.local_constants.get(name)
    }

    fn rename_const(&self, expr: &mut ConstExpr) {
        rename_const_idents(expr, &mut |name| self.rename_ident(name));
    }

    fn rename_expr(&self, expr: &mut Expr) {
        rename_expr_idents(expr, &mut |name| self.rename_ident(name));
    }

    /// Rename a referenced local, or return the value of a local parameter.
    fn rename_ident(&self, name: &mut String) -> Option<String> {
        match self.local_constant(name) {
            Some(value) => Some(value.clone()),
            None => {
                self.rename_name(name);
                None
            }
        }
    }

    fn rename_lvalue(&self, lvalue: &mut LValue) {
        match lvalue {
            // `OBJECT_VALUE` names no local, and keeps its name.
            LValue::Object {
                place,
                select,
                guards,
                ..
            } => {
                for expr in place.exprs_mut().into_iter().chain(guards) {
                    self.rename_expr(expr);
                }
                self.rename_lvalue(select);
            }
            LValue::Ident(name) => self.rename_name(name),
            LValue::Select {
                name,
                msb,
                lsb,
                array_slice_width,
                ..
            } => {
                self.rename_name(name);
                self.rename_const(msb);
                self.rename_const(lsb);
                if let Some(width) = array_slice_width {
                    self.rename_const(width);
                }
            }
        }
    }
}

/// The packed and unpacked shape of a declared type, as selects see it.
pub(super) fn dimensions_from_type(r#type: &Type) -> VariableDimensions {
    VariableDimensions::new(VariableShape {
        chandle: r#type.is_chandle(),
        container: r#type.container.clone(),
        randomizable: r#type.container.is_none()
            && !r#type.holds_chandle()
            && (!r#type.is_enum || r#type.enum_domain.is_some()),
        enum_domain: r#type.enum_domain.clone(),
        packed_enum: r#type.packed_enum,
        real: matches!(r#type.kind(), TypeKind::Real | TypeKind::Shortreal),
        string: matches!(r#type.kind(), TypeKind::String),
        class: r#type.class().map(str::to_owned),
        virtual_interface: r#type.virtual_interface.clone(),
        packed: dimensions::signal_packed_dimension_widths(r#type.packed_ranges()),
        unpacked: unpacked_dimension_widths(r#type.unpacked_ranges()),
        signed: r#type.is_signed(),
        is_2state: matches!(
            r#type.kind(),
            TypeKind::Bit
                | TypeKind::Chandle
                | TypeKind::Real
                | TypeKind::Shortreal
                | TypeKind::String
                | TypeKind::Object
                | TypeKind::Process
                | TypeKind::Event
        ),
        members: r#type.members.clone(),
        signed_element_depth: r#type.signed_element_depth,
        unpacked_struct: r#type.unpacked_struct.clone(),
        declared: type_names::Declared::new(r#type),
    })
}

/// A declaration whose type can be resolved in its own lexical environment.
pub(super) fn bound_dimensions_from_type(
    ty: &Type,
    functions: &const_functions::ConstantFunctions,
    table: &mut semantic_types::TyTable,
    env: &ConstEnv,
) -> VariableDimensions {
    let shape = dimensions_from_type(ty);
    shape.bind_type(table, functions, env);
    shape
}

/// `r#type` with the constants of its declaration scope substituted into its
/// ranges, so it keeps its meaning outside that scope.
pub(super) fn scoped_type(mut r#type: Type, const_env: &ConstEnv) -> Type {
    // The constants of the declaring scope become their values, which keeps
    // the semantic type.
    let shape = r#type.shape_keeping_binding();
    for range in &mut shape.packed_ranges {
        range.left = substitute_dimension_constants(range.left.clone(), const_env);
        range.right = substitute_dimension_constants(range.right.clone(), const_env);
    }
    for range in &mut shape.unpacked_ranges {
        range.left = substitute_dimension_constants(range.left.clone(), const_env);
        range.right = substitute_dimension_constants(range.right.clone(), const_env);
        range.size = range
            .size
            .take()
            .map(|size| substitute_dimension_constants(size, const_env));
    }
    r#type
}

/// Apply `rename` to each identifier of `expr`; one for which it returns a
/// literal is replaced by that literal.
pub(super) fn rename_const_idents(
    expr: &mut ConstExpr,
    rename: &mut impl FnMut(&mut String) -> Option<String>,
) {
    match expr {
        ConstExpr::Ident(name) => {
            if let Some(value) = rename(name) {
                *expr = ConstExpr::Literal(value);
            }
        }
        ConstExpr::Literal(_) => {}
        ConstExpr::SelectRange { expr, msb, lsb, .. } => {
            rename_const_idents(expr, rename);
            rename_const_idents(msb, rename);
            rename_const_idents(lsb, rename);
        }
        ConstExpr::Select { expr, bit } => {
            rename_const_idents(expr, rename);
            rename_const_idents(bit, rename);
        }
        ConstExpr::Function { args, .. } => {
            args.iter_mut()
                .for_each(|arg| rename_const_idents(arg, rename));
        }
        ConstExpr::Resize { expr, .. } | ConstExpr::Unary { expr, .. } => {
            rename_const_idents(expr, rename)
        }
        ConstExpr::Binary { left, right, .. } => {
            rename_const_idents(left, rename);
            rename_const_idents(right, rename);
        }
        ConstExpr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            rename_const_idents(condition, rename);
            rename_const_idents(then_expr, rename);
            rename_const_idents(else_expr, rename);
        }
    }
}

/// [`rename_const_idents`] for an expression and the positions it selects.
pub(super) fn rename_expr_idents(
    expr: &mut Expr,
    rename: &mut impl FnMut(&mut String) -> Option<String>,
) {
    match expr {
        Expr::Ident(name) => {
            if let Some(value) = rename(name) {
                *expr = Expr::Literal(value);
            }
        }
        Expr::Literal(_) => {}
        Expr::Select { expr, msb, lsb, .. } => {
            rename_expr_idents(expr, rename);
            rename_const_idents(msb, rename);
            rename_const_idents(lsb, rename);
        }
        Expr::Concat(parts) => parts
            .iter_mut()
            .for_each(|part| rename_expr_idents(part, rename)),
        Expr::RepeatConcat { count, parts } => {
            rename_const_idents(count, rename);
            parts
                .iter_mut()
                .for_each(|part| rename_expr_idents(part, rename));
        }
        Expr::Resize { expr, .. } | Expr::Unary { expr, .. } => rename_expr_idents(expr, rename),
        Expr::Binary { left, right, .. } => {
            rename_expr_idents(left, rename);
            rename_expr_idents(right, rename);
        }
        Expr::Mux {
            condition,
            then_expr,
            else_expr,
        } => {
            rename_expr_idents(condition, rename);
            rename_expr_idents(then_expr, rename);
            rename_expr_idents(else_expr, rename);
        }
        Expr::Call { args, .. } => args
            .iter_mut()
            .flatten()
            .for_each(|arg| rename_expr_idents(arg, rename)),
        Expr::Inside { expr, items } => {
            rename_expr_idents(expr, rename);
            for item in items {
                match item {
                    InsideItem::Value(value) => rename_expr_idents(value, rename),
                    InsideItem::Range { low, high } => {
                        rename_expr_idents(low, rename);
                        rename_expr_idents(high, rename);
                    }
                }
            }
        }
    }
}

/// The `int` type of a loop variable declared by `foreach`.
pub(super) fn int_type() -> Type {
    Type::from(TypeShape {
        container: None,
        is_enum: false,
        enum_domain: None,
        packed_enum: false,
        kind: TypeKind::Bit,
        is_signed: true,
        packed_ranges: vec![PackedRange::new(
            ConstExpr::Literal("31".to_string()),
            ConstExpr::Literal("0".to_string()),
        )],
        unpacked_ranges: Vec::new(),
        members: Vec::new(),
        signed_element_depth: None,
        class: None,
        virtual_interface: None,
        unpacked_struct: None,
        spelling: type_names::Spelling::keyword("int"),
    })
}

impl Drop for BodyBuilder<'_, '_, '_> {
    fn drop(&mut self) {
        self.flush_locator_locals();
    }
}

impl<'s, 't, 'a> BodyBuilder<'s, 't, 'a> {
    pub(super) fn new(
        tree: &'t AnalysisContext<'t>,
        session: &'s mut Session,
        dims: &PackedDimensions,
        state: &'s mut BodyState<'a>,
        body: system_functions::Body,
    ) -> Self {
        let type_aliases = (*dims.type_aliases).clone();
        let automatic = state.automatic;
        let mut dims = dims.clone();
        dims.locator_locals = Some(locators::LocalSink::new(*state.counter));
        let mut builder = Self {
            automatic,
            subroutine: body == system_functions::Body::Subroutine,
            task_timing: false,
            delayed_nonblocking: false,
            return_shape: None,
            reference_formals: HashSet::default(),
            return_chandle: None,
            sensitivity_body: Vec::new(),
            owner: None,
            owned_locals: 0,
            named_blocks: Vec::new(),
            fork_blocks: Vec::new(),
            tree,
            session,
            dims,
            names: Names {
                scopes: vec![Scope {
                    entries: Vec::new(),
                    shadowed_types: Vec::new(),
                }],
                local_constants: HashMap::default(),
            },
            state,
            type_aliases,
            body,
        };
        // A class whose layout fails is rejected where it is used.
        let _ = builder.bind_static_references();
        builder
    }

    /// Declare the locals that the array locator methods of the bound
    /// expressions use (see [`locators`]).
    pub(super) fn flush_locator_locals(&mut self) {
        if let Some(sink) = &self.dims.locator_locals {
            for local in sink.take() {
                // A call bound again declares the same locals.
                if !self
                    .state
                    .locals
                    .iter()
                    .any(|known| known.name == local.name)
                {
                    self.state.locals.push(local);
                }
            }
        }
    }

    fn push_scope(&mut self) {
        self.names.scopes.push(Scope {
            entries: Vec::new(),
            shadowed_types: Vec::new(),
        });
    }

    /// `bind` in the scope of the design element, as if none of the
    /// body's blocks and formals were declared: the bindings of a named
    /// sequence or property body are those of its declaration (IEEE
    /// 1800-2023 16.8).
    fn in_declaration_scope<T>(&mut self, bind: impl FnOnce(&mut Self) -> T) -> T {
        let dims = self.dims.clone();
        let type_aliases = self.type_aliases.clone();
        let scopes = self.names.scopes.clone();
        while !self.names.scopes.is_empty() {
            self.pop_scope();
        }
        let result = bind(self);
        self.dims = dims;
        self.type_aliases = type_aliases;
        self.names.scopes = scopes;
        result
    }

    fn pop_scope(&mut self) {
        let Some(scope) = self.names.scopes.pop() else {
            return;
        };
        for (name, shadowed) in scope.shadowed_types.into_iter().rev() {
            match shadowed {
                Some(ty) => {
                    self.type_aliases.insert(name.clone(), ty.clone());
                    self.dims.type_aliases.insert(name, ty);
                }
                None => {
                    self.type_aliases.remove(&name);
                    self.dims.type_aliases.remove(&name);
                }
            }
        }
        for entry in scope.entries.into_iter().rev() {
            match entry.shadowed_constant_type {
                Some(binding) => {
                    Arc::make_mut(&mut self.dims.constant_types)
                        .insert(entry.source.clone(), binding);
                }
                None if self.dims.constant_types.contains_key(&entry.source) => {
                    Arc::make_mut(&mut self.dims.constant_types).remove(&entry.source);
                }
                None => {}
            }
            for (key, value) in entry.shadowed_constants {
                if let Some(value) = value {
                    self.dims.const_env.insert(key, value);
                } else if self.dims.const_env.contains_key(&key) {
                    self.dims.const_env.remove(&key);
                }
            }
            match entry.shadowed_literal {
                Some(value) => {
                    self.names
                        .local_constants
                        .insert(entry.source.clone(), value);
                }
                None => {
                    self.names.local_constants.remove(&entry.source);
                }
            }
            match entry.shadowed_parameter_value {
                Some(value) => {
                    self.dims
                        .parameter_values
                        .insert(entry.source.clone(), value);
                }
                None if self.dims.parameter_values.contains_key(&entry.source) => {
                    self.dims.parameter_values.remove(&entry.source);
                }
                None => {}
            }
            match entry.shadowed_dimensions {
                Some(dimensions) => {
                    self.dims.insert(entry.source.clone(), dimensions);
                }
                None => {
                    self.dims.remove(&entry.source);
                }
            }
            let signedness = &mut self.dims.expression_signedness;
            match entry.shadowed_static_member {
                Some(true) => {
                    Arc::make_mut(&mut self.dims.class_static_members).insert(entry.source.clone());
                }
                Some(false) => {
                    Arc::make_mut(&mut self.dims.class_static_members).remove(&entry.source);
                }
                None => {}
            }
            match entry.shadowed_signedness {
                Some(signed) => {
                    signedness.insert(entry.source, signed);
                }
                None => {
                    signedness.remove(&entry.source);
                }
            }
        }
    }

    /// Declare a local in the innermost scope and return its unique name.
    /// One of a subroutine is named by the subroutine and the order of its
    /// declaration in it: every analysis that lowers the subroutine names it
    /// the same, and none names a variable of another subroutine so (see
    /// [`Self::declare_owned`]). The counter of the analysis names the
    /// others.
    pub(super) fn declare(&mut self, source: &str, r#type: Type) -> String {
        let unique = match &self.owner {
            Some(owner) => {
                let ordinal = self.owned_locals;
                self.owned_locals += 1;
                format!(
                    "{source}%{:016x}@{ordinal}",
                    fxhash::hash64(&(owner.as_str(), "local", ordinal))
                )
            }
            None => format!("{source}@{}", *self.state.counter),
        };
        let owned = self.owner.is_some();
        self.declare_as(source, r#type, unique, owned)
    }

    /// Name the variables of the body after subroutine `owner` (see
    /// [`Self::declare`]).
    pub(super) fn set_owner(&mut self, owner: String) {
        self.dims.locator_locals = Some(locators::LocalSink::owned(&owner));
        self.owner = Some(owner);
        self.owned_locals = 0;
    }

    /// Declare a local with a static lifetime, declared at token
    /// `declaration` of its syntax tree. A static variable is one object per
    /// declaration and owner (IEEE 1800-2023 6.21, 8.25): in a subroutine,
    /// its unique name is keyed by the subroutine's qualified name, which
    /// tells apart the package, class specialization and method, and by the
    /// declaration, so every analysis that declares it, of the package, of
    /// its state or of a module importing it, names the same variable, and
    /// no other owner's. A counter numbers the declarations of one analysis
    /// only. The key is a hash, since a name with `::` would read as a
    /// package item: `x%<key>@<declaration>`.
    pub(super) fn declare_static(
        &mut self,
        source: &str,
        r#type: Type,
        declaration: usize,
    ) -> String {
        let unique = self.declare_owned(source, r#type.clone(), declaration);
        self.register_static(source, &unique, r#type);
        unique
    }

    /// Record static variable `source` of the subroutine being lowered,
    /// named `unique`, of type `r#type`, for the hierarchical names that
    /// reach it (see `static_references`). The first of a name is the
    /// subroutine's own, before any of its nested blocks.
    pub(super) fn register_static(&mut self, source: &str, unique: &str, r#type: Type) {
        if let Some(owner) = &self.owner {
            self.state
                .subroutine_statics
                .entry(owner.clone())
                .or_default()
                .entry(source.to_owned())
                .or_insert_with(|| (unique.to_owned(), r#type));
        }
    }

    /// Declare a variable of the subroutine being lowered, declared at
    /// token `declaration`, and return its unique name: keyed by the
    /// subroutine's qualified name and the declaration, as a static
    /// variable is (see [`Self::declare_static`]), so that every analysis
    /// lowering the subroutine names it the same, whatever it counted
    /// before. Outside a subroutine, the counter names it.
    pub(super) fn declare_owned(
        &mut self,
        source: &str,
        r#type: Type,
        declaration: usize,
    ) -> String {
        let unique = match &self.owner {
            Some(owner) => format!(
                "{source}%{:016x}@{declaration}",
                fxhash::hash64(&(owner.as_str(), declaration))
            ),
            None => format!("{source}@{}", *self.state.counter),
        };
        let owned = self.owner.is_some();
        self.declare_as(source, r#type, unique, owned)
    }

    /// The identifier of the named block declared at token `declaration`
    /// of its syntax tree, which names the variables its `disable` shares
    /// between the activations of the block. A module lowers the
    /// subroutines of the packages it imports with its own, and each
    /// analysis numbers its declarations from zero, so a block of a
    /// subroutine is keyed as a static variable of it is (see
    /// [`Self::declare_static`]): by the subroutine's qualified name and
    /// the declaration, with the top bit set. A block of a process body
    /// takes the next number of its module's analysis, top bit clear.
    pub(super) fn block_id(&mut self, declaration: usize) -> u64 {
        let id = match &self.owner {
            Some(owner) => fxhash::hash64(&(owner.as_str(), declaration)) | 1 << 63,
            None => *self.state.counter as u64,
        };
        *self.state.counter += 1;
        id
    }

    fn declare_as(&mut self, source: &str, r#type: Type, unique: String, owned: bool) -> String {
        *self.state.counter += 1;
        let shape = bound_dimensions_from_type(
            &r#type,
            &self.tree.functions,
            &mut self.session.types,
            &self.dims.const_env,
        );
        let entry = self.take_binding(source, Some(unique.clone()), r#type.packed_ranges.len());
        self.dims.insert(source.to_string(), shape);
        self.dims
            .expression_signedness
            .insert(source.to_string(), r#type.is_signed());
        self.state.locals.push(LocalVariable {
            name: unique.clone(),
            source_name: source.to_string(),
            r#type: super::ir_type(
                self.session,
                &self.tree.functions,
                scoped_type(r#type, &self.dims.const_env),
                &self.dims.const_env,
            ),
            owned,
        });
        self.names
            .scopes
            .last_mut()
            .expect("a body always has a scope")
            .entries
            .push(entry);
        unique
    }

    /// Detach every view of a declaration before publishing the shadowing
    /// binding. Save only keys for this name, without copying a growing scope.
    fn take_binding(
        &mut self,
        source: &str,
        unique: Option<String>,
        packed_count: usize,
    ) -> ScopeEntry {
        let keys = parameters::binding_keys(source, &self.dims.const_env, packed_count);
        ScopeEntry {
            source: source.to_string(),
            unique,
            shadowed_constants: keys
                .into_iter()
                .map(|key| {
                    // Most names hide no constant: the shared tables are
                    // edited, and so copied, only for those that do.
                    let value = if self.dims.const_env.contains_key(&key) {
                        self.dims.const_env.remove(&key)
                    } else {
                        None
                    };
                    (key, value)
                })
                .collect(),
            shadowed_literal: self.names.local_constants.remove(source),
            shadowed_parameter_value: if self.dims.parameter_values.contains_key(source) {
                self.dims.parameter_values.remove(source)
            } else {
                None
            },
            shadowed_dimensions: self.dims.remove(source),
            shadowed_constant_type: if self.dims.constant_types.contains_key(source) {
                Arc::make_mut(&mut self.dims.constant_types).remove(source)
            } else {
                None
            },
            shadowed_signedness: self.dims.expression_signedness.remove(source),
            // Any binding of the name hides a static property of it.
            shadowed_static_member: self
                .dims
                .class_static_members
                .contains(source)
                .then(|| Arc::make_mut(&mut self.dims.class_static_members).remove(source)),
        }
    }

    /// Hide the module-scope names that the properties and methods of
    /// `class` shadow inside its methods (IEEE 1800-2023 8.5, 23.9), until
    /// the scope ends.
    /// A static property's name is renamed to the module variable that holds
    /// it (IEEE 1800-2023 8.9).
    fn enter_class_scope(&mut self, class: &classes::Class) {
        self.dims.class_scope = Some(class.name.clone());
        let constants =
            classes::specializations::constant_types(self.tree, self.session, &class.name);
        Arc::make_mut(&mut self.dims.constant_types).extend(constants);
        // The name of a property of unpacked structure type, whose members
        // are properties named by their member paths, is hidden too.
        let aggregates = class
            .aggregates
            .iter()
            .filter(|aggregate| !aggregate.name.contains('.'))
            .map(|aggregate| aggregate.name.as_str());
        // A method's name hides a module variable too, so that an
        // unqualified call names the method.
        let methods = class.methods.iter().map(|method| method.name.as_str());
        for name in class
            .properties
            .iter()
            .map(|property| property.name.as_str())
            .chain(aggregates)
            .chain(methods)
        {
            let entry = self.take_binding(name, None, 0);
            self.names
                .scopes
                .last_mut()
                .expect("a body always has a scope")
                .entries
                .push(entry);
        }
        for property in &class.statics {
            // A base's local static is not visible in the derived class (8.18).
            // Keep it out of the alias table so binding can diagnose its use.
            if property.local && property.owner != class.name {
                continue;
            }
            let shape = bound_dimensions_from_type(
                &property.r#type,
                &self.tree.functions,
                &mut self.session.types,
                &self.dims.const_env,
            );
            let mut entry = self.take_binding(
                &property.name,
                Some(property.variable.clone()),
                property.r#type.packed_ranges.len(),
            );
            // The name denotes the static property until the scope ends or
            // a local hides it.
            entry.shadowed_static_member.get_or_insert(false);
            Arc::make_mut(&mut self.dims.class_static_members).insert(property.name.clone());
            self.dims.insert(property.name.clone(), shape);
            self.dims
                .expression_signedness
                .insert(property.name.clone(), property.r#type.is_signed());
            self.names
                .scopes
                .last_mut()
                .expect("a body always has a scope")
                .entries
                .push(entry);
        }
        // `m.v` of a static variable of a method `m` of the class (23.6). A
        // class whose layout fails is rejected where it is used.
        let _ = self.bind_static_references();
    }

    fn rename_expr(&self, expr: &mut Expr) {
        self.names.rename_expr(expr);
    }

    fn rename_lvalue(&self, lvalue: &mut LValue) {
        self.names.rename_lvalue(lvalue);
    }

    fn native_value(&mut self, id: celox_sv_syntax::ExprId) -> Result<Expr, AnalyzerError> {
        let mut value = expression_binding::value(
            id,
            self.tree.declarations(),
            self.tree,
            self.session,
            &self.dims,
        )?;
        self.rename_expr(&mut value);
        Ok(value)
    }

    /// [`Self::native_value`] of a value that may be a chandle: a condition
    /// tested as a Boolean, or a value compared or assigned as a chandle
    /// (IEEE 1800-2023 6.14).
    fn native_chandle_value(&mut self, id: celox_sv_syntax::ExprId) -> Result<Expr, AnalyzerError> {
        let mut value = expression_binding::bind_chandle_value(
            id,
            self.tree.declarations(),
            self.tree,
            self.session,
            &self.dims,
            None,
        )?;
        self.rename_expr(&mut value);
        Ok(value)
    }

    /// The target `id` of a write, which may be a chandle if `chandle`.
    fn native_lvalue(
        &mut self,
        id: celox_sv_syntax::ExprId,
        chandle: bool,
    ) -> Result<(LValue, LValue), AnalyzerError> {
        let lowered = expression_binding::variable_lvalue(
            id,
            self.tree.declarations(),
            self.tree,
            self.session,
            &self.dims,
            chandle,
        )?;
        virtual_interfaces::check_target(&lowered)?;
        let mut renamed = lowered.clone();
        self.rename_lvalue(&mut renamed);
        Ok((lowered, renamed))
    }
}

/// Qualify the module-scope names of a statement for its generate scope.
pub(super) fn qualify_stmt(item: &generate::Item, stmt: &mut Stmt) {
    stmt.visit_mut(
        &mut |expr| item.expr(expr),
        &mut |lvalue| item.lvalue(lvalue),
        &mut |name| {
            if !name.contains('@') {
                *name = item.name(name);
            }
        },
    );
}

/// Replace parameters and genvars by their values.
pub(super) fn substitute_stmt_constants(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    stmt: &mut Stmt,
    const_env: &ConstEnv,
    literals: &HashMap<String, Expr>,
) {
    stmt.visit_mut(
        &mut |expr| {
            *expr = substitute_expr_constants_with_parameter_literals(
                tree,
                session,
                expr.clone(),
                const_env,
                literals,
            );
        },
        &mut |lvalue| {
            *lvalue = substitute_lvalue_constants(lvalue.clone(), const_env, literals);
        },
        &mut |_| {},
    );
}

/// Original invocation span of a native token range, when it belongs to one file.
pub(super) fn source_span(
    tokens: celox_sv_syntax::TokenRange,
    tree: &SyntaxTree,
) -> Option<crate::procedural::SourceSpan> {
    let native = tree.declarations();
    let mut result: Option<crate::procedural::SourceSpan> = None;
    for token in native
        .tokens
        .get(tokens.start..tokens.end)?
        .iter()
        .filter(|token| !token.is_trivia())
    {
        let span = native.sources.invocation(token.origin);
        let path = native.sources.file(span.file).path.clone();
        if let Some(current) = &mut result {
            if current.path != path {
                return None;
            }
            current.start = current.start.min(span.start);
            current.end = current.end.max(span.end);
        } else {
            let (logical_path, line, _) = native.sources.logical_location(span);
            result = Some(crate::procedural::SourceSpan {
                logical_path: logical_path.to_owned(),
                line,
                path,
                start: span.start,
                end: span.end,
            });
        }
    }
    result
}

/// A `logic` vector of `width` bits.
pub(in crate::ast) fn vector_type(width: usize, signed: bool) -> Type {
    let mut r#type = Type::new(TypeKind::Logic);
    r#type.is_signed = signed;
    if width > 1 {
        r#type.packed_ranges = vec![PackedRange::new(
            ConstExpr::Literal((width - 1).to_string()),
            ConstExpr::Literal("0".into()),
        )];
    }
    r#type
}

#[cfg(test)]
mod enum_binding_tests;
