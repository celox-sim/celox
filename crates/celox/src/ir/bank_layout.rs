//! Access order for scalar banks read by more than one dense switch.
use super::{
    AbsoluteAddr, ExecutionUnit, HashMap, HashSet, OptimizedSir, RegionedAbsoluteAddr,
    SIRInstruction, SIROffset, SIRTerminator, STABLE_REGION,
};
use std::collections::BTreeMap;

type Unit = ExecutionUnit<RegionedAbsoluteAddr>;

/// Preserve key order when several ports select from exactly the same homes.
/// Ordinary rank order remains the tie-breaker for the rest of the region.
/// Lane placement and alignment may still separate the homes.
pub(super) fn rank_shared_banks(program: &OptimizedSir, rank: &mut HashMap<AbsoluteAddr, u32>) {
    let mut uses = BTreeMap::new();
    for unit in &program.sir.updates.observe {
        for block in unit.blocks.values() {
            if let Some(bank) = dense_bank(program, unit, &block.terminator) {
                *uses.entry(bank).or_insert(0usize) += 1;
            }
        }
    }

    let mut banks = HashMap::default();
    for (bank, count) in uses {
        if count < 2 || bank.iter().any(|a| banks.contains_key(a)) {
            continue;
        }
        for home in bank {
            let rank = u32::try_from(banks.len()).unwrap_or(u32::MAX);
            banks.insert(home, rank);
        }
    }
    if !banks.is_empty() {
        let count = u32::try_from(banks.len()).unwrap_or(u32::MAX);
        for old in rank.values_mut() {
            *old = old.saturating_add(count);
        }
        rank.extend(banks);
    }
}

fn dense_bank(
    program: &OptimizedSir,
    unit: &Unit,
    terminator: &SIRTerminator,
) -> Option<Vec<AbsoluteAddr>> {
    let SIRTerminator::Switch { cases, default, .. } = terminator else {
        return None;
    };
    if !(8..=256).contains(&cases.len()) {
        return None;
    }
    let fallback = &unit.blocks[default];
    let [SIRInstruction::Imm(reg, constant)] = fallback.instructions.as_slice() else {
        return None;
    };
    let SIRTerminator::Jump(join, args) = &fallback.terminator else {
        return None;
    };
    if args.as_slice() != [*reg] {
        return None;
    }

    let mut homes = Vec::new();
    for case in cases {
        let arm = &unit.blocks[&case.target];
        let SIRTerminator::Jump(target, args) = &arm.terminator else {
            return None;
        };
        if target != join {
            return None;
        }
        match arm.instructions.as_slice() {
            [SIRInstruction::Load(reg, addr, SIROffset::Static(0), width)]
                if addr.region == STABLE_REGION && args.as_slice() == [*reg] =>
            {
                let address = addr.absolute_addr();
                let object = program.design.state_objects.get(&address)?;
                if !(1..=64).contains(width)
                    || object.width != *width
                    || !object.array_dims.is_empty()
                {
                    return None;
                }
                homes.push((u32::try_from(&case.value).ok()?, address, *width));
            }
            [SIRInstruction::Imm(reg, value)] if args.as_slice() == [*reg] && value == constant => {
            }
            _ => return None,
        }
    }
    homes.sort_unstable_by_key(|&(key, ..)| key);
    if homes.len() < 8
        || !homes
            .windows(2)
            .all(|pair| pair[0].0.checked_add(1) == Some(pair[1].0) && pair[0].2 == pair[1].2)
    {
        return None;
    }
    let addresses = homes
        .into_iter()
        .map(|(_, addr, _)| addr)
        .collect::<Vec<_>>();
    (addresses.iter().collect::<HashSet<_>>().len() == addresses.len()).then_some(addresses)
}
