module top; typedef struct { int a; } s_t; s_t s; initial begin s.a = 1; $display("%0d", s.a); end endmodule
