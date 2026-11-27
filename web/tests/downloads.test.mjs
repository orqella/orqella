import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import vm from "node:vm";

const source = readFileSync(new URL("../js/frames.js", import.meta.url), "utf8");

function harness() {
  const appended = [];
  const removed = [];
  const clicks = [];
  const document = {
    createElement(tag) {
      if (tag === "iframe") {
        return {
          tag,
          name: "",
          src: "",
          hidden: false,
          tabIndex: 0,
          style: {},
          setAttribute() {},
          remove() {
            removed.push(this.name);
          },
        };
      }
      return {
        tag,
        href: "",
        target: "",
        download: "",
        rel: "",
        style: {},
        click() {
          clicks.push({ href: this.href, target: this.target, download: this.download });
        },
        remove() {
          removed.push(this.href);
        },
      };
    },
    body: {
      appendChild(node) {
        appended.push(node);
      },
    },
  };
  const window = {
    addEventListener() {},
    location: {
      origin: "http://orqella.localhost:9477",
      host: "orqella.localhost:9477",
      protocol: "http:",
    },
  };
  const context = vm.createContext({
    window,
    document,
    console,
    URL,
    setTimeout: () => 0,
    clearTimeout: () => {},
    location: window.location,
  });
  vm.runInContext(source, context);
  return { frames: window.OrqellaFrames, appended, removed, clicks, window };
}

test("triggerDownload starts a hidden-frame download without navigating the shell", () => {
  const { frames, appended, removed, clicks } = harness();
  assert.equal(
    frames.triggerDownload({
      url: "/api/v1/downloads/0123456789abcdef0123456789abcdef/report.pdf",
      filename: "report.pdf",
    }),
    true,
  );
  assert.equal(appended.length, 2);
  const frame = appended[0];
  const anchor = appended[1];
  assert.equal(frame.tag, "iframe");
  assert.equal(frame.hidden, true);
  assert.equal(anchor.tag, "a");
  assert.equal(anchor.target, frame.name);
  assert.deepEqual(removed, [anchor.href]);
  assert.deepEqual(clicks, [
    {
      href: "/api/v1/downloads/0123456789abcdef0123456789abcdef/report.pdf",
      target: frame.name,
      download: "",
    },
  ]);
});

test("triggerDownload uses a top-level link in the desktop shell", () => {
  const { frames, appended, removed, clicks, window } = harness();
  window.__ORQELLA_NATIVE_TOKEN = "test-token";
  assert.equal(
    frames.triggerDownload({
      url: "/api/v1/downloads/0123456789abcdef0123456789abcdef/report.pdf",
      filename: "report.pdf",
    }),
    true,
  );
  assert.equal(appended.length, 1);
  assert.equal(appended[0].tag, "a");
  assert.equal(appended[0].target, "");
  assert.deepEqual(removed, [appended[0].href]);
  assert.equal(clicks[0].target, "");
});

test("triggerDownload accepts a peer download and rejects other routes", () => {
  const { frames, appended } = harness();
  assert.equal(
    frames.triggerDownload({
      url: "/api/v1/nodes/link-abc/downloads/0123456789abcdef0123456789abcdef/report.pdf",
    }),
    true,
  );
  assert.equal(
    frames.triggerDownload({ url: "/api/v1/modules/files/remove", filename: "x" }),
    false,
  );
  assert.equal(
    frames.triggerDownload({ url: "/api/v1/downloads/not-a-ticket/report.pdf" }),
    false,
  );
  assert.equal(appended.length, 2);
});

test("triggerDownload rejects a missing or external URL", () => {
  const { frames, appended } = harness();
  assert.equal(frames.triggerDownload({ url: "https://example.com/file" }), false);
  assert.equal(frames.triggerDownload({ url: "" }), false);
  assert.equal(frames.triggerDownload(null), false);
  assert.equal(appended.length, 0);
});
