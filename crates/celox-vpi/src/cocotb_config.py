"""Query one interpreter for the configuration its cocotb installation uses."""

import json
import os
import sys
from pathlib import Path


def linked_libpython():
    """Locate the loaded Python library without a compiler/linker search."""
    if os.name != "posix":
        return None
    try:
        import ctypes

        class DlInfo(ctypes.Structure):
            _fields_ = [
                ("dli_fname", ctypes.c_char_p),
                ("dli_fbase", ctypes.c_void_p),
                ("dli_sname", ctypes.c_char_p),
                ("dli_saddr", ctypes.c_void_p),
            ]

        dladdr = ctypes.CDLL(None).dladdr
        dladdr.argtypes = [ctypes.c_void_p, ctypes.POINTER(DlInfo)]
        dladdr.restype = ctypes.c_int
        symbol = ctypes.cast(ctypes.pythonapi.Py_GetVersion, ctypes.c_void_p).value
        info = DlInfo()
        if not dladdr(symbol, ctypes.byref(info)) or not info.dli_fname:
            return None
        path = Path(os.fsdecode(info.dli_fname)).resolve()
        # A statically linked interpreter can identify the executable instead
        # of a loadable library. Accept only a library resolving the same symbol.
        library = ctypes.PyDLL(str(path))
        if ctypes.cast(library.Py_GetVersion, ctypes.c_void_p).value != symbol:
            return None
        return str(path)
    except (AttributeError, OSError, ValueError):
        return None


def query_config(config, *arguments):
    """Use cocotb's CLI fallback in this process when direct lookup cannot."""
    import contextlib
    import io

    saved = sys.argv
    output = io.StringIO()
    try:
        sys.argv = ["cocotb_tools.config", *arguments]
        with contextlib.redirect_stdout(output):
            config.main()
    finally:
        sys.argv = saved
    value = output.getvalue().strip()
    if not value:
        raise RuntimeError("cocotb returned an empty configuration value")
    return value


def main():
    need_vpi, need_libpython, need_entry = (value == "1" for value in sys.argv[1:])
    result = {
        "python": sys.executable,
        "vpi": None,
        "libpython": None,
        "pygpi_entry_point": None,
    }
    if need_vpi or need_libpython or need_entry:
        from cocotb_tools import config

        if need_vpi:
            result["vpi"] = config.lib_name_path("vpi", "icarus").as_posix()
        if need_libpython:
            result["libpython"] = linked_libpython() or query_config(config, "--libpython")
        if need_entry:
            entry = getattr(config, "pygpi_entry_point", None)
            if entry is not None:
                value = entry()
                if not isinstance(value, str) or not value:
                    raise RuntimeError("cocotb returned an invalid PyGPI entry point")
                result["pygpi_entry_point"] = value
    print(json.dumps(result))


if __name__ == "__main__":
    main()
