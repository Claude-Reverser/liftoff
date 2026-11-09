# liftoff

App launcher because Apple is a fucking trillion dollar company that can't do bullshit.

Apple took Launchpad, the one launcher that worked, and swapped it for a Spotlight
tab that forgets half your apps exist and sometimes doesn't load at all. Five
daemons, a private database, and a whole XPC protocol, and it still can't list
`/Applications`. So fuck it, here's the replacement. And since Spotlight is the same
pile of daemons, that got replaced too.

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

Spotlight keeps ⌘Space for itself. `install.sh` turns that off for you. If you used
the DMG: System Settings → Keyboard → Keyboard Shortcuts → Spotlight → untick
"Show Spotlight search". Or pick another key in
`~/Library/Application Support/Liftoff/search-hotkeys`.

## why it's fast

- the "index" is `readdir` + `stat` over the app folders, under a millisecond, run every time it opens, so it's never stale and never breaks
- icons are rendered on every core at startup, shadows baked in, so paging is just the GPU moving layers
- raw AppKit + Core Animation from Rust via `objc2`. no Electron, no SwiftUI, no web view
- ~500 KB binary, no dependencies outside macOS itself

## download

Grab `Liftoff-x.y.z.dmg` from [Releases](../../releases/latest), drag it into Applications, open it, hit ⌥Space for the launcher or ⌘Space for search.

It's not notarized (not paying Apple $99 a year to say fuck you to them), so the first time macOS will whine.
Right-click → Open, or:

```sh
xattr -dr com.apple.quarantine /Applications/Liftoff.app
```

Want it at login: System Settings → General → Login Items → add Liftoff. Or use `./install.sh` below, which does that for you.

## build from source

Apple Silicon, macOS 14+, Rust toolchain.

```sh
./install.sh
```

Builds it, puts `Liftoff.app` in `/Applications` and starts it at login.
Hotkeys live in `~/Library/Application Support/Liftoff/hotkeys` (launcher) and
`search-hotkeys` (search). Set `LIFTOFF_KEEP_SPOTLIGHT=1` if you don't want it to take ⌘Space.

The Dock icon gets copied off your own Mac at install time, because shipping
Apple's artwork in a repo that tells them to fuck off seemed like a bad idea.

## license

MIT. Do whatever. Just don't sell it back to Apple.
