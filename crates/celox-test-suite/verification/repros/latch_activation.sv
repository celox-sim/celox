module Latch(input logic seed, en, trigger, output logic [7:0] q);
    always_comb begin
        logic overwritten;
        overwritten = trigger;
        overwritten = 0;
        if (seed) q = 0;
        else if (en) q = q + 1;
    end
endmodule
module Top;
    logic seed = 1, en = 0, trigger = 0;
    wire [7:0] q;
    Latch dut(seed, en, trigger, q);
    initial begin
        #1;
        seed = 0; en = 1;
        #1;
        $display("after enabling: q=%0d", q);
        if (q !== 1) $fatal(1, "one activation expected");
        #1;
        if (q !== 1) $fatal(1, "unchanged inputs must not reactivate");
        trigger = 1;
        #1;
        if (q !== 2) $fatal(1, "overwritten syntactic read belongs to sensitivity");
        $finish;
    end
endmodule
