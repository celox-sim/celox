

module gray_encoder #(

    parameter int unsigned WIDTH = 32
) (

    input var logic [WIDTH-1:0] i_bin,

    output var logic [WIDTH-1:0] o_gray
);
    always_comb o_gray = i_bin ^ (i_bin >> 1);
endmodule

module gray_decoder #(

    parameter int unsigned WIDTH = 1
) (

    input var logic [WIDTH-1:0] i_gray,

    output var logic [WIDTH-1:0] o_bin
);
    if (WIDTH == 1) begin :g_base
        always_comb o_bin = i_gray;
    end else begin :g_base
        localparam int unsigned BWIDTH = WIDTH / 2;
        localparam int unsigned TWIDTH = WIDTH - BWIDTH;

        logic [TWIDTH-1:0] top_in ; always_comb top_in  = i_gray[WIDTH - 1:BWIDTH];
        logic [TWIDTH-1:0] top_out;

        gray_decoder #(
            .WIDTH (TWIDTH)
        ) u_top (
            .i_gray (top_in ),
            .o_bin  (top_out)
        );

        logic [BWIDTH-1:0] bot_in ; always_comb bot_in  = i_gray[BWIDTH - 1:0];
        logic [BWIDTH-1:0] bot_out;

        logic [BWIDTH-1:0] bot_red; always_comb bot_red = bot_out ^ {BWIDTH{top_out[0]}};

        gray_decoder #(
            .WIDTH (BWIDTH)
        ) u_bot (
            .i_gray (bot_in ),
            .o_bin  (bot_out)
        );

        always_comb o_bin = {top_out, bot_red};
    end
endmodule

module Top (
    input  var logic [32-1:0] i_bin ,
    output var logic [32-1:0] o_gray,
    output var logic [32-1:0] o_bin 
);
    gray_encoder #(
        .WIDTH  (32    )
    ) u_enc (
        .i_bin  (i_bin ),
        .o_gray (o_gray)
    );
    gray_decoder #(
        .WIDTH  (32    )
    ) u_dec (
        .i_gray (o_gray),
        .o_bin  (o_bin )
    );
endmodule