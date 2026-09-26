module Top;
    logic [31:0] start, stop;
    logic signed [31:0] signed_start, signed_stop;
    logic clk;
    logic [31:0] hits, signed_hits, sum, signed_sum, last, signed_last;
    always_comb begin
        hits = 0; sum = 0;
        for (int i = start; i <= stop; ++i) begin
            hits += 1; sum += i;
        end
        signed_hits = 0; signed_sum = 0;
        for (int i = signed_start; i <= signed_stop; ++i) begin
            signed_hits += 1; signed_sum += i;
        end
    end
    always_ff @(posedge clk) begin
        last <= 32'hdeadbeef;
        for (int i = start; i <= stop; ++i) last <= i;
        signed_last <= 32'hdeadbeef;
        for (int i = signed_start; i <= signed_stop; ++i) signed_last <= i;
    end
    initial begin
        clk = 0; start = 32'hffffffff; stop = 1;
        signed_start = -1; signed_stop = 1;
        #1; clk = 1; #1;
        $display("review:unsigned_hits=%d unsigned_sum=%h unsigned_last=%h", hits, sum, last);
        $display("review:signed_control_hits=%d signed_control_sum=%h signed_control_last=%h", signed_hits, signed_sum, signed_last);
        $finish;
    end
endmodule
