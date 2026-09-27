// Event-driven VPI bridge. Commands arrive before the first HDL evaluation.
// A one-tick callback yields to the scheduler, including NBA/delta updates,
// before returning a value. No polling or forced output values are used.
#include <vpi_user.h>
#include <iostream>
#include <sstream>
#include <string>
#include "vpi_bits.hpp"

static bool dirty = true;
static std::string pending;
static PLI_INT32 commands(p_cb_data);

static void reply(const std::string& text) {
    std::cout << "@suite " << text << std::endl;
}

static void defer(const std::string& command) {
    pending = command;
    s_vpi_time time{};
    time.type = vpiSimTime;
    time.low = 1;
    s_cb_data cb{};
    cb.reason = cbAfterDelay;
    cb.cb_rtn = commands;
    cb.time = &time;
    vpi_register_cb(&cb);
}

static PLI_INT32 commands(p_cb_data) {
    std::string line;
    if (!pending.empty()) {
        line.swap(pending);
        dirty = false;
    }
    while (!line.empty() || std::getline(std::cin, line)) {
        try {
            std::istringstream input(line);
            std::string op, name, bits;
            input >> op >> name >> bits;
            if (op == "quit") break;
            if ((op == "eval" || op == "read") && dirty) {
                defer(line);
                return 0;
            }
            if (op == "eval") {
                reply("ok");
            } else {
                auto handle = vpi_handle_by_name(const_cast<char*>(name.c_str()), nullptr);
                if (!handle) throw std::runtime_error("missing signal " + name);
                if (op == "read") {
                    reply(read_bits(handle));
                } else if (op == "write") {
                    size_t consumed = 0;
                    write_bits(handle, bits, consumed);
                    dirty = true;
                    reply("ok");
                } else {
                    throw std::runtime_error("unknown operation " + op);
                }
                vpi_free_object(handle);
            }
        } catch (const std::exception& error) {
            reply(std::string("error ") + error.what());
        }
        line.clear();
    }
    vpi_control(vpiFinish, 0);
    return 0;
}

static void initialize(vpiHandle scope) {
    // The suite's two-state mode starts all storage at zero. Icarus itself
    // remains four-state so unknown values produced later are still reported.
    for (const int type : {vpiReg, vpiIntegerVar, vpiMemory}) {
        if (auto iter = vpi_iterate(type, scope)) {
            while (auto item = vpi_scan(iter)) {
                size_t consumed = 0;
                write_bits(item, "0", consumed);
            }
        }
    }
    for (const int type : {vpiModule, vpiGenScope}) {
        if (auto iter = vpi_iterate(type, scope)) {
            while (auto child = vpi_scan(iter)) initialize(child);
        }
    }
}

static PLI_INT32 start(p_cb_data) {
    s_vpi_vlog_info info{};
    vpi_get_vlog_info(&info);
    for (int i = 0; i < info.argc; ++i) {
        if (std::string(info.argv[i]) == "+suite_two_state") initialize(nullptr);
    }
    return commands(nullptr);
}

static void register_suite() {
    s_cb_data cb{};
    cb.reason = cbStartOfSimulation;
    cb.cb_rtn = start;
    vpi_register_cb(&cb);
}

extern "C" {
void (*vlog_startup_routines[])() = {register_suite, nullptr};
}
