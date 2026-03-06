//! Portable IR types decoupled from the Veryl analyzer.
//!
//! These types replace analyzer-internal IDs (`VarId`, `StrId`) with
//! self-contained identifiers backed by a [`StringTable`], enabling
//! serialization and caching of SLT/SIR without analyzer dependencies.
//!
//! When `serde` is added as a dependency, add `Serialize`/`Deserialize`
//! derives and `#[serde(skip)]` on `StringTable::lookup`.

#![allow(dead_code, unused_imports)]

use std::collections::BTreeSet;
use std::fmt;

use crate::HashMap;
use crate::ir::{
    AbsoluteAddrBase, GlueAddrBase, GlueBlockBase, ModuleId, RegionedAbsoluteAddrBase,
    RegionedVarAddrBase, TriggerSet, VarAtomBase,
};
use crate::logic_tree::range_store::RangeStore;
use crate::logic_tree::{LogicPath, NodeId, SLTNodeArena};

// ---------------------------------------------------------------------------
// StringTable — intern pool for variable/instance paths
// ---------------------------------------------------------------------------

/// A compact string intern table.
///
/// Strings are interned once and referred to by [`StrIdx`] elsewhere in the
/// portable IR.  The lookup map must be rebuilt after deserialization via
/// [`rebuild_lookup`](StringTable::rebuild_lookup).
#[derive(Clone, Debug)]
pub struct StringTable {
    strings: Vec<String>,
    lookup: HashMap<String, u32>,
}

impl StringTable {
    pub fn new() -> Self {
        Self {
            strings: Vec::new(),
            lookup: HashMap::default(),
        }
    }

    /// Intern a string, returning its index.  Deduplicates.
    pub fn intern(&mut self, s: &str) -> StrIdx {
        if let Some(&idx) = self.lookup.get(s) {
            return StrIdx(idx);
        }
        let idx = self.strings.len() as u32;
        self.strings.push(s.to_owned());
        self.lookup.insert(s.to_owned(), idx);
        StrIdx(idx)
    }

    /// Resolve an index back to a string slice.
    pub fn resolve(&self, idx: StrIdx) -> &str {
        &self.strings[idx.0 as usize]
    }

    /// Rebuild the lookup map (call after deserialization).
    pub fn rebuild_lookup(&mut self) {
        self.lookup.clear();
        for (i, s) in self.strings.iter().enumerate() {
            self.lookup.insert(s.clone(), i as u32);
        }
    }

    pub fn len(&self) -> usize {
        self.strings.len()
    }

    pub fn is_empty(&self) -> bool {
        self.strings.is_empty()
    }
}

impl Default for StringTable {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// StrIdx — index into StringTable
// ---------------------------------------------------------------------------

/// A lightweight handle into a [`StringTable`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StrIdx(pub u32);

impl fmt::Debug for StrIdx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "StrIdx({})", self.0)
    }
}

impl fmt::Display for StrIdx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "s{}", self.0)
    }
}

// ---------------------------------------------------------------------------
// PortableVarKind — mirrors VarKind without analyzer dependency
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PortableVarKind {
    Param,
    Const,
    Input,
    Output,
    Inout,
    Variable,
    Let,
}

impl PortableVarKind {
    pub fn is_port(&self) -> bool {
        matches!(
            self,
            PortableVarKind::Input | PortableVarKind::Output | PortableVarKind::Inout
        )
    }
}

impl fmt::Display for PortableVarKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            PortableVarKind::Param => "param",
            PortableVarKind::Const => "const",
            PortableVarKind::Input => "input",
            PortableVarKind::Output => "output",
            PortableVarKind::Inout => "inout",
            PortableVarKind::Variable => "var",
            PortableVarKind::Let => "let",
        };
        f.write_str(s)
    }
}

// ---------------------------------------------------------------------------
// PortableVariable — minimal variable metadata for IR
// ---------------------------------------------------------------------------

/// Stripped-down variable descriptor carrying only what the SLT/SIR pipeline
/// needs, with no references to analyzer-internal types.
#[derive(Clone, Debug)]
pub struct PortableVariable {
    /// Fully qualified path, interned in the StringTable.
    pub path: StrIdx,
    /// Variable kind (input/output/var/…).
    pub kind: PortableVarKind,
    /// Scalar bit width (excluding array dimensions).
    pub width: usize,
    /// Array dimensions, outermost first.  Empty for scalars.
    pub array_dims: Vec<usize>,
    /// Total bit width = width × product(array_dims).
    pub total_width: usize,
    /// Whether the variable is signed.
    pub signed: bool,
    /// Whether the variable uses 4-state logic (has X/Z).
    pub is_4state: bool,
}

impl PortableVariable {
    pub fn total_array(&self) -> usize {
        self.array_dims.iter().product::<usize>().max(1)
    }
}

// ---------------------------------------------------------------------------
// Portable address aliases — reuse generic base types with StrIdx
// ---------------------------------------------------------------------------

/// Regioned variable address using portable `StrIdx`.
pub type PortableRegionedAddr = RegionedVarAddrBase<StrIdx>;

/// Absolute address (instance + variable) using portable `StrIdx`.
pub type PortableAbsoluteAddr = AbsoluteAddrBase<StrIdx>;

/// Regioned absolute address using portable `StrIdx`.
pub type PortableRegionedAbsoluteAddr = RegionedAbsoluteAddrBase<StrIdx>;

// ---------------------------------------------------------------------------
// Portable glue types — reuse generic base types with StrIdx
// ---------------------------------------------------------------------------

/// Glue address using portable `StrIdx`.
pub type PortableGlueAddr = GlueAddrBase<StrIdx>;

/// Glue block using portable `StrIdx`.
pub type PortableGlueBlock = GlueBlockBase<StrIdx>;

// ---------------------------------------------------------------------------
// PortableSimModule — mirrors SimModule with portable addresses
// ---------------------------------------------------------------------------

use crate::HashSet;
use crate::ir::ExecutionUnit;

/// Portable symbolic store: same structure as `SymbolicStore<A>` but keyed
/// by `StrIdx` instead of `VarId`.
pub type PortableSymbolicStore =
    HashMap<StrIdx, RangeStore<Option<(NodeId, HashSet<VarAtomBase<StrIdx>>)>>>;

#[derive(Clone, Debug)]
pub struct PortableSimModule {
    pub name: StrIdx,
    pub variables: HashMap<StrIdx, PortableVariable>,
    pub eval_only_ff_blocks: HashMap<TriggerSet<StrIdx>, ExecutionUnit<PortableRegionedAddr>>,
    pub apply_ff_blocks: HashMap<TriggerSet<StrIdx>, ExecutionUnit<PortableRegionedAddr>>,
    pub eval_apply_ff_blocks: HashMap<TriggerSet<StrIdx>, ExecutionUnit<PortableRegionedAddr>>,
    pub glue_blocks: HashMap<StrIdx, Vec<PortableGlueBlock>>,
    pub comb_blocks: Vec<LogicPath<StrIdx>>,
    pub comb_boundaries: HashMap<StrIdx, BTreeSet<usize>>,
    pub arena: SLTNodeArena<StrIdx>,
    pub store: PortableSymbolicStore,
    pub reset_clock_map: HashMap<StrIdx, StrIdx>,
}

// ---------------------------------------------------------------------------
// Conversion from analyzer types
// ---------------------------------------------------------------------------

use crate::ir::{GlueAddr, GlueBlock, RegionedVarAddr, SimModule};
use veryl_analyzer::ir::{VarId, VarKind, Variable};

impl From<VarKind> for PortableVarKind {
    fn from(kind: VarKind) -> Self {
        match kind {
            VarKind::Param => PortableVarKind::Param,
            VarKind::Const => PortableVarKind::Const,
            VarKind::Input => PortableVarKind::Input,
            VarKind::Output => PortableVarKind::Output,
            VarKind::Inout => PortableVarKind::Inout,
            VarKind::Variable => PortableVarKind::Variable,
            VarKind::Let => PortableVarKind::Let,
        }
    }
}

/// Context for converting analyzer types to portable types.
pub struct PortableConversion {
    pub string_table: StringTable,
    var_id_map: HashMap<VarId, StrIdx>,
}

impl PortableConversion {
    pub fn new() -> Self {
        Self {
            string_table: StringTable::new(),
            var_id_map: HashMap::default(),
        }
    }

    /// Intern a VarId by resolving its Variable path to a string.
    pub fn intern_var(&mut self, var: &Variable) -> StrIdx {
        if let Some(&idx) = self.var_id_map.get(&var.id) {
            return idx;
        }
        let path_str = var
            .path
            .0
            .iter()
            .map(|str_id| {
                veryl_parser::resource_table::get_str_value(*str_id)
                    .unwrap()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join(".");
        let idx = self.string_table.intern(&path_str);
        self.var_id_map.insert(var.id, idx);
        idx
    }

    /// Intern a `StrId` from the analyzer's resource table.
    pub fn intern_str_id(&mut self, str_id: veryl_parser::resource_table::StrId) -> StrIdx {
        let s = veryl_parser::resource_table::get_str_value(str_id)
            .unwrap()
            .to_string();
        self.string_table.intern(&s)
    }

    /// Convert a Variable to a PortableVariable.
    ///
    /// Returns `None` if width or array dimensions cannot be resolved
    /// (contains unsized parameters).
    pub fn convert_variable(&mut self, var: &Variable) -> Option<PortableVariable> {
        let path = self.intern_var(var);
        let width = var.total_width()?;
        let array_dims: Option<Vec<usize>> = var.r#type.array.as_slice().iter().copied().collect();
        let array_dims = array_dims.unwrap_or_default();
        let total_array = array_dims.iter().product::<usize>().max(1);
        Some(PortableVariable {
            path,
            kind: var.kind.into(),
            width,
            array_dims,
            total_width: width * total_array,
            signed: var.r#type.signed,
            is_4state: var.r#type.is_4state(),
        })
    }

    /// Look up the StrIdx for a VarId that was already interned.
    pub fn get(&self, var_id: &VarId) -> Option<StrIdx> {
        self.var_id_map.get(var_id).copied()
    }

    /// Look up the StrIdx for a VarId, panicking if not found.
    fn map_var_id(&self, var_id: &VarId) -> StrIdx {
        self.var_id_map[var_id]
    }

    /// Convert a `RegionedVarAddr` to `PortableRegionedAddr`.
    fn convert_regioned_addr(&self, addr: &RegionedVarAddr) -> PortableRegionedAddr {
        RegionedVarAddrBase {
            region: addr.region,
            var_id: self.map_var_id(&addr.var_id),
        }
    }

    /// Convert a `GlueAddr` to `PortableGlueAddr`.
    fn convert_glue_addr(&self, addr: &GlueAddr) -> PortableGlueAddr {
        match addr {
            GlueAddr::Parent(v) => GlueAddrBase::Parent(self.map_var_id(v)),
            GlueAddr::Child(v) => GlueAddrBase::Child(self.map_var_id(v)),
        }
    }

    /// Convert a `GlueBlock` to `PortableGlueBlock`.
    fn convert_glue_block(&self, block: &GlueBlock) -> PortableGlueBlock {
        let convert_ports = |ports: &[(Vec<VarId>, LogicPath<GlueAddr>)]| {
            ports
                .iter()
                .map(|(var_ids, lp)| {
                    let ids = var_ids.iter().map(|v| self.map_var_id(v)).collect();
                    let new_lp = LogicPath {
                        target: VarAtomBase {
                            id: self.convert_glue_addr(&lp.target.id),
                            access: lp.target.access,
                        },
                        sources: lp
                            .sources
                            .iter()
                            .map(|s| VarAtomBase {
                                id: self.convert_glue_addr(&s.id),
                                access: s.access,
                            })
                            .collect(),
                        expr: lp.expr,
                    };
                    (ids, new_lp)
                })
                .collect()
        };

        // Convert the SLTNodeArena<GlueAddr> → SLTNodeArena<PortableGlueAddr>
        let mut new_arena = SLTNodeArena::new();
        let mut cache = HashMap::default();
        for (i, node) in block.arena.nodes.iter().enumerate() {
            let node_id = NodeId(i);
            node.map_addr(node_id, &block.arena, &mut new_arena, &mut cache, &|addr| {
                self.convert_glue_addr(addr)
            });
        }

        GlueBlockBase {
            module_id: block.module_id,
            input_ports: convert_ports(&block.input_ports),
            output_ports: convert_ports(&block.output_ports),
            arena: new_arena,
        }
    }

    /// Convert a `TriggerSet<VarId>` to `TriggerSet<StrIdx>`.
    fn convert_trigger_set(&self, ts: &TriggerSet<VarId>) -> TriggerSet<StrIdx> {
        TriggerSet {
            clock: self.map_var_id(&ts.clock),
            resets: ts.resets.iter().map(|v| self.map_var_id(v)).collect(),
        }
    }

    /// Convert an `ExecutionUnit<RegionedVarAddr>` to `ExecutionUnit<PortableRegionedAddr>`.
    fn convert_execution_unit(
        &self,
        eu: &ExecutionUnit<RegionedVarAddr>,
    ) -> ExecutionUnit<PortableRegionedAddr> {
        ExecutionUnit {
            entry_block_id: eu.entry_block_id,
            blocks: eu
                .blocks
                .iter()
                .map(|(id, bb)| {
                    let new_bb = crate::ir::BasicBlock {
                        id: bb.id,
                        params: bb.params.clone(),
                        instructions: bb
                            .instructions
                            .iter()
                            .map(|inst| inst.map_addr(|a| self.convert_regioned_addr(a)))
                            .collect(),
                        terminator: bb.terminator.clone(),
                    };
                    (*id, new_bb)
                })
                .collect(),
            register_map: eu.register_map.clone(),
        }
    }

    /// Convert a `SimModule` to a `PortableSimModule`.
    ///
    /// All `VarId`s referenced in the module (including those in glue blocks
    /// from child modules) must be interned before calling this method.
    /// Call [`intern_var`] for every variable in all referenced modules first.
    pub fn convert_sim_module(&self, module: &SimModule) -> PortableSimModule {
        // Convert name
        let name = self.intern_str_id_readonly(module.name);

        // Convert variables
        let variables = module
            .variables
            .iter()
            .filter_map(|(_, var)| {
                let path = self.map_var_id(&var.id);
                let width = var.total_width()?;
                let array_dims: Option<Vec<usize>> =
                    var.r#type.array.as_slice().iter().copied().collect();
                let array_dims = array_dims.unwrap_or_default();
                let total_array = array_dims.iter().product::<usize>().max(1);
                Some((
                    path,
                    PortableVariable {
                        path,
                        kind: var.kind.into(),
                        width,
                        array_dims,
                        total_width: width * total_array,
                        signed: var.r#type.signed,
                        is_4state: var.r#type.is_4state(),
                    },
                ))
            })
            .collect();

        // Convert FF blocks
        let convert_ff_blocks =
            |blocks: &HashMap<TriggerSet<VarId>, ExecutionUnit<RegionedVarAddr>>| {
                blocks
                    .iter()
                    .map(|(ts, eu)| {
                        (
                            self.convert_trigger_set(ts),
                            self.convert_execution_unit(eu),
                        )
                    })
                    .collect()
            };
        let eval_only_ff_blocks = convert_ff_blocks(&module.eval_only_ff_blocks);
        let apply_ff_blocks = convert_ff_blocks(&module.apply_ff_blocks);
        let eval_apply_ff_blocks = convert_ff_blocks(&module.eval_apply_ff_blocks);

        // Convert glue blocks
        let glue_blocks = module
            .glue_blocks
            .iter()
            .map(|(str_id, blocks)| {
                let key = self.intern_str_id_readonly(*str_id);
                let converted = blocks.iter().map(|b| self.convert_glue_block(b)).collect();
                (key, converted)
            })
            .collect();

        // Convert comb blocks
        let comb_blocks = module
            .comb_blocks
            .iter()
            .map(|lp| LogicPath {
                target: VarAtomBase {
                    id: self.map_var_id(&lp.target.id),
                    access: lp.target.access,
                },
                sources: lp
                    .sources
                    .iter()
                    .map(|s| VarAtomBase {
                        id: self.map_var_id(&s.id),
                        access: s.access,
                    })
                    .collect(),
                expr: lp.expr,
            })
            .collect();

        // Convert comb boundaries
        let comb_boundaries = module
            .comb_boundaries
            .iter()
            .map(|(v, bs)| (self.map_var_id(v), bs.clone()))
            .collect();

        // Convert arena
        let mut new_arena = SLTNodeArena::new();
        let mut cache = HashMap::default();
        for (i, node) in module.arena.nodes.iter().enumerate() {
            let node_id = NodeId(i);
            node.map_addr(
                node_id,
                &module.arena,
                &mut new_arena,
                &mut cache,
                &|addr| self.map_var_id(addr),
            );
        }

        // Convert symbolic store
        let store = module
            .store
            .iter()
            .map(|(var_id, rs)| {
                let key = self.map_var_id(var_id);
                let new_rs = RangeStore {
                    ranges: rs
                        .ranges
                        .iter()
                        .map(|(&lsb, (opt, width, origin))| {
                            let new_opt = opt.as_ref().map(|(node_id, deps)| {
                                let new_deps = deps
                                    .iter()
                                    .map(|va| VarAtomBase {
                                        id: self.map_var_id(&va.id),
                                        access: va.access,
                                    })
                                    .collect();
                                (*node_id, new_deps)
                            });
                            (lsb, (new_opt, *width, *origin))
                        })
                        .collect(),
                };
                (key, new_rs)
            })
            .collect();

        // Convert reset_clock_map
        let reset_clock_map = module
            .reset_clock_map
            .iter()
            .map(|(r, c)| (self.map_var_id(r), self.map_var_id(c)))
            .collect();

        PortableSimModule {
            name,
            variables,
            eval_only_ff_blocks,
            apply_ff_blocks,
            eval_apply_ff_blocks,
            glue_blocks,
            comb_blocks,
            comb_boundaries,
            arena: new_arena,
            store,
            reset_clock_map,
        }
    }

    /// Resolve a `StrId` without mutating (assumes already interned).
    fn intern_str_id_readonly(&self, str_id: veryl_parser::resource_table::StrId) -> StrIdx {
        let s = veryl_parser::resource_table::get_str_value(str_id)
            .unwrap()
            .to_string();
        self.string_table
            .lookup
            .get(&s)
            .map(|&idx| StrIdx(idx))
            .unwrap_or_else(|| panic!("StrId '{}' not interned before conversion", s))
    }

    /// Intern all variables from a set of modules.
    /// Must be called before `convert_sim_module` so that all VarIds
    /// (including cross-module references in glue blocks) are mapped.
    pub fn intern_all_modules(&mut self, modules: &HashMap<crate::ir::ModuleId, SimModule>) {
        for module in modules.values() {
            // Intern module name
            self.intern_str_id(module.name);
            // Intern all variables
            for var in module.variables.values() {
                self.intern_var(var);
            }
            // Intern glue block instance names
            for str_id in module.glue_blocks.keys() {
                self.intern_str_id(*str_id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::ModuleId;
    use crate::parser::module::ModuleParser;
    use veryl_analyzer::{
        Analyzer, Context,
        ir::{Component, Declaration, Ir},
    };
    use veryl_metadata::Metadata;
    use veryl_parser::{Parser, resource_table};

    fn parse_modules(code: &str) -> HashMap<ModuleId, SimModule> {
        let metadata = Metadata::create_default("prj").unwrap();
        let parser = Parser::parse(code, &"").unwrap();
        let analyzer = Analyzer::new(&metadata);
        let mut context = Context::default();
        let mut ir = Ir::default();

        let errors = analyzer.analyze_pass1("prj", &parser.veryl);
        assert!(errors.is_empty(), "pass1 errors: {errors:?}");
        let errors = Analyzer::analyze_post_pass1();
        assert!(errors.is_empty(), "post_pass1 errors: {errors:?}");
        let errors = analyzer.analyze_pass2("prj", &parser.veryl, &mut context, Some(&mut ir));
        assert!(errors.is_empty(), "pass2 errors: {errors:?}");
        let errors = Analyzer::analyze_post_pass2();
        assert!(errors.is_empty(), "post_pass2 errors: {errors:?}");

        let mut name_to_id: HashMap<resource_table::StrId, ModuleId> = HashMap::default();
        let mut ir_modules: HashMap<ModuleId, &veryl_analyzer::ir::Module> = HashMap::default();
        let mut next_id = 0usize;
        for component in &ir.components {
            if let Component::Module(module) = component {
                let id = ModuleId(next_id);
                next_id += 1;
                name_to_id.insert(module.name, id);
                ir_modules.insert(id, module);
            }
        }

        let mut modules: HashMap<ModuleId, SimModule> = HashMap::default();
        for (&mid, &module) in &ir_modules {
            let inst_ids: Vec<ModuleId> = module
                .declarations
                .iter()
                .filter_map(|d| match d {
                    Declaration::Inst(inst) => {
                        let child_name = match &inst.component {
                            Component::Module(m) => m.name,
                            Component::SystemVerilog(sv) => sv.name,
                            Component::Interface(_) => unreachable!(),
                        };
                        Some(name_to_id[&child_name])
                    }
                    _ => None,
                })
                .collect();
            let m = ModuleParser::parse(module, &crate::parser::BuildConfig::default(), &inst_ids)
                .expect("module parse failed");
            modules.insert(mid, m);
        }
        modules
    }

    #[test]
    fn convert_simple_comb_module() {
        let code = r#"
            module Top (
                a: input logic<8>,
                b: input logic<8>,
                out: output logic<8>,
            ) {
                always_comb {
                    out = a + b;
                }
            }
        "#;
        let modules = parse_modules(code);
        assert_eq!(modules.len(), 1);

        let mut conv = PortableConversion::new();
        conv.intern_all_modules(&modules);

        let (_, sim_module) = modules.iter().next().unwrap();
        let portable = conv.convert_sim_module(sim_module);

        // Verify variables were converted
        assert_eq!(portable.variables.len(), sim_module.variables.len());

        // Verify each variable has correct width
        for pv in portable.variables.values() {
            let name = conv.string_table.resolve(pv.path);
            if name == "a" || name == "b" || name == "out" {
                assert_eq!(pv.width, 8, "variable {} should be 8-bit", name);
            }
        }

        // Verify comb blocks
        assert_eq!(portable.comb_blocks.len(), sim_module.comb_blocks.len());

        // Verify arena nodes were converted
        assert_eq!(portable.arena.nodes.len(), sim_module.arena.nodes.len());

        // Verify store entries
        assert_eq!(portable.store.len(), sim_module.store.len());
    }

    #[test]
    fn convert_ff_module() {
        let code = r#"
            module Top (
                clk: input '_ clock,
                rst: input '_ reset,
                d: input logic<8>,
                q: output logic<8>,
            ) {
                always_ff (clk, rst) {
                    if_reset {
                        q = '0;
                    } else {
                        q = d;
                    }
                }
            }
        "#;
        let modules = parse_modules(code);
        assert_eq!(modules.len(), 1);

        let mut conv = PortableConversion::new();
        conv.intern_all_modules(&modules);

        let (_, sim_module) = modules.iter().next().unwrap();
        let portable = conv.convert_sim_module(sim_module);

        // FF module should have FF blocks
        let total_ff_blocks = portable.eval_apply_ff_blocks.len()
            + portable.eval_only_ff_blocks.len()
            + portable.apply_ff_blocks.len();
        assert!(total_ff_blocks > 0, "should have FF blocks");

        // Verify reset_clock_map
        assert_eq!(
            portable.reset_clock_map.len(),
            sim_module.reset_clock_map.len()
        );
    }

    #[test]
    fn convert_hierarchical_module() {
        let code = r#"
            module Child (
                x: input logic<4>,
                y: output logic<4>,
            ) {
                always_comb { y = x; }
            }
            module Top (
                a: input logic<4>,
                b: output logic<4>,
            ) {
                inst u_child: Child (
                    x: a,
                    y: b,
                );
            }
        "#;
        let modules = parse_modules(code);
        assert_eq!(modules.len(), 2);

        let mut conv = PortableConversion::new();
        conv.intern_all_modules(&modules);

        // Convert both modules
        for sim_module in modules.values() {
            let portable = conv.convert_sim_module(sim_module);
            assert!(!portable.variables.is_empty());
        }

        // Verify string table has entries for all variables
        assert!(!conv.string_table.is_empty());
    }

    #[test]
    fn string_table_deduplication() {
        let mut table = StringTable::new();
        let idx1 = table.intern("clk");
        let idx2 = table.intern("clk");
        let idx3 = table.intern("rst");

        assert_eq!(idx1, idx2, "same string should return same index");
        assert_ne!(
            idx1, idx3,
            "different strings should return different indices"
        );
        assert_eq!(table.len(), 2);
        assert_eq!(table.resolve(idx1), "clk");
        assert_eq!(table.resolve(idx3), "rst");
    }

    #[test]
    fn string_table_rebuild_lookup() {
        let mut table = StringTable::new();
        let idx = table.intern("test");

        // Clear and rebuild
        table.lookup.clear();
        assert!(table.lookup.is_empty());
        table.rebuild_lookup();
        assert!(!table.lookup.is_empty());

        // Re-intern should return same index
        let idx2 = table.intern("test");
        assert_eq!(idx, idx2);
    }
}
