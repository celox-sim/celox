use crate::HashMap;
use crate::ir::{InstanceId, InstancePath, ModuleId, SourceAddr, SourceVarId};
use celox_slt::SLTNodeArena;
use veryl_analyzer::{Analyzer, Context, ir::Ir};
use veryl_metadata::Metadata;
use veryl_parser::{Parser, resource_table};

fn setup_to_flatting(
    code: &str,
    top_name: &str,
) -> (
    Vec<celox_slt::LogicPath<SourceAddr>>,
    HashMap<ModuleId, crate::ir::SimModule>,
    SLTNodeArena<SourceAddr>,
) {
    let metadata = Metadata::create_default("prj").unwrap();
    let parser = Parser::parse(code, &"").unwrap();
    let analyzer = Analyzer::new(&metadata);
    let mut context = Context::default();
    let mut ir = Ir::default();

    let errors = analyzer.analyze_pass1("prj", &parser.veryl);
    assert!(errors.is_empty(), "analyze_pass1 errors: {errors:?}");
    let errors = Analyzer::analyze_post_pass1();
    assert!(errors.is_empty(), "analyze_post_pass1 errors: {errors:?}");
    let errors = analyzer.analyze_pass2(&parser.veryl, &mut context, Some(&mut ir));
    assert!(errors.is_empty(), "analyze_pass2 errors: {errors:?}");
    let errors = Analyzer::analyze_post_pass2(&ir);
    assert!(errors.is_empty(), "analyze_post_pass2 errors: {errors:?}");

    let top_id = resource_table::insert_str(top_name);

    let parsed =
        celox_frontend_veryl::parse_ir(&ir, &crate::parser::BuildConfig::default(), &top_id)
            .expect("hierarchy parse failed");
    let mut trace = celox_frontend_core::FrontendTrace::default();
    celox_frontend_veryl::schedule_symbolic_rtl(
        parsed,
        &crate::parser::BuildConfig::default(),
        &[],
        &[],
        false,
        &celox_frontend_core::ParallelScheduleOptions::default(),
        &celox_frontend_core::FrontendTraceOptions {
            sim_modules: true,
            atomized_comb_blocks: true,
            ..Default::default()
        },
        Some(&mut trace),
    )
    .expect("hierarchy scheduling failed");
    let (paths, arena) = trace.atomized_comb_blocks.unwrap();
    (paths, trace.sim_modules.unwrap(), arena)
}

#[test]
fn test_split_by_boundaries() {
    let code = r#"
    module Top (
        a: input logic<32>,
        b: output logic<32>,
    ) {
        var x: logic<32>;
        
        // Split x into [0..15] and [16..31] implicitly by access
        assign x[15:0] = a[15:0];
        assign x[31:16] = a[31:16];
        assign b = x;
    }
    "#;

    let (comb_blocks, modules, _arena) = setup_to_flatting(code, "Top");

    // Find x logic paths
    let top_vars = &modules
        .values()
        .find(|module| module.name == "Top")
        .expect("Top module not found")
        .variables;
    // We can filter by seeing if var path implies "x"
    // But since we have only one internal variable x, it should be easy.
    // Wait, parsing output variables might not be easy to destinguish from inputs/outputs by name unless we check Variable struct.

    // x is a VarKind::Local usually? Or just check if name is "x".
    // Variable struct has `path: VarPath`.

    let x_id = top_vars
        .iter()
        .find(|(_, v)| v.path.len() == 1 && v.path[0] == "x")
        .map(|(id, _)| *id)
        .expect("Variable x not found");

    let x_targets: Vec<_> = comb_blocks
        .iter()
        .filter(|path| path.target.var().unwrap().id.var_id == x_id)
        .collect();

    // Should be 2 paths: 2 real assignments. Identity assignments are no longer generated.
    assert_eq!(
        x_targets.len(),
        2,
        "x should be split into 2 atomic assignments"
    );

    // Verify ranges
    let ranges: Vec<_> = x_targets
        .iter()
        .map(|p| {
            (
                p.target.var().unwrap().access.lsb,
                p.target.var().unwrap().access.msb,
            )
        })
        .collect();
    // Use contains to avoid ordering issues, or sort
    assert!(ranges.contains(&(0, 15)), "Missing range 0..15");
    assert!(ranges.contains(&(16, 31)), "Missing range 16..31");
}

#[test]
fn test_mixed_boundaries() {
    let code = r#"
    module Top (
        a: input logic<32>,
        b: output logic<32>,
    ) {
        var x: logic<32>;
        always_comb {
            x = a;
            // Static access at bit 15
            x[15] = 1'b0;
            b = x;
        }
    }
    "#;

    let (comb_blocks, modules, _arena) = setup_to_flatting(code, "Top");

    let x_id = modules
        .values()
        .find(|module| module.name == "Top")
        .expect("Top module not found")
        .variables
        .iter()
        .find(|(_, v)| v.path.len() == 1 && v.path[0] == "x")
        .map(|(id, _)| *id)
        .expect("Variable x not found");

    let x_targets: Vec<_> = comb_blocks
        .iter()
        .filter(|path| path.target.var().unwrap().id.var_id == x_id)
        .collect();

    // Boundaries: {0, 15, 16, 32} -> 3 atoms [0..14], [15..15], [16..31]
    // always_comb merges assignments into final state. Total 3 paths.
    assert_eq!(
        x_targets.len(),
        3,
        "Mixed boundaries should split merged variable into 3 parts (one for each atom)"
    );

    let ranges: Vec<_> = x_targets
        .iter()
        .map(|p| {
            (
                p.target.var().unwrap().access.lsb,
                p.target.var().unwrap().access.msb,
            )
        })
        .collect();
    assert!(ranges.contains(&(0, 14)));
    assert!(ranges.contains(&(15, 15)));
    assert!(ranges.contains(&(16, 31)));
}

fn setup_and_parse(code: &str, top_name: &str) -> crate::ir::UnoptimizedSir {
    let metadata = Metadata::create_default("prj").unwrap();
    let parser = Parser::parse(code, &"").unwrap();
    let analyzer = Analyzer::new(&metadata);
    let mut context = Context::default();
    let mut ir = Ir::default();

    let errors = analyzer.analyze_pass1("prj", &parser.veryl);
    assert!(errors.is_empty(), "analyze_pass1 errors: {errors:?}");
    let errors = Analyzer::analyze_post_pass1();
    assert!(errors.is_empty(), "analyze_post_pass1 errors: {errors:?}");
    let errors = analyzer.analyze_pass2(&parser.veryl, &mut context, Some(&mut ir));
    assert!(errors.is_empty(), "analyze_pass2 errors: {errors:?}");
    let errors = Analyzer::analyze_post_pass2(&ir);
    assert!(errors.is_empty(), "analyze_post_pass2 errors: {errors:?}");

    let top_id = resource_table::insert_str(top_name);

    // Use the real parser::parse_ir and flatten, but SKIP optimization to verify structure
    // crate::parser::parse(&top_id, &ir).expect("Failed to parse program")
    let build_config = crate::parser::BuildConfig::default();
    let result = crate::parser::parse_ir(&ir, &build_config, &top_id).expect("Failed to parse IR");
    let scheduled = celox_frontend_veryl::schedule_symbolic_rtl(
        result,
        &build_config,
        &[],
        &[],
        false,
        &celox_frontend_core::ParallelScheduleOptions::default(),
        &celox_frontend_core::FrontendTraceOptions::default(),
        None,
    )
    .expect("Failed to flatten");
    let (sir, runtime) = crate::ir::RuntimeProgram::from_scheduled(scheduled.scheduled).unwrap();
    crate::ir::UnoptimizedSir::new(sir, runtime)
}

#[test]
fn test_instances_inherit_module_boundaries() {
    let code = r#"
    module Child (
        a: input logic<32>,
        b: output logic<32>,
    ) {
        var x: logic<32>;
        always_comb {
            x = a;
            // Split x at 16 inside Child
            x[15] = 1'b0;
        }
        assign b = x;
    }
    
    module Top (
        val: input logic<32>,
        out1: output logic<32>,
        out2: output logic<32>,
    ) {
        inst c1: Child (
            a: val,
            b: out1,
        );
        inst c2: Child (
            a: val,
            b: out2,
        );
    }
    "#;

    let program = setup_and_parse(code, "Top");

    // Helper to find instance IDs
    let c1_path = InstancePath(vec![("c1".to_string(), 0)]);
    let c2_path = InstancePath(vec![("c2".to_string(), 0)]);

    let c1 = program
        .design
        .instance_at_path(&c1_path)
        .expect("c1 instance not found");
    let c2 = program
        .design
        .instance_at_path(&c2_path)
        .expect("c2 instance not found");

    // Find the normalized runtime variable for 'x' in Child.
    let x = c1
        .state_addresses()
        .iter()
        .filter_map(|address| program.design.variable(address))
        .find(|variable| variable.path.as_slice() == ["x"])
        .unwrap();
    let x_id = x.source_id;

    // Verify that we actually have different instance IDs in the paths
    let c1_x_stores = find_stores_to_var(&program, c1.id, x_id);
    let c2_x_stores = find_stores_to_var(&program, c2.id, x_id);

    // We expect split stores.
    // Since we can't easily count exact atoms without knowing how scheduler optimizes,
    // let's check that we have multiple stores for the same variable (indicating splitting)
    // or checks the sizes.
    // Child: x[15]=0 (size 1), x=a (size 32 originally, but split).
    // If x is split at [0..14], [15..15], [16..31].
    // x=a writes to [0..14] (size 15), [15..15] (size 1), [16..31] (size 16).
    // x[15]=0 writes to [15..15] (size 1).
    // So we expect stores of size 15, 1, 16.

    let sizes_c1: Vec<_> = c1_x_stores.iter().map(|s| s.bits).collect();
    assert!(
        sizes_c1.contains(&15),
        "Missing store of size 15 in c1.x. Found: {:?}",
        sizes_c1
    );
    assert!(
        sizes_c1.contains(&16),
        "Missing store of size 16 in c1.x. Found: {:?}",
        sizes_c1
    );
    // Size 1 might appear multiple times

    let sizes_c2: Vec<_> = c2_x_stores.iter().map(|s| s.bits).collect();
    assert!(
        sizes_c2.contains(&15),
        "Missing store of size 15 in c2.x. Found: {:?}",
        sizes_c2
    );
    assert!(
        sizes_c2.contains(&16),
        "Missing store of size 16 in c2.x. Found: {:?}",
        sizes_c2
    );
}

#[test]
fn test_boundary_propagation() {
    let code = r#"
    module Child (
        b: input logic<32>,
    ) {
    }
    
    module Top (
        out: output logic<16>,
    ) {
        var v: logic<32>;
        
        inst c1: Child (
            b: v,
        );
        
        // Force boundary on v by assigning to it in slices.
        // v has boundaries {0, 16, 32}.
        // These boundaries should propagate to c1.b (Input port).
        always_comb {
            v[15:0] = 16'hAAAA;
            v[31:16] = 16'hBBBB;
            out = v[15:0];
        }
    }
    "#;

    let (comb_blocks, modules, _arena) = setup_to_flatting(code, "Top");

    let b_id = modules
        .values()
        .find(|m| m.name == "Child")
        .expect("Child module not found")
        .variables
        .iter()
        .find(|(_, v)| v.path.len() == 1 && v.path[0] == "b")
        .map(|(id, _)| *id)
        .expect("Variable b not found");

    let b_targets: Vec<_> = comb_blocks
        .iter()
        .filter(|path| {
            let addr = path.target.var().unwrap().id;
            addr.instance_id != InstanceId(0) && addr.var_id == b_id
        })
        .collect();

    // Check stores to c1.b (Input port, driven by Parent)
    // We expect stores of size 16 (0..15) and 16 (16..31) because boundaries propagated from v.
    let sizes: Vec<_> = b_targets
        .iter()
        .map(|p| p.target.var().unwrap().access.msb - p.target.var().unwrap().access.lsb + 1)
        .collect();

    // c1.b should be split because boundary propagated from v
    assert!(
        sizes.contains(&16),
        "Missing store of size 16 for Child.b (Propagated boundary). Found: {:?}",
        sizes
    );
    assert!(
        !sizes.contains(&32),
        "Should strictly split 32-bit assignment. Found 32-bit store: {:?}",
        sizes
    );
}

struct StoreInfo {
    bits: usize,
}

fn find_stores_to_var(
    program: &crate::ir::UnoptimizedSir,
    instance_id: crate::ir::InstanceId,
    var_id: SourceVarId,
) -> Vec<StoreInfo> {
    let expected = program
        .design
        .instance_variable(instance_id, var_id)
        .expect("runtime state projection is complete")
        .address;
    let mut stores = Vec::new();
    for unit in &program.sir.updates.observe {
        for block in unit.blocks.values() {
            for inst in &block.instructions {
                if let crate::ir::SIRInstruction::Store(addr, _, bits, _, _, _) = inst {
                    if addr.absolute_addr() == expected {
                        stores.push(StoreInfo { bits: *bits });
                    }
                }
            }
        }
    }
    stores
}

#[test]
fn test_assign_partial_no_cycle() {
    // Test: Confirm that no cycles are detected when two independent assign statements
    // assign different bit ranges of the same variable.
    let code = r#"

    module Top (
    ) {
        var v: logic<32>;

        // Two independent assign statements
        // These will be separate CombDeclarations, but since they are different bit ranges, it should not be a cycle.
        assign v[15:0] = 16'hAAAA;
        assign v[31:16] = 16'hBBBB;
    }
    "#;

    let program = setup_and_parse(code, "Top");

    assert!(!program.sir.updates.observe.is_empty());
}
