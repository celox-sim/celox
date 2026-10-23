
module Top;
    logic trigger = 0, en = 0, g = 0;
    logic q_comb = 0, q_latch = 0;
    task copy_comb(input logic unused);
        q_comb = g;
    endtask
    task copy_latch(input logic unused);
        if (en) q_latch = g;
    endtask
    always_comb copy_comb(trigger);
    always_latch copy_latch(trigger);
    initial begin
        #1; en = 1; g = 1;
        #1;
        if (q_comb !== 0 || q_latch !== 0) $fatal(1, "task body globals do not add sensitivity");
        trigger = 1;
        #1;
        if (q_comb !== 1 || q_latch !== 1) $fatal(1, "task argument changes do add sensitivity");
        $finish;
    end
endmodule
