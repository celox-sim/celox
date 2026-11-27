//! The state one analysis owns and changes as it goes.

use super::*;

/// What one analysis owns and changes as it goes: the caches it keys by
/// their inputs and what it reports. Analysis passes it down as `&mut`.
#[derive(Default)]
pub(crate) struct Session {
    /// The formals of a static task that suspends are static variables of
    /// the scope, shared by overlapping calls (IEEE 1800-2023 13.3.2), as an
    /// interface's are where its tasks are called through virtual
    /// interfaces: each instance has them once (see `virtual_interface_calls`).
    pub(super) shares_static_formals: bool,
    /// While a synchronous drive is bound, its target without the selects:
    /// the clockvar there names its clocking signal, which the drive updates
    /// (IEEE 1800-2023 14.16), not the value its clocking block sampled.
    pub(super) clockvar_drive: Option<celox_sv_syntax::ExprId>,
    /// Packages analyzed for their declarations alone, no executable body
    /// (see `Packages::analyze_needed`).
    pub(super) declarations_only: bool,
    pub(super) foreign: classes::foreign::Registry,
    pub(super) foreign_arguments: Vec<(String, classes::specializations::BoundArguments)>,
    pub(super) foreign_names: Vec<(String, classes::specializations::BoundArguments, String)>,
    pub(super) foreign_error: Option<AnalyzerError>,
    /// Whether a scope analysis that makes the specializations requested of
    /// package class templates once it ends is running, so that a request
    /// waits for it rather than analyzing the package at once (see
    /// [`classes::foreign::build_requested`]).
    pub(super) foreign_requests_wait: bool,
    /// Whether the packages are analyzed in rounds that only collect the
    /// specializations they request (see `Packages::analyze_packages`): a
    /// scope analysis ends after one pass, keeping what it would hand over
    /// to the specializations (see [`classes::foreign::keep_finish`]).
    pub(super) foreign_batch: bool,
    pub(super) dimensions: dimensions::State,
    pub(super) inline_randomizations: Vec<classes::constraints::Inline>,
    pub(super) next_call_site: usize,
    pub(super) diagnostics: Vec<warnings::AnalyzerWarning>,
    pub(super) generate_items: generate::Memo,
    /// The locals the array locator calls of each body being lowered have
    /// declared, by the id of its [`locators::LocalSink`].
    pub(super) locator_locals: HashMap<u64, Vec<LocalVariable>>,
    /// The id of the last [`locators::LocalSink`] opened.
    pub(super) next_locator_sink: u64,
    /// Classes laid out so far, by name, each with the recording of the
    /// tables of its declaring scope it was laid out in (see
    /// [`classes::specializations::DeclaringScope`]).
    pub(super) classes: HashMap<String, (std::rc::Rc<classes::Class>, Option<u64>)>,
    /// The classes whose objects dispatchers and `$cast` select from, as
    /// the scope last saw them.
    pub(super) concrete_classes: Option<std::rc::Rc<classes::ConcreteClasses>>,
    /// The constants and types of the scope that declares each class, which
    /// its parameter defaults and body see (IEEE 1800-2023 8.25).
    pub(super) class_declaration_scopes: HashMap<String, classes::specializations::DeclaringScope>,
    /// How many times a scope has recorded its tables for its classes,
    /// which numbers each recording.
    pub(super) class_scope_recordings: u64,
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
    /// The classes being laid out or specialized, one inside another, each
    /// with the declarations and the tokens of its declaration: a type
    /// declared in one belongs to that specialization (see
    /// [`types::declaring_scope`]).
    pub(super) class_scopes: Vec<types::ClassScope>,
    /// The classes whose constraints are being bound, one inside another,
    /// each with its base.
    pub(super) constraint_owners: Vec<(String, Option<String>)>,
    /// The lexical class of the constraint being bound, whose members it
    /// may access (IEEE 1800-2023 8.18), with a laid out class that it is
    /// or derives from: its base while it is laid out, or itself.
    pub(super) constraint_scopes: Vec<(String, Option<String>)>,
    /// The methods of the classes whose constraints are being bound, the
    /// innermost last, the calls of them those constraints make, and the
    /// index of the block being bound (IEEE 1800-2023 18.5.11).
    pub(super) constraint_methods: Vec<(Vec<classes::Method>, Vec<classes::ConstraintCall>, u32)>,
    /// The guards of the constraint operand being bound: the left operands
    /// of the `&&` and `||` it is the right operand of, as truths, so that
    /// a function call in it runs only while they hold (18.5.13).
    pub(super) constraint_guards: Vec<celox_heap::randomization::Expr>,
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
    /// The subroutines that call the tasks and functions of each
    /// specialization of a parameterized interface through virtual
    /// interfaces, by the specialization's key, built for the
    /// specializations earlier analyses named (see
    /// [`virtual_interface_calls::specializations`]).
    pub(super) virtual_interface_calls: virtual_interface_calls::Specializations,
    /// The semantic types of the design.
    pub(super) types: semantic_types::TyTable,
    /// The elaborated scope the session analyzes: a module, interface or
    /// program with its parameter values, which declares types of its own
    /// (see [`types::unit_scope`]). `None` for packages.
    pub(super) scope: Option<Arc<str>>,
    pub(super) expression_types: expression_types::Memo,
    /// The types found during the outermost type query that runs, see
    /// [`expression_types::Query`].
    pub(super) type_query: Option<expression_types::Query>,
    /// Whether the assertions of the design check its assertion control
    /// state (see [`assertion_control`]).
    pub(super) controls_assertions: bool,
    /// While the parameters of a module are collected once its enumeration
    /// members are constants, their types: an item of the pattern of an
    /// array parameter may name one (see
    /// [`array_parameters::constant_elements`]).
    pub(super) enum_constant_types: Option<ConstantTypes>,
    /// The scope-level covergroup types the scope being analyzed declares,
    /// by their reserved names (see [`covergroups`]).
    pub(super) covergroups: HashMap<String, Arc<covergroups::Covergroup>>,
    /// Those of `covergroups` whose `new` or methods the scope has bound, in
    /// the order it bound them: a module makes the subroutines of these
    /// alone.
    pub(super) covergroup_requests: Vec<String>,
    /// Those of `covergroup_requests` whose `get_coverage` or
    /// `get_inst_coverage` the scope has bound with the two `ref int`
    /// arguments (19.8): a module makes `N::$counts` and `N::$type_counts`
    /// of these alone.
    pub(super) covergroup_count_requests: Vec<String>,
    /// Those of `covergroup_requests` with a clocking event that a `new`
    /// other than the initializer of a declaration of the scope constructs
    /// (19.3): a module makes `N::$spawn` of these alone.
    pub(super) covergroup_spawn_requests: Vec<String>,
    /// The hidden variables that hold the constructed instances of those of
    /// `covergroup_spawn_requests` that a sampler of the scope walks (see
    /// `procedural::covergroup_sites::InstanceList`), by type: `N::$spawn`
    /// of such a type adds the instance to them instead of forking.
    pub(super) covergroup_instance_lists:
        HashMap<String, crate::ast::procedural::covergroup_sites::InstanceList>,
    /// What messages name the subroutines `new` and `sample` of the
    /// covergroup types the scope uses by, by the names of the subroutines,
    /// which no source spells (see `covergroups::in_covergroup_terms`).
    pub(super) covergroup_names: HashMap<String, covergroups::Named>,
    /// The elaborated module, interface or program being analyzed, by its
    /// name and the values of its parameters, which names the covergroup
    /// types it declares (see `covergroups::scope_names`): set once its
    /// parameters are known.
    pub(super) covergroup_scope: Option<Arc<str>>,
    /// The variables, ports and subroutines that the interface instances and
    /// port views bound into the scope being analyzed make of its members,
    /// whose types hold handles of a covergroup type of the interface: the
    /// scope's own code may not name them, as the handles belong to the body
    /// of each instance (see `covergroups::reject_outside_names`).
    pub(super) interface_covergroup_names: HashSet<String>,
}

/// The covergroup state of the analysis of a scope while the body of an
/// interface it binds is analyzed (see [`Session::enter_interface_body`]).
pub(super) struct OuterCovergroups {
    covergroups: HashMap<String, Arc<covergroups::Covergroup>>,
    requests: Vec<String>,
    count_requests: Vec<String>,
    spawn_requests: Vec<String>,
    instance_lists: HashMap<String, crate::ast::procedural::covergroup_sites::InstanceList>,
    names: HashSet<String>,
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
            // With the specializations earlier analyses named (25.9).
            virtual_interfaces: design
                .virtual_interfaces
                .clone()
                .or_else(|| packages.virtual_interfaces.clone()),
            virtual_interface_calls: design.virtual_interface_calls.clone(),
            types: std::mem::take(&mut design.types),
            controls_assertions: packages.controls_assertions(),
            ..Self::default()
        }
    }

    /// Starts analyzing the body of an interface for an instance or a port
    /// view of it: what its body binds of its covergroup types makes the
    /// subroutines and samplers of that instance alone, as those of a module
    /// are made for it. [`Self::leave_interface_body`] gives the scope that
    /// binds the interface its own back.
    pub(super) fn enter_interface_body(&mut self) -> OuterCovergroups {
        OuterCovergroups {
            covergroups: std::mem::take(&mut self.covergroups),
            requests: std::mem::take(&mut self.covergroup_requests),
            count_requests: std::mem::take(&mut self.covergroup_count_requests),
            spawn_requests: std::mem::take(&mut self.covergroup_spawn_requests),
            instance_lists: std::mem::take(&mut self.covergroup_instance_lists),
            names: std::mem::take(&mut self.interface_covergroup_names),
        }
    }

    /// Ends what [`Self::enter_interface_body`] started.
    pub(super) fn leave_interface_body(&mut self, outer: OuterCovergroups) {
        self.covergroups = outer.covergroups;
        self.covergroup_requests = outer.requests;
        self.covergroup_count_requests = outer.count_requests;
        self.covergroup_spawn_requests = outer.spawn_requests;
        self.covergroup_instance_lists = outer.instance_lists;
        self.interface_covergroup_names = outer.names;
    }

    /// Gives `packages` back what [`Self::with_packages`] took, with what
    /// this session added to it.
    pub(super) fn restore(self, design: &mut packages::Design) {
        design.call_sites = self.next_call_site;
        design.foreign = self.foreign;
        design.types = self.types;
        design.virtual_interfaces = self.virtual_interfaces;
        design.virtual_interface_calls = self.virtual_interface_calls;
    }
}
