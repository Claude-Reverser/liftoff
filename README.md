# liftoff

App launcher because Apple is a fucking trillion dollar company that can't do bullshit.

Apple took Launchpad, the one launcher that worked, and swapped it for a Spotlight
tab that forgets half your apps exist and sometimes doesn't load at all. Five
daemons, a private database, and a whole XPC protocol, and it still can't list
`/Applications`. So fuck it, here's the replacement. And since Spotlight is the same
pile of daemons, that got replaced too. And then Finder, because why stop.

## install

```sh
curl -fsSL https://raw.githubusercontent.com/Claude-Reverser/liftoff/main/install.sh | bash
```

Downloads the newest release, checks it against its checksum, puts `Liftoff.app` in
`/Applications`, starts it at login and takes ⌘Space off Spotlight. No sudo. Run it
again to update. Options go after `bash -s --`:

| | |
|---|---|
| `--keep-spotlight` | leave ⌘Space to Spotlight |
| `--no-login` | don't start at login, just open it |
| `--version v0.3.0` | a specific release instead of the newest |
| `--uninstall` | take it back out and give ⌘Space back. `--purge` also deletes your settings |

Apple Silicon, macOS 14+.

## what it is

- full-screen icon grid with pages, like Launchpad was
- type to search, Enter to launch, Esc to close
- folders: Apple's apps get sorted into them on first run, drag an app onto another to make your own, click the title to rename
- drag to rearrange, drag out of folders, hold at the screen edge to switch pages
- Frequently Used row, counting every app launch on the system
- ⌥Space (or the Launchpad key) to open, or click it in the Dock

## search (the spotlight replacement)

- ⌘Space opens a search bar with a glowing rim that expands into three panes: **Files**, **Apps** and **Web**
- whichever of Files or Apps matched harder slides to the left. Web stays on the right, it's the least important
- type math and the answer shows up right in the bar, Enter copies it. `2^10`, `sqrt(2)*3`, `1,5*4`
- live Google suggestions in the Web pane, domains open directly
- Tab switches panes, ↑↓ moves, Enter opens, ⌘Enter reveals in Finder, ⌥Enter copies the path
- two words search the whole path: `liftoff view` finds `~/Developer/liftoff/src/view.rs`

The file index is ours, not mds. It crawls outward: Downloads, Documents and Desktop
first (searchable within milliseconds of launch), then the rest of your home, then
iCloud Drive, then external drives. ~600k paths in about a second on all cores, ~8 ms
per search, and FSEvents tells it which folders changed so it rescans just those.
No database, no "Spotlight is indexing", no mds eating your CPU for three days.

Spotlight keeps ⌘Space for itself. The installer turns that off for you, or pick
another key in `~/Library/Application Support/Liftoff/search-hotkeys`.

## files (the finder replacement)

- ⌥⇧Space opens it, or ⌘Enter on any search result
- a big icon grid that's always tidy and reflows with the window. no icons piled on top of each other, no "Clean Up" menu item
- split up by what things are: Folders, Pictures, Code, Archives… The heading sticks to the top while you scroll, click it to jump to another one. Turn it off with ⌃⌘0
- every folder says how many things are in it, every file says how big it is
- text and code files show their first lines instead of the same blank page forty times over. pictures and videos get real previews
- rest the pointer on a folder and it shows you what's inside. click something in there to go straight to it
- every place in the sidebar has its own colour and the folders inside it are painted to match. Finder on a Graphite Mac is a grey soup
- sort, filter as you type, new folder, rename, tags, cut/copy/paste, drag and drop, Space for Quick Look, ⌘1 icons, ⌘2 list, ⌘+/⌘− sizes
- when you sort, filter, add or delete, things glide to where they now belong instead of the whole window blinking
- folders are read with `getattrlistbulk`: a whole directory in a couple of syscalls, where Finder stats every file and then asks three daemons about it

## why it's fast

- the "index" is `readdir` + `stat` over the app folders, under a millisecond, run every time it opens, so it's never stale and never breaks
- icons are rendered on every core at startup, shadows baked in, so paging is just the GPU moving layers
- raw AppKit + Core Animation from Rust via `objc2`. no Electron, no SwiftUI, no web view
- ~500 KB binary, no dependencies outside macOS itself

## download by hand

Prefer clicking: grab `Liftoff-x.y.z.dmg` from [Releases](../../releases/latest) and drag it into Applications.
It's not notarized (not paying Apple $99 a year to say fuck you to them), so the first time macOS will whine.
Right-click → Open, or:

```sh
xattr -dr com.apple.quarantine /Applications/Liftoff.app
```

Then free up ⌘Space: System Settings → Keyboard → Keyboard Shortcuts → Spotlight → untick "Show Spotlight search".
The installer above does all of that for you.

## build from source

Apple Silicon, macOS 14+, Rust toolchain.

```sh
./install.sh --local
```

Builds this checkout and installs it exactly like a release, at login and all.
Hotkeys live in `~/Library/Application Support/Liftoff/`: `hotkeys` (launcher),
`search-hotkeys` and `files-hotkeys`. `scripts/make-dmg.sh` builds the release files.

A local build gets its Dock icon copied off your own Mac, because shipping Apple's
artwork in a repo that tells them to fuck off seemed like a bad idea.

## license

MIT. Do whatever. Just don't sell it back to Apple.
