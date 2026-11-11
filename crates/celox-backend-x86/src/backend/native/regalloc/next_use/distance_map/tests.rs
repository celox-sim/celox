use super::*;

#[test]
fn encoded_lengths_match_the_written_bytes() {
    let row = (0..200u32)
        .map(|id| {
            (
                VReg(id * id % 977 + id * 5),
                (id * 7919) % 70_000 + (id % 3) * (1 << 24),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>()
        .into_iter()
        .collect::<Vec<_>>();
    for chunk in row.chunks(CHUNK_SIZE).chain(row.chunks(INDEX_STRIDE)) {
        let mut data = Vec::new();
        let (mut previous_key, mut previous_value) = (0, 0i64);
        for &(key, value) in chunk {
            write_varint(&mut data, u64::from(key.0 - previous_key));
            let delta = i64::from(value) - previous_value;
            write_varint(&mut data, ((delta << 1) ^ (delta >> 63)) as u64);
            previous_key = key.0;
            previous_value = i64::from(value);
        }
        assert_eq!(encoded_len(chunk), data.len());
    }
}

#[test]
fn frozen_rows_find_every_key_across_index_points() {
    let pool = (0..4096).map(VReg).collect::<Arc<[_]>>();
    let mut map = DistanceMap::default();
    for id in 0..700 {
        map.insert(
            VReg(id * 5 + id % 3),
            NextUseDistance::local((id * 37 % 1000) as usize),
        );
    }
    let expected = map.clone();
    map.freeze(&pool);
    assert!(matches!(map, DistanceMap::Frozen(_)));
    for key in pool.iter() {
        assert_eq!(map.get(key), expected.get(key), "{key}");
    }
    assert_eq!(map, expected);
}

#[test]
fn direct_block_transfers_match_materialized_maps_across_relative_rows() {
    let pool = (0..4096).map(VReg).collect::<Arc<[_]>>();
    let mut base = DistanceMap::default();
    for id in 0..1000 {
        base.insert(
            VReg(id * 3),
            NextUseDistance::Finite {
                loop_exits: (id % 7) as usize,
                instructions: (id % 101) as usize,
            },
        );
    }
    base.freeze(&pool);
    let definitions = [VReg(3), VReg(1500), VReg(4095)]
        .into_iter()
        .collect::<crate::HashSet<_>>();
    for round in 0..12 {
        let instructions = round + 3;
        let local_uses = [(VReg(0), 1), (VReg(1501), 2), (VReg(0), round)];
        let mut expected = DistanceMap::default();
        for (&key, distance) in &base {
            if !definitions.contains(&key) {
                expected.insert(
                    key,
                    distance.checked_prepend_instructions(instructions).unwrap(),
                );
            }
        }
        for &(key, position) in &local_uses {
            expected.insert(key, NextUseDistance::local(position));
        }
        let actual = DistanceMap::try_block_entry(&base, instructions, &definitions, &local_uses)
            .expect("small exact transfer should share its frozen base");
        assert_eq!(actual, expected);
        for key in pool.iter() {
            assert_eq!(actual.get(key), expected.get(key));
        }
        base = actual;
    }
}

#[test]
fn direct_block_transfers_fall_back_for_range_and_edit_limits() {
    let pool = (0..256).map(VReg).collect::<Arc<[_]>>();
    let mut base = DistanceMap::default();
    for id in 0..256 {
        base.insert(VReg(id), NextUseDistance::local(id as usize));
    }
    base.freeze(&pool);
    let empty = crate::HashSet::default();
    let shared = DistanceMap::try_block_entry(&base, 0, &empty, &[]).unwrap();
    let (DistanceMap::Frozen(left), DistanceMap::Frozen(right)) = (&base, &shared) else {
        panic!("unchanged transfer should reuse the frozen row")
    };
    assert!(Arc::ptr_eq(left, right));
    assert!(DistanceMap::try_block_entry(&base, usize::MAX, &empty, &[]).is_none());
    assert!(DistanceMap::try_block_entry(&base, 0, &(0..33).map(VReg).collect(), &[]).is_none());
    assert!(DistanceMap::try_block_entry(&base, 0, &empty, &[(VReg(u32::MAX), 0)]).is_none());
    assert!(
        DistanceMap::try_block_entry(
            &base,
            0,
            &empty,
            &[(VReg(0), PACKED_INSTRUCTION_MASK as usize + 1)]
        )
        .is_none()
    );
    let relative = DistanceMap::try_block_entry(
        &base,
        0,
        &empty,
        &[(VReg(0), PACKED_INSTRUCTION_MASK as usize)],
    )
    .unwrap();
    assert!(DistanceMap::try_block_entry(&relative, 1, &empty, &[]).is_none());
}

#[test]
fn unchanged_entry_shares_the_frozen_exit_directly() {
    let pool = (0..256).map(VReg).collect::<Arc<[_]>>();
    let mut base = DistanceMap::default();
    for id in 0..256 {
        base.insert(VReg(id), NextUseDistance::local(id as usize));
    }
    let mut entry = base.clone();
    base.freeze(&pool);
    assert!(entry.freeze_block_entry(&base, 0, &crate::HashSet::default(), &[]));
    let (DistanceMap::Frozen(base), DistanceMap::Frozen(entry)) = (&base, &entry) else {
        panic!("expected shared frozen rows")
    };
    assert!(Arc::ptr_eq(base, entry));
}

#[test]
fn relative_rows_preserve_transfer_edits_order_and_mutation() {
    let pool = (0..4096).map(VReg).collect::<Arc<[_]>>();
    let mut base = DistanceMap::default();
    for id in 1..1001 {
        base.insert(
            VReg(id * 3),
            NextUseDistance::Finite {
                loop_exits: (id % 7) as usize,
                instructions: (id % 101) as usize,
            },
        );
    }
    base.freeze(&pool);
    let mut entry = base
        .iter()
        .map(|(&key, distance)| (key, distance.checked_prepend_instructions(17).unwrap()))
        .fold(DistanceMap::default(), |mut row, (key, distance)| {
            row.insert(key, distance);
            row
        });
    for id in [3, 1500, 3000] {
        entry.remove(&VReg(id));
    }
    for id in [0, 9, 1501, 4095] {
        entry.insert(VReg(id), NextUseDistance::local(2));
    }
    let expected = entry.clone();
    assert!(entry.freeze_block_entry(
        &base,
        17,
        &[VReg(3), VReg(1500), VReg(3000)].into_iter().collect(),
        &[(VReg(0), 2), (VReg(9), 2), (VReg(1501), 2), (VReg(4095), 2)]
    ));
    assert!(matches!(entry, DistanceMap::Relative(_)));
    assert_eq!(entry, expected);
    assert_eq!(expected, entry);
    let mut frozen = expected.clone();
    frozen.freeze(&pool);
    assert!(matches!(frozen, DistanceMap::Frozen(_)));
    assert_eq!(entry, frozen);
    assert_eq!(frozen, entry);
    let shifted =
        DistanceMap::try_block_entry(&frozen, 1, &crate::HashSet::default(), &[]).unwrap();
    assert_ne!(entry, shifted);
    assert_ne!(shifted, entry);
    frozen.insert(VReg(1501), NextUseDistance::local(3));
    frozen.freeze(&pool);
    assert_eq!(entry.len(), frozen.len());
    assert_ne!(entry, frozen);
    assert_ne!(frozen, entry);
    let mut oracle = expected
        .iter()
        .map(|(&key, distance)| (key, distance))
        .collect::<Vec<_>>();
    oracle.sort_by_key(|&(key, _)| key);
    assert_eq!(
        entry
            .iter()
            .map(|(&key, distance)| (key, distance))
            .collect::<Vec<_>>(),
        oracle
    );
    for key in pool.iter().chain([VReg(u32::MAX)].iter()) {
        assert_eq!(entry.get(key), expected.get(key));
    }
    let saved = entry.clone();
    let mut changed = expected.clone();
    assert_eq!(entry.remove(&VReg(1501)), changed.remove(&VReg(1501)));
    let wide = NextUseDistance::Finite {
        loop_exits: usize::MAX,
        instructions: usize::MAX,
    };
    assert_eq!(entry.insert(VReg(0), wide), changed.insert(VReg(0), wide));
    assert_eq!(entry, changed);
    assert_eq!(saved, expected);
}

#[test]
fn chained_transfers_share_one_base_and_preserve_overrides() {
    let pool = (0..1024).map(VReg).collect::<Arc<[_]>>();
    let mut row = DistanceMap::default();
    for id in 0..512 {
        row.insert(VReg(id), NextUseDistance::local(id as usize));
    }
    row.freeze(&pool);
    let DistanceMap::Frozen(base) = &row else {
        panic!("expected frozen row")
    };
    let original = Arc::clone(base);
    for step in 0..256 {
        let mut next = DistanceMap::default();
        for (&key, distance) in &row {
            next.insert(key, distance.checked_prepend_instructions(3).unwrap());
        }
        let removed = VReg(step % 8);
        let used = VReg(512 + step % 8);
        next.remove(&removed);
        next.insert(used, NextUseDistance::local(1));
        let expected = next.clone();
        assert!(next.freeze_block_entry(&row, 3, &[removed].into_iter().collect(), &[(used, 1)],));
        assert_eq!(next, expected);
        let DistanceMap::Relative(relative) = &next else {
            panic!("expected relative row")
        };
        assert!(Arc::ptr_eq(&relative.base, &original));
        for key in pool.iter() {
            assert_eq!(next.get(key), expected.get(key));
        }
        let mut actual = next.iter().map(|(&k, v)| (k, v)).collect::<Vec<_>>();
        let mut oracle = expected.iter().map(|(&k, v)| (k, v)).collect::<Vec<_>>();
        actual.sort_unstable_by_key(|&(k, _)| k);
        oracle.sort_unstable_by_key(|&(k, _)| k);
        assert_eq!(actual, oracle);
        row = next;
    }
    // Too many accumulated edits fall back to the exact ordinary row.
    let mut next = DistanceMap::default();
    for (&key, distance) in &row {
        next.insert(key, distance);
    }
    let definitions = (16..48).map(VReg).collect::<crate::HashSet<_>>();
    for key in &definitions {
        next.remove(key);
    }
    let expected = next.clone();
    assert!(!next.freeze_block_entry(&row, 0, &definitions, &[]));
    next.freeze(&pool);
    assert_eq!(next, expected);
}

#[test]
fn relative_rows_handle_packed_boundaries_and_fallbacks() {
    let pool = (0..1024).map(VReg).collect::<Arc<[_]>>();
    let mut base = DistanceMap::default();
    for id in 0..256 {
        base.insert(
            VReg(id),
            NextUseDistance::Finite {
                loop_exits: 255,
                instructions: PACKED_INSTRUCTION_MASK as usize,
            },
        );
    }
    base.freeze(&pool);
    let mut entry = base.clone();
    entry.remove(&VReg(0));
    entry.insert(VReg(1), NextUseDistance::local(0));
    let expected = entry.clone();
    assert!(entry.freeze_block_entry(&base, 0, &[VReg(0)].into_iter().collect(), &[(VReg(1), 0)]));
    assert_eq!(entry, expected);
    assert!(
        !expected
            .clone()
            .freeze_block_entry(&base, 1, &crate::HashSet::default(), &[])
    );
    assert!(!expected.clone().freeze_block_entry(
        &base,
        usize::MAX,
        &crate::HashSet::default(),
        &[]
    ));
    let mut composed = expected.clone();
    assert!(composed.freeze_block_entry(&entry, 0, &crate::HashSet::default(), &[]));
    assert_eq!(composed, expected);
    let mut many_edits = base.clone();
    for id in 0..33 {
        many_edits.remove(&VReg(id));
    }
    let unchanged = many_edits.clone();
    assert!(!many_edits.freeze_block_entry(&base, 0, &(0..33).map(VReg).collect(), &[]));
    assert_eq!(many_edits, unchanged);
    many_edits.freeze(&pool);
    assert_eq!(many_edits, unchanged);
    let mut wide = base.clone();
    wide.insert(VReg(2), NextUseDistance::Dead);
    assert!(!wide.freeze_block_entry(&base, 0, &crate::HashSet::default(), &[]));
    assert_eq!(wide.get(&VReg(2)), Some(NextUseDistance::Dead));
}

#[test]
fn relative_rows_fall_back_when_removed_values_would_overflow_shift() {
    let pool = (0..256).map(VReg).collect::<Arc<[_]>>();
    let mut base = DistanceMap::default();
    let mut entry = DistanceMap::default();
    for id in 0..256 {
        base.insert(
            VReg(id),
            NextUseDistance::Finite {
                loop_exits: 255,
                instructions: if id < 2 {
                    PACKED_INSTRUCTION_MASK as usize
                } else {
                    id as usize
                },
            },
        );
        if id != 0 {
            entry.insert(
                VReg(id),
                if id == 1 {
                    NextUseDistance::local(0)
                } else {
                    NextUseDistance::Finite {
                        loop_exits: 255,
                        instructions: id as usize + 1,
                    }
                },
            );
        }
    }
    base.freeze(&pool);
    let expected = entry.clone();
    assert!(!entry.freeze_block_entry(&base, 1, &[VReg(0)].into_iter().collect(), &[(VReg(1), 0)]));
    entry.freeze(&pool);
    assert_eq!(entry, expected);
    assert_eq!(expected, entry);
    assert_eq!(entry.iter().count(), 255);
    for key in &*pool {
        assert_eq!(entry.get(key), expected.get(key));
    }
}

#[test]
fn frozen_rows_preserve_lookup_iteration_and_mutation() {
    let values = (0..4096).map(VReg).collect::<Arc<[_]>>();
    let mut row = DistanceMap::default();
    for i in 0..1000 {
        row.insert(
            VReg(i * 3),
            NextUseDistance::Finite {
                loop_exits: if i % 100 < 50 { 255 } else { 0 },
                instructions: (1000 - i) as usize,
            },
        );
    }
    let expected = row.clone();
    row.freeze(&values);
    assert!(matches!(row, DistanceMap::Frozen(_)));
    assert_eq!(row, expected);
    assert_eq!(expected, row);
    let mut different = expected.clone();
    different.insert(VReg(3), NextUseDistance::local(0));
    assert_ne!(different, row);
    assert_ne!(row, different);
    assert_eq!(row.iter().count(), expected.len());
    for (key, value) in row.iter() {
        assert_eq!(expected.get(key), Some(value));
    }
    for key in values.iter() {
        assert_eq!(row.get(key), expected.get(key));
    }
    let mut changed = row.clone();
    assert_eq!(changed.remove(&VReg(3)), expected.get(&VReg(3)));
    assert_eq!(row, expected);
    assert_eq!(changed.get(&VReg(3)), None);
    changed.freeze(&values);
    let wide = NextUseDistance::Finite {
        loop_exits: usize::MAX,
        instructions: usize::MAX,
    };
    changed.insert(VReg(3), wide);
    assert_eq!(changed.get(&VReg(3)), Some(wide));
    assert_eq!(changed.len(), expected.len());
}

#[test]
fn frozen_rows_handle_exact_chunks_and_empty_rows() {
    let values = (0..256).map(VReg).collect::<Arc<[_]>>();
    for size in [0, 1, 63, 64, 65, 127, 128, 129, 256] {
        let mut row = DistanceMap::default();
        for i in 0..size {
            row.insert(VReg(i), NextUseDistance::local(0));
        }
        let expected = row.clone();
        row.freeze(&values);
        assert_eq!(row, expected);
        assert_eq!(row.iter().count(), size as usize);
        for key in values.iter() {
            assert_eq!(row.get(key), expected.get(key));
        }
    }
}

#[test]
fn packed_boundaries_fall_back_independently_without_changing_rows() {
    for overflow in [
        NextUseDistance::Finite {
            loop_exits: 256,
            instructions: 0,
        },
        NextUseDistance::Finite {
            loop_exits: 0,
            instructions: 1 << 24,
        },
        NextUseDistance::Dead,
    ] {
        let mut row = DistanceMap::default();
        let boundary = NextUseDistance::Finite {
            loop_exits: 255,
            instructions: (1 << 24) - 1,
        };
        row.insert(VReg(0), boundary);
        assert!(matches!(row, DistanceMap::Packed(_)));
        let packed = row.clone();
        row.insert(VReg(1), overflow);
        assert_eq!(row.get(&VReg(0)), Some(boundary));
        assert_eq!(row.remove(&VReg(1)), Some(overflow));
        assert_eq!(row, packed);
    }
}

#[test]
fn compact_rows_preserve_boundaries_and_promote_without_truncation() {
    let mut row = DistanceMap::default();
    let mut expected = HashMap::default();
    for (index, distance) in [
        NextUseDistance::Finite {
            loop_exits: 0,
            instructions: 0,
        },
        NextUseDistance::Finite {
            loop_exits: u32::MAX as usize,
            instructions: u32::MAX as usize,
        },
        NextUseDistance::Finite {
            loop_exits: usize::MAX,
            instructions: usize::MAX,
        },
        NextUseDistance::Dead,
    ]
    .into_iter()
    .enumerate()
    {
        let key = VReg(index as u32);
        row.insert(key, distance);
        expected.insert(key, distance);
        if index == 0 {
            assert!(matches!(row, DistanceMap::Packed(_)));
        } else if index == 1 {
            assert!(matches!(row, DistanceMap::Compact(_)));
        }
        assert_eq!(
            row.iter()
                .map(|(&key, value)| (key, value))
                .collect::<HashMap<_, _>>(),
            expected
        );
        for (key, value) in &expected {
            assert_eq!(row.get(key), Some(*value));
        }
    }
    for key in expected.keys() {
        assert_eq!(row.remove(key), expected.get(key).copied());
    }
    assert_eq!(row.len(), 0);
}

#[test]
fn minimum_updates_and_equality_do_not_depend_on_storage() {
    let mut compact = DistanceMap::default();
    let mut wide = DistanceMap::Wide(HashMap::default());
    for instructions in [30, 10, 20, 0, 40] {
        let distance = NextUseDistance::Finite {
            loop_exits: 0,
            instructions,
        };
        compact.insert_min(VReg(1), distance);
        wide.insert_min(VReg(1), distance);
        assert_eq!(compact, wide);
    }
    assert_eq!(compact.get(&VReg(1)), Some(NextUseDistance::local(0)));
}
