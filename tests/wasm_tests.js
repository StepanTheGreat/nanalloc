const fs = require("node:fs");
const { exit } = require("node:process");

const TEXT_DECODER = new TextDecoder();
const wasmBuffer = fs.readFileSync(`./target/wasm32-unknown-unknown/debug/nanalloc.wasm`);

let wasmMemory;
const hostImports = {
  js_panic: (line, filePtr, fileLen, msgPtr, msgLen) => {
    const memory = wasmMemory.buffer;

    let fileStr = TEXT_DECODER.decode(memory.slice(filePtr, filePtr+fileLen));
    let msgStr = TEXT_DECODER.decode(memory.slice(msgPtr, msgPtr+msgLen));

    console.error(`Error in file ${fileStr}, line ${line} : "${msgStr}"`);

    exit(1);
  }
}

WebAssembly.instantiate(wasmBuffer, { "js": hostImports }).then(wasmModule => {
  wasmMemory = wasmModule.instance.exports.memory;

  const { run } = wasmModule.instance.exports;

  run();
});