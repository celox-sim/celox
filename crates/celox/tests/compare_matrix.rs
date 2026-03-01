use celox::Simulator;

#[test]
#[ignore]
fn test_compare_matrix_sort() {
    let code = r#"
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

module Top #(
    param P: u32 = 4,
) (
    in_data : input  logic<32> [P],
    out_data: output logic<32> [P],
) {
    inst sorter: CompareMatrixStage1Int32 #(
        P: P,
    ) (
        in_data : in_data,
        out_data: out_data,
    );
}
    "#;

    let result = Simulator::builder(code, "Top").build();
    match &result {
        Ok(_) => println!("Build succeeded"),
        Err(e) => println!("Build error: {e}"),
    }
    result.unwrap();
}
