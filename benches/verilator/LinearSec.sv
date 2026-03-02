// Hamming SEC encoder + decoder
// Equivalent to Veryl std linear_sec_encoder / linear_sec_decoder (P=6)
// K = (1<<P)-1 = 63  codeword bits
// N = K - P   = 57  data bits

module linear_sec_encoder #(
    parameter int P = 6,
    parameter int K = (1 << P) - 1,
    parameter int N = K - P
)(
    input  logic [N-1:0] i_word,
    output logic [K-1:0] o_codeword
);
    logic [K-1:0] d;           // data bits placed, parity positions = 0
    logic [K-1:0] parity_only; // parity bits placed, data positions = 0

    // Place data bits at non-power-of-2 positions (1-indexed)
    generate
        for (genvar k = 0; k < K; k++) begin : g_d
            localparam int pos    = k + 1;
            localparam bit IS_P2  = (pos & (pos - 1)) == 0;
            if (!IS_P2) begin : g_data
                // word_idx = pos - $clog2(pos) - 1  (matches Veryl formula)
                assign d[k] = i_word[pos - $clog2(pos) - 1];
            end else begin : g_zero
                assign d[k] = 1'b0;
            end
        end
    endgenerate

    // Parity bits: XOR over data bits whose position has bit p set (1-indexed)
    generate
        for (genvar p = 0; p < P; p++) begin : g_par
            logic pbit;
            always_comb begin
                pbit = 1'b0;
                for (int k = 0; k < K; k++) begin
                    if ((((k + 1) >> p) & 1) != 0)
                        pbit ^= d[k];
                end
            end
            assign parity_only[(1 << p) - 1] = pbit;
        end

        for (genvar k = 0; k < K; k++) begin : g_par_zero
            localparam int pos   = k + 1;
            localparam bit IS_P2 = (pos & (pos - 1)) == 0;
            if (!IS_P2) begin : g_zero
                assign parity_only[k] = 1'b0;
            end
        end
    endgenerate

    assign o_codeword = d | parity_only;
endmodule


module linear_sec_decoder #(
    parameter int P = 6,
    parameter int K = (1 << P) - 1,
    parameter int N = K - P
)(
    input  logic [K-1:0] i_codeword,
    output logic [N-1:0] o_word,
    output logic         o_corrected
);
    // 1-indexed codeword: codeword[0]=0, codeword[1..K]=i_codeword[0..K-1]
    logic [K:0] codeword;
    assign codeword = {i_codeword, 1'b0};

    logic [P-1:0] errors;
    logic [K:0]   codeword_corrected;

    // Compute syndrome: errors[p] = XOR of codeword[idx] where bit p of idx is set
    generate
        for (genvar p = 0; p < P; p++) begin : g_syndrome
            always_comb begin
                errors[p] = 1'b0;
                for (int idx = 1; idx <= K; idx++) begin
                    if (((idx >> p) & 1) != 0)
                        errors[p] ^= codeword[idx];
                end
            end
        end
    endgenerate

    // Correct single-bit error (errors == 0 flips unused padding bit 0)
    always_comb begin
        codeword_corrected         = codeword;
        codeword_corrected[errors] ^= 1'b1;
    end

    assign o_corrected = |errors;

    // Extract data bits from corrected codeword (non-power-of-2 positions)
    generate
        for (genvar idx = 1; idx <= K; idx++) begin : g_word
            localparam bit IS_P2 = (idx & (idx - 1)) == 0;
            if (!IS_P2) begin : g_data
                assign o_word[idx - 1 - $clog2(idx)] = codeword_corrected[idx];
            end
        end
    endgenerate
endmodule


module LinearSecTop #(
    parameter int P = 6,
    parameter int K = (1 << P) - 1,
    parameter int N = K - P
)(
    input  logic [N-1:0] i_word,
    output logic [K-1:0] o_codeword,
    output logic [N-1:0] o_word,
    output logic         o_corrected
);
    linear_sec_encoder #(.P(P)) u_enc (
        .i_word    (i_word),
        .o_codeword(o_codeword)
    );
    linear_sec_decoder #(.P(P)) u_dec (
        .i_codeword(o_codeword),
        .o_word    (o_word),
        .o_corrected(o_corrected)
    );
endmodule
