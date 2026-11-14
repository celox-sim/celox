//! Serializable scalar constraint programs shared by the SV compiler and heap.
//! The program carries no pointers or persistent solver state.
use crate::{Fault, Handle, Heap};
use aplite::{
    Env, Limits, Scalar, Sort, Term, Verdict, and, boolv, bv, eq, ite, node, not, solve_seeded, var,
};

#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Field {
    pub offset: u32,
    pub width: u32,
    pub random: bool,
    /// The values a random enumeration property may take (IEEE 1800-2023
    /// 18.3); empty for others.
    #[serde(default)]
    pub domain: Vec<u64>,
    /// A `randc` property, which cycles through the values its constraints
    /// allow before repeating one (IEEE 1800-2023 18.4.2).
    #[serde(default)]
    pub cyclic: bool,
    /// A 4-state property of a 4-state simulation, which holds the
    /// complement of its value bits and then of its unknown-bit mask, so
    /// that zeroed bytes read as X: the solver gives it a 2-state value, and
    /// a constraint takes its value only while it has no X or Z bits (IEEE
    /// 1800-2023 18.3).
    #[serde(default)]
    pub four_state: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Expr {
    /// Top-level named block predicate; inline predicates remain unconditional.
    Enabled {
        index: u32,
        value: Box<Expr>,
    },
    /// A soft constraint (IEEE 1800-2023 18.5.13), at the top of a
    /// program's constraint or of an [`Expr::Enabled`] one: the solver keeps
    /// it only when the hard constraints and the soft ones it kept of
    /// higher `priority` allow it, and otherwise treats it as true. The
    /// priorities of a program order its own soft constraints; those of an
    /// object the call solves through a rand handle are below them.
    Soft {
        priority: u64,
        value: Box<Expr>,
    },
    /// `disable soft` of random variable `variable`, an [`Expr::Field`] or
    /// [`Expr::Size`] (18.5.13.2): the soft constraints of lower `priority`
    /// in which the variable appears are discarded, or under a guard, while
    /// `condition` holds.
    DisableSoft {
        priority: u64,
        variable: Box<Expr>,
        #[serde(default)]
        condition: Option<Box<Expr>>,
    },
    Literal(u32, u64),
    Field(usize),
    /// The class handle at this byte offset of the object, which no
    /// randomization changes: a guard compares it, such as with `null`
    /// (IEEE 1800-2023 18.5.13), as an integer of [`crate::HANDLE_BYTES`].
    Handle(u32),
    /// The size of array `0` of the program, a 32-bit `int` (IEEE 1800-2023
    /// 7.5.2, 7.10.2.1): a solver variable while the array is random and a
    /// constraint names its size (18.4).
    Size(usize),
    /// `body` for each element of array `array` of the program, a state
    /// array whose elements are known when solving (IEEE 1800-2023
    /// 18.5.8.1): the conjunction of `body` for each, where
    /// [`Expr::Index`] and [`Expr::Element`] of the loop's `level` (its
    /// nesting, from 0 outermost) are the element's index and value.
    Foreach {
        array: usize,
        body: Box<Expr>,
    },
    /// The index of the element of loop `level`, a 32-bit `int`.
    Index {
        level: u32,
    },
    /// Static rand property `0` of the class whose constraint names it (IEEE
    /// 1800-2023 18.4), its static rand properties numbered from those of
    /// its bases. Building a program solves each in a field after those of
    /// the class's properties, which the program names instead: a program
    /// never holds it.
    Static(usize),
    /// Solver variable `0` of the call, among those of every object it
    /// solves: a member of a loop element that is one of them. Only the
    /// unrolling of a loop makes it; a program never holds it.
    Solved(usize),
    /// The element of loop `level`, or with `member`, the `width`-bit
    /// property at that byte offset of the object the element refers to.
    Element {
        level: u32,
        member: Option<u32>,
        width: u32,
    },
    /// The sum of the elements of array `0` of the program, at their width
    /// (IEEE 1800-2023 7.12.3, 18.5.8.2).
    Sum(usize),
    /// The `width`-bit property at byte offset `member` of the object the
    /// handle at byte offset `handle` of the object refers to (IEEE
    /// 1800-2023 18.5.9): its solver variable when the call solves that
    /// object, its value otherwise.
    Member {
        handle: u32,
        member: u32,
        width: u32,
    },
    /// Element `index` of array `array` of the program, of `width` bits,
    /// outside a loop over it (IEEE 1800-2023 18.4, 18.5.8): its solver
    /// variable while the array is random, else its value, and the default
    /// value 0 past the end of the array (7.4.6).
    ElementAt {
        array: usize,
        index: u64,
        width: u32,
    },
    /// Read-only scalar supplied by this call, never a solver variable.
    Input {
        index: usize,
        width: u32,
    },
    Resize {
        value: Box<Expr>,
        width: u32,
        signed: bool,
    },
    Unary {
        op: String,
        value: Box<Expr>,
    },
    Binary {
        op: String,
        left: Box<Expr>,
        right: Box<Expr>,
        signed: bool,
    },
    /// `value dist { ... }` (IEEE 1800-2023 18.5.3): true while `membership`
    /// is, the value being one of an item of nonzero weight. As a constraint
    /// of its own, the solver first tries one of `choices`, each an item's
    /// membership, picked by its weight.
    Dist {
        membership: Box<Expr>,
        choices: Vec<DistChoice>,
    },
}

/// An item of a distribution, by its membership and its weight: an
/// unsigned value (IEEE 1800-2023 18.5.3) of any size, as a `:=` range
/// weighs its weight times its size.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct DistChoice {
    pub predicate: Expr,
    pub weight: num_bigint::BigUint,
}

/// The priority of a soft constraint among those of a randomize call: the
/// rank of its object (see `Heap::handle_ranks`), then its priority in its
/// program (IEEE 1800-2023 18.5.13.1).
type Rank = (Vec<u32>, u64);

#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Program {
    pub fields: Vec<Field>,
    pub constraints: Vec<Expr>,
    /// The byte offsets of the object's rand class handles: the variables
    /// and constraints of the objects they refer to are solved with the
    /// object's (IEEE 1800-2023 18.4).
    #[serde(default)]
    pub handles: Vec<u32>,
    /// The object's dynamic arrays and queues of integral elements whose
    /// size a constraint may name, and which are solved when random.
    #[serde(default)]
    pub arrays: Vec<Array>,
    /// The arguments of the functions the constraints call, in the order of
    /// the calls and their arguments, each converted to its formal (IEEE
    /// 1800-2023 18.5.11). The random variables in them are solved first
    /// ([`Heap::solve_arguments`]); the values of the calls are inputs of
    /// the solve of the rest.
    #[serde(default)]
    pub arguments: Vec<Expr>,
    /// The inputs that hold 4-state values, each with the input that holds
    /// the mask of its X and Z bits, which are 0 in the value: a constraint
    /// may read only its other bits (IEEE 1800-2023 18.3).
    #[serde(default)]
    pub unknown_inputs: Vec<(usize, usize)>,
}

/// The solution of a solve of arguments (see [`Heap::solve_arguments`]):
/// the object, the values of the random variables in the arguments by
/// solver variable, and the value of each argument.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Arguments {
    object: Handle,
    variables: Vec<(String, u64)>,
    values: Vec<u64>,
}

/// What a solve takes: every constraint, or only those that name no
/// variable but the random variables in the arguments of the functions the
/// constraints call (IEEE 1800-2023 18.5.11), or every constraint but those
/// on the elements of the random arrays whose size a constraint names too
/// and those that read a value of a call, whose sizes are solved first
/// (18.5.7.1). A call orders them so: its sizes, its arguments, the rest,
/// the latter two with the sizes of the first.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    All,
    Arguments,
    Sizes,
}

/// The sizes a solve of sizes gave the arrays of the randomize call of
/// `object`, by object and array (see [`Phase::Sizes`]): for the solve of
/// arguments, then the solve of the rest, or for the solve of the rest
/// once the solve of arguments `kept` them.
#[derive(Clone, Debug, Default)]
pub(crate) struct Staged {
    object: Handle,
    kept: bool,
    sizes: fxhash::FxHashMap<(Handle, usize), usize>,
}

/// A dynamic array or queue property of a constraint program.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Array {
    /// The byte offset of the property, which holds the container handle.
    pub offset: u32,
    /// The heap parameter of the container (see [`crate::containers::parameter`]).
    pub layout: u64,
    /// The width of an element, 1 to 64 bits, for a random array.
    pub width: u32,
    pub random: bool,
    /// The values a random enumeration element may take (18.3); empty for
    /// others.
    #[serde(default)]
    pub domain: Vec<u64>,
}

/// The size up to which a random array's size is first sought; only when
/// the constraints leave no such size is a larger one taken.
const PREFERRED_ARRAY_SIZE: u64 = 1 << 12;
/// The largest size a random array is resized to.
const ARRAY_SIZE_LIMIT: u64 = 1 << 24;

/// The objects a randomize call in progress runs the hooks of (IEEE
/// 1800-2023 18.6.2), in order, each once.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct Visits {
    /// The object whose randomize call runs the hooks.
    root: Handle,
    pub(crate) objects: Vec<Handle>,
    seen: fxhash::FxHashSet<Handle>,
    /// Whether the visits discover the members, as pre_randomize visits do.
    discover: bool,
}

/// The program a class registers for the objects of it that rand handles
/// reach, see [`Heap::register_program`].
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Registered {
    Program(std::sync::Arc<Program>),
    /// The class has random variables or constraints the solver cannot
    /// take, for this reason.
    Unsupported(String),
}

/// Whether `term` is one of `values`, as a balanced tree so that a large
/// domain stays within the solver's depth limit.
fn member(term: &Term, width: u32, values: impl IntoIterator<Item = u64>) -> Term {
    fn all(mut terms: Vec<Term>) -> Term {
        match terms.len() {
            0 => boolv(true),
            1 => terms.pop().expect("one term"),
            len => {
                let right = terms.split_off(len / 2);
                and(all(terms), all(right))
            }
        }
    }
    let differs = values
        .into_iter()
        .map(|value| not(eq(term.clone(), bv(width, value))))
        .collect();
    not(all(differs))
}

/// The number of values in the cycle of randc `field`: the members of an
/// enumeration, which the cycle holds by their indices, or every value of
/// its width.
fn cycle_size(field: &Field) -> u64 {
    if field.domain.is_empty() {
        1 << field.width
    } else {
        field.domain.len() as u64
    }
}

/// The value of cycle entry `index` of randc `field`.
fn cycle_value(field: &Field, index: u64) -> u64 {
    if field.domain.is_empty() {
        index
    } else {
        field.domain[index as usize]
    }
}

/// The cycle entry of value `value` of randc `field`.
fn cycle_index(field: &Field, value: u64) -> Option<u64> {
    if field.domain.is_empty() {
        Some(value)
    } else {
        field
            .domain
            .iter()
            .position(|member| *member == value)
            .map(|index| index as u64)
    }
}

/// `a` shifted by `b` (IEEE 1800-2023 11.4.10): of the width of `a`, by
/// the unsigned amount `b` of any width, so that an amount of at least the
/// width shifts every bit out; `>>>` of a `signed` result fills with the sign.
fn shifted(op: &str, a: Term, b: Term, signed: bool) -> Term {
    let (Sort::Bv(width), Sort::Bv(amount)) = (a.0.sort.clone(), b.0.sort.clone()) else {
        unreachable!()
    };
    let wide = width.max(amount);
    let arithmetic = op == ">>>" && signed;
    let extend = |value: Term, from: u32, sign: bool| {
        if from == wide {
            value
        } else {
            let kind = if sign { "sign" } else { "zero" };
            node(
                Sort::Bv(wide),
                format!("(_ {kind}_extend {})", wide - from),
                vec![value],
            )
        }
    };
    let (a, b) = (extend(a, width, arithmetic), extend(b, amount, false));
    let shift = |name: &str, a: Term| node(Sort::Bv(wide), name, vec![a, b.clone()]);
    let not = |value: Term| node(Sort::Bv(wide), "bvnot", vec![value]);
    let value = match op {
        "<<" | "<<<" => shift("bvshl", a),
        // A negative value shifts in ones: the complement of the
        // complement shifted.
        _ if arithmetic => ite(
            node(Sort::Bool, "bvslt", vec![a.clone(), bv(wide, 0)]),
            not(shift("bvlshr", not(a.clone()))),
            shift("bvlshr", a),
        ),
        _ => shift("bvlshr", a),
    };
    if wide == width {
        value
    } else {
        node(
            Sort::Bv(width),
            format!("(_ extract {} 0)", width - 1),
            vec![value],
        )
    }
}

/// The attempt that decides a call: each of `tiers` in order, first
/// without and then with the restarts of `restarts`, until one is not
/// unsatisfiable. The first `weighted` tiers, which add the items picked
/// from distributions, also give way when the solver cannot decide them,
/// so that the weights never make a call fail (IEEE 1800-2023 18.5.3).
fn search<T>(
    tiers: usize,
    weighted: usize,
    restarts: &[bool],
    mut attempt: impl FnMut(usize, bool) -> T,
    verdict: impl Fn(&T) -> &Verdict,
) -> T {
    let mut found = None;
    for &restart in restarts {
        for tier in 0..tiers {
            let outcome = attempt(tier, restart);
            let decided = match verdict(&outcome) {
                Verdict::Unsat => false,
                Verdict::Unknown => tier >= weighted,
                Verdict::Sat => true,
            };
            if decided {
                return outcome;
            }
            found = Some(outcome);
        }
    }
    found.expect("a solver attempt")
}

/// The arrays of the objects a call solves, by object and array: the first
/// solver variable of the elements of each random array a constraint names
/// the elements of, with their number, and the size of each array whose
/// size the call does not solve, a known value (IEEE 1800-2023 18.4).
#[derive(Default)]
struct Arrays {
    elements: fxhash::FxHashMap<(Handle, usize), (usize, usize)>,
    sizes: fxhash::FxHashMap<(Handle, usize), usize>,
}

/// A soft constraint a call takes (IEEE 1800-2023 18.5.13): its rank, its
/// truth, the variables that appear in it, whether X or Z bits decide its
/// truth (see [`x_truth`]) and the weighted items of its distributions (see
/// [`weighted_items`]).
struct Soft {
    rank: Rank,
    constraint: Term,
    names: Vec<String>,
    unknown: Option<Term>,
    distributions: Vec<Vec<(Term, num_bigint::BigUint)>>,
}

/// The items of each distribution of `constraint`, also one under `if` or
/// `->`, or in a `foreach` one for each element, with their nonzero weights
/// (IEEE 1800-2023 18.5.3): each holds where a guard keeps the distribution
/// from applying.
fn weighted_items(
    constraint: &Expr,
    terms: &[Term],
    base: usize,
    sizes: &[Term],
    inputs: &[u64],
) -> Result<Vec<Vec<(Term, num_bigint::BigUint)>>, String> {
    let mut found = Vec::new();
    distributions_of(constraint, &mut Vec::new(), &mut found);
    found
        .into_iter()
        .map(|(guards, choices)| {
            // The distribution applies unless a guard holds.
            let mut skipped = boolv(false);
            for guard in guards {
                let guard = truth(guard.term(terms, base, sizes, inputs)?);
                skipped = not(and(not(skipped), not(guard)));
            }
            choices
                .iter()
                .filter(|choice| choice.weight != num_bigint::BigUint::ZERO)
                .map(|choice| {
                    let item = truth(choice.predicate.term(terms, base, sizes, inputs)?);
                    Ok((
                        not(and(not(skipped.clone()), not(item))),
                        choice.weight.clone(),
                    ))
                })
                .collect()
        })
        .collect()
}

/// The distributions of `constraint`, each with the operands whose truth
/// keeps it from applying: the other operand of each `||` above it, such as
/// the negated condition of an `if` or `->` (IEEE 1800-2023 18.5.6, 18.5.7).
fn distributions_of<'a>(
    constraint: &'a Expr,
    guards: &mut Vec<&'a Expr>,
    found: &mut Vec<(Vec<&'a Expr>, &'a [DistChoice])>,
) {
    match constraint {
        Expr::Dist { choices, .. } => found.push((guards.clone(), choices)),
        Expr::Binary {
            op, left, right, ..
        } if op == "&&" => {
            distributions_of(left, guards, found);
            distributions_of(right, guards, found);
        }
        Expr::Binary {
            op, left, right, ..
        } if op == "||" => {
            guards.push(left);
            distributions_of(right, guards, found);
            guards.pop();
            guards.push(right);
            distributions_of(left, guards, found);
            guards.pop();
        }
        _ => {}
    }
}

fn truth(value: Term) -> Term {
    let Sort::Bv(width) = value.0.sort else {
        unreachable!()
    };
    not(eq(value, bv(width, 0)))
}
fn boolean(value: Term) -> Term {
    ite(value, bv(1, 1), bv(1, 0))
}

impl Expr {
    /// Whether the expression reads the solver variable of an object the
    /// call solves.
    fn mentions_solved(&self) -> bool {
        match self {
            Self::Solved(_) => true,
            Self::Foreach { body, .. } => body.mentions_solved(),
            Self::Enabled { value, .. }
            | Self::Soft { value, .. }
            | Self::Resize { value, .. }
            | Self::Unary { value, .. } => value.mentions_solved(),
            Self::Binary { left, right, .. } => left.mentions_solved() || right.mentions_solved(),
            Self::DisableSoft {
                variable,
                condition,
                ..
            } => {
                variable.mentions_solved()
                    || condition
                        .as_ref()
                        .is_some_and(|condition| condition.mentions_solved())
            }
            Self::Dist {
                membership,
                choices,
            } => {
                membership.mentions_solved()
                    || choices
                        .iter()
                        .any(|choice| choice.predicate.mentions_solved())
            }
            Self::Literal(..)
            | Self::Field(_)
            | Self::Static(_)
            | Self::Size(_)
            | Self::Sum(_)
            | Self::Input { .. }
            | Self::Index { .. }
            | Self::Member { .. }
            | Self::Handle(_)
            | Self::Element { .. }
            | Self::ElementAt { .. } => false,
        }
    }

    /// Whether the expression names the elements of array `index`: a loop
    /// over it or its sum.
    fn names_elements(&self, index: usize) -> bool {
        match self {
            Self::Sum(array) | Self::ElementAt { array, .. } => *array == index,
            Self::Foreach { array, body } => *array == index || body.names_elements(index),
            Self::Enabled { value, .. }
            | Self::Soft { value, .. }
            | Self::Resize { value, .. }
            | Self::Unary { value, .. } => value.names_elements(index),
            Self::DisableSoft {
                variable,
                condition,
                ..
            } => {
                variable.names_elements(index)
                    || condition
                        .as_ref()
                        .is_some_and(|condition| condition.names_elements(index))
            }
            Self::Binary { left, right, .. } => {
                left.names_elements(index) || right.names_elements(index)
            }
            Self::Dist {
                membership,
                choices,
            } => {
                membership.names_elements(index)
                    || choices
                        .iter()
                        .any(|choice| choice.predicate.names_elements(index))
            }
            Self::Literal(..)
            | Self::Field(_)
            | Self::Handle(_)
            | Self::Size(_)
            | Self::Input { .. }
            | Self::Index { .. }
            | Self::Static(_)
            | Self::Solved(_)
            | Self::Member { .. }
            | Self::Element { .. } => false,
        }
    }

    /// The solver variables that appear in the unrolled expression of the
    /// object whose fields are the variables from `base` on and whose array
    /// sizes are those from `size_base` on, by name.
    fn variables(&self, base: usize, size_base: usize, names: &mut Vec<String>) {
        match self {
            Self::Field(index) => names.push((base + index).to_string()),
            Self::Solved(index) => names.push(index.to_string()),
            Self::Size(index) => names.push(format!("s{}", size_base + index)),
            Self::Enabled { value, .. }
            | Self::Soft { value, .. }
            | Self::DisableSoft {
                variable: value, ..
            }
            | Self::Resize { value, .. }
            | Self::Unary { value, .. } => value.variables(base, size_base, names),
            Self::Binary { left, right, .. } => {
                left.variables(base, size_base, names);
                right.variables(base, size_base, names);
            }
            Self::Foreach { body, .. } => body.variables(base, size_base, names),
            Self::Dist { membership, .. } => membership.variables(base, size_base, names),
            Self::Literal(..)
            | Self::Sum(_)
            | Self::Static(_)
            | Self::Handle(_)
            | Self::Input { .. }
            | Self::Index { .. }
            | Self::Member { .. }
            | Self::Element { .. }
            | Self::ElementAt { .. } => {}
        }
    }

    /// Whether the expression reads an input from index `first` on.
    pub fn reads_input_from(&self, first: usize) -> bool {
        match self {
            Self::Input { index, .. } => *index >= first,
            Self::Enabled { value, .. }
            | Self::Soft { value, .. }
            | Self::Resize { value, .. }
            | Self::Unary { value, .. } => value.reads_input_from(first),
            Self::DisableSoft {
                variable,
                condition,
                ..
            } => {
                variable.reads_input_from(first)
                    || condition
                        .as_deref()
                        .is_some_and(|condition| condition.reads_input_from(first))
            }
            Self::Binary { left, right, .. } => {
                left.reads_input_from(first) || right.reads_input_from(first)
            }
            Self::Foreach { body, .. } => body.reads_input_from(first),
            Self::Dist {
                membership,
                choices,
            } => {
                membership.reads_input_from(first)
                    || choices
                        .iter()
                        .any(|choice| choice.predicate.reads_input_from(first))
            }
            Self::Literal(..)
            | Self::Sum(_)
            | Self::Field(_)
            | Self::Static(_)
            | Self::Handle(_)
            | Self::Size(_)
            | Self::Index { .. }
            | Self::Solved(_)
            | Self::Member { .. }
            | Self::Element { .. }
            | Self::ElementAt { .. } => false,
        }
    }

    /// Whether the expression names the size of array `index`.
    fn names_size(&self, index: usize) -> bool {
        match self {
            Self::Size(array) => *array == index,
            Self::Enabled { value, .. }
            | Self::Soft { value, .. }
            | Self::Resize { value, .. }
            | Self::Unary { value, .. } => value.names_size(index),
            // A guard of `disable soft` names what it reads, as any guard.
            Self::DisableSoft {
                variable,
                condition,
                ..
            } => {
                variable.names_size(index)
                    || condition
                        .as_ref()
                        .is_some_and(|condition| condition.names_size(index))
            }
            Self::Binary { left, right, .. } => left.names_size(index) || right.names_size(index),
            Self::Foreach { body, .. } => body.names_size(index),
            Self::Sum(_) | Self::ElementAt { .. } => false,
            Self::Dist {
                membership,
                choices,
            } => {
                membership.names_size(index)
                    || choices
                        .iter()
                        .any(|choice| choice.predicate.names_size(index))
            }
            Self::Literal(..)
            | Self::Field(_)
            | Self::Handle(_)
            | Self::Input { .. }
            | Self::Index { .. }
            | Self::Static(_)
            | Self::Solved(_)
            | Self::Member { .. }
            | Self::Element { .. } => false,
        }
    }

    /// The term of the constraint of the object whose fields are those of
    /// `terms` from `base` on.
    fn term(
        &self,
        terms: &[Term],
        base: usize,
        sizes: &[Term],
        inputs: &[u64],
    ) -> Result<Term, String> {
        self.four_state(terms, base, sizes, inputs, &Unknown::default())
            .map(|(value, _)| value)
    }

    /// [`Self::term`] of the expression with the mask of its X and Z bits,
    /// `None` while it has none, by the 4-state semantics of each operator
    /// (IEEE 1800-2023 11.4), from those of the state fields and inputs in
    /// `unknown`: an operand bit that cannot change the result, as one a
    /// known 0 masks with `&` or a decided `&&` or `||` leaves out, makes no
    /// result bit X. A value bit under the mask is arbitrary.
    fn four_state(
        &self,
        terms: &[Term],
        base: usize,
        sizes: &[Term],
        inputs: &[u64],
        unknown: &Unknown,
    ) -> Result<(Term, Option<Term>), String> {
        let masked = |width: u32, bits: Option<&u64>| {
            bits.map(|bits| bits & low(width))
                .filter(|bits| *bits != 0)
                .map(|bits| bv(width, bits))
        };
        Ok(match self {
            Self::Enabled { .. } => return Err("nested constraint mode predicate".into()),
            Self::Dist { membership, .. } => {
                membership.four_state(terms, base, sizes, inputs, unknown)?
            }
            Self::Soft { .. } | Self::DisableSoft { .. } => {
                return Err("nested soft constraint".into());
            }
            Self::Literal(width, value) => (bv(*width, *value), None),
            Self::Input { index, width } => {
                if !(1..=64).contains(width) {
                    return Err("invalid constraint input width".into());
                }
                (
                    bv(
                        *width,
                        *inputs.get(*index).ok_or("missing constraint input")?,
                    ),
                    masked(*width, unknown.inputs.get(index)),
                )
            }
            Self::Field(index) | Self::Solved(index) => {
                let index = if matches!(self, Self::Field(_)) {
                    base + *index
                } else {
                    *index
                };
                let term = terms.get(index).ok_or("invalid constraint field")?.clone();
                let mask = masked(width_of(&term), unknown.fields.get(&index));
                (term, mask)
            }
            Self::Static(_) => return Err("a static rand property outside a program".into()),
            Self::Size(index) => (
                sizes.get(*index).ok_or("invalid constraint array")?.clone(),
                None,
            ),
            Self::Member { .. } => return Err("constraint member of an unread handle".into()),
            Self::Foreach { .. } | Self::Index { .. } | Self::Element { .. } => {
                return Err("constraint loop outside its array".into());
            }
            Self::ElementAt { .. } => return Err("constraint element of an unread array".into()),
            Self::Sum(_) => return Err("constraint sum of an unread array".into()),
            Self::Handle(_) => return Err("a handle the call did not read".into()),
            Self::Resize {
                value,
                width,
                signed,
            } => {
                let (value, mask) = value.four_state(terms, base, sizes, inputs, unknown)?;
                let old = width_of(&value);
                // A sign extension extends an X sign bit too.
                let resize = |value: Term| {
                    if old == *width {
                        value
                    } else {
                        let op = if old > *width {
                            format!("(_ extract {} 0)", width - 1)
                        } else {
                            format!(
                                "(_ {}_extend {})",
                                if *signed { "sign" } else { "zero" },
                                width - old
                            )
                        };
                        node(Sort::Bv(*width), op, vec![value])
                    }
                };
                (resize(value), mask.map(resize))
            }
            Self::Unary { op, value } => {
                let (value, mask) = value.four_state(terms, base, sizes, inputs, unknown)?;
                let width = width_of(&value);
                match op.as_str() {
                    "!" => {
                        let mask = mask.map(|mask| boolean(x_truth(&value, &mask)));
                        (boolean(not(truth(value))), mask)
                    }
                    "~" => (node(Sort::Bv(width), "bvnot", vec![value]), mask),
                    "-" => (
                        node(Sort::Bv(width), "bvsub", vec![bv(width, 0), value]),
                        mask.map(|mask| any_x(&mask, width)),
                    ),
                    _ => return Err("invalid constraint unary operation".into()),
                }
            }
            Self::Binary {
                op,
                left,
                right,
                signed,
            } => {
                let (a, ma) = left.four_state(terms, base, sizes, inputs, unknown)?;
                let (b, mb) = right.four_state(terms, base, sizes, inputs, unknown)?;
                let relation = match op.as_str() {
                    "==" => Some(eq(a.clone(), b.clone())),
                    "!=" => Some(not(eq(a.clone(), b.clone()))),
                    "&&" => Some(and(truth(a.clone()), truth(b.clone()))),
                    "||" => Some(node(
                        Sort::Bool,
                        "or",
                        vec![truth(a.clone()), truth(b.clone())],
                    )),
                    "<" | "<=" | ">" | ">=" => {
                        let suffix = if op.ends_with('=') { "le" } else { "lt" };
                        let operands = if op.starts_with('>') {
                            vec![b.clone(), a.clone()]
                        } else {
                            vec![a.clone(), b.clone()]
                        };
                        Some(node(
                            Sort::Bool,
                            format!("bv{}{suffix}", if *signed { "s" } else { "u" }),
                            operands,
                        ))
                    }
                    _ => None,
                };
                let mask = if ma.is_none() && mb.is_none() {
                    None
                } else {
                    let (wa, wb) = (width_of(&a), width_of(&b));
                    let ma = ma.unwrap_or_else(|| bv(wa, 0));
                    let mb = mb.unwrap_or_else(|| bv(wb, 0));
                    Some(binary_mask(op, (&a, &ma), (&b, &mb), *signed)?)
                };
                let value = if let Some(relation) = relation {
                    boolean(relation)
                } else if matches!(op.as_str(), "<<" | "<<<" | ">>" | ">>>") {
                    shifted(op, a, b, *signed)
                } else {
                    let op = match op.as_str() {
                        "+" => "bvadd",
                        "-" => "bvsub",
                        "*" => "bvmul",
                        "&" => "bvand",
                        "|" => "bvor",
                        "^" => "bvxor",
                        _ => return Err("invalid constraint binary operation".into()),
                    };
                    node(a.0.sort.clone(), op, vec![a, b])
                };
                (value, mask)
            }
        })
    }
}

/// The X and Z bits of the state values of a call, the masks of the state
/// fields by their solver variables and of the inputs by their indices
/// (see [`Program::unknown_inputs`]).
#[derive(Default)]
struct Unknown {
    fields: fxhash::FxHashMap<usize, u64>,
    inputs: fxhash::FxHashMap<usize, u64>,
}

/// The error of a constraint whose value or guard X or Z bits decide
/// (IEEE 1800-2023 18.3).
const UNKNOWN_VALUE: &str = "a constraint on a value with X or Z bits";

/// The truth of Boolean term `check` with the variables of `assignments`,
/// `None` while it reads another.
fn evaluate_check(
    check: &Term,
    assignments: &std::collections::BTreeMap<String, Scalar>,
) -> Option<bool> {
    let terms = aplite::Env::from([(String::new(), check.clone())]);
    match aplite::finite::evaluate_scalar_terms(&terms, assignments, Limits::default())
        .ok()?
        .remove("")?
    {
        Scalar::Bool(value) => Some(value),
        Scalar::Bv { .. } => None,
    }
}

/// Whether X or Z bits decide the truth of one of `checks` (see
/// [`x_truth`]) with the state values `state` and the solution of
/// `outcome`; one that cannot be evaluated counts.
fn decided_by_unknown(
    checks: &[Term],
    state: &std::collections::BTreeMap<String, Scalar>,
    outcome: &aplite::finite::Outcome,
) -> bool {
    if checks.is_empty() {
        return false;
    }
    let mut values = state.clone();
    values.extend(outcome.assignments.clone());
    values.extend(outcome.context_values.clone());
    checks
        .iter()
        .any(|check| evaluate_check(check, &values) != Some(false))
}

/// The low `width` bits.
fn low(width: u32) -> u64 {
    u64::MAX.checked_shr(64 - width.min(64)).unwrap_or(0)
}

/// The width of bit-vector term `term`.
fn width_of(term: &Term) -> u32 {
    let Sort::Bv(width) = term.0.sort else {
        unreachable!("a bit-vector term")
    };
    width
}

/// Whether a value with X or Z bits `mask` has a truth value that they
/// decide: no known bit is 1 and some bit is X (IEEE 1800-2023 11.4.7, 12.4).
fn x_truth(value: &Term, mask: &Term) -> Term {
    let width = width_of(mask);
    let known = node(
        Sort::Bv(width),
        "bvand",
        vec![
            value.clone(),
            node(Sort::Bv(width), "bvnot", vec![mask.clone()]),
        ],
    );
    and(eq(known, bv(width, 0)), not(eq(mask.clone(), bv(width, 0))))
}

/// The truth of a known value, its width, value and mask of X and Z bits:
/// true with a known 1 bit, false with every bit a known 0, and `None`, X,
/// otherwise (IEEE 1800-2023 12.4).
fn known_truth((_, value, mask): (u32, u64, u64)) -> Option<bool> {
    if value & !mask != 0 {
        Some(true)
    } else {
        (mask == 0).then_some(false)
    }
}

/// All `width` bits X while `mask` has an X bit, as an arithmetic result is.
fn any_x(mask: &Term, width: u32) -> Term {
    let source = width_of(mask);
    ite(
        eq(mask.clone(), bv(source, 0)),
        bv(width, 0),
        bv(width, low(width)),
    )
}

/// The mask of the X bits of binary operation `op` of `a` and `b`, each a
/// value and the mask of its X bits (IEEE 1800-2023 11.4).
fn binary_mask(
    op: &str,
    (a, ma): (&Term, &Term),
    (b, mb): (&Term, &Term),
    signed: bool,
) -> Result<Term, String> {
    let width = width_of(a);
    let op2 = |op: &str, x: Term, y: Term| node(x.0.sort.clone(), op, vec![x, y]);
    let invert = |x: &Term| node(x.0.sort.clone(), "bvnot", vec![x.clone()]);
    let either = op2("bvor", ma.clone(), mb.clone());
    let none = |mask: &Term| eq(mask.clone(), bv(width_of(mask), 0));
    // A known value: a known 1 bit makes it true, all bits known 0 false.
    let known_true =
        |value: &Term, mask: &Term| not_zero(&op2("bvand", value.clone(), invert(mask)));
    let known_false =
        |value: &Term, mask: &Term| and(none(mask), eq(value.clone(), bv(width_of(value), 0)));
    Ok(match op {
        // Ambiguous by an X bit where the known bits are equal (11.4.5).
        "==" | "!=" => boolean(and(
            eq(
                op2("bvand", op2("bvxor", a.clone(), b.clone()), invert(&either)),
                bv(width, 0),
            ),
            not_zero(&either),
        )),
        // Decided by a known operand that decides it (11.4.7).
        "&&" | "||" => {
            let decided = if op == "&&" {
                node(
                    Sort::Bool,
                    "or",
                    vec![known_false(a, ma), known_false(b, mb)],
                )
            } else {
                node(Sort::Bool, "or", vec![known_true(a, ma), known_true(b, mb)])
            };
            boolean(and(
                not(decided),
                node(Sort::Bool, "or", vec![x_truth(a, ma), x_truth(b, mb)]),
            ))
        }
        "<" | "<=" | ">" | ">=" => boolean(not_zero(&either)),
        // A known 0 of the other operand masks an X bit of `&`, a known 1
        // of `|` (11.4.8).
        "&" => op2(
            "bvand",
            either,
            op2(
                "bvand",
                op2("bvor", ma.clone(), a.clone()),
                op2("bvor", mb.clone(), b.clone()),
            ),
        ),
        "|" => op2(
            "bvand",
            either,
            op2(
                "bvand",
                op2("bvor", ma.clone(), invert(a)),
                op2("bvor", mb.clone(), invert(b)),
            ),
        ),
        "^" => either,
        // A shift moves the X bits of its value; an X amount makes all X
        // (11.4.10).
        "<<" | "<<<" | ">>" | ">>>" => ite(
            none(mb),
            shifted(op, ma.clone(), b.clone(), signed),
            bv(width, low(width)),
        ),
        "+" | "-" | "*" => any_x(&either, width),
        _ => return Err("invalid constraint binary operation".into()),
    })
}

/// Whether bit-vector `value` is not 0.
fn not_zero(value: &Term) -> Term {
    not(eq(value.clone(), bv(width_of(value), 0)))
}

impl Heap {
    /// `constraint` of object `object` with each loop over a state array
    /// replaced by the conjunction of its body for each element, which
    /// `loops` gives for the enclosing loops: (array, index).
    /// The width, value and mask of X and Z bits of unrolled constraint
    /// expression `expr` of `object` if known before solving: one of
    /// literals, inputs, the fields of the object that are not random for
    /// the call and the sizes of its arrays that are not, as the guards of
    /// constraints are (IEEE 1800-2023 18.5.13). It is the value of the term
    /// the solver takes for `expr` by its 4-state semantics (see
    /// [`Expr::four_state`]), with those as constants and the random ones
    /// free, by the solver's own evaluation; `&&`, `||` and `!`
    /// short-circuit (11.4.7), so that a guard that a known operand decides
    /// is known though the other is random.
    fn known(
        &mut self,
        object: Handle,
        program: &Program,
        expr: &Expr,
        check_only: bool,
        inputs: &[u64],
    ) -> Option<(u32, u64, u64)> {
        let boolean = |value: Option<bool>| match value {
            Some(value) => (1, u64::from(value), 0),
            None => (1, 0, 1),
        };
        match expr {
            Expr::Unary { op, value } if op == "!" => {
                let value = self.known(object, program, value, check_only, inputs)?;
                return Some(boolean(known_truth(value).map(|value| !value)));
            }
            Expr::Binary {
                op, left, right, ..
            } if matches!(op.as_str(), "&&" | "||") => {
                // A known operand of the value that decides it decides it,
                // also with an X other; else both known decide it, and an X
                // one makes it X (11.4.7).
                let decisive = op == "||";
                let left = self
                    .known(object, program, left, check_only, inputs)
                    .map(known_truth);
                if left == Some(Some(decisive)) {
                    return Some(boolean(Some(decisive)));
                }
                let right = self
                    .known(object, program, right, check_only, inputs)
                    .map(known_truth);
                if right == Some(Some(decisive)) {
                    return Some(boolean(Some(decisive)));
                }
                return match (left?, right?) {
                    (Some(_), Some(_)) => Some(boolean(Some(!decisive))),
                    _ => Some(boolean(None)),
                };
            }
            _ => {}
        }
        // A variable of another object the call solves is not known.
        if expr.mentions_solved() {
            return None;
        }
        // The fields and array sizes as the solver takes them: a value when
        // the call does not randomize it, free otherwise, with the X and Z
        // bits of a 4-state one, as those of an input of the call.
        let mut unknown = Unknown::default();
        for &(input, mask) in &program.unknown_inputs {
            if let Some(&bits) = inputs.get(mask) {
                unknown.inputs.insert(input, bits);
            }
        }
        let fields = program
            .fields
            .iter()
            .enumerate()
            .map(|(index, field)| {
                let width = field.width.clamp(1, 64);
                let value = (1..=64).contains(&field.width)
                    && !(!check_only && self.random_mode(object, field.offset, field.random));
                if !value {
                    return var(format!("field{index}"), Sort::Bv(width));
                }
                let (value, bits) = self.field_bits(object, field);
                unknown.fields.insert(index, bits);
                bv(width, value)
            })
            .collect::<Vec<_>>();
        let sizes = program
            .arrays
            .iter()
            .enumerate()
            .map(|(index, array)| {
                if !check_only && self.random_mode(object, array.offset, array.random) {
                    var(format!("size{index}"), Sort::Bv(32))
                } else {
                    let contents = self.load(object, array.offset as usize, crate::HANDLE_BYTES);
                    bv(32, self.sequence_len(contents as Handle) as u64)
                }
            })
            .collect::<Vec<_>>();
        let (value, mask) = expr.four_state(&fields, 0, &sizes, inputs, &unknown).ok()?;
        let width = width_of(&value);
        let terms = aplite::Env::from([
            ("value".to_owned(), value),
            ("mask".to_owned(), mask.unwrap_or_else(|| bv(width, 0))),
        ]);
        let mut values =
            aplite::finite::evaluate_scalar_terms(&terms, &Default::default(), Limits::default())
                .ok()?;
        let mut scalar = |name: &str| match values.remove(name)? {
            Scalar::Bool(value) => Some((1, u64::from(value))),
            Scalar::Bv { width, value } => Some((width, value)),
        };
        let (width, value) = scalar("value")?;
        let (_, mask) = scalar("mask")?;
        Some((width, value, mask))
    }

    /// The value of `field` of `object`, 1 to 64 bits, as constraints read
    /// it, the one decoding of every reader: its bits, or for a 4-state
    /// field, which holds the complements of its value and of its
    /// unknown-bit mask (see [`Field::four_state`]), its known bits, `0`
    /// for each X or Z bit, and the mask of those (IEEE 1800-2023 18.3).
    fn field_bits(&mut self, object: Handle, field: &Field) -> (u64, u64) {
        let bytes = field.width.div_ceil(8) as usize;
        let bits = u64::MAX >> (64 - field.width);
        let value = self.load(object, field.offset as usize, bytes);
        if !field.four_state {
            return (value & bits, 0);
        }
        let unknown = !self.load(object, field.offset as usize + bytes, bytes) & bits;
        (!value & bits & !unknown, unknown)
    }

    /// Store solution `value` in `field` of `object`, as [`Self::field_bits`]
    /// reads it: a 4-state field holds the complements of the value and of
    /// a mask without X or Z bits.
    fn store_field(&mut self, object: Handle, field: &Field, value: u64) {
        let bytes = field.width.div_ceil(8) as usize;
        if field.four_state {
            let bits = u64::MAX >> (64 - field.width);
            self.store(object, field.offset as usize, bytes, !value & bits);
            self.store(object, field.offset as usize + bytes, bytes, bits);
        } else {
            self.store(object, field.offset as usize, bytes, value);
        }
    }

    /// The solver variable of the member at `offset` of `object`, when the
    /// call solves the object and the member is random in it: not a state
    /// member, nor one whose rand_mode is off, nor any in a call that only
    /// checks (IEEE 1800-2023 18.8, 18.11.1), which keep their values.
    fn solver_variable(
        &mut self,
        object: Handle,
        offset: u32,
        solved: &fxhash::FxHashMap<Handle, (usize, &Program)>,
        check_only: bool,
    ) -> Option<usize> {
        let &(base, program) = solved.get(&object)?;
        let index = program
            .fields
            .iter()
            .position(|field| field.offset == offset)?;
        (!check_only && self.random_mode(object, offset, program.fields[index].random))
            .then_some(base + index)
    }

    fn unroll(
        &mut self,
        object: Handle,
        program: &Program,
        constraint: &Expr,
        loops: &mut Vec<(usize, u64)>,
        solved: &fxhash::FxHashMap<Handle, (usize, &Program)>,
        arrays: &Arrays,
        check_only: bool,
        inputs: &[u64],
    ) -> Result<Expr, String> {
        let boxed = |value: Expr| Box::new(value);
        Ok(match constraint {
            Expr::Foreach { array, body } => {
                let looped = program
                    .arrays
                    .get(*array)
                    .ok_or("invalid constraint array")?;
                let offset = looped.offset;
                // An array the call randomizes has a solver variable for
                // each element (see `Arrays`).
                if !check_only
                    && self.random_mode(object, offset, looped.random)
                    && !arrays.elements.contains_key(&(object, *array))
                {
                    return Err("a constraint loop over an array the call randomizes".into());
                }
                let contents = self.load(object, offset as usize, crate::HANDLE_BYTES) as Handle;
                // A random array has the elements of its solved size, which
                // a solve of sizes may have given it (see `Phase::Sizes`).
                let length = match arrays.elements.get(&(object, *array)) {
                    Some(&(_, length)) => length,
                    None => self.sequence_len(contents),
                };
                let mut each = Vec::new();
                for index in 0..length as u64 {
                    loops.push((*array, index));
                    each.push(self.unroll(
                        object, program, body, loops, solved, arrays, check_only, inputs,
                    )?);
                    loops.pop();
                }
                // A balanced conjunction stays within the solver's depth.
                fn all(mut each: Vec<Expr>) -> Expr {
                    match each.len() {
                        0 => Expr::Literal(1, 1),
                        1 => each.pop().expect("one constraint"),
                        len => {
                            let right = each.split_off(len / 2);
                            Expr::Binary {
                                op: "&&".into(),
                                left: Box::new(all(each)),
                                right: Box::new(all(right)),
                                signed: false,
                            }
                        }
                    }
                }
                all(each)
            }
            Expr::Index { level } => {
                let &(_, index) = loops
                    .get(*level as usize)
                    .ok_or("invalid constraint loop")?;
                Expr::Literal(32, index)
            }
            Expr::Element {
                level,
                member,
                width,
            } => {
                if !(1..=64).contains(width) {
                    return Err("invalid constraint element width".into());
                }
                let &(array, index) = loops
                    .get(*level as usize)
                    .ok_or("invalid constraint loop")?;
                if member.is_none()
                    && let Some(&(base, _)) = arrays.elements.get(&(object, array))
                {
                    return Ok(Expr::Solved(base + index as usize));
                }
                let offset = program.arrays[array].offset;
                let contents = self.load(object, offset as usize, crate::HANDLE_BYTES) as Handle;
                let value = self
                    .sequence_element(contents, index as usize)
                    .ok_or("constraint array element")?;
                let value = match member {
                    None => value,
                    Some(member) => {
                        let element = value as Handle;
                        if element == crate::objects::NULL {
                            return Err("a null object handle in a constraint".into());
                        }
                        // A random member of an object the call solves is
                        // its solver variable; a fixed one, its value.
                        if let Some(field) =
                            self.solver_variable(element, *member, solved, check_only)
                        {
                            return Ok(Expr::Solved(field));
                        }
                        self.load(element, *member as usize, width.div_ceil(8) as usize)
                    }
                };
                Expr::Literal(*width, value & (u64::MAX >> (64 - width)))
            }
            Expr::Sum(array) => {
                let summed = program
                    .arrays
                    .get(*array)
                    .ok_or("invalid constraint array")?;
                let width = summed.width;
                if !(1..=64).contains(&width) {
                    return Err("invalid constraint array element width".into());
                }
                let contents =
                    self.load(object, summed.offset as usize, crate::HANDLE_BYTES) as Handle;
                let length = self.sequence_len(contents);
                let mask = u64::MAX >> (64 - width);
                match arrays.elements.get(&(object, *array)) {
                    // The sum of the solver variables of its elements, at
                    // their width, as a balanced tree.
                    Some(&(base, length)) => {
                        fn add(mut each: Vec<Expr>, width: u32) -> Expr {
                            match each.len() {
                                0 => Expr::Literal(width, 0),
                                1 => each.pop().expect("one term"),
                                len => {
                                    let right = each.split_off(len / 2);
                                    Expr::Binary {
                                        op: "+".into(),
                                        left: Box::new(add(each, width)),
                                        right: Box::new(add(right, width)),
                                        signed: false,
                                    }
                                }
                            }
                        }
                        add(
                            (0..length)
                                .map(|index| Expr::Solved(base + index))
                                .collect(),
                            width,
                        )
                    }
                    // The sum of its elements' values.
                    None => {
                        let mut total = 0u64;
                        for index in 0..length {
                            let value = self
                                .sequence_element(contents, index)
                                .ok_or("constraint array element")?;
                            total = total.wrapping_add(value);
                        }
                        Expr::Literal(width, total & mask)
                    }
                }
            }
            Expr::Member {
                handle,
                member,
                width,
            } => {
                if !(1..=64).contains(width) {
                    return Err("invalid constraint member width".into());
                }
                let target = self.load(object, *handle as usize, crate::HANDLE_BYTES) as Handle;
                if target == crate::objects::NULL {
                    return Err("a null object handle in a constraint".into());
                }
                // A random member of an object the call solves is its
                // solver variable; a fixed one, or one of another object,
                // its value, which a guard reads before solving (18.5.13).
                if let Some(field) = self.solver_variable(target, *member, solved, check_only) {
                    return Ok(Expr::Solved(field));
                }
                let value = self.load(target, *member as usize, width.div_ceil(8) as usize);
                Expr::Literal(*width, value & (u64::MAX >> (64 - width)))
            }
            Expr::ElementAt {
                array,
                index,
                width,
            } => {
                if !(1..=64).contains(width) {
                    return Err("invalid constraint element width".into());
                }
                let read = program
                    .arrays
                    .get(*array)
                    .ok_or("invalid constraint array")?;
                // An element past the end reads as the default value
                // (IEEE 1800-2023 7.4.6).
                if let Some(&(base, length)) = arrays.elements.get(&(object, *array)) {
                    return Ok(match usize::try_from(*index) {
                        Ok(index) if index < length => Expr::Solved(base + index),
                        _ => Expr::Literal(*width, 0),
                    });
                }
                if !check_only && self.random_mode(object, read.offset, read.random) {
                    return Err("a constraint on an element of an array the call randomizes".into());
                }
                let contents =
                    self.load(object, read.offset as usize, crate::HANDLE_BYTES) as Handle;
                let value = usize::try_from(*index)
                    .ok()
                    .and_then(|index| self.sequence_element(contents, index))
                    .unwrap_or(0);
                Expr::Literal(*width, value & (u64::MAX >> (64 - width)))
            }
            Expr::Enabled { index, value } => Expr::Enabled {
                index: *index,
                value: boxed(self.unroll(
                    object, program, value, loops, solved, arrays, check_only, inputs,
                )?),
            },
            Expr::Soft { priority, value } => Expr::Soft {
                priority: *priority,
                value: boxed(self.unroll(
                    object, program, value, loops, solved, arrays, check_only, inputs,
                )?),
            },
            Expr::DisableSoft { .. } => constraint.clone(),
            Expr::Handle(offset) => Expr::Literal(
                8 * crate::HANDLE_BYTES as u32,
                self.load(object, *offset as usize, crate::HANDLE_BYTES),
            ),
            Expr::Resize {
                value,
                width,
                signed,
            } => Expr::Resize {
                value: boxed(self.unroll(
                    object, program, value, loops, solved, arrays, check_only, inputs,
                )?),
                width: *width,
                signed: *signed,
            },
            Expr::Unary { op, value } => Expr::Unary {
                op: op.clone(),
                value: boxed(self.unroll(
                    object, program, value, loops, solved, arrays, check_only, inputs,
                )?),
            },
            Expr::Binary {
                op,
                left,
                right,
                signed,
            } => {
                let left = self.unroll(
                    object, program, left, loops, solved, arrays, check_only, inputs,
                )?;
                // A guard whose value is known before solving (IEEE
                // 1800-2023 18.5.13), as that of `if` or `->` is the left
                // operand of `||`, decides whether what it guards applies:
                // when it does not, that is not unrolled, nor the handles it
                // names dereferenced. An X or Z guard decides nothing: what
                // it guards may then not be unrolled either (18.3).
                let known = self
                    .known(object, program, &left, check_only, inputs)
                    .map(known_truth);
                match (op.as_str(), known) {
                    ("||", Some(Some(true))) => return Ok(Expr::Literal(1, 1)),
                    ("&&", Some(Some(false))) => return Ok(Expr::Literal(1, 0)),
                    _ => {}
                }
                let right = self
                    .unroll(
                        object, program, right, loops, solved, arrays, check_only, inputs,
                    )
                    .map_err(|reason| match known {
                        Some(None) => UNKNOWN_VALUE.to_owned(),
                        _ => reason,
                    })?;
                Expr::Binary {
                    op: op.clone(),
                    left: boxed(left),
                    right: boxed(right),
                    signed: *signed,
                }
            }
            Expr::Dist {
                membership,
                choices,
            } => Expr::Dist {
                membership: boxed(self.unroll(
                    object, program, membership, loops, solved, arrays, check_only, inputs,
                )?),
                choices: choices
                    .iter()
                    .map(|choice| {
                        Ok(DistChoice {
                            predicate: self.unroll(
                                object,
                                program,
                                &choice.predicate,
                                loops,
                                solved,
                                arrays,
                                check_only,
                                inputs,
                            )?,
                            weight: choice.weight.clone(),
                        })
                    })
                    .collect::<Result<_, String>>()?,
            },
            // A size the call does not solve is a known value (18.4).
            Expr::Size(index) => match arrays.sizes.get(&(object, *index)) {
                Some(&length) => Expr::Literal(32, length as u64),
                None => constraint.clone(),
            },
            Expr::Literal(..)
            | Expr::Field(_)
            | Expr::Input { .. }
            | Expr::Static(_)
            | Expr::Solved(_) => constraint.clone(),
        })
    }

    /// Register `program` for the objects of class `class`, once: the class
    /// of an object rand handles reach is known only at run time, and an
    /// object was constructed, which registers the program of its class.
    pub fn register_program(&mut self, class: u32, program: Registered) {
        self.programs.entry(class).or_insert(program);
    }

    /// Begin the pre_randomize visits of a randomize call of `object`, whose
    /// own hook ran (IEEE 1800-2023 18.6.2): the objects its enabled rand
    /// object handles refer to, by the handles its class registered. Each
    /// visited object adds those of its own once its hook ran
    /// ([`Heap::expand_visits`]), so a member a hook allocates is visited,
    /// each object once. Kept until [`Heap::end_visits`], so a randomize
    /// call inside a hook has its own.
    pub fn begin_visits(&mut self, object: Handle) -> usize {
        let mut visits = Visits {
            root: object,
            objects: Vec::new(),
            seen: fxhash::FxHashSet::from_iter([object]),
            discover: true,
        };
        let handles = self.registered_handles(object);
        self.reach(object, &handles, &mut visits.seen, &mut visits.objects);
        let count = visits.objects.len();
        self.visits.push(visits);
        count
    }

    /// Add the objects the enabled rand object handles of visited object
    /// `object` refer to, once its pre_randomize ran: the number of objects
    /// the visits in progress have now.
    pub fn expand_visits(&mut self, object: Handle) -> usize {
        let Some(mut visits) = self.visits.pop() else {
            return 0;
        };
        let handles = self.registered_handles(object);
        self.reach(object, &handles, &mut visits.seen, &mut visits.objects);
        let count = visits.objects.len();
        self.visits.push(visits);
        count
    }

    /// End the visits in progress. The members whose pre_randomize ran are
    /// those the solve of the call takes.
    pub fn end_visits(&mut self) {
        if let Some(visits) = self.visits.pop()
            && visits.discover
        {
            self.prepared = Some((visits.root, visits.objects));
        }
    }

    /// The objects whose post_randomize runs (18.6.2): the rand object
    /// members the last successful solve took, without reading a handle
    /// again, since a hook may have changed them. Begun right after the
    /// solve, as [`Heap::begin_visits`] begins visits.
    pub fn begin_solved_visits(&mut self) -> usize {
        let objects = std::mem::take(&mut self.solved);
        let count = objects.len();
        self.visits.push(Visits {
            root: crate::objects::NULL,
            objects,
            seen: Default::default(),
            discover: false,
        });
        count
    }

    /// The rand object handles of the class of `object`, by its registered
    /// program.
    fn registered_handles(&self, object: Handle) -> Vec<u32> {
        match self.programs.get(&self.class_of(object)) {
            Some(Registered::Program(program)) => program.handles.clone(),
            _ => Vec::new(),
        }
    }

    /// Add to `found` the non-null objects the enabled (18.8) rand object
    /// handles at `handles` of `object` refer to that are not in `seen`.
    fn reach(
        &mut self,
        object: Handle,
        handles: &[u32],
        seen: &mut fxhash::FxHashSet<Handle>,
        found: &mut Vec<Handle>,
    ) {
        for &offset in handles {
            if !self.random_mode(object, offset, true) {
                continue;
            }
            let child = self.load(object, offset as usize, crate::HANDLE_BYTES) as Handle;
            if child != crate::objects::NULL && seen.insert(child) {
                found.push(child);
            }
        }
    }

    /// The rank of each object a randomize call of `object` reaches through
    /// its enabled rand object handles at `handles`, for the priorities of
    /// their soft constraints (IEEE 1800-2023 18.5.13.1): the indices of the
    /// handles on the path from `object`, ended by `u32::MAX`, so that a
    /// container is above the objects it contains and an object a later
    /// handle refers to is above one an earlier handle does; an object
    /// several handles reach takes the highest. `object` is `[u32::MAX]`.
    fn handle_ranks(
        &mut self,
        object: Handle,
        handles: &[u32],
    ) -> fxhash::FxHashMap<Handle, Vec<u32>> {
        let mut ranks = fxhash::FxHashMap::default();
        ranks.insert(object, vec![u32::MAX]);
        let mut pending = vec![(object, handles.to_vec(), Vec::new())];
        while let Some((current, handles, path)) = pending.pop() {
            for (index, &offset) in handles.iter().enumerate() {
                if !self.random_mode(current, offset, true) {
                    continue;
                }
                let child = self.load(current, offset as usize, crate::HANDLE_BYTES) as Handle;
                if child == crate::objects::NULL {
                    continue;
                }
                let mut rank: Vec<u32> = path.clone();
                rank.push(index as u32);
                let prefix = rank.clone();
                rank.push(u32::MAX);
                // An object takes its best route, and so do the objects it
                // reaches: a better one revisits its handles. A route
                // through a cycle ranks below the route it leaves, so this
                // ends.
                if ranks.get(&child).is_none_or(|best| rank > *best) {
                    ranks.insert(child, rank);
                    pending.push((child, self.registered_handles(child), prefix));
                }
            }
        }
        ranks
    }

    /// The objects the enabled rand object handles at `handles` of `object`
    /// reach, each once and without `object`, breadth first, by the handles
    /// their classes registered.
    fn reached(&mut self, object: Handle, handles: &[u32]) -> Vec<Handle> {
        let mut seen = fxhash::FxHashSet::from_iter([object]);
        let mut found = Vec::new();
        self.reach(object, handles, &mut seen, &mut found);
        let mut next = 0;
        while let Some(&current) = found.get(next) {
            next += 1;
            let handles = self.registered_handles(current);
            self.reach(current, &handles, &mut seen, &mut found);
        }
        found
    }

    /// Whether class `class` registered its program.
    pub fn program_registered(&self, class: u32) -> bool {
        self.programs.contains_key(&class)
    }

    /// Evaluate against the state after pre_randomize; commit only a complete
    /// assignment. A solver failure is a runtime fault, never an UNSAT result.
    pub fn randomize_object(
        &mut self,
        handle: Handle,
        program: &Program,
        check_only: bool,
    ) -> bool {
        self.randomize_object_with_inputs(handle, program, check_only, &[])
    }

    /// Solve using call-local, read-only scalar values. Inputs are not retained
    /// in the object or its RNG state and are never included in the projection.
    pub fn randomize_object_with_inputs(
        &mut self,
        handle: Handle,
        program: &Program,
        check_only: bool,
        inputs: &[u64],
    ) -> bool {
        self.randomize_object_with_limits(handle, program, check_only, inputs, Limits::default())
    }

    /// Solve the random variables in the arguments of the functions the
    /// constraints of `program` call, by the constraints that name no other
    /// variable and read no value of a call, `inputs` the values those read
    /// (IEEE 1800-2023 18.5.11): whether they have a solution. The object
    /// keeps its values; the solution is the values of the arguments
    /// ([`Heap::argument_value`]) and holds the variables for the next
    /// solve of the object, which takes every constraint.
    pub fn solve_arguments(
        &mut self,
        handle: Handle,
        program: &Program,
        check_only: bool,
        inputs: &[u64],
    ) -> bool {
        // A solve of this object that did not reach its solve of the rest
        // left its solution behind.
        self.arguments
            .retain(|arguments| arguments.object != handle);
        self.solve(
            handle,
            program,
            check_only,
            inputs,
            Limits::default(),
            Phase::Arguments,
        )
    }

    /// The value of argument `index` of the solve of arguments of the
    /// innermost randomize call in progress.
    pub fn argument_value(&self, index: usize) -> u64 {
        self.arguments
            .last()
            .and_then(|arguments| arguments.values.get(index).copied())
            .unwrap_or(0)
    }

    fn randomize_object_with_limits(
        &mut self,
        handle: Handle,
        program: &Program,
        check_only: bool,
        inputs: &[u64],
        limits: Limits,
    ) -> bool {
        self.solve(handle, program, check_only, inputs, limits, Phase::All)
    }

    fn solve(
        &mut self,
        handle: Handle,
        program: &Program,
        check_only: bool,
        inputs: &[u64],
        limits: Limits,
        phase: Phase,
    ) -> bool {
        if !self.check_object(handle, 0) {
            return false;
        }
        let result = (|| -> Result<bool, String> {
            // The sizes a solve of sizes gave this call's arrays.
            // A solve of arguments takes those of a solve of sizes just
            // before it, the solve of the rest those of either.
            // This call's sizes: those of another call stay for it, as a
            // function a constraint calls may randomize another object
            // between the solves of this call.
            let mine =
                |staged: &Staged| staged.object == handle && (phase == Phase::All || !staged.kept);
            let presized = match phase {
                Phase::All | Phase::Arguments => self
                    .staged
                    .iter()
                    .rposition(mine)
                    .map(|position| self.staged.remove(position).sizes)
                    .unwrap_or_default(),
                Phase::Sizes => fxhash::FxHashMap::default(),
            };
            // A solve of arguments starts a call: what an earlier call of
            // the object left behind is not its.
            if phase == Phase::Arguments {
                self.staged.retain(|staged| staged.object != handle);
            }
            // The object and those its enabled rand handles refer to, each
            // once, with the program of its class (18.4, 18.8). The handles
            // themselves are never changed.
            let mut members: Vec<(Handle, std::sync::Arc<Program>)> =
                vec![(handle, std::sync::Arc::new(program.clone()))];
            // The members are those whose pre_randomize ran, also of classes
            // with nothing to solve; post_randomize runs on them (18.6.2).
            // A check takes the same: randomize(null) makes the rand handles
            // of the object state too (18.11, 18.11.1). A call that ran no
            // hooks finds them here.
            let participants = match self.prepared.take() {
                Some((root, objects)) if root == handle => objects,
                _ => self.reached(handle, &program.handles),
            };
            // The arguments of the calls of the blocks that are on, each in
            // its block's constraint mode (18.5.11, 18.9); those of a block
            // that is off are none.
            let arguments = program
                .arguments
                .iter()
                .map(|argument| match argument {
                    Expr::Enabled { index, value } => self
                        .constraint_mode(handle, *index)
                        .then(|| value.as_ref().clone()),
                    argument => Some(argument.clone()),
                })
                .collect::<Vec<_>>();
            for &child in &participants {
                // A class that registered nothing has nothing to solve.
                match self.programs.get(&self.class_of(child)) {
                    None => {}
                    Some(Registered::Program(nested)) => members.push((child, nested.clone())),
                    Some(Registered::Unsupported(reason)) => {
                        return Err(format!("rand object member: {reason}"));
                    }
                }
            }
            let mut projection = Env::new();
            let mut terms = Vec::new();
            let mut active = Vec::new();
            let mut cyclic = Vec::new();
            // The X and Z bits of the 4-state state values of the call, the
            // values of its state fields, and whether those bits decide the
            // truth of each constraint the call takes or of a guard, which
            // is an error (IEEE 1800-2023 18.3), by 4-state semantics (see
            // [`Expr::four_state`]): a bit an operation leaves out decides
            // nothing.
            let mut unknown = Unknown::default();
            for &(input, mask) in &program.unknown_inputs {
                if let Some(&bits) = inputs.get(mask) {
                    unknown.inputs.insert(input, bits);
                }
            }
            let mut state = std::collections::BTreeMap::new();
            let mut checks = Vec::new();
            let mut formula = boolv(true);
            let mut bases = Vec::new();
            let mut size_terms = Vec::new();
            let mut size_bases = Vec::new();
            let mut refills = Vec::new();
            // The solver variables of the elements of the arrays and their
            // known sizes (see [`Arrays`]).
            let mut arrays = Arrays::default();
            // The random arrays whose size and elements constraints name,
            // with the variables of their sizes, in a solve of sizes; in a
            // solve of the rest, whether one has no size yet (18.5.7.1).
            let mut staged: Vec<(Handle, usize, String)> = Vec::new();
            let mut unsized_arrays = false;
            let mut preferred = boolv(true);
            // The soft constraints, with their ranks and the variables
            // that appear in them, and the variables `disable soft` names,
            // with its ranks (18.5.13).
            let mut soft: Vec<Soft> = Vec::new();
            let mut disabled: Vec<(Rank, String, Option<Term>)> = Vec::new();
            let ranks = self.handle_ranks(handle, &program.handles);
            let mut limited = boolv(true);
            let mut bounded = false;
            // The constraints of each object that take part, by their
            // constraint modes (18.9), with those of the call: every
            // decision below reads this one set.
            let mut constraints: Vec<Vec<&Expr>> = Vec::new();
            for (object, current) in &members {
                let mut enabled = Vec::new();
                for constraint in &current.constraints {
                    match constraint {
                        Expr::Enabled { index, value } => {
                            if self.constraint_mode(*object, *index) {
                                enabled.push(value.as_ref());
                            }
                        }
                        constraint => enabled.push(constraint),
                    }
                }
                constraints.push(enabled);
            }
            for (position, (object, current)) in members.iter().enumerate() {
                let (object, current) = (*object, current.as_ref());
                bases.push(terms.len());
                for field in &current.fields {
                    if !(1..=64).contains(&field.width) {
                        return Err("invalid constraint field width".into());
                    }
                    let name = terms.len().to_string();
                    let term = var(name.clone(), Sort::Bv(field.width));
                    let enabled =
                        !check_only && self.random_mode(object, field.offset, field.random);
                    if enabled {
                        projection.insert(name, term.clone());
                        active.push((terms.len(), object, field));
                        if !field.domain.is_empty() {
                            formula = and(
                                formula,
                                member(&term, field.width, field.domain.iter().copied()),
                            );
                        }
                        if field.cyclic {
                            if field.domain.is_empty() && field.width > 8
                                || field.domain.len() > 256
                            {
                                return Err("randc property with more than 256 values".into());
                            }
                            cyclic.push((object, field, term.clone()));
                        }
                    } else {
                        // Its known bits, with the mask of its X and Z bits.
                        let (value, bits) = self.field_bits(object, field);
                        formula = and(formula, eq(term.clone(), bv(field.width, value)));
                        unknown.fields.insert(terms.len(), bits);
                        state.insert(
                            name,
                            Scalar::Bv {
                                width: field.width,
                                value,
                            },
                        );
                    }
                    terms.push(term);
                }
                // The size of each array: a solver variable while the array
                // is random and a constraint names its size, then resized to
                // it; otherwise its size, kept (IEEE 1800-2023 18.4).
                size_bases.push(size_terms.len());
                for (index, array) in current.arrays.iter().enumerate() {
                    let contents = self.load(object, array.offset as usize, crate::HANDLE_BYTES);
                    let length = self.sequence_len(contents as Handle);
                    let enabled =
                        !check_only && self.random_mode(object, array.offset, array.random);
                    if enabled && !(1..=64).contains(&array.width) {
                        return Err("invalid random array element width".into());
                    }
                    let sized = enabled
                        && (constraints[position]
                            .iter()
                            .any(|constraint| constraint.names_size(index))
                            || position == 0
                                && arguments
                                    .iter()
                                    .flatten()
                                    .any(|argument| argument.names_size(index)));
                    let named = enabled
                        && (constraints[position]
                            .iter()
                            .any(|constraint| constraint.names_elements(index))
                            || position == 0
                                && arguments
                                    .iter()
                                    .flatten()
                                    .any(|argument| argument.names_elements(index)));
                    // The size of an array whose size and elements
                    // constraints name is solved first, and is then state
                    // to the constraints on the elements (18.5.7.1).
                    let solved_size = presized.get(&(object, index)).copied();
                    if sized && named && phase != Phase::Sizes && solved_size.is_none() {
                        unsized_arrays = true;
                    }
                    let length = solved_size.unwrap_or(length);
                    let sized = sized && solved_size.is_none();
                    let term = if sized {
                        let name = format!("s{}", size_terms.len());
                        let term = var(name.clone(), Sort::Bv(32));
                        projection.insert(name.clone(), term.clone());
                        formula = and(
                            formula,
                            node(Sort::Bool, "bvsle", vec![bv(32, 0), term.clone()]),
                        );
                        let capacity = crate::containers::Layout(array.layout).capacity();
                        if capacity != usize::MAX {
                            formula = and(
                                formula,
                                node(
                                    Sort::Bool,
                                    "bvule",
                                    vec![term.clone(), bv(32, capacity as u64)],
                                ),
                            );
                        }
                        preferred = and(
                            preferred,
                            node(
                                Sort::Bool,
                                "bvule",
                                vec![term.clone(), bv(32, PREFERRED_ARRAY_SIZE)],
                            ),
                        );
                        limited = and(
                            limited,
                            node(
                                Sort::Bool,
                                "bvule",
                                vec![term.clone(), bv(32, ARRAY_SIZE_LIMIT)],
                            ),
                        );
                        bounded = true;
                        Some(name)
                    } else {
                        None
                    };
                    size_terms.push(match &term {
                        Some(name) => var(name.clone(), Sort::Bv(32)),
                        None => {
                            arrays.sizes.insert((object, index), length);
                            bv(32, length as u64)
                        }
                    });
                    // A random array whose elements a constraint names has a
                    // solver variable for each (18.4, 18.5.8): its size is
                    // the one it has or a solve of sizes gave it.
                    if let (true, Some(name)) = (named && phase == Phase::Sizes, &term) {
                        staged.push((object, index, name.clone()));
                    }
                    let elements = if named && term.is_none() {
                        let base = terms.len();
                        for _ in 0..length {
                            let name = terms.len().to_string();
                            let element = var(name.clone(), Sort::Bv(array.width));
                            projection.insert(name, element.clone());
                            if !array.domain.is_empty() {
                                formula = and(
                                    formula,
                                    member(&element, array.width, array.domain.iter().copied()),
                                );
                            }
                            terms.push(element);
                        }
                        arrays.elements.insert((object, index), (base, length));
                        Some(base)
                    } else {
                        None
                    };
                    if enabled {
                        refills.push((object, array, term, length, elements));
                    }
                }
            }
            // A solve of sizes first, then of the rest with those sizes
            // (18.5.7.1), each with the members whose hooks ran.
            if unsized_arrays {
                self.prepared = Some((handle, participants));
                if !self.solve(
                    handle,
                    program,
                    check_only,
                    inputs,
                    limits.clone(),
                    Phase::Sizes,
                ) {
                    return Ok(false);
                }
                return Ok(self.solve(handle, program, check_only, inputs, limits, phase));
            }
            // The items of each distribution the call solves, with their
            // weights (18.5.3).
            let mut distributions: Vec<Vec<(Term, num_bigint::BigUint)>> = Vec::new();
            // The first solver variable of each object the call solves.
            let solved: fxhash::FxHashMap<Handle, (usize, &Program)> = members
                .iter()
                .zip(&bases)
                .map(|((object, current), &base)| (*object, (base, current.as_ref())))
                .collect();
            // The random variables in the arguments of the calls, which a
            // solve of arguments takes alone (18.5.11).
            // An element of an array an argument names, as `a[0]`, is its
            // solver variable. A solve of sizes takes no argument.
            let mut ordered = Vec::new();
            for argument in arguments.iter().flatten().filter(|_| phase != Phase::Sizes) {
                let argument = self.unroll(
                    handle,
                    program,
                    argument,
                    &mut Vec::new(),
                    &solved,
                    &arrays,
                    check_only,
                    inputs,
                )?;
                argument.variables(0, 0, &mut ordered);
            }
            let first = |names: &[String], value: &Expr| {
                phase != Phase::Arguments
                    || (names.iter().all(|name| ordered.contains(name))
                        && !value.reads_input_from(inputs.len()))
            };
            for ((((object, current), &base), &size_base), enabled) in members
                .iter()
                .zip(&bases)
                .zip(&size_bases)
                .zip(&constraints)
            {
                let sizes = &size_terms[size_base..size_base + current.arrays.len()];
                for constraint in enabled {
                    // A solve of sizes leaves the constraints on the
                    // elements whose sizes it solves to the solve of the
                    // rest (18.5.7.1).
                    if phase == Phase::Sizes
                        && (constraint.reads_input_from(inputs.len())
                            || staged.iter().any(|(staged, array, _)| {
                                staged == object && constraint.names_elements(*array)
                            }))
                    {
                        continue;
                    }
                    // The object's rank among those the call solves: the
                    // object of the call above those of its rand handles,
                    // a later one above an earlier one (18.5.13.1).
                    let rank = |priority: u64| {
                        let path = ranks.get(object).cloned().unwrap_or_else(|| vec![u32::MAX]);
                        (path, priority)
                    };
                    match constraint {
                        Expr::Soft { priority, value } => {
                            let value = self.unroll(
                                *object,
                                current,
                                value,
                                &mut Vec::new(),
                                &solved,
                                &arrays,
                                check_only,
                                inputs,
                            )?;
                            let mut names = Vec::new();
                            value.variables(base, size_base, &mut names);
                            if !first(&names, &value) {
                                continue;
                            }
                            // Its X and Z bits count once it is kept (see
                            // below).
                            let (term, mask) =
                                value.four_state(&terms, base, sizes, inputs, &unknown)?;
                            soft.push(Soft {
                                rank: rank(*priority),
                                unknown: mask.map(|mask| x_truth(&term, &mask)),
                                constraint: truth(term),
                                names,
                                // A soft distribution weights the solutions
                                // while it is kept (18.5.13).
                                distributions: weighted_items(&value, &terms, base, sizes, inputs)?,
                            });
                        }
                        Expr::DisableSoft {
                            priority,
                            variable,
                            condition,
                        } => {
                            // A member of an object the call solves through
                            // a rand handle is its solver variable.
                            let variable = self.unroll(
                                *object,
                                current,
                                variable,
                                &mut Vec::new(),
                                &solved,
                                &arrays,
                                check_only,
                                inputs,
                            )?;
                            let mut names = Vec::new();
                            variable.variables(base, size_base, &mut names);
                            let condition = match condition {
                                Some(condition) => {
                                    let condition = self.unroll(
                                        *object,
                                        current,
                                        condition,
                                        &mut Vec::new(),
                                        &solved,
                                        &arrays,
                                        check_only,
                                        inputs,
                                    )?;
                                    // A solve of arguments takes a guard that
                                    // names only their variables.
                                    let mut guard = Vec::new();
                                    condition.variables(base, size_base, &mut guard);
                                    if !first(&guard, &condition) {
                                        continue;
                                    }
                                    let (term, mask) = condition
                                        .four_state(&terms, base, sizes, inputs, &unknown)?;
                                    checks.extend(mask.map(|mask| x_truth(&term, &mask)));
                                    Some(truth(term))
                                }
                                None => None,
                            };
                            disabled.extend(
                                names
                                    .into_iter()
                                    .map(|name| (rank(*priority), name, condition.clone())),
                            );
                        }
                        constraint => {
                            let constraint = self.unroll(
                                *object,
                                current,
                                constraint,
                                &mut Vec::new(),
                                &solved,
                                &arrays,
                                check_only,
                                inputs,
                            )?;
                            let mut names = Vec::new();
                            constraint.variables(base, size_base, &mut names);
                            if !first(&names, &constraint) {
                                continue;
                            }
                            let (term, mask) =
                                constraint.four_state(&terms, base, sizes, inputs, &unknown)?;
                            checks.extend(mask.map(|mask| x_truth(&term, &mask)));
                            formula = and(formula, truth(term));
                            distributions.extend(weighted_items(
                                &constraint,
                                &terms,
                                base,
                                sizes,
                                inputs,
                            )?);
                        }
                    }
                }
            }
            // The soft constraints a `disable soft` of higher priority does
            // not discard (18.5.13.2), which alone read what they name; from
            // the highest priority down, each is kept when the constraints
            // kept so far allow it, and otherwise treated as true (18.5.13,
            // 18.5.13.1).
            // A guarded `disable soft` discards them while its condition
            // holds: the soft constraint is then `condition || constraint`.
            // A soft distribution applies, as the constraint, where no such
            // condition holds.
            let mut kept = Vec::new();
            'soft: for mut soft in soft {
                for (disable, name, condition) in &disabled {
                    if disable > &soft.rank && soft.names.contains(name) {
                        match condition {
                            None => continue 'soft,
                            Some(condition) => {
                                soft.constraint = node(
                                    Sort::Bool,
                                    "or",
                                    vec![condition.clone(), soft.constraint],
                                );
                                soft.unknown = soft
                                    .unknown
                                    .map(|unknown| and(not(condition.clone()), unknown));
                                for (item, _) in soft.distributions.iter_mut().flatten() {
                                    *item = node(
                                        Sort::Bool,
                                        "or",
                                        vec![condition.clone(), item.clone()],
                                    );
                                }
                            }
                        }
                    }
                }
                kept.push((soft.rank, soft.constraint, soft.distributions, soft.unknown));
            }
            // The solve of the rest keeps the solution of the arguments
            // (18.5.11).
            if phase == Phase::All
                && let Some(position) = self
                    .arguments
                    .iter()
                    .rposition(|arguments| arguments.object == handle)
            {
                let arguments = self.arguments.remove(position);
                for (name, value) in &arguments.variables {
                    let term = match name.strip_prefix('s') {
                        Some(index) => index.parse::<usize>().ok().and_then(|i| size_terms.get(i)),
                        None => name.parse::<usize>().ok().and_then(|i| terms.get(i)),
                    };
                    if let Some(term) = term
                        && let Sort::Bv(width) = term.0.sort
                    {
                        formula = and(formula, eq(term.clone(), bv(width, *value)));
                    }
                }
            }
            let seed = if projection.is_empty() {
                0
            } else {
                self.random_object(handle)
            };
            kept.sort_by(|a, b| b.0.cmp(&a.0));
            for (_, constraint, weighted, unknown) in kept {
                let attempt = and(formula.clone(), constraint);
                let bound = if bounded {
                    and(attempt.clone(), limited.clone())
                } else {
                    attempt.clone()
                };
                // A trial the solver cannot decide drops no preference: the
                // call fails with the reason, as a solve that cannot be
                // decided does.
                let outcome = solve_seeded(&bound, &projection, limits.clone(), seed);
                // A discarded one has no effect on the distribution of
                // the solutions (18.5.13).
                match outcome.verdict {
                    Verdict::Sat => {
                        formula = attempt;
                        distributions.extend(weighted);
                        checks.extend(unknown);
                    }
                    Verdict::Unsat => {}
                    Verdict::Unknown => {
                        return Err(outcome
                            .reason
                            .unwrap_or_else(|| "soft constraint solving incomplete".into()));
                    }
                }
            }
            // A constraint or guard whose truth X or Z bits decide without
            // the solution is an error; the others are checked against it.
            let mut deferred = Vec::new();
            for check in checks {
                match evaluate_check(&check, &state) {
                    Some(true) => return Err(UNKNOWN_VALUE.into()),
                    Some(false) => {}
                    None => deferred.push(check),
                }
            }
            // A solve of arguments gives the values of the variables in
            // them and of each argument, each in a variable of its own, and
            // changes nothing (18.5.11); it is solved as every phase is,
            // below, so that the weights and soft constraints shape it too.
            if phase == Phase::Arguments {
                let sizes = &size_terms[..program.arrays.len().min(size_terms.len())];
                for (index, argument) in arguments.iter().enumerate() {
                    let Some(argument) = argument else {
                        continue;
                    };
                    let argument = self.unroll(
                        handle,
                        program,
                        argument,
                        &mut Vec::new(),
                        &solved,
                        &arrays,
                        check_only,
                        inputs,
                    )?;
                    let term = argument.term(&terms, 0, sizes, inputs)?;
                    let Sort::Bv(width) = term.0.sort else {
                        return Err("a function argument other than a vector".into());
                    };
                    let name = format!("a{index}");
                    let value = var(name.clone(), Sort::Bv(width));
                    formula = and(formula, eq(value.clone(), term));
                    projection.insert(name, value);
                }
            }
            // Initial integration uses seeded SAT search, not hash-cell acceptance.
            // It finds a valid model without claiming uniform sampling (#2187).
            // A randc property takes a value its cycle has not produced yet
            // (IEEE 1800-2023 18.4.2). A cycle restarts only when the
            // constraints leave it no value: first those cycles exhausted on
            // their own, then, while the remaining values of the others
            // still exclude every solution, one more cycle at a time.
            // A restart takes effect only when the call succeeds.
            let restriction =
                |heap: &mut Self, object: Handle, field: &Field, term: &Term, restart: bool| {
                    let remaining = if restart {
                        (0..cycle_size(field)).collect()
                    } else {
                        heap.cycle_remaining(object, field.offset, cycle_size(field), false)
                    };
                    member(
                        term,
                        field.width,
                        remaining.into_iter().map(|entry| cycle_value(field, entry)),
                    )
                };
            // A size within the preferred bound is sought first.
            // Without `restart`, no randc cycle restarts.
            let run = |heap: &mut Self, formula: &Term, restart: bool| {
                let mut restarted = vec![false; cyclic.len()];
                let solve_with = |heap: &mut Self, restarted: &[bool]| {
                    let mut attempt = formula.clone();
                    for ((object, field, term), restart) in cyclic.iter().zip(restarted) {
                        attempt = and(attempt, restriction(heap, *object, field, term, *restart));
                    }
                    solve_seeded(&attempt, &projection, limits.clone(), seed)
                };
                let mut outcome = solve_with(heap, &restarted);
                if restart && matches!(outcome.verdict, Verdict::Unsat) && !cyclic.is_empty() {
                    for (position, (object, field, term)) in cyclic.iter().enumerate() {
                        let alone = and(
                            formula.clone(),
                            restriction(heap, *object, field, term, false),
                        );
                        restarted[position] = matches!(
                            solve_seeded(&alone, &projection, limits.clone(), seed).verdict,
                            Verdict::Unsat
                        );
                    }
                    let mut next = 0;
                    loop {
                        if restarted.iter().any(|restart| *restart) {
                            outcome = solve_with(heap, &restarted);
                            if !matches!(outcome.verdict, Verdict::Unsat) {
                                break;
                            }
                        }
                        let Some(position) = (next..cyclic.len()).find(|&index| !restarted[index])
                        else {
                            break;
                        };
                        restarted[position] = true;
                        next = position + 1;
                    }
                }
                (outcome, restarted)
            };
            // Sizes up to the preferred bound are sought first, then any
            // supported size, each before a randc cycle restarts (18.4.2).
            let mut tiers = if bounded {
                vec![
                    and(formula.clone(), preferred),
                    and(formula.clone(), limited),
                ]
            } else {
                vec![formula.clone()]
            };
            // An item of each distribution, picked by its weight, is tried
            // first; the weights never make the call fail (18.5.3).
            let mut weighted = 0;
            if !distributions.is_empty() {
                let mut state = seed ^ 0x9e37_79b9_7f4a_7c15;
                let mut next = || {
                    // splitmix64
                    state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
                    let mut z = state;
                    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
                    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
                    z ^ (z >> 31)
                };
                let mut picked = boolv(true);
                for items in &distributions {
                    let total = items
                        .iter()
                        .map(|(_, weight)| weight)
                        .sum::<num_bigint::BigUint>();
                    if total == num_bigint::BigUint::ZERO {
                        continue;
                    }
                    // A point drawn with 64 bits more than the total, so
                    // that each is as likely as the weights say to 1 part
                    // in 2^64.
                    let words = total.bits().div_ceil(64) + 1;
                    let bytes = (0..words)
                        .flat_map(|_| next().to_le_bytes())
                        .collect::<Vec<_>>();
                    let mut point = num_bigint::BigUint::from_bytes_le(&bytes) % &total;
                    for (predicate, weight) in items {
                        if point < *weight {
                            picked = and(picked, predicate.clone());
                            break;
                        }
                        point -= weight;
                    }
                }
                let preferred: Vec<Term> = tiers
                    .iter()
                    .map(|tier| and(tier.clone(), picked.clone()))
                    .collect();
                weighted = preferred.len();
                tiers.splice(0..0, preferred);
            }
            let restarts: &[bool] = if cyclic.is_empty() {
                &[false]
            } else {
                &[false, true]
            };
            let (outcome, restarted) = search(
                tiers.len(),
                weighted,
                restarts,
                |tier, restart| run(self, &tiers[tier], restart),
                |attempt| &attempt.0.verdict,
            );
            // Only sizes above the supported ones satisfy the constraints.
            if bounded
                && matches!(outcome.verdict, Verdict::Unsat)
                && matches!(run(self, &formula, true).0.verdict, Verdict::Sat)
            {
                return Err(format!(
                    "random array sizes above the limit of {ARRAY_SIZE_LIMIT}"
                ));
            }
            match outcome.verdict {
                Verdict::Unsat => Ok(false),
                Verdict::Unknown => Err(outcome
                    .reason
                    .unwrap_or_else(|| "constraint solving incomplete".into())),
                Verdict::Sat if phase == Phase::Arguments => {
                    if decided_by_unknown(&deferred, &state, &outcome) {
                        return Err(UNKNOWN_VALUE.into());
                    }
                    let model = &outcome.context_values;
                    let value = |name: &str| match model.get(name) {
                        Some(Scalar::Bv { value, .. }) => Some(*value),
                        _ => None,
                    };
                    let variables = ordered
                        .iter()
                        .filter_map(|name| Some((name.clone(), value(name)?)))
                        .collect();
                    let values = (0..arguments.len())
                        .map(|index| value(&format!("a{index}")).unwrap_or(0))
                        .collect();
                    self.arguments.push(Arguments {
                        object: handle,
                        variables,
                        values,
                    });
                    if !presized.is_empty() {
                        self.staged.push(Staged {
                            object: handle,
                            kept: true,
                            sizes: presized,
                        });
                    }
                    Ok(true)
                }
                Verdict::Sat if phase == Phase::Sizes => {
                    // The sizes, for the solve of the rest, which changes
                    // the object.
                    let model = &outcome.context_values;
                    let mut sizes = fxhash::FxHashMap::default();
                    for (object, array, name) in &staged {
                        let Some(Scalar::Bv { value, .. }) = model.get(name) else {
                            return Err("missing constraint array size".into());
                        };
                        if *value > ARRAY_SIZE_LIMIT {
                            return Err(format!(
                                "random array size {value} above the limit of {ARRAY_SIZE_LIMIT}"
                            ));
                        }
                        sizes.insert((*object, *array), *value as usize);
                    }
                    self.staged.push(Staged {
                        object: handle,
                        kept: false,
                        sizes,
                    });
                    self.prepared = Some((handle, participants));
                    Ok(true)
                }
                Verdict::Sat => {
                    if decided_by_unknown(&deferred, &state, &outcome) {
                        return Err(UNKNOWN_VALUE.into());
                    }
                    let model = &outcome.context_values;
                    let updates = active
                        .into_iter()
                        .map(
                            |(index, object, field)| match model.get(&index.to_string()) {
                                Some(Scalar::Bv { value, .. }) => Ok((object, field, *value)),
                                _ => Err("missing constraint field value".to_owned()),
                            },
                        )
                        .collect::<Result<Vec<_>, _>>()?;
                    // The size of each random array, checked like every
                    // other value before anything changes.
                    let resized = refills
                        .into_iter()
                        .map(|(object, array, size, length, elements)| {
                            let length = match size {
                                Some(name) => match model.get(&name) {
                                    Some(Scalar::Bv { value, .. }) => *value,
                                    _ => return Err("missing constraint array size".to_owned()),
                                },
                                None => length as u64,
                            };
                            if length > ARRAY_SIZE_LIMIT {
                                return Err(format!(
                                    "random array size {length} above the limit of {ARRAY_SIZE_LIMIT}"
                                ));
                            }
                            // The solved elements, each of the model.
                            let solved = match elements {
                                Some(base) => Some(
                                    (base..base + length as usize)
                                        .map(|index| match model.get(&index.to_string()) {
                                            Some(Scalar::Bv { value, .. }) => Ok(*value),
                                            _ => Err("missing constraint array element".to_owned()),
                                        })
                                        .collect::<Result<Vec<_>, _>>()?,
                                ),
                                None => None,
                            };
                            Ok((object, array, length, solved))
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    for ((object, field, _), restart) in cyclic.iter().zip(&restarted) {
                        if *restart {
                            self.cycle_remaining(*object, field.offset, cycle_size(field), true);
                        }
                    }
                    for (object, field, value) in &updates {
                        if field.cyclic
                            && let Some(entry) = cycle_index(field, *value)
                        {
                            self.take_cycle_entry(*object, field.offset, cycle_size(field), entry);
                        }
                    }
                    for (object, field, value) in updates {
                        self.store_field(object, field, value);
                    }
                    // Each random array, resized to its size, gets the solved
                    // elements, or random ones when no constraint names them.
                    for (object, array, length, solved) in resized {
                        let mask = u64::MAX >> (64 - array.width);
                        let elements = solved.unwrap_or_else(|| {
                            (0..length)
                                .map(|_| {
                                    let draw = self.random_object(object);
                                    if array.domain.is_empty() {
                                        draw & mask
                                    } else {
                                        array.domain[(draw % array.domain.len() as u64) as usize]
                                    }
                                })
                                .collect()
                        });
                        let contents =
                            self.load(object, array.offset as usize, crate::HANDLE_BYTES);
                        let contents =
                            self.random_sequence(contents as Handle, array.layout, elements);
                        self.store(
                            object,
                            array.offset as usize,
                            crate::HANDLE_BYTES,
                            contents.into(),
                        );
                    }
                    self.solved = participants;
                    Ok(true)
                }
            }
        })();
        match result {
            Ok(success) => success,
            Err(reason) => {
                self.fault
                    .get_or_insert(Fault::ConstraintRandomization(reason));
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weighted_attempts_give_way_when_undecided() {
        // Tiers 0 and 1 add the picked items of distributions: one the
        // solver cannot decide goes on to the next tier, as an
        // unsatisfiable one does, while an undecided unweighted tier
        // decides the call (IEEE 1800-2023 18.5.3).
        let verdicts = [Verdict::Unknown, Verdict::Unsat, Verdict::Sat];
        let mut tried = Vec::new();
        let found = search(
            3,
            2,
            &[false],
            |tier, _| {
                tried.push(tier);
                (tier, verdicts[tier])
            },
            |attempt| &attempt.1,
        );
        assert_eq!((found.0, tried), (2, vec![0, 1, 2]));
        let verdicts = [Verdict::Unsat, Verdict::Unknown, Verdict::Sat];
        let found = search(
            3,
            1,
            &[false],
            |tier, _| (tier, verdicts[tier]),
            |attempt| &attempt.1,
        );
        assert_eq!(found.0, 1);
    }

    fn program() -> Program {
        Program {
            fields: vec![Field {
                offset: 0,
                width: 4,
                random: true,
                domain: Vec::new(),
                cyclic: false,
                four_state: false,
            }],
            constraints: vec![Expr::Binary {
                op: "<".into(),
                left: Box::new(Expr::Field(0)),
                right: Box::new(Expr::Literal(4, 3)),
                signed: false,
            }],
            ..Default::default()
        }
    }

    /// The random 4-bit field at 0 of [`program`].
    fn program_fields() -> Vec<Field> {
        program().fields
    }

    /// Soft constraints are kept from the highest priority down while the
    /// hard constraints and those kept allow them, and are otherwise true;
    /// a `disable soft` discards the lower ones that name its variable
    /// (IEEE 1800-2023 18.5.13); a call with only soft constraints succeeds.
    #[test]
    fn soft_constraints_keep_the_highest_priorities_they_can() {
        let mut heap = Heap::new();
        let object = heap.new_object(1, 8);
        let field = |op: &str, value| Expr::Binary {
            op: op.into(),
            left: Box::new(Expr::Field(0)),
            right: Box::new(Expr::Literal(4, value)),
            signed: false,
        };
        let soft = |priority, value| Expr::Soft {
            priority,
            value: Box::new(value),
        };
        let mut program = Program {
            fields: program_fields(),
            constraints: vec![
                soft(1, field("==", 3)),
                soft(2, field("==", 5)),
                soft(3, field(">", 4)),
                field("<", 8),
            ],
            ..Program::default()
        };
        let value = |heap: &mut Heap, program: &Program| {
            assert!(heap.randomize_object(object, program, false));
            heap.load(object, 0, 1)
        };
        assert_eq!(value(&mut heap, &program), 5);
        // A hard constraint against every soft one discards them all.
        program.constraints.push(field("==", 0));
        assert_eq!(value(&mut heap, &program), 0);
        program.constraints.pop();
        // `disable soft` above priority 2 leaves only `x > 4`.
        program.constraints.push(Expr::DisableSoft {
            priority: 3,
            variable: Box::new(Expr::Field(0)),
            condition: None,
        });
        for _ in 0..8 {
            assert!((5..8).contains(&value(&mut heap, &program)));
        }
        // Soft constraints that contradict each other never fail a call.
        program.constraints = vec![soft(1, field("==", 3)), soft(2, field("==", 4))];
        assert_eq!(value(&mut heap, &program), 4);
        assert_eq!(heap.take_fault(), None);
    }

    /// A solution of a 4-state field has no X or Z bits, and a constraint
    /// that reads a 4-state state value with X or Z bits is an error (IEEE
    /// 1800-2023 18.3); one no constraint reads may have them.
    #[test]
    fn four_state_fields_take_two_state_values() {
        let mut heap = Heap::new();
        let object = heap.new_object(1, 8);
        let field = |offset, random| Field {
            offset,
            width: 8,
            random,
            domain: Vec::new(),
            cyclic: false,
            four_state: true,
        };
        let program = Program {
            // Each mask byte follows its value byte. The field at 4, which
            // no constraint reads, stays X.
            fields: vec![field(0, true), field(2, false), field(4, false)],
            constraints: vec![Expr::Binary {
                op: "==".into(),
                left: Box::new(Expr::Field(0)),
                right: Box::new(Expr::Field(1)),
                signed: false,
            }],
            ..Program::default()
        };
        // Each field holds the complements of its planes: zeroed bytes are X.
        heap.store(object, 2, 1, !7 & 0xff);
        heap.store(object, 3, 1, 0xff);
        assert!(heap.randomize_object(object, &program, false));
        assert_eq!(
            (heap.load(object, 0, 1), heap.load(object, 1, 1)),
            (!7 & 0xff, 0xff)
        );
        assert_eq!((heap.load(object, 4, 1), heap.load(object, 5, 1)), (0, 0));
        heap.store(object, 3, 1, 0xfe);
        assert!(!heap.randomize_object(object, &program, false));
        assert!(matches!(
            heap.take_fault(),
            Some(Fault::ConstraintRandomization(reason)) if reason.contains("X or Z")
        ));
    }

    /// A guard decided before solving reads a 4-state state field by its
    /// value, not by the complemented planes it holds (IEEE 1800-2023 18.3,
    /// 18.5.13): `if (state == 0) x == 1; else x == 2;`.
    #[test]
    fn guards_read_four_state_fields_by_their_values() {
        let mut heap = Heap::new();
        let object = heap.new_object(1, 8);
        let compare = |op: &str, left: Expr, right: Expr| Expr::Binary {
            op: op.into(),
            left: Box::new(left),
            right: Box::new(right),
            signed: false,
        };
        let state = || compare("==", Expr::Field(1), Expr::Literal(4, 0));
        let program = Program {
            fields: vec![
                Field {
                    offset: 0,
                    width: 8,
                    random: true,
                    domain: Vec::new(),
                    cyclic: false,
                    four_state: false,
                },
                Field {
                    offset: 2,
                    width: 4,
                    random: false,
                    domain: Vec::new(),
                    cyclic: false,
                    four_state: true,
                },
            ],
            constraints: vec![
                compare(
                    "||",
                    Expr::Unary {
                        op: "!".into(),
                        value: Box::new(state()),
                    },
                    compare("==", Expr::Field(0), Expr::Literal(8, 1)),
                ),
                compare(
                    "||",
                    state(),
                    compare("==", Expr::Field(0), Expr::Literal(8, 2)),
                ),
            ],
            ..Program::default()
        };
        // state = 0: the complements of the value 0 and of a known mask.
        heap.store(object, 2, 1, 0xf);
        heap.store(object, 3, 1, 0xf);
        for _ in 0..8 {
            assert!(heap.randomize_object(object, &program, false));
            assert_eq!(heap.load(object, 0, 1), 1);
        }
        // state = 3.
        heap.store(object, 2, 1, !3 & 0xf);
        assert!(heap.randomize_object(object, &program, false));
        assert_eq!(heap.load(object, 0, 1), 2);
        // An X guard is no value: the call fails as it reads it.
        heap.store(object, 3, 1, 0);
        assert!(!heap.randomize_object(object, &program, false));
        assert!(matches!(
            heap.take_fault(),
            Some(Fault::ConstraintRandomization(reason)) if reason.contains("X or Z")
        ));
    }

    /// A select of a 4-state state field reads only the bits it selects
    /// (IEEE 1800-2023 11.5.1): known ones are its value, an X or Z one is
    /// an error (18.3).
    #[test]
    fn four_state_selects_read_only_their_bits() {
        let mut heap = Heap::new();
        let object = heap.new_object(1, 8);
        let resize = |value: Expr, width| Expr::Resize {
            value: Box::new(value),
            width,
            signed: false,
        };
        let shifted = |shift| Expr::Binary {
            op: ">>".into(),
            left: Box::new(Expr::Field(1)),
            right: Box::new(Expr::Literal(32, shift)),
            signed: false,
        };
        let program = |selected: Expr| Program {
            fields: vec![
                Field {
                    offset: 0,
                    width: 8,
                    random: true,
                    domain: Vec::new(),
                    cyclic: false,
                    four_state: false,
                },
                Field {
                    offset: 2,
                    width: 8,
                    random: false,
                    domain: Vec::new(),
                    cyclic: false,
                    four_state: true,
                },
            ],
            constraints: vec![Expr::Binary {
                op: "==".into(),
                left: Box::new(Expr::Field(0)),
                right: Box::new(selected),
                signed: false,
            }],
            ..Program::default()
        };
        let fails = |heap: &mut Heap, program: &Program| {
            !heap.randomize_object(object, program, false)
                && matches!(
                    heap.take_fault(),
                    Some(Fault::ConstraintRandomization(reason)) if reason.contains("X or Z")
                )
        };
        // state = 8'bxxxx_0101: the complements of the value and of the mask.
        heap.store(object, 2, 1, !0x05 & 0xff);
        heap.store(object, 3, 1, !0xf0 & 0xff);
        // x == state[3:0]
        let low = program(resize(resize(Expr::Field(1), 4), 8));
        assert!(heap.randomize_object(object, &low, false));
        assert_eq!(heap.load(object, 0, 1), 5);
        // x == state[7:4] and x == state read X bits.
        assert!(fails(&mut heap, &program(resize(resize(shifted(4), 4), 8))));
        assert!(fails(&mut heap, &program(Expr::Field(1))));
        // state = 8'b1010_xxxx: x == state[7:4]
        heap.store(object, 2, 1, !0xa0 & 0xff);
        heap.store(object, 3, 1, !0x0f & 0xff);
        let high = program(resize(resize(shifted(4), 4), 8));
        assert!(heap.randomize_object(object, &high, false));
        assert_eq!(heap.load(object, 0, 1), 0xa);
        assert!(fails(&mut heap, &low));
    }

    /// A 4-state input of a call has its known bits and the mask of its X
    /// and Z bits (see [`Program::unknown_inputs`]), which operations carry
    /// by their 4-state semantics (IEEE 1800-2023 11.4): a constraint or a
    /// guard whose truth they do not decide, as through a shift, a
    /// narrowing, a mask or a decided `&&`, takes its value; one whose truth
    /// they decide is an error (18.3).
    #[test]
    fn four_state_inputs_take_four_state_semantics() {
        let mut heap = Heap::new();
        let object = heap.new_object(1, 1);
        let binary = |op: &str, left: Expr, right: Expr| Expr::Binary {
            op: op.into(),
            left: Box::new(left),
            right: Box::new(right),
            signed: false,
        };
        let input = || Expr::Input { index: 0, width: 8 };
        let x = || Expr::Field(0);
        let program = |constraints| Program {
            fields: vec![Field {
                offset: 0,
                width: 8,
                random: true,
                domain: Vec::new(),
                cyclic: false,
                four_state: false,
            }],
            constraints,
            unknown_inputs: vec![(0, 1)],
            ..Program::default()
        };
        let fails = |heap: &mut Heap, program: &Program, inputs: &[u64]| {
            !heap.randomize_object_with_inputs(object, program, false, inputs)
                && matches!(
                    heap.take_fault(),
                    Some(Fault::ConstraintRandomization(reason)) if reason.contains("X or Z")
                )
        };
        // x == base >> 4 and x == (base & 8'hf0) with base = 8'b1010_xxxx.
        let shifted = program(vec![binary(
            "==",
            x(),
            binary(">>", input(), Expr::Literal(32, 4)),
        )]);
        let masked = program(vec![binary(
            "==",
            x(),
            binary("&", input(), Expr::Literal(8, 0xf0)),
        )]);
        let narrowed = program(vec![binary(
            "==",
            x(),
            Expr::Resize {
                value: Box::new(Expr::Resize {
                    value: Box::new(input()),
                    width: 4,
                    signed: false,
                }),
                width: 8,
                signed: false,
            },
        )]);
        assert!(heap.randomize_object_with_inputs(object, &shifted, false, &[0xa0, 0x0f]));
        assert_eq!(heap.load(object, 0, 1), 0xa);
        assert!(heap.randomize_object_with_inputs(object, &masked, false, &[0xa0, 0x0f]));
        assert_eq!(heap.load(object, 0, 1), 0xa0);
        assert!(fails(&mut heap, &narrowed, &[0xa0, 0x0f]));
        // base = 8'bxxxx_0101: its low bits only.
        assert!(heap.randomize_object_with_inputs(object, &narrowed, false, &[0x05, 0xf0]));
        assert_eq!(heap.load(object, 0, 1), 5);
        assert!(fails(&mut heap, &shifted, &[0x05, 0xf0]));
        // `if (base >> 7) x == 1; else x == 2;` with base = 8'b0xxx_xxxx:
        // the guard reads only bit 7.
        let guard = || {
            binary(
                "!=",
                binary(">>", input(), Expr::Literal(32, 7)),
                Expr::Literal(8, 0),
            )
        };
        let guarded = program(vec![
            binary(
                "||",
                Expr::Unary {
                    op: "!".into(),
                    value: Box::new(guard()),
                },
                binary("==", x(), Expr::Literal(8, 1)),
            ),
            binary("||", guard(), binary("==", x(), Expr::Literal(8, 2))),
        ]);
        assert!(heap.randomize_object_with_inputs(object, &guarded, false, &[0x00, 0x7f]));
        assert_eq!(heap.load(object, 0, 1), 2);
        assert!(heap.randomize_object_with_inputs(object, &guarded, false, &[0x80, 0x7f]));
        assert_eq!(heap.load(object, 0, 1), 1);
        assert!(fails(&mut heap, &guarded, &[0x00, 0x80]));
        // `if ((base >> 7) && 1'b0) x == 1;` with X in bit 7: a known 0
        // decides the guard; with `1'b1` the X does, as an X `x == base`
        // decides nothing while it is not the value of a constraint.
        let decided = |right| {
            program(vec![
                binary(
                    "||",
                    Expr::Unary {
                        op: "!".into(),
                        value: Box::new(binary(
                            "&&",
                            binary(">>", input(), Expr::Literal(32, 7)),
                            Expr::Literal(1, right),
                        )),
                    },
                    binary("==", x(), Expr::Literal(8, 1)),
                ),
                binary("==", x(), Expr::Literal(8, 2)),
            ])
        };
        assert!(heap.randomize_object_with_inputs(object, &decided(0), false, &[0x00, 0x80]));
        assert_eq!(heap.load(object, 0, 1), 2);
        assert!(fails(&mut heap, &decided(1), &[0x00, 0x80]));
        // x == (base & ~8'h0f): the mask leaves out the X bits.
        let inverted = program(vec![binary(
            "==",
            x(),
            binary(
                "&",
                input(),
                Expr::Unary {
                    op: "~".into(),
                    value: Box::new(Expr::Literal(8, 0x0f)),
                },
            ),
        )]);
        assert!(heap.randomize_object_with_inputs(object, &inverted, false, &[0xa0, 0x0f]));
        assert_eq!(heap.load(object, 0, 1), 0xa0);
        assert!(fails(&mut heap, &inverted, &[0xa0, 0xf0]));
    }

    /// A soft constraint that a `disable soft` of higher priority discards
    /// reads nothing (IEEE 1800-2023 18.5.13.2): an X state value in it is
    /// no error (18.3).
    #[test]
    fn discarded_soft_constraints_read_no_unknown_values() {
        let mut heap = Heap::new();
        let object = heap.new_object(1, 8);
        let soft = Expr::Soft {
            priority: 0,
            value: Box::new(Expr::Binary {
                op: "==".into(),
                left: Box::new(Expr::Field(0)),
                right: Box::new(Expr::Field(1)),
                signed: false,
            }),
        };
        let program = |constraints| Program {
            fields: vec![
                Field {
                    offset: 0,
                    width: 8,
                    random: true,
                    domain: Vec::new(),
                    cyclic: false,
                    four_state: false,
                },
                Field {
                    offset: 2,
                    width: 8,
                    random: false,
                    domain: Vec::new(),
                    cyclic: false,
                    four_state: true,
                },
            ],
            constraints,
            ..Program::default()
        };
        // state is all X: zeroed planes.
        let disabled = program(vec![
            soft.clone(),
            Expr::DisableSoft {
                priority: 1,
                variable: Box::new(Expr::Field(0)),
                condition: None,
            },
        ]);
        assert!(heap.randomize_object(object, &disabled, false));
        assert!(!heap.randomize_object(object, &program(vec![soft]), false));
        assert!(matches!(
            heap.take_fault(),
            Some(Fault::ConstraintRandomization(reason)) if reason.contains("X or Z")
        ));
    }

    /// A solve of arguments solves the variables in them by the constraints
    /// that name only them, gives the values of the arguments and changes
    /// nothing; the solve of the rest keeps those variables, through a
    /// nested randomize call, and takes the value of the call as an input
    /// (IEEE 1800-2023 18.5.11).
    #[test]
    fn functions_in_constraints_solve_their_arguments_first() {
        let mut heap = Heap::new();
        let object = heap.new_object(1, 8);
        let field = |offset| Field {
            offset,
            width: 8,
            random: true,
            domain: Vec::new(),
            cyclic: false,
            four_state: false,
        };
        let compare = |op: &str, left: Expr, right: Expr| Expr::Binary {
            op: op.into(),
            left: Box::new(left),
            right: Box::new(right),
            signed: false,
        };
        let program = Program {
            fields: vec![field(0), field(1)],
            constraints: vec![
                compare("<", Expr::Field(0), Expr::Literal(8, 4)),
                compare(">", Expr::Field(0), Expr::Literal(8, 2)),
                compare("==", Expr::Field(1), Expr::Input { index: 0, width: 8 }),
            ],
            arguments: vec![compare("+", Expr::Field(0), Expr::Literal(8, 1))],
            ..Program::default()
        };
        heap.store(object, 0, 1, 9);
        assert!(heap.solve_arguments(object, &program, false, &[]));
        assert_eq!(heap.argument_value(0), 4);
        assert_eq!(heap.load(object, 0, 1), 9);
        // A function that randomizes another object with a function in its
        // constraints leaves the solution of this call's arguments as it
        // was.
        let nested = heap.new_object(1, 8);
        let inner = Program {
            fields: vec![field(0)],
            constraints: vec![compare("==", Expr::Field(0), Expr::Literal(8, 7))],
            arguments: vec![Expr::Field(0)],
            ..Program::default()
        };
        assert!(heap.solve_arguments(nested, &inner, false, &[]));
        assert_eq!(heap.argument_value(0), 7);
        assert!(heap.randomize_object_with_inputs(nested, &inner, false, &[]));
        assert_eq!(heap.load(nested, 0, 1), 7);
        assert_eq!(heap.argument_value(0), 4);
        assert!(heap.randomize_object_with_inputs(object, &program, false, &[40]));
        assert_eq!(heap.load(object, 0, 1), 3);
        assert_eq!(heap.load(object, 1, 1), 40);
        assert_eq!(heap.take_fault(), None);
    }

    /// A call orders its solves: the sizes of the arrays whose size and
    /// elements constraints name (18.5.7.1), then the arguments of the
    /// functions its constraints call, an element `a[0]` among them
    /// (18.5.11), then the rest, both with those sizes.
    #[test]
    fn sizes_come_before_arguments_and_elements() {
        let mut heap = Heap::new();
        let object = heap.new_object(1, 16);
        let layout = crate::containers::parameter(8, true, 0, 0, 0, false, false, false);
        let compare = |op: &str, left: Expr, right: Expr| Expr::Binary {
            op: op.into(),
            left: Box::new(left),
            right: Box::new(right),
            signed: false,
        };
        let element = || Expr::ElementAt {
            array: 0,
            index: 0,
            width: 8,
        };
        let program = Program {
            fields: vec![Field {
                offset: 0,
                width: 8,
                random: true,
                domain: Vec::new(),
                cyclic: false,
                four_state: false,
            }],
            arrays: vec![Array {
                offset: 8,
                layout,
                width: 8,
                random: true,
                domain: Vec::new(),
            }],
            constraints: vec![
                compare(
                    "&&",
                    compare(">", Expr::Size(0), Expr::Literal(32, 1)),
                    compare("<", Expr::Size(0), Expr::Literal(32, 5)),
                ),
                compare(
                    "&&",
                    compare(">", element(), Expr::Literal(8, 3)),
                    compare("<", element(), Expr::Literal(8, 6)),
                ),
                // The value of a call of `id(a[0])`.
                compare("==", Expr::Field(0), Expr::Input { index: 0, width: 8 }),
            ],
            arguments: vec![element()],
            ..Program::default()
        };
        for _ in 0..8 {
            assert!(heap.solve_arguments(object, &program, false, &[]));
            let argument = heap.argument_value(0);
            assert!((4..6).contains(&argument));
            assert!(heap.randomize_object_with_inputs(object, &program, false, &[argument]));
            let contents = heap.load(object, 8, crate::HANDLE_BYTES) as Handle;
            assert!((2..5).contains(&heap.sequence_len(contents)));
            assert_eq!(heap.sequence_element(contents, 0), Some(argument));
            assert_eq!(heap.load(object, 0, 1), argument);
        }
        assert_eq!(heap.take_fault(), None);
    }

    /// A randomize call between the solve of arguments and the solve of the
    /// rest of another call, as a function its constraints call makes,
    /// leaves the sizes that call solved for it.
    #[test]
    fn nested_calls_leave_staged_sizes() {
        let mut heap = Heap::new();
        let layout = crate::containers::parameter(8, true, 0, 0, 0, false, false, false);
        let compare = |op: &str, left: Expr, right: Expr| Expr::Binary {
            op: op.into(),
            left: Box::new(left),
            right: Box::new(right),
            signed: false,
        };
        let program = Program {
            fields: vec![Field {
                offset: 0,
                width: 32,
                random: true,
                domain: Vec::new(),
                cyclic: false,
                four_state: false,
            }],
            arrays: vec![Array {
                offset: 8,
                layout,
                width: 8,
                random: true,
                domain: Vec::new(),
            }],
            constraints: vec![
                compare(
                    "&&",
                    compare(">", Expr::Size(0), Expr::Literal(32, 1)),
                    compare("<", Expr::Size(0), Expr::Literal(32, 9)),
                ),
                compare(
                    "==",
                    Expr::ElementAt {
                        array: 0,
                        index: 0,
                        width: 8,
                    },
                    Expr::Literal(8, 7),
                ),
                // The value of a call of `f(a.size())`.
                compare(
                    "==",
                    Expr::Field(0),
                    Expr::Input {
                        index: 0,
                        width: 32,
                    },
                ),
            ],
            arguments: vec![Expr::Size(0)],
            ..Program::default()
        };
        let (outer, inner) = (heap.new_object(1, 16), heap.new_object(1, 16));
        for _ in 0..8 {
            assert!(heap.solve_arguments(outer, &program, false, &[]));
            let size = heap.argument_value(0);
            // The function randomizes another object of the class.
            assert!(heap.solve_arguments(inner, &program, false, &[]));
            let inner_size = heap.argument_value(0);
            assert!(heap.randomize_object_with_inputs(inner, &program, false, &[inner_size]));
            assert!(heap.randomize_object_with_inputs(outer, &program, false, &[size]));
            let contents = heap.load(outer, 8, crate::HANDLE_BYTES) as Handle;
            assert_eq!(heap.sequence_len(contents) as u64, size);
            assert_eq!(heap.load(outer, 0, 4), size);
        }
        assert_eq!(heap.take_fault(), None);
    }

    /// A soft constraint whose trial the solver cannot decide fails the
    /// call with the reason, rather than being dropped (18.5.13).
    #[test]
    fn undecided_soft_constraints_fail_the_call() {
        let mut heap = Heap::new();
        let object = heap.new_object(1, 8);
        let mut value = Expr::Literal(1, 0);
        for constant in 0..200 {
            value = Expr::Binary {
                op: "||".into(),
                left: Box::new(value),
                right: Box::new(Expr::Binary {
                    op: "==".into(),
                    left: Box::new(Expr::Field(0)),
                    right: Box::new(Expr::Literal(4, constant % 16)),
                    signed: false,
                }),
                signed: false,
            };
        }
        let program = Program {
            fields: program_fields(),
            constraints: vec![Expr::Soft {
                priority: 1,
                value: Box::new(value),
            }],
            ..Program::default()
        };
        let limits = Limits {
            max_terms: 64,
            ..Limits::default()
        };
        assert!(!heap.randomize_object_with_limits(object, &program, false, &[], limits));
        assert!(matches!(
            heap.take_fault(),
            Some(Fault::ConstraintRandomization(_))
        ));
    }

    /// A size that only the guard of a `disable soft` reads is a solver
    /// variable, as one any guard reads (IEEE 1800-2023 18.4, 18.5.13.2):
    /// the soft constraint holds wherever the guard does not.
    #[test]
    fn disable_soft_guards_name_array_sizes() {
        let mut heap = Heap::new();
        let object = heap.new_object(1, 16);
        let layout = crate::containers::parameter(8, true, 0, 0, 0, false, false, false);
        let program = Program {
            fields: program_fields(),
            arrays: vec![Array {
                offset: 8,
                layout,
                width: 8,
                random: true,
                domain: Vec::new(),
            }],
            constraints: vec![
                Expr::Soft {
                    priority: 1,
                    value: Box::new(Expr::Binary {
                        op: "==".into(),
                        left: Box::new(Expr::Field(0)),
                        right: Box::new(Expr::Literal(4, 3)),
                        signed: false,
                    }),
                },
                Expr::DisableSoft {
                    priority: 2,
                    variable: Box::new(Expr::Field(0)),
                    condition: Some(Box::new(Expr::Binary {
                        op: ">".into(),
                        left: Box::new(Expr::Size(0)),
                        right: Box::new(Expr::Literal(32, 2)),
                        signed: true,
                    })),
                },
            ],
            ..Program::default()
        };
        let mut resized = false;
        for _ in 0..20 {
            assert!(heap.randomize_object(object, &program, false));
            let contents = heap.load(object, 8, crate::HANDLE_BYTES) as Handle;
            let length = heap.sequence_len(contents);
            resized |= length != 0;
            if length <= 2 {
                assert_eq!(heap.load(object, 0, 1), 3);
            }
        }
        assert!(resized);
        assert_eq!(heap.take_fault(), None);
    }

    /// The objects rand handles reach rank below their container, those of
    /// a later handle above those of an earlier one, and an object several
    /// handles reach, with the objects it reaches, as the last of them
    /// (18.5.13.1).
    #[test]
    fn rand_handles_rank_the_objects_they_reach() {
        let mut heap = Heap::new();
        let parent = heap.new_object(1, 12);
        let shared = heap.new_object(2, 4);
        let last = heap.new_object(3, 4);
        let nested = heap.new_object(2, 4);
        let inner = heap.new_object(4, 4);
        heap.store(parent, 0, crate::HANDLE_BYTES, shared.into());
        heap.store(parent, 4, crate::HANDLE_BYTES, shared.into());
        heap.store(parent, 8, crate::HANDLE_BYTES, last.into());
        heap.store(last, 0, crate::HANDLE_BYTES, nested.into());
        heap.store(shared, 0, crate::HANDLE_BYTES, inner.into());
        // The objects of class 2 refer to others too: what `shared` reaches
        // takes its best route, the second handle (18.5.13.1).
        heap.register_program(
            2,
            Registered::Program(
                Program {
                    handles: vec![0],
                    ..Program::default()
                }
                .into(),
            ),
        );
        heap.register_program(
            3,
            Registered::Program(
                Program {
                    handles: vec![0],
                    ..Program::default()
                }
                .into(),
            ),
        );
        let ranks = heap.handle_ranks(parent, &[0, 4, 8]);
        assert_eq!(ranks[&parent], vec![u32::MAX]);
        assert_eq!(ranks[&shared], vec![1, u32::MAX]);
        assert_eq!(ranks[&last], vec![2, u32::MAX]);
        assert_eq!(ranks[&nested], vec![2, 0, u32::MAX]);
        assert!(ranks[&parent] > ranks[&last]);
        assert!(ranks[&last] > ranks[&nested]);
        assert!(ranks[&nested] > ranks[&shared]);
        assert_eq!(ranks[&inner], vec![1, 0, u32::MAX]);
    }

    /// The size of a random array whose size and elements constraints name
    /// is solved first, without the constraints on its elements, and is
    /// then state to them (IEEE 1800-2023 18.5.7.1); an element past the
    /// end reads as 0 (7.4.6).
    #[test]
    fn sizes_are_solved_before_the_elements_they_size() {
        let mut heap = Heap::new();
        let object = heap.new_object(1, 16);
        let layout = crate::containers::parameter(8, true, 0, 0, 0, false, false, false);
        let binary = |op: &str, left: Expr, right: Expr| Expr::Binary {
            op: op.into(),
            left: Box::new(left),
            right: Box::new(right),
            signed: false,
        };
        let element = |index| Expr::ElementAt {
            array: 0,
            index,
            width: 8,
        };
        let length = Expr::Resize {
            value: Box::new(Expr::Field(0)),
            width: 32,
            signed: false,
        };
        let mut program = Program {
            fields: vec![Field {
                offset: 0,
                width: 8,
                random: true,
                domain: Vec::new(),
                cyclic: false,
                four_state: false,
            }],
            arrays: vec![Array {
                offset: 8,
                layout,
                width: 8,
                random: true,
                domain: Vec::new(),
            }],
            constraints: vec![
                binary(
                    "&&",
                    binary(">", Expr::Field(0), Expr::Literal(8, 1)),
                    binary("<", Expr::Field(0), Expr::Literal(8, 10)),
                ),
                binary("==", element(0), Expr::Field(0)),
                binary(
                    "==",
                    Expr::Size(0),
                    binary("+", length, Expr::Literal(32, 1)),
                ),
                binary("==", element(20), Expr::Literal(8, 0)),
            ],
            ..Program::default()
        };
        for _ in 0..20 {
            assert!(heap.randomize_object(object, &program, false));
            let burst = heap.load(object, 0, 1);
            let contents = heap.load(object, 8, crate::HANDLE_BYTES) as Handle;
            assert!((2..10).contains(&burst));
            assert_eq!(heap.sequence_len(contents) as u64, burst + 1);
            assert_eq!(heap.sequence_element(contents, 0), Some(burst));
        }
        // A size the constraints on the elements cannot take fails the call.
        program.constraints = vec![
            binary("==", Expr::Size(0), Expr::Literal(32, 0)),
            binary("==", element(0), Expr::Literal(8, 5)),
        ];
        assert!(!heap.randomize_object(object, &program, false));
        assert_eq!(heap.take_fault(), None);
    }

    /// A random array whose size a constraint names is resized to a size
    /// the constraints allow and gets random elements; one without a size
    /// constraint keeps its size, a disabled one is state (IEEE 1800-2023
    /// 18.4), and a size above the preferred bound is taken only when the
    /// constraints require it.
    #[test]
    fn random_arrays_take_the_sizes_their_constraints_allow() {
        let mut heap = Heap::new();
        let object = heap.new_object(1, 24);
        let layout = crate::containers::parameter(8, true, 0, 0, 0, false, false, false);
        let array = |offset| Array {
            offset,
            layout,
            width: 8,
            random: true,
            domain: Vec::new(),
        };
        let size = |op: &str, value| Expr::Binary {
            op: op.into(),
            left: Box::new(Expr::Size(0)),
            right: Box::new(Expr::Literal(32, value)),
            signed: true,
        };
        let mut program = Program {
            arrays: vec![array(8), array(16)],
            constraints: vec![Expr::Binary {
                op: "&&".into(),
                left: Box::new(size(">", 2)),
                right: Box::new(size("<", 6)),
                signed: false,
            }],
            ..Program::default()
        };
        let length = |heap: &mut Heap, offset| {
            let contents = heap.load(object, offset, crate::HANDLE_BYTES) as Handle;
            heap.sequence_len(contents)
        };
        let mut nonzero = false;
        for _ in 0..20 {
            assert!(heap.randomize_object(object, &program, false));
            assert!((3..6).contains(&length(&mut heap, 8)));
            // No constraint names the second array's size.
            assert_eq!(length(&mut heap, 16), 0);
            let contents = heap.load(object, 8, crate::HANDLE_BYTES) as Handle;
            nonzero |= heap.containers[&contents].contents
                != crate::containers::Contents::Sequence(Default::default());
        }
        assert!(nonzero);
        // A disabled array keeps its contents; a check reads its size.
        let kept = heap.load(object, 8, crate::HANDLE_BYTES);
        heap.set_random_mode(object, 8, false);
        assert!(heap.randomize_object(object, &program, false));
        assert_eq!(heap.load(object, 8, crate::HANDLE_BYTES), kept);
        assert!(heap.randomize_object(object, &program, true));
        heap.set_random_mode(object, 8, true);
        // A size the preferred bound excludes.
        program.constraints = vec![size("==", 5000)];
        assert!(heap.randomize_object(object, &program, false));
        assert_eq!(length(&mut heap, 8), 5000);
        assert_eq!(heap.take_fault(), None);
        // A disabled block names no size: the array keeps its size.
        program.constraints = vec![Expr::Enabled {
            index: 0,
            value: Box::new(size("==", 3)),
        }];
        heap.set_constraint_mode(object, Some(0), false);
        assert!(heap.randomize_object(object, &program, false));
        assert_eq!(length(&mut heap, 8), 5000);
        heap.set_constraint_mode(object, Some(0), true);
        assert!(heap.randomize_object(object, &program, false));
        assert_eq!(length(&mut heap, 8), 3);
        // A size above the limit faults before anything changes: the
        // scalar field and the arrays keep their values.
        program.fields = program_fields();
        program.constraints = vec![size("==", ARRAY_SIZE_LIMIT + 1)];
        heap.store(object, 0, 1, 2);
        let contents = heap.load(object, 8, crate::HANDLE_BYTES);
        assert!(!heap.randomize_object(object, &program, false));
        assert!(matches!(
            heap.take_fault(),
            Some(Fault::ConstraintRandomization(reason)) if reason.contains("above the limit")
        ));
        assert_eq!(heap.load(object, 0, 1), 2);
        assert_eq!(heap.load(object, 8, crate::HANDLE_BYTES), contents);
        // Beyond the preferred bound, only supported sizes are sought.
        program.fields = Vec::new();
        program.constraints = vec![Expr::Binary {
            op: "&&".into(),
            left: Box::new(size(">", 4096)),
            right: Box::new(Expr::Binary {
                op: "||".into(),
                left: Box::new(size("<", 4200)),
                right: Box::new(size(">", ARRAY_SIZE_LIMIT + 10)),
                signed: false,
            }),
            signed: false,
        }];
        for _ in 0..5 {
            assert!(heap.randomize_object(object, &program, false));
            assert!((4097..4200).contains(&length(&mut heap, 8)));
        }
        assert_eq!(heap.take_fault(), None);
    }

    /// A randc value that only a size beyond the preferred bound allows is
    /// taken before its cycle restarts (IEEE 1800-2023 18.4.2).
    #[test]
    fn randc_cycles_restart_after_every_size_is_tried() {
        let mut heap = Heap::new();
        let object = heap.new_object(1, 16);
        let layout = crate::containers::parameter(8, true, 0, 0, 0, false, false, false);
        let compare = |op: &str, left: Expr, value| Expr::Binary {
            op: op.into(),
            left: Box::new(left),
            right: Box::new(Expr::Literal(32, value)),
            signed: true,
        };
        let either = |left: Expr, right: Expr| Expr::Binary {
            op: "||".into(),
            left: Box::new(left),
            right: Box::new(right),
            signed: false,
        };
        let cycle = Expr::Resize {
            value: Box::new(Expr::Field(0)),
            width: 32,
            signed: false,
        };
        let program = Program {
            fields: vec![Field {
                offset: 0,
                width: 1,
                random: true,
                domain: Vec::new(),
                cyclic: true,
                four_state: false,
            }],
            arrays: vec![Array {
                offset: 8,
                layout,
                width: 8,
                random: true,
                domain: Vec::new(),
            }],
            constraints: vec![
                // c == 1 -> 4096 < size < 4100; c == 0 -> size < 10.
                either(
                    compare("!=", cycle.clone(), 1),
                    Expr::Binary {
                        op: "&&".into(),
                        left: Box::new(compare(">", Expr::Size(0), 4096)),
                        right: Box::new(compare("<", Expr::Size(0), 4100)),
                        signed: false,
                    },
                ),
                either(compare("!=", cycle, 0), compare("<", Expr::Size(0), 10)),
            ],
            ..Program::default()
        };
        for _ in 0..4 {
            assert!(heap.randomize_object(object, &program, false));
            let first = heap.load(object, 0, 1);
            assert!(heap.randomize_object(object, &program, false));
            assert_ne!(heap.load(object, 0, 1), first);
        }
        assert_eq!(heap.take_fault(), None);
    }

    /// The objects rand handles refer to are solved with the object, with
    /// the programs their classes registered, each once (IEEE 1800-2023
    /// 18.4); a disabled or null handle and a class without a program add
    /// nothing, and an unsupported one faults.
    #[test]
    fn rand_handles_solve_the_objects_they_refer_to() {
        let mut heap = Heap::new();
        // Parent: a 4-bit field at 0 and rand handles at 4 and 8.
        let parent = heap.new_object(1, 12);
        let child = heap.new_object(2, 1);
        heap.store(parent, 4, crate::HANDLE_BYTES, child.into());
        heap.store(parent, 8, crate::HANDLE_BYTES, parent.into());
        let parent_program = Program {
            handles: vec![4, 8],
            ..program()
        };
        let mut child_program = program();
        child_program.constraints = vec![Expr::Binary {
            op: "==".into(),
            left: Box::new(Expr::Field(0)),
            right: Box::new(Expr::Literal(4, 9)),
            signed: false,
        }];
        heap.register_program(2, Registered::Program(child_program.clone().into()));
        // A later registration keeps the first.
        heap.register_program(2, Registered::Unsupported("later".into()));
        assert!(heap.program_registered(2));
        assert!(heap.randomize_object(parent, &parent_program, false));
        assert!(heap.load(parent, 0, 1) < 3);
        assert_eq!(heap.load(child, 0, 1), 9);
        assert_eq!(heap.load(parent, 4, crate::HANDLE_BYTES), u64::from(child));
        // A check takes the objects of the enabled handles too; generated
        // randomize(null) disables them for the call (18.11).
        heap.store(child, 0, 1, 4);
        assert!(!heap.randomize_object(parent, &parent_program, true));
        // A disabled handle leaves its object out (18.8).
        heap.set_random_mode(parent, 4, false);
        assert!(heap.randomize_object(parent, &parent_program, false));
        assert_eq!(heap.load(child, 0, 1), 4);
        heap.set_random_mode(parent, 4, true);
        // A null handle refers to nothing.
        heap.store(parent, 4, crate::HANDLE_BYTES, 0);
        assert!(heap.randomize_object(parent, &parent_program, false));
        assert_eq!(heap.take_fault(), None);
        // A class whose program the solver cannot take faults.
        let other = heap.new_object(3, 1);
        heap.register_program(3, Registered::Unsupported("rand dynamic array".into()));
        heap.store(parent, 4, crate::HANDLE_BYTES, other.into());
        assert!(!heap.randomize_object(parent, &parent_program, false));
        assert!(matches!(
            heap.take_fault(),
            Some(Fault::ConstraintRandomization(reason)) if reason.contains("rand dynamic array")
        ));
        // A class that registered nothing has nothing to solve.
        let plain = heap.new_object(4, 1);
        heap.store(parent, 4, crate::HANDLE_BYTES, plain.into());
        assert!(heap.randomize_object(parent, &parent_program, false));
        // Registration and visits through the heap operations.
        let json =
            heap.intern(&serde_json::to_vec(&Registered::Program(child_program.into())).unwrap());
        assert_eq!(heap.call(crate::ops::OBJECT_PROGRAM_REGISTERED, 5, 0, 0), 0);
        heap.call(crate::ops::OBJECT_REGISTER_PROGRAM, 5, json.into(), 0);
        assert_eq!(heap.call(crate::ops::OBJECT_PROGRAM_REGISTERED, 5, 0, 0), 1);
        // The objects the hooks visit: through the registered handles, each
        // once, without the object itself.
        heap.register_program(1, Registered::Program(parent_program.clone().into()));
        heap.store(parent, 4, crate::HANDLE_BYTES, child.into());
        assert_eq!(
            heap.call(crate::ops::OBJECT_VISIT_BEGIN, parent.into(), 0, 0),
            1
        );
        assert_eq!(
            heap.call(crate::ops::OBJECT_VISIT, 0, 0, 0),
            u64::from(child)
        );
        heap.set_random_mode(parent, 4, false);
        assert_eq!(
            heap.call(crate::ops::OBJECT_VISIT_BEGIN, parent.into(), 0, 0),
            0
        );
        heap.call(crate::ops::OBJECT_VISIT_END, 0, 0, 0);
        assert_eq!(
            heap.call(crate::ops::OBJECT_VISIT, 0, 0, 0),
            u64::from(child)
        );
        heap.call(crate::ops::OBJECT_VISIT_END, 0, 0, 0);
        // post_randomize visits the members the solve took, also after a
        // hook cleared the handle.
        heap.set_random_mode(parent, 4, true);
        assert!(heap.randomize_object(parent, &parent_program, false));
        heap.store(parent, 4, crate::HANDLE_BYTES, crate::objects::NULL.into());
        assert_eq!(heap.call(crate::ops::OBJECT_VISIT_SOLVED, 0, 0, 0), 1);
        assert_eq!(
            heap.call(crate::ops::OBJECT_VISIT, 0, 0, 0),
            u64::from(child)
        );
        heap.call(crate::ops::OBJECT_VISIT_END, 0, 0, 0);
        assert_eq!(heap.take_fault(), None);
    }

    #[test]
    fn per_call_inputs_are_read_only_checked_and_not_retained() {
        let mut heap = Heap::new();
        let object = heap.new_object(1, 1);
        let mut program = program();
        program.constraints = vec![Expr::Binary {
            op: "==".into(),
            left: Box::new(Expr::Field(0)),
            right: Box::new(Expr::Input { index: 0, width: 4 }),
            signed: false,
        }];
        for value in [3, 12] {
            assert!(heap.randomize_object_with_inputs(object, &program, false, &[value]));
            assert_eq!(heap.load(object, 0, 1), value);
        }
        let mut before = heap.clone();
        assert!(!heap.randomize_object_with_inputs(object, &program, true, &[3]));
        assert_eq!(heap.load(object, 0, 1), 12);
        assert_eq!(heap.random_object(object), before.random_object(object));
        assert!(!heap.randomize_object(object, &program, false));
        assert!(matches!(
            heap.take_fault(),
            Some(Fault::ConstraintRandomization(_))
        ));
        let json = heap.intern(&serde_json::to_vec(&program).unwrap());
        let mut bytes = vec![0];
        bytes.extend(5u64.to_be_bytes());
        let inputs = heap.new_buffer(bytes);
        assert_eq!(
            heap.call(
                crate::ops::OBJECT_RANDOMIZE_INPUTS,
                object.into(),
                json.into(),
                inputs
            ),
            1,
            "{:?}",
            heap.take_fault()
        );
        assert_eq!(heap.load(object, 0, 1), 5);
        let bad = heap.new_buffer(vec![0, 1]);
        assert_eq!(
            heap.call(
                crate::ops::OBJECT_RANDOMIZE_INPUTS,
                object.into(),
                json.into(),
                bad
            ),
            0
        );
        assert!(matches!(
            heap.take_fault(),
            Some(Fault::ConstraintRandomization(_))
        ));
    }

    #[test]
    fn cyclic_fields_draw_each_allowed_value_once_per_cycle() {
        let mut heap = Heap::new();
        let object = heap.new_object(1, 1);
        let mut program = program();
        program.fields[0].width = 2;
        program.fields[0].cyclic = true;
        program.constraints[0] = Expr::Binary {
            op: "<".into(),
            left: Box::new(Expr::Field(0)),
            right: Box::new(Expr::Literal(2, 3)),
            signed: false,
        };
        for _ in 0..3 {
            let mut seen = Vec::new();
            for _ in 0..3 {
                assert!(heap.randomize_object(object, &program, false));
                seen.push(heap.load(object, 0, 1));
            }
            seen.sort_unstable();
            assert_eq!(seen, [0, 1, 2]);
        }
    }

    #[test]
    fn exhausted_cycles_restart_alone() {
        // a < 2 runs out after two draws; b < 3 keeps its cycle.
        let mut heap = Heap::new();
        let object = heap.new_object(1, 2);
        let field = |offset| Field {
            offset,
            width: 2,
            random: true,
            domain: Vec::new(),
            cyclic: true,
            four_state: false,
        };
        let below = |field, bound| Expr::Binary {
            op: "<".into(),
            left: Box::new(Expr::Field(field)),
            right: Box::new(Expr::Literal(2, bound)),
            signed: false,
        };
        let program = Program {
            fields: vec![field(0), field(1)],
            constraints: vec![below(0, 2), below(1, 3)],
            ..Default::default()
        };
        for _ in 0..4 {
            let mut seen = Vec::new();
            for _ in 0..3 {
                assert!(heap.randomize_object(object, &program, false));
                seen.push(heap.load(object, 1, 1));
            }
            seen.sort_unstable();
            assert_eq!(seen, [0, 1, 2]);
        }
    }

    #[test]
    fn enumeration_cycles_hold_member_indices() {
        // The unconstrained draw and the solver share the cycle of a randc
        // enumeration with values {4, 9}.
        let mut heap = Heap::new();
        let object = heap.new_object(1, 1);
        let mut program = program();
        program.fields[0].domain = vec![4, 9];
        program.fields[0].cyclic = true;
        program.constraints.clear();
        for _ in 0..4 {
            let first = [4, 9][heap.random_cyclic_object(object, 0, 2) as usize];
            assert!(heap.randomize_object(object, &program, false));
            let second = heap.load(object, 0, 1);
            assert_ne!(first, second);
            assert!([4, 9].contains(&second));
        }
    }

    #[test]
    fn failed_calls_keep_unfinished_cycles() {
        let mut heap = Heap::new();
        let object = heap.new_object(1, 1);
        let mut program = program();
        program.fields[0].width = 1;
        program.fields[0].cyclic = true;
        program.constraints.clear();
        let mut impossible = program.clone();
        impossible.constraints = vec![Expr::Literal(1, 0)];
        for _ in 0..4 {
            assert!(heap.randomize_object(object, &program, false));
            let first = heap.load(object, 0, 1);
            assert!(!heap.randomize_object(object, &impossible, false));
            assert!(heap.randomize_object(object, &program, false));
            assert_ne!(heap.load(object, 0, 1), first);
        }
    }

    #[test]
    fn large_domains_stay_within_the_solver_depth() {
        let mut heap = Heap::new();
        let object = heap.new_object(1, 2);
        let mut program = program();
        program.fields[0].width = 12;
        program.fields[0].domain = (0..1024).map(|value| value * 3).collect();
        program.constraints.clear();
        assert!(heap.randomize_object(object, &program, false));
        assert_eq!(heap.load(object, 0, 2) % 3, 0);
    }

    #[test]
    fn domains_restrict_random_fields_only() {
        let mut heap = Heap::new();
        let object = heap.new_object(1, 1);
        let mut program = program();
        program.fields[0].domain = vec![1, 9];
        program.constraints.clear();
        for _ in 0..16 {
            assert!(heap.randomize_object(object, &program, false));
            assert!([1, 9].contains(&heap.load(object, 0, 1)));
        }
        heap.set_random_mode(object, 0, false);
        heap.store(object, 0, 1, 5);
        assert!(heap.randomize_object(object, &program, false));
        assert_eq!(heap.load(object, 0, 1), 5);
    }

    #[test]
    fn modes_unsat_faults_and_snapshot_replay() {
        let mut heap = Heap::new();
        let object = heap.new_object(1, 1);
        heap.store(object, 0, 1, 9);
        let program = program();
        let mut before_check = heap.clone();
        assert!(!heap.randomize_object(object, &program, true));
        assert_eq!(
            heap.random_object(object),
            before_check.random_object(object)
        );
        assert_eq!(heap.load(object, 0, 1), 9);
        assert_eq!(heap.take_fault(), None);
        let limits = Limits {
            max_work: 0,
            ..Limits::default()
        };
        assert!(!heap.randomize_object_with_limits(object, &program, false, &[], limits));
        assert_eq!(heap.load(object, 0, 1), 9);
        assert!(matches!(
            heap.take_fault(),
            Some(Fault::ConstraintRandomization(_))
        ));
        heap.seed_object(object, 42);
        let mut restored = heap.clone();
        for _ in 0..16 {
            assert!(heap.randomize_object(object, &program, false));
            assert!(restored.randomize_object(object, &program, false));
            let value = heap.load(object, 0, 1);
            assert!(value < 3);
            assert_eq!(value, restored.load(object, 0, 1));
        }
        heap.set_random_mode(object, 0, false);
        heap.store(object, 0, 1, 7);
        assert!(!heap.randomize_object(object, &program, false));
        assert_eq!(heap.load(object, 0, 1), 7);
        assert_eq!(heap.take_fault(), None);
    }

    #[test]
    fn signed_inputs_in_unsigned_arithmetic_context() {
        let field = |i, signed| Expr::Resize {
            value: Box::new(Expr::Field(i)),
            width: 32,
            signed,
        };
        let binary = |op: &str, left, right| Expr::Binary {
            op: op.into(),
            left: Box::new(left),
            right: Box::new(right),
            signed: false,
        };
        let program = Program {
            fields: [8, 16, 32]
                .into_iter()
                .enumerate()
                .map(|(i, width)| Field {
                    offset: i as u32 * 8,
                    width,
                    random: true,
                    domain: Vec::new(),
                    cyclic: false,
                    four_state: false,
                })
                .collect(),
            constraints: vec![
                binary(
                    "==",
                    field(0, true),
                    Expr::Unary {
                        op: "-".into(),
                        value: Box::new(Expr::Literal(32, 1)),
                    },
                ),
                binary("==", field(1, true), Expr::Literal(32, 1)),
                binary("==", Expr::Field(2), Expr::Literal(32, 256)),
                binary(
                    "==",
                    binary("+", field(0, false), field(1, false)),
                    Expr::Field(2),
                ),
            ],
            ..Default::default()
        };
        let mut heap = Heap::new();
        let object = heap.new_object(1, 24);
        assert!(
            heap.randomize_object(object, &program, false),
            "{:?}",
            heap.take_fault()
        );
        assert_eq!(heap.load(object, 0, 1), 255);
        assert_eq!(heap.load(object, 8, 2), 1);
        assert_eq!(heap.load(object, 16, 4), 256);
    }
}
