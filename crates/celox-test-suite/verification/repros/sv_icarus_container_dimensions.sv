// $dimensions of a queue and of a dynamic array.
// IEEE 1800-2023 20.7: each counts its unpacked dimension and the packed
// dimension of its element type, so both are 2. Icarus 13.0 prints 0 0.
module Top;
    int q[$];
    logic [7:0] d[];
    initial $display("%0d %0d", $dimensions(q), $dimensions(d));
endmodule
