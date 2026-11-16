//! `disable` of a named block from outside it (IEEE 1800-2023 9.6.2).
//!
//! A `disable` names a block that runs in another process, or in the same
//! process outside the statement: the block's activity ends and its process
//! continues after the block. A named block that some `disable` of the scope
//! names runs as the one branch of a `fork`-`join`, which records the branch
//! process in a variable of the scope while it runs:
//!
//! ```text
//! begin : b ... end   fork begin handle = self(); armed = 1; ...; armed = 0; end join
//! disable b;          if (armed) begin kill(handle); armed = 0; end
//! ```
//!
//! Killing the branch completes the join, so the process of the block
//! continues after it.
use super::*;

/// The named blocks of a scope that a `disable` reaches from outside them.
#[derive(Default)]
pub(in crate::ast) struct ExternalDisables {
    /// How many `disable` statements of the scope name each simple name,
    /// counted once.
    names: Option<HashMap<String, usize>>,
    /// How many of them each named block holds itself, by block.
    inside: HashMap<u64, usize>,
    /// The blocks that run as a branch, by name.
    pub(in crate::ast) blocks: HashSet<String>,
    /// The names of more than one such block, which no `disable` from
    /// outside may name.
    pub(in crate::ast) ambiguous: HashSet<String>,
    /// The names that a `disable` outside its block names.
    pub(in crate::ast) targets: Vec<String>,
}

/// The variables of the branch of block `label`: its process and whether it
/// runs.
pub(in crate::ast) fn variables(label: &str) -> (String, String) {
    (
        format!("disable%{label}%process"),
        format!("disable%{label}%armed"),
    )
}

/// The variables of the scope that the branch of block `label` keeps.
pub(in crate::ast) fn signals(label: &str, types: &mut semantic_types::TyTable) -> [Signal; 2] {
    let (process, armed) = variables(label);
    [
        Signal::new(process, classes::handle(types, processes::CLASS)),
        Signal::new(armed, control::unsigned_type(1)),
    ]
}

impl BodyBuilder<'_, '_, '_> {
    /// The simple names that the `disable` statements in `tokens` name.
    fn disabled_names(&self, tokens: celox_sv_syntax::TokenRange) -> HashMap<String, usize> {
        let words: Vec<_> = self
            .tree
            .declarations()
            .tokens
            .get(tokens.start..tokens.end)
            .into_iter()
            .flatten()
            .filter(|token| !token.is_trivia())
            .map(|token| token.text.as_str())
            .collect();
        let mut names = HashMap::default();
        for window in words.windows(3) {
            if window[0] == "disable" && !matches!(window[2], "." | "::") {
                *names
                    .entry(instances::normalize_identifier(window[1]))
                    .or_default() += 1;
            }
        }
        names
    }

    /// Record how many `disable` statements of named block `label`, at
    /// `tokens`, name it from within.
    pub(super) fn enter_named_block(
        &mut self,
        label: &str,
        id: u64,
        tokens: celox_sv_syntax::TokenRange,
    ) {
        let inside = self.disabled_names(tokens).remove(label).unwrap_or(0);
        self.state.disables.inside.insert(id, inside);
    }

    /// Whether a `disable` of the scope names block `label`, numbered `id`,
    /// from outside it.
    fn disabled_from_outside(&mut self, label: &str, id: u64) -> bool {
        if self.state.disables.names.is_none() {
            let native = self.tree.declarations();
            let tokens = match self.syntax().scope_handle().unit(native) {
                Some(unit) => unit.tokens,
                None => celox_sv_syntax::TokenRange {
                    start: 0,
                    end: native.tokens.len(),
                },
            };
            self.state.disables.names = Some(self.disabled_names(tokens));
        }
        let all = self
            .state
            .disables
            .names
            .as_ref()
            .and_then(|names| names.get(label))
            .copied()
            .unwrap_or(0);
        all > self.state.disables.inside.get(&id).copied().unwrap_or(0)
    }

    /// The statements of named block `label`, `body`, as the branch that a
    /// `disable` from outside it ends, when some `disable` of the scope
    /// outside the block names it; otherwise `body` itself. A block whose statements leave it
    /// otherwise, or in a context without processes, stays as it is, and a
    /// `disable` from outside it remains unsupported.
    pub(super) fn external_disable_block(
        &mut self,
        label: String,
        id: u64,
        body: Vec<Stmt>,
    ) -> Vec<Stmt> {
        // Only a hierarchical name reaches a block of a subroutine from
        // outside it (23.6), and those stay unsupported.
        if self.owner.is_some()
            || !self.disabled_from_outside(&label, id)
            || self
                .allow_timing("disable of a named block from outside it")
                .is_err()
        {
            return body;
        }
        let mut leaves = false;
        for stmt in &body {
            stmt.walk(&mut |stmt| {
                leaves |= matches!(stmt, Stmt::Return(_) | Stmt::Break | Stmt::Continue)
            });
        }
        if leaves {
            return body;
        }
        if !self.state.disables.blocks.insert(label.clone()) {
            self.state.disables.ambiguous.insert(label);
            return body;
        }
        let (process, armed) = variables(&label);
        let assign = |name: &str, rhs: Expr| Stmt::Assign {
            lhs: LValue::Ident(name.to_string()),
            rhs,
            nonblocking: false,
        };
        let mut branch = vec![
            assign(
                &process,
                events::call(celox_design::ProcessOp::SelfHandle, Vec::new()),
            ),
            assign(&armed, Expr::Literal("1'b1".into())),
        ];
        branch.extend(body);
        branch.push(assign(&armed, Expr::Literal("1'b0".into())));
        vec![Stmt::Fork {
            join: celox_design::ForkJoin::All,
            branches: vec![branch],
        }]
    }

    /// `disable label` of a block outside the statement.
    pub(super) fn external_disable(&mut self, label: String) -> Vec<Stmt> {
        let (process, armed) = variables(&label);
        self.state.disables.targets.push(label);
        vec![Stmt::If {
            condition: Expr::Ident(armed.clone()),
            then_body: vec![
                Stmt::Eval(events::call(
                    celox_design::ProcessOp::Kill,
                    vec![Expr::Ident(process)],
                )),
                Stmt::Assign {
                    lhs: LValue::Ident(armed),
                    rhs: Expr::Literal("1'b0".into()),
                    nonblocking: false,
                },
            ],
            else_body: Vec::new(),
        }]
    }
}
