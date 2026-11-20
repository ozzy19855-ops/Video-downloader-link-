// Downloads the yt-dlp and ffmpeg binaries Tauri bundles as "sidecars".
// Tauri expects them at src-tauri/binaries/<name>-<target-triple>[.exe].
import { execSync } from 'node:child_process';
import { mkdirSync, copyFileSync, writeFileSync, chmodSync, existsSync } from 'node:fs';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const require = createRequire(import.meta.url);
const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const outDir = path.join(root, 'src-tauri', 'binaries');
mkdirSync(outDir, { recursive: true });

const triple = 
  process.env.TAURI_ENV_TARGET_TRIPLE || 
  process.env.TARGET_TRIPLE || 
  execSync('rustc -vV').toString().match(/host:\s*(\S+)/)[1];

const isWin = triple.includes('windows');
const isMac = triple.includes('apple-darwin');
const isArmLinux = triple.startsWith('aarch64') && triple.includes('linux');

const ext = isWin ? '.exe' : '';
const ytAsset = isWin ? 'yt-dlp.exe' : isMac ? 'yt-dlp_macos' : isArmLinux ? 'yt-dlp_linux_aarch64' : 'yt-dlp_linux';

const ytDest = path.join(outDir, `yt-dlp-${triple}${ext}`);
const ffDest = path.join(outDir, `ffmpeg-${triple}${ext}`);

async function fetchYtDlp() {
  if (existsSync(ytDest) && !process.env.FORCE) {
    console.log(`yt-dlp already present: ${path.basename(ytDest)}`);
    return;
  }
  const url = `https://github.com/yt-dlp/yt-dlp/releases/latest/download/${ytAsset}`;
  console.log(`Downloading ${url}`);
  const res = await fetch(url, { redirect: 'follow' });
  if (!res.ok) throw new Error(`yt-dlp download failed: HTTP ${res.status}`);
  writeFileSync(ytDest, Buffer.from(await res.arrayBuffer()));
  if (!isWin) chmodSync(ytDest, 0o755);
  console.log(`Saved ${path.basename(ytDest)}`);
}

function copyFfmpeg() {
  if (existsSync(ffDest) && !process.env.FORCE) {
    console.log(`ffmpeg already present: ${path.basename(ffDest)}`);
    return;
  }
  const src = require('ffmpeg-static');
  if (!src || !existsSync(src)) {
    throw new Error('ffmpeg-static did not provide a binary. Run `npm install` first.');
  }
  copyFileSync(src, ffDest);
  if (!isWin) chmodSync(ffDest, 0o755);
  console.log(`Saved ${path.basename(ffDest)}`);
}

await fetchYtDlp();
copyFfmpeg();
console.log(`Sidecars ready for ${triple}`);