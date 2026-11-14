//! `randsequence` (IEEE 1800-2023 18.17).
//!
//! The statement is expanded where it is written: each production in turn,
//! from the main one, as its rule's items in order, a production as its own
//! expansion and a code block as its statements, which see the variables of
//! the scope around the statement. A production of several rules picks one
//! at random by their weights (18.17.1), 1 unless given, from the thread's
//! random number generator as `randcase` does (18.14.2, 18.16). A
//! production that refers to itself, directly or through others, has no
//! expansion and is rejected; so are the forms not provided yet, each by
//! its name.

use super::*;
use celox_sv_syntax::{
    ExprKind, Randsequence, RsProd, RsProduction, RsRule, StatementId, StatementKind,
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
    /// Rule `rule`, its items before `next` expanded into `output`.
    Rule {
        rule: &'p RsRule,
        next: usize,
        output: Vec<Stmt>,
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
}

/// What the frame on top of the stack does next.
enum Step<'p> {
    /// Expand the production of this name.
    Enter(String),
    /// Expand this rule.
    Push(Frame<'p>),
    /// The frame is done, with these statements.
    Finish(Vec<Stmt>),
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
        let expansion = self.expand(&productions, &main);
        self.randsequence_depth -= 1;
        expansion
    }

    /// The statements of production `main` (see the module documentation).
    fn expand(
        &mut self,
        productions: &HashMap<String, &RsProduction>,
        main: &str,
    ) -> Converted<Vec<Stmt>> {
        let mut frames = vec![self.enter(productions, main)?];
        // The statements of the frame last finished, for the one below.
        let mut finished = None;
        loop {
            let native = self.tree.declarations();
            let step = match frames.last_mut() {
                None => return Ok(finished.expect("the main production is expanded")),
                Some(Frame::Rule { rule, next, output }) => {
                    output.extend(finished.take().into_iter().flatten());
                    loop {
                        let Some(item) = rule.items.get(*next) else {
                            break Step::Finish(std::mem::take(output));
                        };
                        *next += 1;
                        match item {
                            RsProd::Code(block) => {
                                output.extend(self.native_body_machine(&[*block])?);
                            }
                            RsProd::Item(item) => break Step::Enter(item_name(native, *item)?),
                            _ => unreachable!("rejected by `check`"),
                        }
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
            };
            match step {
                Step::Enter(name) => frames.push(self.enter(productions, &name)?),
                Step::Push(frame) => frames.push(frame),
                Step::Finish(output) => {
                    frames.pop();
                    finished = Some(output);
                }
            }
        }
    }

    /// The frame expanding production `name`: its rule, or its rules one of
    /// which is picked by their weights, evaluated first.
    fn enter<'p>(
        &mut self,
        productions: &HashMap<String, &'p RsProduction>,
        name: &str,
    ) -> Converted<Frame<'p>> {
        self.randsequence_expanded += 1;
        if self.randsequence_expanded > MAX_EXPANDED {
            return Err(unsupported(format!(
                "randsequence expanding more than {MAX_EXPANDED} productions"
            )));
        }
        let production = productions[name];
        if let [rule] = production.rules.as_slice()
            && rule.weight.is_none()
        {
            return Ok(Frame::Rule {
                rule,
                next: 0,
                output: Vec::new(),
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
            if rule.join.is_some() {
                return Err(unsupported("randsequence rand join"));
            }
            if rule.weight_code.is_some() {
                return Err(unsupported("randsequence weight code block"));
            }
            for item in &rule.items {
                match item {
                    RsProd::Item(item) => jobs.push(Visit::Enter(item_name(native, *item)?)),
                    RsProd::Code(block) => check_code(native, *block)?,
                    RsProd::If { .. } => return Err(unsupported("randsequence if production")),
                    RsProd::Repeat { .. } => {
                        return Err(unsupported("randsequence repeat production"));
                    }
                    RsProd::Case { .. } => return Err(unsupported("randsequence case production")),
                }
            }
        }
    }
    Ok(())
}

/// Reject in code block `block` a `break` outside a loop and a `return`,
/// which leave the randsequence or the production (18.17.6), and a
/// `continue` outside a loop. Those in a loop of the block are its own
/// (12.8).
fn check_code(native: &celox_sv_syntax::Declarations, block: StatementId) -> Converted<()> {
    let mut pending = vec![(block, false)];
    while let Some((id, looping)) = pending.pop() {
        let kind = &native.statement(id).kind;
        match kind {
            StatementKind::Break if !looping => {
                return Err(unsupported("break in a randsequence code block"));
            }
            StatementKind::Return(_) => {
                return Err(unsupported("return in a randsequence code block"));
            }
            StatementKind::Continue if !looping => {
                return Err(unsupported(
                    "continue outside a loop in a randsequence code block",
                ));
            }
            // A nested randsequence is checked when it is expanded.
            StatementKind::Unconverted { category, .. } if category == "randsequence" => {}
            StatementKind::Loop { .. } => pending.extend(kind.children().map(|id| (id, true))),
            _ => pending.extend(kind.children().map(|id| (id, looping))),
        }
    }
    Ok(())
}
