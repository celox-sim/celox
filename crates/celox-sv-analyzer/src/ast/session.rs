//! The state one analysis owns and changes as it goes.

use super::*;

/// What one analysis owns and changes as it goes: the caches it keys by
/// their inputs and what it reports. Analysis passes it down as `&mut`.
#[derive(Default)]
pub(crate) struct Session {
    pub(super) foreign: classes::foreign::Registry,
    pub(super) foreign_arguments: Vec<(String, classes::specializations::BoundArguments)>,
    pub(super) foreign_names: Vec<(String, classes::specializations::BoundArguments, String)>,
    pub(super) foreign_error: Option<AnalyzerError>,
    pub(super) dimensions: dimensions::State,
    pub(super) inline_randomizations: Vec<classes::constraints::Inline>,
    pub(super) next_call_site: usize,
    pub(super) diagnostics: Vec<warnings::AnalyzerWarning>,
    pub(super) generate_items: generate::Memo,
    /// Classes laid out so far, by name.
    pub(super) classes: HashMap<String, std::rc::Rc<classes::Class>>,
    /// The classes whose objects dispatchers and `$cast` select from, as
    /// the scope last saw them.
    pub(super) concrete_classes: Option<std::rc::Rc<classes::ConcreteClasses>>,
    /// The constants and types of the scope that declares each class, which
    /// its parameter defaults and body see (IEEE 1800-2023 8.25).
    pub(super) class_declaration_scopes: HashMap<String, (SharedEnv, SharedAliases)>,
    /// The specializations that class references have named, by class
    /// declaration (see [`classes::specializations`]).
    pub(super) class_specialization_names: HashMap<String, Vec<classes::specializations::Named>>,
    /// The classes declared in a generate block that elaboration repeats,
    /// which would be a class of their own in each repetition (27.4).
    pub(super) class_declarations_repeated: HashSet<String>,
    /// Set once the class methods of the scope being analyzed have their
    /// signatures, after which a new specialization would have none.
    pub(super) class_specializations_frozen: bool,
    /// The class specializations being made, which their own declarations
    /// may name before they are complete.
    pub(super) class_specializations_building: Vec<String>,
    /// The classes whose constraints are being bound, one inside another,
    /// each with its base.
    pub(super) constraint_owners: Vec<(String, Option<String>)>,
    /// The lexical class of the constraint being bound, whose members it
    /// may access (IEEE 1800-2023 8.18), with a laid out class that it is
    /// or derives from: its base while it is laid out, or itself.
    pub(super) constraint_scopes: Vec<(String, Option<String>)>,
    /// The methods of the classes whose constraints are being bound, the
    /// innermost last: the functions a constraint calls (IEEE 1800-2023
    /// 18.5.11).
    pub(super) constraint_methods: Vec<Vec<classes::Method>>,
    /// How many class specializations are being made, one inside another.
    pub(super) class_specialization_depth: usize,
    /// The specializations of classes made so far, in order.
    pub(super) class_specializations: Vec<(
        String,
        std::rc::Rc<classes::specializations::Specialization>,
    )>,
    /// The virtual interfaces of the design, if it has any (IEEE 1800-2023
    /// 25.9).
    pub(super) virtual_interfaces: Option<Arc<virtual_interfaces::VirtualInterfaces>>,
    /// The semantic types of the design.
    pub(super) types: semantic_types::TyTable,
    pub(super) expression_types: expression_types::Memo,
    /// The types found during the outermost type query that runs, see
    /// [`expression_types::Query`].
    pub(super) type_query: Option<expression_types::Query>,
}

impl Session {
    /// A session for a scope analyzed with `packages`, which takes over the
    /// package class specializations, types and call sites of the design
    /// until [`Self::restore`] gives them back.
    pub(super) fn with_packages(
        packages: &packages::Packages,
        design: &mut packages::Design,
    ) -> Self {
        Self {
            next_call_site: design.call_sites,
            foreign: std::mem::take(&mut design.foreign),
            virtual_interfaces: packages.virtual_interfaces.clone(),
            types: std::mem::take(&mut design.types),
            ..Self::default()
        }
    }

    /// Gives `packages` back what [`Self::with_packages`] took, with what
    /// this session added to it.
    pub(super) fn restore(self, design: &mut packages::Design) {
        design.call_sites = self.next_call_site;
        design.foreign = self.foreign;
        design.types = self.types;
    }
}
