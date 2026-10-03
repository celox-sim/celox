// Tool health check only. Not CPU RTL and not a board-ready physical top.
module ToolSmoke(input clk, input [7:0] d, output reg [7:0] q);
    reg [7:0] stage;
    always @(posedge clk) begin stage <= d; q <= stage + 8'd1; end
endmodule
