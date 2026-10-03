module Top;
    logic a = 0;
    logic b = 1;
    logic [7:0] x = 0;
    logic [7:0] y = 0;
    wire inverted = ~b;
    always @(posedge a) x <= y + 8'd1;
    always @(posedge inverted) y <= x + 8'd1;
    initial begin
        a = 1; #2; a = 0; #1;
        a = 1; #2; a = 0;
    end
    initial begin
        #3; b = 0;
    end
    initial begin
        #6;
        if (x !== 8'd1 || y !== 8'd2) $fatal(1, "x=%0d y=%0d", x, y);
        $display("PASS x=%0d y=%0d", x, y);
        $finish;
    end
endmodule
