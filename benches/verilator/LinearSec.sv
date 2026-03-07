
module linear_sec_encoder #(

    parameter int unsigned P = 4,

    parameter int unsigned K = (1 << P) - 1,

    parameter int unsigned N = K - P
) (
    input  var logic [N-1:0] i_word    ,
    output var logic [K-1:0] o_codeword
);

    logic [P-1:0][K-1:0] h;
    for (genvar p = 0; p < P; p++) begin :gen_vector
        for (genvar k = 0; k < K; k++) begin :gen_bit
            localparam int unsigned IDX     = k + 1;
            always_comb h[p][k] = IDX[p];
        end
    end

    logic [K-1:0] codeword_data_only;
    for (genvar k = 1; k < K + 1; k++) begin :gen_move_data
        localparam int unsigned CODEWORD_IDX = k - 1;
        if (!$onehot(k)) begin :gen_move_data_bit
            localparam int unsigned WORD_IDX                         = k - $clog2(k) - 1;
            always_comb codeword_data_only[CODEWORD_IDX] = i_word[WORD_IDX];
        end else begin :gen_move_data_bit
            always_comb codeword_data_only[CODEWORD_IDX] = 1'b0;
        end
    end

    logic [K-1:0] codeword_parity_only;
    for (genvar p = 0; p < P; p++) begin :gen_parities
        localparam int unsigned CODEWORD_IDX                       = (1 << p) - 1;
        always_comb codeword_parity_only[CODEWORD_IDX] = ^(h[p] & codeword_data_only);
    end
    for (genvar k = 0; k < K; k++) begin :gen_zeros
        if (!$onehot(k + 1)) begin :gen_zero_bit
            always_comb codeword_parity_only[k] = 1'b0;
        end
    end

    always_comb o_codeword = codeword_data_only | codeword_parity_only;
endmodule

module linear_sec_decoder #(

    parameter int unsigned P = 4,

    parameter int unsigned K = (1 << P) - 1,

    parameter int unsigned N = K - P
) (
    input  var logic [K-1:0] i_codeword,
    output var logic [N-1:0] o_word    ,

    output var logic o_corrected

);

    logic [(K + 1)-1:0] codeword          ; always_comb codeword           = {i_codeword, 1'b0};
    logic [(K + 1)-1:0] codeword_corrected;

    logic [P-1:0] errors;

    for (genvar idx = 1; idx < (K + 2); idx++) begin :g_create_word
        if (!$onehot(idx)) begin :g_data_bit
            localparam int unsigned CEIL             = $clog2(idx);
            localparam int unsigned WORD_IDX         = idx - 1 - CEIL;
            always_comb o_word[WORD_IDX] = codeword_corrected[idx];
        end
    end

    for (genvar pbit = 1; pbit < P + 1; pbit++) begin :g_check_parities
        localparam int unsigned               ONE_IDX_SET_BIT = pbit - 1;
        logic        [(K + 1)-1:0] masked_bits    ;
        always_comb masked_bits[0]  = 1'b0;
        for (genvar idx = 1; idx < K + 1; idx++) begin :g_check_bits
            if (idx[ONE_IDX_SET_BIT]) begin :g_take_parity
                always_comb masked_bits[idx] = codeword[idx];
            end else begin :g_take_parity
                always_comb masked_bits[idx] = 1'b0;
            end
        end
        always_comb errors[ONE_IDX_SET_BIT] = ^masked_bits;
    end

    always_comb o_corrected = |errors;
    always_comb begin
        codeword_corrected         =  codeword;
        codeword_corrected[errors] ^= 1;
    end
endmodule

module Top #(
    parameter  int unsigned P = 6           ,
    localparam int unsigned K = (1 << P) - 1,
    localparam int unsigned N = K - P   
) (
    input  var logic [N-1:0] i_word     ,
    output var logic [K-1:0] o_codeword ,
    output var logic [N-1:0] o_word     ,
    output var logic         o_corrected
);
    linear_sec_encoder #(
        .P (P)
    ) u_enc (
        .i_word     (i_word    ),
        .o_codeword (o_codeword)
    );
    linear_sec_decoder #(
        .P (P)
    ) u_dec (
        .i_codeword  (o_codeword ),
        .o_word      (o_word     ),
        .o_corrected (o_corrected)
    );
endmodule