# Actual exported array fixture

`array_read_write2.veryl` is a32-bit ×2-word synchronous read-old/write-enable
memory. `array_read_write2.json` is its direct compile-only export from the pinned
frontend after65f53b5. Recreate with a design document containing top `Top`, the
source text and `four_state:false`, using `veryl-proof-frontend`.

Its flattened storage metadata is width64, array_dims[2]; each element is32bits.
SIR uses Element(element_width32) and a64-bit sparse commit. This tests the real
frontend boundary, which synthetic Storage(lane,count) unit tests alone missed.
The source and compiled artifact are checked in; no simulator or oracle values
were extracted. Expected symbolic read/write equations are handwritten in the
Rust test.
