
module Top;
 bit d=1;
 logic q=0, mirror=0;
 int activations=0;
 function automatic logic sample_data(); mirror <= d; return d; endfunction
 always_latch begin activations++; q <= sample_data() ^ mirror; end
 initial begin
   #1; if (q !== 1 || mirror !== 1 || activations != 1) $fatal(1,"function write self trigger");
   d=0;
   #1; if (q !== 1 || mirror !== 0 || activations != 2) $fatal(1,"function write sensitivity");
   $finish;
 end
endmodule