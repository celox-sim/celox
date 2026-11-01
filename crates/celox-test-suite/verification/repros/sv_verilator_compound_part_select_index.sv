// IEEE 1800-2023 11.4.1: an assignment operator evaluates any left-hand index
// expression only once. next_bit() returns 2 and counts its calls, so both
// lines print 04 3. Verilator 5.052 calls it twice for the indexed
// part-select and prints part 08 4; the bit-select is right.
module Top;
    logic [7:0] v;
    int n;
    function automatic int next_bit();
        n++;
        return n - 1;
    endfunction
    initial begin
        v = 8'h00;
        n = 2;
        v[next_bit() +: 2] += 2'd1;
        $display("part %h %0d", v, n);
        v = 8'h00;
        n = 2;
        v[next_bit()] += 1'b1;
        $display("bit %h %0d", v, n);
    end
endmodule
