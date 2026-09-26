module Top;
    logic [127:0] a, shifted;
    logic [7:0] sh;
    always_comb shifted = a >> sh;
    initial begin
        // The original payload=0xaa/mask=0xff at bits 71:64 encodes X/Z.
        a = {56'b0, 8'bxzxzxzxz, 64'h55}; sh = 0;
        #1;
        $display("review:shift0=%b", shifted[71:64]);
        sh = 64; #1;
        $display("review:shift64=%b", shifted[7:0]);
        // Control: an all-X input does match the original test's description.
        a[71:64] = 8'bxxxxxxxx; sh = 0; #1;
        $display("review:all_x_control=%b", shifted[71:64]);
        $finish;
    end
endmodule
