// Unit tests for the pure filename → icon mapping (issue #174). Run with `npm test`.
import { test } from "node:test";
import assert from "node:assert/strict";
import {
  iconCategory,
  iconSvg,
  fileIconSvg,
  folderIconSvg,
  type IconCategory,
} from "../src/fileicons.ts";

test("known extensions map to their category", () => {
  assert.equal(iconCategory("main.ts"), "code");
  assert.equal(iconCategory("app.jsx"), "code");
  assert.equal(iconCategory("lib.rs"), "rust");
  assert.equal(iconCategory("script.py"), "python");
  assert.equal(iconCategory("data.json"), "json");
  assert.equal(iconCategory("README.md"), "markdown");
  assert.equal(iconCategory("index.html"), "web");
  assert.equal(iconCategory("style.css"), "style");
  assert.equal(iconCategory("run.sh"), "shell");
  assert.equal(iconCategory("logo.png"), "image");
  assert.equal(iconCategory("Cargo.toml"), "config");
});

test("classification is case-insensitive", () => {
  assert.equal(iconCategory("MAIN.TS"), "code");
  assert.equal(iconCategory("Photo.PNG"), "image");
});

test("multi-dot names use the final extension", () => {
  assert.equal(iconCategory("app.test.ts"), "code");
  assert.equal(iconCategory("vite.config.js"), "code");
  assert.equal(iconCategory("archive.tar.gz"), "file"); // gz unknown → generic
});

test("dotfiles and special base names resolve sanely", () => {
  assert.equal(iconCategory(".gitignore"), "config");
  assert.equal(iconCategory(".editorconfig"), "config");
  assert.equal(iconCategory("Dockerfile"), "config");
  assert.equal(iconCategory("Makefile"), "config");
  assert.equal(iconCategory("Cargo.lock"), "lock");
  assert.equal(iconCategory("package-lock.json"), "lock");
});

test("unknown / edge names fall back to the generic file bucket, never throw", () => {
  assert.equal(iconCategory("mystery.qzx"), "file");
  assert.equal(iconCategory("noextension"), "file");
  assert.equal(iconCategory(".foorc"), "file"); // unknown dotfile
  assert.equal(iconCategory(""), "file");
  assert.doesNotThrow(() => iconCategory(""));
});

test("every category yields a non-empty inline SVG", () => {
  const cats: IconCategory[] = [
    "folder", "folder-open", "code", "rust", "python", "json", "markdown",
    "web", "style", "shell", "image", "config", "lock", "text", "file",
  ];
  for (const c of cats) {
    const s = iconSvg(c);
    assert.ok(s.startsWith("<svg") && s.includes("currentColor"), `bad svg for ${c}`);
  }
});

test("convenience helpers return SVG strings", () => {
  assert.ok(fileIconSvg("main.ts").startsWith("<svg"));
  assert.notEqual(folderIconSvg(true), folderIconSvg(false));
});
