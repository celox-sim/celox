module Top(
    input logic [7:0] a,
    output logic [7:0] app_y, lib_y, app_wide, lib_wide
);
    app_Child app_child(.a(a), .y(app_y));
    lib_Child lib_child(.a(a), .y(lib_y));
    app_Child #(.K(16)) app_specialized(.a(a), .y(app_wide));
    lib_Child #(.K(32)) lib_specialized(.a(a), .y(lib_wide));
endmodule
