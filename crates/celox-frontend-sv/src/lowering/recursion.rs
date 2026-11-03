//! The recursive call cycles of a module, reported for the design of
//! out-of-line calls.
//!
//! The call graph is recorded by the executable lowering itself: an edge is
//! added where [`Ff`](super::ff) or [`Comb`](super::comb) inlines a call,
//! from the subroutine being expanded, or from the unit when none is. Calls
//! in argument defaults are made at the call site that uses the default, and
//! calls in timing controls where the control is lowered, so the graph is the
//! one the lowering follows, by construction. A unit is a process, named once
//! however many lowerings of it are attempted, and an attempt that fails
//! leaves nothing in the graph. The size of an expansion is the code the
//! lowering built for it, its nested calls, unrolled loops and folding
//! included: Ff's SIR instructions and blocks, Comb's SLT nodes (which become
//! SIR only with their whole path). The out-of-line threshold is defined on
//! the instruction scale, since out-of-line calls are SIR calls.
//!
//! The report is written only when the analysis options name a file
//! (`CELOX_SV_RECURSION_REPORT` through the diagnostics options), to which
//! each lowered module appends its lines. Recording only observes: the
//! lowering, and the recursive call it rejects, are the same without it. The
//! arguments of the rejected call are not evaluated, so the calls they would
//! make are taken from their expressions.
use super::procedural::ProcModule;
use super::*;
use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;

/// A node of the call graph: a unit that the lowering builds, or a
/// subroutine.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(super) enum Caller {
    Unit(usize),
    Subroutine(String),
}

/// The code built by an inline expansion: the SIR of an imperative (Ff)
/// expansion, the SLT nodes of a symbolic (Comb) one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Expansion {
    pub instructions: usize,
    pub blocks: usize,
    pub nodes: usize,
}

impl Expansion {
    /// The larger of each measure.
    fn widest(self, other: Expansion) -> Expansion {
        Expansion {
            instructions: self.instructions.max(other.instructions),
            blocks: self.blocks.max(other.blocks),
            nodes: self.nodes.max(other.nodes),
        }
    }
}

/// What a lowering, or one attempt of it, recorded.
#[derive(Debug, Clone, Default)]
struct Record {
    edges: BTreeSet<(Caller, String)>,
    /// The largest inline expansion of each subroutine, of each measure.
    expansions: HashMap<String, Expansion>,
}

impl Record {
    fn merge(&mut self, other: Record) {
        self.edges.extend(other.edges);
        for (name, size) in other.expansions {
            self.expanded(name, size);
        }
    }

    fn expanded(&mut self, name: String, size: Expansion) {
        let largest = self.expansions.entry(name).or_default();
        *largest = largest.widest(size);
    }
}

/// The calls a module's lowering inlines.
///
/// A unit is a process (or another body the lowering builds as one), named
/// once however many times its lowering is attempted. A speculative attempt,
/// which the lowering abandons for another when it fails, records into its
/// own frame: committed when it is kept, discarded when it fails.
#[derive(Debug, Clone, Default)]
pub(crate) struct CallGraph {
    units: usize,
    /// The unit being lowered.
    current: usize,
    record: Record,
    /// The open attempts, innermost last.
    attempts: Vec<Record>,
}

impl CallGraph {
    fn top(&mut self) -> &mut Record {
        match self.attempts.last_mut() {
            Some(record) => record,
            None => &mut self.record,
        }
    }

    /// Start lowering a new unit.
    pub(super) fn unit(&mut self) -> usize {
        self.units += 1;
        self.current = self.units - 1;
        self.current
    }

    /// Lower `unit` again, as another attempt or part of it.
    pub(super) fn resume(&mut self, unit: usize) {
        self.current = unit;
    }

    /// Start a speculative attempt.
    pub(super) fn attempt(&mut self) {
        self.attempts.push(Record::default());
    }

    /// Keep the innermost attempt.
    pub(super) fn commit(&mut self) {
        if let Some(record) = self.attempts.pop() {
            self.top().merge(record);
        }
    }

    /// Drop the innermost attempt and what it recorded.
    pub(super) fn discard(&mut self) {
        self.attempts.pop();
    }

    /// A call of `callee` from the innermost expanded subroutine of
    /// `active`, or from the current unit when none is being expanded.
    pub(super) fn call(&mut self, active: &[String], callee: &str) {
        let caller = active.last().map_or(Caller::Unit(self.current), |name| {
            Caller::Subroutine(name.clone())
        });
        self.top().edges.insert((caller, callee.to_string()));
    }

    /// An inline expansion of `callee` built `size`.
    pub(super) fn expanded(&mut self, callee: &str, size: Expansion) {
        self.top().expanded(callee.to_string(), size);
    }

    /// The called subroutines, and the members of each recursive strongly
    /// connected component among them (Tarjan, iterative).
    fn components(&self) -> (Vec<String>, Vec<Vec<usize>>) {
        let names: Vec<String> = self
            .record
            .edges
            .iter()
            .flat_map(|(caller, callee)| {
                let caller = match caller {
                    Caller::Subroutine(name) => Some(name.clone()),
                    Caller::Unit(_) => None,
                };
                caller.into_iter().chain([callee.clone()])
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let index: HashMap<&str, usize> = names
            .iter()
            .enumerate()
            .map(|(position, name)| (name.as_str(), position))
            .collect();
        let mut calls = vec![Vec::new(); names.len()];
        for (caller, callee) in &self.record.edges {
            if let Caller::Subroutine(caller) = caller {
                calls[index[caller.as_str()]].push(index[callee.as_str()]);
            }
        }
        let count = names.len();
        let mut order = vec![usize::MAX; count];
        let mut low = vec![0; count];
        let mut on_stack = vec![false; count];
        let mut members: Vec<Vec<usize>> = Vec::new();
        let mut stack = Vec::new();
        let mut next = 0;
        for root in 0..count {
            if order[root] != usize::MAX {
                continue;
            }
            let mut work = vec![(root, 0)];
            while let Some((node, edge)) = work.pop() {
                if edge == 0 {
                    order[node] = next;
                    low[node] = next;
                    next += 1;
                    stack.push(node);
                    on_stack[node] = true;
                }
                if let Some(&callee) = calls[node].get(edge) {
                    work.push((node, edge + 1));
                    if order[callee] == usize::MAX {
                        work.push((callee, 0));
                    } else if on_stack[callee] {
                        low[node] = low[node].min(order[callee]);
                    }
                    continue;
                }
                if low[node] == order[node] {
                    let mut nodes = Vec::new();
                    loop {
                        let member = stack.pop().expect("a component member");
                        on_stack[member] = false;
                        nodes.push(member);
                        if member == node {
                            break;
                        }
                    }
                    members.push(nodes);
                }
                if let Some(&(caller, _)) = work.last() {
                    low[caller] = low[caller].min(low[node]);
                }
            }
        }
        let recursive = members
            .into_iter()
            .filter(|nodes| nodes.len() > 1 || calls[nodes[0]].contains(&nodes[0]))
            .collect();
        (names, recursive)
    }
}

fn statements(body: &[sv::ir::Stmt]) -> u64 {
    let mut count = 0;
    for stmt in body {
        stmt.walk(&mut |_| count += 1);
    }
    count
}

/// The call recording that [`Ff`](super::ff) and [`Comb`](super::comb)
/// share. Without a graph each is a no-op.
impl ProcModule<'_> {
    /// Name a new unit, which the calls lowered next come from.
    pub(super) fn call_unit(&mut self) -> usize {
        self.calls.as_deref_mut().map_or(0, CallGraph::unit)
    }

    /// Lower `unit` again (see [`CallGraph::resume`]).
    pub(super) fn resume_call_unit(&mut self, unit: usize) {
        if let Some(calls) = self.calls.as_deref_mut() {
            calls.resume(unit);
        }
    }

    /// Start a speculative attempt (see [`CallGraph::attempt`]).
    pub(super) fn attempt_calls(&mut self) {
        if let Some(calls) = self.calls.as_deref_mut() {
            calls.attempt();
        }
    }

    /// End the innermost attempt, keeping what it recorded if `kept`.
    pub(super) fn end_attempt_calls(&mut self, kept: bool) {
        if let Some(calls) = self.calls.as_deref_mut() {
            if kept {
                calls.commit();
            } else {
                calls.discard();
            }
        }
    }

    /// Record a call of `callee` while `active` are being expanded.
    pub(super) fn record_call(&mut self, active: &[String], callee: &str) {
        if let Some(calls) = self.calls.as_deref_mut() {
            calls.call(active, callee);
        }
    }

    /// Record the calls that the arguments `args` of a call of `subroutine`
    /// that the lowering rejects as recursive, and the defaults they leave
    /// to it, would make: the lowering evaluates none of them, so they are
    /// taken from the expressions.
    pub(super) fn record_argument_calls(
        &mut self,
        active: &[String],
        subroutine: &sv::ir::Subroutine,
        args: &[Option<sv::ir::Expr>],
    ) {
        let Some(calls) = self.calls.as_deref_mut() else {
            return;
        };
        let mut made = Vec::new();
        for arg in args.iter().flatten() {
            super::procedural::collect_calls(arg, &mut made);
        }
        super::procedural::default_calls(subroutine, args, &mut made);
        for (callee, _) in made {
            if let sv::ir::Callee::Subroutine(name) = callee {
                calls.call(active, &name);
            }
        }
    }

    /// An inline expansion of `callee` built `size` (see
    /// [`CallGraph::expanded`]).
    pub(super) fn expanded_call(&mut self, callee: &str, size: Expansion) {
        if let Some(calls) = self.calls.as_deref_mut() {
            calls.expanded(callee, size);
        }
    }
}

/// The report lines of `module` from the calls its lowering inlined: its
/// recursive components, with their members, own statements, largest
/// expansion and the units that reach them, directly or through other
/// subroutines, and its largest expansions of other subroutines.
pub(super) fn report(module: &sv::ir::Module, graph: &CallGraph) -> Vec<String> {
    let (names, recursive) = graph.components();
    let record = &graph.record;
    let own: HashMap<&str, u64> = module
        .subroutines()
        .iter()
        .map(|subroutine| (subroutine.name.as_str(), statements(&subroutine.body)))
        .collect();
    let in_cycle: HashSet<&str> = recursive
        .iter()
        .flatten()
        .map(|node| names[*node].as_str())
        .collect();
    let mut callers: HashMap<&str, Vec<&Caller>> = HashMap::default();
    for (caller, callee) in &record.edges {
        callers.entry(callee.as_str()).or_default().push(caller);
    }
    let mut lines = vec![format!(
        "module {} units {} called_subroutines {} recursive_components {}",
        module.name(),
        graph.units,
        names.len(),
        recursive.len()
    )];
    let others: Vec<(&str, Expansion)> = record
        .expansions
        .iter()
        .filter(|(name, _)| !in_cycle.contains(name.as_str()))
        .map(|(name, size)| (name.as_str(), *size))
        .collect();
    let mut imperative: Vec<(usize, usize, &str)> = others
        .iter()
        .filter(|(_, size)| size.instructions > 0 || size.blocks > 0)
        .map(|(name, size)| (size.instructions, size.blocks, *name))
        .collect();
    imperative.sort_unstable_by(|left, right| right.cmp(left));
    for (instructions, blocks, name) in imperative.into_iter().take(5) {
        lines.push(format!(
            "  expansion_instructions {instructions} blocks {blocks} : {name}"
        ));
    }
    let mut symbolic: Vec<(usize, &str)> = others
        .iter()
        .filter(|(_, size)| size.nodes > 0)
        .map(|(name, size)| (size.nodes, *name))
        .collect();
    symbolic.sort_unstable_by(|left, right| right.cmp(left));
    for (nodes, name) in symbolic.into_iter().take(5) {
        lines.push(format!("  expansion_nodes {nodes} : {name}"));
    }
    let mut rows = Vec::new();
    for nodes in &recursive {
        let members: BTreeSet<&str> = nodes.iter().map(|node| names[*node].as_str()).collect();
        // The units that reach a member through any chain of calls.
        let mut units = BTreeSet::new();
        let mut seen: HashSet<&str> = members.iter().copied().collect();
        let mut work: Vec<&str> = members.iter().copied().collect();
        while let Some(callee) = work.pop() {
            for caller in callers.get(callee).into_iter().flatten() {
                match caller {
                    Caller::Unit(unit) => {
                        units.insert(*unit);
                    }
                    Caller::Subroutine(name) => {
                        if seen.insert(name.as_str()) {
                            work.push(name.as_str());
                        }
                    }
                }
            }
        }
        let own_statements: u64 = members
            .iter()
            .map(|name| own.get(name).copied().unwrap_or(0))
            .sum();
        let expansion = members
            .iter()
            .filter_map(|name| record.expansions.get(*name).copied())
            .fold(Expansion::default(), Expansion::widest);
        let joined = members.iter().copied().collect::<Vec<_>>().join(" ");
        rows.push((
            expansion.instructions,
            expansion.blocks,
            expansion.nodes,
            members.len(),
            own_statements,
            units.len(),
            joined,
        ));
    }
    rows.sort_unstable_by(|left, right| right.cmp(left));
    for (instructions, blocks, nodes, members, own_statements, units, joined) in rows {
        lines.push(format!(
            "  component members {members} own_statements {own_statements} \
             expansion_instructions {instructions} blocks {blocks} nodes {nodes} \
             reaching_units {units} : {joined}"
        ));
    }
    lines
}

/// Append the report of `module` to `path`.
pub(super) fn append(module: &sv::ir::Module, graph: &CallGraph, path: &Path) {
    let lines = report(module, graph);
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = file.write_all((lines.join("\n") + "\n").as_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::super::{FrontendTraceOptions, schedule_sources_with_options, sv};
    use std::sync::Arc;

    /// The SIR, or the error, of scheduling `source`, and the report lines
    /// when `report` names a file.
    fn schedule(source: &str, report: Option<&std::path::Path>) -> (String, Vec<String>) {
        if let Some(path) = report {
            let _ = std::fs::remove_file(path);
        }
        let scheduled = schedule_sources_with_options(
            &[(source, std::path::Path::new("recursion.sv"))],
            "Top",
            &[],
            &[],
            &[],
            false,
            &celox_frontend_core::ParallelScheduleOptions::default(),
            &FrontendTraceOptions::default(),
            None,
            &mut Vec::new(),
            sv::AnalysisOptions {
                recursion_report: report.map(Arc::from),
                ..sv::AnalysisOptions::default()
            },
        );
        let result = match scheduled {
            Ok(output) => format!("{:?}", output.scheduled.sir),
            Err(error) => format!("error {error:?}"),
        };
        let lines = report
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|report| report.lines().map(str::to_string).collect())
            .unwrap_or_default();
        if let Some(path) = report {
            let _ = std::fs::remove_file(path);
        }
        (result, lines)
    }

    /// The report lines of scheduling `source`, and its error if it fails.
    /// Recording changes nothing: the SIR or error is the one of scheduling
    /// it without the report.
    fn record(name: &str, source: &str) -> (Vec<String>, Option<String>) {
        let path =
            std::env::temp_dir().join(format!("celox-recursion-{}-{name}.txt", std::process::id()));
        let (recorded, lines) = schedule(source, Some(&path));
        let (plain, _) = schedule(source, None);
        assert_eq!(recorded, plain, "the report changed the lowering of {name}");
        let error = recorded.starts_with("error ").then_some(recorded);
        (lines, error)
    }

    fn components(lines: &[String]) -> Vec<&String> {
        lines
            .iter()
            .filter(|line| line.contains("component members"))
            .collect()
    }

    #[test]
    fn records_recursive_components_the_lowering_follows() {
        let (lines, error) = record(
            "follows",
            "module Top(input clk, input int a, output int r);
               function automatic int even(int n); return n == 0 ? 1 : odd(n - 1); endfunction
               function automatic int odd(int n); return n == 0 ? 0 : even(n - 1); endfunction
               function automatic int plain(int n); return n + 1; endfunction
               always_ff @(posedge clk) r <= plain(a) + even(a);
             endmodule",
        );
        assert!(error.is_some_and(|error| error.contains("recursive call of `even`")));
        let found = components(&lines);
        assert_eq!(found.len(), 1, "{lines:?}");
        assert!(
            found[0].ends_with("reaching_units 1 : even odd"),
            "{lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line.starts_with("  expansion_instructions ")
                    && line.ends_with(": plain")),
            "{lines:?}"
        );
    }

    #[test]
    fn ignores_calls_the_lowering_does_not_make() {
        // A branch that constant folding removes, and a default that every
        // call supplies, make no call.
        let (lines, error) = record(
            "ignores",
            "module Top(input clk, input int a, output int r);
               function automatic int f(int n, int x = g()); if (0) return f(n); return n + x; endfunction
               function automatic int g(); return f(1, 2); endfunction
               always_ff @(posedge clk) r <= f(a, 4);
             endmodule",
        );
        assert!(error.is_none(), "{error:?}");
        assert!(
            lines[0].ends_with("called_subroutines 1 recursive_components 0"),
            "{lines:?}"
        );
    }

    #[test]
    fn records_calls_in_defaults() {
        // `f(a)` omits `x`, so its default calls `h` at that call site.
        let (lines, _) = record(
            "defaults",
            "module Top(input clk, input int a, output int r);
               function automatic int h(int n); return n == 0 ? 0 : h(n - 1); endfunction
               function automatic int f(int n, int x = h(a)); return n + x; endfunction
               always_ff @(posedge clk) r <= f(a);
             endmodule",
        );
        let found = components(&lines);
        assert_eq!(found.len(), 1, "{lines:?}");
        assert!(found[0].ends_with("reaching_units 1 : h"), "{lines:?}");
    }

    #[test]
    fn records_calls_in_timing_controls() {
        let (lines, _) = record(
            "controls",
            "module Top(input int a, output int r);
               function automatic bit ready(int n); return n == 0 || ready(n - 1); endfunction
               initial begin wait (ready(a)); r = 1; end
             endmodule",
        );
        let found = components(&lines);
        assert_eq!(found.len(), 1, "{lines:?}");
        assert!(found[0].ends_with(": ready"), "{lines:?}");
    }

    #[test]
    fn counts_units_that_reach_a_component_through_other_calls() {
        let (lines, _) = record(
            "transitive",
            "module Top(input clk, input int a, output int r);
               function automatic int f(int n); return n == 0 ? 0 : f(n - 1); endfunction
               function automatic int wrap(int n); return f(n) + 1; endfunction
               always_ff @(posedge clk) r <= wrap(a);
             endmodule",
        );
        let found = components(&lines);
        assert_eq!(found.len(), 1, "{lines:?}");
        assert!(found[0].ends_with("reaching_units 1 : f"), "{lines:?}");
    }

    #[test]
    fn names_one_unit_per_process_across_attempts() {
        // The initial process is first tried as a folded initializer, which
        // rejects the recursive call, and then lowered as a kernel.
        let (lines, _) = record(
            "attempts",
            "module Top(input int a, output int r);
               function automatic int f(int n); return n == 0 ? 0 : f(n - 1); endfunction
               initial r = f(a);
             endmodule",
        );
        assert!(lines[0].starts_with("module Top units 1 "), "{lines:?}");
        let found = components(&lines);
        assert_eq!(found.len(), 1, "{lines:?}");
        assert!(found[0].ends_with("reaching_units 1 : f"), "{lines:?}");
    }

    #[test]
    fn records_the_calls_in_the_arguments_of_a_rejected_call() {
        // The recursive `f(g(n))` is rejected before `g(n)` is evaluated.
        let (lines, _) = record(
            "arguments",
            "module Top(input clk, input int a, output int r);
               function automatic int g(int n); return n + 1; endfunction
               function automatic int f(int n); return n == 0 ? 0 : f(g(n)); endfunction
               always_ff @(posedge clk) r <= f(a);
             endmodule",
        );
        assert!(
            lines[0].ends_with("called_subroutines 2 recursive_components 1"),
            "{lines:?}"
        );
        assert!(components(&lines)[0].ends_with(": f"), "{lines:?}");
    }

    /// The largest expansion of `name` in `lines` of the measure `measure`.
    fn expansion(lines: &[String], measure: &str, name: &str) -> usize {
        lines
            .iter()
            .find(|line| {
                line.starts_with(&format!("  {measure} ")) && line.ends_with(&format!(": {name}"))
            })
            .and_then(|line| line.split_whitespace().nth(1)?.parse().ok())
            .unwrap_or_else(|| panic!("no {measure} of {name}: {lines:?}"))
    }

    const PROCESSES: [(&str, &str); 2] = [
        ("always_comb r = f(a);", "expansion_nodes"),
        (
            "always_ff @(posedge clk) r <= f(a);",
            "expansion_instructions",
        ),
    ];

    #[test]
    fn sizes_an_expansion_with_the_calls_it_makes() {
        // Symbolic (Comb, in SLT nodes) and imperative (Ff, in SIR) alike.
        for (process, measure) in PROCESSES {
            let (lines, error) = record(
                "nested",
                &format!(
                    "module Top(input clk, input int a, output int r);
                       function automatic int g(int n); return n + 1; endfunction
                       function automatic int f(int n); return g(n) * 2; endfunction
                       {process}
                     endmodule"
                ),
            );
            assert!(error.is_none(), "{error:?}");
            let (f, g) = (
                expansion(&lines, measure, "f"),
                expansion(&lines, measure, "g"),
            );
            assert!(f > g && g > 0, "{process} {lines:?}");
        }
    }

    #[test]
    fn sizes_an_expansion_by_the_code_it_builds() {
        let size = |(process, measure): (&str, &str), body: &str| {
            let (lines, error) = record(
                "built",
                &format!(
                    "module Top(input clk, input int a, output int r);
                       function automatic int f(int n);
                         int x = n;
                         {body}
                         return x;
                       endfunction
                       {process}
                     endmodule"
                ),
            );
            assert!(error.is_none(), "{error:?}");
            expansion(&lines, measure, "f")
        };
        let [symbolic, imperative] = PROCESSES;
        let single = "x = x * 3 + n;";
        let repeated = "repeat (1000) x = x * 3 + n;";
        // The symbolic lowering unrolls the loop: at least one node for each
        // further iteration.
        assert!(size(symbolic, repeated) >= size(symbolic, single) + 999);
        // The imperative lowering builds the loop once, whatever its count.
        let looped = size(imperative, repeated);
        assert!(looped > size(imperative, single));
        assert_eq!(looped, size(imperative, "repeat (10) x = x * 3 + n;"));
    }

    #[test]
    fn drops_the_calls_of_a_dropped_alternative() {
        // The symbolic lowering folds `f(0)`; the imperative alternative
        // kept for the wait rejects the recursive call and is dropped, and
        // its calls with it.
        let (lines, error) = record(
            "alternative",
            "module Top(input int a, output int r);
               function automatic int f(int n); if (n == 0) return 0; return f(n - 1); endfunction
               always_comb begin r = 0; r = f(0) + a; end
               initial wait (r == 5);
             endmodule",
        );
        assert!(error.is_none(), "{error:?} {lines:?}");
        assert!(lines[0].ends_with("recursive_components 0"), "{lines:?}");
    }
}
