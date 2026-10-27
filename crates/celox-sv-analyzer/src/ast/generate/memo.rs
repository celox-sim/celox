//! One elaboration per scope and bindings while a module is analyzed.
//!
//! The collectors of a module each ask for the active items of the same
//! scope. A window opened for the module keeps each elaboration, keyed by its
//! arguments and by the revisions of the analysis state that elaboration
//! reads (imported symbols, constant functions, active interface instances,
//! time scopes), so a change of that state makes earlier results unreachable.
//! A repeated elaboration allocates as many call sites as the first one did,
//! so later calls keep their sites.
use super::*;
use std::cell::Cell;

mod sites;
pub(in crate::ast) use sites::CallSites;

/// Names a value of analysis state that elaboration reads. Each change gives
/// a new revision; a guard that restores the previous value restores its
/// revision too, so equal revisions mean equal state.
#[derive(Default)]
pub(in crate::ast) struct Revision {
    current: Cell<u64>,
    last: Cell<u64>,
}

impl Revision {
    /// Record a change, returning the revision to restore with the value.
    pub(in crate::ast) fn change(&self) -> u64 {
        self.last.set(self.last.get() + 1);
        self.current.replace(self.last.get())
    }

    pub(in crate::ast) fn restore(&self, previous: u64) {
        self.current.set(previous);
    }

    pub(in crate::ast) fn get(&self) -> u64 {
        self.current.get()
    }
}

#[derive(Default)]
pub(in crate::ast) struct State(RefCell<Option<Window>>);

struct Window {
    /// The syntax tree of the module, borrowed by the [`Memoized`] guard.
    tree: usize,
    entries: Vec<Entry>,
}

struct Entry {
    revisions: [u64; 3],
    literal_use: LiteralUse,
    overrides: HashMap<String, ConstExpr>,
    env: HashMap<String, i128>,
    aliases: HashMap<String, Type>,
    items: Vec<Item>,
    /// How many call sites the elaboration allocated.
    sites: usize,
}

/// Keeps elaborations of `tree` until dropped; the previous window, of an
/// enclosing analysis, is restored then.
pub(in crate::ast) struct Memoized<'a> {
    state: std::rc::Rc<AnalysisState>,
    previous: Option<Window>,
    _tree: &'a SyntaxTree,
}

impl Drop for Memoized<'_> {
    fn drop(&mut self) {
        // The items retain the state; release them outside the borrow.
        let window = self.state.generate_items.0.replace(self.previous.take());
        drop(window);
    }
}

pub(in crate::ast) fn memoize<'a>(tree: &AnalysisContext<'a>) -> Memoized<'a> {
    let window = Window {
        tree: address(tree.syntax_tree),
        entries: Vec::new(),
    };
    Memoized {
        state: tree.state.clone(),
        previous: tree.state.generate_items.0.replace(Some(window)),
        _tree: tree.syntax_tree,
    }
}

fn address(tree: &SyntaxTree) -> usize {
    tree as *const SyntaxTree as usize
}

fn revisions(state: &AnalysisState) -> [u64; 3] {
    [
        state.revision.get(),
        const_functions::revision(&state.const_functions).get(),
        timescales::revision(&state.timescales).get(),
    ]
}

/// The items `elaborate` gives for these arguments, elaborated once per window.
pub(super) fn items(
    tree: &AnalysisContext<'_>,
    env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
    literal_use: LiteralUse,
    elaborate: impl FnOnce() -> Result<Vec<Item>, AnalyzerError>,
) -> Result<Vec<Item>, AnalyzerError> {
    #[cfg(test)]
    let elaborate = || {
        tests::ELABORATIONS.with(|count| count.set(count.get() + 1));
        elaborate()
    };
    #[cfg(test)]
    if tests::DISABLED.with(Cell::get) {
        return elaborate();
    }
    let state = &tree.state;
    let applies = |window: &Window| window.tree == address(tree.syntax_tree);
    {
        let window = state.generate_items.0.borrow();
        let Some(window) = window.as_ref().filter(|window| applies(window)) else {
            drop(window);
            return elaborate();
        };
        let current = revisions(state);
        if let Some(entry) = window.entries.iter().find(|entry| {
            entry.revisions == current
                && entry.literal_use == literal_use
                && entry.overrides == *tree.parameter_overrides
                && entry.env == *env
                && entry.aliases == *aliases
        }) {
            let next = state.next_call_site.get() + entry.sites;
            state.next_call_site.set(next);
            return Ok(entry.items.clone());
        }
    }
    let first_site = state.next_call_site.get();
    let current = revisions(state);
    let items = elaborate()?;
    // Elaboration that leaves the state changed is not repeatable.
    if revisions(state) != current {
        return Ok(items);
    }
    let sites = first_site..state.next_call_site.get();
    // Effectful calls the items retain must get new sites when they are
    // elaborated again.
    if !sites.is_empty() && sites::refer_to(&items, sites.clone()) {
        return Ok(items);
    }
    let mut window = state.generate_items.0.borrow_mut();
    if let Some(window) = window.as_mut().filter(|window| applies(window)) {
        window.entries.push(Entry {
            revisions: current,
            literal_use,
            overrides: tree.parameter_overrides.clone().into_owned(),
            env: env.clone(),
            aliases: aliases.clone(),
            items: items.clone(),
            sites: sites.len(),
        });
    }
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;

    thread_local! {
        pub(super) static DISABLED: Cell<bool> = const { Cell::new(false) };
        pub(super) static ELABORATIONS: Cell<usize> = const { Cell::new(0) };
    }

    /// The analysis of `code` and its number of elaborations.
    fn analyze(code: &str, memoized: bool) -> (Result<crate::ir::Ir, AnalyzerError>, usize) {
        DISABLED.with(|disabled| disabled.set(!memoized));
        ELABORATIONS.with(|count| count.set(0));
        let result = crate::analyze_source(code, Path::new("memo.sv"));
        DISABLED.with(|disabled| disabled.set(false));
        (result, ELABORATIONS.with(Cell::get))
    }

    const PACKAGE: &str = r#"
        package p;
            function automatic int twice(input int x);
                return 2 * x;
            endfunction
            typedef enum logic [1:0] { A, B, C } e_t;
        endpackage
    "#;

    const GENERATE: &str = r#"
        module Leaf #(parameter int W = 1) (input logic [W-1:0] a, output logic [W-1:0] y);
            assign y = ~a;
        endmodule
        module Top import p::*; #(parameter int N = 3, parameter int M = twice(N))
            (input logic clk, input logic [M-1:0] a, output logic [M-1:0] y,
             output logic [M-1:0] q, output e_t s);
            localparam int K = twice(M);
            function automatic int pick(input int i);
                return i % 2;
            endfunction
            for (genvar i = 0; i < M; i++) begin : g
                if (pick(i) == 0) begin : even
                    logic t;
                    assign t = a[i];
                    assign y[i] = t;
                end else begin : odd
                    Leaf #(.W(1)) u (.a(a[i]), .y(y[i]));
                end
            end
            case (K % 3)
                0: begin : c0
                    always_ff @(posedge clk) q <= a;
                end
                default: begin : cd
                    always_ff @(posedge clk) q <= ~a;
                end
            endcase
            always_comb s = (K > 4) ? B : C;
            initial begin
                for (int j = 0; j < N; j++) $display("%0d", twice(j));
            end
        endmodule
    "#;

    const INTERFACE: &str = r#"
        interface bus #(parameter int W = 4);
            logic [W-1:0] data;
            modport src(output data);
        endinterface
        module Drive #(parameter int W = 4) (bus.src b);
            for (genvar i = 0; i < W; i++) begin : g
                assign b.data[i] = i[0];
            end
        endmodule
        module Top(output logic [7:0] y);
            bus #(.W(8)) b();
            Drive #(.W(8)) d(.b(b));
            assign y = b.data;
        endmodule
    "#;

    const SITES: &str = r#"
        module Top #(parameter int N = 2) (input logic [1:0] i, output logic [3:0] y);
            function automatic logic [1:0] pick(input logic [1:0] x);
                return x;
            endfunction
            localparam int P = pick(N);
            for (genvar g = 0; g < P; g++) begin : b
                always_comb y[g] = i[pick(g)];
            end
            always_comb begin
                y[3:2] = 2'd0;
                y[pick(i) + 2] = 1'b1;
            end
        endmodule
    "#;

    #[test]
    fn memoized_elaboration_gives_the_same_analysis() {
        let generate = format!("{PACKAGE}{GENERATE}");
        for code in [generate.as_str(), INTERFACE, SITES] {
            let (direct, direct_count) = analyze(code, false);
            let (memoized, memoized_count) = analyze(code, true);
            assert!(direct.is_ok(), "{direct:?}");
            assert_eq!(direct, memoized);
            assert!(
                memoized_count < direct_count,
                "{memoized_count} elaborations, {direct_count} without memo"
            );
        }
    }
}
