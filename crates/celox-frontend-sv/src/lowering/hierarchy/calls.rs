//! Subroutines of other instances that hierarchical names call (IEEE
//! 1800-2023 23.8), imported into the calling module.
//!
//! The calling module is analyzed with the signature of each subroutine it
//! calls through a hierarchical name, under that name. Its analysis then
//! receives the subroutine as the callee's analysis bound it, renamed: the
//! callee's ports, signals and static storage become signals of the caller
//! that denote the callee instance's objects, as other hierarchical names
//! do, the hierarchical names of the callee denote from the caller what
//! they denote from the callee, and its automatic storage becomes the
//! caller's. Each call site thus runs the callee's body in its own process,
//! on the objects of the callee instance.

use super::*;
use sv::ir::{Callee, ConstExpr, Expr, LValue};

/// The callee of a [`HierarchicalCall`]: the specialization and its
/// analysis.
pub(in crate::lowering) struct CallTarget<'a> {
    pub call: &'a HierarchicalCall,
    pub id: ModuleId,
    pub module: &'a sv::ir::Module,
    /// The references of the callee's own hierarchical names.
    pub references: &'a [HierarchicalReference],
}

fn unsupported(what: String) -> sv::AnalyzerError {
    sv::AnalyzerError::Unsupported(what)
}

impl HierarchicalCall {
    /// The specialization that declares the subroutine: one for every
    /// instance of the calling module, which runs its one body.
    pub(in crate::lowering) fn callee(&self) -> Result<(ModuleId, &str), sv::AnalyzerError> {
        match self.targets.as_slice() {
            [(module, name)] => Ok((*module, name)),
            _ => Err(unsupported(format!(
                "hierarchical call `{}` of the subroutines of different specializations",
                self.written
            ))),
        }
    }
}

impl CallTarget<'_> {
    fn subroutine(&self, name: &str) -> Option<&sv::ir::Subroutine> {
        self.module
            .subroutines()
            .iter()
            .find(|subroutine| subroutine.name == name)
    }

    fn target(&self) -> Result<&sv::ir::Subroutine, sv::AnalyzerError> {
        let (_, name) = self.call.callee()?;
        self.subroutine(name).ok_or_else(|| {
            unsupported(format!(
                "hierarchical call `{}` of a subroutine without a body",
                self.call.written
            ))
        })
    }

    /// The signature the calling module's analysis binds the calls by.
    pub(in crate::lowering) fn signature(
        &self,
        hierarchy: &Hierarchy<'_>,
    ) -> Result<sv::ast::HierarchicalSubroutine, sv::AnalyzerError> {
        let elaborated = &hierarchy.elaborated[&self.id];
        let subroutine = self.target()?;
        let resolve = |r#type: &sv::ir::Type| {
            resolved_type(elaborated, r#type).ok_or_else(|| {
                unsupported(format!(
                    "hierarchical call `{}` of a subroutine with a formal or result of this type",
                    self.call.written
                ))
            })
        };
        Ok(sv::ast::HierarchicalSubroutine {
            paths: vec![self.call.written.clone()],
            name: self.call.written.clone(),
            formals: subroutine
                .params
                .iter()
                .map(|param| {
                    Ok(sv::ast::HierarchicalFormal {
                        name: param.source_name.clone(),
                        direction: param.direction,
                        r#type: resolve(&param.r#type)?,
                        has_default: param.default.is_some(),
                    })
                })
                .collect::<Result<_, sv::AnalyzerError>>()?,
            result: subroutine.return_type.as_ref().map(resolve).transpose()?,
            is_task: subroutine.is_task,
        })
    }
}

/// What a module receives for the subroutines it calls through
/// hierarchical names (see [`sv::ir::Module::import_subroutines`]).
#[derive(Default)]
pub(in crate::lowering) struct Imported {
    pub subroutines: Vec<sv::ir::Subroutine>,
    pub signals: Vec<sv::ir::Signal>,
    pub locals: Vec<sv::ir::LocalVariable>,
    pub dpi_imports: Vec<sv::ir::DpiImport>,
    /// The references of the signals among `signals` that denote objects of
    /// other instances.
    pub references: Vec<HierarchicalReference>,
}

/// What a name of the callee becomes in the caller.
enum Renamed {
    Name(String),
    /// A parameter of the callee, by its value.
    Value(String),
}

/// The subroutines of the callee that `name` reaches, by its calls, among
/// those the callee declares itself.
fn closure<'a>(callee: &CallTarget<'a>, name: &str) -> Vec<&'a sv::ir::Subroutine> {
    let mut found: Vec<&sv::ir::Subroutine> = Vec::new();
    let mut pending = vec![name.to_string()];
    while let Some(name) = pending.pop() {
        if found.iter().any(|subroutine| subroutine.name == name) {
            continue;
        }
        let Some(subroutine) = callee
            .module
            .subroutines()
            .iter()
            .find(|subroutine| subroutine.name == name)
        else {
            continue;
        };
        found.push(subroutine);
        let mut names = Names::default();
        names.subroutine(subroutine);
        pending.extend(names.callees);
    }
    found
}

/// The names that subroutine bodies read, write and call.
#[derive(Default)]
struct Names {
    identifiers: HashSet<String>,
    callees: Vec<String>,
}

impl Names {
    fn subroutine(&mut self, subroutine: &sv::ir::Subroutine) {
        for param in &subroutine.params {
            self.identifiers.insert(param.name.clone());
            self.identifiers.extend(param.leaves.iter().cloned());
            if let Some(default) = &param.default {
                self.expr(default);
            }
        }
        self.identifiers
            .extend(subroutine.return_var.iter().cloned());
        self.identifiers
            .extend(subroutine.automatic_locals.iter().cloned());
        self.identifiers
            .extend(subroutine.persistent_locals.iter().cloned());
        // The traversal in place visits every expression, lvalue and call;
        // the names are collected from a copy.
        for mut stmt in subroutine
            .body
            .iter()
            .chain(&subroutine.sensitivity_body)
            .cloned()
        {
            let mut exprs = Vec::new();
            let mut lvalues = Vec::new();
            let mut calls = Vec::new();
            stmt.visit_mut(
                &mut |expr| exprs.push(expr.clone()),
                &mut |lvalue| lvalues.push(lvalue.clone()),
                &mut |name| calls.push(name.clone()),
            );
            for expr in &exprs {
                self.expr(expr);
            }
            for lvalue in &lvalues {
                self.lvalue(lvalue);
            }
            self.callees.extend(calls);
        }
    }

    fn expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Ident(name) => {
                self.identifiers.insert(name.clone());
            }
            Expr::Literal(_) => {}
            Expr::Select { expr, msb, lsb, .. } => {
                self.expr(expr);
                self.constant(msb);
                self.constant(lsb);
            }
            Expr::Concat(parts) => parts.iter().for_each(|part| self.expr(part)),
            Expr::RepeatConcat { count, parts } => {
                self.constant(count);
                parts.iter().for_each(|part| self.expr(part));
            }
            Expr::Resize { expr, .. } | Expr::Unary { expr, .. } => self.expr(expr),
            Expr::Binary { left, right, .. } => {
                self.expr(left);
                self.expr(right);
            }
            Expr::Mux {
                condition,
                then_expr,
                else_expr,
            } => {
                self.expr(condition);
                self.expr(then_expr);
                self.expr(else_expr);
            }
            Expr::Call { callee, args, .. } => {
                if let Callee::Subroutine(name) = callee {
                    self.callees.push(name.clone());
                }
                args.iter().flatten().for_each(|arg| self.expr(arg));
            }
            Expr::Inside { expr, items } => {
                self.expr(expr);
                for item in items {
                    for operand in item.exprs() {
                        self.expr(operand);
                    }
                }
            }
        }
    }

    fn constant(&mut self, expr: &ConstExpr) {
        match expr {
            ConstExpr::Ident(name) => {
                self.identifiers.insert(name.clone());
            }
            ConstExpr::Literal(_) => {}
            ConstExpr::Select { expr, bit } => {
                self.constant(expr);
                self.constant(bit);
            }
            ConstExpr::SelectRange { expr, msb, lsb, .. } => {
                self.constant(expr);
                self.constant(msb);
                self.constant(lsb);
            }
            ConstExpr::Function { callee, args, .. } => {
                if let Callee::Subroutine(name) = callee {
                    self.callees.push(name.clone());
                }
                args.iter().for_each(|arg| self.constant(arg));
            }
            ConstExpr::Resize { expr, .. } | ConstExpr::Unary { expr, .. } => self.constant(expr),
            ConstExpr::Binary { left, right, .. } => {
                self.constant(left);
                self.constant(right);
            }
            ConstExpr::Mux {
                condition,
                then_expr,
                else_expr,
            } => {
                self.constant(condition);
                self.constant(then_expr);
                self.constant(else_expr);
            }
        }
    }

    fn lvalue(&mut self, lvalue: &LValue) {
        match lvalue {
            LValue::Ident(name) => {
                self.identifiers.insert(name.clone());
            }
            LValue::Select {
                name,
                msb,
                lsb,
                array_slice_width,
                ..
            } => {
                self.identifiers.insert(name.clone());
                self.constant(msb);
                self.constant(lsb);
                if let Some(width) = array_slice_width {
                    self.constant(width);
                }
            }
        }
    }
}

/// The renaming of a callee's names in its imported subroutines.
struct Renames {
    names: HashMap<String, Renamed>,
}

impl Renames {
    fn name(&self, name: &mut String) {
        if let Some(Renamed::Name(renamed)) = self.names.get(name.as_str()) {
            *name = renamed.clone();
        }
    }

    fn callee(&self, callee: &mut Callee) {
        if let Callee::Subroutine(name) = callee {
            self.name(name);
        }
    }

    fn expr(&self, expr: &mut Expr) {
        match expr {
            Expr::Ident(name) => match self.names.get(name.as_str()) {
                Some(Renamed::Name(renamed)) => *name = renamed.clone(),
                Some(Renamed::Value(value)) => *expr = Expr::Literal(value.clone()),
                None => {}
            },
            Expr::Literal(_) => {}
            Expr::Select { expr, msb, lsb, .. } => {
                self.expr(expr);
                self.constant(msb);
                self.constant(lsb);
            }
            Expr::Concat(parts) => parts.iter_mut().for_each(|part| self.expr(part)),
            Expr::RepeatConcat { count, parts } => {
                self.constant(count);
                parts.iter_mut().for_each(|part| self.expr(part));
            }
            Expr::Resize { expr, .. } | Expr::Unary { expr, .. } => self.expr(expr),
            Expr::Binary { left, right, .. } => {
                self.expr(left);
                self.expr(right);
            }
            Expr::Mux {
                condition,
                then_expr,
                else_expr,
            } => {
                self.expr(condition);
                self.expr(then_expr);
                self.expr(else_expr);
            }
            Expr::Call { callee, args, .. } => {
                self.callee(callee);
                args.iter_mut().flatten().for_each(|arg| self.expr(arg));
            }
            Expr::Inside { expr, items } => {
                self.expr(expr);
                for item in items {
                    *item = item.map(&mut |operand| {
                        let mut operand = operand.clone();
                        self.expr(&mut operand);
                        operand
                    });
                }
            }
        }
    }

    fn constant(&self, expr: &mut ConstExpr) {
        match expr {
            ConstExpr::Ident(name) => match self.names.get(name.as_str()) {
                Some(Renamed::Name(renamed)) => *name = renamed.clone(),
                Some(Renamed::Value(value)) => *expr = ConstExpr::Literal(value.clone()),
                None => {}
            },
            ConstExpr::Literal(_) => {}
            ConstExpr::Select { expr, bit } => {
                self.constant(expr);
                self.constant(bit);
            }
            ConstExpr::SelectRange { expr, msb, lsb, .. } => {
                self.constant(expr);
                self.constant(msb);
                self.constant(lsb);
            }
            ConstExpr::Function { callee, args, .. } => {
                self.callee(callee);
                args.iter_mut().for_each(|arg| self.constant(arg));
            }
            ConstExpr::Resize { expr, .. } | ConstExpr::Unary { expr, .. } => self.constant(expr),
            ConstExpr::Binary { left, right, .. } => {
                self.constant(left);
                self.constant(right);
            }
            ConstExpr::Mux {
                condition,
                then_expr,
                else_expr,
            } => {
                self.constant(condition);
                self.constant(then_expr);
                self.constant(else_expr);
            }
        }
    }

    fn lvalue(&self, lvalue: &mut LValue) {
        match lvalue {
            LValue::Ident(name) => self.name(name),
            LValue::Select {
                name,
                msb,
                lsb,
                array_slice_width,
                ..
            } => {
                self.name(name);
                self.constant(msb);
                self.constant(lsb);
                if let Some(width) = array_slice_width {
                    self.constant(width);
                }
            }
        }
    }

    fn subroutine(&self, mut subroutine: sv::ir::Subroutine) -> sv::ir::Subroutine {
        self.name(&mut subroutine.name);
        for param in &mut subroutine.params {
            self.name(&mut param.name);
            param.leaves.iter_mut().for_each(|leaf| self.name(leaf));
            if let Some(default) = &mut param.default {
                self.expr(default);
            }
        }
        if let Some(result) = &mut subroutine.return_var {
            self.name(result);
        }
        subroutine
            .automatic_locals
            .iter_mut()
            .for_each(|local| self.name(local));
        subroutine
            .persistent_locals
            .iter_mut()
            .for_each(|local| self.name(local));
        for stmt in subroutine
            .body
            .iter_mut()
            .chain(subroutine.sensitivity_body.iter_mut())
        {
            stmt.visit_mut(
                &mut |expr| self.expr(expr),
                &mut |lvalue| self.lvalue(lvalue),
                &mut |name| self.name(name),
            );
        }
        subroutine
    }
}

/// The literal of the value `value` of a parameter of type `r#type`, a
/// width and signedness when the declaration gives them.
fn parameter_literal(value: i128, r#type: Option<&(usize, bool)>) -> String {
    match r#type {
        Some(&(width, signed)) if (1..=128).contains(&width) => {
            let mask = if width == 128 {
                u128::MAX
            } else {
                (1u128 << width) - 1
            };
            format!(
                "{width}'{}h{:x}",
                if signed { "s" } else { "" },
                (value as u128) & mask
            )
        }
        _ => value.to_string(),
    }
}

/// The signals of a caller that denote the objects a subroutine it imports
/// names.
struct Aliases<'a, 'i> {
    call: &'a HierarchicalCall,
    elaborated: &'a ElaboratedModule,
    caller: &'a sv::ir::Module,
    references: &'a [HierarchicalReference],
    imported: &'i mut Imported,
}

impl Aliases<'_, '_> {
    /// The value of a bound of a declaration of the callee.
    fn bound(&self, expr: &ConstExpr) -> Option<i128> {
        sv::typecheck::eval_const_expr_with_types(
            expr,
            &self.elaborated.constants,
            &self.elaborated.parameter_types,
        )
    }

    fn declared(&self, name: &str) -> bool {
        self.caller
            .signals()
            .iter()
            .chain(&self.imported.signals)
            .any(|signal| signal.name() == name)
    }

    /// The signal `alias` of the caller that denotes the object `name` of
    /// type `ir_type` that `reference` (start, path, targets) reaches, made
    /// unless the caller has it already.
    fn alias(
        &mut self,
        name: &str,
        alias: String,
        ir_type: &sv::ir::Type,
        is_net: bool,
        (start, path, targets): (
            HierarchicalStart,
            Vec<(String, usize)>,
            Vec<(ModuleId, String)>,
        ),
    ) -> Result<String, sv::AnalyzerError> {
        if self.declared(&alias)
            || self
                .references
                .iter()
                .any(|reference| reference.signal.name == alias)
        {
            return Ok(alias);
        }
        let unsupported_type = || {
            unsupported(format!(
                "hierarchical call `{}` of a subroutine that names `{name}` of this type",
                self.call.written
            ))
        };
        let r#type = resolved_type(self.elaborated, ir_type).ok_or_else(unsupported_type)?;
        let ir_type = ir_type
            .with_literal_bounds(&|expr| self.bound(expr))
            .ok_or_else(unsupported_type)?;
        self.imported
            .signals
            .push(sv::ir::Signal::hierarchical_alias(
                alias.clone(),
                ir_type,
                is_net,
            ));
        self.imported.references.push(HierarchicalReference {
            signal: sv::ast::HierarchicalSignal {
                paths: vec![alias.clone()],
                name: alias.clone(),
                r#type,
                is_net,
                local: false,
            },
            start,
            path,
            targets,
        });
        Ok(alias)
    }
}

/// The subroutines, signals and locals that the call `callee` imports
/// into the calling module `caller`, whose own hierarchical references are
/// `references` (see the module documentation).
pub(in crate::lowering) fn import(
    hierarchy: &Hierarchy<'_>,
    caller: &sv::ir::Module,
    callee: &CallTarget<'_>,
    references: &[HierarchicalReference],
    imported: &mut Imported,
) -> Result<(), sv::AnalyzerError> {
    let call = callee.call;
    let (_, target) = call.callee()?;
    let elaborated = &hierarchy.elaborated[&callee.id];
    let subroutines = closure(callee, target);
    if !subroutines
        .iter()
        .any(|subroutine| subroutine.name == target)
    {
        return Err(unsupported(format!(
            "hierarchical call `{}` of a subroutine without a body",
            call.written
        )));
    }
    let prefix = call
        .written
        .rsplit_once('.')
        .map_or(call.written.as_str(), |(prefix, _)| prefix);
    let scoped = |name: &str| format!("{prefix}.{name}");
    let mut names = Names::default();
    for subroutine in &subroutines {
        names.subroutine(subroutine);
    }
    let automatic: HashSet<&str> = subroutines
        .iter()
        .filter(|subroutine| subroutine.automatic)
        .flat_map(|subroutine| {
            let persistent: HashSet<&str> = subroutine
                .persistent_locals
                .iter()
                .map(String::as_str)
                .collect();
            subroutine
                .params
                .iter()
                .map(|param| param.name.as_str())
                .chain(subroutine.return_var.as_deref())
                .chain(subroutine.automatic_locals.iter().map(String::as_str))
                .filter(move |name| !persistent.contains(name))
                .collect::<Vec<_>>()
        })
        .collect();
    let mut renames = HashMap::default();
    for subroutine in &subroutines {
        let renamed = if subroutine.name == target {
            call.written.clone()
        } else if subroutine.name.contains("::") {
            // A package subroutine, which the caller calls by its name.
            if !caller
                .subroutines()
                .iter()
                .chain(&imported.subroutines)
                .any(|own| own.name == subroutine.name)
            {
                imported.subroutines.push((*subroutine).clone());
            }
            continue;
        } else {
            scoped(&subroutine.name)
        };
        renames.insert(subroutine.name.clone(), Renamed::Name(renamed));
    }
    for import in callee.module.dpi_imports() {
        if names.callees.iter().any(|name| name == import.name())
            && !caller
                .dpi_imports()
                .iter()
                .chain(&imported.dpi_imports)
                .any(|own| own.name() == import.name())
        {
            imported.dpi_imports.push(import.clone());
        }
    }
    let mut aliases = Aliases {
        call,
        elaborated,
        caller,
        references,
        imported,
    };
    let own = |name: &str| {
        (
            call.start,
            call.path.clone(),
            vec![(callee.id, name.to_string())],
        )
    };
    let mut identifiers: Vec<_> = names.identifiers.iter().collect();
    identifiers.sort();
    for name in identifiers {
        if renames.contains_key(name) {
            continue;
        }
        let renamed = if let Some(port) = callee
            .module
            .ports()
            .iter()
            .find(|port| port.name() == name)
        {
            aliases.alias(name, scoped(name), port.r#type(), port.is_net(), own(name))?
        } else if let Some(signal) = callee
            .module
            .signals()
            .iter()
            .find(|signal| signal.name() == name)
        {
            if signal.package_variable().is_some() {
                // A package variable, which the caller denotes by its name.
                if !aliases.declared(name) {
                    aliases.imported.signals.push(signal.clone());
                }
                continue;
            }
            if signal.hierarchical().is_none() {
                aliases.alias(
                    name,
                    scoped(name),
                    signal.r#type(),
                    signal.is_net(),
                    own(name),
                )?
            } else {
                // A hierarchical name of the callee denotes from the caller
                // what it denotes from the callee instance.
                let reference = callee
                    .references
                    .iter()
                    .find(|reference| reference.signal.name == *name)
                    .ok_or_else(|| {
                        unsupported(format!(
                            "hierarchical call `{}` of a subroutine that names `{name}`",
                            call.written
                        ))
                    })?;
                if reference.signal.local {
                    let target = reference.signal.name.clone();
                    aliases.alias(
                        name,
                        scoped(&target),
                        signal.r#type(),
                        signal.is_net(),
                        own(&target),
                    )?
                } else {
                    let (start, path) =
                        compose((call.start, &call.path), (reference.start, &reference.path))
                            .ok_or_else(|| {
                                unsupported(format!(
                                    "hierarchical call `{}` of a subroutine whose name `{name}` \
                             leaves the design",
                                    call.written
                                ))
                            })?;
                    aliases.alias(
                        name,
                        scoped(name),
                        signal.r#type(),
                        signal.is_net(),
                        (start, path, reference.targets.clone()),
                    )?
                }
            }
        } else if let Some(local) = callee
            .module
            .locals()
            .iter()
            .find(|local| local.name == *name)
        {
            if automatic.contains(name.as_str()) {
                // An activation of the caller's own.
                let renamed = scoped(name);
                let r#type = local
                    .r#type
                    .with_literal_bounds(&|expr| aliases.bound(expr))
                    .ok_or_else(|| {
                        unsupported(format!(
                            "hierarchical call `{}` of a subroutine with a local `{name}` \
                             of this type",
                            call.written
                        ))
                    })?;
                aliases.imported.locals.push(sv::ir::LocalVariable {
                    name: renamed.clone(),
                    source_name: local.source_name.clone(),
                    r#type,
                    owned: local.owned,
                });
                renamed
            } else {
                // Static storage, which the callee instance's own calls share.
                aliases.alias(name, scoped(name), &local.r#type, false, own(name))?
            }
        } else if let Some(&value) = elaborated.constants.get(name) {
            renames.insert(
                name.clone(),
                Renamed::Value(parameter_literal(
                    value,
                    elaborated.parameter_types.get(name),
                )),
            );
            continue;
        } else {
            continue;
        };
        renames.insert(name.clone(), Renamed::Name(renamed));
    }
    let imported = aliases.imported;
    let renames = Renames { names: renames };
    for subroutine in subroutines {
        if subroutine.name.contains("::") {
            continue;
        }
        imported
            .subroutines
            .push(renames.subroutine(subroutine.clone()));
    }
    Ok(())
}
