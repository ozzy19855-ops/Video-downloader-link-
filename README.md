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
