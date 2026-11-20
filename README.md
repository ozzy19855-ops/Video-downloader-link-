# SiphonPull

A minimal, native desktop video downloader. Paste a link, get the file. No browser tab, no ads, no account.

Built with **Tauri + Rust** for a tiny footprint and powered by **[yt-dlp](https://github.com/yt-dlp/yt-dlp)** and **ffmpeg** (both bundled).

> SiphonPull is a front end for yt-dlp. Only download content you own, have permission to save, or that is licensed for it. You are responsible for following the terms of the sites you use and the laws where you live.

## Features

- **Instant paste box.** Paste a link (or several) and it is queued immediately. With *watch clipboard* on, links you copy are picked up when you switch back to the app.
- **Clean, original streams.** For TikTok, SiphonPull requests the app-API stream with Chrome impersonation and explicitly excludes yt-dlp's watermarked `download` variant. If only a watermarked stream exists, the download is stopped instead of saving it.
- **Embedded streams.** If yt-dlp has no extractor for a page (or hits an SSL error), SiphonPull opens the page in an invisible WebView for up to 20 seconds, captures the `.m3u8` / `.mpd` request the player makes together with its Referer and User-Agent, closes the WebView and downloads the stream with those headers. Downloads retry up to 15 times and fetch 4 fragments at once.
- **`siphonpull://` links.** `siphonpull://download?url=<stream>&referer=<page>&ua=<user agent>&title=<name>` (values percent-encoded) queues a stream straight from a browser extension.
- **Video or audio.** Video at 1080p, 4K or best available (merged to MP4), or audio extracted to MP3.
- **Minimal queue.** Title, status, speed, ETA and percentage per item. Cancel, retry, or show the finished file in your file manager. Two downloads run at once.
- **Light on resources.** The UI is plain HTML, CSS and JavaScript in the system webview (no Chromium bundled, no framework). The Rust shell only spawns and monitors yt-dlp.
- **Monochrome dark UI.** Deep blacks, charcoal grays, thin borders, one muted error tone.
- **Real installers.** Windows setup wizard (`.exe`), macOS `.dmg`, Linux `.AppImage`. End users need no terminal, Node.js, Python, yt-dlp or ffmpeg; everything is bundled.

the project uses the SignPath Foundation for code signing.

