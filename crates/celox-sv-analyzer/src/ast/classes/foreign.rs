//! Package class templates instantiated by scopes that use the package.
use super::*;
use crate::ast::scope::ScopeSymbols;
use specializations::BoundArguments;
use std::cell::{Cell, RefCell};

#[derive(Debug)]
struct Owner {
    tree: Rc<SyntaxTree>,
    imports: ScopeSymbols,
    calls: Rc<Cell<usize>>,
    timescales: Arc<timescales::Timescales>,
}

#[derive(Clone, Debug)]
struct Request {
    generic: String,
    arguments: BoundArguments,
    name: Option<String>,
}

/// Bound declarations inherited from a class, excluding its enclosing package.
#[derive(Clone, Debug, Default)]
pub(in crate::ast) struct DeclarationScope {
    pub env: HashMap<String, Option<i128>>,
    pub types: HashMap<String, Type>,
    pub literals: HashMap<String, Expr>,
}

impl DeclarationScope {
    pub fn apply(&self, env: &mut HashMap<String, i128>, types: &mut HashMap<String, Type>) {
        for (name, value) in &self.env {
            match value {
                Some(value) => {
                    env.insert(name.clone(), *value);
                }
                None => {
                    env.remove(name);
                }
            }
        }
        types.extend(self.types.clone());
    }
}

#[derive(Debug, Default)]
pub(in crate::ast) struct Registry {
    owners: RefCell<HashMap<String, Rc<Owner>>>,
    templates: RefCell<HashMap<String, String>>,
    requests: RefCell<HashMap<String, Vec<Request>>>,
    prepared: RefCell<HashMap<String, ScopeSymbols>>,
    complete: RefCell<HashSet<String>>,
    argument_symbols: RefCell<ScopeSymbols>,
    active: RefCell<HashSet<String>>,
    states: RefCell<HashMap<String, crate::ir::Module>>,
    revision: Cell<u64>,
    generics: RefCell<HashMap<String, String>>,
    declarations: RefCell<HashMap<String, Rc<DeclarationScope>>>,
}

impl Registry {
    pub fn revision(&self) -> u64 {
        self.revision.get()
    }
    pub fn has_templates(&self) -> bool {
        !self.templates.borrow().is_empty()
    }
    pub fn generic(&self, name: &str) -> Option<String> {
        self.generics.borrow().get(name).cloned()
    }
    pub fn parameter(&self, name: &str, parameter: &str) -> Option<Expr> {
        self.declarations
            .borrow()
            .get(name)?
            .literals
            .get(parameter)
            .cloned()
    }
    pub fn declarations(&self, name: &str) -> Option<Rc<DeclarationScope>> {
        self.declarations.borrow().get(name).cloned()
    }
    pub fn member_type(&self, name: &str, member: &str) -> Option<Type> {
        self.prepared
            .borrow()
            .values()
            .find_map(|symbols| symbols.class_member_types.get(name)?.get(member).cloned())
    }
    pub fn names(&self) -> Vec<String> {
        self.templates.borrow().keys().cloned().collect()
    }
    pub fn contains(&self, name: &str) -> bool {
        self.templates.borrow().contains_key(name)
    }
    pub fn register(
        &self,
        name: &str,
        tree: &SyntaxTree,
        imports: &ScopeSymbols,
        calls: Rc<Cell<usize>>,
        timescales: Arc<timescales::Timescales>,
    ) {
        if self.owners.borrow().contains_key(name) {
            return;
        }
        let scope = tree.root_unit().ok().map(|unit| unit.tokens);
        for declaration in &tree.declarations().classes {
            if declaration.parameter_ports.is_some()
                && enclosing_unit(tree.declarations(), declaration.tokens).map(|unit| unit.tokens)
                    == scope
            {
                self.templates.borrow_mut().insert(
                    format!(
                        "{}::{}",
                        name,
                        instances::normalize_identifier(&declaration.name.spelling)
                    ),
                    name.to_owned(),
                );
            }
        }
        self.owners.borrow_mut().insert(
            name.into(),
            Rc::new(Owner {
                tree: Rc::new(tree.clone()),
                imports: imports.clone(),
                calls,
                timescales,
            }),
        );
    }
    pub fn extend(&self, tree: &AnalysisContext<'_>, symbols: &mut ScopeSymbols) {
        let local_classes = scope_declarations(tree)
            .map(|class| declared_name(tree, class))
            .collect::<Vec<_>>();
        for (owner, extra) in self.prepared.borrow().iter() {
            if self.active.borrow().contains(owner) {
                continue;
            }
            // An owner may import a caller's static field while binding T's
            // methods. Returning those symbols to the caller must not redeclare
            // storage that the caller's own class declarations already supply.
            // Storage the symbols already hold, as a signal or as state, is
            // not declared again under the other kind: a scope analyzed inside
            // another, such as a module bound to an interface instance, starts
            // from symbols this registry already extended.
            let present = symbols
                .signals
                .iter()
                .chain(&symbols.state_signals)
                .map(|signal| signal.name())
                .collect::<HashSet<_>>();
            let local_storage = extra
                .signals
                .iter()
                .chain(&extra.state_signals)
                .filter(|signal| {
                    present.contains(signal.name())
                        || local_classes.iter().any(|class| {
                            signal.name().strip_prefix(class).is_some_and(|suffix| {
                                suffix.starts_with("::") || suffix.starts_with("#(")
                            })
                        })
                })
                .map(|signal| signal.name().to_owned())
                .collect::<HashSet<_>>();
            let declarations_only = symbols.declarations_only;
            if local_storage.is_empty() {
                symbols.extend(extra);
            } else {
                symbols.extend(&extra.clone().without(&local_storage));
            }
            symbols.declarations_only = declarations_only;
        }
    }
    pub fn states(&self) -> HashMap<String, crate::ir::Module> {
        self.states.borrow().clone()
    }

    pub fn request(
        self: &Rc<Self>,
        tree: &AnalysisContext<'_>,
        generic: &str,
        arguments: &[celox_sv_syntax::Argument],
        context: &PackedDimensions,
    ) -> Converted<Option<String>> {
        let Some(owner_name) = self.templates.borrow().get(generic).cloned() else {
            return Ok(None);
        };
        let owner = self.owners.borrow()[&owner_name].clone();
        let native = owner.tree.declarations();
        // The source can declare classes of the same name in other scopes;
        // the template is the one the owner package declares, as `register`
        // found it.
        let scope = owner.tree.root_unit().ok().map(|unit| unit.tokens);
        let declaration = native
            .classes
            .iter()
            .find(|class| {
                enclosing_unit(native, class.tokens).map(|unit| unit.tokens) == scope
                    && format!(
                        "{}::{}",
                        owner_name,
                        instances::normalize_identifier(&class.name.spelling)
                    ) == generic
            })
            .ok_or_else(|| unsupported(format!("class template `{generic}`")))?;
        let Some(arguments) = specializations::bind_arguments(
            tree,
            generic,
            declaration,
            native,
            arguments,
            context,
        )?
        else {
            return Ok(None);
        };
        // Module-local class names do not yet carry instance identity. They
        // cannot safely key a design-wide package specialization (8.25).
        fn module_class<'a>(tree: &AnalysisContext<'_>, ty: &'a Type) -> Option<&'a str> {
            ty.class()
                .filter(|class| {
                    !class.contains("::")
                        || super::declaration(tree, class).is_some_and(|declaration| {
                            enclosing_unit(tree.declarations(), declaration.tokens)
                                .is_some_and(|unit| unit.kind != celox_sv_syntax::UnitKind::Package)
                        })
                })
                .or_else(|| {
                    let container = ty.container.as_ref()?;
                    module_class(tree, &container.element).or_else(|| {
                        container
                            .key
                            .as_ref()
                            .and_then(|key| module_class(tree, key))
                    })
                })
        }
        if let Some(class) = arguments
            .types
            .values()
            .find_map(|ty| module_class(tree, ty))
        {
            return Err(unsupported(format!(
                "module-local class `{class}` as a package class type argument"
            )));
        }
        let index = {
            let mut requests = self.requests.borrow_mut();
            let requests = requests.entry(owner_name.clone()).or_default();
            match requests
                .iter()
                .position(|r| r.generic == generic && r.arguments == arguments)
            {
                Some(index) => index,
                None => {
                    if requests.len() >= 1024 {
                        return Err(unsupported("more than 1024 foreign class specializations"));
                    }
                    requests.push(Request {
                        generic: generic.into(),
                        arguments,
                        name: None,
                    });
                    self.complete.borrow_mut().remove(&owner_name);
                    requests.len() - 1
                }
            }
        };
        if self.requests.borrow()[&owner_name][index].name.is_none() {
            self.build(&owner_name, None)?;
        }
        Ok(self.requests.borrow()[&owner_name][index].name.clone())
    }

    fn build(self: &Rc<Self>, name: &str, caller: Option<&ScopeSymbols>) -> Converted<()> {
        if self.active.borrow().len() >= 16 {
            return Err(unsupported("foreign class scopes nested more than 16 deep"));
        }
        if !self.active.borrow_mut().insert(name.into()) {
            return Ok(());
        }
        let result = self.build_inner(name, caller);
        self.active.borrow_mut().remove(name);
        result
    }
    fn build_inner(self: &Rc<Self>, name: &str, caller: Option<&ScopeSymbols>) -> Converted<()> {
        let owner = self.owners.borrow()[name].clone();
        let requests = self.requests.borrow()[name].clone();
        let state = Rc::new(AnalysisState {
            foreign: self.clone(),
            next_call_site: owner.calls.clone(),
            foreign_arguments: RefCell::new(
                requests
                    .iter()
                    .map(|r| (r.generic.clone(), r.arguments.clone()))
                    .collect(),
            ),
            ..Default::default()
        });
        let context = AnalysisContext {
            syntax_tree: &owner.tree,
            state: state.clone(),
            parameter_overrides: Cow::Owned(HashMap::default()),
        };
        let _timescales =
            timescales::install_for_tree(&state.timescales, owner.timescales.clone(), &owner.tree)?;
        let _package = scope::enter_package(&state, name);
        let mut imports = owner.imports.clone();
        if let Some(caller) = caller {
            // Preserve the owner's existing bindings when caller metadata also
            // contains imported classes or routines from that package.
            imports.extend(&ScopeSymbols {
                classes: caller.classes.clone(),
                class_member_types: caller.class_member_types.clone(),
                state_signals: caller
                    .state_signals
                    .iter()
                    .filter(|signal| {
                        !signal.name().starts_with(&format!("{name}::"))
                            && signal
                                .package_variable
                                .as_ref()
                                .is_none_or(|(owner, _)| owner != name)
                    })
                    .cloned()
                    .collect(),
                // Only qualified routines, the methods of classes the actual
                // type arguments name: a body of the package resolves an
                // unqualified call in its own scope, not the caller's (26.3).
                subroutine_params: qualified(&caller.subroutine_params),
                subroutine_shapes: qualified(&caller.subroutine_shapes),
                subroutines: caller
                    .subroutines
                    .iter()
                    .filter(|subroutine| subroutine.name.contains("::"))
                    .cloned()
                    .collect(),
                function_return_types: qualified(&caller.function_return_types),
                ..Default::default()
            });
        } else {
            imports.declarations_only = true;
        }
        let self_qualified = imports::ScopeImports::from_scope(&context)?
            .qualified
            .iter()
            .any(|(package, _)| package == name);
        let mut package = packages::analyze_package_with_symbols(
            name,
            &context,
            imports.clone(),
            HashMap::default(),
            None,
        )?;
        if self_qualified {
            drop(_package);
            // Class statics already have qualified names. Unlike ordinary
            // package variables, their new declarations would collide with
            // the first pass's imported storage.
            let statics = package
                .symbols
                .classes
                .iter()
                .flat_map(|class| class.statics.iter())
                .filter(|property| property.owner.starts_with(&format!("{name}::")))
                .map(|property| property.variable.clone())
                .collect();
            let own_items = package.symbols.clone().without(&statics);
            package = packages::analyze_package_with_symbols(
                name,
                &context,
                imports,
                HashMap::default(),
                Some(&own_items),
            )?;
        }
        if let Some(error) = state.foreign_error.borrow_mut().take() {
            return Err(error);
        }
        let mut symbols = package.symbols;
        symbols.aliases.clear();
        symbols.declarations_only = false;
        for request in self.requests.borrow_mut().get_mut(name).unwrap() {
            if let Some((_, _, bound)) =
                state
                    .foreign_names
                    .borrow()
                    .iter()
                    .find(|(generic, arguments, _)| {
                        generic == &request.generic && arguments == &request.arguments
                    })
            {
                request.name = Some(bound.clone());
            }
        }
        for (class, specialization) in state.class_specializations.borrow().iter() {
            self.generics
                .borrow_mut()
                .insert(class.clone(), specialization.generic.clone());
        }
        let declarations =
            specializations::export_declarations(&context, &symbols.class_member_types);
        self.declarations.borrow_mut().extend(declarations);
        self.prepared.borrow_mut().insert(name.into(), symbols);
        if caller.is_some() {
            self.complete.borrow_mut().insert(name.into());
            if let Some(module) = package.state_module {
                self.states.borrow_mut().insert(name.into(), module);
            }
        }
        self.revision.set(self.revision.get() + 1);
        Ok(())
    }
    pub fn finish(
        self: &Rc<Self>,
        tree: &AnalysisContext<'_>,
        caller: &ScopeSymbols,
    ) -> Converted<()> {
        let declarations = specializations::export_declarations(tree, &caller.class_member_types);
        self.declarations.borrow_mut().extend(declarations);
        let package_owner = match tree.root_unit() {
            Ok(unit) if unit.kind == celox_sv_syntax::UnitKind::Package => {
                Some(instances::normalize_identifier(&unit.name.spelling))
            }
            Err(_) => Some(packages::UNIT.to_owned()),
            _ => None,
        };
        {
            let mut known = self.argument_symbols.borrow_mut();
            known
                .class_member_types
                .extend(caller.class_member_types.clone());
            let variables = caller
                .classes
                .iter()
                .flat_map(|class| {
                    class
                        .statics
                        .iter()
                        .map(|property| property.variable.as_str())
                })
                .collect::<HashSet<_>>();
            for signal in caller
                .signals
                .iter()
                .chain(&caller.state_signals)
                .filter(|signal| variables.contains(signal.name()))
            {
                if !known
                    .state_signals
                    .iter()
                    .any(|old| old.name == signal.name)
                {
                    let mut signal = signal.clone();
                    if signal.package_variable.is_none()
                        && let Some(owner) = &package_owner
                    {
                        signal.package_variable = Some((owner.clone(), signal.name().into()));
                    }
                    known.state_signals.push(signal);
                }
            }

            for class in &caller.classes {
                if !known.classes.iter().any(|old| old.name == class.name) {
                    known.classes.push(class.clone());
                }
            }
            known
                .subroutine_params
                .extend(caller.subroutine_params.clone());
            known
                .subroutine_shapes
                .extend(caller.subroutine_shapes.clone());
            for subroutine in &caller.subroutines {
                if !known
                    .subroutines
                    .iter()
                    .any(|old| old.name == subroutine.name)
                {
                    known.subroutines.push(subroutine.clone());
                }
            }
            known
                .function_return_types
                .extend(caller.function_return_types.clone());
        }
        let caller = self.argument_symbols.borrow().clone();
        let names = self
            .requests
            .borrow()
            .keys()
            .filter(|name| {
                !self.complete.borrow().contains(*name) && !self.active.borrow().contains(*name)
            })
            .cloned()
            .collect::<Vec<_>>();
        for name in names {
            self.build(&name, Some(&caller))?;
        }
        Ok(())
    }
}

pub(in crate::ast) fn is_foreign(tree: &AnalysisContext<'_>, generic: &str) -> bool {
    let registry = &tree.state.foreign;
    let Some(owner) = registry.templates.borrow().get(generic).cloned() else {
        return false;
    };
    tree.root_unit()
        .ok()
        .map(|unit| instances::normalize_identifier(&unit.name.spelling))
        .as_deref()
        != Some(owner.as_str())
}

/// The entries of `table` with qualified names, `scope::name`.
fn qualified<V: Clone>(table: &HashMap<String, V>) -> HashMap<String, V> {
    table
        .iter()
        .filter(|(name, _)| name.contains("::"))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
}
