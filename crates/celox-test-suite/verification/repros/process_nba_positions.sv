module Top;
    logic [7:0] q = 8'd0;
    logic [13:0] p = 14'd0;
    int n, j;
    initial begin
        for (n = 0; n < 2; n++) begin
            for (j = n; j < n + 6; j++) p[2 * j +: 3] <= j;
            #1 $display("p=%h", p);
        end
        if (p !== 14'h24e4) $fatal(1, "nonblocking updates in issue order give 24e4, not %h", p);
        q[8] <= 1'b1;
        #1 $display("q=%h", q);
        if (q !== 8'h00) $fatal(1, "an update outside the target changed q to %h", q);
        $finish;
    end
endmodule
