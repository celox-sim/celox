import { copyFileSync } from "node:fs";
import process from "node:process";

const p = process.platform;
const a = process.arch;

const src =
  p === "win32"
    ? "target/debug/celox_napi.dll"
    : p === "darwin"
      ? "target/debug/libcelox_napi.dylib"
      : "target/debug/libcelox_napi.so";

const dst =
  p === "darwin"
    ? `crates/celox-napi/celox.darwin-${a}.node`
    : p === "win32"
      ? `crates/celox-napi/celox.win32-${a}-msvc.node`
      : `crates/celox-napi/celox.linux-${a}-gnu.node`;

copyFileSync(src, dst);
console.log(`${src} -> ${dst}`);
