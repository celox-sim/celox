#!/bin/sh
# This is an executable tripwire, never a solver.
printf '%s\n' "unexpected solver invocation: $*" >> "${Z3_TRIPWIRE_MARKER:?set a fresh tripwire marker path}"
exit 99
