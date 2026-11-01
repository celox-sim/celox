// IEEE 1800-2023 10.4.2: evaluate the destination index when scheduling.
// Expected: r0=ab r1=00 same=1. Verilator 5.052 prints r0=00 r1=ab same=0.

    module Top(output bit same);
      typedef struct { bit [128:0] data; bit [2:0] tag; } row_t;
      row_t rows[2] = '{default: '{129'h100000000000000000000000000000001, 3'd2}};
      row_t copy;
      int index = 0;
      initial begin
        rows[index].data[67:60] <= 8'hab;
        index = 1;
        copy = rows[1];
        #1;
        same = rows[0].data[128] && rows[0].data[67:60] == 8'hab
          && rows[0].data[0] && rows[0].tag == 2
          && rows[1] == copy && rows[1].data[67:60] == 0;
        $display("r0=%h r1=%h same=%b", rows[0].data[67:60], rows[1].data[67:60], same);
        $finish;
      end
    endmodule
