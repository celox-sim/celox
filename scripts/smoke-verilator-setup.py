#!/usr/bin/env python3
"""Compile and execute a real VPI design with the installed Verilator."""
from pathlib import Path
import sys
import tempfile
import xml.etree.ElementTree as ET

from cocotb_tools.runner import get_runner


def main():
    with tempfile.TemporaryDirectory(prefix='verilator-vpi-smoke-') as temporary:
        root = Path(temporary)
        source = root / 'increment.sv'
        source.write_text('''module increment(input logic [7:0] i, output logic [7:0] o);
  timeunit 1ns; timeprecision 1ps;
  assign o = i + 8'd1;
endmodule
''')
        (root / 'test_increment.py').write_text('''import cocotb
from cocotb.triggers import Timer

@cocotb.test()
async def increment(dut):
    for value in range(256):
        dut.i.value = value
        await Timer(1, unit="ns")
        assert int(dut.o.value) == (value + 1) % 256
''')
        sys.path.insert(0, str(root))
        runner = get_runner('verilator')
        runner.build(sources=[source], hdl_toplevel='increment', build_dir=root / 'build')
        report = root / 'results.xml'
        runner.test(hdl_toplevel='increment', test_module='test_increment', results_xml=report)
        tests = list(ET.parse(report).getroot().iter('testcase'))
        if len(tests) != 1 or any(tests[0].find(tag) is not None for tag in ('failure', 'error', 'skipped')):
            raise RuntimeError('Installed Verilator did not pass its cocotb/VPI smoke test')
        print('Verilator cocotb/VPI smoke passed (256 assertions)', flush=True)


if __name__ == '__main__':
    main()
