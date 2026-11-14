//! Hierarchical references to the static variables of subroutines (IEEE
//! 1800-2023 23.6): `f.v` names the variable `v` that subroutine `f`
//! declares with a static lifetime, a local or the formal of a static
//! subroutine, the one object every call of `f` shares (6.21, 13.3.1), as
//! uvm-1.2's `uvm_component_name_check_visitor::end_v` frees
//! `visit.compiled_regex`.
//!
//! `f` is the subroutine its name denotes where the reference is: a method
//! of the class, its own or one of a base that is visible there (8.18), or
//! else one of the design element. `v` is one of the static variables that
//! `f`'s lowering declared, with the type it gave it (see
//! `BodyState::subroutine_statics`); an automatic variable is none (6.21).
//! A file's subroutines whose statics it names are lowered before its
//! others (see [`named`]).

use super::*;
use celox_sv_syntax::{Declarations, ExprKind};

/// An `f.v` name of a file whose `f` names a subroutine: a member select
/// that may be a hierarchical reference to a static variable of one.
#[derive(Debug)]
pub(crate) struct Candidate {
    subroutine: String,
    variable: String,
}

/// The `f.v` names of the file whose `f` names a subroutine, without
/// repeats.
pub(crate) fn candidates(native: &Declarations) -> Vec<Candidate> {
    let subroutines = native
        .subroutines
        .iter()
        .map(|subroutine| instances::normalize_identifier(&subroutine.name.spelling))
        .collect::<HashSet<_>>();
    let mut seen = HashSet::default();
    let mut found = Vec::new();
    for expression in &native.expressions {
        let ExprKind::Member {
            expression: receiver,
            separator,
            name,
        } = &expression.kind
        else {
            continue;
        };
        if separator != "." {
            continue;
        }
        let ExprKind::Name(subroutine) = &native.expression(*receiver).kind else {
            continue;
        };
        let subroutine = instances::normalize_identifier(&subroutine.spelling);
        if !subroutines.contains(&subroutine) {
            continue;
        }
        let variable = instances::normalize_identifier(&name.spelling);
        if seen.insert((subroutine.clone(), variable.clone())) {
            found.push(Candidate {
                subroutine,
                variable,
            });
        }
    }
    found
}

/// The candidates of the file of `tree`.
fn file_candidates<'t>(tree: &'t AnalysisContext<'_>) -> &'t [Candidate] {
    tree.spans()
        .static_references
        .get_or_init(|| candidates(tree.declarations()))
}

/// Whether the file of `tree` names variable `variable` of a subroutine
/// named `subroutine` hierarchically, `subroutine.variable`.
pub(in crate::ast) fn referenced(
    tree: &AnalysisContext<'_>,
    subroutine: &str,
    variable: &str,
) -> bool {
    file_candidates(tree)
        .iter()
        .any(|candidate| candidate.subroutine == subroutine && candidate.variable == variable)
}

/// Whether the file of `tree` names a static variable of a subroutine
/// named `name`, which is then lowered before the subroutines that may
/// name it.
pub(in crate::ast) fn named(tree: &AnalysisContext<'_>, name: &str) -> bool {
    file_candidates(tree)
        .iter()
        .any(|candidate| candidate.subroutine == name)
}

impl BodyBuilder<'_, '_, '_> {
    /// Bind each `f.v` of the file that names a static variable `v` of the
    /// subroutine `f` denotes in the body (see the module documentation).
    /// In a class scope, a method `f` hides a subroutine of the design
    /// element that a binding outside it found.
    pub(super) fn bind_static_references(&mut self) -> Converted<()> {
        let tree = self.tree;
        for candidate in file_candidates(tree) {
            let (subroutine, variable) = (&candidate.subroutine, &candidate.variable);
            // A variable `f` of the scope is no subroutine.
            if self.dims.get(subroutine).is_some() {
                continue;
            }
            let Some(owner) = self.static_owner(subroutine)? else {
                continue;
            };
            let Some((unique, r#type)) = self
                .state
                .subroutine_statics
                .get(&owner)
                .and_then(|statics| statics.get(variable))
                .cloned()
            else {
                continue;
            };
            if r#type.unpacked_struct().is_some() {
                continue;
            }
            let source = format!("{subroutine}.{variable}");
            let shape = bound_dimensions_from_type(
                &r#type,
                &tree.functions,
                &mut self.session.types,
                &self.dims.const_env,
            );
            let entry = self.take_binding(&source, Some(unique), r#type.packed_ranges.len());
            self.dims.insert(source.clone(), shape);
            self.dims
                .expression_signedness
                .insert(source, r#type.is_signed());
            self.names
                .scopes
                .last_mut()
                .expect("a body always has a scope")
                .entries
                .push(entry);
        }
        Ok(())
    }

    /// The owner name (see `BodyBuilder::set_owner`) of the subroutine
    /// `name` denotes in the body: in a class scope, a method of the class
    /// that is visible there (8.18); else one of the design element.
    fn static_owner(&mut self, name: &str) -> Converted<Option<String>> {
        if let Some(scope) = self.dims.class_scope.clone() {
            let class =
                expression_binding::object_class(self.tree, self.session, &self.dims, &scope)?;
            let Some(method) = class.method(name) else {
                return Ok(None);
            };
            let visible = expression_binding::check_member_access(
                self.tree,
                self.session,
                &self.dims,
                &method.owner,
                &method.name,
                method.qualifiers.local,
                method.qualifiers.protected,
            )
            .is_ok();
            return Ok(visible.then(|| classes::Class::subroutine_name(&method.owner, name)));
        }
        Ok(Some(name.to_owned()))
    }
}
