// IEEE 1800-2023 6.5 permits primitive outputs to independent struct elements;
// the assignment applies the destination's automatic type conversion.
typedef struct packed { bit clean; logic other; } pair_t;
module Top(input logic a, output pair_t y);
    and (y.clean, a);
    assign y.other = 1'b0;
endmodule
