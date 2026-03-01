use celox::Simulator;

/// Tests that two different packages can instantiate the same generic module,
/// each getting a unique ModuleId. This validates the ModuleId refactor that
/// enables multiple concrete instantiations of a single generic module.
#[test]
fn test_generic_module_instantiation() {
    let code = r#"
proto package DataType {
    type data;
}

module GenericPass::<E: DataType> (
    i: input  E::data,
    o: output E::data,
) {
    assign o = i;
}

package Byte for DataType {
    type data = logic<8>;
}

package Word for DataType {
    type data = logic<16>;
}

module BytePass (
    i: input  logic<8>,
    o: output logic<8>,
) {
    inst inner: GenericPass::<Byte> (i, o);
}

module WordPass (
    i: input  logic<16>,
    o: output logic<16>,
) {
    inst inner: GenericPass::<Word> (i, o);
}

module Top (
    a: input  logic<8>,
    b: output logic<8>,
    c: input  logic<16>,
    d: output logic<16>,
) {
    inst bp: BytePass (i: a, o: b);
    inst wp: WordPass (i: c, o: d);
}
    "#;

    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    let c = sim.signal("c");
    let d = sim.signal("d");

    // Verify 8-bit passthrough via GenericPass::<Byte>
    sim.modify(|io| {
        io.set(a, 0xABu8);
        io.set(c, 0x1234u16);
    })
    .unwrap();
    assert_eq!(sim.get(b), 0xABu8.into());
    assert_eq!(sim.get(d), 0x1234u16.into());

    // Verify with different values
    sim.modify(|io| {
        io.set(a, 0xFFu8);
        io.set(c, 0xFFFFu16);
    })
    .unwrap();
    assert_eq!(sim.get(b), 0xFFu8.into());
    assert_eq!(sim.get(d), 0xFFFFu16.into());
}

/// Shared Veryl source for the compare matrix sorter tests.
/// Proto package functions (`E::gt`, `E::ge`) and constants (`E::max_value`)
/// are not yet supported in comb lowering, so all tests using this code are `#[ignore]`.
const COMPARE_MATRIX_CODE: &str = r#"
proto package Element {
    type data;
    function gt(
        a: input data,
        b: input data,
    ) -> logic ;
    function ge(
        a: input data,
        b: input data,
    ) -> logic ;
    const max_value: data;
}

module CompareMatrixStage1CM::<E: Element> #(
    param P: u32 = 32,
) (
    in_data  : input  E::data        [P],
    out_score: output logic<$clog2(P)> [P],
) {
    var matrix: logic [P, P];

    always_comb {
        for y: u32 in 0..P {
            for x: u32 in 0..P {
                if y >: x {
                    matrix[y][x] = E::ge(in_data[y], in_data[x]);
                } else if y <: x {
                    matrix[y][x] = E::gt(in_data[y] ,in_data[x]);
                } else {
                    matrix[y][x] = 0;
                }
            }
        }
    }

    always_comb {
        for y: u32 in 0..P {
            out_score[y] = 0;
            for x: u32 in 0..P {
                out_score[y] += matrix[y][x];
            }
        }
    }
}

module CompareMatrixSelector::<E: Element> #(
    param P: u32 = 32,
) (
    in_data  : input  E::data         [P],
    in_scores: input  logic<$clog2(P)> [P],
    out_data : output E::data        [P],

) {
    always_comb {
        for j: u32 in 0..P {
            out_data[j] = E::max_value;
            for i: u32 in 0..P {
                if in_scores[i] == j {
                    out_data[j] = in_data[i];
                }
            }
        }
    }
}

module CompareMatrixStage1::<E: Element> #(
    param P: u32 = 32,
) (
    in_data : input  E::data [P],
    out_data: output E::data [P],
) {
    var scores: logic<$clog2(P)> [P];

    inst stage1: CompareMatrixStage1CM::<E> #(
        P: P,
    ) (
        in_data  : in_data,
        out_score: scores ,
    );

    inst selector: CompareMatrixSelector::<E> #(
        P: P,
    ) (
        in_data  : in_data ,
        in_scores: scores  ,
        out_data : out_data,
    );
}

module CompareMatrixMerger::<E: Element> #(
    param A: u32 = 32,
    param B: u32 = 10,
) (
    in_a    : input E::data [A]    ,
    in_b    : input E::data [B]    ,
    out_data: output E::data [A + B],
) {
    var scores_a: logic<$clog2(A + B)> [A];
    var scores_b: logic<$clog2(A + B)> [B];

    always_comb {
        for i: u32 in 0..A {
            scores_a[i] = i;
            for j: u32 in 0..B {
                if E::gt(in_a[i],in_b[j]) {
                    scores_a[i] += 1;
                }
            }
        }

        for i: u32 in 0..B {
            scores_b[i] = i;
            for j: u32 in 0..A {
                if E::ge(in_b[i],in_a[j]) {
                    scores_b[i] += 1;
                }
            }
        }
    }

    always_comb {
        for k: u32 in 0..(A + B) {
            out_data[k] = E::max_value;
            for i: u32 in 0..A {
                if scores_a[i] == k {
                    out_data[k] = in_a[i];
                }
            }
            for i: u32 in 0..B {
                if scores_b[i] == k {
                    out_data[k] = in_b[i];
                }
            }
        }
    }
}

package IntElement::<W: u32 = 32> for Element {
    type data = logic<W>;
    function gt(
        a: input data,
        b: input data,
    ) -> logic {
        return a >: b;
    }
    function ge(
        a: input data,
        b: input data,
    ) -> logic {
        return a >= b;
    }
    const max_value: data = ~0;
}

module CompareMatrixStage1CMInt32 #(
    param P: u32 = 32,
) (
    in_data  : input  logic<32>        [P],
    out_score: output logic<$clog2(P)> [P],
) {
    inst inner: CompareMatrixStage1CM::<IntElement> (in_data, out_score);
}

module CompareMatrixSelectorInt32 #(
    param P: u32 = 32,
) (
    in_data  : input  logic<32>         [P],
    in_scores: input  logic<$clog2(P)> [P],
    out_data : output logic<32>        [P],
) {
    inst inner: CompareMatrixSelector::<IntElement> (in_data, in_scores, out_data);
}

module CompareMatrixStage1Int32 #(
    param P: u32 = 32,
) (
    in_data : input  logic<32> [P],
    out_data: output logic<32> [P],
) {
    inst inner: CompareMatrixStage1::<IntElement> (in_data, out_data);
}

module CompareMatrixMergerInt32 #(
    param A: u32 = 32,
    param B: u32 = 10,
) (
    in_a    : input  logic<32> [A],
    in_b    : input  logic<32> [B],
    out_data: output logic<32> [A + B],
) {
    inst inner: CompareMatrixMerger::<IntElement> (in_a, in_b, out_data);
}
"#;

/// Tests the compare matrix scoring module.
/// Input 4 values, verify scores reflect sorted order (descending).
#[test]
#[ignore]
fn test_compare_matrix_stage1cm() {
    let top = r#"
module Top #(
    param P: u32 = 4,
) (
    in_data  : input  logic<32>        [P],
    out_score: output logic<$clog2(P)> [P],
) {
    inst cm: CompareMatrixStage1CMInt32 #(P: P) (in_data, out_score);
}
    "#;
    let code = format!("{COMPARE_MATRIX_CODE}\n{top}");

    let mut sim = Simulator::builder(&code, "Top").build().unwrap();
    let in0 = sim.signal("in_data[0]");
    let in1 = sim.signal("in_data[1]");
    let in2 = sim.signal("in_data[2]");
    let in3 = sim.signal("in_data[3]");
    let s0 = sim.signal("out_score[0]");
    let s1 = sim.signal("out_score[1]");
    let s2 = sim.signal("out_score[2]");
    let s3 = sim.signal("out_score[3]");

    // Input: [10, 40, 20, 30] → descending order: 40(3), 30(2), 20(1), 10(0)
    sim.modify(|io| {
        io.set(in0, 10u32);
        io.set(in1, 40u32);
        io.set(in2, 20u32);
        io.set(in3, 30u32);
    })
    .unwrap();

    assert_eq!(sim.get(s0), 0u32.into()); // 10 is smallest → score 0
    assert_eq!(sim.get(s1), 3u32.into()); // 40 is largest  → score 3
    assert_eq!(sim.get(s2), 1u32.into()); // 20 → score 1
    assert_eq!(sim.get(s3), 2u32.into()); // 30 → score 2
}

/// Tests the compare matrix selector module.
/// Input data + pre-computed scores, verify reordered output.
#[test]
#[ignore]
fn test_compare_matrix_selector() {
    let top = r#"
module Top #(
    param P: u32 = 4,
) (
    in_data  : input  logic<32>         [P],
    in_scores: input  logic<$clog2(P)> [P],
    out_data : output logic<32>        [P],
) {
    inst sel: CompareMatrixSelectorInt32 #(P: P) (in_data, in_scores, out_data);
}
    "#;
    let code = format!("{COMPARE_MATRIX_CODE}\n{top}");

    let mut sim = Simulator::builder(&code, "Top").build().unwrap();
    let in0 = sim.signal("in_data[0]");
    let in1 = sim.signal("in_data[1]");
    let in2 = sim.signal("in_data[2]");
    let in3 = sim.signal("in_data[3]");
    let sc0 = sim.signal("in_scores[0]");
    let sc1 = sim.signal("in_scores[1]");
    let sc2 = sim.signal("in_scores[2]");
    let sc3 = sim.signal("in_scores[3]");
    let o0 = sim.signal("out_data[0]");
    let o1 = sim.signal("out_data[1]");
    let o2 = sim.signal("out_data[2]");
    let o3 = sim.signal("out_data[3]");

    // Data: [10, 40, 20, 30], Scores: [0, 3, 1, 2]
    // Output should place each value at its score index
    sim.modify(|io| {
        io.set(in0, 10u32);
        io.set(in1, 40u32);
        io.set(in2, 20u32);
        io.set(in3, 30u32);
        io.set(sc0, 0u32);
        io.set(sc1, 3u32);
        io.set(sc2, 1u32);
        io.set(sc3, 2u32);
    })
    .unwrap();

    assert_eq!(sim.get(o0), 10u32.into()); // score 0 → slot 0
    assert_eq!(sim.get(o1), 20u32.into()); // score 1 → slot 1
    assert_eq!(sim.get(o2), 30u32.into()); // score 2 → slot 2
    assert_eq!(sim.get(o3), 40u32.into()); // score 3 → slot 3
}

/// Tests full sorting via CompareMatrixStage1 (scoring + selection).
/// Input unsorted values, output sorted descending.
#[test]
#[ignore]
fn test_compare_matrix_stage1_sort() {
    let top = r#"
module Top #(
    param P: u32 = 4,
) (
    in_data : input  logic<32> [P],
    out_data: output logic<32> [P],
) {
    inst sorter: CompareMatrixStage1Int32 #(P: P) (in_data, out_data);
}
    "#;
    let code = format!("{COMPARE_MATRIX_CODE}\n{top}");

    let mut sim = Simulator::builder(&code, "Top").build().unwrap();
    let in0 = sim.signal("in_data[0]");
    let in1 = sim.signal("in_data[1]");
    let in2 = sim.signal("in_data[2]");
    let in3 = sim.signal("in_data[3]");
    let o0 = sim.signal("out_data[0]");
    let o1 = sim.signal("out_data[1]");
    let o2 = sim.signal("out_data[2]");
    let o3 = sim.signal("out_data[3]");

    // Input: [10, 40, 20, 30] → sorted ascending: [10, 20, 30, 40]
    sim.modify(|io| {
        io.set(in0, 10u32);
        io.set(in1, 40u32);
        io.set(in2, 20u32);
        io.set(in3, 30u32);
    })
    .unwrap();

    assert_eq!(sim.get(o0), 10u32.into());
    assert_eq!(sim.get(o1), 20u32.into());
    assert_eq!(sim.get(o2), 30u32.into());
    assert_eq!(sim.get(o3), 40u32.into());
}

/// Tests the compare matrix merger.
/// Two sorted arrays in, one merged sorted array out.
#[test]
#[ignore]
fn test_compare_matrix_merger() {
    let top = r#"
module Top #(
    param A: u32 = 3,
    param B: u32 = 2,
) (
    in_a    : input  logic<32> [A],
    in_b    : input  logic<32> [B],
    out_data: output logic<32> [A + B],
) {
    inst merger: CompareMatrixMergerInt32 #(A: A, B: B) (in_a, in_b, out_data);
}
    "#;
    let code = format!("{COMPARE_MATRIX_CODE}\n{top}");

    let mut sim = Simulator::builder(&code, "Top").build().unwrap();
    let a0 = sim.signal("in_a[0]");
    let a1 = sim.signal("in_a[1]");
    let a2 = sim.signal("in_a[2]");
    let b0 = sim.signal("in_b[0]");
    let b1 = sim.signal("in_b[1]");
    let o0 = sim.signal("out_data[0]");
    let o1 = sim.signal("out_data[1]");
    let o2 = sim.signal("out_data[2]");
    let o3 = sim.signal("out_data[3]");
    let o4 = sim.signal("out_data[4]");

    // Sorted inputs: A=[50, 30, 10], B=[40, 20]
    // Merged sorted descending: [50, 40, 30, 20, 10]
    sim.modify(|io| {
        io.set(a0, 50u32);
        io.set(a1, 30u32);
        io.set(a2, 10u32);
        io.set(b0, 40u32);
        io.set(b1, 20u32);
    })
    .unwrap();

    assert_eq!(sim.get(o0), 50u32.into());
    assert_eq!(sim.get(o1), 40u32.into());
    assert_eq!(sim.get(o2), 30u32.into());
    assert_eq!(sim.get(o3), 20u32.into());
    assert_eq!(sim.get(o4), 10u32.into());
}
