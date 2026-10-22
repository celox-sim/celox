module Top;
    logic [7:0] a = 8'd0;
    logic [7:0] b = 8'd0;
    logic c = 1'b0;
    logic [7:0] y;
    initial y = 8'd1;
    always @(b, a) y = a + b;
    always @(a or b or c) $display("sum=%0d", a + b);
    initial begin
        #1; if (y !== 1) $fatal(1, "declarations do not trigger event list process");
        a = 8'd3;
        #1; if (y !== 3) $fatal(1, "later change triggers event list process");
        $finish;
    end
endmodule
