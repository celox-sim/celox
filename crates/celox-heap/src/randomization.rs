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

#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Program {
    pub fields: Vec<Field>,
    pub constraints: Vec<Expr>,
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
            let mut projection = Env::new();
            let mut terms = Vec::new();
            let mut active = Vec::new();
            let mut cyclic = Vec::new();
            let mut formula = boolv(true);
            for (index, field) in program.fields.iter().enumerate() {
                if !(1..=64).contains(&field.width) {
                    return Err("invalid constraint field width".into());
                }
                let name = index.to_string();
                let term = var(name.clone(), Sort::Bv(field.width));
                let enabled = !check_only && self.random_mode(handle, field.offset, field.random);
                if enabled {
                    projection.insert(name, term.clone());
                    active.push((index, field));
                    if !field.domain.is_empty() {
                        formula = and(
                            formula,
                            member(&term, field.width, field.domain.iter().copied()),
                        );
                    }
                    if field.cyclic {
                        if field.domain.is_empty() && field.width > 8 || field.domain.len() > 256 {
                            return Err("randc property with more than 256 values".into());
                        }
                        cyclic.push((field, term.clone()));
                    }
                } else {
                    let value = self.load(
                        handle,
                        field.offset as usize,
                        field.width.div_ceil(8) as usize,
                    );
                    formula = and(formula, eq(term.clone(), bv(field.width, value)));
                }
                terms.push(term);
            }
            for constraint in &program.constraints {
                let constraint = if let Expr::Enabled { index, value } = constraint {
                    if !self.constraint_mode(handle, *index) {
                        continue;
                    }
                    value.as_ref()
                } else {
                    constraint
                };
                formula = and(formula, truth(constraint.term(&terms, inputs)?));
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
            let restriction = |heap: &mut Self, field: &Field, term: &Term, restart: bool| {
                let remaining = if restart {
                    (0..cycle_size(field)).collect()
                } else {
                    heap.cycle_remaining(handle, field.offset, cycle_size(field), false)
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
                for ((field, term), restart) in cyclic.iter().zip(restarted) {
                    attempt = and(attempt, restriction(heap, field, term, *restart));
                }
                solve_seeded(&attempt, &projection, limits.clone(), seed)
            };
            let mut outcome = solve_with(self, &restarted);
            if matches!(outcome.verdict, Verdict::Unsat) && !cyclic.is_empty() {
                for (position, (field, term)) in cyclic.iter().enumerate() {
                    let alone = and(formula.clone(), restriction(self, field, term, false));
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
                        .map(|(index, field)| match model.get(&index.to_string()) {
                            Some(Scalar::Bv { value, .. }) => Ok((field, *value)),
                            _ => Err("missing constraint field value".to_owned()),
                        })
                        .collect::<Result<Vec<_>, _>>()?;
                    for ((field, _), restart) in cyclic.iter().zip(&restarted) {
                        if *restart {
                            self.cycle_remaining(handle, field.offset, cycle_size(field), true);
                        }
                    }
                    for (field, value) in &updates {
                        if field.cyclic
                            && let Some(entry) = cycle_index(field, *value)
                        {
                            self.take_cycle_entry(handle, field.offset, cycle_size(field), entry);
                        }
                    }
                    for (field, value) in updates {
                        self.store(
                            handle,
                            field.offset as usize,
                            field.width.div_ceil(8) as usize,
                            value,
                        );
                    }
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
        }
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
