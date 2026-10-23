
module Latch(input wire a, output wire [7:0] q);
    logic [7:0] held = 0;
    assign q = held;
    always @* if (!a) held = held + 1;
endmodule
module Top(output wire [7:0] q);
    logic a = 0;
    wire b;
    assign b = a;
    Latch dut(b, q);
    initial begin
        #1; if (q !== 0) $fatal(1, "wildcard startup");
        a = 1; a = 0;
        #1; if (q !== 1) $fatal(1, "hierarchical sensitivity pulse");
        $finish;
    end
endmodule
