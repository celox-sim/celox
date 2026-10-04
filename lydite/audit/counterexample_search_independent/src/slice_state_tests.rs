//! Test-only child module appended to a byte-identified finite.rs snapshot.
//! No production or historical file is edited. This complements public-API
//! independent semantics with direct evidence that state survives suspension.
use super::*;

fn budget(work: u64, clauses: usize) -> Budget {
    Budget { limits: Limits { max_work: work, max_clauses: clauses, timeout_ms: 60_000, ..Limits::default() },
        start: Instant::now(), work: 0, time_check_in: 0 }
}
fn brute(vars: usize, clauses: &[Vec<Lit>]) -> Verdict {
    for bits in 0..1u64 << vars {
        if clauses.iter().all(|c| c.iter().any(|&p| {
            let value = bits & (1 << (p.unsigned_abs() - 1)) != 0;
            value == (p > 0)
        })) { return Verdict::Sat; }
    }
    Verdict::Unsat
}
#[derive(Default)]
struct Coverage { yields: u64, initialized: u64, decisions: u64, learned: u64, pending_propagations: u64, implied: u64 }
fn sliced(s: &mut Sat, b: &mut Budget, quantum: u64, c: &mut Coverage) -> Res<Verdict> {
    for _ in 0..1_000_000 {
        let before = s.clauses.len();
        match s.run_slice(b, quantum)? {
            Search::Complete(v) => return Ok(v),
            Search::Pending => {
                c.yields += 1;
                c.initialized += u64::from(s.initialized);
                c.decisions += u64::from(s.decisions > 0);
                c.learned += u64::from(s.clauses.len() > before);
                c.pending_propagations += u64::from(s.head < s.trail.len());
                c.implied += u64::from(s.reasons.iter().any(|r| r.is_some()));
            }
        }
    }
    panic!("slice search failed to make bounded progress")
}
fn same_state(a: &Sat, b: &Sat) {
    assert_eq!(a.initialized, b.initialized);
    assert_eq!(a.previous_clauses, b.previous_clauses);
    assert_eq!(a.clauses, b.clauses);
    assert_eq!(a.watches, b.watches);
    assert_eq!(a.values, b.values);
    assert_eq!(a.levels, b.levels);
    assert_eq!(a.reasons, b.reasons);
    assert_eq!(a.trail, b.trail);
    assert_eq!(a.starts, b.starts);
    assert_eq!(a.head, b.head);
    assert_eq!(a.activity, b.activity);
    assert_eq!(a.order.heap, b.order.heap);
    assert_eq!(a.order.positions, b.order.positions);
    assert_eq!(a.increment, b.increment);
    assert_eq!(a.phase, b.phase);
    assert_eq!(a.decisions, b.decisions);
    assert_eq!(a.conflicts, b.conflicts);
}
fn validate_model(s: &Sat, clauses: &[Vec<Lit>], result: Verdict) {
    if result == Verdict::Sat {
        assert!(s.values.iter().skip(1).all(|v| *v != 0));
        assert!(clauses.iter().all(|c| c.iter().any(|&p| truth(&s.values, p) > 0)));
    }
}
fn next(state: &mut u64) -> u64 {
    *state ^= *state << 13;
    *state ^= *state >> 7;
    *state ^= *state << 17;
    *state
}
fn random_cnf(vars: usize, seed: u64) -> Vec<Vec<Lit>> {
    let mut state = seed * 7919 + 17;
    (0..vars*4+7).map(|_| {
        let mut clause = Vec::new();
        while clause.len() < 3.min(vars) {
            let p = (next(&mut state) % vars as u64 + 1) as Lit;
            if clause.iter().any(|q: &Lit| q.unsigned_abs() == p.unsigned_abs()) { continue; }
            clause.push(if next(&mut state) & 1 != 0 { p } else { -p });
        }
        clause
    }).collect()
}
fn pigeonhole(pigeons: usize, holes: usize) -> (usize, Vec<Vec<Lit>>) {
    let v = |p: usize,h: usize| (p * holes + h + 1) as Lit;
    let mut clauses = Vec::new();
    for p in 0..pigeons { clauses.push((0..holes).map(|h| v(p,h)).collect()); }
    for h in 0..holes { for p in 0..pigeons { for q in p+1..pigeons {
        clauses.push(vec![-v(p,h), -v(q,h)]);
    } } }
    (pigeons*holes, clauses)
}

#[test]
fn independent_slice_state_exhaustive_random_cnf() {
    let mut coverage = Coverage::default();
    let mut checks = 0;
    for vars in 3..=8 {
        for seed in 1..=128 {
            let cnf = random_cnf(vars, seed);
            let expected = brute(vars, &cnf);
            let mut full = Sat::new(vars, cnf.clone());
            let mut full_budget = budget(100_000_000, 1_000_000);
            assert_eq!(full.run(&mut full_budget).unwrap(), expected);
            validate_model(&full, &cnf, expected);
            for quantum in [1, 2, 7, 31, 127] {
                let mut partial = Sat::new(vars, cnf.clone());
                let mut partial_budget = budget(100_000_000, 1_000_000);
                assert_eq!(sliced(&mut partial, &mut partial_budget, quantum, &mut coverage).unwrap(), expected);
                assert_eq!(full_budget.work, partial_budget.work);
                same_state(&full, &partial);
                validate_model(&partial, &cnf, expected);
                checks += 1;
            }
        }
    }
    assert!(coverage.yields > 0 && coverage.initialized > 0 && coverage.decisions > 0);
    assert!(coverage.learned > 0 && coverage.pending_propagations > 0 && coverage.implied > 0);
    println!("slice_state_checks={checks} yields={} initialized={} decisions={} learned={} pending_propagations={} implied={}",
        coverage.yields, coverage.initialized, coverage.decisions, coverage.learned, coverage.pending_propagations, coverage.implied);
}

#[test]
fn independent_slice_state_exact_global_work_and_clause_limits() {
    let (vars, cnf) = pigeonhole(5,4);
    let mut full = Sat::new(vars, cnf.clone());
    full.previous_clauses = 137;
    let mut full_budget = budget(100_000_000, 1_000_000);
    assert_eq!(full.run(&mut full_budget).unwrap(), Verdict::Unsat);
    assert!(full.clauses.len() > cnf.len());
    let mut coverage = Coverage::default();
    let mut checks = 0;
    for quantum in [1, 7, 31, 4096] {
        let mut s = Sat::new(vars,cnf.clone()); s.previous_clauses = 137;
        let mut b = budget(full_budget.work, 137 + full.clauses.len());
        assert_eq!(sliced(&mut s,&mut b,quantum,&mut coverage).unwrap(),Verdict::Unsat);
        same_state(&full,&s);
        assert_eq!(b.work,full_budget.work);
        for cap in [0,1,2,7,31,127,full_budget.work/4,full_budget.work/2,full_budget.work-1] {
            let mut s = Sat::new(vars,cnf.clone()); s.previous_clauses = 137;
            let mut b = budget(cap,1_000_000);
            let error = sliced(&mut s,&mut b,quantum,&mut coverage).unwrap_err();
            assert!(error.contains("work budget"),"{error}");
            checks += 1;
        }
        let mut s = Sat::new(vars,cnf.clone()); s.previous_clauses = 137;
        let mut b = budget(100_000_000,137 + full.clauses.len()-1);
        let error = sliced(&mut s,&mut b,quantum,&mut coverage).unwrap_err();
        assert!(error.contains("learned-clause budget"),"{error}");
        assert_eq!(137+s.clauses.len(),b.limits.max_clauses);
        checks += 2;
    }
    println!("slice_exact_budget_checks={checks} reference_work={} original_clauses={} learned_clauses={} extra_charged_clauses=137",full_budget.work,cnf.len(),full.clauses.len()-cnf.len());
}

#[test]
fn independent_slice_state_terminal_errors_are_not_yields() {
    let (vars,cnf) = pigeonhole(5,4);
    let mut s = Sat::new(vars,cnf);
    let mut b = budget(100_000_000,1_000_000);
    assert_eq!(s.run_slice(&mut b,1).unwrap(),Search::Pending);
    b.limits.timeout_ms=0;
    b.time_check_in=0;
    let error = s.run_slice(&mut b,1).unwrap_err();
    assert!(error.contains("time budget"),"{error}");
    // Errors can occur mid-mutation and are intentionally never resumed.
}

#[test]
fn independent_slice_state_interleaved_branch_assumptions() {
    let (vars,hard) = pigeonhole(5,4);
    let guard = (vars+1) as Lit;
    let mut base = hard.into_iter().map(|mut c| { c.push(-guard); c }).collect::<Vec<_>>();
    let easy = (vars+2) as Lit;
    base.push(vec![guard,easy]);
    let mut hard_cnf=base.clone(); hard_cnf.push(vec![guard]);
    let mut easy_cnf=base.clone(); easy_cnf.push(vec![easy]); easy_cnf.push(vec![-guard]);
    let mut hard=Sat::new(vars+2,hard_cnf);
    let mut easy=Sat::new(vars+2,easy_cnf.clone());
    let mut b=budget(100_000_000,1_000_000);
    let mut hard_result=None;
    let mut easy_result=None;
    let mut hard_yields=0;
    let mut learned_before_easy=false;
    while hard_result.is_none() || easy_result.is_none() {
        if hard_result.is_none() {
            match hard.run_slice(&mut b,1).unwrap() {
                Search::Complete(v)=>hard_result=Some(v),
                Search::Pending=>hard_yields+=1,
            }
        }
        // Start the easy branch only after hard branch has learned, ensuring
        // assumption-local clauses exist when a second branch is initialized.
        if hard.clauses.len()>base.len()+1 && easy_result.is_none() {
            learned_before_easy=true;
            if let Search::Complete(v)=easy.run_slice(&mut b,1).unwrap() { easy_result=Some(v); }
        }
    }
    assert_eq!(hard_result,Some(Verdict::Unsat));
    assert_eq!(easy_result,Some(Verdict::Sat));
    assert!(learned_before_easy && hard_yields>0);
    validate_model(&easy,&easy_cnf,Verdict::Sat);
    println!("interleaved_assumption_checks=2 hard_yields={hard_yields} hard_learned={}",hard.clauses.len()-base.len()-1);
}

#[test]
fn independent_slice_state_unsplit_learned_clause_reuse() {
    let mut entailment_checks = 0;
    let mut branch_checks = 0;
    let mut sat_sources_with_learning = 0;
    let mut pending_probes = 0;
    for vars in 3..=8 {
        for seed in 1..=128 {
            let cnf = random_cnf(vars, seed);
            let mut probe = Sat::new(vars,cnf.clone());
            let mut b=budget(100_000_000,1_000_000);
            loop {
                match probe.run_slice(&mut b,1).unwrap() {
                    Search::Complete(_)=>break,
                    Search::Pending if probe.clauses.len()>cnf.len()=>{ pending_probes+=1; break; }
                    Search::Pending=>{},
                }
            }
            if probe.clauses.len()==cnf.len() { continue; }
            let mut sat=false;
            for bits in 0..1u64<<vars {
                let holds = |c: &Vec<Lit>| c.iter().any(|&p| ((bits & (1 << (p.unsigned_abs()-1)))!=0)==(p>0));
                if !cnf.iter().all(holds) { continue; }
                sat=true;
                for learned in &probe.clauses[cnf.len()..] {
                    assert!(holds(learned),"unsplit learned clause not entailed by original CNF");
                    entailment_checks+=1;
                }
            }
            sat_sources_with_learning+=u64::from(sat);
            // Probe's clauses have mutated watched-literal order as well as
            // appended globally entailed clauses. Fresh branch initialization
            // must handle both without inherited assignments or watches.
            for var in 1..=vars as Lit { for sign in [-1,1] {
                let mut original=cnf.clone(); original.push(vec![var*sign]);
                let mut reused=probe.clauses.clone(); reused.push(vec![var*sign]);
                let mut branch=Sat::new(vars,reused);
                let mut b=budget(100_000_000,1_000_000);
                let mut c=Coverage::default();
                let actual=sliced(&mut branch,&mut b,7,&mut c).unwrap();
                assert_eq!(actual,brute(vars,&original));
                validate_model(&branch,&original,actual);
                branch_checks+=1;
            } }
        }
    }
    assert!(entailment_checks>0 && sat_sources_with_learning>0 && pending_probes>0);
    println!("probe_learned_reuse_branch_checks={branch_checks} entailment_checks={entailment_checks} sat_sources_with_learning={sat_sources_with_learning} pending_probes={pending_probes}");
}
