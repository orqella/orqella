import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";

const source = readFileSync(new URL("../client/app.js", import.meta.url), "utf8");

test("wasm is requested with the same version as the JavaScript glue", () => {
  const glue = source.match(/orqella_web_client\.js\?v=([^'"]+)/);
  const wasm = source.match(/orqella_web_client_bg\.wasm\?v=([^'"]+)/);
  assert.ok(glue, "glue import has a version query");
  assert.ok(wasm, "wasm URL has a version query");
  assert.equal(wasm[1], glue[1]);
});
