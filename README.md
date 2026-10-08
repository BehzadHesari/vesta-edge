<div align="center">

<img src="docs/icon.png" width="96" alt="Vesta Edge icon">

# Vesta Edge

**A liquid side dock for the edge of your screen.**<br>
Drag it to any edge, and it sticks there: emoji, clipboard history with styled paste, colours, notes, your apps and a set of handy tools, one hover away.

[فارسی](README.fa.md) · [Download](https://github.com/BehzadHesari/vesta-edge/releases/latest) · [Report a problem](https://github.com/BehzadHesari/vesta-edge/issues)

![License](https://img.shields.io/badge/license-MIT-000?style=flat-square)
![Platform](https://img.shields.io/badge/Windows-10%20%2F%2011-0078D4?style=flat-square)

</div>

---

## What it does

- **Dock on any edge.** Drag the dock by its body and let go near an edge: it sticks to the nearest one (left, right, top or bottom) and turns into a column or a row to fit. Approach the top or bottom edge and it turns into a row by itself.
- **Clipboard history with styled paste.** Copied text keeps its bold, italic, colours, links and lists. Every item has two paste buttons: **Aa** pastes with its style, **T** pastes plain text. Pinned items survive restarts. Images are kept too.
- **Emoji**, searchable, with categories and recents. Clicking one types it into the app you were using.
- **Colour picker**: pick any pixel on the screen, copy HEX, RGB or HSL, keep a palette and recent colours.
- **Notes** in tabs, saved as you type. **Snippets** you can paste with one click.
- **Tools:** calculator, unit converter, password generator, timer, stopwatch, world clock, text tools (case, slug, Base64, word count…), media keys, keep awake, lock, screen off, Persian (Jalali) calendar.
- **Your apps and folders:** pin them to the dock, or drag a file onto the dock.
- **Themes:** 15 colour themes plus a custom accent, and an appearance setting: follow the theme, follow Windows, or force dark or light.
- **Built-in updates:** Vesta Edge checks GitHub once a day and can download and run the new installer for you.
- Runs quietly in the tray, can start with Windows, and only one copy runs at a time.

## Install

1. Download **`VestaEdge-Setup.exe`** from the [latest release](https://github.com/BehzadHesari/vesta-edge/releases/latest).
2. Run it. It installs for your user only, so no administrator rights are needed.
3. Choose the options you want (start with Windows, right-click menu entry), then finish.

Other downloads on the release page:

| File | What it is |
|---|---|
| `VestaEdge-Setup.exe` | Installer (recommended) |
| `VestaEdge-portable.exe` | Single file, no installation |
| `VestaEdge-windows-x64.zip` | Folder with the program and this readme |

Requirements: Windows 10 or 11 (64-bit). The Microsoft Edge WebView2 runtime is already part of Windows 10 and 11.

## Using it

- **Open:** hover the thin rail at an edge, or click it.
- **Move:** press the dock where there is no icon and drag. Let go anywhere: it snaps to the nearest edge.
- **Paste with style:** open the clipboard, hover an item, then click **Aa** (styled) or **T** (plain text). Clicking the item itself pastes with its style.
- **Add an app:** Settings → Widgets → *Add app*, or drag a file onto the dock.
- **Right-click a file or folder** → *Add to Vesta Edge* (if you enabled the right-click option).
- **Settings:** the cog icon at the end of the dock. Theme, appearance, paste behaviour, monitor, start with Windows, updates.
- **Quit:** Settings → *Quit*, or the tray icon.

## Where your data lives

Everything is stored on your computer, in `%APPDATA%\VestaEdge`: settings, notes, pinned clipboard images, the dock position (`dock.json`) and a small log. Nothing is sent anywhere, except the update check that asks GitHub for the latest release number.

## Uninstall

Windows Settings → Apps → *Vesta Edge* → Uninstall. The uninstaller removes the program and the right-click entry. Your settings folder is kept; delete `%APPDATA%\VestaEdge` if you also want to remove them.

## Build from source

```bash
git clone https://github.com/BehzadHesari/vesta-edge
cd vesta-edge
cargo build --release
```

The Windows installer is built with [Inno Setup 6](https://jrsoftware.org/isinfo.php):

```bash
iscc /DAppVersion=1.0.0 installer/vesta-edge.iss
```

The app is written in Rust (tao and wry, which use WebView2 on Windows). The whole interface is one file: [`src/ui.html`](src/ui.html).

## Known limits

- **Windows only for now.** The dock, edge snapping and styled paste are implemented for Windows. macOS and Linux builds are not part of this release.
- Styled paste keeps formatting that the target application understands (HTML and RTF). Some applications only accept plain text.
- Paste into an application needs that application to be the one you were using before opening the dock.

## Contributing

Issues and pull requests are welcome at [github.com/BehzadHesari/vesta-edge](https://github.com/BehzadHesari/vesta-edge). Please keep changes small and tested on Windows 10 or 11.

## Credits and license

Vesta Edge is released under the [MIT License](LICENSE), free of charge. It is developed by **Vesta IT Group** ([vestait.ir](https://vestait.ir)).

It is an independent, rewritten version of the open-source project "Right Panel" (MIT). The original notice and license text are kept in [NOTICE](NOTICE), as the MIT License requires.
