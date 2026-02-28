use crate::{HashMap, ir::VarAtom, parser::bitaccess::eval_var_select};
use std::collections::BTreeSet;
use veryl_analyzer::ir::{
    AssignDestination, AssignStatement, Declaration, Module, Statement, VarId,
};

pub struct BitSlicer {
    // Set of "boundaries" for each variable (e.g., [0, 8, 16] means ranges 0-7, 8-15)
    boundaries: HashMap<VarId, BTreeSet<usize>>,
}

pub fn new_atom(module: &Module, dst: &AssignDestination) -> VarAtom {
    let access = eval_var_select(module, dst.id, &dst.index, &dst.select);
    VarAtom { id: dst.id, access }
}
impl BitSlicer {
    pub fn new(module: &Module) -> Self {
        let mut slicer = Self {
            boundaries: HashMap::default(),
        };

        for (id, var) in &module.variables {
            if let Some(w) = var.total_width() {
                slicer.add_boundary(*id, 0);
                slicer.add_boundary(*id, w);
            }
        }

        for decl in &module.declarations {
            slicer.scan_declaration(module, decl);
        }

        slicer
    }

    fn add_boundary(&mut self, id: VarId, bit: usize) {
        self.boundaries.entry(id).or_default().insert(bit);
    }

    fn scan_assign(&mut self, module: &Module, assign: &AssignStatement) {
        for dst in &assign.dst {
            let range = self.calculate_dst_range(module, dst);
            self.add_boundary(dst.id, range.access.lsb); // lsb
            self.add_boundary(dst.id, range.access.msb + 1); // msb + 1
        }
    }

    pub fn boundaries(&self) -> &HashMap<VarId, BTreeSet<usize>> {
        &self.boundaries
    }

    fn scan_declaration(&mut self, module: &Module, decl: &Declaration) {
        if let Declaration::Comb(comb) = decl {
            for stmt in &comb.statements {
                if let Statement::Assign(assign) = stmt {
                    self.scan_assign(module, assign);
                }
            }
        }
    }

    fn calculate_dst_range(
        &self,
        module: &Module,
        dst: &veryl_analyzer::ir::AssignDestination,
    ) -> VarAtom {
        new_atom(module, dst)
    }
}
