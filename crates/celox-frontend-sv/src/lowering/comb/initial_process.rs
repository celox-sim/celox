//! The execution of an `initial` block into the initial state it defines.

use super::*;

impl Comb<'_, '_> {
    /// Execute an `initial` block and return the initial state it defines.
    /// Its writes must have constant values.
    /// The initial state an `initial` process, or the variable declaration
    /// initializers when `initializer` is set, define.
    ///
    /// The block starts from `seed`, the values the declaration initializers
    /// before it define; only the values it changes are returned, together
    /// with its final store.
    pub fn lower_initial(
        &mut self,
        body: &[sv::ir::Stmt],
        initializer: bool,
        seed: &Store,
    ) -> Result<(Vec<InitialStateValue<SourceVarId>>, Store), sv::AnalyzerError> {
        self.initial = true;
        self.allow_nonblocking = true;
        self.initial_seed = seed.fork();
        let store = self.exec_block(seed.fork(), &[], body)?;
        // The memory files the block loads, in order, before the values it
        // writes after them; the words they load are not written again.
        let loads = std::mem::take(&mut self.memory_loads);
        let loaded = |id: SourceVarId, node: NodeId| {
            loads
                .iter()
                .any(|(load_id, _, loaded)| *load_id == id && *loaded == Some(node))
        };
        // A value is unchanged when the seed holds the same node at the same
        // origin over its bits.
        let unchanged = |id: SourceVarId, lsb: usize, width: usize, node: NodeId, origin: usize| {
            seed.get(&id).is_some_and(|range| {
                range
                    .ranges
                    .iter()
                    .any(|(&seed_lsb, (value, seed_width, seed_origin))| {
                        value
                            .as_ref()
                            .is_some_and(|(seed_node, _)| *seed_node == node)
                            && *seed_origin == origin
                            && seed_lsb <= lsb
                            && lsb + width <= seed_lsb + seed_width
                    })
            })
        };
        let mut ids: Vec<SourceVarId> = store
            .keys()
            .copied()
            .filter(|id| !self.m.is_hidden(*id))
            .collect();
        ids.sort_by_key(|id| id.0);
        let mut values: Vec<_> = loads
            .iter()
            .map(|(id, data, _)| InitialStateValue {
                address: *id,
                data: data.clone(),
            })
            .collect();
        for id in ids {
            let Some(range) = store.get(&id) else {
                continue;
            };
            let mut runs = Vec::new();
            for (&lsb, (value, width, origin)) in &range.ranges {
                let Some((node, _)) = value else { continue };
                if unchanged(id, lsb, *width, *node, *origin) || loaded(id, *node) {
                    continue;
                }
                let (value, mask, _) =
                    slt_const4(self.arena, &mut self.consts, *node).ok_or_else(|| {
                        let name = self.m.var(id).path.join(".");
                        unsupported(if initializer {
                            format!("declaration initializer of `{name}` that is not constant")
                        } else {
                            format!("initial block value of `{name}` that depends on design state")
                        })
                    })?;
                let shift = lsb - origin;
                let keep = (BigUint::from(1u8) << *width) - BigUint::from(1u8);
                let value = (value >> shift) & &keep;
                let mask = (mask >> shift) & &keep;
                runs.push(celox_design::InitialStateWriteRun {
                    bit_offset: lsb,
                    bit_width: *width,
                    value_bytes: celox_frontend_core::memory_file::biguint_to_fixed_le_bytes(
                        &value, *width,
                    ),
                    mask_bytes: celox_frontend_core::memory_file::biguint_to_fixed_le_bytes(
                        &mask, *width,
                    ),
                });
            }
            if !runs.is_empty() {
                values.push(InitialStateValue {
                    address: id,
                    data: InitialStateData::Writes(runs),
                });
            }
        }
        Ok((values, store))
    }
}
