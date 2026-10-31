// IEEE 1800-2023 7.3.2, 10.4.1 and Table 10-1.
`ifdef TAGGED
module Top(output logic [7:0] result);
  typedef union tagged packed {void Empty; logic [7:0] Full;} item_t;
  item_t item, source_value;
  initial begin
    item=tagged Empty;
    source_value=tagged Full 8'ha5;
    item.Full = #4 source_value.Full;
    result=item.Full;
  end
  initial begin #2 item=tagged Full 8'd0; source_value=tagged Empty; end
endmodule
`elsif TASK_NET
module Top;
  wire w;
  task automatic write_value(); w = #1 1'b1; endtask
  initial write_value();
endmodule
`elsif NET_SHADOW
module Top(output bit value);
  wire w = 1'b0;
  initial begin : local_scope
    bit w;
    w = #1 1'b1;
    value = w;
  end
endmodule
`else
module Top(output wire [7:0] w);
  initial w[3:0] = #1 4'hf;
endmodule
`endif
