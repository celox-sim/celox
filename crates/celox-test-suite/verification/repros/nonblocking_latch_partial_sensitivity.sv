
module Top;
 logic [7:0] value=8'h21;
 int activations=0;
 always_latch begin activations++; value[3:0] <= value[7:4]; end
 initial begin
   #1; if (value !== 8'h22 || activations != 1) $fatal(1,"partial startup");
   value[7:4]=4'ha;
   #1; if (value !== 8'haa || activations != 2) $fatal(1,"partial sensitivity");
   $finish;
 end
endmodule