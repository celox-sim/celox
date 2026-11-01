//! One elaboration per scope and bindings while a module is analyzed.
//!
//! The collectors of a module each ask for the active items of the same
//! scope. A window opened for the module keeps each elaboration, keyed by its
//! arguments and by the parts of its context that elaboration reads: the
//! imports, package, constant functions, active interface instances and time
//! scopes, so a change of any of them makes earlier results unreachable.
//! A repeated elaboration allocates as many call sites as the first one did,
//! so later calls keep their sites.
use super::*;

mod sites;
pub(in crate::ast) use sites::CallSites;

/// The window of the module being analyzed, if one is open.
#[derive(Default)]
pub(in crate::ast) struct State(Option<Window>);

pub(in crate::ast) struct Window {
    /// The syntax tree of the module.
    tree: usize,
    entries: Vec<Entry>,
}

struct Entry {
    /// The symbols the scope imported and the package it was in.
    imported: Arc<scope::ScopeSymbols>,
    package: Option<Arc<str>>,
    functions: const_functions::ConstantFunctions,
    active_interfaces: Arc<Vec<interface_ast::ActiveInstance>>,
    timescale: Arc<timescales::QueryContext>,
    interface_instance: Option<Arc<str>>,
    literal_use: LiteralUse,
    overrides: HashMap<String, ConstExpr>,
    env: HashMap<String, i128>,
    aliases: HashMap<String, Type>,
    items: Vec<Item>,
    /// How many call sites the elaboration allocated.
    sites: usize,
}

/// Opens a window that keeps the elaborations of `tree`, giving back the
/// window of an enclosing analysis for [`restore`].
pub(in crate::ast) fn memoize(tree: &AnalysisContext<'_>, session: &mut Session) -> Option<Window> {
    let window = Window {
        tree: address(tree.syntax_tree),
        entries: Vec::new(),
    };
    session.generate_items.0.replace(window)
}

/// Closes the window [`memoize`] opened, reopening `previous`.
pub(in crate::ast) fn restore(session: &mut Session, previous: Option<Window>) {
    session.generate_items.0 = previous;
}

fn address(tree: &SyntaxTree) -> usize {
    tree as *const SyntaxTree as usize
}

/// The items `elaborate` gives for these arguments, elaborated once per window.
pub(super) fn items(
    tree: &AnalysisContext<'_>,
    session: &mut Session,
    env: &HashMap<String, i128>,
    aliases: &HashMap<String, Type>,
    literal_use: LiteralUse,
    elaborate: impl FnOnce(&mut Session) -> Result<Vec<Item>, AnalyzerError>,
) -> Result<Vec<Item>, AnalyzerError> {
    #[cfg(test)]
    let elaborate = |session: &mut Session| {
        tests::ELABORATIONS.with(|count| count.set(count.get() + 1));
        elaborate(session)
    };
    #[cfg(test)]
    if tests::DISABLED.with(std::cell::Cell::get) {
        return elaborate(session);
    }
    let applies = |window: &Window| window.tree == address(tree.syntax_tree);
    let Some(window) = session
        .generate_items
        .0
        .as_ref()
        .filter(|window| applies(window))
    else {
        return elaborate(session);
    };
    if let Some(entry) = window.entries.iter().find(|entry| {
        Arc::ptr_eq(&entry.imported, &tree.imported)
            && entry.package == tree.package
            && entry.functions.same(&tree.functions)
            && Arc::ptr_eq(&entry.active_interfaces, &tree.active_interfaces)
            && Arc::ptr_eq(&entry.timescale, &tree.timescale)
            && entry.interface_instance == tree.interface_instance
            && entry.literal_use == literal_use
            && entry.overrides == *tree.parameter_overrides
            && entry.env == *env
            && entry.aliases == *aliases
    }) {
        let items = entry.items.clone();
        session.next_call_site += entry.sites;
        return Ok(items);
    }
    let first_site = session.next_call_site;
    let items = elaborate(session)?;
    let sites = first_site..session.next_call_site;
    // Effectful calls the items retain must get new sites when they are
    // elaborated again.
    if !sites.is_empty() && sites::refer_to(&items, sites.clone()) {
        return Ok(items);
    }
    if let Some(window) = session
        .generate_items
        .0
        .as_mut()
        .filter(|window| applies(window))
    {
        window.entries.push(Entry {
            imported: tree.imported.clone(),
            package: tree.package.clone(),
            functions: tree.functions.clone(),
            active_interfaces: tree.active_interfaces.clone(),
            timescale: tree.timescale.clone(),
            interface_instance: tree.interface_instance.clone(),
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
    use std::cell::Cell;

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
