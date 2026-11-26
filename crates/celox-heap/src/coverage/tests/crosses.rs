//! Crosses: their automatic bins, the bins they declare with `binsof` and
//! `intersect`, their options, and their coverage for an instance and for a
//! type (IEEE 1800-2023 19.6, 19.11.2, 19.11.3).

use super::*;

/// `binsof(item)` of the coverpoint at `position` of a cross.
fn binsof(position: u32) -> Select {
    Select::BinsOf {
        position,
        bin: None,
        intersect: None,
        negate: false,
    }
}

/// `binsof(item.bin)` of bin declaration `bin` of the coverpoint.
fn binsof_bin(position: u32, bin: u32) -> Select {
    Select::BinsOf {
        position,
        bin: Some(bin),
        intersect: None,
        negate: false,
    }
}

/// `… intersect {ranges}`.
fn intersect(select: Select, ranges: Vec<RangeDescriptor>) -> Select {
    let Select::BinsOf {
        position,
        bin,
        negate,
        ..
    } = select
    else {
        panic!("`intersect` of no `binsof`");
    };
    Select::BinsOf {
        position,
        bin,
        intersect: Some(ranges),
        negate,
    }
}

/// `! …`.
fn not(select: Select) -> Select {
    let Select::BinsOf {
        position,
        bin,
        intersect,
        ..
    } = select
    else {
        panic!("`!` of no `binsof`");
    };
    Select::BinsOf {
        position,
        bin,
        intersect,
        negate: true,
    }
}

fn cross_bins(name: &str, select: Select) -> CrossBinDescriptor {
    CrossBinDescriptor::new(name, select)
}

fn cross_ignore(name: &str, select: Select) -> CrossBinDescriptor {
    CrossBinDescriptor {
        kind: BinKind::Ignore,
        ..CrossBinDescriptor::new(name, select)
    }
}

fn cross_illegal(name: &str, select: Select) -> CrossBinDescriptor {
    CrossBinDescriptor {
        kind: BinKind::Illegal,
        ..CrossBinDescriptor::new(name, select)
    }
}

fn cross(name: &str, points: &[u32], bins: Vec<CrossBinDescriptor>) -> CrossDescriptor {
    let mut cross = CrossDescriptor::new(name, points.to_vec());
    cross.bins = bins;
    cross
}

fn crossing(
    name: &str,
    points: Vec<PointDescriptor>,
    crosses: Vec<CrossDescriptor>,
) -> TypeDescriptor {
    let mut descriptor = group(name, points);
    descriptor.crosses = crosses;
    descriptor
}

/// `bins name[] = …`.
fn each(mut bin: BinDescriptor) -> BinDescriptor {
    bin.array = BinArray::Open;
    bin
}

fn ignored(mut bin: BinDescriptor) -> BinDescriptor {
    bin.kind = BinKind::Ignore;
    bin
}

/// A sized unsigned value.
fn number(width: u32, value: u64) -> RangeDescriptor {
    value_of(sized(width, value))
}

fn value_of(constant: Constant) -> RangeDescriptor {
    value(constant)
}

fn span(width: u32, low: u64, high: u64) -> RangeDescriptor {
    range(sized(width, low), sized(width, high))
}

/// Sample `values`, the value of each coverpoint in turn: the number of
/// illegal bins hit.
fn sample_illegal(heap: &mut Heap, object: Handle, values: &[u64]) -> u64 {
    assert_eq!(call(heap, op::SAMPLING, 0, 0, [object.into(), 0, 0]), 1);
    for (index, value) in values.iter().enumerate() {
        let item = index as u64 + 1;
        call(heap, op::POINT, item, 0, [object.into(), *value, 0]);
    }
    call(heap, op::SAMPLE_END, 0, 0, [object.into(), 0, 0])
}

/// Sample `values` with the predicate bits `bits` of item `item`.
fn sample_guarded(heap: &mut Heap, object: Handle, values: &[u64], item: u64, bits: u64) -> u64 {
    for (index, value) in values.iter().enumerate() {
        let point = index as u64 + 1;
        call(heap, op::POINT, point, 0, [object.into(), *value, 0]);
    }
    call(heap, op::PREDICATES, item, 0, [object.into(), bits, 0]);
    call(heap, op::SAMPLE_END, 0, 0, [object.into(), 0, 0])
}

fn fault(heap: &mut Heap) -> Option<String> {
    heap.take_fault().map(|fault| fault.to_string())
}

/// Item `item` of instance `instance` of the first covergroup of the
/// report, where item 0 is the first coverpoint.
fn instance_item(heap: &Heap, instance: usize, item: usize) -> CoverItemReport {
    heap.coverage_report().covergroups[0].instances[instance].items[item].clone()
}

/// The same for the type.
fn type_item(heap: &Heap, item: usize) -> CoverItemReport {
    heap.coverage_report().covergroups[0].items[item].clone()
}

fn names(item: &CoverItemReport) -> Vec<(&str, u64)> {
    item.bins
        .iter()
        .map(|bin| (bin.name.as_str(), bin.hits))
        .collect()
}

/// The covergroup `ct` of 19.6.1, with as many missing bins listed as
/// there are.
fn ct() -> TypeDescriptor {
    let array = |name: &str| {
        point(
            name,
            point_type(32, true),
            vec![each(bins(name, vec![range(int(0), int(1))]))],
        )
    };
    let mut descriptor = crossing(
        "ct",
        vec![array("i"), array("j")],
        vec![
            cross("x1", &[0, 1], vec![]),
            cross(
                "x2",
                &[0, 1],
                vec![cross_bins(
                    "i_zero",
                    intersect(binsof(0), vec![value_of(int(0))]),
                )],
            ),
        ],
    );
    descriptor.options = vec![option(0, OptionId::CrossNumPrintMissing, 4)];
    descriptor
}

#[test]
fn a_declared_cross_bin_takes_its_cross_products_from_the_automatic_bins() {
    let mut heap = Heap::new();
    let key = register(&mut heap, ct());
    let object = construct(&mut heap, key, &[]);
    // "Cross x1 has the following bins", in the order of the keys here.
    assert_eq!(
        names(&instance_item(&heap, 0, 2)),
        [
            ("<i[0],j[0]>", 0),
            ("<i[0],j[1]>", 0),
            ("<i[1],j[0]>", 0),
            ("<i[1],j[1]>", 0),
        ]
    );
    // "Cross x2 has the following bins": `i_zero` for `<i[0],j[0]>` and
    // `<i[0],j[1]>`, and the two automatic bins that are retained.
    assert_eq!(
        names(&instance_item(&heap, 0, 3)),
        [("i_zero", 0), ("<i[1],j[0]>", 0), ("<i[1],j[1]>", 0)]
    );
    assert_eq!(inst_counts(&mut heap, object, 3), (0, 4));
    assert_eq!(inst_counts(&mut heap, object, 4), (0, 3));
    sample(&mut heap, object, &[0, 0]);
    sample(&mut heap, object, &[0, 1]);
    sample(&mut heap, object, &[1, 0]);
    assert_eq!(
        names(&instance_item(&heap, 0, 2)),
        [
            ("<i[0],j[0]>", 1),
            ("<i[0],j[1]>", 1),
            ("<i[1],j[0]>", 1),
            ("<i[1],j[1]>", 0),
        ]
    );
    assert_eq!(
        names(&instance_item(&heap, 0, 3)),
        [("i_zero", 2), ("<i[1],j[0]>", 1), ("<i[1],j[1]>", 0)]
    );
    assert_close(inst_coverage(&mut heap, object, 3), 75.0);
    assert_eq!(inst_counts(&mut heap, object, 3), (3, 4));
    assert_close(inst_coverage(&mut heap, object, 4), 200.0 / 3.0);
    assert_eq!(inst_counts(&mut heap, object, 4), (2, 3));
    // The covergroup is the average of its four items, and has their bins.
    assert_close(
        inst_coverage(&mut heap, object, 0),
        (100.0 + 100.0 + 75.0 + 200.0 / 3.0) / 4.0,
    );
    assert_eq!(
        inst_counts(&mut heap, object, 0),
        (2 + 2 + 3 + 2, 2 + 2 + 4 + 3)
    );
    assert_close(type_coverage(&mut heap, key, 4), 200.0 / 3.0);
    let item = instance_item(&heap, 0, 3);
    assert_eq!(item.kind, CoverItemKind::Cross);
    assert_eq!(item.coverpoints, ["i", "j"]);
    assert_eq!((item.covered, item.total, item.omitted_bins), (2, 3, 0));
    assert_eq!(
        item.bins
            .iter()
            .map(|bin| (bin.kind, bin.covered))
            .collect::<Vec<_>>(),
        [
            (BinReportKind::Bins, true),
            (BinReportKind::Bins, true),
            (BinReportKind::Bins, false),
        ]
    );
    // A sample of a value that is in no bin of a coverpoint is none of the
    // crosses.
    sample(&mut heap, object, &[1, 2]);
    assert_eq!(inst_counts(&mut heap, object, 3), (3, 4));
    assert_eq!(
        heap.coverage_report().covergroups[0].instances[0].samples,
        4
    );
}

#[test]
fn a_cross_bin_is_covered_with_at_least_as_many_hits() {
    let mut heap = Heap::new();
    let key = register(&mut heap, ct());
    let object = construct(&mut heap, key, &[]);
    set_option(&mut heap, object, 4, OptionId::AtLeast, 2);
    let covered = |heap: &Heap| {
        instance_item(heap, 0, 3)
            .bins
            .iter()
            .map(|bin| (bin.hits, bin.covered))
            .collect::<Vec<_>>()
    };
    // One hit of `i_zero` and one of `<i[1],j[0]>` cover neither.
    sample(&mut heap, object, &[0, 0]);
    sample(&mut heap, object, &[1, 0]);
    assert_eq!(inst_counts(&mut heap, object, 4), (0, 3));
    assert_eq!(covered(&heap), [(1, false), (1, false), (0, false)]);
    // The other cross of the covergroup needs one.
    assert_eq!(inst_counts(&mut heap, object, 3), (2, 4));
    sample(&mut heap, object, &[0, 1]);
    assert_eq!(inst_counts(&mut heap, object, 4), (1, 3));
    sample(&mut heap, object, &[1, 0]);
    assert_eq!(inst_counts(&mut heap, object, 4), (2, 3));
    assert_eq!(covered(&heap), [(2, true), (2, true), (0, false)]);
    // A bin of the type needs what the instance that asks for most needs
    // (19.11.1), of the hits of all instances.
    let other = construct(&mut heap, key, &[]);
    sample(&mut heap, other, &[0, 0]);
    set_type_option(&mut heap, key, 0, TypeOptionId::MergeInstances, 1);
    assert_eq!(type_counts(&mut heap, key, 4), (2, 3));
    set_option(&mut heap, object, 4, OptionId::AtLeast, 3);
    assert_eq!(type_counts(&mut heap, key, 4), (1, 3));
    set_option(&mut heap, object, 4, OptionId::AtLeast, 4);
    assert_eq!(type_counts(&mut heap, key, 4), (0, 3));
    assert_eq!(
        type_item(&heap, 3)
            .bins
            .iter()
            .map(|bin| (bin.hits, bin.covered))
            .collect::<Vec<_>>(),
        [(3, false), (2, false), (0, false)]
    );
    assert_eq!(heap.take_fault(), None);
}

/// The covergroup `cg` of 19.6.1.1.
fn select_example() -> TypeDescriptor {
    let a = point(
        "a",
        point_type(8, false),
        vec![
            bins("a1", vec![span(8, 0, 63)]),
            bins("a2", vec![span(8, 64, 127)]),
            bins("a3", vec![span(8, 128, 191)]),
            bins("a4", vec![span(8, 192, 255)]),
        ],
    );
    let b = point(
        "b",
        point_type(8, false),
        vec![
            bins("b1", vec![number(8, 0)]),
            bins("b2", vec![span(8, 1, 84)]),
            bins("b3", vec![span(8, 85, 169)]),
            bins("b4", vec![span(8, 170, 255)]),
        ],
    );
    let c = cross(
        "c",
        &[0, 1],
        vec![
            cross_bins(
                "c1",
                not(intersect(binsof(0), vec![range(int(100), int(200))])),
            ),
            cross_bins("c2", Select::Or(vec![binsof_bin(0, 1), binsof_bin(1, 1)])),
            cross_bins("c3", Select::And(vec![binsof_bin(0, 0), binsof_bin(1, 3)])),
        ],
    );
    crossing("cg", vec![a, b], vec![c])
}

#[test]
fn select_expressions_select_the_cross_products_of_19_6_1_1() {
    let mut heap = Heap::new();
    let key = register(&mut heap, select_example());
    let object = construct(&mut heap, key, &[]);
    // Three declared bins and "6 of these": the denominator is 9.
    assert_eq!(inst_counts(&mut heap, object, 3), (0, 9));
    let item = instance_item(&heap, 0, 2);
    assert_eq!(names(&item), [("c1", 0), ("c2", 0), ("c3", 0)]);
    assert_eq!(item.omitted_bins, 6);
    // The automatic bins that are retained.
    set_option(&mut heap, object, 3, OptionId::CrossNumPrintMissing, 100);
    let item = instance_item(&heap, 0, 2);
    assert_eq!(
        names(&item)[3..],
        [
            ("<a3,b1>", 0),
            ("<a3,b3>", 0),
            ("<a3,b4>", 0),
            ("<a4,b1>", 0),
            ("<a4,b3>", 0),
            ("<a4,b4>", 0),
        ]
    );
    assert_eq!(item.omitted_bins, 0);
    // A sample of each of the 16 cross products: `c1` has 4 of them, `c2`
    // 7 and `c3` 1, and each of the 6 others is a bin of its own.
    for a in [0, 64, 128, 192] {
        for b in [0, 1, 85, 170] {
            sample(&mut heap, object, &[a, b]);
        }
    }
    assert_eq!(
        names(&instance_item(&heap, 0, 2)),
        [
            ("c1", 4),
            ("c2", 7),
            ("c3", 1),
            ("<a3,b1>", 1),
            ("<a3,b3>", 1),
            ("<a3,b4>", 1),
            ("<a4,b1>", 1),
            ("<a4,b3>", 1),
            ("<a4,b4>", 1),
        ]
    );
    assert_eq!(inst_counts(&mut heap, object, 3), (9, 9));
    assert_close(inst_coverage(&mut heap, object, 3), 100.0);
    // `<a1,b4>` is a cross product of `c1` and of `c3`, `<a1,b2>` one of
    // `c1` and of `c2`, and the ends of the ranges are in their bins.
    let other = construct(&mut heap, key, &[]);
    sample(&mut heap, other, &[63, 255]);
    assert_eq!(
        names(&instance_item(&heap, 1, 2)),
        [("c1", 1), ("c2", 0), ("c3", 1)]
    );
    sample(&mut heap, other, &[63, 84]);
    sample(&mut heap, other, &[127, 0]);
    sample(&mut heap, other, &[191, 169]);
    assert_eq!(
        names(&instance_item(&heap, 1, 2)),
        [("c1", 2), ("c2", 2), ("c3", 1), ("<a3,b3>", 1)]
    );
    assert_eq!(inst_counts(&mut heap, other, 3), (4, 9));
}

/// The cross `xx` of the reference test `c19_bins2`: a bin array of a
/// fixed size crossed with automatic bins, with a bin, an ignore bin and
/// an illegal bin of its own.
fn reference_select() -> TypeDescriptor {
    let mut fixed = bins("f", vec![range(int(0), int(9))]);
    fixed.array = BinArray::Fixed(Operand::Constant(int(3)));
    let cpfix = point("cpfix", point_type(4, false), vec![fixed]);
    let cpy = point("cpy", point_type(2, false), vec![]);
    let xx = cross(
        "xx",
        &[0, 1],
        vec![
            cross_bins(
                "sel",
                intersect(binsof_bin(0, 0), vec![range(int(0), int(2))]),
            ),
            cross_ignore("ig", intersect(binsof(1), vec![value_of(int(3))])),
            cross_illegal(
                "il",
                Select::And(vec![
                    intersect(binsof(0), vec![value_of(int(9))]),
                    intersect(binsof(1), vec![value_of(int(2))]),
                ]),
            ),
        ],
    );
    crossing("c19_bins2.cg", vec![cpfix, cpy], vec![xx])
}

#[test]
fn ignored_and_illegal_cross_products_are_those_of_no_other_bin() {
    let mut heap = Heap::new();
    let key = register(&mut heap, reference_select());
    let object = construct(&mut heap, key, &[]);
    // `f[0]` is 0 to 2, `f[1]` 3 to 5 and `f[2]` 6 to 9. Of the 12 cross
    // products `sel` has the 4 of `f[0]`, `ig` those of `auto[3]`, of
    // which 2 are not in `sel`, and `il` has `<f[2],auto[2]>`: 7 are in a
    // declared bin and 5 are automatic bins, with `sel` 6 bins.
    assert_eq!(inst_counts(&mut heap, object, 3), (0, 6));
    let stimulus = [0, 1, 2, 3, 3, 3, 7, 8, 9, 1, 2, 12, 14, 0, 5];
    let mut errors = Vec::new();
    for (index, &x) in stimulus.iter().enumerate() {
        if sample_illegal(&mut heap, object, &[x, index as u64 % 4]) != 0 {
            errors.push((index, message(&mut heap, object)));
        }
    }
    assert_eq!(heap.take_fault(), None);
    assert_eq!(
        errors,
        [(
            6,
            "illegal_bins `il` of cross `xx` of covergroup `c19_bins2.cg` (instance \
             `c19_bins2.cg#0`): <f[2],auto[2]>"
                .to_string()
        )]
    );
    // The reference value 83.33: `sel` and four of the five automatic
    // bins.
    assert_close(inst_coverage(&mut heap, object, 3), 500.0 / 6.0);
    assert_eq!(inst_counts(&mut heap, object, 3), (5, 6));
    let item = instance_item(&heap, 0, 2);
    assert_eq!(
        names(&item),
        [
            ("sel", 6),
            ("ig", 2),
            ("il", 1),
            ("<f[1],auto[0]>", 1),
            ("<f[1],auto[1]>", 1),
            ("<f[1],auto[2]>", 1),
            ("<f[2],auto[0]>", 1),
        ]
    );
    assert_eq!(
        item.bins[..3]
            .iter()
            .map(|bin| bin.kind)
            .collect::<Vec<_>>(),
        [
            BinReportKind::Bins,
            BinReportKind::Ignore,
            BinReportKind::Illegal
        ]
    );
    // `<f[2],auto[1]>` was not sampled.
    assert_eq!(item.omitted_bins, 1);
    // `<f[0],auto[3]>` is a cross product of `sel` that is ignored: "even
    // if they are included in other cross coverage bins" (19.6.2).
    let other = construct(&mut heap, key, &[]);
    sample(&mut heap, other, &[0, 3]);
    assert_eq!(
        names(&instance_item(&heap, 1, 2)),
        [("sel", 0), ("ig", 1), ("il", 0)]
    );
    assert_eq!(inst_counts(&mut heap, other, 3), (0, 6));
}

#[test]
fn illegal_cross_products_take_precedence() {
    // `<p[1],q[1]>` is illegal, ignored and a cross product of `both`
    // (19.6.3): an error, and a hit of no `bins` bin. `gone` has no cross
    // product that is not ignored, so it is no bin of the coverage.
    let pair = |name: &str| {
        point(
            name,
            point_type(1, false),
            vec![each(bins(name, vec![span(1, 0, 1)]))],
        )
    };
    let one = |position| intersect(binsof(position), vec![number(1, 1)]);
    let mut guarded = cross_illegal("bad", Select::And(vec![one(0), one(1)]));
    guarded.iff = Some(1);
    let x = cross(
        "x",
        &[0, 1],
        vec![
            cross_bins("both", Select::Or(vec![one(0), one(1)])),
            guarded,
            cross_ignore("skip", Select::And(vec![one(0), one(1)])),
            cross_bins("gone", Select::And(vec![one(0), one(1)])),
        ],
    );
    let mut descriptor = crossing("t", vec![pair("p"), pair("q")], vec![x]);
    descriptor.crosses[0].predicates = 2;
    let mut heap = Heap::new();
    let key = register(&mut heap, descriptor);
    let object = construct(&mut heap, key, &[]);
    // `both` and the automatic bin `<p[0],q[0]>`.
    assert_eq!(inst_counts(&mut heap, object, 3), (0, 2));
    assert_eq!(sample_illegal(&mut heap, object, &[1, 1]), 1);
    assert_eq!(
        message(&mut heap, object),
        "illegal_bins `bad` of cross `x` of covergroup `t` (instance `t#0`): <p[1],q[1]>"
    );
    assert_eq!(
        names(&instance_item(&heap, 0, 2)),
        [("both", 0), ("bad", 1), ("skip", 1)]
    );
    // With its guard false the illegal bin is no error and counts
    // nothing, and the cross product stays one of no other `bins` bin.
    assert_eq!(sample_guarded(&mut heap, object, &[1, 1], 3, 0b01), 0);
    assert_eq!(
        names(&instance_item(&heap, 0, 2)),
        [("both", 0), ("bad", 1), ("skip", 2)]
    );
    assert_eq!(sample_illegal(&mut heap, object, &[1, 0]), 0);
    assert_eq!(sample_illegal(&mut heap, object, &[0, 0]), 0);
    assert_eq!(
        names(&instance_item(&heap, 0, 2)),
        [("both", 1), ("bad", 1), ("skip", 2), ("<p[0],q[0]>", 1)]
    );
    assert_eq!(inst_counts(&mut heap, object, 3), (2, 2));
    // The type has the same bins, also as the union of its instances:
    // `gone` is a bin of none of them.
    let bins = [("both", 1), ("bad", 1), ("skip", 2), ("<p[0],q[0]>", 1)];
    assert_eq!(names(&type_item(&heap, 2)), bins);
    set_type_option(&mut heap, key, 0, TypeOptionId::MergeInstances, 1);
    assert_eq!(names(&type_item(&heap, 2)), bins);
    assert_eq!(type_counts(&mut heap, key, 3), (2, 2));
    assert_eq!(heap.take_fault(), None);
}

#[test]
fn the_reference_cross_of_c19_basic_is_covered() {
    // `cg_x`: `ig` has the 2 cross products of `auto[3]`, `hb` those of
    // `h` with `auto[0]` and `auto[1]`, and 4 are automatic bins.
    let cpa = point(
        "cpa",
        point_type(4, false),
        vec![
            bins("l", vec![range(int(0), int(7))]),
            bins("h", vec![range(int(8), int(15))]),
        ],
    );
    let cpb = point("cpb", point_type(2, false), vec![]);
    let xc = cross(
        "xc",
        &[0, 1],
        vec![
            cross_ignore("ig", intersect(binsof(1), vec![value_of(int(3))])),
            cross_bins(
                "hb",
                Select::And(vec![
                    binsof_bin(0, 1),
                    intersect(binsof(1), vec![range(int(0), int(1))]),
                ]),
            ),
        ],
    );
    let mut heap = Heap::new();
    let key = register(&mut heap, crossing("cg_x", vec![cpa, cpb], vec![xc]));
    let object = construct(&mut heap, key, &[]);
    assert_eq!(inst_counts(&mut heap, object, 3), (0, 5));
    let stimulus = [
        1, 2, 3, 4, 5, 5, 1, 6, 8, 0, 8, 9, 10, 0, 10, 12, 15, 7, 13, 4,
    ];
    for (index, &a) in stimulus.iter().enumerate() {
        sample(&mut heap, object, &[a, index as u64 % 4]);
    }
    assert_eq!(
        names(&instance_item(&heap, 0, 2)),
        [
            ("ig", 5),
            ("hb", 3),
            ("<l,auto[0]>", 2),
            ("<l,auto[1]>", 5),
            ("<l,auto[2]>", 2),
            ("<h,auto[2]>", 3),
        ]
    );
    // The reference: cpa=100.00 cpb=100.00 xc=100.00 tot=100.00.
    for item in 0..4 {
        assert_close(type_coverage(&mut heap, key, item), 100.0);
    }
    assert_eq!(type_counts(&mut heap, key, 3), (5, 5));
}

/// The embedded covergroup of the reference test `c19_more`.
fn automatic_example() -> TypeDescriptor {
    let ck = point("ck", point_type(3, false), vec![]);
    let cm = point(
        "cm",
        point_type(2, false),
        vec![
            bins("m0", vec![value_of(int(0))]),
            bins("m1", vec![value_of(int(1))]),
        ],
    );
    crossing("Tr::cg", vec![ck, cm], vec![cross("kxm", &[0, 1], vec![])])
}

#[test]
fn the_automatic_bins_of_instances_unite_by_name() {
    let mut heap = Heap::new();
    let key = register(&mut heap, automatic_example());
    let t1 = construct(&mut heap, key, &[]);
    let t2 = construct(&mut heap, key, &[]);
    for value in 0..8 {
        sample(&mut heap, t1, &[value, value % 2]);
    }
    sample(&mut heap, t2, &[1, 0]);
    sample(&mut heap, t2, &[0, 0]);
    // 8 and 2 of the 16 cross products.
    assert_close(inst_coverage(&mut heap, t1, 3), 50.0);
    assert_close(inst_coverage(&mut heap, t2, 3), 12.5);
    assert_eq!(inst_counts(&mut heap, t2, 3), (2, 16));
    // The average of the instances, whose bins add up (19.11.3).
    assert_close(type_coverage(&mut heap, key, 3), 31.25);
    assert_eq!(type_counts(&mut heap, key, 3), (10, 32));
    // As a union they are 9 of 16 bins, which is what an instance gives
    // too unless it tracks its own coverage.
    set_type_option(&mut heap, key, 0, TypeOptionId::MergeInstances, 1);
    assert_close(type_coverage(&mut heap, key, 3), 56.25);
    assert_eq!(type_counts(&mut heap, key, 3), (9, 16));
    assert_close(inst_coverage(&mut heap, t1, 3), 56.25);
    set_option(&mut heap, t1, 0, OptionId::GetInstCoverage, 1);
    assert_close(inst_coverage(&mut heap, t1, 3), 50.0);
    // The bins of the type in the order instances first have them: the
    // hits of `<auto[0],m0>` add up.
    let item = type_item(&heap, 2);
    assert_eq!(
        names(&item),
        [
            ("<auto[0],m0>", 2),
            ("<auto[1],m1>", 1),
            ("<auto[2],m0>", 1),
            ("<auto[3],m1>", 1),
            ("<auto[4],m0>", 1),
            ("<auto[5],m1>", 1),
            ("<auto[6],m0>", 1),
            ("<auto[7],m1>", 1),
            ("<auto[1],m0>", 1),
        ]
    );
    assert_eq!((item.covered, item.total, item.omitted_bins), (9, 16, 7));
    // A bin of the type is covered with the hits that the instance that
    // asks for most needs.
    set_option(&mut heap, t2, 3, OptionId::AtLeast, 2);
    assert_eq!(type_counts(&mut heap, key, 3), (1, 16));
    // With no hit needed every bin is covered, sampled or not.
    for object in [t1, t2] {
        set_option(&mut heap, object, 3, OptionId::AtLeast, 0);
    }
    assert_eq!(type_counts(&mut heap, key, 3), (16, 16));
    set_type_option(&mut heap, key, 0, TypeOptionId::MergeInstances, 0);
    assert_eq!(inst_counts(&mut heap, t2, 3), (16, 16));
    assert_eq!(heap.take_fault(), None);
}

/// A covergroup whose coverpoint `a` has a bin for each value from input 0
/// to input 1, crossed with a bit, where the cross has a bin for the bins
/// of the value of input 0.
fn ranged() -> TypeDescriptor {
    let a = point(
        "a",
        point_type(32, true),
        vec![each(bins("b", vec![input_range(0, 1)]))],
    );
    let c = point("c", point_type(1, false), vec![]);
    let x = cross(
        "x",
        &[0, 1],
        vec![cross_bins(
            "sel",
            intersect(binsof(0), vec![RangeDescriptor::Value(Operand::Input(0))]),
        )],
    );
    let mut descriptor = crossing("gt", vec![a, c], vec![x]);
    descriptor.inputs = int_inputs(2);
    descriptor
}

#[test]
fn instances_of_different_bins_unite_their_crosses_by_name() {
    let mut heap = Heap::new();
    let key = register(&mut heap, ranged());
    // `sel` is `<b[0],*>` in the first instance and `<b[1],*>` in the
    // second, whose automatic bins are `<b[1],*>` and `<b[2],*>`.
    let first = construct(&mut heap, key, &[0, 1]);
    let second = construct(&mut heap, key, &[1, 2]);
    sample(&mut heap, first, &[1, 0]);
    sample(&mut heap, first, &[0, 1]);
    sample(&mut heap, second, &[1, 0]);
    sample(&mut heap, second, &[2, 1]);
    assert_eq!(
        names(&instance_item(&heap, 0, 2)),
        [("sel", 1), ("<b[1],auto[0]>", 1)]
    );
    assert_eq!(
        names(&instance_item(&heap, 1, 2)),
        [("sel", 1), ("<b[2],auto[1]>", 1)]
    );
    assert_eq!(inst_counts(&mut heap, first, 3), (2, 3));
    assert_eq!(inst_counts(&mut heap, second, 3), (2, 3));
    assert_close(type_coverage(&mut heap, key, 3), 200.0 / 3.0);
    assert_eq!(type_counts(&mut heap, key, 3), (4, 6));
    // The type has `sel` and the four automatic bins of `b[1]` and `b[2]`:
    // `<b[1],auto[0]>` is one of them though it is a cross product of
    // `sel` in the second instance.
    set_type_option(&mut heap, key, 0, TypeOptionId::MergeInstances, 1);
    assert_eq!(type_counts(&mut heap, key, 3), (3, 5));
    assert_close(type_coverage(&mut heap, key, 3), 60.0);
    // `a` has `b[0]` to `b[2]` and `c` its two bins, all hit.
    assert_close(
        type_coverage(&mut heap, key, 0),
        (100.0 + 100.0 + 60.0) / 3.0,
    );
    assert_eq!(type_counts(&mut heap, key, 0), (3 + 2 + 3, 3 + 2 + 5));
    let item = type_item(&heap, 2);
    assert_eq!(
        names(&item),
        [("sel", 2), ("<b[1],auto[0]>", 1), ("<b[2],auto[1]>", 1)]
    );
    assert_eq!((item.covered, item.total, item.omitted_bins), (3, 5, 2));
    // The type lists as many missing bins as the instance that asks for
    // most.
    set_option(&mut heap, second, 3, OptionId::CrossNumPrintMissing, 1);
    let item = type_item(&heap, 2);
    assert_eq!(
        names(&item),
        [
            ("sel", 2),
            ("<b[1],auto[0]>", 1),
            ("<b[2],auto[1]>", 1),
            ("<b[1],auto[1]>", 0),
        ]
    );
    assert_eq!(item.omitted_bins, 1);
    assert_eq!(
        item.bins.iter().map(|bin| bin.covered).collect::<Vec<_>>(),
        [true, true, true, false]
    );
    set_option(&mut heap, first, 0, OptionId::CrossNumPrintMissing, 9);
    let item = type_item(&heap, 2);
    assert_eq!(
        names(&item)[3..],
        [("<b[1],auto[1]>", 0), ("<b[2],auto[0]>", 0)]
    );
    assert_eq!(item.omitted_bins, 0);
    // The hits of a bin of two instances add up.
    sample(&mut heap, second, &[2, 1]);
    sample(&mut heap, first, &[1, 1]);
    let third = construct(&mut heap, key, &[2, 2]);
    sample(&mut heap, third, &[2, 1]);
    assert_eq!(names(&instance_item(&heap, 2, 2)), [("sel", 1)]);
    assert_eq!(
        names(&type_item(&heap, 2)),
        [
            ("sel", 3),
            ("<b[1],auto[0]>", 1),
            ("<b[1],auto[1]>", 1),
            ("<b[2],auto[1]>", 2),
            ("<b[2],auto[0]>", 0),
        ]
    );
    set_option(&mut heap, third, 3, OptionId::AtLeast, 2);
    assert_eq!(type_counts(&mut heap, key, 3), (2, 5));
    assert_eq!(heap.take_fault(), None);
}

/// A covergroup whose coverpoint `p` has a bin for each value from 0 to 5
/// that a filter keeps which does not share (`FilterDescriptor::shared`
/// clear), crossed with a bit: every construction has a shape of its own.
fn unshared() -> TypeDescriptor {
    let ty = point_type(3, false);
    let mut b = each(bins("b", vec![range(int(0), int(5))]));
    let BinValues::Ranges { filter, .. } = &mut b.values else {
        unreachable!()
    };
    *filter = Some(BinFilter::Enumerate(0));
    let p = point("p", ty.clone(), vec![b]);
    let q = point("q", point_type(1, false), vec![]);
    let mut descriptor = crossing("un", vec![p, q], vec![cross("x", &[0, 1], vec![])]);
    let mut filter = FilterDescriptor::new(vec![ValueType {
        width: ty.width,
        signed: ty.signed,
    }]);
    filter.shared = false;
    descriptor.filters = vec![filter];
    descriptor
}

/// A built instance of `unshared` whose filter keeps the values below
/// `limit`.
fn unshared_instance(heap: &mut Heap, key: u64, limit: u64) -> Handle {
    let object = call(heap, op::NEW, 0, 0, [key, 8, CLASS.into()]) as Handle;
    let mut kept = 0;
    while call(heap, op::FILTER_NEXT, 0, 0, [object.into(), kept, 0]) != 0 {
        let item = call(heap, op::FILTER_ITEM, 0, 0, [object.into(), 0, 0]);
        kept = (item < limit).into();
    }
    assert_eq!(call(heap, op::BUILD, 0, 0, [object.into(), 0, 0]), 0);
    assert_eq!(heap.take_fault(), None);
    object
}

/// The crosses of a type whose constructions share no shape (a filter that
/// is not `shared`): each instance has its cross of its own bins, and the
/// union of the type unites the automatic bins by name, also of instances
/// whose shapes are equal but not one.
#[test]
fn crosses_of_instances_that_share_no_shape_unite_by_name() {
    let mut heap = Heap::new();
    let key = register(&mut heap, unshared());
    let first = unshared_instance(&mut heap, key, 2);
    let second = unshared_instance(&mut heap, key, 4);
    let third = unshared_instance(&mut heap, key, 2);
    let shapes: Vec<Option<u32>> = heap.coverage.instances.iter().map(|i| i.shape).collect();
    assert_eq!(shapes, [Some(0), Some(1), Some(2)]);
    sample(&mut heap, first, &[0, 0]);
    sample(&mut heap, first, &[1, 1]);
    sample(&mut heap, second, &[0, 0]);
    sample(&mut heap, second, &[3, 1]);
    sample(&mut heap, third, &[1, 1]);
    // 2 of 4, 2 of 8 and 1 of 4 cross products.
    assert_eq!(inst_counts(&mut heap, first, 3), (2, 4));
    assert_eq!(inst_counts(&mut heap, second, 3), (2, 8));
    assert_eq!(inst_counts(&mut heap, third, 3), (1, 4));
    assert_close(type_coverage(&mut heap, key, 3), (50.0 + 25.0 + 25.0) / 3.0);
    assert_eq!(type_counts(&mut heap, key, 3), (5, 16));
    // As a union: `<b[0..3],auto[0..1]>`, of which three were hit.
    set_type_option(&mut heap, key, 0, TypeOptionId::MergeInstances, 1);
    assert_close(type_coverage(&mut heap, key, 3), 37.5);
    assert_eq!(type_counts(&mut heap, key, 3), (3, 8));
    let item = type_item(&heap, 2);
    assert_eq!(
        names(&item),
        [
            ("<b[0],auto[0]>", 2),
            ("<b[1],auto[1]>", 2),
            ("<b[3],auto[1]>", 1),
        ]
    );
    assert_eq!((item.covered, item.total, item.omitted_bins), (3, 8, 5));
    set_option(&mut heap, first, 3, OptionId::CrossNumPrintMissing, 2);
    let item = type_item(&heap, 2);
    assert_eq!(
        names(&item)[3..],
        [("<b[0],auto[1]>", 0), ("<b[1],auto[0]>", 0)]
    );
    assert_eq!(item.omitted_bins, 3);
    assert_eq!(heap.take_fault(), None);
    // The shapes and the hits survive a checkpoint.
    let bytes = postcard::to_allocvec(&heap).unwrap();
    let restored: Heap = postcard::from_bytes(&bytes).unwrap();
    assert!(restored.same_contents(&heap));
    assert_eq!(names(&type_item(&restored, 2)), names(&item));
}

/// A cross of `count` coverpoints that each have a bin for each value from
/// input 0 to input 1.
fn many_ranged(count: u32) -> TypeDescriptor {
    let points = (0..count)
        .map(|index| {
            point(
                &format!("p{index}"),
                point_type(32, true),
                vec![each(bins("b", vec![input_range(0, 1)]))],
            )
        })
        .collect();
    let all: Vec<u32> = (0..count).collect();
    let mut descriptor = crossing(
        &format!("many{count}"),
        points,
        vec![cross("x", &all, vec![])],
    );
    descriptor.inputs = int_inputs(2);
    descriptor
}

#[test]
fn a_union_of_instances_of_different_bins_is_counted_by_classes() {
    let mut heap = Heap::new();
    // Three coverpoints: each instance has 8 automatic bins, of which
    // `<b[1],b[1],b[1]>` is one of both.
    let small = register(&mut heap, many_ranged(3));
    for inputs in [[0, 1], [1, 2]] {
        construct(&mut heap, small, &inputs);
    }
    set_type_option(&mut heap, small, 0, TypeOptionId::MergeInstances, 1);
    assert_eq!(type_counts(&mut heap, small, 4), (0, 15));
    assert_eq!(heap.take_fault(), None);
    // Thirteen: the bins of each coverpoint are of three classes, in the
    // first instance, in the second and in both: 3^13 classes of cross
    // products, which a query looks at within its steps.
    let key = register(&mut heap, many_ranged(13));
    let first = construct(&mut heap, key, &[0, 1]);
    let second = construct(&mut heap, key, &[1, 2]);
    sample(&mut heap, first, &[1; 13]);
    sample(&mut heap, second, &[1; 13]);
    assert_eq!(inst_counts(&mut heap, first, 14), (1, 8192));
    assert_eq!(type_counts(&mut heap, key, 14), (2, 16384));
    assert_eq!(heap.take_fault(), None);
    set_type_option(&mut heap, key, 0, TypeOptionId::MergeInstances, 1);
    // The union: 2 * 2^13 - 1 bins, of which the one that both instances
    // sampled has 2 hits.
    assert_eq!(type_counts(&mut heap, key, 14), (1, 16383));
    assert_eq!(inst_counts(&mut heap, first, 14), (1, 16383));
    // An instance that tracks its own coverage asks for no union.
    set_option(&mut heap, first, 0, OptionId::GetInstCoverage, 1);
    assert_eq!(inst_counts(&mut heap, first, 14), (1, 8192));
    assert_eq!(heap.take_fault(), None);
    // The report has the same union.
    let report = heap.coverage_report();
    let group = report
        .covergroups
        .iter()
        .find(|group| group.name == "many13")
        .unwrap();
    let item = &group.items[13];
    assert_eq!((item.covered, item.total), (1, 16383));
    assert_eq!(item.bins.len(), 1);
    assert_eq!(item.bins[0].hits, 2);
    assert_eq!(item.omitted_bins, 16382);
    assert_eq!(
        item.bins[0].name,
        format!("<{}>", vec!["b[1]"; 13].join(","))
    );
}

#[test]
fn a_cross_without_bins_takes_no_part() {
    let bit = |name: &str| point(name, point_type(1, false), vec![]);
    // `off` has no automatic bins as it declares a bin, which is ignored:
    // no bin at all (Table 19-1, 19.11.2). `none` crosses a coverpoint
    // without a bin that counts.
    let mut only_default = point("d", point_type(1, false), vec![]);
    only_default.bins = vec![BinDescriptor::new("rest", BinValues::Default)];
    let mut descriptor = crossing(
        "zero",
        vec![bit("p"), bit("q"), only_default],
        vec![
            cross(
                "off",
                &[0, 1],
                vec![cross_ignore("ig", intersect(binsof(0), vec![number(1, 1)]))],
            ),
            cross("none", &[0, 2], vec![]),
            cross("on", &[0, 1], vec![]),
            // Bins that select all there is of nothing.
            cross(
                "void",
                &[0, 2],
                vec![
                    cross_bins("all", binsof(0)),
                    cross_ignore("some", not(binsof(1))),
                ],
            ),
        ],
    );
    descriptor.options = vec![
        option(4, OptionId::CrossRetainAutoBins, 0),
        option_input(5, OptionId::Weight, 0),
    ];
    descriptor.inputs = int_inputs(1);
    let mut heap = Heap::new();
    let key = register(&mut heap, descriptor);
    let object = construct(&mut heap, key, &[0]);
    let weighted = construct(&mut heap, key, &[3]);
    sample(&mut heap, object, &[1, 1, 0]);
    sample(&mut heap, object, &[0, 1, 0]);
    // a) to d) of 19.11.2: 0 with a weight, 100 without, and no bins.
    assert_close(inst_coverage(&mut heap, object, 4), 0.0);
    assert_eq!(inst_counts(&mut heap, object, 4), (0, 0));
    assert_close(inst_coverage(&mut heap, object, 5), 100.0);
    assert_close(inst_coverage(&mut heap, weighted, 5), 0.0);
    assert_eq!(inst_counts(&mut heap, object, 5), (0, 0));
    assert_close(type_coverage(&mut heap, key, 4), 0.0);
    assert_eq!(type_counts(&mut heap, key, 4), (0, 0));
    set_type_option(&mut heap, key, 4, TypeOptionId::Weight, 0);
    assert_close(type_coverage(&mut heap, key, 4), 100.0);
    // The covergroup is the average of `p`, `q` and `on`: `d` has no bin
    // either.
    assert_close(
        inst_coverage(&mut heap, object, 0),
        (50.0 + 100.0 + 50.0) / 3.0,
    );
    assert_eq!(inst_counts(&mut heap, object, 0), (1 + 2 + 2, 2 + 2 + 4));
    // The ignore bin still counts its samples.
    assert_eq!(names(&instance_item(&heap, 0, 3)), [("ig", 1)]);
    assert_eq!(instance_item(&heap, 0, 3).omitted_bins, 0);
    assert_eq!(names(&instance_item(&heap, 0, 4)), []);
    assert_eq!(inst_counts(&mut heap, object, 7), (0, 0));
    assert_eq!(names(&instance_item(&heap, 0, 6)), []);
    assert_eq!(names(&type_item(&heap, 6)), []);
    // The same for the union of the instances.
    set_type_option(&mut heap, key, 0, TypeOptionId::MergeInstances, 1);
    assert_eq!(type_counts(&mut heap, key, 7), (0, 0));
    assert_eq!(names(&type_item(&heap, 6)), []);
    assert_close(type_coverage(&mut heap, key, 4), 100.0);
    assert_close(type_coverage(&mut heap, key, 5), 0.0);
    assert_eq!(type_counts(&mut heap, key, 5), (0, 0));
    assert_close(
        type_coverage(&mut heap, key, 0),
        (50.0 + 100.0 + 50.0) / 3.0,
    );
    assert_eq!(heap.take_fault(), None);
}

/// A cross of two bits with a bin for the cross products of `p == 1`,
/// where input 0 is `option.cross_retain_auto_bins` of the covergroup.
fn retaining() -> TypeDescriptor {
    let bit = |name: &str| point(name, point_type(1, false), vec![]);
    let x = |name: &str| {
        cross(
            name,
            &[0, 1],
            vec![cross_bins("one", intersect(binsof(0), vec![number(1, 1)]))],
        )
    };
    let mut descriptor = crossing("keep", vec![bit("p"), bit("q")], vec![x("x"), x("own")]);
    descriptor.inputs = int_inputs(1);
    descriptor.options = vec![
        option_input(0, OptionId::CrossRetainAutoBins, 0),
        option(4, OptionId::CrossRetainAutoBins, 1),
    ];
    descriptor
}

#[test]
fn automatic_bins_are_retained_on_request() {
    let mut heap = Heap::new();
    let key = register(&mut heap, retaining());
    let kept = construct(&mut heap, key, &[1]);
    let dropped = construct(&mut heap, key, &[0]);
    // The option of the covergroup is the default of its crosses, and a
    // cross has its own (Table 19-2).
    for (object, group, cross) in [(kept, 1, 1), (dropped, 0, 0)] {
        assert_eq!(
            get_option(&mut heap, object, 0, OptionId::CrossRetainAutoBins),
            group
        );
        assert_eq!(
            get_option(&mut heap, object, 3, OptionId::CrossRetainAutoBins),
            cross
        );
        assert_eq!(
            get_option(&mut heap, object, 4, OptionId::CrossRetainAutoBins),
            1
        );
    }
    assert_eq!(inst_counts(&mut heap, kept, 3), (0, 3));
    assert_eq!(inst_counts(&mut heap, dropped, 3), (0, 1));
    assert_eq!(inst_counts(&mut heap, dropped, 4), (0, 3));
    for object in [kept, dropped] {
        sample(&mut heap, object, &[0, 1]);
        sample(&mut heap, object, &[1, 1]);
    }
    assert_eq!(
        names(&instance_item(&heap, 0, 2)),
        [("one", 1), ("<auto[0],auto[1]>", 1)]
    );
    // "all automatically generated bins are removed by the presence of any
    // explicit cross bin": the sample of `<auto[0],auto[1]>` counts
    // nowhere.
    assert_eq!(names(&instance_item(&heap, 1, 2)), [("one", 1)]);
    assert_eq!(instance_item(&heap, 1, 2).omitted_bins, 0);
    assert_eq!(
        names(&instance_item(&heap, 1, 3)),
        [("one", 1), ("<auto[0],auto[1]>", 1)]
    );
    assert_eq!(inst_counts(&mut heap, kept, 3), (2, 3));
    assert_eq!(inst_counts(&mut heap, dropped, 3), (1, 1));
    // The bins were made by the construction: assigning the option later
    // changes what is read back only.
    set_option(&mut heap, dropped, 3, OptionId::CrossRetainAutoBins, 1);
    assert_eq!(
        get_option(&mut heap, dropped, 3, OptionId::CrossRetainAutoBins),
        1
    );
    assert_eq!(inst_counts(&mut heap, dropped, 3), (1, 1));
    // The two constructions made bins of their own, and one more of each
    // takes them.
    for retain in [1, 0, 1] {
        construct(&mut heap, key, &[retain]);
    }
    assert_eq!(heap.coverage.types[0].shapes.len(), 2);
    // A cross without a bin of its own has its automatic bins whatever the
    // option is.
    let plain = register(&mut heap, {
        let mut descriptor = bits(2);
        descriptor.name = "plain".into();
        descriptor.inputs = int_inputs(1);
        descriptor.options = vec![option_input(3, OptionId::CrossRetainAutoBins, 0)];
        descriptor
    });
    for retain in [0, 1] {
        let object = construct(&mut heap, plain, &[retain]);
        assert_eq!(
            get_option(&mut heap, object, 3, OptionId::CrossRetainAutoBins),
            retain as u64
        );
        assert_eq!(inst_counts(&mut heap, object, 3), (0, 4));
    }
    // The union of the two: `one` and the two automatic bins of the first.
    set_type_option(&mut heap, key, 0, TypeOptionId::MergeInstances, 1);
    assert_eq!(type_counts(&mut heap, key, 3), (2, 3));
    assert_eq!(
        names(&type_item(&heap, 2)),
        [("one", 2), ("<auto[0],auto[1]>", 1)]
    );
    assert_eq!(type_item(&heap, 2).omitted_bins, 1);
    assert_eq!(heap.take_fault(), None);
}

#[test]
fn guards_of_a_cross_and_of_its_bins() {
    let bit = |name: &str| point(name, point_type(1, false), vec![]);
    let mut one = cross_bins("one", intersect(binsof(0), vec![number(1, 1)]));
    one.iff = Some(65);
    let mut x = cross("x", &[0, 1], vec![one]);
    x.iff = Some(0);
    x.predicates = 66;
    let mut guarded = bit("q");
    guarded.bins = vec![
        BinDescriptor {
            iff: Some(0),
            ..bins("lo", vec![number(1, 0)])
        },
        bins("hi", vec![number(1, 1)]),
    ];
    guarded.predicates = 1;
    let mut heap = Heap::new();
    let key = register(&mut heap, crossing("g", vec![bit("p"), guarded], vec![x]));
    let object = construct(&mut heap, key, &[]);
    let hits = |heap: &Heap| {
        let item = instance_item(heap, 0, 2);
        names(&item)
            .into_iter()
            .map(|(name, hits)| (name.to_string(), hits))
            .collect::<Vec<_>>()
    };
    let one = |hits: u64| ("one".to_string(), hits);
    // Without predicate words every guard holds.
    sample(&mut heap, object, &[1, 1]);
    assert_eq!(hits(&heap), [one(1)]);
    // "If at any sample point, the condition evaluates to false, the cross
    // coverage is ignored": the coverpoints still count.
    assert_eq!(sample_guarded(&mut heap, object, &[1, 1], 3, !1), 0);
    assert_eq!(sample_guarded(&mut heap, object, &[0, 1], 3, !1), 0);
    assert_eq!(hits(&heap), [one(1)]);
    assert_eq!(
        instance_bins(&heap, 0, 0),
        [("auto[0]".to_string(), 1), ("auto[1]".to_string(), 2)]
    );
    // "If the expression is false, the cross bin is ignored": the guard of
    // `one` is bit 1 of the second word.
    for (bits, counted) in [(0, 1), (2, 2), (1, 2)] {
        for (item, word, bits) in [(3, 0, u64::MAX), (3, 1, bits)] {
            call(
                &mut heap,
                op::PREDICATES,
                item,
                word,
                [object.into(), bits, 0],
            );
        }
        sample(&mut heap, object, &[1, 1]);
        assert_eq!(hits(&heap), [one(counted)]);
    }
    // A bin of a coverpoint whose guard does not hold does not count the
    // sample, so no cross product has it.
    assert_eq!(sample_guarded(&mut heap, object, &[0, 0], 2, 0), 0);
    assert_eq!(hits(&heap), [one(2)]);
    assert_eq!(sample_guarded(&mut heap, object, &[0, 0], 2, 1), 0);
    assert_eq!(hits(&heap), [one(2), ("<auto[0],lo>".to_string(), 1)]);
    // A coverpoint or cross that is stopped is not sampled, and a
    // coverpoint without a value leaves its crosses without one (19.8).
    call(&mut heap, op::COLLECT, 3, 0, [object.into(), 0, 0]);
    sample(&mut heap, object, &[0, 0]);
    call(&mut heap, op::COLLECT, 3, 1, [object.into(), 0, 0]);
    call(&mut heap, op::COLLECT, 1, 0, [object.into(), 0, 0]);
    sample(&mut heap, object, &[0, 0]);
    call(&mut heap, op::COLLECT, 1, 1, [object.into(), 0, 0]);
    sample_some(&mut heap, object, &[None, Some(0)]);
    assert_eq!(hits(&heap), [one(2), ("<auto[0],lo>".to_string(), 1)]);
    sample(&mut heap, object, &[0, 0]);
    assert_eq!(hits(&heap), [one(2), ("<auto[0],lo>".to_string(), 2)]);
    assert_eq!(heap.take_fault(), None);
    // The predicates of a cross have the words of its bits.
    call(&mut heap, op::PREDICATES, 3, 2, [object.into(), 0, 0]);
    call(&mut heap, op::SAMPLE_END, 0, 0, [object.into(), 0, 0]);
    assert_eq!(
        fault(&mut heap).as_deref(),
        Some("covergroup: type `g`: the predicates of item 3 have no word 2")
    );
    call(&mut heap, op::PREDICATES, 4, 0, [object.into(), 0, 0]);
    call(&mut heap, op::SAMPLE_END, 0, 0, [object.into(), 0, 0]);
    assert_eq!(
        fault(&mut heap).as_deref(),
        Some("covergroup: type `g`: item 4 is not a coverpoint or cross")
    );
}

#[test]
fn a_sample_in_two_bins_of_a_coverpoint_is_two_cross_products() {
    // 1 is a value of `low` and of `mid`.
    let p = point(
        "p",
        point_type(2, false),
        vec![
            bins("low", vec![span(2, 0, 1)]),
            bins("mid", vec![span(2, 1, 2)]),
        ],
    );
    let q = point("q", point_type(1, false), vec![]);
    let plain = cross("plain", &[0, 1], vec![]);
    let wide = cross(
        "wide",
        &[0, 1],
        vec![cross_bins("any", intersect(binsof(1), vec![number(1, 0)]))],
    );
    let mut heap = Heap::new();
    let key = register(&mut heap, crossing("o", vec![p, q], vec![plain, wide]));
    let object = construct(&mut heap, key, &[]);
    sample(&mut heap, object, &[1, 0]);
    sample(&mut heap, object, &[2, 1]);
    assert_eq!(
        names(&instance_item(&heap, 0, 2)),
        [
            ("<low,auto[0]>", 1),
            ("<mid,auto[0]>", 1),
            ("<mid,auto[1]>", 1)
        ]
    );
    // A declared bin counts a sample once, however many of its cross
    // products the sample is.
    assert_eq!(
        names(&instance_item(&heap, 0, 3)),
        [("any", 1), ("<mid,auto[1]>", 1)]
    );
    // The values of a pack are the same samples.
    call(&mut heap, op::POINTS, 1, 2, [object.into(), 0b0_01, 0]);
    assert_eq!(
        call(&mut heap, op::SAMPLE_END, 0, 0, [object.into(), 0, 0]),
        0
    );
    call(&mut heap, op::PRESENT, 1, 2, [object.into(), 0b01, 0]);
    call(&mut heap, op::POINTS, 1, 2, [object.into(), 0b0_01, 0]);
    assert_eq!(
        call(&mut heap, op::SAMPLE_END, 0, 0, [object.into(), 0, 0]),
        0
    );
    assert_eq!(
        names(&instance_item(&heap, 0, 2)),
        [
            ("<low,auto[0]>", 2),
            ("<mid,auto[0]>", 2),
            ("<mid,auto[1]>", 1)
        ]
    );
    assert_eq!(heap.take_fault(), None);
}

/// A cross of `count` coverpoints of one bit each.
fn bits(count: u32) -> TypeDescriptor {
    let points = (0..count)
        .map(|index| point(&format!("p{index}"), point_type(1, false), vec![]))
        .collect();
    let all: Vec<u32> = (0..count).collect();
    crossing("bits", points, vec![cross("x", &all, vec![])])
}

#[test]
fn a_cross_lists_the_bins_that_were_sampled_and_counts_the_others() {
    let mut heap = Heap::new();
    let key = register(&mut heap, bits(9));
    let object = construct(&mut heap, key, &[]);
    let size = heap.content_size();
    // The values of the nine coverpoints come in one word.
    for values in [0b0_0000_0000, 0b1_0000_0101, 0b1_1111_1111, 0b1_0000_0101] {
        call(&mut heap, op::POINTS, 1, 9, [object.into(), values, 0]);
        assert_eq!(
            call(&mut heap, op::SAMPLE_END, 0, 0, [object.into(), 0, 0]),
            0
        );
    }
    assert_eq!(inst_counts(&mut heap, object, 10), (3, 512));
    assert_close(inst_coverage(&mut heap, object, 10), 300.0 / 512.0);
    // An instance keeps a count for each of the three bins it sampled.
    assert_eq!(heap.content_size(), size + 3 * 24);
    let item = instance_item(&heap, 0, 9);
    let ones = |bits: u32| {
        let names: Vec<String> = (0..9)
            .map(|bit| format!("auto[{}]", bits >> bit & 1))
            .collect();
        format!("<{}>", names.join(","))
    };
    // The first coverpoint counts most in the order of the bins.
    assert_eq!(
        names(&item),
        [
            (ones(0).as_str(), 1),
            (ones(0b1_0000_0101).as_str(), 2),
            (ones(0b1_1111_1111).as_str(), 1),
        ]
    );
    assert_eq!(item.omitted_bins, 509);
    assert_eq!(item.coverpoints.len(), 9);
    // Five of the others on request.
    set_option(&mut heap, object, 0, OptionId::CrossNumPrintMissing, 5);
    let item = instance_item(&heap, 0, 9);
    assert_eq!(item.bins.len(), 8);
    assert_eq!(item.omitted_bins, 504);
    assert_eq!(
        names(&item)[3..],
        [
            (ones(0b1_0000_0000).as_str(), 0),
            (ones(0b0_1000_0000).as_str(), 0),
            (ones(0b1_1000_0000).as_str(), 0),
            (ones(0b0_0100_0000).as_str(), 0),
            (ones(0b1_0100_0000).as_str(), 0),
        ]
    );
    assert!(item.bins[3..].iter().all(|bin| !bin.covered));
    // The type lists what its instances ask for.
    let item = type_item(&heap, 9);
    assert_eq!((item.bins.len(), item.omitted_bins), (8, 504));
    assert_eq!(heap.take_fault(), None);
}

#[test]
fn the_cross_products_of_a_large_cross_are_never_listed() {
    // Coverpoints of 64 automatic bins each.
    let large = |count: u32| {
        let points = (0..count)
            .map(|index| point(&format!("p{index}"), point_type(8, false), vec![]))
            .collect();
        let all: Vec<u32> = (0..count).collect();
        let mut x = cross("x", &all, vec![]);
        // A bin of its own takes the cross products of one bin of `p0`.
        x.bins = vec![cross_bins(
            "first",
            intersect(binsof(0), vec![number(8, 0)]),
        )];
        crossing(&format!("large{count}"), points, vec![x])
    };
    let mut heap = Heap::new();
    // 64^6 = 2^36 cross products, of which `first` has 2^30.
    let key = register(&mut heap, large(6));
    let object = construct(&mut heap, key, &[]);
    set_option(&mut heap, object, 7, OptionId::CrossNumPrintMissing, 2);
    sample(&mut heap, object, &[255, 0, 4, 8, 12, 16]);
    sample(&mut heap, object, &[3, 0, 4, 8, 12, 16]);
    let total = (1u64 << 36) - (1 << 30) + 1;
    let item = instance_item(&heap, 0, 6);
    assert_eq!((item.covered, item.total), (2, total));
    assert_close(item.coverage, 200.0 / total as f64);
    assert_eq!(
        names(&item),
        [
            ("first", 1),
            (
                "<auto[252:255],auto[0:3],auto[4:7],auto[8:11],auto[12:15],auto[16:19]>",
                1
            ),
            (
                "<auto[4:7],auto[0:3],auto[0:3],auto[0:3],auto[0:3],auto[0:3]>",
                0
            ),
            (
                "<auto[4:7],auto[0:3],auto[0:3],auto[0:3],auto[0:3],auto[4:7]>",
                0
            ),
        ]
    );
    assert_eq!(item.omitted_bins, total - 1 - 3);
    // The two-argument form gives an `int` (19.8).
    assert_eq!(inst_counts(&mut heap, object, 7), (2, i32::MAX as u64));
    // 64^11 = 2^66: the report has the largest number it can.
    let key = register(&mut heap, large(11));
    let object = construct(&mut heap, key, &[]);
    sample(&mut heap, object, &[255; 11]);
    let item = instance_item(&heap, 0, 11);
    assert_eq!((item.covered, item.total), (1, u64::MAX));
    assert_eq!(item.omitted_bins, u64::MAX);
    let expected = 100.0 / ((1u128 << 66) - (1 << 60) + 1) as f64;
    assert!((item.coverage - expected).abs() < expected * 1e-9);
    assert_eq!(heap.take_fault(), None);
    // 64^22 = 2^132 cross products have no key.
    let key = register(&mut heap, large(22));
    let object = call(&mut heap, op::NEW, 0, 0, [key, 8, CLASS.into()]);
    call(&mut heap, op::BUILD, 0, 0, [object, 0, 0]);
    assert_eq!(
        fault(&mut heap).as_deref(),
        Some("covergroup: type `large22`: cross `x` has more than 2^128 cross products")
    );
}

#[test]
fn too_many_classes_of_cross_products_fail_the_build() {
    // Each of the 1025 bins of each coverpoint is a class of its own.
    let values = |name: &str| {
        point(
            name,
            point_type(11, false),
            vec![each(bins("v", vec![span(11, 0, 1024)]))],
        )
    };
    let mut x = cross("x", &[0, 1], vec![]);
    for position in 0..2 {
        for value in 0..1025 {
            x.bins.push(cross_bins(
                &format!("b{position}_{value}"),
                intersect(binsof(position), vec![number(11, value)]),
            ));
        }
    }
    let mut heap = Heap::new();
    let key = register(
        &mut heap,
        crossing("fine", vec![values("p"), values("q")], vec![x]),
    );
    let object = call(&mut heap, op::NEW, 0, 0, [key, 8, CLASS.into()]);
    call(&mut heap, op::BUILD, 0, 0, [object, 0, 0]);
    assert_eq!(
        fault(&mut heap).as_deref(),
        Some(
            "covergroup: type `fine`: the bins of cross `x` tell more than 1048576 classes of \
             cross products apart"
        )
    );
}

#[test]
fn intersect_selects_the_bins_that_hold_a_value_of_the_list() {
    // `auto[0:1]` to `auto[6:7]`, of which the ignored values leave
    // `auto[2:3]`, `auto[4:5]` with 4 and `auto[6:7]` with 7 (19.11.1).
    let auto = point(
        "p",
        point_type(3, false),
        vec![ignored(bins("ig", vec![span(3, 0, 1), span(3, 5, 6)]))],
    );
    // `m` has 2 and 3 once the ignored values are taken.
    let listed = point(
        "q",
        point_type(4, false),
        vec![
            bins("m", vec![span(4, 2, 5)]),
            ignored(bins("i", vec![span(4, 4, 5)])),
            each(bins("v", vec![span(4, 8, 10)])),
        ],
    );
    let select = |position: u32, ranges: Vec<RangeDescriptor>| intersect(binsof(position), ranges);
    let mut x = cross("x", &[0, 1], vec![]);
    x.bins = vec![
        // 5 is ignored: no bin holds it.
        cross_bins("five", select(0, vec![number(3, 5)])),
        cross_bins("four", select(0, vec![number(3, 4)])),
        cross_bins("upper", select(0, vec![span(3, 5, 7)])),
        // The ignored `auto[0:1]` is no bin of the cross.
        cross_bins("lower", select(0, vec![span(3, 0, 2)])),
        cross_bins("taken", select(1, vec![span(4, 4, 7)])),
        cross_bins("left", select(1, vec![span(4, 3, 4)])),
        // The bins of a declaration, and those of them that hold a value.
        cross_bins("array", binsof_bin(1, 2)),
        cross_bins("nine", intersect(binsof_bin(1, 2), vec![span(4, 9, 15)])),
        cross_bins(
            "not_nine",
            not(intersect(binsof_bin(1, 2), vec![span(4, 9, 15)])),
        ),
        // An ignore bin has no bin in a cross, and the automatic bins are
        // those of no declaration.
        cross_bins("ignored", binsof_bin(1, 1)),
        cross_bins("ignored_automatic", binsof_bin(0, 0)),
        cross_bins("not_ignored", not(binsof_bin(1, 1))),
        // `$` is an end of the type of the coverpoint.
        cross_bins(
            "open",
            select(
                1,
                vec![RangeDescriptor::Range {
                    low: Bound::Operand(Operand::Constant(sized(4, 10))),
                    high: Bound::Open,
                }],
            ),
        ),
    ];
    x.bins.extend([cross_bins(
        "outside",
        select(0, vec![value_of(int(8)), value_of(int(-1))]),
    )]);
    let mut descriptor = crossing("sel", vec![auto, listed], vec![x]);
    descriptor.options = vec![option(1, OptionId::AutoBinMax, 4)];
    let mut heap = Heap::new();
    let key = register(&mut heap, descriptor);
    let (object, warnings) = construct_with_warnings(&mut heap, key, &[]);
    // The values of an `intersect` are resolved as those of a bin
    // (19.5.7).
    assert_eq!(warnings, 2);
    assert_eq!(
        message(&mut heap, object),
        "bins `outside` of cross `x` of covergroup `sel` (instance `sel#0`): value 8 is not a \
         value of the coverpoint type and does not take part (IEEE 1800-2023 19.5.7); bins \
         `outside` of cross `x` of covergroup `sel` (instance `sel#0`): value -1 is not a value \
         of the coverpoint type and does not take part (IEEE 1800-2023 19.5.7)"
    );
    // The cross products are those of `auto[2:3]`, `auto[4:5]` and
    // `auto[6:7]` with `m`, `v[8]`, `v[9]` and `v[10]`. A sample of each
    // shows the bins it is in; `not_ignored` has all of them.
    let p = [("auto[2:3]", 2), ("auto[4:5]", 4), ("auto[6:7]", 7)];
    let q = [("m", 2), ("v[8]", 8), ("v[9]", 9), ("v[10]", 10)];
    let mut found: Vec<(String, Vec<String>)> = Vec::new();
    for (p_name, p_value) in p {
        for (q_name, q_value) in q {
            let before = instance_item(&heap, 0, 2);
            sample(&mut heap, object, &[p_value, q_value]);
            let after = instance_item(&heap, 0, 2);
            let hit = after
                .bins
                .iter()
                .filter(|bin| {
                    before
                        .bins
                        .iter()
                        .find(|other| other.name == bin.name)
                        .is_none_or(|other| other.hits != bin.hits)
                })
                .map(|bin| bin.name.clone())
                .filter(|name| name != "not_ignored")
                .collect();
            found.push((format!("<{p_name},{q_name}>"), hit));
        }
    }
    let expected = [
        ("<auto[2:3],m>", vec!["lower", "left", "not_nine"]),
        ("<auto[2:3],v[8]>", vec!["lower", "array", "not_nine"]),
        ("<auto[2:3],v[9]>", vec!["lower", "array", "nine"]),
        ("<auto[2:3],v[10]>", vec!["lower", "array", "nine", "open"]),
        ("<auto[4:5],m>", vec!["four", "left", "not_nine"]),
        ("<auto[4:5],v[8]>", vec!["four", "array", "not_nine"]),
        ("<auto[4:5],v[9]>", vec!["four", "array", "nine"]),
        ("<auto[4:5],v[10]>", vec!["four", "array", "nine", "open"]),
        ("<auto[6:7],m>", vec!["upper", "left", "not_nine"]),
        ("<auto[6:7],v[8]>", vec!["upper", "array", "not_nine"]),
        ("<auto[6:7],v[9]>", vec!["upper", "array", "nine"]),
        ("<auto[6:7],v[10]>", vec!["upper", "array", "nine", "open"]),
    ];
    assert_eq!(
        found
            .iter()
            .map(|(name, bins)| (
                name.as_str(),
                bins.iter().map(String::as_str).collect::<Vec<_>>()
            ))
            .collect::<Vec<_>>(),
        expected
    );
    // `five`, `taken`, `ignored`, `ignored_automatic` and `outside` have no
    // cross product.
    // Every cross product is in `not_ignored`, so the cross has no
    // automatic bin.
    let item = instance_item(&heap, 0, 2);
    assert_eq!(
        names(&item),
        [
            ("four", 4),
            ("upper", 4),
            ("lower", 4),
            ("left", 3),
            ("array", 9),
            ("nine", 6),
            ("not_nine", 6),
            ("not_ignored", 12),
            ("open", 3),
        ]
    );
    assert_eq!((item.covered, item.total, item.omitted_bins), (9, 9, 0));
    assert_eq!(heap.take_fault(), None);
}

#[test]
fn the_examples_of_excluded_and_illegal_cross_products() {
    // `yy` of 19.6.2: the cross products of four of the 16 bins of `a`
    // are ignored.
    let nibble = |name: &str| point(name, point_type(4, false), vec![]);
    let yy = cross(
        "yy",
        &[0, 1],
        vec![cross_ignore(
            "ignore",
            intersect(binsof(0), vec![value_of(int(5)), range(int(1), int(3))]),
        )],
    );
    // `zz` of 19.6.3, whose value is an argument of the covergroup.
    let zz = cross(
        "zz",
        &[0, 1],
        vec![cross_illegal(
            "illegal",
            intersect(binsof(1), vec![RangeDescriptor::Value(Operand::Input(0))]),
        )],
    );
    let mut descriptor = crossing("ex", vec![nibble("a"), nibble("b")], vec![yy, zz]);
    descriptor.inputs = int_inputs(1);
    let mut heap = Heap::new();
    let key = register(&mut heap, descriptor);
    let object = construct(&mut heap, key, &[7]);
    assert_eq!(inst_counts(&mut heap, object, 3), (0, 256 - 4 * 16));
    assert_eq!(inst_counts(&mut heap, object, 4), (0, 256 - 16));
    for a in [0, 1, 2, 3, 4, 5, 6] {
        assert_eq!(sample_illegal(&mut heap, object, &[a, 0]), 0);
    }
    assert_eq!(inst_counts(&mut heap, object, 3), (3, 192));
    assert_eq!(names(&instance_item(&heap, 0, 2))[0], ("ignore", 4));
    assert_eq!(sample_illegal(&mut heap, object, &[3, 7]), 1);
    assert_eq!(
        message(&mut heap, object),
        "illegal_bins `illegal` of cross `zz` of covergroup `ex` (instance `ex#0`): \
         <auto[3],auto[7]>"
    );
    assert_eq!(inst_counts(&mut heap, object, 4), (7, 240));
    // Another instance has another illegal value.
    let other = construct(&mut heap, key, &[0]);
    assert_eq!(sample_illegal(&mut heap, other, &[3, 7]), 0);
    assert_eq!(sample_illegal(&mut heap, other, &[3, 0]), 1);
    assert_eq!(heap.take_fault(), None);
    // The cross of 19.5.1 that keeps one cross product: the negation of a
    // `binsof` is the other bins of its coverpoint.
    let three = |name: &str| {
        point(
            name,
            point_type(2, false),
            vec![
                bins("special", vec![number(2, 3)]),
                each(bins("other", vec![span(2, 0, 2)])),
            ],
        )
    };
    let ab = cross(
        "ab_cross",
        &[0, 1],
        vec![
            cross_bins(
                "az_and_bx",
                Select::And(vec![binsof_bin(0, 0), binsof_bin(1, 0)]),
            ),
            cross_ignore(
                "others",
                Select::Or(vec![not(binsof_bin(0, 0)), not(binsof_bin(1, 0))]),
            ),
        ],
    );
    let key = register(
        &mut heap,
        crossing("real", vec![three("cvp_a"), three("cvp_b")], vec![ab]),
    );
    let object = construct(&mut heap, key, &[]);
    assert_eq!(inst_counts(&mut heap, object, 3), (0, 1));
    for (a, b) in [(3, 0), (0, 3), (1, 2), (3, 3)] {
        sample(&mut heap, object, &[a, b]);
    }
    let report = heap.coverage_report();
    let item = &report.covergroups[1].instances[0].items[2];
    assert_eq!(names(item), [("az_and_bx", 1), ("others", 3)]);
    assert_eq!((item.covered, item.total, item.omitted_bins), (1, 1, 0));
}

#[test]
fn intersect_meets_the_values_of_automatic_bins() {
    // `auto[4:5]` holds 5 only.
    let p = point(
        "p",
        point_type(3, false),
        vec![ignored(bins("ig", vec![number(3, 4)]))],
    );
    // 64 bins of four values from -128 on, and 64 bins of 2^94 values.
    let s = point("s", point_type(8, true), vec![]);
    let w = point("w", point_type(100, false), vec![]);
    let select = |position: u32, ranges: Vec<RangeDescriptor>| intersect(binsof(position), ranges);
    let wide = |value: u128| constant(101, false, value as i128);
    let x = cross(
        "x",
        &[0, 1, 2],
        vec![
            cross_bins("three_four", select(0, vec![span(3, 3, 4)])),
            cross_bins("four", select(0, vec![number(3, 4)])),
            cross_bins("four_five", select(0, vec![span(3, 4, 5)])),
            cross_bins("around_zero", select(1, vec![range(int(-1), int(0))])),
            cross_bins("lowest", select(1, vec![value_of(int(-128))])),
            cross_bins(
                "wide",
                select(2, vec![range(wide((1 << 94) - 1), wide(1 << 94))]),
            ),
            cross_bins("wide_top", select(2, vec![value_of(wide((1 << 100) - 1))])),
        ],
    );
    let mut descriptor = crossing("auto", vec![p, s, w], vec![x]);
    descriptor.options = vec![option(1, OptionId::AutoBinMax, 4)];
    let mut heap = Heap::new();
    let key = register(&mut heap, descriptor);
    let object = construct(&mut heap, key, &[]);
    // 4 * 64 * 64 cross products, of which 2^12 are of `auto[2:3]`, as
    // many of `auto[4:5]`, 2 * 2 * 64 more of the two bins around zero, 2 *
    // 64 more of the lowest, and of the 2 * 61 * 64 that are left 2 * 61 *
    // 2 are of the two wide bins and 2 * 61 of the last.
    let declared = 2 * 4096 + 2 * 2 * 64 + 2 * 64 + 2 * 61 * 2 + 2 * 61;
    assert_eq!(
        inst_counts(&mut heap, object, 4),
        (0, 4 * 64 * 64 - declared + 6)
    );
    // The bins of `x` that a sample counts in.
    let mut hits = |p: u64, s: i8, w: u128| {
        let before = instance_item(&heap, 0, 3);
        call(&mut heap, op::POINT, 1, 0, [object.into(), p, 0]);
        call(
            &mut heap,
            op::POINT,
            2,
            0,
            [object.into(), s as u8 as u64, 0],
        );
        call(&mut heap, op::POINT, 3, 0, [object.into(), w as u64, 0]);
        call(
            &mut heap,
            op::POINT,
            3,
            1,
            [object.into(), (w >> 64) as u64, 0],
        );
        assert_eq!(
            call(&mut heap, op::SAMPLE_END, 0, 0, [object.into(), 0, 0]),
            0
        );
        let after = instance_item(&heap, 0, 3);
        after
            .bins
            .iter()
            .filter(|bin| {
                before
                    .bins
                    .iter()
                    .find(|other| other.name == bin.name)
                    .is_none_or(|other| other.hits != bin.hits)
            })
            .map(|bin| bin.name.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(hits(3, -1, 0), ["three_four", "around_zero", "wide"]);
    assert_eq!(hits(5, 0, 1 << 94), ["four_five", "around_zero", "wide"]);
    assert_eq!(hits(5, 4, 1 << 95), ["four_five"]);
    assert_eq!(
        hits(2, -128, (1 << 100) - 1),
        ["three_four", "lowest", "wide_top"]
    );
    assert_eq!(hits(6, -4, (1 << 95) - 1), ["around_zero", "wide"]);
    assert_eq!(hits(0, -124, (1 << 100) - (1 << 94)), ["wide_top"]);
    // A cross product in none of them is an automatic bin.
    assert_eq!(
        hits(6, -5, (1 << 95) + 5),
        [format!(
            "<auto[6:7],auto[-8:-5],auto[{}:{}]>",
            1u128 << 95,
            (3u128 << 94) - 1
        )]
    );
    // No bin holds 4.
    let item = instance_item(&heap, 0, 3);
    assert!(item.bins.iter().all(|bin| bin.name != "four"));
    assert_eq!(item.bins.len(), 6 + 1);
    assert_eq!(heap.take_fault(), None);
}

#[test]
fn intersect_sees_values_with_x_or_z_and_wildcard_patterns() {
    // A bin that holds exactly `1x00`, and a wildcard bin of the odd
    // values of 20 bits, which are more ranges than a bin lists.
    let exact = Constant {
        ty: ValueType {
            width: 4,
            signed: false,
        },
        value: BigUint::from(0b1100u8),
        mask: BigUint::from(0b0100u8),
    };
    let p = point(
        "p",
        PointType {
            four_state: true,
            ..point_type(4, false)
        },
        vec![
            bins("unknown", vec![value_of(exact.clone())]),
            bins("known", vec![span(4, 0, 15)]),
        ],
    );
    let odd = Constant {
        ty: ValueType {
            width: 20,
            signed: false,
        },
        value: BigUint::from(1u8),
        mask: BigUint::from((1u32 << 20) - 2),
    };
    let high = Constant {
        ty: ValueType {
            width: 20,
            signed: false,
        },
        value: BigUint::from(1u32 << 19 | 1),
        mask: BigUint::from((1u32 << 19) - 2),
    };
    let wild = |name: &str, value: Constant| BinDescriptor {
        wildcard: true,
        ..bins(name, vec![value_of(value)])
    };
    let q = point(
        "q",
        point_type(20, false),
        vec![
            wild("odd", odd),
            // The odd values from 2^19 on, less the ignored ones.
            wild("high", high),
            ignored(bins("ig", vec![span(20, 1 << 19, (1 << 19) + 8)])),
            bins("even", vec![number(20, 2)]),
        ],
    );
    let select = |position: u32, ranges: Vec<RangeDescriptor>| intersect(binsof(position), ranges);
    let x = cross(
        "x",
        &[0, 1],
        vec![
            cross_bins("x_value", select(0, vec![value_of(exact)])),
            cross_bins("two", select(1, vec![number(20, 2)])),
            cross_bins("two_three", select(1, vec![span(20, 2, 3)])),
            cross_bins(
                "top",
                select(1, vec![span(20, (1 << 19) - 1, (1 << 19) + 9)]),
            ),
            cross_bins("gone", select(1, vec![span(20, 1 << 19, (1 << 19) + 8)])),
            cross_bins("last", select(1, vec![number(20, (1 << 20) - 1)])),
            // `high` alone: the bins but `odd` that hold 2^19 + 9, and none
            // of them holds a value from 2^19 - 1 to 2^19 + 8.
            cross_bins(
                "only_high",
                Select::And(vec![
                    select(1, vec![number(20, (1 << 19) + 9)]),
                    not(binsof_bin(1, 0)),
                ]),
            ),
            cross_bins(
                "below_high",
                Select::And(vec![
                    select(1, vec![span(20, (1 << 19) - 1, (1 << 19) + 8)]),
                    not(binsof_bin(1, 0)),
                ]),
            ),
        ],
    );
    let mut heap = Heap::new();
    let key = register(&mut heap, crossing("w", vec![p, q], vec![x]));
    let (object, warnings) = construct_with_warnings(&mut heap, key, &[]);
    // The two wildcard values that are kept as patterns.
    assert_eq!(warnings, 2, "{}", message(&mut heap, object));
    // The bins of `q` each cross product is in, by the bins of `x` that a
    // sample of it with `known` counts in.
    let mut hits = |q: u64| {
        let before = instance_item(&heap, 0, 2);
        sample(&mut heap, object, &[0, q]);
        let after = instance_item(&heap, 0, 2);
        after
            .bins
            .iter()
            .filter(|bin| {
                before
                    .bins
                    .iter()
                    .find(|other| other.name == bin.name)
                    .is_none_or(|other| other.hits != bin.hits)
            })
            .map(|bin| bin.name.clone())
            .collect::<Vec<_>>()
    };
    // `even` holds 2; `odd` holds 3, 2^19 - 1, 2^19 + 9 and the last
    // value; `high` holds 2^19 + 9 and the last value, but none of 2^19
    // to 2^19 + 8.
    assert_eq!(hits(2), ["two", "two_three"]);
    assert_eq!(hits(5), ["two_three", "top", "last"]);
    // A value of `odd` and of `high` is a sample of two cross products.
    assert_eq!(
        hits((1 << 19) + 11),
        ["two_three", "top", "last", "only_high"]
    );
    // An ignored value is one of neither.
    assert_eq!(hits((1 << 19) + 7), [] as [&str; 0]);
    assert_eq!(heap.take_fault(), None);
    // `gone` and `below_high` have no cross product.
    let item = instance_item(&heap, 0, 2);
    assert_eq!(
        item.bins
            .iter()
            .map(|bin| bin.name.as_str())
            .collect::<Vec<_>>(),
        ["x_value", "two", "two_three", "top", "last", "only_high"]
    );
    // The value with an x bit is only in `unknown`.
    call(&mut heap, op::POINT, 1, 0, [object.into(), 0b1100, 0b0100]);
    call(&mut heap, op::POINT, 2, 0, [object.into(), 2, 0]);
    assert_eq!(
        call(&mut heap, op::SAMPLE_END, 0, 0, [object.into(), 0, 0]),
        0
    );
    let item = instance_item(&heap, 0, 2);
    assert_eq!(
        names(&item)[..3],
        [("x_value", 1), ("two", 2), ("two_three", 4)]
    );
}

#[test]
fn intersect_finds_the_values_of_a_wildcard_pattern() {
    // A wildcard value of 20 bits with bits 19, 7 and 0 given, which is
    // 2^17 ranges of values and so kept as a pattern, in a bin of a
    // coverpoint of either signedness. A cross bin for each of many ranges
    // has a cross product exactly when the range holds a value of it.
    const CARE: u32 = 1 << 19 | 1 << 7 | 1;
    for (signed, given) in [
        (false, 1 << 19 | 1),
        (true, 1 << 19 | 1),
        (true, 1 << 7),
        (false, 1 << 7),
    ] {
        let wild = Constant {
            ty: ValueType {
                width: 20,
                signed: false,
            },
            value: BigUint::from(given),
            mask: BigUint::from(((1u32 << 20) - 1) & !CARE),
        };
        let q = point(
            "q",
            point_type(20, signed),
            vec![BinDescriptor {
                wildcard: true,
                ..bins("w", vec![value_of(wild)])
            }],
        );
        let p = point("p", point_type(1, false), vec![]);
        // The value of the bits `bits` in the type of the coverpoint.
        let numeric = |bits: u32| {
            if signed && bits >> 19 != 0 {
                i64::from(bits) - (1 << 20)
            } else {
                i64::from(bits)
            }
        };
        let (lowest, highest) = if signed {
            (-(1i64 << 19), (1i64 << 19) - 1)
        } else {
            (0, (1i64 << 20) - 1)
        };
        let mut state = 0x2545_f491u32;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state
        };
        let mut ranges = Vec::new();
        for _ in 0..300 {
            let low = lowest + i64::from(next() % (1 << 20));
            let length = match next() % 4 {
                0 => 0,
                1 => i64::from(next() % 4),
                2 => i64::from(next() % 200),
                _ => i64::from(next() % (1 << 19)),
            };
            ranges.push((low, (low + length).min(highest)));
        }
        ranges.extend([(lowest, lowest), (highest, highest), (lowest, highest)]);
        if !signed {
            // From the last value below bit 19 on: the next value with
            // bit 0 clear has bit 19 set.
            ranges.extend([(0x7_ffff, 0x8_0100), (0x7_ff7f, 0x7_ff80)]);
        }
        let mut x = cross("x", &[0, 1], vec![]);
        for (index, &(low, high)) in ranges.iter().enumerate() {
            x.bins.push(cross_bins(
                &format!("r{index}"),
                intersect(
                    binsof(1),
                    vec![range(
                        constant(21, true, low.into()),
                        constant(21, true, high.into()),
                    )],
                ),
            ));
        }
        let mut heap = Heap::new();
        let key = register(&mut heap, crossing("pattern", vec![p, q], vec![x]));
        construct(&mut heap, key, &[]);
        // The values of the wildcard value, in order.
        let mut values: Vec<i64> = (0..1u32 << 20)
            .filter(|bits| bits & CARE == given)
            .map(numeric)
            .collect();
        values.sort_unstable();
        let expected: Vec<String> = ranges
            .iter()
            .enumerate()
            .filter(|&(_, &(low, high))| {
                values
                    .get(values.partition_point(|&value| value < low))
                    .is_some_and(|&value| value <= high)
            })
            .map(|(index, _)| format!("r{index}"))
            .collect();
        // Some hold a value and some do not.
        assert!(expected.len() > 50 && expected.len() < ranges.len() - 50);
        let item = instance_item(&heap, 0, 2);
        assert_eq!(
            item.bins
                .iter()
                .map(|bin| bin.name.clone())
                .collect::<Vec<_>>(),
            expected,
            "signed {signed}, given {given:#x}"
        );
    }
}

#[test]
fn crosses_have_the_options_of_a_cross() {
    let mut descriptor = automatic_example();
    descriptor.crosses[0].type_options = ItemTypeOptions {
        weight: 3,
        goal: 80,
        comment: "crossed".into(),
    };
    descriptor.options = vec![
        option(0, OptionId::AtLeast, 2),
        option(0, OptionId::CrossNumPrintMissing, 7),
        option(3, OptionId::Weight, 2),
        option(3, OptionId::Goal, 60),
        OptionInit {
            item: 3,
            option: OptionId::Comment,
            value: OptionValue::String("a cross".into()),
        },
    ];
    let mut heap = Heap::new();
    let key = register(&mut heap, descriptor);
    let object = construct(&mut heap, key, &[]);
    // The covergroup's are the default of `at_least`,
    // `cross_num_print_missing` and `cross_retain_auto_bins` (Table 19-2).
    for (option, value) in [
        (OptionId::Weight, 2),
        (OptionId::Goal, 60),
        (OptionId::AtLeast, 2),
        (OptionId::CrossNumPrintMissing, 7),
        (OptionId::CrossRetainAutoBins, 1),
    ] {
        assert_eq!(
            get_option(&mut heap, object, 3, option),
            value,
            "{option:?}"
        );
    }
    let comment = get_option(&mut heap, object, 3, OptionId::Comment);
    assert_eq!(heap.string(comment as Handle), b"a cross");
    set_option(&mut heap, object, 3, OptionId::CrossNumPrintMissing, 1);
    set_option(&mut heap, object, 0, OptionId::CrossNumPrintMissing, 4);
    set_option(&mut heap, object, 0, OptionId::AtLeast, 1);
    assert_eq!(
        get_option(&mut heap, object, 3, OptionId::CrossNumPrintMissing),
        1
    );
    assert_eq!(
        get_option(&mut heap, object, 0, OptionId::CrossNumPrintMissing),
        4
    );
    assert_eq!(get_option(&mut heap, object, 3, OptionId::AtLeast), 1);
    assert_eq!(heap.take_fault(), None);
    // A weight of 2 beside the two coverpoints.
    for value in 0..8 {
        sample(&mut heap, object, &[value, 0]);
    }
    assert_close(inst_coverage(&mut heap, object, 3), 50.0);
    assert_close(
        inst_coverage(&mut heap, object, 0),
        (100.0 + 50.0 + 2.0 * 50.0) / 4.0,
    );
    set_option(&mut heap, object, 3, OptionId::Weight, 0);
    assert_close(inst_coverage(&mut heap, object, 0), 75.0);
    assert_close(inst_coverage(&mut heap, object, 3), 50.0);
    // The type options of a cross.
    for (option, value) in [(TypeOptionId::Weight, 3), (TypeOptionId::Goal, 80)] {
        assert_eq!(get_type_option(&mut heap, key, 3, option), value);
    }
    let comment = get_type_option(&mut heap, key, 3, TypeOptionId::Comment);
    assert_eq!(heap.string(comment as Handle), b"crossed");
    set_type_option(&mut heap, key, 0, TypeOptionId::MergeInstances, 1);
    assert_close(
        type_coverage(&mut heap, key, 0),
        (100.0 + 50.0 + 3.0 * 50.0) / 5.0,
    );
    set_type_option(&mut heap, key, 3, TypeOptionId::Weight, 0);
    assert_close(type_coverage(&mut heap, key, 0), 75.0);
    let item = type_item(&heap, 2);
    assert_eq!(
        (item.comment.as_str(), item.weight, item.goal, item.at_least),
        ("crossed", 0, 80, 1)
    );
    let item = instance_item(&heap, 0, 2);
    assert_eq!(
        (item.comment.as_str(), item.weight, item.goal, item.at_least),
        ("a cross", 0, 60, 1)
    );
    assert_eq!((item.bins.len(), item.omitted_bins), (8 + 1, 7));
    assert_eq!(heap.take_fault(), None);
    // What a cross and a coverpoint do not have (19.10).
    for (item, option) in [
        (3, OptionId::AutoBinMax),
        (3, OptionId::DetectOverlap),
        (3, OptionId::Name),
        (3, OptionId::PerInstance),
        (1, OptionId::CrossNumPrintMissing),
        (2, OptionId::CrossRetainAutoBins),
        (4, OptionId::Weight),
    ] {
        let reason = format!(
            "covergroup: type `Tr::cg`: item {item} has no option `{}`",
            option.name()
        );
        set_option(&mut heap, object, item, option, 1);
        assert_eq!(fault(&mut heap), Some(reason.clone()));
        get_option(&mut heap, object, item, option);
        assert_eq!(fault(&mut heap), Some(reason));
    }
    for (item, option) in [(3, TypeOptionId::MergeInstances), (4, TypeOptionId::Goal)] {
        set_type_option(&mut heap, key, item, option, 1);
        assert_eq!(
            fault(&mut heap),
            Some(format!(
                "covergroup: type `Tr::cg`: item {item} has no type option `{}`",
                option.name()
            ))
        );
    }
    inst_coverage(&mut heap, object, 4);
    assert_eq!(
        fault(&mut heap).as_deref(),
        Some("covergroup: type `Tr::cg`: no item 4")
    );
    set_option(
        &mut heap,
        object,
        3,
        OptionId::Weight,
        (-1i32) as u32 as u64,
    );
    assert_eq!(
        fault(&mut heap).as_deref(),
        Some("covergroup: type `Tr::cg`: the negative weight -1")
    );
}

#[test]
fn a_wrong_cross_descriptor_is_refused() {
    let bit = |name: &str| point(name, point_type(1, false), vec![]);
    let with = |change: &dyn Fn(&mut TypeDescriptor)| {
        let mut descriptor = crossing(
            "bad",
            vec![
                bit("p"),
                point(
                    "q",
                    point_type(1, false),
                    vec![bins("only", vec![number(1, 0)])],
                ),
            ],
            vec![cross("x", &[0, 1], vec![cross_bins("b", binsof(0))])],
        );
        change(&mut descriptor);
        let mut heap = Heap::new();
        heap.register_cover_type(1, Arc::new(descriptor));
        fault(&mut heap)
    };
    let refused = |reason: &str| Some(format!("covergroup: descriptor of type `bad`: {reason}"));
    assert_eq!(with(&|_| {}), None);
    assert_eq!(
        with(&|descriptor| descriptor.crosses[0].points = vec![0]),
        refused("cross `x` of fewer than two coverpoints")
    );
    assert_eq!(
        with(&|descriptor| descriptor.crosses[0].points = vec![0, 2]),
        refused("cross `x` of coverpoint 2, which is none")
    );
    assert_eq!(
        with(&|descriptor| descriptor.crosses[0].type_options.weight = -1),
        refused("a negative type weight of `x`")
    );
    assert_eq!(
        with(&|descriptor| descriptor.crosses[0].iff = Some(0)),
        refused("cross `x` has no predicate bit 0")
    );
    assert_eq!(
        with(&|descriptor| descriptor.crosses[0].bins[0].iff = Some(3)),
        refused("cross `x` has no predicate bit 3")
    );
    assert_eq!(
        with(&|descriptor| descriptor.crosses[0].bins[0].select = binsof(2)),
        refused("bin `b` of cross `x` selects bins of cross item 2, which is none")
    );
    assert_eq!(
        with(&|descriptor| descriptor.crosses[0].bins[0].select = binsof_bin(1, 1)),
        refused("bin `b` of cross `x` selects bin 1 of coverpoint `q`, which is none")
    );
    assert_eq!(
        with(&|descriptor| descriptor.crosses[0].bins[0].select = binsof_bin(0, 0)),
        refused("bin `b` of cross `x` selects bin 0 of coverpoint `p`, which is none")
    );
    assert_eq!(
        with(&|descriptor| {
            descriptor.crosses[0].bins[0].select =
                intersect(binsof(0), vec![RangeDescriptor::Value(Operand::Input(0))])
        }),
        refused("input 0 is not an integral input")
    );
    assert_eq!(
        with(&|descriptor| descriptor.crosses[0].bins[0].select = Select::Or(Vec::new())),
        refused("bin `b` of cross `x` combines no selections")
    );
    // `&&` and `||` nest as deep as `MAX_SELECT_DEPTH`.
    let nested = |depth: usize| {
        let mut select = binsof(0);
        for level in 0..depth {
            select = if level % 2 == 0 {
                Select::And(vec![select, binsof(1)])
            } else {
                Select::Or(vec![binsof(1), select])
            };
        }
        select
    };
    assert_eq!(
        with(&|descriptor| descriptor.crosses[0].bins[0].select = nested(MAX_SELECT_DEPTH)),
        None
    );
    assert_eq!(
        with(&|descriptor| descriptor.crosses[0].bins[0].select = nested(MAX_SELECT_DEPTH + 1)),
        refused("bin `b` of cross `x` has a selection nested deeper than 32")
    );
    // The options of the definition are those of the level of their item.
    assert_eq!(
        with(&|descriptor| descriptor.options = vec![option(1, OptionId::CrossRetainAutoBins, 0)]),
        refused("item 1 has no option `cross_retain_auto_bins`")
    );
    assert_eq!(
        with(&|descriptor| descriptor.options = vec![option(3, OptionId::AutoBinMax, 4)]),
        refused("item 3 has no option `auto_bin_max`")
    );
    assert_eq!(
        with(&|descriptor| descriptor.options = vec![option(4, OptionId::Weight, 1)]),
        refused("an option of item 4, which it has not")
    );
    assert_eq!(
        with(&|descriptor| descriptor.options = vec![option(3, OptionId::CrossNumPrintMissing, 4)]),
        None
    );
}

#[test]
fn a_deeply_nested_selection_selects() {
    // ((((p && q) || q) && q) || …) is `p && q` or `q` at every depth: the
    // cross products of `q == 1` here, as `q` stands for that.
    let bit = |name: &str| point(name, point_type(1, false), vec![]);
    let one = |position: u32| intersect(binsof(position), vec![number(1, 1)]);
    let mut select = one(0);
    for level in 0..MAX_SELECT_DEPTH {
        select = if level % 2 == 0 {
            Select::And(vec![select, one(1)])
        } else {
            Select::Or(vec![one(1), select])
        };
    }
    let x = cross("x", &[0, 1], vec![cross_bins("deep", select)]);
    let descriptor = crossing("deep", vec![bit("p"), bit("q")], vec![x]);
    // A descriptor of that depth travels as JSON.
    let json = serde_json::to_string(&descriptor).unwrap();
    assert_eq!(
        serde_json::from_str::<TypeDescriptor>(&json).unwrap(),
        descriptor
    );
    let mut heap = Heap::new();
    let key = register(&mut heap, descriptor);
    let object = construct(&mut heap, key, &[]);
    assert_eq!(inst_counts(&mut heap, object, 3), (0, 3));
    for (p, q) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
        sample(&mut heap, object, &[p, q]);
    }
    assert_eq!(
        names(&instance_item(&heap, 0, 2)),
        [
            ("deep", 2),
            ("<auto[0],auto[0]>", 1),
            ("<auto[1],auto[0]>", 1)
        ]
    );
}

#[test]
fn crosses_survive_a_round_trip() {
    let mut heap = Heap::new();
    let key = register(&mut heap, reference_select());
    let ranged_key = register(&mut heap, ranged());
    let object = construct(&mut heap, key, &[]);
    let first = construct(&mut heap, ranged_key, &[0, 1]);
    let second = construct(&mut heap, ranged_key, &[1, 2]);
    sample(&mut heap, object, &[3, 0]);
    sample(&mut heap, object, &[0, 0]);
    sample(&mut heap, first, &[1, 0]);
    sample(&mut heap, second, &[2, 1]);
    set_type_option(&mut heap, ranged_key, 0, TypeOptionId::MergeInstances, 1);
    // A checkpoint file is the postcard encoding of the heap.
    let mut restored: Heap = postcard::from_bytes(&postcard::to_allocvec(&heap).unwrap()).unwrap();
    assert!(restored.same_contents(&heap));
    assert_eq!(restored.coverage_report(), heap.coverage_report());
    // The hits of an automatic bin are contents of a heap.
    let checkpoint = heap.clone();
    for heap in [&mut heap, &mut restored] {
        sample(heap, object, &[3, 0]);
        sample(heap, object, &[6, 1]);
        sample(heap, first, &[1, 1]);
        let again = construct(heap, ranged_key, &[0, 1]);
        sample(heap, again, &[1, 1]);
    }
    assert!(restored.same_contents(&heap));
    assert!(!checkpoint.same_contents(&heap));
    assert_eq!(restored.coverage_report(), heap.coverage_report());
    assert_eq!(
        names(&instance_item(&restored, 0, 2)),
        [
            ("sel", 1),
            ("ig", 0),
            ("il", 0),
            ("<f[1],auto[0]>", 2),
            ("<f[2],auto[1]>", 1)
        ]
    );
    assert_eq!(type_counts(&mut restored, ranged_key, 3), (3, 5));
    let mut other = heap.clone();
    sample(&mut other, object, &[6, 1]);
    assert!(!other.same_contents(&heap));
    // The descriptor travels as JSON, and so does the report.
    let descriptor = select_example();
    let json = serde_json::to_string(&descriptor).unwrap();
    assert_eq!(
        serde_json::from_str::<TypeDescriptor>(&json).unwrap(),
        descriptor
    );
    let mut heap = Heap::new();
    let key = type_key("cg");
    for chunk in json.as_bytes().chunks(8) {
        let mut word = [0; 8];
        word[8 - chunk.len()..].copy_from_slice(chunk);
        let word = u64::from_be_bytes(word);
        call(
            &mut heap,
            op::REGISTER_BYTES,
            chunk.len() as u64,
            0,
            [key, word, 0],
        );
    }
    call(&mut heap, op::REGISTER, 0, 0, [key, 0, 0]);
    assert_eq!(heap.take_fault(), None);
    let object = construct(&mut heap, key, &[]);
    assert_eq!(inst_counts(&mut heap, object, 3), (0, 9));
    let report = heap.coverage_report();
    let json = serde_json::to_string(&report).unwrap();
    assert_eq!(
        serde_json::from_str::<CoverageReport>(&json).unwrap(),
        report
    );
    assert!(json.contains(r#""kind":"cross","coverpoints":["a","b"]"#));
}

#[test]
fn an_instance_that_is_not_built_has_no_cross_bins() {
    let mut heap = Heap::new();
    let key = register(&mut heap, ct());
    // A type without an instance takes no part.
    assert_close(type_coverage(&mut heap, key, 3), 0.0);
    assert_eq!(type_counts(&mut heap, key, 4), (0, 0));
    assert_close(overall(&mut heap), 100.0);
    let object = call(&mut heap, op::NEW, 0, 0, [key, 8, CLASS.into()]) as Handle;
    assert_close(inst_coverage(&mut heap, object, 3), 0.0);
    assert_eq!(inst_counts(&mut heap, object, 4), (0, 0));
    assert_eq!(names(&instance_item(&heap, 0, 3)), []);
    assert_eq!(names(&type_item(&heap, 3)), []);
    set_type_option(&mut heap, key, 0, TypeOptionId::MergeInstances, 1);
    assert_eq!(type_counts(&mut heap, key, 4), (0, 0));
    assert_eq!(type_item(&heap, 2).omitted_bins, 0);
    assert_eq!(heap.take_fault(), None);
    // Once it is built it has, and the type with it.
    assert_eq!(call(&mut heap, op::BUILD, 0, 0, [object.into(), 0, 0]), 0);
    assert_eq!(type_counts(&mut heap, key, 4), (0, 3));
    assert_eq!(names(&type_item(&heap, 3)).len(), 3);
    assert_close(overall(&mut heap), 0.0);
}

/// The covergroup `cg(int lo, int hi)` whose coverpoints `a` and `c` each
/// have a bin for every value from `lo` to `hi`, crossed.
fn sliding() -> TypeDescriptor {
    let ranged = |name: &str| {
        point(
            name,
            point_type(32, true),
            vec![each(bins("b", vec![input_range(0, 1)]))],
        )
    };
    let mut descriptor = crossing(
        "sliding",
        vec![ranged("a"), ranged("c")],
        vec![cross("x", &[0, 1], vec![])],
    );
    descriptor.inputs = int_inputs(2);
    descriptor
}

/// The automatic bins of `sliding` united over instances `new(lo, lo + 9)`
/// for `lo` below `n`: the pairs of values from 0 to `n + 8` that one such
/// range holds both of.
fn sliding_union(n: i64) -> u64 {
    let mut pairs = 0;
    for a in 0..n + 9 {
        for c in 0..n + 9 {
            let (low, high) = ((a.max(c) - 9).max(0), a.min(c).min(n - 1));
            pairs += u64::from(low <= high);
        }
    }
    pairs
}

/// How often the queries of the first type of `heap` looked for a union of
/// automatic cross bins.
fn unions_found(heap: &Heap) -> u64 {
    heap.coverage.types[0].unions.found
}

/// The report of `heap`, with how often it looked for a union of automatic
/// cross bins.
fn counted_report(heap: &Heap) -> (CoverageReport, u64) {
    let mut found = 0;
    let report = report::report(&heap.coverage, &mut found);
    (report, found)
}

/// With `type_option.merge_instances`, the automatic bins of a cross over
/// instances of different bins are united once for each set of shapes the
/// type has: queries take what the type keeps, a report finds it once for
/// the type and all instances, and an instance with a shape of its own
/// makes the next query find it again.
#[test]
fn the_union_of_a_merged_cross_is_found_once_for_its_shapes() {
    let mut heap = Heap::new();
    let key = register(&mut heap, sliding());
    let objects: Vec<Handle> = (0..200)
        .map(|low| construct(&mut heap, key, &[low, low + 9]))
        .collect();
    for (value, &object) in objects.iter().enumerate() {
        sample(&mut heap, object, &[value as u64, value as u64]);
    }
    set_type_option(&mut heap, key, 0, TypeOptionId::MergeInstances, 1);
    assert_eq!(sliding_union(200), 3881);
    // The report finds the union once, for the type and for the instances,
    // which take the coverage of the type.
    let (report, found) = counted_report(&heap);
    assert_eq!(found, 1);
    let group = &report.covergroups[0];
    assert_eq!((group.items[2].covered, group.items[2].total), (200, 3881));
    assert_eq!(group.instances.len(), 200);
    for instance in &group.instances {
        let item = &instance.items[2];
        assert_eq!((item.covered, item.total), (200, 3881));
        assert_eq!(instance.covered, group.covered);
    }
    assert_eq!(unions_found(&heap), 0);
    // The first query finds it and the type keeps it.
    assert_eq!(type_counts(&mut heap, key, 3), (200, 3881));
    assert_eq!(unions_found(&heap), 1);
    assert_eq!(inst_counts(&mut heap, objects[7], 3), (200, 3881));
    // `a` and `c` have the 209 bins of the values 0 to 208 each.
    assert_eq!(type_counts(&mut heap, key, 0), (600, 3881 + 2 * 209));
    let overall = f64::from_bits(call(&mut heap, op::OVERALL, 0, 0, [0, 0, 0]));
    assert_close(overall, type_coverage(&mut heap, key, 0));
    assert_eq!(counted_report(&heap).1, 0);
    assert_eq!(unions_found(&heap), 1);
    // An instance of the bins of an earlier one has its shape.
    let again = construct(&mut heap, key, &[0, 9]);
    sample(&mut heap, again, &[3, 5]);
    assert_eq!(type_counts(&mut heap, key, 3), (201, 3881));
    assert_eq!(unions_found(&heap), 1);
    // One of other bins adds a shape.
    let other = construct(&mut heap, key, &[200, 209]);
    sample(&mut heap, other, &[200, 200]);
    assert_eq!(type_counts(&mut heap, key, 3), (202, sliding_union(201)));
    assert_eq!(unions_found(&heap), 2);
    assert_eq!(counted_report(&heap).1, 0);
    assert_eq!(heap.take_fault(), None);
    // A checkpoint keeps no union: the restored heap is the same and finds
    // it at its first query.
    let mut restored: Heap = postcard::from_bytes(&postcard::to_allocvec(&heap).unwrap()).unwrap();
    assert!(restored.same_contents(&heap));
    assert_eq!(restored.coverage, heap.coverage);
    assert_eq!(
        type_counts(&mut restored, key, 3),
        (202, sliding_union(201))
    );
    assert_eq!(unions_found(&restored), 1);
}

/// With `type_option.merge_instances`, a query whose union of automatic
/// cross bins takes more than [`MAX_CROSS_WORK`] steps is a fault, which
/// the type keeps for its shapes: 14 coverpoints with a bin for each value
/// from input 0 to input 1, and two instances `new(0, 1)` and `new(1, 2)`,
/// whose bins of each coverpoint are of three classes.
#[test]
fn a_union_that_takes_too_many_steps_is_a_fault_of_the_query() {
    let mut heap = Heap::new();
    let key = register(&mut heap, many_ranged(14));
    let first = construct(&mut heap, key, &[0, 1]);
    let second = construct(&mut heap, key, &[1, 2]);
    sample(&mut heap, first, &[1; 14]);
    set_type_option(&mut heap, key, 0, TypeOptionId::MergeInstances, 1);
    let reason = "covergroup: type `many14`: uniting the automatic bins of cross `x` over the \
                  instances (type_option.merge_instances) takes more than 67108864 steps";
    assert_eq!(type_counts(&mut heap, key, 15), (0, 0));
    assert_eq!(fault(&mut heap).as_deref(), Some(reason));
    // The type keeps what keeps the union from being found.
    assert_close(inst_coverage(&mut heap, second, 15), 0.0);
    assert_eq!(fault(&mut heap).as_deref(), Some(reason));
    assert_eq!(call(&mut heap, op::OVERALL, 0, 0, [0, 0, 0]), 0);
    assert_eq!(fault(&mut heap).as_deref(), Some(reason));
    assert_eq!(unions_found(&heap), 1);
    // Without the union there is no fault.
    set_option(&mut heap, first, 0, OptionId::GetInstCoverage, 1);
    assert_eq!(inst_counts(&mut heap, first, 15), (1, 1 << 14));
    set_type_option(&mut heap, key, 0, TypeOptionId::MergeInstances, 0);
    assert_eq!(type_counts(&mut heap, key, 15), (1, 2 << 14));
    assert_eq!(heap.take_fault(), None);
}

/// The coverpoint `a` of 20 bits with `bins b = {[0:3]}` and the
/// `wildcard ignore_bins odd` of every odd value, which is matched as a
/// pattern, or with `b` filtered at each sample; crossed with a bit by
/// `x { bins one = binsof(a) intersect {value}; }`.
fn sorted_by_samples(pattern: bool, value: u64) -> TypeDescriptor {
    let mut b = bins("b", vec![range(sized(20, 0), sized(20, 3))]);
    let mut a = point("a", point_type(20, false), vec![]);
    if pattern {
        let odd = Constant {
            ty: ValueType {
                width: 20,
                signed: false,
            },
            value: BigUint::from(1u8),
            mask: BigUint::from((1u32 << 20) - 2),
        };
        let mut ignore = ignored(bins("odd", vec![value_of(odd)]));
        ignore.wildcard = true;
        a.bins = vec![b, ignore];
    } else {
        let BinValues::Ranges { filter, .. } = &mut b.values else {
            unreachable!()
        };
        *filter = Some(BinFilter::Predicate(0));
        a.bins = vec![b];
        a.predicates = 1;
    }
    let c = point("c", point_type(1, false), vec![]);
    let x = cross(
        "x",
        &[0, 1],
        vec![cross_bins(
            "one",
            intersect(binsof(0), vec![number(20, value)]),
        )],
    );
    crossing("sorted", vec![a, c], vec![x])
}

/// `intersect` of values that only a sample tells are in a bin or not: an
/// ignore pattern that a build cannot list, and a filter that a sample
/// evaluates. Where the range list meets such a value of a bin that the
/// term selects, the build is a fault; where it does not, the cross is
/// built.
#[test]
fn intersect_with_values_only_a_sample_sorts_is_a_fault_of_the_build() {
    for pattern in [true, false] {
        let mut heap = Heap::new();
        let key = register(&mut heap, sorted_by_samples(pattern, 1));
        let object = call(&mut heap, op::NEW, 0, 0, [key, 8, CLASS.into()]);
        call(&mut heap, op::BUILD, 0, 0, [object, 0, 0]);
        assert_eq!(
            fault(&mut heap).as_deref(),
            Some(
                "covergroup: type `sorted`: covergroup cross `intersect` of bins `one` of cross \
                 `x` meets values of coverpoint `a` that only a sample sorts into bins: those of \
                 an ignore or illegal wildcard value of more than 65536 ranges, or of a bin \
                 whose `with` filter a sample evaluates (IEEE 1800-2023 19.6.1)"
            ),
            "pattern {pattern}"
        );
        // A value that no pattern takes and that no bin holds unfiltered.
        let value = if pattern { 2 } else { 7 << 4 };
        let mut heap = Heap::new();
        let key = register(&mut heap, sorted_by_samples(pattern, value));
        let (object, _) = construct_with_warnings(&mut heap, key, &[]);
        let selected = if pattern { 1 } else { 0 };
        assert_eq!(
            instance_item(&heap, 0, 2).bins.len(),
            selected,
            "pattern {pattern}"
        );
        assert_eq!(
            inst_counts(&mut heap, object, 3).1,
            if pattern { 1 } else { 2 }
        );
        assert_eq!(heap.take_fault(), None);
    }
}

/// The probe of the build cost: two coverpoints of 512 one-value bins and
/// 500 bins `d_i = binsof(p) intersect {i} && binsof(q) intersect {i}`.
/// Each tuple of classes looks at the one bin its class of `p` may hold,
/// not at all 500: evaluating every bin for every one of the 501^2 tuples
/// would take more than [`MAX_CROSS_WORK`] steps.
#[test]
fn a_build_looks_at_the_declared_bins_a_class_may_hold() {
    let values = |name: &str| {
        point(
            name,
            point_type(10, false),
            vec![each(bins("v", vec![span(10, 0, 511)]))],
        )
    };
    let mut x = cross("x", &[0, 1], vec![]);
    for value in 0..500 {
        x.bins.push(cross_bins(
            &format!("d{value}"),
            Select::And(vec![
                intersect(binsof(0), vec![number(10, value)]),
                intersect(binsof(1), vec![number(10, value)]),
            ]),
        ));
    }
    const { assert!(501u64 * 501 * 500 * 3 > MAX_CROSS_WORK) };
    let mut heap = Heap::new();
    let key = register(
        &mut heap,
        crossing("guarded", vec![values("p"), values("q")], vec![x]),
    );
    let object = construct(&mut heap, key, &[]);
    // 512^2 - 500 automatic bins and the 500 declared ones.
    assert_eq!(inst_counts(&mut heap, object, 3), (0, 512 * 512));
    sample(&mut heap, object, &[3, 3]);
    sample(&mut heap, object, &[3, 4]);
    let item = instance_item(&heap, 0, 2);
    assert_eq!(item.bins[3].name, "d3");
    assert_eq!(item.bins[3].hits, 1);
    assert_eq!(item.bins[500].name, "<v[3],v[4]>");
    assert_eq!(inst_counts(&mut heap, object, 3), (2, 512 * 512));
    assert_eq!(heap.take_fault(), None);
}

/// A build that takes more than [`MAX_CROSS_WORK`] steps is a fault: bins
/// that every tuple of classes has to look at (`||` of terms of two
/// coverpoints), and terms of so many bins of a coverpoint.
#[test]
fn a_build_that_takes_too_many_steps_is_a_fault() {
    let reason = "covergroup: type `slow`: sorting the cross products of cross `x` into its \
                  bins takes more than 67108864 steps";
    let values = |name: &str, width: u32, count: u64| {
        point(
            name,
            point_type(width, false),
            vec![each(bins("v", vec![span(width, 0, count - 1)]))],
        )
    };
    let build = |descriptor: TypeDescriptor| {
        let mut heap = Heap::new();
        let key = register(&mut heap, descriptor);
        let object = call(&mut heap, op::NEW, 0, 0, [key, 8, CLASS.into()]);
        call(&mut heap, op::BUILD, 0, 0, [object, 0, 0]);
        fault(&mut heap)
    };
    let mut x = cross("x", &[0, 1], vec![]);
    for value in 0..300 {
        x.bins.push(cross_bins(
            &format!("e{value}"),
            Select::Or(vec![
                intersect(binsof(0), vec![number(9, value)]),
                intersect(binsof(1), vec![number(9, value)]),
            ]),
        ));
    }
    let points = vec![values("p", 9, 300), values("q", 9, 300)];
    assert_eq!(
        build(crossing("slow", points, vec![x])).as_deref(),
        Some(reason)
    );
    // 4097 terms of a coverpoint of 8192 bins.
    let mut x = cross("x", &[0, 1], vec![]);
    for value in 0..4097 {
        x.bins.push(cross_bins(
            &format!("t{value}"),
            intersect(binsof(0), vec![number(13, value)]),
        ));
    }
    let points = vec![
        values("p", 13, 8192),
        point("q", point_type(1, false), vec![]),
    ];
    assert_eq!(
        build(crossing("slow", points, vec![x])).as_deref(),
        Some(reason)
    );
}

/// The report lists at most [`MAX_CROSS_MISSING_ROWS`] uncovered automatic
/// bins of a cross whatever `option.cross_num_print_missing` asks for.
#[test]
fn the_missing_bins_a_report_lists_are_capped() {
    let six = |name: &str| point(name, point_type(6, false), vec![]);
    let mut heap = Heap::new();
    let key = register(
        &mut heap,
        crossing(
            "wide",
            vec![six("a"), six("b"), six("c")],
            vec![cross("x", &[0, 1, 2], vec![])],
        ),
    );
    let object = construct(&mut heap, key, &[]);
    sample(&mut heap, object, &[1, 2, 3]);
    set_option(
        &mut heap,
        object,
        4,
        OptionId::CrossNumPrintMissing,
        0x7fff_ffff,
    );
    assert_eq!(heap.take_fault(), None);
    let products = 64 * 64 * 64;
    for item in [instance_item(&heap, 0, 3), type_item(&heap, 3)] {
        assert_eq!(item.bins.len(), 1 + MAX_CROSS_MISSING_ROWS);
        assert_eq!(item.bins[0].hits, 1);
        assert_eq!(
            item.omitted_bins,
            products - 1 - MAX_CROSS_MISSING_ROWS as u64
        );
    }
}
