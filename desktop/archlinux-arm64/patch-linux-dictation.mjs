#!/usr/bin/env node
// SPDX-FileCopyrightText: 2026 Arch Linux Contributors
// SPDX-License-Identifier: 0BSD
//
// Enable Codex Desktop's global dictation controller on Linux/X11. Upstream
// ships the recorder and transcription renderer on every platform, but gates
// the desktop shortcut/paste lifecycle to macOS and Windows. Electron handles
// Linux global shortcut registration; xinput observes the matching key release
// for hold-to-dictate and xdotool pastes the completed transcript into the
// field that retained focus while the non-activating recorder was visible.

import {
  existsSync,
  readdirSync,
  readFileSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { join } from "node:path";

const TAG = "patch-linux-dictation";
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
let changed = false;

function replaceUnique(label, unpatched, patched) {
  const unpatchedCount = source.split(unpatched).length - 1;
  const patchedCount = source.split(patched).length - 1;

  if (unpatchedCount === 1 && patchedCount === 0) {
    source = source.replace(unpatched, patched);
    changed = true;
    console.log(`${TAG}: patched ${label}`);
    return;
  }

  if (unpatchedCount === 0 && patchedCount === 1) {
    console.log(`${TAG}: ${label} already patched`);
    return;
  }

  fail(
    `expected one ${label} fragment (unpatched=${unpatchedCount}, patched=${patchedCount})`,
  );
}

replaceUnique(
  "platform capability gate",
  "function K7(){return process.platform===`darwin`||process.platform===`win32`}",
  "function K7(){return process.platform===`darwin`||process.platform===`win32`||process.platform===`linux`}",
);

const shortcutUnpatched =
  /function ([A-Za-z_$][\w$]*)\(e,t\)\{return t===`darwin`\?([A-Za-z_$][\w$]*)\(e\)\.length>0:([A-Za-z_$][\w$]*)\(e,t\)!=null\}/g;
const shortcutPatched =
  /function ([A-Za-z_$][\w$]*)\(e,t\)\{return t===`linux`\?!0:t===`darwin`\?([A-Za-z_$][\w$]*)\(e\)\.length>0:([A-Za-z_$][\w$]*)\(e,t\)!=null\}/g;
const shortcutUnpatchedMatches = [...source.matchAll(shortcutUnpatched)];
const shortcutPatchedMatches = [...source.matchAll(shortcutPatched)];

if (shortcutUnpatchedMatches.length === 1 && shortcutPatchedMatches.length === 0) {
  const [fullMatch, functionName, darwinValidator, otherValidator] =
    shortcutUnpatchedMatches[0];
  source = source.replace(
    fullMatch,
    `function ${functionName}(e,t){return t===\`linux\`?!0:t===\`darwin\`?${darwinValidator}(e).length>0:${otherValidator}(e,t)!=null}`,
  );
  changed = true;
  console.log(`${TAG}: patched Linux shortcut validation`);
} else if (
  shortcutUnpatchedMatches.length === 0 &&
  shortcutPatchedMatches.length === 1
) {
  console.log(`${TAG}: Linux shortcut validation already patched`);
} else {
  fail(
    `expected one Linux shortcut validation fragment (unpatched=${shortcutUnpatchedMatches.length}, patched=${shortcutPatchedMatches.length})`,
  );
}

const watcherBinding = source.match(
  /return ([A-Za-z_$][\w$]*)\(\(0,([A-Za-z_$][\w$]*)\.spawn\)\(`powershell\.exe`/,
);
if (!watcherBinding) {
  fail("could not find the Windows dictation release-watcher bindings");
}
const watcherHelperName = watcherBinding[1];
const spawnBindingName = watcherBinding[2];

replaceUnique(
  "Linux hold shortcut release watcher",
  "case`aix`:case`android`:case`cygwin`:case`freebsd`:case`haiku`:case`linux`:case`netbsd`:case`openbsd`:case`sunos`:throw Error(`Global dictation hotkey release watching is not supported.`)",
  `case\`linux\`:{let n=(0,${spawnBindingName}.spawn)(\`/bin/sh\`,[\`-c\`,\`xinput test-xi2 --root 2>/dev/null | awk '/EVENT type 3 \\\\(KeyRelease\\\\)/ { exit }'\`],{stdio:\`ignore\`});return ${watcherHelperName}(n,t)}case\`aix\`:case\`android\`:case\`cygwin\`:case\`freebsd\`:case\`haiku\`:case\`netbsd\`:case\`openbsd\`:case\`sunos\`:throw Error(\`Global dictation hotkey release watching is not supported.\`)`,
);

const pasteHelper = source.match(
  /case`darwin`:[\s\S]{0,500}?await ([A-Za-z_$][\w$]*)\(`\/usr\/bin\/osascript`/,
);
if (!pasteHelper) {
  fail("could not find the macOS dictation paste helper binding");
}
const pasteHelperName = pasteHelper[1];

replaceUnique(
  "Linux transcript paste",
  "case`aix`:case`android`:case`cygwin`:case`freebsd`:case`haiku`:case`linux`:case`netbsd`:case`openbsd`:case`sunos`:throw Error(`Global dictation paste is not supported on this OS.`)",
  `case\`linux\`:await ${pasteHelperName}(\`/usr/bin/xdotool\`,[\`key\`,\`--clearmodifiers\`,\`ctrl+v\`]);return;case\`aix\`:case\`android\`:case\`cygwin\`:case\`freebsd\`:case\`haiku\`:case\`netbsd\`:case\`openbsd\`:case\`sunos\`:throw Error(\`Global dictation paste is not supported on this OS.\`)`,
);

if (changed) {
  writeFileSync(mainFile, source);
  console.log(`${TAG}: patched ${mainFile}`);
}

for (const marker of [
  "process.platform===`win32`||process.platform===`linux`",
  "t===`linux`?!0:t===`darwin`",
  "xinput test-xi2 --root",
  "/usr/bin/xdotool",
]) {
  if (!source.includes(marker)) {
    fail(`patch verification failed; missing marker: ${marker}`);
  }
}
