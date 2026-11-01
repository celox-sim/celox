// IEEE 1800-2023 10.9.2: copy the matching array-valued default, retaining X/Z and value
// semantics. Expected dst=10xz,z001 same=1. Xezim 0.11.4 prints dst=xxxx,xxxx same=0; the
// default fails to copy the source array.

    module Top(output bit same);
      typedef struct { logic [3:0] a[2]; } row_t;
      row_t src = '{'{4'b10xz, 4'bz001}}, dst;
      initial begin
        dst = '{default: src.a};
        src.a[0] = 0;
        same = dst.a[0] === 4'b10xz && dst.a[1] === 4'bz001;
        $display("dst=%b,%b same=%b", dst.a[0], dst.a[1], same);
        $finish;
      end
    endmodule
