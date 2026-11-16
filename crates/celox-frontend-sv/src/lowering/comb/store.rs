//! The symbolic store: reads and writes of the bits of a variable at the
//! current point, and the hidden flag variables of jumps.

use super::*;

impl Comb<'_, '_> {
    /// The value of bits `access` of variable `id` at this point.
    pub(super) fn read(
        &mut self,
        store: &Store,
        id: SourceVarId,
        access: BitAccess,
    ) -> Result<Value, sv::AnalyzerError> {
        let mut value = self.read_unforced(store, id, access)?;
        // Initial-state folding runs before a foreign caller can apply force.
        // Runtime initial processes are lowered separately through Ff.
        if !self.initial
            && let Some((enabled, forced)) = self.m.force_read_controls(id)
        {
            let cond = self.alloc(SLTNode::Input {
                variable: enabled,
                signed: false,
                index: vec![],
                access: full(1),
            })?;
            let replacement = self.alloc(SLTNode::Input {
                variable: forced,
                signed: false,
                index: vec![],
                access,
            })?;
            value.0 = self.alloc(SLTNode::Mux {
                cond,
                then_expr: replacement,
                else_expr: value.0,
            })?;
            value.1.insert(VarAtomBase::new(enabled, 0, 0));
            value
                .1
                .insert(VarAtomBase::new(forced, access.lsb, access.msb));
        }
        Ok(value)
    }

    pub(super) fn read_unforced(
        &mut self,
        store: &Store,
        id: SourceVarId,
        access: BitAccess,
    ) -> Result<Value, sv::AnalyzerError> {
        let signed =
            self.m.var(id).signed && access.lsb == 0 && access.msb + 1 == self.m.var(id).width;
        let Some(range) = store.get(&id) else {
            let node = self.alloc(SLTNode::Input {
                variable: id,
                signed,
                index: Vec::new(),
                access,
            })?;
            return Ok((
                node,
                [VarAtomBase::new(id, access.lsb, access.msb)]
                    .into_iter()
                    .collect(),
            ));
        };
        let parts = range.get_parts(access).map_err(range_error)?;
        let mut nodes = Vec::with_capacity(parts.len());
        let mut sources = Sources::default();
        let mut lsb = access.lsb;
        for (value, relative) in parts {
            let part_width = relative.msb - relative.lsb + 1;
            let node = match value {
                Some((node, part_sources)) => {
                    sources.extend(part_sources.iter().copied());
                    self.slice(node, relative)?
                }
                None => {
                    let part = BitAccess::new(lsb, lsb + part_width - 1);
                    sources.insert(VarAtomBase::new(id, part.lsb, part.msb));
                    self.alloc(SLTNode::Input {
                        variable: id,
                        signed: signed && part_width == access.msb - access.lsb + 1,
                        index: Vec::new(),
                        access: part,
                    })?
                }
            };
            nodes.push((node, part_width));
            lsb += part_width;
        }
        Ok((self.concat_lsb_first(nodes)?, sources))
    }

    pub(super) fn write(
        &mut self,
        store: &mut Store,
        id: SourceVarId,
        access: BitAccess,
        value: Value,
    ) -> Result<(), sv::AnalyzerError> {
        let width = self.m.var(id).width;
        let mut value = value;
        // Initial-state folding runs before a foreign caller can apply force.
        // Runtime initial processes are lowered separately through Ff.
        if !self.initial
            && let Some((enabled, forced)) = self.m.force_controls(id)
        {
            let condition = self.alloc(SLTNode::Input {
                variable: enabled,
                signed: false,
                index: vec![],
                access: full(1),
            })?;
            let replacement = self.alloc(SLTNode::Input {
                variable: forced,
                signed: false,
                index: vec![],
                access,
            })?;
            value.0 = self.alloc(SLTNode::Mux {
                cond: condition,
                then_expr: replacement,
                else_expr: value.0,
            })?;
            value.1.insert(VarAtomBase::new(enabled, 0, 0));
            value
                .1
                .insert(VarAtomBase::new(forced, access.lsb, access.msb));
        }
        debug_assert_eq!(self.width(value.0), access.msb - access.lsb + 1);
        store
            .entry(id)
            .or_insert_with(|| RangeStore::new(None, width))
            .update(access, Some(value))
            .map_err(range_error)
    }

    pub(super) fn set_flag(
        &mut self,
        store: &mut Store,
        id: SourceVarId,
        value: bool,
    ) -> Result<(), sv::AnalyzerError> {
        let node = self.constant(u64::from(value), 1)?;
        self.write(store, id, full(1), (node, Sources::default()))
    }

    /// The default value of a variable: all X for four-state, zero otherwise.
    pub(super) fn default_value(&mut self, id: SourceVarId) -> Result<NodeId, sv::AnalyzerError> {
        let var = self.m.var(id);
        let width = var.width;
        if var.is_4state && self.m.four_state {
            slt_unknown(self.arena, width)
        } else {
            self.constant(0, width)
        }
    }

    pub(super) fn flag(&mut self, purpose: &str) -> SourceVarId {
        self.m.temp(purpose, 1, false, false).0
    }
}
