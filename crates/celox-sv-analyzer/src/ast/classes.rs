//! Classes (IEEE 1800-2023 clause 8).
//!
//! A class variable holds a handle of an object in the simulator's heap. The
//! analyzer lays out the properties of each class as bytes of the object,
//! in the encoding of state memory, and rewrites property accesses into
//! [`ObjectOp`] calls that carry that layout. Methods become subroutines whose
//! first argument is the handle `this`; a constructor allocates the object
//! and returns its handle.

use std::rc::Rc;

use super::*;
use crate::system_functions::{ContainerKind, HandleProperty, ObjectField, ObjectOp};

pub(super) mod constraints;
pub(super) mod foreign;
pub(super) mod specializations;

/// Bit width of a handle, see `celox_design::HANDLE_WIDTH`.
pub(super) const HANDLE_WIDTH: usize = 32;

/// The name of the implicit handle argument of a method.
pub(super) const THIS: &str = "this";

/// A property of a class as laid out in its objects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Property {
    pub name: String,
    pub owner: String,
    pub constant: bool,
    pub local: bool,
    pub protected: bool,
    pub r#type: Type,
    pub field: ObjectField,
    pub rand: bool,
    pub randc: bool,
    /// The initializer of the declaration, evaluated by the constructor.
    pub initializer: Option<celox_sv_syntax::ExprId>,
    /// The container an object holds a fixed-size unpacked array property
    /// in, one element for each index (IEEE 1800-2023 7.4): its type stays
    /// the array, which every type check and query sees, and an assignment
    /// or a copy of the object copies the elements.
    pub storage: Option<Arc<ContainerType>>,
}

/// Where a randomize call solves a static rand property (see
/// [`StaticProperty::rand`]): a slot of each object of the class, which is
/// no member of it, keyed by the declaration of the property, its variable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StaticSlot {
    /// The variable that holds the property.
    pub variable: String,
    /// The variable of the bit that turns it off for every object (18.8).
    pub mode: String,
    pub r#type: Type,
    /// The bytes of the object the call solves it in.
    pub field: ObjectField,
}

/// The member of a class that a name denotes in it (IEEE 1800-2023 8.14):
/// the one the most derived of the class and its bases declares, an
/// instance property, one of unpacked structure type, or a static
/// property, which hide each other alike.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Member {
    /// Property `0` of [`Class::properties`].
    Instance(usize),
    /// Property `0` of [`Class::aggregates`], of unpacked structure type.
    Aggregate(usize),
    /// Property `0` of [`Class::statics`].
    Static(usize),
}

/// The member that `name` denotes in a class whose lineage, from the class
/// itself to its root, is `lineage`, with `properties`, `aggregates` and
/// `statics` (see [`Member`]).
pub(super) fn member(
    name: &str,
    lineage: &[String],
    properties: &[Property],
    aggregates: &[AggregateProperty],
    statics: &[StaticProperty],
) -> Option<Member> {
    let instance = |owner: Option<&str>| {
        properties
            .iter()
            .rposition(|property| {
                property.name == name && owner.is_none_or(|owner| property.owner == owner)
            })
            .map(Member::Instance)
    };
    let aggregate = |owner: Option<&str>| {
        aggregates
            .iter()
            .rposition(|aggregate| {
                aggregate.name == name && owner.is_none_or(|owner| aggregate.owner == owner)
            })
            .map(Member::Aggregate)
    };
    let shared = |owner: Option<&str>| {
        statics
            .iter()
            .rposition(|property| {
                property.name == name
                    && property.mode_of.is_none()
                    && owner.is_none_or(|owner| property.owner == owner)
            })
            .map(Member::Static)
    };
    let declared = |owner: Option<&str>| {
        instance(owner)
            .or_else(|| aggregate(owner))
            .or_else(|| shared(owner))
    };
    lineage
        .iter()
        .find_map(|owner| declared(Some(owner)))
        // A class whose lineage is not known names its members as declared.
        .or_else(|| declared(None))
}

impl Property {
    /// A rand object handle: the variables and constraints of the object it
    /// refers to are solved with those of the object that holds it (IEEE
    /// 1800-2023 18.4).
    /// Whether the property is a dynamic array or queue (IEEE 1800-2023
    /// 7.5, 7.10), whose size a constraint may name (18.4).
    pub fn sequence(&self) -> bool {
        self.r#type.unpacked_ranges.is_empty()
            && self.r#type.container.as_ref().is_some_and(|container| {
                matches!(
                    container.layout.kind,
                    ContainerKind::Dynamic | ContainerKind::Queue { .. }
                )
            })
    }

    /// The container that holds the elements of an array property a
    /// constraint program sees as an array: a dynamic array or queue, or a
    /// fixed-size array (see `Property::storage`).
    pub fn array_storage(&self) -> Option<&Arc<ContainerType>> {
        self.storage
            .as_ref()
            .or_else(|| self.r#type.container.as_ref().filter(|_| self.sequence()))
    }

    /// Whether a constraint program holds the property as an array.
    pub fn constraint_array(&self) -> bool {
        self.array_storage().is_some()
    }

    /// Whether the property is a rand array: a dynamic array or queue, or
    /// a fixed-size array (18.4).
    pub fn rand_array(&self) -> bool {
        (self.rand || self.randc) && (self.sequence() || self.storage.is_some())
    }

    pub fn rand_object(&self) -> bool {
        self.rand
            && self.r#type.class().is_some()
            && self.r#type.container.is_none()
            && self.r#type.unpacked_ranges.is_empty()
    }

    pub fn cyclic_domain_size(&self) -> Option<u16> {
        if self.field.handle.is_some() {
            return None;
        }
        if self.r#type.is_enum {
            self.r#type.enum_domain.as_ref().and_then(|domain| {
                let values = &domain.members;
                (1..=256)
                    .contains(&values.len())
                    .then_some(values.len() as u16)
            })
        } else {
            (1..=8)
                .contains(&self.field.width)
                .then(|| 1u16 << self.field.width)
        }
    }
}

/// A property of unpacked structure type, or a member of one that is itself
/// an unpacked structure. It is not laid out: its leaf members are
/// properties named by their member path (see `unpacked_structs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AggregateProperty {
    pub name: String,
    pub owner: String,
    pub local: bool,
    pub protected: bool,
    pub r#type: Type,
    pub initializer: Option<celox_sv_syntax::ExprId>,
    /// How many properties precede it: a later declaration of the same
    /// name hides an earlier one.
    pub position: usize,
}

/// A static property of a class: one variable of the module that declares
/// the class, shared by all of its objects (IEEE 1800-2023 8.9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StaticProperty {
    pub name: String,
    pub local: bool,
    pub protected: bool,
    /// The class that declares the property.
    pub owner: String,
    pub r#type: Type,
    /// The name of the variable that holds the property.
    pub variable: String,
    pub initializer: Option<celox_sv_syntax::ExprId>,
    /// A `static rand` property, one variable of the class that every
    /// randomize call of an object of it solves (IEEE 1800-2023 18.4): the
    /// call solves it in the object's [`StaticSlot`] for it, seeded from it,
    /// and writes it back; whether it is random is a static bit all objects
    /// share (18.8).
    pub rand: bool,
    /// For the bit that turns static rand property `name` off, `name`: a
    /// variable of the class no source names (see [`static_rand_mode`]).
    pub mode_of: Option<String>,
}

/// The static properties the constraints of class `owner` read as inputs
/// of its program: those of its base, at the indices the base's constraints
/// read them at, then those it declares that the solver holds, as it holds
/// a property (see `constraints::solver_type`). An input is its variable:
/// a static the class redeclares is another input (8.14, 18.5).
fn static_inputs(
    owner: &str,
    base: Option<&[StaticInput]>,
    statics: &[StaticProperty],
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &ConstEnv,
) -> Vec<StaticInput> {
    let mut inputs = base.map(<[_]>::to_vec).unwrap_or_default();
    for property in statics {
        if property.owner != owner || property.rand || property.mode_of.is_some() {
            continue;
        }
        // A class handle, which a guard compares (18.5.13), by its handle.
        if property.r#type.class().is_some() {
            inputs.push(StaticInput {
                name: property.name.clone(),
                variable: property.variable.clone(),
                width: 8 * celox_heap::HANDLE_BYTES as u32,
                signed: false,
            });
            continue;
        }
        if constraints::solver_type(&property.r#type, tree, session, env).is_err() {
            continue;
        }
        let Some(shape) = casts::expr_type_from_type(&tree.functions, &property.r#type, env) else {
            continue;
        };
        inputs.push(StaticInput {
            name: property.name.clone(),
            variable: property.variable.clone(),
            width: shape.width as u32,
            signed: shape.signed,
        });
    }
    inputs
}

/// Whether constraint `expr` reads an input of its program.
fn reads_input(expr: &celox_heap::randomization::Expr) -> bool {
    use celox_heap::randomization::Expr;
    match expr {
        Expr::Input { .. } => true,
        Expr::Enabled { value, .. }
        | Expr::Soft { value, .. }
        | Expr::Resize { value, .. }
        | Expr::Unary { value, .. } => reads_input(value),
        Expr::DisableSoft {
            variable,
            condition,
            ..
        } => reads_input(variable) || condition.as_deref().is_some_and(reads_input),
        Expr::Foreach { body, .. } => reads_input(body),
        Expr::Binary { left, right, .. } => reads_input(left) || reads_input(right),
        Expr::Dist {
            membership,
            choices,
        } => reads_input(membership) || choices.iter().any(|choice| reads_input(&choice.predicate)),
        Expr::Literal(..)
        | Expr::Field(_)
        | Expr::Handle(_)
        | Expr::Size(_)
        | Expr::Sum(_)
        | Expr::Index { .. }
        | Expr::Static(_)
        | Expr::Solved(_)
        | Expr::Member { .. }
        | Expr::Element { .. } => false,
    }
}

/// The name of the static bit that turns static rand property `name` off,
/// for every object: a `$` starts it, as no identifier of a source starts
/// once normalized, so that it names no property of the class.
fn static_rand_mode(name: &str) -> String {
    format!("$rand_mode${name}")
}

/// The change key of the slot of static rand property `name` (see
/// [`StaticSlot`]), which no identifier of a source is either (see
/// [`static_rand_mode`]).
fn static_rand_slot(name: &str) -> String {
    format!("$slot${name}")
}

/// The variable of the bit that turns static rand property `name` of class
/// `owner` off (see [`StaticSlot::mode`]).
pub(super) fn static_rand_mode_variable(owner: &str, name: &str) -> String {
    Class::subroutine_name(owner, &static_rand_mode(name))
}

/// A static property that a constraint reads: the value of its variable,
/// input `index` of the program when a randomize call starts (IEEE
/// 1800-2023 18.5).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StaticInput {
    pub name: String,
    pub variable: String,
    pub width: u32,
    pub signed: bool,
}

/// A call of a function of the class in a constraint (IEEE 1800-2023
/// 18.5.11): the function, by the name the class's randomize call calls it
/// by, so that an override of the object's class runs, each argument with
/// its width and signedness, which the call converts as any call does, and
/// the width and signedness of the function's value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ConstraintCall {
    pub method: String,
    /// The class that declares the method the class of the constraint
    /// sees, which a call of a non-virtual method runs; a call of a
    /// virtual one runs the override of the object's class (IEEE
    /// 1800-2023 8.20).
    pub owner: String,
    pub is_virtual: bool,
    pub arguments: Vec<(celox_heap::randomization::Expr, u32, bool)>,
    pub width: u32,
    pub signed: bool,
    /// Whether the function's value is 4-state: its X and Z bits reach the
    /// constraints with it (see `Program::unknown_inputs`).
    pub four_state: bool,
    /// The constraint block that makes the call, by index among
    /// [`Class::constraints`], whose constraint mode its arguments follow.
    pub block: u32,
    /// Whether the block is the class's: a block a derived class overrides
    /// makes no call.
    pub live: bool,
}

/// A method of a class.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Method {
    pub name: String,
    /// The class that declares the method.
    pub owner: String,
    /// The definition, or the prototype of a pure virtual method, in the
    /// source of the scope that declares the class.
    pub native: celox_sv_syntax::SubroutineId,
    /// The names of its formals, which a class declared in another source
    /// is called with.
    pub formals: Vec<Option<String>>,
    /// A function, not a task.
    pub is_function: bool,
    /// Its return type, resolved in the scope of the class: `None` for a
    /// task, `void` or a constructor, and unknown when it cannot be bound.
    pub result: Option<Option<Type>>,
    pub qualifiers: celox_sv_syntax::ClassQualifiers,
    /// Declared `virtual` here or in a base: a call through a handle runs
    /// the override of the object's class (IEEE 1800-2023 8.20).
    pub is_virtual: bool,
}

impl Method {
    /// A pure virtual method, which has no implementation (8.21).
    pub fn is_pure(&self) -> bool {
        self.qualifiers.is_pure
    }
}

/// A class with the layout of its objects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Class {
    pub name: String,
    /// Identifies the class at run time; stable across modules and runs.
    pub id: u32,
    pub base: Option<String>,
    /// The interface classes the class implements, or the interface class
    /// extends (IEEE 1800-2023 8.26).
    pub interfaces: Vec<String>,
    /// A `virtual class` or an interface class, which cannot be constructed
    /// (IEEE 1800-2023 8.21, 8.26).
    pub is_abstract: bool,
    /// An interface class: it has no properties and its methods are pure
    /// virtual (IEEE 1800-2023 8.26).
    pub is_interface: bool,
    /// Properties of the class and its bases, bases first.
    pub properties: Vec<Property>,
    /// How many of `properties` the bases declare.
    pub base_properties: usize,
    /// Properties of unpacked structure type of the class and its bases.
    pub aggregates: Vec<AggregateProperty>,
    /// Static properties of the class and its bases.
    pub statics: Vec<StaticProperty>,
    /// The static properties its constraints read, input `n` of its program
    /// the `n`th (see [`StaticInput`]).
    pub static_inputs: Vec<StaticInput>,
    /// The number of classes it derives from, whose constraints are below
    /// its own (IEEE 1800-2023 18.5.13.1).
    pub depth: usize,
    /// The slots its randomize calls solve its static rand properties in,
    /// those of its base first, [`Expr::Static`] `n` the `n`th.
    ///
    /// [`Expr::Static`]: celox_heap::randomization::Expr::Static
    pub static_slots: Vec<StaticSlot>,
    /// The bit of the argument of a randomize call that selects each of
    /// `properties` and each of `static_slots` (see
    /// [`Class::selection_type`]): a base's bits are those of the base, so
    /// that a mask a call through a base handle builds selects the same
    /// members in the class it dispatches to.
    pub property_bits: Vec<usize>,
    pub slot_bits: Vec<usize>,
    /// The class and its bases, from the class to its root.
    pub lineage: Vec<String>,
    /// The static constraint blocks of the class, by index among
    /// [`Class::constraints`], each with the identifier of the class that
    /// declares it, whose objects share its constraint mode (IEEE 1800-2023
    /// 18.5.11); the heap keeps the mode (see `program_registration`).
    pub static_constraints: Vec<(u32, u32)>,
    /// The calls of functions its constraints make, those of its base
    /// first, value `k` of a constraint the input
    /// [`constraints::RESULT_INPUTS`]` + k` (IEEE 1800-2023 18.5.11).
    pub constraint_calls: Vec<ConstraintCall>,
    /// Methods visible in the class, overrides replacing inherited ones.
    pub methods: Vec<Method>,
    /// The parameters and typedefs of an interface class, its own and
    /// those it inherits, each with the class that declares it (IEEE
    /// 1800-2023 8.26.3).
    pub declarations: Vec<(String, String)>,
    /// Bytes of an object.
    pub size: u32,
    pub has_constraints: bool,
    pub constraints: Vec<(String, Result<Vec<celox_heap::randomization::Expr>, String>)>,
    /// The constraint program of the class, or why it has none, built once
    /// for each class when it is laid out.
    pub program: Converted<celox_heap::randomization::Program>,
}

/// 4-state value `value` that constraints read as the solver takes it (see
/// `Program::unknown_inputs`): its 2-state value, which has 0 for each X or
/// Z bit, and the mask of those, as a known bit and its complement are 1
/// once (IEEE 1800-2023 18.3).
pub(in crate::ast) fn known_bits(value: Expr) -> (Expr, Expr) {
    let two_state = |value: Expr| Expr::Unary {
        op: UnaryOp::ToTwoState,
        expr: Box::new(value),
    };
    let known = Expr::Binary {
        left: Box::new(two_state(value.clone())),
        op: BinaryOp::BitXor,
        right: Box::new(two_state(Expr::Unary {
            op: UnaryOp::BitNot,
            expr: Box::new(value.clone()),
        })),
    };
    let mask = Expr::Unary {
        op: UnaryOp::BitNot,
        expr: Box::new(known),
    };
    (two_state(value), mask)
}

/// An internal unsigned packed argument of a generated method.
pub(super) fn unsigned_type(width: usize) -> Type {
    let mut ty = procedural::int_type();
    ty.is_signed = false;
    ty.packed_ranges = vec![PackedRange::new(
        ConstExpr::Literal((width - 1).to_string()),
        ConstExpr::Literal("0".into()),
    )];
    ty
}

impl Class {
    /// The bits of the selection of a randomize call: whether it has
    /// arguments, then whether it names each property, then each static
    /// rand property, by its slot (18.11).
    pub fn selection_type(&self) -> Type {
        unsigned_type(self.properties.len() + self.static_slots.len() + 1)
    }

    /// The bits of a selection for the members a class adds to those of
    /// `base`: its new properties and slots take the bits after the base's
    /// (see [`Class::property_bits`]).
    fn selection_bits(
        base: Option<&Class>,
        properties: usize,
        slots: usize,
    ) -> (Vec<usize>, Vec<usize>) {
        let (mut property_bits, mut slot_bits) = base
            .map(|base| (base.property_bits.clone(), base.slot_bits.clone()))
            .unwrap_or_default();
        let mut next = 1 + property_bits.len() + slot_bits.len();
        for bits in [(&mut property_bits, properties), (&mut slot_bits, slots)] {
            while bits.0.len() < bits.1 {
                bits.0.push(next);
                next += 1;
            }
        }
        (property_bits, slot_bits)
    }

    /// The member that `name` denotes in the class (see [`Member`]).
    pub(super) fn member(&self, name: &str) -> Option<Member> {
        member(
            name,
            &self.lineage,
            &self.properties,
            &self.aggregates,
            &self.statics,
        )
    }

    /// The slot of static rand property `property` (see [`StaticSlot`]).
    pub(super) fn static_slot(&self, property: &StaticProperty) -> Option<usize> {
        self.static_slots
            .iter()
            .position(|slot| slot.variable == property.variable)
    }

    /// Register the class's constraint program once, from its constructor,
    /// for the objects of it that rand object handles reach: which classes
    /// those are is known only at run time (IEEE 1800-2023 18.4). A class
    /// with nothing random registers nothing; one the solver cannot take
    /// registers why, which faults only when such an object is reached.
    pub fn program_registration(&self) -> Option<Stmt> {
        use celox_heap::randomization::Registered;
        if !self.has_constraints
            && !self.properties.iter().any(|p| p.rand || p.randc)
            && self.static_slots.is_empty()
        {
            return None;
        }
        let program = match self
            .check_randomize()
            .and_then(|()| self.constraint_program())
        {
            // The static properties of the class are read and written by
            // its own randomize call (see `StaticInput`), which an object a
            // rand handle reaches does not make.
            Ok(program)
                if !self.static_slots.is_empty() || program.constraints.iter().any(reads_input) =>
            {
                Registered::Unsupported(
                    if self.constraint_calls.is_empty() {
                        "a rand object member whose constraints name static properties"
                    } else {
                        "a rand object member whose constraints call functions"
                    }
                    .into(),
                )
            }
            Ok(program) => Registered::Program(program.into()),
            Err(error) => Registered::Unsupported(error.to_string()),
        };
        Some(Stmt::If {
            condition: Expr::Binary {
                left: Box::new(object_call(
                    ObjectOp::ProgramRegistered(self.id),
                    Vec::new(),
                )),
                op: BinaryOp::Eq,
                right: Box::new(Expr::Literal("1'b0".into())),
            },
            then_body: std::iter::once(Stmt::Eval(object_call(
                ObjectOp::RegisterProgram(self.id, program),
                Vec::new(),
            )))
            // The heap keeps the modes of its static blocks, which every
            // constraint_mode call and randomize call of an object of the
            // class reads by the object's class (18.5.11).
            .chain((!self.static_constraints.is_empty()).then(|| {
                Stmt::Eval(object_call(
                    ObjectOp::RegisterStaticBlocks(self.id, self.static_constraints.clone()),
                    Vec::new(),
                ))
            }))
            .collect(),
            else_body: Vec::new(),
        })
    }

    /// Whether a rand object handle of the class makes `randomize` solve the
    /// objects it refers to (IEEE 1800-2023 18.4).
    pub fn has_rand_objects(&self) -> bool {
        self.properties.iter().any(Property::rand_object)
    }

    /// Whether `randomize` solves the class's constraint program: it has
    /// constraints, rand object handles or rand arrays. Otherwise each
    /// random property, and each static rand one in its slot (see
    /// `Class::static_slots`), is drawn on its own.
    pub fn solved(&self) -> bool {
        self.has_constraints
            || self.has_rand_objects()
            || self.properties.iter().any(Property::rand_array)
    }

    /// Validate the supported unconstrained subset before binding a call.
    pub fn check_randomize(&self) -> Converted<()> {
        if self.solved() {
            self.check_constraint_program()?;
        }
        for property in &self.properties {
            if property.randc && property.cyclic_domain_size().is_none() {
                return Err(unsupported(
                    "randc requires a 1-8 bit integer or an enum with at most 256 declared values",
                ));
            }
            if (property.rand || property.randc)
                && ((property.field.handle.is_some()
                    && !property.rand_object()
                    && !property.rand_array())
                    || property.r#type.holds_chandle()
                    || (property.r#type.is_enum && property.r#type.enum_domain.is_none()))
            {
                return Err(unsupported(
                    "class randomize requires integral rand properties with a supported enum domain",
                ));
            }
        }
        // A static rand property is checked as an instance one.
        for slot in &self.static_slots {
            if slot.field.handle.is_some()
                || slot.r#type.holds_chandle()
                || (slot.r#type.is_enum && slot.r#type.enum_domain.is_none())
            {
                return Err(unsupported(
                    "class randomize requires integral rand properties with a supported enum domain",
                ));
            }
        }
        for hook in ["pre_randomize", "post_randomize"] {
            if let Some(method) = self.method(hook) {
                if !method.is_function
                    || !method.formals.is_empty()
                    || method.qualifiers.is_static
                    || method.qualifiers.is_virtual
                {
                    return Err(unsupported(
                        "randomization hooks require non-static, non-virtual functions without arguments",
                    ));
                }
            }
        }
        Ok(())
    }

    /// The property `name`; one the class declares hides one of a base
    /// (IEEE 1800-2023 8.14).
    pub fn property(&self, name: &str) -> Option<&Property> {
        // The member the first name of a path denotes, a static property of
        // a derived class hiding an instance one of a base too (8.14).
        let root = name.split('.').next().unwrap_or(name);
        if matches!(self.member(root), Some(Member::Static(_))) {
            return None;
        }
        let (index, property) = self
            .properties
            .iter()
            .enumerate()
            .rev()
            .find(|(_, property)| property.name == name)?;
        (!self.hidden(name, index + 1, index)).then_some(property)
    }

    /// The property or member of unpacked structure type `name`.
    pub fn aggregate(&self, name: &str) -> Option<&AggregateProperty> {
        // The member the first name of a path denotes, a static property of
        // a derived class hiding one of a base too (8.14).
        let root = name.split('.').next().unwrap_or(name);
        if matches!(self.member(root), Some(Member::Static(_))) {
            return None;
        }
        let aggregate = self
            .aggregates
            .iter()
            .rev()
            .find(|aggregate| aggregate.name == name)?;
        (!self.hidden(name, aggregate.position, aggregate.position)).then_some(aggregate)
    }

    /// Whether a later declaration hides the property or member `name`: a
    /// property from position `properties` on, or a property of unpacked
    /// structure type after position `aggregates`, of that name or of a
    /// structure that contains it (`p` for `p.a`).
    fn hidden(&self, name: &str, properties: usize, aggregates: usize) -> bool {
        let paths = name
            .match_indices('.')
            .map(|(end, _)| &name[..end])
            .chain([name])
            .collect::<Vec<_>>();
        self.properties
            .iter()
            .enumerate()
            .any(|(index, property)| index >= properties && paths.contains(&property.name.as_str()))
            || self.aggregates.iter().any(|aggregate| {
                aggregate.position > aggregates && paths.contains(&aggregate.name.as_str())
            })
    }

    /// The static property `name` denotes in the class (see [`member`]):
    /// none when an instance property of a more derived class hides it.
    pub fn static_property(&self, name: &str) -> Option<&StaticProperty> {
        match self.member(name)? {
            Member::Static(index) => Some(&self.statics[index]),
            Member::Instance(_) | Member::Aggregate(_) => None,
        }
    }

    pub fn method(&self, name: &str) -> Option<&Method> {
        self.methods.iter().find(|method| method.name == name)
    }

    /// The properties the class itself declares.
    pub fn own_properties(&self) -> &[Property] {
        &self.properties[self.base_properties..]
    }

    /// The actuals that `arguments` of a constructor call pass to each
    /// formal of the constructor, by position or by name (IEEE 1800-2023
    /// 13.5.4), `None` for an omitted one, which takes its default; and the
    /// formals in the order their actuals are written, then the rest, which
    /// is the order they are evaluated in.
    pub fn constructor_arguments(
        &self,
        arguments: &[celox_sv_syntax::Argument],
    ) -> Converted<(Vec<Option<celox_sv_syntax::ExprId>>, Vec<usize>)> {
        let formals = self
            .method("new")
            .map_or_else(Vec::new, |method| method.formals.clone());
        expression_binding::formal_arguments(
            &Self::subroutine_name(&self.name, "new"),
            arguments,
            &formals,
        )
    }

    /// The subroutine name of `method` of `owner`, and the variable name of
    /// its static property `method`.
    pub fn subroutine_name(owner: &str, method: &str) -> String {
        format!("{owner}::{method}")
    }

    /// The subroutine that calls the override of virtual method `method`
    /// of `owner` for the class of its `this` (IEEE 1800-2023 8.20).
    pub fn dispatcher_name(owner: &str, method: &str) -> String {
        format!("{owner}::{method}$virtual")
    }

    /// The virtual methods the class itself declares, each with a
    /// dispatcher.
    pub fn dispatched_methods(&self) -> impl Iterator<Item = &Method> {
        self.methods
            .iter()
            .filter(|method| method.owner == self.name && method.is_virtual)
    }

    /// Whether the class is `ancestor`, derives from it or implements it,
    /// so that a handle of the class may be assigned to a variable of
    /// `ancestor` (IEEE 1800-2023 8.16, 8.26.5).
    pub fn derives_from(
        &self,
        tree: &AnalysisContext<'_>,
        session: &mut Session,
        ancestor: &str,
        env: &ConstEnv,
        aliases: &TypeAliases,
    ) -> bool {
        let class = self.record_lineage(tree, session, env, aliases);
        let ancestor = handle_type_id(&mut session.types, ancestor);
        session.types.lineage(class).contains(&ancestor)
    }

    /// The classes that [`Self::derives_from`] is true of.
    fn lineage(
        &self,
        tree: &AnalysisContext<'_>,
        session: &mut Session,
        env: &ConstEnv,
        aliases: &TypeAliases,
    ) -> HashSet<String> {
        let class = self.record_lineage(tree, session, env, aliases);
        session
            .types
            .lineage(class)
            .into_iter()
            .filter_map(|class| expression_types::class_name(&session.types, class))
            .collect()
    }

    /// Records in the semantic types the supertypes of the class and of the
    /// classes it derives from or implements, as far as they resolve, and
    /// returns the type of a handle of the class.
    pub fn record_lineage(
        &self,
        tree: &AnalysisContext<'_>,
        session: &mut Session,
        env: &ConstEnv,
        aliases: &TypeAliases,
    ) -> semantic_types::TyId {
        let class = handle_type_id(&mut session.types, &self.name);
        if session.types.supertypes(class).is_none() {
            let supertypes = self.supertype_ids(&mut session.types);
            session.types.record_supertypes(class, supertypes);
        }
        // A class that does not resolve is retried by the next query, as its
        // own supertypes stay unknown.
        let mut tried = HashSet::default();
        loop {
            let missing = session
                .types
                .lineage(class)
                .into_iter()
                .filter(|&ancestor| {
                    session.types.supertypes(ancestor).is_none() && tried.insert(ancestor)
                })
                .collect::<Vec<_>>();
            if missing.is_empty() {
                return class;
            }
            for ancestor in missing {
                let Some(name) = expression_types::class_name(&session.types, ancestor) else {
                    continue;
                };
                if let Ok(resolved) = self::class(tree, session, &name, env, aliases) {
                    let supertypes = resolved.supertype_ids(&mut session.types);
                    session.types.record_supertypes(ancestor, supertypes);
                }
            }
        }
    }

    /// The types of handles of the base class and the interface classes.
    fn supertype_ids(&self, table: &mut semantic_types::TyTable) -> semantic_types::Supertypes {
        semantic_types::Supertypes {
            base: self.base.as_deref().map(|name| handle_type_id(table, name)),
            interfaces: self
                .interfaces
                .iter()
                .map(|name| handle_type_id(table, name))
                .collect(),
            interface: self.is_interface,
        }
    }

    /// The subroutine that `$cast` to a variable of `class` calls (IEEE
    /// 1800-2023 8.16).
    pub fn cast_name(class: &str) -> String {
        format!("{class}::$cast")
    }

    /// An allocation of an uninitialized object of the class.
    pub fn allocation(&self) -> Expr {
        let handles = self
            .properties
            .iter()
            .filter_map(|property| {
                property
                    .field
                    .handle
                    .map(|handle| (property.field.offset, handle))
            })
            .collect();
        object_call(
            ObjectOp::New {
                class: self.id,
                size: self.size,
                handles,
            },
            Vec::new(),
        )
    }
}

/// The declaration of class `name`, or of the class it specializes, if one
/// exists.
pub(super) fn declaration<'a>(
    tree: &'a AnalysisContext<'_>,
    session: &mut Session,
    name: &str,
) -> Option<&'a celox_sv_syntax::ClassDeclaration> {
    let name = specializations::generic(session, name);
    let classes = &tree.declarations().classes;
    let index = &tree.spans().classes;
    // The name of a `$unit` class tells the compilation path too, which the
    // file's index does not know.
    if let Some(identifier) = unit_class_identifier(&name) {
        return index
            .unit
            .get(identifier)
            .map(|index| &classes[*index])
            .filter(|class| declared_name(tree, class) == name);
    }
    index.by_name.get(&name).map(|index| &classes[*index])
}

/// Whether class declaration `declaration` of `native` declares a
/// covergroup `member` (IEEE 1800-2023 19.4).
pub(super) fn declares_covergroup(
    native: &celox_sv_syntax::Declarations,
    declaration: &celox_sv_syntax::ClassDeclaration,
    member: &str,
) -> bool {
    let within = declaration.tokens.range();
    native.covergroups.iter().any(|covergroup| {
        within.contains(&covergroup.tokens.start)
            && instances::normalize_identifier(&covergroup.name.spelling) == member
    })
}

/// The class declarations of a file, the first of each name, indexed when
/// its arenas are built: by [`declared_name`], and those of the compilation
/// unit, whose declared names tell the compilation path too, by their
/// identifiers.
#[derive(Debug)]
pub(crate) struct ClassIndex {
    by_name: HashMap<String, usize>,
    unit: HashMap<String, usize>,
}

impl ClassIndex {
    pub(crate) fn new(native: &celox_sv_syntax::Declarations) -> Self {
        let mut by_name = HashMap::default();
        let mut unit = HashMap::default();
        for (index, class) in native.classes.iter().enumerate().rev() {
            let name = instances::normalize_identifier(&class.name.spelling);
            match enclosing_unit(native, class.tokens) {
                Some(enclosing) if enclosing.kind == celox_sv_syntax::UnitKind::Package => {
                    let package = instances::normalize_identifier(&enclosing.name.spelling);
                    by_name.insert(format!("{package}::{name}"), index);
                }
                Some(_) => {
                    by_name.insert(name, index);
                }
                None => {
                    unit.insert(name, index);
                }
            }
        }
        Self { by_name, unit }
    }
}

/// The name of class declaration `declaration`: qualified by the package
/// that declares it, or by `$unit` when it is declared in the compilation
/// unit outside any module, interface, program or package, so that each
/// names one class of the design (IEEE 1800-2023 3.12.1, 26.3).
pub(super) fn declared_name(
    tree: &AnalysisContext<'_>,
    declaration: &celox_sv_syntax::ClassDeclaration,
) -> String {
    let name = instances::normalize_identifier(&declaration.name.spelling);
    match enclosing_unit(tree.declarations(), declaration.tokens) {
        Some(unit) if unit.kind == celox_sv_syntax::UnitKind::Package => format!(
            "{}::{name}",
            instances::normalize_identifier(&unit.name.spelling)
        ),
        Some(_) => name,
        None => packages::unit_item_identity(tree.compilation_path(), &name),
    }
}

/// The identifier that names class `name` of the compilation unit, which
/// [`declared_name`] made: `C` of `$unit::C@<hash>`.
pub(super) fn unit_class_identifier(name: &str) -> Option<&str> {
    let (identifier, hash) = name
        .strip_prefix(packages::UNIT)?
        .strip_prefix("::")?
        .rsplit_once('@')?;
    (hash.len() == 16 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some(identifier)
}

/// The class declarations of the current scope, not those of the scopes
/// nested in it.
pub(super) fn scope_declarations<'a>(
    tree: &'a AnalysisContext<'_>,
) -> impl Iterator<Item = &'a celox_sv_syntax::ClassDeclaration> {
    let native = tree.declarations();
    let scope = tree.root_unit().ok().map(|unit| unit.tokens);
    native
        .classes
        .iter()
        .filter(move |class| enclosing_unit(native, class.tokens).map(|unit| unit.tokens) == scope)
}

/// The classes the current scope declares, as they have been laid out, for
/// the scopes that use it (see `scope::ScopeSymbols::classes`).
pub(super) fn scope_classes(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &ConstEnv,
) -> Vec<Class> {
    let names = scope_declarations(tree)
        .flat_map(|declaration| specializations_of(tree, session, declaration))
        .collect::<Vec<_>>();
    names
        .into_iter()
        .filter_map(|name| {
            let mut class = Class::clone(&*laid_out(session, &name)?);
            // The types are fixed in the scope that declares the class,
            // whose names another scope does not see.
            let (env, _) = specializations::scope(session, &name, env, &TypeAliases::default());
            let resolve = |ty: &mut Type| {
                if let Some(resolved) = type_parameters::resolve(&tree.functions, ty.clone(), &env)
                {
                    *ty = resolved;
                }
            };
            for property in &mut class.properties {
                resolve(&mut property.r#type);
            }
            for property in &mut class.statics {
                resolve(&mut property.r#type);
            }
            Some(class)
        })
        .collect()
}

/// The classes the current scope declares as types of the scope, by the
/// names it declares them with. A parameterized class is a type only with
/// a specialization.
pub(super) fn scope_class_types(tree: &AnalysisContext<'_>) -> Vec<(String, Type)> {
    scope_declarations(tree)
        .filter(|declaration| declaration.parameter_ports.is_none())
        .map(|declaration| {
            (
                instances::normalize_identifier(&declaration.name.spelling),
                handle(&declared_name(tree, declaration)),
            )
        })
        .collect()
}

/// The class that the unqualified name `name` refers to where the current
/// scope sees it: one the scope declares, or else one the compilation unit
/// declares (IEEE 1800-2023 3.12.1). Classes of packages are seen through
/// imports, as types.
pub(super) fn resolve(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    name: &str,
) -> Option<String> {
    if session.foreign.contains(name) {
        return Some(name.to_owned());
    }
    let native = tree.declarations();
    let scope = tree.root_unit().ok().map(|unit| unit.tokens);
    let mut unit_class = None;
    let package_scope = tree
        .root_unit()
        .is_ok_and(|unit| unit.kind == celox_sv_syntax::UnitKind::Package);
    for class in &native.classes {
        if instances::normalize_identifier(&class.name.spelling) != name {
            continue;
        }
        let enclosing = enclosing_unit(native, class.tokens).map(|unit| unit.tokens);
        if enclosing == scope {
            return Some(declared_name(tree, class));
        }
        if enclosing.is_none() && !package_scope {
            unit_class = Some(declared_name(tree, class));
        }
    }
    // A class an import makes visible (IEEE 1800-2023 26.3).
    let imported = scope::imported(tree);
    if let Some(alias) = imported.aliases.get(name) {
        let class = imported
            .type_aliases
            .get(alias)
            .and_then(Type::class)
            .unwrap_or(alias);
        if imported.classes.iter().any(|known| known.name == class)
            || session.foreign.contains(class)
        {
            return Some(class.to_owned());
        }
    }
    unit_class
        .or_else(|| {
            if package_scope {
                return None;
            }
            let alias = format!("{}::{name}", packages::UNIT);
            imported
                .type_aliases
                .get(&alias)
                .and_then(Type::class)
                .map(str::to_owned)
        })
        .or_else(|| match name {
            "process" => Some(super::processes::CLASS.to_owned()),
            "semaphore" => Some(super::semaphores::CLASS.to_owned()),
            _ => None,
        })
}

/// Whether class `name` is declared in the source of the current scope.
pub(super) fn is_declared(tree: &AnalysisContext<'_>, session: &mut Session, name: &str) -> bool {
    declaration(tree, session, name).is_some()
}

/// The innermost module, interface, program or package of `native` whose
/// declaration contains `tokens`.
pub(super) fn enclosing_unit(
    native: &celox_sv_syntax::Declarations,
    tokens: celox_sv_syntax::TokenRange,
) -> Option<&celox_sv_syntax::Unit> {
    native
        .all_units()
        .filter(|unit| unit.tokens.start <= tokens.start && tokens.end <= unit.tokens.end)
        .min_by_key(|unit| unit.tokens.end - unit.tokens.start)
}

/// Whether `name` is an interface class (IEEE 1800-2023 8.26).
pub(super) fn is_interface(tree: &AnalysisContext<'_>, session: &mut Session, name: &str) -> bool {
    declaration(tree, session, name).is_some_and(|declaration| declaration.interface)
}

/// Whether `name` is a class declared with a parameter port list (IEEE
/// 1800-2023 8.25).
pub(super) fn is_parameterized(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    name: &str,
) -> bool {
    declaration(tree, session, name).is_some_and(|class| class.parameter_ports.is_some())
        || session.foreign.contains(name)
}

/// The classes and interface classes that body item `item` declares: of a
/// parameterized class, the specializations made of it (IEEE 1800-2023
/// 8.25).
pub(super) fn item_classes(
    item: celox_sv_syntax::BodyItemId,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
) -> Vec<String> {
    let native = tree.declarations();
    let item = native.body_item(item);
    if !matches!(&item.kind, celox_sv_syntax::BodyItemKind::Unconverted { category, .. } if category == "class")
    {
        return Vec::new();
    }
    native
        .classes
        .iter()
        .filter(|class| {
            item.tokens.start <= class.tokens.start && class.tokens.end <= item.tokens.end
        })
        .flat_map(|class| specializations_of(tree, session, class))
        .collect()
}

/// The specializations of class declaration `declaration`: the class itself
/// when it has no parameter port list.
pub(super) fn specializations_of(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    declaration: &celox_sv_syntax::ClassDeclaration,
) -> Vec<String> {
    let name = declared_name(tree, declaration);
    if declaration.parameter_ports.is_some() {
        specializations::of(session, &name)
    } else {
        vec![name]
    }
}

/// The methods that class `name` implements, with their definitions: the
/// method itself or, for an `extern` method, its out-of-block definition.
/// A pure virtual method has none (IEEE 1800-2023 8.21). Each comes with
/// whether it is static.
pub(super) fn declared_methods(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    name: &str,
) -> Converted<Vec<(celox_sv_syntax::SubroutineId, bool)>> {
    let Some(declaration) = declaration(tree, session, name) else {
        return Ok(Vec::new());
    };
    let native = tree.declarations();
    declaration
        .methods
        .iter()
        .filter(|method| !method.qualifiers.is_pure)
        .map(|method| {
            let subroutine = native.subroutine(method.subroutine);
            if !subroutine.prototype {
                return Ok((method.subroutine, method.qualifiers.is_static));
            }
            let method_name = instances::normalize_identifier(&subroutine.name.spelling);
            out_of_block_definition(tree, declaration, &method_name)
                .map(|id| (id, method.qualifiers.is_static))
                .ok_or_else(|| {
                    unsupported(format!(
                        "extern method `{name}::{method_name}` without a body"
                    ))
                })
        })
        .collect()
}

/// The static properties that the classes of body item `item` declare,
/// which are variables of the module (IEEE 1800-2023 8.9). A class that
/// cannot be laid out declares none here; its uses report why.
pub(super) fn item_statics(
    item: celox_sv_syntax::BodyItemId,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &ConstEnv,
    aliases: &TypeAliases,
) -> Vec<StaticProperty> {
    item_classes(item, tree, session)
        .into_iter()
        .filter_map(|name| {
            let class = class(tree, session, &name, env, aliases).ok()?;
            Some(
                class
                    .statics
                    .iter()
                    .filter(|property| property.owner == name)
                    .cloned()
                    .collect::<Vec<_>>(),
            )
        })
        .flatten()
        .collect()
}

/// The arguments that the `extends` clause of class `name` passes to the
/// constructor of its base (IEEE 1800-2023 8.17).
pub(super) fn base_arguments<'a>(
    tree: &'a AnalysisContext<'_>,
    session: &mut Session,
    name: &str,
) -> Option<&'a [celox_sv_syntax::Argument]> {
    declaration(tree, session, name)?
        .base
        .as_ref()?
        .arguments
        .as_deref()
}

/// Whether class `name` is a built-in class whose objects the scheduler owns:
/// `process` (IEEE 1800-2023 9.7), `semaphore` (15.3), `mailbox` (15.4) or
/// the type of named events (15.5).
pub(super) fn builtin(name: &str) -> bool {
    matches!(
        name,
        super::processes::CLASS | super::events::CLASS | super::semaphores::CLASS
    ) || super::mailboxes::is_class(name)
}

/// The semantic type of a handle of class `name`.
pub(super) fn handle_type_id(
    table: &mut semantic_types::TyTable,
    name: &str,
) -> semantic_types::TyId {
    table.intern(semantic_types::TyData::Class(semantic_types::Decl::Class(
        name.into(),
    )))
}

/// The handle type of class `name`, if it is a class.
pub(super) fn handle_type(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    name: &str,
) -> Option<Type> {
    (builtin(name) || declaration(tree, session, name).is_some() || imported(tree, name).is_some())
        .then(|| {
            let mut ty = handle(name);
            semantic_types::bind_declared(
                &mut session.types,
                &tree.functions,
                &mut ty,
                &ConstEnv::default(),
            );
            ty
        })
}

/// Class `name` as the scope that declares it laid it out, when the current
/// scope uses that scope (see `scope::ScopeSymbols::classes`).
fn imported(tree: &AnalysisContext<'_>, name: &str) -> Option<Rc<Class>> {
    scope::imported(tree)
        .classes
        .iter()
        .find(|class| class.name == name)
        .map(|class| Rc::new((**class).clone()))
}

/// The classes whose objects the current scope may see: those it declares
/// and those of the scopes it uses.
pub(super) fn visible_classes(tree: &AnalysisContext<'_>, session: &mut Session) -> Vec<String> {
    let mut names = scope_declarations(tree)
        .flat_map(|declaration| specializations_of(tree, session, declaration))
        .collect::<Vec<_>>();
    for class in &scope::imported(tree).classes {
        if !names.contains(&class.name) {
            names.push(class.name.clone());
        }
    }
    names
}

/// The handle type of class `name`.
pub(super) fn handle(name: &str) -> Type {
    let mut ty = Type::new(
        // A semaphore is a heap object, which collection reclaims.
        if name == super::processes::CLASS {
            TypeKind::Process
        } else if name == super::events::CLASS {
            TypeKind::Event
        } else {
            TypeKind::Object
        },
    );
    ty.packed_ranges.push(PackedRange::new(
        ConstExpr::Literal((HANDLE_WIDTH - 1).to_string()),
        ConstExpr::Literal("0".into()),
    ));
    ty.class = Some(Arc::from(name));
    ty
}

/// Class `name` as it was laid out, unless the scope that declares it has
/// recorded other tables since: a layout in tables the scope has replaced,
/// such as those before its constants were all known, is not served again.
fn laid_out(session: &Session, name: &str) -> Option<Rc<Class>> {
    let (class, recording) = session.classes.get(name)?;
    (*recording == specializations::declaring_recording(session, name)).then(|| class.clone())
}

/// Keep `class`, laid out as class `name`, with the recording of the tables
/// of its declaring scope it was laid out in.
fn keep_layout(session: &mut Session, name: &str, class: Rc<Class>) {
    let recording = specializations::declaring_recording(session, name);
    session.classes.insert(name.to_owned(), (class, recording));
}

/// The class `name`, laid out with its bases.
pub(super) fn class(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    name: &str,
    env: &ConstEnv,
    aliases: &TypeAliases,
) -> Converted<Rc<Class>> {
    if let Some(class) = laid_out(session, name) {
        return Ok(class);
    }
    shared_class(
        tree,
        name,
        &SharedMap::from(env.clone()),
        &SharedMap::from(aliases.clone()),
        session,
    )
}

/// [`class`] in shared tables, which the scope of the class and of its
/// bases share instead of copying.
pub(super) fn shared_class(
    tree: &AnalysisContext<'_>,
    name: &str,
    env: &SharedEnv,
    aliases: &SharedAliases,
    session: &mut Session,
) -> Converted<Rc<Class>> {
    if let Some(class) = laid_out(session, name) {
        return Ok(class);
    }
    // A class of another scope is laid out in its own scope, where the
    // names it uses are declared.
    let declaration = declaration(tree, session, name).filter(|declaration| {
        let native = tree.declarations();
        enclosing_unit(native, declaration.tokens).map(|unit| unit.tokens)
            == tree.root_unit().ok().map(|unit| unit.tokens)
    });
    let Some(declaration) = declaration else {
        let class = imported(tree, name).ok_or_else(|| unsupported(format!("class `{name}`")))?;
        keep_layout(session, name, class.clone());
        return Ok(class);
    };
    // It is laid out in the tables of the scope that declares it, not in
    // those of the scope that first names it, such as the method of another
    // class with names of its own (IEEE 1800-2023 8.13, 8.25).
    let declaring = specializations::declaring_tables(session, name);
    let (env, aliases) = declaring
        .as_ref()
        .map_or((env, aliases), |(env, aliases)| (env, aliases));
    let (mut scope_env, mut scope_aliases) =
        specializations::shared_scope(session, name, env, aliases);
    // A class no specialization records declares its typedefs and
    // parameters in its body, which hide a type or constant of that name in
    // the tables it is laid out in, such as one of a class whose method
    // names it (IEEE 1800-2023 8.13, 8.25).
    if specializations::lookup(session, name).is_none() {
        let native = tree.declarations();
        for &id in &declaration.type_aliases {
            let own = instances::normalize_identifier(&native.type_alias(id).name.spelling);
            if scope_aliases.contains_key(&own) {
                scope_aliases.remove(&own);
            }
        }
        for &id in &declaration.parameters {
            for parameter in &native.parameter_declaration(id).parameters {
                let own = instances::normalize_identifier(&parameter.name.spelling);
                if matches!(parameter.kind, celox_sv_syntax::ParameterKind::Type { .. }) {
                    if scope_aliases.contains_key(&own) {
                        scope_aliases.remove(&own);
                    }
                } else if scope_env.contains_key(&own) {
                    scope_env.remove(&own);
                }
            }
        }
    }
    // The types of the class are bound in one context of its scope, which
    // the declarations of the scope around it are discovered without.
    let outer = PackedDimensions {
        const_env: env.clone(),
        type_aliases: aliases.clone(),
        ..PackedDimensions::default()
    };
    let context = PackedDimensions {
        const_env: scope_env.clone(),
        type_aliases: scope_aliases.clone(),
        class_base: Some(specializations::class_base(session, name, &outer)),
        // The property types name the types visible in the class body.
        type_declarations: specializations::lookup(session, name)
            .map(|specialization| specialization.type_declarations.clone()),
        ..PackedDimensions::default()
    };
    let (env, aliases) = (&*scope_env, &*scope_aliases);
    let native = tree.declarations();
    // The specialization of an interface class named the interface classes
    // it extends where its header is, as a class names its base below.
    let recorded = declaration
        .interface
        .then(|| specializations::lookup(session, name))
        .flatten()
        .map(|specialization| specialization.bases.clone())
        .filter(|bases| {
            bases.len() == declaration.interfaces.len()
                && bases.iter().all(|base| {
                    self::declaration(tree, session, base)
                        .is_some_and(|declaration| declaration.interface)
                })
        });
    let interfaces = match recorded {
        Some(bases) => bases,
        None => declaration
            .interfaces
            .iter()
            .map(|&interface| interface_name(tree, session, interface, env, aliases))
            .collect::<Converted<Vec<_>>>()?,
    };
    if declaration.interface {
        return interface_class(tree, session, name, declaration, interfaces, env, &context);
    }
    let base = match &declaration.base {
        None => None,
        Some(base) => {
            // The specialization named its base where the class header is,
            // before the declarations it inherits could hide those names.
            let recorded = specializations::lookup(session, name)
                .and_then(|specialization| specialization.bases.first().cloned());
            let ty = match recorded {
                Some(ty) => ty,
                None => {
                    type_binding::binding::bind_in(base.data_type, native, tree, session, &context)?
                        .and_then(|ty| ty.class().map(str::to_owned))
                        .ok_or_else(|| unsupported("base class type"))?
                }
            };
            // The base is declared outside the class, so the names the
            // class declares do not reach it.
            Some(shared_class(
                tree,
                &ty,
                &outer.const_env,
                &outer.type_aliases,
                session,
            )?)
        }
    };
    let mut properties = base
        .as_ref()
        .map(|base| base.properties.clone())
        .unwrap_or_default();
    let base_properties = properties.len();
    let mut aggregates = base
        .as_ref()
        .map(|base| base.aggregates.clone())
        .unwrap_or_default();
    let mut statics = base
        .as_ref()
        .map(|base| base.statics.clone())
        .unwrap_or_default();
    let mut static_slots = base
        .as_ref()
        .map(|base| base.static_slots.clone())
        .unwrap_or_default();
    let lineage = std::iter::once(name.to_owned())
        .chain(base.iter().flat_map(|base| base.lineage.iter().cloned()))
        .collect::<Vec<_>>();
    let mut size = base.as_ref().map_or(0, |base| base.size);
    for property in &declaration.properties {
        if property.declaration.constant && (property.qualifiers.rand || property.qualifiers.randc)
        {
            return Err(unsupported("const random class property"));
        }
        if property.qualifiers.is_static && property.qualifiers.randc {
            return Err(unsupported("randc static class property"));
        }
        let ty = type_binding::binding::bind_in(
            property.declaration.data_type,
            native,
            tree,
            session,
            &context,
        )?
        .ok_or_else(|| unsupported("class property type"))?;
        for variable in &property.declaration.variables {
            let ty = containers::with_dimensions(
                ty.clone(),
                &variable.dimensions,
                tree,
                session,
                &context,
            )?;
            // A fixed-size unpacked array is held in a container of its
            // elements (see `Property::storage`).
            let storage = if ty.unpacked_ranges.is_empty() {
                None
            } else {
                if property.qualifiers.is_static {
                    return Err(unsupported(
                        "static class property with unpacked dimensions",
                    ));
                }
                // A rand one is a random array of its size (18.4); a
                // cyclic one would cycle each element.
                if property.qualifiers.randc {
                    return Err(unsupported("randc fixed-size array class property"));
                }
                // Its element type keeps its meaning outside the class too.
                Some(containers::fixed_storage(
                    &procedural::scoped_type(ty.clone(), env),
                    tree,
                    &context,
                )?)
            };
            if ty.container.is_some() && property.declaration.constant {
                return Err(unsupported("const dynamic container class property"));
            }
            let property_name = instances::normalize_identifier(&variable.name.spelling);
            if let Some(aggregate) = ty.unpacked_struct().filter(|_| storage.is_none()) {
                if property.qualifiers.is_static {
                    return Err(unsupported(
                        "static class property of unpacked structure type",
                    ));
                }
                if property.qualifiers.rand || property.qualifiers.randc {
                    return Err(unsupported(
                        "random class property of unpacked structure type",
                    ));
                }
                if property.declaration.constant {
                    return Err(unsupported(
                        "const class property of unpacked structure type",
                    ));
                }
                for (aggregate_name, aggregate_type) in
                    unpacked_structs::aggregates(&property_name, &ty)
                {
                    aggregates.push(AggregateProperty {
                        initializer: (aggregate_name == property_name)
                            .then_some(variable.value)
                            .flatten(),
                        name: aggregate_name,
                        owner: name.to_owned(),
                        local: property.qualifiers.local,
                        protected: property.qualifiers.protected,
                        r#type: aggregate_type,
                        position: properties.len(),
                    });
                }
                for leaf in aggregate.leaves() {
                    let mut field = field(&tree.functions, &leaf.r#type, env, size)?;
                    size = field.offset + field.storage();
                    let leaf_name = leaf.name(&property_name);
                    field.key = crate::system_functions::ChangeKey::Property(property_key(
                        name, &leaf_name,
                    ));
                    properties.push(Property {
                        name: leaf_name,
                        owner: name.to_owned(),
                        constant: false,
                        local: property.qualifiers.local,
                        protected: property.qualifiers.protected,
                        r#type: procedural::scoped_type(leaf.r#type, env),
                        field,
                        rand: false,
                        randc: false,
                        initializer: None,
                        storage: None,
                    });
                }
                continue;
            }
            if property.qualifiers.is_static {
                let rand = property.qualifiers.rand;
                let variable_name = Class::subroutine_name(name, &property_name);
                statics.retain(|known| known.mode_of.is_some() || known.name != property_name);
                statics.push(StaticProperty {
                    local: property.qualifiers.local,
                    protected: property.qualifiers.protected,
                    variable: variable_name.clone(),
                    name: property_name.clone(),
                    owner: name.to_owned(),
                    r#type: ty.clone(),
                    initializer: variable.value,
                    rand,
                    mode_of: None,
                });
                if rand {
                    // The bit and the field are the class's own: a derived
                    // class that redeclares the property solves both.
                    let mode = static_rand_mode_variable(name, &property_name);
                    statics.push(StaticProperty {
                        local: true,
                        protected: false,
                        variable: mode.clone(),
                        name: static_rand_mode(&property_name),
                        owner: name.to_owned(),
                        r#type: unsigned_type(1),
                        initializer: None,
                        rand: false,
                        mode_of: Some(property_name.clone()),
                    });
                    // The slot is laid out as an instance property of the
                    // type is, and named by no member (see `StaticSlot`).
                    let mut field = field(&tree.functions, &ty, env, size)?;
                    size = field.offset + field.storage();
                    field.key = crate::system_functions::ChangeKey::Property(property_key(
                        name,
                        &static_rand_slot(&property_name),
                    ));
                    static_slots.push(StaticSlot {
                        variable: variable_name,
                        mode,
                        r#type: procedural::scoped_type(ty.clone(), env),
                        field,
                    });
                }
                continue;
            }
            let owner = name.to_owned();
            let name = property_name;
            let layout_type = storage
                .as_ref()
                .map_or_else(|| ty.clone(), |storage| containers::of((**storage).clone()));
            let mut field = field(&tree.functions, &layout_type, env, size)?;
            size = field.offset + field.storage();
            field.key = crate::system_functions::ChangeKey::Property(property_key(&owner, &name));
            properties.push(Property {
                name,
                owner,
                constant: property.declaration.constant,
                local: property.qualifiers.local,
                protected: property.qualifiers.protected,
                // Its ranges keep their meaning outside the class, which
                // may name its parameters.
                r#type: procedural::scoped_type(ty.clone(), env),
                field,
                rand: property.qualifiers.rand,
                randc: property.qualifiers.randc,
                initializer: variable.value,
                storage,
            });
        }
    }
    // A constructor is not inherited: a class without one has an implicit
    // constructor (IEEE 1800-2023 8.7).
    let mut methods = base
        .as_ref()
        .map(|base| base.methods.clone())
        .unwrap_or_default();
    methods.retain(|method| method.name != "new");
    for method in &declaration.methods {
        let subroutine = native.subroutine(method.subroutine);
        let method_name = instances::normalize_identifier(&subroutine.name.spelling);
        if matches!(
            method_name.as_str(),
            "randomize"
                | "srandom"
                | "rand_mode"
                | "constraint_mode"
                | "get_randstate"
                | "set_randstate"
        ) {
            return Err(unsupported(
                "overriding built-in class randomization methods",
            ));
        }
        // A method overriding a virtual method is virtual (8.20).
        let is_virtual = method.qualifiers.is_virtual
            || methods
                .iter()
                .any(|known| known.name == method_name && known.is_virtual);
        let native_id = if method.qualifiers.is_pure {
            method.subroutine
        } else if subroutine.prototype {
            out_of_block_definition(tree, declaration, &method_name).ok_or_else(|| {
                unsupported(format!(
                    "extern method `{name}::{method_name}` without a body"
                ))
            })?
        } else {
            method.subroutine
        };
        methods.retain(|known| known.name != method_name);
        let (formals, result) = signature(tree, session, native_id, &context);
        methods.push(Method {
            name: method_name,
            owner: name.to_owned(),
            native: native_id,
            formals,
            is_function: native.subroutine(native_id).kind
                == celox_sv_syntax::SubroutineKind::Function,
            result,
            qualifiers: method.qualifiers,
            is_virtual,
        });
    }
    // A method of an implemented interface class is virtual, and the class
    // implements it with the same return type (8.26.2, 8.26.6.1). An
    // abstract class may instead re-declare it pure (8.26.7).
    let mut required = Vec::new();
    for interface in &interfaces {
        required.extend(
            class(tree, session, interface, env, aliases)?
                .methods
                .iter()
                .map(|method| (method.name.clone(), method.result.clone())),
        );
    }
    for (method_name, prototype) in required {
        match methods.iter_mut().find(|method| method.name == method_name) {
            Some(method) => {
                if !method.is_virtual && method.owner != name {
                    return Err(unsupported(format!(
                        "interface class method `{method_name}` implemented by a non-virtual method of a base class"
                    )));
                }
                let (Some(implemented), Some(prototype)) = (&method.result, &prototype) else {
                    return Err(unsupported(format!(
                        "return type of interface class method `{method_name}`"
                    )));
                };
                if implemented != prototype {
                    return Err(unsupported(format!(
                        "method `{method_name}` of class `{name}` whose return type differs from its interface class"
                    )));
                }
                method.is_virtual = true;
            }
            None => {
                return Err(unsupported(format!(
                    "class `{name}` that does not implement interface class method `{method_name}`"
                )));
            }
        }
    }
    // A class that is not abstract implements every pure virtual method
    // (8.21).
    if !declaration.virtual_class
        && let Some(method) = methods.iter().find(|method| method.is_pure())
    {
        return Err(unsupported(format!(
            "class `{name}` that does not implement pure virtual method `{}`",
            method.name
        )));
    }
    let mut constraints = base
        .as_ref()
        .map(|base| base.constraints.clone())
        .unwrap_or_default();
    // A constraint reads a static property as an input of the program, and
    // solves a static rand property in the object's slot for it (see
    // `StaticProperty::rand`).
    let static_inputs = static_inputs(
        name,
        base.as_ref().map(|base| base.static_inputs.as_slice()),
        &statics,
        tree,
        session,
        env,
    );
    let names = constraints::Names {
        lineage: &lineage,
        aggregates: &aggregates,
        statics: &statics,
        inputs: &static_inputs,
        slots: &static_slots,
    };
    let scope = (name.to_owned(), base.as_ref().map(|base| base.name.clone()));
    session.constraint_owners.push(scope.clone());
    session.constraint_scopes.push(scope);
    session.constraint_methods.push((
        methods.clone(),
        base.as_ref()
            .map(|base| base.constraint_calls.clone())
            .unwrap_or_default(),
        0,
    ));
    // The classes above this one, whose constraints are below its own
    // (IEEE 1800-2023 18.5.13.1).
    let depth = base.as_ref().map_or(0, |base| base.depth + 1);
    let mut static_constraints = base
        .as_ref()
        .map(|base| base.static_constraints.clone())
        .unwrap_or_default();
    for (position, block) in declaration.constraints.iter().enumerate() {
        let block_name = block.name.spelling.clone();
        // The index the block takes, which the calls it makes record; a
        // block it overrides makes none (IEEE 1800-2023 18.5.11).
        let block_index = constraints
            .iter()
            .position(|(name, _)| *name == block_name)
            .unwrap_or(constraints.len()) as u32;
        if let Some((_, calls, current)) = session.constraint_methods.last_mut() {
            *current = block_index;
            for call in calls.iter_mut().filter(|call| call.block == block_index) {
                call.live = false;
            }
        }

        let expressions = block
            .expressions
            .as_ref()
            .ok_or_else(|| "unsupported constraint block form".to_owned())
            .and_then(|expressions| {
                expressions
                    .iter()
                    .enumerate()
                    .map(|(index, &id)| {
                        constraints::bind_inputs(
                            id,
                            tree,
                            session,
                            &properties,
                            &names,
                            env,
                            &HashMap::default(),
                        )
                        .map(|constraint| {
                            constraints::prioritize(
                                constraint,
                                constraints::class_priority(depth, position, index),
                            )
                        })
                    })
                    .collect()
            });
        // Constraint indices survive overrides, including calls via base handles.
        let index = match constraints.iter().position(|(name, _)| *name == block_name) {
            Some(index) => {
                constraints[index].1 = expressions;
                index
            }
            None => {
                constraints.push((block_name.clone(), expressions));
                constraints.len() - 1
            }
        } as u32;
        // A static block has one constraint mode, which every object of
        // the class that declares it shares (IEEE 1800-2023 18.5.11); a
        // block that overrides it is the overriding class's.
        static_constraints.retain(|(known, _)| *known != index);
        if block.is_static {
            static_constraints.push((index, class_id(name)));
        }
    }
    session.constraint_owners.pop();
    session.constraint_scopes.pop();
    let (_, constraint_calls, _) = session.constraint_methods.pop().unwrap_or_default();
    let (property_bits, slot_bits) =
        Class::selection_bits(base.as_deref(), properties.len(), static_slots.len());
    let mut class = Class {
        name: name.to_owned(),
        id: class_id(name),
        program: Ok(Default::default()),
        constraints,
        has_constraints: declaration.has_constraints
            || base.as_ref().is_some_and(|base| base.has_constraints),
        base: base.map(|base| base.name.clone()),
        interfaces,
        is_abstract: declaration.virtual_class,
        is_interface: false,
        properties,
        base_properties,
        aggregates,
        statics,
        static_inputs,
        depth,
        static_slots,
        property_bits,
        slot_bits,
        lineage,
        static_constraints,
        constraint_calls,
        methods,
        declarations: Vec::new(),
        size: size.max(1),
    };
    class.program = class.build_constraint_program(tree, session, env);
    let class = Rc::new(class);
    keep_layout(session, name, class.clone());
    Ok(class)
}

/// The interface class that type `interface` of an `implements` or an
/// interface class's `extends` list names (IEEE 1800-2023 8.26).
fn interface_name(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    interface: celox_sv_syntax::TypeId,
    env: &ConstEnv,
    aliases: &TypeAliases,
) -> Converted<String> {
    let native = tree.declarations();
    let name = type_binding::procedural_type(interface, native, tree, session, env, aliases)?
        .and_then(|ty| ty.class().map(str::to_owned))
        .ok_or_else(|| unsupported("implemented interface class type"))?;
    // An interface class of another scope is known by its layout.
    let interface = match imported(tree, &name) {
        Some(class) => class.is_interface,
        None => declaration(tree, session, &name).is_some_and(|declaration| declaration.interface),
    };
    if !interface {
        return Err(unsupported(format!(
            "`implements` of `{name}`, not an interface class"
        )));
    }
    Ok(name)
}

/// Interface class `name`, whose methods are the pure virtual methods it
/// declares and those of the interface classes it extends (IEEE 1800-2023
/// 8.26). A handle of it refers to an object of a class implementing it.
fn interface_class(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    name: &str,
    declaration: &celox_sv_syntax::ClassDeclaration,
    interfaces: Vec<String>,
    env: &ConstEnv,
    context: &PackedDimensions,
) -> Converted<Rc<Class>> {
    let aliases = &*context.type_aliases;
    let native = tree.declarations();
    // The parameters and typedefs it declares hide inherited ones; those it
    // inherits from one interface class in several ways are one (8.26.6.3).
    let mut declarations = own_declarations(native, declaration)
        .into_iter()
        .map(|own| (own, name.to_owned()))
        .collect::<Vec<_>>();
    let own = declarations.len();
    let mut methods: Vec<Method> = Vec::new();
    for interface in &interfaces {
        let interface = class(tree, session, interface, env, aliases)?;
        for method in &interface.methods {
            methods.retain(|known| known.name != method.name);
            methods.push(method.clone());
        }
        for (inherited, origin) in &interface.declarations {
            if declarations[..own]
                .iter()
                .any(|(known, _)| known == inherited)
            {
                continue;
            }
            match declarations.iter().find(|(known, _)| known == inherited) {
                Some((_, first)) if first != origin => {
                    return Err(AnalyzerError::ConflictingInterfaceClassDeclaration {
                        class: name.to_owned(),
                        name: inherited.clone(),
                        first: first.clone(),
                        second: origin.clone(),
                    });
                }
                Some(_) => {}
                None => declarations.push((inherited.clone(), origin.clone())),
            }
        }
    }
    for method in &declaration.methods {
        let subroutine = native.subroutine(method.subroutine);
        // A method whose types cannot be resolved, such as one using a
        // type parameter or a typedef of the interface class, leaves the
        // interface class without a layout; it is lowered only when used.
        type_binding::subroutine_formals(subroutine, tree, session, context)?;
        return_type(tree, session, method.subroutine, context)?;
        let method_name = instances::normalize_identifier(&subroutine.name.spelling);
        methods.retain(|known| known.name != method_name);
        let (formals, result) = signature(tree, session, method.subroutine, context);
        methods.push(Method {
            name: method_name,
            owner: name.to_owned(),
            native: method.subroutine,
            formals,
            is_function: subroutine.kind == celox_sv_syntax::SubroutineKind::Function,
            result,
            qualifiers: method.qualifiers,
            is_virtual: true,
        });
    }
    let class = Rc::new(Class {
        name: name.to_owned(),
        id: class_id(name),
        base: None,
        interfaces,
        is_abstract: true,
        is_interface: true,
        has_constraints: false,
        constraints: Vec::new(),
        program: Ok(Default::default()),
        properties: Vec::new(),
        base_properties: 0,
        aggregates: Vec::new(),
        statics: Vec::new(),
        static_inputs: Vec::new(),
        depth: 0,
        static_slots: Vec::new(),
        property_bits: Vec::new(),
        slot_bits: Vec::new(),
        lineage: vec![name.to_owned()],
        static_constraints: Vec::new(),
        constraint_calls: Vec::new(),
        methods,
        declarations,
        size: 1,
    });
    keep_layout(session, name, class.clone());
    Ok(class)
}

/// The names of the parameters and typedefs that class `declaration`
/// declares.
fn own_declarations(
    native: &celox_sv_syntax::Declarations,
    declaration: &celox_sv_syntax::ClassDeclaration,
) -> Vec<String> {
    let parameters = declaration
        .parameter_ports
        .iter()
        .chain(&declaration.parameters)
        .flat_map(|&id| &native.parameter_declaration(id).parameters)
        .map(|parameter| &parameter.name);
    let aliases = declaration
        .type_aliases
        .iter()
        .map(|&id| &native.type_alias(id).name);
    parameters
        .chain(aliases)
        .map(|name| instances::normalize_identifier(&name.spelling))
        .collect()
}

/// The formal names and the return type of method `id`, see [`Method`].
fn signature(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    id: celox_sv_syntax::SubroutineId,
    context: &PackedDimensions,
) -> (Vec<Option<String>>, Option<Option<Type>>) {
    let formals = tree
        .declarations()
        .subroutine(id)
        .ports
        .iter()
        .flatten()
        .map(|port| match &port.name {
            Some(name) => Some(instances::normalize_identifier(&name.spelling)),
            None => prototype_formal_name(port, tree, session, context),
        })
        .collect();
    let result = return_type(tree, session, id, context).ok().map(|ty| {
        ty.and_then(|ty| type_parameters::resolve(&tree.functions, ty, &context.const_env))
    });
    (formals, result)
}

/// The name of formal `port` of a prototype that the parser read as an
/// unnamed formal of a named type: a lone identifier there is a type when
/// one of its name is visible, and otherwise the formal's name, of the type
/// of the formal before it, as in `bit is_int, is_bitstream` (IEEE 1800-2023
/// 13.4, A.2.7). `None` for a formal that names a type or its formal.
pub(super) fn prototype_formal_name(
    port: &celox_sv_syntax::SubroutinePort,
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    context: &PackedDimensions,
) -> Option<String> {
    if port.name.is_some() {
        return None;
    }
    let native = tree.declarations();
    let data = native.data_type(port.data_type);
    let celox_sv_syntax::DataTypeKind::Named(segments) = &data.kind else {
        return None;
    };
    let [segment] = segments.as_slice() else {
        return None;
    };
    if segment.arguments.is_some() || data.signed.is_some() || !data.packed_dimensions.is_empty() {
        return None;
    }
    match type_binding::binding::bind_in(port.data_type, native, tree, session, context) {
        Ok(Some(_)) => None,
        _ => Some(instances::normalize_identifier(&segment.name.spelling)),
    }
}

/// The return type of subroutine `id`, `None` for a task or `void`.
fn return_type(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    id: celox_sv_syntax::SubroutineId,
    context: &PackedDimensions,
) -> Converted<Option<Type>> {
    let native = tree.declarations();
    match native.subroutine(id).return_type {
        Some(ty) => type_binding::binding::bind_in(ty, native, tree, session, context),
        None => Ok(None),
    }
}

/// The definition `function ... class::method` outside class declaration
/// `declaration`, in the scope that declares the class (IEEE 1800-2023 8.24).
fn out_of_block_definition(
    tree: &AnalysisContext<'_>,
    declaration: &celox_sv_syntax::ClassDeclaration,
    method: &str,
) -> Option<celox_sv_syntax::SubroutineId> {
    let native = tree.declarations();
    let class = instances::normalize_identifier(&declaration.name.spelling);
    let scope = enclosing_unit(native, declaration.tokens).map(|unit| unit.tokens);
    native.subroutine_ids().find(|&id| {
        let subroutine = native.subroutine(id);
        !subroutine.prototype
            && instances::normalize_identifier(&subroutine.name.spelling) == method
            && matches!(&subroutine.scope[..], [name]
                if instances::normalize_identifier(&name.name.spelling) == class)
            && enclosing_unit(native, subroutine.tokens).map(|unit| unit.tokens) == scope
    })
}

/// The layout of a property of type `ty` at the first aligned offset at or
/// after `offset`.
pub(super) fn field(
    functions: &const_functions::ConstantFunctions,
    ty: &Type,
    env: &ConstEnv,
    offset: u32,
) -> Converted<ObjectField> {
    if ty.unpacked_struct().is_some() {
        return Err(unsupported("class property of unpacked structure type"));
    }
    let (width, signed, handle) = match ty.kind() {
        _ if ty.container.is_some() => (HANDLE_WIDTH, false, Some(HandleProperty::Object)),
        TypeKind::String => (HANDLE_WIDTH, false, Some(HandleProperty::String)),
        TypeKind::Object => (HANDLE_WIDTH, false, Some(HandleProperty::Object)),
        TypeKind::Process | TypeKind::Event => (HANDLE_WIDTH, false, None),
        TypeKind::Chandle
        | TypeKind::Bit
        | TypeKind::Logic
        | TypeKind::Reg
        | TypeKind::Implicit => {
            let shape = casts::expr_type_from_type(functions, ty, env)
                .ok_or_else(|| unsupported("class property type"))?;
            (shape.width, shape.signed, None)
        }
        TypeKind::Real => (64, false, None),
        TypeKind::Shortreal => (32, false, None),
    };
    Ok(ObjectField {
        offset: offset.next_multiple_of(8),
        width: width as u32,
        signed,
        handle,
        // A 4-state property keeps its X and Z bits (IEEE 1800-2023 6.3.1).
        four_state: handle.is_none()
            && matches!(
                ty.kind(),
                TypeKind::Logic | TypeKind::Reg | TypeKind::Implicit
            ),
        nullable: false,
        real: matches!(ty.kind(), TypeKind::Real | TypeKind::Shortreal),
        key: crate::system_functions::ChangeKey::None,
    })
}

/// The [`ObjectField::key`] of property `property` that class `owner`
/// declares: an FNV-1a hash, never 0.
fn property_key(owner: &str, property: &str) -> u32 {
    let hash = owner
        .bytes()
        .chain(std::iter::once(b'.'))
        .chain(property.bytes())
        .fold(0x811c_9dc5u32, |hash, byte| {
            (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
        });
    hash.max(1)
}

/// A run-time identifier of class `name`: an FNV-1a hash, never 0, which
/// `null` reads as, nor a class the heap reserves (see
/// `celox_heap::class_identifier`).
fn class_id(name: &str) -> u32 {
    // Compilation-unit declaration names already contain their source identity.
    let hash = name.bytes().fold(0x811c_9dc5u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193)
    });
    celox_heap::class_identifier(hash)
}

/// The classes whose objects a handle of class `ancestor` may refer to:
/// those deriving from it or implementing it that are not abstract (IEEE
/// 1800-2023 8.16), laid out.
fn object_classes(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    ancestor: &str,
    env: &ConstEnv,
    aliases: &TypeAliases,
) -> Converted<Vec<Rc<Class>>> {
    let concrete = ConcreteClasses::of(tree, session, env, aliases);
    let mut classes = Vec::new();
    let mut ids = HashMap::default();
    for (name, class, lineage) in &concrete.classes {
        if !lineage.contains(ancestor) {
            continue;
        }
        if let Some(other) = ids.insert(class.id, name) {
            return Err(unsupported(format!(
                "classes `{other}` and `{name}` with the same run-time identifier"
            )));
        }
        classes.push(class.clone());
    }
    Ok(classes)
}

/// The classes of [`visible_classes`] that can be laid out and are neither
/// abstract nor interface classes, each with the classes it is, derives from
/// or implements (see [`Class::derives_from`]). A scope keeps them while no
/// class is laid out or specialized, which is what could change them.
pub(super) struct ConcreteClasses {
    syntax_tree: *const SyntaxTree,
    imported: Arc<scope::ScopeSymbols>,
    generation: (usize, usize),
    classes: Vec<(String, Rc<Class>, HashSet<String>)>,
}

impl ConcreteClasses {
    fn of(
        tree: &AnalysisContext<'_>,
        session: &mut Session,
        env: &ConstEnv,
        aliases: &TypeAliases,
    ) -> Rc<Self> {
        let generation =
            |session: &Session| (session.classes.len(), session.class_specializations.len());
        if let Some(known) = &session.concrete_classes
            && std::ptr::eq(known.syntax_tree, tree.syntax_tree)
            && Arc::ptr_eq(&known.imported, &tree.imported)
            && known.generation == generation(session)
        {
            return known.clone();
        }
        let mut classes = Vec::new();
        for name in visible_classes(tree, session) {
            // A class that cannot be laid out has no objects.
            let Ok(class) = class(tree, session, &name, env, aliases) else {
                continue;
            };
            if class.is_abstract || class.is_interface {
                continue;
            }
            let lineage = class.lineage(tree, session, env, aliases);
            classes.push((name, class, lineage));
        }
        let concrete = Rc::new(Self {
            syntax_tree: tree.syntax_tree,
            imported: tree.imported.clone(),
            generation: generation(session),
            classes,
        });
        session.concrete_classes = Some(concrete.clone());
        concrete
    }
}

/// The classes of the objects the rand object handles of an object of
/// `class` may reach: those deriving from the class of each such handle,
/// and so on through their own (IEEE 1800-2023 18.4).
pub(super) fn rand_member_classes(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    class: &Class,
    env: &ConstEnv,
    aliases: &TypeAliases,
) -> Converted<Vec<Rc<Class>>> {
    let handle_classes = |class: &Class| {
        class
            .properties
            .iter()
            .filter(|property| property.rand_object())
            .filter_map(|property| property.r#type.class().map(str::to_owned))
            .collect::<Vec<_>>()
    };
    let mut pending = handle_classes(class);
    let mut seen = HashSet::default();
    let mut ids = HashSet::default();
    let mut classes = Vec::new();
    while let Some(name) = pending.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        for member in object_classes(tree, session, &name, env, aliases)? {
            if ids.insert(member.id) {
                pending.extend(handle_classes(&member));
                classes.push(member);
            }
        }
    }
    Ok(classes)
}

/// The implementations of randomize hook `method` (`pre_randomize` or
/// `post_randomize`) that the objects of each of `classes` run, by class id
/// (IEEE 1800-2023 18.6.2).
pub(super) fn hook_targets(classes: &[Rc<Class>], method: &str) -> Vec<(u32, String)> {
    classes
        .iter()
        .filter_map(|class| {
            class
                .method(method)
                .map(|hook| (class.id, Class::subroutine_name(&hook.owner, &hook.name)))
        })
        .collect()
}

/// The overrides that dispatcher `Class::dispatcher_name(owner, method)`
/// calls: each implementation of the method with the classes, deriving from
/// or implementing `owner` and not abstract, whose objects run it.
pub(super) fn dispatch_targets(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    owner: &str,
    method: &str,
    env: &ConstEnv,
    aliases: &TypeAliases,
) -> Converted<Vec<(String, Vec<u32>)>> {
    let mut targets: Vec<(String, Vec<u32>)> = Vec::new();
    for class in object_classes(tree, session, owner, env, aliases)? {
        let Some(implementation) = class.method(method) else {
            continue;
        };
        let target = Class::subroutine_name(&implementation.owner, method);
        match targets.iter_mut().find(|(known, _)| *known == target) {
            Some((_, classes)) => classes.push(class.id),
            None => targets.push((target, vec![class.id])),
        }
    }
    Ok(targets)
}

/// The run-time identifiers of the classes whose objects `$cast` may assign
/// to a variable of class `name` (IEEE 1800-2023 8.16).
pub(super) fn cast_targets(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    name: &str,
    env: &ConstEnv,
    aliases: &TypeAliases,
) -> Converted<Vec<u32>> {
    Ok(object_classes(tree, session, name, env, aliases)?
        .iter()
        .map(|class| class.id)
        .collect())
}

/// A call of object operation `op`.
pub(super) fn object_call(op: ObjectOp, args: Vec<Expr>) -> Expr {
    Expr::Call {
        callee: Callee::Object(Box::new(op)),
        args: args.into_iter().map(Some).collect(),
        arg_order: Vec::new(),
    }
}
