<div align="center">

<img src="docs/icon.png" width="112" alt="Liftoff">

# Liftoff

**Launchpad, Spotlight and Finder, rebuilt in Rust.**<br>
Because Apple is a fucking trillion dollar company that can't keep a launcher working.

[![Release](https://img.shields.io/github/v/release/Claude-Reverser/liftoff?label=release&color=5b6cff)](../../releases/latest)
![macOS 14+](https://img.shields.io/badge/macOS-14%2B-1f1f24)
![Apple Silicon](https://img.shields.io/badge/Apple%20Silicon-native-1f1f24)
![MIT](https://img.shields.io/badge/license-MIT-1f1f24)

</div>

```sh
curl -fsSL https://raw.githubusercontent.com/Claude-Reverser/liftoff/main/install.sh | bash
```

Downloads the newest release, checks its checksum, puts it in `/Applications`, starts it at login and takes ⌘Space back from Spotlight. No sudo. Run it again to update.

<br>

## Launcher &nbsp;<kbd>⌥</kbd> <kbd>Space</kbd>

<img src="docs/launcher.png" alt="The launcher: a full-screen grid of apps with a frequently used row on top">

Launchpad, back the way it was before Apple swapped it for a Spotlight tab that forgets half your apps exist. Pages, folders, drag to rearrange, type to filter, and a frequently used row that counts every launch on the system. Apple's own apps get sorted into folders on first run.

## Search &nbsp;<kbd>⌘</kbd> <kbd>Space</kbd>

<img src="docs/search.png" alt="The search bar with its glowing rim, showing files, apps and web results side by side">

- **Files, Apps and Web** side by side. Whichever matched harder slides to the left
- its own file index, no mds: ~600k paths in about a second, ~8 ms a search, FSEvents keeps it current
- math right in the bar: `2^10`, `sqrt(2)*3`. Enter copies the answer
- <kbd>Tab</kbd> switches columns, <kbd>⌘</kbd><kbd>↩</kbd> opens the file in Files, <kbd>⌥</kbd><kbd>↩</kbd> copies the path

## Files &nbsp;<kbd>⌥</kbd> <kbd>⇧</kbd> <kbd>Space</kbd>

<img src="docs/files.png" alt="The Files window: Downloads grouped into folders, pictures and documents, with a folder's contents shown in a card">

- grouped by what things are. The heading sticks while you scroll, click it to jump
- text and code files show their first lines, pictures and videos get real previews
- rest the pointer on a folder to see what's inside without opening it
- every place has its own colour and the folders in it match. Finder on a Graphite Mac is grey soup
- sort, filter, add or delete and things glide to where they belong instead of the window blinking

## Why it's fast

Raw AppKit and Core Animation from Rust through `objc2`. No Electron, no SwiftUI, no web view, one 1.3 MB binary. The app list is a `readdir` of the app folders every time it opens, under a millisecond, so it's never stale. Folders are read with `getattrlistbulk`, a whole directory in a couple of syscalls. Icons are rendered on every core at launch.

<details>
<summary><b>Installer options</b></summary>
<br>

Add them after `bash -s --`, e.g. `curl -fsSL … | bash -s -- --keep-spotlight`

| | |
|---|---|
| `--keep-spotlight` | leave ⌘Space to Spotlight |
| `--no-login` | don't start at login, just open it |
| `--version v0.3.0` | a specific release instead of the newest |
| `--force` | reinstall even if it's already that version |
| `--uninstall` | take it back out and give ⌘Space back. `--purge` also deletes your settings |

Hotkeys live in `~/Library/Application Support/Liftoff/`: `hotkeys`, `search-hotkeys`, `files-hotkeys`.

</details>

<details>
<summary><b>Build from source</b></summary>
<br>

Apple Silicon, macOS 14+, a Rust toolchain.

```sh
./install.sh --local
```

Builds this checkout and installs it exactly like a release. `scripts/make-dmg.sh` makes the release files. A local build gets its Dock icon copied off your own Mac, because shipping Apple's artwork in a repo that tells them to fuck off seemed like a bad idea.

</details>

<details>
<summary><b>Installing by hand</b></summary>
<br>

Grab the `.dmg` from [Releases](../../releases/latest) and drag Liftoff into Applications. It isn't notarized (not paying Apple $99 a year to tell them to fuck off), so open it once with right-click → Open, or run `xattr -dr com.apple.quarantine /Applications/Liftoff.app`. Then free ⌘Space under System Settings → Keyboard → Keyboard Shortcuts → Spotlight.

</details>

<br>

<sub>MIT. Do whatever. Just don't sell it back to Apple.</sub>
