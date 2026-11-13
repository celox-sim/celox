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
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Expr {
    /// Top-level named block predicate; inline predicates remain unconditional.
    Enabled {
        index: u32,
        value: Box<Expr>,
    },
    Literal(u32, u64),
    Field(usize),
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
    /// Whether the expression names the elements of array `index`: a loop
    /// over it or its sum.
    fn names_elements(&self, index: usize) -> bool {
        match self {
            Self::Sum(array) => *array == index,
            Self::Foreach { array, body } => *array == index || body.names_elements(index),
            Self::Enabled { value, .. }
            | Self::Resize { value, .. }
            | Self::Unary { value, .. } => value.names_elements(index),
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
            | Self::Size(_)
            | Self::Input { .. }
            | Self::Index { .. }
            | Self::Solved(_)
            | Self::Element { .. } => false,
        }
    }

    /// Whether the expression names the size of array `index`.
    fn names_size(&self, index: usize) -> bool {
        match self {
            Self::Size(array) => *array == index,
            Self::Enabled { value, .. }
            | Self::Resize { value, .. }
            | Self::Unary { value, .. } => value.names_size(index),
            Self::Binary { left, right, .. } => left.names_size(index) || right.names_size(index),
            Self::Foreach { body, .. } => body.names_size(index),
            Self::Sum(_) => false,
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
            | Self::Input { .. }
            | Self::Index { .. }
            | Self::Solved(_)
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
        Ok(match self {
            Self::Enabled { .. } => return Err("nested constraint mode predicate".into()),
            Self::Dist { membership, .. } => membership.term(terms, base, sizes, inputs)?,
            Self::Literal(width, value) => bv(*width, *value),
            Self::Input { index, width } => {
                if !(1..=64).contains(width) {
                    return Err("invalid constraint input width".into());
                }
                bv(
                    *width,
                    *inputs.get(*index).ok_or("missing constraint input")?,
                )
            }
            Self::Field(index) => terms
                .get(base + *index)
                .ok_or("invalid constraint field")?
                .clone(),
            Self::Solved(index) => terms.get(*index).ok_or("invalid solver variable")?.clone(),
            Self::Size(index) => sizes.get(*index).ok_or("invalid constraint array")?.clone(),
            Self::Foreach { .. } | Self::Index { .. } | Self::Element { .. } => {
                return Err("constraint loop outside its array".into());
            }
            Self::Sum(_) => return Err("constraint sum of an unread array".into()),
            Self::Resize {
                value,
                width,
                signed,
            } => {
                let value = value.term(terms, base, sizes, inputs)?;
                let Sort::Bv(old) = value.0.sort else {
                    unreachable!()
                };
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
            }
            Self::Unary { op, value } => {
                let value = value.term(terms, base, sizes, inputs)?;
                match op.as_str() {
                    "!" => boolean(not(truth(value))),
                    "~" => node(value.0.sort.clone(), "bvnot", vec![value]),
                    "-" => {
                        let Sort::Bv(width) = value.0.sort else {
                            unreachable!()
                        };
                        node(Sort::Bv(width), "bvsub", vec![bv(width, 0), value])
                    }
                    _ => return Err("invalid constraint unary operation".into()),
                }
            }
            Self::Binary {
                op,
                left,
                right,
                signed,
            } => {
                let a = left.term(terms, base, sizes, inputs)?;
                let b = right.term(terms, base, sizes, inputs)?;
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
                if let Some(relation) = relation {
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
                }
            }
        })
    }
}

impl Heap {
    /// `constraint` of object `object` with each loop over a state array
    /// replaced by the conjunction of its body for each element, which
    /// `loops` gives for the enclosing loops: (array, index).
    fn unroll(
        &mut self,
        object: Handle,
        program: &Program,
        constraint: &Expr,
        loops: &mut Vec<(usize, u64)>,
        solved: &fxhash::FxHashMap<Handle, (usize, &Program)>,
        elements: &fxhash::FxHashMap<(Handle, usize), usize>,
        check_only: bool,
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
                // each element (see `element_bases`).
                if !check_only
                    && self.random_mode(object, offset, looped.random)
                    && !elements.contains_key(&(object, *array))
                {
                    return Err("a constraint loop over an array the call randomizes".into());
                }
                let contents = self.load(object, offset as usize, crate::HANDLE_BYTES) as Handle;
                let mut each = Vec::new();
                for index in 0..self.sequence_len(contents) as u64 {
                    loops.push((*array, index));
                    each.push(
                        self.unroll(object, program, body, loops, solved, elements, check_only)?,
                    );
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
                    && let Some(&base) = elements.get(&(object, array))
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
                        // A member of an object the call solves is its
                        // solver variable.
                        if let Some(&(base, program)) = solved.get(&element)
                            && let Some(field) = program
                                .fields
                                .iter()
                                .position(|field| field.offset == *member)
                        {
                            return Ok(Expr::Solved(base + field));
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
                match elements.get(&(object, *array)) {
                    // The sum of the solver variables of its elements, at
                    // their width, as a balanced tree.
                    Some(&base) => {
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
            Expr::Enabled { index, value } => Expr::Enabled {
                index: *index,
                value: boxed(
                    self.unroll(object, program, value, loops, solved, elements, check_only)?,
                ),
            },
            Expr::Resize {
                value,
                width,
                signed,
            } => Expr::Resize {
                value: boxed(
                    self.unroll(object, program, value, loops, solved, elements, check_only)?,
                ),
                width: *width,
                signed: *signed,
            },
            Expr::Unary { op, value } => Expr::Unary {
                op: op.clone(),
                value: boxed(
                    self.unroll(object, program, value, loops, solved, elements, check_only)?,
                ),
            },
            Expr::Binary {
                op,
                left,
                right,
                signed,
            } => Expr::Binary {
                op: op.clone(),
                left: boxed(
                    self.unroll(object, program, left, loops, solved, elements, check_only)?,
                ),
                right: boxed(
                    self.unroll(object, program, right, loops, solved, elements, check_only)?,
                ),
                signed: *signed,
            },
            Expr::Dist {
                membership,
                choices,
            } => Expr::Dist {
                membership: boxed(self.unroll(
                    object, program, membership, loops, solved, elements, check_only,
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
                                elements,
                                check_only,
                            )?,
                            weight: choice.weight.clone(),
                        })
                    })
                    .collect::<Result<_, String>>()?,
            },
            Expr::Literal(..)
            | Expr::Field(_)
            | Expr::Input { .. }
            | Expr::Size(_)
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

    fn randomize_object_with_limits(
        &mut self,
        handle: Handle,
        program: &Program,
        check_only: bool,
        inputs: &[u64],
        limits: Limits,
    ) -> bool {
        if !self.check_object(handle, 0) {
            return false;
        }
        let result = (|| -> Result<bool, String> {
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
            let mut formula = boolv(true);
            let mut bases = Vec::new();
            let mut size_terms = Vec::new();
            let mut size_bases = Vec::new();
            let mut refills = Vec::new();
            // The first solver variable of the elements of each random array
            // a constraint names the elements of, by object and array.
            let mut element_bases: fxhash::FxHashMap<(Handle, usize), usize> =
                fxhash::FxHashMap::default();
            let mut preferred = boolv(true);
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
                        let value = self.load(
                            object,
                            field.offset as usize,
                            field.width.div_ceil(8) as usize,
                        );
                        formula = and(formula, eq(term.clone(), bv(field.width, value)));
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
                        && constraints[position]
                            .iter()
                            .any(|constraint| constraint.names_size(index));
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
                        None => bv(32, length as u64),
                    });
                    // A random array whose elements a constraint names has a
                    // solver variable for each (18.4, 18.5.8): its size is
                    // the one it has.
                    let named = enabled
                        && constraints[position]
                            .iter()
                            .any(|constraint| constraint.names_elements(index));
                    let elements = if named {
                        if term.is_some() {
                            return Err(
                                "a constraint on the elements of a random array whose size a constraint names"
                                    .into(),
                            );
                        }
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
                        element_bases.insert((object, index), base);
                        Some(base)
                    } else {
                        None
                    };
                    if enabled {
                        refills.push((object, array, term, length, elements));
                    }
                }
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
            for ((((object, current), &base), &size_base), enabled) in members
                .iter()
                .zip(&bases)
                .zip(&size_bases)
                .zip(&constraints)
            {
                let sizes = &size_terms[size_base..size_base + current.arrays.len()];
                for constraint in enabled {
                    let constraint = self.unroll(
                        *object,
                        current,
                        constraint,
                        &mut Vec::new(),
                        &solved,
                        &element_bases,
                        check_only,
                    )?;
                    formula = and(
                        formula,
                        truth(constraint.term(&terms, base, sizes, inputs)?),
                    );
                    // Each distribution, also one under `if` or `->`, whose
                    // item is tried only where its guards let it apply, or
                    // in a `foreach`, one for each element.
                    let mut found = Vec::new();
                    distributions_of(&constraint, &mut Vec::new(), &mut found);
                    for (guards, choices) in found {
                        // The distribution applies unless a guard holds.
                        let mut skipped = boolv(false);
                        for guard in guards {
                            let guard = truth(guard.term(&terms, base, sizes, inputs)?);
                            skipped = not(and(not(skipped), not(guard)));
                        }
                        let mut weighted = Vec::new();
                        for choice in choices
                            .iter()
                            .filter(|choice| choice.weight != num_bigint::BigUint::ZERO)
                        {
                            let item = truth(choice.predicate.term(&terms, base, sizes, inputs)?);
                            weighted.push((
                                not(and(not(skipped.clone()), not(item))),
                                choice.weight.clone(),
                            ));
                        }
                        distributions.push(weighted);
                    }
                }
            }
            let seed = if projection.is_empty() {
                0
            } else {
                self.random_object(handle)
            };
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
                Verdict::Sat => {
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
                        self.store(
                            object,
                            field.offset as usize,
                            field.width.div_ceil(8) as usize,
                            value,
                        );
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
