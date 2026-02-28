use crate::HashMap;
use crate::flatting;
use crate::ir::{AbsoluteAddr, InstanceId, InstancePath};
use crate::logic_tree::SLTNodeArena;
use crate::parser::module::ModuleParser;
use crate::parser::registry::ModuleRegistry;
use veryl_analyzer::{
    Analyzer, Context,
    ir::{Component, Ir, VarId},
};
use veryl_metadata::Metadata;
use veryl_parser::{Parser, resource_table};

fn setup_to_flatting(
    code: &str,
    top_name: &str,
) -> (
    crate::ir::RelocationModule,
    HashMap<veryl_parser::resource_table::StrId, crate::ir::SimModule>,
    crate::logic_tree::SLTNodeArena<crate::ir::AbsoluteAddr>,
) {
    let metadata = Metadata::create_default("prj").unwrap();
    let parser = Parser::parse(&code, &"").unwrap();
    let analyzer = Analyzer::new(&metadata);
    let mut context = Context::default();
    let mut ir = Ir::default();

    analyzer.analyze_pass1(&"prj", &parser.veryl);
    Analyzer::analyze_post_pass1();
    analyzer.analyze_pass2(&"prj", &parser.veryl, &mut context, Some(&mut ir));
    Analyzer::analyze_post_pass2();

    let top_id = resource_table::insert_str(top_name);

    // Mock parse_ir logic
    let mut module_registry = ModuleRegistry {
        modules: HashMap::default(),
    };
    for component in &ir.components {
        if let Component::Module(module) = component {
            module_registry.modules.insert(module.name, module);
        }
    }

    let mut modules = HashMap::default();
    for component in &ir.components {
        if let Component::Module(module) = component {
            // Need to handle potential errors in real code, but unwrap/panic is fine for tests?
            // ModuleParser::parse returns SimModule directly.
            let m = ModuleParser::parse(module, &module_registry).expect("module parse failed");
            modules.insert(m.name, m);
        }
    }

    // Prepare for flatting
    let instance_id = InstanceId(0);
    let path = InstancePath(vec![]);
    let mut instance_ids = HashMap::default();
    instance_ids.insert(path.clone(), instance_id);

    let sim_module = &modules[&top_id];

    // Assign IDs to children (simple 1-level support for tests)
    let mut next_instance_id = 1;
    let mut glue_instance_map = HashMap::default(); // StrId -> InstanceId

    for inst_name in sim_module.glue_blocks.keys() {
        let mut child_path = path.0.clone();
        child_path.push((*inst_name, 0));
        let child_id = InstanceId(next_instance_id);
        instance_ids.insert(InstancePath(child_path), child_id);
        glue_instance_map.insert(*inst_name, child_id);
        next_instance_id += 1;
    }

    // Calculate global boundaries for the top module
    let mut global_boundaries = HashMap::default();

    // 1. Local boundaries of Top
    for (var_id, boundaries) in &sim_module.comb_boundaries {
        let addr = AbsoluteAddr {
            instance_id,
            var_id: *var_id,
        };
        global_boundaries.insert(addr, boundaries.clone());
    }

    // 2. Propagate to children (Input Ports)
    let mut new_child_boundaries = HashMap::default();

    for (inst_name, glues) in &sim_module.glue_blocks {
        for glue in glues {
            let child_id = glue_instance_map[inst_name];

            for (_, logic_path) in &glue.input_ports {
                // logic_path.target is GlueAddr::Child
                let target_glue_addr = logic_path.target.id;
                let target_addr = if let crate::ir::GlueAddr::Child(v) = target_glue_addr {
                    AbsoluteAddr {
                        instance_id: child_id,
                        var_id: v,
                    }
                } else {
                    continue;
                };

                for source in &logic_path.sources {
                    // source.id is GlueAddr::Parent usually
                    if let crate::ir::GlueAddr::Parent(parent_var) = source.id {
                        let parent_addr = AbsoluteAddr {
                            instance_id,
                            var_id: parent_var,
                        };
                        if let Some(bounds) = global_boundaries.get(&parent_addr) {
                            use std::ops::Bound::*;
                            // Check which bounds fall into source access
                            // BitAccess::calculate_atoms uses range((Excluded(self.lsb), Included(self.msb)))
                            for &bound in bounds
                                .range((Excluded(source.access.lsb), Included(source.access.msb)))
                            {
                                let offset = bound - source.access.lsb;
                                let target_bound = logic_path.target.access.lsb + offset;

                                new_child_boundaries
                                    .entry(target_addr)
                                    .or_insert_with(std::collections::BTreeSet::new)
                                    .insert(target_bound);
                            }
                        }
                    }
                }
            }
        }
    }

    // Merge new boundaries
    for (addr, bounds) in new_child_boundaries {
        global_boundaries.entry(addr).or_default().extend(bounds);
    }

    // Call flatting
    let mut arena = SLTNodeArena::<AbsoluteAddr>::new();
    let r = flatting::flatting(
        sim_module,
        &path,
        &instance_ids,
        &global_boundaries,
        &mut arena,
        &crate::debug::TraceOptions::default(),
        None,
    );
    (r, modules, arena)
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

    let (relocation_module, _, _arena) = setup_to_flatting(code, "Top");

    // Find x logic paths
    let top_vars = &relocation_module.variables;
    // We can filter by seeing if var path implies "x"
    // But since we have only one internal variable x, it should be easy.
    // Wait, parsing output variables might not be easy to destinguish from inputs/outputs by name unless we check Variable struct.

    // x is a VarKind::Local usually? Or just check if name is "x".
    // Variable struct has `path: VarPath`.

    let x_id = top_vars
        .iter()
        .find(|(_, v)| v.path.0.len() == 1 && v.path.0[0] == resource_table::insert_str("x"))
        .map(|(id, _)| *id)
        .expect("Variable x not found");

    let x_targets: Vec<_> = relocation_module
        .comb_blocks
        .iter()
        .filter(|path| path.target.id.var_id == x_id)
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
        .map(|p| (p.target.access.lsb, p.target.access.msb))
        .collect();
    // Use contains to avoid ordering issues, or sort
    assert!(ranges.contains(&(0, 15)), "Missing range 0..15");
    assert!(ranges.contains(&(16, 31)), "Missing range 16..31");
}

#[test]
fn test_dynamic_index_no_split() {
    let code = r#"
    module Top (
        a: input logic<32>,
        i: input logic<5>,
        b: output logic<32>,
    ) {
        var x: logic<32>;
        
        assign x = a; 
        assign x[i] = 1'b1;
        assign b = x;
    }
    "#;

    let (relocation_module, _, _arena) = setup_to_flatting(code, "Top");

    let x_id = relocation_module
        .variables
        .iter()
        .find(|(_, v)| v.path.0.len() == 1 && v.path.0[0] == resource_table::insert_str("x"))
        .map(|(id, _)| *id)
        .expect("Variable x not found");

    let x_targets: Vec<_> = relocation_module
        .comb_blocks
        .iter()
        .filter(|path| path.target.id.var_id == x_id)
        .collect();

    // 2 assignments (x=a, x[i]=1) -> 2 paths. No limit boundaries -> No splitting.
    // So 2 paths total.
    assert_eq!(
        x_targets.len(),
        2,
        "Dynamic indexing should not induce splitting (2 original paths preserved)"
    );
    assert_eq!(
        x_targets[0].target.access.msb - x_targets[0].target.access.lsb + 1,
        32
    );
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

    let (relocation_module, _, _arena) = setup_to_flatting(code, "Top");

    let x_id = relocation_module
        .variables
        .iter()
        .find(|(_, v)| v.path.0.len() == 1 && v.path.0[0] == resource_table::insert_str("x"))
        .map(|(id, _)| *id)
        .expect("Variable x not found");

    let x_targets: Vec<_> = relocation_module
        .comb_blocks
        .iter()
        .filter(|path| path.target.id.var_id == x_id)
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
        .map(|p| (p.target.access.lsb, p.target.access.msb))
        .collect();
    assert!(ranges.contains(&(0, 14)));
    assert!(ranges.contains(&(15, 15)));
    assert!(ranges.contains(&(16, 31)));
}

fn setup_and_parse(code: &str, top_name: &str) -> crate::ir::Program {
    let metadata = Metadata::create_default("prj").unwrap();
    let parser = Parser::parse(&code, &"").unwrap();
    let analyzer = Analyzer::new(&metadata);
    let mut context = Context::default();
    let mut ir = Ir::default();

    analyzer.analyze_pass1(&"prj", &parser.veryl);
    Analyzer::analyze_post_pass1();
    analyzer.analyze_pass2(&"prj", &parser.veryl, &mut context, Some(&mut ir));
    Analyzer::analyze_post_pass2();

    let top_id = resource_table::insert_str(top_name);

    // Use the real parser::parse_ir and flatten, but SKIP optimization to verify structure
    // crate::parser::parse(&top_id, &ir).expect("Failed to parse program")
    let (registry, sim_modules) = crate::parser::parse_ir(&ir).expect("Failed to parse IR");
    crate::parser::flatten(
        &top_id,
        &registry,
        sim_modules,
        &[],
        &[],
        false,
        &crate::debug::TraceOptions::default(),
        None,
    )
    .expect("Failed to flatten")
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
    let c1_path = InstancePath(vec![(resource_table::insert_str("c1"), 0)]);
    let c2_path = InstancePath(vec![(resource_table::insert_str("c2"), 0)]);

    let c1_id = program
        .instance_ids
        .get(&c1_path)
        .expect("c1 instance not found");
    let c2_id = program
        .instance_ids
        .get(&c2_path)
        .expect("c2 instance not found");

    // Find VarId for 'x' in Child module
    let child_name = resource_table::insert_str("Child");
    let child_vars = &program.module_variables[&child_name];
    let x_info = child_vars
        .iter()
        .find(|(path, _)| path.0.len() == 1 && path.0[0] == resource_table::insert_str("x"))
        .unwrap()
        .1;
    let x_id = x_info.id;

    // Verify that we actually have different instance IDs in the paths
    let c1_x_stores = find_stores_to_var(&program, *c1_id, x_id);
    let c2_x_stores = find_stores_to_var(&program, *c2_id, x_id);

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
        }
    }
    "#;

    let (relocation_module, modules, _arena) = setup_to_flatting(code, "Top");

    let b_id = modules[&resource_table::insert_str("Child")]
        .variables
        .iter()
        .find(|(_, v)| v.path.0.len() == 1 && v.path.0[0] == resource_table::insert_str("b"))
        .map(|(id, _)| *id)
        .expect("Variable b not found");

    let b_targets: Vec<_> = relocation_module
        .comb_blocks
        .iter()
        .filter(|path| path.target.id.var_id == b_id)
        .collect();

    // Check stores to c1.b (Input port, driven by Parent)
    // We expect stores of size 16 (0..15) and 16 (16..31) because boundaries propagated from v.
    let sizes: Vec<_> = b_targets
        .iter()
        .map(|p| p.target.access.msb - p.target.access.lsb + 1)
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
    program: &crate::ir::Program,
    instance_id: crate::ir::InstanceId,
    var_id: VarId,
) -> Vec<StoreInfo> {
    let mut stores = Vec::new();
    for unit in &program.eval_comb {
        for block in unit.blocks.values() {
            for inst in &block.instructions {
                if let crate::ir::SIRInstruction::Store(addr, _, bits, _, _) = inst {
                    if addr.instance_id == instance_id && addr.var_id == var_id {
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

    assert!(program.eval_comb.len() > 0);
}
