//! The symbols one scope makes visible to another: the items of a package,
//! and the package items a module or package imports (IEEE 1800-2023 26).
//!
//! A package is analyzed once, with the collectors that analyze a module.
//! Its symbols are then exported under their qualified names, `p::x`, and
//! every reference a body makes to another item of the package, or to an
//! item the package imports, is bound to that item's qualified name. A scope
//! that uses packages starts its own analysis from the symbols of those
//! packages, plus an alias for each name its imports make visible.

use std::cell::RefCell;

use super::*;

/// The symbols of a scope, independent of the syntax they came from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct ScopeSymbols {
    /// Constant values of parameters and enum members, with their type
    /// markers.
    pub const_env: HashMap<String, i128>,
    /// Literal values of parameters and enum members.
    pub parameter_values: HashMap<String, Expr>,
    pub parameters: Vec<Parameter>,
    pub type_aliases: HashMap<String, Type>,
    pub enum_constants: EnumMemberConstants,
    pub functions: HashMap<String, Function>,
    pub function_return_types: HashMap<String, FunctionReturnMetadata>,
    pub constant_functions: const_functions::ConstantFunctions,
    pub subroutine_params: HashMap<String, Vec<String>>,
    pub subroutine_shapes: HashMap<String, Vec<VariableDimensions>>,
    pub subroutines: Vec<Subroutine>,
    pub locals: Vec<LocalVariable>,
    pub dpi_imports: Vec<crate::ir::DpiImport>,
    /// Array parameters and constant variables, which are constant signals
    /// copied into each scope, and their initializers.
    pub signals: Vec<Signal>,
    /// Package variables: each scope's signal denotes the package's object.
    pub state_signals: Vec<Signal>,
    pub initial_processes: Vec<InitialProcess>,
    /// The names imports make visible, and the qualified names they denote.
    pub aliases: HashMap<String, String>,
    /// The names the imports of a generate block bind there to another
    /// declaration than outside it, by the source offset of the block.
    pub generate_imports: HashMap<usize, HashMap<String, String>>,
}

thread_local! {
    static IMPORTED: RefCell<Arc<ScopeSymbols>> = RefCell::new(Arc::default());
    static PACKAGE: RefCell<Option<String>> = const { RefCell::new(None) };
}

/// Restores the previously analyzed package when dropped.
pub(super) struct InPackage {
    previous: Option<String>,
}

impl Drop for InPackage {
    fn drop(&mut self) {
        let previous = self.previous.take();
        PACKAGE.with(|current| *current.borrow_mut() = previous);
    }
}

/// Analyze the package `name` until the guard is dropped: inside it, `p::x`
/// with `p` the package names its own item `x`.
pub(super) fn enter_package(name: &str) -> InPackage {
    let previous = PACKAGE.with(|current| current.borrow_mut().replace(name.to_string()));
    InPackage { previous }
}

/// The name a reference to item `name` of `package` has in the scope being
/// analyzed: `package::name`, or `name` inside the package itself.
pub(super) fn qualified_name(package: &str, name: &str) -> String {
    PACKAGE.with(|current| {
        if current.borrow().as_deref() == Some(package) {
            name.to_string()
        } else {
            format!("{package}::{name}")
        }
    })
}

/// Restores the previously imported symbols when dropped.
pub(super) struct Installed {
    previous: Arc<ScopeSymbols>,
}

impl Drop for Installed {
    fn drop(&mut self) {
        let previous = std::mem::take(&mut self.previous);
        IMPORTED.with(|current| *current.borrow_mut() = previous);
    }
}

/// Make `symbols` the symbols the scope being analyzed imports, until the
/// guard is dropped.
pub(super) fn install(symbols: Arc<ScopeSymbols>) -> Installed {
    let previous = IMPORTED.with(|current| std::mem::replace(&mut *current.borrow_mut(), symbols));
    Installed { previous }
}

/// The symbols the scope being analyzed imports.
pub(super) fn imported() -> Arc<ScopeSymbols> {
    IMPORTED.with(|current| current.borrow().clone())
}

/// The name a constant-environment marker key describes, with the marker
/// text before it.
pub(super) fn split_marker(key: &str) -> Option<(&str, &str)> {
    const FIXED: [&str; 11] = [
        "__parameter::local::",
        "__parameter::width::",
        "__parameter::signed_element::",
        "__parameter::signed::",
        "__parameter::dimensions::",
        "__parameter::rank::",
        "__enum::",
        "__variable::bits::",
        "__variable::size::",
        "__variable::signed::",
        "__variable::dimensions::",
    ];
    for prefix in FIXED {
        if let Some(name) = key.strip_prefix(prefix) {
            return Some((prefix, name));
        }
    }
    // `__parameter::dimension::{index}::{bound}::{name}`
    if let Some(rest) = key.strip_prefix("__parameter::dimension::") {
        let mut parts = rest.splitn(3, "::");
        let (index, bound, name) = (parts.next()?, parts.next()?, parts.next()?);
        let prefix_len = key.len() - name.len();
        debug_assert_eq!(&key[prefix_len - 2..prefix_len], "::");
        let _ = (index, bound);
        return Some((&key[..prefix_len], name));
    }
    key.strip_prefix("__parameter::")
        .map(|name| ("__parameter::", name))
}

impl ScopeSymbols {
    /// Add the symbols of `other` that this scope does not already have.
    pub fn extend(&mut self, other: &ScopeSymbols) {
        for (name, value) in &other.const_env {
            self.const_env.entry(name.clone()).or_insert(*value);
        }
        for (name, value) in &other.parameter_values {
            self.parameter_values
                .entry(name.clone())
                .or_insert_with(|| value.clone());
        }
        for parameter in &other.parameters {
            if !self
                .parameters
                .iter()
                .any(|known| known.name == parameter.name)
            {
                self.parameters.push(parameter.clone());
            }
        }
        extend_missing(&mut self.type_aliases, &other.type_aliases);
        extend_missing(
            &mut self.enum_constants.numbers,
            &other.enum_constants.numbers,
        );
        extend_missing(&mut self.enum_constants.exprs, &other.enum_constants.exprs);
        extend_missing(&mut self.enum_constants.types, &other.enum_constants.types);
        extend_missing(&mut self.functions, &other.functions);
        extend_missing(
            &mut self.function_return_types,
            &other.function_return_types,
        );
        self.constant_functions
            .extend_missing(&other.constant_functions);
        extend_missing(&mut self.subroutine_params, &other.subroutine_params);
        extend_missing(&mut self.subroutine_shapes, &other.subroutine_shapes);
        for subroutine in &other.subroutines {
            if !self
                .subroutines
                .iter()
                .any(|known| known.name == subroutine.name)
            {
                self.subroutines.push(subroutine.clone());
            }
        }
        for local in &other.locals {
            if !self.locals.iter().any(|known| known.name == local.name) {
                self.locals.push(local.clone());
            }
        }
        for signal in &other.state_signals {
            if !self
                .state_signals
                .iter()
                .any(|known| known.name == signal.name)
            {
                self.state_signals.push(signal.clone());
            }
        }
        let mut added = Vec::new();
        for signal in &other.signals {
            if !self.signals.iter().any(|known| known.name == signal.name) {
                self.signals.push(signal.clone());
                added.push(signal.name.as_str());
            }
        }
        for process in &other.initial_processes {
            if added.iter().any(|name| initializes(process, name))
                && !self.initial_processes.contains(process)
            {
                self.initial_processes.push(process.clone());
            }
        }
        for import in &other.dpi_imports {
            if !self
                .dpi_imports
                .iter()
                .any(|known| known.name() == import.name())
            {
                self.dpi_imports.push(import.clone());
            }
        }
    }

    /// Make the symbol `target` visible as `name` too, as an import makes a
    /// package item visible by its simple name (IEEE 1800-2023 26.3).
    /// Subroutines and DPI imports keep one entry: calls of `name` are bound
    /// to `target` after the scope is analyzed.
    pub fn alias(&mut self, name: &str, target: &str) {
        self.aliases.insert(name.to_string(), target.to_string());
        if let Some(parameter) = self
            .parameters
            .iter()
            .find(|parameter| parameter.name == target)
            .cloned()
        {
            self.parameters.push(Parameter {
                name: name.to_string(),
                ..parameter
            });
        }
        let markers: Vec<_> = self
            .const_env
            .iter()
            .filter_map(|(key, value)| {
                if key == target {
                    return Some((name.to_string(), *value));
                }
                let (prefix, marked) = split_marker(key)?;
                (marked == target).then(|| (format!("{prefix}{name}"), *value))
            })
            .collect();
        self.const_env.extend(markers);
        alias_entry(&mut self.parameter_values, name, target);
        alias_entry(&mut self.type_aliases, name, target);
        alias_entry(&mut self.enum_constants.numbers, name, target);
        alias_entry(&mut self.enum_constants.exprs, name, target);
        alias_entry(&mut self.enum_constants.types, name, target);
        alias_entry(&mut self.functions, name, target);
        alias_entry(&mut self.function_return_types, name, target);
        self.constant_functions.alias(name, target);
        alias_entry(&mut self.subroutine_params, name, target);
        alias_entry(&mut self.subroutine_shapes, name, target);
        // A package variable is denoted by a signal of that name too.
        if let Some(signal) = self
            .state_signals
            .iter()
            .find(|signal| signal.name == target)
            .cloned()
        {
            self.state_signals.push(Signal {
                name: name.to_string(),
                ..signal
            });
        }
        // A constant signal, such as an array parameter, is copied with its
        // initializer under the name.
        if let Some(signal) = self.signals.iter().find(|signal| signal.name == target) {
            let signal = Signal {
                name: name.to_string(),
                ..signal.clone()
            };
            let names = HashMap::from_iter([(target.to_string(), name.to_string())]);
            let rename = Renamer { names: &names };
            let initializers: Vec<_> = self
                .initial_processes
                .iter()
                .filter(|process| initializes(process, target))
                .map(|process| InitialProcess {
                    condition: process.condition.clone(),
                    body: process
                        .body
                        .iter()
                        .filter(|stmt| stmt_initializes(stmt, target))
                        .cloned()
                        .map(|stmt| rename.stmt(stmt))
                        .collect(),
                    initializer: process.initializer,
                })
                .collect();
            self.signals.push(signal);
            self.initial_processes.extend(initializers);
        }
    }

    /// These symbols without those named in `names`.
    pub fn without(mut self, names: &HashSet<String>) -> ScopeSymbols {
        let kept = |name: &String| !names.contains(name);
        self.const_env.retain(|key, _| {
            !names.contains(split_marker(key).map_or(key.as_str(), |(_, name)| name))
        });
        self.parameter_values.retain(|name, _| kept(name));
        self.parameters.retain(|parameter| kept(&parameter.name));
        self.type_aliases.retain(|name, _| kept(name));
        self.enum_constants.numbers.retain(|name, _| kept(name));
        self.enum_constants.exprs.retain(|name, _| kept(name));
        self.enum_constants.types.retain(|name, _| kept(name));
        self.functions.retain(|name, _| kept(name));
        self.function_return_types.retain(|name, _| kept(name));
        self.constant_functions
            .retain(|name| kept(&name.to_string()));
        self.subroutine_params.retain(|name, _| kept(name));
        self.subroutine_shapes.retain(|name, _| kept(name));
        self.subroutines.retain(|subroutine| kept(&subroutine.name));
        self.locals.retain(|local| kept(&local.name));
        self.dpi_imports
            .retain(|import| kept(&import.name().to_string()));
        self.initial_processes.retain(|process| {
            !self
                .signals
                .iter()
                .any(|signal| names.contains(&signal.name) && initializes(process, &signal.name))
        });
        self.signals.retain(|signal| kept(&signal.name));
        self.state_signals.retain(|signal| kept(&signal.name));
        self
    }

    /// The names of the locals of the scope's subroutines and constant
    /// functions, including their arguments and result variables.
    pub fn local_names(&self) -> HashSet<String> {
        let mut names: HashSet<String> =
            self.locals.iter().map(|local| local.name.clone()).collect();
        for subroutine in &self.subroutines {
            names.extend(subroutine.params.iter().map(|param| param.name.clone()));
            names.extend(subroutine.return_var.clone());
        }
        names.extend(self.constant_functions.local_names().cloned());
        names
    }

    /// These symbols with every name in `names` replaced, in keys and in the
    /// bodies and values that refer to them.
    pub fn renamed(&self, names: &HashMap<String, String>) -> ScopeSymbols {
        let rename = Renamer { names };
        let key = |name: &String| rename.name(name);
        let const_env = self
            .const_env
            .iter()
            .map(|(name, value)| {
                let name = match split_marker(name) {
                    Some((prefix, marked)) => format!("{prefix}{}", rename.name(marked)),
                    None => rename.name(name),
                };
                (name, *value)
            })
            .collect();
        ScopeSymbols {
            const_env,
            parameter_values: self
                .parameter_values
                .iter()
                .map(|(name, value)| (key(name), rename.expr(value.clone())))
                .collect(),
            parameters: {
                let mut parameters: Vec<Parameter> = Vec::new();
                for parameter in &self.parameters {
                    let parameter = rename.parameter(parameter.clone());
                    if !parameters.iter().any(|known| known.name == parameter.name) {
                        parameters.push(parameter);
                    }
                }
                parameters
            },
            type_aliases: self
                .type_aliases
                .iter()
                .map(|(name, r#type)| (key(name), rename.r#type(r#type.clone())))
                .collect(),
            enum_constants: EnumMemberConstants {
                numbers: self
                    .enum_constants
                    .numbers
                    .iter()
                    .map(|(name, value)| (key(name), *value))
                    .collect(),
                exprs: self
                    .enum_constants
                    .exprs
                    .iter()
                    .map(|(name, value)| (key(name), rename.expr(value.clone())))
                    .collect(),
                types: self
                    .enum_constants
                    .types
                    .iter()
                    .map(|(name, value)| (key(name), *value))
                    .collect(),
            },
            functions: self
                .functions
                .iter()
                .map(|(name, function)| (key(name), rename.function(function.clone())))
                .collect(),
            function_return_types: self
                .function_return_types
                .iter()
                .map(|(name, value)| (key(name), *value))
                .collect(),
            constant_functions: self
                .constant_functions
                .renamed(&mut |name| rename.name(name), &mut |stmt| rename.stmt(stmt)),
            subroutine_params: self
                .subroutine_params
                .iter()
                // The formal names bind named arguments; they are not
                // references.
                .map(|(name, params)| (key(name), params.clone()))
                .collect(),
            subroutine_shapes: self
                .subroutine_shapes
                .iter()
                .map(|(name, shapes)| {
                    (
                        key(name),
                        shapes
                            .iter()
                            .map(|shape| rename.dimensions(shape.clone()))
                            .collect(),
                    )
                })
                .collect(),
            subroutines: self
                .subroutines
                .iter()
                .map(|subroutine| rename.subroutine(subroutine.clone()))
                .collect(),
            locals: self
                .locals
                .iter()
                .map(|local| LocalVariable {
                    name: rename.name(&local.name),
                    ..local.clone()
                })
                .collect(),
            dpi_imports: self
                .dpi_imports
                .iter()
                .map(|import| import.renamed(rename.name(import.name())))
                .collect(),
            signals: self
                .signals
                .iter()
                .map(|signal| Signal {
                    name: rename.name(&signal.name),
                    r#type: rename.r#type(signal.r#type.clone()),
                    ..signal.clone()
                })
                .collect(),
            state_signals: self
                .state_signals
                .iter()
                .map(|signal| Signal {
                    name: rename.name(&signal.name),
                    r#type: rename.r#type(signal.r#type.clone()),
                    ..signal.clone()
                })
                .collect(),
            initial_processes: self
                .initial_processes
                .iter()
                .map(|process| InitialProcess {
                    condition: process
                        .condition
                        .clone()
                        .map(|condition| rename.const_expr(condition)),
                    body: process
                        .body
                        .iter()
                        .cloned()
                        .map(|stmt| rename.stmt(stmt))
                        .collect(),
                    initializer: process.initializer,
                })
                .collect(),
            aliases: HashMap::default(),
            generate_imports: HashMap::default(),
        }
    }
}

/// Whether `process` assigns the signal `name`.
pub(super) fn initializes(process: &InitialProcess, name: &str) -> bool {
    process.body.iter().any(|stmt| stmt_initializes(stmt, name))
}

fn stmt_initializes(stmt: &Stmt, name: &str) -> bool {
    match stmt {
        Stmt::Assign { lhs, .. } => lhs.name() == name,
        Stmt::AssignConcat { parts, .. } => parts.iter().any(|part| part.name() == name),
        _ => false,
    }
}

fn extend_missing<V: Clone>(map: &mut HashMap<String, V>, other: &HashMap<String, V>) {
    for (name, value) in other {
        map.entry(name.clone()).or_insert_with(|| value.clone());
    }
}

fn alias_entry<V: Clone>(map: &mut HashMap<String, V>, name: &str, target: &str) {
    if let Some(value) = map.get(target).cloned() {
        map.insert(name.to_string(), value);
    }
}

/// Replaces names in the expressions, statements and types of exported
/// symbols. Every name a body uses for a scope item is a key of `names`
/// when it must change; locals already have scope-unique names.
pub(super) struct Renamer<'a> {
    pub names: &'a HashMap<String, String>,
}

impl Renamer<'_> {
    pub fn name(&self, name: &str) -> String {
        self.names
            .get(name)
            .cloned()
            .unwrap_or_else(|| name.to_string())
    }

    pub fn const_expr(&self, expr: ConstExpr) -> ConstExpr {
        match expr {
            ConstExpr::Ident(name) => ConstExpr::Ident(self.name(&name)),
            ConstExpr::Literal(value) => ConstExpr::Literal(value),
            ConstExpr::Select { expr, bit } => ConstExpr::Select {
                expr: Box::new(self.const_expr(*expr)),
                bit: Box::new(self.const_expr(*bit)),
            },
            ConstExpr::Function { name, args, site } => ConstExpr::Function {
                name: self.name(&name),
                args: args.into_iter().map(|arg| self.const_expr(arg)).collect(),
                site,
            },
            ConstExpr::Unary { op, expr } => ConstExpr::Unary {
                op,
                expr: Box::new(self.const_expr(*expr)),
            },
            ConstExpr::Binary { left, op, right } => ConstExpr::Binary {
                left: Box::new(self.const_expr(*left)),
                op,
                right: Box::new(self.const_expr(*right)),
            },
            ConstExpr::Mux {
                condition,
                then_expr,
                else_expr,
            } => ConstExpr::Mux {
                condition: Box::new(self.const_expr(*condition)),
                then_expr: Box::new(self.const_expr(*then_expr)),
                else_expr: Box::new(self.const_expr(*else_expr)),
            },
        }
    }

    pub fn expr(&self, expr: Expr) -> Expr {
        let boxed = |expr: Box<Expr>| Box::new(self.expr(*expr));
        match expr {
            Expr::Ident(name) => Expr::Ident(self.name(&name)),
            Expr::Literal(value) => Expr::Literal(value),
            Expr::Select {
                expr,
                msb,
                lsb,
                signed,
            } => Expr::Select {
                expr: boxed(expr),
                msb: self.const_expr(msb),
                lsb: self.const_expr(lsb),
                signed,
            },
            Expr::Concat(parts) => {
                Expr::Concat(parts.into_iter().map(|part| self.expr(part)).collect())
            }
            Expr::RepeatConcat { count, parts } => Expr::RepeatConcat {
                count: self.const_expr(count),
                parts: parts.into_iter().map(|part| self.expr(part)).collect(),
            },
            Expr::Resize {
                expr,
                width,
                signed,
            } => Expr::Resize {
                expr: boxed(expr),
                width,
                signed,
            },
            Expr::Unary { op, expr } => Expr::Unary {
                op,
                expr: boxed(expr),
            },
            Expr::Binary { left, op, right } => Expr::Binary {
                left: boxed(left),
                op,
                right: boxed(right),
            },
            Expr::Mux {
                condition,
                then_expr,
                else_expr,
            } => Expr::Mux {
                condition: boxed(condition),
                then_expr: boxed(then_expr),
                else_expr: boxed(else_expr),
            },
            Expr::Call { name, args } => Expr::Call {
                name: self.name(&name),
                args: args.into_iter().map(|arg| self.expr(arg)).collect(),
            },
            Expr::Inside { expr, items } => Expr::Inside {
                expr: boxed(expr),
                items: items
                    .into_iter()
                    .map(|item| match item {
                        InsideItem::Value(value) => InsideItem::Value(self.expr(value)),
                        InsideItem::Range { low, high } => InsideItem::Range {
                            low: self.expr(low),
                            high: self.expr(high),
                        },
                    })
                    .collect(),
            },
        }
    }

    pub fn lvalue(&self, lvalue: LValue) -> LValue {
        match lvalue {
            LValue::Ident(name) => LValue::Ident(self.name(&name)),
            LValue::Select {
                name,
                msb,
                lsb,
                signed,
                array_slice_width,
                array_slice_reversed,
                is_2state,
            } => LValue::Select {
                name: self.name(&name),
                msb: self.const_expr(msb),
                lsb: self.const_expr(lsb),
                signed,
                array_slice_width: array_slice_width.map(|width| self.const_expr(width)),
                array_slice_reversed,
                is_2state,
            },
        }
    }

    pub fn stmt(&self, stmt: Stmt) -> Stmt {
        let stmt = match stmt {
            Stmt::Call { name, args } => Stmt::Call {
                name: self.name(&name),
                args,
            },
            Stmt::Local { name, init } => Stmt::Local {
                name: self.name(&name),
                init,
            },
            stmt => stmt,
        };
        // Nested calls and locals are renamed by `map` only through their
        // expressions; rename them in the nested bodies as well.
        let stmt = stmt.map(&mut |expr| self.expr(expr), &mut |lvalue| {
            self.lvalue(lvalue)
        });
        self.nested(stmt)
    }

    fn nested(&self, stmt: Stmt) -> Stmt {
        let body = |stmts: Vec<Stmt>| {
            stmts
                .into_iter()
                .map(|stmt| self.stmt_names(stmt))
                .collect()
        };
        match stmt {
            Stmt::If {
                condition,
                then_body,
                else_body,
            } => Stmt::If {
                condition,
                then_body: body(then_body),
                else_body: body(else_body),
            },
            Stmt::Case {
                kind,
                selector,
                items,
                default,
            } => Stmt::Case {
                kind,
                selector,
                items: items
                    .into_iter()
                    .map(|item| crate::procedural::CaseItemBase {
                        labels: item.labels,
                        body: body(item.body),
                    })
                    .collect(),
                default: default.map(body),
            },
            Stmt::Loop {
                kind,
                init,
                condition,
                step,
                body: loop_body,
            } => Stmt::Loop {
                kind,
                init: body(init),
                condition,
                step: body(step),
                body: body(loop_body),
            },
            stmt => stmt,
        }
    }

    /// Rename the call and local names of `stmt` and its nested bodies; its
    /// expressions were renamed already.
    fn stmt_names(&self, stmt: Stmt) -> Stmt {
        let stmt = match stmt {
            Stmt::Call { name, args } => Stmt::Call {
                name: self.name(&name),
                args,
            },
            Stmt::Local { name, init } => Stmt::Local {
                name: self.name(&name),
                init,
            },
            stmt => stmt,
        };
        self.nested(stmt)
    }

    pub fn parameter(&self, mut parameter: Parameter) -> Parameter {
        parameter.name = self.name(&parameter.name);
        parameter.value = parameter.value.map(|value| self.const_expr(value));
        for range in &mut parameter.packed_ranges {
            *range = self.packed_range(range.clone());
        }
        parameter
    }

    fn packed_range(&self, range: PackedRange) -> PackedRange {
        PackedRange::new(self.const_expr(range.left), self.const_expr(range.right))
    }

    pub fn r#type(&self, mut r#type: Type) -> Type {
        r#type.packed_ranges = r#type
            .packed_ranges
            .into_iter()
            .map(|range| self.packed_range(range))
            .collect();
        for range in &mut r#type.unpacked_ranges {
            range.left = self.const_expr(range.left.clone());
            range.right = self.const_expr(range.right.clone());
            range.size = range.size.take().map(|size| self.const_expr(size));
        }
        r#type.members = r#type
            .members
            .into_iter()
            .map(|member| member.with_type(|r#type| self.r#type(r#type)))
            .collect();
        r#type
    }

    pub fn dimensions(&self, mut dimensions: VariableDimensions) -> VariableDimensions {
        for dimension in &mut dimensions.packed {
            dimension.left = self.const_expr(dimension.left.clone());
            dimension.right = self.const_expr(dimension.right.clone());
            dimension.width = self.const_expr(dimension.width.clone());
        }
        for dimension in &mut dimensions.unpacked {
            dimension.left = self.const_expr(dimension.left.clone());
            dimension.right = self.const_expr(dimension.right.clone());
            dimension.width = self.const_expr(dimension.width.clone());
        }
        dimensions.members = dimensions
            .members
            .into_iter()
            .map(|member| member.with_type(|r#type| self.r#type(r#type)))
            .collect();
        dimensions
    }

    /// An expression function. Its parameters shadow the names of the scope
    /// in its body.
    pub fn function(&self, mut function: Function) -> Function {
        let mut names = self.names.clone();
        for param in &function.params {
            names.remove(&param.name);
        }
        let inner = Renamer { names: &names };
        function.name = self.name(&function.name);
        function.body = function.body.map(|body| inner.expr(body));
        function.outputs = function
            .outputs
            .into_iter()
            .map(|(name, value)| (name, inner.expr(value)))
            .collect();
        for param in &mut function.params {
            for dimension in &mut param.packed_dimensions {
                dimension.left = self.const_expr(dimension.left.clone());
                dimension.right = self.const_expr(dimension.right.clone());
                dimension.width = self.const_expr(dimension.width.clone());
            }
        }
        function
    }

    pub fn subroutine(&self, subroutine: Subroutine) -> Subroutine {
        Subroutine {
            name: self.name(&subroutine.name),
            params: subroutine
                .params
                .into_iter()
                .map(|param| crate::procedural::SubroutineParamBase {
                    name: self.name(&param.name),
                    default: param.default.map(|default| self.expr(default)),
                    ..param
                })
                .collect(),
            return_var: subroutine.return_var.map(|name| self.name(&name)),
            body: subroutine
                .body
                .into_iter()
                .map(|stmt| self.stmt(stmt))
                .collect(),
            ..subroutine
        }
    }
}
