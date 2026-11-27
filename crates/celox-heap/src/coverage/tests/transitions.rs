//! Transition bins: their sequences, repetitions, arrays, `default
//! sequence`, the ignored and illegal sequences that remove matches, and
//! their part in coverage, crosses and checkpoints (IEEE 1800-2023 19.5.2,
//! 19.5.5, 19.5.6, 19.6.1.1, 19.11).

use super::*;

/// The values `values`, `int` literals.
fn ints(values: &[i64]) -> Vec<RangeDescriptor> {
    values.iter().map(|&number| value(int(number))).collect()
}

/// A step of the `int` values `values` without a repetition.
fn once(values: &[i64]) -> TransitionStep {
    TransitionStep::new(ints(values))
}

/// A step of the `int` values `values` repeated as `repeat` of the counts
/// `low` and `high`.
fn repeated(
    values: &[i64],
    repeat: fn(Operand, Operand) -> Repeat,
    low: i64,
    high: i64,
) -> TransitionStep {
    TransitionStep {
        values: ints(values),
        repeat: repeat(Operand::Constant(int(low)), Operand::Constant(int(high))),
    }
}

/// The sequence of one value per step, `4 => 5 => 6`.
fn chain(values: &[i64]) -> Vec<TransitionStep> {
    values.iter().map(|&number| once(&[number])).collect()
}

fn transitions(name: &str, sequences: Vec<Vec<TransitionStep>>) -> BinDescriptor {
    BinDescriptor::new(name, BinValues::Transitions(sequences))
}

fn default_sequence(name: &str) -> BinDescriptor {
    BinDescriptor::new(name, BinValues::DefaultSequence)
}

fn each(mut bin: BinDescriptor) -> BinDescriptor {
    bin.array = BinArray::Open;
    bin
}

fn with_kind(mut bin: BinDescriptor, kind: BinKind) -> BinDescriptor {
    bin.kind = kind;
    bin
}

fn guarded(mut bin: BinDescriptor, bit: u32) -> BinDescriptor {
    bin.iff = Some(bit);
    bin
}

/// A type of one coverpoint of `width` unsigned bits with the bins `bins`.
fn one_point(name: &str, width: u32, bins: Vec<BinDescriptor>) -> TypeDescriptor {
    group(name, vec![point("v", point_type(width, false), bins)])
}

/// The hits of the bin `name` of the coverpoint of instance `instance` as
/// the report lists it, or `None` where it lists no such bin.
fn hits_of(heap: &Heap, instance: usize, name: &str) -> Option<u64> {
    instance_bins(heap, instance, 0)
        .into_iter()
        .find(|(bin, _)| bin == name)
        .map(|(_, hits)| hits)
}

fn names(heap: &Heap, instance: usize) -> Vec<String> {
    instance_bins(heap, instance, 0)
        .into_iter()
        .map(|(name, _)| name)
        .collect()
}

/// Sample the value `value` of the only coverpoint: the number of illegal
/// bins hit.
fn sample_one(heap: &mut Heap, object: Handle, value: u64) -> u64 {
    call(heap, op::POINT, 1, 0, [object.into(), value, 0]);
    let hits = call(heap, op::SAMPLE_END, 0, 0, [object.into(), 0, 0]);
    assert_eq!(heap.take_fault(), None);
    hits
}

/// Sample `trace` and give, for each sample, which of the bins `bins` it
/// counted in.
fn counted_by_sample(
    heap: &mut Heap,
    object: Handle,
    trace: &[u64],
    bins: &[&str],
) -> Vec<Vec<String>> {
    let mut before: Vec<u64> = bins
        .iter()
        .map(|bin| hits_of(heap, 0, bin).unwrap_or(0))
        .collect();
    let mut counted = Vec::new();
    for &value in trace {
        assert_eq!(sample_one(heap, object, value), 0);
        let after: Vec<u64> = bins
            .iter()
            .map(|bin| hits_of(heap, 0, bin).unwrap_or(0))
            .collect();
        counted.push(
            bins.iter()
                .zip(before.iter().zip(&after))
                .filter(|(_, (before, after))| after > before)
                .map(|(bin, _)| bin.to_string())
                .collect(),
        );
        before = after;
    }
    counted
}

/// The fault that registering `descriptor` records, as a message shows it.
fn registration_fault(descriptor: TypeDescriptor) -> Option<String> {
    let mut heap = Heap::new();
    heap.register_cover_type(type_key(&descriptor.name), Arc::new(descriptor));
    heap.take_fault().map(|fault| fault.to_string())
}

/// The fault that building an instance of `descriptor` from the `int`
/// inputs `inputs` records.
fn build_fault(descriptor: TypeDescriptor, inputs: &[i64]) -> Option<String> {
    let mut heap = Heap::new();
    let key = register(&mut heap, descriptor);
    let object = call(&mut heap, op::NEW, 0, 0, [key, 8, CLASS.into()]);
    for (index, &input) in inputs.iter().enumerate() {
        let word = input as u32 as u64;
        call(&mut heap, op::INPUT, index as u64, 0, [object, word, 0]);
    }
    call(&mut heap, op::BUILD, 0, 0, [object, 0, 0]);
    heap.take_fault().map(|fault| fault.to_string())
}

/// The covergroup `cg` of 19.5.2 on `bit [4:1] v_a`.
fn sa_example() -> TypeDescriptor {
    let sequences = || {
        vec![
            chain(&[4, 5, 6]),
            vec![
                TransitionStep::new(vec![range(int(7), int(9)), value(int(10))]),
                once(&[11, 12]),
            ],
        ]
    };
    one_point(
        "cg",
        4,
        vec![
            transitions("sa", sequences()),
            each(transitions("sb", sequences())),
            transitions(
                "sc",
                vec![vec![once(&[12]), repeated(&[3], Repeat::Goto, 1, 1)]],
            ),
            default_sequence("allother"),
        ],
    )
}

#[test]
fn the_transition_bins_of_cg_in_19_5_2() {
    let mut heap = Heap::new();
    let key = register(&mut heap, sa_example());
    let object = construct(&mut heap, key, &[]);
    // "The second bins construct associates an individual bin with each of
    // the above sequences: sb[4=>5=>6], ..., sb[10=>12]."
    let sb = [
        "sb[4=>5=>6]",
        "sb[7=>11]",
        "sb[8=>11]",
        "sb[9=>11]",
        "sb[10=>11]",
        "sb[7=>12]",
        "sb[8=>12]",
        "sb[9=>12]",
        "sb[10=>12]",
    ];
    let mut listed = vec!["sa"];
    listed.extend(sb);
    listed.extend(["sc", "allother"]);
    assert_eq!(names(&heap, 0), text(&listed));
    let counted = counted_by_sample(
        &mut heap,
        object,
        &[4, 5, 7, 11, 8, 12, 2, 2, 3],
        &["sa", "sb[7=>11]", "sb[8=>12]", "sc", "allother"],
    );
    // "The bin allother increments twice": on 7, as 5=>7 ends the pending
    // 4=>5=>6, and on 8, as no bin increments on 11=>8 and nothing was
    // pending; not on 12=>2=>2, as sc stays pending.
    let expected: [&[&str]; 9] = [
        &[],
        &[],
        &["allother"],
        &["sa", "sb[7=>11]"],
        &["allother"],
        &["sa", "sb[8=>12]"],
        &[],
        &[],
        &["sc"],
    ];
    assert_eq!(
        counted,
        expected.iter().map(|bins| text(bins)).collect::<Vec<_>>()
    );
    // The bins of the coverage are sa, the nine of sb and sc.
    assert_eq!(inst_counts(&mut heap, object, 1), (4, 11));
    assert_close(inst_coverage(&mut heap, object, 1), 400.0 / 11.0);
    let report = heap.coverage_report();
    let bins = &report.covergroups[0].instances[0].items[0].bins;
    assert_eq!(bins.last().unwrap().kind, BinReportKind::Default);
    assert_eq!(bins[0].kind, BinReportKind::Bins);
}

fn text(names: &[&str]) -> Vec<String> {
    names.iter().map(|name| name.to_string()).collect()
}

#[test]
fn the_overlapping_transitions_of_sg_in_19_5_2() {
    let mut heap = Heap::new();
    use Repeat::{Consecutive, Goto, NonConsecutive};
    let descriptor = one_point(
        "sg",
        8,
        vec![
            transitions("b2", vec![vec![repeated(&[2], Goto, 3, 5)]]),
            transitions("b3", vec![vec![repeated(&[3], Goto, 3, 5)]]),
            transitions("b5", vec![vec![repeated(&[5], Consecutive, 3, 3)]]),
            transitions(
                "b6",
                vec![vec![once(&[1]), repeated(&[3], Goto, 4, 6), once(&[1])]],
            ),
            transitions(
                "b7",
                vec![vec![
                    once(&[1]),
                    repeated(&[2], NonConsecutive, 3, 6),
                    once(&[5]),
                ]],
            ),
        ],
    );
    let key = register(&mut heap, descriptor);
    let object = construct(&mut heap, key, &[]);
    let trace = [1, 4, 3, 2, 3, 3, 2, 2, 3, 2, 3, 1, 5, 5, 5, 5, 5, 5];
    let bins = ["b2", "b3", "b5", "b6", "b7"];
    let counted = counted_by_sample(&mut heap, object, &trace, &bins);
    // "b2 to be incremented on the 8th sample ... and transition bin b3 to be
    // incremented on the 6th sample. Likewise, transition bin b2 is
    // incremented on the 10th sample, and transition bin b3 is incremented
    // on the 9th and 11th samples. Transition bin b5 is incremented on the
    // 15th, 16th, 17th, and 18th samples. Transition bin b6 is incremented
    // on the 12th sample. Transition bin b7 is incremented on the 13th
    // sample."
    let mut expected = vec![Vec::new(); trace.len()];
    for (bin, samples) in [
        ("b2", &[8, 10][..]),
        ("b3", &[6, 9, 11]),
        ("b5", &[15, 16, 17, 18]),
        ("b6", &[12]),
        ("b7", &[13]),
    ] {
        for &sample in samples {
            expected[sample - 1].push(bin.to_string());
        }
    }
    assert_eq!(counted, expected);
    // "on the 10th sample, the transition bin b2 is incremented only once".
    assert_eq!(hits_of(&heap, 0, "b2"), Some(2));
}

#[test]
fn a_set_of_transitions_is_each_transition_of_its_values() {
    let mut heap = Heap::new();
    // `1,5 => 6, 7` "specifies the following four transitions: (1=>6),
    // (1=>7), (5=>6), (5=>7)".
    let sequence = || vec![vec![once(&[1, 5]), once(&[6, 7])]];
    let key = register(
        &mut heap,
        one_point(
            "sets",
            4,
            vec![
                transitions("any", sequence()),
                each(transitions("t", sequence())),
                transitions("long", vec![chain(&[1, 3, 4, 5])]),
            ],
        ),
    );
    let object = construct(&mut heap, key, &[]);
    for value in [1, 6, 5, 7, 1, 7, 6, 5, 1, 3, 1, 3, 4, 5] {
        assert_eq!(sample_one(&mut heap, object, value), 0);
    }
    assert_eq!(
        instance_bins(&heap, 0, 0),
        [
            ("any".to_string(), 3),
            ("t[1=>6]".to_string(), 1),
            ("t[5=>6]".to_string(), 0),
            ("t[1=>7]".to_string(), 1),
            ("t[5=>7]".to_string(), 1),
            // `value1 => value3 => value4 => value5`: the first 1=>3 is not
            // followed by 4.
            ("long".to_string(), 1),
        ]
    );
}

#[test]
fn consecutive_repetitions_are_the_sequences_they_stand_for() {
    let mut heap = Heap::new();
    use Repeat::Consecutive;
    let key = register(
        &mut heap,
        one_point(
            "consecutive",
            4,
            vec![
                // `3 [* 5]` "is the same as 3=>3=>3=>3=>3".
                transitions("five", vec![vec![repeated(&[3], Consecutive, 5, 5)]]),
                transitions("written", vec![chain(&[3, 3, 3, 3, 3])]),
                // `3 [* 3:5]` "is the same as (3=>3=>3), (3=>3=>3=>3),
                // (3=>3=>3=>3=>3)".
                transitions("range", vec![vec![repeated(&[3], Consecutive, 3, 5)]]),
                transitions(
                    "spelled",
                    vec![
                        chain(&[3, 3, 3]),
                        chain(&[3, 3, 3, 3]),
                        chain(&[3, 3, 3, 3, 3]),
                    ],
                ),
                // `b[] = (3[*3:5])` gives three bins (DESIGN 5.5).
                each(transitions(
                    "b",
                    vec![vec![repeated(&[3], Consecutive, 3, 5)]],
                )),
            ],
        ),
    );
    let object = construct(&mut heap, key, &[]);
    for value in [3, 3, 3, 3, 3, 3, 3, 1, 3, 3, 3] {
        assert_eq!(sample_one(&mut heap, object, value), 0);
    }
    // Seven 3s match five 3s three times, and one to three of them at each
    // of the samples from the third on, then once more after the 1.
    assert_eq!(
        instance_bins(&heap, 0, 0),
        [
            ("five".to_string(), 3),
            ("written".to_string(), 3),
            ("range".to_string(), 6),
            ("spelled".to_string(), 6),
            ("b[3=>3=>3]".to_string(), 6),
            ("b[3=>3=>3=>3]".to_string(), 4),
            ("b[3=>3=>3=>3=>3]".to_string(), 3),
        ]
    );
}

#[test]
fn goto_and_nonconsecutive_repetitions_as_in_19_5_2() {
    use Repeat::{Goto, NonConsecutive};
    let descriptor = one_point(
        "repetitions",
        4,
        vec![
            // `3 [-> 3]` "is the same as ...=>3...=>3...=>3".
            transitions("goto", vec![vec![repeated(&[3], Goto, 3, 3)]]),
            // `1 => 3 [-> 3] => 5` "is the same as 1...=>3...=>3...=>3 =>5".
            transitions(
                "goto_then",
                vec![vec![once(&[1]), repeated(&[3], Goto, 3, 3), once(&[5])]],
            ),
            // `3 [= 2]` "is the same as ...=>3...=>3...".
            transitions(
                "nonconsecutive",
                vec![vec![repeated(&[3], NonConsecutive, 2, 2)]],
            ),
            // `1 => 3 [=2] => 6` "is the same as 1...=>3...=>3...=>6".
            transitions(
                "nonconsecutive_then",
                vec![vec![
                    once(&[1]),
                    repeated(&[3], NonConsecutive, 2, 2),
                    once(&[6]),
                ]],
            ),
        ],
    );
    let run = |trace: &[u64]| {
        let mut heap = Heap::new();
        let key = register(&mut heap, descriptor.clone());
        let object = construct(&mut heap, key, &[]);
        let bins = ["goto", "goto_then", "nonconsecutive", "nonconsecutive_then"];
        counted_by_sample(&mut heap, object, trace, &bins)
    };
    let at = |samples: &[(usize, &[&str])], length: usize| {
        let mut expected = vec![Vec::new(); length];
        for &(sample, bins) in samples {
            expected[sample - 1] = text(bins);
        }
        expected
    };
    // The 3s are the 3rd, 5th and 6th samples: the goto matches with the
    // 6th, and the 5 follows it at once.
    assert_eq!(
        run(&[1, 2, 3, 2, 3, 3, 5]),
        at(
            &[
                (5, &["nonconsecutive"]),
                (6, &["goto", "nonconsecutive"]),
                (7, &["goto_then"]),
            ],
            7
        )
    );
    // The 5 does not follow the last 3 at once, and a third 3 ends `[=2]`.
    assert_eq!(
        run(&[1, 3, 2, 3, 3, 2, 5, 6]),
        at(
            &[(4, &["nonconsecutive"]), (5, &["goto", "nonconsecutive"]),],
            8
        )
    );
    // `[=2]` with a third 3 before the 6 does not match; two 3s and two 6s
    // match with the first 6 only. The goto takes the 3s of both runs.
    assert_eq!(
        run(&[1, 3, 3, 3, 6, 1, 3, 3, 6, 6]),
        at(
            &[
                (3, &["nonconsecutive"]),
                (4, &["goto", "nonconsecutive"]),
                (7, &["goto", "nonconsecutive"]),
                (8, &["goto", "nonconsecutive"]),
                (9, &["nonconsecutive_then"]),
            ],
            10
        )
    );
}

#[test]
fn transitions_of_length_0_and_unbounded_arrays_are_refused() {
    use Repeat::{Consecutive, NonConsecutive};
    let refused = |bin: BinDescriptor| registration_fault(one_point("refused", 4, vec![bin]));
    let length_0 = |fault: Option<String>| {
        fault.is_some_and(|fault| fault.contains("has a transition of length 0"))
    };
    // "(0) or ([0:1]), or ... (0[*1]) or ([0:1][*1])".
    assert!(length_0(refused(transitions("a", vec![vec![once(&[0])]]))));
    assert!(length_0(refused(transitions(
        "b",
        vec![vec![TransitionStep::new(vec![range(int(0), int(1))])]]
    ))));
    assert!(length_0(refused(transitions(
        "c",
        vec![vec![repeated(&[0], Consecutive, 1, 1)]]
    ))));
    assert!(length_0(refused(transitions(
        "d",
        vec![vec![TransitionStep {
            values: vec![range(int(0), int(1))],
            repeat: Repeat::Consecutive(Operand::Constant(int(1)), Operand::Constant(int(1))),
        }]]
    ))));
    // A count a construction hands over is checked when it builds.
    let mut counted = one_point(
        "counted",
        4,
        vec![transitions(
            "e",
            vec![vec![TransitionStep {
                values: ints(&[0]),
                repeat: Repeat::Consecutive(Operand::Input(0), Operand::Input(1)),
            }]],
        )],
    );
    counted.inputs = int_inputs(2);
    assert_eq!(build_fault(counted.clone(), &[2, 3]), None);
    assert!(
        build_fault(counted.clone(), &[1, 1])
            .is_some_and(|fault| fault.contains("has a transition of length 0"))
    );
    assert!(
        build_fault(counted.clone(), &[0, 3])
            .is_some_and(|fault| fault.contains("has the repetition count 0"))
    );
    assert!(
        build_fault(counted, &[3, 2])
            .is_some_and(|fault| fault.contains("has the repetition range 3:2"))
    );
    // "An attempt to specify multiple bins with such sequences shall result
    // in an error", and "A default sequence specification shall not accept
    // multiple transition bins".
    assert!(
        refused(each(transitions(
            "f",
            vec![vec![repeated(&[3], NonConsecutive, 2, 2)]]
        )))
        .is_some_and(|fault| fault.contains("goto or nonconsecutive"))
    );
    assert!(
        refused(each(default_sequence("g")))
            .is_some_and(|fault| fault.contains("`default sequence` bin array"))
    );
    assert!(
        refused(with_kind(default_sequence("h"), BinKind::Ignore))
            .is_some_and(|fault| fault.contains("ignored `default sequence`"))
    );
    // "Ignored transition bins cannot specify a sequence of unbounded or
    // undetermined varying length."
    assert!(
        refused(with_kind(
            transitions(
                "i",
                vec![vec![once(&[1]), repeated(&[3], NonConsecutive, 2, 2)]]
            ),
            BinKind::Ignore
        ))
        .is_some_and(|fault| fault.contains("goto or nonconsecutive"))
    );
}

#[test]
fn an_ignored_transition_removes_the_sequences_it_is_part_of() {
    let mut heap = Heap::new();
    let key = register(
        &mut heap,
        one_point(
            "removed",
            4,
            vec![
                // "the ignored sequence 2=>3 would remove the covered sequence
                // 1=>2=>3=>4".
                transitions("gone", vec![chain(&[1, 2, 3, 4])]),
                transitions(
                    "some",
                    vec![vec![
                        once(&[1]),
                        TransitionStep::new(vec![range(int(1), int(4))]),
                        TransitionStep::new(vec![range(int(1), int(4))]),
                        once(&[4]),
                    ]],
                ),
                with_kind(transitions("i", vec![chain(&[2, 3])]), BinKind::Ignore),
            ],
        ),
    );
    let object = construct(&mut heap, key, &[]);
    // `gone` has no sequence left and takes no part in coverage (19.11).
    assert_eq!(names(&heap, 0), text(&["some", "i"]));
    assert_eq!(inst_counts(&mut heap, object, 1), (0, 1));
    for value in [1, 2, 3, 4, 1, 3, 2, 4] {
        assert_eq!(sample_one(&mut heap, object, value), 0);
    }
    // 1=>2=>3=>4 is removed from `some`, 1=>3=>2=>4 is not.
    assert_eq!(
        instance_bins(&heap, 0, 0),
        [("some".to_string(), 1), ("i".to_string(), 1)]
    );
    assert_eq!(inst_counts(&mut heap, object, 1), (1, 1));
}

#[test]
fn ignored_and_illegal_values_have_no_effect_on_transitions() {
    let mut heap = Heap::new();
    // cg23 of 19.5.5 and cg3 of 19.5.6, with a transition through an
    // ignored and an illegal value.
    let key = register(
        &mut heap,
        group(
            "cg23",
            vec![
                point(
                    "a",
                    point_type(4, false),
                    vec![
                        with_kind(bins("ignore_vals", ints(&[7, 8])), BinKind::Ignore),
                        with_kind(
                            transitions("ignore_trans", vec![chain(&[1, 3, 5])]),
                            BinKind::Ignore,
                        ),
                    ],
                ),
                point(
                    "b",
                    point_type(4, false),
                    vec![
                        with_kind(bins("bad_vals", ints(&[1, 2, 3])), BinKind::Illegal),
                        with_kind(
                            transitions("bad_trans", vec![chain(&[4, 5, 6])]),
                            BinKind::Illegal,
                        ),
                        transitions("through", vec![chain(&[7, 2, 9]), chain(&[8, 8])]),
                    ],
                ),
            ],
        ),
    );
    let object = construct(&mut heap, key, &[]);
    let mut lines = Vec::new();
    for (a, b) in [(1, 7), (3, 2), (5, 9), (7, 4), (8, 5), (1, 6)] {
        call(&mut heap, op::POINT, 1, 0, [object.into(), a, 0]);
        call(&mut heap, op::POINT, 2, 0, [object.into(), b, 0]);
        let illegal = call(&mut heap, op::SAMPLE_END, 0, 0, [object.into(), 0, 0]);
        if illegal != 0 {
            lines.push(message(&mut heap, object));
        }
    }
    assert_eq!(
        lines,
        [
            "illegal_bins `bad_vals` of coverpoint `b` of covergroup `cg23` (instance \
             `cg23#0`): value 2",
            "illegal_bins `bad_trans` of coverpoint `b` of covergroup `cg23` (instance \
             `cg23#0`): transition 4=>5=>6",
        ]
    );
    let report = heap.coverage_report();
    let items = &report.covergroups[0].instances[0].items;
    let row = |item: usize, name: &str| {
        items[item]
            .bins
            .iter()
            .find(|bin| bin.name == name)
            .map(|bin| (bin.kind, bin.hits))
    };
    // The automatic bins of `a` are those of the values but 7 and 8; the
    // ignored transition takes no value from them.
    assert_eq!(items[0].total, 14);
    assert_eq!(row(0, "auto[1]"), Some((BinReportKind::Bins, 2)));
    assert_eq!(row(0, "ignore_trans"), Some((BinReportKind::Ignore, 1)));
    // The illegal value 2 of `b` still takes 7=>2=>9 on.
    assert_eq!(row(1, "through"), Some((BinReportKind::Bins, 1)));
    assert_eq!(row(1, "bad_trans"), Some((BinReportKind::Illegal, 1)));
    assert_eq!(row(1, "bad_vals"), Some((BinReportKind::Illegal, 1)));
}

#[test]
fn an_illegal_sequence_removes_matches_and_default_sequence_can_be_illegal() {
    let mut heap = Heap::new();
    let key = register(
        &mut heap,
        one_point(
            "strict",
            4,
            vec![
                transitions(
                    "wide",
                    vec![vec![
                        TransitionStep::new(vec![range(int(0), int(15))]),
                        TransitionStep::new(vec![range(int(0), int(15))]),
                        TransitionStep::new(vec![range(int(0), int(15))]),
                    ]],
                ),
                with_kind(transitions("bad", vec![chain(&[3, 1])]), BinKind::Illegal),
                with_kind(default_sequence("other"), BinKind::Illegal),
            ],
        ),
    );
    let object = construct(&mut heap, key, &[]);
    // The first sample is no transition; 2 starts `wide`.
    assert_eq!(sample_one(&mut heap, object, 2), 0);
    assert_eq!(sample_one(&mut heap, object, 3), 0);
    // 2=>3=>1 holds the illegal 3=>1.
    assert_eq!(sample_one(&mut heap, object, 1), 1);
    assert_eq!(
        message(&mut heap, object),
        "illegal_bins `bad` of coverpoint `v` of covergroup `strict` (instance `strict#0`): \
         transition 3=>1"
    );
    assert_eq!(hits_of(&heap, 0, "wide"), Some(0));
    // So does 3=>1=>4; 1=>4=>5 does not.
    assert_eq!(sample_one(&mut heap, object, 4), 0);
    assert_eq!(hits_of(&heap, 0, "wide"), Some(0));
    assert_eq!(sample_one(&mut heap, object, 5), 0);
    assert_eq!(hits_of(&heap, 0, "wide"), Some(1));
    // An illegal `default sequence` reports the transition no bin has.
    let mut lone = one_point(
        "lone",
        4,
        vec![with_kind(default_sequence("other"), BinKind::Illegal)],
    );
    lone.points[0]
        .bins
        .push(transitions("t", vec![chain(&[1, 2])]));
    let key = register(&mut heap, lone);
    let object = construct(&mut heap, key, &[]);
    assert_eq!(sample_one(&mut heap, object, 1), 0);
    assert_eq!(sample_one(&mut heap, object, 2), 0);
    assert_eq!(sample_one(&mut heap, object, 3), 1);
    assert_eq!(
        message(&mut heap, object),
        "illegal_bins `other` of coverpoint `v` of covergroup `lone` (instance `lone#0`): \
         transition 2=>3"
    );
}

#[test]
fn an_unsampled_coverpoint_keeps_its_transitions_where_they_are() {
    let mut heap = Heap::new();
    let key = register(
        &mut heap,
        group(
            "frozen",
            vec![
                point(
                    "a",
                    point_type(4, false),
                    vec![transitions("t", vec![chain(&[1, 2])])],
                ),
                point("b", point_type(4, false), vec![]),
            ],
        ),
    );
    let object = construct(&mut heap, key, &[]);
    sample(&mut heap, object, &[1, 0]);
    // A false `iff` of the coverpoint: no value of it.
    sample_some(&mut heap, object, &[None, Some(1)]);
    // A stopped coverpoint, then a stopped covergroup.
    call(&mut heap, op::COLLECT, 1, 0, [object.into(), 0, 0]);
    call(&mut heap, op::POINT, 1, 0, [object.into(), 7, 0]);
    call(&mut heap, op::SAMPLE_END, 0, 0, [object.into(), 0, 0]);
    call(&mut heap, op::COLLECT, 1, 1, [object.into(), 0, 0]);
    call(&mut heap, op::COLLECT, 0, 0, [object.into(), 0, 0]);
    call(&mut heap, op::POINT, 1, 0, [object.into(), 7, 0]);
    call(&mut heap, op::SAMPLE_END, 0, 0, [object.into(), 0, 0]);
    call(&mut heap, op::COLLECT, 0, 1, [object.into(), 0, 0]);
    assert_eq!(heap.take_fault(), None);
    // The 2 follows the 1 among the samples of `a`.
    sample(&mut heap, object, &[2, 0]);
    assert_eq!(hits_of(&heap, 0, "t"), Some(1));
}

#[test]
fn a_guard_holds_at_the_sample_a_transition_ends_with() {
    let mut heap = Heap::new();
    let mut descriptor = one_point(
        "guarded",
        4,
        vec![guarded(transitions("t", vec![chain(&[1, 2])]), 0)],
    );
    descriptor.points[0].predicates = 1;
    let key = register(&mut heap, descriptor);
    let object = construct(&mut heap, key, &[]);
    let sample_with = |heap: &mut Heap, value: u64, bit: u64| {
        call(heap, op::POINT, 1, 0, [object.into(), value, 0]);
        call(heap, op::PREDICATES, 1, 0, [object.into(), bit, 0]);
        call(heap, op::SAMPLE_END, 0, 0, [object.into(), 0, 0]);
    };
    sample_with(&mut heap, 1, 1);
    sample_with(&mut heap, 2, 0);
    assert_eq!(hits_of(&heap, 0, "t"), Some(0));
    sample_with(&mut heap, 1, 0);
    sample_with(&mut heap, 2, 1);
    assert_eq!(hits_of(&heap, 0, "t"), Some(1));
}

#[test]
fn wildcard_and_x_transitions() {
    let mut heap = Heap::new();
    let digits = |digits: &str| {
        let mut value = BigUint::ZERO;
        let mut mask = BigUint::ZERO;
        for digit in digits.chars() {
            value <<= 1;
            mask <<= 1;
            match digit {
                '0' => {}
                '1' => value |= BigUint::from(1u8),
                _ => {
                    value |= BigUint::from(1u8);
                    mask |= BigUint::from(1u8);
                }
            }
        }
        Constant {
            ty: ValueType {
                width: digits.len() as u32,
                signed: false,
            },
            value,
            mask,
        }
    };
    let mut wild = transitions(
        "wild",
        vec![vec![
            TransitionStep::new(vec![value(digits("1x"))]),
            TransitionStep::new(vec![value(digits("0x"))]),
        ]],
    );
    wild.wildcard = true;
    let exact = transitions(
        "exact",
        vec![vec![
            once(&[1]),
            TransitionStep::new(vec![value(digits("xx"))]),
        ]],
    );
    let mut four_state = point_type(2, false);
    four_state.four_state = true;
    let key = register(
        &mut heap,
        group("wildcards", vec![point("v", four_state, vec![wild, exact])]),
    );
    let object = construct(&mut heap, key, &[]);
    for (value, mask) in [(2, 0), (1, 0), (3, 0), (0, 0), (1, 0), (3, 3), (2, 2)] {
        call(&mut heap, op::POINT, 1, 0, [object.into(), value, mask]);
        call(&mut heap, op::SAMPLE_END, 0, 0, [object.into(), 0, 0]);
    }
    assert_eq!(
        instance_bins(&heap, 0, 0),
        [("wild".to_string(), 2), ("exact".to_string(), 1)]
    );
}

#[test]
fn transitions_survive_a_checkpoint_mid_sequence() {
    let mut heap = Heap::new();
    let key = register(
        &mut heap,
        one_point(
            "kept",
            4,
            vec![
                transitions("t", vec![chain(&[1, 2, 3])]),
                each(transitions(
                    "arr",
                    vec![vec![
                        once(&[1]),
                        TransitionStep::new(vec![range(int(2), int(3))]),
                        once(&[3]),
                    ]],
                )),
                transitions(
                    "goto",
                    vec![vec![once(&[1]), repeated(&[3], Repeat::Goto, 2, 2)]],
                ),
                default_sequence("rest"),
            ],
        ),
    );
    let object = construct(&mut heap, key, &[]);
    sample_one(&mut heap, object, 1);
    sample_one(&mut heap, object, 2);
    let mut restored: Heap = postcard::from_bytes(&postcard::to_allocvec(&heap).unwrap()).unwrap();
    assert!(restored.same_contents(&heap));
    for heap in [&mut heap, &mut restored] {
        sample_one(heap, object, 3);
        sample_one(heap, object, 5);
        sample_one(heap, object, 3);
    }
    assert!(restored.same_contents(&heap));
    assert_eq!(restored.coverage_report(), heap.coverage_report());
    assert_eq!(
        instance_bins(&heap, 0, 0),
        [
            ("t".to_string(), 1),
            ("arr[1=>2=>3]".to_string(), 1),
            ("arr[1=>3=>3]".to_string(), 0),
            ("goto".to_string(), 1),
            ("rest".to_string(), 0),
        ]
    );
    // A clone goes on on its own.
    let clone = heap.clone();
    sample_one(&mut heap, object, 7);
    assert!(!heap.same_contents(&clone));
}

#[test]
fn transition_bins_of_instances_merge_by_name() {
    let mut descriptor = one_point(
        "merged",
        4,
        vec![each(transitions(
            "t",
            vec![vec![
                TransitionStep::new(vec![RangeDescriptor::Value(Operand::Input(0))]),
                TransitionStep::new(vec![input_range(1, 2)]),
            ]],
        ))],
    );
    descriptor.inputs = int_inputs(3);
    let run = |merge: bool| {
        let mut heap = Heap::new();
        let mut descriptor = descriptor.clone();
        descriptor.type_options.merge_instances = merge;
        let key = register(&mut heap, descriptor);
        let first = construct(&mut heap, key, &[1, 2, 2]);
        let second = construct(&mut heap, key, &[1, 2, 3]);
        for value in [1, 2] {
            sample_one(&mut heap, first, value);
        }
        for value in [1, 3] {
            sample_one(&mut heap, second, value);
        }
        assert_eq!(names(&heap, 1), text(&["t[1=>2]", "t[1=>3]"]));
        (
            type_coverage(&mut heap, key, 1),
            type_counts(&mut heap, key, 1),
        )
    };
    // Each instance's own: 100 and 50, of 1 and 2 bins.
    let (coverage, counts) = run(false);
    assert_close(coverage, 75.0);
    assert_eq!(counts, (2, 3));
    // The union by name (19.11.3): t[1=>2] and t[1=>3], both hit.
    let (coverage, counts) = run(true);
    assert_close(coverage, 100.0);
    assert_eq!(counts, (2, 2));
}

#[test]
fn at_least_counts_the_hits_of_transition_bins() {
    let mut heap = Heap::new();
    let mut descriptor = one_point(
        "often",
        4,
        vec![
            transitions("up", vec![chain(&[1, 2])]),
            transitions("down", vec![chain(&[2, 1])]),
        ],
    );
    descriptor.options = vec![option(1, OptionId::AtLeast, 2)];
    let key = register(&mut heap, descriptor);
    let object = construct(&mut heap, key, &[]);
    for value in [1, 2, 1, 2] {
        sample_one(&mut heap, object, value);
    }
    // up 2 hits, down 1.
    assert_eq!(inst_counts(&mut heap, object, 1), (1, 2));
    assert_close(type_coverage(&mut heap, key, 0), 50.0);
}

#[test]
fn intersect_selects_transition_bins_by_their_last_values() {
    let mut heap = Heap::new();
    let mut descriptor = group(
        "crossed",
        vec![
            point(
                "a",
                point_type(4, false),
                vec![
                    transitions("up", vec![chain(&[1, 2])]),
                    transitions("down", vec![chain(&[2, 1])]),
                    bins("five", ints(&[5])),
                    each(transitions(
                        "arr",
                        vec![vec![
                            once(&[1]),
                            TransitionStep::new(vec![range(int(2), int(3))]),
                        ]],
                    )),
                ],
            ),
            point("b", point_type(1, false), vec![]),
        ],
    );
    let ending = |name: &str, bin: Option<u32>, last: i64| {
        CrossBinDescriptor::new(
            name,
            Select::BinsOf {
                position: 0,
                bin,
                intersect: Some(ints(&[last])),
                negate: false,
            },
        )
    };
    let mut cross = CrossDescriptor::new("x", vec![0, 1]);
    // `binsof(a) intersect {2}` and `binsof(a.arr) intersect {3}`.
    cross.bins = vec![
        ending("ends_with_2", None, 2),
        ending("ends_with_3", Some(3), 3),
    ];
    descriptor.crosses = vec![cross];
    let key = register(&mut heap, descriptor);
    let object = construct(&mut heap, key, &[]);
    for (a, b) in [(1, 0), (2, 1), (1, 1), (5, 0), (1, 0), (3, 0)] {
        sample(&mut heap, object, &[a, b]);
    }
    let report = heap.coverage_report();
    let cross = &report.covergroups[0].instances[0].items[2];
    let rows: Vec<(String, u64)> = cross
        .bins
        .iter()
        .map(|bin| (bin.name.clone(), bin.hits))
        .collect();
    // up and arr[1=>2] end with 2 (19.6.1.1), and the second sample is
    // one hit of the bin of both; arr[1=>3] ends with 3. down and five are
    // in automatic cross bins.
    assert_eq!(
        rows,
        [
            ("ends_with_2".to_string(), 1),
            ("ends_with_3".to_string(), 1),
            ("<down,auto[1]>".to_string(), 1),
            ("<five,auto[0]>".to_string(), 1),
        ]
    );
    // The bins of the cross: the two declared ones and the 5 * 2 - 6
    // automatic ones.
    assert_eq!(inst_counts(&mut heap, object, 3), (4, 6));
}

#[test]
fn array_bins_of_one_sequence_twice_are_one_bin() {
    let mut heap = Heap::new();
    let key = register(
        &mut heap,
        one_point(
            "twice",
            4,
            vec![each(transitions(
                "b",
                vec![
                    chain(&[1, 2]),
                    vec![TransitionStep::new(vec![range(int(1), int(2))]), once(&[2])],
                ],
            ))],
        ),
    );
    let object = construct(&mut heap, key, &[]);
    sample_one(&mut heap, object, 1);
    sample_one(&mut heap, object, 2);
    assert_eq!(
        instance_bins(&heap, 0, 0),
        [("b[1=>2]".to_string(), 1), ("b[2=>2]".to_string(), 0)]
    );
}

#[test]
fn transitions_beyond_the_limits_fail_the_build() {
    use Repeat::Consecutive;
    let states = one_point(
        "states",
        4,
        vec![transitions(
            "t",
            vec![vec![
                repeated(&[1], Consecutive, 70_000, 70_000),
                once(&[2]),
            ]],
        )],
    );
    assert!(build_fault(states, &[]).is_some_and(|fault| fault.contains("more than 65536 states")));
    let wide = || TransitionStep::new(vec![range(int(0), int(2047))]);
    let elements = one_point(
        "elements",
        16,
        vec![each(transitions("t", vec![vec![wide(), wide()]]))],
    );
    assert!(
        build_fault(elements, &[])
            .is_some_and(|fault| fault.contains("lists more than 1048576 sequences of values"))
    );
    // A scalar bin of as many sequences of values is one bin.
    let scalar = one_point(
        "scalar",
        16,
        vec![transitions("t", vec![vec![wide(), wide()]])],
    );
    assert_eq!(build_fault(scalar, &[]), None);
}

#[test]
fn a_covergroup_without_transitions_keeps_no_transition_state() {
    let mut heap = Heap::new();
    let key = register(&mut heap, merge_example());
    let object = construct(&mut heap, key, &[0, 0, 1, 1, 1, 0, 1]);
    sample(&mut heap, object, &[0]);
    sample(&mut heap, object, &[1]);
    // No coverpoint has a matcher of sequences, and the instance has no
    // threads to look at: sampling it does what it did before transitions.
    let registry = &heap.coverage;
    assert!(
        registry.types[0].shapes[0]
            .points
            .iter()
            .all(|point| point.transitions.is_none())
    );
    assert!(registry.instances[0].transitions.is_empty());
    // A coverpoint with transitions beside one without has state for both,
    // but only the first has a matcher of sequences.
    let key = register(
        &mut heap,
        group(
            "beside",
            vec![
                point("a", point_type(4, false), vec![]),
                point(
                    "b",
                    point_type(4, false),
                    vec![transitions("t", vec![chain(&[1, 2])])],
                ),
            ],
        ),
    );
    let object = construct(&mut heap, key, &[]);
    sample(&mut heap, object, &[1, 1]);
    let registry = &heap.coverage;
    let points = &registry.types[1].shapes[0].points;
    assert!(points[0].transitions.is_none() && points[1].transitions.is_some());
    assert_eq!(registry.instances[1].transitions[0], Progress::default());
}

#[test]
fn a_collected_instance_forgets_its_threads() {
    let mut heap = Heap::new();
    let key = register(
        &mut heap,
        one_point(
            "collected",
            4,
            vec![transitions(
                "t",
                vec![vec![once(&[1]), repeated(&[2], Repeat::Goto, 9, 9)]],
            )],
        ),
    );
    let object = construct(&mut heap, key, &[]);
    for value in [1, 1, 1, 2] {
        sample_one(&mut heap, object, value);
    }
    let size = heap.content_size();
    heap.collect([]);
    assert!(heap.content_size() < size - 8);
    assert_eq!(hits_of(&heap, 0, "t"), Some(0));
    assert_eq!(heap.take_fault(), None);
}

#[test]
fn a_removal_that_takes_too_many_steps_fails_the_build() {
    use Repeat::Consecutive;
    // No sequence of `t` ends without 3=>3, so finding that looks at every
    // run of up to 30000 values with each ignored transition under way.
    let mut bins = vec![transitions(
        "t",
        vec![vec![
            TransitionStep {
                values: vec![range(int(0), int(15))],
                repeat: Consecutive(
                    Operand::Constant(int(30_000)),
                    Operand::Constant(int(30_000)),
                ),
            },
            once(&[3]),
            once(&[3]),
        ]],
    )];
    bins.extend((0..16).map(|value| {
        with_kind(
            transitions("i", vec![chain(&[value, value])]),
            BinKind::Ignore,
        )
    }));
    assert!(
        build_fault(one_point("work", 4, bins), &[])
            .is_some_and(|fault| fault.contains("takes more than 67108864 steps"))
    );
    // A small one is found.
    let mut bins = vec![transitions(
        "t",
        vec![vec![
            repeated(&[0, 1, 2, 3], Consecutive, 3, 3),
            once(&[3]),
            once(&[3]),
        ]],
    )];
    bins.extend((0..16).map(|value| {
        with_kind(
            transitions("i", vec![chain(&[value, value])]),
            BinKind::Ignore,
        )
    }));
    let mut heap = Heap::new();
    let key = register(&mut heap, one_point("small", 4, bins));
    let object = construct(&mut heap, key, &[]);
    assert_eq!(inst_counts(&mut heap, object, 1), (0, 0));
}

#[test]
fn a_repetition_range_from_1_alone_is_a_transition_of_length_0() {
    use Repeat::{Consecutive, Goto, NonConsecutive};
    // `3 [* 1:2]` "is the same as" (3), (3=>3), and (3) is of length 0
    // (19.5.2); so for each repetition, constant or handed over.
    for repeat in [Consecutive, Goto, NonConsecutive] {
        let alone = |bin: BinDescriptor| registration_fault(one_point("alone", 4, vec![bin]));
        let length_0 = |fault: Option<String>| {
            fault.is_some_and(|fault| fault.contains("has a transition of length 0"))
        };
        assert!(length_0(alone(transitions(
            "t",
            vec![vec![repeated(&[3], repeat, 1, 2)]]
        ))));
        let mut counted = one_point(
            "counted",
            4,
            vec![transitions(
                "t",
                vec![vec![TransitionStep {
                    values: ints(&[3]),
                    repeat: repeat(Operand::Input(0), Operand::Input(1)),
                }]],
            )],
        );
        counted.inputs = int_inputs(2);
        assert!(length_0(build_fault(counted.clone(), &[1, 2])));
        assert_eq!(build_fault(counted, &[2, 2]), None);
        // Before another step it is no transition of length 0.
        let before = one_point(
            "before",
            4,
            vec![transitions(
                "t",
                vec![vec![repeated(&[3], repeat, 1, 2), once(&[4])]],
            )],
        );
        assert_eq!(build_fault(before, &[]), None);
    }
    let array = one_point(
        "array",
        4,
        vec![each(transitions(
            "b",
            vec![vec![repeated(&[3], Consecutive, 1, 2)]],
        ))],
    );
    assert!(registration_fault(array).is_some_and(|fault| fault.contains("length 0")));
}

#[test]
fn the_gap_after_a_nonconsecutive_repetition_ends_with_its_first_match() {
    use Repeat::{Consecutive, NonConsecutive};
    let run = |steps: Vec<TransitionStep>, trace: &[u64]| {
        let mut heap = Heap::new();
        let key = register(
            &mut heap,
            one_point("gap", 4, vec![transitions("t", vec![steps])]),
        );
        let object = construct(&mut heap, key, &[]);
        counted_by_sample(&mut heap, object, trace, &["t"])
            .iter()
            .enumerate()
            .filter(|(_, bins)| !bins.is_empty())
            .map(|(at, _)| at + 1)
            .collect::<Vec<_>>()
    };
    let then = |next: Vec<TransitionStep>, low, high| {
        let mut steps = vec![once(&[1]), repeated(&[2], NonConsecutive, low, high)];
        steps.extend(next);
        steps
    };
    // "the transition following the nonconsecutive repetition may occur
    // after any number of sample points so long as the repetition value
    // does not occur again" (19.5.2): 5 3 is a gap before 5=>6.
    assert_eq!(run(then(chain(&[5, 6]), 1, 1), &[1, 2, 5, 3, 5, 6]), [6]);
    // The first match ends the gap.
    assert_eq!(run(then(chain(&[5, 6]), 1, 1), &[1, 2, 5, 6, 5, 6]), [4]);
    assert_eq!(
        run(
            then(vec![repeated(&[5], Consecutive, 2, 2)], 1, 1),
            &[1, 2, 5, 3, 5, 5]
        ),
        [6]
    );
    // The 5 of the third sample is in the gap of the first 2 and before
    // the second: `[= 1:2]` matches with both counts.
    assert_eq!(run(then(chain(&[5]), 1, 2), &[1, 2, 5, 2, 5]), [3, 5]);
    assert_eq!(run(then(chain(&[5]), 1, 1), &[1, 2, 5, 5]), [3]);
    // A 2 in the gap ends it, but not a match under way.
    let any = || TransitionStep::new(vec![range(int(0), int(15))]);
    assert_eq!(
        run(
            then(vec![once(&[5]), any(), once(&[6])], 1, 1),
            &[1, 2, 5, 2, 6]
        ),
        [5]
    );
    assert_eq!(
        run(then(chain(&[5]), 1, 1), &[1, 2, 3, 2, 5]),
        Vec::<usize>::new()
    );
}

#[test]
fn a_long_consecutive_repetition_is_one_run() {
    let mut heap = Heap::new();
    let key = register(
        &mut heap,
        one_point(
            "long",
            3,
            vec![transitions(
                "t",
                vec![vec![TransitionStep {
                    values: vec![range(int(0), int(7))],
                    repeat: Repeat::Consecutive(
                        Operand::Constant(int(1000)),
                        Operand::Constant(int(1000)),
                    ),
                }]],
            )],
        ),
    );
    let object = construct(&mut heap, key, &[]);
    for value in 0..3000 {
        sample_one(&mut heap, object, value % 8);
    }
    // Each sample from the 1000th on ends a run of 1000.
    assert_eq!(hits_of(&heap, 0, "t"), Some(2001));
    // The threads are one run of the step, which a sample takes as a
    // whole: no thread of its own for each count.
    let progress = &heap.coverage.instances[0].transitions[0];
    assert!(progress.threads.is_empty());
    assert_eq!(progress.runs.len(), 1);
    assert!(progress.runs[0].pending.len() <= 1000 && progress.runs[0].ready.len() <= 1);
}

#[test]
fn huge_counts_fault_before_anything_grows_with_them() {
    use Repeat::Consecutive;
    let most = || Operand::Constant(constant(33, false, i128::from(u32::MAX)));
    let huge = || TransitionStep {
        values: ints(&[3]),
        repeat: Consecutive(most(), most()),
    };
    let started = std::time::Instant::now();
    for bin in [
        transitions("scalar", vec![vec![huge(), once(&[4])]]),
        each(transitions("array", vec![vec![huge(), once(&[4])]])),
        with_kind(
            transitions("ignored", vec![vec![huge(), once(&[4])]]),
            BinKind::Ignore,
        ),
        each(transitions(
            "lengths",
            vec![vec![
                TransitionStep {
                    values: ints(&[3]),
                    repeat: Consecutive(Operand::Constant(int(2)), most()),
                },
                once(&[4]),
            ]],
        )),
    ] {
        let fault = build_fault(one_point("huge", 4, vec![bin]), &[]);
        assert!(fault.is_some_and(|fault| fault.contains("more than 65536 states")));
    }
    // A count of an argument: `bins b[] = (3 [* n]);` with n = 2^31 - 1.
    let mut argument = one_point(
        "argument",
        4,
        vec![each(transitions(
            "b",
            vec![vec![
                TransitionStep {
                    values: ints(&[3]),
                    repeat: Consecutive(Operand::Input(0), Operand::Input(0)),
                },
                once(&[4]),
            ]],
        ))],
    );
    argument.inputs = int_inputs(1);
    assert!(
        build_fault(argument, &[i64::from(i32::MAX)])
            .is_some_and(|fault| fault.contains("more than 65536 states"))
    );
    // A list of 2^32 positions took seconds and 16 GB; the limit is found
    // first.
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
}

#[test]
fn an_ignored_transition_beside_a_wildcard_pattern_fails_the_build() {
    // A 40-bit coverpoint: x at bits 1, 3, ..., 35 is 2^18 ranges, which is
    // matched as a pattern, so the bins an ignored transition empties are
    // not known (19.5.5, 19.11.1).
    let width = 40;
    let mut mask = BigUint::ZERO;
    for bit in (1..36).step_by(2) {
        mask |= BigUint::from(1u8) << bit;
    }
    let pattern = Constant {
        ty: ValueType {
            width,
            signed: false,
        },
        value: mask.clone(),
        mask,
    };
    let wild = || {
        let mut bin = transitions(
            "i",
            vec![vec![
                TransitionStep::new(vec![value(pattern.clone())]),
                TransitionStep::new(vec![value(pattern.clone())]),
            ]],
        );
        bin.wildcard = true;
        bin
    };
    let descriptor = |bins| group("wide", vec![point("v", point_type(width, false), bins)]);
    let removing = descriptor(vec![
        transitions("t", vec![chain(&[0, 0])]),
        with_kind(wild(), BinKind::Ignore),
    ]);
    assert!(
        build_fault(removing, &[])
            .is_some_and(|fault| fault.contains("wildcard transition value of more than 65536"))
    );
    // Without an ignored or illegal transition the pattern is matched.
    let mut heap = Heap::new();
    let key = register(&mut heap, descriptor(vec![wild()]));
    let object = construct(&mut heap, key, &[]);
    for _ in 0..2 {
        sample_one(&mut heap, object, 0b10);
    }
    assert_eq!(hits_of(&heap, 0, "i"), Some(1));
}

#[test]
fn arrays_of_argument_counts_merge_by_name() {
    // `bins b[] = (1 => 2 [* n]);` with n an argument: b[1=>2] and
    // b[1=>2=>2] in two instances, one bin of each name in the type.
    let mut descriptor = one_point(
        "counts",
        4,
        vec![each(transitions(
            "b",
            vec![vec![
                once(&[1]),
                TransitionStep {
                    values: ints(&[2]),
                    repeat: Repeat::Consecutive(Operand::Input(0), Operand::Input(0)),
                },
            ]],
        ))],
    );
    descriptor.inputs = int_inputs(1);
    descriptor.type_options.merge_instances = true;
    let mut heap = Heap::new();
    let key = register(&mut heap, descriptor);
    let one = construct(&mut heap, key, &[1]);
    let two = construct(&mut heap, key, &[2]);
    for value in [1, 2, 2] {
        sample_one(&mut heap, one, value);
        sample_one(&mut heap, two, value);
    }
    assert_eq!(instance_bins(&heap, 0, 0), [("b[1=>2]".to_string(), 1)]);
    assert_eq!(instance_bins(&heap, 1, 0), [("b[1=>2=>2]".to_string(), 1)]);
    assert_eq!(type_counts(&mut heap, key, 1), (2, 2));
    let report = heap.coverage_report();
    let names: Vec<&str> = report.covergroups[0].items[0]
        .bins
        .iter()
        .map(|bin| bin.name.as_str())
        .collect();
    assert_eq!(names, ["b[1=>2]", "b[1=>2=>2]"]);
}

#[test]
fn transitions_of_an_enumeration() {
    // An enumeration has no automatic bins beside a transition bin, and
    // its values are named by number in an array.
    let mut ty = point_type(2, false);
    ty.members = vec![
        ("IDLE".to_string(), sized(2, 0)),
        ("BUSY".to_string(), sized(2, 1)),
        ("DONE".to_string(), sized(2, 2)),
    ];
    let steps = || {
        vec![vec![
            TransitionStep::new(vec![value(sized(2, 0))]),
            TransitionStep::new(vec![value(sized(2, 1))]),
            TransitionStep::new(vec![value(sized(2, 2))]),
        ]]
    };
    let mut heap = Heap::new();
    let key = register(
        &mut heap,
        group(
            "states",
            vec![point(
                "s",
                ty,
                vec![
                    transitions("cycle", steps()),
                    each(transitions("each", steps())),
                ],
            )],
        ),
    );
    let object = construct(&mut heap, key, &[]);
    for value in [0, 1, 2, 0, 2] {
        sample_one(&mut heap, object, value);
    }
    assert_eq!(
        instance_bins(&heap, 0, 0),
        [("cycle".to_string(), 1), ("each[0=>1=>2]".to_string(), 1)]
    );
    assert_eq!(inst_counts(&mut heap, object, 1), (2, 2));
}

// A brute-force model of the transitions, against which random bins and
// traces are compared.

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Consecutive,
    Goto,
    NonConsecutive,
}

#[derive(Clone, Debug)]
struct ModelStep {
    set: Vec<u64>,
    kind: Kind,
    low: u32,
    high: u32,
}

/// The gap a match leaves a nonconsecutive repetition by: its step, its
/// count and the sample of its last value.
type GapKey = Option<(usize, u32, usize)>;

/// Add to `found` each match of `steps` that goes on at step `at` from
/// sample `position` of `trace` with `count` samples of the step, the last
/// sample having been one of them where `occurred` is set, and that left
/// its first gap by `gap`: the sample it ends with and that gap.
#[allow(clippy::too_many_arguments)]
fn model_matches(
    steps: &[ModelStep],
    trace: &[u64],
    at: usize,
    position: usize,
    count: u32,
    occurred: bool,
    gap: GapKey,
    found: &mut Vec<(usize, GapKey)>,
) {
    let step = &steps[at];
    let last = at + 1 == steps.len();
    if !last && count >= step.low {
        match step.kind {
            Kind::Consecutive => {
                model_matches(steps, trace, at + 1, position, 0, false, gap, found)
            }
            Kind::Goto if occurred => {
                model_matches(steps, trace, at + 1, position, 0, false, gap, found)
            }
            Kind::NonConsecutive if occurred => {
                // Any number of samples without a value of the repetition,
                // then the next step.
                let gap = gap.or(Some((at, count, position - 1)));
                let mut next = position;
                while next < trace.len() {
                    model_matches(steps, trace, at + 1, next, 0, false, gap, found);
                    if step.set.contains(&trace[next]) {
                        break;
                    }
                    next += 1;
                }
            }
            Kind::Goto | Kind::NonConsecutive => {}
        }
    }
    let Some(&value) = trace.get(position) else {
        return;
    };
    let inside = step.set.contains(&value);
    if inside && count < step.high {
        let count = count + 1;
        if last && count >= step.low {
            found.push((position, gap));
        }
        if !last || count < step.high {
            model_matches(steps, trace, at, position + 1, count, true, gap, found);
        }
    } else if !inside
        && step.kind != Kind::Consecutive
        && count < step.high
        && (at, count) != (0, 0)
    {
        model_matches(steps, trace, at, position + 1, count, false, gap, found);
    }
}

/// The samples at which a match of `steps` started with sample `start`
/// counts: every match that leaves no gap, and of those that leave one gap
/// the first.
fn model_ends(steps: &[ModelStep], trace: &[u64], start: usize) -> Vec<usize> {
    let mut found = Vec::new();
    model_matches(steps, trace, 0, start, 0, false, None, &mut found);
    let mut first: std::collections::BTreeMap<(usize, u32, usize), usize> = Default::default();
    let mut ends = Vec::new();
    for (end, gap) in found {
        match gap {
            None => ends.push(end),
            Some(gap) => {
                let earliest = first.entry(gap).or_insert(end);
                *earliest = (*earliest).min(end);
            }
        }
    }
    ends.extend(first.into_values());
    ends.sort_unstable();
    ends.dedup();
    ends
}

struct ModelBin {
    name: String,
    kind: BinKind,
    array: bool,
    sequences: Vec<Vec<ModelStep>>,
}

/// By bin name, the samples (from 1) at which the model counts.
fn model_counts(
    bins: &[ModelBin],
    trace: &[u64],
) -> std::collections::BTreeMap<String, Vec<usize>> {
    let mut removers = Vec::new();
    for bin in bins.iter().filter(|bin| bin.kind != BinKind::Bins) {
        for steps in &bin.sequences {
            for start in 0..trace.len() {
                removers.extend(
                    model_ends(steps, trace, start)
                        .into_iter()
                        .map(|end| (start, end)),
                );
            }
        }
    }
    let mut counts: std::collections::BTreeMap<String, Vec<usize>> = Default::default();
    for bin in bins {
        let mut hits: std::collections::BTreeSet<(String, usize)> = Default::default();
        for steps in &bin.sequences {
            for start in 0..trace.len() {
                for end in model_ends(steps, trace, start) {
                    let removed = removers
                        .iter()
                        .any(|&(first, last)| first >= start && last <= end);
                    if bin.kind == BinKind::Bins && removed {
                        continue;
                    }
                    let name = if bin.array {
                        let values: Vec<String> =
                            trace[start..=end].iter().map(u64::to_string).collect();
                        format!("{}[{}]", bin.name, values.join("=>"))
                    } else {
                        bin.name.clone()
                    };
                    hits.insert((name, end + 1));
                }
            }
        }
        for (name, at) in hits {
            counts.entry(name).or_default().push(at);
        }
    }
    for samples in counts.values_mut() {
        samples.sort_unstable();
    }
    counts
}

/// A xorshift generator.
struct Random(u64);

impl Random {
    fn below(&mut self, bound: u64) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0 % bound
    }

    fn step(&mut self, bounded: bool) -> ModelStep {
        let mut set = vec![self.below(4)];
        let other = self.below(4);
        if self.below(3) == 0 && !set.contains(&other) {
            set.push(other);
        }
        let kind = match (bounded, self.below(4)) {
            (true, _) | (false, 0 | 1) => Kind::Consecutive,
            (false, 2) => Kind::Goto,
            (false, _) => Kind::NonConsecutive,
        };
        let (low, high) = if self.below(2) == 0 {
            (1, 1)
        } else {
            let low = 1 + self.below(2) as u32;
            (low, low + self.below(2) as u32)
        };
        ModelStep {
            set,
            kind,
            low,
            high,
        }
    }

    /// Steps of a sequence that is no transition of length 0.
    fn sequence(&mut self, bounded: bool, steps: u64) -> Vec<ModelStep> {
        loop {
            let count = 1 + self.below(steps) as usize;
            let sequence: Vec<ModelStep> = (0..count).map(|_| self.step(bounded)).collect();
            if sequence.len() > 1 || sequence[0].low > 1 {
                return sequence;
            }
        }
    }
}

fn model_step(step: &ModelStep) -> TransitionStep {
    let values: Vec<i64> = step.set.iter().map(|&value| value as i64).collect();
    let repeat = match step.kind {
        Kind::Consecutive if (step.low, step.high) == (1, 1) => return once(&values),
        Kind::Consecutive => Repeat::Consecutive,
        Kind::Goto => Repeat::Goto,
        Kind::NonConsecutive => Repeat::NonConsecutive,
    };
    repeated(&values, repeat, i64::from(step.low), i64::from(step.high))
}

/// Compare `cases` random coverpoints and traces from `seed` with the
/// model: the descriptions of those that differ.
fn differences(seed: u64, cases: usize, bounded: bool) -> Vec<String> {
    let mut random = Random(seed);
    let mut differences = Vec::new();
    for case in 0..cases {
        let mut bins = Vec::new();
        for index in 0..3 {
            let sequences = (0..1 + random.below(2))
                .map(|_| random.sequence(bounded, 3))
                .collect();
            bins.push(ModelBin {
                name: format!("b{index}"),
                kind: BinKind::Bins,
                array: false,
                sequences,
            });
        }
        bins.push(ModelBin {
            name: "a".into(),
            kind: BinKind::Bins,
            array: true,
            sequences: vec![random.sequence(true, 2)],
        });
        for index in 0..random.below(3) {
            bins.push(ModelBin {
                name: format!("i{index}"),
                kind: if random.below(2) == 0 {
                    BinKind::Ignore
                } else {
                    BinKind::Illegal
                },
                array: false,
                sequences: vec![random.sequence(true, 2)],
            });
        }
        let trace: Vec<u64> = (0..24).map(|_| random.below(4)).collect();
        let descriptor = one_point(
            "model",
            3,
            bins.iter()
                .map(|bin| {
                    let sequences = bin
                        .sequences
                        .iter()
                        .map(|steps| steps.iter().map(model_step).collect())
                        .collect();
                    let mut descriptor = with_kind(transitions(&bin.name, sequences), bin.kind);
                    if bin.array {
                        descriptor = each(descriptor);
                    }
                    descriptor
                })
                .collect(),
        );
        let mut heap = Heap::new();
        let key = register(&mut heap, descriptor);
        let object = construct(&mut heap, key, &[]);
        let listed = names(&heap, 0);
        let mut observed: std::collections::BTreeMap<String, Vec<usize>> = Default::default();
        let mut before: std::collections::BTreeMap<String, u64> =
            instance_bins(&heap, 0, 0).into_iter().collect();
        for (at, &value) in trace.iter().enumerate() {
            call(&mut heap, op::POINT, 1, 0, [object.into(), value, 0]);
            call(&mut heap, op::SAMPLE_END, 0, 0, [object.into(), 0, 0]);
            message(&mut heap, object);
            assert_eq!(heap.take_fault(), None);
            let after: std::collections::BTreeMap<String, u64> =
                instance_bins(&heap, 0, 0).into_iter().collect();
            for (name, &hits) in &after {
                let was = before.get(name).copied().unwrap_or(0);
                assert!(hits <= was + 1, "{name} counted twice in a sample");
                if hits > was {
                    observed.entry(name.clone()).or_default().push(at + 1);
                }
            }
            before = after;
        }
        let expected = model_counts(&bins, &trace);
        let mut wrong = Vec::new();
        for (name, samples) in &expected {
            if !listed.contains(name) {
                wrong.push(format!(
                    "{name}: {samples:?} expected of a bin that is not listed"
                ));
            } else if observed.get(name) != Some(samples) {
                wrong.push(format!(
                    "{name}: {samples:?} expected, {:?} counted",
                    observed.get(name)
                ));
            }
        }
        for (name, samples) in &observed {
            if !expected.contains_key(name) {
                wrong.push(format!("{name}: none expected, {samples:?} counted"));
            }
        }
        if !wrong.is_empty() {
            differences.push(format!(
                "case {case} trace {trace:?} bins {:?}: {wrong:?}",
                bins.iter()
                    .map(|bin| format!("{} {:?}", bin.name, bin.sequences))
                    .collect::<Vec<_>>()
            ));
        }
    }
    differences
}

#[test]
fn bounded_transitions_count_as_the_model_does() {
    let differences = differences(0x9E37_79B9_7F4A_7C15, 2000, true);
    assert!(
        differences.is_empty(),
        "{:#?}",
        &differences[..differences.len().min(3)]
    );
}

#[test]
fn goto_and_nonconsecutive_transitions_count_as_the_model_does() {
    for seed in [0xD1B5_4A32_D192_ED03, 0x0123_4567_89AB_CDEF] {
        let differences = differences(seed, 2000, false);
        assert!(
            differences.is_empty(),
            "{:#?}",
            &differences[..differences.len().min(3)]
        );
    }
}

#[test]
fn the_model_reads_the_examples_of_19_5_2() {
    let step = |value: u64, kind, low, high| ModelStep {
        set: vec![value],
        kind,
        low,
        high,
    };
    let once = |value| step(value, Kind::Consecutive, 1, 1);
    let counted = |steps: &[ModelStep], trace: &[u64]| {
        let mut ends: Vec<usize> = (0..trace.len())
            .flat_map(|start| model_ends(steps, trace, start))
            .map(|end| end + 1)
            .collect();
        ends.sort_unstable();
        ends.dedup();
        ends
    };
    let sg = [1, 4, 3, 2, 3, 3, 2, 2, 3, 2, 3, 1, 5, 5, 5, 5, 5, 5];
    let b7 = [once(1), step(2, Kind::NonConsecutive, 3, 6), once(5)];
    assert_eq!(counted(&b7, &sg), [13]);
    let b6 = [once(1), step(3, Kind::Goto, 4, 6), once(1)];
    assert_eq!(counted(&b6, &sg), [12]);
    let b3 = [step(3, Kind::Goto, 3, 5)];
    assert_eq!(counted(&b3, &sg), [6, 9, 11]);
}

/// The values of a type of `width` bits as step values, those with x or z
/// bits too in four-state, but for `left_out` (its value and mask).
fn every_value(width: u32, four_state: bool, left_out: Option<(u64, u64)>) -> Vec<RangeDescriptor> {
    let masks = if four_state { 1u64 << width } else { 1 };
    (0..masks)
        .flat_map(|mask| (0..1u64 << width).map(move |value| (value, mask)))
        .filter(|pair| Some(*pair) != left_out)
        .map(|(value, mask)| {
            super::value(Constant {
                ty: ValueType {
                    width,
                    signed: false,
                },
                value: BigUint::from(value),
                mask: BigUint::from(mask),
            })
        })
        .collect()
}

/// 19.5.5: a bin is empty when each of its matches contains an ignored
/// sequence, which `(0 => v)` for every value `v` of the type is for
/// `(0 => 1 [-> 2])`: the value after the 0 is one. The values of a
/// four-state type are those with x or z bits too, and a value that the
/// ignored sequence leaves out keeps the bin.
#[test]
fn an_ignored_transition_of_every_value_empties_the_bins_it_is_part_of() {
    for (width, four_state, left_out, empty) in [
        (2, true, None, true),
        // `xxx`, `0z1`.
        (3, true, Some((0, 0b111)), false),
        (3, true, Some((0b001, 0b010)), false),
        (3, true, None, true),
        (1, true, None, true),
        (1, true, Some((1, 1)), false),
        (2, false, None, true),
        (2, false, Some((3, 0)), false),
    ] {
        let context = format!("{width} bits, four_state={four_state}, without {left_out:?}");
        let mut ty = point_type(width, false);
        ty.four_state = four_state;
        let mut heap = Heap::new();
        let key = register(
            &mut heap,
            group(
                "every",
                vec![point(
                    "v",
                    ty,
                    vec![
                        transitions(
                            "t",
                            vec![vec![once(&[0]), repeated(&[1], Repeat::Goto, 2, 2)]],
                        ),
                        with_kind(
                            transitions(
                                "i",
                                vec![vec![
                                    once(&[0]),
                                    TransitionStep::new(every_value(width, four_state, left_out)),
                                ]],
                            ),
                            BinKind::Ignore,
                        ),
                    ],
                )],
            ),
        );
        let object = construct(&mut heap, key, &[]);
        let expected = if empty {
            text(&["i"])
        } else {
            text(&["t", "i"])
        };
        assert_eq!(names(&heap, 0), expected, "{context}");
        assert_eq!(
            inst_counts(&mut heap, object, 1),
            (0, u64::from(!empty)),
            "{context}"
        );
    }
}
