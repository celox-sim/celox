//! The assertion control tasks `$asserton` and `$assertoff` (IEEE 1800-2023
//! 20.11) without arguments, which stop and restart the checking of every
//! assertion of the design.
//!
//! A design that calls one has one variable, zero while assertions are
//! checked, that every module shares as it shares a package variable. The
//! tasks assign it, and each immediate, deferred and concurrent assertion
//! starts an attempt only while it is zero: an attempt that already started,
//! a queued deferred report included, is not affected.
use super::*;

/// The package, which no source declares, of the assertion control state.
pub(super) const PACKAGE: &str = crate::ASSERTION_CONTROL_PACKAGE;

/// The variable of [`PACKAGE`] and of each module that is one while
/// assertions are off.
pub(super) const OFF: &str = crate::ASSERTIONS_OFF;

/// Whether a source calls `$asserton` or `$assertoff`. A call that
/// elaboration drops, in a generate block it does not select (27.5) or a
/// module no instance names, also counts: it never assigns the state, which
/// stays zero, so the assertions behave as without control.
pub(super) fn called(native: &celox_sv_syntax::Declarations) -> bool {
    native.expressions.iter().any(|expression| {
        matches!(
            &expression.kind,
            celox_sv_syntax::ExprKind::Name(name)
                if matches!(name.spelling.as_str(), "$asserton" | "$assertoff")
        )
    })
}

/// The state module of [`PACKAGE`].
pub(super) fn state() -> crate::ir::Module {
    crate::ir::Module::shared_variables(PACKAGE, &[OFF.to_string()], 1)
}

/// The variable of a module that denotes the assertion control state.
pub(super) fn signal() -> Signal {
    let mut signal = Signal::new(OFF.to_string(), Type::new(TypeKind::Bit));
    signal.package_variable = Some((PACKAGE.to_string(), OFF.to_string()));
    signal
}

/// `$assertoff;` (`off`) or `$asserton;`.
pub(super) fn set(off: bool) -> Stmt {
    Stmt::Assign {
        lhs: LValue::Ident(OFF.to_string()),
        rhs: Expr::Literal(if off { "1'b1" } else { "1'b0" }.to_string()),
        nonblocking: false,
    }
}

/// Whether assertions are on, so that an assertion starts an attempt.
pub(super) fn on() -> Expr {
    Expr::Unary {
        op: UnaryOp::LogicNot,
        expr: Box::new(Expr::Ident(OFF.to_string())),
    }
}

/// The statements of an assertion, which start an attempt only while
/// assertions are on.
pub(super) fn checked(assertion: Vec<Stmt>) -> Stmt {
    Stmt::If {
        condition: on(),
        then_body: assertion,
        else_body: Vec::new(),
    }
}
