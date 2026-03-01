use celox::{BigUint, Simulator};

/// Helper: pack 32-bit values into a single BigUint for an array port.
/// Element 0 occupies the least-significant bits.
fn pack_u32(values: &[u32]) -> BigUint {
    let mut result = BigUint::from(0u32);
    for (i, &v) in values.iter().enumerate() {
        result |= BigUint::from(v) << (i * 32);
    }
    result
}

/// Helper: extract a 32-bit element from a packed BigUint array.
fn extract_u32(packed: &BigUint, index: usize) -> u32 {
    let shifted = packed >> (index * 32);
    let mask = BigUint::from(u32::MAX);
    u32::try_from(shifted & mask).unwrap_or(0)
}

/// Helper: extract an N-bit element from a packed BigUint array.
fn extract_bits(packed: &BigUint, index: usize, width: usize) -> u32 {
    let shifted = packed >> (index * width);
    let mask = (BigUint::from(1u32) << width) - BigUint::from(1u32);
    u32::try_from(shifted & mask).unwrap_or(0)
}

/// Helper: pack N-bit values into a single BigUint.
fn pack_bits(values: &[u32], width: usize) -> BigUint {
    let mut result = BigUint::from(0u32);
    for (i, &v) in values.iter().enumerate() {
        result |= BigUint::from(v) << (i * width);
    }
    result
}

/// Tests that two different packages can instantiate the same generic module,
/// each getting a unique ModuleId.
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

    sim.modify(|io| {
        io.set(a, 0xABu8);
        io.set(c, 0x1234u16);
    })
    .unwrap();
    assert_eq!(sim.get(b), 0xABu8.into());
    assert_eq!(sim.get(d), 0x1234u16.into());

    sim.modify(|io| {
        io.set(a, 0xFFu8);
        io.set(c, 0xFFFFu16);
    })
    .unwrap();
    assert_eq!(sim.get(b), 0xFFu8.into());
    assert_eq!(sim.get(d), 0xFFFFu16.into());
}

/// Tests proto package function resolution (E::gt → IntElement::gt).
#[test]
fn test_proto_function_basic() {
    let code = r#"
proto package Element {
    type data;
    function gt(
        a: input data,
        b: input data,
    ) -> logic;
}

package IntElement for Element {
    type data = logic<8>;
    function gt(
        a: input data,
        b: input data,
    ) -> logic {
        return a >: b;
    }
}

module GenericCompare::<E: Element> (
    a: input  E::data,
    b: input  E::data,
    r: output logic,
) {
    always_comb {
        r = E::gt(a, b);
    }
}

module Top (
    a: input  logic<8>,
    b: input  logic<8>,
    r: output logic,
) {
    inst inner: GenericCompare::<IntElement> (a, b, r);
}
    "#;

    let mut sim = Simulator::builder(code, "Top").build().unwrap();
    let a = sim.signal("a");
    let b = sim.signal("b");
    let r = sim.signal("r");

    sim.modify(|io| {
        io.set(a, 10u8);
        io.set(b, 5u8);
    })
    .unwrap();
    assert_eq!(sim.get(r), 1u8.into()); // 10 > 5

    sim.modify(|io| {
        io.set(a, 3u8);
        io.set(b, 7u8);
    })
    .unwrap();
    assert_eq!(sim.get(r), 0u8.into()); // 3 > 7 is false
}

/// Shared Veryl source for the compare matrix sorter tests.
/// Uses proto package functions (`E::gt`, `E::ge`) and constants (`E::max_value`).
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
    inst inner: CompareMatrixStage1CM::<IntElement> #(P: P) (in_data, out_score);
}

module CompareMatrixSelectorInt32 #(
    param P: u32 = 32,
) (
    in_data  : input  logic<32>         [P],
    in_scores: input  logic<$clog2(P)> [P],
    out_data : output logic<32>        [P],
) {
    inst inner: CompareMatrixSelector::<IntElement> #(P: P) (in_data, in_scores, out_data);
}

module CompareMatrixStage1Int32 #(
    param P: u32 = 32,
) (
    in_data : input  logic<32> [P],
    out_data: output logic<32> [P],
) {
    inst inner: CompareMatrixStage1::<IntElement> #(P: P) (in_data, out_data);
}

module CompareMatrixMergerInt32 #(
    param A: u32 = 32,
    param B: u32 = 10,
) (
    in_a    : input  logic<32> [A],
    in_b    : input  logic<32> [B],
    out_data: output logic<32> [A + B],
) {
    inst inner: CompareMatrixMerger::<IntElement> #(A: A, B: B) (in_a, in_b, out_data);
}
"#;

/// Tests proto constant (E::max_value) resolution.
#[test]
fn test_proto_const_max_value() {
    let code = format!(
        "{COMPARE_MATRIX_CODE}\n{}",
        r#"
module TestConst::<E: Element> (
    out: output E::data,
) {
    assign out = E::max_value;
}

module Top (
    out: output logic<32>,
) {
    inst t: TestConst::<IntElement> (out);
}
    "#
    );

    let mut sim = Simulator::builder(&code, "Top").build().unwrap();
    let out = sim.signal("out");
    assert_eq!(
        sim.get(out),
        BigUint::from(u32::MAX),
        "E::max_value should be ~0 = 0xFFFFFFFF"
    );
}

/// Tests the compare matrix scoring module (CompareMatrixStage1CM).
/// Input 4 values, verify scores reflect sorted order.
#[test]
fn test_compare_matrix_stage1cm() {
    let code = format!(
        "{COMPARE_MATRIX_CODE}\n{}",
        r#"
module Top (
    in_data  : input  logic<32>        [4],
    out_score: output logic<2>         [4],
) {
    inst cm: CompareMatrixStage1CM::<IntElement> #(P: 4) (in_data, out_score);
}
    "#
    );

    let mut sim = Simulator::builder(&code, "Top").build().unwrap();
    let in_data = sim.signal("in_data");
    let out_score = sim.signal("out_score");

    // Input: [10, 40, 20, 30] → scores: 10→0, 40→3, 20→1, 30→2
    sim.modify(|io| {
        io.set_wide(in_data, pack_u32(&[10, 40, 20, 30]));
    })
    .unwrap();

    let scores = sim.get(out_score);
    assert_eq!(extract_bits(&scores, 0, 2), 0); // 10 is smallest → score 0
    assert_eq!(extract_bits(&scores, 1, 2), 3); // 40 is largest  → score 3
    assert_eq!(extract_bits(&scores, 2, 2), 1); // 20 → score 1
    assert_eq!(extract_bits(&scores, 3, 2), 2); // 30 → score 2
}

/// Tests the compare matrix selector module (through wrapper, verifying parameter forwarding).
#[test]
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
    let in_data = sim.signal("in_data");
    let in_scores = sim.signal("in_scores");
    let out_data = sim.signal("out_data");

    // Data: [10, 40, 20, 30], Scores: [0, 3, 1, 2]
    // Output should place each value at its score index
    sim.modify(|io| {
        io.set_wide(in_data, pack_u32(&[10, 40, 20, 30]));
        io.set_wide(in_scores, pack_bits(&[0, 3, 1, 2], 2));
    })
    .unwrap();

    let out = sim.get(out_data);
    assert_eq!(extract_u32(&out, 0), 10); // score 0 → slot 0
    assert_eq!(extract_u32(&out, 1), 20); // score 1 → slot 1
    assert_eq!(extract_u32(&out, 2), 30); // score 2 → slot 2
    assert_eq!(extract_u32(&out, 3), 40); // score 3 → slot 3
}

/// Tests full sorting via CompareMatrixStage1 (scoring + selection through wrapper chain).
/// Input unsorted values, output sorted ascending.
#[test]
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
    let in_data = sim.signal("in_data");
    let out_data = sim.signal("out_data");

    // Input: [10, 40, 20, 30] → sorted ascending: [10, 20, 30, 40]
    sim.modify(|io| {
        io.set_wide(in_data, pack_u32(&[10, 40, 20, 30]));
    })
    .unwrap();

    let out = sim.get(out_data);
    assert_eq!(extract_u32(&out, 0), 10);
    assert_eq!(extract_u32(&out, 1), 20);
    assert_eq!(extract_u32(&out, 2), 30);
    assert_eq!(extract_u32(&out, 3), 40);
}

/// Tests the compare matrix merger.
/// Two sorted ascending arrays in, one merged sorted ascending array out.
#[test]
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
    let in_a = sim.signal("in_a");
    let in_b = sim.signal("in_b");
    let out_data = sim.signal("out_data");

    // Sorted ascending inputs: A=[10, 30, 50], B=[20, 40]
    // Merged sorted ascending: [10, 20, 30, 40, 50]
    sim.modify(|io| {
        io.set_wide(in_a, pack_u32(&[10, 30, 50]));
        io.set_wide(in_b, pack_u32(&[20, 40]));
    })
    .unwrap();

    let out = sim.get(out_data);
    assert_eq!(extract_u32(&out, 0), 10);
    assert_eq!(extract_u32(&out, 1), 20);
    assert_eq!(extract_u32(&out, 2), 30);
    assert_eq!(extract_u32(&out, 3), 40);
    assert_eq!(extract_u32(&out, 4), 50);
}
