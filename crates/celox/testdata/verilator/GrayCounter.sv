
module counter #(

    parameter int unsigned WIDTH = 2,

    parameter bit [WIDTH-1:0] MAX_COUNT = '1,

    parameter bit [WIDTH-1:0] MIN_COUNT = '0,

    parameter bit [WIDTH-1:0] INITIAL_COUNT = MIN_COUNT,

    parameter bit WRAP_AROUND = 1,

    localparam type COUNT = logic [WIDTH-1:0]
) (

    input var logic i_clk,

    input var logic i_rst,

    input var logic i_clear,

    input var logic i_set,

    input var COUNT i_set_value,

    input var logic i_up,

    input var logic i_down,

    output var COUNT o_count,

    output var COUNT o_count_next,

    output var logic o_wrap_around
);
    function automatic COUNT count_up(
        input var COUNT current_count
    ) ;
        if (current_count == MAX_COUNT) begin
            if (WRAP_AROUND) begin
                return MIN_COUNT;
            end else begin
                return MAX_COUNT;
            end
        end else begin
            return current_count + 1;
        end
    endfunction

    function automatic COUNT count_down(
        input var COUNT current_count
    ) ;
        if (current_count == MIN_COUNT) begin
            if (WRAP_AROUND) begin
                return MAX_COUNT;
            end else begin
                return MIN_COUNT;
            end
        end else begin
            return current_count - 1;
        end
    endfunction

    function automatic logic get_wrap_around_flag(
        input var logic clear        ,
        input var logic set          ,
        input var logic up           ,
        input var logic down         ,
        input var COUNT current_count
    ) ;
        logic [2-1:0] up_down;
        up_down = {up, down};
        if (clear || set) begin
            return '0;
        end else if ((current_count == MAX_COUNT) && (up_down == 2'b10)) begin
            return '1;
        end else if ((current_count == MIN_COUNT) && (up_down == 2'b01)) begin
            return '1;
        end else begin
            return '0;
        end
    endfunction

    function automatic COUNT get_count_next(
        input var logic clear        ,
        input var logic set          ,
        input var COUNT set_value    ,
        input var logic up           ,
        input var logic down         ,
        input var COUNT current_count
    ) ;
        case (1'b1)
            clear          : return INITIAL_COUNT;
            set            : return set_value;
            (up && (!down)): return count_up(current_count);
            (down && (!up)): return count_down(current_count);
            default        : return current_count;
        endcase
    endfunction

    COUNT count     ;
    COUNT count_next;

    always_comb o_count      = count;
    always_comb o_count_next = count_next;

    always_comb count_next = get_count_next(i_clear, i_set, i_set_value, i_up, i_down, count);
    always_ff @ (posedge i_clk, negedge i_rst) begin
        if (!i_rst) begin
            count <= INITIAL_COUNT;
        end else begin
            count <= count_next;
        end
    end

    if ((WRAP_AROUND)) begin :g
        always_comb o_wrap_around = get_wrap_around_flag(i_clear, i_set, i_up, i_down, count);
    end else begin :g
        always_comb o_wrap_around = '0;
    end
endmodule

module gray_encoder #(

    parameter int unsigned WIDTH = 32
) (

    input var logic [WIDTH-1:0] i_bin,

    output var logic [WIDTH-1:0] o_gray
);
    always_comb o_gray = i_bin ^ (i_bin >> 1);
endmodule

module gray_counter #(

    parameter int unsigned WIDTH = 2,

    parameter bit [WIDTH-1:0] MAX_COUNT = '1,

    parameter bit [WIDTH-1:0] MIN_COUNT = '0,

    parameter bit [WIDTH-1:0] INITIAL_COUNT = MIN_COUNT,

    parameter bit WRAP_AROUND = 1,

    localparam type COUNT = logic [WIDTH-1:0]
) (

    input var logic i_clk,

    input var logic i_rst,

    input var logic i_clear,

    input var logic i_set,

    input var COUNT i_set_value,

    input var logic i_up,

    input var logic i_down,

    output var COUNT o_count,

    output var COUNT o_count_next,

    output var logic o_wrap_around
);

    COUNT bin_count     ;
    COUNT bin_count_next;

    counter #(
        .WIDTH         (WIDTH        ),
        .MAX_COUNT     (MAX_COUNT    ),
        .MIN_COUNT     (MIN_COUNT    ),
        .INITIAL_COUNT (INITIAL_COUNT),
        .WRAP_AROUND   (WRAP_AROUND  )
    ) u_bin_counter (
        .i_clk         (i_clk         ),
        .i_rst         (i_rst         ),
        .i_clear       (i_clear       ),
        .i_set         (i_set         ),
        .i_set_value   (i_set_value   ),
        .i_up          (i_up          ),
        .i_down        (i_down        ),
        .o_count       (bin_count     ),
        .o_count_next  (bin_count_next),
        .o_wrap_around (o_wrap_around )
    );

    gray_encoder #(
        .WIDTH (WIDTH)
    ) u_gray_cur (
        .i_bin  (bin_count),
        .o_gray (o_count  )
    );
    gray_encoder #(
        .WIDTH (WIDTH)
    ) u_gray_next (
        .i_bin  (bin_count_next),
        .o_gray (o_count_next  )
    );

endmodule

module Top (
    input  var logic          clk    ,
    input  var logic          rst    ,
    input  var logic          i_up   ,
    output var logic [32-1:0] o_count
);
    gray_counter #(
        .WIDTH         (32     )
    ) u (
        .i_clk         (clk    ),
        .i_rst         (rst    ),
        .i_clear       (1'b0   ),
        .i_set         (1'b0   ),
        .i_set_value   (32'b0  ),
        .i_up          (i_up   ),
        .i_down        (1'b0   ),
        .o_count       (o_count),
        .o_count_next  (       ),
        .o_wrap_around (       )
    );
endmodule