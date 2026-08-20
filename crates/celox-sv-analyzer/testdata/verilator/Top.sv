module Top #(
    parameter int unsigned N = 1000
) (
    input  var logic          clk     ,
    input  var logic          rst     ,
    output var logic [32-1:0] cnt  [N],
    output var logic [32-1:0] cnt0
);
    always_comb cnt0 = cnt[0];
    for (genvar i = 0; i < N; i++) begin :g
        always_ff @ (posedge clk, negedge rst) begin
            if (!rst) begin
                cnt[i] <= 0;
            end else begin
                cnt[i] <= cnt[i] + (1);
            end
        end
    end
endmodule