// Regression test for duplicate VarPath crash.
//
// When multiple scoped variables share the same VarPath (e.g., `var avail: logic;`
// declared in different `for` loop scopes within the same `always_comb`), the old
// `module_variables` implementation used VarPath as the HashMap key, silently
// overwriting entries and losing VarIds. This caused a "no entry found for key"
// panic in the Cranelift translator's memory layout lookup.
//
// The fix uses VarId as the primary key and maintains a separate path index that
// detects ambiguous paths.

#[path = "test_utils/mod.rs"]
#[macro_use]
#[allow(unused_macros)]
mod test_utils;

all_backends! {

// Minimal reproduction: two `for` loops in `always_comb` each declare `var tmp: logic`.
// The Veryl analyzer assigns different VarIds but identical VarPaths to these scoped
// variables. Without the fix this panics during JIT compilation.
fn test_duplicate_scoped_var_in_always_comb(sim) {
    @ignore_on(sv);
    @case "duplicate_varpath::test_duplicate_scoped_var_in_always_comb";
}

// Generate-for with same-named scoped variables in always_comb.
// Mirrors the original AdcGroup MRE: generate-for creates instances with
// internal vars, and always_comb inside uses `var flag: logic;` in multiple
// for-loop scopes.
fn test_duplicate_scoped_var_with_generate_for(sim) {
    @ignore_on(sv);
    @case "duplicate_varpath::test_duplicate_scoped_var_with_generate_for";
}

}
