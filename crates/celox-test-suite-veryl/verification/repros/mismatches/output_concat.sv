module Child(input var logic [1:0] i, output var logic [1:0] o);
    always_comb o = i;
endmodule
module Dut(input var logic [1:0] value, output var logic [1:0] mem, output var logic tmp);
    // Intentionally retains the nonconstant selection in the original case.
    Child child(.i(value), .o({mem[tmp], tmp}));
endmodule
module Top;
    logic [1:0] value;
    wire [1:0] mem;
    wire tmp;
    Dut dut(value, mem, tmp);
    initial begin
        value = 0; #1; value = 3; #1;
        $display("review:mem=%b tmp=%b", mem, tmp);
        $finish;
    end
endmodule
