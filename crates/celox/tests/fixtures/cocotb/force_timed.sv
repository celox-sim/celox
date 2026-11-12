`timescale 1ns/1ps
module Top(output logic [7:0] x, output logic [7:0] y);
    logic clk = 0;
    always #5 clk = ~clk;
    initial x = 0;
    always_ff @(posedge clk) x <= x + 1;
    assign y = x + 8'd100;
endmodule
