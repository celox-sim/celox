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
}

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
    fn term(&self, fields: &[Term], inputs: &[u64]) -> Result<Term, String> {
        Ok(match self {
            Self::Enabled { .. } => return Err("nested constraint mode predicate".into()),
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
            Self::Field(index) => fields
                .get(*index)
                .ok_or("invalid constraint field")?
                .clone(),
            Self::Resize {
                value,
                width,
                signed,
            } => {
                let value = value.term(fields, inputs)?;
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
                let value = value.term(fields, inputs)?;
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
                let a = left.term(fields, inputs)?;
                let b = right.term(fields, inputs)?;
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
            for (object, current) in &members {
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
            }
            for ((object, current), &base) in members.iter().zip(&bases) {
                let object = *object;
                let fields = &terms[base..base + current.fields.len()];
                for constraint in &current.constraints {
                    let constraint = if let Expr::Enabled { index, value } = constraint {
                        if !self.constraint_mode(object, *index) {
                            continue;
                        }
                        value.as_ref()
                    } else {
                        constraint
                    };
                    formula = and(formula, truth(constraint.term(fields, inputs)?));
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
            let mut restarted = vec![false; cyclic.len()];
            let solve_with = |heap: &mut Self, restarted: &[bool]| {
                let mut attempt = formula.clone();
                for ((object, field, term), restart) in cyclic.iter().zip(restarted) {
                    attempt = and(attempt, restriction(heap, *object, field, term, *restart));
                }
                solve_seeded(&attempt, &projection, limits.clone(), seed)
            };
            let mut outcome = solve_with(self, &restarted);
            if matches!(outcome.verdict, Verdict::Unsat) && !cyclic.is_empty() {
                for (position, (object, field, term)) in cyclic.iter().enumerate() {
                    let alone = and(
                        formula.clone(),
                        restriction(self, *object, field, term, false),
                    );
                    restarted[position] = matches!(
                        solve_seeded(&alone, &projection, limits.clone(), seed).verdict,
                        Verdict::Unsat
                    );
                }
                let mut next = 0;
                loop {
                    if restarted.iter().any(|restart| *restart) {
                        outcome = solve_with(self, &restarted);
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
