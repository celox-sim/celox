module ram #(
    parameter int unsigned WORD_SIZE     = 1                                                 ,
    parameter int unsigned ADDRESS_WIDTH = ((WORD_SIZE >= 2) ? ( $clog2(WORD_SIZE) ) : ( 1 )),
    parameter int unsigned DATA_WIDTH    = 8                                                 ,
    parameter type         DATA_TYPE     = logic [DATA_WIDTH-1:0]                            ,
    parameter bit          BUFFER_OUT    = 1'b0                                              ,
    parameter bit          USE_RESET     = 1'b0                                              ,
    parameter DATA_TYPE    INITIAL_VALUE = DATA_TYPE'(0                                     )
) (
    input  var logic                         i_clk ,
    input  var logic                         i_rst ,
    input  var logic                         i_clr ,
    input  var logic                         i_mea ,
    input  var logic                         i_wea ,
    input  var logic     [ADDRESS_WIDTH-1:0] i_adra,
    input  var DATA_TYPE                     i_da  ,
    input  var logic                         i_meb ,
    input  var logic     [ADDRESS_WIDTH-1:0] i_adrb,
    output var DATA_TYPE                     o_qb  
);
    logic [$bits(DATA_TYPE)-1:0] ram_data [WORD_SIZE];
    logic [$bits(DATA_TYPE)-1:0] q                   ;

    if (USE_RESET) begin :g_ram
        always_ff @ (posedge i_clk, negedge i_rst) begin
            if (!i_rst) begin
                ram_data <= '{default: INITIAL_VALUE};
            end else if (i_clr) begin
                ram_data <= '{default: INITIAL_VALUE};
            end else if (i_mea && i_wea) begin
                ram_data[i_adra] <= i_da;
            end
        end
    end else begin :g_ram
        always_ff @ (posedge i_clk) begin
            if (i_mea && i_wea) begin
                ram_data[i_adra] <= i_da;
            end
        end
    end

    always_comb begin
        o_qb = DATA_TYPE'(q);
    end

    if (!BUFFER_OUT) begin :g_out
        always_comb begin
            q = ram_data[i_adrb];
        end
    end else if (USE_RESET) begin :g_out
        always_ff @ (posedge i_clk, negedge i_rst) begin
            if (!i_rst) begin
                q <= INITIAL_VALUE;
            end else if (i_clr) begin
                q <= INITIAL_VALUE;
            end else if (i_meb) begin
                q <= ram_data[i_adrb];
            end
        end
    end else begin :g_out
        always_ff @ (posedge i_clk) begin
            if (i_meb) begin
                q <= ram_data[i_adrb];
            end
        end
    end
endmodule

module fifo_controller #(
    parameter  type         TYPE              = logic                                             ,
    parameter  int unsigned DEPTH             = 8                                                 ,
    parameter  int unsigned THRESHOLD         = DEPTH                                             ,
    parameter  bit          FLAG_FF_OUT       = 1'b1                                              ,
    parameter  bit          DATA_FF_OUT       = 1'b1                                              ,
    parameter  bit          PUSH_ON_CLEAR     = 1'b0                                              ,
    parameter  int unsigned RAM_WORDS         = ((DATA_FF_OUT) ? ( DEPTH - 1 ) : ( DEPTH        )),
    parameter  int unsigned RAM_POINTER_WIDTH = ((RAM_WORDS >= 2) ? ( $clog2(RAM_WORDS) ) : ( 1 )),
    parameter  int unsigned MATCH_COUNT_WIDTH = 0                                                 ,
    parameter  int unsigned POINTER_WIDTH     = ((DEPTH >= 2) ? ( $clog2(DEPTH) ) : ( 1         )),
    localparam type         RAM_POINTER       = logic [RAM_POINTER_WIDTH-1:0]                     ,
    localparam type         POINTER           = logic [POINTER_WIDTH-1:0]                         ,
    localparam type         COUNTER           = logic [$clog2(DEPTH + 1)-1:0]                 
) (
    input  var logic       i_clk          ,
    input  var logic       i_rst          ,
    input  var logic       i_clear        ,
    output var logic       o_empty        ,
    output var logic       o_almost_full  ,
    output var logic       o_full         ,
    output var COUNTER     o_word_count   ,
    input  var logic       i_push         ,
    input  var TYPE        i_data         ,
    input  var logic       i_pop          ,
    output var RAM_POINTER o_write_pointer,
    output var logic       o_write_to_ff  ,
    output var logic       o_write_to_ram ,
    output var RAM_POINTER o_read_pointer ,
    output var logic       o_read_from_ram
);
    typedef struct packed {
        logic empty      ;
        logic almost_full;
        logic full       ;
    } s_status_flag;

    logic                 push             ;
    logic                 pop              ;
    logic         [2-1:0] clear            ;
    logic                 update_state     ;
    COUNTER               word_counter     ;
    COUNTER               word_counter_next;
    logic                 word_counter_eq_1;
    logic                 word_counter_eq_2;
    logic                 word_counter_ge_2;
    s_status_flag         status_flag      ;
    logic                 write_to_ff      ;
    logic                 write_to_ram     ;
    RAM_POINTER           ram_write_pointer;
    logic                 read_from_ram    ;
    RAM_POINTER           ram_read_pointer ;
    logic                 ram_empty_next   ;
    logic                 match_data       ;
    logic                 last_pop_data    ;

    always_comb begin
        push = i_push && ((PUSH_ON_CLEAR && i_clear) || ((!status_flag.full) && (!match_data)));
        pop  = i_pop && (!status_flag.empty) && last_pop_data;
    end

    always_comb begin
        clear[0] = i_clear && ((!PUSH_ON_CLEAR) || (!push));
        clear[1] = i_clear && PUSH_ON_CLEAR && push;
    end

    always_comb begin
        update_state = push || pop || i_clear;
    end

    function automatic COUNTER get_word_counter_next(
        input var logic           push        ,
        input var logic           pop         ,
        input var logic   [2-1:0] clear       ,
        input var COUNTER         word_counter
    ) ;
        logic up  ;
        logic down;
        up   = push && (!pop);
        down = (!push) && pop;
        case (1'b1)
            clear[0]: return COUNTER'(0);
            clear[1]: return COUNTER'(1);
            up      : return word_counter + COUNTER'(1);
            down    : return word_counter - COUNTER'(1);
            default : return word_counter;
        endcase
    endfunction

    always_comb begin
        o_word_count = word_counter;
    end

    always_comb begin
        word_counter_eq_1 = (DEPTH >= 1) && (word_counter == COUNTER'(1));
        word_counter_eq_2 = (DEPTH >= 2) && (word_counter == COUNTER'(2));
        word_counter_ge_2 = (DEPTH >= 2) && (word_counter >= COUNTER'(2));
    end

    always_comb begin
        word_counter_next = get_word_counter_next(push, pop, clear, word_counter);
    end

    always_ff @ (posedge i_clk, negedge i_rst) begin
        if (!i_rst) begin
            word_counter <= '0;
        end else if (update_state) begin
            word_counter <= word_counter_next;
        end
    end

    function automatic s_status_flag get_status_flag(
        input var COUNTER word_count
    ) ;
        s_status_flag flag            ;
        flag.empty       = word_count == 0;
        flag.almost_full = word_count >= THRESHOLD;
        flag.full        = word_count >= DEPTH;
        return flag;
    endfunction

    always_comb begin
        o_empty       = status_flag.empty;
        o_almost_full = status_flag.almost_full;
        o_full        = status_flag.full && (!match_data);
    end

    if (FLAG_FF_OUT) begin :g_flag_ff_out
        always_ff @ (posedge i_clk, negedge i_rst) begin
            if (!i_rst) begin
                status_flag.empty       <= '1;
                status_flag.almost_full <= '0;
                status_flag.full        <= '0;
            end else if (update_state) begin
                status_flag <= get_status_flag(word_counter_next);
            end
        end
    end else begin :g_flag_logic_out
        always_comb begin
            status_flag = get_status_flag(word_counter);
        end
    end

    always_comb begin
        o_write_pointer = ram_write_pointer;
        o_write_to_ff   = write_to_ff;
        o_write_to_ram  = write_to_ram;
        o_read_pointer  = ram_read_pointer;
        o_read_from_ram = read_from_ram;
    end

    if (DATA_FF_OUT) begin :g_data_ff_out
        always_comb begin
            if ((word_counter_eq_1 && pop) || status_flag.empty || clear[1]) begin
                write_to_ff  = push;
                write_to_ram = '0;
            end else begin
                write_to_ff  = '0;
                write_to_ram = push;
            end
            read_from_ram  = pop && word_counter_ge_2;
            ram_empty_next = read_from_ram && (!write_to_ram) && word_counter_eq_2;
        end
    end else begin :g_data_ram_out
        always_comb begin
            write_to_ff    = '0;
            write_to_ram   = push;
            read_from_ram  = pop;
            ram_empty_next = read_from_ram && (!write_to_ram) && word_counter_eq_1;
        end
    end

    if (RAM_WORDS >= 2) begin :g_multi_word_ram
        always_ff @ (posedge i_clk, negedge i_rst) begin
            if (!i_rst) begin
                ram_write_pointer <= RAM_POINTER'(0);
            end else if ((clear[0])) begin
                ram_write_pointer <= RAM_POINTER'(0);
            end else if ((clear[1])) begin
                ram_write_pointer <= ((DATA_FF_OUT) ? ( RAM_POINTER'(0) ) : ( RAM_POINTER'(1) ));
            end else if ((ram_empty_next)) begin
                ram_write_pointer <= ram_read_pointer;
            end else if ((write_to_ram)) begin
                if ((ram_write_pointer == RAM_POINTER'((RAM_WORDS - 1)))) begin
                    ram_write_pointer <= RAM_POINTER'(0);
                end else begin
                    ram_write_pointer <= ram_write_pointer + (RAM_POINTER'(1));
                end
            end
        end

        always_ff @ (posedge i_clk, negedge i_rst) begin
            if (!i_rst) begin
                ram_read_pointer <= RAM_POINTER'(0);
            end else if ((i_clear)) begin
                ram_read_pointer <= RAM_POINTER'(0);
            end else if ((ram_empty_next)) begin
                ram_read_pointer <= ram_read_pointer;
            end else if ((read_from_ram)) begin
                if ((ram_read_pointer == RAM_POINTER'((RAM_WORDS - 1)))) begin
                    ram_read_pointer <= RAM_POINTER'(0);
                end else begin
                    ram_read_pointer <= ram_read_pointer + (RAM_POINTER'(1));
                end
            end
        end
    end else begin :g_single_word_ram
        always_comb begin
            ram_write_pointer = RAM_POINTER'(0);
            ram_read_pointer  = RAM_POINTER'(0);
        end
    end

    if (MATCH_COUNT_WIDTH > 0) begin :g_data_match
        logic   [DEPTH-1:0][MATCH_COUNT_WIDTH-1:0] match_count     ;
        logic   [DEPTH-1:0]                        match_count_full;
        logic   [DEPTH-1:0]                        match_count_eq_1;
        logic   [DEPTH-1:0]                        last_match_data ;
        POINTER [2-1:0]                            write_pointer   ;
        POINTER                                    read_pointer    ;
        TYPE                                       data            ;

        if (DEPTH == RAM_WORDS) begin :g_pointer
            always_comb begin
                write_pointer[0] = ram_write_pointer;
                read_pointer     = ram_read_pointer;
            end
        end else begin :g_pointer
            always_ff @ (posedge i_clk, negedge i_rst) begin
                if (!i_rst) begin
                    write_pointer[0] <= POINTER'(0);
                end else if (clear[0]) begin
                    write_pointer[0] <= POINTER'(0);
                end else if (clear[1]) begin
                    write_pointer[0] <= POINTER'(1);
                end else if (push) begin
                    if (write_pointer[0] == POINTER'((DEPTH - 1))) begin
                        write_pointer[0] <= POINTER'(0);
                    end else begin
                        write_pointer[0] <= write_pointer[0] + (POINTER'(1));
                    end
                end
            end

            always_ff @ (posedge i_clk, negedge i_rst) begin
                if (!i_rst) begin
                    read_pointer <= POINTER'(0);
                end else if (i_clear) begin
                    read_pointer <= POINTER'(0);
                end else if (pop) begin
                    if (read_pointer == POINTER'((DEPTH - 1))) begin
                        read_pointer <= POINTER'(0);
                    end else begin
                        read_pointer <= read_pointer + (POINTER'(1));
                    end
                end
            end
        end

        always_comb begin
            if (write_pointer[0] == POINTER'(0)) begin
                write_pointer[1] = POINTER'((DEPTH - 1));
            end else begin
                write_pointer[1] = write_pointer[0] - POINTER'(1);
            end
        end

        always_ff @ (posedge i_clk) begin
            if (push) begin
                data <= i_data;
            end
        end

        always_comb begin
            match_data    = (!status_flag.empty) && (i_data == data) && (!match_count_full[write_pointer[1]]);
            last_pop_data = last_match_data[read_pointer];
        end

        for (genvar i = 0; i < DEPTH; i++) begin :g_match_count
            logic [3-1:0] up_down;

            always_comb begin
                match_count_full[i] = match_count[i] == '1;
                match_count_eq_1[i] = match_count[i] == MATCH_COUNT_WIDTH'(1);
                last_match_data[i]  = match_count_eq_1[i] && (up_down[2:1] == '0);
            end

            always_comb begin
                up_down[2] = (match_data == '0) && (write_pointer[0] == POINTER'(i)) && push;
                up_down[1] = (match_data == '1) && (write_pointer[1] == POINTER'(i)) && i_push;
                up_down[0] = (!status_flag.empty) && (read_pointer == POINTER'(i)) && i_pop;
            end

            always_ff @ (posedge i_clk, negedge i_rst) begin
                if (!i_rst) begin
                    match_count[i] <= MATCH_COUNT_WIDTH'(0);
                end else if (clear[0] || (i_clear && (i >= 1))) begin
                    match_count[i] <= MATCH_COUNT_WIDTH'(0);
                end else if (clear[1] && (i == 0)) begin
                    match_count[i] <= MATCH_COUNT_WIDTH'(1);
                end else if (((up_down) inside {3'b1x0, 3'bx10})) begin
                    match_count[i] <= match_count[i] + (MATCH_COUNT_WIDTH'(1));
                end else if (up_down == 3'b001) begin
                    match_count[i] <= match_count[i] - (MATCH_COUNT_WIDTH'(1));
                end
            end
        end
    end else begin :g
        always_comb begin
            match_data    = '0;
            last_pop_data = '1;
        end
    end
endmodule

module fifo #(
    parameter  int unsigned WIDTH             = 8                            ,
    parameter  type         TYPE              = logic [WIDTH-1:0]            ,
    parameter  int unsigned DEPTH             = 8                            ,
    parameter  int unsigned THRESHOLD         = DEPTH                        ,
    parameter  bit          FLAG_FF_OUT       = 1'b1                         ,
    parameter  bit          DATA_FF_OUT       = 1'b1                         ,
    parameter  bit          RESET_RAM         = 1'b0                         ,
    parameter  bit          RESET_DATA_FF     = 1'b1                         ,
    parameter  bit          CLEAR_DATA        = 1'b0                         ,
    parameter  bit          PUSH_ON_CLEAR     = 1'b0                         ,
    parameter  int unsigned MATCH_COUNT_WIDTH = 0                            ,
    localparam type         COUNTER           = logic [$clog2(DEPTH + 1)-1:0]
) (
    input  var logic   i_clk        ,
    input  var logic   i_rst        ,
    input  var logic   i_clear      ,
    output var logic   o_empty      ,
    output var logic   o_almost_full,
    output var logic   o_full       ,
    output var COUNTER o_word_count ,
    input  var logic   i_push       ,
    input  var TYPE    i_data       ,
    input  var logic   i_pop        ,
    output var TYPE    o_data   
);
    localparam int unsigned RAM_WORDS = ((DATA_FF_OUT) ? ( DEPTH - 1 ) : ( DEPTH ));

    logic clear_data;

    always_comb begin
        clear_data = CLEAR_DATA && i_clear;
    end

    localparam int unsigned RAM_POINTER_WIDTH = ((RAM_WORDS >= 2) ? ( $clog2(RAM_WORDS) ) : ( 1 ));

    logic [RAM_POINTER_WIDTH-1:0] write_pointer;
    logic                         write_to_ff  ;
    logic                         write_to_ram ;
    logic [RAM_POINTER_WIDTH-1:0] read_pointer ;
    logic                         read_from_ram;

    fifo_controller #(
        .TYPE              (TYPE             ),
        .DEPTH             (DEPTH            ),
        .THRESHOLD         (THRESHOLD        ),
        .FLAG_FF_OUT       (FLAG_FF_OUT      ),
        .DATA_FF_OUT       (DATA_FF_OUT      ),
        .PUSH_ON_CLEAR     (PUSH_ON_CLEAR    ),
        .RAM_WORDS         (RAM_WORDS        ),
        .RAM_POINTER_WIDTH (RAM_POINTER_WIDTH),
        .MATCH_COUNT_WIDTH (MATCH_COUNT_WIDTH)
    ) u_controller (
        .i_clk           (i_clk        ),
        .i_rst           (i_rst        ),
        .i_clear         (i_clear      ),
        .o_empty         (o_empty      ),
        .o_almost_full   (o_almost_full),
        .o_full          (o_full       ),
        .i_push          (i_push       ),
        .i_data          (i_data       ),
        .i_pop           (i_pop        ),
        .o_word_count    (o_word_count ),
        .o_write_pointer (write_pointer),
        .o_write_to_ff   (write_to_ff  ),
        .o_write_to_ram  (write_to_ram ),
        .o_read_pointer  (read_pointer ),
        .o_read_from_ram (read_from_ram)

    );

    TYPE ram_read_data;

    if (RAM_WORDS >= 1) begin :g_ram
        ram #(
            .WORD_SIZE     (RAM_WORDS        ),
            .ADDRESS_WIDTH (RAM_POINTER_WIDTH),
            .DATA_TYPE     (TYPE             ),
            .BUFFER_OUT    (0                ),
            .USE_RESET     (RESET_RAM        )
        ) u_ram (
            .i_clk  (i_clk        ),
            .i_rst  (i_rst        ),
            .i_clr  (clear_data   ),
            .i_mea  ('1           ),
            .i_wea  (write_to_ram ),
            .i_adra (write_pointer),
            .i_da   (i_data       ),
            .i_meb  ('1           ),
            .i_adrb (read_pointer ),
            .o_qb   (ram_read_data)
        );
    end else begin :g_no_ram
        always_comb begin
            ram_read_data = TYPE'(0);
        end
    end

    if (DATA_FF_OUT) begin :g_data_out
        TYPE data_out;

        always_comb begin
            o_data = data_out;
        end

        if (RESET_DATA_FF) begin :g
            always_ff @ (posedge i_clk, negedge i_rst) begin
                if (!i_rst) begin
                    data_out <= TYPE'(0);
                end else if (clear_data) begin
                    data_out <= TYPE'(0);
                end else if (write_to_ff) begin
                    data_out <= i_data;
                end else if (read_from_ram) begin
                    data_out <= ram_read_data;
                end
            end
        end else begin :g
            always_ff @ (posedge i_clk) begin
                if (clear_data) begin
                    data_out <= TYPE'(0);
                end else if (write_to_ff) begin
                    data_out <= i_data;
                end else if (read_from_ram) begin
                    data_out <= ram_read_data;
                end
            end
        end
    end else begin :g
        always_comb begin
            o_data = ram_read_data;
        end
    end
endmodule

module Top (
    input  var logic         clk         ,
    input  var logic         rst         ,
    input  var logic         i_push      ,
    input  var logic [8-1:0] i_data      ,
    input  var logic         i_pop       ,
    output var logic [8-1:0] o_data      ,
    output var logic         o_empty     ,
    output var logic         o_full      ,
    output var logic [5-1:0] o_word_count
);
    logic almost_full;
    logic clear      ;
    always_comb begin
        clear       = '0;
    end
    fifo #(
        .WIDTH (8 ),
        .DEPTH (16)
    ) u_fifo (
        .i_clk         (clk         ),
        .i_rst         (rst         ),
        .i_clear       (clear       ),
        .o_empty       (o_empty     ),
        .o_almost_full (almost_full ),
        .o_full        (o_full      ),
        .o_word_count  (o_word_count),
        .i_push        (i_push      ),
        .i_data        (i_data      ),
        .i_pop         (i_pop       ),
        .o_data        (o_data      )
    );
endmodule