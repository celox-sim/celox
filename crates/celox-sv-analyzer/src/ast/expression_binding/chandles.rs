//! The uses IEEE 1800-2023 6.14 allows for a `chandle`: equality with
//! another chandle or `null`, a Boolean test, and assignment from another
//! chandle or `null`.
use super::*;

/// Whether native expression `id` is a chandle: a chandle variable, a
/// chandle property, or an element of a container of chandles.
pub(in crate::ast) fn native_chandle(
    tree: &AnalysisContext<'_>,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
    session: &mut Session,
) -> bool {
    let id = ungroup(id, native);
    if let Some(chandle) = locator_index(tree, id, native, dimensions, session) {
        return chandle;
    }
    match &native.expression(id).kind {
        ExprKind::Name(name) => {
            if let Some(shape) = dimensions.get(&instances::normalize_identifier(&name.spelling)) {
                return shape.chandle && shape.unpacked.is_empty();
            }
        }
        ExprKind::Select {
            expression,
            dimension,
        } => {
            // A slice of an unpacked array is an array (7.4.6); `q[$]` is
            // the last element of a queue (7.10.1).
            if !element_select(&dimension.kind) {
                return false;
            }
            if let Ok(Some(container)) =
                containers::native_type(*expression, native, tree, session, dimensions)
            {
                return container.element.chandle;
            }
            let mut base = ungroup(*expression, native);
            let mut indices = 1;
            while let ExprKind::Select {
                expression,
                dimension,
            } = &native.expression(base).kind
            {
                if !element_select(&dimension.kind) {
                    return false;
                }
                base = ungroup(*expression, native);
                indices += 1;
            }
            return array_of_chandles(tree, base, native, dimensions, session)
                .is_some_and(|rank| rank == indices);
        }
        ExprKind::Call { .. } => {
            return call_result(tree, id, native, dimensions, session) == Some(true);
        }
        // A pop of a queue may omit its parentheses.
        ExprKind::Member {
            expression: receiver,
            separator,
            name,
        } if separator == "." => {
            if let Some(chandle) =
                container_method(tree, *receiver, &name.spelling, native, dimensions, session)
            {
                return chandle;
            }
        }
        // A conditional choosing between chandles, or a chandle and `null`.
        ExprKind::Conditional {
            then_expression,
            else_expression,
            ..
        } => {
            // Each branch is classified once, or nested conditionals would
            // be classified exponentially often.
            let branches = [*then_expression, *else_expression]
                .map(|branch| native_chandle(tree, branch, native, dimensions, session));
            return branches.contains(&true)
                && [*then_expression, *else_expression]
                    .iter()
                    .zip(branches)
                    .all(|(&branch, chandle)| chandle || null(branch, native));
        }
        _ => {}
    }
    // A member of a variable of unpacked structure type is a variable.
    if let Some(variable) = unpacked_structs::native_member(id, native, dimensions) {
        return dimensions
            .get(&variable)
            .is_some_and(|shape| shape.chandle && shape.unpacked.is_empty());
    }
    // A member selected through a virtual interface has its declared type.
    if virtual_member(tree, id, native, dimensions, session) {
        return true;
    }
    // A signal of another instance or interface named by a path.
    if let ExprKind::Member { .. } = &native.expression(id).kind
        && let Ok((mut parts, None)) = direct::path(id, native, tree, session, dimensions)
        && parts.len() > 1
        && packed_structs::bind_interface_path(tree, &mut parts, dimensions)
        && let [variable] = parts.as_slice()
    {
        return dimensions
            .get(variable)
            .is_some_and(|shape| shape.chandle && shape.unpacked.is_empty());
    }
    // A member of an unpacked structure element of a container.
    if let Ok(Some((member, _))) = containers::element_member(id, native, tree, session, dimensions)
    {
        return member.chandle && member.unpacked_ranges.is_empty();
    }
    if let Ok(Some(variable)) = objects::static_property(tree, session, id, native, dimensions) {
        return dimensions
            .get(&variable)
            .is_some_and(|shape| shape.chandle && shape.unpacked.is_empty());
    }
    objects::property(tree, session, id, native, dimensions)
        .ok()
        .flatten()
        .is_some_and(|(property, _)| {
            property.r#type.chandle && property.r#type.unpacked_ranges.is_empty()
        })
}

/// Whether `id`, an iterator index query `item.index` of a locator method
/// (7.12.4), has the chandle index type of its array; `None` if `id` is no
/// such query.
#[inline(never)]
fn locator_index(
    tree: &AnalysisContext<'_>,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
    session: &mut Session,
) -> Option<bool> {
    let Ok(Some(Expr::Ident(index))) = locators::index(id, native, tree, session, dimensions)
    else {
        return None;
    };
    Some(
        dimensions
            .get(&index)
            .is_some_and(|shape| shape.chandle && shape.unpacked.is_empty()),
    )
}

/// Whether call native expression `id` of a function, method or DPI-C
/// import returns a chandle, `None` if the callee has no known result.
fn call_result(
    tree: &AnalysisContext<'_>,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
    session: &mut Session,
) -> Option<bool> {
    let ExprKind::Call { expression, .. } = &native.expression(ungroup(id, native)).kind else {
        return None;
    };
    match &native.expression(ungroup(*expression, native)).kind {
        // No system function returns a chandle.
        ExprKind::Name(name) if name.spelling.starts_with('$') => return Some(false),
        // Of the built-in container methods, only the pops return an element.
        ExprKind::Member {
            expression: receiver,
            separator,
            name,
        } if separator == "." => {
            if let Some(chandle) =
                container_method(tree, *receiver, &name.spelling, native, dimensions, session)
            {
                return Some(chandle);
            }
        }
        _ => {}
    }
    dimensions
        .function_return_types
        .get(&callee_name(
            tree,
            *expression,
            native,
            dimensions,
            session,
        )?)
        .map(|metadata| metadata.chandle)
}

/// The subroutine that callee `callee`, a function, a method or a DPI-C
/// import, names.
fn callee_name(
    tree: &AnalysisContext<'_>,
    callee: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
    session: &mut Session,
) -> Option<String> {
    objects::method(tree, session, callee, native, dimensions)
        .ok()
        .flatten()
        .map(|(name, ..)| name)
        .or_else(|| {
            direct::bound_callable_name(tree, session, callee, native, dimensions)
                .ok()
                .flatten()
        })
}

/// The number of unpacked dimensions of array `id` of chandles: a variable
/// or a signal of another scope named by a path. `None` if `id` is no such
/// array.
#[inline(never)]
fn array_of_chandles(
    tree: &AnalysisContext<'_>,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
    session: &mut Session,
) -> Option<usize> {
    variable(tree, id, native, dimensions, session)
        .filter(|shape| shape.chandle && shape.container.is_none())
        .map(|shape| shape.unpacked.len())
}

/// The shape of variable `id`: a name, or a signal of another scope named
/// by a path. `None` if `id` is no such variable.
fn variable<'a>(
    tree: &AnalysisContext<'_>,
    id: ExprId,
    native: &Declarations,
    dimensions: &'a PackedDimensions,
    session: &mut Session,
) -> Option<&'a VariableDimensions> {
    match &native.expression(id).kind {
        ExprKind::Name(name) => {
            return dimensions.get(&instances::normalize_identifier(&name.spelling));
        }
        ExprKind::Member { .. } => {}
        _ => return None,
    }
    let (mut parts, None) = direct::path(id, native, tree, session, dimensions).ok()? else {
        return None;
    };
    // A member of a structure variable is named by one collapsed path.
    if parts.len() > 1 && !packed_structs::bind_interface_path(tree, &mut parts, dimensions) {
        return None;
    }
    match parts.as_slice() {
        [name] => dimensions.get(name),
        _ => None,
    }
}

/// Whether `id` is a chandle member selected through a virtual interface
/// (25.9), whose members are scalar.
#[inline(never)]
fn virtual_member(
    tree: &AnalysisContext<'_>,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
    session: &mut Session,
) -> bool {
    virtual_interfaces::native_member(tree, session, id, native, dimensions).is_some_and(
        |(_, ty, member)| {
            virtual_interfaces::member(session, &ty, &member)
                .is_ok_and(|(member, _)| member.r#type.chandle)
        },
    )
}

/// Whether a select of `kind` selects one element.
fn element_select(kind: &DimensionKind) -> bool {
    matches!(kind, DimensionKind::Index(_) | DimensionKind::Queue(None))
}

/// Whether built-in method `name` of container `receiver` returns a
/// chandle, `None` if `receiver` is no container. Of these methods, only the
/// pops return an element.
fn container_method(
    tree: &AnalysisContext<'_>,
    receiver: ExprId,
    name: &str,
    native: &Declarations,
    dimensions: &PackedDimensions,
    session: &mut Session,
) -> Option<bool> {
    let container = containers::native_type(receiver, native, tree, session, dimensions).ok()??;
    Some(
        matches!(name, "pop_front" | "pop_back")
            && container.element.chandle
            && container.element.unpacked_ranges.is_empty(),
    )
}

/// Whether native expression `id` is a hierarchical name not yet bound to a
/// signal of another scope: the binder reports it, and the analysis that
/// binds it checks its type (IEEE 1800-2023 23.6).
fn unbound_hierarchical(
    tree: &AnalysisContext<'_>,
    id: ExprId,
    native: &Declarations,
    dimensions: &PackedDimensions,
    session: &mut Session,
) -> bool {
    let id = ungroup(id, native);
    if !matches!(native.expression(id).kind, ExprKind::Member { .. }) {
        return false;
    }
    let Ok((mut parts, None)) = direct::path(id, native, tree, session, dimensions) else {
        return false;
    };
    parts.len() > 1
        && dimensions.get(&parts[0]).is_none()
        && !packed_structs::bind_interface_path(tree, &mut parts, dimensions)
}

fn null(id: ExprId, native: &Declarations) -> bool {
    matches!(&native.expression(ungroup(id, native)).kind, ExprKind::Literal(text) if text == "null")
}

/// Check that memory `memory`, which a file is read into or written from,
/// holds no chandle, also as a structure member: a file holds integral
/// words (6.14, 21.3.4.4, 21.4).
#[inline(never)]
pub(in crate::ast) fn check_memory(
    memory: ExprId,
    tree: &AnalysisContext<'_>,
    dimensions: &PackedDimensions,
    session: &mut Session,
) -> Converted<()> {
    let native = tree.declarations();
    let chandle = match containers::native_type(memory, native, tree, session, dimensions)? {
        Some(container) => type_holds_chandle(&container.element),
        // A variable, or a signal of another instance named by a path.
        None => variable(tree, ungroup(memory, native), native, dimensions, session)
            .is_some_and(holds_chandle),
    };
    if chandle {
        return Err(illegal("a memory of chandles"));
    }
    Ok(())
}

/// The error of a bit-select or part-select of a chandle.
pub(super) fn packed_select() -> AnalyzerError {
    illegal("a chandle has no bits to select")
}

fn illegal(detail: impl Into<String>) -> AnalyzerError {
    AnalyzerError::IllegalChandleUse {
        detail: detail.into(),
    }
}

/// The values one binding of an expression may read as chandles. A chandle
/// is not integral: IEEE 1800-2023 6.14 lets one be compared for equality
/// with another chandle or `null`, tested as a Boolean, and assigned to a
/// chandle (passed to a chandle formal, returned as a chandle result, stored
/// as a chandle element or property). The binder checks every value it reads
/// against this list, so any other context, an operand, an index, a count, a
/// cast, an argument of a system subroutine, rejects a chandle without a
/// check of its own.
pub(super) struct Permits {
    /// The native expressions that may be chandles.
    granted: Vec<ExprId>,
}

impl Permits {
    /// The permits of a binding of `root`, which may be a chandle if
    /// `chandle`.
    pub(super) fn new(root: ExprId, chandle: bool) -> Self {
        Self {
            granted: if chandle { vec![root] } else { Vec::new() },
        }
    }

    /// Let `id` be a chandle.
    pub(super) fn grant(&mut self, id: ExprId) {
        self.granted.push(id);
    }

    /// Let `id` be a chandle if `granted`.
    pub(super) fn grant_if(&mut self, granted: bool, id: ExprId) {
        if granted {
            self.grant(id);
        }
    }

    /// Check value `id`, which the binder reads for a destination of shape
    /// `target`, returning whether it may be a chandle.
    #[inline(never)]
    pub(super) fn check(
        &mut self,
        session: &mut Session,
        id: ExprId,
        target: Option<&VariableDimensions>,
        tree: &AnalysisContext<'_>,
        native: &Declarations,
        dimensions: &PackedDimensions,
    ) -> Converted<bool> {
        // A type query reads no value (20.6).
        let permitted = self.granted.contains(&id)
            || target.is_some_and(receives_chandle)
            || dimensions.type_query;
        if !permitted
            && matches!(
                native.expression(id).kind,
                ExprKind::Name(_)
                    | ExprKind::Select { .. }
                    | ExprKind::Member { .. }
                    | ExprKind::Call { .. }
            )
            && native_chandle(tree, id, native, dimensions, session)
        {
            return Err(illegal(format!(
                "chandle `{}` used as a value of another type",
                native.token_text(native.expression(id).tokens).trim()
            )));
        }
        Ok(permitted)
    }

    /// Grant the branches of a conditional that may be a chandle if
    /// `granted`. A conditional is a chandle only if both branches are
    /// chandles or `null` (11.4.11), whatever reads it.
    #[inline(never)]
    pub(super) fn conditional(
        &mut self,
        session: &mut Session,
        granted: bool,
        branches: [ExprId; 2],
        tree: &AnalysisContext<'_>,
        native: &Declarations,
        dimensions: &PackedDimensions,
    ) -> Converted<()> {
        if !granted {
            return Ok(());
        }
        if branches
            .iter()
            .any(|id| chandle_valued(*id, tree, native, dimensions, session))
            && !branches
                .iter()
                .all(|id| assignable(*id, tree, native, dimensions, session))
        {
            return Err(illegal(
                "a conditional chooses a chandle only with another chandle or null",
            ));
        }
        branches.into_iter().for_each(|id| self.grant(id));
        Ok(())
    }

    /// Grant the operands of binary operator `operator` that may be
    /// chandles: those of a Boolean operator, and those of an equality that
    /// compares a chandle with another chandle or `null`.
    #[inline(never)]
    pub(super) fn binary(
        &mut self,
        session: &mut Session,
        operator: &str,
        left: ExprId,
        right: ExprId,
        tree: &AnalysisContext<'_>,
        native: &Declarations,
        dimensions: &PackedDimensions,
    ) -> Converted<()> {
        if operator == "&&"
            || operator == "||"
            || check_equality(operator, left, right, tree, native, dimensions, session)?
        {
            self.grant(left);
            self.grant(right);
        }
        Ok(())
    }
}

/// Whether `operator` compares `left` and `right` as chandles: an equality
/// of a chandle with another chandle or `null` (6.14), as a `case`
/// statement compares its expression with its items (12.5). An equality of
/// a chandle with another value is an error; other operators are left to
/// the binder, which rejects chandle operands.
#[inline(never)]
pub(in crate::ast) fn check_equality(
    operator: &str,
    left: ExprId,
    right: ExprId,
    tree: &AnalysisContext<'_>,
    native: &Declarations,
    dimensions: &PackedDimensions,
    session: &mut Session,
) -> Converted<bool> {
    if !matches!(operator, "==" | "!=" | "===" | "!==") {
        return Ok(false);
    }
    let chandles = [left, right].map(|id| native_chandle(tree, id, native, dimensions, session));
    if !chandles.contains(&true) {
        return Ok(false);
    }
    let mut comparable = |chandle: bool, id: ExprId| {
        chandle || null(id, native) || unbound_hierarchical(tree, id, native, dimensions, session)
    };
    if !(comparable(chandles[0], left) && comparable(chandles[1], right)) {
        return Err(illegal(format!(
            "`{operator}` compares a chandle only with another chandle or null"
        )));
    }
    Ok(true)
}

/// Check write target `root`, which may be a chandle if `chandle`. Like
/// [`Permits`] for values, this is the one check of every target: a chandle
/// is written only by a procedural `=` assignment (6.14), not as a part of
/// a concatenation or stream, by a continuous assignment or an operator.
#[inline(never)]
pub(in crate::ast) fn check_target(
    chandle: bool,
    root: ExprId,
    tree: &AnalysisContext<'_>,
    native: &Declarations,
    dimensions: &PackedDimensions,
    session: &mut Session,
) -> Converted<()> {
    if !chandle && native_chandle(tree, root, native, dimensions, session) {
        return Err(illegal(format!(
            "chandle `{}` written as a variable of another type",
            native.token_text(native.expression(root).tokens).trim()
        )));
    }
    Ok(())
}

/// The check of [`Permits`] for a value `id` that a binder reads without
/// [`direct::bind_value`], such as the name of a delay.
pub(in crate::ast) fn read(
    id: ExprId,
    tree: &AnalysisContext<'_>,
    native: &Declarations,
    dimensions: &PackedDimensions,
    session: &mut Session,
) -> Converted<()> {
    Permits::new(id, false)
        .check(session, id, None, tree, native, dimensions)
        .map(drop)
}

/// Whether data of `shape` holds a chandle, also as a structure member: no
/// integral or bit-stream value (IEEE 1800-2023 6.14, 6.24.3).
pub(in crate::ast) fn holds_chandle(shape: &VariableDimensions) -> bool {
    shape.chandle
        || shape
            .unpacked_struct
            .as_ref()
            .is_some_and(|aggregate| aggregate.leaves().iter().any(|leaf| leaf.r#type.chandle))
}

/// [`holds_chandle`] for data of type `ty`, such as a container element.
pub(in crate::ast) fn type_holds_chandle(ty: &Type) -> bool {
    ty.chandle || holds_chandle(&procedural::dimensions_from_type(ty))
}

/// Whether a destination of shape `shape` receives a chandle.
pub(super) fn receives_chandle(shape: &VariableDimensions) -> bool {
    shape.chandle && shape.unpacked.is_empty() && shape.container.is_none()
}

/// Check that value native expression `value` may be assigned to a target
/// that is a chandle if `target`: only a chandle or `null` (6.14). That a
/// chandle is assigned to no other target the binder checks (see
/// [`Permits`]).
pub(in crate::ast) fn check_assignment(
    target: bool,
    value: ExprId,
    tree: &AnalysisContext<'_>,
    native: &Declarations,
    dimensions: &PackedDimensions,
    session: &mut Session,
) -> Converted<()> {
    if target && !assignable(value, tree, native, dimensions, session) {
        return Err(illegal(
            "only a chandle or null can be assigned to a chandle",
        ));
    }
    Ok(())
}

/// Whether native expression `id` is a chandle, also as a branch of a
/// conditional.
fn chandle_valued(
    id: ExprId,
    tree: &AnalysisContext<'_>,
    native: &Declarations,
    dimensions: &PackedDimensions,
    session: &mut Session,
) -> bool {
    let id = ungroup(id, native);
    match &native.expression(id).kind {
        ExprKind::Conditional {
            then_expression,
            else_expression,
            ..
        } => [*then_expression, *else_expression]
            .iter()
            .any(|branch| chandle_valued(*branch, tree, native, dimensions, session)),
        _ => native_chandle(tree, id, native, dimensions, session),
    }
}

/// Whether native expression `id` may be assigned to a chandle: a chandle,
/// `null`, a call without a known result (such as a container method), or a
/// conditional choosing between such values.
fn assignable(
    id: ExprId,
    tree: &AnalysisContext<'_>,
    native: &Declarations,
    dimensions: &PackedDimensions,
    session: &mut Session,
) -> bool {
    let id = ungroup(id, native);
    match &native.expression(id).kind {
        ExprKind::Call { .. } => call_result(tree, id, native, dimensions, session) == Some(true),
        ExprKind::Conditional {
            then_expression,
            else_expression,
            ..
        } => {
            assignable(*then_expression, tree, native, dimensions, session)
                && assignable(*else_expression, tree, native, dimensions, session)
        }
        _ => {
            null(id, native)
                || native_chandle(tree, id, native, dimensions, session)
                || unbound_hierarchical(tree, id, native, dimensions, session)
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{AnalyzerError, analyze_source};
    use std::path::Path;

    fn analyze(body: &str) -> Result<crate::ir::Ir, AnalyzerError> {
        analyze_source(
            &format!("module Top; {body} endmodule"),
            Path::new("chandle.sv"),
        )
    }

    #[test]
    fn chandles_are_compared_tested_assigned_and_passed() {
        analyze(
            "chandle a, b; chandle lookup[string];
             class Holder; local chandle h; function bit set(chandle x); h = x; return h != null; endfunction endclass
             function automatic chandle pass(chandle h); return h; endfunction
             export \"DPI-C\" function pass;
             initial begin
                 automatic chandle c = null;
                 automatic Holder holder = new;
                 b = a; c = pass(b); lookup[\"k\"] = c;
                 if (a == b && c === null && !c && (a || b) && a != lookup[\"k\"]) a = null;
                 void'(holder.set(c));
                 if (pass(a) == a && pass(b) != null) b = pass(a);
             end
             import \"DPI-C\" function chandle make();
             import \"DPI-C\" function int read(input chandle h);
             initial if (make() != null && read(make()) == 0) a = make();
             initial void'(make());
             function automatic chandle defaulted(input chandle x = a); return x; endfunction
             initial a = b != null ? pass(b) : null;
             class Registry; static chandle h; endclass
             initial if (Registry::h == a) a = Registry::h;
             bit choose;
             initial if ((choose ? a : b) == a && (choose ? null : a) != null) b = null;
             initial if ((choose ? a : null) && !(choose ? null : b)) b = null;
             function automatic chandle choose_handle(chandle h); return choose ? h : null; endfunction
             function automatic chandle or_null(chandle h = null); return h; endfunction
             chandle q[$];
             class Keeper; chandle h; function new(input chandle a[$]); h = a[0]; endfunction endclass
             initial begin automatic Keeper k = new(q); end
             class KeeperChild extends Keeper; function new(input chandle a[$]); super.new(a); endfunction endclass
             bit clk;
             initial @(posedge clk iff a) b = null;
             initial @(posedge clk iff !a && b != null) b = null;
             initial begin q.push_back(a); q.push_back(or_null()); a = q[0]; end
             initial begin automatic chandle p[$] = '{a, null}; end
             initial begin q.push_back(a); if (q.pop_front() == a) a = q.pop_back(); end
             class Base; chandle h; function new(chandle x = null); h = x; endfunction endclass
             class Derived extends Base; chandle k = null; function new(chandle y); super.new(y); k = y; endfunction endclass
             initial begin automatic Derived d = new(a); automatic Base e = new(null); end
             initial case (a) null, b: a = null; q[0]: b = null; endcase
             int keyed[chandle];
             initial keyed[a] = 1;
             typedef chandle H;
             H aliased = null;
             typedef struct { chandle h; int x; } Holds;
             Holds holds;
             initial begin holds.h = a; a = holds.h; if (holds.h == a) holds = '{h: null, x: 1}; end
             Holds held[$];
             initial begin held.push_back(holds); a = held[0].h; if (held[0].h != a) a = null; end
             initial begin a = q.pop_front; if (q.pop_back != a) a = null; end
             initial begin q = keyed.find_index() with (item.index == a); q = keyed.find_first_index() with (item.index() != null); end
             initial begin automatic chandle k; if (keyed.first(k)) void'(keyed.next(k)); end
             initial begin q = q.find() with (item != null); q = q.unique(); q.reverse(); end
             initial for (chandle i = null, j = i; i != j; i = null) a = j;
             initial foreach (keyed[k]) begin keyed[k] = 2; a = k; end
             initial begin q = q.min() with (item != null); q = q.unique() with (item); end
             initial begin q.push_back(null); a = q[$]; if (q[$] == a) a = null; end
             function automatic chandle take_ref(ref chandle x[$]); return x[0]; endfunction
             initial a = take_ref(q);
             int sizes; initial sizes = $size(q, 1);
             class Source; function chandle get(); return null; endfunction endclass
             initial begin automatic Source s = new; s.get(); void'(s.get()); end
             initial case (a) default: a = null; endcase",
        )
        .unwrap();
    }

    /// No context needs a check of its own to reject a chandle: the binder
    /// lets one be only where IEEE 1800-2023 6.14 allows it.
    #[test]
    fn unlisted_contexts_reject_chandles() {
        for body in [
            "chandle h; int n; initial n = $clog2(h);",
            "chandle h; int n; initial n = $countones(h);",
            "chandle h; longint b; initial b = $signed(h);",
            "chandle h; int r; initial r = $urandom(h);",
            "chandle h; string s; initial s = $sformatf(\"%d\", h);",
            "chandle h; initial $display(h);",
            "chandle h; longint unsigned b; initial b = h << 1;",
            "chandle h; bit b; initial b = h < h;",
            "chandle h; bit b; initial b = h ^ h;",
            "chandle h; bit b; initial b = -h == 0;",
            "chandle h; bit b; initial wait (h) b = 1;",
            "chandle h; int q[$]; initial q.push_back(h);",
            "chandle h; bit [63:0] w; initial w = h + 0;",
            "chandle h; real r; initial r = h;",
            "chandle h; string s; initial s = h;",
            "chandle h; initial h += 64'd1;",
            "chandle h, g; initial {h, g} = 128'd1;",
            "int a[2]; chandle h; int n; initial n = $left(a, h);",
        ] {
            let result = analyze(body);
            assert!(
                matches!(result, Err(AnalyzerError::IllegalChandleUse { .. })),
                "{body}: {result:?}"
            );
        }
        // The Boolean tests and comparisons 6.14 lists.
        analyze(
            "chandle h, g; bit b; int n;
             initial begin
                 if (!h && h == null || g != h) n = 1;
                 while (h) h = null;
                 do h = null; while (h);
                 for (; h; ) h = null;
                 assert (h || !g);
                 b = h ? 1'b1 : 1'b0;
                 b = h === g;
                 b = (h != null) ? h == g : h !== null;
             end",
        )
        .unwrap();
    }

    #[test]
    fn chandle_results_have_no_dimensions() {
        analyze(
            "function automatic chandle pass(chandle h); return h; endfunction
             import \"DPI-C\" function chandle make();
             if ($dimensions(pass(null)) != 0 || $dimensions(make()) != 0) begin : wrong
                 chandle w, v; assign w = v;
             end",
        )
        .unwrap();
        // Container queries count no packed dimension for chandle elements.
        let ir = analyze(
            "chandle q[$]; chandle m[int][$]; chandle a, b; bit choose; int n, k, c;
             initial begin n = $dimensions(q); k = $dimensions(m); c = $dimensions(choose ? a : b); end",
        )
        .unwrap();
        let body = format!("{:?}", ir.modules()[0].initial_processes());
        assert!(
            body.contains(r#"lhs: Ident("n"), rhs: Literal("1")"#)
                && body.contains(r#"lhs: Ident("k"), rhs: Literal("2")"#)
                && body.contains(r#"lhs: Ident("c"), rhs: Literal("0")"#),
            "{body}"
        );
        assert!(
            analyze("if ($dimensions(64'd0) != 0) begin : wrong chandle w, v; assign w = v; end")
                .is_err()
        );
        // A chandle result has 64 bits of storage but no dimension to size.
        let ir = analyze(
            "function automatic chandle pass(chandle h); return h; endfunction
             import \"DPI-C\" function chandle make();
             int b, d, s, t;
             initial begin b = $bits(pass(null)); d = $bits(make()); s = $size(make()); t = $size(pass(null)); end",
        )
        .unwrap();
        // Also where a declaration or generate condition is discovered.
        analyze(
            "function automatic chandle pass(chandle h); return h; endfunction
             import \"DPI-C\" function chandle make();
             if ($size(make()) === 64 || $size(pass(null)) === 64) begin : wrong
                 chandle w, v; assign w = v;
             end",
        )
        .unwrap();
        let body = format!("{:?}", ir.modules()[0].initial_processes());
        assert!(
            body.contains(r#"lhs: Ident("b"), rhs: Literal("64")"#)
                && body.contains(r#"lhs: Ident("d"), rhs: Literal("64")"#)
                && body.contains(r#"lhs: Ident("s"), rhs: Literal("32'bx")"#)
                && body.contains(r#"lhs: Ident("t"), rhs: Literal("32'bx")"#),
            "{body}"
        );
    }

    /// Nested conditionals are classified in time linear in their depth.
    #[test]
    fn nested_conditionals_of_chandles_are_classified_once() {
        let nested = (0..48).fold("h".to_owned(), |inner, _| format!("(c ? {inner} : null)"));
        analyze(&format!(
            "chandle h; bit c, r; initial begin r = {nested} == null; if ({nested}) r = 1; end"
        ))
        .unwrap();
    }

    /// UVM keeps chandles in scalars and dynamic containers only; a
    /// fixed-size unpacked array of them is rejected where its type is
    /// bound, whatever declares it.
    #[test]
    fn fixed_size_arrays_of_chandles_are_unsupported() {
        for body in [
            "chandle a[2];",
            "chandle a[0:1];",
            "chandle a[2][3];",
            "initial begin automatic chandle a[2]; end",
            "typedef chandle H[2];",
            "typedef chandle H; H a[2];",
            "typedef chandle H[2]; H q[$];",
            "chandle q[$][2];",
            "chandle m[int][2];",
            "class C; chandle h[2]; endclass",
            "function automatic void f(input chandle a[2]); endfunction",
        ] {
            let result = analyze(body);
            assert!(
                matches!(&result, Err(AnalyzerError::Unsupported(reason)) if reason.contains("fixed-size unpacked array of chandle")),
                "{body}: {result:?}"
            );
        }
        analyze("chandle h; chandle q[$]; chandle d[]; chandle m[string]; chandle n[int][$];")
            .unwrap();
    }

    #[test]
    fn chandles_are_named_chandle() {
        let ir = analyze(
            "chandle h; chandle q[$]; string s, t;
             initial begin s = $typename(h); t = $typename(q); end",
        )
        .unwrap();
        let body = format!("{:?}", ir.modules()[0].initial_processes());
        // "chandle" and "chandle$[$]" (IEEE 1800-2023 20.6.1).
        assert!(
            body.contains(r#"Literal("56'h6368616e646c65")"#)
                && body.contains(r#"Literal("88'h6368616e646c65245b245d")"#),
            "{body}"
        );
    }

    #[test]
    fn other_uses_of_chandles_are_rejected() {
        for body in [
            "chandle a; int i; initial i = a;",
            "chandle a; initial a = 1;",
            "chandle q[$]; int i; initial i = q.pop_front;",
            "int m[int]; chandle h; initial void'(m.first(h));",
            "int m[chandle]; longint unsigned k; initial void'(m.last(k));",
            "chandle q[$]; longint unsigned s; initial s = q.sum();",
            "chandle q[$], r[$]; initial r = q.min();",
            "chandle q[$], r[$]; initial r = q.min() with (item);",
            "chandle q[$]; initial q.sort();",
            "initial for (chandle h = 64'd1; h != null; h = null) begin end",
            "chandle q[$]; int i; initial i = q[$];",
            "typedef struct { chandle h; } S; S q[$]; longint unsigned b; initial b = q[0].h[63:0];",
            "chandle h; longint unsigned b; initial b = {>>{h}};",
            "chandle h; longint unsigned b; initial {>>{h}} = b;",
            "class C; chandle h; endclass C c; longint unsigned b; initial b = {>>{c.h}};",
            "typedef struct { chandle h; } S; S s; bit b; initial b = s.h[0];",
            "class C; chandle h; endclass C c; bit b; initial b = c.h[3];",
            "chandle q[]; initial $writememh(\"handles.hex\", q);",
            "chandle h; bit done; initial #(h) done = 1;",
            "chandle h; bit done; initial #h done = 1;",
            "chandle a; int i; initial a = i;",
            "chandle a; initial a = a + 1;",
            "chandle a; initial if (a == 0) a = null;",
            "chandle a; int i; initial i = int'(a);",
            "chandle a; initial a += 1;",
            "chandle a; initial @(a) a = null;",
            "chandle a, b; assign a = b;",
            "chandle a; function automatic void f(int x); endfunction initial f(a);",
            "chandle a; int i; function automatic void f(chandle x); endfunction initial f(i);",
            "import \"DPI-C\" function int read(input chandle h); int r; initial r = read(1);",
            "chandle a; function automatic chandle f(); return null; endfunction initial if (f() + 1) a = null;",
            "chandle a; function automatic int number(); return 1; endfunction initial a = number();",
            "import \"DPI-C\" function int read(input chandle h); function automatic int number(); return 1; endfunction int r; initial r = read(number());",
            "chandle a; initial a = a != null ? a : 1;",
            "chandle a = 64'd1;",
            "class C; static chandle h; endclass int i; initial i = C::h;",
            "chandle a, b; bit c; int i; initial i = c ? a : b;",
            "chandle h; bit c; int n; initial if (c ? h : 64'd1) n = 1;",
            "chandle h, g; bit c, d; int n; initial if (c ? (d ? h : g) : 5) n = 1;",
            "chandle h; bit c; int n; initial n = !(c ? 1 : h);",
            "chandle a; initial a++;",
            "chandle a; initial --a;",
            "chandle a; int i; initial i = a++;",
            "chandle a; int i; initial i = 1 + --a;",
            "chandle a; function automatic chandle forge(); return 1; endfunction initial a = forge();",
            "chandle a; function automatic int leak(); return a; endfunction",
            "chandle a; initial a[63:0] = 64'd1;",
            "chandle a; int i; initial i = a[3];",
            "initial begin automatic chandle q[$] = '{64'd1}; end",
            "chandle q[$]; initial q.push_back(64'd1);",
            "chandle a; function automatic chandle forge(input chandle h = 64'd1); return h; endfunction initial a = forge();",
            "chandle a; function automatic int f(input int x = a); return x; endfunction",
            "chandle a; int q[$]; initial a = q.pop_front();",
            "chandle a; chandle q[$]; int n; initial a = q.size();",
            "chandle a; initial a = $urandom();",
            "chandle a; longint unsigned bits; initial bits = {a};",
            "chandle a; longint unsigned bits; initial bits = {1{a}};",
            "chandle a; initial {a} = 64'd1;",
            "class C; chandle h; function new(chandle x); h = x; endfunction endclass initial begin automatic C c = new(64'd1); end",
            "class B; function new(chandle x); endfunction endclass class D extends B; function new(); super.new(64'd1); endfunction endclass",
            "class B; function new(chandle x); endfunction endclass class D extends B(64'd1); endclass",
            "class C; chandle h = 64'd1; endclass",
            "chandle a; bit c; longint unsigned bits; initial bits = c ? a : 64'd0;",
            "chandle a; bit c; initial a = c ? 64'd0 : a;",
            "typedef chandle H; H [1:0] pair;",
            "typedef chandle H; H [1:0] q[$];",
            "typedef chandle H; function automatic void f(H [1:0] x); endfunction",
            "typedef chandle H; class C; H [1:0] p; endclass",
            "chandle a; int i; initial case (i) a: i = 1; endcase",
            "chandle a; initial case (a) 0: a = null; endcase",
            "chandle a; initial casez (a) null: a = null; endcase",
            "chandle a; initial case (a) inside null: a = null; endcase",
            "typedef struct { chandle h; int x; } S; S s; initial s.h = 64'd1;",
            "typedef struct { chandle h; } S; S s; int i; initial i = s.h;",
            "typedef struct { chandle h; } S; S s; initial s.h++;",
            "class C; chandle h; endclass C c; initial begin c = new; c.h += null; end",
            "class C; chandle h; endclass C c; initial begin c = new; c.h++; end",
            "chandle q[$]; initial q[0] += null;",
            "chandle a[string]; initial a[\"k\"] |= null;",
            "typedef struct { chandle h; } S; S s; longint unsigned b; initial b = {s.h};",
        ] {
            let result = analyze(body);
            assert!(
                matches!(
                    &result,
                    Err(AnalyzerError::IllegalChandleUse { .. }
                        | AnalyzerError::IncompatibleUnpackedArray { .. })
                ),
                "{body}: {result:?}"
            );
        }
        for body in [
            "class C; rand chandle h; endclass initial begin automatic C c = new; void'(c.randomize()); end",
            "chandle h; initial void'(std::randomize(h));",
            "typedef chandle H; localparam H P = 64'd1;",
            "typedef chandle H; localparam H Q = null;",
            "class C; chandle h; int x; endclass initial begin automatic C c = new; void'(c.randomize(h)); end",
            "chandle q[$]; int fd, n; initial n = $fread(q, fd);",
            "class C; chandle h; function new(input chandle a[$]); h = a[0]; endfunction endclass bit [63:0] words[$]; initial begin automatic C c = new(words); end",
            "chandle h; function automatic void f(input chandle a[$]); h = a[0]; endfunction bit [63:0] words[$]; initial f(words);",
            "class B; chandle h; function new(input chandle a[$]); h = a[0]; endfunction endclass class D extends B; function new(input bit [63:0] words[$]); super.new(words); endfunction endclass",
            "typedef struct { chandle h; } S; S s; initial void'(std::randomize(s));",
            "typedef chandle H; typedef struct { H [1:0] m; } S; S s;",
            "typedef struct { chandle h; } S; S q[$]; int i; initial i = q[0].h;",
            "chandle h; bit r; initial r = 64'd0 inside {h};",
            "chandle h; bit r; initial r = 64'd0 inside {[h:h]};",
            "chandle h; longint unsigned bits; initial bits = $unsigned(h);",
            "chandle h; initial void'($value$plusargs(\"ptr=%h\", h));",
            "chandle h; int a[2], x; initial x = a[h];",
            "chandle h; int q[$], x; initial x = q[h];",
            "chandle h; int m[int], x; initial x = m[h];",
            "chandle h; int m[chandle], x; initial x = m[0];",
            "chandle h; bit b; always @(posedge h) b = 1'b1;",
            "chandle h; bit b; always_ff @(posedge h) b <= 1'b1;",
            "mailbox #(chandle) m;",
            "bit [63:0] words[$]; chandle h; function automatic chandle take(ref chandle x[$]); return x[0]; endfunction initial h = take(words);",
            "chandle q[$]; function automatic void take(ref bit [63:0] x[$]); endfunction initial take(q);",
        ] {
            assert!(analyze(body).is_err(), "{body}");
        }
        // The constraint solver supports no chandle operand.
        for body in [
            "class C; chandle h; rand bit [63:0] bits; constraint leak { bits == h; } endclass initial begin automatic C c = new; void'(c.randomize()); end",
            "class C; chandle h; rand bit [63:0] bits; constraint leak { h != null -> bits == 1; } endclass initial begin automatic C c = new; void'(c.randomize()); end",
        ] {
            let result = analyze(body);
            assert!(
                matches!(&result, Err(AnalyzerError::Unsupported(reason)) if reason.contains("a chandle in a constraint")),
                "{body}: {result:?}"
            );
        }
        for body in [
            "chandle h; bit done; initial @h done = 1;",
            "chandle h; int a[]; initial a = new[h];",
            "chandle h; int a[]; initial a = new[h](a);",
            "chandle q[$]; bit [63:0] b[$]; initial b = {>>{q}};",
            "chandle d[]; bit [63:0] b; initial b = {<<64{d}};",
            "chandle m[int]; bit [63:0] b[$]; initial b = {>>{m}};",
            "chandle q[$]; bit [63:0] b[$]; initial q = {>>{b}};",
            "chandle q[$]; bit [63:0] b[$]; initial {>>{q}} = b;",
            "typedef struct { chandle h; } S; S q[$]; initial q = {>>{64'd1}};",
            "typedef struct { chandle h; } S; S q[$]; longint unsigned b; initial b = q.sum();",
            "typedef struct { chandle h; } S; S d[]; longint unsigned b; initial b = d.xor();",
            "typedef struct { chandle h; } S; S q[$]; bit [63:0] b[$]; initial b = {>>{q}};",
            "chandle h; string src = \"x\", dst; initial dst = {h{src}};",
            "chandle h; string s = \"abc\"; int c; initial c = s[h];",
            "chandle h; string s = \"abc\"; initial s[h] = \"z\";",
            "chandle h; string s = \"abc\"; int c; initial c = s.getc(h);",
            "chandle h; string s = \"abc\"; initial s.putc(h, \"z\");",
            "chandle h; string s = \"abc\", t; initial t = s.substr(h, h);",
            "class C; rand bit [63:0] bits; endclass chandle h; initial begin automatic C c = new; void'(c.randomize() with { bits == h; }); end",
            "typedef chandle H; typedef enum H { BAD = 64'd1 } E;",
            "chandle h; semaphore s; initial s = new(h);",
            "chandle h; semaphore s = new; initial s.put(h);",
            "chandle h; semaphore s = new; initial s.get(h);",
            "chandle h; semaphore s = new; bit ok; initial ok = s.try_get(h);",
            "chandle h; int n; initial repeat (h) n++;",
            "string s = \"abc\"; chandle h; initial h = s.len();",
            "string s = \"abc\"; chandle h; initial h = s.substr(0, 1);",
            "typedef enum { A, B } E; E e; chandle h; initial h = e.first();",
            "semaphore s = new; chandle h; initial h = s.try_get();",
            "class C; rand int x; endclass chandle h; initial begin automatic C c = new; h = c.randomize(); end",
            "class C; endclass C c; chandle h; bit ok; initial ok = $cast(c, h);",
            "class C; endclass C c; chandle h; initial $cast(c, h);",
            "chandle h; mailbox #(int) m; initial m = new(h);",
            "chandle h; mailbox #(bit [63:0]) m = new; initial m.put(h);",
            "chandle h; mailbox #(bit [63:0]) m = new; initial m.get(h);",
            "chandle h; mailbox #(bit [63:0]) m = new; bit ok; initial ok = m.try_peek(h);",
            "mailbox #(int) m = new; chandle h; initial h = m.num();",
            "typedef enum { A, B, C } E; E e; chandle h; initial e = e.next(h);",
            "typedef enum { A, B, C } E; E e; chandle h; initial e = e.prev(h);",
            "int a[2]; chandle h; initial h = a.sum();",
            "bit [63:0] a[2]; chandle h; initial h = a.xor() with (item);",
            "chandle h; initial $fclose(h);",
            "chandle h; initial $fflush(h);",
            "chandle h; initial $fdisplay(h, \"x\");",
            "chandle h; initial $fwrite(h, \"x\");",
            "chandle h; initial $finish(h);",
            "typedef struct { chandle h; } S; S s; initial s = '{h: '{default: 1}};",
            "typedef struct { chandle h; } S; S s; initial s = '{h: '{64'd1}};",
            "chandle h; wire [63:0] bits = h;",
            "typedef struct { chandle h; } S; S s; int fd, n; initial n = $fread(s, fd);",
            "typedef struct { chandle h; } Inner; typedef struct { Inner inner; } Outer; Outer s; int fd, n; initial n = $fread(s.inner, fd);",
            "typedef struct { int x; chandle h; } S; S q[$]; int fd, n; initial n = $fread(q, fd);",
            "typedef struct { chandle h; } S; S s; initial $writememh(\"s.hex\", s);",
        ] {
            let result = analyze(body);
            assert!(
                matches!(result, Err(AnalyzerError::IllegalChandleUse { .. })),
                "{body}: {result:?}"
            );
        }
        // Ports shall not have the chandle data type, also as a structure
        // member (6.14).
        for source in [
            "module Top(input chandle h); endmodule",
            "typedef struct { chandle h; int x; } S; module Top(input S s); endmodule",
            "typedef struct { int x; chandle h; } S; module Top(output S s); endmodule",
            "typedef struct { chandle h; } S; module Top(s); input S s; endmodule",
        ] {
            let result = analyze_source(source, Path::new("chandle_port.sv"));
            assert!(
                matches!(&result, Err(error) if error.to_string().contains("chandle")),
                "{source}: {result:?}"
            );
        }
    }
}
