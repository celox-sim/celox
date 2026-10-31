// Assignments that IEEE 1800-2023 25.9 makes illegal: a virtual interface of
// another interface, and one with a modport to one without it. Expected:
// both rejected at compile time.
interface bus_if;
    logic data;
    modport mon(input data);
endinterface
interface other_if;
    logic data;
endinterface
module Top;
    other_if other();
    virtual bus_if v;
    virtual bus_if.mon m;
    virtual other_if o;
    initial begin
        o = other;
        v = o;
        v = m;
    end
endmodule
