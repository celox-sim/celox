// Equivalent of the Veryl benchmark module used by Celox:
//   N=1000 parallel 32-bit counters (active-high async reset).
module Top #(
    parameter N = 1000
)(
    input  logic        clk,
    input  logic        rst,
    output logic [31:0] cnt [0:N-1]
);
    for (genvar i = 0; i < N; i++) begin : g
        always_ff @(posedge clk or posedge rst) begin
            if (rst)
                cnt[i] <= 32'd0;
            else
                cnt[i] <= cnt[i] + 32'd1;
        end
    end
endmodule
