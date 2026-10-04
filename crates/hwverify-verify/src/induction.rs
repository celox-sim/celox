//! Checked strengthening for scalar, source-bound safety. Proposals are state
//! predicates, never assumptions or proof receipts. The only temporal rule here
//! is reset induction; its initialization, preservation and target uses each go
//! through the existing live sequent checker with unchanged finite budgets.
use hwverify_ir::*;
use hwverify_solver::{Check, CutBudgetMode, ProofBundle};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::PathBuf};

struct Proposed {
    name: String,
    predicate: Term,
    dependencies: Vec<usize>,
}
// Private, invocation-local authority, constructed only after BOTH live proofs
// finish. Neither JSON reports nor caller-supplied success flags mint this value.
struct Established {
    predicate: Term,
}
fn conjunction(terms: impl IntoIterator<Item = Term>) -> Term {
    terms.into_iter().fold(boolv(true), and)
}
fn sequent(q: &mut Check, name: &str, pre: Term, post: Term, context: &Env) -> Res<bool> {
    let original = and(pre.clone(), not(post.clone()));
    let mut bundle = ProofBundle::new(
        q,
        name,
        &original,
        context,
        CutBudgetMode::IndependentLemmas,
    )?;
    let checked = bundle.prove(name, pre, post);
    match checked {
        Ok(handle) => {
            bundle.finish(&handle)?;
            Ok(true)
        }
        Err(_) => Ok(false), // Drop records counterexample/Unknown; no authority.
    }
}

/// Acyclic strengthening: each predicate is initialized independently and is
/// preserved under itself plus explicitly named EARLIER established predicates.
/// No target invariant, relation, environment premise, or candidate guard is
/// assumed to establish a strengthening. All inputs remain arbitrary.
pub fn check_inductive_safety(spec: &Specification, proposals: &Value, out: PathBuf) -> Res<Value> {
    crate::reachable::supported(spec)?;
    let items = proposals
        .as_array()
        .filter(|a| a.len() <= 32)
        .ok_or("expected at most 32 induction candidates")?;
    let imp = spec.implementation().unwrap();
    let machine = &imp.machine;
    let state_env = machine
        .state
        .iter()
        .map(|(n, t)| (format!("s.{n}"), t.clone()))
        .collect::<Env>();
    let mut parsed: Vec<Proposed> = vec![];
    let mut names = BTreeMap::new();
    for item in items {
        keys(item, &["name", "predicate", "depends_on"], &[])?;
        let name = text(&item["name"])?;
        if name.is_empty()
            || name.len() > 80
            || !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
            || names.contains_key(name)
        {
            return Err("invalid or duplicate induction candidate name".into());
        }
        let dependencies = item["depends_on"].as_array().ok_or("depends_on must be an array")?.iter().map(|d| {
            let n = text(d)?;
            names.get(n).copied().ok_or(format!("candidate {name} requires an earlier dependency {n}; forward/self/cyclic dependencies are forbidden"))
        }).collect::<Res<Vec<usize>>>()?;
        // Reuse the candidate type checker, with a fixed true context/guard.
        // The restricted environment excludes inputs, next state and reports.
        let typed = crate::lemma_candidate::LemmaCandidate {
            context: json!(true),
            guard: json!(true),
            claim: item["predicate"].clone(),
            depends_on: vec![],
            source: Some(name.into()),
        }
        .lower(&state_env)?;
        names.insert(name.to_owned(), parsed.len());
        parsed.push(Proposed {
            name: name.into(),
            predicate: typed.claim().clone(),
            dependencies,
        });
    }
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let mut q = Check {
        z3: "external-solver-forbidden".into(),
        out,
        reports: vec![],
    };
    let safety = crate::spec_binding::safety_model(spec)?;
    let reset_sub = machine
        .state
        .iter()
        .map(|(n, t)| (t.clone(), machine.reset[n].clone()))
        .collect::<BTreeMap<_, _>>();
    let next_sub = machine
        .state
        .iter()
        .map(|(n, t)| (t.clone(), machine.next[n].clone()))
        .collect::<BTreeMap<_, _>>();
    let mut established: Vec<Established> = vec![];
    let mut candidates = vec![];
    let mut target_uses = vec![];
    let mut stage = "reset_establishment".to_owned();
    let mut complete = sequent(
        &mut q,
        "induction_target_reset",
        safety.reset.clone(),
        safety.reset_good.clone(),
        &safety.context,
    )?;
    if complete {
        // Feasibility is not a proof assumption. Scalar domains are nonempty,
        // and this live SAT check also records a concrete valid reset valuation.
        q.query_with_witness(
            "induction_reset_nonempty",
            and(safety.reset.clone(), safety.reset_good.clone()),
            true,
            &safety.context,
        )?;
        complete = q.reports.last().unwrap()["status"] == "passed";
        stage = "reset_nonempty".into();
    }
    for candidate in &parsed {
        if !complete {
            break;
        }
        let index = established.len();
        let initial_name = format!("induction_candidate_{index}_initialization");
        let preservation_name = format!("induction_candidate_{index}_preservation");
        stage = initial_name.clone();
        let initial = sequent(
            &mut q,
            &initial_name,
            safety.reset.clone(),
            substitute(&candidate.predicate, &reset_sub),
            &safety.context,
        )?;
        let mut preserved = false;
        if initial {
            stage = preservation_name.clone();
            let hypotheses = conjunction(
                std::iter::once(candidate.predicate.clone()).chain(
                    candidate
                        .dependencies
                        .iter()
                        .map(|&i| established[i].predicate.clone()),
                ),
            );
            preserved = sequent(
                &mut q,
                &preservation_name,
                and(not(safety.reset.clone()), hypotheses),
                substitute(&candidate.predicate, &next_sub),
                &safety.context,
            )?;
        }
        candidates.push(json!({"name":candidate.name,"initialization_checked":initial,"preservation_checked":preserved,
            "dependencies":candidate.dependencies.iter().map(|&i|parsed[i].name.clone()).collect::<Vec<_>>(),
            "status":if initial&&preserved {"established"}else{"not_established"},
            "preservation_hypothesis":"self plus explicitly named earlier established invariants; no original target assumptions"}));
        complete = initial && preserved;
        if complete {
            established.push(Established {
                predicate: candidate.predicate.clone(),
            });
        }
    }
    if complete {
        let invariant = conjunction(established.iter().map(|h| h.predicate.clone()));
        for (index, (name, bad, operation)) in safety.checks.iter().enumerate() {
            let proof = format!("induction_target_use_{index}");
            stage = proof.clone();
            // Keep the exact original safety predicate, including its own
            // mapped invariant and operation selectors. Never rewrite the spec.
            let checked = sequent(
                &mut q,
                &proof,
                invariant.clone(),
                not(bad.clone()),
                &safety.context,
            )?;
            target_uses.push(
                json!({"name":name,"operation":operation,"checked":checked,"proof":proof,
                "established_invariants":parsed.iter().map(|c|c.name.clone()).collect::<Vec<_>>()}),
            );
            if !checked {
                complete = false;
                break;
            }
        }
    }
    let counterexample = q
        .reports
        .iter()
        .any(|r| r["status"] == "counterexample" || r["status"] == "failed_nonvacuity");
    Ok(
        json!({"status":if complete {"inductive_safety_verified"}else if counterexample {"induction_counterexample"}else{"unknown"},
        "document":spec.document(),"candidates":candidates,"target_uses":target_uses,"proofs":q.reports,
        "failed_stage":if complete {Value::Null}else{json!(stage)},"environment_assumptions":[],
        "claim":if complete {"Unbounded reset-inductive safety of the original state binding and selected contract under all scalar inputs; repeated reset restarts the proof"}else{"Inductive safety is not established"},
        "authority":"Fresh initialization, acyclic preservation and target-use sequent handles; serialized reports are not authority",
        "limitations":["No liveness, response deadline or fairness theorem","One-step induction countermodels are not reset-reachable counterexamples; use bounded search/replay separately","Source lowering and actual signal bindings remain in the source trust boundary","No synthesized timing or general AXI-compliance claim"]}),
    )
}
