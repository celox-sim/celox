
module Top;
    logic en = 1;
    logic [7:0] d = 7, q = 0;
    always @* if (en) q = d;
    initial begin
        #1; if (q !== 0) $fatal(1, "declarations do not trigger wildcard process");
        d = 13;
        #1; if (q !== 13) $fatal(1, "later data change triggers wildcard process");
        $finish;
    end
endmodule
