#!/usr/bin/env node
// SPDX-FileCopyrightText: 2026 Arch Linux Contributors
// SPDX-License-Identifier: 0BSD
//
// Stabilize Codex window surfaces on Linux. Transparent BrowserWindow
// backgrounds intended for native macOS/Windows effects can render poorly on
// Linux compositors, and Linux cannot forward mouse movement through an
// ignored window. Keep normal surfaces opaque while giving the pet a shaped,
// transparent, interactive input region.

import {
  existsSync,
  readdirSync,
  readFileSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { join } from "node:path";

const TAG = "patch-linux-opaque-bg";
const appRoot = process.argv[2] ?? "app-extracted";
const buildRoot = join(appRoot, ".vite", "build");
const webviewAssets = join(appRoot, "webview", "assets");
const webviewIndex = join(appRoot, "webview", "index.html");

function fail(message) {
  console.error(`${TAG}: ${message}`);
  process.exit(1);
}

function ensureDirectory(dir, label) {
  if (!existsSync(dir) || !statSync(dir).isDirectory()) {
    fail(`could not find ${label}: ${dir}`);
  }
}

function jsFiles(dir) {
  return readdirSync(dir, { withFileTypes: true })
    .filter((entry) => entry.isFile() && entry.name.endsWith(".js"))
    .map((entry) => join(dir, entry.name));
}

ensureDirectory(buildRoot, "Vite build directory");

const mainFiles = jsFiles(buildRoot).filter((file) =>
  /\/main-[^/]+\.js$/.test(file),
);

if (mainFiles.length !== 1) {
  fail(`expected one main-*.js bundle, found ${mainFiles.length}`);
}

const mainFile = mainFiles[0];
let mainSource = readFileSync(mainFile, "utf8");

const opaqueGateRe =
  /function\s+([A-Za-z_$][\w$]*)\(\{appearance:([A-Za-z_$][\w$]*),opaqueWindowsEnabled:([A-Za-z_$][\w$]*),platform:([A-Za-z_$][\w$]*)\}\)\{return\s+\3&&!([A-Za-z_$][\w$]*)\(\2\)&&\(\4===`darwin`\|\|\4===`win32`\)\}/;
const patchedOpaqueGateRe =
  /function\s+([A-Za-z_$][\w$]*)\(\{appearance:([A-Za-z_$][\w$]*),opaqueWindowsEnabled:([A-Za-z_$][\w$]*),platform:([A-Za-z_$][\w$]*)\}\)\{return\s+\3&&!([A-Za-z_$][\w$]*)\(\2\)&&\(\4===`darwin`\|\|\4===`win32`\|\|\4===`linux`\)\}/;

function matches(source, regex) {
  return [...source.matchAll(new RegExp(regex.source, "g"))];
}

const opaqueGateMatches = matches(mainSource, opaqueGateRe);
const patchedOpaqueGateMatches = matches(mainSource, patchedOpaqueGateRe);
if (opaqueGateMatches.length === 1 && patchedOpaqueGateMatches.length === 0) {
  const fullMatch = opaqueGateMatches[0][0];
  const platformVar = opaqueGateMatches[0][4];
  const patched = `${fullMatch.slice(0, -2)}||${platformVar}===\`linux\`)}`;

  mainSource = mainSource.replace(fullMatch, patched);
  writeFileSync(mainFile, mainSource);
  console.log(`${TAG}: patched ${mainFile}`);
} else if (
  opaqueGateMatches.length === 0 &&
  patchedOpaqueGateMatches.length === 1
) {
  console.log(`${TAG}: main bundle already patched`);
} else {
  fail(
    `expected one opaque-window platform gate (unpatched=${opaqueGateMatches.length}, patched=${patchedOpaqueGateMatches.length})`,
  );
}

if (
  matches(mainSource, opaqueGateRe).length !== 0 ||
  matches(mainSource, patchedOpaqueGateRe).length !== 1
) {
  fail("patch verification failed: Linux opaque-window gate not found");
}

function replaceUniqueMainBundleFragment(label, unpatched, patched) {
  const unpatchedCount = mainSource.split(unpatched).length - 1;
  const patchedCount = mainSource.split(patched).length - 1;

  if (unpatchedCount === 1 && patchedCount === 0) {
    mainSource = mainSource.replace(unpatched, patched);
    console.log(`${TAG}: patched ${label}`);
    return true;
  }

  if (unpatchedCount === 0 && patchedCount === 1) {
    console.log(`${TAG}: ${label} already patched`);
    return false;
  }

  fail(
    `expected one ${label} fragment (unpatched=${unpatchedCount}, patched=${patchedCount})`,
  );
}

const linuxPetPatches = [
  {
    label: "pet tray visibility state",
    unpatched: "resolutionKey=null;traySize=null;compositionHost;",
    patched:
      "resolutionKey=null;traySize=null;trayVisible=!1;compositionHost;",
  },
  {
    label: "pet tray visibility reset",
    unpatched:
      "this.resolutionKey=null,this.traySize=null,process.platform===`darwin`",
    patched:
      "this.resolutionKey=null,this.traySize=null,this.trayVisible=!1,process.platform===`darwin`",
  },
  {
    label: "pet tray visibility update",
    unpatched:
      "this.pendingElementSizeRevision=t??null;let s=n==null?`native`:`legacy`",
    patched:
      "this.pendingElementSizeRevision=t??null,this.trayVisible=n===!0;let s=n==null?`native`:`legacy`",
  },
  {
    label: "Linux pet input shape",
    unpatched:
      "this.setWindowBounds(e,a.windowBounds,n,r),this.compositionHost.updateMascotRect(a.mascot)",
    patched:
      "this.setWindowBounds(e,a.windowBounds,n,r),process.platform===`linux`&&e.setShape([a.mascot,...this.trayVisible&&a.tray!=null?[a.tray]:[]].map(({left:e,top:t,width:n,height:r})=>({x:Math.floor(e),y:Math.floor(t),width:Math.ceil(n),height:Math.ceil(r)}))),this.compositionHost.updateMascotRect(a.mascot)",
  },
  {
    label: "Linux pet pointer interactivity",
    unpatched:
      "applyPointerInteractivityPolicy(){let e=this.window;if(e==null||e.isDestroyed()){this.mousePassthroughEnabled=!1;return}let t=!this.pointerInteractive;",
    patched:
      "applyPointerInteractivityPolicy(){let e=this.window;if(e==null||e.isDestroyed()){this.mousePassthroughEnabled=!1;return}if(process.platform===`linux`){this.mousePassthroughEnabled=!1,e.setIgnoreMouseEvents(!1);return}let t=!this.pointerInteractive;",
  },
];

let linuxPetPatched = false;
for (const patch of linuxPetPatches) {
  linuxPetPatched =
    replaceUniqueMainBundleFragment(
      patch.label,
      patch.unpatched,
      patch.patched,
    ) || linuxPetPatched;
}

if (linuxPetPatched) {
  writeFileSync(mainFile, mainSource);
}

for (const patch of linuxPetPatches) {
  if (
    mainSource.includes(patch.unpatched) ||
    !mainSource.includes(patch.patched)
  ) {
    fail(`patch verification failed: ${patch.label}`);
  }
}

if (!existsSync(webviewAssets) || !statSync(webviewAssets).isDirectory()) {
  console.log(`${TAG}: no webview/assets directory found, skipping theme patch`);
  process.exit(0);
}

const falseThemeFlagRe = /opaqueWindows:(?:!1|false)/g;
const trueThemeFlagRe = /opaqueWindows:(?:!0|true)/g;
let themeFilesPatched = 0;
let themeFlagsPatched = 0;
let themeFlagsFound = 0;
let falseThemeFlagsRemaining = 0;
for (const file of jsFiles(webviewAssets)) {
  const source = readFileSync(file, "utf8");
  const falseFlags = source.match(falseThemeFlagRe)?.length ?? 0;
  const trueFlags = source.match(trueThemeFlagRe)?.length ?? 0;
  const updated = source
    .replaceAll("opaqueWindows:!1", "opaqueWindows:!0")
    .replaceAll("opaqueWindows:false", "opaqueWindows:true");

  themeFlagsFound += falseFlags + trueFlags;
  themeFlagsPatched += falseFlags;
  falseThemeFlagsRemaining += updated.match(falseThemeFlagRe)?.length ?? 0;
  if (updated === source) {
    continue;
  }

  writeFileSync(file, updated);
  themeFilesPatched += 1;
  console.log(`${TAG}: patched theme defaults in ${file}`);
}

if (themeFlagsFound === 0) {
  fail("could not find any opaqueWindows theme flags");
}

if (falseThemeFlagsRemaining !== 0) {
  fail(
    `patch verification failed: ${falseThemeFlagsRemaining} transparent theme flag(s) remain`,
  );
}

if (themeFlagsPatched === 0) {
  console.log(`${TAG}: theme defaults already patched`);
} else {
  console.log(
    `${TAG}: patched ${themeFlagsPatched} theme flag(s) in ${themeFilesPatched} file(s)`,
  );
}

const avatarTransparencyMarker = "codex-linux-avatar-overlay-transparency";
const avatarTransparencyBlock = `    <style>
      /* ${avatarTransparencyMarker} */
      html:has([data-avatar-overlay-content-frame="true"]),
      html:has([data-avatar-overlay-content-frame="true"]) body {
        background: transparent !important;
        background-color: transparent !important;
        background-image: none !important;
      }
    </style>`;

if (!existsSync(webviewIndex) || !statSync(webviewIndex).isFile()) {
  fail(`could not find webview index: ${webviewIndex}`);
}

let webviewSource = readFileSync(webviewIndex, "utf8");
if (!webviewSource.includes(avatarTransparencyMarker)) {
  const closingHeadMatches = webviewSource.match(/<\/head>/g)?.length ?? 0;
  if (closingHeadMatches !== 1) {
    fail(`expected one closing head tag, found ${closingHeadMatches}`);
  }

  webviewSource = webviewSource.replace(
    /([ \t]*)<\/head>/,
    `${avatarTransparencyBlock}\n$1</head>`,
  );
  writeFileSync(webviewIndex, webviewSource);
  console.log(`${TAG}: patched avatar overlay transparency in ${webviewIndex}`);
} else {
  console.log(`${TAG}: avatar overlay transparency already patched`);
}

if (
  !webviewSource.includes(avatarTransparencyMarker) ||
  !webviewSource.includes(
    'html:has([data-avatar-overlay-content-frame="true"]) body',
  )
) {
  fail("patch verification failed: avatar overlay transparency rule not found");
}
