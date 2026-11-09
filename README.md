# liftoff

App launcher because Apple is a fucking trillion dollar company that can't do bullshit.

Apple took Launchpad, the one launcher that worked, and swapped it for a Spotlight
tab that forgets half your apps exist and sometimes doesn't load at all. Five
daemons, a private database, and a whole XPC protocol, and it still can't list
`/Applications`. So fuck it, here's the replacement.

## what it is

- full-screen icon grid with pages, like Launchpad was
- type to search, Enter to launch, Esc to close
- folders: Apple's apps get sorted into them on first run, drag an app onto another to make your own, click the title to rename
- drag to rearrange, drag out of folders, hold at the screen edge to switch pages
- Frequently Used row, counting every app launch on the system
- ⌥Space (or the Launchpad key) to open, or click it in the Dock

## why it's fast

- the "index" is `readdir` + `stat` over the app folders, under a millisecond, run every time it opens, so it's never stale and never breaks
- icons are rendered on every core at startup, shadows baked in, so paging is just the GPU moving layers
- raw AppKit + Core Animation from Rust via `objc2`. no Electron, no SwiftUI, no web view
- ~500 KB binary, no dependencies outside macOS itself

## download

Grab `Liftoff-x.y.z.dmg` from [Releases](../../releases/latest), drag it into Applications, open it, hit ⌥Space.

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
Hotkeys live in `~/Library/Application Support/Liftoff/hotkeys`.

The Dock icon gets copied off your own Mac at install time, because shipping
Apple's artwork in a repo that tells them to fuck off seemed like a bad idea.

## license

MIT. Do whatever. Just don't sell it back to Apple.
