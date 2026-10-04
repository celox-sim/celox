#pragma once
#include <algorithm>
#include <stdexcept>
#include <string>

static bool array(vpiHandle h) {
    const int type = vpi_get(vpiType, h);
    return type == vpiRegArray || type == vpiNetArray || type == vpiMemory;
}

static int bound(vpiHandle h, int kind) {
    auto range = vpi_handle(kind, h);
    s_vpi_value value{};
    value.format = vpiIntVal;
    vpi_get_value(range, &value);
    vpi_release_handle(range);
    return value.value.integer;
}

static std::string read_bits(vpiHandle h) {
    if (array(h)) {
        const int a = bound(h, vpiLeftRange), b = bound(h, vpiRightRange);
        std::string result;
        for (int i = std::max(a, b); i >= std::min(a, b); --i) {
            auto child = vpi_handle_by_index(h, i);
            if (!child) throw std::runtime_error("array index is not accessible");
            result += read_bits(child);
            vpi_release_handle(child);
        }
        return result;
    }
    s_vpi_value value{};
    value.format = vpiBinStrVal;
    vpi_get_value(h, &value);
    if (!value.value.str) throw std::runtime_error("signal is not readable");
    return value.value.str;
}

static void write_bits(vpiHandle h, const std::string& bits, size_t& consumed) {
    if (array(h)) {
        const int a = bound(h, vpiLeftRange), b = bound(h, vpiRightRange);
        for (int i = std::min(a, b); i <= std::max(a, b); ++i) {
            auto child = vpi_handle_by_index(h, i);
            if (!child) throw std::runtime_error("array index is not accessible");
            write_bits(child, bits, consumed);
            vpi_release_handle(child);
        }
        return;
    }
    const size_t width = vpi_get(vpiSize, h);
    std::string part(width, '0');
    for (size_t i = 0; i < width && consumed + i < bits.size(); ++i) {
        part[width - 1 - i] = bits[bits.size() - 1 - consumed - i];
    }
    consumed += width;
    s_vpi_value value{};
    value.format = vpiBinStrVal;
    value.value.str = const_cast<char*>(part.c_str());
    vpi_put_value(h, &value, nullptr, vpiNoDelay);
}
