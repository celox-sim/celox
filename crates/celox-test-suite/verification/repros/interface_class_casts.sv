// $cast from a handle of an interface class assigns the object when its class
// is assignment compatible with the destination (IEEE 1800-2023 8.16, 8.26.5).
// Expected output: 1 1.
module Top;
    interface class Named;
        pure virtual function int id();
    endclass
    interface class Sized;
        pure virtual function int size();
    endclass
    class Box implements Named, Sized;
        virtual function int id();
            return 1;
        endfunction
        virtual function int size();
            return 8;
        endfunction
    endclass
    Box box;
    Box back;
    Named named;
    Sized sized;
    initial begin
        box = new;
        named = box;
        $display("%0d %0d", $cast(sized, named), $cast(back, named));
    end
endmodule
