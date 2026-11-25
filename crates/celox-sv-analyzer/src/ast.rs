//! SystemVerilog frontend AST.
//!
//! This semantic language frontend is separate from Celox runtime IR.
//!
//! Public AST types live here. Child modules construct modules, collect
//! declarations and processes, resolve types and constants, expand functions,
//! and normalize procedural expressions and assignments.

use fxhash::{FxHashMap as HashMap, FxHashSet as HashSet};
use std::{
    borrow::Cow,
    ops::{Deref, DerefMut},
    path::Path,
    sync::Arc,
};

use crate::source_tree::SourceTree as SyntaxTree;

use crate::procedural::ParamDirection;
pub use crate::system_functions::{
    ArrayQuery, Callee, ContainerKind, ContainerLayout, ContainerOp, ObjectOp, StringOp,
    SystemFunction, SystemTask,
};
use crate::{AnalyzerError, system_functions, typecheck};

mod array_compatibility;
mod array_parameters;
mod assignment_analysis;
mod assignments;
mod bound_patterns;
mod case;
mod casts;
mod classes;
pub(crate) use classes::ClassIndex;
pub(crate) use procedural::static_references::Candidate as StaticReference;
pub(crate) use procedural::static_references::candidates as static_reference_candidates;
mod assertion_control;
mod comb_process;
mod continuous_delays;
mod covergroups;
pub(crate) use covergroups::Bound as CovergroupBound;
pub(crate) use covergroups::CovergroupIndex;
pub(crate) use covergroups::formal_ports as covergroup_formal_ports;
pub(crate) use covergroups::is_constructor as covergroup_constructor;
pub(crate) use warnings::COVERAGE_DATABASE_TASKS;
mod events;
mod mailboxes;
mod net_delays;
mod net_drives;
mod procedural_assigns;
mod procedural_forces;
mod processes;
mod semaphores;
pub use bound_patterns::BoundPattern;
mod comb_rewrite;
pub(crate) mod const_functions;
mod constant_binding;
mod constant_folding;
mod constants;
mod declarations;
mod dimensions;
mod dpi;
mod enums;
mod expression_binding;
mod expression_types;
mod variable_dimensions;
use variable_dimensions::{VariableDimensions, VariableShape};
mod expressions;
mod ff_process;
mod function_returns;
mod functions;
mod generate;
pub(crate) mod imports;
mod inlining;
pub(crate) mod instances;
mod interface_ast;
mod interface_binding;
pub mod interfaces;
pub(crate) mod lets;
mod lifetimes;
pub(crate) mod module_index;
mod package_owners;
pub mod packages;
mod packed_structs;
mod parameters;
mod port_owners;
mod primitives;
mod timing;
mod type_binding;
pub(crate) use constant_folding::MAX_CONSTANT_VALUE_BITS;
pub(crate) use parameters::unbounded_parameter_marker;
mod array_reductions;
mod containers;
mod locators;
pub(crate) mod module_overrides;
mod patterns;
mod procedural;
mod scope;
mod scoped_map;
mod selects;
pub(crate) mod semantic_types;
mod shared_map;
mod statements;
pub(crate) mod subroutine_owners;
pub(crate) mod timescales;
mod type_names;
pub(crate) mod type_parameters;
mod types;
mod unpacked_structs;
pub(crate) mod validation;
mod virtual_interface_calls;
pub mod virtual_interfaces;
pub(crate) mod warnings;

mod context;
mod declaration_nodes;
mod expression_nodes;
mod module;
mod module_elaboration;
mod operators;
mod packed_dimensions;
mod process_nodes;
mod session;
mod source;
pub(crate) use source::Publication;
mod type_nodes;
pub(crate) use context::*;
pub(crate) use expression_nodes::*;
use module_elaboration::*;
pub use operators::*;
use packed_dimensions::*;
pub(crate) use session::*;
pub(crate) use type_nodes::*;

use array_compatibility::selected_unpacked_shape;
use assignment_analysis::two_state_conditions_are_complements;
use case::expr_is_two_state;
use casts::{
    expr_type_from_type, resize_integral_literal_for_cast, resize_unbased_fill_literal_for_cast,
};
use comb_process::{comb_processes_from_scope, port_connections};
use comb_rewrite::{expr_static_width, whole_packed_lvalue};
use constant_folding::{
    fold_const_integral_expr_preserving_mask, simplify_constant_mux_conditions,
};
use constants::{
    bind_generate_parameter, eval_ast_const_expr, expr_to_const, expr_to_index_const,
    substitute_assignment_constants_with_parameter_literals, substitute_const_expr_constants,
    substitute_const_expr_constants_preserving_enum_types, substitute_dimension_constants,
    substitute_expr_constants_with_parameter_literals, substitute_lvalue_constants,
    substitute_process_constants, substitute_process_constants_with_parameter_literals,
};
use declarations::{
    module_interface_from_scope, parameters_from_scope, ports_from_scope, signals_from_body_item,
    signals_from_scope,
};
use dimensions::{
    enum_marker, extend_const_env_with_variable_types, function_packed_dimension_widths,
    insert_parameter_type_markers, local_parameter_marker,
    packed_dimensions_from_ports_and_signals, parameter_dimension_marker,
    parameter_dimensions_marker, parameter_element_member_marker, parameter_element_members_marker,
    parameter_marker, parameter_member_chunk_marker, parameter_member_type_marker,
    parameter_member_value_marker, parameter_members_marker, parameter_packed_dimensions,
    parameter_rank_marker, parameter_signed_element_marker, parameter_signed_marker,
    parameter_type_from_const_env, parameter_types_from_const_env, parameter_width_marker,
    unpacked_dimension_widths, variable_bits_marker, variable_dimensions_marker,
    variable_signed_marker, variable_size_function_width, variable_size_marker,
    variable_unpacked_dimensions_marker,
};
use expressions::guard_zero_divisions;
use ff_process::ff_processes_from_scope;
use functions::{functions_from_scope, procedural_truth_condition};
use inlining::{expand_expr_calls, expr_signedness_with_return_types, substitute_expr_idents};
pub(crate) use instances::normalize_identifier;
use instances::{expr_ident_name, instances_from_scope};
#[cfg(test)]
use parameters::const_env_from_parameters;
use parameters::{
    apply_parameter_overrides, coerce_const_parameter_value, const_expr_from_i128,
    const_expr_to_expr, enum_member_constants_from_scope, extend_const_env_with_parameters,
    format_typed_parameter_literal, infer_const_expr_type, infer_parameter_value_type,
    parameter_element_literal, parameter_value_env, substitute_typed_parameter_literals,
};
use scoped_map::{ScopedMap, Signedness};
use selects::{add_expr, product_expr};
pub(crate) use shared_map::{ConstEnv, TypeAliases};
use shared_map::{ConstantTypeTable, ConstantTypes};
use shared_map::{SharedAliases, SharedEnv, SharedMap, ToShared};
use statements::{
    assignment_op_expr, coerce_procedural_assignment_rhs, expr_from_lvalue, lvalue_expr_type,
};
use types::{
    type_aliases_from_scope, type_aliases_from_scope_with_env, type_with_unpacked_ranges,
    validate_unpacked_dimension_sizes,
};
use validation::{AlwaysKind, always_comb_body, always_kind, reject_silently_ignored_constructs};

/// A procedural statement of the analyzer AST.
pub type Stmt = crate::procedural::StmtBase<Expr, LValue>;

/// The result of converting syntax that Celox may not be able to represent.
type Converted<T> = Result<T, AnalyzerError>;

/// The error for a construct the analyzer cannot represent.
fn unsupported(construct: impl Into<String>) -> AnalyzerError {
    AnalyzerError::Unsupported(construct.into())
}
pub type LocalVariable = crate::procedural::LocalVariableBase<crate::ir::Type>;
pub type Subroutine = crate::procedural::SubroutineBase<Expr, LValue, crate::ir::Type>;
pub type SubroutineParam = crate::procedural::SubroutineParamBase<Expr, crate::ir::Type>;

/// The positional interface of a module: its ports in declaration order and
/// the parameters of its `#(...)` list that an instantiation may override.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModuleInterface {
    pub ports: Vec<String>,
    pub parameters: Vec<String>,
}

/// The interface of each module, by module name.
pub type ModuleInterfaces = HashMap<String, ModuleInterface>;

/// Local declarations take precedence over interfaces supplied by other files.
struct InterfaceLookup<'a> {
    local: &'a ModuleInterfaces,
    extra: &'a ModuleInterfaces,
}

impl InterfaceLookup<'_> {
    fn get(&self, name: &str) -> Option<&ModuleInterface> {
        self.local.get(name).or_else(|| self.extra.get(name))
    }
}

#[derive(Debug, Clone)]
pub struct Source {
    modules: Vec<Module>,
    diagnostics: Vec<warnings::AnalyzerWarning>,
    /// The types the analysis interned, which its types are bound in: the
    /// IR is published with them (see [`Publication`]).
    types: semantic_types::TyTable,
}

/// The executable bodies of a module, moved out of it (see
/// [`Module::take_bodies`]).
pub(crate) struct ModuleBodies {
    pub assignments: Vec<Assignment>,
    pub comb_processes: Vec<CombProcess>,
    pub ff_processes: Vec<FfProcess>,
    pub initial_processes: Vec<InitialProcess>,
    pub subroutines: Vec<Subroutine>,
}

/// Overrides supplied by one instantiation. Types have been resolved in its scope.
#[derive(Clone, Copy)]
pub(crate) struct ModuleParameterBindings<'a> {
    pub declarations_only: bool,
    pub values: &'a HashMap<String, ConstExpr>,
    pub types: &'a [(String, Type)],
    pub patterns: &'a [(String, BoundPattern)],
    pub hierarchical: HierarchicalObjects<'a>,
    /// The variables and nets of the module that other modules force
    /// through hierarchical names.
    pub forced: &'a [String],
}

/// The objects of other instances that hierarchical names of the analyzed
/// module denote, resolved by the caller against the elaborated hierarchy.
#[derive(Debug, Clone, Copy, Default)]
pub struct HierarchicalObjects<'a> {
    pub signals: &'a [HierarchicalSignal],
    pub subroutines: &'a [HierarchicalSubroutine],
}

/// A function or task of another instance that hierarchical names of the
/// analyzed module call (IEEE 1800-2023 23.8), resolved by the caller
/// against the elaborated hierarchy. The module calls it as its subroutine
/// `name`, whose body the caller supplies once the module is analyzed: the
/// analysis binds the calls by this signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HierarchicalSubroutine {
    /// The names that denote the subroutine, as
    /// [`crate::ParsedSource::hierarchical_names`] spells them.
    pub paths: Vec<String>,
    pub name: String,
    pub formals: Vec<HierarchicalFormal>,
    /// The result of a function; `None` for a task or a `void` function.
    pub result: Option<Type>,
    pub is_task: bool,
}

/// A formal argument of a [`HierarchicalSubroutine`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HierarchicalFormal {
    pub name: String,
    pub direction: crate::procedural::ParamDirection,
    pub r#type: Type,
    /// The formal has a default, which an omitted argument takes.
    pub has_default: bool,
}

/// A signal of another instance that hierarchical names of the analyzed
/// module denote (IEEE 1800-2023 23.6), resolved by the caller against the
/// elaborated hierarchy. The module refers to it by a local signal `name`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HierarchicalSignal {
    /// The names that denote the signal, as [`crate::ParsedSource::hierarchical_names`]
    /// spells them.
    pub paths: Vec<String>,
    pub name: String,
    pub r#type: Type,
    pub is_net: bool,
    /// The names denote the module's own signal `name`, which they alias.
    pub local: bool,
    /// The interface instance of the module whose body writes the names
    /// (IEEE 1800-2023 23.8), as [`crate::InterfaceBodyScope::handle`]
    /// names it: they are names of that body, not of the module. `name` is
    /// then the body's placeholder for the object
    /// ([`virtual_interfaces::VirtualInterfaceSignal::Upward`]), never a
    /// name the body's own members could have.
    pub interface_instance: Option<String>,
    /// The module's own signal that the names of an interface body alias,
    /// when `local`.
    pub aliased: Option<String>,
    /// The typed literal of the value of the parameter of another instance
    /// that the names denote (IEEE 1800-2023 23.6), such as `32'sd12`:
    /// they read that value, and no signal stands for them.
    pub constant: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Module {
    name: String,
    /// A program (IEEE 1800-2023 24.3), whose processes run in the reactive
    /// region set.
    program: bool,
    parameters: Vec<Parameter>,
    ports: Vec<Port>,
    signals: Vec<Signal>,
    instances: Vec<Instance>,
    assignments: Vec<Assignment>,
    comb_processes: Vec<CombProcess>,
    ff_processes: Vec<FfProcess>,
    initial_processes: Vec<InitialProcess>,
    locals: Vec<LocalVariable>,
    subroutines: Vec<Subroutine>,
    dpi_imports: Vec<crate::ir::DpiImport>,
    /// The functions its constant expressions may call.
    constant_functions: const_functions::ConstantFunctions,
    /// Expression bodies used to publish symbolic declaration contracts.
    symbolic_functions: HashMap<String, Function>,
    declaration_subroutines: Vec<crate::ir::Subroutine>,
    /// The package parameters the module sees, by qualified name and by the
    /// names its imports bind.
    imported_parameters: Vec<Parameter>,
    /// Elaborated interface scopes, including those without data members.
    interface_scope_names: Vec<String>,
    /// The interface instances and ports a virtual interface may denote.
    interface_handles: Vec<crate::ir::InterfaceHandle>,
    /// Every interface instance and port of the module, whose connections
    /// tell the instance a port denotes (see [`crate::ir::Module::home_handles`]).
    home_handles: Vec<crate::ir::InterfaceHandle>,
    /// The classes of a module, whose overrides other scopes may call (see
    /// `module_overrides`).
    module_classes: Option<crate::ir::ModuleClasses>,
    /// The symbols of a package, for the scopes that use it.
    symbols: Option<scope::ScopeSymbols>,
}

pub(crate) fn generated_time_declarations(
    sources: &[(&str, &std::path::Path)],
    roots: Option<&[&str]>,
) -> Result<String, AnalyzerError> {
    timescales::generated_source_declarations(sources, roots, crate::AnalysisOptions::default())
}

/// The most iterations the loop generate constructs of one module expand to
/// in all, a bound on elaboration that IEEE 1800-2023 27.4 does not set: a
/// loop whose condition never becomes false is rejected rather than run
/// forever. OpenTitan's one-hot multiplexer of 312 bits from 32 inputs
/// expands to 10_296 iterations.
const MAX_GENERATE_LOOP_EXPANSION: usize = 1_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parameter {
    floating: Option<TypeKind>,
    semantic_type: Option<semantic_types::ConstantType>,
    name: String,
    value: Option<ConstExpr>,
    declared_width: Option<usize>,
    packed_ranges: Vec<PackedRange>,
    members: Vec<packed_structs::PackedMember>,
    declared_signed: Option<bool>,
    declared_is_2state: bool,
    has_declared_type: bool,
    is_local: bool,
    /// See [`Type::signed_element_depth`].
    signed_element_depth: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Port {
    randomizable: bool,
    is_const: bool,
    name: String,
    direction: PortDirection,
    r#type: Type,
    is_net: bool,
    unresolved_net: bool,
    net_resolution: NetResolution,
    default: Option<ConstExpr>,
    /// The expression of an explicitly named port (IEEE 1800-2023
    /// 23.2.2.2), which a continuous connection joins to the port.
    expression: Option<celox_sv_syntax::ExprId>,
}

/// The implicit driver of a pull or supply net (IEEE 1800-2023 6.6.5-6):
/// a weak pull to the value, or a supply of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NetDrive {
    #[default]
    None,
    Pull(bool),
    Supply(bool),
}

/// How the values of the drivers of a net combine (IEEE 1800-2023 6.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NetResolution {
    /// `wire` and `tri` (Table 6-2).
    #[default]
    Wire,
    /// `wand` and `triand` (Table 6-3).
    And,
    /// `wor` and `trior` (Table 6-4).
    Or,
    /// A net whose drivers Celox does not combine: `uwire`, the pull and
    /// supply nets, `trireg`.
    Single,
    /// A generic interconnect net or port (6.6.8), whose type and driver
    /// resolution the connections determine at elaboration (23.3.3.7.1).
    Interconnect,
}

impl NetResolution {
    pub(crate) fn of(net_type: &str) -> Self {
        match net_type {
            "wire" | "tri" => Self::Wire,
            "wand" | "triand" => Self::And,
            "wor" | "trior" => Self::Or,
            "interconnect" => Self::Interconnect,
            _ => Self::Single,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signal {
    is_const: bool,
    name: String,
    r#type: Type,
    is_net: bool,
    unresolved_net: bool,
    net_resolution: NetResolution,
    net_drive: NetDrive,
    /// The package variable the signal denotes, as `(package, variable)`.
    package_variable: Option<(String, String)>,
    /// The hierarchical name of the signal of another instance the signal
    /// denotes (IEEE 1800-2023 23.6).
    hierarchical: Option<String>,
    /// The force control the signal is (see `procedural_forces`).
    force_control: Option<crate::ir::ForceControl>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Instance {
    module_name: String,
    name: String,
    parameter_names: Vec<String>,
    parameter_overrides: Vec<ParameterOverride>,
    condition: Option<ConstExpr>,
    port_names: Vec<String>,
    port_connections: Vec<PortConnection>,
    /// The declared `[left:right]` bounds of an instance array.
    array_range: Option<(i128, i128)>,
    /// See [`crate::ir::Instance::interface_actuals`].
    interface_actuals: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParameterOverride {
    name: String,
    value: Option<ConstExpr>,
    /// The data type resolved in the instantiating scope.
    bound_type: Option<Type>,
    bound_pattern: Option<BoundPattern>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortConnection {
    formal: String,
    actual: String,
    actual_expr: Option<Expr>,
    bound_pattern: Option<BoundPattern>,
    array_operand: Option<crate::ir::ArrayAssignmentOperand>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortDirection {
    Input,
    Output,
    Inout,
    Ref,
    Unspecified,
}

/// A data type as binding resolves it: its [`TypeShape`], shared by its
/// copies until one changes, and the semantic type it was bound to, which
/// any change of the shape forgets. It is small, as types are held on
/// deeply recursive binding frames.
#[derive(Debug)]
pub struct Type {
    shape: Arc<TypeShape>,
    binding: semantic_types::Binding,
    /// The semantic type of an element of the type's unpacked dimensions
    /// (IEEE 1800-2023 7.4), which a change of those dimensions keeps
    /// (see [`Type::unpacked_ranges_mut`]) and any other change forgets.
    element: semantic_types::Binding,
    /// The enumerated type (IEEE 1800-2023 6.19) the type is, or whose
    /// elements a packed or unpacked array of the type has, which changes
    /// of its dimensions keep (see [`Type::reshape_keeping_enumeration`])
    /// and any other change forgets. Only [`Type::keep_enumeration`] sets
    /// it, with the fields that tell it (see
    /// [`variable_dimensions::kept`]).
    enumeration: variable_dimensions::kept::KeptEnumeration,
    /// The unpacked structure type (7.2) the type is, or whose elements a
    /// fixed-size unpacked array of the type has, which changes of its
    /// unpacked dimensions keep and any other change forgets. Only
    /// [`Type::keep_structure`] sets it, with the field that tells it.
    structure: variable_dimensions::kept::KeptStructure,
    /// The container type (7.5, 7.10, 7.8) the type is, or whose elements a
    /// fixed-size unpacked array of the type has, which changes of its
    /// unpacked dimensions keep and any other change forgets. Only
    /// [`Type::keep_container`] sets it, with the field that tells it.
    kept_container: variable_dimensions::kept::KeptContainer,
}

/// What a [`Type`] is made of.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TypeShape {
    kind: TypeKind,
    is_signed: bool,
    packed_ranges: Vec<PackedRange>,
    unpacked_ranges: Vec<UnpackedRange>,
    members: Vec<packed_structs::PackedMember>,
    /// How many leading packed dimensions select an element of a signed
    /// named type, which is signed although the whole array is not (IEEE
    /// 1800-2023 7.4.1).
    signed_element_depth: Option<usize>,
    /// The integer type keyword that declared the type, for `$typename`.
    keyword: type_names::Keyword,
}

/// Two types are equal if their shapes, the identities of their elements
/// and their enumerated and unpacked structure types are: a virtual
/// interface type has the shape of the number that holds its value, and its
/// element type tells its interface (IEEE 1800-2023 25.9); an enumerated
/// type, or an array of one, has the shape of its base type, and its
/// declaration tells it (6.19); an unpacked structure type has the shape of
/// its bits, and its declaration tells it (7.2, 6.22.1).
impl PartialEq for Type {
    fn eq(&self, other: &Self) -> bool {
        self.shape == other.shape
            && self.element.identity() == other.element.identity()
            && self.enumeration.declaration() == other.enumeration.declaration()
            && self.structure.identity() == other.structure.identity()
            && self.kept_container.declaration() == other.kept_container.declaration()
    }
}

impl Eq for Type {}

impl std::hash::Hash for Type {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.shape.hash(state);
        self.element.identity().hash(state);
        self.enumeration.declaration().hash(state);
        self.structure.identity().hash(state);
        self.kept_container.declaration().hash(state);
    }
}

impl std::ops::Deref for Type {
    type Target = TypeShape;

    fn deref(&self) -> &TypeShape {
        &self.shape
    }
}

impl std::ops::DerefMut for Type {
    /// A change of the shape may change its semantic type.
    fn deref_mut(&mut self) -> &mut TypeShape {
        self.binding.forget();
        self.element.forget();
        self.enumeration.forget();
        self.structure.forget();
        self.kept_container.forget();
        Arc::make_mut(&mut self.shape)
    }
}

impl Type {
    /// The shape of the type, for a change that keeps its semantic type,
    /// such as a bound replaced by its value: unlike through `DerefMut`, the
    /// type keeps its binding.
    pub(in crate::ast) fn shape_keeping_binding(&mut self) -> &mut TypeShape {
        Arc::make_mut(&mut self.shape)
    }

    /// The unpacked dimensions of the type, for a change that keeps the
    /// type of its elements: the type forgets its binding but not that of
    /// its elements, which the conversion of the changed type takes.
    pub(in crate::ast) fn unpacked_ranges_mut(&mut self) -> &mut Vec<UnpackedRange> {
        self.binding.forget();
        &mut Arc::make_mut(&mut self.shape).unpacked_ranges
    }

    /// What declares the enumerated type (IEEE 1800-2023 6.19) the type
    /// is, or whose elements a packed or unpacked array of the type has.
    pub(in crate::ast) fn enumeration(&self) -> Option<&semantic_types::EnumDeclaration> {
        self.enumeration.declaration()
    }

    /// The members of the enumerated type the type is, or whose elements
    /// an unpacked array of it has, if resolved: none for a packed array of
    /// it.
    pub(in crate::ast) fn enum_domain(&self) -> Option<&Arc<enums::EnumDomain>> {
        self.enumeration()
            .filter(|enumeration| self.packed_ranges.len() == enumeration.dims)
            .and_then(|enumeration| enumeration.domain.as_ref())
    }

    /// Bind the type to semantic type `id` of `table`, its elements to the
    /// type of an element of its unpacked dimensions, and its enumeration
    /// to that element if it is an enumerated type. A packed array of an
    /// enumerated type keeps the enumeration it has.
    pub(in crate::ast) fn bind(
        &mut self,
        table: &semantic_types::TyTable,
        id: semantic_types::TyId,
    ) {
        self.binding.set(table, id);
        let element = table.unpacked_element(id, self.unpacked_ranges.len());
        match element {
            Some(element) => self.element.set(table, element),
            None => self.element.forget(),
        }
        match element.map(|element| (element, table.enumeration(element))) {
            Some((element, Some(false))) => {
                let enumeration = semantic_types::EnumBinding::of(table, element);
                self.keep_enumeration(enumeration.unwrap_or_default());
            }
            Some((_, Some(true))) => {}
            _ => self.enumeration.forget(),
        }
        // The structure registered for the semantic type, or for that of
        // its elements (7.4.2).
        match table.unpacked_struct(id) {
            Some(aggregate) => self.keep_structure(Some(aggregate.clone())),
            None => self.structure.forget(),
        }
        // The type's own container, whose bounds its scope names, or else
        // the one registered for the semantic type or that of its elements.
        match self
            .kept_container
            .binding()
            .or_else(|| table.container(id).cloned())
        {
            Some(container) => self.keep_container(Some(container)),
            None => self.kept_container.forget(),
        }
    }
}

impl From<TypeShape> for Type {
    fn from(shape: TypeShape) -> Self {
        Self {
            shape: Arc::new(shape),
            binding: semantic_types::Binding::default(),
            element: semantic_types::Binding::default(),
            enumeration: variable_dimensions::kept::KeptEnumeration::default(),
            structure: variable_dimensions::kept::KeptStructure::default(),
            kept_container: variable_dimensions::kept::KeptContainer::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ContainerType {
    key: Option<Type>,
    layout: system_functions::ContainerLayout,
    element: Type,
    /// The record that holds an element of unpacked structure type which is
    /// not kept as a flat value.
    record: Option<system_functions::Record>,
    /// The declared range, left then right, of a fixed-size unpacked array
    /// class property, which an object holds in a container of that many
    /// elements: its type stays the fixed-size array (see
    /// `classes::Property::storage`).
    fixed: Option<(i128, i128)>,
    /// The semantic type of the container type, as the type that holds the
    /// container was bound to it (see [`semantic_types::Binding`]), so a
    /// reader that has the container alone takes it too.
    binding: semantic_types::Binding,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TypeKind {
    Real,
    Shortreal,
    /// IEEE 1800-2023 6.16: a dynamically sized string, held as a heap handle.
    String,
    /// IEEE 1800-2023 8.4: a handle of a class object, `null` or an object
    /// in the heap.
    Object,
    /// A scheduler-owned process reference (9.7), independent of the heap.
    Process,
    /// A handle of the synchronization object of a named event (15.5), or
    /// `null`.
    Event,
    /// A `chandle` (IEEE 1800-2023 6.14): an opaque C pointer, held as 64
    /// two-state bits, whose uses the analyzer restricts.
    Chandle,
    Bit,
    Logic,
    Reg,
    Implicit,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PackedRange {
    left: ConstExpr,
    right: ConstExpr,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UnpackedRange {
    left: ConstExpr,
    right: ConstExpr,
    size: Option<ConstExpr>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ConstExpr {
    Literal(String),
    Ident(String),
    Select {
        expr: Box<ConstExpr>,
        bit: Box<ConstExpr>,
    },
    /// A typed selection used inside another selection's index.
    SelectRange {
        expr: Box<ConstExpr>,
        msb: Box<ConstExpr>,
        lsb: Box<ConstExpr>,
        signed: bool,
    },
    /// An integral cast whose width and signedness remain independent of
    /// the enclosing index arithmetic.
    Resize {
        expr: Box<ConstExpr>,
        width: usize,
        signed: bool,
    },
    Function {
        callee: Callee,
        args: Vec<ConstExpr>,
        /// Tells a user subroutine call apart from a call written alike: a
        /// select repeats its index in its bounds and range checks, and the
        /// copies of one call share its site, so the call runs once.
        site: Option<usize>,
    },
    Unary {
        op: UnaryOp,
        expr: Box<ConstExpr>,
    },
    Binary {
        left: Box<ConstExpr>,
        op: BinaryOp,
        right: Box<ConstExpr>,
    },
    Mux {
        condition: Box<ConstExpr>,
        then_expr: Box<ConstExpr>,
        else_expr: Box<ConstExpr>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Assignment {
    lhs: LValue,
    rhs: Expr,
}

/// The window of a select of an analyzed lvalue, see
/// [`crate::ir::SelectWindow`], whose run-time position is a constant
/// expression of the analysis.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SelectWindow {
    pub lsb: u32,
    pub msb: u32,
    pub at: Option<Box<ConstExpr>>,
}

impl SelectWindow {
    /// The constant window of bits `lsb..=msb` of the variable.
    pub fn bits(lsb: u32, msb: u32) -> Self {
        Self { lsb, msb, at: None }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LValue {
    Ident(String),
    Select {
        name: String,
        msb: ConstExpr,
        lsb: ConstExpr,
        signed: bool,
        array_slice_width: Option<ConstExpr>,
        array_slice_reversed: bool,
        /// The bits of the variable a run-time select stays within: those of
        /// the structure member it is written on (see
        /// [`SelectWindow`]).
        window: Option<SelectWindow>,
        /// Whether this selection names a two-state packed struct member.
        is_2state: bool,
    },
    /// A class property or a container element, which no variable holds,
    /// or a selection of its value: `select` is the selection of a variable
    /// `OBJECT_VALUE` of the value's type (see `value_selects`), the whole
    /// value `LValue::Ident(OBJECT_VALUE)`. An assignment loads the value,
    /// assigns the selection and stores the value back, once for each
    /// statement (see `BodyBuilder::place_assignment`).
    Object {
        place: Box<ObjectPlace>,
        select: Box<LValue>,
        /// The checks of the tags of the members the selection crosses,
        /// which read `OBJECT_VALUE` (7.3.2): every write of the place
        /// applies them.
        guards: Vec<Expr>,
        /// The native expression of the whole value at the place, whose
        /// type the local that a write goes through has.
        whole: celox_sv_syntax::ExprId,
    },
}

/// The variable a selection of an [`ObjectPlace`] selects from.
pub(crate) const OBJECT_VALUE: &str = crate::ir::PLACE_VALUE;

/// Where an [`LValue::Object`] is: a property of the object `handle`, an
/// element, held by value, of the container `container` at `index`, or a
/// member of the unpacked structure value at another place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObjectPlace {
    Property {
        field: crate::system_functions::ObjectField,
        handle: Expr,
        r#type: Type,
    },
    Element {
        container: Expr,
        index: Expr,
        layout: crate::system_functions::ContainerLayout,
        element: Type,
        /// The immutable record that holds an element of unpacked
        /// structure type: a write stores a new record.
        record: Option<crate::system_functions::Record>,
    },
    /// The bits `offset` up of width `width` of the flat value at `outer`,
    /// of `outer_width` bits: the leaves of a member of an unpacked
    /// structure (7.2). A write stores the whole value at `outer` with
    /// these bits replaced.
    Member {
        outer: Box<ObjectPlace>,
        offset: usize,
        width: usize,
        outer_width: usize,
        r#type: Type,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CombProcess {
    /// Elaborated ticks, consumed by continuous_delays before IR publication.
    delay: Option<u64>,
    /// The value a delayed assignment drives before its first delay ends:
    /// the initial value of a sequential UDP's output (IEEE 1800-2023
    /// 29.3.3); x if `None`.
    delay_initial: Option<Expr>,
    /// A declaration's independent net delay, consumed before IR publication.
    net_delay: bool,
    /// The drive strength the continuous assignment or gate states (IEEE
    /// 1800-2023 10.3.4, 28.11).
    drive_strength: Option<crate::ir::DriveStrength>,
    creation_order: usize,
    site: Option<crate::procedural::SourceSpan>,
    /// Where the procedure is written, for an `always @*` that becomes a
    /// process when it calls a timed task.
    process_site: Option<crate::procedural::ProcessSite>,
    kind: CombProcessKind,
    condition: Option<ConstExpr>,
    assignments: Vec<Assignment>,
    body: Vec<Stmt>,
    sensitivity_body: Vec<Stmt>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CombProcessKind {
    ContinuousAssign,
    AlwaysComb,
    AlwaysLatch,
    AlwaysStar,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfProcess {
    always_ff: bool,
    suspendable: bool,
    creation_order: usize,
    site: Option<crate::procedural::ProcessSite>,
    events: Vec<FfEvent>,
    body: Vec<Stmt>,
}

/// A scheduled procedure, including initial/always processes and final procedures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InitialProcess {
    creation_order: usize,
    site: Option<crate::procedural::ProcessSite>,
    condition: Option<ConstExpr>,
    body: Vec<Stmt>,
    kind: celox_design::ProcessKind,
    /// Folded-away statements retained only for final-procedure legality checks.
    validation_body: Vec<Stmt>,
    synthetic: bool,
    /// An `always` process: its body runs forever.
    repeats: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FfEdge {
    Pos,
    Neg,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FfEvent {
    edge: FfEdge,
    signal: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConditionalAssignment {
    condition: Option<Expr>,
    assignment: Assignment,
    exhaustive_fallback_start: Option<usize>,
    /// Statement slot at which the controlling if/case chain was evaluated.
    guard_boundary: Option<usize>,
    /// Nested branch paths, from innermost to outermost.
    path_epochs: Vec<usize>,
}

impl crate::procedural::VisitFormats for Expr {
    fn visit_formats_mut(
        &mut self,
        f: &mut impl FnMut(&mut crate::system_functions::FormatOptions),
    ) {
        match self {
            Expr::Ident(_) | Expr::Literal(_) => {}
            Expr::Select { expr, .. } | Expr::Resize { expr, .. } | Expr::Unary { expr, .. } => {
                expr.visit_formats_mut(f)
            }
            Expr::Concat(parts) | Expr::RepeatConcat { parts, .. } => {
                for part in parts {
                    part.visit_formats_mut(f);
                }
            }
            Expr::Binary { left, right, .. } => {
                left.visit_formats_mut(f);
                right.visit_formats_mut(f);
            }
            Expr::Mux {
                condition,
                then_expr,
                else_expr,
            } => {
                condition.visit_formats_mut(f);
                then_expr.visit_formats_mut(f);
                else_expr.visit_formats_mut(f);
            }
            Expr::Call { callee, args, .. } => {
                if let Callee::String(crate::system_functions::StringOp::Format { options }) =
                    callee
                {
                    f(options);
                }
                for arg in args.iter_mut().flatten() {
                    arg.visit_formats_mut(f);
                }
            }
            Expr::Inside { expr, items } => {
                expr.visit_formats_mut(f);
                for item in items {
                    match item {
                        InsideItem::Value(value) => value.visit_formats_mut(f),
                        InsideItem::Range { low, high } => {
                            low.visit_formats_mut(f);
                            high.visit_formats_mut(f);
                        }
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Function {
    /// The resolved return type, retained through body binding and renaming.
    return_semantic_type: Option<semantic_types::TyId>,
    /// The aggregate return type, frozen in its declaration scope.
    return_type: Option<Type>,
    return_real: bool,
    /// Whether the function returns a `string`.
    return_string: bool,
    /// Whether the function returns a class handle.
    return_handle: bool,
    /// The function returns a `chandle`.
    return_chandle: bool,
    name: String,
    params: Vec<FunctionParam>,
    /// The function's value as one expression of its arguments, or `None`
    /// for a function or task written as statements: a call of it is not
    /// expanded into an expression.
    body: Option<Expr>,
    /// For each `output` / `inout` parameter, its value when the body ends,
    /// in terms of the input parameters.
    outputs: Vec<(String, Expr)>,
    return_width: Option<usize>,
    return_first_packed_dimension_width: Option<usize>,
    return_dimensions: Option<usize>,
    return_signed: bool,
    return_is_2state: bool,
    /// The container type of a dynamic array, associative array or queue
    /// result (IEEE 1800-2023 7.5, 7.8, 7.10).
    return_container: Option<Arc<ContainerType>>,
}

fn function_argument_metadata(params: &[FunctionParam]) -> Arc<[FunctionArgumentMetadata]> {
    params
        .iter()
        .map(|param| FunctionArgumentMetadata {
            name: param.name.clone(),
            direction: param.direction,
            has_default: param.has_default,
        })
        .collect()
}

/// The direction of a subroutine formal that a DPI-C argument has.
fn dpi_direction(direction: crate::ir::DpiDirection) -> ParamDirection {
    match direction {
        crate::ir::DpiDirection::Input => ParamDirection::Input,
        crate::ir::DpiDirection::Output => ParamDirection::Output,
        crate::ir::DpiDirection::Inout => ParamDirection::Inout,
    }
}

fn dpi_argument_metadata(import: &crate::ir::DpiImport) -> Arc<[FunctionArgumentMetadata]> {
    import
        .arguments()
        .iter()
        .map(|argument| FunctionArgumentMetadata {
            name: argument.name().to_string(),
            direction: dpi_direction(argument.direction()),
            has_default: false,
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FunctionParam {
    has_default: bool,
    direction: ParamDirection,
    name: String,
    width: Option<usize>,
    signed: bool,
    is_2state: bool,
    packed_dimensions: Vec<PackedDimension>,
    /// See [`Type::signed_element_depth`].
    signed_element_depth: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FunctionLocalType {
    width: usize,
    signed: bool,
    is_2state: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Expr {
    Ident(String),
    Literal(String),
    Select {
        expr: Box<Expr>,
        msb: ConstExpr,
        lsb: ConstExpr,
        signed: bool,
    },
    Concat(Vec<Expr>),
    RepeatConcat {
        count: ConstExpr,
        parts: Vec<Expr>,
    },
    Resize {
        expr: Box<Expr>,
        width: usize,
        signed: bool,
    },
    Unary {
        op: UnaryOp,
        expr: Box<Expr>,
    },
    Binary {
        left: Box<Expr>,
        op: BinaryOp,
        right: Box<Expr>,
    },
    Mux {
        condition: Box<Expr>,
        then_expr: Box<Expr>,
        else_expr: Box<Expr>,
    },
    Call {
        callee: Callee,
        /// Missing actuals retain their formal slots for declaration-scope defaults.
        args: Vec<Option<Expr>>,
        /// Formal positions in source evaluation/copy-out order; empty means positional.
        arg_order: Vec<usize>,
    },
    /// `expr inside { items }`: true when `expr` matches any item.
    Inside {
        expr: Box<Expr>,
        items: Vec<InsideItem>,
    },
}

/// One item of an `inside` set.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum InsideItem {
    /// A value matched with wildcard equality (`==?`).
    Value(Expr),
    /// An inclusive range `[low:high]`.
    Range { low: Expr, high: Expr },
}

/// Enum member constants collected from module-level `typedef enum`
/// declarations.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct EnumMemberConstants {
    /// Evaluated values for the module constant environment.
    numbers: ConstEnv,
    /// Resolved literal expressions for process-expression substitution.
    exprs: HashMap<String, Expr>,
    /// Base width and signedness retained for early constant substitution.
    types: HashMap<String, ExprType>,
    declarations: HashMap<String, semantic_types::ConstantType>,
}

impl EnumMemberConstants {
    /// Add the members of `other` that are not known already.
    fn extend_missing(&mut self, other: &EnumMemberConstants) {
        for (name, value) in &other.numbers {
            self.numbers.entry(name.clone()).or_insert(*value);
        }
        for (name, value) in &other.exprs {
            self.exprs
                .entry(name.clone())
                .or_insert_with(|| value.clone());
        }
        for (name, value) in &other.types {
            self.types.entry(name.clone()).or_insert(*value);
        }
        for (name, value) in &other.declarations {
            self.declarations
                .entry(name.clone())
                .or_insert_with(|| value.clone());
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ExprType {
    pub(crate) width: usize,
    pub(crate) signed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PackedDimension {
    left: ConstExpr,
    right: ConstExpr,
    width: ConstExpr,
    normalize_single: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UnpackedDimension {
    left: ConstExpr,
    right: ConstExpr,
    width: ConstExpr,
}

type VariablePackedDimensions = HashMap<String, VariableDimensions>;

#[derive(Debug, Clone, PartialEq, Eq)]
struct FunctionArgumentMetadata {
    name: String,
    direction: ParamDirection,
    has_default: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct FunctionReturnMetadata {
    /// Resolved self-determined result type in this design's semantic table.
    semantic_type: Option<semantic_types::TyId>,
    /// Validate calls in type queries without retaining or executing bodies.
    arguments: Option<Arc<[FunctionArgumentMetadata]>>,
    width: Option<usize>,
    first_packed_dimension_width: Option<usize>,
    dimensions: Option<usize>,
    signed: bool,
    is_2state: bool,
    /// The function returns a `string`.
    string: bool,
    /// The function returns a `chandle` (IEEE 1800-2023 6.14).
    chandle: bool,
    /// Declared aggregate type, available before function bodies are inlined.
    pattern_type: Option<Type>,
    /// The function returns a dynamic array, associative array or queue of
    /// this type (IEEE 1800-2023 7.5, 7.8, 7.10).
    container: Option<Arc<ContainerType>>,
}
