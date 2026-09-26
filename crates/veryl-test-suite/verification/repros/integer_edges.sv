// Adapter-free reproduction for the two simulators' arithmetic disagreements.
// Values come from plusargs so constant folding cannot replace the runtime op.
module Child(input var logic [8:0] sum, output var logic [8:0] y);
    always_comb y = sum;
endmodule
module Top;
    longint a, b;
    logic [7:0] x, z;
    logic [8:0] y;
    longint q;
    always_comb q = a / b;
    Child child(.sum(x + z), .y(y));
    initial begin
        if (!$value$plusargs("a=%h", a)) $fatal(1, "supply +a=<hex>");
        if (!$value$plusargs("b=%h", b)) $fatal(1, "supply +b=<hex>");
        if (!$value$plusargs("x=%h", x)) $fatal(1, "supply +x=<hex>");
        if (!$value$plusargs("z=%h", z)) $fatal(1, "supply +z=<hex>");
        #1;
        $display("signed_division=%h port_sum=%h", q, y);
        $finish;
    end
endmodule
