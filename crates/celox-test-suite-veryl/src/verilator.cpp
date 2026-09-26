// Line protocol for the independent Veryl suite. HDL stdout is kept separate
// from replies, which always begin with "@suite ".
#include "Vdut.h"
#include "verilated.h"
#include "verilated_vpi.h"
#include <algorithm>
#include <iostream>
#include <sstream>
#include <stdexcept>
#include <string>

#include "vpi_bits.hpp"

int main(int argc, char** argv) {
    VerilatedContext context;
    context.commandArgs(argc, argv);
    context.randReset(0);
    Vdut dut{&context};
    // Drive the first batch before evaluating. Zero-initialized runtime loop
    // bounds/steps may be invalid until the test supplies its inputs.
    bool dirty = true;
    std::string line;
    while (std::getline(std::cin, line)) {
        try {
            std::istringstream input(line);
            std::string op, name, bits;
            input >> op >> name >> bits;
            if (op == "quit") break;
            if (op == "eval") {
                context.timeInc(1);
                if (dirty) dut.eval();
                dirty = false;
                std::cout << "@suite ok" << std::endl;
                continue;
            }
            // Top ports also have an internal public copy. Drive the external
            // port so eval() does not overwrite a VPI write with its old value.
            const auto split = name.find('.', 4);
            const auto port_name = "TOP.TOP." + name.substr(split + 1);
            auto handle = vpi_handle_by_name(const_cast<char*>(port_name.c_str()), nullptr);
            if (!handle) handle = vpi_handle_by_name(const_cast<char*>(name.c_str()), nullptr);
            if (!handle) throw std::runtime_error("missing signal " + name);
            if (op == "read") {
                if (dirty) dut.eval();
                dirty = false;
                const auto value = read_bits(handle);
                std::cout << "@suite " << value << std::endl;
            } else if (op == "write") {
                size_t consumed = 0;
                write_bits(handle, bits, consumed);
                dirty = true;
                std::cout << "@suite ok" << std::endl;
            } else {
                throw std::runtime_error("unknown operation " + op);
            }
            vpi_release_handle(handle);
        } catch (const std::exception& error) {
            std::cout << "@suite error " << error.what() << std::endl;
        }
    }
    dut.final();
}
