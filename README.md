# SiphonPull

A minimal, native desktop video downloader. Paste a link, get the file. No browser tab, no ads, no account.

Built with **Tauri + Rust** for a tiny footprint and powered by **[yt-dlp](https://github.com/yt-dlp/yt-dlp)** and **ffmpeg** (both bundled).

> SiphonPull is a front end for yt-dlp. Only download content you own, have permission to save, or that is licensed for it. You are responsible for following the terms of the sites you use and the laws where you live.

## Features

- **Instant paste box.** Paste a link (or several) and it is queued immediately. With *watch clipboard* on, links you copy are picked up when you switch back to the app.
- **Clean, original streams.** For TikTok, SiphonPull requests the app-API stream with Chrome impersonation and explicitly excludes yt-dlp's watermarked `download` variant. If only a watermarked stream exists, the download is stopped instead of saving it.
- **Video or audio.** Video at 1080p, 4K or best available (merged to MP4), or audio extracted to MP3.
- **Minimal queue.** Title, status, speed, ETA and percentage per item. Cancel, retry, or show the finished file in your file manager. Two downloads run at once.
- **Light on resources.** The UI is plain HTML, CSS and JavaScript in the system webview (no Chromium bundled, no framework). The Rust shell only spawns and monitors yt-dlp.
- **Monochrome dark UI.** Deep blacks, charcoal grays, thin borders, one muted error tone.
- **Real installers.** Windows setup wizard (`.exe`), macOS `.dmg`, Linux `.AppImage`. End users need no terminal, Node.js, Python, yt-dlp or ffmpeg; everything is bundled.

## Quick start (development)

### Prerequisites

- [Node.js](https://nodejs.org) 18 or newer
- [Rust](https://rustup.rs) (stable)
- Tauri system dependencies for your OS: see the [Tauri prerequisites guide](https://v2.tauri.app/start/prerequisites/)
  - **Linux:** `libwebkit2gtk-4.1-dev build-essential curl wget file libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev`
  - **Windows:** Microsoft C++ Build Tools and WebView2 (preinstalled on Windows 10/11)
  - **macOS:** Xcode Command Line Tools (`xcode-select --install`)

### Run it

```bash
git clone https://github.com/<your-username>/siphonpull.git
cd siphonpull
npm install
npm run dev
```

`npm run dev` first runs `npm run setup`, which downloads the latest yt-dlp release and copies an ffmpeg binary into `src-tauri/binaries/` under the names Tauri expects, then launches the app. The first Rust compile takes a few minutes; later runs are fast.

## Build installers

Run these two commands from the project root (on Windows PowerShell, run them as separate lines):

```bash
npm install
npm run build
```

`npm run build` downloads the yt-dlp and ffmpeg binaries for your machine, compiles the Rust app in release mode, and packages the installer. The first build takes several minutes.

The installer is written to `src-tauri/target/release/bundle/` (relative to the project folder):

| OS      | Installer path                                                        | What the user does                                   |
| ------- | --------------------------------------------------------------------- | ---------------------------------------------------- |
| Windows | `src-tauri\target\release\bundle\nsis\SiphonPull_0.1.0_x64-setup.exe` | Double-click, follow the setup wizard                |
| macOS   | `src-tauri/target/release/bundle/dmg/SiphonPull_0.1.0_aarch64.dmg` (`_x64.dmg` on Intel) | Open the DMG, drag SiphonPull to Applications |
| Linux   | `src-tauri/target/release/bundle/appimage/SiphonPull_0.1.0_amd64.AppImage` | `chmod +x`, then double-click                  |

File names follow the version in `src-tauri/tauri.conf.json`. Tauri does not cross-compile installers, so you get the installer for the OS you build on; the GitHub Actions workflow below builds all three.

What the installers do:

- **Windows:** per-user install (no admin prompt), Start Menu entry, and a "Create desktop shortcut" option on the final wizard page. The WebView2 runtime is embedded if it is missing.
- **macOS:** a standard drag-to-Applications DMG. Builds are ad-hoc signed, so on first launch right-click the app and choose Open. A paid Apple Developer ID removes that prompt.
- **Linux:** an AppImage is a single portable file and does not add a desktop icon by itself; tools like AppImageLauncher can integrate it. Some distros need `libfuse2` installed to run AppImages.

yt-dlp and ffmpeg are declared as `externalBin` sidecars, so Tauri packs them inside the installer and places them next to the app executable. Nothing is downloaded at runtime.

### Automated releases (GitHub Actions)

`.github/workflows/release.yml` builds the Windows `.exe`, macOS `.dmg` (Apple Silicon) and Linux `.AppImage` installers and attaches them to a **draft** GitHub Release.

1. Bump `version` in `src-tauri/tauri.conf.json`, `src-tauri/Cargo.toml` and `package.json`.
2. Commit, then tag and push:

   ```bash
   git tag v0.1.0
   git push origin v0.1.0
   ```

3. When the workflow finishes, open the draft release on GitHub, check the files, and publish it.

For Intel Macs, run `npm run build` on an Intel Mac, or add a matching runner to the workflow matrix.

> Unsigned builds trigger Windows SmartScreen and macOS Gatekeeper warnings. For public distribution, add code-signing certificates; see the [Tauri signing docs](https://v2.tauri.app/distribute/sign/).

## How it works

```
┌────────────────────┐   invoke / events   ┌──────────────────────┐   spawn   ┌────────┐
│ src/ (HTML/CSS/JS) │ ──────────────────► │ src-tauri/src/main.rs │ ────────► │ yt-dlp │
│ paste box + queue  │ ◄────────────────── │ Rust shell            │ ◄──────── │ ffmpeg │
└────────────────────┘  progress, done     └──────────────────────┘  stdout    └────────┘
```

- `fetch_info` runs `yt-dlp --dump-single-json` to get the title, uploader and duration.
- `start_download` spawns yt-dlp with a custom progress template, parses its output line by line and emits `download-progress`, `download-done`, `download-error` and `download-cancelled` events.
- `cancel_download` terminates yt-dlp and its child processes.
- yt-dlp and ffmpeg ship as Tauri [sidecars](https://v2.tauri.app/develop/sidecar/) and are installed next to the app executable. If no bundled yt-dlp is found, the app falls back to `yt-dlp` on your `PATH`.

## Project layout

```
siphonpull/
├── src/                      Frontend (no build step)
│   ├── index.html
│   ├── styles.css
│   └── main.js
├── src-tauri/
│   ├── src/main.rs           Rust backend and yt-dlp process handling
│   ├── tauri.conf.json       Window, bundle and sidecar configuration
│   ├── capabilities/         Permissions for the main window
│   ├── icons/                App icons
│   ├── binaries/             yt-dlp and ffmpeg sidecars (fetched by `npm run setup`)
│   ├── Cargo.toml
│   └── build.rs
├── scripts/fetch-sidecars.mjs
├── app-icon.png              Icon source (regenerate sizes with `npx tauri icon app-icon.png`)
└── .github/workflows/release.yml
```

## Keeping yt-dlp current

Sites change often and yt-dlp ships fixes frequently. To refresh the bundled binary, run:

```bash
FORCE=1 npm run setup
```

then rebuild. If a site suddenly stops working, updating yt-dlp is the first thing to try.

## Troubleshooting

- **"yt-dlp: not found" in the footer.** Run `npm run setup`, or install yt-dlp and make sure it is on your `PATH`.
- **Linux AppImage won't start.** Make it executable (`chmod +x SiphonPull*.AppImage`) and install `libfuse2` if your distro does not ship it.
- **First download is slow to start.** The single-file yt-dlp builds unpack themselves on launch. This is normal.
- **TikTok/Instagram errors.** Some sites rate-limit or require login for certain content. Update yt-dlp and try again.

## Contributing

Issues and pull requests are welcome. Keep the app small: no heavy frameworks, no extra background processes, no bright colors.

## License

[MIT](LICENSE). yt-dlp is released under the Unlicense; ffmpeg binaries are provided by [`ffmpeg-static`](https://github.com/eugeneware/ffmpeg-static) and are subject to their own licenses.
