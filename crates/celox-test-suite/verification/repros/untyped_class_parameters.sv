// A class parameter without a type takes the type of the value that a
// specialization assigns it (IEEE 1800-2023 6.20.2, 8.25). Expected output:
// 8 32.
module Top;
    class Sized #(parameter V = 1);
        function int bits();
            return $bits(V);
        endfunction
    endclass
    Sized #(8'h1) narrow;
    Sized #(32'h1) wide;
    initial begin
        narrow = new;
        wide = new;
        $display("%0d %0d", narrow.bits(), wide.bits());
    end
endmodule
