//! Reload recipes, pure recipes and the homes of stored values.

use super::*;

/// Provenance for one CFG factoring introduced after reload planning.
///
/// The final verifier may inline the corresponding query-specific MemoryPhi
/// only after checking that the final block still has exactly these incoming
/// edges, exactly this successor, and no write which aliases SimState.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in super::super) struct MemoryPhiFactoring {
    pub block: BlockId,
    pub successor: BlockId,
    pub predecessors: Box<[BlockId]>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(in super::super) struct StateRecipe {
    pub load: StateLoad,
    pub(super) snapshot: MemorySnapshot,
    pub(super) observed_bits: StateBitRange,
}

#[cfg(test)]
impl StateRecipe {
    pub(in super::super) fn live_on_entry_for_test(load: StateLoad) -> Self {
        Self {
            load,
            snapshot: {
                let bytes = load.bytes().unwrap();
                MemorySnapshot::new(
                    vec![SnapshotSegment {
                        start: bytes.start,
                        end: bytes.end,
                        access: SnapshotAccess::LiveOnEntry,
                    }],
                    Vec::new(),
                )
            },
            observed_bits: StateBitRange::from_load(load).unwrap(),
        }
    }
}

/// One independently versioned physical fragment of a 32/64-bit machine
/// value.  `value_bit_offset` describes assembly of that machine value; it is
/// not an arbitrary-width VReg type.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(in super::super) struct StateFragmentRecipe {
    pub state: StateRecipe,
    pub value_bit_offset: usize,
    pub state_bit_offset: usize,
    pub width_bits: usize,
}

/// A closed recipe which reconstructs the significant low bits of one machine
/// value from multiple independently checked MemorySSA fragments.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(in super::super) struct CompositeStateRecipe {
    pub fragments: Box<[StateFragmentRecipe]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct StateBitRange {
    pub(super) start: i64,
    pub(super) end: i64,
}

impl StateBitRange {
    pub(super) fn from_load(load: StateLoad) -> Option<Self> {
        let bytes = load.bytes()?;
        Some(Self {
            start: bytes.start.checked_mul(8)?,
            end: bytes.end.checked_mul(8)?,
        })
    }

    pub(super) fn inserted(load: StateLoad, bit_offset: usize, width_bits: usize) -> Option<Self> {
        if width_bits == 0 {
            return None;
        }
        let base = i64::from(load.offset).checked_mul(8)?;
        let bit_offset = i64::try_from(bit_offset).ok()?;
        let width_bits = i64::try_from(width_bits).ok()?;
        let start = base.checked_add(bit_offset)?;
        let end = start.checked_add(width_bits)?;
        let range = Self { start, end };
        let physical = Self::from_load(load)?;
        (range.start >= physical.start && range.end <= physical.end).then_some(range)
    }

    pub(super) fn overlaps(self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
    }
}

/// A value's preferred reload mechanism before a concrete spill plan chooses
/// materialization sites.
//
// `Pure` is represented explicitly now so pure-expression rematerialization
// does not get folded back into storage metadata when it is enabled in the
// allocator-owned splitting slice.  Step 3b initially constructs only the
// constant and state forms; the stack form remains the correctness fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in super::super) enum ReloadRecipe {
    Constant { value: u64 },
    StateVersion(StateRecipe),
    Pure { expression: PureRecipeId },
    Stack,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum PlanningRecipe {
    Constant,
    State,
    Pure { expression: PureRecipeId },
    Stack,
}

/// Reload costs used by spill planning.
///
/// Globally valid constants and state loads have one value-wide cost.  Values
/// committed after their SSA definition instead use sparse, MemorySSA-proved
/// costs at actual MIR uses and phi edges.  Reconstruction independently
/// rebuilds and verifies every recipe which the planner ultimately selects;
/// these cost facts can influence placement but cannot authorize emission.
#[derive(Debug)]
pub(in super::super) struct PlanningRecipes {
    pub(super) global_costs: Vec<Option<u16>>,
    pub(super) point_costs: BTreeMap<PointUse, u16>,
    pub(super) edge_costs: BTreeMap<EdgeUse, u16>,
}

impl PlanningRecipes {
    #[cfg(test)]
    pub fn global_materialization_costs(&self) -> Result<Vec<Option<u16>>, ReloadRecipeError> {
        Ok(self.global_costs.clone())
    }

    pub(in super::super) fn global_materialization_cost(&self, value: VReg) -> Option<u16> {
        self.global_costs.get(value.0 as usize).copied().flatten()
    }

    pub(in super::super) fn materialization_cost_at_point(&self, point: PointUse) -> Option<u16> {
        minimum_cost(
            self.global_materialization_cost(point.value),
            self.point_costs.get(&point).copied(),
        )
    }

    /// Cost of a path-specific MemorySSA recipe, excluding globally available
    /// rematerialization. Spill placement uses this distinction only when it
    /// can omit creation of a persistent stack home altogether.
    pub(in super::super) fn point_specific_materialization_cost(
        &self,
        point: PointUse,
    ) -> Option<u16> {
        self.point_costs.get(&point).copied()
    }

    pub(in super::super) fn materialization_cost_on_edge(&self, edge: EdgeUse) -> Option<u16> {
        minimum_cost(
            self.global_materialization_cost(edge.value),
            self.edge_costs.get(&edge).copied(),
        )
    }

    #[cfg(test)]
    pub(in super::super) fn stack_only(value_count: u32) -> Self {
        Self::with_global_costs(vec![None; value_count as usize])
    }

    #[cfg(test)]
    pub(in super::super) fn with_global_costs(global_costs: Vec<Option<u16>>) -> Self {
        Self {
            global_costs,
            point_costs: BTreeMap::new(),
            edge_costs: BTreeMap::new(),
        }
    }
}

pub(super) fn minimum_cost(left: Option<u16>, right: Option<u16>) -> Option<u16> {
    match (left, right) {
        (Some(left), Some(right)) => Some(left.min(right)),
        (Some(cost), None) | (None, Some(cost)) => Some(cost),
        (None, None) => None,
    }
}

pub(super) fn global_materialization_costs(
    recipes: &[PlanningRecipe],
    pure_recipes: &[PureRecipe],
) -> Result<Vec<Option<u16>>, ReloadRecipeError> {
    let mut costs = vec![None::<Option<u16>>; recipes.len()];
    for start in 0..recipes.len() {
        if costs[start].is_some() {
            continue;
        }
        let mut path = Vec::<usize>::new();
        let mut seen = BTreeSet::<usize>::new();
        let mut current = start;
        let mut cost = loop {
            if let Some(cost) = costs[current] {
                break cost;
            }
            if !seen.insert(current) {
                return Err(ReloadRecipeError::new(
                    "RELOAD_RECIPE.PURE_CYCLE",
                    None,
                    None,
                    Some(VReg(current as u32)),
                    "planning recipe graph contains a cycle",
                ));
            }
            match recipes[current] {
                PlanningRecipe::Constant | PlanningRecipe::State => {
                    costs[current] = Some(Some(1));
                    break Some(1);
                }
                PlanningRecipe::Stack => {
                    costs[current] = Some(None);
                    break None;
                }
                PlanningRecipe::Pure { expression } => {
                    let Some(recipe) = pure_recipes.get(expression.0 as usize) else {
                        return Err(ReloadRecipeError::new(
                            "RELOAD_RECIPE.PURE_EXPRESSION",
                            None,
                            None,
                            Some(VReg(current as u32)),
                            "planning pure-expression identifier is outside its table",
                        ));
                    };
                    path.push(current);
                    current = recipe.source().0 as usize;
                    if current >= recipes.len() {
                        return Err(ReloadRecipeError::new(
                            "RELOAD_RECIPE.VALUE_COVERAGE",
                            None,
                            None,
                            Some(VReg(current as u32)),
                            "planning pure recipe source is outside the VReg table",
                        ));
                    }
                }
            }
        };
        for value in path.into_iter().rev() {
            cost = cost.map(|value| value.saturating_add(1));
            costs[value] = Some(cost);
        }
    }
    Ok(costs
        .into_iter()
        .map(|cost| cost.expect("every planning recipe cost is visited"))
        .collect())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(in super::super) struct PureRecipeId(pub u32);

/// One target operation which may be recomputed if its operand recipes are
/// available at the chosen split point.  Width-changing x86 operations are
/// separate variants; no arbitrary HDL bit width is attached to a VReg.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in super::super) enum PureRecipe {
    MulImm64 {
        source: VReg,
        immediate: i32,
    },
    MulImm32 {
        source: VReg,
        immediate: i32,
    },
    Copy64 {
        source: VReg,
    },
    Copy32 {
        source: VReg,
    },
    AndImm64 {
        source: VReg,
        immediate: u64,
    },
    AndImm32 {
        source: VReg,
        immediate: u32,
    },
    OrImm64 {
        source: VReg,
        immediate: u64,
    },
    ShrImm64 {
        source: VReg,
        immediate: u8,
    },
    ShlImm64 {
        source: VReg,
        immediate: u8,
    },
    SarImm64 {
        source: VReg,
        immediate: u8,
    },
    AddImm64 {
        source: VReg,
        immediate: i32,
    },
    SubImm64 {
        source: VReg,
        immediate: i32,
    },
    CmpImm64 {
        source: VReg,
        immediate: i32,
        kind: CmpKind,
    },
    BitNot64 {
        source: VReg,
    },
    Neg64 {
        source: VReg,
    },
}

impl PureRecipe {
    pub(super) fn source(self) -> VReg {
        match self {
            Self::Copy64 { source }
            | Self::MulImm64 { source, .. }
            | Self::MulImm32 { source, .. }
            | Self::Copy32 { source }
            | Self::AndImm64 { source, .. }
            | Self::AndImm32 { source, .. }
            | Self::OrImm64 { source, .. }
            | Self::ShrImm64 { source, .. }
            | Self::ShlImm64 { source, .. }
            | Self::SarImm64 { source, .. }
            | Self::AddImm64 { source, .. }
            | Self::SubImm64 { source, .. }
            | Self::CmpImm64 { source, .. }
            | Self::BitNot64 { source }
            | Self::Neg64 { source } => source,
        }
    }

    pub(super) fn step(self) -> PureStep {
        match self {
            Self::Copy64 { .. } => PureStep::Copy64,
            Self::MulImm64 { immediate, .. } => PureStep::MulImm64 { immediate },
            Self::MulImm32 { immediate, .. } => PureStep::MulImm32 { immediate },
            Self::Copy32 { .. } => PureStep::Copy32,
            Self::AndImm64 { immediate, .. } => PureStep::AndImm64 { immediate },
            Self::AndImm32 { immediate, .. } => PureStep::AndImm32 { immediate },
            Self::OrImm64 { immediate, .. } => PureStep::OrImm64 { immediate },
            Self::ShrImm64 { immediate, .. } => PureStep::ShrImm64 { immediate },
            Self::ShlImm64 { immediate, .. } => PureStep::ShlImm64 { immediate },
            Self::SarImm64 { immediate, .. } => PureStep::SarImm64 { immediate },
            Self::AddImm64 { immediate, .. } => PureStep::AddImm64 { immediate },
            Self::SubImm64 { immediate, .. } => PureStep::SubImm64 { immediate },
            Self::CmpImm64 {
                immediate, kind, ..
            } => PureStep::CmpImm64 { immediate, kind },
            Self::BitNot64 { .. } => PureStep::BitNot64,
            Self::Neg64 { .. } => PureStep::Neg64,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(in super::super) enum PureStep {
    MulImm64 { immediate: i32 },
    MulImm32 { immediate: i32 },
    Copy64,
    Copy32,
    AndImm64 { immediate: u64 },
    AndImm32 { immediate: u32 },
    OrImm64 { immediate: u64 },
    ShrImm64 { immediate: u8 },
    ShlImm64 { immediate: u8 },
    SarImm64 { immediate: u8 },
    AddImm64 { immediate: i32 },
    SubImm64 { immediate: i32 },
    CmpImm64 { immediate: i32, kind: CmpKind },
    BitNot64,
    Neg64,
}

/// Lower one width-explicit reload-recipe operation to canonical MIR.
///
/// Recipe discovery, allocator-owned materialization, and legacy SSA
/// reconstruction must agree on the exact 32/64-bit machine semantics. Keep
/// that mapping here, next to [`PureStep`], instead of re-deriving it in each
/// lowering path.
pub(in super::super) fn materialize_pure_step(step: PureStep, dst: VReg, source: VReg) -> MInst {
    match step {
        PureStep::MulImm64 { immediate } => MInst::MulImm {
            dst,
            src: source,
            imm: immediate,
        },
        PureStep::MulImm32 { immediate } => MInst::MulImm32 {
            dst,
            src: source,
            imm: immediate,
        },
        PureStep::Copy64 => MInst::Mov { dst, src: source },
        PureStep::Copy32 => MInst::Mov32 { dst, src: source },
        PureStep::AndImm64 { immediate } => MInst::AndImm {
            dst,
            src: source,
            imm: immediate,
        },
        PureStep::AndImm32 { immediate } => MInst::AndImm32 {
            dst,
            src: source,
            imm: immediate,
        },
        PureStep::OrImm64 { immediate } => MInst::OrImm {
            dst,
            src: source,
            imm: immediate,
        },
        PureStep::ShrImm64 { immediate } => MInst::ShrImm {
            dst,
            src: source,
            imm: immediate,
        },
        PureStep::ShlImm64 { immediate } => MInst::ShlImm {
            dst,
            src: source,
            imm: immediate,
        },
        PureStep::SarImm64 { immediate } => MInst::SarImm {
            dst,
            src: source,
            imm: immediate,
        },
        PureStep::AddImm64 { immediate } => MInst::AddImm {
            dst,
            src: source,
            imm: immediate,
        },
        PureStep::SubImm64 { immediate } => MInst::SubImm {
            dst,
            src: source,
            imm: immediate,
        },
        PureStep::CmpImm64 { immediate, kind } => MInst::CmpImm {
            dst,
            lhs: source,
            imm: immediate,
            kind,
        },
        PureStep::BitNot64 => MInst::BitNot { dst, src: source },
        PureStep::Neg64 => MInst::Neg { dst, src: source },
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(in super::super) enum ResolvedBase {
    Constant(u64),
    State(StateRecipe),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(in super::super) struct ResolvedRecipe {
    pub base: ResolvedBase,
    pub steps: Vec<PureStep>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StoreHomeSpec {
    pub(super) value: VReg,
    pub(super) load: StateLoad,
    pub(super) steps: Vec<PureStep>,
    pub(super) observed_bits: StateBitRange,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StoreHome {
    pub(super) state: StateRecipe,
    pub(super) steps: Vec<PureStep>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StateFragmentHome {
    pub(super) state: StateRecipe,
    pub(super) value_bit_offset: usize,
    pub(super) state_bit_offset: usize,
    pub(super) width_bits: usize,
}

pub(super) type StoreHomeIndex = BTreeMap<i64, BTreeMap<VReg, usize>>;

pub(super) type FragmentHomeIndex = BTreeMap<i64, BTreeMap<VReg, usize>>;

pub(super) fn index_store_home(index: &mut StoreHomeIndex, value: VReg, home: &StoreHome) {
    let bytes = home
        .state
        .load
        .bytes()
        .expect("validated store-home load has a finite byte range");
    for byte in bytes {
        *index.entry(byte).or_default().entry(value).or_default() += 1;
    }
}

pub(super) fn unindex_store_home(index: &mut StoreHomeIndex, value: VReg, home: &StoreHome) {
    let bytes = home
        .state
        .load
        .bytes()
        .expect("validated store-home load has a finite byte range");
    for byte in bytes {
        let remove_byte = {
            let values = index
                .get_mut(&byte)
                .expect("popped store home is present in its byte index");
            let count = values
                .get_mut(&value)
                .expect("popped store home value is present in its byte index");
            *count -= 1;
            if *count == 0 {
                values.remove(&value);
            }
            values.is_empty()
        };
        if remove_byte {
            index.remove(&byte);
        }
    }
}

pub(super) fn index_fragment_home(
    index: &mut FragmentHomeIndex,
    value: VReg,
    home: &StateFragmentHome,
) {
    let bytes = home
        .state
        .load
        .bytes()
        .expect("validated fragment-home load has a finite byte range");
    for byte in bytes {
        *index.entry(byte).or_default().entry(value).or_default() += 1;
    }
}

pub(super) fn unindex_fragment_home(
    index: &mut FragmentHomeIndex,
    value: VReg,
    home: &StateFragmentHome,
) {
    let bytes = home
        .state
        .load
        .bytes()
        .expect("validated fragment-home load has a finite byte range");
    for byte in bytes {
        let remove_byte = {
            let values = index
                .get_mut(&byte)
                .expect("popped fragment home is present in its byte index");
            let count = values
                .get_mut(&value)
                .expect("popped fragment-home value is present in its byte index");
            *count -= 1;
            if *count == 0 {
                values.remove(&value);
            }
            values.is_empty()
        };
        if remove_byte {
            index.remove(&byte);
        }
    }
}
