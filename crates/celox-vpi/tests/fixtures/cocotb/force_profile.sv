module Top(input logic [7:0] a, output logic [7:0] x, total);
    always_comb begin
        total = a;
        for (int i = 0; i < 4; i++) begin
            x = 1;
            total += x;
        end
    end
endmodule
