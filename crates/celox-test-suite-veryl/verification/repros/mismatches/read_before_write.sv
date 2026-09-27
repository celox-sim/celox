module Dut(input var logic a, output var logic c);
    // Explicit two-state storage gives the same zero initialization as the suite.
    bit b;
    always_comb begin
        c = b;
        b = a;
    end
endmodule
module Top;
    bit a;
    wire c;
    Dut dut(a, c);
    initial begin
        a = 0; #1;
        $display("review:step0=%b", c);
        a = 1; #1;
        $display("review:step1=%b", c);
        a = 0; #1;
        $display("review:step2=%b", c);
        $finish;
    end
endmodule
