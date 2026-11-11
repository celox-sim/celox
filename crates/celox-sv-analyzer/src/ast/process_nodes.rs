//! Processes of the AST: construction and accessors.

use super::*;

impl CombProcess {
    /// Where an `always` procedure is written, for diagnostics about it as
    /// a process.
    pub fn process_site(&self) -> Option<&crate::procedural::ProcessSite> {
        self.process_site.as_ref()
    }

    pub fn site(&self) -> Option<&crate::procedural::SourceSpan> {
        self.site.as_ref()
    }

    /// Reads removed from executable branches still affect implicit sensitivity.
    pub fn sensitivity_body(&self) -> &[Stmt] {
        &self.sensitivity_body
    }

    /// Position among the elaborated module items, shared across process kinds.
    pub fn creation_order(&self) -> usize {
        self.creation_order
    }

    pub(super) fn new(
        kind: CombProcessKind,
        condition: Option<ConstExpr>,
        assignments: Vec<Assignment>,
    ) -> Self {
        let body = assignments
            .iter()
            .map(|assignment| Stmt::Assign {
                lhs: assignment.lhs.clone(),
                rhs: assignment.rhs.clone(),
                nonblocking: false,
            })
            .collect();
        Self {
            delay: None,
            net_delay: false,
            creation_order: 0,
            site: None,
            process_site: None,
            sensitivity_body: Vec::new(),
            kind,
            condition,
            assignments,
            body,
        }
    }

    pub(super) fn procedural(
        kind: CombProcessKind,
        condition: Option<ConstExpr>,
        body: Vec<Stmt>,
    ) -> Self {
        Self {
            delay: None,
            net_delay: false,
            creation_order: 0,
            site: None,
            process_site: None,
            sensitivity_body: Vec::new(),
            kind,
            condition,
            assignments: Vec::new(),
            body,
        }
    }

    pub fn body(&self) -> &[Stmt] {
        &self.body
    }

    pub fn kind(&self) -> CombProcessKind {
        self.kind
    }

    pub fn condition(&self) -> Option<&ConstExpr> {
        self.condition.as_ref()
    }

    pub fn assignments(&self) -> &[Assignment] {
        &self.assignments
    }
}

impl FfProcess {
    pub fn is_always_ff(&self) -> bool {
        self.always_ff
    }

    /// Position among the elaborated module items, shared across process kinds.
    pub fn creation_order(&self) -> usize {
        self.creation_order
    }

    pub fn site(&self) -> Option<&crate::procedural::ProcessSite> {
        self.site.as_ref()
    }

    pub(super) fn new(events: Vec<FfEvent>, body: Vec<Stmt>) -> Self {
        Self {
            always_ff: false,
            suspendable: false,
            creation_order: 0,
            site: None,
            events,
            body,
        }
    }

    pub fn events(&self) -> &[FfEvent] {
        &self.events
    }

    pub fn body(&self) -> &[Stmt] {
        &self.body
    }
}

impl InitialProcess {
    /// Position among the elaborated module items, shared across process kinds.
    pub fn creation_order(&self) -> usize {
        self.creation_order
    }

    pub fn site(&self) -> Option<&crate::procedural::ProcessSite> {
        self.site.as_ref()
    }

    pub fn condition(&self) -> Option<&ConstExpr> {
        self.condition.as_ref()
    }

    /// The procedure's simulation phase.
    pub fn kind(&self) -> celox_design::ProcessKind {
        self.kind
    }

    /// A final procedure runs once when the simulation finishes (9.2.3).
    pub fn is_final(&self) -> bool {
        self.kind == celox_design::ProcessKind::Final
    }

    /// Compiler-generated storage initialization, without a user static process.
    pub fn is_synthetic(&self) -> bool {
        self.synthetic
    }

    /// Whether this process holds variable declaration initializers.
    pub fn is_initializer(&self) -> bool {
        self.kind == celox_design::ProcessKind::Initializer
    }

    /// Whether this is an `always` process, which runs its body forever and
    /// never finishes on its own.
    pub fn repeats(&self) -> bool {
        self.repeats
    }

    pub fn body(&self) -> &[Stmt] {
        &self.body
    }
}

impl FfEvent {
    pub(super) fn new(edge: FfEdge, signal: String) -> Self {
        Self { edge, signal }
    }

    pub fn edge(&self) -> FfEdge {
        self.edge
    }

    pub fn signal(&self) -> &str {
        &self.signal
    }
}

impl ConditionalAssignment {
    pub fn condition(&self) -> Option<&Expr> {
        self.condition.as_ref()
    }

    pub fn assignment(&self) -> &Assignment {
        &self.assignment
    }
}
