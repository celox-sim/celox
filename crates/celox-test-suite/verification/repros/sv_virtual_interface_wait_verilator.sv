// A static virtual interface variable watched by `wait` while another
// branch retargets it (IEEE 1800-2023 9.4.3, 25.9). Expected output:
// "module 5".
`timescale 1ns/1ns
interface bus_if;
    logic ready;
endinterface
module Top;
    bus_if a();
    bus_if b();
    virtual bus_if watched;
    initial begin
        a.ready = 0;
        b.ready = 0;
        watched = a;
        fork
            begin wait (watched.ready); $display("module %0d", $time); end
            begin #3 watched = b; end
            begin #5 b.ready = 1; end
        join
        $finish;
    end
endmodule
