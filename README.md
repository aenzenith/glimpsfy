<div align="center">
  <img src=".github/readme/icon.png" width="112" height="112" alt="">
  <h1>Glimpsfy</h1>
  <p>
    <b>Capture it, mark it up, hand it over.</b><br>
    Screenshots, screen recordings and text out of any pixel, one shortcut away in the menu bar.
  </p>
  <p>
    <a href="https://github.com/aenzenith/glimpsfy/releases/latest"><b>Download</b></a>
  </p>
  <sub>Free and open source · macOS 14+, with Windows and Linux from the next release · seven languages</sub>
</div>

<br>

Taking a screenshot, marking it up and sending it usually takes three apps: one to capture, one
to draw on it and one to read the text off it, with another for screen recordings. Glimpsfy puts
the whole loop in one app. You press a key, select a region, annotate it where you took it, and
the result is on your clipboard, on your disk or behind a link within seconds.

## Features

- **Capture** takes an area, a window, a display or all displays, with a loupe, window snapping
  and a delayed mode.
- **Annotate** draws arrows, shapes, text, numbered steps, blur and pixelate on a toolbar attached
  to the selection.
- **OCR** copies the text of any region and decodes QR codes offline: through Apple's Vision
  engine on macOS, Windows' own engine on Windows, and PaddleOCR models that Glimpsfy downloads
  once on Windows and Linux.
- **Record** captures the screen with system audio and the microphone, then trims it or exports it
  as a GIF.
- **Share** copies, saves, pins or uploads a capture to a free image host, an S3 bucket or any
  ShareX endpoint.
- **History** keeps every capture and makes the text inside them searchable.

Glimpsfy has no account and no analytics, and nothing leaves your machine unless you upload it.

## Install

Download the latest build from [Releases](https://github.com/aenzenith/glimpsfy/releases/latest).

- **macOS 14+** (Apple Silicon): drag Glimpsfy from the `.dmg` to Applications. It is signed and notarised, so
  the first time, macOS asks whether to open an app downloaded from the internet; click Open. The
  first-run screen then walks you through the Screen Recording, Microphone and Accessibility
  permissions.
- **Windows 10/11** (`-setup.exe` or `.msi`): the builds are attached to every release from the
  next one on. The setup installs for the current user without an administrator prompt, and it
  fetches WebView2 when the machine doesn't have it. The installers aren't signed yet, so
  SmartScreen stops the first start once: click More info, then Run anyway.
- **Linux** (`.deb`, `.rpm` or `.AppImage`): the builds are attached to every release from the
  next one on. `sudo apt install ./Glimpsfy_*.deb` or `sudo dnf install ./Glimpsfy-*.rpm` brings
  the GStreamer plugins that recording needs. The AppImage carries its own, so you
  `chmod +x Glimpsfy_*.AppImage` and run it; it still needs FUSE to start, xdg-desktop-portal and
  PipeWire to capture on Wayland, and a Secret Service such as GNOME Keyring to keep upload keys.
  Glimpsfy lives in the tray, which GNOME shows through its AppIndicator extension.

## Development

You need Node 22 and Rust 1.94 or newer. macOS also needs the Xcode Command Line Tools, Windows
the Visual Studio C++ Build Tools, and Linux the packages listed in
`tools/linux-check/packages.txt` (WebKitGTK 4.1, GStreamer and the X11 libraries).
`tools/linux-check/run.sh` runs the Linux checks in Docker from any machine.

```sh
npm ci
npm run tauri dev
cargo test --workspace && npm test
```

## Licence

[MIT](LICENSE)
