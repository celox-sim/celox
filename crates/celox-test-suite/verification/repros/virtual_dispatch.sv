// A virtual function called through a base-class handle runs the override of
// the object's class (IEEE 1800-2023 8.20). Expected output: 2 3 4 4.
module Top;
    class A;
        virtual function int f();
            return 1;
        endfunction
    endclass
    class B extends A;
        function int f();
            return 2;
        endfunction
    endclass
    class C extends A;
        virtual function int f();
            return 3;
        endfunction
    endclass
    class D extends C;
        function int f();
            return 4;
        endfunction
    endclass
    A a;
    B b;
    C c;
    D d;
    initial begin
        b = new;
        c = new;
        d = new;
        a = b;
        $write("%0d ", a.f());
        a = c;
        $write("%0d ", a.f());
        a = d;
        $write("%0d ", a.f());
        c = d;
        $display("%0d", c.f());
    end
endmodule
