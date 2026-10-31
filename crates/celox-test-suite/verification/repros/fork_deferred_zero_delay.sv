`timescale 1ns/1ns
module Top;
initial begin
 assert #0 (1) $display("observed");
 assert final (1) $display("final");
 #0; $display("after zero");
 #1; $finish;
end
endmodule
