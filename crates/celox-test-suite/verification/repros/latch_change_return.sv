
module Top;
    logic [7:0] qc = 0, ql = 0, qs = 0;
    logic a = 0;
    logic [1:0] bus = 0;
    always @* if (!a) qc = qc + 1;
    always_latch if (!a) ql = ql + 1;
    always_latch if (!bus[0]) qs = qs + 1;
    initial begin
        #1;
        if (qc !== 0 || ql !== 1 || qs !== 1) $fatal(1, "startup");
        a = 1; a = 0;
        #1;
        if (qc !== 1 || ql !== 2 || qs !== 1) $fatal(1, "whole sensitivity pulse");
        bus[0] = 1; bus[0] = 0;
        #1;
        if (qc !== 1 || ql !== 2 || qs !== 2) $fatal(1, "slice sensitivity pulse or own write retrigger");
        $finish;
    end
endmodule
