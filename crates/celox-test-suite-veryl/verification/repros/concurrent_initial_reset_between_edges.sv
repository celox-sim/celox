module Top;
    logic slow = 0;
    logic rst = 1;
    logic [7:0] count = 0;
    always @(posedge slow or posedge rst)
        if (rst) count <= 0;
        else count <= count + 8'd1;
    initial begin
        slow = 1; #5; slow = 0;
        #5; rst = 0; slow = 1;
        #5; slow = 0;
    end
    initial begin
        #12;
        if (count !== 8'd1) $fatal(1, "before reset: count=%0d", count);
        rst = 1;
    end
    initial begin
        #14;
        if (count !== 8'd0) $fatal(1, "between edges: count=%0d", count);
        $display("PASS time 14: count=%0d", count);
        $finish;
    end
endmodule
