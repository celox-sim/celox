//! Conditional unsigned-rank progress, shared by v2 and relational responses.
use lydite_ir::*;
use lydite_solver::Check;
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(crate) fn decreases(next: Term, current: Term) -> Term {
    node(Sort::Bool, "bvult", vec![next, current])
}
fn or(a: Term, b: Term) -> Term {
    node(Sort::Bool, "or", vec![a, b])
}
fn implies(a: Term, b: Term) -> Term {
    or(not(a), b)
}

pub(crate) fn responses(
    implementation: &SpecImplementation,
    reset: Term,
    invariant: Term,
    context: &Env,
    inputs: &Env,
    q: &mut Check,
) -> Res<Vec<Value>> {
    let machine = &implementation.machine;
    let next_sub = machine
        .state
        .iter()
        .map(|(k, v)| (v.clone(), machine.next[k].clone()))
        .collect::<BTreeMap<_, _>>();
    let reset_sub = machine
        .state
        .iter()
        .map(|(k, v)| (v.clone(), machine.reset[k].clone()))
        .collect::<BTreeMap<_, _>>();
    let mut results = vec![];
    for (index, (name, c)) in implementation.responses.iter().enumerate() {
        let before = q.reports.len();
        let complete = implementation.operations[&c.operation].clone();
        let pending_next = substitute(&c.pending, &next_sub);
        let rank_next = substitute(&c.rank, &next_sub);
        let Sort::Bv(width) = c.rank.0.sort else {
            return Err("invalid validated response rank".into());
        };
        let bounded = implies(
            c.pending.clone(),
            decreases(c.rank.clone(), bv(width, c.bound)),
        );
        let base = and(
            not(reset.clone()),
            and(invariant.clone(), c.assumption.clone()),
        );
        let active = and(base.clone(), bounded);
        let mut ctx = context.clone();
        for (key, value) in [
            ("accept", c.accept.clone()),
            ("pending", c.pending.clone()),
            ("complete", complete.clone()),
            ("rank", c.rank.clone()),
            ("rank_next", rank_next.clone()),
            ("pending_next", pending_next.clone()),
            ("assume", c.assumption.clone()),
        ] {
            ctx.insert(format!("response.{name}.{key}"), value);
        }
        let mut query = |suffix: &str, formula: Term, sat: bool, field: &str| -> Res<()> {
            q.query(&format!("response_{index}_{suffix}"), formula, sat, &ctx)?;
            let report = q.reports.last_mut().unwrap();
            report["response"] = json!(name);
            report["source_path"] = json!(format!("/implementation/responses/{name}/{field}"));
            Ok(())
        };
        // Input-only assumptions cannot exclude inconvenient implementation states.
        query(
            "environment_nonempty",
            and(not(reset.clone()), c.assumption.clone()),
            true,
            "assume",
        )?;
        query(
            "accept_nonempty",
            and(active.clone(), c.accept.clone()),
            true,
            "accept",
        )?;
        query(
            "reset_cancels",
            and(reset.clone(), substitute(&c.pending, &reset_sub)),
            false,
            "pending",
        )?;
        query(
            "no_overlapping_acceptance",
            and(base, and(c.pending.clone(), c.accept.clone())),
            false,
            "accept",
        )?;
        query(
            "completion_has_request",
            and(
                active.clone(),
                and(
                    complete.clone(),
                    not(or(c.pending.clone(), c.accept.clone())),
                ),
            ),
            false,
            "operation",
        )?;
        let expected_pending = and(
            or(c.pending.clone(), c.accept.clone()),
            not(complete.clone()),
        );
        query(
            "pending_preserved",
            and(
                active.clone(),
                not(eq(pending_next.clone(), expected_pending)),
            ),
            false,
            "pending",
        )?;
        let next_bounded = implies(
            pending_next,
            decreases(rank_next.clone(), bv(width, c.bound)),
        );
        query(
            "countdown_bound",
            and(active.clone(), not(next_bounded)),
            false,
            "bound",
        )?;
        query(
            "countdown_decreases",
            and(
                active,
                and(
                    c.pending.clone(),
                    and(not(complete), not(decreases(rank_next, c.rank.clone()))),
                ),
            ),
            false,
            "rank",
        )?;
        let reports = &q.reports[before..];
        let status = if reports
            .iter()
            .any(|r| r["status"] == "counterexample" || r["status"] == "failed_nonvacuity")
        {
            "failed"
        } else if reports.iter().any(|r| r["status"] == "unknown") {
            "unknown"
        } else {
            "verified"
        };
        let mut cover = crate::acceptance_cover::check(implementation, inputs, c);
        let cover_field = if c.cover_depth.is_some() {
            "cover_depth"
        } else {
            "accept"
        };
        cover["source_path"] = json!(format!("/implementation/responses/{name}/{cover_field}"));
        let reachability = cover["status"].as_str().unwrap_or("unchecked");
        results.push(json!({"name":name,"operation":c.operation,"status":status,"bound":c.bound,
            "adequacy":{"reset_reachable_acceptance":reachability,"reset_acceptance_cover":cover,"external_request_to_acceptance":"not_specified","source_path":format!("/implementation/responses/{name}/accept"),"message":"Conditional completion only: no external request-to-acceptance obligation is specified. See the separate bounded reset-acceptance cover; a verified theorem alone does not establish service after reset."},
            "claim":"Every accepted request completes on its acceptance edge or within bound subsequent nonreset steps, provided the input-only assumption holds on every step; reset cancels outstanding work",
            "semantics":{"outstanding":"single per contract; overlapping acceptance and unsolicited completion are checked errors","completion":"the named operation selector on the current edge","latency":"nonreset implementation edges, not abstract operations or enabled-only ticks","proof":"inductive pending/countdown invariant plus strict unsigned rank decrease; not finite trace enumeration","feasibility":"environment and acceptance SAT witnesses are not reset-reachability proofs"},
            "limitations":["No eventual acceptance, unbounded fairness, or guarantee after an assumption violation","No payload correspondence beyond the separate relational safety binding","Contracts do not imply progress for undeclared operations"]}));
    }
    Ok(results)
}
