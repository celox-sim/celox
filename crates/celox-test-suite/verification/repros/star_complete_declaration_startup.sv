
module Top;
    logic a = 1, q = 0;
    always @* q = a;
    initial begin
        #1; if (q !== 0) $fatal(1, "complete wildcard process waits after declaration initialization");
        a = 0;
        #1; a = 1;
        #1; if (q !== 1) $fatal(1, "complete wildcard process observes later source changes");
        $finish;
    end
endmodule
