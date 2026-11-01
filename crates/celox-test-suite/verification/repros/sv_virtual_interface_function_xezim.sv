interface bus_if(input logic clk);
    logic [7:0] data;
endinterface
module Top;
    logic clk = 0;
    bus_if first(clk);
    bus_if second(clk);
    virtual bus_if v;
    function automatic virtual bus_if pick(bit which);
        if (which) return second;
        return first;
    endfunction
    initial begin
        v = pick(1);
        v.data = 8'h66;
        $display("%h %0d %0d", second.data, v == pick(1), v == pick(0));
    end
endmodule
