// SPDX-License-Identifier: MIT
// Original deterministic 6502 program and tile data; no commercial ROM needed.
import { writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
export function roms() {
  const code = [];
  const emit = (...bytes) => code.push(...bytes);
  const lda = value => emit(0xa9, value);
  const sta = address => emit(0x8d, address & 255, address >> 8);
  emit(0x78, 0xd8, 0xa2, 0xff, 0x9a); // SEI; CLD; LDX #ff; TXS
  lda(0); sta(0x2000); sta(0x2001);
  lda(0x3f); sta(0x2006); lda(0); sta(0x2006);
  for (const color of [0x0f, 0x01, 0x21, 0x31]) { lda(color); sta(0x2007); }
  lda(0x20); sta(0x2006); lda(0); sta(0x2006);
  emit(0xa0, 4, 0xa2, 0); // LDY #4; LDX #0
  const fill = code.length;
  sta(0x2007); emit(0xe8); // INX
  emit(0xd0, (fill - (code.length + 2)) & 255); // BNE fill
  emit(0x88); // DEY
  emit(0xd0, (fill - (code.length + 2)) & 255);
  lda(0); sta(0x2005); sta(0x2005);
  lda(1); sta(0x4015); lda(0xbf); sta(0x4000);
  lda(0x40); sta(0x4002); lda(0x08); sta(0x4003);
  lda(0x0a); sta(0x2001);
  const loop = code.length + 0x8000;
  emit(0xe6, 0, 0x4c, loop & 255, loop >> 8); // INC $00; JMP loop
  const prg = new Uint8Array(32768).fill(0xea);
  prg.set(code);
  for (const offset of [0x7ffa, 0x7ffc, 0x7ffe]) { prg[offset] = 0; prg[offset + 1] = 0x80; }
  const chr = new Uint8Array(8192);
  for (let row = 0; row < 8; row++) { chr[row] = row % 2 ? 0x55 : 0xaa; chr[8 + row] = chr[row] ^ 255; }
  return { prg, chr };
}
export function writeRoms(directory) {
  for (const [name, bytes] of Object.entries(roms())) {
    writeFileSync(join(directory, `benchmark_${name}.hex`), [...bytes].map(b => b.toString(16).padStart(2, "0")).join("\n") + "\n");
  }
}
if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) writeRoms(process.argv[2]);
