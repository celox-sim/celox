//! Snapshots of the memory state a reload reads.

use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(in super::super) struct StateLoad {
    pub offset: i32,
    pub size: OpSize,
}

impl StateLoad {
    pub(super) fn bytes(self) -> Option<std::ops::Range<i64>> {
        let start = i64::from(self.offset);
        let end = start.checked_add(i64::from(self.size.bytes()))?;
        Some(start..end)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) enum SnapshotAccess {
    LiveOnEntry,
    Write { block: BlockId, ordinal: usize },
    Phi(BlockId),
}

/// Reaching version of the bytes `start..end` of one state load.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct SnapshotSegment {
    pub(super) start: i64,
    pub(super) end: i64,
    pub(super) access: SnapshotAccess,
}

/// Inputs of the per-byte MemoryPhi of `block` over bytes `start..end`, one
/// per CFG predecessor in predecessor order.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(super) struct SnapshotPhi {
    pub(super) block: BlockId,
    pub(super) start: i64,
    pub(super) end: i64,
    pub(super) inputs: Box<[(BlockId, SnapshotAccess)]>,
}

/// Every per-byte MemoryPhi equation of one function, interned once: the
/// pieces of each block's phi ranges that have one input tuple, sorted by
/// block and byte range.  Snapshots reference this table instead of copying
/// the equations they reach, so a function's certificates take memory linear
/// in its roots plus its phi pieces.
#[derive(Debug, Default)]
pub(super) struct PhiTable {
    pub(super) pieces: HashMap<BlockId, Vec<SnapshotPhi>>,
}

impl PhiTable {
    pub(super) fn from_pieces(pieces: Vec<SnapshotPhi>) -> Self {
        let mut table = Self::default();
        for piece in pieces {
            table.pieces.entry(piece.block).or_default().push(piece);
        }
        for pieces in table.pieces.values_mut() {
            pieces.sort_by_key(|piece| piece.start);
        }
        table
    }

    /// Pieces of `block`'s phi equations overlapping `start..end`, in
    /// address order and clipped to that range.
    pub(super) fn runs(
        &self,
        block: BlockId,
        start: i64,
        end: i64,
    ) -> impl Iterator<Item = (i64, i64, &[(BlockId, SnapshotAccess)])> + '_ {
        let pieces = self.pieces.get(&block).map_or(&[][..], Vec::as_slice);
        let first = pieces.partition_point(|piece| piece.end <= start);
        pieces[first..]
            .iter()
            .take_while(move |piece| piece.start < end)
            .map(move |piece| {
                (
                    piece.start.max(start),
                    piece.end.min(end),
                    piece.inputs.as_ref(),
                )
            })
    }
}

/// Stable certificate of the memory state one physical state load observes:
/// the reaching version of every byte, as the per-byte minimal SSA of
/// [`celox_analysis::memory_versions`] defines it, plus the phi equations
/// those versions depend on.
///
/// Two snapshots are equal exactly when every byte has the same reaching
/// version and the same reachable phi equations at both points, so the
/// certificate is a canonical function of the memory state.  Phi equations
/// are retained so final-MIR verification also detects changed incoming
/// definitions at a join with the same block identity.  Within one function
/// the equations are determined by the root, so equal roots share one
/// allocation and compare by pointer.
#[derive(Clone)]
pub(in super::super) struct MemorySnapshot {
    /// Maximal runs of equal versions covering exactly the load's bytes.
    pub(super) root: Rc<[SnapshotSegment]>,
    pub(super) phis: Rc<PhiTable>,
}

impl PartialEq for MemorySnapshot {
    fn eq(&self, other: &Self) -> bool {
        if Rc::ptr_eq(&self.phis, &other.phis) {
            return Rc::ptr_eq(&self.root, &other.root) || self.root == other.root;
        }
        first_snapshot_difference(self, other, &BTreeMap::new(), &BTreeMap::new()).is_none()
    }
}

impl Eq for MemorySnapshot {}

impl Hash for MemorySnapshot {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.root.hash(state);
    }
}

impl fmt::Debug for MemorySnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemorySnapshot")
            .field("root", &self.root)
            .field("phis", &self.reachable_phis())
            .finish()
    }
}

impl MemorySnapshot {
    #[cfg(test)]
    pub(super) fn new(root: Vec<SnapshotSegment>, phis: Vec<SnapshotPhi>) -> Self {
        Self {
            root: root.into(),
            phis: Rc::new(PhiTable::from_pieces(phis)),
        }
    }

    /// Bytes this snapshot covers.
    pub(super) fn span(&self) -> Option<(i64, i64)> {
        Some((self.root.first()?.start, self.root.last()?.end))
    }

    pub(super) fn root_at(&self, byte: i64) -> Option<SnapshotAccess> {
        self.root
            .iter()
            .find(|segment| segment.start <= byte && byte < segment.end)
            .map(|segment| segment.access)
    }

    /// Phi equation pieces of `block` over `start..end`.
    pub(super) fn phi_runs(
        &self,
        block: BlockId,
        start: i64,
        end: i64,
    ) -> impl Iterator<Item = (i64, i64, &[(BlockId, SnapshotAccess)])> + '_ {
        self.phis.runs(block, start, end)
    }

    /// The phi equation pieces reachable from the root.
    pub(super) fn reachable_phis(&self) -> Vec<SnapshotPhi> {
        let mut referenced = BTreeMap::<BlockId, BTreeMap<i64, i64>>::new();
        let mut pending = self
            .root
            .iter()
            .filter_map(|segment| match segment.access {
                SnapshotAccess::Phi(block) => Some((block, segment.start, segment.end)),
                _ => None,
            })
            .collect::<Vec<_>>();
        let mut added = Vec::new();
        while let Some((block, start, end)) = pending.pop() {
            added.clear();
            insert_byte_range(referenced.entry(block).or_default(), start, end, &mut added);
            for &(start, end) in &added {
                for (start, end, inputs) in self.phi_runs(block, start, end) {
                    pending.extend(inputs.iter().filter_map(|&(_, access)| match access {
                        SnapshotAccess::Phi(block) => Some((block, start, end)),
                        _ => None,
                    }));
                }
            }
        }
        let mut phis = Vec::new();
        for (block, ranges) in referenced {
            for (start, end) in ranges {
                phis.extend(
                    self.phi_runs(block, start, end)
                        .map(|(start, end, inputs)| SnapshotPhi {
                            block,
                            start,
                            end,
                            inputs: inputs.into(),
                        }),
                );
            }
        }
        phis
    }

    #[cfg(test)]
    pub(super) fn segments(&self) -> &[SnapshotSegment] {
        &self.root
    }
}
