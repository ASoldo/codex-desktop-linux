#!/usr/bin/env node
// SPDX-FileCopyrightText: 2026 Arch Linux Contributors
// SPDX-License-Identifier: 0BSD
//
// Codex Desktop reconciles the bundled marketplace before the renderer has
// finished publishing Linux's in-app Browser feature availability. The first
// pass consequently omits Browser and treats an existing installation as
// disabled, uninstalling it. A second startup pass restores Browser to the
// marketplace, but cannot restore the user's prior installation state.
//
// Preserve an installed Browser during that transient Linux-only omission.
// The normal reconciler still controls availability and installation, while
// every other bundled plugin keeps the upstream removal behavior.

import {
  existsSync,
  readdirSync,
  readFileSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { join } from "node:path";

const TAG = "patch-linux-browser-persistence";
const appRoot = process.argv[2] ?? "app-extracted";
const buildRoot = join(appRoot, ".vite", "build");

function fail(message) {
  console.error(`${TAG}: ${message}`);
  process.exit(1);
}

if (!existsSync(buildRoot) || !statSync(buildRoot).isDirectory()) {
  fail(`could not find Vite build directory: ${buildRoot}`);
}

const mainFiles = readdirSync(buildRoot, { withFileTypes: true })
  .filter((entry) => entry.isFile() && /^main-[^/]+\.js$/.test(entry.name))
  .map((entry) => join(buildRoot, entry.name));

if (mainFiles.length !== 1) {
  fail(`expected one main-*.js bundle, found ${mainFiles.length}`);
}

const mainFile = mainFiles[0];
let source = readFileSync(mainFile, "utf8");

const unpatched =
  "l=s?.plugins.filter(e=>e.installed&&!c.has(e.name))??[]";
const patched =
  "l=s?.plugins.filter(e=>e.installed&&!c.has(e.name)&&!(process.platform===`linux`&&e.name===`browser`))??[]";
const unpatchedCount = source.split(unpatched).length - 1;
const patchedCount = source.split(patched).length - 1;

if (unpatchedCount === 1 && patchedCount === 0) {
  source = source.replace(unpatched, patched);
  writeFileSync(mainFile, source);
  console.log(`${TAG}: patched ${mainFile}`);
} else if (unpatchedCount === 0 && patchedCount === 1) {
  console.log(`${TAG}: main bundle already patched`);
} else {
  fail(
    `expected one bundled-plugin cleanup fragment (unpatched=${unpatchedCount}, patched=${patchedCount})`,
  );
}

const verified = readFileSync(mainFile, "utf8");
if (verified.includes(unpatched) || !verified.includes(patched)) {
  fail("patch verification failed");
}
