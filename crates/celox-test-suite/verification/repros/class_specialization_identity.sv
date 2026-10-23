// Two parameter value assignments that give a parameterized class the same
// values name one specialization, which has one copy of each static property
// (IEEE 1800-2023 8.25). Expected output: 1 1 1.
module Top;
    class Box #(int N = 4, type T = bit [7:0]);
        static int made;
        function new();
            made++;
        endfunction
    endclass
    Box #(.T(int), .N(2)) box;
    initial begin
        box = new;
        $display("%0d %0d %0d", Box#(2, int)::made, Box#(.T(int), .N(2))::made, Box#(.N(2), .T(int))::made);
    end
endmodule
