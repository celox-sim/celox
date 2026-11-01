// A virtual interface variable (IEEE 1800-2023 25.9). Expected output: "5a".
interface bus_if;
    logic [7:0] data;
endinterface
module Top;
    bus_if bus();
    virtual bus_if v;
    initial begin
        v = bus;
        v.data = 8'h5a;
        $display("%h", bus.data);
    end
endmodule
