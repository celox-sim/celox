`timescale 1ns/1ps
module Top(input logic clk, input logic d, output logic q);
    initial q = 0;
    always_ff @(posedge clk) q <= d;
endmodule
