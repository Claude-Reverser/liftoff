#!/bin/bash
# liftoff installer.
#
#   curl -fsSL https://raw.githubusercontent.com/Claude-Reverser/liftoff/main/install.sh | bash
#
# grabs the newest release off github, checks it against its checksum, puts
# Liftoff.app in /Applications, starts it at login and takes ⌘Space back from
# spotlight. run it again to update. nothing here needs sudo.
#
# options. pass them after `bash -s --` when piping, or set the env var instead:
#   --version v0.3.0   LIFTOFF_VERSION=v0.3.0       that release instead of the newest
#   --keep-spotlight   LIFTOFF_KEEP_SPOTLIGHT=1     leave ⌘Space to spotlight
#   --no-login         LIFTOFF_NO_LOGIN=1           don't start it at login, just open it
#   --force            LIFTOFF_FORCE=1              reinstall even if it's already that version
#   --local                                         build this checkout instead (needs rust)
#   --uninstall                                     take it all back out, settings stay
#   --purge                                         uninstall and delete the settings too
#
# bash 3.2 on purpose: it's the one every mac has.

set -euo pipefail

REPO="Claude-Reverser/liftoff"
ASSET="Liftoff-macos-arm64.zip"
LABEL="app.liftoff.launcher"
AGENT="$HOME/Library/LaunchAgents/$LABEL.plist"
SUPPORT="$HOME/Library/Application Support/Liftoff"
LOG="$HOME/Library/Logs/Liftoff.log"
DOMAIN="gui/$(id -u)"

VERSION="${LIFTOFF_VERSION:-}"
KEEP_SPOTLIGHT="${LIFTOFF_KEEP_SPOTLIGHT:-}"
NO_LOGIN="${LIFTOFF_NO_LOGIN:-}"
FORCE="${LIFTOFF_FORCE:-}"
MODE="release"

# ---------- talking ----------

if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
  B=$'\033[1m' D=$'\033[2m' G=$'\033[32m' Y=$'\033[33m' R=$'\033[31m' N=$'\033[0m'
else
  B="" D="" G="" Y="" R="" N=""
fi
step() { printf '%s›%s %s\n' "$D" "$N" "$*"; }
ok()   { printf '%s✓%s %s\n' "$G" "$N" "$*"; }
warn() { printf '%s!%s %s\n' "$Y" "$N" "$*" >&2; }
die()  { printf '%s✗%s %s\n' "$R" "$N" "$*" >&2; exit 1; }

usage() {
  cat <<'TXT'
install or update Liftoff from the newest github release

  curl -fsSL https://raw.githubusercontent.com/Claude-Reverser/liftoff/main/install.sh | bash
  curl -fsSL .../install.sh | bash -s -- [options]

  --version v0.3.0   that release instead of the newest      (LIFTOFF_VERSION)
  --keep-spotlight   leave ⌘Space to spotlight                (LIFTOFF_KEEP_SPOTLIGHT=1)
  --no-login         don't start it at login, just open it    (LIFTOFF_NO_LOGIN=1)
  --force            reinstall even if it's already that one  (LIFTOFF_FORCE=1)
  --local            build this checkout instead (needs rust)
  --uninstall        take it back out, settings stay
  --purge            take it out and delete the settings
TXT
  exit 0
}

parse_args() {
  while [ $# -gt 0 ]; do
    case "$1" in
      --version) [ $# -ge 2 ] || die "--version needs a tag, like v0.3.0"; VERSION="$2"; shift ;;
      --version=*) VERSION="${1#*=}" ;;
      --keep-spotlight) KEEP_SPOTLIGHT=1 ;;
      --no-login) NO_LOGIN=1 ;;
      --force) FORCE=1 ;;
      --local) MODE="local" ;;
      --uninstall) MODE="uninstall" ;;
      --purge) MODE="purge" ;;
      -h|--help) usage ;;
      *) die "don't know $1 (try --help)" ;;
    esac
    shift
  done
}

TMP=""
cleanup() { if [ -n "$TMP" ]; then rm -rf "$TMP"; fi; }
trap cleanup EXIT
fresh_tmp() { TMP="$(mktemp -d "${TMPDIR:-/tmp}/liftoff.XXXXXX")"; }

# ~/Library/... reads better than the whole home path
tilde() { case "$1" in "$HOME"*) printf '~%s' "${1#"$HOME"}" ;; *) printf '%s' "$1" ;; esac; }

plist_get() { /usr/libexec/PlistBuddy -c "Print :$2" "$1" 2>/dev/null || true; }

# ---------- what's there now ----------

installed_app() {
  for d in /Applications "$HOME/Applications"; do
    if [ -d "$d/Liftoff.app" ]; then echo "$d/Liftoff.app"; return; fi
  done
}

stop_running() {
  launchctl bootout "$DOMAIN/$LABEL" >/dev/null 2>&1 || true
  pkill -x liftoff >/dev/null 2>&1 || true
  # give it a moment to let go of its hotkeys before the new one grabs them
  for _ in 1 2 3 4 5 6 7 8 9 10; do
    pgrep -x liftoff >/dev/null 2>&1 || return 0
    sleep 0.3
  done
  pkill -9 -x liftoff >/dev/null 2>&1 || true
}

# spotlight's "show search" is symbolic hotkey 64. off means ⌘Space is free for us
spotlight_key() {
  local on="$1"
  defaults write com.apple.symbolichotkeys AppleSymbolicHotKeys -dict-add 64 \
    "<dict><key>enabled</key><$on/><key>value</key><dict><key>parameters</key><array><integer>32</integer><integer>49</integer><integer>1048576</integer></array><key>type</key><string>standard</string></dict></dict>"
  /System/Library/PrivateFrameworks/SystemAdministration.framework/Resources/activateSettings -u >/dev/null 2>&1 || true
}

# ---------- checks ----------

preflight() {
  [ "$(uname -s)" = "Darwin" ] || die "Liftoff is a macOS app"
  local major
  major="$(sw_vers -productVersion | cut -d. -f1)"
  [ "$major" -ge 14 ] 2>/dev/null || die "needs macOS 14 or newer, this is $(sw_vers -productVersion)"
  local arch
  arch="$(uname -m)"
  # a terminal running under rosetta says x86_64 on an apple silicon mac
  if [ "$arch" != "arm64" ] && [ "$(sysctl -n sysctl.proc_translated 2>/dev/null || echo 0)" != "1" ]; then
    die "the release is built for Apple Silicon. on an Intel Mac, clone the repo and run ./install.sh --local"
  fi
  for c in curl ditto shasum codesign launchctl; do
    command -v "$c" >/dev/null 2>&1 || die "missing $c"
  done
}

# ---------- getting the app ----------

# the newest tag, read off where github's /releases/latest redirect lands. no api,
# so no rate limit and no json to pick apart
latest_tag() {
  local url
  url="$(curl -fsSLI --retry 3 --proto '=https' -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest")" || return 1
  case "$url" in
    */tag/*) echo "${url##*/}" ;;
    *) return 1 ;;
  esac
}

download() {
  local tag="$1" base="https://github.com/$REPO/releases/download/$1"
  local bar="-s"
  [ -t 2 ] && bar="-#"
  step "downloading Liftoff $tag"
  curl -fL $bar --retry 3 --proto '=https' --tlsv1.2 -o "$TMP/$ASSET" "$base/$ASSET" \
    || die "couldn't download $base/$ASSET"
  curl -fsSL --retry 3 --proto '=https' --tlsv1.2 -o "$TMP/$ASSET.sha256" "$base/$ASSET.sha256" \
    || die "couldn't download the checksum for $tag"
  (cd "$TMP" && shasum -a 256 -c "$ASSET.sha256" >/dev/null 2>&1) \
    || die "checksum doesn't match. the download is broken or not what was released, so nothing was installed"
  ok "checksum matches"
  ditto -x -k "$TMP/$ASSET" "$TMP/out" || die "couldn't unpack $ASSET"
  [ -x "$TMP/out/Liftoff.app/Contents/MacOS/liftoff" ] || die "the download has no Liftoff.app in it"
  NEW_APP="$TMP/out/Liftoff.app"
}

# --local: the same app the release script makes, from this checkout
build_local() {
  local src="${BASH_SOURCE[0]:-}" here=""
  [ -n "$src" ] && [ -f "$src" ] && here="$(cd "$(dirname "$src")" && pwd)"
  [ -n "$here" ] && [ -f "$here/Cargo.toml" ] || die "--local has to be run from a liftoff checkout: ./install.sh --local"
  command -v cargo >/dev/null 2>&1 || die "--local needs rust: https://rustup.rs"
  cd "$here"
  step "building from $(basename "$here")"
  cargo build --release --quiet || die "build failed"
  local ver
  ver="$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)"
  if [ ! -f assets/AppIcon.icns ] && command -v swift >/dev/null 2>&1; then
    # apple's own launchpad icon, pulled off this mac. their art doesn't go in the repo
    swift scripts/exporticon.swift /System/Applications/Apps.app "$TMP/AppIcon.iconset" >/dev/null 2>&1 \
      && mkdir -p assets && iconutil -c icns "$TMP/AppIcon.iconset" -o assets/AppIcon.icns || true
  fi
  NEW_APP="$TMP/out/Liftoff.app"
  mkdir -p "$NEW_APP/Contents/MacOS" "$NEW_APP/Contents/Resources"
  cp target/release/liftoff "$NEW_APP/Contents/MacOS/liftoff"
  if [ -f assets/AppIcon.icns ]; then
    cp assets/AppIcon.icns "$NEW_APP/Contents/Resources/AppIcon.icns"
  elif command -v swift >/dev/null 2>&1; then
    swift scripts/liftoff-icon.swift "$TMP/own.iconset" >/dev/null 2>&1 \
      && iconutil -c icns "$TMP/own.iconset" -o "$NEW_APP/Contents/Resources/AppIcon.icns" || true
  fi
  scripts/info-plist.sh "$ver" > "$NEW_APP/Contents/Info.plist"
  codesign --force --sign - "$NEW_APP" >/dev/null 2>&1 || true
  ok "built $ver"
}

# ---------- putting it in place ----------

place_app() {
  local dest_dir="/Applications"
  [ -w "$dest_dir" ] || { dest_dir="$HOME/Applications"; mkdir -p "$dest_dir"; }
  APP="$dest_dir/Liftoff.app"
  stop_running
  # copy next to the old one first, then swap, so a failed copy never leaves you with
  # half an app
  rm -rf "$dest_dir/.Liftoff.app.new"
  ditto "$NEW_APP" "$dest_dir/.Liftoff.app.new" || die "couldn't write to $dest_dir"
  xattr -dr com.apple.quarantine "$dest_dir/.Liftoff.app.new" >/dev/null 2>&1 || true
  if [ -d "$APP" ]; then mv "$APP" "$TMP/old.app"; fi
  mv "$dest_dir/.Liftoff.app.new" "$APP"
  # an older copy in the other applications folder would confuse launch services
  for d in /Applications "$HOME/Applications"; do
    if [ "$d/Liftoff.app" != "$APP" ] && [ -d "$d/Liftoff.app" ]; then
      rm -rf "$d/Liftoff.app" 2>/dev/null && step "removed the old copy in $d" || true
    fi
  done
  # not notarized (apple wants $99 a year to say fuck you to them), so it's signed
  # ad hoc. make sure that signature survived the trip
  codesign --verify "$APP" >/dev/null 2>&1 || codesign --force --sign - "$APP" >/dev/null 2>&1 || true
  /System/Library/Frameworks/CoreServices.framework/Frameworks/LaunchServices.framework/Support/lsregister -f "$APP" >/dev/null 2>&1 || true
  ok "installed in $APP"
}

# builds made by the old install.sh were "app.liftoff.launcher". the app is
# "eu.instantnode.liftoff" everywhere now, so bring the saved settings along once
migrate_prefs() {
  local id
  id="$(plist_get "$APP/Contents/Info.plist" CFBundleIdentifier)"
  [ -n "$id" ] && [ "$id" != "$LABEL" ] || return 0
  if defaults read "$LABEL" >/dev/null 2>&1 && ! defaults read "$id" >/dev/null 2>&1; then
    defaults export "$LABEL" - 2>/dev/null | defaults import "$id" - 2>/dev/null && step "brought your window and view settings along" || true
  fi
}

start_app() {
  mkdir -p "$(dirname "$LOG")"
  if [ -n "$NO_LOGIN" ]; then
    launchctl bootout "$DOMAIN/$LABEL" >/dev/null 2>&1 || true
    rm -f "$AGENT"
    open "$APP"
    ok "started (not at login)"
    return
  fi
  mkdir -p "$(dirname "$AGENT")"
  cat > "$AGENT" <<PL
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>Label</key><string>$LABEL</string>
  <key>ProgramArguments</key><array><string>$APP/Contents/MacOS/liftoff</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>ProcessType</key><string>Interactive</string>
  <key>StandardErrorPath</key><string>$LOG</string>
</dict></plist>
PL
  launchctl bootstrap "$DOMAIN" "$AGENT" >/dev/null 2>&1 \
    || { sleep 1; launchctl bootstrap "$DOMAIN" "$AGENT" >/dev/null 2>&1; } \
    || die "launchd wouldn't take it. try: launchctl bootstrap $DOMAIN $AGENT"
  for _ in 1 2 3 4 5 6 7 8 9 10; do
    if pgrep -x liftoff >/dev/null 2>&1; then ok "running, and starts at login"; return; fi
    sleep 0.3
  done
  warn "it was installed but didn't start. what it said: $(tilde "$LOG")"
}

take_cmd_space() {
  if [ -n "$KEEP_SPOTLIGHT" ]; then
    step "left ⌘Space to spotlight. pick a key for search in $(tilde "$SUPPORT")/search-hotkeys"
    return
  fi
  spotlight_key false
  ok "⌘Space is Liftoff's now (spotlight's shortcut is off)"
}

summary() {
  printf '\n%sLiftoff %s%s\n' "$B" "$1" "$N"
  printf '  ⌥Space     launcher\n'
  printf '  ⌘Space     search\n'
  printf '  ⌥⇧Space    files\n'
  printf '%s  keys: %s/{hotkeys,search-hotkeys,files-hotkeys}%s\n' "$D" "$(tilde "$SUPPORT")" "$N"
  printf '%s  log:  %s%s\n' "$D" "$(tilde "$LOG")" "$N"
  printf "%s  the first time it opens Desktop, Documents or Downloads, macOS asks if that's ok%s\n" "$D" "$N"
  printf '%s  update: run this again. remove: add --uninstall%s\n\n' "$D" "$N"
}

# ---------- the jobs ----------

uninstall() {
  local purge="$1"
  stop_running
  rm -f "$AGENT"
  local gone=0
  for d in /Applications "$HOME/Applications"; do
    if [ -d "$d/Liftoff.app" ]; then rm -rf "$d/Liftoff.app"; gone=1; fi
  done
  [ "$gone" = 1 ] && ok "removed Liftoff.app" || step "Liftoff.app wasn't installed"
  spotlight_key true
  ok "⌘Space is spotlight's again"
  if [ "$purge" = 1 ]; then
    rm -rf "$SUPPORT" "$LOG"
    ok "settings deleted"
  elif [ -d "$SUPPORT" ]; then
    step "kept your settings in $(tilde "$SUPPORT") (--purge deletes them)"
  fi
}

# everything runs from here, after bash has read the whole file. with curl | bash a
# cut-off download then does nothing at all instead of half an install
main() {
  parse_args "$@"
  case "$MODE" in
    uninstall) uninstall 0; exit 0 ;;
    purge) uninstall 1; exit 0 ;;
  esac

  preflight
  fresh_tmp

  if [ "$MODE" = "local" ]; then
    build_local
    TAG="$(plist_get "$NEW_APP/Contents/Info.plist" CFBundleShortVersionString)"
  else
    if [ -n "$VERSION" ]; then
      TAG="v${VERSION#v}"
    else
      TAG="$(latest_tag)" || die "couldn't reach github to find the newest release"
    fi
    HAVE_APP="$(installed_app)"
    HAVE=""
    [ -n "$HAVE_APP" ] && HAVE="$(plist_get "$HAVE_APP/Contents/Info.plist" CFBundleShortVersionString)"
    if [ -n "$HAVE" ] && [ "v${HAVE#v}" = "$TAG" ] && [ -z "$FORCE" ]; then
      ok "Liftoff $TAG is already installed (--force reinstalls it)"
      APP="$HAVE_APP"
      if ! pgrep -x liftoff >/dev/null 2>&1; then start_app; fi
      take_cmd_space
      summary "$TAG"
      exit 0
    fi
    [ -n "$HAVE" ] && step "updating from v${HAVE#v}"
    download "$TAG"
  fi

  place_app
  migrate_prefs
  start_app
  take_cmd_space
  summary "$TAG"
}

main "$@"
