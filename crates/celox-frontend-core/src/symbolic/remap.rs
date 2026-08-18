use std::{fmt, hash::Hash};

use celox_slt::{GlueAddrBase, GlueBlockBase, NodeId, SLTNodeArena, SLTNodeFactsError};

use crate::HashMap;

/// Remap every frontend-local identity in a symbolic instance glue block.
pub fn glue_block<A, B>(
    block: &GlueBlockBase<A>,
    parent_ids: &HashMap<A, B>,
    child_ids: &HashMap<A, B>,
) -> Result<GlueBlockBase<B>, SLTNodeFactsError>
where
    A: Copy + Eq + Hash + fmt::Debug + fmt::Display,
    B: Copy + Eq + Hash,
{
    let mut arena = SLTNodeArena::new();
    let mut cache = HashMap::<NodeId, NodeId>::default();
    let map = |address: &GlueAddrBase<A>| match address {
        GlueAddrBase::Parent(id) => GlueAddrBase::Parent(parent_ids[id]),
        GlueAddrBase::Child(id) => GlueAddrBase::Child(child_ids[id]),
    };
    let map_paths = |paths: &[(Vec<A>, celox_slt::LogicPath<GlueAddrBase<A>>)],
                     arena: &mut SLTNodeArena<GlueAddrBase<B>>,
                     cache: &mut HashMap<NodeId, NodeId>|
     -> Result<
        Vec<(Vec<B>, celox_slt::LogicPath<GlueAddrBase<B>>)>,
        SLTNodeFactsError,
    > {
        paths
            .iter()
            .map(|(ports, path)| {
                Ok((
                    ports.iter().map(|id| parent_ids[id]).collect(),
                    path.map_addr(&block.arena, arena, cache, &map)?,
                ))
            })
            .collect()
    };
    let input_ports = map_paths(&block.input_ports, &mut arena, &mut cache)?;
    let output_ports = map_paths(&block.output_ports, &mut arena, &mut cache)?;
    Ok(GlueBlockBase {
        module_id: block.module_id,
        input_ports,
        output_ports,
        arena,
    })
}
