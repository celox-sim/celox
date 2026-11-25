//! Tests of the coverage operations through [`Heap::call`], with
//! descriptors written out as the analyzer builds them.

use std::sync::Arc;

use num_bigint::BigUint;

use super::*;
use crate::{HandleKind, Root};

const CLASS: u32 = 77;

fn call(heap: &mut Heap, sub: u64, x: u64, y: u64, arguments: [u64; 3]) -> u64 {
    heap.call(code(sub, x, y), arguments[0], arguments[1], arguments[2])
}

fn point_type(width: u32, signed: bool) -> PointType {
    PointType {
        width,
        signed,
        four_state: false,
        members: Vec::new(),
    }
}

/// The constant of `width` bits whose bits are those of `value`.
fn constant(width: u32, signed: bool, value: i128) -> Constant {
    let bits = BigUint::from(value as u128) & ((BigUint::from(1u8) << width) - 1u8);
    Constant {
        ty: ValueType { width, signed },
        value: bits,
        mask: BigUint::ZERO,
    }
}

/// An `int` literal.
fn int(value: i64) -> Constant {
    constant(32, true, value.into())
}

/// A sized unsigned literal.
fn sized(width: u32, value: u64) -> Constant {
    constant(width, false, value.into())
}

fn value(constant: Constant) -> RangeDescriptor {
    RangeDescriptor::Value(Operand::Constant(constant))
}

fn range(low: Constant, high: Constant) -> RangeDescriptor {
    RangeDescriptor::Range {
        low: Bound::Operand(Operand::Constant(low)),
        high: Bound::Operand(Operand::Constant(high)),
    }
}

fn input_range(low: u32, high: u32) -> RangeDescriptor {
    RangeDescriptor::Range {
        low: Bound::Operand(Operand::Input(low)),
        high: Bound::Operand(Operand::Input(high)),
    }
}

fn bins(name: &str, ranges: Vec<RangeDescriptor>) -> BinDescriptor {
    BinDescriptor::new(name, BinValues::ranges(ranges))
}

fn point(name: &str, ty: PointType, bins: Vec<BinDescriptor>) -> PointDescriptor {
    let mut point = PointDescriptor::new(name, ty);
    point.bins = bins;
    point
}

fn group(name: &str, points: Vec<PointDescriptor>) -> TypeDescriptor {
    let mut descriptor = TypeDescriptor::new(name);
    descriptor.points = points;
    descriptor
}

fn option(item: u32, option: OptionId, value: i64) -> OptionInit {
    OptionInit {
        item,
        option,
        value: OptionValue::Operand(Operand::Constant(int(value))),
    }
}

fn option_input(item: u32, option: OptionId, input: u32) -> OptionInit {
    OptionInit {
        item,
        option,
        value: OptionValue::Operand(Operand::Input(input)),
    }
}

fn int_inputs(count: usize) -> Vec<InputDescriptor> {
    vec![
        InputDescriptor::Integral(ValueType {
            width: 32,
            signed: true,
        });
        count
    ]
}

/// Register `descriptor` under the key of its name.
fn register(heap: &mut Heap, descriptor: TypeDescriptor) -> u64 {
    let key = type_key(&descriptor.name);
    heap.register_cover_type(key, Arc::new(descriptor));
    assert_eq!(heap.take_fault(), None);
    assert_eq!(call(heap, op::REGISTERED, 0, 0, [key, 0, 0]), 1);
    key
}

/// A new instance of the type of `key` built from the `int` inputs
/// `inputs`: its handle and the number of warnings.
fn construct_with_warnings(heap: &mut Heap, key: u64, inputs: &[i64]) -> (Handle, u64) {
    let object = call(heap, op::NEW, 0, 0, [key, 8, CLASS.into()]);
    for (index, &input) in inputs.iter().enumerate() {
        let word = input as u32 as u64;
        call(heap, op::INPUT, index as u64, 0, [object, word, 0]);
    }
    let warnings = call(heap, op::BUILD, 0, 0, [object, 0, 0]);
    assert_eq!(heap.take_fault(), None);
    (object as Handle, warnings)
}

fn construct(heap: &mut Heap, key: u64, inputs: &[i64]) -> Handle {
    let (object, warnings) = construct_with_warnings(heap, key, inputs);
    assert_eq!(warnings, 0, "{}", message(heap, object));
    object
}

fn message(heap: &mut Heap, object: Handle) -> String {
    let handle = call(heap, op::MESSAGE, 0, 0, [object.into(), 0, 0]);
    String::from_utf8(heap.string(handle as Handle).to_vec()).unwrap()
}

/// Sample `values`, the value of each coverpoint in turn or `None` for one
/// that is not sampled, with one `POINT` call each.
fn sample_some(heap: &mut Heap, object: Handle, values: &[Option<u64>]) {
    assert_eq!(call(heap, op::SAMPLING, 0, 0, [object.into(), 0, 0]), 1);
    for (index, value) in values.iter().enumerate() {
        if let Some(value) = value {
            let item = index as u64 + 1;
            call(heap, op::POINT, item, 0, [object.into(), *value, 0]);
        }
    }
    assert_eq!(call(heap, op::SAMPLE_END, 0, 0, [object.into(), 0, 0]), 0);
}

fn sample(heap: &mut Heap, object: Handle, values: &[u64]) {
    let values: Vec<Option<u64>> = values.iter().copied().map(Some).collect();
    sample_some(heap, object, &values);
}

fn inst_coverage(heap: &mut Heap, object: Handle, item: u64) -> f64 {
    f64::from_bits(call(
        heap,
        op::INST_COVERAGE,
        0,
        0,
        [object.into(), item, 0],
    ))
}

/// The covered bins and the bins of `get_inst_coverage(ref int, ref int)`.
fn inst_counts(heap: &mut Heap, object: Handle, item: u64) -> (u64, u64) {
    (
        call(heap, op::INST_COVERAGE, 1, 0, [object.into(), item, 0]),
        call(heap, op::INST_COVERAGE, 2, 0, [object.into(), item, 0]),
    )
}

fn type_coverage(heap: &mut Heap, key: u64, item: u64) -> f64 {
    f64::from_bits(call(heap, op::TYPE_COVERAGE, 0, 0, [key, item, 0]))
}

fn type_counts(heap: &mut Heap, key: u64, item: u64) -> (u64, u64) {
    (
        call(heap, op::TYPE_COVERAGE, 1, 0, [key, item, 0]),
        call(heap, op::TYPE_COVERAGE, 2, 0, [key, item, 0]),
    )
}

fn overall(heap: &mut Heap) -> f64 {
    f64::from_bits(call(heap, op::OVERALL, 0, 0, [0, 0, 0]))
}

fn set_option(heap: &mut Heap, object: Handle, item: u64, option: OptionId, value: u64) {
    let option = u64::from(option.code());
    call(
        heap,
        op::SET_OPTION,
        item,
        option,
        [object.into(), value, 0],
    );
}

fn get_option(heap: &mut Heap, object: Handle, item: u64, option: OptionId) -> u64 {
    let option = u64::from(option.code());
    call(heap, op::GET_OPTION, item, option, [object.into(), 0, 0])
}

fn set_type_option(heap: &mut Heap, key: u64, item: u64, option: TypeOptionId, value: u64) {
    let option = u64::from(option.code());
    call(heap, op::SET_TYPE_OPTION, item, option, [key, value, 0]);
}

fn get_type_option(heap: &mut Heap, key: u64, item: u64, option: TypeOptionId) -> u64 {
    let option = u64::from(option.code());
    call(heap, op::GET_TYPE_OPTION, item, option, [key, 0, 0])
}

#[track_caller]
fn assert_close(actual: f64, expected: f64) {
    assert!(
        (actual - expected).abs() < 1e-9,
        "{actual} is not {expected}"
    );
}

/// The names and hits of the bins of coverpoint `point` of instance
/// `instance` of the first covergroup of the report.
fn instance_bins(heap: &Heap, instance: usize, point: usize) -> Vec<(String, u64)> {
    heap.coverage_report().covergroups[0].instances[instance].items[point]
        .bins
        .iter()
        .map(|bin| (bin.name.clone(), bin.hits))
        .collect()
}

fn ranges(resolved: &Resolved) -> Vec<(i64, i64)> {
    resolved
        .items
        .iter()
        .map(|item| match item {
            ResolvedItem::Range { low, high } => {
                (i64::try_from(low).unwrap(), i64::try_from(high).unwrap())
            }
            ResolvedItem::Exact { .. } => panic!("a value with x or z bits"),
        })
        .collect()
}

fn resolve_value(ty: &PointType, constant: Constant) -> Resolved {
    resolve(ty, RangeValue::Value(&constant))
}

fn resolve_range(ty: &PointType, low: Constant, high: Constant) -> Resolved {
    resolve(
        ty,
        RangeValue::Range {
            low: Some(&low),
            high: Some(&high),
        },
    )
}

/// The bins `b1` to `b4` of the example of 19.5.7.
fn resolution_example() -> TypeDescriptor {
    let first = || vec![value(int(1)), range(int(2), int(5)), range(int(6), int(10))];
    let second = || vec![value(int(-1)), range(int(1), int(10)), value(int(15))];
    group(
        "g1",
        vec![
            point(
                "p1",
                point_type(3, false),
                vec![bins("b1", first()), bins("b2", second())],
            ),
            point(
                "p2",
                point_type(3, true),
                vec![bins("b3", first()), bins("b4", second())],
            ),
        ],
    )
}

#[test]
fn bin_values_resolve_as_the_examples_of_19_5_7() {
    let p1 = point_type(3, false);
    let p2 = point_type(3, true);
    // b1: "a warning is issued for the range [6:10]", treated as
    // {1, [2:5], [6:7]}.
    assert_eq!(ranges(&resolve_value(&p1, int(1))), [(1, 1)]);
    assert_eq!(ranges(&resolve_range(&p1, int(2), int(5))), [(2, 5)]);
    let clipped = resolve_range(&p1, int(6), int(10));
    assert_eq!(ranges(&clipped), [(6, 7)]);
    assert_eq!(clipped.warnings.len(), 1);
    // b2: warnings for [1:10], -1 and 15, treated as {[1:7]}.
    let negative = resolve_value(&p1, int(-1));
    assert_eq!((ranges(&negative), negative.warnings.len()), (vec![], 1));
    let clipped = resolve_range(&p1, int(1), int(10));
    assert_eq!(
        (ranges(&clipped), clipped.warnings.len()),
        (vec![(1, 7)], 1)
    );
    let large = resolve_value(&p1, int(15));
    assert_eq!((ranges(&large), large.warnings.len()), (vec![], 1));
    // b3: warnings for [2:5] and [6:10], treated as {1, [2:3]}.
    assert_eq!(ranges(&resolve_value(&p2, int(1))), [(1, 1)]);
    let clipped = resolve_range(&p2, int(2), int(5));
    assert_eq!(
        (ranges(&clipped), clipped.warnings.len()),
        (vec![(2, 3)], 1)
    );
    let outside = resolve_range(&p2, int(6), int(10));
    assert_eq!((ranges(&outside), outside.warnings.len()), (vec![], 1));
    // b4: warnings for [1:10] and 15, treated as {-1, [1:3]}.
    let negative = resolve_value(&p2, int(-1));
    assert_eq!(
        (ranges(&negative), negative.warnings.len()),
        (vec![(-1, -1)], 0)
    );
    let clipped = resolve_range(&p2, int(1), int(10));
    assert_eq!(
        (ranges(&clipped), clipped.warnings.len()),
        (vec![(1, 3)], 1)
    );
    let large = resolve_value(&p2, int(15));
    assert_eq!((ranges(&large), large.warnings.len()), (vec![], 1));
}

#[test]
fn a_build_resolves_bin_values_and_reports_the_warnings() {
    let mut heap = Heap::new();
    let key = register(&mut heap, resolution_example());
    let (object, warnings) = construct_with_warnings(&mut heap, key, &[]);
    assert_eq!(warnings, 8);
    let text = message(&mut heap, object);
    assert_eq!(text.matches("; ").count(), 7);
    assert!(
        text.starts_with(
            "bins `b1` of coverpoint `p1` of covergroup `g1` (instance `g1#0`): range [6:10] is \
             limited to the values of the coverpoint type (IEEE 1800-2023 19.5.7); bins `b2` of \
             coverpoint `p1` of covergroup `g1` (instance `g1#0`): value -1 is not a value of the \
             coverpoint type and does not take part (IEEE 1800-2023 19.5.7)"
        ),
        "{text}"
    );
    assert!(
        text.contains(
            "bins `b3` of coverpoint `p2` of covergroup `g1` (instance `g1#0`): range \
             [6:10] holds no value of the coverpoint type and does not take part"
        ),
        "{text}"
    );
    // The text is given once.
    assert_eq!(message(&mut heap, object), "");
    // Every value of both coverpoints, once each.
    for bits in 0..8 {
        sample(&mut heap, object, &[bits, bits]);
    }
    assert_eq!(
        instance_bins(&heap, 0, 0),
        [("b1".to_string(), 7), ("b2".to_string(), 7)]
    );
    assert_eq!(
        instance_bins(&heap, 0, 1),
        [("b3".to_string(), 3), ("b4".to_string(), 4)]
    );
    // -1 of `p2` is in `b4` only.
    let before = instance_bins(&heap, 0, 1);
    sample_some(&mut heap, object, &[None, Some(0b111)]);
    let after = instance_bins(&heap, 0, 1);
    assert_eq!((after[0].1, after[1].1), (before[0].1, before[1].1 + 1));
    // A second instance of the same values warns again.
    let (second, warnings) = construct_with_warnings(&mut heap, key, &[]);
    assert_eq!(warnings, 8);
    assert!(message(&mut heap, second).contains("(instance `g1#1`)"));
    assert_eq!(heap.coverage.types[0].shapes.len(), 1);
}

#[test]
fn sized_unsigned_literals_keep_their_bits_on_a_signed_coverpoint() {
    let s = point_type(8, true);
    // 8'hFF == s holds for s == -1, as the cast keeps the bits.
    let all = resolve_value(&s, sized(8, 0xff));
    assert_eq!((ranges(&all), all.warnings.len()), (vec![(-1, -1)], 0));
    let negative = resolve_range(&s, sized(8, 0x80), sized(8, 0xff));
    assert_eq!(
        (ranges(&negative), negative.warnings.len()),
        (vec![(-128, -1)], 0)
    );
    let both = resolve_range(&s, sized(8, 0x70), sized(8, 0x90));
    assert_eq!(
        (ranges(&both), both.warnings.len()),
        (vec![(112, 127), (-128, -112)], 0)
    );
    // 9'h1FF does not equal its cast to 8 bits.
    let wide = resolve_value(&s, sized(9, 0x1ff));
    assert_eq!((ranges(&wide), wide.warnings.len()), (vec![], 1));
    assert!(wide.warnings[0].starts_with("value 511 is not a value of the coverpoint type"));
    // A range that reaches past the bit patterns of the type is limited.
    let limited = resolve_range(&s, sized(9, 0xf0), sized(9, 0x1ff));
    assert_eq!(
        (ranges(&limited), limited.warnings.len()),
        (vec![(-16, -1)], 1)
    );
}

#[test]
fn signed_literals_on_an_unsigned_coverpoint_must_not_be_negative() {
    let u = point_type(8, false);
    let positive = resolve_value(&u, constant(8, true, 100));
    assert_eq!(
        (ranges(&positive), positive.warnings.len()),
        (vec![(100, 100)], 0)
    );
    // 8'sh80 is -128.
    let negative = resolve_value(&u, constant(8, true, -128));
    assert_eq!((ranges(&negative), negative.warnings.len()), (vec![], 1));
    assert!(negative.warnings[0].starts_with("value -128 is not"));
    let straddling = resolve_range(&u, int(-2), int(3));
    assert_eq!(
        (ranges(&straddling), straddling.warnings.len()),
        (vec![(0, 3)], 1)
    );
    let large = resolve_range(&u, int(200), int(300));
    assert_eq!(
        (ranges(&large), large.warnings.len()),
        (vec![(200, 255)], 1)
    );
    // An empty range is not worth a warning (11.4.13).
    let empty = resolve_range(&u, int(5), int(4));
    assert_eq!(empty, Resolved::default());
}

#[test]
fn range_bounds_of_two_types_take_their_common_type() {
    let s = point_type(8, true);
    // 4'sd3 and 8'd200 are unsigned together: 3 to 200 as bit patterns.
    let mixed = resolve_range(&s, constant(4, true, 3), sized(8, 200));
    assert_eq!(
        (ranges(&mixed), mixed.warnings.len()),
        (vec![(3, 127), (-128, -56)], 0)
    );
    // 4'sb1111 is extended by zeros beside an unsigned bound: 15, not -1,
    // which is worth a warning.
    let extended = resolve_range(&s, constant(4, true, -1), sized(8, 20));
    assert_eq!(ranges(&extended), [(15, 20)]);
    assert_eq!(
        extended.warnings,
        [
            "bound -1 of range [-1:20] is read as 15 beside an unsigned bound (IEEE 1800-2023 \
             19.5.7)"
        ]
    );
    // Two signed bounds keep their values.
    let signed = resolve_range(&s, constant(4, true, -1), constant(8, true, 20));
    assert_eq!(
        (ranges(&signed), signed.warnings.len()),
        (vec![(-1, 20)], 0)
    );
    // -1 beside an unsigned bound is the largest value of 32 bits, so the
    // range is empty. A negative value on an unsigned coverpoint warns
    // (19.5.7 b) 1), the empty range does not.
    let u = point_type(8, false);
    let empty = resolve_range(&u, int(-1), sized(8, 5));
    assert!(empty.items.is_empty());
    assert_eq!(
        empty.warnings,
        [
            "bound -1 of range [-1:5] is read as 4294967295 beside an unsigned bound (IEEE \
             1800-2023 19.5.7)"
        ]
    );
    // As the high bound it also limits the range to the type.
    let limited = resolve_range(&u, sized(8, 5), int(-1));
    assert_eq!(ranges(&limited), [(5, 255)]);
    assert_eq!(limited.warnings.len(), 2);
    assert!(limited.warnings[0].starts_with("bound -1 of range [5:-1] is read as 4294967295"));
    assert!(limited.warnings[1].starts_with("range [5:-1] is limited to the values"));
}

#[test]
fn open_bounds_are_the_ends_of_the_coverpoint_type() {
    let open = |ty: &PointType, low: Option<Constant>, high: Option<Constant>| {
        resolve(
            ty,
            RangeValue::Range {
                low: low.as_ref(),
                high: high.as_ref(),
            },
        )
    };
    let u = point_type(4, false);
    assert_eq!(ranges(&open(&u, None, Some(int(3)))), [(0, 3)]);
    assert_eq!(ranges(&open(&u, Some(int(12)), None)), [(12, 15)]);
    assert_eq!(ranges(&open(&u, None, None)), [(0, 15)]);
    let s = point_type(8, true);
    assert_eq!(ranges(&open(&s, None, Some(int(-100)))), [(-128, -100)]);
    assert_eq!(ranges(&open(&s, Some(int(100)), None)), [(100, 127)]);
    // `$` is the lowest value of the type also beside a bit pattern.
    assert_eq!(
        ranges(&open(&s, None, Some(sized(8, 0x90)))),
        [(-128, -112)]
    );
    // A bound outside the type limits the range, or leaves nothing.
    let limited = open(&u, None, Some(int(20)));
    assert_eq!(
        (ranges(&limited), limited.warnings.len()),
        (vec![(0, 15)], 1)
    );
    let nothing = open(&u, None, Some(int(-1)));
    assert_eq!((ranges(&nothing), nothing.warnings.len()), (vec![], 1));
    assert!(nothing.warnings[0].starts_with("range [$:-1] holds no value"));
    let nothing = open(&u, Some(int(16)), None);
    assert_eq!((ranges(&nothing), nothing.warnings.len()), (vec![], 1));
}

#[test]
fn scalar_bins_hold_values_ranges_and_open_ranges() {
    let mut heap = Heap::new();
    let low = RangeDescriptor::Range {
        low: Bound::Open,
        high: Bound::Operand(Operand::Constant(int(3))),
    };
    let high = RangeDescriptor::Range {
        low: Bound::Operand(Operand::Constant(int(12))),
        high: Bound::Open,
    };
    let key = register(
        &mut heap,
        group(
            "g",
            vec![point(
                "a",
                point_type(4, false),
                vec![
                    bins("low", vec![low]),
                    bins("mid", vec![value(int(5)), range(int(7), int(9))]),
                    bins("high", vec![high]),
                ],
            )],
        ),
    );
    let object = construct(&mut heap, key, &[]);
    for bits in [0, 3, 4, 5, 6, 7, 9, 10, 12, 15, 15] {
        sample(&mut heap, object, &[bits]);
    }
    assert_eq!(
        instance_bins(&heap, 0, 0),
        [
            ("low".to_string(), 2),
            ("mid".to_string(), 3),
            ("high".to_string(), 3)
        ]
    );
    assert_close(inst_coverage(&mut heap, object, 0), 100.0);
    assert_eq!(inst_counts(&mut heap, object, 1), (3, 3));
    assert_eq!(
        heap.coverage_report().covergroups[0].instances[0].samples,
        11
    );
}

#[test]
fn descriptors_travel_as_bytes() {
    let descriptor = resolution_example();
    let key = type_key(&descriptor.name);
    let json = serde_json::to_vec(&descriptor).unwrap();
    let mut heap = Heap::new();
    assert_eq!(call(&mut heap, op::REGISTERED, 0, 0, [key, 0, 0]), 0);
    for chunk in json.chunks(8) {
        let word = chunk
            .iter()
            .fold(0, |word, &byte| word << 8 | u64::from(byte));
        call(
            &mut heap,
            op::REGISTER_BYTES,
            chunk.len() as u64,
            0,
            [key, word, 0],
        );
    }
    assert_eq!(call(&mut heap, op::REGISTER, 0, 0, [key, 0, 0]), 0);
    assert_eq!(call(&mut heap, op::REGISTERED, 0, 0, [key, 0, 0]), 1);
    assert_eq!(heap.take_fault(), None);
    let mut typed = Heap::new();
    register(&mut typed, descriptor);
    assert!(heap.same_contents(&typed));
    // A registered type stays as it is, whatever bytes come for its key.
    call(
        &mut heap,
        op::REGISTER_BYTES,
        2,
        0,
        [key, u64::from(u16::from_be_bytes(*b"{}")), 0],
    );
    call(&mut heap, op::REGISTER, 0, 0, [key, 0, 0]);
    assert_eq!(heap.take_fault(), None);
    assert!(heap.same_contents(&typed));
    // Bytes that are no descriptor are a fault and register nothing.
    let other = type_key("other");
    call(&mut heap, op::REGISTER_BYTES, 1, 0, [other, b'['.into(), 0]);
    call(&mut heap, op::REGISTER, 0, 0, [other, 0, 0]);
    assert!(matches!(heap.take_fault(), Some(Fault::Coverage(_))));
    assert_eq!(call(&mut heap, op::REGISTERED, 0, 0, [other, 0, 0]), 0);
    // So is a descriptor that names an input it does not have.
    let mut broken = group("broken", vec![]);
    broken.options = vec![option_input(0, OptionId::Weight, 0)];
    heap.register_cover_type(type_key("broken"), Arc::new(broken));
    assert_eq!(
        heap.take_fault().map(|fault| fault.to_string()),
        Some(
            "covergroup: descriptor of type `broken`: input 0 is not an integral input".to_string()
        )
    );
    // An operation on a type that is not registered is one too.
    assert_eq!(call(&mut heap, op::NEW, 0, 0, [other, 8, CLASS.into()]), 0);
    assert!(matches!(heap.take_fault(), Some(Fault::Coverage(_))));
    type_coverage(&mut heap, other, 0);
    assert!(matches!(heap.take_fault(), Some(Fault::Coverage(_))));
}

#[test]
fn formals_round_trip_in_both_planes() {
    let mut heap = Heap::new();
    let mut descriptor = group("g", vec![]);
    descriptor.formals = vec![
        ValueType {
            width: 5,
            signed: true,
        },
        ValueType {
            width: 100,
            signed: false,
        },
    ];
    let key = register(&mut heap, descriptor);
    let object = construct(&mut heap, key, &[]);
    let other = construct(&mut heap, key, &[]);
    let words: [(u64, u64); 2] = [(0x0123_4567_89ab_cdef, 0xf0f0), (0xa_bcde_f012, 0x3)];
    call(&mut heap, op::FORMAL_SET, 0, 0, [object.into(), 0x1b, 0x4]);
    for (word, (value, mask)) in words.into_iter().enumerate() {
        call(
            &mut heap,
            op::FORMAL_SET,
            1,
            word as u64,
            [object.into(), value, mask],
        );
    }
    assert_eq!(
        call(&mut heap, op::FORMAL, 0, 0, [object.into(), 0, 0]),
        0x1b
    );
    assert_eq!(
        call(&mut heap, op::FORMAL, 0, 1, [object.into(), 0, 0]),
        0x4
    );
    for (word, (value, mask)) in words.into_iter().enumerate() {
        let plane = |heap: &mut Heap, plane: u64| {
            call(
                heap,
                op::FORMAL,
                1,
                word as u64 * 2 + plane,
                [object.into(), 0, 0],
            )
        };
        assert_eq!(plane(&mut heap, 0), value);
        assert_eq!(plane(&mut heap, 1), mask);
    }
    // Each instance keeps its own.
    assert_eq!(call(&mut heap, op::FORMAL, 1, 0, [other.into(), 0, 0]), 0);
    assert_eq!(heap.take_fault(), None);
    // A word the formal does not have is a fault.
    call(&mut heap, op::FORMAL, 0, 2, [object.into(), 0, 0]);
    assert!(matches!(heap.take_fault(), Some(Fault::Coverage(_))));
    call(&mut heap, op::FORMAL_SET, 2, 0, [object.into(), 0, 0]);
    assert!(matches!(heap.take_fault(), Some(Fault::Coverage(_))));
}

#[test]
fn packs_count_as_the_same_values_one_by_one() {
    let descriptor = || {
        group(
            "g",
            vec![
                point("a", point_type(3, false), vec![]),
                point(
                    "b",
                    point_type(8, true),
                    vec![
                        bins("negative", vec![range(int(-128), int(-1))]),
                        bins("rest", vec![range(int(0), int(127))]),
                    ],
                ),
                point("c", point_type(1, false), vec![]),
            ],
        )
    };
    let mut packed = Heap::new();
    let mut single = Heap::new();
    let key = register(&mut packed, descriptor());
    register(&mut single, descriptor());
    let object = construct(&mut packed, key, &[]);
    assert_eq!(construct(&mut single, key, &[]), object);
    // (a, b, c) and which of them are sampled.
    let samples: [([u64; 3], u64); 5] = [
        ([5, 0xff, 1], 0b111),
        ([7, 0x7f, 0], 0b111),
        ([0, 0x80, 1], 0b101),
        ([3, 0x01, 1], 0b010),
        ([6, 0x10, 0], 0b000),
    ];
    for (values, present) in samples {
        let [a, b, c] = values;
        // Bits above the pack and x or z bits of an item that is not
        // sampled are not read.
        let word = a | b << 3 | c << 11 | 0xdead << 12;
        if present != 0b111 {
            call(&mut packed, op::PRESENT, 1, 3, [object.into(), present, 0]);
        }
        let masks = if present & 0b010 == 0 { 0xff << 3 } else { 0 };
        call(&mut packed, op::POINTS, 1, 3, [object.into(), word, masks]);
        assert_eq!(
            call(&mut packed, op::SAMPLE_END, 0, 0, [object.into(), 0, 0]),
            0
        );
        let values: Vec<Option<u64>> = values
            .iter()
            .enumerate()
            .map(|(index, &value)| (present >> index & 1 != 0).then_some(value))
            .collect();
        sample_some(&mut single, object, &values);
    }
    assert!(packed.same_contents(&single));
    assert_eq!(
        instance_bins(&packed, 0, 1),
        [("negative".to_string(), 1), ("rest".to_string(), 2)]
    );
    assert_eq!(instance_bins(&packed, 0, 2)[1], ("auto[1]".to_string(), 2));
    // Two packs of one sample, the second from item 2 on.
    let mut split = single.clone();
    call(
        &mut packed,
        op::POINTS,
        1,
        3,
        [object.into(), 2 | 9 << 3, 0],
    );
    call(&mut packed, op::SAMPLE_END, 0, 0, [object.into(), 0, 0]);
    call(&mut split, op::POINTS, 1, 1, [object.into(), 2, 0]);
    call(&mut split, op::POINTS, 2, 2, [object.into(), 9, 0]);
    call(&mut split, op::SAMPLE_END, 0, 0, [object.into(), 0, 0]);
    assert!(packed.same_contents(&split));
    assert_eq!(packed.take_fault(), None);
}

#[test]
fn automatic_bins_partition_the_values() {
    let mut heap = Heap::new();
    // "if M is 3 and N is 3, then the eight possible values are
    // distributed as follows: <0,1>, <2,3>, <4,5,6,7>" (19.5.3).
    let mut three = group(
        "three",
        vec![
            point("a", point_type(3, false), vec![]),
            point("b", point_type(8, false), vec![]),
            point("c", point_type(2, false), vec![]),
        ],
    );
    three.options = vec![option(1, OptionId::AutoBinMax, 3)];
    let key = register(&mut heap, three);
    let object = construct(&mut heap, key, &[]);
    for bits in [0, 1, 3, 5, 6, 7] {
        sample(&mut heap, object, &[bits, 4 * bits + 3, 2]);
    }
    assert_eq!(
        instance_bins(&heap, 0, 0),
        [
            ("auto[0:1]".to_string(), 2),
            ("auto[2:3]".to_string(), 1),
            ("auto[4:7]".to_string(), 3)
        ]
    );
    // 256 values in the 64 bins of the default `auto_bin_max`.
    let b = instance_bins(&heap, 0, 1);
    assert_eq!(b.len(), 64);
    assert_eq!(b[0], ("auto[0:3]".to_string(), 1));
    assert_eq!(b[1], ("auto[4:7]".to_string(), 1));
    assert_eq!(b[7], ("auto[28:31]".to_string(), 1));
    assert_eq!(b[63], ("auto[252:255]".to_string(), 0));
    assert_eq!(b.iter().map(|bin| bin.1).sum::<u64>(), 6);
    // Fewer values than `auto_bin_max`: a bin for each.
    assert_eq!(
        instance_bins(&heap, 0, 2),
        [
            ("auto[0]".to_string(), 0),
            ("auto[1]".to_string(), 0),
            ("auto[2]".to_string(), 6),
            ("auto[3]".to_string(), 0)
        ]
    );
    assert_eq!(inst_counts(&mut heap, object, 1), (3, 3));
    assert_eq!(inst_counts(&mut heap, object, 2), (6, 64));
    assert_eq!(inst_counts(&mut heap, object, 3), (1, 4));
    assert_close(
        inst_coverage(&mut heap, object, 0),
        (100.0 + 600.0 / 64.0 + 25.0) / 3.0,
    );
    assert_eq!(inst_counts(&mut heap, object, 0), (10, 71));
}

#[test]
fn automatic_bins_of_one_bin_and_of_too_many() {
    let mut heap = Heap::new();
    let mut descriptor = group(
        "g",
        vec![
            point("a", point_type(64, false), vec![]),
            point("b", point_type(4, false), vec![]),
        ],
    );
    descriptor.inputs = int_inputs(2);
    descriptor.options = vec![
        option_input(0, OptionId::AutoBinMax, 0),
        option_input(2, OptionId::AutoBinMax, 1),
    ];
    let key = register(&mut heap, descriptor);
    // A value below 1 gives one bin, with a warning.
    let (object, warnings) = construct_with_warnings(&mut heap, key, &[1, 0]);
    assert_eq!(warnings, 1);
    assert_eq!(
        message(&mut heap, object),
        "coverpoint `b` of covergroup `g` (instance `g#0`): option.auto_bin_max is 0: one \
         automatic bin is created"
    );
    sample(&mut heap, object, &[u64::MAX, 9]);
    sample(&mut heap, object, &[0, 0]);
    assert_eq!(
        instance_bins(&heap, 0, 0),
        [("auto[0:18446744073709551615]".to_string(), 2)]
    );
    assert_eq!(instance_bins(&heap, 0, 1), [("auto[0:15]".to_string(), 2)]);
    // The coverpoint's own value, not the covergroup's, decides its bins.
    let object = construct(&mut heap, key, &[1, 16]);
    sample(&mut heap, object, &[u64::MAX, 9]);
    assert_eq!(instance_bins(&heap, 1, 1)[9], ("auto[9]".to_string(), 1));
    // Two bins of a 64-bit coverpoint split at the top bit.
    let object = construct(&mut heap, key, &[2, 4]);
    sample(&mut heap, object, &[1 << 63, 0]);
    sample(&mut heap, object, &[(1 << 63) - 1, 0]);
    assert_eq!(
        instance_bins(&heap, 2, 0),
        [
            ("auto[0:9223372036854775807]".to_string(), 1),
            (
                "auto[9223372036854775808:18446744073709551615]".to_string(),
                1
            )
        ]
    );
    // More bins than an item may have are a fault.
    let object = call(&mut heap, op::NEW, 0, 0, [key, 8, CLASS.into()]);
    let limit = MAX_BINS_PER_ITEM as u64 + 1;
    call(&mut heap, op::INPUT, 0, 0, [object, limit, 0]);
    call(&mut heap, op::INPUT, 1, 0, [object, 4, 0]);
    assert_eq!(call(&mut heap, op::BUILD, 0, 0, [object, 0, 0]), 0);
    assert_eq!(
        heap.take_fault().map(|fault| fault.to_string()),
        Some(
            "covergroup: type `g`: coverpoint `a` has more than 1048576 automatic bins".to_string()
        )
    );
    assert_eq!(call(&mut heap, op::SAMPLING, 0, 0, [object, 0, 0]), 0);
}

#[test]
fn an_enumeration_has_a_bin_for_each_enumerator() {
    let mut heap = Heap::new();
    let mut ty = point_type(3, false);
    ty.members = vec![
        ("RED".to_string(), sized(3, 0)),
        ("GREEN".to_string(), sized(3, 1)),
        ("BLUE".to_string(), sized(3, 5)),
    ];
    let mut descriptor = group("g", vec![point("color", ty, vec![])]);
    // `auto_bin_max` does not limit the bins of an enumeration.
    descriptor.options = vec![option(0, OptionId::AutoBinMax, 2)];
    let key = register(&mut heap, descriptor);
    let object = construct(&mut heap, key, &[]);
    for bits in [5, 5, 0, 2, 7] {
        sample(&mut heap, object, &[bits]);
    }
    assert_eq!(
        instance_bins(&heap, 0, 0),
        [
            ("auto[RED]".to_string(), 1),
            ("auto[GREEN]".to_string(), 0),
            ("auto[BLUE]".to_string(), 2)
        ]
    );
    assert_close(inst_coverage(&mut heap, object, 1), 200.0 / 3.0);
    assert_eq!(inst_counts(&mut heap, object, 0), (2, 3));
}

#[test]
fn an_enumerator_with_x_or_z_has_no_bin_to_cover() {
    let mut heap = Heap::new();
    // enum integer {IDLE, XX = 'x, S1 = 2'b01, S2 = 2'b10} of 6.19, in
    // four bits.
    let ty = PointType {
        four_state: true,
        members: vec![
            ("IDLE".to_string(), sized(4, 0)),
            (
                "XX".to_string(),
                Constant {
                    ty: ValueType {
                        width: 4,
                        signed: false,
                    },
                    value: BigUint::from(0b1111u8),
                    mask: BigUint::from(0b1111u8),
                },
            ),
            ("S1".to_string(), sized(4, 1)),
            ("S2".to_string(), sized(4, 2)),
        ],
        ..point_type(4, false)
    };
    let key = register(&mut heap, group("g", vec![point("state", ty, vec![])]));
    let object = construct(&mut heap, key, &[]);
    let sample_planes = |heap: &mut Heap, value: u64, mask: u64| {
        call(heap, op::POINT, 1, 0, [object.into(), value, mask]);
        call(heap, op::SAMPLE_END, 0, 0, [object.into(), 0, 0]);
    };
    // Automatic bins hold no value with x or z (19.5.3), so the bin of `XX`
    // has none and is no bin of the coverpoint (19.11.1): the value of `XX`
    // hits nothing, and three bins are all there is to cover.
    sample_planes(&mut heap, 0b1111, 0b1111);
    assert_eq!(inst_counts(&mut heap, object, 1), (0, 3));
    for bits in [0, 1, 2, 2] {
        sample_planes(&mut heap, bits, 0);
    }
    sample_planes(&mut heap, 0b1111, 0b1111);
    assert_eq!(
        instance_bins(&heap, 0, 0),
        [
            ("auto[IDLE]".to_string(), 1),
            ("auto[S1]".to_string(), 1),
            ("auto[S2]".to_string(), 2)
        ]
    );
    assert_eq!(inst_counts(&mut heap, object, 1), (3, 3));
    assert_close(inst_coverage(&mut heap, object, 0), 100.0);
    assert_eq!(type_counts(&mut heap, key, 1), (3, 3));
    let report = heap.coverage_report();
    assert_eq!(report.covergroups[0].items[0].bins.len(), 3);
    assert_eq!(report.covergroups[0].instances[0].samples, 6);
    assert_eq!(heap.take_fault(), None);
}

/// A covergroup of two coverpoints of two bins each whose weights the
/// inputs of a construction give: of the covergroup, of `a` and of `b`.
fn weighted() -> TypeDescriptor {
    let mut descriptor = group(
        "w",
        vec![
            point("a", point_type(1, false), vec![]),
            point("b", point_type(1, false), vec![]),
        ],
    );
    descriptor.inputs = int_inputs(3);
    descriptor.options = vec![
        option_input(0, OptionId::Weight, 0),
        option_input(1, OptionId::Weight, 1),
        option_input(2, OptionId::Weight, 2),
    ];
    descriptor
}

#[test]
fn a_covergroup_weight_is_not_the_default_of_its_items() {
    let mut heap = Heap::new();
    let key = register(&mut heap, weighted());
    // `option.weight = 0` of the covergroup: the items keep weight 1.
    let dropped = construct(&mut heap, key, &[0, 1, 1]);
    assert_eq!(get_option(&mut heap, dropped, 0, OptionId::Weight), 0);
    assert_eq!(get_option(&mut heap, dropped, 1, OptionId::Weight), 1);
    sample(&mut heap, dropped, &[0, 0]);
    sample(&mut heap, dropped, &[1, 0]);
    assert_close(inst_coverage(&mut heap, dropped, 0), 75.0);
    // The only instance takes no part in the type average.
    assert_close(type_coverage(&mut heap, key, 0), 0.0);
    assert_eq!(type_counts(&mut heap, key, 0), (0, 0));
    assert_close(overall(&mut heap), 100.0);
    let counted = construct(&mut heap, key, &[1, 1, 1]);
    sample(&mut heap, counted, &[0, 0]);
    assert_close(inst_coverage(&mut heap, counted, 0), 50.0);
    assert_close(type_coverage(&mut heap, key, 0), 50.0);
    // The bins of both instances are counted all the same.
    assert_eq!(type_counts(&mut heap, key, 0), (5, 8));
    assert_close(overall(&mut heap), 50.0);
    // The coverpoints average over both instances by their own weights.
    assert_close(type_coverage(&mut heap, key, 1), 75.0);
    assert_close(type_coverage(&mut heap, key, 2), 50.0);
}

#[test]
fn zero_denominators_at_every_level() {
    let mut heap = Heap::new();
    let empty = || vec![range(int(1), int(0))];
    let mut descriptor = group(
        "z",
        vec![
            point("none", point_type(2, false), vec![bins("b", empty())]),
            point("some", point_type(1, false), vec![]),
        ],
    );
    descriptor.inputs = int_inputs(3);
    descriptor.options = vec![
        option_input(0, OptionId::Weight, 0),
        option_input(1, OptionId::Weight, 1),
        option_input(2, OptionId::Weight, 2),
    ];
    let key = register(&mut heap, descriptor);
    // 19.11 b) to d) for a type without an instance.
    assert_close(type_coverage(&mut heap, key, 0), 0.0);
    assert_close(type_coverage(&mut heap, key, 2), 0.0);
    assert_eq!(type_counts(&mut heap, key, 0), (0, 0));
    assert_close(overall(&mut heap), 100.0);
    set_type_option(&mut heap, key, 0, TypeOptionId::Weight, 0);
    set_type_option(&mut heap, key, 2, TypeOptionId::Weight, 0);
    assert_close(type_coverage(&mut heap, key, 0), 100.0);
    assert_close(type_coverage(&mut heap, key, 2), 100.0);
    set_type_option(&mut heap, key, 0, TypeOptionId::Weight, 1);
    set_type_option(&mut heap, key, 2, TypeOptionId::Weight, 1);

    // A coverpoint whose only bin has no value (19.11.1 a to d): it reads
    // 0, or 100 with weight 0, has no bins to count, and takes no part in
    // the covergroup.
    let object = construct(&mut heap, key, &[1, 1, 1]);
    sample(&mut heap, object, &[1, 1]);
    assert_close(inst_coverage(&mut heap, object, 1), 0.0);
    assert_eq!(inst_counts(&mut heap, object, 1), (0, 0));
    assert_close(inst_coverage(&mut heap, object, 0), 50.0);
    assert_eq!(inst_counts(&mut heap, object, 0), (1, 2));
    assert!(instance_bins(&heap, 0, 0).is_empty());
    set_option(&mut heap, object, 1, OptionId::Weight, 0);
    assert_close(inst_coverage(&mut heap, object, 1), 100.0);
    assert_close(inst_coverage(&mut heap, object, 0), 50.0);
    assert_close(type_coverage(&mut heap, key, 1), 0.0);
    set_type_option(&mut heap, key, 1, TypeOptionId::Weight, 0);
    assert_close(type_coverage(&mut heap, key, 1), 100.0);
    assert_eq!(type_counts(&mut heap, key, 1), (0, 0));

    // A coverpoint of weight 0 with bins reads its coverage but takes no
    // part: here that leaves the covergroup without a denominator (19.11 a
    // to d).
    set_option(&mut heap, object, 2, OptionId::Weight, 0);
    assert_close(inst_coverage(&mut heap, object, 2), 50.0);
    assert_eq!(inst_counts(&mut heap, object, 2), (1, 2));
    assert_close(inst_coverage(&mut heap, object, 0), 0.0);
    assert_eq!(inst_counts(&mut heap, object, 0), (0, 0));
    set_option(&mut heap, object, 0, OptionId::Weight, 0);
    assert_close(inst_coverage(&mut heap, object, 0), 100.0);
    // Its type is then without one too, and `$get_coverage` with it.
    assert_close(type_coverage(&mut heap, key, 0), 0.0);
    assert_eq!(type_counts(&mut heap, key, 0), (0, 0));
    assert_close(overall(&mut heap), 100.0);
    set_type_option(&mut heap, key, 0, TypeOptionId::Weight, 0);
    assert_close(type_coverage(&mut heap, key, 0), 100.0);
    assert_eq!(heap.take_fault(), None);

    // A covergroup without items.
    let key = register(&mut heap, group("items", vec![]));
    let object = construct(&mut heap, key, &[]);
    sample(&mut heap, object, &[]);
    assert_close(inst_coverage(&mut heap, object, 0), 0.0);
    assert_eq!(inst_counts(&mut heap, object, 0), (0, 0));
    assert_close(type_coverage(&mut heap, key, 0), 0.0);
    assert_close(overall(&mut heap), 100.0);
    set_option(&mut heap, object, 0, OptionId::Weight, 0);
    assert_close(inst_coverage(&mut heap, object, 0), 100.0);
    // An item the type does not have is a fault.
    inst_coverage(&mut heap, object, 1);
    assert_eq!(
        heap.take_fault(),
        Some(Fault::Coverage("type `items`: no item 1".into()))
    );
}

#[test]
fn a_bin_is_covered_with_at_least_as_many_hits() {
    let mut heap = Heap::new();
    let mut descriptor = group(
        "g",
        vec![
            point("a", point_type(1, false), vec![]),
            point("b", point_type(1, false), vec![]),
        ],
    );
    descriptor.options = vec![
        option(0, OptionId::AtLeast, 2),
        option(2, OptionId::AtLeast, 1),
    ];
    let key = register(&mut heap, descriptor);
    let object = construct(&mut heap, key, &[]);
    // The covergroup's value is the default of its coverpoints.
    assert_eq!(get_option(&mut heap, object, 0, OptionId::AtLeast), 2);
    assert_eq!(get_option(&mut heap, object, 1, OptionId::AtLeast), 2);
    assert_eq!(get_option(&mut heap, object, 2, OptionId::AtLeast), 1);
    sample(&mut heap, object, &[0, 0]);
    assert_eq!(inst_counts(&mut heap, object, 1), (0, 2));
    assert_eq!(inst_counts(&mut heap, object, 2), (1, 2));
    sample(&mut heap, object, &[0, 1]);
    assert_eq!(inst_counts(&mut heap, object, 1), (1, 2));
    assert_eq!(inst_counts(&mut heap, object, 2), (2, 2));
    assert_close(inst_coverage(&mut heap, object, 0), 75.0);
    // A coverpoint that was not assigned follows the covergroup.
    set_option(&mut heap, object, 0, OptionId::AtLeast, 3);
    assert_eq!(get_option(&mut heap, object, 1, OptionId::AtLeast), 3);
    assert_eq!(inst_counts(&mut heap, object, 1), (0, 2));
    assert_eq!(inst_counts(&mut heap, object, 2), (2, 2));
    set_option(&mut heap, object, 1, OptionId::AtLeast, 1);
    assert_eq!(inst_counts(&mut heap, object, 1), (1, 2));
    // The type takes the most that any instance needs when it merges
    // (19.11.1).
    let other = construct(&mut heap, key, &[]);
    sample(&mut heap, other, &[1, 1]);
    set_type_option(&mut heap, key, 0, TypeOptionId::MergeInstances, 1);
    assert_eq!(get_option(&mut heap, other, 1, OptionId::AtLeast), 2);
    // `a`: 0 was hit twice and 1 once, where 2 hits are needed.
    assert_eq!(type_counts(&mut heap, key, 1), (1, 2));
    assert_eq!(type_counts(&mut heap, key, 2), (2, 2));
    let report = heap.coverage_report();
    let a = &report.covergroups[0].items[0];
    assert_eq!(a.at_least, 2);
    assert_eq!(
        a.bins
            .iter()
            .map(|bin| (bin.hits, bin.covered))
            .collect::<Vec<_>>(),
        [(2, true), (1, false)]
    );
    assert_eq!(heap.take_fault(), None);
}

#[test]
fn weights_average_as_the_formulas_of_19_7_1() {
    let mut heap = Heap::new();
    // `a` has `option.weight = 2` and `type_option.weight = 3`, `b` has
    // `option.weight = w` and `type_option.weight = 5`.
    let mut descriptor = group(
        "g1",
        vec![
            point("a", point_type(2, false), vec![]),
            point("b", point_type(1, false), vec![]),
        ],
    );
    descriptor.inputs = int_inputs(1);
    descriptor.options = vec![
        option(1, OptionId::Weight, 2),
        option_input(2, OptionId::Weight, 0),
    ];
    descriptor.points[0].type_options.weight = 3;
    descriptor.points[1].type_options.weight = 5;
    let key = register(&mut heap, descriptor);
    let first = construct(&mut heap, key, &[6]);
    let second = construct(&mut heap, key, &[1]);
    sample(&mut heap, first, &[0, 0]);
    sample(&mut heap, second, &[1, 1]);
    sample(&mut heap, second, &[2, 1]);
    // (((instance coverage of a) × 2) + ((instance coverage of b) × w)) /
    // (2 + w)
    assert_close(
        inst_coverage(&mut heap, first, 0),
        (25.0 * 2.0 + 50.0 * 6.0) / 8.0,
    );
    assert_close(
        inst_coverage(&mut heap, second, 0),
        (50.0 * 2.0 + 50.0) / 3.0,
    );
    // Without merging, the type is the average of its instances by their
    // `option.weight`, and an item that of the item by its weight in each
    // (19.11.3).
    assert_close(type_coverage(&mut heap, key, 0), (43.75 + 50.0) / 2.0);
    assert_close(
        type_coverage(&mut heap, key, 1),
        (25.0 * 2.0 + 50.0 * 2.0) / 4.0,
    );
    assert_close(type_coverage(&mut heap, key, 2), (50.0 * 6.0 + 50.0) / 7.0);
    assert_eq!(type_counts(&mut heap, key, 0), (5, 12));
    set_option(&mut heap, second, 0, OptionId::Weight, 3);
    assert_close(type_coverage(&mut heap, key, 0), (43.75 + 50.0 * 3.0) / 4.0);
    // ((merge of coverage of a from all instances) × 3 + (merge of coverage
    // of b from all instances) × 5) / (3 + 5)
    set_type_option(&mut heap, key, 0, TypeOptionId::MergeInstances, 1);
    assert_close(type_coverage(&mut heap, key, 1), 75.0);
    assert_close(type_coverage(&mut heap, key, 2), 100.0);
    assert_close(
        type_coverage(&mut heap, key, 0),
        (75.0 * 3.0 + 100.0 * 5.0) / 8.0,
    );
    assert_eq!(type_counts(&mut heap, key, 0), (5, 6));
    // `$get_coverage` weighs the types by `type_option.weight`.
    let other = register(
        &mut heap,
        group("g2", vec![point("c", point_type(1, false), vec![])]),
    );
    let object = construct(&mut heap, other, &[]);
    sample(&mut heap, object, &[1]);
    assert_close(overall(&mut heap), (90.625 + 50.0) / 2.0);
    set_type_option(&mut heap, other, 0, TypeOptionId::Weight, 3);
    assert_close(overall(&mut heap), (90.625 + 50.0 * 3.0) / 4.0);
    assert_eq!(heap.take_fault(), None);
}

/// The covergroup `gt (int l, h)` of 19.11.3 with `a` from 0 to 2: the bin
/// of each value is a scalar bin of a range that the construction leaves
/// empty where `b[] = {[l:h]}` has no such bin.
fn merge_example() -> TypeDescriptor {
    let mut descriptor = group(
        "gt",
        vec![point(
            "a",
            point_type(2, false),
            vec![
                bins("b[0]", vec![input_range(0, 1)]),
                bins("b[1]", vec![input_range(2, 3)]),
                bins("b[2]", vec![input_range(4, 5)]),
            ],
        )],
    );
    descriptor.inputs = int_inputs(7);
    descriptor.options = vec![option_input(0, OptionId::GetInstCoverage, 6)];
    descriptor.type_options.merge_instances = true;
    descriptor
}

#[test]
fn merged_instances_unite_their_bins_by_name() {
    let mut heap = Heap::new();
    let key = register(&mut heap, merge_example());
    for get_inst_coverage in [0, 1] {
        // `gt gv1 = new (0,1); gt gv2 = new (1,2);`
        let gv1 = construct(&mut heap, key, &[0, 0, 1, 1, 1, 0, get_inst_coverage]);
        let gv2 = construct(&mut heap, key, &[1, 0, 1, 1, 2, 2, get_inst_coverage]);
        // "If a==0 were sampled by gv1 and a==1 sampled by both gv1 and
        // gv2".
        sample(&mut heap, gv1, &[0]);
        sample(&mut heap, gv1, &[1]);
        sample(&mut heap, gv2, &[1]);
        if get_inst_coverage == 0 {
            // "the gt::get_coverage() value would be 66.6667 because 2 out
            // of 3 type bins were covered; the cumulative count for the
            // bin b[1] would be 2".
            assert_close(type_coverage(&mut heap, key, 0), 200.0 / 3.0);
            assert_eq!(type_counts(&mut heap, key, 0), (2, 3));
            let report = heap.coverage_report();
            assert_eq!(
                report.covergroups[0].items[0]
                    .bins
                    .iter()
                    .map(|bin| (bin.name.as_str(), bin.hits, bin.covered))
                    .collect::<Vec<_>>(),
                [("b[0]", 1, true), ("b[1]", 2, true), ("b[2]", 0, false)]
            );
            // Without `option.get_inst_coverage`, "the value returned by
            // get_inst_coverage shall equal the value returned by
            // get_coverage" (Table 19-1).
            assert_close(inst_coverage(&mut heap, gv1, 0), 200.0 / 3.0);
            assert_close(inst_coverage(&mut heap, gv2, 1), 200.0 / 3.0);
            assert_eq!(inst_counts(&mut heap, gv2, 0), (2, 3));
        } else {
            // "gv1.get_inst_coverage() would return 100.0 and
            // gv2.get_inst_coverage() would return 50.0."
            assert_close(inst_coverage(&mut heap, gv1, 0), 100.0);
            assert_close(inst_coverage(&mut heap, gv2, 0), 50.0);
            assert_eq!(inst_counts(&mut heap, gv2, 1), (1, 2));
            // The type now has the hits of all four instances.
            assert_close(type_coverage(&mut heap, key, 0), 200.0 / 3.0);
            assert_eq!(
                heap.coverage_report().covergroups[0].items[0].bins[1].hits,
                4
            );
        }
    }
    // Without merging the type is the average of its instances.
    set_type_option(&mut heap, key, 0, TypeOptionId::MergeInstances, 0);
    assert_close(type_coverage(&mut heap, key, 0), 75.0);
    assert_eq!(type_counts(&mut heap, key, 0), (6, 8));
    assert_eq!(heap.take_fault(), None);
}

#[test]
fn overall_coverage_is_100_where_nothing_takes_part() {
    let mut heap = Heap::new();
    // "$get_coverage shall return a value of 100.0 if called on a design
    // that has no covergroup instances, or if called on a design in which
    // all covergroups have a weight of 0" (19.11).
    assert_close(overall(&mut heap), 100.0);
    assert_close(heap.overall_coverage(), 100.0);
    let single = |name: &str| group(name, vec![point("a", point_type(1, false), vec![])]);
    let first = register(&mut heap, single("first"));
    let second = register(&mut heap, single("second"));
    assert_close(overall(&mut heap), 100.0);
    assert!(heap.coverage_report().covergroups.is_empty());
    let object = construct(&mut heap, first, &[]);
    assert_close(overall(&mut heap), 0.0);
    sample(&mut heap, object, &[1]);
    assert_close(overall(&mut heap), 50.0);
    let object = construct(&mut heap, second, &[]);
    sample(&mut heap, object, &[0]);
    sample(&mut heap, object, &[1]);
    assert_close(overall(&mut heap), 75.0);
    set_type_option(&mut heap, first, 0, TypeOptionId::Weight, 0);
    assert_close(overall(&mut heap), 100.0);
    set_type_option(&mut heap, second, 0, TypeOptionId::Weight, 0);
    assert_close(overall(&mut heap), 100.0);
    assert_close(heap.coverage_report().coverage, 100.0);
    assert_eq!(heap.take_fault(), None);
}

#[test]
fn options_are_set_and_read() {
    let mut heap = Heap::new();
    let mut descriptor = group("g", vec![point("a", point_type(4, false), vec![])]);
    descriptor.inputs = vec![InputDescriptor::String];
    descriptor.options = vec![
        OptionInit {
            item: 0,
            option: OptionId::Comment,
            value: OptionValue::StringInput(0),
        },
        OptionInit {
            item: 1,
            option: OptionId::Comment,
            value: OptionValue::String("of a".into()),
        },
        option(0, OptionId::PerInstance, 1),
        option(0, OptionId::Goal, 90),
    ];
    descriptor.type_options.comment = "the type".into();
    descriptor.points[0].type_options.goal = 80;
    let key = register(&mut heap, descriptor);
    let object = call(&mut heap, op::NEW, 0, 0, [key, 8, CLASS.into()]) as Handle;
    let comment = heap.intern(b"per instance");
    call(
        &mut heap,
        op::INPUT_STRING,
        0,
        0,
        [object.into(), comment.into(), 0],
    );
    assert_eq!(call(&mut heap, op::BUILD, 0, 0, [object.into(), 0, 0]), 0);
    let text = |heap: &mut Heap, handle: u64| heap.string(handle as Handle).to_vec();
    // The definition's assignments and the defaults of Table 19-1.
    let name = get_option(&mut heap, object, 0, OptionId::Name);
    assert_eq!(text(&mut heap, name), b"g#0");
    let read = get_option(&mut heap, object, 0, OptionId::Comment);
    assert_eq!(read, u64::from(comment));
    let read = get_option(&mut heap, object, 1, OptionId::Comment);
    assert_eq!(text(&mut heap, read), b"of a");
    for (item, option, expected) in [
        (0, OptionId::Weight, 1),
        (0, OptionId::Goal, 90),
        (0, OptionId::AtLeast, 1),
        (0, OptionId::AutoBinMax, 64),
        (0, OptionId::DetectOverlap, 0),
        (0, OptionId::PerInstance, 1),
        (0, OptionId::GetInstCoverage, 0),
        (1, OptionId::Weight, 1),
        (1, OptionId::Goal, 100),
        (1, OptionId::AtLeast, 1),
        (1, OptionId::AutoBinMax, 64),
        (1, OptionId::DetectOverlap, 0),
    ] {
        assert_eq!(get_option(&mut heap, object, item, option), expected);
    }
    // Every member reads back what it was assigned.
    let renamed = heap.intern(b"mine");
    set_option(&mut heap, object, 0, OptionId::Name, renamed.into());
    assert_eq!(
        get_option(&mut heap, object, 0, OptionId::Name),
        u64::from(renamed)
    );
    for (item, option, value) in [
        (0, OptionId::Weight, 7),
        (0, OptionId::Goal, -5i64 as u64),
        (0, OptionId::AtLeast, 4),
        (0, OptionId::AutoBinMax, 16),
        (0, OptionId::DetectOverlap, 1),
        (0, OptionId::PerInstance, 0),
        (0, OptionId::GetInstCoverage, 1),
        (1, OptionId::Weight, 0),
        (1, OptionId::Goal, 50),
        (1, OptionId::AtLeast, 9),
        (1, OptionId::AutoBinMax, 8),
        (1, OptionId::DetectOverlap, 0),
    ] {
        set_option(&mut heap, object, item, option, value);
        assert_eq!(get_option(&mut heap, object, item, option), value);
    }
    // An `int` is its low 32 bits, a bit its lowest.
    set_option(&mut heap, object, 0, OptionId::Goal, 0x1_0000_0003);
    assert_eq!(get_option(&mut heap, object, 0, OptionId::Goal), 3);
    set_option(&mut heap, object, 0, OptionId::PerInstance, 2);
    assert_eq!(get_option(&mut heap, object, 0, OptionId::PerInstance), 0);
    assert_eq!(heap.take_fault(), None);
    // The report shows them.
    let report = heap.coverage_report();
    let instance = &report.covergroups[0].instances[0];
    assert_eq!(
        (instance.name.as_str(), instance.comment.as_str()),
        ("mine", "per instance")
    );
    assert_eq!(
        (instance.weight, instance.goal, instance.at_least),
        (7, 3, 4)
    );
    assert_eq!(instance.items[0].comment, "of a");
    assert_eq!(
        (
            instance.items[0].weight,
            instance.items[0].goal,
            instance.items[0].at_least
        ),
        (0, 50, 9)
    );
    // A weight is not negative (Table 19-1), and a coverpoint has no name.
    set_option(&mut heap, object, 0, OptionId::Weight, -1i64 as u64);
    assert_eq!(
        heap.take_fault(),
        Some(Fault::Coverage("type `g`: the negative weight -1".into()))
    );
    assert_eq!(get_option(&mut heap, object, 0, OptionId::Weight), 7);
    set_option(&mut heap, object, 1, OptionId::Name, renamed.into());
    assert!(matches!(heap.take_fault(), Some(Fault::Coverage(_))));
    get_option(&mut heap, object, 1, OptionId::PerInstance);
    assert!(matches!(heap.take_fault(), Some(Fault::Coverage(_))));
    get_option(&mut heap, object, 2, OptionId::Weight);
    assert!(matches!(heap.take_fault(), Some(Fault::Coverage(_))));

    // Type options (Table 19-3), before and after an assignment.
    let read = get_type_option(&mut heap, key, 0, TypeOptionId::Comment);
    assert_eq!(text(&mut heap, read), b"the type");
    for (item, option, expected, value) in [
        (0, TypeOptionId::Weight, 1, 4),
        (0, TypeOptionId::Goal, 100, 60),
        (0, TypeOptionId::MergeInstances, 0, 1),
        (0, TypeOptionId::DistributeFirst, 0, 1),
        (1, TypeOptionId::Weight, 1, 2),
        (1, TypeOptionId::Goal, 80, 70),
    ] {
        assert_eq!(get_type_option(&mut heap, key, item, option), expected);
        set_type_option(&mut heap, key, item, option, value);
        assert_eq!(get_type_option(&mut heap, key, item, option), value);
    }
    let comment = heap.intern(b"of every a");
    set_type_option(&mut heap, key, 1, TypeOptionId::Comment, comment.into());
    assert_eq!(
        get_type_option(&mut heap, key, 1, TypeOptionId::Comment),
        u64::from(comment)
    );
    assert_eq!(heap.take_fault(), None);
    let report = heap.coverage_report();
    let covergroup = &report.covergroups[0];
    assert_eq!(covergroup.comment, "the type");
    assert_eq!(
        (
            covergroup.weight,
            covergroup.goal,
            covergroup.merge_instances
        ),
        (4, 60, true)
    );
    assert_eq!(covergroup.items[0].comment, "of every a");
    assert_eq!(
        (covergroup.items[0].weight, covergroup.items[0].goal),
        (2, 70)
    );
    set_type_option(&mut heap, key, 1, TypeOptionId::Weight, -3i64 as u64);
    assert!(matches!(heap.take_fault(), Some(Fault::Coverage(_))));
    set_type_option(&mut heap, key, 1, TypeOptionId::MergeInstances, 1);
    assert!(matches!(heap.take_fault(), Some(Fault::Coverage(_))));
    get_type_option(&mut heap, key, 1, TypeOptionId::DistributeFirst);
    assert!(matches!(heap.take_fault(), Some(Fault::Coverage(_))));
}

#[test]
fn a_negative_weight_in_the_definition_fails_the_build() {
    let mut heap = Heap::new();
    let key = register(&mut heap, weighted());
    let object = call(&mut heap, op::NEW, 0, 0, [key, 8, CLASS.into()]);
    call(&mut heap, op::INPUT, 0, 0, [object, 1, 0]);
    call(
        &mut heap,
        op::INPUT,
        1,
        0,
        [object, u64::from(-2i32 as u32), 0],
    );
    call(&mut heap, op::BUILD, 0, 0, [object, 0, 0]);
    assert_eq!(
        heap.take_fault(),
        Some(Fault::Coverage("type `w`: the negative weight -2".into()))
    );
    // An input after the build, and a second build, are faults too.
    let object = construct(&mut heap, key, &[1, 1, 1]);
    call(&mut heap, op::INPUT, 0, 0, [object.into(), 1, 0]);
    assert!(matches!(heap.take_fault(), Some(Fault::Coverage(_))));
    call(&mut heap, op::BUILD, 0, 0, [object.into(), 0, 0]);
    assert!(matches!(heap.take_fault(), Some(Fault::Coverage(_))));
}

#[test]
fn stopped_items_are_not_sampled() {
    let mut heap = Heap::new();
    let key = register(
        &mut heap,
        group(
            "g",
            vec![
                point("a", point_type(1, false), vec![]),
                point("b", point_type(1, false), vec![]),
            ],
        ),
    );
    let object = construct(&mut heap, key, &[]);
    let collect = |heap: &mut Heap, item: u64, start: u64| {
        call(heap, op::COLLECT, item, start, [object.into(), 0, 0]);
    };
    let sampling = |heap: &mut Heap| call(heap, op::SAMPLING, 0, 0, [object.into(), 0, 0]);
    let hits = |heap: &Heap| {
        let report = heap.coverage_report();
        let instance = &report.covergroups[0].instances[0];
        (
            instance.samples,
            instance.items[0].bins[1].hits,
            instance.items[1].bins[1].hits,
        )
    };
    sample(&mut heap, object, &[1, 1]);
    assert_eq!(hits(&heap), (1, 1, 1));
    // `stop()` of a coverpoint: the covergroup still samples the other.
    collect(&mut heap, 2, 0);
    assert_eq!(sampling(&mut heap), 1);
    sample(&mut heap, object, &[1, 1]);
    assert_eq!(hits(&heap), (2, 2, 1));
    // `stop()` of the covergroup: nothing is sampled, and a sample that is
    // committed all the same counts nothing.
    collect(&mut heap, 0, 0);
    assert_eq!(sampling(&mut heap), 0);
    assert!(!heap.coverage_report().covergroups[0].instances[0].collecting);
    call(&mut heap, op::POINT, 1, 0, [object.into(), 1, 0]);
    call(&mut heap, op::SAMPLE_END, 0, 0, [object.into(), 0, 0]);
    assert_eq!(hits(&heap), (2, 2, 1));
    // The two are independent: `start()` of the covergroup leaves the
    // coverpoint stopped.
    collect(&mut heap, 0, 1);
    assert_eq!(sampling(&mut heap), 1);
    sample(&mut heap, object, &[1, 1]);
    assert_eq!(hits(&heap), (3, 3, 1));
    collect(&mut heap, 2, 1);
    sample(&mut heap, object, &[1, 1]);
    assert_eq!(hits(&heap), (4, 4, 2));
    assert_eq!(heap.take_fault(), None);
    // An item the type does not have is a fault.
    collect(&mut heap, 3, 0);
    assert_eq!(
        heap.take_fault(),
        Some(Fault::Coverage("type `g`: no item 3".into()))
    );
}

#[test]
fn a_sample_of_what_the_type_does_not_have_is_a_fault() {
    let mut heap = Heap::new();
    let key = register(
        &mut heap,
        group(
            "g",
            vec![
                point("a", point_type(40, false), vec![]),
                point("b", point_type(40, false), vec![]),
            ],
        ),
    );
    let object = construct(&mut heap, key, &[]);
    let samples = |heap: &Heap| heap.coverage_report().covergroups[0].instances[0].samples;
    let fault = |reason: &str| Some(Fault::Coverage(format!("type `g`: {reason}")));
    // The calls that note a value look nothing up: the sample fails.
    for item in [0, 3, 99] {
        call(&mut heap, op::POINT, item, 0, [object.into(), 1, 0]);
        assert_eq!(heap.take_fault(), None);
        call(&mut heap, op::SAMPLE_END, 0, 0, [object.into(), 0, 0]);
        assert_eq!(
            heap.take_fault(),
            fault(&format!("item {item} is not a coverpoint"))
        );
    }
    // A pack from the covergroup on, and one that runs past the last
    // coverpoint, also where that item is not sampled.
    for (first, count, missing) in [(0, 1, 0), (2, 2, 3)] {
        call(&mut heap, op::PRESENT, first, count, [object.into(), 1, 0]);
        call(&mut heap, op::POINTS, first, count, [object.into(), 0, 0]);
        call(&mut heap, op::SAMPLE_END, 0, 0, [object.into(), 0, 0]);
        assert_eq!(
            heap.take_fault(),
            fault(&format!("item {missing} is not a coverpoint"))
        );
    }
    // Two coverpoints of 40 bits are not the values of one word.
    call(&mut heap, op::POINTS, 1, 2, [object.into(), 0, 0]);
    call(&mut heap, op::SAMPLE_END, 0, 0, [object.into(), 0, 0]);
    assert_eq!(
        heap.take_fault(),
        fault("the values of the 2 items from item 1 on do not fit a word")
    );
    // A value of 40 bits has one word.
    call(&mut heap, op::POINT, 2, 0, [object.into(), 1, 0]);
    call(&mut heap, op::POINT, 2, 1, [object.into(), 1, 0]);
    call(&mut heap, op::SAMPLE_END, 0, 0, [object.into(), 0, 0]);
    assert_eq!(
        heap.take_fault(),
        fault("the value of item 2 has no word 1")
    );
    // None of these was a sample, and the next one starts afresh.
    assert_eq!(samples(&heap), 0);
    sample(&mut heap, object, &[1, 1]);
    call(&mut heap, op::POINTS, 2, 1, [object.into(), 1, 0]);
    call(&mut heap, op::SAMPLE_END, 0, 0, [object.into(), 0, 0]);
    assert_eq!(heap.take_fault(), None);
    assert_eq!(samples(&heap), 2);
    assert_eq!(inst_counts(&mut heap, object, 2), (1, 64));
    // A covergroup that is not collecting counts nothing and checks
    // nothing.
    call(&mut heap, op::COLLECT, 0, 0, [object.into(), 0, 0]);
    call(&mut heap, op::POINT, 99, 0, [object.into(), 1, 0]);
    call(&mut heap, op::SAMPLE_END, 0, 0, [object.into(), 0, 0]);
    assert_eq!(heap.take_fault(), None);
    assert_eq!(samples(&heap), 2);
}

#[test]
fn sampling_null_is_no_fault() {
    let mut heap = Heap::new();
    let key = register(&mut heap, group("g", vec![]));
    // `sample()` of a covergroup that was not created samples nothing
    // (19.4).
    assert_eq!(call(&mut heap, op::SAMPLING, 0, 0, [NULL.into(), 0, 0]), 0);
    let object = heap.new_object(CLASS, 8);
    assert_eq!(
        call(&mut heap, op::SAMPLING, 0, 0, [object.into(), 0, 0]),
        0
    );
    assert_eq!(heap.take_fault(), None);
    // An instance is sampled once it is built.
    let unbuilt = call(&mut heap, op::NEW, 0, 0, [key, 8, CLASS.into()]);
    assert_eq!(call(&mut heap, op::SAMPLING, 0, 0, [unbuilt, 0, 0]), 0);
    call(&mut heap, op::BUILD, 0, 0, [unbuilt, 0, 0]);
    assert_eq!(call(&mut heap, op::SAMPLING, 0, 0, [unbuilt, 0, 0]), 1);
    assert_eq!(heap.take_fault(), None);
    // Every other operation through `null` is a null dereference.
    let weight = u64::from(OptionId::Weight.code());
    for (sub, x, y) in [
        (op::INPUT, 0, 0),
        (op::INPUT_STRING, 0, 0),
        (op::BUILD, 0, 0),
        (op::SAMPLE_END, 0, 0),
        (op::INST_COVERAGE, 0, 0),
        (op::COLLECT, 0, 0),
        (op::SET_OPTION, 0, weight),
        (op::GET_OPTION, 0, weight),
        (op::FORMAL_SET, 0, 0),
        (op::FORMAL, 0, 0),
    ] {
        assert_eq!(call(&mut heap, sub, x, y, [NULL.into(), 0, 0]), 0);
        assert_eq!(heap.take_fault(), Some(Fault::NullDereference), "{sub}");
    }
}

#[test]
fn wide_and_signed_coverpoints() {
    let mut heap = Heap::new();
    let top = BigUint::from(1u8) << 99u32;
    let wide = |value: &BigUint| Constant {
        ty: ValueType {
            width: 100,
            signed: false,
        },
        value: value.clone(),
        mask: BigUint::ZERO,
    };
    let key = register(
        &mut heap,
        group(
            "g",
            vec![
                point(
                    "bins",
                    point_type(100, false),
                    vec![
                        bins(
                            "top",
                            vec![RangeDescriptor::Range {
                                low: Bound::Operand(Operand::Constant(wide(&top))),
                                high: Bound::Open,
                            }],
                        ),
                        bins("one", vec![value(int(1)), value(wide(&(&top - 1u8)))]),
                    ],
                ),
                point("auto", point_type(100, false), vec![]),
                point("signed", point_type(8, true), vec![]),
                point(
                    "wide_signed",
                    point_type(65, true),
                    vec![bins("negative", vec![range(int(-5), int(-1))])],
                ),
            ],
        ),
    );
    let object = construct(&mut heap, key, &[]);
    let words = |value: &BigUint| {
        let mut words = value.to_u64_digits();
        words.resize(2, 0);
        words
    };
    let ones65 = (BigUint::from(1u8) << 65u32) - 1u8;
    let sample_wide = |heap: &mut Heap, bits: &BigUint, signed: u64, wide_signed: &BigUint| {
        for item in [1, 2] {
            for (word, value) in words(bits).into_iter().enumerate() {
                call(
                    heap,
                    op::POINT,
                    item,
                    word as u64,
                    [object.into(), value, 0],
                );
            }
        }
        call(heap, op::POINT, 3, 0, [object.into(), signed, 0]);
        for (word, value) in words(wide_signed).into_iter().enumerate() {
            call(heap, op::POINT, 4, word as u64, [object.into(), value, 0]);
        }
        assert_eq!(call(heap, op::SAMPLE_END, 0, 0, [object.into(), 0, 0]), 0);
    };
    // -1 of the 65-bit coverpoint has every bit set.
    sample_wide(&mut heap, &top, 0xff, &ones65);
    sample_wide(&mut heap, &BigUint::from(1u8), 0x80, &(&ones65 - 4u8));
    sample_wide(&mut heap, &(&top - 1u8), 0x7f, &(&ones65 - 5u8));
    sample_wide(&mut heap, &BigUint::from(2u8), 0, &BigUint::ZERO);
    assert_eq!(
        instance_bins(&heap, 0, 0),
        [("top".to_string(), 1), ("one".to_string(), 2)]
    );
    // 2^100 values in 64 bins of 2^94.
    let auto = instance_bins(&heap, 0, 1);
    assert_eq!(auto.len(), 64);
    let per = BigUint::from(1u8) << 94u32;
    assert_eq!(auto[0], (format!("auto[0:{}]", &per - 1u8), 2));
    assert_eq!(auto[31].1, 1);
    assert_eq!(
        auto[32],
        (format!("auto[{}:{}]", top, &top + &per - 1u8), 1)
    );
    // The bins of a signed coverpoint start at its lowest value.
    let signed = instance_bins(&heap, 0, 2);
    assert_eq!(signed[0], ("auto[-128:-125]".to_string(), 1));
    assert_eq!(signed[31], ("auto[-4:-1]".to_string(), 1));
    assert_eq!(signed[32], ("auto[0:3]".to_string(), 1));
    assert_eq!(signed[63], ("auto[124:127]".to_string(), 1));
    assert_eq!(instance_bins(&heap, 0, 3), [("negative".to_string(), 2)]);
    assert_eq!(heap.take_fault(), None);
    // The numbers of a query are those of an `int` at most.
    assert_eq!(saturate(u64::MAX), i32::MAX as u64);
}

#[test]
fn values_with_x_or_z_match_only_themselves() {
    let mut heap = Heap::new();
    // 4'b1x0z: x is a set bit of both planes, z one of the mask only.
    let pattern = Constant {
        ty: ValueType {
            width: 4,
            signed: false,
        },
        value: BigUint::from(0b1100u8),
        mask: BigUint::from(0b0101u8),
    };
    let four_state = PointType {
        four_state: true,
        ..point_type(4, false)
    };
    let declared = || {
        vec![
            bins("xz", vec![value(pattern.clone())]),
            bins("plain", vec![value(sized(4, 0b1100))]),
        ]
    };
    // 5'b01x0z is the same value of four bits.
    let longer = Constant {
        ty: ValueType {
            width: 5,
            signed: false,
        },
        ..pattern.clone()
    };
    let repeated = vec![
        value(pattern.clone()),
        value(sized(4, 0b1100)),
        value(pattern.clone()),
        value(longer),
    ];
    let key = register(
        &mut heap,
        group(
            "g",
            vec![
                point("logic", four_state.clone(), declared()),
                point("bit", point_type(4, false), declared()),
                point("auto", four_state.clone(), vec![]),
                point(
                    "repeated",
                    four_state.clone(),
                    vec![bins("twice", repeated)],
                ),
            ],
        ),
    );
    // On a 2-state type the value takes no part (19.5.7 b) 3).
    let (object, warnings) = construct_with_warnings(&mut heap, key, &[]);
    assert_eq!(warnings, 1);
    assert_eq!(
        message(&mut heap, object),
        "bins `xz` of coverpoint `bit` of covergroup `g` (instance `g#0`): value 4'b1x0z has x \
         or z bits and does not take part (IEEE 1800-2023 19.5.7)"
    );
    let sample_planes = |heap: &mut Heap, value: u64, mask: u64| {
        for item in 1..=4 {
            call(heap, op::POINT, item, 0, [object.into(), value, mask]);
        }
        call(heap, op::SAMPLE_END, 0, 0, [object.into(), 0, 0]);
    };
    sample_planes(&mut heap, 0b1100, 0b0101);
    // A bin that lists the value more than once has one more hit for a
    // sample of it, not one for each time.
    assert_eq!(instance_bins(&heap, 0, 3), [("twice".to_string(), 1)]);
    sample_planes(&mut heap, 0b1100, 0);
    // 4'b1x00 and 4'b1z0z are other values.
    sample_planes(&mut heap, 0b1100, 0b0100);
    sample_planes(&mut heap, 0b1000, 0b0101);
    assert_eq!(
        instance_bins(&heap, 0, 0),
        [("xz".to_string(), 1), ("plain".to_string(), 1)]
    );
    assert_eq!(instance_bins(&heap, 0, 3), [("twice".to_string(), 2)]);
    // The bin without a value is not a bin of the coverpoint (19.11.1).
    assert_eq!(instance_bins(&heap, 0, 1), [("plain".to_string(), 1)]);
    assert_eq!(inst_counts(&mut heap, object, 2), (1, 1));
    // Automatic bins hold no value with x or z (19.5.3).
    let auto = instance_bins(&heap, 0, 2);
    assert_eq!(auto.iter().map(|bin| bin.1).sum::<u64>(), 1);
    assert_eq!(auto[12], ("auto[12]".to_string(), 1));
    assert_eq!(
        heap.coverage_report().covergroups[0].instances[0].samples,
        4
    );
    // A range with x or z in a bound takes no part on any type.
    let dropped = resolve(
        &four_state,
        RangeValue::Range {
            low: Some(&pattern),
            high: None,
        },
    );
    assert_eq!((dropped.items.len(), dropped.warnings.len()), (0, 1));
    assert!(dropped.warnings[0].starts_with("range [4'b1x0z:$] has x or z bits in a bound"));
    // A value is cast to the width of the type as it is written.
    let narrow = Constant {
        ty: ValueType {
            width: 2,
            signed: true,
        },
        value: BigUint::from(0b10u8),
        mask: BigUint::from(0b10u8),
    };
    assert_eq!(
        resolve(
            &PointType {
                signed: true,
                ..four_state.clone()
            },
            RangeValue::Value(&narrow)
        )
        .items,
        [ResolvedItem::Exact {
            value: BigUint::from(0b1110u8),
            mask: BigUint::from(0b1110u8),
        }]
    );
    let wide = Constant {
        ty: ValueType {
            width: 6,
            signed: false,
        },
        value: BigUint::from(0b10_1100u8),
        mask: BigUint::from(0b00_0001u8),
    };
    let resolved = resolve(&four_state, RangeValue::Value(&wide));
    assert_eq!((resolved.items.len(), resolved.warnings.len()), (0, 1));
}

#[test]
fn overlapping_bins_warn_on_request() {
    let mut heap = Heap::new();
    let mut descriptor = group(
        "g",
        vec![point(
            "p",
            point_type(3, false),
            vec![
                bins("a", vec![range(int(0), int(3))]),
                bins("b", vec![range(int(3), int(5)), value(int(3))]),
                bins("c", vec![value(int(7))]),
            ],
        )],
    );
    descriptor.inputs = int_inputs(1);
    descriptor.options = vec![option_input(1, OptionId::DetectOverlap, 0)];
    let key = register(&mut heap, descriptor);
    let (object, warnings) = construct_with_warnings(&mut heap, key, &[1]);
    assert_eq!(warnings, 1);
    assert_eq!(
        message(&mut heap, object),
        "coverpoint `p` of covergroup `g` (instance `g#0`): bins `a` and `b` overlap \
         (option.detect_overlap)"
    );
    // A value of two bins counts in both.
    sample(&mut heap, object, &[3]);
    assert_eq!(
        instance_bins(&heap, 0, 0),
        [
            ("a".to_string(), 1),
            ("b".to_string(), 1),
            ("c".to_string(), 0)
        ]
    );
    // Without the option the same bins do not warn.
    construct(&mut heap, key, &[0]);
    assert_eq!(heap.coverage.types[0].shapes.len(), 2);
}

#[test]
fn instances_of_equal_inputs_share_their_bins() {
    let mut heap = Heap::new();
    let key = register(&mut heap, merge_example());
    let first = construct(&mut heap, key, &[0, 0, 1, 1, 1, 0, 0]);
    let same = construct(&mut heap, key, &[0, 0, 1, 1, 1, 0, 0]);
    let other = construct(&mut heap, key, &[1, 0, 1, 1, 2, 2, 0]);
    let shapes = |heap: &Heap| {
        heap.coverage
            .instances
            .iter()
            .map(|instance| instance.shape)
            .collect::<Vec<_>>()
    };
    assert_eq!(shapes(&heap), [Some(0), Some(0), Some(1)]);
    assert_eq!(heap.coverage.types[0].shapes.len(), 2);
    // But not their hits.
    sample(&mut heap, first, &[0]);
    assert_eq!(instance_bins(&heap, 0, 0)[0].1, 1);
    assert_eq!(instance_bins(&heap, 1, 0)[0].1, 0);
    assert_eq!(instance_bins(&heap, 2, 0)[0].0, "b[1]");
    // The object of an instance is of the class of the call, zeroed, and
    // its random state is that of an object that was never seeded.
    let plain = heap.new_object(CLASS, 8);
    let unseeded = heap.object_randstate(plain);
    for object in [first, same, other] {
        assert_eq!(heap.class_of(object), CLASS);
        assert_eq!(heap.load(object, 0, 8), 0);
        assert_eq!(heap.object_randstate(object), unseeded);
    }
}

#[test]
fn coverage_survives_a_round_trip() {
    let mut heap = Heap::new();
    let key = register(&mut heap, merge_example());
    let wide = register(
        &mut heap,
        group("wide", vec![point("a", point_type(100, true), vec![])]),
    );
    let gv1 = construct(&mut heap, key, &[0, 0, 1, 1, 1, 0, 1]);
    let gv2 = construct(&mut heap, key, &[1, 0, 1, 1, 2, 2, 0]);
    let object = construct(&mut heap, wide, &[]);
    sample(&mut heap, gv1, &[0]);
    sample(&mut heap, gv2, &[1]);
    sample(&mut heap, object, &[5]);
    let comment = heap.intern(b"kept");
    set_option(&mut heap, gv2, 0, OptionId::Comment, comment.into());
    // A checkpoint file is the postcard encoding of the heap.
    let mut restored: Heap = postcard::from_bytes(&postcard::to_allocvec(&heap).unwrap()).unwrap();
    assert!(restored.same_contents(&heap));
    assert_eq!(restored.coverage_report(), heap.coverage_report());
    for heap in [&mut heap, &mut restored] {
        sample(heap, gv1, &[1]);
        sample(heap, object, &[u64::MAX]);
        let again = construct(heap, key, &[0, 0, 1, 1, 1, 0, 1]);
        sample(heap, again, &[0]);
    }
    assert!(restored.same_contents(&heap));
    assert_close(
        type_coverage(&mut restored, key, 0),
        type_coverage(&mut heap, key, 0),
    );
    // The descriptor and the report are JSON.
    let descriptor = merge_example();
    let json = serde_json::to_string(&descriptor).unwrap();
    assert_eq!(
        serde_json::from_str::<TypeDescriptor>(&json).unwrap(),
        descriptor
    );
    let report = heap.coverage_report();
    let json = serde_json::to_string(&report).unwrap();
    assert_eq!(
        serde_json::from_str::<CoverageReport>(&json).unwrap(),
        report
    );
}

#[test]
fn a_clone_counts_on_its_own() {
    let mut heap = Heap::new();
    let key = register(&mut heap, merge_example());
    let object = construct(&mut heap, key, &[0, 0, 1, 1, 1, 0, 0]);
    sample(&mut heap, object, &[0]);
    let checkpoint = heap.clone();
    assert!(heap.same_contents(&checkpoint));
    sample(&mut heap, object, &[1]);
    assert!(!heap.same_contents(&checkpoint));
    assert_close(type_coverage(&mut heap, key, 0), 100.0);
    let mut restored = checkpoint.clone();
    assert_close(type_coverage(&mut restored, key, 0), 50.0);
    // The same sample brings the clone to the same contents.
    sample(&mut restored, object, &[1]);
    assert!(heap.same_contents(&restored));
    // Options and instances are contents too.
    let mut other = heap.clone();
    set_option(&mut other, object, 0, OptionId::AtLeast, 2);
    assert!(!heap.same_contents(&other));
    let mut other = heap.clone();
    set_type_option(&mut other, key, 0, TypeOptionId::Goal, 1);
    assert!(!heap.same_contents(&other));
    let mut other = heap.clone();
    call(&mut other, op::COLLECT, 0, 0, [object.into(), 0, 0]);
    assert!(!heap.same_contents(&other));
    assert_eq!(heap.coverage_report().covergroups[0].instances.len(), 1);
    // What a sample in progress noted is not: no code runs between its
    // calls.
    let mut other = heap.clone();
    call(&mut other, op::POINT, 1, 0, [object.into(), 1, 0]);
    assert!(heap.same_contents(&other));
}

#[test]
fn a_collected_instance_keeps_its_coverage() {
    let mut heap = Heap::new();
    let key = register(&mut heap, merge_example());
    let kept = construct(&mut heap, key, &[0, 0, 1, 1, 1, 0, 0]);
    let lost = construct(&mut heap, key, &[1, 0, 1, 1, 2, 2, 0]);
    sample(&mut heap, kept, &[0]);
    sample(&mut heap, lost, &[1]);
    sample(&mut heap, lost, &[2]);
    let before = type_coverage(&mut heap, key, 0);
    let size = heap.content_size();
    heap.collect([Root::object(kept)]);
    assert!(heap.contains_object(kept));
    assert!(!heap.contains_object(lost));
    assert_close(type_coverage(&mut heap, key, 0), before);
    assert_close(before, 100.0);
    let report = heap.coverage_report();
    let instances = &report.covergroups[0].instances;
    assert_eq!(
        instances
            .iter()
            .map(|instance| (instance.live, instance.samples))
            .collect::<Vec<_>>(),
        [(true, 1), (false, 2)]
    );
    assert_eq!(instances[1].items[0].bins[1].hits, 1);
    // The hits are part of what the heap holds; the object was 8 bytes.
    assert_eq!(heap.content_size(), size - 8);
    // The flag is what a collection of a clone gives too.
    let mut clone = heap.clone();
    clone.collect([Root::object(kept)]);
    assert!(heap.same_contents(&clone));
    clone.collect([]);
    assert!(!heap.same_contents(&clone));
    assert_close(type_coverage(&mut clone, key, 0), 100.0);
    assert_eq!(heap.take_fault(), None);
}

#[test]
fn a_shallow_copy_has_no_embedded_covergroup() {
    let mut heap = Heap::new();
    let mut embedded = group("C::cg", vec![]);
    embedded.embedded = true;
    let embedded = register(&mut heap, embedded);
    let named = register(&mut heap, group("cg", vec![]));
    // An object with nothing of the kind copies as before.
    let plain = heap.new_object(1, 4);
    heap.store(plain, 0, 4, 9);
    let copy = heap.copy_object(plain);
    assert_eq!(heap.load(copy, 0, 4), 9);

    let embedded = construct(&mut heap, embedded, &[]);
    let named = construct(&mut heap, named, &[]);
    let other = heap.new_object(2, 4);
    let owner = heap.new_object(1, 20);
    // A string handle may be the number of an instance.
    for (offset, kind, value) in [
        (0u32, HandleKind::Object, embedded),
        (4, HandleKind::Object, named),
        (8, HandleKind::Object, other),
        (12, HandleKind::String, embedded),
    ] {
        heap.declare_handle(owner, offset, kind);
        heap.store(owner, offset as usize, 4, value.into());
    }
    // A value that is no handle property stays as well.
    heap.store(owner, 16, 4, embedded.into());
    let copy = heap.copy_object(owner);
    // "the embedded covergroup shall be null in the new object" (8.12).
    assert_eq!(heap.load(copy, 0, 4), u64::from(NULL));
    assert_eq!(heap.load(copy, 4, 4), u64::from(named));
    assert_eq!(heap.load(copy, 8, 4), u64::from(other));
    assert_eq!(heap.load(copy, 12, 4), u64::from(embedded));
    assert_eq!(heap.load(copy, 16, 4), u64::from(embedded));
    assert_eq!(heap.load(owner, 0, 4), u64::from(embedded));
    assert_eq!(heap.take_fault(), None);
    // The copy does not keep the instance's object alive.
    heap.collect([Root::object(copy)]);
    assert!(!heap.contains_object(embedded));
    assert!(heap.contains_object(named));
}

#[test]
fn the_report_lists_types_instances_items_and_bins() {
    let mut heap = Heap::new();
    let mut descriptor = group(
        "Top.cg",
        vec![
            point(
                "a",
                point_type(2, false),
                vec![
                    bins("low", vec![range(int(0), int(1))]),
                    bins("high", vec![range(int(2), int(3))]),
                ],
            ),
            point("b", point_type(1, false), vec![]),
        ],
    );
    descriptor.type_options.comment = "a type".into();
    descriptor.type_options.goal = 90;
    let key = register(&mut heap, descriptor);
    // Types are listed by name, and only those with an instance.
    let earlier = register(&mut heap, group("A.cg", vec![]));
    register(&mut heap, group("B.cg", vec![]));
    let object = construct(&mut heap, key, &[]);
    construct(&mut heap, earlier, &[]);
    sample(&mut heap, object, &[0, 1]);
    sample(&mut heap, object, &[1, 1]);
    let item = |name: &str, coverage: f64, covered: u64, bins: &[(&str, u64)]| CoverItemReport {
        name: name.into(),
        kind: CoverItemKind::Coverpoint,
        coverpoints: Vec::new(),
        comment: String::new(),
        weight: 1,
        goal: 100,
        at_least: 1,
        coverage,
        covered,
        total: 2,
        bins: bins
            .iter()
            .map(|&(name, hits)| BinReport {
                name: name.into(),
                kind: BinReportKind::Bins,
                hits,
                covered: hits != 0,
            })
            .collect(),
        omitted_bins: 0,
    };
    let items = || {
        vec![
            item("a", 50.0, 1, &[("low", 2), ("high", 0)]),
            item("b", 50.0, 1, &[("auto[0]", 0), ("auto[1]", 2)]),
        ]
    };
    let report = heap.coverage_report();
    assert_eq!(
        report,
        CoverageReport {
            coverage: 50.0,
            covergroups: vec![
                CovergroupReport {
                    name: "A.cg".into(),
                    comment: String::new(),
                    weight: 1,
                    goal: 100,
                    merge_instances: false,
                    coverage: 0.0,
                    covered: 0,
                    total: 0,
                    items: Vec::new(),
                    instances: vec![CovergroupInstanceReport {
                        name: "A.cg#0".into(),
                        comment: String::new(),
                        weight: 1,
                        goal: 100,
                        at_least: 1,
                        per_instance: false,
                        collecting: true,
                        live: true,
                        samples: 0,
                        coverage: 0.0,
                        covered: 0,
                        total: 0,
                        items: Vec::new(),
                    }],
                },
                CovergroupReport {
                    name: "Top.cg".into(),
                    comment: "a type".into(),
                    weight: 1,
                    goal: 90,
                    merge_instances: false,
                    coverage: 50.0,
                    covered: 2,
                    total: 4,
                    items: items(),
                    instances: vec![CovergroupInstanceReport {
                        name: "Top.cg#0".into(),
                        comment: String::new(),
                        weight: 1,
                        goal: 100,
                        at_least: 1,
                        per_instance: false,
                        collecting: true,
                        live: true,
                        samples: 2,
                        coverage: 50.0,
                        covered: 2,
                        total: 4,
                        items: items(),
                    }],
                },
            ],
        }
    );
    // The names of the fields are those of the TypeScript API.
    let json = serde_json::to_value(&report).unwrap();
    let covergroup = &json["covergroups"][1];
    assert_eq!(covergroup["mergeInstances"], false);
    let instance = &covergroup["instances"][0];
    assert_eq!(instance["perInstance"], false);
    assert_eq!(instance["atLeast"], 1);
    let item = &instance["items"][0];
    assert_eq!(item["kind"], "coverpoint");
    assert_eq!(item["omittedBins"], 0);
    assert_eq!(
        item["bins"][0],
        serde_json::json!({"name": "low", "kind": "bins", "hits": 2, "covered": true})
    );
    assert_eq!(
        serde_json::to_value(BinReportKind::Illegal).unwrap(),
        "illegal"
    );
    assert_eq!(serde_json::to_value(CoverItemKind::Cross).unwrap(), "cross");
}

#[test]
fn type_keys_are_the_fnv_1a_hash_of_the_name() {
    assert_eq!(type_key(""), 0xcbf2_9ce4_8422_2325);
    assert_eq!(type_key("a"), 0xaf63_dc4c_8601_ec8c);
    assert_eq!(type_key("foobar"), 0x8594_4171_f739_67e8);
    // An operation code holds the operation and its two operands.
    assert_eq!(code(op::POINT, 3, 2) & 0xff, COVERAGE);
    assert_eq!(code(op::POINT, 3, 2) >> 8, 10 | 3 << 8 | 2 << 32);
    for option in [OptionId::Name, OptionId::GetInstCoverage] {
        assert_eq!(OptionId::from_code(option.code()), Some(option));
    }
    assert_eq!(OptionId::from_code(99), None);
    assert_eq!(TypeOptionId::from_code(99), None);
}
