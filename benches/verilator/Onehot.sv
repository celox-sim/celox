module onehot #(
    parameter int unsigned W = 16
) (
    input var logic [W-1:0] i_data,

    output var logic o_onehot,

    output var logic o_zero
);
    logic o_gt_one;

    _onehot #(
        .W (W)
    ) u_onehot (
        .i_data   (i_data  ),
        .o_zero   (o_zero  ),
        .o_onehot (o_onehot),
        .o_gt_one (o_gt_one)
    );
endmodule

module _onehot #(
    parameter int unsigned W = 16
) (
    input  var logic [W-1:0] i_data  ,
    output var logic         o_onehot,
    output var logic         o_zero  ,
    output var logic         o_gt_one
);
    if ((W == 1)) begin :gen_base_case
        always_comb o_onehot = i_data;
        always_comb o_zero   = ~i_data;
    end else begin :gen_rec_case
        localparam int unsigned            WBOT       = W / 2;
        localparam int unsigned            WTOP       = W - WBOT;
        logic        [WBOT-1:0] data_bot  ; always_comb data_bot   = i_data[WBOT - 1:0];
        logic        [WTOP-1:0] data_top  ; always_comb data_top   = i_data[W - 1:WBOT];
        logic                   onehot_top;
        logic                   onehot_bot;
        logic                   zero_top  ;
        logic                   zero_bot  ;
        logic                   gt_one_top;
        logic                   gt_one_bot;

        _onehot #(
            .W (WBOT)
        ) u_bot (
            .i_data   (data_bot  ),
            .o_onehot (onehot_bot),
            .o_zero   (zero_bot  ),
            .o_gt_one (gt_one_bot)
        );
        _onehot #(
            .W (WTOP)
        ) u_top (
            .i_data   (data_top  ),
            .o_onehot (onehot_top),
            .o_zero   (zero_top  ),
            .o_gt_one (gt_one_top)
        );
        always_comb o_zero   = zero_top & zero_bot;
        always_comb o_onehot = (onehot_top ^ onehot_bot) & ~gt_one_top & ~gt_one_bot;
        always_comb o_gt_one = gt_one_top | gt_one_bot | (onehot_top & onehot_bot);
    end

endmodule

module Top (
    input  var logic [64-1:0] i_data  ,
    output var logic          o_onehot,
    output var logic          o_zero  
);
    onehot #(
        .W        (64      )
    ) u (
        .i_data   (i_data  ),
        .o_onehot (o_onehot),
        .o_zero   (o_zero  )
    );
endmodule