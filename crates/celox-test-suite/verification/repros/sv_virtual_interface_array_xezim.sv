interface bus_if; logic [7:0] data; endinterface
module Top;
  bus_if a();
  virtual bus_if grid[2][2];
  initial begin
    grid[0][1] = a;
    grid[0][1].data = 8'h12;
    $display("%h", a.data);
  end
endmodule
