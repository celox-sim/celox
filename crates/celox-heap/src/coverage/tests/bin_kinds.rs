//! Bin arrays, `default`, `ignore_bins`, `illegal_bins` and wildcard bins,
//! with the filters, sets and predicates of their declarations.

use super::*;

/// `bins name[] = …`.
fn open(mut bin: BinDescriptor) -> BinDescriptor {
    bin.array = BinArray::Open;
    bin
}

/// `bins name[count] = …`.
fn fixed(mut bin: BinDescriptor, count: Operand) -> BinDescriptor {
    bin.array = BinArray::Fixed(count);
    bin
}

fn ignore(mut bin: BinDescriptor) -> BinDescriptor {
    bin.kind = BinKind::Ignore;
    bin
}

fn illegal(mut bin: BinDescriptor) -> BinDescriptor {
    bin.kind = BinKind::Illegal;
    bin
}

fn wildcard(mut bin: BinDescriptor) -> BinDescriptor {
    bin.wildcard = true;
    bin
}

/// `… iff (predicate bit)`.
fn guarded(mut bin: BinDescriptor, bit: u32) -> BinDescriptor {
    bin.iff = Some(bit);
    bin
}

/// `… with (…)`.
fn filtered(mut bin: BinDescriptor, with: BinFilter) -> BinDescriptor {
    let BinValues::Ranges { filter, .. } = &mut bin.values else {
        panic!("a filter of no range list");
    };
    *filter = Some(with);
    bin
}

/// `bins name = default`.
fn default_bins(name: &str) -> BinDescriptor {
    BinDescriptor::new(name, BinValues::Default)
}

/// The unsigned literal of the binary digits `digits`, where `x`, `z` and
/// `?` are what they are in source code.
fn pattern(digits: &str) -> Constant {
    let mut value = BigUint::ZERO;
    let mut mask = BigUint::ZERO;
    for digit in digits.chars().filter(|&digit| digit != '_') {
        value <<= 1;
        mask <<= 1;
        match digit {
            '0' => {}
            '1' => value |= BigUint::from(1u8),
            'x' => {
                value |= BigUint::from(1u8);
                mask |= BigUint::from(1u8);
            }
            'z' | '?' => mask |= BigUint::from(1u8),
            other => panic!("no binary digit: {other}"),
        }
    }
    Constant {
        ty: ValueType {
            width: digits.chars().filter(|&digit| digit != '_').count() as u32,
            signed: false,
        },
        value,
        mask,
    }
}

fn signed(mut constant: Constant) -> Constant {
    constant.ty.signed = true;
    constant
}

/// The filter of a bin of a coverpoint of type `ty`.
fn item_filter(ty: &PointType) -> FilterDescriptor {
    FilterDescriptor::new(vec![ValueType {
        width: ty.width,
        signed: ty.signed,
    }])
}

fn four_state(width: u32, signed: bool) -> PointType {
    PointType {
        four_state: true,
        ..point_type(width, signed)
    }
}

/// A new instance of the type of `key` with the `int` inputs `inputs`,
/// which is not built yet.
fn start(heap: &mut Heap, key: u64, inputs: &[i64]) -> Handle {
    let object = call(heap, op::NEW, 0, 0, [key, 8, CLASS.into()]);
    for (index, &input) in inputs.iter().enumerate() {
        let word = input as u32 as u64;
        call(heap, op::INPUT, index as u64, 0, [object, word, 0]);
    }
    object as Handle
}

/// Build `object`: the number of warnings.
fn finish(heap: &mut Heap, object: Handle) -> u64 {
    let warnings = call(heap, op::BUILD, 0, 0, [object.into(), 0, 0]);
    assert_eq!(heap.take_fault(), None);
    warnings
}

/// Run filter `filter` of the construction of `object` as the code of a
/// constructor does, with `keep` as the `with` expression of the low word
/// of a value: the low words of the values it was asked about.
fn ask(heap: &mut Heap, object: Handle, filter: u64, keep: impl Fn(u64) -> bool) -> Vec<u64> {
    let mut asked = Vec::new();
    let mut kept = 0;
    while call(heap, op::FILTER_NEXT, filter, 0, [object.into(), kept, 0]) != 0 {
        let item = call(heap, op::FILTER_ITEM, filter, 0, [object.into(), 0, 0]);
        asked.push(item);
        kept = keep(item).into();
    }
    asked
}

/// The fault of the last operations, as a message shows it.
fn fault(heap: &mut Heap) -> Option<String> {
    heap.take_fault().map(|fault| fault.to_string())
}

/// Sample the 2-state `values` of the coverpoints in turn: the number of
/// illegal bins hit.
fn sample_illegal(heap: &mut Heap, object: Handle, values: &[u64]) -> u64 {
    for (index, value) in values.iter().enumerate() {
        let item = index as u64 + 1;
        call(heap, op::POINT, item, 0, [object.into(), *value, 0]);
    }
    let hits = call(heap, op::SAMPLE_END, 0, 0, [object.into(), 0, 0]);
    assert_eq!(heap.take_fault(), None);
    hits
}

/// Sample the value of bits `value` with the x and z bits `mask` for each
/// of the first `points` coverpoints: the number of illegal bins hit.
fn sample_planes(heap: &mut Heap, object: Handle, points: u64, value: u64, mask: u64) -> u64 {
    for item in 1..=points {
        call(heap, op::POINT, item, 0, [object.into(), value, mask]);
    }
    let hits = call(heap, op::SAMPLE_END, 0, 0, [object.into(), 0, 0]);
    assert_eq!(heap.take_fault(), None);
    hits
}

/// The names, kinds and hits of the bins of coverpoint `point` of instance
/// `instance` of the first covergroup of the report.
fn listed(heap: &Heap, instance: usize, point: usize) -> Vec<(String, BinReportKind, u64)> {
    heap.coverage_report().covergroups[0].instances[instance].items[point]
        .bins
        .iter()
        .map(|bin| (bin.name.clone(), bin.kind, bin.hits))
        .collect()
}

fn hits(heap: &Heap, instance: usize, point: usize, name: &str) -> u64 {
    listed(heap, instance, point)
        .into_iter()
        .find(|bin| bin.0 == name)
        .unwrap_or_else(|| panic!("no bin `{name}`"))
        .2
}

fn names(heap: &Heap, instance: usize, point: usize) -> Vec<String> {
    listed(heap, instance, point)
        .into_iter()
        .map(|bin| bin.0)
        .collect()
}

fn text(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| name.to_string()).collect()
}

#[test]
fn a_fixed_number_of_bins_takes_the_values_in_their_order() {
    let mut heap = Heap::new();
    let mut descriptor = group(
        "g",
        vec![
            // "bins fixed [4] = {[1:10], 1, 4, 7};"
            point(
                "four",
                point_type(4, false),
                vec![fixed(
                    bins(
                        "fixed",
                        vec![
                            range(int(1), int(10)),
                            value(int(1)),
                            value(int(4)),
                            value(int(7)),
                        ],
                    ),
                    Operand::Constant(int(4)),
                )],
            ),
            // "bins fixed [5] = {1, 4, 7};"
            point(
                "five",
                point_type(4, false),
                vec![fixed(
                    bins("fixed", vec![value(int(1)), value(int(4)), value(int(7))]),
                    Operand::Constant(int(5)),
                )],
            ),
            point(
                "sized",
                point_type(3, false),
                vec![fixed(
                    bins("f", vec![range(int(0), int(7))]),
                    Operand::Input(0),
                )],
            ),
        ],
    );
    descriptor.inputs = int_inputs(1);
    let key = register(&mut heap, descriptor);
    let object = construct(&mut heap, key, &[3]);
    for value in 0..=10 {
        sample(&mut heap, object, &[value, value, value & 7]);
    }
    // "B = 13/4 = 3. The 13 values are distributed as follows: <1,2,3>,
    // <4,5,6>, <7,8,9>, <10,1,4,7>": 1, 4 and 7 are values of two bins.
    assert_eq!(
        instance_bins(&heap, 0, 0),
        [
            ("fixed[0]".to_string(), 3),
            ("fixed[1]".to_string(), 3),
            ("fixed[2]".to_string(), 3),
            ("fixed[3]".to_string(), 4),
        ]
    );
    // "B = 1. The 3 values are distributed as follows: <1>, <4>, <7>, < >,
    // < >": the empty bins are no bins of the coverage.
    assert_eq!(
        instance_bins(&heap, 0, 1),
        [
            ("fixed[0]".to_string(), 1),
            ("fixed[1]".to_string(), 1),
            ("fixed[2]".to_string(), 1),
        ]
    );
    assert_eq!(inst_counts(&mut heap, object, 2), (3, 3));
    // Eight values in three bins: two each, and the last takes the rest.
    // The samples were 0 to 7 and again 0 to 2.
    assert_eq!(
        instance_bins(&heap, 0, 2),
        [
            ("f[0]".to_string(), 4),
            ("f[1]".to_string(), 3),
            ("f[2]".to_string(), 4),
        ]
    );
    // The number of bins is positive, and no more than an item may have.
    for (size, reason) in [
        (0, "has the size 0"),
        (-1, "has the size -1"),
        (MAX_BINS_PER_ITEM as i64 + 1, "has more than 1048576 bins"),
    ] {
        let object = start(&mut heap, key, &[size]);
        call(&mut heap, op::BUILD, 0, 0, [object.into(), 0, 0]);
        assert_eq!(
            fault(&mut heap),
            Some(format!(
                "covergroup: type `g`: bins `f` of coverpoint `sized` {reason}"
            ))
        );
    }
}

#[test]
fn the_bin_groupings_of_19_5_1() {
    let mut heap = Heap::new();
    // bit [9:0] v_a;
    //   bins a   = { [0:63],65 };
    //   bins b[] = { [127:150],[148:191] };
    //   bins c[] = { 200,201,202 };
    //   bins d   = { [1000:$] };
    //   bins others[] = default;
    let key = register(
        &mut heap,
        group(
            "cg",
            vec![point(
                "v_a",
                point_type(10, false),
                vec![
                    bins("a", vec![range(int(0), int(63)), value(int(65))]),
                    open(bins(
                        "b",
                        vec![range(int(127), int(150)), range(int(148), int(191))],
                    )),
                    open(bins(
                        "c",
                        vec![value(int(200)), value(int(201)), value(int(202))],
                    )),
                    bins(
                        "d",
                        vec![RangeDescriptor::Range {
                            low: Bound::Operand(Operand::Constant(int(1000))),
                            high: Bound::Open,
                        }],
                    ),
                    open(default_bins("others")),
                ],
            )],
        ),
    );
    let object = construct(&mut heap, key, &[]);
    // "The second bins construct creates a set of 65 bins b[127] . . .
    // b[191]", "3 bins: c[200], c[201], and c[202]": with a and d 70 bins
    // of the coverage, and no automatic bin beside them.
    assert_eq!(inst_counts(&mut heap, object, 1), (0, 70));
    let declared = names(&heap, 0, 0);
    assert_eq!(declared.len(), 70);
    assert_eq!(declared[..3], text(&["a", "b[127]", "b[128]"]));
    assert_eq!(
        declared[65..],
        text(&["b[191]", "c[200]", "c[201]", "c[202]", "d"])
    );
    for value in [64, 65, 148, 150, 1023, 999, 64] {
        sample(&mut heap, object, &[value]);
    }
    // A value that the list of `b` has twice has one bin, which a sample
    // hits once.
    for (name, count) in [
        ("a", 1),
        ("b[148]", 1),
        ("b[150]", 1),
        ("b[149]", 0),
        ("d", 1),
    ] {
        assert_eq!(hits(&heap, 0, 0, name), count, "{name}");
    }
    // "Every value that does not match bins a, b[], c[], or d is added
    // into its own distinct bin in the others[] bin array", which takes no
    // part in the coverage (19.5).
    assert_eq!(
        listed(&heap, 0, 0)[70..],
        [
            ("others[64]".to_string(), BinReportKind::Default, 2),
            ("others[999]".to_string(), BinReportKind::Default, 1),
        ]
    );
    assert_eq!(inst_counts(&mut heap, object, 1), (4, 70));
    assert_close(inst_coverage(&mut heap, object, 1), 400.0 / 70.0);
    // The bins of the type are those of its instances.
    let report = heap.coverage_report();
    assert_eq!(
        report.covergroups[0].items[0].bins,
        report.covergroups[0].instances[0].items[0].bins
    );
}

#[test]
fn wildcard_bins_hold_every_value_of_their_pattern() {
    let mut heap = Heap::new();
    let key = register(
        &mut heap,
        group(
            "g",
            vec![
                point(
                    "logic",
                    four_state(4, false),
                    vec![
                        // "wildcard bins g12_15 = { 4'b11?? };"
                        wildcard(bins("g12_15", vec![value(pattern("11??"))])),
                        // "wildcard bins g12_15_array[] = { 4'b11?? };"
                        wildcard(open(bins("g12_15_array", vec![value(pattern("11??"))]))),
                        wildcard(open(bins("odd", vec![value(pattern("1xz1"))]))),
                    ],
                ),
                point(
                    "signed",
                    point_type(4, true),
                    vec![wildcard(open(bins("neg", vec![value(pattern("1?0?"))])))],
                ),
            ],
        ),
    );
    // Neither type warns of the x and z bits (19.5.7 b) 3).
    let object = construct(&mut heap, key, &[]);
    assert_eq!(
        names(&heap, 0, 0),
        text(&[
            "g12_15",
            "g12_15_array[12]",
            "g12_15_array[13]",
            "g12_15_array[14]",
            "g12_15_array[15]",
            "odd[9]",
            "odd[11]",
            "odd[13]",
            "odd[15]",
        ])
    );
    // The values of a signed coverpoint are in their order.
    assert_eq!(
        names(&heap, 0, 1),
        text(&["neg[-8]", "neg[-7]", "neg[-4]", "neg[-3]"])
    );
    // "The count of bin g12_15 is incremented when the sampled variable is
    // between 12 and 15".
    for value in [12, 13, 14, 15, 11, 9, 3] {
        sample(&mut heap, object, &[value, value]);
    }
    // "sampled values containing x or z are excluded".
    assert_eq!(sample_planes(&mut heap, object, 2, 0b1100, 0b0001), 0);
    assert_eq!(hits(&heap, 0, 0, "g12_15"), 4);
    for name in [
        "g12_15_array[12]",
        "g12_15_array[13]",
        "g12_15_array[14]",
        "g12_15_array[15]",
        "odd[9]",
        "odd[11]",
        "odd[13]",
        "odd[15]",
    ] {
        assert_eq!(hits(&heap, 0, 0, name), 1, "{name}");
    }
    // The samples 12 and 13 are -4 and -3, 9 is -7.
    assert_eq!(
        instance_bins(&heap, 0, 1),
        [
            ("neg[-8]".to_string(), 0),
            ("neg[-7]".to_string(), 1),
            ("neg[-4]".to_string(), 1),
            ("neg[-3]".to_string(), 1),
        ]
    );
}

#[test]
fn wildcard_values_are_resolved_as_the_values_they_stand_for() {
    let mut heap = Heap::new();
    let key = register(
        &mut heap,
        group(
            "g",
            vec![point(
                "p",
                point_type(4, false),
                vec![
                    // Every value it stands for is one of the type.
                    wildcard(bins("fits", vec![value(pattern("0011??"))])),
                    // Half of them are.
                    wildcard(bins("half", vec![value(pattern("?011??"))])),
                    // None is.
                    wildcard(bins("none", vec![value(pattern("0111??"))])),
                    // Signed and negative on an unsigned type (19.5.7 b) 1).
                    wildcard(bins("negative", vec![value(signed(pattern("1???")))])),
                    wildcard(bins("either", vec![value(signed(pattern("??11")))])),
                    // A range holds what the ranges of all values of its
                    // bounds hold: 1 to 6, and 0 to 31.
                    wildcard(bins("bound", vec![range(pattern("00?1"), pattern("01?0"))])),
                    wildcard(bins(
                        "wide",
                        vec![range(pattern("00???"), pattern("1????"))],
                    )),
                ],
            )],
        ),
    );
    let (object, warnings) = construct_with_warnings(&mut heap, key, &[]);
    assert_eq!(warnings, 5);
    let subject =
        |name: &str| format!("bins `{name}` of coverpoint `p` of covergroup `g` (instance `g#0`)");
    assert_eq!(
        message(&mut heap, object),
        [
            format!(
                "{}: wildcard value 6'bz011zz is limited to the values of the coverpoint type \
                 (IEEE 1800-2023 19.5.7)",
                subject("half")
            ),
            format!(
                "{}: wildcard value 6'b0111zz stands for no value of the coverpoint type and \
                 does not take part (IEEE 1800-2023 19.5.7)",
                subject("none")
            ),
            format!(
                "{}: wildcard value 4'b1zzz stands for no value of the coverpoint type and does \
                 not take part (IEEE 1800-2023 19.5.7)",
                subject("negative")
            ),
            format!(
                "{}: wildcard value 4'bzz11 is limited to the values of the coverpoint type \
                 (IEEE 1800-2023 19.5.7)",
                subject("either")
            ),
            format!(
                "{}: range [0:31] is limited to the values of the coverpoint type (IEEE \
                 1800-2023 19.5.7)",
                subject("wide")
            ),
        ]
        .join("; ")
    );
    for value in 0..16 {
        sample(&mut heap, object, &[value]);
    }
    // 12 to 15, the same, and 3 and 7: 11 and 15 are negative as `b` has
    // them.
    assert_eq!(
        instance_bins(&heap, 0, 0),
        [
            ("fits".to_string(), 4),
            ("half".to_string(), 4),
            ("either".to_string(), 2),
            ("bound".to_string(), 6),
            ("wide".to_string(), 16),
        ]
    );
    assert_eq!(inst_counts(&mut heap, object, 1), (5, 5));

    // On a signed coverpoint a signed value is extended by its sign, and
    // the values of either sign are resolved for themselves; an unsigned
    // value is extended by 0.
    let key = register(
        &mut heap,
        group(
            "s",
            vec![point(
                "p",
                point_type(4, true),
                vec![
                    wildcard(open(bins("neg", vec![value(signed(pattern("1111??")))]))),
                    wildcard(open(bins("part", vec![value(signed(pattern("??10??")))]))),
                    wildcard(open(bins("ext", vec![value(signed(pattern("?1")))]))),
                    wildcard(open(bins("zero", vec![value(pattern("?1"))]))),
                    // The lowest value of a signed bound has its sign
                    // set: -4 to 7.
                    wildcard(bins(
                        "from_negative",
                        vec![range(signed(pattern("?1??")), signed(pattern("?1??")))],
                    )),
                ],
            )],
        ),
    );
    let (object, warnings) = construct_with_warnings(&mut heap, key, &[]);
    assert_eq!(warnings, 1);
    // 6'sb0?10?? is 8 to 11, which the type has not; 6'sb1?10?? is -8 to
    // -5 where the free bit is 1.
    assert_eq!(
        message(&mut heap, object),
        "bins `part` of coverpoint `p` of covergroup `s` (instance `s#0`): wildcard value \
         6'bzz10zz is limited to the values of the coverpoint type (IEEE 1800-2023 19.5.7)"
    );
    assert_eq!(
        heap.coverage_report().covergroups[1].instances[0].items[0]
            .bins
            .iter()
            .map(|bin| bin.name.clone())
            .collect::<Vec<_>>(),
        text(&[
            "neg[-4]",
            "neg[-3]",
            "neg[-2]",
            "neg[-1]",
            "part[-8]",
            "part[-7]",
            "part[-6]",
            "part[-5]",
            "ext[-1]",
            "ext[1]",
            "zero[1]",
            "zero[3]",
            "from_negative",
        ])
    );
    for value in 0..16 {
        sample(&mut heap, object, &[value]);
    }
    assert_eq!(
        heap.coverage_report().covergroups[1].instances[0].items[0].bins[12].hits,
        12
    );
    // What the analyzer asks about a constant value: the bits of a
    // wildcard value are no warning, what it stands for may be.
    let ty = point_type(4, false);
    let wild = pattern("11??");
    assert_eq!(
        resolve_warnings(&ty, true, RangeValue::Value(&wild)),
        Vec::<String>::new()
    );
    assert_eq!(
        resolve_warnings(&ty, false, RangeValue::Value(&wild)),
        ["value 4'b11zz has x or z bits and does not take part (IEEE 1800-2023 19.5.7)"]
    );
    assert_eq!(
        resolve_warnings(&ty, true, RangeValue::Value(&pattern("1_11??"))),
        [
            "wildcard value 5'b111zz stands for no value of the coverpoint type and does not take \
          part (IEEE 1800-2023 19.5.7)"
        ]
    );
    assert_eq!(
        resolve_warnings(&ty, true, RangeValue::Value(&int(16))),
        resolve(&ty, RangeValue::Value(&int(16))).warnings
    );
}

/// A range compares its bounds in their common type (11.8.1): a signed
/// bound beside an unsigned one is read without its sign. The lowest and
/// the highest value that a wildcard bound stands for are those of that
/// reading.
#[test]
fn wildcard_range_bounds_are_read_in_their_common_type() {
    let mut heap = Heap::new();
    let key = register(
        &mut heap,
        group(
            "g",
            vec![point(
                "p",
                point_type(4, false),
                vec![
                    // 4'sb0000 and 4'sb1000, which is 8 beside 4'hf: the
                    // ranges 0 to 15 and 8 to 15.
                    wildcard(bins(
                        "low",
                        vec![range(signed(pattern("?000")), pattern("1111"))],
                    )),
                    // 4'sb0111 and 4'sb1111, which is 15 beside 4'h0: the
                    // ranges 0 to 7 and 0 to 15.
                    wildcard(bins(
                        "high",
                        vec![range(pattern("0000"), signed(pattern("?111")))],
                    )),
                    // Both signed: -8 to 7, of which the type has 0 to 7.
                    wildcard(bins(
                        "both",
                        vec![range(signed(pattern("?000")), signed(pattern("?111")))],
                    )),
                    // Beside `$` a bound keeps its sign: -8 is below the
                    // type, so the range is all of it.
                    wildcard(bins(
                        "open",
                        vec![RangeDescriptor::Range {
                            low: Bound::Operand(Operand::Constant(signed(pattern("?000")))),
                            high: Bound::Open,
                        }],
                    )),
                ],
            )],
        ),
    );
    let (object, warnings) = construct_with_warnings(&mut heap, key, &[]);
    assert_eq!(warnings, 3);
    let subject =
        |name: &str| format!("bins `{name}` of coverpoint `p` of covergroup `g` (instance `g#0`)");
    // `low` warns of nothing: its lowest value is 0 in either reading.
    assert_eq!(
        message(&mut heap, object),
        [
            format!(
                "{}: bound -1 of range [0:-1] is read as 15 beside an unsigned bound (IEEE \
                 1800-2023 19.5.7)",
                subject("high")
            ),
            format!(
                "{}: range [-8:7] is limited to the values of the coverpoint type (IEEE \
                 1800-2023 19.5.7)",
                subject("both")
            ),
            format!(
                "{}: range [-8:$] is limited to the values of the coverpoint type (IEEE \
                 1800-2023 19.5.7)",
                subject("open")
            ),
        ]
        .join("; ")
    );
    for value in 0..16 {
        sample(&mut heap, object, &[value]);
    }
    assert_eq!(
        instance_bins(&heap, 0, 0),
        [
            ("low".to_string(), 16),
            ("high".to_string(), 16),
            ("both".to_string(), 8),
            ("open".to_string(), 16),
        ]
    );
}

#[test]
fn wildcard_values_are_listed_in_the_order_of_their_values() {
    let mut heap = Heap::new();
    let key = register(
        &mut heap,
        group(
            "g",
            vec![
                point(
                    "s",
                    point_type(4, true),
                    vec![
                        // 0 and -8: the sign is a bit as any other.
                        wildcard(bins("sign", vec![value(pattern("?000"))])),
                        // Every value, as one range.
                        wildcard(bins("any", vec![value(pattern("????"))])),
                        wildcard(open(bins("each", vec![value(pattern("?00?"))]))),
                    ],
                ),
                // 9, 11, 13 and 15 for two bins: two each, the lowest
                // first.
                point(
                    "u",
                    point_type(4, false),
                    vec![wildcard(fixed(
                        bins("w", vec![value(pattern("1??1"))]),
                        Operand::Constant(int(2)),
                    ))],
                ),
            ],
        ),
    );
    let object = construct(&mut heap, key, &[]);
    for value in 0..16 {
        sample(&mut heap, object, &[value, value]);
    }
    assert_eq!(
        instance_bins(&heap, 0, 0),
        [
            ("sign".to_string(), 2),
            ("any".to_string(), 16),
            ("each[-8]".to_string(), 1),
            ("each[-7]".to_string(), 1),
            ("each[0]".to_string(), 1),
            ("each[1]".to_string(), 1),
        ]
    );
    assert_eq!(
        instance_bins(&heap, 0, 1),
        [("w[0]".to_string(), 2), ("w[1]".to_string(), 2)]
    );
    let object = construct(&mut heap, key, &[]);
    for value in [9, 11, 13] {
        sample(&mut heap, object, &[0, value]);
    }
    assert_eq!(
        instance_bins(&heap, 1, 1),
        [("w[0]".to_string(), 2), ("w[1]".to_string(), 1)]
    );
}

#[test]
fn ignored_values_leave_automatic_bins_as_in_19_11_1() {
    let mut heap = Heap::new();
    // bit [2:0] b;
    // coverpoint b { option.auto_bin_max = 4; ignore_bins ig = {[0:1], [5:6]}; }
    let removed = || vec![range(int(0), int(1)), range(int(5), int(6))];
    let mut descriptor = group(
        "ct",
        vec![
            point(
                "b",
                point_type(3, false),
                vec![ignore(bins("ig", removed()))],
            ),
            point(
                "bad",
                point_type(3, false),
                vec![illegal(bins("ill", removed()))],
            ),
            point(
                "rest",
                point_type(3, false),
                vec![default_bins("others"), ignore(bins("ig", removed()))],
            ),
        ],
    );
    descriptor.options = vec![
        option(1, OptionId::AutoBinMax, 4),
        option(2, OptionId::AutoBinMax, 4),
        option(3, OptionId::AutoBinMax, 4),
    ];
    let key = register(&mut heap, descriptor);
    let object = construct(&mut heap, key, &[]);
    // "After applying the ignore_bins, the bins are: auto[], auto[2,3],
    // auto[4], auto[7]. Since it is no longer is associated with any
    // value, auto[] does not contribute to coverage." The names of the
    // bins are unaffected (19.11.3).
    assert_eq!(inst_counts(&mut heap, object, 1), (0, 3));
    assert_eq!(
        names(&heap, 0, 0),
        text(&["auto[2:3]", "auto[4:5]", "auto[6:7]", "ig"])
    );
    // A coverpoint with illegal bins only has automatic bins as well
    // (19.5.3), one with a `default` bin has none.
    assert_eq!(inst_counts(&mut heap, object, 2), (0, 3));
    assert_eq!(inst_counts(&mut heap, object, 3), (0, 0));
    assert_eq!(names(&heap, 0, 2), text(&["others", "ig"]));
    // 4 is a value of auto[4:5] still, 5 is one of no automatic bin.
    assert_eq!(sample_illegal(&mut heap, object, &[4, 4, 4]), 0);
    assert_eq!(sample_illegal(&mut heap, object, &[5, 5, 5]), 1);
    assert_eq!(sample_illegal(&mut heap, object, &[7, 7, 7]), 0);
    assert_eq!(
        listed(&heap, 0, 0),
        [
            ("auto[2:3]".to_string(), BinReportKind::Bins, 0),
            ("auto[4:5]".to_string(), BinReportKind::Bins, 1),
            ("auto[6:7]".to_string(), BinReportKind::Bins, 1),
            ("ig".to_string(), BinReportKind::Ignore, 1),
        ]
    );
    assert_eq!(
        listed(&heap, 0, 1),
        [
            ("auto[2:3]".to_string(), BinReportKind::Bins, 0),
            ("auto[4:5]".to_string(), BinReportKind::Bins, 1),
            ("auto[6:7]".to_string(), BinReportKind::Bins, 1),
            ("ill".to_string(), BinReportKind::Illegal, 1),
        ]
    );
    // An ignored value is in a bin, so it is none of the `default` bin.
    assert_eq!(
        listed(&heap, 0, 2),
        [
            ("others".to_string(), BinReportKind::Default, 2),
            ("ig".to_string(), BinReportKind::Ignore, 1),
        ]
    );
    assert_close(inst_coverage(&mut heap, object, 1), 200.0 / 3.0);
    // The coverpoint of the `default` bin has no bin to cover: it takes no
    // part in the covergroup (19.11.1 a).
    assert_close(inst_coverage(&mut heap, object, 3), 0.0);
    assert_close(inst_coverage(&mut heap, object, 0), 200.0 / 3.0);
    assert_eq!(inst_counts(&mut heap, object, 0), (4, 6));
}

#[test]
fn ignored_values_empty_the_automatic_bins_they_are_all_of() {
    let mut heap = Heap::new();
    let all = RangeDescriptor::Range {
        low: Bound::Open,
        high: Bound::Open,
    };
    let negative = RangeDescriptor::Range {
        low: Bound::Open,
        high: Bound::Operand(Operand::Constant(int(-1))),
    };
    // typedef enum bit [1:0] {A = 0, B = 1, C = 3} e;
    let mut enumeration = point_type(2, false);
    enumeration.members = vec![
        ("A".to_string(), sized(2, 0)),
        ("B".to_string(), sized(2, 1)),
        ("C".to_string(), sized(2, 3)),
    ];
    let mut descriptor = group(
        "g",
        vec![
            // Every value of 64 bits is ignored.
            point(
                "all",
                point_type(64, false),
                vec![ignore(bins("ig", vec![all]))],
            ),
            // One bin of all values but one.
            point(
                "one",
                point_type(64, false),
                vec![ignore(bins("ig", vec![value(int(5))]))],
            ),
            // The 32 bins of the negative values of a signed type.
            point(
                "half",
                point_type(64, true),
                vec![ignore(bins("ig", vec![negative]))],
            ),
            // The bin of an enumerator.
            point(
                "e",
                enumeration,
                vec![ignore(bins("ig", vec![value(int(1))]))],
            ),
            // An array of ignore bins: each counts its own samples.
            point(
                "p",
                point_type(2, false),
                vec![
                    open(bins("b", vec![range(int(0), int(3))])),
                    ignore(open(bins("ig", vec![range(int(0), int(1))]))),
                ],
            ),
        ],
    );
    descriptor.options = vec![option(2, OptionId::AutoBinMax, 1)];
    let key = register(&mut heap, descriptor);
    let object = construct(&mut heap, key, &[]);
    assert_eq!(inst_counts(&mut heap, object, 1), (0, 0));
    assert_eq!(inst_counts(&mut heap, object, 2), (0, 1));
    assert_eq!(inst_counts(&mut heap, object, 3), (0, 32));
    assert_eq!(names(&heap, 0, 0), text(&["ig"]));
    assert_eq!(
        names(&heap, 0, 1),
        text(&["auto[0:18446744073709551615]", "ig"])
    );
    let half = names(&heap, 0, 2);
    assert_eq!(half.len(), 33);
    assert_eq!(half[0], "auto[0:288230376151711743]");
    // The names of the automatic bins of an enumeration stay too.
    assert_eq!(names(&heap, 0, 3), text(&["auto[A]", "auto[C]", "ig"]));
    assert_eq!(
        names(&heap, 0, 4),
        text(&["b[2]", "b[3]", "ig[0]", "ig[1]"])
    );
    sample(&mut heap, object, &[5, 5, u64::MAX, 1, 1]);
    sample(&mut heap, object, &[6, 6, 7, 2, 2]);
    assert_eq!(hits(&heap, 0, 0, "ig"), 2);
    assert_eq!(
        listed(&heap, 0, 1),
        [
            (
                "auto[0:18446744073709551615]".to_string(),
                BinReportKind::Bins,
                1
            ),
            ("ig".to_string(), BinReportKind::Ignore, 1),
        ]
    );
    assert_eq!(hits(&heap, 0, 2, "auto[0:288230376151711743]"), 1);
    assert_eq!(hits(&heap, 0, 2, "ig"), 1);
    // 2 is the value of no enumerator.
    assert_eq!(
        listed(&heap, 0, 3),
        [
            ("auto[A]".to_string(), BinReportKind::Bins, 0),
            ("auto[C]".to_string(), BinReportKind::Bins, 0),
            ("ig".to_string(), BinReportKind::Ignore, 1),
        ]
    );
    assert_eq!(
        listed(&heap, 0, 4),
        [
            ("b[2]".to_string(), BinReportKind::Bins, 1),
            ("b[3]".to_string(), BinReportKind::Bins, 0),
            ("ig[0]".to_string(), BinReportKind::Ignore, 0),
            ("ig[1]".to_string(), BinReportKind::Ignore, 1),
        ]
    );
    // The coverpoint without a bin of a value takes no part (19.11.1 a):
    // 100, 100 / 32, 0 and 50 of the four others.
    assert_close(inst_coverage(&mut heap, object, 1), 0.0);
    assert_close(
        inst_coverage(&mut heap, object, 0),
        (100.0 + 100.0 / 32.0 + 0.0 + 50.0) / 4.0,
    );
    assert_eq!(inst_counts(&mut heap, object, 0), (3, 37));
}

#[test]
fn the_automatic_bins_of_19_11_3_keep_their_names() {
    let mut heap = Heap::new();
    // bit [7:0] a;
    // covergroup ga (int abm);
    //    option.auto_bin_max = abm;
    //    coverpoint a {ignore_bins i = {3};}
    // endgroup
    let mut descriptor = group(
        "ga",
        vec![point(
            "a",
            point_type(8, false),
            vec![ignore(bins("i", vec![value(int(3))]))],
        )],
    );
    descriptor.inputs = int_inputs(1);
    descriptor.options = vec![option_input(0, OptionId::AutoBinMax, 0)];
    descriptor.type_options.merge_instances = true;
    let key = register(&mut heap, descriptor);
    // ga gv1 = new (64); ga gv2 = new (32);
    let gv1 = construct(&mut heap, key, &[64]);
    let gv2 = construct(&mut heap, key, &[32]);
    // "the bins of the instance referenced by gv1 are auto[0:3] through
    // auto[252:255], while the bins of the instance referenced by gv2 are
    // auto[0:7] through auto[248:255]. Note how the ignored value 3 does
    // not have an effect on the bin names. Because none of the bin names
    // overlap between the two instances, the covergroup type ga has 96
    // cumulative bins."
    assert_eq!(type_counts(&mut heap, key, 1), (0, 96));
    let first = names(&heap, 0, 0);
    assert_eq!(
        (first[0].as_str(), first[63].as_str()),
        ("auto[0:3]", "auto[252:255]")
    );
    let second = names(&heap, 1, 0);
    assert_eq!(
        (second[0].as_str(), second[31].as_str()),
        ("auto[0:7]", "auto[248:255]")
    );
    sample(&mut heap, gv1, &[3]);
    sample(&mut heap, gv1, &[2]);
    sample(&mut heap, gv2, &[3]);
    assert_eq!(type_counts(&mut heap, key, 1), (1, 96));
    // The report of the type has the 96 bins and the ignore bin of both
    // instances, which is one bin by its name.
    let report = heap.coverage_report();
    let merged = &report.covergroups[0].items[0].bins;
    assert_eq!(merged.len(), 97);
    let bin = |name: &str| {
        let bin = merged.iter().find(|bin| bin.name == name).unwrap();
        (bin.kind, bin.hits)
    };
    assert_eq!(bin("auto[0:3]"), (BinReportKind::Bins, 1));
    assert_eq!(bin("auto[0:7]"), (BinReportKind::Bins, 0));
    assert_eq!(bin("i"), (BinReportKind::Ignore, 2));
}

#[test]
fn the_instances_of_19_11_3_unite_the_bins_of_their_arrays() {
    let mut heap = Heap::new();
    // covergroup gt (int l, h);
    //    coverpoint a {bins b[] = { [l:h] };}
    // endgroup
    let mut descriptor = group(
        "gt",
        vec![point(
            "a",
            point_type(2, false),
            vec![open(bins("b", vec![input_range(0, 1)]))],
        )],
    );
    descriptor.inputs = int_inputs(2);
    descriptor.type_options.merge_instances = true;
    let key = register(&mut heap, descriptor);
    // gt gv1 = new (0,1); gt gv2 = new (1,2);
    let gv1 = construct(&mut heap, key, &[0, 1]);
    let gv2 = construct(&mut heap, key, &[1, 2]);
    sample(&mut heap, gv1, &[0]);
    sample(&mut heap, gv1, &[1]);
    sample(&mut heap, gv2, &[1]);
    // "The covergroup gt has bins b[0], b[1], and b[2]. … the
    // gt::get_coverage() value would be 66.6667 because 2 out of 3 type
    // bins were covered; the cumulative count for the bin b[1] would be 2."
    assert_close(type_coverage(&mut heap, key, 0), 200.0 / 3.0);
    assert_eq!(type_counts(&mut heap, key, 1), (2, 3));
    assert_eq!(
        heap.coverage_report().covergroups[0].items[0]
            .bins
            .iter()
            .map(|bin| (bin.name.as_str(), bin.hits))
            .collect::<Vec<_>>(),
        [("b[0]", 1), ("b[1]", 2), ("b[2]", 0)]
    );
    assert_eq!(
        instance_bins(&heap, 1, 0),
        [("b[1]".to_string(), 1), ("b[2]".to_string(), 0)]
    );
    // "gv1.get_inst_coverage() would return 100.0 and
    // gv2.get_inst_coverage() would return 50.0."
    for object in [gv1, gv2] {
        set_option(&mut heap, object, 0, OptionId::GetInstCoverage, 1);
    }
    assert_close(inst_coverage(&mut heap, gv1, 0), 100.0);
    assert_close(inst_coverage(&mut heap, gv2, 0), 50.0);
}

/// The coverpoints whose bins have `with` filters that a construction
/// evaluates, and the filters.
fn filter_example() -> TypeDescriptor {
    let narrow = point_type(3, false);
    let byte = point_type(8, false);
    let divisible = |name: &str, filter: u32| {
        filtered(
            bins(name, vec![range(int(0), int(255))]),
            BinFilter::Enumerate(filter),
        )
    };
    let mut descriptor = group(
        "g",
        vec![
            // bins b[] = {[0:7]};
            // ignore_bins odd = {[0:7]} with (item % 2);
            point(
                "a",
                narrow.clone(),
                vec![
                    open(bins("b", vec![range(int(0), int(7))])),
                    ignore(filtered(
                        bins("odd", vec![range(int(0), int(7))]),
                        BinFilter::Enumerate(0),
                    )),
                ],
            ),
            // "bins mod3[] = {[0:255]} with (item % 3 == 0);"
            point("m", byte.clone(), vec![open(divisible("mod3", 1))]),
            // The same in one bin.
            point(
                "s",
                byte.clone(),
                vec![divisible("third", 2), default_bins("rest")],
            ),
            // illegal_bins big = {[0:3], 2} with (item >= 2);
            point(
                "i",
                narrow.clone(),
                vec![illegal(filtered(
                    bins("big", vec![range(int(0), int(3)), value(int(2))]),
                    BinFilter::Enumerate(3),
                ))],
            ),
        ],
    );
    descriptor.filters = vec![
        item_filter(&narrow),
        item_filter(&byte),
        item_filter(&byte),
        item_filter(&narrow),
    ];
    descriptor
}

/// Construct an instance of [`filter_example`]: the values that the
/// filters of `odd` and of `big` were asked about.
fn construct_filter_example(heap: &mut Heap, key: u64) -> (Handle, Vec<u64>, Vec<u64>) {
    let object = start(heap, key, &[]);
    let odd = ask(heap, object, 0, |item| item % 2 != 0);
    ask(heap, object, 1, |item| item % 3 == 0);
    ask(heap, object, 2, |item| item % 3 == 0);
    let big = ask(heap, object, 3, |item| item >= 2);
    assert_eq!(finish(heap, object), 0);
    (object, odd, big)
}

#[test]
fn a_construction_asks_the_filters_about_each_value() {
    let mut heap = Heap::new();
    let key = register(&mut heap, filter_example());
    let (object, odd, big) = construct_filter_example(&mut heap, key);
    // A filter is asked about the values in their order, as often as the
    // list has them (19.5.1.1).
    assert_eq!(odd, [0, 1, 2, 3, 4, 5, 6, 7]);
    assert_eq!(big, [0, 1, 2, 3, 2]);
    // The values that the filter of the ignore bin keeps are removed from
    // the bins of `b`: four of the eight have a value.
    assert_eq!(inst_counts(&mut heap, object, 1), (0, 4));
    assert_eq!(
        names(&heap, 0, 0),
        text(&["b[0]", "b[2]", "b[4]", "b[6]", "odd"])
    );
    // "This bin definition selects all values from 0 to 255 that are
    // evenly divisible by 3": 86 values.
    assert_eq!(inst_counts(&mut heap, object, 2), (0, 86));
    let mod3 = names(&heap, 0, 1);
    assert_eq!(mod3[..3], text(&["mod3[0]", "mod3[3]", "mod3[6]"]));
    assert_eq!(mod3[85], "mod3[255]");
    assert_eq!(inst_counts(&mut heap, object, 3), (0, 1));
    // 2 and 3 are illegal values, and values of no automatic bin.
    assert_eq!(inst_counts(&mut heap, object, 4), (0, 6));
    assert_eq!(sample_illegal(&mut heap, object, &[1, 3, 3, 1]), 0);
    assert_eq!(sample_illegal(&mut heap, object, &[2, 4, 4, 2]), 1);
    assert_eq!(hits(&heap, 0, 0, "odd"), 1);
    assert_eq!(hits(&heap, 0, 0, "b[2]"), 1);
    assert_eq!(hits(&heap, 0, 1, "mod3[3]"), 1);
    // A value that the filter does not keep is none of the bin, so one of
    // the `default` bin.
    assert_eq!(
        listed(&heap, 0, 2),
        [
            ("third".to_string(), BinReportKind::Bins, 1),
            ("rest".to_string(), BinReportKind::Default, 1),
        ]
    );
    assert_eq!(hits(&heap, 0, 3, "auto[1]"), 1);
    assert_eq!(hits(&heap, 0, 3, "big"), 1);
    assert_eq!(inst_counts(&mut heap, object, 0), (4, 97));
}

#[test]
fn an_equal_construction_takes_the_bins_without_its_filters() {
    let mut heap = Heap::new();
    // bins f[2] = {[0:7]} with (item < 6);
    // bins e[] = {[0:3]} with (item != 1);
    let ty = point_type(3, false);
    let mut descriptor = group(
        "g",
        vec![point(
            "p",
            ty.clone(),
            vec![
                fixed(
                    filtered(
                        bins("f", vec![range(int(0), int(7))]),
                        BinFilter::Enumerate(0),
                    ),
                    Operand::Constant(int(2)),
                ),
                open(filtered(
                    bins("e", vec![input_range(0, 1)]),
                    BinFilter::Enumerate(1),
                )),
            ],
        )],
    );
    descriptor.inputs = int_inputs(2);
    descriptor.filters = vec![item_filter(&ty), item_filter(&ty)];
    let key = register(&mut heap, descriptor);
    let build = |heap: &mut Heap, inputs: &[i64]| {
        let object = start(heap, key, inputs);
        let asked = (
            ask(heap, object, 0, |item| item < 6),
            ask(heap, object, 1, |item| item != 1),
        );
        assert_eq!(finish(heap, object), 0);
        (object, asked)
    };
    let (first, asked) = build(&mut heap, &[0, 3]);
    assert_eq!(asked, (vec![0, 1, 2, 3, 4, 5, 6, 7], vec![0, 1, 2, 3]));
    // The filter is applied before the values are distributed: the six
    // values it keeps are three for each bin of `f`, and `e` has a bin for
    // each of the three values it keeps.
    assert_eq!(
        names(&heap, 0, 0),
        text(&["f[0]", "f[1]", "e[0]", "e[2]", "e[3]"])
    );
    assert_eq!(heap.coverage.types[0].shapes[0].counters, 5);
    // A construction of the same values has nothing to ask (19.5.1.1
    // allows "caching of previously calculated results").
    let (same, asked) = build(&mut heap, &[0, 3]);
    assert_eq!(asked, (vec![], vec![]));
    assert_eq!(heap.coverage.types[0].shapes.len(), 1);
    assert_eq!(names(&heap, 1, 0), names(&heap, 0, 0));
    // One of other values has.
    let (_, asked) = build(&mut heap, &[0, 2]);
    assert_eq!(asked.1, [0, 1, 2]);
    assert_eq!(heap.coverage.types[0].shapes.len(), 2);
    // With `type_option.distribute_first` the values are distributed
    // first: four for each bin of `f`, of which the filter keeps four and
    // two, and a bin of `e` for each value, of which one is left without.
    set_type_option(&mut heap, key, 0, TypeOptionId::DistributeFirst, 1);
    let (distributed, asked) = build(&mut heap, &[0, 3]);
    assert_eq!(asked, (vec![0, 1, 2, 3, 4, 5, 6, 7], vec![0, 1, 2, 3]));
    assert_eq!(heap.coverage.types[0].shapes.len(), 3);
    assert_eq!(heap.coverage.types[0].shapes[2].counters, 6);
    assert_eq!(names(&heap, 3, 0), names(&heap, 0, 0));
    for object in [first, same, distributed] {
        sample(&mut heap, object, &[3]);
        sample(&mut heap, object, &[6]);
    }
    let expected = |f0: u64, f1: u64| {
        vec![
            ("f[0]".to_string(), f0),
            ("f[1]".to_string(), f1),
            ("e[0]".to_string(), 0),
            ("e[2]".to_string(), 0),
            ("e[3]".to_string(), 1),
        ]
    };
    assert_eq!(instance_bins(&heap, 0, 0), expected(0, 1));
    assert_eq!(instance_bins(&heap, 1, 0), expected(0, 1));
    assert_eq!(instance_bins(&heap, 3, 0), expected(1, 0));
    // The shape that was built without the option is taken again without
    // it.
    set_type_option(&mut heap, key, 0, TypeOptionId::DistributeFirst, 0);
    let (_, asked) = build(&mut heap, &[0, 3]);
    assert_eq!(asked, (vec![], vec![]));
    assert_eq!(heap.coverage.types[0].shapes.len(), 3);
}

#[test]
fn filters_are_asked_in_the_type_of_the_coverpoint() {
    let mut heap = Heap::new();
    let small = four_state(4, true);
    let wide = point_type(100, false);
    let top = Constant {
        ty: ValueType {
            width: 100,
            signed: false,
        },
        value: (BigUint::from(5u8) << 64) | BigUint::from(9u8),
        mask: BigUint::ZERO,
    };
    let mut descriptor = group(
        "g",
        vec![
            point(
                "small",
                small.clone(),
                vec![filtered(
                    bins("b", vec![range(int(-2), int(1)), value(pattern("1x0z"))]),
                    BinFilter::Enumerate(0),
                )],
            ),
            point(
                "wide",
                wide.clone(),
                vec![open(filtered(
                    bins("w", vec![value(top), value(int(7))]),
                    BinFilter::Enumerate(1),
                ))],
            ),
        ],
    );
    descriptor.filters = vec![item_filter(&small), item_filter(&wide)];
    let key = register(&mut heap, descriptor);
    let object = start(&mut heap, key, &[]);
    // The bits of a value are zero extended; bit 23 of the second
    // operand selects the plane of the x and z bits.
    let mut asked = Vec::new();
    let mut keep = 0;
    while call(&mut heap, op::FILTER_NEXT, 0, 0, [object.into(), keep, 0]) != 0 {
        let value = call(&mut heap, op::FILTER_ITEM, 0, 0, [object.into(), 0, 0]);
        let mask = call(
            &mut heap,
            op::FILTER_ITEM,
            0,
            1 << 23,
            [object.into(), 0, 0],
        );
        asked.push((value, mask));
        // Keep -2, 0 and the value with x and z.
        keep = u64::from(value != 0b1111 && value != 1);
    }
    assert_eq!(
        asked,
        [(0b1110, 0), (0b1111, 0), (0, 0), (1, 0), (0b1100, 0b0101)]
    );
    // A value of more than a word has a word index from bit 8 on.
    let mut words = Vec::new();
    let mut keep = 0;
    while call(&mut heap, op::FILTER_NEXT, 1, 0, [object.into(), keep, 0]) != 0 {
        let low = call(&mut heap, op::FILTER_ITEM, 1, 0, [object.into(), 0, 0]);
        let high = call(&mut heap, op::FILTER_ITEM, 1, 1 << 8, [object.into(), 0, 0]);
        words.push((low, high));
        keep = u64::from(high != 0);
    }
    assert_eq!(words, [(9, 5), (7, 0)]);
    assert_eq!(finish(&mut heap, object), 0);
    assert_eq!(names(&heap, 0, 1), text(&["w[92233720368547758089]"]));
    for (value, mask, count) in [(0b1110, 0, 1), (0, 0, 2), (1, 0, 2), (0b1100, 0b0101, 3)] {
        call(&mut heap, op::POINT, 1, 0, [object.into(), value, mask]);
        call(&mut heap, op::SAMPLE_END, 0, 0, [object.into(), 0, 0]);
        assert_eq!(instance_bins(&heap, 0, 0), [("b".to_string(), count)]);
    }
}

#[test]
fn a_filter_that_is_not_run_as_it_is_to_be_is_a_fault() {
    let mut heap = Heap::new();
    let ty = point_type(32, false);
    let mut descriptor = group(
        "g",
        vec![point(
            "p",
            ty.clone(),
            vec![
                filtered(
                    bins("few", vec![input_range(0, 1)]),
                    BinFilter::Enumerate(0),
                ),
                wildcard(filtered(
                    bins("wild", vec![RangeDescriptor::Value(Operand::Input(2))]),
                    BinFilter::Enumerate(1),
                )),
            ],
        )],
    );
    descriptor.inputs = int_inputs(3);
    descriptor.filters = vec![item_filter(&ty), item_filter(&ty)];
    let key = register(&mut heap, descriptor);
    let failed = |heap: &mut Heap, reason: &str| {
        assert_eq!(fault(heap), Some(format!("covergroup: type `g`: {reason}")));
    };
    // A build before its filters were asked about all their values.
    let object = start(&mut heap, key, &[0, 3, 0]);
    assert_eq!(call(&mut heap, op::BUILD, 0, 0, [object.into(), 0, 0]), 0);
    failed(&mut heap, "filter 0 was not asked about all its values");
    let object = start(&mut heap, key, &[0, 3, 0]);
    assert_eq!(
        call(&mut heap, op::FILTER_NEXT, 0, 0, [object.into(), 0, 0]),
        1
    );
    ask(&mut heap, object, 1, |_| true);
    assert_eq!(call(&mut heap, op::BUILD, 0, 0, [object.into(), 0, 0]), 0);
    failed(&mut heap, "filter 0 was not asked about all its values");
    // A value of a filter that is at none, and a filter the type has not.
    let object = start(&mut heap, key, &[0, 3, 0]);
    assert_eq!(
        call(&mut heap, op::FILTER_ITEM, 0, 0, [object.into(), 0, 0]),
        0
    );
    failed(&mut heap, "filter 0 is at no value");
    assert_eq!(
        call(&mut heap, op::FILTER_NEXT, 2, 0, [object.into(), 0, 0]),
        0
    );
    failed(&mut heap, "no filter 2");
    // An input once a filter was called: the bins of an earlier
    // construction were looked up by then.
    call(&mut heap, op::INPUT, 0, 0, [object.into(), 1, 0]);
    failed(
        &mut heap,
        "an input of a construction after a filter of it was called",
    );
    // A component that the value of a coverpoint has not.
    assert_eq!(
        call(&mut heap, op::FILTER_NEXT, 0, 0, [object.into(), 0, 0]),
        1
    );
    assert_eq!(
        call(&mut heap, op::FILTER_ITEM, 0, 1, [object.into(), 0, 0]),
        0
    );
    failed(&mut heap, "a value of filter 0 has no component 1");
    // More values than a construction asks about.
    let object = start(&mut heap, key, &[0, MAX_FILTER_CANDIDATES as i64, 0]);
    assert_eq!(
        call(&mut heap, op::FILTER_NEXT, 0, 0, [object.into(), 0, 0]),
        0
    );
    failed(
        &mut heap,
        "the `with` filter of bins `few` of coverpoint `p` has more than 1048576 values to ask \
         about",
    );
    // As many are asked about.
    let object = start(&mut heap, key, &[1, MAX_FILTER_CANDIDATES as i64, 0]);
    let asked = ask(&mut heap, object, 0, |item| item % 4 == 0);
    assert_eq!(asked.len(), MAX_FILTER_CANDIDATES);
    // A wildcard value that is kept as a pattern has no values to ask
    // about: the value has 29 free bits above its lowest bit to have.
    let object = start(&mut heap, key, &[0, 3, 0]);
    call(
        &mut heap,
        op::INPUT,
        2,
        0,
        [object.into(), 0x4000_0001, 0x3fff_fffe],
    );
    assert_eq!(
        call(&mut heap, op::FILTER_NEXT, 1, 0, [object.into(), 0, 0]),
        0
    );
    failed(
        &mut heap,
        "the `with` filter of bins `wild` of coverpoint `p` has a wildcard value of more than \
         65536 ranges of values",
    );
    // A filter of an instance that is built.
    let object = start(&mut heap, key, &[0, 3, 0]);
    ask(&mut heap, object, 0, |_| true);
    ask(&mut heap, object, 1, |_| true);
    assert_eq!(finish(&mut heap, object), 0);
    assert_eq!(
        call(&mut heap, op::FILTER_NEXT, 0, 0, [object.into(), 0, 0]),
        0
    );
    failed(&mut heap, "a filter of an instance that is built already");
    // A filter of no instance.
    assert_eq!(call(&mut heap, op::FILTER_NEXT, 0, 0, [0, 0, 0]), 0);
    assert_eq!(heap.take_fault(), Some(Fault::NullDereference));
}

#[test]
fn values_are_removed_once_they_are_distributed() {
    let mut heap = Heap::new();
    // bins f[2] = {[0:3]};
    // ignore_bins ig = {0, 1};
    let key = register(
        &mut heap,
        group(
            "g",
            vec![point(
                "p",
                point_type(2, false),
                vec![
                    fixed(
                        bins("f", vec![range(int(0), int(3))]),
                        Operand::Constant(int(2)),
                    ),
                    ignore(bins("ig", vec![value(int(0)), value(int(1))])),
                ],
            )],
        ),
    );
    let object = construct(&mut heap, key, &[]);
    // "The removal of ignored values shall occur after the distribution of
    // values to the specified bins" (19.5.5): f[0] got 0 and 1 and is left
    // without a value, where the two values that are left would have been
    // one for each bin.
    assert_eq!(names(&heap, 0, 0), text(&["f[1]", "ig"]));
    assert_eq!(inst_counts(&mut heap, object, 1), (0, 1));
    for value in 0..4 {
        sample(&mut heap, object, &[value]);
    }
    assert_eq!(
        instance_bins(&heap, 0, 0),
        [("f[1]".to_string(), 2), ("ig".to_string(), 2)]
    );
}

/// The wildcard value of 32 bits whose first and last bit are 1: it has 30
/// free bits above its lowest bit to have, so more ranges of values than a
/// bin lists.
fn spread() -> Constant {
    pattern(&format!("1{}1", "?".repeat(30)))
}

#[test]
fn wildcard_ignore_bins_remove_the_values_they_list() {
    let mut heap = Heap::new();
    let word = point_type(32, false);
    let key = register(
        &mut heap,
        group(
            "w",
            vec![
                // Within the limit the values are known.
                point(
                    "p",
                    point_type(4, false),
                    vec![
                        bins("lo", vec![range(int(0), int(7))]),
                        bins("hi", vec![range(int(8), int(15))]),
                        wildcard(ignore(bins("ig", vec![value(pattern("1???"))]))),
                    ],
                ),
                // Above it they are not.
                point(
                    "q",
                    word.clone(),
                    vec![
                        bins("one", vec![value(sized(32, 0x8000_0001))]),
                        bins("other", vec![value(int(5))]),
                        wildcard(ignore(bins("ig", vec![value(spread())]))),
                    ],
                ),
                point(
                    "r",
                    word.clone(),
                    vec![
                        wildcard(bins("w", vec![value(spread())])),
                        default_bins("d"),
                    ],
                ),
                point(
                    "t",
                    word.clone(),
                    vec![
                        wildcard(bins("w", vec![value(spread())])),
                        ignore(bins("ig", vec![value(sized(32, 0x8000_0003))])),
                    ],
                ),
                point(
                    "s",
                    point_type(32, true),
                    vec![wildcard(bins("neg", vec![value(spread())]))],
                ),
            ],
        ),
    );
    let (object, warnings) = construct_with_warnings(&mut heap, key, &[]);
    assert_eq!(warnings, 2);
    assert_eq!(
        message(&mut heap, object),
        "ignore_bins `ig` of coverpoint `q` of covergroup `w` (instance `w#0`): a wildcard value \
         of more than 65536 ranges of values is matched as a pattern: its values are in no bin \
         when they are sampled, but still count as values of the bins they are taken from (IEEE \
         1800-2023 19.11.1); bins `w` of coverpoint `t` of covergroup `w` (instance `w#0`): a \
         wildcard value of more than 65536 ranges of values is matched as a pattern: \
         `ignore_bins`, `illegal_bins` and option.detect_overlap do not see its values"
    );
    // `hi` is left without a value.
    assert_eq!(inst_counts(&mut heap, object, 1), (0, 1));
    // `one` is not, though every sample of its value is ignored.
    assert_eq!(inst_counts(&mut heap, object, 2), (0, 2));
    sample(
        &mut heap,
        object,
        &[9, 0x8000_0001, 0x8000_0001, 0x8000_0003, 0x8000_0001],
    );
    sample(&mut heap, object, &[3, 5, 0x8000_0000, 0x8000_0001, 2]);
    assert_eq!(
        listed(&heap, 0, 0),
        [
            ("lo".to_string(), BinReportKind::Bins, 1),
            ("ig".to_string(), BinReportKind::Ignore, 1),
        ]
    );
    assert_eq!(
        listed(&heap, 0, 1),
        [
            ("one".to_string(), BinReportKind::Bins, 0),
            ("other".to_string(), BinReportKind::Bins, 1),
            ("ig".to_string(), BinReportKind::Ignore, 1),
        ]
    );
    // A value of the pattern is in a bin, so none of the `default` bin.
    assert_eq!(
        listed(&heap, 0, 2),
        [
            ("w".to_string(), BinReportKind::Bins, 1),
            ("d".to_string(), BinReportKind::Default, 1),
        ]
    );
    // A value that an ignore bin lists is none of the pattern when it is
    // sampled.
    assert_eq!(
        listed(&heap, 0, 3),
        [
            ("w".to_string(), BinReportKind::Bins, 1),
            ("ig".to_string(), BinReportKind::Ignore, 1),
        ]
    );
    // The pattern is of the bits of a value, whatever its sign: of the
    // two samples the one whose first and last bit are set.
    assert_eq!(instance_bins(&heap, 0, 4), [("neg".to_string(), 1)]);
}

#[test]
fn a_sample_is_one_hit_of_a_bin_that_lists_its_value_more_than_once() {
    let mut heap = Heap::new();
    let word = point_type(32, false);
    // Three more wildcard values: one whose bit 30 and last bit are 1 and
    // one whose last bit is 1, of more ranges of values than a bin lists as
    // `spread` is, and one whose first bit is 1, which is one range.
    let inner = || pattern(&format!("?1{}1", "?".repeat(29)));
    let odd = pattern(&format!("{}1", "?".repeat(31)));
    let top = pattern(&format!("1{}", "?".repeat(31)));
    let both = || sized(32, 0x8000_0001);
    let mut descriptor = group(
        "w",
        vec![
            // A pattern that is listed twice, with a value of it that is
            // listed as a value too, in a bin between two others that hold
            // that value.
            point(
                "p",
                word.clone(),
                vec![
                    bins("a", vec![value(both())]),
                    wildcard(bins(
                        "w",
                        vec![value(spread()), value(both()), value(spread())],
                    )),
                    bins(
                        "z",
                        vec![range(sized(32, 0x8000_0000), sized(32, 0x8000_0002))],
                    ),
                ],
            ),
            // Two patterns that share values, in a bin that follows a bin
            // of one of them.
            point(
                "q",
                word.clone(),
                vec![
                    wildcard(bins("v", vec![value(inner())])),
                    wildcard(bins("w", vec![value(spread()), value(inner())])),
                ],
            ),
            // A pattern and a range that share values.
            point(
                "r",
                word.clone(),
                vec![wildcard(bins("w", vec![value(odd), value(top)]))],
            ),
            point(
                "s",
                word.clone(),
                vec![
                    bins("x", vec![value(int(5))]),
                    wildcard(illegal(bins(
                        "bad",
                        vec![value(spread()), value(both()), value(spread())],
                    ))),
                ],
            ),
            point(
                "t",
                word.clone(),
                vec![
                    bins("x", vec![value(int(5))]),
                    wildcard(ignore(bins("ig", vec![value(spread()), value(inner())]))),
                ],
            ),
        ],
    );
    descriptor.options = vec![option(1, OptionId::AtLeast, 2)];
    let key = register(&mut heap, descriptor);
    // The patterns of the illegal and of the ignore bin.
    let (object, warnings) = construct_with_warnings(&mut heap, key, &[]);
    assert_eq!(warnings, 2);
    assert_ne!(message(&mut heap, object), "");
    // Every bin that holds the value of a coverpoint is hit once: no bin
    // of `p` has the two hits it needs, and the illegal value is one.
    assert_eq!(
        sample_illegal(
            &mut heap,
            object,
            &[
                0x8000_0001,
                0xC000_0001,
                0x8000_0001,
                0x8000_0001,
                0xC000_0001
            ],
        ),
        1
    );
    assert_eq!(
        message(&mut heap, object),
        "illegal_bins `bad` of coverpoint `s` of covergroup `w` (instance `w#0`): value 2147483649"
    );
    let all = |heap: &Heap| -> Vec<Vec<(String, u64)>> {
        (0..5).map(|point| instance_bins(heap, 0, point)).collect()
    };
    let counted = |counts: [&[(&str, u64)]; 5]| -> Vec<Vec<(String, u64)>> {
        counts
            .iter()
            .map(|bins| {
                bins.iter()
                    .map(|&(name, hits)| (name.to_string(), hits))
                    .collect()
            })
            .collect()
    };
    assert_eq!(
        all(&heap),
        counted([
            &[("a", 1), ("w", 1), ("z", 1)],
            &[("v", 1), ("w", 1)],
            &[("w", 1)],
            &[("x", 0), ("bad", 1)],
            &[("x", 0), ("ig", 1)],
        ])
    );
    assert_eq!(inst_counts(&mut heap, object, 1), (0, 3));
    // A value of one of the values of a bin only is a hit as well: of
    // `spread` alone, of the range of `r` alone, and of no pattern.
    assert_eq!(
        sample_illegal(
            &mut heap,
            object,
            &[0x8000_0001, 0x8000_0001, 0x8000_0000, 5, 5]
        ),
        0
    );
    assert_eq!(
        all(&heap),
        counted([
            &[("a", 2), ("w", 2), ("z", 2)],
            &[("v", 1), ("w", 2)],
            &[("w", 2)],
            &[("x", 1), ("bad", 1)],
            &[("x", 1), ("ig", 1)],
        ])
    );
    assert_eq!(inst_counts(&mut heap, object, 1), (3, 3));
    // Of no value of `p`, of the second pattern of `q` alone, of the
    // pattern of `r` alone, and of the first pattern of `s` and `t` alone.
    assert_eq!(
        sample_illegal(
            &mut heap,
            object,
            &[0, 0x4000_0001, 1, 0x8000_0003, 0x8000_0003],
        ),
        1
    );
    assert_eq!(
        all(&heap),
        counted([
            &[("a", 2), ("w", 2), ("z", 2)],
            &[("v", 2), ("w", 3)],
            &[("w", 3)],
            &[("x", 1), ("bad", 2)],
            &[("x", 1), ("ig", 2)],
        ])
    );
}

#[test]
fn a_wildcard_value_is_ranges_of_values_up_to_the_limit() {
    let mut heap = Heap::new();
    // A value whose first and last bit are 1 with `free` free bits between
    // them is a range for each choice of those.
    let ends = |free: usize| pattern(&format!("1{}1", "?".repeat(free)));
    let key = register(
        &mut heap,
        group(
            "g",
            vec![
                // 65,536 ranges: the values are known, and `hi` is left
                // without one.
                point(
                    "p",
                    point_type(18, false),
                    vec![
                        bins("lo", vec![value(int(0))]),
                        bins("hi", vec![value(int((1 << 17) + 1))]),
                        wildcard(ignore(bins("ig", vec![value(ends(16))]))),
                    ],
                ),
                // One more free bit: a pattern.
                point(
                    "q",
                    point_type(19, false),
                    vec![
                        bins("lo", vec![value(int(0))]),
                        bins("hi", vec![value(int((1 << 18) + 1))]),
                        wildcard(ignore(bins("ig", vec![value(ends(17))]))),
                    ],
                ),
            ],
        ),
    );
    let (object, warnings) = construct_with_warnings(&mut heap, key, &[]);
    assert_eq!(warnings, 1);
    assert!(message(&mut heap, object).starts_with(
        "ignore_bins `ig` of coverpoint `q` of covergroup `g` (instance `g#0`): a wildcard \
             value of more than 65536 ranges of values is matched as a pattern"
    ));
    assert_eq!(inst_counts(&mut heap, object, 1), (0, 1));
    assert_eq!(inst_counts(&mut heap, object, 2), (0, 2));
    assert_eq!(heap.coverage.types[0].shapes[0].counters, 6);
    // Sampled, a value of either is ignored all the same.
    sample(&mut heap, object, &[(1 << 17) + 1, (1 << 18) + 1]);
    sample(&mut heap, object, &[(1 << 17) + 3, (1 << 18) + 2]);
    assert_eq!(
        listed(&heap, 0, 0),
        [
            ("lo".to_string(), BinReportKind::Bins, 0),
            ("ig".to_string(), BinReportKind::Ignore, 2),
        ]
    );
    assert_eq!(
        listed(&heap, 0, 1),
        [
            ("lo".to_string(), BinReportKind::Bins, 0),
            ("hi".to_string(), BinReportKind::Bins, 0),
            ("ig".to_string(), BinReportKind::Ignore, 1),
        ]
    );
}

#[test]
fn wildcard_values_of_too_many_ranges_are_for_scalar_bins() {
    let mut heap = Heap::new();
    let key = register(
        &mut heap,
        group(
            "a",
            vec![point(
                "p",
                point_type(32, false),
                vec![wildcard(fixed(
                    bins("w", vec![value(spread())]),
                    Operand::Constant(int(2)),
                ))],
            )],
        ),
    );
    let object = start(&mut heap, key, &[]);
    call(&mut heap, op::BUILD, 0, 0, [object.into(), 0, 0]);
    assert_eq!(
        fault(&mut heap),
        Some(
            "covergroup: type `a`: bins `w` of coverpoint `p` has a wildcard value of more than \
             65536 ranges of values, which only a bin that is no array and has no `with` filter \
             may have"
                .to_string()
        )
    );
    // A wildcard value of one range of too many values for a bin each.
    let key = register(
        &mut heap,
        group(
            "b",
            vec![point(
                "p",
                point_type(20, false),
                vec![
                    wildcard(open(bins("few", vec![value(pattern(&"?".repeat(16)))]))),
                    wildcard(open(bins("many", vec![value(pattern(&"?".repeat(17)))]))),
                ],
            )],
        ),
    );
    let object = start(&mut heap, key, &[]);
    call(&mut heap, op::BUILD, 0, 0, [object.into(), 0, 0]);
    assert_eq!(
        fault(&mut heap),
        Some(
            "covergroup: type `b`: bins `many` of coverpoint `p` has more than 65536 bins"
                .to_string()
        )
    );
    // An array of a bin for each value of more values than an item has
    // bins.
    let key = register(
        &mut heap,
        group(
            "c",
            vec![point(
                "p",
                point_type(32, false),
                vec![
                    bins("one", vec![value(int(1))]),
                    open(bins(
                        "all",
                        vec![range(int(1), int(MAX_BINS_PER_ITEM as i64))],
                    )),
                ],
            )],
        ),
    );
    let object = start(&mut heap, key, &[]);
    call(&mut heap, op::BUILD, 0, 0, [object.into(), 0, 0]);
    assert_eq!(
        fault(&mut heap),
        Some("covergroup: type `c`: coverpoint `p` has more than 1048576 bins".to_string())
    );
}

#[test]
fn values_in_no_bin_are_values_of_the_default_bins() {
    let mut heap = Heap::new();
    let logic = four_state(4, false);
    let low = || bins("a", vec![range(int(0), int(3))]);
    let key = register(
        &mut heap,
        group(
            "g",
            vec![
                point("one", logic.clone(), vec![low(), default_bins("d")]),
                // Only an illegal `default` bin: the automatic bins have
                // every value without x and z.
                point("bad", logic.clone(), vec![illegal(default_bins("bad"))]),
                point(
                    "each",
                    logic.clone(),
                    vec![low(), open(default_bins("others"))],
                ),
                point(
                    "known",
                    logic.clone(),
                    vec![
                        bins("xz", vec![value(pattern("1x0z"))]),
                        ignore(bins("ig", vec![value(int(1))])),
                        illegal(bins("ill", vec![value(int(2))])),
                        default_bins("d"),
                        default_bins("too"),
                    ],
                ),
            ],
        ),
    );
    let object = construct(&mut heap, key, &[]);
    // A value with x or z bits is in no bin that does not list it, so it
    // is one of the `default` bin, and an illegal value where that is an
    // illegal bin (19.5).
    assert_eq!(sample_planes(&mut heap, object, 4, 0b1100, 0b0101), 1);
    assert_eq!(
        message(&mut heap, object),
        "illegal_bins `bad` of coverpoint `bad` of covergroup `g` (instance `g#0`): value \
         4'b1x0z"
    );
    assert_eq!(sample_planes(&mut heap, object, 4, 0b1100, 0b0100), 1);
    assert_eq!(sample_planes(&mut heap, object, 4, 5, 0), 0);
    assert_eq!(sample_planes(&mut heap, object, 4, 5, 0), 0);
    // The ignored and the illegal value are in a bin.
    assert_eq!(sample_planes(&mut heap, object, 4, 1, 0), 0);
    assert_eq!(sample_planes(&mut heap, object, 4, 2, 0), 1);
    assert_eq!(
        listed(&heap, 0, 0),
        [
            ("a".to_string(), BinReportKind::Bins, 2),
            ("d".to_string(), BinReportKind::Default, 4),
        ]
    );
    let bad = listed(&heap, 0, 1);
    assert_eq!(bad.len(), 17);
    assert_eq!(bad[5], ("auto[5]".to_string(), BinReportKind::Bins, 2));
    assert_eq!(bad[16], ("bad".to_string(), BinReportKind::Illegal, 2));
    // A bin of the array for each value, those with x or z bits last.
    assert_eq!(
        listed(&heap, 0, 2),
        [
            ("a".to_string(), BinReportKind::Bins, 2),
            ("others[5]".to_string(), BinReportKind::Default, 2),
            ("others[4'b1x00]".to_string(), BinReportKind::Default, 1),
            ("others[4'b1x0z]".to_string(), BinReportKind::Default, 1),
        ]
    );
    // Every `default` bin of a coverpoint counts.
    assert_eq!(
        listed(&heap, 0, 3),
        [
            ("xz".to_string(), BinReportKind::Bins, 1),
            ("ig".to_string(), BinReportKind::Ignore, 1),
            ("ill".to_string(), BinReportKind::Illegal, 1),
            ("d".to_string(), BinReportKind::Default, 3),
            ("too".to_string(), BinReportKind::Default, 3),
        ]
    );
    // No `default` bin is a bin of the coverage.
    assert_eq!(inst_counts(&mut heap, object, 1), (1, 1));
    assert_eq!(inst_counts(&mut heap, object, 2), (3, 16));
    assert_eq!(inst_counts(&mut heap, object, 3), (1, 1));
    assert_eq!(inst_counts(&mut heap, object, 4), (1, 1));
}

#[test]
fn values_with_x_or_z_are_removed_and_illegal_as_others() {
    let mut heap = Heap::new();
    let logic = four_state(4, false);
    let key = register(
        &mut heap,
        group(
            "g",
            vec![
                point(
                    "p",
                    logic.clone(),
                    vec![
                        bins("a", vec![value(pattern("1x0z")), value(int(3))]),
                        bins("only", vec![value(pattern("1x0z"))]),
                        ignore(bins("ig", vec![value(pattern("1x0z"))])),
                    ],
                ),
                point("q", logic.clone(), vec![illegal(open(default_bins("bad")))]),
            ],
        ),
    );
    let object = construct(&mut heap, key, &[]);
    // The ignored value is one of no other bin: `only` is left without a
    // value.
    assert_eq!(inst_counts(&mut heap, object, 1), (0, 1));
    assert_eq!(sample_planes(&mut heap, object, 2, 0b1100, 0b0101), 1);
    assert_eq!(
        message(&mut heap, object),
        "illegal_bins `bad` of coverpoint `q` of covergroup `g` (instance `g#0`): value 4'b1x0z"
    );
    assert_eq!(sample_planes(&mut heap, object, 2, 3, 0), 0);
    assert_eq!(
        listed(&heap, 0, 0),
        [
            ("a".to_string(), BinReportKind::Bins, 1),
            ("ig".to_string(), BinReportKind::Ignore, 1),
        ]
    );
    // An illegal `default` bin array has a bin for each value too.
    let bad = listed(&heap, 0, 1);
    assert_eq!(bad.len(), 17);
    assert_eq!(bad[3], ("auto[3]".to_string(), BinReportKind::Bins, 1));
    assert_eq!(
        bad[16],
        ("bad[4'b1x0z]".to_string(), BinReportKind::Illegal, 1)
    );
}

#[test]
fn a_wide_coverpoint_matches_patterns_as_a_narrow_one() {
    let mut heap = Heap::new();
    // 100 bits whose first and last are 1, and of which the two last are.
    let ends = pattern(&format!("1{}1", "?".repeat(98)));
    let low = pattern(&format!("1{}11", "?".repeat(97)));
    let key = register(
        &mut heap,
        group(
            "g",
            vec![point(
                "p",
                point_type(100, true),
                vec![
                    wildcard(bins("w", vec![value(ends)])),
                    wildcard(ignore(bins("ig", vec![value(low)]))),
                ],
            )],
        ),
    );
    let (object, warnings) = construct_with_warnings(&mut heap, key, &[]);
    assert_eq!(warnings, 2);
    for (low, high) in [(1, 1 << 35), (3, 1 << 35), (5, 1 << 35), (1, 0)] {
        call(&mut heap, op::POINT, 1, 0, [object.into(), low, 0]);
        call(&mut heap, op::POINT, 1, 1, [object.into(), high, 0]);
        call(&mut heap, op::SAMPLE_END, 0, 0, [object.into(), 0, 0]);
    }
    assert_eq!(heap.take_fault(), None);
    assert_eq!(
        listed(&heap, 0, 0),
        [
            ("w".to_string(), BinReportKind::Bins, 2),
            ("ig".to_string(), BinReportKind::Ignore, 1),
        ]
    );
}

#[test]
fn a_default_bin_array_has_a_bin_for_a_limited_number_of_values() {
    let mut heap = Heap::new();
    let key = register(
        &mut heap,
        group(
            "g",
            vec![point(
                "p",
                point_type(100, true),
                vec![
                    bins("zero", vec![value(int(0))]),
                    open(default_bins("others")),
                ],
            )],
        ),
    );
    let object = construct(&mut heap, key, &[]);
    let size = heap.content_size();
    sample(&mut heap, object, &[1]);
    sample(&mut heap, object, &[1]);
    // Values of more than a word, which are negative: all bits from 64 on
    // are set.
    for value in 1..=MAX_DEFAULT_ARRAY as u64 + 3 {
        call(&mut heap, op::POINT, 1, 0, [object.into(), value, 0]);
        call(
            &mut heap,
            op::POINT,
            1,
            1,
            [object.into(), u64::MAX >> 28, 0],
        );
        call(&mut heap, op::SAMPLE_END, 0, 0, [object.into(), 0, 0]);
    }
    assert_eq!(heap.take_fault(), None);
    let report = heap.coverage_report();
    let item = &report.covergroups[0].instances[0].items[0];
    // The bin of `zero` and one for each of the first values; the samples
    // of the four values after them are counted together, here and in the
    // report of the type.
    assert_eq!(item.bins.len(), 1 + MAX_DEFAULT_ARRAY);
    assert_eq!(item.omitted_bins, 4);
    assert_eq!(report.covergroups[0].items[0].omitted_bins, 4);
    // The values are in their order: the negative ones first.
    let most_negative = -(1i128 << 64) + 1;
    assert_eq!(
        (item.bins[1].name.as_str(), item.bins[1].hits),
        (format!("others[{most_negative}]").as_str(), 1)
    );
    assert_eq!(
        (
            item.bins[MAX_DEFAULT_ARRAY].name.as_str(),
            item.bins[MAX_DEFAULT_ARRAY].hits
        ),
        ("others[1]", 2)
    );
    // A value that the array counts is 24 bytes of the heap.
    assert_eq!(heap.content_size(), size + MAX_DEFAULT_ARRAY * 24);
    // The samples it has no bin for are contents of the heap too.
    let restored: Heap = postcard::from_bytes(&postcard::to_allocvec(&heap).unwrap()).unwrap();
    assert!(restored.same_contents(&heap));
    assert_eq!(restored.coverage_report(), report);
    let mut clone = heap.clone();
    call(&mut clone, op::POINT, 1, 0, [object.into(), 77, 0]);
    call(&mut clone, op::POINT, 1, 1, [object.into(), 1, 0]);
    call(&mut clone, op::SAMPLE_END, 0, 0, [object.into(), 0, 0]);
    assert_eq!(
        clone.coverage_report().covergroups[0].instances[0].items[0].omitted_bins,
        5
    );
    assert!(!clone.same_contents(&heap));
}

#[test]
fn the_instances_of_a_type_unite_the_bins_of_a_default_array() {
    let mut heap = Heap::new();
    // A value of 100 bits whose highest bit is set.
    let high = |low: u64| Constant {
        ty: ValueType {
            width: 100,
            signed: false,
        },
        value: (BigUint::from(1u8) << 99) | BigUint::from(low),
        mask: BigUint::ZERO,
    };
    let mut descriptor = group(
        "g",
        vec![
            point(
                "p",
                point_type(4, false),
                vec![bins("a", vec![value(int(0))]), open(default_bins("o"))],
            ),
            // Ten values for three bins: <0,1,2>, <3,4,5> and <6,7,8,9>
            // above the high bit, of which a wildcard value takes 0 to 3.
            point(
                "w",
                point_type(100, false),
                vec![
                    fixed(
                        bins("f", vec![range(high(0), high(9))]),
                        Operand::Constant(int(3)),
                    ),
                    ignore(wildcard(bins(
                        "ig",
                        vec![value(pattern(&format!("1{}??", "0".repeat(97))))],
                    ))),
                ],
            ),
        ],
    );
    descriptor.type_options.merge_instances = true;
    let key = register(&mut heap, descriptor);
    let first = construct(&mut heap, key, &[]);
    let second = construct(&mut heap, key, &[]);
    let sample_both = |heap: &mut Heap, object: Handle, low: u64| {
        call(heap, op::POINT, 1, 0, [object.into(), low, 0]);
        call(heap, op::POINT, 2, 0, [object.into(), low, 0]);
        call(heap, op::POINT, 2, 1, [object.into(), 1 << 35, 0]);
        assert_eq!(call(heap, op::SAMPLE_END, 0, 0, [object.into(), 0, 0]), 0);
        assert_eq!(heap.take_fault(), None);
    };
    for low in [5, 5, 7, 3, 4] {
        sample_both(&mut heap, first, low);
    }
    for low in [5, 9, 0] {
        sample_both(&mut heap, second, low);
    }
    // The bins of the array of each instance are those of the values it
    // sampled; the type has a bin for each value, with the hits of both.
    let report = heap.coverage_report();
    let bins = |item: &CoverItemReport| -> Vec<(String, BinReportKind, u64)> {
        item.bins
            .iter()
            .map(|bin| (bin.name.clone(), bin.kind, bin.hits))
            .collect()
    };
    assert_eq!(
        bins(&report.covergroups[0].instances[1].items[0]),
        [
            ("a".to_string(), BinReportKind::Bins, 1),
            ("o[5]".to_string(), BinReportKind::Default, 1),
            ("o[9]".to_string(), BinReportKind::Default, 1),
        ]
    );
    assert_eq!(
        bins(&report.covergroups[0].items[0]),
        [
            ("a".to_string(), BinReportKind::Bins, 1),
            ("o[3]".to_string(), BinReportKind::Default, 1),
            ("o[4]".to_string(), BinReportKind::Default, 1),
            ("o[5]".to_string(), BinReportKind::Default, 3),
            ("o[7]".to_string(), BinReportKind::Default, 1),
            ("o[9]".to_string(), BinReportKind::Default, 1),
        ]
    );
    // No bin of the array is one of the coverage of the type.
    assert_eq!(type_counts(&mut heap, key, 1), (1, 1));
    // `f[0]` is left without a value, `f[1]` with 4 and 5.
    assert_eq!(
        listed(&heap, 0, 1),
        [
            ("f[1]".to_string(), BinReportKind::Bins, 3),
            ("f[2]".to_string(), BinReportKind::Bins, 1),
            ("ig".to_string(), BinReportKind::Ignore, 1),
        ]
    );
    assert_eq!(
        bins(&report.covergroups[0].items[1]),
        [
            ("f[1]".to_string(), BinReportKind::Bins, 4),
            ("f[2]".to_string(), BinReportKind::Bins, 2),
            ("ig".to_string(), BinReportKind::Ignore, 2),
        ]
    );
    assert_eq!(type_counts(&mut heap, key, 2), (2, 2));
}

#[test]
fn a_bin_counts_while_its_guard_holds() {
    let mut heap = Heap::new();
    // bins a = {0} iff (g0);
    // ignore_bins ig = {1} iff (g1);
    // bins b = {[1:2]};
    // illegal_bins ill = {3} iff (g2);
    // bins large = {[4:$]} with (…), evaluated for the sampled value as g70;
    // bins d = default iff (g3);
    let mut guarded_point = point(
        "p",
        point_type(4, false),
        vec![
            guarded(bins("a", vec![value(int(0))]), 0),
            guarded(ignore(bins("ig", vec![value(int(1))])), 1),
            bins("b", vec![range(int(1), int(2))]),
            guarded(illegal(bins("ill", vec![value(int(3))])), 2),
            filtered(
                bins(
                    "large",
                    vec![RangeDescriptor::Range {
                        low: Bound::Operand(Operand::Constant(int(4))),
                        high: Bound::Open,
                    }],
                ),
                BinFilter::Predicate(70),
            ),
            guarded(default_bins("d"), 3),
        ],
    );
    guarded_point.predicates = 71;
    let key = register(
        &mut heap,
        group(
            "g",
            vec![guarded_point, point("q", point_type(1, false), vec![])],
        ),
    );
    let object = construct(&mut heap, key, &[]);
    // A sample of `value` with the low predicate bits `low` and bit 70
    // `kept`: the number of illegal bins hit.
    let sample_guarded = |heap: &mut Heap, value: u64, low: u64, kept: bool| {
        call(heap, op::POINT, 1, 0, [object.into(), value, 0]);
        call(heap, op::PREDICATES, 1, 0, [object.into(), low, 0]);
        call(
            heap,
            op::PREDICATES,
            1,
            1,
            [object.into(), u64::from(kept) << 6, 0],
        );
        let hits = call(heap, op::SAMPLE_END, 0, 0, [object.into(), 0, 0]);
        assert_eq!(heap.take_fault(), None);
        hits
    };
    let counts =
        |heap: &Heap| ["a", "ig", "b", "ill", "large", "d"].map(|name| hits(heap, 0, 0, name));
    // With its guard false a bin does not count, and its value is in a bin
    // still: none of the `default` bin.
    assert_eq!(sample_guarded(&mut heap, 0, 0b1110, true), 0);
    assert_eq!(counts(&heap), [0, 0, 0, 0, 0, 0]);
    assert_eq!(sample_guarded(&mut heap, 0, 0b0001, true), 0);
    assert_eq!(counts(&heap), [1, 0, 0, 0, 0, 0]);
    // The guard of an ignore bin is that of its own hits: the value is
    // removed from `b` whether it holds or not (19.5.5).
    assert_eq!(sample_guarded(&mut heap, 1, 0b1101, true), 0);
    assert_eq!(counts(&heap), [1, 0, 0, 0, 0, 0]);
    assert_eq!(sample_guarded(&mut heap, 1, 0b0010, true), 0);
    assert_eq!(counts(&heap), [1, 1, 0, 0, 0, 0]);
    assert_eq!(sample_guarded(&mut heap, 2, 0, true), 0);
    assert_eq!(counts(&heap), [1, 1, 1, 0, 0, 0]);
    // An illegal bin whose guard is false is no error.
    assert_eq!(sample_guarded(&mut heap, 3, 0b1011, true), 0);
    assert_eq!(sample_guarded(&mut heap, 3, 0b0100, true), 1);
    assert_eq!(counts(&heap), [1, 1, 1, 1, 0, 0]);
    // A filter that is evaluated for the sampled value: the value is one
    // of the bin when the filter keeps it, and else in no bin.
    assert_eq!(sample_guarded(&mut heap, 9, 0b1000, true), 0);
    assert_eq!(counts(&heap), [1, 1, 1, 1, 1, 0]);
    assert_eq!(sample_guarded(&mut heap, 9, 0b1000, false), 0);
    assert_eq!(counts(&heap), [1, 1, 1, 1, 1, 1]);
    assert_eq!(sample_guarded(&mut heap, 9, 0b0111, false), 0);
    assert_eq!(counts(&heap), [1, 1, 1, 1, 1, 1]);
    // The bits of a word that a sample does not hand over are set.
    sample(&mut heap, object, &[0]);
    assert_eq!(sample_illegal(&mut heap, object, &[3]), 1);
    sample(&mut heap, object, &[9]);
    assert_eq!(counts(&heap), [2, 1, 1, 2, 2, 1]);
    assert_eq!(
        heap.coverage_report().covergroups[0].instances[0].samples,
        13
    );
    // Predicates of what is no coverpoint, and a word they have not.
    for (item, word, reason) in [
        (3, 0, "item 3 is not a coverpoint or cross"),
        (0, 0, "item 0 is not a coverpoint or cross"),
        (1, 2, "the predicates of item 1 have no word 2"),
        (2, 0, "the predicates of item 2 have no word 0"),
    ] {
        call(&mut heap, op::PREDICATES, item, word, [object.into(), 0, 0]);
        call(&mut heap, op::SAMPLE_END, 0, 0, [object.into(), 0, 0]);
        assert_eq!(
            fault(&mut heap),
            Some(format!("covergroup: type `g`: {reason}"))
        );
    }
}

#[test]
fn the_predicates_of_a_coverpoint_gate_its_packed_value() {
    let mut heap = Heap::new();
    // bins f = {[0:7]} with (…), evaluated for the sampled value as bit 1;
    // bins h = {[4:5]} iff (bit 0);
    // bins d = default;
    let mut second = point(
        "q",
        point_type(4, false),
        vec![
            filtered(
                bins("f", vec![range(int(0), int(7))]),
                BinFilter::Predicate(1),
            ),
            guarded(bins("h", vec![range(int(4), int(5))]), 0),
            default_bins("d"),
        ],
    );
    second.predicates = 2;
    let key = register(
        &mut heap,
        group(
            "g",
            vec![
                point(
                    "p",
                    point_type(4, false),
                    vec![bins("a", vec![value(int(1))])],
                ),
                second,
            ],
        ),
    );
    let object = construct(&mut heap, key, &[]);
    // The values of both coverpoints in one word, `p` in the low bits, and
    // the predicate bits of `q`.
    let pack = |heap: &mut Heap, q: u64, predicates: u64| {
        call(heap, op::POINTS, 1, 2, [object.into(), 1 | q << 4, 0]);
        call(heap, op::PREDICATES, 2, 0, [object.into(), predicates, 0]);
        assert_eq!(call(heap, op::SAMPLE_END, 0, 0, [object.into(), 0, 0]), 0);
        assert_eq!(heap.take_fault(), None);
    };
    // 4 is a value of `f` and of `h`.
    pack(&mut heap, 4, 0b11);
    // Not one of `f`, and `h` has it but does not count: in a bin still.
    pack(&mut heap, 4, 0b00);
    // 6 is a value of `f` only where the filter keeps it.
    pack(&mut heap, 6, 0b01);
    pack(&mut heap, 6, 0b10);
    pack(&mut heap, 9, 0b11);
    assert_eq!(
        listed(&heap, 0, 1),
        [
            ("f".to_string(), BinReportKind::Bins, 2),
            ("h".to_string(), BinReportKind::Bins, 1),
            ("d".to_string(), BinReportKind::Default, 2),
        ]
    );
    assert_eq!(instance_bins(&heap, 0, 0), [("a".to_string(), 5)]);
}

#[test]
fn an_illegal_value_is_reported_and_counted() {
    let mut heap = Heap::new();
    let key = register(
        &mut heap,
        group(
            "Top.cg",
            vec![
                point(
                    "cp_a",
                    point_type(4, false),
                    vec![
                        bins("all", vec![range(int(0), int(15))]),
                        illegal(bins("ill", vec![value(int(15))])),
                        ignore(bins("ig", vec![range(int(14), int(15))])),
                    ],
                ),
                point(
                    "cp_b",
                    point_type(4, true),
                    vec![illegal(open(bins("bad", vec![range(int(-1), int(0))])))],
                ),
            ],
        ),
    );
    let object = construct(&mut heap, key, &[]);
    assert_eq!(sample_illegal(&mut heap, object, &[15, 3]), 1);
    assert_eq!(
        message(&mut heap, object),
        "illegal_bins `ill` of coverpoint `cp_a` of covergroup `Top.cg` (instance `Top.cg#0`): \
         value 15"
    );
    // The text is given once.
    assert_eq!(message(&mut heap, object), "");
    // Two illegal values of a sample are two lines; a value of a signed
    // coverpoint is shown with its sign, and a bin of an array by its
    // name.
    assert_eq!(sample_illegal(&mut heap, object, &[15, 15]), 2);
    assert_eq!(
        message(&mut heap, object),
        "illegal_bins `ill` of coverpoint `cp_a` of covergroup `Top.cg` (instance `Top.cg#0`): \
         value 15; illegal_bins `bad[-1]` of coverpoint `cp_b` of covergroup `Top.cg` (instance \
         `Top.cg#0`): value -1"
    );
    // A sample without one leaves no text of the one before.
    assert_eq!(sample_illegal(&mut heap, object, &[15, 0]), 2);
    assert_eq!(sample_illegal(&mut heap, object, &[14, 1]), 0);
    assert_eq!(message(&mut heap, object), "");
    // "Illegal bins take precedence over any other bins": the value is one
    // of no other `bins` bin, and the run goes on.
    assert_eq!(
        listed(&heap, 0, 0),
        [
            ("all".to_string(), BinReportKind::Bins, 0),
            ("ill".to_string(), BinReportKind::Illegal, 3),
            ("ig".to_string(), BinReportKind::Ignore, 4),
        ]
    );
    assert_eq!(
        listed(&heap, 0, 1)[..2],
        [
            ("auto[-8]".to_string(), BinReportKind::Bins, 0),
            ("auto[-7]".to_string(), BinReportKind::Bins, 0),
        ]
    );
    assert_eq!(
        listed(&heap, 0, 1)[14..],
        [
            ("bad[-1]".to_string(), BinReportKind::Illegal, 1),
            ("bad[0]".to_string(), BinReportKind::Illegal, 1),
        ]
    );
    assert_eq!(hits(&heap, 0, 1, "auto[3]"), 1);
    assert_eq!(hits(&heap, 0, 1, "auto[1]"), 1);
    assert_eq!(
        heap.coverage_report().covergroups[0].instances[0].samples,
        4
    );
    // The 14 automatic bins that have a value, and `all`.
    assert_eq!(inst_counts(&mut heap, object, 0), (2, 15));
}

/// A coverpoint of 8 bits whose bins are the elements of two arrays of
/// `int`.
fn set_example() -> TypeDescriptor {
    let int_type = ValueType {
        width: 32,
        signed: true,
    };
    let mut descriptor = group(
        "s",
        vec![point(
            "p",
            point_type(8, false),
            vec![
                open(BinDescriptor::new("each", BinValues::Set(0))),
                BinDescriptor::new("all", BinValues::Set(1)),
            ],
        )],
    );
    descriptor.sets = vec![int_type, int_type];
    descriptor
}

fn push(heap: &mut Heap, object: Handle, set: u64, elements: &[i64]) {
    for &element in elements {
        let word = element as u32 as u64;
        call(heap, op::SET_PUSH, set, 0, [object.into(), word, 0]);
    }
}

#[test]
fn the_elements_of_a_set_are_the_values_of_a_bin() {
    let mut heap = Heap::new();
    let key = register(&mut heap, set_example());
    let object = start(&mut heap, key, &[]);
    // Each element is resolved as a value is (19.5.7): 300 and -1 are no
    // values of the coverpoint.
    push(&mut heap, object, 0, &[5, 7, 300, 5, -1, 6]);
    push(&mut heap, object, 1, &[10, 11, 12, 20]);
    assert_eq!(finish(&mut heap, object), 1);
    assert_eq!(
        message(&mut heap, object),
        "bins `each` of coverpoint `p` of covergroup `s` (instance `s#0`): value 300 is not a \
         value of the coverpoint type and does not take part (IEEE 1800-2023 19.5.7); so do 1 \
         more of its elements"
    );
    assert_eq!(
        names(&heap, 0, 0),
        text(&["each[5]", "each[7]", "each[6]", "all"])
    );
    for value in [5, 6, 11, 20, 13] {
        sample(&mut heap, object, &[value]);
    }
    assert_eq!(
        instance_bins(&heap, 0, 0),
        [
            ("each[5]".to_string(), 1),
            ("each[7]".to_string(), 0),
            ("each[6]".to_string(), 1),
            ("all".to_string(), 2),
        ]
    );
    // Instances of equal sets have the same bins, others their own.
    let same = start(&mut heap, key, &[]);
    push(&mut heap, same, 0, &[5, 7, 300, 5, -1, 6]);
    push(&mut heap, same, 1, &[10, 11, 12, 20]);
    assert_eq!(finish(&mut heap, same), 1);
    let other = start(&mut heap, key, &[]);
    push(&mut heap, other, 0, &[5, 7]);
    assert_eq!(finish(&mut heap, other), 0);
    assert_eq!(
        heap.coverage
            .instances
            .iter()
            .map(|instance| instance.shape)
            .collect::<Vec<_>>(),
        [Some(0), Some(0), Some(1)]
    );
    // A bin of an empty set has no value.
    assert_eq!(names(&heap, 2, 0), text(&["each[5]", "each[7]"]));
    // A set the type has not, and an element after the bins were looked
    // up.
    let object = start(&mut heap, key, &[]);
    call(&mut heap, op::SET_PUSH, 2, 0, [object.into(), 1, 0]);
    assert_eq!(
        fault(&mut heap),
        Some("covergroup: type `s`: no set 2".into())
    );
    call(&mut heap, op::SET_PUSH, 0, 1, [object.into(), 1, 0]);
    assert_eq!(
        fault(&mut heap),
        Some("covergroup: type `s`: an element of set 0 has no word 1".into())
    );
    assert_eq!(finish(&mut heap, object), 0);
    call(&mut heap, op::SET_PUSH, 0, 0, [object.into(), 1, 0]);
    assert_eq!(
        fault(&mut heap),
        Some("covergroup: type `s`: a set element of an instance that is built already".into())
    );
}

#[test]
fn set_elements_have_the_planes_and_words_of_their_type() {
    let mut heap = Heap::new();
    let element = ValueType {
        width: 100,
        signed: false,
    };
    let nibble = ValueType {
        width: 4,
        signed: false,
    };
    let mut descriptor = group(
        "g",
        vec![
            point(
                "wide",
                point_type(100, false),
                vec![open(BinDescriptor::new("w", BinValues::Set(0)))],
            ),
            point(
                "logic",
                four_state(4, false),
                vec![
                    open(BinDescriptor::new("exact", BinValues::Set(1))),
                    wildcard(open(BinDescriptor::new("wild", BinValues::Set(1)))),
                ],
            ),
        ],
    );
    descriptor.sets = vec![element, nibble];
    let key = register(&mut heap, descriptor);
    let object = start(&mut heap, key, &[]);
    // The last word completes an element; the bits above its width are
    // none of it.
    call(&mut heap, op::SET_PUSH, 0, 0, [object.into(), 9, 0]);
    call(
        &mut heap,
        op::SET_PUSH,
        0,
        1,
        [object.into(), 5 | 1 << 36, 0],
    );
    call(&mut heap, op::SET_PUSH, 0, 0, [object.into(), 7, 0]);
    // The words of an element that is being handed over are contents of
    // the heap.
    let restored: Heap = postcard::from_bytes(&postcard::to_allocvec(&heap).unwrap()).unwrap();
    assert!(restored.same_contents(&heap));
    let before = std::mem::replace(&mut heap, restored);
    call(&mut heap, op::SET_PUSH, 0, 0, [object.into(), 8, 0]);
    assert!(!before.same_contents(&heap));
    call(&mut heap, op::SET_PUSH, 0, 0, [object.into(), 7, 0]);
    assert!(before.same_contents(&heap));
    // An element that is not complete.
    call(&mut heap, op::BUILD, 0, 0, [object.into(), 0, 0]);
    assert_eq!(
        fault(&mut heap),
        Some("covergroup: type `g`: an element of set 0 is not complete".into())
    );
    call(&mut heap, op::SET_PUSH, 0, 1, [object.into(), 0, 0]);
    // 4'b11zz and 3.
    call(
        &mut heap,
        op::SET_PUSH,
        1,
        0,
        [object.into(), 0b1100, 0b0011],
    );
    call(&mut heap, op::SET_PUSH, 1, 0, [object.into(), 0xf3, 0]);
    assert_eq!(finish(&mut heap, object), 0);
    assert_eq!(
        names(&heap, 0, 0),
        text(&["w[92233720368547758089]", "w[7]"])
    );
    // The element with z bits is itself in one bin and the values it
    // stands for in the other.
    assert_eq!(
        names(&heap, 0, 1),
        text(&[
            "exact[4'b11zz]",
            "exact[3]",
            "wild[12]",
            "wild[13]",
            "wild[14]",
            "wild[15]",
            "wild[3]",
        ])
    );
    call(&mut heap, op::POINT, 2, 0, [object.into(), 0b1100, 0b0011]);
    call(&mut heap, op::SAMPLE_END, 0, 0, [object.into(), 0, 0]);
    sample_some(&mut heap, object, &[None, Some(13)]);
    assert_eq!(hits(&heap, 0, 1, "exact[4'b11zz]"), 1);
    assert_eq!(hits(&heap, 0, 1, "wild[13]"), 1);
    assert_eq!(inst_counts(&mut heap, object, 2), (2, 7));
    // The bits above the width of an element decide nothing: a
    // construction that differs in them takes the bins of this one.
    let same = start(&mut heap, key, &[]);
    call(&mut heap, op::SET_PUSH, 0, 0, [same.into(), 9, 0]);
    call(&mut heap, op::SET_PUSH, 0, 1, [same.into(), 5, 1 << 40]);
    call(&mut heap, op::SET_PUSH, 0, 0, [same.into(), 7, 0]);
    call(&mut heap, op::SET_PUSH, 0, 1, [same.into(), 1 << 50, 0]);
    call(&mut heap, op::SET_PUSH, 1, 0, [same.into(), 0b1100, 0x33]);
    call(&mut heap, op::SET_PUSH, 1, 0, [same.into(), 3, 0x10]);
    assert_eq!(finish(&mut heap, same), 0);
    assert_eq!(heap.coverage.types[0].shapes.len(), 1);
    assert_eq!(heap.coverage.instances[1].shape, Some(0));
}

#[test]
fn overlaps_are_those_of_the_values_bins_are_left_with() {
    let mut heap = Heap::new();
    // bins a[] = {[0:3]};  bins b = {[2:5]};  ignore_bins ig = {2};
    let mut descriptor = group(
        "g",
        vec![
            point(
                "p",
                point_type(3, false),
                vec![
                    open(bins("a", vec![range(int(0), int(3))])),
                    bins("b", vec![range(int(2), int(5))]),
                    ignore(bins("ig", vec![value(int(2))])),
                ],
            ),
            point(
                "many",
                point_type(8, false),
                vec![
                    open(bins("a", vec![range(int(0), int(99))])),
                    open(bins("b", vec![range(int(0), int(99))])),
                ],
            ),
            // A value with x and z bits that two bins list.
            point(
                "logic",
                four_state(4, false),
                vec![
                    bins("a", vec![value(pattern("1x0z")), value(int(1))]),
                    bins("b", vec![value(pattern("1x0z")), value(int(2))]),
                    bins("c", vec![value(pattern("1x0x"))]),
                ],
            ),
        ],
    );
    descriptor.options = vec![option(0, OptionId::DetectOverlap, 1)];
    let key = register(&mut heap, descriptor);
    let (object, warnings) = construct_with_warnings(&mut heap, key, &[]);
    // The pair that shares 3, not the one that shared the ignored 2; and
    // of the hundred pairs of the second coverpoint the first 64.
    assert_eq!(warnings, 1 + 64 + 1 + 1);
    let message = message(&mut heap, object);
    let lines: Vec<&str> = message.split("; ").collect();
    assert_eq!(
        lines[0],
        "coverpoint `p` of covergroup `g` (instance `g#0`): bins `a[3]` and `b` overlap \
         (option.detect_overlap)"
    );
    assert_eq!(
        lines[1],
        "coverpoint `many` of covergroup `g` (instance `g#0`): bins `a[0]` and `b[0]` overlap \
         (option.detect_overlap)"
    );
    assert_eq!(
        lines[65],
        "coverpoint `many` of covergroup `g` (instance `g#0`): more bins overlap \
         (option.detect_overlap)"
    );
    assert_eq!(
        lines[66],
        "coverpoint `logic` of covergroup `g` (instance `g#0`): bins `a` and `b` overlap \
         (option.detect_overlap)"
    );
}

#[test]
fn descriptors_with_bins_they_may_not_have_are_not_registered() {
    let ty = point_type(4, false);
    let rejected =
        |bins: Vec<BinDescriptor>, change: &dyn Fn(&mut TypeDescriptor), reason: &str| {
            let mut heap = Heap::new();
            let mut descriptor = group("g", vec![point("p", ty.clone(), bins)]);
            change(&mut descriptor);
            heap.register_cover_type(1, Arc::new(descriptor));
            assert_eq!(
                fault(&mut heap),
                Some(format!("covergroup: descriptor of type `g`: {reason}"))
            );
            assert_eq!(call(&mut heap, op::REGISTERED, 0, 0, [1, 0, 0]), 0);
        };
    let unchanged = |_: &mut TypeDescriptor| {};
    let all = || bins("b", vec![range(int(0), int(15))]);
    // "A default bin cannot be explicitly ignored" (19.5).
    rejected(
        vec![ignore(default_bins("b"))],
        &unchanged,
        "bin `b` of coverpoint `p` is an ignored `default` bin",
    );
    rejected(
        vec![fixed(default_bins("b"), Operand::Constant(int(2)))],
        &unchanged,
        "bin `b` of coverpoint `p` is a `default` bin array with a size",
    );
    rejected(
        vec![wildcard(default_bins("b"))],
        &unchanged,
        "bin `b` of coverpoint `p` is a wildcard `default` bin",
    );
    rejected(
        vec![BinDescriptor::new("b", BinValues::Set(0))],
        &unchanged,
        "bin `b` of coverpoint `p` has set 0, which is none",
    );
    rejected(
        vec![fixed(all(), Operand::Input(0))],
        &unchanged,
        "input 0 is not an integral input",
    );
    rejected(
        vec![guarded(all(), 0)],
        &unchanged,
        "coverpoint `p` has no predicate bit 0",
    );
    rejected(
        vec![filtered(all(), BinFilter::Predicate(1))],
        &|descriptor| descriptor.points[0].predicates = 1,
        "coverpoint `p` has no predicate bit 1",
    );
    // A filter that is evaluated for each sample is one of a scalar bin
    // of the coverage.
    for bin in [open(all()), ignore(all())] {
        rejected(
            vec![filtered(bin, BinFilter::Predicate(0))],
            &|descriptor| descriptor.points[0].predicates = 1,
            "bin `b` of coverpoint `p` is filtered at each sample but no scalar `bins`",
        );
    }
    rejected(
        vec![filtered(all(), BinFilter::Enumerate(0))],
        &unchanged,
        "bin `b` of coverpoint `p` has filter 0, which is none",
    );
    rejected(
        vec![filtered(all(), BinFilter::Enumerate(0))],
        &|descriptor| descriptor.filters = vec![item_filter(&point_type(4, true))],
        "bin `b` of coverpoint `p` has filter 0, which is not of its values",
    );
    rejected(
        vec![
            filtered(all(), BinFilter::Enumerate(0)),
            filtered(all(), BinFilter::Enumerate(0)),
        ],
        &|descriptor| descriptor.filters = vec![item_filter(&ty)],
        "bin `b` of coverpoint `p` has filter 0, which another bin has",
    );
    rejected(
        vec![all()],
        &|descriptor| descriptor.filters = vec![item_filter(&ty)],
        "filter 0 is of no bin",
    );
    rejected(
        vec![all()],
        &|descriptor| {
            descriptor.sets = vec![ValueType {
                width: 0,
                signed: false,
            }]
        },
        "a set of elements of no bits",
    );
}

#[test]
fn bins_of_every_kind_survive_a_round_trip() {
    let mut heap = Heap::new();
    let filters = register(&mut heap, filter_example());
    let sets = register(&mut heap, set_example());
    let mut defaults = group(
        "d",
        vec![point(
            "p",
            four_state(4, false),
            vec![
                guarded(bins("a", vec![value(int(0))]), 0),
                wildcard(open(bins("w", vec![value(pattern("11??"))]))),
                illegal(bins("ill", vec![value(int(1))])),
                open(default_bins("others")),
            ],
        )],
    );
    defaults.points[0].predicates = 1;
    let defaults = register(&mut heap, defaults);
    let (with_filters, ..) = construct_filter_example(&mut heap, filters);
    let with_sets = start(&mut heap, sets, &[]);
    push(&mut heap, with_sets, 0, &[1, 2]);
    push(&mut heap, with_sets, 1, &[3]);
    assert_eq!(finish(&mut heap, with_sets), 0);
    let with_defaults = construct(&mut heap, defaults, &[]);
    sample_illegal(&mut heap, with_filters, &[1, 3, 3, 1]);
    sample(&mut heap, with_sets, &[2]);
    sample(&mut heap, with_defaults, &[5]);
    assert_eq!(
        sample_planes(&mut heap, with_defaults, 1, 0b1100, 0b0001),
        0
    );
    // A construction that is in the middle of a filter and of a set. With
    // another value of the option it does not take the bins of the first,
    // so it has values to ask about.
    set_type_option(&mut heap, filters, 0, TypeOptionId::DistributeFirst, 1);
    let half = start(&mut heap, filters, &[]);
    ask(&mut heap, half, 0, |item| item % 2 == 0);
    for keep in [0, 1, 1] {
        call(&mut heap, op::FILTER_NEXT, 1, 0, [half.into(), keep, 0]);
    }
    let pushed = start(&mut heap, sets, &[]);
    push(&mut heap, pushed, 0, &[9]);
    assert_eq!(heap.take_fault(), None);

    // A checkpoint file is the postcard encoding of the heap.
    let mut restored: Heap = postcard::from_bytes(&postcard::to_allocvec(&heap).unwrap()).unwrap();
    assert!(restored.same_contents(&heap));
    assert_eq!(restored.coverage_report(), heap.coverage_report());
    assert_eq!(restored.content_size(), heap.content_size());
    let mut clone = heap.clone();
    for heap in [&mut heap, &mut restored, &mut clone] {
        // The construction goes on where it was: the filter is at 2.
        assert_eq!(call(heap, op::FILTER_ITEM, 1, 0, [half.into(), 0, 0]), 2);
        ask(heap, half, 1, |item| item % 3 == 0);
        ask(heap, half, 2, |item| item % 3 == 0);
        ask(heap, half, 3, |item| item >= 2);
        assert_eq!(finish(heap, half), 0);
        push(heap, pushed, 0, &[8]);
        assert_eq!(finish(heap, pushed), 0);
        assert_eq!(sample_illegal(heap, half, &[2, 3, 4, 3]), 1);
        sample(heap, pushed, &[8]);
        sample(heap, with_defaults, &[5]);
        sample(heap, with_defaults, &[13]);
        assert_eq!(sample_illegal(heap, with_defaults, &[1]), 1);
        // The other filter of the ignore bin: the even values are removed.
        assert_eq!(
            heap.coverage_report().covergroups[1].instances[1].items[0]
                .bins
                .iter()
                .map(|bin| bin.name.clone())
                .collect::<Vec<_>>(),
            text(&["b[1]", "b[3]", "b[5]", "b[7]", "odd"])
        );
        assert_eq!(
            listed(heap, 0, 0),
            [
                ("a".to_string(), BinReportKind::Bins, 0),
                ("w[12]".to_string(), BinReportKind::Bins, 0),
                ("w[13]".to_string(), BinReportKind::Bins, 1),
                ("w[14]".to_string(), BinReportKind::Bins, 0),
                ("w[15]".to_string(), BinReportKind::Bins, 0),
                ("ill".to_string(), BinReportKind::Illegal, 1),
                ("others[5]".to_string(), BinReportKind::Default, 2),
                ("others[4'b110z]".to_string(), BinReportKind::Default, 1),
            ]
        );
    }
    assert!(restored.same_contents(&heap));
    assert!(clone.same_contents(&heap));
    assert_eq!(restored.coverage_report(), heap.coverage_report());
    // What a `default` bin array counts, the elements of a set and the
    // answers of a filter are contents.
    sample(&mut clone, with_defaults, &[6]);
    assert!(!clone.same_contents(&heap));
    let mut clone = heap.clone();
    let (first, second) = (start(&mut heap, sets, &[]), start(&mut clone, sets, &[]));
    assert_eq!(first, second);
    assert!(clone.same_contents(&heap));
    push(&mut clone, second, 0, &[1]);
    assert!(!clone.same_contents(&heap));
    let ty = point_type(3, false);
    let mut asking = group(
        "f",
        vec![point(
            "p",
            ty.clone(),
            vec![filtered(
                bins("b", vec![range(int(0), int(7))]),
                BinFilter::Enumerate(0),
            )],
        )],
    );
    asking.filters = vec![item_filter(&ty)];
    let asking = register(&mut heap, asking);
    let mut clone = heap.clone();
    let (first, second) = (
        start(&mut heap, asking, &[]),
        start(&mut clone, asking, &[]),
    );
    for heap in [&mut heap, &mut clone] {
        call(heap, op::FILTER_NEXT, 0, 0, [first.into(), 0, 0]);
    }
    assert!(clone.same_contents(&heap));
    call(&mut heap, op::FILTER_NEXT, 0, 0, [first.into(), 0, 0]);
    call(&mut clone, op::FILTER_NEXT, 0, 0, [second.into(), 1, 0]);
    assert!(!clone.same_contents(&heap));
    // The descriptors and the report are JSON.
    let mut every = filter_example();
    every.sets = set_example().sets;
    every.points.extend(set_example().points);
    every.points[0].predicates = 2;
    every.points[0].bins.extend([
        guarded(wildcard(bins("w", vec![value(pattern("1?0"))])), 1),
        filtered(bins("f", vec![value(int(1))]), BinFilter::Predicate(0)),
        fixed(bins("n", vec![value(int(1))]), Operand::Constant(int(2))),
        illegal(open(default_bins("d"))),
    ]);
    assert_eq!(every.validate(), Ok(()));
    let json = serde_json::to_string(&every).unwrap();
    assert_eq!(
        serde_json::from_str::<TypeDescriptor>(&json).unwrap(),
        every
    );
    let report = heap.coverage_report();
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(
        serde_json::from_value::<CoverageReport>(json.clone()).unwrap(),
        report
    );
    let bins = &json["covergroups"][0]["instances"][0]["items"][0]["bins"];
    assert_eq!(
        bins[5],
        serde_json::json!({"name": "ill", "kind": "illegal", "hits": 1, "covered": true})
    );
    assert_eq!(bins[6]["kind"], "default");
}

/// `value_count` is how many values a `with` filter is asked about for an
/// element of a bin: those that take part (19.5.7), a repeated one as often
/// as it is listed, and none for a wildcard value that is kept as a
/// pattern.
#[test]
fn the_values_of_an_element_are_counted_as_a_filter_is_asked_about_them() {
    let count = |ty: &PointType, wildcard: bool, range: RangeValue<'_>| {
        value_count(ty, wildcard, range).map(|count| count.to_string())
    };
    let byte = point_type(8, false);
    let (three, three_hundred) = (int(3), int(300));
    assert_eq!(
        count(&byte, false, RangeValue::Value(&three)).as_deref(),
        Some("1")
    );
    // 300 is no value of 8 bits, and of 3 to 300 those up to 255 are.
    assert_eq!(
        count(&byte, false, RangeValue::Value(&three_hundred)).as_deref(),
        Some("0")
    );
    assert_eq!(
        count(
            &byte,
            false,
            RangeValue::Range {
                low: Some(&three),
                high: Some(&three_hundred),
            },
        )
        .as_deref(),
        Some("253")
    );
    // `[$:$]` is every value of the type, also of 64 bits.
    assert_eq!(
        count(
            &point_type(64, true),
            false,
            RangeValue::Range {
                low: None,
                high: None,
            },
        )
        .as_deref(),
        Some("18446744073709551616")
    );
    // `4'b1??1` is four values as a wildcard value and one value with x
    // bits otherwise, which a 2-state coverpoint has not.
    let nibble = point_type(4, false);
    let ends = pattern("1??1");
    assert_eq!(
        count(&nibble, true, RangeValue::Value(&ends)).as_deref(),
        Some("4")
    );
    assert_eq!(
        count(&nibble, false, RangeValue::Value(&ends)).as_deref(),
        Some("0")
    );
    // 65,536 ranges of values are listed, twice as many are a pattern.
    let ends = |free: usize| pattern(&format!("1{}1", "?".repeat(free)));
    assert_eq!(
        count(&point_type(18, false), true, RangeValue::Value(&ends(16))).as_deref(),
        Some("65536")
    );
    assert_eq!(
        count(&point_type(19, false), true, RangeValue::Value(&ends(17))),
        None
    );
}

/// What a `with` filter reads beside the integral inputs decides the bins
/// of a construction too: a string that no option takes is part of what an
/// earlier construction must have handed over for its bins to be taken,
/// the string of an option is not. A type with a filter that does not
/// share has the bins each of its constructions is asked about.
#[test]
fn bins_are_shared_by_what_the_filters_read() {
    let ty = point_type(3, false);
    // bins b[] = {[0:5]} with (s == "x" ? item < 2 : item < 4);
    let descriptor = |shared: bool| {
        let mut descriptor = group(
            "g",
            vec![point(
                "p",
                ty.clone(),
                vec![open(filtered(
                    bins("b", vec![range(int(0), int(5))]),
                    BinFilter::Enumerate(0),
                ))],
            )],
        );
        descriptor.inputs = vec![InputDescriptor::String, InputDescriptor::String];
        descriptor.options = vec![OptionInit {
            item: 0,
            option: OptionId::Name,
            value: OptionValue::StringInput(0),
        }];
        let mut filter = item_filter(&ty);
        filter.shared = shared;
        descriptor.filters = vec![filter];
        descriptor
    };
    let build = |heap: &mut Heap, key: u64, name: &str, read: &str| {
        let object = start(heap, key, &[]);
        for (input, text) in [name, read].into_iter().enumerate() {
            let text = heap.intern(text.as_bytes());
            call(
                heap,
                op::INPUT_STRING,
                input as u64,
                0,
                [object.into(), text.into(), 0],
            );
        }
        let limit = if read == "x" { 2 } else { 4 };
        let asked = ask(heap, object, 0, |item| item < limit);
        assert_eq!(finish(heap, object), 0);
        asked.len()
    };
    let mut heap = Heap::new();
    let key = register(&mut heap, descriptor(true));
    assert_eq!(build(&mut heap, key, "first", "x"), 6);
    assert_eq!(names(&heap, 0, 0), text(&["b[0]", "b[1]"]));
    // Another string for the filter: other bins, which are asked about.
    assert_eq!(build(&mut heap, key, "second", "y"), 6);
    assert_eq!(names(&heap, 1, 0), text(&["b[0]", "b[1]", "b[2]", "b[3]"]));
    // The strings of the first again, under another name: its bins.
    assert_eq!(build(&mut heap, key, "third", "x"), 0);
    assert_eq!(names(&heap, 2, 0), text(&["b[0]", "b[1]"]));
    assert_eq!(build(&mut heap, key, "fourth", "y"), 0);
    assert_eq!(names(&heap, 3, 0), names(&heap, 1, 0));
    assert_eq!(heap.coverage.types[0].shapes.len(), 2);
    // A filter that does not share is asked by every construction.
    let mut heap = Heap::new();
    let key = register(&mut heap, descriptor(false));
    for (instance, read) in ["x", "x", "y", "x"].into_iter().enumerate() {
        assert_eq!(build(&mut heap, key, "same", read), 6);
        assert_eq!(
            names(&heap, instance, 0).len(),
            if read == "x" { 2 } else { 4 }
        );
    }
    assert_eq!(heap.coverage.types[0].shapes.len(), 4);
    // The instances and their bins survive a checkpoint.
    let bytes = postcard::to_allocvec(&heap).unwrap();
    let restored: Heap = postcard::from_bytes(&bytes).unwrap();
    assert!(restored.same_contents(&heap));
    assert_eq!(names(&restored, 2, 0).len(), 4);
}
