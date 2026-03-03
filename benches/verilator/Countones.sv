module countones #(
    parameter  int unsigned W     = 16                                   ,
    localparam int unsigned CLOGW = ((W > 1) ? ( $clog2(W) + 1 ) : ( 1 ))
) (
    input  var logic [W-1:0]     i_data,
    output var logic [CLOGW-1:0] o_ones
);
    if ((W == 1)) begin :gen_base_case
        always_comb o_ones = i_data;
    end else begin :gen_rec_case
        localparam int unsigned            WBOT     = W / 2;
        localparam int unsigned            WTOP     = W - WBOT;
        logic        [WBOT-1:0] data_bot; always_comb data_bot = i_data[WBOT - 1:0];
        logic        [WTOP-1:0] data_top; always_comb data_top = i_data[W - 1:WBOT];

        localparam int unsigned                CLOGWBOT = ((WBOT > 1) ? ( $clog2(WBOT) + 1 ) : ( 1 ));
        localparam int unsigned                CLOGWTOP = ((WTOP > 1) ? ( $clog2(WTOP) + 1 ) : ( 1 ));
        logic        [CLOGWBOT-1:0] ones_bot;
        logic        [CLOGWTOP-1:0] ones_top;

        countones #(
            .W (WBOT)
        ) u_bot (
            .i_data (data_bot),
            .o_ones (ones_bot)
        );
        countones #(
            .W (WTOP)
        ) u_top (
            .i_data (data_top),
            .o_ones (ones_top)
        );
        always_comb o_ones = {{(W - WTOP){1'b0}}, ones_top} + {{(W - WBOT){1'b0}}, ones_bot};

    end
endmodule

module Top (
    input  var logic [64-1:0] i_data,
    output var logic [7-1:0]  o_ones
);
    countones #(
        .W      (64    )
    ) u (
        .i_data (i_data),
        .o_ones (o_ones)
    );
endmodule