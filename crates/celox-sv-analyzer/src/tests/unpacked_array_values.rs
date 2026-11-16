//! Values that are no unpacked array given to an unpacked array (IEEE
//! 1800-2023 7.6).
use super::*;

fn module(body: &str) -> Result<Ir, AnalyzerError> {
    let source = format!("module test; {body} initial $display(\"x\"); endmodule");
    analyze_source(&source, Path::new("arrays.sv"))
}

#[test]
fn rejects_continuous_assignments_of_scalars_to_unpacked_arrays() {
    for body in [
        "wire [1:0] x[1:0]; assign x = 1'b1 + 1'b1;",
        "wire [1:0] x[1:0]; reg [1:0] y; assign x = y;",
        "wire a[1:0]; wire x; assign a = x;",
        "wire a[1:0]; wire x[1:0]; assign a = x[0];",
        "parameter j = 0; reg [3:0] in [7:0]; wire [3:0] out [7:0]; \
         assign out[(j+1)*4 - 1 : j*4] = in[j];",
    ] {
        let error = module(body).unwrap_err();
        assert!(
            matches!(error.source_error(), AnalyzerError::NotUnpackedArray { .. }),
            "{body}: {error}"
        );
    }
}

#[test]
fn accepts_unpacked_array_values_of_unpacked_arrays() {
    for body in [
        "wire [1:0] x[1:0]; wire [1:0] y[1:0]; assign x = y;",
        "wire a[1:0]; wire x[1:0][1:0]; assign a = x[0];",
        "wire [1:0] x[1:0]; assign x = '{2'b01, 2'b10};",
        "wire [1:0] x[1:0]; assign x = '{default: 2'b11};",
        "wire [1:0] x[1:0]; wire [1:0] p, q; assign x = {p, q};",
        "wire [3:0] out [7:0]; reg [3:0] in [7:0]; assign out[3:0] = in[7:4];",
        "wire [3:0] out [7:0]; reg [3:0] in [7:0]; assign out[0] = in[0];",
        "wire [1:0] x[1:0]; wire [1:0] y[1:0], z[1:0]; wire s; assign x = s ? y : z;",
        // A string literal fills an unpacked array of bytes (5.9).
        "byte b[3:0] = \"hi2\";",
    ] {
        module(body).unwrap_or_else(|error| panic!("{body}: {error}"));
    }
}
