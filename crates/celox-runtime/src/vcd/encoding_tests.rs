use super::*;

fn changes(bytes: &[u8]) -> Vec<(u64, String)> {
    let mut parser = vcd::Parser::new(bytes);
    parser.parse_header().unwrap();
    let mut time = 0;
    parser
        .filter_map(|command| match command.unwrap() {
            vcd::Command::Timestamp(t) => {
                time = t;
                None
            }
            vcd::Command::ChangeScalar(_, value) => Some((time, value.to_string())),
            vcd::Command::ChangeVector(_, value) => Some((time, value.to_string())),
            _ => None,
        })
        .collect()
}

#[test]
fn integer_encoding_matches_binary_format_at_every_bit_length() {
    let mut values = vec![0, u64::MAX, 0x0123_4567_89ab_cdef, 0xaaaa_5555_aaaa_5555];
    let mut random = 0x8314_40be_9d6a_2785u64;
    for bit in 0..64 {
        let mask = u64::MAX >> (63 - bit);
        values.extend([1 << bit, mask]);
        for _ in 0..32 {
            random ^= random << 13;
            random ^= random >> 7;
            random ^= random << 17;
            values.push((random & mask) | (1 << bit));
        }
    }
    // Exercise unaligned output, capacity growth, and reuse of spare bytes
    // after truncation, checking that neither prefixes nor lengths change.
    for prefix in [0, 1, 15, 16, 63] {
        let mut actual = vec![b'#'; prefix];
        let mut expected = actual.clone();
        for &value in &values {
            actual.reserve(65);
            let len = encode_u64(actual.spare_capacity_mut(), value);
            // SAFETY: encode_u64 initialized len bytes of spare capacity.
            unsafe { actual.set_len(actual.len() + len) };
            expected.extend_from_slice(format!("b{value:b}").as_bytes());
            assert_eq!(actual, expected, "prefix={prefix} value={value:#x}");
            if actual.len() > 4096 {
                actual.truncate(prefix);
                expected.truncate(prefix);
            }
        }
    }
}

#[test]
fn record_suffixes_preserve_ids_and_never_publish_padding() {
    for (number, expected) in [(0, "!"), (93, "~"), (94, "!!"), (8929, "~~"), (8930, "!!!")] {
        assert_eq!(VcdWriter::<Vec<u8>>::generate_vcd_id(number), expected);
    }
    // Test both sides of the inline boundary without allocating the huge
    // signal set needed to reach long IDs through normal registration.
    for id in [
        "!",
        "~",
        "!!",
        "~~~",
        "abcdef",
        "abcdefg",
        "abcdefgh",
        "abcdefghi",
        "abcdefghijklmnop",
    ] {
        for width in [1, 64] {
            let suffix = VcdRecordSuffix::new(width, id);
            let expected_suffix = format!("{}{id}\n", if width == 1 { "" } else { " " });
            for prefix in 0..16 {
                let mut guarded = [MaybeUninit::new(b'#'); 64];
                let end = prefix + suffix.capacity();
                let len = suffix.encode(&mut guarded[prefix..end]);
                // SAFETY: all guard bytes started initialized, and encoding
                // only overwrites them with initialized suffix bytes.
                let actual = guarded.map(|byte| unsafe { byte.assume_init() });
                assert_eq!(&actual[..prefix], vec![b'#'; prefix]);
                assert_eq!(&actual[prefix..prefix + len], expected_suffix.as_bytes());
                assert!(actual[end..].iter().all(|&byte| byte == b'#'));

                let mut records = vec![b'#'; prefix];
                let mut expected = records.clone();
                for value in [0u64, 1, 1 << 63, u64::MAX, 0] {
                    let value_capacity = if width == 1 { 1 } else { 65 };
                    records.reserve(value_capacity + suffix.capacity());
                    // Bound the spare slice to exactly the reserved record
                    // space, including fixed-store padding. Later records
                    // must overwrite that padding at the visible length.
                    let out =
                        &mut records.spare_capacity_mut()[..value_capacity + suffix.capacity()];
                    let value_len = if width == 1 {
                        out[0].write(b'0' + (value & 1) as u8);
                        1
                    } else {
                        encode_u64(out, value)
                    };
                    let suffix_len = suffix.encode(&mut out[value_len..]);
                    // SAFETY: both encoders initialized their returned lengths.
                    unsafe { records.set_len(records.len() + value_len + suffix_len) };
                    let value_text = if width == 1 {
                        (value & 1).to_string()
                    } else {
                        format!("b{value:b}")
                    };
                    expected.extend_from_slice(value_text.as_bytes());
                    expected.extend_from_slice(expected_suffix.as_bytes());
                    assert_eq!(records, expected, "id={id} width={width} prefix={prefix}");
                }
            }
        }
    }
}

#[test]
fn complete_records_cross_small_blocks_with_scalar_and_mixed_widths() {
    for scalar_only in [true, false] {
        let descs = (0..100)
            .map(|i| VcdSignalDesc {
                scope: format!("scope{}", i % 2),
                name: format!("s{i}"),
                offset: i * 8,
                width: if scalar_only { 1 } else { [1, 9, 64][i % 3] },
                is_4state: false,
            })
            .collect::<Vec<_>>();
        for capacity in [1, 3, 7, 8, 9, 16, 64, 71, 72, 73] {
            let mut writer = VcdWriter::from_writer(Vec::new(), &descs);
            writer.output.writer = BufWriter::with_capacity(capacity, Vec::new());
            for (time, value) in [0xff, 0, 0, 0xff].into_iter().enumerate() {
                writer.dump(time as u64, &[value; 800]).unwrap();
            }
            assert_eq!(writer.statistics().changes, 300);
            let bytes = writer.into_inner().unwrap();
            let mut parser = vcd::Parser::new(bytes.as_slice());
            let header = parser.parse_header().unwrap();
            let mut names = fxhash::FxHashMap::default();
            for item in header.items {
                if let vcd::ScopeItem::Scope(scope) = item {
                    for item in scope.items {
                        if let vcd::ScopeItem::Var(var) = item {
                            names.insert(var.code, var.reference);
                        }
                    }
                }
            }
            assert_eq!(names.len(), descs.len());
            let mut time = 0;
            let actual = parser
                .filter_map(|command| {
                    let (id, value) = match command.unwrap() {
                        vcd::Command::Timestamp(t) => {
                            time = t;
                            return None;
                        }
                        vcd::Command::ChangeScalar(id, value) => (id, value.to_string()),
                        vcd::Command::ChangeVector(id, value) => (id, value.to_string()),
                        _ => return None,
                    };
                    Some((time, names[&id].clone(), value))
                })
                .collect::<Vec<_>>();
            let expected = [0, 1, 3]
                .into_iter()
                .flat_map(|time| {
                    descs.iter().map(move |desc| {
                        let value = if time == 1 {
                            "0".into()
                        } else {
                            "1".repeat(desc.width)
                        };
                        (time, desc.name.clone(), value)
                    })
                })
                .collect::<Vec<_>>();
            assert_eq!(
                actual, expected,
                "scalar_only={scalar_only} capacity={capacity}"
            );
        }
    }
}

#[test]
fn integer_memory_at_unaligned_buffer_end_preserves_changes_and_aliases() {
    for offset in 1..=8 {
        let descs = [
            VcdSignalDesc {
                scope: "top".into(),
                name: "prefix".into(),
                offset: 0,
                width: 7,
                is_4state: false,
            },
            VcdSignalDesc {
                scope: "top".into(),
                name: "q".into(),
                offset,
                width: 64,
                is_4state: false,
            },
            VcdSignalDesc {
                scope: "top".into(),
                name: "alias".into(),
                offset,
                width: 64,
                is_4state: false,
            },
        ];
        let mut writer = VcdWriter::from_writer(Vec::new(), &descs);
        // Both previous values follow a 7-bit signal, so their cached
        // offsets are unaligned too. No padding follows the memory value.
        let mut memory = vec![0; offset + 8];
        memory[0] = 0x45;
        let mut expected = vec![(0, format!("{:b}", memory[0]))];
        for (step, value) in std::iter::once(0u64)
            .chain((0..64).map(|bit| 1 << bit))
            .chain([u64::MAX, 0])
            .enumerate()
        {
            let time = (step * 2) as u64;
            memory[offset..].copy_from_slice(&value.to_le_bytes());
            writer.dump(time, &memory).unwrap();
            writer.dump(time + 1, &memory).unwrap();
            expected.extend(std::iter::repeat_n((time, format!("{value:b}")), 2));
        }
        assert_eq!(writer.statistics().changes, expected.len() as u64);
        assert_eq!(
            changes(&writer.into_inner().unwrap()),
            expected,
            "offset={offset}"
        );
    }
}

#[test]
fn emitted_values_match_independent_biguint_oracle() {
    for width in [1, 7, 8, 9, 31, 32, 63, 64, 65, 257, 1024] {
        for four_state in [false, true] {
            let desc = VcdSignalDesc {
                scope: "top".into(),
                name: "q".into(),
                offset: 0,
                width,
                is_4state: four_state,
            };
            let mut writer = VcdWriter::from_writer(Vec::new(), &[desc]);
            let size = width.div_ceil(8);
            let mut memory = vec![0; size * 2];
            let limit = (BigUint::from(1u8) << width) - 1u8;
            let mut expected = vec![];
            let mut previous = None;
            let mut random = 0x8314_40be_9d6a_2785u64;
            for step in 0..96u64 {
                if step % 4 != 1 {
                    for byte in &mut memory {
                        random ^= random << 13;
                        random ^= random >> 7;
                        random ^= random << 17;
                        *byte = random as u8;
                    }
                }
                if step % 3 == 0 {
                    memory[size..].fill(0);
                }
                if step % 8 == 0 {
                    memory[..size].fill(0);
                }
                // Deliberately include nonzero padding above the declared width.
                let value = BigUint::from_bytes_le(&memory[..size]) & &limit;
                let mask = if four_state {
                    BigUint::from_bytes_le(&memory[size..]) & &limit
                } else {
                    BigUint::default()
                };
                let state = (value.clone(), mask.clone());
                if previous.as_ref() != Some(&state) {
                    let text = if mask == BigUint::default() {
                        value.to_str_radix(2)
                    } else {
                        (0..width)
                            .rev()
                            .map(|bit| match (mask.bit(bit as u64), value.bit(bit as u64)) {
                                (false, false) => '0',
                                (false, true) => '1',
                                (true, false) => 'z',
                                (true, true) => 'x',
                            })
                            .collect()
                    };
                    expected.push((step / 2, text));
                    previous = Some(state);
                }
                writer.dump(step / 2, &memory).unwrap();
            }
            assert_eq!(
                changes(&writer.into_inner().unwrap()),
                expected,
                "width={width} four_state={four_state}"
            );
        }
    }
}

#[test]
fn typed_sparse_runs_preserve_aliases_external_values_and_order() {
    let descs = [
        (512, 64, false),
        (0, 1, false),
        (128, 64, false),
        (256, 64, true),
        (512, 64, false),
        (640, 1, false),
        (768, 33, true),
        (896, 64, false),
        (0, 1, false),
        (1024, 9, false),
        (1152, 64, false),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (offset, width, is_4state))| VcdSignalDesc {
        scope: format!("scope{}", index % 2),
        name: format!("s{index}"),
        offset,
        width,
        is_4state,
    })
    .collect::<Vec<_>>();
    let external_descs = [1, 64, 65]
        .into_iter()
        .enumerate()
        .map(|(index, width)| VcdExternalSignalDesc {
            scope: "component".into(),
            name: format!("e{index}"),
            width,
        })
        .collect::<Vec<_>>();
    let mut sparse = VcdWriter::from_writer(Vec::new(), &descs);
    let mut full = VcdWriter::from_writer(Vec::new(), &descs);
    sparse.add_external_signals(&external_descs).unwrap();
    full.add_external_signals(&external_descs).unwrap();
    let mut memory = vec![0; 1216];
    for (step, groups) in [
        vec![],
        vec![8, 0, 8],
        vec![4, 2],
        vec![12, 10],
        vec![16, 14],
        vec![18, 0],
        vec![],
    ]
    .into_iter()
    .enumerate()
    {
        for &group in &groups {
            for (index, byte) in memory[group * 64..][..64].iter_mut().enumerate() {
                *byte = (step * 19 + index) as u8;
            }
        }
        let external = external_descs
            .iter()
            .map(|desc| {
                (
                    (BigUint::from(step) << desc.width.saturating_sub(1)) + BigUint::from(step % 2),
                    BigUint::from(step % 3) << desc.width.saturating_sub(1),
                )
            })
            .collect::<Vec<_>>();
        sparse
            .dump_with_activity((step / 2) as u64, &memory, &external, Some(&groups))
            .unwrap();
        full.dump_with_external((step / 2) as u64, &memory, &external)
            .unwrap();
    }
    // Re-registering the same external descriptors remains idempotent.
    sparse.add_external_signals(&external_descs).unwrap();
    assert!(sparse.statistics().comparisons < full.statistics().comparisons);
    let parse = |bytes: Vec<u8>| {
        let mut parser = vcd::Parser::new(bytes.as_slice());
        parser.parse_header().unwrap();
        parser.map(Result::unwrap).collect::<Vec<_>>()
    };
    assert_eq!(
        parse(sparse.into_inner().unwrap()),
        parse(full.into_inner().unwrap())
    );
}

#[test]
fn typed_plan_checks_each_dump_and_only_requires_selected_memory() {
    use std::panic::{AssertUnwindSafe, catch_unwind};
    let descs = [
        (0, 64, false),
        (64, 1, false),
        (128, 64, false),
        (192, 64, true),
        (256, 65, false),
        (320, 64, false),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (offset, width, is_4state))| VcdSignalDesc {
        scope: "top".into(),
        name: format!("s{index}"),
        offset,
        width,
        is_4state,
    })
    .collect::<Vec<_>>();
    let mut writer = VcdWriter::from_writer(Vec::new(), &descs);
    writer
        .dump_with_activity(0, &[0; 328], &[], Some(&[]))
        .unwrap();
    writer.dump_with_activity(1, &[], &[], Some(&[])).unwrap();
    writer
        .dump_with_activity(2, &[1; 8], &[], Some(&[0]))
        .unwrap();
    writer
        .dump_with_activity(3, &[1; 65], &[], Some(&[1]))
        .unwrap();
    assert_eq!(writer.statistics().comparisons, 8);
    for (group, short_len) in [(0, 7), (1, 64), (2, 135), (3, 207), (4, 264)] {
        assert!(
            catch_unwind(AssertUnwindSafe(|| {
                writer
                    .dump_with_activity(4, &vec![0; short_len], &[], Some(&[group]))
                    .unwrap();
            }))
            .is_err()
        );
    }
    assert!(catch_unwind(AssertUnwindSafe(|| writer.dump(5, &[0; 327]).unwrap())).is_err());
    let overflowing = VcdSignalDesc {
        offset: usize::MAX,
        ..descs[0].clone()
    };
    assert!(catch_unwind(|| VcdWriter::from_writer(Vec::new(), &[overflowing])).is_err());
}

#[test]
fn initial_snapshot_is_retried_after_a_record_write_error() {
    #[derive(Default)]
    struct FailRecordOnce {
        bytes: Vec<u8>,
        fail: bool,
    }
    impl Write for FailRecordOnce {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.fail
                && self.bytes.last() == Some(&b'\n')
                && matches!(bytes.first(), Some(b'0' | b'1' | b'b'))
            {
                self.fail = false;
                return Err(std::io::ErrorKind::BrokenPipe.into());
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let descs = [64, 1, 9, 64]
        .into_iter()
        .enumerate()
        .map(|(index, width)| VcdSignalDesc {
            scope: "top".into(),
            name: format!("s{index}"),
            offset: index * 64,
            width,
            is_4state: false,
        })
        .collect::<Vec<_>>();
    let mut writer = VcdWriter::from_writer(FailRecordOnce::default(), &descs);
    writer.output.writer = BufWriter::with_capacity(1, FailRecordOnce::default());
    writer.write_header().unwrap();
    writer.flush().unwrap();
    writer.output.writer.get_mut().fail = true;
    assert!(
        writer
            .dump_with_activity(1, &[0; 200], &[], Some(&[]))
            .is_err()
    );
    assert!(!writer.initial_values_written);
    writer
        .dump_with_activity(2, &[0; 200], &[], Some(&[]))
        .unwrap();
    assert!(writer.initial_values_written);
    // Finish the accepted record at its original time, then repeat the
    // incomplete initial snapshot in full at the retry's timestamp.
    assert_eq!(writer.statistics().changes, 5);
    assert_eq!(writer.statistics().comparisons, 5);
    assert_eq!(
        changes(&writer.into_inner().unwrap().bytes),
        [(1, "0".into())]
            .into_iter()
            .chain(vec![(2, "0".into()); 4])
            .collect::<Vec<_>>()
    );
}

#[test]
fn comparison_statistics_count_the_visited_prefix_on_io_error() {
    #[derive(Default)]
    struct FailAfterRecord {
        bytes: Vec<u8>,
        remaining: Option<usize>,
    }
    impl Write for FailAfterRecord {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.bytes.last() == Some(&b'\n')
                && matches!(bytes.first(), Some(b'0' | b'1' | b'b'))
                && let Some(remaining) = &mut self.remaining
            {
                if *remaining == 0 {
                    self.remaining = None;
                    return Err(std::io::ErrorKind::BrokenPipe.into());
                }
                *remaining -= 1;
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    for width in [1, 64, 9] {
        let descs = (0..8)
            .map(|index| VcdSignalDesc {
                scope: "top".into(),
                name: format!("s{index}"),
                offset: index * 16,
                width,
                is_4state: false,
            })
            .collect::<Vec<_>>();
        let mut writer = VcdWriter::from_writer(FailAfterRecord::default(), &descs);
        writer.output.writer = BufWriter::with_capacity(1, FailAfterRecord::default());
        let mut memory = [0; 128];
        writer.dump(0, &memory).unwrap();
        writer.flush().unwrap();
        memory[0] = 1;
        memory[4 * 16] = 1;
        writer.output.writer.get_mut().remaining = Some(1);
        assert!(writer.dump(1, &memory).is_err());
        // Visit indices 0 through 4, including the three unchanged values.
        assert_eq!(writer.statistics().comparisons, 8 + 5, "width={width}");
        assert_eq!(writer.statistics().changes, 8 + 1, "width={width}");
    }
}

#[test]
fn sparse_selection_preserves_aliases_initial_values_and_registration_order() {
    let descs = [128, 0, 129, 0, 512, 640, 768, 896, 1024, 1152]
        .into_iter()
        .enumerate()
        .map(|(index, offset)| VcdSignalDesc {
            scope: "top".into(),
            name: format!("s{index}"),
            offset,
            width: 8,
            is_4state: false,
        })
        .collect::<Vec<_>>();
    let mut sparse = VcdWriter::from_writer(Vec::new(), &descs);
    let mut full = VcdWriter::from_writer(Vec::new(), &descs);
    let mut memory = vec![0; 1153];
    for time in 0..5 {
        memory[0] = time as u8;
        memory[129] = (time * 3) as u8;
        // First sample ignores an empty candidate set; later sets arrive unsorted.
        let groups = if time == 0 { &[][..] } else { &[2, 0, 2][..] };
        sparse
            .dump_with_activity(time, &memory, &[], Some(groups))
            .unwrap();
        full.dump(time, &memory).unwrap();
    }
    assert!(sparse.statistics().comparisons < full.statistics().comparisons);
    let parse = |bytes: Vec<u8>| {
        let mut parser = vcd::Parser::new(bytes.as_slice());
        parser.parse_header().unwrap();
        parser.map(Result::unwrap).collect::<Vec<_>>()
    };
    assert_eq!(
        parse(sparse.into_inner().unwrap()),
        parse(full.into_inner().unwrap())
    );
}

#[test]
fn padding_and_mask_only_changes_have_exact_semantics() {
    let desc = VcdSignalDesc {
        scope: "top".into(),
        name: "q".into(),
        offset: 0,
        width: 1,
        is_4state: true,
    };
    let mut writer = VcdWriter::from_writer(Vec::new(), &[desc]);
    for (time, bytes) in [[0, 0], [0xfe, 0xfe], [0, 1], [1, 1], [1, 0]]
        .iter()
        .enumerate()
    {
        writer.dump(time as u64, bytes).unwrap();
    }
    assert_eq!(
        changes(&writer.into_inner().unwrap()),
        [(0, "0"), (2, "z"), (3, "x"), (4, "1")].map(|(t, s)| (t, s.to_string()))
    );
}

#[derive(Default)]
struct ShortWrites {
    bytes: Vec<u8>,
    calls: usize,
    fail_after: Option<usize>,
}

impl Write for ShortWrites {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.calls += 1;
        if self.calls.is_multiple_of(7) {
            return Err(std::io::ErrorKind::Interrupted.into());
        }
        let remaining = self
            .fail_after
            .unwrap_or(usize::MAX)
            .saturating_sub(self.bytes.len());
        if remaining == 0 && !bytes.is_empty() {
            return Err(std::io::ErrorKind::BrokenPipe.into());
        }
        let len = bytes.len().min(113).min(remaining);
        self.bytes.extend_from_slice(&bytes[..len]);
        Ok(len)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn failed_value_blocks_resume_without_losing_cached_transitions() {
    for width in [1usize, 64, 9, 65, 256 * 1024 + 17] {
        for four_state in [false, true] {
            for capacity in [1, 64] {
                for accepted in [0, 1, 31] {
                    let count = if width > 1024 { 2 } else { 80 };
                    let stride = width.div_ceil(8) * if four_state { 2 } else { 1 };
                    let descs = (0..count)
                        .map(|index| VcdSignalDesc {
                            scope: "top".into(),
                            name: format!("s{index}"),
                            offset: index * stride,
                            width,
                            is_4state: four_state,
                        })
                        .collect::<Vec<_>>();
                    let mut memory = vec![0; count * stride];
                    let mut writer = VcdWriter::from_writer(ShortWrites::default(), &descs);
                    writer.output.writer =
                        BufWriter::with_capacity(capacity, ShortWrites::default());
                    let mut reference = VcdWriter::from_writer(Vec::new(), &descs);
                    writer.dump(0, &memory).unwrap();
                    writer.flush().unwrap();
                    reference.dump(0, &memory).unwrap();

                    // Fail before accepting a value block, or after a short
                    // prefix of it. A block may contain several cache updates.
                    let limit = writer.get_ref().bytes.len() + b"#1\n".len() + accepted;
                    writer.output.writer.get_mut().fail_after = Some(limit);
                    memory.fill(0xff);
                    for _ in 0..2 {
                        assert_eq!(
                            writer.dump(1, &memory).unwrap_err().kind(),
                            std::io::ErrorKind::BrokenPipe
                        );
                    }
                    writer.output.writer.get_mut().fail_after = None;
                    writer.dump(1, &memory).unwrap();
                    reference.dump(1, &memory).unwrap();
                    // Also check that recovery leaves the caches current.
                    writer.dump(2, &memory).unwrap();
                    reference.dump(2, &memory).unwrap();
                    assert_eq!(writer.statistics().changes, reference.statistics().changes);
                    assert_eq!(
                        writer.statistics().value_bytes,
                        reference.statistics().value_bytes
                    );
                    assert_eq!(
                        changes(&writer.into_inner().unwrap().bytes),
                        changes(&reference.into_inner().unwrap()),
                        "width={width}, four_state={four_state}, capacity={capacity}, accepted={accepted}"
                    );
                }
            }
        }
    }
}

#[test]
fn pending_value_records_finish_before_the_next_timestamp() {
    for flush_first in [false, true] {
        let descs = (0..4)
            .map(|index| VcdSignalDesc {
                scope: "top".into(),
                name: format!("s{index}"),
                offset: index * 8,
                width: 64,
                is_4state: false,
            })
            .collect::<Vec<_>>();
        let mut writer = VcdWriter::from_writer(ShortWrites::default(), &descs);
        // Keep the unwritten suffix larger than BufWriter's capacity so it
        // cannot accept another record before surfacing the injected error.
        writer.output.writer = BufWriter::with_capacity(1, ShortWrites::default());
        writer.dump(0, &[0; 32]).unwrap();
        writer.flush().unwrap();
        let limit = writer.get_ref().bytes.len() + b"#1\n".len() + 7;
        writer.output.writer.get_mut().fail_after = Some(limit);
        assert!(writer.dump(1, &[0xff; 32]).is_err());
        writer.output.writer.get_mut().fail_after = None;
        if flush_first {
            writer.flush().unwrap();
        }
        // The memory has changed again since the failed attempt. The old
        // queued record must finish at #1, followed by the new value at #2.
        writer.dump(2, &[0; 32]).unwrap();
        let mut expected = vec![(0, "0".to_owned()); 4];
        expected.extend([(1, "1".repeat(64)), (2, "0".into())]);
        assert_eq!(changes(&writer.into_inner().unwrap().bytes), expected);
    }
}

#[test]
fn header_and_timestamp_resume_after_partial_writes() {
    let desc = VcdSignalDesc {
        scope: "top".into(),
        name: "q".into(),
        offset: 0,
        width: 64,
        is_4state: false,
    };
    for accepted in [0, 1, 17] {
        let mut writer =
            VcdWriter::from_writer(ShortWrites::default(), std::slice::from_ref(&desc));
        writer.output.writer = BufWriter::with_capacity(
            1,
            ShortWrites {
                fail_after: Some(accepted),
                ..Default::default()
            },
        );
        assert!(writer.dump(0, &[0; 8]).is_err());
        writer.output.writer.get_mut().fail_after = None;
        writer.dump(0, &[0; 8]).unwrap();
        writer.flush().unwrap();
        let limit = writer.get_ref().bytes.len() + 2;
        writer.output.writer.get_mut().fail_after = Some(limit);
        assert!(writer.dump(123, &[1; 8]).is_err());
        writer.output.writer.get_mut().fail_after = None;
        writer.dump(123, &[1; 8]).unwrap();
        assert_eq!(
            changes(&writer.into_inner().unwrap().bytes),
            [
                (0, "0".to_owned()),
                (123, format!("{:b}", u64::from_le_bytes([1; 8])))
            ],
        );
    }
}

#[test]
fn blocks_and_oversized_records_survive_short_writes_and_drop() {
    let mut offset = 0;
    let descs = (0..1025)
        .map(|i| {
            let width = if i == 1024 {
                256 * 1024 + 17
            } else {
                [1, 9, 65, 1024][i % 4]
            };
            let is_4state = i % 3 == 0 || i == 1024;
            let desc = VcdSignalDesc {
                scope: "top".into(),
                name: format!("s{i}"),
                offset,
                width,
                is_4state,
            };
            offset += width.div_ceil(8) * if is_4state { 2 } else { 1 };
            desc
        })
        .collect::<Vec<_>>();
    let mut memory = vec![0xff; offset];
    let mut sink = ShortWrites::default();
    let mut expected = Vec::new();
    let stats = {
        let mut writer = VcdWriter::from_writer(&mut sink, &descs);
        writer
            .add_external_signals(&[VcdExternalSignalDesc {
                scope: "component".into(),
                name: "state".into(),
                width: 9,
            }])
            .unwrap();
        for time in 0..4 {
            if time == 1 {
                memory.fill(0);
            } else if time == 3 {
                for desc in &descs {
                    let size = desc.width.div_ceil(8);
                    if desc.is_4state {
                        memory[desc.offset + size..desc.offset + size * 2].fill(0xff);
                    } else {
                        memory[desc.offset..desc.offset + size].fill(0xff);
                    }
                }
            }
            let external = match time {
                0 => (BigUint::from(0x1ffu16), BigUint::from(0x1ffu16)),
                3 => (BigUint::default(), BigUint::from(0x1ffu16)),
                _ => (BigUint::default(), BigUint::default()),
            };
            writer
                .dump_with_external(time, &memory, &[external])
                .unwrap();
            if time != 2 {
                for desc in &descs {
                    let value = if time == 1 {
                        "0".into()
                    } else if desc.is_4state {
                        if time == 0 { "x" } else { "z" }.repeat(desc.width)
                    } else {
                        "1".repeat(desc.width)
                    };
                    expected.push((time, value));
                }
                expected.push((
                    time,
                    match time {
                        0 => "x".repeat(9),
                        3 => "z".repeat(9),
                        _ => "0".into(),
                    },
                ));
            }
        }
        // Leave the final partial block buffered and exercise Drop.
        writer.statistics()
    };
    assert_eq!(changes(&sink.bytes), expected);
    let text = std::str::from_utf8(&sink.bytes).unwrap();
    assert!(text.contains("\n#2\n#3\n"));
    assert_eq!(stats.changes, expected.len() as u64);
    let values = text.split_once("$dumpvars\n$end\n").unwrap().1;
    assert_eq!(
        stats.value_bytes,
        values
            .lines()
            .filter(|line| !line.starts_with('#'))
            .map(|line| (line.len() + 1) as u64)
            .sum::<u64>()
    );
}

#[test]
fn block_and_tail_io_errors_are_reported() {
    for (width, fail_after, fails_during_dump) in
        [(64, 16, false), (512 * 1024, 256 * 1024 + 13, true)]
    {
        let desc = VcdSignalDesc {
            scope: "top".into(),
            name: "q".into(),
            offset: 0,
            width,
            is_4state: false,
        };
        let mut sink = ShortWrites {
            fail_after: Some(fail_after),
            ..Default::default()
        };
        {
            let mut writer = VcdWriter::from_writer(&mut sink, &[desc]);
            let dump = writer.dump(0, &vec![0xff; width.div_ceil(8)]);
            assert_eq!(dump.is_err(), fails_during_dump);
            let error = dump.and_then(|()| writer.flush()).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::BrokenPipe);
        }
        assert_eq!(sink.bytes.len(), fail_after);
    }
}

#[test]
fn explicit_flush_reports_sink_failure() {
    struct BadFlush;
    impl Write for BadFlush {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Err(std::io::Error::other("flush failed"))
        }
    }
    let mut writer = VcdWriter::from_writer(BadFlush, &[]);
    writer.dump(0, &[]).unwrap();
    assert_eq!(writer.flush().unwrap_err().to_string(), "flush failed");
}

#[test]
fn rejects_timestamps_earlier_than_the_last_dump() {
    let desc = VcdSignalDesc {
        scope: "top".into(),
        name: "q".into(),
        offset: 0,
        width: 8,
        is_4state: false,
    };
    let mut writer = VcdWriter::from_writer(Vec::new(), &[desc]);
    writer.dump(5, &[1]).unwrap();
    writer.dump(5, &[2]).unwrap();
    let error = writer.dump(4, &[3]).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
    writer.dump(6, &[4]).unwrap();
    let bytes = writer.into_inner().unwrap();
    assert_eq!(
        changes(&bytes),
        [(5, "1".into()), (5, "10".into()), (6, "100".into())]
    );
}
