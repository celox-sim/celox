create_clock -period 10 [get_ports clk]
set_input_delay -clock clk 1 [get_ports d]
