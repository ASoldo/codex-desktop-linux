#!/usr/bin/env node
// SPDX-FileCopyrightText: 2026 Arch Linux Contributors
// SPDX-License-Identifier: 0BSD
//
// Codex Desktop derives xterm's font from the active editor theme. On Linux,
// that normally resolves to a generic monospace face without Powerline/Nerd
// Font glyphs. Put the installed Nerd Font first while retaining the theme's
// original font stack as a fallback.

import {
  existsSync,
  readdirSync,
  readFileSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { join } from "node:path";

const TAG = "patch-linux-terminal-font";
const appRoot = process.argv[2] ?? "app-extracted";
const webviewAssets = join(appRoot, "webview", "assets");

function fail(message) {
  console.error(`${TAG}: ${message}`);
  process.exit(1);
}

if (!existsSync(webviewAssets) || !statSync(webviewAssets).isDirectory()) {
  fail(`could not find webview assets: ${webviewAssets}`);
}

const terminalFiles = readdirSync(webviewAssets, { withFileTypes: true })
  .filter((entry) => entry.isFile() && entry.name.endsWith(".js"))
  .map((entry) => join(webviewAssets, entry.name))
  .filter((file) => readFileSync(file, "utf8").includes("data-codex-xterm"));

const interactiveFiles = terminalFiles.filter((file) =>
  readFileSync(file, "utf8").includes("data-codex-terminal"),
);
const outputFiles = terminalFiles.filter((file) => {
  const source = readFileSync(file, "utf8");
  return (
    !source.includes("data-codex-terminal") &&
    source.includes("disableStdin:!0") &&
    source.includes(".options.fontFamily=")
  );
});

if (interactiveFiles.length !== 1 || outputFiles.length !== 1) {
  fail(
    `expected one interactive and one output xterm bundle (interactive=${interactiveFiles.length}, output=${outputFiles.length})`,
  );
}

const patchedFontRe =
  /([A-Za-z_$][\w$]*)=`"JetBrainsMono Nerd Font Mono", \$\{\1\}`,/g;

function patchTerminal(file, kind, initializerRe, fontVariableIndex) {
  let source = readFileSync(file, "utf8");
  const initializerMatches = [...source.matchAll(initializerRe)];
  const patchedFontMatches = [...source.matchAll(patchedFontRe)];

  if (initializerMatches.length === 1 && patchedFontMatches.length === 0) {
    const fullMatch = initializerMatches[0][0];
    const fontVariable = initializerMatches[0][fontVariableIndex];
    const patched = `${fullMatch}${fontVariable}=\`"JetBrainsMono Nerd Font Mono", \${${fontVariable}}\`,`;
    source = source.replace(fullMatch, patched);
    writeFileSync(file, source);
    console.log(`${TAG}: patched ${kind} terminal bundle: ${file}`);
  } else if (
    initializerMatches.length === 1 &&
    patchedFontMatches.length === 1
  ) {
    console.log(`${TAG}: ${kind} terminal bundle already patched`);
  } else {
    fail(
      `expected one ${kind} terminal font initializer (initializer=${initializerMatches.length}, patched=${patchedFontMatches.length})`,
    );
  }

  const verified = [...source.matchAll(patchedFontRe)];
  if (verified.length !== 1) {
    fail(
      `${kind} terminal patch verification failed: Nerd Font prefix count is ${verified.length}`,
    );
  }
}

patchTerminal(
  outputFiles[0],
  "output",
  /let\s+([A-Za-z_$][\w$]*)=([A-Za-z_$][\w$]*)\.fonts\.code\?\.trim\(\)\|\|`ui-monospace,[^`]*monospace`;/g,
  1,
);
patchTerminal(
  interactiveFiles[0],
  "interactive",
  /let\s+([A-Za-z_$][\w$]*)=([A-Za-z_$][\w$]*)\.fonts\.code\?\.trim\(\)\?\?``,([A-Za-z_$][\w$]*)=\1\.length>0\?\1:[A-Za-z_$][\w$]*;/g,
  3,
);
