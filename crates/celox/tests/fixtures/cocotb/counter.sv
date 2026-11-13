module Child (
    input logic i_clk,
    input logic i_rst,
    input logic [7:0] d,
    output logic [7:0] q
);
    always_ff @(posedge i_clk or negedge i_rst) begin
        if (!i_rst) q <= '0;
        else q <= d;
    end
endmodule

module Top (
    input logic i_clk,
    input logic i_rst,
    input logic [7:0] d,
    output logic [7:0] q
);
    Child child (.i_clk(i_clk), .i_rst(i_rst), .d(d), .q(q));
endmodule
