//! `randsequence` (IEEE 1800-2023 18.17).
//!
//! The statement is expanded where it is written: each production in turn,
//! from the main one, as its rule's items in order, a production as its own
//! expansion and a code block as its statements, which see the variables of
//! the scope around the statement. A production of several rules picks one
//! at random by their weights (18.17.1), 1 unless given, from the thread's
//! random number generator as `randcase` does (18.14.2, 18.16). The
//! production controls `if`, `case` and `repeat` (18.17.2-18.17.4) become
//! the statements of their names around their productions. A `rand join`
//! (18.17.5) interleaves its productions' items at random, a loop running
//! the next item of a production it picks each time (see `join`). In a
//! code
//! block, `break` outside a loop leaves the statement and `return` the
//! production whose rule the block is in (18.17.6). Each sets a hidden flag
//! of the activation, of the statement or of that use of the rule, and
//! the code after it up to the end of what it leaves runs only while the
//! flag is clear: in the code block, the rest of each statement list and
//! the loops it is in; around it, the items that follow in each rule and
//! the repetitions of a `repeat` production. A production that refers to itself, directly or
//! through others, has no expansion and is rejected; so are the forms not
//! provided yet, each by its name.

use super::*;
use celox_sv_syntax::{
    ExprId, ExprKind, Randsequence, RsCaseItem, RsProd, RsProduction, RsRule, StatementId,
    StatementKind,
};

/// How deep `randsequence` statements in code blocks may nest.
const MAX_DEPTH: usize = 64;

/// How many productions one `randsequence` statement may expand, those of
/// statements in its code blocks included. Each production is expanded
/// where it is used, so a grammar whose productions use the next one
/// twice doubles at each step; a larger expansion is rejected.
const MAX_EXPANDED: usize = 1 << 16;

/// A production being expanded, waiting for the expansion of one it uses.
/// Expansion keeps these on a stack of its own, so a long chain of
/// productions does not nest native calls.
enum Frame<'p> {
    /// Rule `rule`, its items before `next` expanded into `output`. A
    /// `return` in its code blocks sets flag `exit`, its marker the first;
    /// once an item may have left it, `armed`, the items that follow run
    /// only while no flag is set.
    /// With `units`, a rule of a production a `rand join` interleaves:
    /// each item's statements are a unit of their own instead.
    Rule {
        rule: &'p RsRule,
        next: usize,
        output: Vec<Stmt>,
        exit: Option<(u64, String)>,
        armed: bool,
        units: Option<Vec<Vec<Stmt>>>,
    },
    /// A `rand join` (18.17.5) of bias `bias` interleaving productions
    /// `sequences`: the units of the rules of those before `sequence`, and
    /// of the rules of that one before `rule`, expanded into `units`; a
    /// `return` in a production's code blocks sets its flag of `exits`.
    Join {
        bias: Option<ExprId>,
        sequences: Vec<&'p RsProduction>,
        sequence: usize,
        rule: usize,
        units: Vec<Vec<Vec<Vec<Stmt>>>>,
        exits: Vec<(u64, String)>,
    },
    /// The rules of a production one of which is picked by weights
    /// `weights` of width `width`, evaluated by `prelude`: those before
    /// `next` expanded into `items`.
    Choice {
        rules: &'p [RsRule],
        next: usize,
        prelude: Vec<Stmt>,
        weights: Vec<String>,
        width: usize,
        items: Vec<Vec<Stmt>>,
    },
    /// An `if` production control (18.17.2) on `condition`, its `then`
    /// production expanded into `then_body` once done, then its `otherwise`
    /// one if any.
    /// `prelude` evaluates the condition's effects.
    If {
        prelude: Vec<Stmt>,
        condition: Expr,
        then_body: Option<Vec<Stmt>>,
        otherwise: Option<ExprId>,
    },
    /// A `repeat` production control (18.17.4) of count `count`, evaluated
    /// before it by `prelude`.
    Repeat { prelude: Vec<Stmt>, count: Expr },
    /// A `case` production control (18.17.3), of kind `kind` on `selector`
    /// comparing chandles if `chandles`, as the case statement is: the
    /// productions of `items` before `next` expanded into `arms` and
    /// `default`, after `prelude` evaluates the selector's effects.
    Case {
        prelude: Vec<Stmt>,
        kind: CaseKind,
        selector: Expr,
        chandles: bool,
        items: &'p [RsCaseItem],
        next: usize,
        arms: Vec<CaseItemBase<Expr, LValue>>,
        default: Option<Vec<Stmt>>,
    },
}

/// What the frame on top of the stack does next.
enum Step<'p> {
    /// Expand the production of this name.
    Enter(String),
    /// Push this frame.
    Push(Frame<'p>),
    /// Push this frame and expand the production of this name for it.
    Within(Frame<'p>, String),
    /// The frame is done, with these statements.
    Finish(Vec<Stmt>),
    /// The frame of a rule a `rand join` interleaves is done, with these
    /// units.
    FinishUnits(Vec<Vec<Stmt>>),
}

impl BodyBuilder<'_, '_, '_> {
    /// The statements of `randsequence` statement `id`.
    pub(super) fn randsequence(&mut self, id: StatementId) -> Converted<Vec<Stmt>> {
        let native = self.tree.declarations();
        let randsequence = native
            .randsequence(id)
            .ok_or_else(|| unsupported("randsequence form"))?;
        if self.randsequence_depth == 0 {
            self.randsequence_expanded = 0;
        }
        if self.randsequence_depth >= MAX_DEPTH {
            return Err(unsupported(format!(
                "randsequence nested more than {MAX_DEPTH} deep"
            )));
        }
        let productions = productions(randsequence)?;
        let main = match &randsequence.main {
            Some(name) => instances::normalize_identifier(&name.spelling),
            None => randsequence
                .productions
                .first()
                .map(|production| instances::normalize_identifier(&production.name.spelling))
                .ok_or_else(|| unsupported("randsequence without productions"))?,
        };
        let native = self.tree.declarations();
        check(native, &productions, &main)?;
        self.randsequence_depth += 1;
        let expansion = self.expand(id, &productions, &main);
        self.randsequence_depth -= 1;
        expansion
    }

    /// The statements of production `main` (see the module documentation).
    fn expand<'p>(
        &mut self,
        id: StatementId,
        productions: &HashMap<String, &'p RsProduction>,
        main: &str,
    ) -> Converted<Vec<Stmt>> {
        let start = self.tree.declarations().statement(id).tokens.start;
        // A `break` or `return` is lowered first as a `disable` of a marker
        // like no block's, by the statement, the expansion so far and their
        // order, and then as the setting of its flag.
        let mut markers = 0usize;
        let mut exit_flag = |builder: &mut Self| {
            markers += 1;
            let key = fxhash::hash64(&(start, builder.randsequence_expanded, markers)) as usize;
            let marker = builder.block_id(key | 1 << (usize::BITS - 1));
            (marker, builder.hidden(super::control::unsigned_type(1)).1)
        };
        // The flag of a `break`, which leaves the whole statement.
        let mut statement_exit: Option<(u64, String)> = None;
        let mut frames = vec![self.enter(productions, main)?];
        // The statements of the frame last finished, for the one below.
        let mut finished: Option<Vec<Stmt>> = None;
        // The units of the rule frame last finished, for a `rand join`.
        let mut finished_units: Option<Vec<Vec<Stmt>>> = None;
        loop {
            let native = self.tree.declarations();
            // The `else` production an `if` expands after its `then` one.
            let mut frames_step = None;
            let step = match frames.last_mut() {
                None => {
                    let mut output = finished.expect("the main production is expanded");
                    if let Some((_, flag)) = &statement_exit {
                        output.insert(0, set(flag, false));
                    }
                    return Ok(output);
                }
                Some(Frame::Rule {
                    rule,
                    next,
                    output,
                    exit,
                    armed,
                    units,
                }) => {
                    let rule: &'p RsRule = rule;
                    // The flags an item before the next may have set.
                    let flags = statement_exit
                        .iter()
                        .chain(exit.iter())
                        .map(|(_, flag)| flag.clone())
                        .collect::<Vec<_>>();
                    if let Some(body) = finished.take() {
                        match units {
                            Some(units) => units.push(body),
                            None => {
                                output.extend(guard(body, *armed, &flags));
                                *armed |= !flags.is_empty();
                            }
                        }
                    }
                    // A `rand join` interleaves its productions (18.17.5);
                    // `check` has seen they are all production items.
                    if *next == 0
                        && let Some(bias) = rule.join
                    {
                        *next = rule.items.len();
                        let mut sequences = Vec::new();
                        for item in &rule.items {
                            let RsProd::Item(item) = item else {
                                unreachable!("rejected by `check`")
                            };
                            self.count_expansion()?;
                            sequences.push(productions[&item_name(native, *item)?]);
                        }
                        let mut exits = Vec::new();
                        for _ in &sequences {
                            exits.push(exit_flag(self));
                        }
                        Step::Push(Frame::Join {
                            bias,
                            units: sequences.iter().map(|_| Vec::new()).collect(),
                            sequences,
                            sequence: 0,
                            rule: 0,
                            exits,
                        })
                    } else {
                        loop {
                            let Some(item) = rule.items.get(*next) else {
                                if let Some(units) = units {
                                    break Step::FinishUnits(std::mem::take(units));
                                }
                                let mut output = std::mem::take(output);
                                if let Some((_, flag)) = exit {
                                    output.insert(0, set(flag, false));
                                }
                                break Step::Finish(output);
                            };
                            *next += 1;
                            match item {
                                RsProd::Code(block) => {
                                    let (breaks, returns) = code_exits(native, *block)?;
                                    if !breaks.is_empty() && statement_exit.is_none() {
                                        statement_exit = Some(exit_flag(self));
                                    }
                                    if !returns.is_empty() && exit.is_none() {
                                        *exit = Some(exit_flag(self));
                                    }
                                    let markers = breaks
                                        .iter()
                                        .map(|id| (*id, statement_exit.clone()))
                                        .chain(returns.iter().map(|id| (*id, exit.clone())))
                                        .map(|(id, exit)| (id, exit.expect("allocated above")))
                                        .collect::<Vec<_>>();
                                    self.randsequence_exits.extend(
                                        markers.iter().map(|(id, (marker, _))| (*id, *marker)),
                                    );
                                    let body = self.native_body_machine(&[*block]);
                                    for (id, _) in &markers {
                                        self.randsequence_exits.remove(id);
                                    }
                                    let flags_of = markers
                                        .into_iter()
                                        .map(|(_, exit)| exit)
                                        .collect::<HashMap<_, _>>();
                                    let body = lower_exits(body?, &flags_of, false);
                                    if let Some(units) = units {
                                        units.push(body);
                                        continue;
                                    }
                                    let flags = statement_exit
                                        .iter()
                                        .chain(exit.iter())
                                        .map(|(_, flag)| flag.clone())
                                        .collect::<Vec<_>>();
                                    output.extend(guard(body, *armed, &flags));
                                    *armed |= !flags.is_empty();
                                }
                                RsProd::Item(item) => break Step::Enter(item_name(native, *item)?),
                                // A production control (18.17.2-18.17.4) behaves
                                // as the statement of its name around its
                                // productions.
                                RsProd::If {
                                    condition,
                                    then,
                                    otherwise,
                                } => {
                                    let prelude = self.effects(&[*condition])?;
                                    let condition = self.native_chandle_value(*condition)?;
                                    let frame = Frame::If {
                                        prelude,
                                        condition,
                                        then_body: None,
                                        otherwise: *otherwise,
                                    };
                                    break Step::Within(frame, item_name(native, *then)?);
                                }
                                // Its count is evaluated once, before the first
                                // repetition.
                                RsProd::Repeat { count, item } => {
                                    let mut prelude = self.effects(&[*count])?;
                                    let value = self.native_value(*count)?;
                                    let saved = self.hidden_of(*count)?.1;
                                    prelude.push(Stmt::Assign {
                                        lhs: LValue::Ident(saved.clone()),
                                        rhs: value,
                                        nonblocking: false,
                                    });
                                    let frame = Frame::Repeat {
                                        prelude,
                                        count: Expr::Ident(saved),
                                    };
                                    break Step::Within(frame, item_name(native, *item)?);
                                }
                                // As the case statement is (18.17.3, 12.5).
                                RsProd::Case { selector, items } => {
                                    let labels = items
                                        .iter()
                                        .flat_map(|item| item.labels.iter().copied())
                                        .collect::<Vec<_>>();
                                    let (prelude, kind, selector, chandles) =
                                        self.case_header("case", None, *selector, &labels)?;
                                    break Step::Push(Frame::Case {
                                        prelude,
                                        kind,
                                        selector,
                                        chandles,
                                        items,
                                        next: 0,
                                        arms: Vec::new(),
                                        default: None,
                                    });
                                }
                            }
                        }
                    }
                }
                Some(Frame::Join {
                    bias,
                    sequences,
                    sequence,
                    rule,
                    units,
                    exits,
                }) => {
                    if let Some(done) = finished_units.take() {
                        units[*sequence].push(done);
                        *rule += 1;
                    }
                    while *sequence < sequences.len() && *rule == sequences[*sequence].rules.len() {
                        *sequence += 1;
                        *rule = 0;
                    }
                    match sequences.get(*sequence) {
                        Some(production) => {
                            let production: &'p RsProduction = production;
                            Step::Push(Frame::Rule {
                                rule: &production.rules[*rule],
                                next: 0,
                                output: Vec::new(),
                                exit: Some(exits[*sequence].clone()),
                                armed: false,
                                units: Some(Vec::new()),
                            })
                        }
                        None => Step::Finish(self.join(
                            *bias,
                            sequences,
                            std::mem::take(units),
                            exits,
                            statement_exit.as_ref().map(|(_, flag)| flag.as_str()),
                        )?),
                    }
                }
                Some(Frame::Choice {
                    rules,
                    next,
                    prelude,
                    weights,
                    width,
                    items,
                }) => {
                    items.extend(finished.take());
                    match (*rules).get(*next) {
                        Some(rule) => {
                            *next += 1;
                            Step::Push(Frame::Rule {
                                rule,
                                next: 0,
                                output: Vec::new(),
                                exit: None,
                                armed: false,
                                units: None,
                            })
                        }
                        None => {
                            let mut output = std::mem::take(prelude);
                            output.extend(self.weighted_choice(
                                std::mem::take(weights),
                                *width,
                                std::mem::take(items),
                            ));
                            Step::Finish(output)
                        }
                    }
                }
                Some(Frame::If {
                    prelude,
                    condition,
                    then_body,
                    otherwise,
                }) => {
                    let body = finished.take().expect("a production is expanded");
                    let (then_body, else_body) = match (then_body.is_none(), otherwise.take()) {
                        (true, Some(item)) => {
                            *then_body = Some(body);
                            frames_step = Some(Step::Enter(item_name(native, item)?));
                            (Vec::new(), Vec::new())
                        }
                        (true, None) => (body, Vec::new()),
                        (false, _) => (then_body.take().unwrap_or_default(), body),
                    };
                    match frames_step.take() {
                        Some(step) => step,
                        None => {
                            let mut output = std::mem::take(prelude);
                            output.push(Stmt::If {
                                condition: condition.clone(),
                                then_body,
                                else_body,
                            });
                            Step::Finish(output)
                        }
                    }
                }
                // After a `break` in its production, the repetitions stop.
                Some(Frame::Repeat { prelude, count }) => {
                    let mut body = finished.take().expect("a production is expanded");
                    if let Some((_, flag)) = &statement_exit {
                        body.push(Stmt::If {
                            condition: Expr::Ident(flag.clone()),
                            then_body: vec![Stmt::Break],
                            else_body: Vec::new(),
                        });
                    }
                    let mut output = std::mem::take(prelude);
                    output.push(Stmt::Loop {
                        kind: LoopKind::Repeat(count.clone()),
                        init: Vec::new(),
                        condition: None,
                        step: Vec::new(),
                        body,
                    });
                    Step::Finish(output)
                }
                Some(Frame::Case {
                    prelude,
                    kind,
                    selector,
                    chandles,
                    items,
                    next,
                    arms,
                    default,
                }) => {
                    if let Some(body) = finished.take() {
                        let item = &items[*next - 1];
                        if item.labels.is_empty() {
                            *default = Some(body);
                        } else {
                            let matches = item
                                .labels
                                .iter()
                                .map(|label| celox_sv_syntax::CaseMatch::Expression(*label))
                                .collect::<Vec<_>>();
                            let labels = self.native_case_labels(&matches, false, *chandles)?;
                            arms.push(CaseItemBase { labels, body });
                        }
                    }
                    match (*items).get(*next) {
                        Some(item) => {
                            *next += 1;
                            Step::Enter(item_name(native, item.item)?)
                        }
                        None => {
                            let mut output = std::mem::take(prelude);
                            output.push(Stmt::Case {
                                kind: *kind,
                                selector: selector.clone(),
                                items: std::mem::take(arms),
                                default: default.take(),
                            });
                            Step::Finish(output)
                        }
                    }
                }
            };
            match step {
                Step::Enter(name) => frames.push(self.enter(productions, &name)?),
                Step::Push(frame) => frames.push(frame),
                Step::Within(frame, name) => {
                    frames.push(frame);
                    frames.push(self.enter(productions, &name)?);
                }
                Step::Finish(output) => {
                    frames.pop();
                    finished = Some(output);
                }
                Step::FinishUnits(units) => {
                    frames.pop();
                    finished_units = Some(units);
                }
            }
        }
    }

    /// The statements of a `rand join` (18.17.5) of bias `bias`, interleaving
    /// productions `sequences` whose rules have units `units`, a `return`
    /// in a production setting its flag of `exits` and a `break` flag
    /// `statement_exit`. Each production first picks its rule as it would
    /// alone; then, while some have units left, one of them runs its next
    /// unit. It is picked from the thread's random number generator
    /// (18.14.2): with probability |2 * bias - 1| among the productions
    /// with the fewest units left when the bias is below 0.5, or the most
    /// when it is above, and otherwise among all with units left, each as
    /// likely. A bias of 0.5, the default, so prefers no length, and 0.0
    /// and 1.0 always the shortest and the longest (18.17.5 gives no
    /// formula). The bias is taken to 1/1024.
    fn join(
        &mut self,
        bias: Option<ExprId>,
        sequences: &[&RsProduction],
        units: Vec<Vec<Vec<Vec<Stmt>>>>,
        exits: &[(u64, String)],
        statement_exit: Option<&str>,
    ) -> Converted<Vec<Stmt>> {
        let ident = |name: &str| Expr::Ident(name.to_owned());
        let literal = |value: i64| Expr::Literal(value.to_string());
        let binary = |left: Expr, op: BinaryOp, right: Expr| Expr::Binary {
            left: Box::new(left),
            op,
            right: Box::new(right),
        };
        let assign = |name: &str, rhs: Expr| Stmt::Assign {
            lhs: LValue::Ident(name.to_owned()),
            rhs,
            nonblocking: false,
        };
        let mux = |condition: Expr, then_expr: Expr, else_expr: Expr| Expr::Mux {
            condition: Box::new(condition),
            then_expr: Box::new(then_expr),
            else_expr: Box::new(else_expr),
        };
        let draw = |max: Expr| Expr::Call {
            callee: Callee::System(SystemFunction::UrandomRange),
            args: vec![Some(max), Some(literal(0))],
            arg_order: Vec::new(),
        };
        let mut output = Vec::new();
        let bias_1024 = self.hidden(super::int_type()).1;
        match bias {
            Some(bias) => {
                output.extend(self.effects(&[bias])?);
                let value = self.native_value(bias)?;
                output.push(assign(
                    &bias_1024,
                    Expr::Call {
                        callee: Callee::System(SystemFunction::Rtoi),
                        args: vec![Some(binary(
                            value,
                            BinaryOp::Mul,
                            Expr::Literal("1024.0".into()),
                        ))],
                        arg_order: Vec::new(),
                    },
                ));
            }
            None => output.push(assign(&bias_1024, literal(512))),
        }
        // Each production's rule, its length and its next unit.
        let mut picks = Vec::new();
        for (index, production) in sequences.iter().enumerate() {
            let [rule, length, cursor] = [(); 3].map(|_| self.hidden(super::int_type()).1);
            output.push(set(&exits[index].1, false));
            output.push(assign(&length, literal(0)));
            output.push(assign(&cursor, literal(0)));
            output.push(assign(&rule, literal(0)));
            let lengths = units[index].iter().map(Vec::len).collect::<Vec<_>>();
            if let [only] = production.rules.as_slice()
                && only.weight.is_none()
            {
                output.push(assign(&length, literal(lengths[0] as i64)));
            } else {
                let weights = production
                    .rules
                    .iter()
                    .map(|rule| rule.weight)
                    .collect::<Vec<_>>();
                let headroom = (usize::BITS - (weights.len() - 1).leading_zeros()) as usize;
                let (prelude, weights, width) =
                    self.randcase_weights(&weights, 32, headroom, "randsequence production")?;
                output.extend(prelude);
                let bodies = lengths
                    .iter()
                    .enumerate()
                    .map(|(choice, units)| {
                        vec![
                            assign(&rule, literal(choice as i64)),
                            assign(&length, literal(*units as i64)),
                        ]
                    })
                    .collect();
                output.extend(self.weighted_choice(weights, width, bodies));
            }
            picks.push((rule, length, cursor));
        }
        // Whether production `index` has units left, and how many.
        let remaining = |index: usize| {
            let (_, length, cursor) = &picks[index];
            binary(
                binary(ident(cursor), BinaryOp::Lt, ident(length)),
                BinaryOp::LogicAnd,
                Expr::Unary {
                    op: UnaryOp::LogicNot,
                    expr: Box::new(ident(&exits[index].1)),
                },
            )
        };
        let mut body = Vec::new();
        let left = sequences
            .iter()
            .map(|_| self.hidden(super::int_type()).1)
            .collect::<Vec<_>>();
        let [fewest, most, target, count, nth, chosen] =
            [(); 6].map(|_| self.hidden(super::int_type()).1);
        body.push(assign(&fewest, literal(i64::from(i32::MAX))));
        body.push(assign(&most, literal(0)));
        for (index, left) in left.iter().enumerate() {
            let (_, length, cursor) = &picks[index];
            body.push(assign(
                left,
                mux(
                    remaining(index),
                    binary(ident(length), BinaryOp::Sub, ident(cursor)),
                    literal(0),
                ),
            ));
            body.push(Stmt::If {
                condition: binary(
                    binary(ident(left), BinaryOp::Gt, literal(0)),
                    BinaryOp::LogicAnd,
                    binary(ident(left), BinaryOp::Lt, ident(&fewest)),
                ),
                then_body: vec![assign(&fewest, ident(left))],
                else_body: Vec::new(),
            });
            body.push(Stmt::If {
                condition: binary(ident(left), BinaryOp::Gt, ident(&most)),
                then_body: vec![assign(&most, ident(left))],
                else_body: Vec::new(),
            });
        }
        // 0 picks among all with units left, 1 the fewest, 2 the most.
        let skew = binary(
            binary(ident(&bias_1024), BinaryOp::Mul, literal(2)),
            BinaryOp::Sub,
            literal(1024),
        );
        let strength = mux(
            binary(skew.clone(), BinaryOp::Lt, literal(0)),
            binary(literal(0), BinaryOp::Sub, skew),
            binary(
                binary(ident(&bias_1024), BinaryOp::Mul, literal(2)),
                BinaryOp::Sub,
                literal(1024),
            ),
        );
        body.push(assign(
            &target,
            mux(
                binary(draw(literal(1023)), BinaryOp::Lt, strength),
                mux(
                    binary(ident(&bias_1024), BinaryOp::Lt, literal(512)),
                    literal(1),
                    literal(2),
                ),
                literal(0),
            ),
        ));
        let member = |index: usize| {
            let left = ident(&left[index]);
            let is = |value: i64| binary(ident(&target), BinaryOp::Eq, literal(value));
            binary(
                binary(left.clone(), BinaryOp::Gt, literal(0)),
                BinaryOp::LogicAnd,
                binary(
                    binary(
                        is(0),
                        BinaryOp::LogicOr,
                        binary(
                            is(1),
                            BinaryOp::LogicAnd,
                            binary(left.clone(), BinaryOp::Eq, ident(&fewest)),
                        ),
                    ),
                    BinaryOp::LogicOr,
                    binary(
                        is(2),
                        BinaryOp::LogicAnd,
                        binary(left, BinaryOp::Eq, ident(&most)),
                    ),
                ),
            )
        };
        body.push(assign(&count, literal(0)));
        for index in 0..sequences.len() {
            body.push(Stmt::If {
                condition: member(index),
                then_body: vec![assign(
                    &count,
                    binary(ident(&count), BinaryOp::Add, literal(1)),
                )],
                else_body: Vec::new(),
            });
        }
        body.push(assign(
            &nth,
            draw(binary(ident(&count), BinaryOp::Sub, literal(1))),
        ));
        body.push(assign(&chosen, literal(0)));
        for index in 0..sequences.len() {
            body.push(Stmt::If {
                condition: member(index),
                then_body: vec![
                    Stmt::If {
                        condition: binary(ident(&nth), BinaryOp::Eq, literal(0)),
                        then_body: vec![assign(&chosen, literal(index as i64))],
                        else_body: Vec::new(),
                    },
                    assign(&nth, binary(ident(&nth), BinaryOp::Sub, literal(1))),
                ],
                else_body: Vec::new(),
            });
        }
        // The chosen production's next unit of its rule.
        let case = |selector: &str, arms: Vec<Vec<Stmt>>| Stmt::Case {
            kind: CaseKind::Exact,
            selector: ident(selector),
            items: arms
                .into_iter()
                .enumerate()
                .map(|(index, body)| CaseItemBase {
                    labels: vec![CaseLabel::Value(literal(index as i64))],
                    body,
                })
                .collect(),
            default: None,
        };
        let arms = units
            .into_iter()
            .zip(&picks)
            .map(|(rules, (rule, _, cursor))| {
                let rules = rules
                    .into_iter()
                    .map(|units| vec![case(cursor, units)])
                    .collect();
                vec![
                    case(rule, rules),
                    assign(cursor, binary(ident(cursor), BinaryOp::Add, literal(1))),
                ]
            })
            .collect();
        body.push(case(&chosen, arms));
        if let Some(flag) = statement_exit {
            body.push(Stmt::If {
                condition: ident(flag),
                then_body: vec![Stmt::Break],
                else_body: Vec::new(),
            });
        }
        let condition = (0..sequences.len())
            .map(remaining)
            .reduce(|left, right| binary(left, BinaryOp::LogicOr, right))
            .expect("a rand join joins two or more productions");
        output.push(Stmt::Loop {
            kind: LoopKind::While,
            init: Vec::new(),
            condition: Some(condition),
            step: Vec::new(),
            body,
        });
        Ok(output)
    }

    /// Count one more production the statement expands, rejecting more
    /// than [`MAX_EXPANDED`]: a production of a `rand join` too, whose
    /// rules are expanded without entering it.
    fn count_expansion(&mut self) -> Converted<()> {
        self.randsequence_expanded += 1;
        if self.randsequence_expanded > MAX_EXPANDED {
            return Err(unsupported(format!(
                "randsequence expanding more than {MAX_EXPANDED} productions"
            )));
        }
        Ok(())
    }

    /// The frame expanding production `name`: its rule, or its rules one of
    /// which is picked by their weights, evaluated first.
    fn enter<'p>(
        &mut self,
        productions: &HashMap<String, &'p RsProduction>,
        name: &str,
    ) -> Converted<Frame<'p>> {
        self.count_expansion()?;
        let production = productions[name];
        if let [rule] = production.rules.as_slice()
            && rule.weight.is_none()
        {
            return Ok(Frame::Rule {
                rule,
                next: 0,
                output: Vec::new(),
                exit: None,
                armed: false,
                units: None,
            });
        }
        let weights = production
            .rules
            .iter()
            .map(|rule| rule.weight)
            .collect::<Vec<_>>();
        // Weights are integral (18.17.1), a rule's default weight of 1 an
        // `int`. Unlike randcase's (18.16), their sum is not given a
        // precision: it is taken wide enough not to wrap, one bit wider
        // than the widest weight per doubling of the rules.
        let headroom = (usize::BITS - (weights.len() - 1).leading_zeros()) as usize;
        let (prelude, weights, width) =
            self.randcase_weights(&weights, 32, headroom, "randsequence production")?;
        Ok(Frame::Choice {
            rules: &production.rules,
            next: 0,
            prelude,
            weights,
            width,
            items: Vec::new(),
        })
    }
}

/// The assignment of `value` to flag `flag`.
fn set(flag: &str, value: bool) -> Stmt {
    Stmt::Assign {
        lhs: LValue::Ident(flag.to_owned()),
        rhs: Expr::Literal(if value { "1'b1" } else { "1'b0" }.into()),
        nonblocking: false,
    }
}

/// Whether none of flags `flags` is set.
fn clear(flags: &[String]) -> Expr {
    let any = flags
        .iter()
        .map(|flag| Expr::Ident(flag.clone()))
        .reduce(|left, right| Expr::Binary {
            left: Box::new(left),
            op: BinaryOp::LogicOr,
            right: Box::new(right),
        })
        .expect("a flag");
    Expr::Unary {
        op: UnaryOp::LogicNot,
        expr: Box::new(any),
    }
}

/// Statements `body` of an item of a rule, which run only while no flag of
/// `flags` is set once an item before them may have set one, `armed`.
fn guard(body: Vec<Stmt>, armed: bool, flags: &[String]) -> Vec<Stmt> {
    if !armed || flags.is_empty() || body.is_empty() {
        return body;
    }
    vec![Stmt::If {
        condition: clear(flags),
        then_body: body,
        else_body: Vec::new(),
    }]
}

/// Statements `body` of a code block with each exit, a `disable` of a marker
/// of `flags_of`, setting the marker's flag instead; the statements after it
/// run only while no flag is set: the rest of each statement list it is in
/// and, leaving each loop it is in (`looping` for the innermost), the rest
/// of the code block's loops.
fn lower_exits(body: Vec<Stmt>, flags_of: &HashMap<u64, String>, looping: bool) -> Vec<Stmt> {
    if flags_of.is_empty() {
        return body;
    }
    let mut flags = flags_of.values().cloned().collect::<Vec<_>>();
    flags.sort();
    flags.dedup();
    let exits = |stmt: &Stmt| {
        let mut found = false;
        stmt.walk(&mut |stmt| {
            found |= matches!(stmt, Stmt::DisableBlock(marker) if flags_of.contains_key(marker));
        });
        found
    };
    let mut output = Vec::new();
    let mut statements = body.into_iter();
    while let Some(stmt) = statements.next() {
        if !exits(&stmt) {
            output.push(stmt);
            continue;
        }
        match stmt {
            Stmt::DisableBlock(marker) => {
                output.push(set(&flags_of[&marker], true));
                if looping {
                    output.push(Stmt::Break);
                }
            }
            Stmt::If {
                condition,
                then_body,
                else_body,
            } => output.push(Stmt::If {
                condition,
                then_body: lower_exits(then_body, flags_of, looping),
                else_body: lower_exits(else_body, flags_of, looping),
            }),
            Stmt::Case {
                kind,
                selector,
                items,
                default,
            } => output.push(Stmt::Case {
                kind,
                selector,
                items: items
                    .into_iter()
                    .map(|item| CaseItemBase {
                        labels: item.labels,
                        body: lower_exits(item.body, flags_of, looping),
                    })
                    .collect(),
                default: default.map(|body| lower_exits(body, flags_of, looping)),
            }),
            Stmt::Block { id, body } => output.push(Stmt::Block {
                id,
                body: lower_exits(body, flags_of, looping),
            }),
            Stmt::Loop {
                kind,
                init,
                condition,
                step,
                body,
            } => {
                output.push(Stmt::Loop {
                    kind,
                    init,
                    condition,
                    step,
                    body: lower_exits(body, flags_of, true),
                });
                // Leaving this loop leaves the one around it too.
                if looping {
                    output.push(Stmt::If {
                        condition: clear(&flags),
                        then_body: Vec::new(),
                        else_body: vec![Stmt::Break],
                    });
                }
            }
            other => output.push(other),
        }
        // In a loop each exit has left it; otherwise the rest runs only
        // while no flag is set.
        let rest = statements.by_ref().collect::<Vec<_>>();
        if !rest.is_empty() {
            let rest = lower_exits(rest, flags_of, looping);
            if looping {
                output.extend(rest);
            } else {
                output.push(Stmt::If {
                    condition: clear(&flags),
                    then_body: rest,
                    else_body: Vec::new(),
                });
            }
        }
    }
    output
}

/// The productions of `randsequence` by name; a name declared twice is an
/// error.
fn productions(randsequence: &Randsequence) -> Converted<HashMap<String, &RsProduction>> {
    let mut productions = HashMap::default();
    for production in &randsequence.productions {
        let name = instances::normalize_identifier(&production.name.spelling);
        if productions.insert(name.clone(), production).is_some() {
            return Err(AnalyzerError::Parse(format!(
                "randsequence production `{name}` declared twice"
            )));
        }
    }
    Ok(productions)
}

/// The production a production item names, by itself (18.17.7 passes
/// arguments).
fn item_name(
    native: &celox_sv_syntax::Declarations,
    item: celox_sv_syntax::ExprId,
) -> Converted<String> {
    match &native.expression(item).kind {
        ExprKind::Name(name) => Ok(instances::normalize_identifier(&name.spelling)),
        ExprKind::Call { .. } => Err(unsupported("randsequence production with arguments")),
        _ => Err(unsupported("randsequence production item")),
    }
}

/// Check the productions that `main` reaches: each named one is declared,
/// none reaches itself, and each uses only the forms expanded here.
fn check(
    native: &celox_sv_syntax::Declarations,
    productions: &HashMap<String, &RsProduction>,
    main: &str,
) -> Converted<()> {
    // Depth-first, a production on the path when its items are entered.
    enum Visit {
        Enter(String),
        Leave(String),
    }
    let mut path = HashSet::default();
    let mut done = HashSet::default();
    let mut jobs = vec![Visit::Enter(main.to_owned())];
    while let Some(job) = jobs.pop() {
        let name = match job {
            Visit::Leave(name) => {
                path.remove(&name);
                done.insert(name);
                continue;
            }
            Visit::Enter(name) => name,
        };
        if path.contains(&name) {
            return Err(unsupported(format!(
                "recursive randsequence production `{name}`"
            )));
        }
        if done.contains(&name) {
            continue;
        }
        let production =
            productions
                .get(&name)
                .ok_or_else(|| AnalyzerError::UndeclaredIdentifier {
                    name: name.clone(),
                    context: "no randsequence production of this name".into(),
                })?;
        if production.return_type.is_some() || production.formals.is_some() {
            return Err(unsupported(format!(
                "randsequence production `{name}` with a result or formals"
            )));
        }
        path.insert(name.clone());
        jobs.push(Visit::Leave(name));
        for rule in &production.rules {
            // `rand join` interleaves two or more production items
            // (A.6.12).
            if rule.join.is_some()
                && (rule.items.len() < 2
                    || !rule
                        .items
                        .iter()
                        .all(|item| matches!(item, RsProd::Item(_))))
            {
                return Err(AnalyzerError::Parse(
                    "randsequence rand join of fewer than two production items".into(),
                ));
            }
            if rule.weight_code.is_some() {
                return Err(unsupported("randsequence weight code block"));
            }
            for item in &rule.items {
                match item {
                    RsProd::Item(item) => jobs.push(Visit::Enter(item_name(native, *item)?)),
                    RsProd::Code(block) => check_code(native, *block)?,
                    RsProd::If {
                        then, otherwise, ..
                    } => {
                        for item in std::iter::once(then).chain(otherwise) {
                            jobs.push(Visit::Enter(item_name(native, *item)?));
                        }
                    }
                    RsProd::Repeat { item, .. } => {
                        jobs.push(Visit::Enter(item_name(native, *item)?))
                    }
                    RsProd::Case { items, .. } => {
                        // At most one default (18.17.3).
                        if items.iter().filter(|item| item.labels.is_empty()).count() > 1 {
                            return Err(AnalyzerError::Parse(
                                "randsequence case production with more than one default".into(),
                            ));
                        }
                        for item in items {
                            jobs.push(Visit::Enter(item_name(native, item.item)?));
                        }
                    }
                }
            }
        }
    }
    Ok(())
}

/// Reject in code block `block` a `continue` outside a loop, which has
/// nothing to continue (12.8), and a `return` with a value, which gives a
/// production its value (18.17.7).
fn check_code(native: &celox_sv_syntax::Declarations, block: StatementId) -> Converted<()> {
    let mut pending = vec![(block, false)];
    while let Some((id, looping)) = pending.pop() {
        let kind = &native.statement(id).kind;
        match kind {
            StatementKind::Continue if !looping => {
                return Err(unsupported(
                    "continue outside a loop in a randsequence code block",
                ));
            }
            StatementKind::Return(Some(_)) => {
                return Err(unsupported("randsequence production value returned"));
            }
            // A nested randsequence is checked when it is expanded.
            StatementKind::Unconverted { category, .. } if category == "randsequence" => {}
            StatementKind::Loop { .. } => pending.extend(kind.children().map(|id| (id, true))),
            _ => pending.extend(kind.children().map(|id| (id, looping))),
        }
    }
    Ok(())
}

/// The `break` statements outside loops and the `return` statements of code
/// block `block`, those of `randsequence` statements in it aside: the first
/// leave the statement, the others the production (18.17.6). Neither can
/// leave a fork branch (12.8, 9.3.2).
fn code_exits(
    native: &celox_sv_syntax::Declarations,
    block: StatementId,
) -> Converted<(Vec<StatementId>, Vec<StatementId>)> {
    let mut breaks = Vec::new();
    let mut returns = Vec::new();
    // Each statement, whether in a loop of the block, and whether in a fork
    // branch.
    let mut pending = vec![(block, false, false)];
    while let Some((id, looping, forked)) = pending.pop() {
        let kind = &native.statement(id).kind;
        match kind {
            StatementKind::Break if !looping && forked => {
                return Err(AnalyzerError::Parse(
                    "break leaving a fork branch in a randsequence code block".into(),
                ));
            }
            StatementKind::Return(_) if forked => {
                return Err(AnalyzerError::Parse(
                    "return in a fork branch in a randsequence code block".into(),
                ));
            }
            StatementKind::Break if !looping => breaks.push(id),
            StatementKind::Return(_) => returns.push(id),
            StatementKind::Unconverted { category, .. } if category == "randsequence" => {}
            StatementKind::Loop { .. } => {
                pending.extend(kind.children().map(|id| (id, true, forked)));
            }
            StatementKind::Parallel { .. } => {
                pending.extend(kind.children().map(|id| (id, false, true)));
            }
            _ => pending.extend(kind.children().map(|id| (id, looping, forked))),
        }
    }
    Ok((breaks, returns))
}
