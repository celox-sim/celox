//! Dispatchers and `$cast` helpers completed with the classes of the design.
//!
//! A virtual method call dispatches on the class of the object (IEEE
//! 1800-2023 8.20), and `$cast` checks it (8.16), whatever scope runs the
//! code. A scope lowers both over the classes it sees, but an object may
//! reach it from code of another scope: a handle of a base class in a
//! package variable that another scope assigned an object of a class of a
//! package this scope does not use, or the code of an interface instance it
//! binds. The scope adds to each dispatcher and `$cast` helper its code
//! reaches the classes of the design's packages it misses: only those it
//! reaches, so a scope that calls none pays nothing.

use super::reachable_imports::subroutine_calls;
use super::*;

/// What a dispatcher or `$cast` helper is of, from its name (see
/// `classes::Class::dispatcher_name` and `classes::Class::cast_name`).
enum Helper<'a> {
    Dispatch { owner: &'a str, method: &'a str },
    Cast { owner: &'a str },
}

fn helper(name: &str) -> Option<Helper<'_>> {
    if let Some(owner) = name.strip_suffix("::$cast") {
        return Some(Helper::Cast { owner });
    }
    // A class name may have `::` in its parameters, a method name has not.
    let (owner, method) = name.strip_suffix("$virtual")?.rsplit_once("::")?;
    Some(Helper::Dispatch { owner, method })
}

/// The classes of the design and of the scope's imports, by name, and those
/// with objects, in name order.
struct Classes<'a> {
    by_name: HashMap<&'a str, &'a classes::Class>,
    concrete: Vec<&'a classes::Class>,
}

impl<'a> Classes<'a> {
    fn new(design: &'a packages::DesignSubroutines, imported: &'a scope::ScopeSymbols) -> Self {
        let mut by_name: HashMap<&str, &classes::Class> = design
            .classes
            .iter()
            .map(|(name, class)| (name.as_str(), &**class))
            .collect();
        for class in imported.classes.iter().chain(&imported.object_classes) {
            by_name.entry(class.name.as_str()).or_insert(class);
        }
        let mut concrete: Vec<_> = by_name
            .values()
            .copied()
            .filter(|class| !class.is_abstract && !class.is_interface)
            .collect();
        concrete.sort_unstable_by(|a, b| a.name.cmp(&b.name));
        Self { by_name, concrete }
    }

    /// Whether `class` is `ancestor`, derives from it or implements it.
    fn derives(&self, class: &classes::Class, ancestor: &str) -> bool {
        let mut pending = vec![class];
        let mut seen = HashSet::default();
        while let Some(class) = pending.pop() {
            for name in &class.lineage {
                if name == ancestor {
                    return true;
                }
                let Some(base) = self.by_name.get(name.as_str()) else {
                    continue;
                };
                for interface in &base.interfaces {
                    if interface == ancestor {
                        return true;
                    }
                    if seen.insert(interface.as_str())
                        && let Some(&interface) = self.by_name.get(interface.as_str())
                    {
                        pending.push(interface);
                    }
                }
            }
        }
        false
    }

    /// `helper` with the arms or classes it misses added, if it misses any.
    fn complete(&self, name: &str, helper: &Subroutine) -> Option<Subroutine> {
        let covered = procedural::case_ids(&helper.body);
        match self::helper(name)? {
            Helper::Dispatch { owner, method } => {
                let mut targets: Vec<(String, Vec<u32>)> = Vec::new();
                for class in &self.concrete {
                    if covered.contains(&class.id) || !self.derives(class, owner) {
                        continue;
                    }
                    let Some(implementation) = class.method(method) else {
                        continue;
                    };
                    let target = classes::Class::subroutine_name(&implementation.owner, method);
                    match targets.iter_mut().find(|(known, _)| *known == target) {
                        Some((_, ids)) => ids.push(class.id),
                        None => targets.push((target, vec![class.id])),
                    }
                }
                if targets.is_empty() {
                    return None;
                }
                let mut helper = helper.clone();
                procedural::extend_dispatch(&mut helper, &targets);
                Some(helper)
            }
            Helper::Cast { owner } => {
                let ids: Vec<u32> = self
                    .concrete
                    .iter()
                    .filter(|class| !covered.contains(&class.id) && self.derives(class, owner))
                    .map(|class| class.id)
                    .collect();
                if ids.is_empty() {
                    return None;
                }
                let mut helper = helper.clone();
                procedural::extend_cast(&mut helper, &ids);
                Some(helper)
            }
        }
    }
}

impl ScopeElaboration<'_> {
    /// Add to each dispatcher and `$cast` helper the scope's code reaches,
    /// its own or one it imports, the classes of the design it misses. The
    /// overrides the added arms call are reached through them, as a package
    /// subroutine the scope does not import is (see
    /// [`Self::reachable_imports`]).
    pub(super) fn complete_dispatchers(&mut self) {
        if self.is_package {
            return;
        }
        let Some(design) = self.imported.design_subroutines.0.clone() else {
            return;
        };
        if design.classes.is_empty() {
            return;
        }
        let imported = self.imported;
        let mut first_imported: HashMap<&str, &Subroutine> = HashMap::default();
        for subroutine in &imported.subroutines {
            first_imported
                .entry(subroutine.name.as_str())
                .or_insert(subroutine);
        }
        let own: HashMap<String, usize> = self
            .subroutines
            .iter()
            .enumerate()
            .map(|(index, subroutine)| (subroutine.name.clone(), index))
            .collect();
        // A helper of the scope's own is reached only through a call.
        let mut pending: Vec<String> = self
            .own_calls_except(|subroutine| helper(&subroutine.name).is_some())
            .into_iter()
            .map(str::to_owned)
            .collect();
        let mut reached: HashSet<String> = pending.iter().cloned().collect();
        let mut classes = None;
        let mut completed: Vec<Subroutine> = Vec::new();
        while let Some(name) = pending.pop() {
            let subroutine = match own.get(&name) {
                Some(&index) => &self.subroutines[index],
                None => match first_imported.get(name.as_str()).copied().or_else(|| {
                    design
                        .subroutines
                        .get(&name)
                        .map(|subroutine| &**subroutine)
                }) {
                    Some(subroutine) => subroutine,
                    None => continue,
                },
            };
            let mut subroutine = subroutine;
            if helper(&name).is_some() {
                let classes = classes.get_or_insert_with(|| Classes::new(&design, imported));
                if let Some(complete) = classes.complete(&name, subroutine) {
                    completed.push(complete);
                    subroutine = completed.last().unwrap();
                }
            }
            let mut calls = Vec::new();
            subroutine_calls(subroutine, &mut calls);
            for call in calls {
                if !reached.contains(call) {
                    reached.insert(call.to_owned());
                    pending.push(call.to_owned());
                }
            }
        }
        for subroutine in completed {
            match own.get(&subroutine.name) {
                Some(&index) => self.subroutines[index] = subroutine,
                None => self.subroutines.push(subroutine),
            }
        }
    }
}
