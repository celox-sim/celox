module Top;
    logic sel;
    logic [4:0] a;
    logic signed [7:0] signed_arm;
    logic [31:0] bits_arm, unsigned_bits_arm;
    function automatic logic f(input logic [3:0] x);
        return x[2];
    endfunction
    always_comb bits_arm = sel ? $bits(a) : signed_arm;
    always_comb unsigned_bits_arm = sel ? $unsigned($bits(a)) : signed_arm;
    initial begin
        sel = 0; a = 0; signed_arm = 8'hff;
        #1;
        $display("review:bits_signed=%h bits_unsigned_control=%h", bits_arm, unsigned_bits_arm);
        $display("review:argument_context=%b argument_self_sized_control=%b", f(2'b11 + 2'b01), f({2'b11 + 2'b01}));
        sel = 1;
        #1;
        $display("review:bits_selected=%h", bits_arm);
        $finish;
    end
endmodule
