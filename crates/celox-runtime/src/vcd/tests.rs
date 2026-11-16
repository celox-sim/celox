use super::*;

#[test]
fn limited_checkpoints_preserve_on_off_state_when_recording_resumes() {
    for command in ["dumpoff", "dumpon"] {
        let descs = [VcdSignalDesc {
            scope: "Top".into(),
            name: "value".into(),
            offset: 0,
            width: 1,
            is_4state: false,
        }];
        let mut writer = VcdWriter::from_writer(Vec::new(), &descs);
        writer.dump(0, &[0]).unwrap();
        if command == "dumpon" {
            writer.checkpoint("dumpoff", 1, &[0], &[]).unwrap();
        }
        writer.set_byte_limit(1);
        writer.checkpoint(command, 2, &[1], &[]).unwrap();
        writer.set_byte_limit(0);
        writer.dump(3, &[1]).unwrap();
        let output = String::from_utf8(writer.into_inner().unwrap()).unwrap();
        assert_eq!(
            output.contains("#3\n1"),
            command == "dumpon",
            "{command}: {output}"
        );
        if command == "dumpoff" {
            assert!(
                !output.contains("#3"),
                "off under the limit must remain suspended: {output}"
            );
        }
    }
}

#[test]
fn byte_limit_pauses_header_at_command_boundaries_and_resumes_once() {
    let descs = (0..100)
        .map(|index| VcdSignalDesc {
            scope: "Top".into(),
            name: format!("signal_{index}"),
            offset: index,
            width: 1,
            is_4state: false,
        })
        .collect::<Vec<_>>();
    let mut reference = VcdWriter::from_writer(Vec::new(), &descs);
    let records = reference.prepare_header();
    let max_record = records.iter().map(Vec::len).max().unwrap();
    let header_size = records.iter().map(Vec::len).sum::<usize>();
    let mut boundary = 0;
    let mut limits = vec![1];
    for record in &records {
        boundary += record.len();
        limits.extend([boundary.saturating_sub(1), boundary]);
    }
    for limit in limits {
        let mut writer = VcdWriter::from_writer(Vec::new(), &descs);
        writer.set_byte_limit(limit as u64);
        writer.dump(0, &[0; 100]).unwrap();
        writer.flush().unwrap();
        let paused = String::from_utf8(writer.get_ref().clone()).unwrap();
        assert!(
            paused.len() <= limit + max_record + 36,
            "{limit}/{header_size}: {paused}"
        );
        assert!(
            paused.contains("VCD dump limit reached"),
            "{limit}: {paused}"
        );
        assert!(!paused.contains("#0"), "{limit}: {paused}");
        assert_eq!(writer.statistics().changes, 0);
        writer.set_byte_limit(0);
        writer.dump(5, &[1; 100]).unwrap();
        let output = String::from_utf8(writer.into_inner().unwrap()).unwrap();
        assert_eq!(output.matches("$date\n").count(), 1, "{limit}: {output}");
        assert_eq!(
            output.matches("$var wire ").count(),
            100,
            "{limit}: {output}"
        );
        assert_eq!(
            output.matches("$enddefinitions $end").count(),
            1,
            "{limit}: {output}"
        );
        let (_, values) = output.split_once("#5\n").unwrap();
        assert_eq!(
            values.lines().filter(|line| line.starts_with('1')).count(),
            100,
            "{limit}: {output}"
        );
    }
}

#[test]
fn header_limit_keeps_checkpoint_suspension_and_external_registration() {
    let descs = [VcdSignalDesc {
        scope: "Top".into(),
        name: "state".into(),
        offset: 0,
        width: 1,
        is_4state: false,
    }];
    let mut writer = VcdWriter::from_writer(Vec::new(), &descs);
    writer.set_byte_limit(1);
    writer.dump(0, &[0]).unwrap();
    assert!(
        writer
            .add_external_signals(&[VcdExternalSignalDesc {
                scope: "Top".into(),
                name: "late".into(),
                width: 1
            }])
            .is_err()
    );
    writer.checkpoint("dumpoff", 1, &[1], &[]).unwrap();
    writer.set_byte_limit(0);
    writer.dump(2, &[1]).unwrap();
    writer.checkpoint("dumpon", 3, &[1], &[]).unwrap();
    let output = String::from_utf8(writer.into_inner().unwrap()).unwrap();
    assert!(!output.contains("#2"), "{output}");
    assert!(output.contains("#3\n$dumpon\n1!\n$end"), "{output}");
}

#[test]
fn byte_limit_stops_initial_changes_and_checkpoint_at_one_record() {
    for width in [1usize, 4, 64] {
        let bytes = width.div_ceil(8);
        let descs = (0..100)
            .map(|i| VcdSignalDesc {
                scope: "Top".into(),
                name: format!("s{i}"),
                offset: i * bytes,
                width,
                is_4state: false,
            })
            .collect::<Vec<_>>();
        for command in ["initial", "changes", "dumpall", "dumpoff", "dumpon"] {
            let mut writer = VcdWriter::from_writer(Vec::new(), &descs);
            let mut memory = vec![0u8; bytes * descs.len()];
            writer.write_header().unwrap();
            if command != "initial" {
                writer.dump(0, &memory).unwrap();
            }
            let before = writer.statistics().changes;
            writer.output.write_encoded().unwrap();
            // Leave room for the timestamp/command and one record, but not
            // for the complete hundred-signal snapshot or checkpoint.
            let allowance = if command.starts_with("dump") { 22 } else { 6 };
            let limit = writer.output.accepted_bytes + allowance;
            writer.set_byte_limit(limit);
            memory.fill(255);
            if command.starts_with("dump") {
                writer.checkpoint(command, 1, &memory, &[]).unwrap();
            } else {
                writer.dump(1, &memory).unwrap();
            }
            writer.flush().unwrap();
            let records = writer.statistics().changes - before;
            assert!(
                records < 10,
                "{command}/{width}: emitted {records} records past a small limit"
            );
            let output = String::from_utf8(writer.into_inner().unwrap()).unwrap();
            assert!(
                output.contains("VCD dump limit reached"),
                "{command}/{width}: {output}"
            );
            let max_record = width.max(1) as u64 + 5;
            assert!(
                output.len() as u64 <= limit + max_record + 5 + 36,
                "{command}/{width}: {} exceeds bound {limit}",
                output.len()
            );
        }
    }
}

#[test]
fn system_task_checkpoints_and_limit() {
    let descs = [VcdSignalDesc {
        scope: "Top".into(),
        name: "value".into(),
        offset: 0,
        width: 4,
        is_4state: true,
    }];
    let mut writer = VcdWriter::from_writer(Vec::new(), &descs);
    writer.dump(0, &[0b1011, 0b0011]).unwrap();
    writer.checkpoint("dumpoff", 1, &[15, 0], &[]).unwrap();
    writer.dump(2, &[0, 0]).unwrap();
    writer.checkpoint("dumpon", 3, &[15, 0], &[]).unwrap();
    writer.checkpoint("dumpall", 4, &[15, 0], &[]).unwrap();
    writer.set_byte_limit(1);
    writer.dump(5, &[0, 0]).unwrap();
    let output = String::from_utf8(writer.into_inner().unwrap()).unwrap();
    assert!(output.contains("b10xx "));
    assert!(output.contains("$dumpoff\nbxxxx "));
    assert!(!output.contains("#2"));
    assert!(output.contains("$dumpon\nb1111 "));
    assert!(output.contains("$dumpall\nb1111 "));
    assert!(!output.contains("#5"));
    assert!(output.contains("VCD dump limit reached"));
}

#[test]
fn headers_encode_supported_time_precisions() {
    for (exponent, expected) in [
        (2, "100s"),
        (0, "1s"),
        (-1, "100ms"),
        (-5, "10us"),
        (-9, "1ns"),
        (-12, "1ps"),
        (-14, "10fs"),
        (-15, "1fs"),
    ] {
        let precision = celox_design::TimePrecision::try_from(exponent).unwrap();
        let mut writer = VcdWriter::from_writer_with_time_precision(Vec::new(), &[], precision);
        writer.dump_with_external(123, &[], &[]).unwrap();
        writer.flush().unwrap();
        let output = std::str::from_utf8(writer.get_ref()).unwrap();
        assert!(
            output.contains(&format!("$timescale {expected} $end")),
            "{output}"
        );
        assert!(output.contains("#123\n"));
    }
    assert!(celox_design::TimePrecision::try_from(-16).is_err());
    assert!(celox_design::TimePrecision::try_from(3).is_err());
}

#[test]
fn appended_external_signals_survive_restart() {
    let dir = tempfile::tempdir().unwrap();
    let first_path = dir.path().join("first.vcd");
    let second_path = dir.path().join("second.vcd");
    let desc = |name: &str, width| VcdExternalSignalDesc {
        scope: "component".into(),
        name: name.into(),
        width,
    };
    let descriptors = [desc("state", 8), desc("count", 4), desc("ready", 1)];
    let mut writer = VcdWriter::new(&first_path, &[]).unwrap();
    writer.add_external_signals(&descriptors[..2]).unwrap();
    writer.add_external_signals(&descriptors[..2]).unwrap();
    writer.add_external_signals(&descriptors[2..]).unwrap();
    writer.add_external_signals(&descriptors).unwrap();
    writer.add_external_signals(&[]).unwrap();
    let values = [
        (BigUint::from(0xabu8), BigUint::default()),
        (BigUint::from(5u8), BigUint::default()),
        (BigUint::from(1u8), BigUint::default()),
    ];
    writer.dump_with_external(0, &[], &values).unwrap();
    writer.add_external_signals(&descriptors).unwrap();
    assert!(writer.add_external_signals(&[desc("late", 1)]).is_err());
    writer.restart(&second_path).unwrap();
    writer.dump_with_external(0, &[], &values).unwrap();
    writer.flush().unwrap();

    // Restart changes the date, but preserves declarations and values.
    let records = |path: &std::path::Path| {
        std::fs::read_to_string(path)
            .unwrap()
            .split("$version")
            .nth(1)
            .unwrap()
            .to_owned()
    };
    assert_eq!(records(&first_path), records(&second_path));
}

#[test]
fn external_values_are_masked_to_their_declared_width() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("external-width.vcd");
    let mut writer = VcdWriter::new(&path, &[]).unwrap();
    writer
        .add_external_signals(&[VcdExternalSignalDesc {
            scope: "component".into(),
            name: "state".into(),
            width: 8,
        }])
        .unwrap();

    writer
        .dump_with_external(
            0,
            &[],
            &[(BigUint::from(0x1ffu16), BigUint::from(0x100u16))],
        )
        .unwrap();
    writer
        .dump_with_external(1, &[], &[(BigUint::from(0xffu8), BigUint::default())])
        .unwrap();
    writer
        .dump_with_external(2, &[], &[(BigUint::default(), BigUint::from(0xffu8))])
        .unwrap();
    writer
        .dump_with_external(3, &[], &[(BigUint::default(), BigUint::default())])
        .unwrap();

    writer.flush().unwrap();
    let dump = std::fs::read_to_string(path).unwrap();
    assert!(!dump.contains("b111111111"), "{dump}");
    assert_eq!(dump.matches("b11111111 !").count(), 1, "{dump}");
    assert_eq!(dump.matches("bzzzzzzzz !").count(), 1, "{dump}");
    assert_eq!(dump.matches("b0 !").count(), 1, "{dump}");
}
