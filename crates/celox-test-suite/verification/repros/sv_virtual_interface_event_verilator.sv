interface bus_if(input logic clk); endinterface
module Top;
    logic fast = 0;
    always #1 fast = ~fast;
    bus_if a(fast);
    virtual bus_if va;
    initial begin
        va = a;
        @(posedge va.clk);
        $display("edge %0d", $time);
        $finish;
    end
endmodule
