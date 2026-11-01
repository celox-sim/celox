// IEEE 1800-2023 10.9.2: array-valued defaults matching array members are copied as whole
// values, while scalar defaults descend to array elements. Expected dst=11,22 nested=11,22
// values=11,22 selected=31,32 matrix=31,32,41,42 scalar=7,7 same=1. Xezim 0.11.4 prints
// zero for every array-valued default and scalar=7,7 same=0. Verilator 5.052 reports an
// internal error (V3Ast.cpp:665, Node has no back, already unlinked?) at values = default
// src.a.

    module Top(output bit same);
      typedef bit signed [31:0] element_t;
      typedef struct { element_t a[2]; } row_t;
      typedef struct { row_t inner; } nested_t;
      typedef struct { element_t a[3:2][5:4]; } source_t;
      typedef struct { element_t a[2][2]; } matrix_t;
      row_t src = '{'{11, 22}}, dst, selected, scalar;
      nested_t nested;
      source_t source_rows = '{'{'{31, 32}, '{41, 42}}};
      matrix_t matrix;
      element_t values[2][2];
      initial begin
        dst = '{default: src.a};
        nested = '{default: (src.a)};
        values = '{default: src.a};
        selected = '{default: source_rows.a[3]};
        matrix = '{default: source_rows.a[3:2]};
        scalar = '{default: 7};
        src.a[0] = 99;
        same = dst.a[0] == 11 && dst.a[1] == 22
          && nested.inner.a[0] == 11 && nested.inner.a[1] == 22
          && values[0][0] == 11 && values[1][1] == 22
          && selected.a[0] == 31 && selected.a[1] == 32
          && matrix.a[0][0] == 31 && matrix.a[0][1] == 32
          && matrix.a[1][0] == 41 && matrix.a[1][1] == 42
          && scalar.a[0] == 7 && scalar.a[1] == 7;
        $display("dst=%0d,%0d nested=%0d,%0d values=%0d,%0d selected=%0d,%0d matrix=%0d,%0d,%0d,%0d scalar=%0d,%0d same=%b", dst.a[0], dst.a[1], nested.inner.a[0], nested.inner.a[1], values[0][0], values[1][1], selected.a[0], selected.a[1], matrix.a[0][0], matrix.a[0][1], matrix.a[1][0], matrix.a[1][1], scalar.a[0], scalar.a[1], same);
        $finish;
      end
    endmodule
