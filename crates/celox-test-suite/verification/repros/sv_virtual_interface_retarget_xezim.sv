interface bus_if(input logic clk); endinterface
module Top;
    logic low = 0;
    logic high = 1;
    bus_if lo(low);
    bus_if hi(high);
    virtual bus_if moved;
    initial begin
        moved = lo;
        fork
            begin @(posedge moved.clk); $display("moved %0d", $time); end
            begin #2 moved = hi; end
        join
        $finish;
    end
    initial #10 begin $display("timeout"); $finish; end
endmodule
