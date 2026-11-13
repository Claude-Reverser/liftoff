use std::cell::{Cell, RefCell};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::ptr::NonNull;
use std::rc::Rc;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use objc2::rc::{Allocated, Retained};
use objc2::runtime::{AnyClass, AnyObject, Bool, ProtocolObject, Sel};
use objc2::{define_class, msg_send, sel, AnyThread, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSApplication, NSApplicationActivationPolicy, NSAutoresizingMaskOptions, NSBackingStoreType, NSBezierPath,
    NSBitmapImageFileType, NSBitmapImageRep, NSBorderType, NSBox, NSBoxType, NSCollectionView, NSColor, NSControl, NSControlSize,
    NSControlTextEditingDelegate, NSDragOperation, NSDraggingInfo, NSEvent, NSEventModifierFlags,
    NSEventType, NSFont, NSFontWeightRegular, NSFontWeightSemibold, NSGradient, NSImage, NSImageScaling,
    NSImageSymbolConfiguration, NSImageView, NSLineBreakMode,
    NSMenu, NSMenuDelegate, NSMenuItem, NSMenuItemValidation, NSMenuToolbarItem, NSPasteboard, NSPasteboardTypeFileURL, NSPasteboardTypeString,
    NSPasteboardWriting, NSPathControl, NSPathStyle, NSScrollView, NSSearchField, NSSearchFieldDelegate, NSSearchToolbarItem,
    NSSegmentStyle, NSSegmentSwitchTracking, NSSegmentedControl, NSSplitViewController,
    NSSplitViewItem, NSTableCellView, NSTableColumn, NSTableRowView, NSTableView, NSTableViewColumnAutoresizingStyle,
    NSTableViewDataSource, NSTableViewDelegate, NSTableViewDropOperation, NSTableViewRowSizeStyle,
    NSTableViewStyle, NSTextAlignment, NSTextField, NSTextFieldDelegate, NSTextView, NSToolbar,
    NSToolbarDelegate, NSToolbarDisplayMode, NSToolbarFlexibleSpaceItemIdentifier, NSToolbarItem,
    NSToolbarItemIdentifier, NSToolbarSidebarTrackingSeparatorItemIdentifier,
    NSUserInterfaceItemIdentification, NSView, NSViewController, NSWindow, NSWindowDelegate,
    NSWindowStyleMask, NSWindowTabbingMode, NSWindowToolbarStyle, NSWorkspace,
};
use objc2_core_foundation::CFRetained;
use objc2_core_graphics::CGImage;
use objc2_quartz_core::{CABasicAnimation, CALayer, CAMediaTiming, CAMediaTimingFunction};
use objc2_foundation::{
    NSArray, NSByteCountFormatter, NSByteCountFormatterCountStyle, NSData, NSDate, NSDateFormatter,
    NSDateFormatterStyle, NSDictionary, NSFileManager, NSInteger, NSMutableIndexSet,
    NSNotification, NSNumber, NSObject, NSObjectProtocol, NSPoint, NSRange, NSRect, NSRunLoop, NSSize,
    NSSortDescriptor, NSString, NSURL,
};

use crate::dir::{self, Ent};
use crate::files;

mod grid;

// finder. the app that takes four seconds to show a folder with 300 files in it,
// beachballs the whole desktop when a network drive sneezes and still has no cut
// and paste in 2026. twenty five years of this.
//
// so this is the same window, built from the same appkit parts, minus everything
// that makes finder slow: the folder is read with one bulk syscall on a worker
// thread and the table only ever builds the rows you can see. cmd+x works.

#[link(name = "Quartz", kind = "framework")]
unsafe extern "C" {}

#[link(name = "QuickLookThumbnailing", kind = "framework")]
unsafe extern "C" {}

// every place owns a colour and whatever lives under it wears that colour: the folder
// icons, the selection, the path bar. apple hangs all of that off one accent setting,
// so the day you pick graphite your whole file manager turns into wet cement.
const HUES: [(f64, f64, f64); 11] = [
    (0.26, 0.58, 1.00),
    (0.62, 0.45, 1.00),
    (0.42, 0.48, 1.00),
    (0.20, 0.78, 0.72),
    (0.28, 0.80, 0.45),
    (1.00, 0.58, 0.20),
    (1.00, 0.36, 0.36),
    (1.00, 0.38, 0.66),
    (0.98, 0.76, 0.18),
    (0.25, 0.80, 0.95),
    (0.66, 0.68, 0.74),
];
const BLUE: usize = 0;
const VIOLET: usize = 1;
const INDIGO: usize = 2;
const TEAL: usize = 3;
const GREEN: usize = 4;
const ORANGE: usize = 5;
const RED: usize = 6;
const PINK: usize = 7;
const YELLOW: usize = 8;
const CYAN: usize = 9;
const SILVER: usize = 10;

// finder's seven tag colours, indexed by the number finder stores for them
const TAG_RGB: [(f64, f64, f64); 8] = [
    (0.0, 0.0, 0.0),
    (0.58, 0.58, 0.61),
    (0.22, 0.80, 0.37),
    (0.72, 0.36, 0.90),
    (0.10, 0.54, 1.00),
    (1.00, 0.80, 0.04),
    (1.00, 0.30, 0.26),
    (1.00, 0.60, 0.06),
];
// the order finder lists them in: red orange yellow green blue purple grey
const TAG_ORDER: [u8; 7] = [6, 7, 5, 2, 4, 3, 1];
const TAG_NAMES: [&str; 8] = ["", "Grey", "Green", "Purple", "Blue", "Yellow", "Red", "Orange"];

// k below 1 pulls the colour toward black, above 1 toward white
fn tone(hue: usize, k: f64, alpha: f64) -> Retained<NSColor> {
    let (r, g, b) = HUES[hue.min(HUES.len() - 1)];
    let f = |c: f64| if k <= 1.0 { c * k } else { c + (1.0 - c) * (k - 1.0) };
    NSColor::colorWithSRGBRed_green_blue_alpha(f(r), f(g), f(b), alpha)
}

// how far a colour has to be darkened before white text is readable on top of it.
// blue barely moves, yellow has to give up a lot
fn deep(hue: usize) -> f64 {
    let (r, g, b) = HUES[hue.min(HUES.len() - 1)];
    let lin = |c: f64| if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
    let mut k = 0.80;
    while k > 0.5 && 0.2126 * lin(r * k) + 0.7152 * lin(g * k) + 0.0722 * lin(b * k) > 0.26 {
        k -= 0.02;
    }
    k
}

fn hue_in(places: &[Place], path: &str) -> usize {
    let mut best = (0, BLUE);
    for p in places {
        if p.header || p.path.len() <= 1 || p.path.len() <= best.0 {
            continue;
        }
        if path == p.path || (path.starts_with(&p.path) && path.as_bytes().get(p.path.len()) == Some(&b'/')) {
            best = (p.path.len(), p.hue);
        }
    }
    best.1
}

// what a folder is made of, in the order the icon view lists it
const CATS: [&str; 9] = ["Folders", "Applications", "Pictures", "Movies", "Music", "Documents", "Code", "Archives", "Other"];

fn category(e: &Ent) -> usize {
    let ext = ext_of(&e.name);
    if e.dir {
        return match ext.as_str() {
            "app" => 1,
            "photoslibrary" | "musiclibrary" | "tvlibrary" | "imovielibrary" | "bundle" | "framework" | "plugin" | "kext" | "xcodeproj" | "xcworkspace"
            | "playground" | "rtfd" | "pkg" | "mpkg" | "band" | "logicx" | "fcpbundle" => 8,
            _ => 0,
        };
    }
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "heic" | "heif" | "gif" | "webp" | "tiff" | "tif" | "bmp" | "avif" | "psd" | "svg" | "ico" | "icns" | "dng" | "cr2" | "cr3"
        | "nef" | "arw" | "raf" | "jxl" => 2,
        "mp4" | "mov" | "m4v" | "mkv" | "avi" | "webm" | "wmv" | "flv" | "mpg" | "mpeg" => 3,
        "mp3" | "m4a" | "wav" | "flac" | "aac" | "ogg" | "aiff" | "aif" | "opus" | "mid" | "midi" => 4,
        "pdf" | "doc" | "docx" | "pages" | "txt" | "rtf" | "md" | "odt" | "xls" | "xlsx" | "numbers" | "csv" | "ppt" | "pptx" | "key" | "epub" => 5,
        "rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "c" | "h" | "cpp" | "hpp" | "cc" | "m" | "mm" | "swift" | "java" | "kt" | "go" | "rb" | "php" | "lua"
        | "luau" | "sh" | "zsh" | "bash" | "json" | "toml" | "yaml" | "yml" | "xml" | "html" | "css" | "scss" | "sql" | "lock" => 6,
        "zip" | "rar" | "7z" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "dmg" | "iso" | "pkg" | "jar" => 7,
        _ => 8,
    }
}

fn ext_of(name: &str) -> String {
    name.rsplit_once('.').filter(|x| !x.0.is_empty()).map(|x| x.1.to_lowercase()).unwrap_or_default()
}

const BAR: f64 = 28.0;
// icon size and row height. the first is finder's list, the other two are for people
// who'd like to actually see the thumbnail
const SIZES: [(f64, f64); 3] = [(16.0, 24.0), (20.0, 28.0), (32.0, 40.0)];
const SIDE_ROW: f64 = 28.0;

fn ns(s: &str) -> Retained<NSString> {
    NSString::from_str(s)
}

fn file_url(p: &str) -> Retained<NSURL> {
    NSURL::fileURLWithPath(&ns(p))
}

fn home() -> String {
    std::env::var("HOME").unwrap_or_else(|_| "/".into())
}

fn tilde(p: &str) -> String {
    let h = home();
    match p.strip_prefix(&h) {
        Some(r) if r.is_empty() || r.starts_with('/') => format!("~{r}"),
        _ => p.to_owned(),
    }
}

fn parent_of(p: &str) -> &str {
    match p.rsplit_once('/') {
        Some(("", _)) | None => "/",
        Some((d, _)) => d,
    }
}

fn later(ms: u64, f: impl FnOnce() + Send + 'static) {
    let when = dispatch2::DispatchTime::try_from(Duration::from_millis(ms)).unwrap();
    let _ = dispatch2::DispatchQueue::main().after(when, f);
}

fn nat_cmp(a: &str, b: &str) -> Ordering {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let (mut i, mut j) = (0, 0);
    while i < a.len() && j < b.len() {
        if a[i].is_ascii_digit() && b[j].is_ascii_digit() {
            let (si, sj) = (i, j);
            while i < a.len() && a[i].is_ascii_digit() {
                i += 1;
            }
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            let na: &[u8] = &a[si..i][a[si..i].iter().take_while(|c| **c == b'0').count()..];
            let nb: &[u8] = &b[sj..j][b[sj..j].iter().take_while(|c| **c == b'0').count()..];
            let o = na.len().cmp(&nb.len()).then_with(|| na.cmp(nb));
            if o != Ordering::Equal {
                return o;
            }
        } else {
            if a[i] != b[j] {
                return a[i].cmp(&b[j]);
            }
            i += 1;
            j += 1;
        }
    }
    (a.len() - i).cmp(&(b.len() - j))
}

fn free_name(dir: &str, name: &str, copy_suffix: bool) -> String {
    let plain = format!("{dir}/{name}");
    if !copy_suffix && !Path::new(&plain).exists() {
        return plain;
    }
    let (stem, ext) = match name.rfind('.').filter(|&d| d > 0) {
        Some(d) => (&name[..d], &name[d..]),
        None => (name, ""),
    };
    let mut n = 1;
    loop {
        let cand = if n == 1 { format!("{dir}/{stem} copy{ext}") } else { format!("{dir}/{stem} copy {n}{ext}") };
        if !Path::new(&cand).exists() {
            return cand;
        }
        n += 1;
    }
}

#[derive(Clone)]
struct Place {
    label: String,
    path: String,
    symbol: &'static str,
    hue: usize,
    header: bool,
}

fn places() -> Vec<Place> {
    let h = home();
    let head = |l: &str| Place { label: l.into(), path: String::new(), symbol: "", hue: BLUE, header: true };
    let mut v = vec![head("Favourites")];
    let add = |v: &mut Vec<Place>, label: &str, path: String, symbol: &'static str, hue: usize| {
        if Path::new(&path).is_dir() {
            v.push(Place { label: label.into(), path, symbol, hue, header: false });
        }
    };
    add(&mut v, "Applications", "/Applications".into(), "square.grid.2x2.fill", VIOLET);
    add(&mut v, "Desktop", format!("{h}/Desktop"), "menubar.dock.rectangle", INDIGO);
    add(&mut v, "Documents", format!("{h}/Documents"), "doc.fill", TEAL);
    add(&mut v, "Downloads", format!("{h}/Downloads"), "arrow.down", GREEN);
    add(&mut v, "Developer", format!("{h}/Developer"), "hammer.fill", ORANGE);
    add(&mut v, "Movies", format!("{h}/Movies"), "film", RED);
    add(&mut v, "Music", format!("{h}/Music"), "music.note", PINK);
    add(&mut v, "Pictures", format!("{h}/Pictures"), "photo", YELLOW);
    add(&mut v, h.rsplit('/').next().unwrap_or("Home"), h.clone(), "house.fill", BLUE);
    v.push(head("Locations"));
    add(&mut v, "iCloud Drive", format!("{h}/Library/Mobile Documents/com~apple~CloudDocs"), "icloud.fill", CYAN);
    add(&mut v, "Macintosh HD", "/".into(), "internaldrive.fill", SILVER);
    if let Ok(rd) = fs::read_dir("/Volumes") {
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if e.file_type().is_ok_and(|t| t.is_dir()) && !n.starts_with('.') {
                v.push(Place { label: n, path: e.path().to_string_lossy().into_owned(), symbol: "externaldrive.fill", hue: ORANGE, header: false });
            }
        }
    }
    v
}

struct Model {
    cwd: String,
    all: Vec<Ent>,
    rows: Vec<Ent>,
    sort: (u8, bool),
    hidden: bool,
    back: Vec<String>,
    fwd: Vec<String>,
    query: String,
    cut: Vec<String>,
    note: Option<String>,
    stamp: i64,
    load_gen: u64,
    renaming: Option<String>,
    places: Vec<Place>,
    free: String,
    loading: bool,
    // the icon view's headings: each one covers a run of `rows`
    sections: Vec<Section>,
}

struct Section {
    title: String,
    start: usize,
    len: usize,
}

struct Ui {
    window: Retained<NSWindow>,
    table: Retained<NSTableView>,
    side: Retained<NSTableView>,
    cols: [Retained<NSTableColumn>; 4],
    path: Retained<NSPathControl>,
    status: Retained<NSTextField>,
    search: Retained<NSSearchField>,
    nav: Retained<NSSegmentedControl>,
    ctl: Retained<Controller>,
    dates: Retained<NSDateFormatter>,
    menu: Retained<NSMenu>,
    edit_menu: Retained<NSMenu>,
    empty: Retained<NSView>,
    empty_icon: Retained<NSImageView>,
    empty_title: Retained<NSTextField>,
    empty_hint: Retained<NSTextField>,
    // the highlight behind the place you're in. ours, so it can slide when you switch
    side_pill: Retained<NSBox>,
    grid: Retained<NSCollectionView>,
    grid_scroll: Retained<NSScrollView>,
    list_scroll: Retained<NSScrollView>,
    // holds whichever of the two views is showing. this is what fades and slides
    stage: Retained<NSView>,
    mode_seg: Retained<NSSegmentedControl>,
    _grid_ctl: Retained<grid::GridCtl>,
}

thread_local! {
    static UI: RefCell<Option<Rc<Ui>>> = const { RefCell::new(None) };
    static M: RefCell<Model> = RefCell::new(Model {
        cwd: home(),
        all: Vec::new(),
        rows: Vec::new(),
        sort: (0, true),
        hidden: false,
        back: Vec::new(),
        fwd: Vec::new(),
        query: String::new(),
        cut: Vec::new(),
        note: None,
        stamp: 0,
        load_gen: 0,
        renaming: None,
        places: Vec::new(),
        free: String::new(),
        loading: false,
        sections: Vec::new(),
    });
    static ICONS: RefCell<HashMap<String, Retained<NSImage>>> = RefCell::new(HashMap::new());
    static KINDS: RefCell<HashMap<(bool, String), String>> = RefCell::new(HashMap::new());
    static TOOLBAR: RefCell<HashMap<String, Retained<NSToolbarItem>>> = RefCell::new(HashMap::new());
    static QL: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static FILES_PTR: Cell<*const NSTableView> = const { Cell::new(std::ptr::null()) };
    static POLLING: Cell<bool> = const { Cell::new(false) };
    static WANT: Cell<[f64; 3]> = const { Cell::new([170.0, 90.0, 170.0]) };
    static FITTING: Cell<bool> = const { Cell::new(false) };
    static PAD: Cell<f64> = const { Cell::new(-1.0) };
    static ACCENT: Cell<usize> = const { Cell::new(BLUE) };
    static PACKAGES: RefCell<HashMap<String, bool>> = RefCell::new(HashMap::new());
    static THUMBS: RefCell<HashMap<String, (i64, f64, Option<Retained<NSImage>>)>> = RefCell::new(HashMap::new());
    // text pages: when the file changed, how big, drawn for dark or light, and the page
    static TEXTS: RefCell<HashMap<String, (i64, f64, bool, Option<Retained<NSImage>>)>> = RefCell::new(HashMap::new());
    static DARK: Cell<bool> = const { Cell::new(true) };
    static ZOOM: Cell<usize> = const { Cell::new(0) };
    static REMEMBER: Cell<bool> = const { Cell::new(false) };
    // 1 = icons, 0 = list
    // the colour the glow is right now, on its way from one place's colour to the next
    // which way the next folder comes in from: 1 deeper, -1 back out, 0 somewhere else
    static NAV: Cell<f64> = const { Cell::new(0.0) };
    static MODE: Cell<u8> = const { Cell::new(1) };
    static GROUP: Cell<bool> = const { Cell::new(true) };
    // where the folder that's being opened sits on screen, so its contents can grow out of it
    static ORIGIN: Cell<Option<NSPoint>> = const { Cell::new(None) };
    // a rename that just happened, old path to new, so the tile moves instead of being
    // swapped for a stranger
    static RENAMED: RefCell<Option<(String, String)>> = const { RefCell::new(None) };
    // the off-screen renders run the animated paths too, even with no window showing
    static TESTING: Cell<bool> = const { Cell::new(false) };
}

fn grid_mode() -> bool {
    MODE.with(|m| m.get()) == 1
}

// the two views share one model. these are the only places that care which is showing
fn picked(u: &Ui) -> Vec<usize> {
    if grid_mode() { grid::selected_rows(u) } else { selected(&u.table) }
}

fn pick_rows(u: &Ui, rows: &[usize], scroll: bool) {
    if grid_mode() {
        return grid::select_rows(u, rows, scroll);
    }
    let set = NSMutableIndexSet::new();
    for &i in rows {
        set.addIndex(i);
    }
    u.table.selectRowIndexes_byExtendingSelection(&set, false);
    if let (true, Some(&f)) = (scroll, rows.first()) {
        u.table.scrollRowToVisible(f as NSInteger);
    }
}

fn reload_view(u: &Ui) {
    if grid_mode() { grid::reload(u) } else { u.table.reloadData() }
}

fn focus(u: &Ui) {
    if grid_mode() {
        u.window.makeFirstResponder(Some(&u.grid));
    } else {
        u.window.makeFirstResponder(Some(&u.table));
    }
}

fn scroll_top(u: &Ui) {
    if grid_mode() {
        grid::scroll_top(u);
    } else if u.table.numberOfRows() > 0 {
        u.table.scrollRowToVisible(0);
    }
}

fn icon_pt() -> f64 {
    SIZES[ZOOM.with(|z| z.get()).min(2)].0
}

fn row_h() -> f64 {
    SIZES[ZOOM.with(|z| z.get()).min(2)].1
}

fn ui() -> Option<Rc<Ui>> {
    UI.with(|u| u.borrow().clone())
}

// only ever touch plain data in here. calling into appkit from inside this closure
// lets appkit call us back while the model is borrowed, and that's a crash.
fn m<R>(f: impl FnOnce(&mut Model) -> R) -> R {
    M.with(|x| f(&mut x.borrow_mut()))
}

pub fn is_open() -> bool {
    ui().is_some_and(|u| u.window.isVisible())
}

fn is_files(tv: &NSTableView) -> bool {
    FILES_PTR.with(|p| std::ptr::eq(p.get(), tv))
}

// ---------- kinds, icons ----------

// the exact text finder puts in its kind column, cached per extension so a folder
// of ten thousand pngs asks the system once
fn kind_of(path: &str, name: &str, is_dir: bool) -> String {
    let ext = name.rsplit_once('.').filter(|x| !x.0.is_empty()).map(|x| x.1.to_lowercase()).unwrap_or_default();
    if ext.is_empty() && is_dir {
        return "Folder".into();
    }
    let key = (is_dir, ext);
    if !key.1.is_empty() {
        if let Some(k) = KINDS.with(|c| c.borrow().get(&key).cloned()) {
            return k;
        }
    }
    let kind = (|| {
        let k = unsafe { objc2_foundation::NSURLLocalizedTypeDescriptionKey };
        let d = file_url(path).resourceValuesForKeys_error(&NSArray::from_slice(&[k])).ok()?;
        let v = d.objectForKey(k)?.downcast::<NSString>().ok()?.to_string();
        (!v.is_empty()).then_some(v)
    })()
    .unwrap_or_else(|| if is_dir { "Folder".into() } else if key.1.is_empty() { "Document".into() } else { format!("{} file", key.1.to_uppercase()) });
    if !key.1.is_empty() {
        KINDS.with(|c| c.borrow_mut().insert(key, kind.clone()));
    }
    kind
}

fn cached(key: &str, make: impl FnOnce() -> Option<Retained<NSImage>>) -> Option<Retained<NSImage>> {
    if let Some(i) = ICONS.with(|c| c.borrow().get(key).cloned()) {
        return Some(i);
    }
    let img = make()?;
    ICONS.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() > 6000 {
            c.clear();
        }
        c.insert(key.to_owned(), img.clone());
    });
    Some(img)
}

fn is_package(path: &str, ext: &str) -> bool {
    if let Some(p) = PACKAGES.with(|c| c.borrow().get(ext).copied()) {
        return p;
    }
    let p = NSWorkspace::sharedWorkspace().isFilePackageAtPath(&ns(path));
    PACKAGES.with(|c| c.borrow_mut().insert(ext.to_owned(), p));
    p
}

// a real folder icon, repainted in the colour of the place it lives in. `pt` is the
// size on screen, drawn at twice that for retina
fn folder_icon(path: &str, generic: bool, hue: usize, pt: f64) -> Option<Retained<NSImage>> {
    let key = if generic { format!("f{hue}@{pt}") } else { format!("f{hue}@{pt}:{path}") };
    cached(&key, || {
        let cg = crate::icons::tinted(if generic { "/usr/lib" } else { path }, (pt * 2.0) as usize, HUES[hue])?;
        Some(NSImage::initWithCGImage_size(NSImage::alloc(), &cg, NSSize::new(pt, pt)))
    })
}

fn icon_at(e: &Ent, hue: usize, pt: f64) -> Retained<NSImage> {
    icon_kind(e, hue, pt).0
}

// the picture for a file, and whether it's a preview of what's inside (true) or an icon
fn icon_kind(e: &Ent, hue: usize, pt: f64) -> (Retained<NSImage>, bool) {
    let ext = ext_of(&e.name);
    let parent = parent_of(&e.path);
    if e.dir && !e.custom && parent != "/Volumes" && (ext.is_empty() || !is_package(&e.path, &ext)) {
        // the ones sitting in / and ~ have their own artwork with the little glyphs on
        let generic = parent != "/" && parent != home() && parent != "/Users";
        if let Some(i) = folder_icon(&e.path, generic, hue, pt) {
            return (i, false);
        }
    }
    if let Some(t) = thumb_for(e, &ext, pt) {
        return (t, true);
    }
    if let Some(t) = text_for(e, &ext, pt) {
        return (t, true);
    }
    let key = if !e.dir && !ext.is_empty() && ext.len() < 10 && !matches!(ext.as_str(), "app" | "icns" | "exe" | "webloc") {
        format!("ext:{ext}@{pt}")
    } else {
        format!("{}@{pt}", e.path)
    };
    let icon = cached(&key, || {
        let img = NSWorkspace::sharedWorkspace().iconForFile(&ns(&e.path));
        img.setSize(NSSize::new(pt, pt));
        Some(img)
    });
    (icon.unwrap(), false)
}

// the coloured tile in the sidebar: a rounded square in the place's colour with the
// glyph knocked out in white, same idea as system settings
fn chip(name: &str, hue: usize) -> Option<Retained<NSImage>> {
    cached(&format!("chip:{name}:{hue}"), || {
        let cfg = NSImageSymbolConfiguration::configurationWithPointSize_weight(10.5, unsafe { NSFontWeightSemibold })
            .configurationByApplyingConfiguration(&NSImageSymbolConfiguration::configurationWithPaletteColors(&NSArray::from_retained_slice(&[NSColor::whiteColor()])));
        let glyph = symbol(name)?.imageWithSymbolConfiguration(&cfg)?;
        let (top, bottom) = (tone(hue, 1.16, 1.0), tone(hue, 0.86, 1.0));
        let draw = block2::RcBlock::new(move |r: NSRect| -> Bool {
            let shape = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(r, 5.5, 5.5);
            if let Some(g) = NSGradient::initWithStartingColor_endingColor(NSGradient::alloc(), &top, &bottom) {
                g.drawInBezierPath_angle(&shape, -90.0);
            }
            let s = glyph.size();
            glyph.drawInRect(NSRect::new(
                NSPoint::new(r.origin.x + (r.size.width - s.width) / 2.0, r.origin.y + (r.size.height - s.height) / 2.0),
                s,
            ));
            Bool::YES
        });
        Some(NSImage::imageWithSize_flipped_drawingHandler(NSSize::new(20.0, 20.0), false, &draw))
    })
}

// ---------- thumbnails ----------

const THUMB_EXTS: [&str; 24] = [
    "png", "jpg", "jpeg", "heic", "heif", "gif", "webp", "tiff", "tif", "bmp", "avif", "psd", "svg", "pdf", "mp4", "mov", "m4v", "dng", "cr2",
    "cr3", "nef", "arw", "raf", "jxl",
];

// pictures, films and pdfs show what's in them instead of a blank page. asked for
// once per file, off the main thread, and only for rows that actually got on screen.
fn thumb_for(e: &Ent, ext: &str, pt: f64) -> Option<Retained<NSImage>> {
    if e.dir || !THUMB_EXTS.contains(&ext) {
        return None;
    }
    if let Some(got) = THUMBS.with(|t| t.borrow().get(&e.path).filter(|x| x.0 == e.mtime && x.1 == pt).map(|x| x.2.clone())) {
        return got;
    }
    THUMBS.with(|t| {
        let mut t = t.borrow_mut();
        // big previews are a quarter megabyte each, so keep far fewer of those around
        if t.len() > if pt > 40.0 { 500 } else { 4000 } {
            t.clear();
        }
        t.insert(e.path.clone(), (e.mtime, pt, None));
    });
    request_thumb(e.path.clone(), e.mtime, pt);
    None
}

fn request_thumb(path: String, mtime: i64, pt: f64) {
    let (Some(rc), Some(gc)) = (AnyClass::get(c"QLThumbnailGenerationRequest"), AnyClass::get(c"QLThumbnailGenerator")) else { return };
    let url = file_url(&path);
    // 4 = QLThumbnailGenerationRequestRepresentationTypeThumbnail
    let req: Option<Retained<AnyObject>> = unsafe {
        let a: Allocated<AnyObject> = msg_send![rc, alloc];
        msg_send![a, initWithFileAtURL: &*url, size: NSSize::new(pt, pt), scale: 2.0f64, representationTypes: 4usize]
    };
    let generator: Option<Retained<AnyObject>> = unsafe { msg_send![gc, sharedGenerator] };
    let (Some(req), Some(generator)) = (req, generator) else { return };
    let done = block2::RcBlock::new(move |rep: *mut AnyObject, _err: *mut AnyObject| {
        if rep.is_null() {
            return;
        }
        let cg: *mut CGImage = unsafe { msg_send![rep, CGImage] };
        let Some(cg) = NonNull::new(cg) else { return };
        let cg = unsafe { CFRetained::retain(cg) };
        let path = path.clone();
        dispatch2::DispatchQueue::main().exec_async(move || thumb_ready(path, mtime, pt, cg));
    });
    let _: () = unsafe { msg_send![&*generator, generateBestRepresentationForRequest: &*req, completionHandler: &*done] };
}

fn thumb_ready(path: String, mtime: i64, pt: f64, cg: CFRetained<CGImage>) {
    let (w, h) = (CGImage::width(Some(&cg)) as f64, CGImage::height(Some(&cg)) as f64);
    let img = NSImage::initWithCGImage_size(NSImage::alloc(), &cg, NSSize::new(w / 2.0, h / 2.0));
    THUMBS.with(|t| t.borrow_mut().insert(path.clone(), (mtime, pt, Some(img.clone()))));
    deliver(&path, pt, &img);
}

// a picture that turned up after its file was already on screen
fn deliver(path: &str, pt: f64, img: &NSImage) {
    let Some(u) = ui() else { return };
    if grid_mode() {
        if pt == grid::icon() {
            grid::set_image(&u, path, img);
        }
        return;
    }
    if pt != icon_pt() {
        return;
    }
    let vis = u.table.rowsInRect(u.table.visibleRect());
    let row = m(|s| (vis.location..vis.location + vis.length).find(|&i| s.rows.get(i).is_some_and(|e| e.path == path)));
    let Some(row) = row else { return };
    let cell = u.table.viewAtColumn_row_makeIfNecessary(0, row as NSInteger, false).and_then(|v| v.downcast::<NSTableCellView>().ok());
    if let Some(iv) = cell.and_then(|c| unsafe { c.imageView() }) {
        iv.setImage(Some(img));
    }
}

// ---------- text previews ----------

// finder gives every text file the same blank page with fake grey lines on it. a
// folder of forty source files is forty identical pages. here the page shows the
// start of what's actually in the file.

const TEXT_EXTS: [&str; 62] = [
    "txt", "md", "markdown", "json", "toml", "yaml", "yml", "xml", "html", "htm", "css", "scss", "js", "mjs", "ts", "tsx", "jsx", "rs", "py", "c", "h",
    "cpp", "hpp", "cc", "m", "mm", "swift", "java", "kt", "go", "rb", "php", "lua", "luau", "sh", "zsh", "bash", "sql", "csv", "tsv", "log", "ini",
    "conf", "cfg", "env", "lock", "plist", "tex", "srt", "vtt", "gradle", "cmake", "mk", "r", "pl", "cs", "dart", "vue", "svelte", "gitignore", "patch",
    "diff",
];

fn kind_colour(ext: &str) -> (f64, f64, f64) {
    match ext {
        "rs" | "java" | "kt" | "swift" => (0.96, 0.56, 0.26),
        "py" | "ts" | "tsx" | "go" | "dart" => (0.36, 0.62, 0.96),
        "js" | "mjs" | "jsx" | "lua" | "luau" => (0.95, 0.80, 0.26),
        "json" | "toml" | "yaml" | "yml" | "xml" | "plist" | "ini" | "conf" | "cfg" | "env" | "lock" => (0.52, 0.80, 0.46),
        "html" | "htm" | "css" | "scss" | "vue" | "svelte" => (0.96, 0.46, 0.38),
        "sh" | "zsh" | "bash" | "mk" | "cmake" | "gradle" => (0.42, 0.84, 0.66),
        "c" | "h" | "cpp" | "hpp" | "cc" | "m" | "mm" | "cs" => (0.60, 0.56, 0.98),
        "sql" | "csv" | "tsv" => (0.30, 0.78, 0.86),
        _ => (0.64, 0.68, 0.76),
    }
}

// the page for one text file, if it's ready. not for the small list rows, where a
// page of text would be twelve grey pixels
fn text_for(e: &Ent, ext: &str, pt: f64) -> Option<Retained<NSImage>> {
    if e.dir || e.size == 0 || pt < 32.0 {
        return None;
    }
    // known text, anything that counts as code, and small files of no known kind
    // (README, Makefile, LICENSE). those get a sniff before they get a page
    let cat = category(e);
    if !(TEXT_EXTS.contains(&ext) || cat == 6 || (cat == 8 && e.size < 2_000_000)) {
        return None;
    }
    let dark = DARK.with(|d| d.get());
    if let Some(got) = TEXTS.with(|t| t.borrow().get(&e.path).filter(|x| x.0 == e.mtime && x.1 == pt && x.2 == dark).map(|x| x.3.clone())) {
        return got;
    }
    TEXTS.with(|t| {
        let mut t = t.borrow_mut();
        if t.len() > 3000 {
            t.clear();
        }
        t.insert(e.path.clone(), (e.mtime, pt, dark, None));
    });
    let (path, mtime, ext) = (e.path.clone(), e.mtime, ext.to_owned());
    dispatch2::DispatchQueue::global_queue(dispatch2::GlobalQueueIdentifier::QualityOfService(dispatch2::DispatchQoS::Utility)).exec_async(move || {
        let Some(lines) = text_head(&path) else { return };
        dispatch2::DispatchQueue::main().exec_async(move || {
            let img = text_page(&lines, &ext, pt, dark);
            TEXTS.with(|t| t.borrow_mut().insert(path.clone(), (mtime, pt, dark, Some(img.clone()))));
            deliver(&path, pt, &img);
        });
    });
    None
}

// the first lines of a file, or nothing if it doesn't look like text
fn text_head(path: &str) -> Option<Vec<String>> {
    use std::io::Read;
    let mut buf = vec![0u8; 6144];
    let n = fs::File::open(path).ok()?.read(&mut buf).ok()?;
    buf.truncate(n);
    if n == 0 || buf.contains(&0) {
        return None;
    }
    let text = String::from_utf8_lossy(&buf);
    // a file that's mostly replacement characters was never text
    if text.chars().filter(|c| *c == '\u{FFFD}').count() * 20 > text.chars().count() {
        return None;
    }
    Some(text.lines().take(48).map(|l| l.replace('\t', "  ").chars().take(90).collect()).collect())
}

// a sheet of paper with the top of the file printed on it. the text thins out toward
// the bottom instead of being cut off, and the kind of file is a small label down
// there, the way it's printed on finder's own document icons. paper in dark mode too,
// a little dimmed: a document is a page, not a terminal window
fn text_page(lines: &[String], ext: &str, pt: f64, dark: bool) -> Retained<NSImage> {
    let (w, h) = ((pt * 0.78).round(), pt);
    let size = (pt / 17.0).clamp(3.4, 8.0);
    let paper = if dark { (0.92, 0.915, 0.9) } else { (1.0, 1.0, 1.0) };
    let ink = (0.24, 0.25, 0.28);
    let (ar, ag, ab) = kind_colour(ext);
    // the kind colour, deepened enough to read on paper
    let label_c = (ar * 0.72, ag * 0.72, ab * 0.72);
    let rgb = |c: (f64, f64, f64), a: f64| NSColor::colorWithSRGBRed_green_blue_alpha(c.0, c.1, c.2, a);
    let attrs = |font: &NSFont, color: &NSColor| -> Retained<NSDictionary<NSString, AnyObject>> {
        let (fk, ck) = unsafe { (objc2_app_kit::NSFontAttributeName, objc2_app_kit::NSForegroundColorAttributeName) };
        let (f, c): (&AnyObject, &AnyObject) = (font, color);
        NSDictionary::from_slices(&[fk, ck], &[f, c])
    };
    let body = attrs(&NSFont::monospacedSystemFontOfSize_weight(size, unsafe { NSFontWeightRegular }), &rgb(ink, 0.9));
    let tag_size = (pt / 12.0).clamp(6.0, 10.5);
    let tag = attrs(&NSFont::systemFontOfSize_weight(tag_size, unsafe { NSFontWeightSemibold }), &rgb(label_c, 1.0));
    let label = ns(&ext.to_uppercase().chars().take(5).collect::<String>());
    let lines: Vec<Retained<NSString>> = lines.iter().map(|l| ns(l)).collect();
    let paper_c = rgb(paper, 1.0);
    let veil = NSGradient::initWithStartingColor_endingColor(NSGradient::alloc(), &rgb(paper, 0.0), &rgb(paper, 1.0));
    let pad = (pt * 0.08).round().max(3.0);
    let draw = block2::RcBlock::new(move |r: NSRect| -> Bool {
        use objc2_app_kit::NSStringDrawing;
        paper_c.setFill();
        NSBezierPath::fillRect(r);
        let step = (size * 1.32).round().max(4.0);
        let mut y = pad + 1.0;
        for l in &lines {
            if y + step > r.size.height - 1.0 {
                break;
            }
            unsafe { l.drawAtPoint_withAttributes(NSPoint::new(pad, y), Some(&body)) };
            y += step;
        }
        // the last third of the page dissolves into paper, so the label sits on a
        // clean bottom margin and the text just runs out
        let fade_h = r.size.height * 0.38;
        if let Some(v) = &veil {
            v.drawInRect_angle(NSRect::new(NSPoint::new(0.0, r.size.height - fade_h), NSSize::new(r.size.width, fade_h)), 90.0);
        }
        if label.length() > 0 {
            let sz = unsafe { label.sizeWithAttributes(Some(&tag)) };
            let at = NSPoint::new(r.size.width - sz.width - pad, r.size.height - sz.height - pad * 0.7);
            unsafe { label.drawAtPoint_withAttributes(at, Some(&tag)) };
        }
        Bool::YES
    });
    NSImage::imageWithSize_flipped_drawingHandler(NSSize::new(w, h), true, &draw)
}

// ---------- tags ----------

fn tag_strings(path: &str) -> Vec<String> {
    let Some(blob) = dir::tag_blob(path) else { return Vec::new() };
    let data = NSData::with_bytes(&blob);
    let Ok(obj) = (unsafe {
        objc2_foundation::NSPropertyListSerialization::propertyListWithData_options_format_error(
            &data,
            objc2_foundation::NSPropertyListMutabilityOptions(0),
            std::ptr::null_mut(),
        )
    }) else {
        return Vec::new();
    };
    let Ok(arr) = obj.downcast::<NSArray>() else { return Vec::new() };
    (0..arr.count()).filter_map(|i| arr.objectAtIndex(i).downcast::<NSString>().ok().map(|s| s.to_string())).collect()
}

fn tag_colour(tag: &str) -> u8 {
    tag.rsplit_once('\n').and_then(|x| x.1.trim().parse::<u8>().ok()).filter(|n| (1..=7).contains(n)).unwrap_or(0)
}

// the colour numbers on a file. the finder label came with the directory read, so
// the xattr is only looked at for files that have a colour at all
fn tag_colours(e: &Ent) -> Vec<u8> {
    if e.label == 0 {
        return Vec::new();
    }
    let mut v: Vec<u8> = Vec::new();
    for n in tag_strings(&e.path).iter().map(|t| tag_colour(t)) {
        if n != 0 && !v.contains(&n) {
            v.push(n);
        }
    }
    if v.is_empty() {
        v.push(e.label.min(7));
    }
    v
}

// what fits beside the name: the last three, like finder
fn tags_of(e: &Ent) -> Vec<u8> {
    let v = tag_colours(e);
    v[v.len().saturating_sub(3)..].to_vec()
}

fn dots(tags: &[u8]) -> Option<Retained<NSImage>> {
    let key: String = tags.iter().map(|t| char::from(b'0' + t)).collect();
    let tags = tags.to_vec();
    let w = tags.len() as f64 * 8.0 + (tags.len() as f64 - 1.0).max(0.0) * 3.0;
    cached(&format!("tags:{key}"), || {
        let draw = block2::RcBlock::new(move |_r: NSRect| -> Bool {
            for (i, &t) in tags.iter().enumerate() {
                let (r, g, b) = TAG_RGB[(t as usize).min(7)];
                NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, 1.0).setFill();
                NSBezierPath::bezierPathWithOvalInRect(NSRect::new(NSPoint::new(i as f64 * 11.0, 0.0), NSSize::new(8.0, 8.0))).fill();
            }
            Bool::YES
        });
        Some(NSImage::imageWithSize_flipped_drawingHandler(NSSize::new(w, 8.0), false, &draw))
    })
}

fn tag_name(n: u8) -> String {
    let labels = NSWorkspace::sharedWorkspace().fileLabels();
    if labels.count() == 8 {
        return labels.objectAtIndex(n as usize).to_string();
    }
    TAG_NAMES[(n as usize).min(7)].to_owned()
}

// click a colour that every picked file already has and it comes off, otherwise it goes on
fn toggle_tag(u: &Rc<Ui>, n: u8, picked: &[Ent]) {
    if picked.is_empty() || !(1..=7).contains(&n) {
        return;
    }
    let lists: Vec<Vec<String>> = picked
        .iter()
        .map(|e| {
            let mut t = tag_strings(&e.path);
            if t.is_empty() && e.label != 0 {
                t.push(format!("{}\n{}", tag_name(e.label), e.label));
            }
            t
        })
        .collect();
    let all_have = lists.iter().all(|t| t.iter().any(|x| tag_colour(x) == n));
    let name = tag_name(n);
    let mut failed = 0;
    for (e, mut tags) in picked.iter().zip(lists) {
        if all_have {
            tags.retain(|x| tag_colour(x) != n);
        } else if !tags.iter().any(|x| tag_colour(x) == n) {
            tags.push(format!("{name}\n{n}"));
        }
        let last = tags.iter().rev().map(|x| tag_colour(x)).find(|&c| c != 0).unwrap_or(0);
        // the label number first, it rewrites the tag list. then the real list on top
        let num = NSNumber::new_isize(last as isize);
        let ok = unsafe { file_url(&e.path).setResourceValue_forKey_error(Some(&num), objc2_foundation::NSURLLabelNumberKey) }.is_ok();
        let wrote = if tags.is_empty() {
            dir::set_tag_blob(&e.path, None) || dir::tag_blob(&e.path).is_none()
        } else {
            let list: Vec<Retained<NSString>> = tags.iter().map(|t| ns(t)).collect();
            let arr = NSArray::from_retained_slice(&list);
            unsafe {
                objc2_foundation::NSPropertyListSerialization::dataWithPropertyList_format_options_error(
                    &arr,
                    objc2_foundation::NSPropertyListFormat::BinaryFormat_v1_0,
                    0,
                )
            }
            .is_ok_and(|d| dir::set_tag_blob(&e.path, Some(&d.to_vec())))
        };
        if !ok || !wrote {
            failed += 1;
        }
    }
    m(|s| s.note = (failed > 0).then(|| format!("Couldn't tag {failed} item{}", if failed == 1 { "" } else { "s" })));
    reload(u);
}

// ---------- model ----------

fn rebuild(s: &mut Model) {
    let q = s.query.trim().to_lowercase();
    let hidden = s.hidden;
    let mut rows: Vec<Ent> = s.all.iter().filter(|e| (hidden || !e.hidden) && (q.is_empty() || e.lc.contains(&q))).cloned().collect();
    let (col, asc) = s.sort;
    match col {
        3 => {
            rows.sort_by(|a, b| nat_cmp(&a.lc, &b.lc));
            rows.sort_by_cached_key(|e| kind_of(&e.path, &e.name, e.dir));
            if !asc {
                rows.reverse();
            }
        }
        1 | 2 => rows.sort_by(|a, b| {
            let o = if col == 1 { a.mtime.cmp(&b.mtime) } else { a.size.cmp(&b.size) };
            (if asc { o } else { o.reverse() }).then_with(|| nat_cmp(&a.lc, &b.lc))
        }),
        _ => rows.sort_by(|a, b| {
            let o = nat_cmp(&a.lc, &b.lc);
            b.dir.cmp(&a.dir).then(if asc { o } else { o.reverse() })
        }),
    }
    let local = rows.len();
    if q.chars().count() >= 2 {
        let have: HashSet<String> = rows.iter().map(|e| e.path.clone()).collect();
        let hits = files::with_index(|i| i.search(&q, &HashMap::new(), 200)).unwrap_or_default();
        for h in hits {
            if have.contains(&h.path) {
                continue;
            }
            if let Some(mut e) = dir::ent_of(&h.path) {
                e.place = tilde(parent_of(&h.path));
                rows.push(e);
            }
        }
    }
    let mut sections = Vec::new();
    if grid_mode() && !q.is_empty() {
        if local > 0 {
            sections.push(Section { title: "In this folder".into(), start: 0, len: local });
        }
        if rows.len() > local {
            sections.push(Section { title: "Everywhere else".into(), start: local, len: rows.len() - local });
        }
    } else if grid_mode() && GROUP.with(|g| g.get()) {
        // stable, so inside a group things keep the order they were just sorted into
        rows.sort_by_cached_key(category);
        let mut i = 0;
        while i < rows.len() {
            let (c, start) = (category(&rows[i]), i);
            while i < rows.len() && category(&rows[i]) == c {
                i += 1;
            }
            sections.push(Section { title: CATS[c].into(), start, len: i - start });
        }
    }
    if sections.is_empty() {
        sections.push(Section { title: String::new(), start: 0, len: rows.len() });
    }
    s.rows = rows;
    s.sections = sections;
}

fn selected(tv: &NSTableView) -> Vec<usize> {
    let set = tv.selectedRowIndexes();
    let mut v = Vec::with_capacity(set.count());
    let mut i = set.firstIndex();
    while i != objc2_foundation::NSNotFound as usize {
        v.push(i);
        i = set.indexGreaterThanIndex(i);
    }
    v
}

// right click on a row that isn't selected acts on that row, like finder
fn target_rows(u: &Ui) -> Vec<usize> {
    if grid_mode() {
        return grid::selected_rows(u);
    }
    let c = u.table.clickedRow();
    let sel = selected(&u.table);
    if c >= 0 && !sel.contains(&(c as usize)) { vec![c as usize] } else { sel }
}

fn target_ents(u: &Ui) -> Vec<Ent> {
    let idx = target_rows(u);
    m(|s| idx.iter().filter_map(|&i| s.rows.get(i).cloned()).collect())
}

fn target_paths(u: &Ui) -> Vec<String> {
    target_ents(u).into_iter().map(|e| e.path).collect()
}

fn selected_paths(u: &Ui) -> Vec<String> {
    let idx = picked(u);
    m(|s| idx.iter().filter_map(|&i| s.rows.get(i).map(|e| e.path.clone())).collect())
}

fn select_paths(u: &Ui, paths: &[String], scroll: bool) {
    let want: HashSet<&str> = paths.iter().map(String::as_str).collect();
    let idx: Vec<usize> = if want.is_empty() {
        Vec::new()
    } else {
        m(|s| s.rows.iter().enumerate().filter(|(_, e)| want.contains(e.path.as_str())).map(|x| x.0).collect())
    };
    pick_rows(u, &idx, scroll);
}

fn update_status(u: &Ui) {
    let sel = picked(u);
    let text = m(|s| {
        if let Some(n) = &s.note {
            return n.clone();
        }
        let n = s.rows.len();
        if sel.is_empty() {
            return format!("{n} item{}{}", if n == 1 { "" } else { "s" }, s.free);
        }
        let bytes: u64 = sel.iter().filter_map(|&i| s.rows.get(i)).map(|e| e.size).sum();
        let size = if bytes > 0 {
            format!(", {}", NSByteCountFormatter::stringFromByteCount_countStyle(bytes as i64, NSByteCountFormatterCountStyle::File))
        } else {
            String::new()
        };
        format!("{} of {n} selected{size}", sel.len())
    });
    u.status.setStringValue(&ns(&text));
    refresh_ql(u);
}

fn note(u: &Ui, text: Option<String>) {
    m(|s| s.note = text);
    update_status(u);
}

fn chrome(u: &Ui) {
    let (cwd, back, fwd, place, searching, hue) = m(|s| {
        (
            s.cwd.clone(),
            !s.back.is_empty(),
            !s.fwd.is_empty(),
            s.places.iter().position(|p| !p.header && p.path == s.cwd),
            !s.query.trim().is_empty(),
            hue_in(&s.places, &s.cwd),
        )
    });
    if ACCENT.with(|a| a.replace(hue)) != hue {
        let _: () = unsafe { msg_send![&*u.table, setNeedsDisplay: true] };
    }
    DARK.with(|d| d.set(is_dark(&u.stage)));
    let title = if cwd == "/" { "Macintosh HD".to_owned() } else { cwd.rsplit('/').next().unwrap_or(&cwd).to_owned() };
    let url = file_url(&cwd);
    u.window.setTitle(&ns(&title));
    u.window.setRepresentedURL(Some(&url));
    u.path.setURL(Some(&url));
    paint_path(u);
    u.nav.setEnabled_forSegment(back, 0);
    u.nav.setEnabled_forSegment(fwd, 1);
    // the sidebar never has a selected row of its own: a selected row gets its text
    // greyed out by the system. where you are is shown by the pill alone
    slide_pill(u, place, hue);
    u.cols[3].setTitle(&ns(if searching { "Where" } else { "Kind" }));
}

// the sidebar highlight travels to the place you picked instead of blinking from one
// row to the other. a spring with no bounce, starting from wherever it is on screen,
// so clicking down the list fast makes it follow along instead of jumping back
fn slide_pill(u: &Ui, row: Option<usize>, hue: usize) {
    use objc2_app_kit::{NSAnimatablePropertyContainer, NSAnimationContext};
    let pill = &u.side_pill;
    // a window that was never shown doesn't run appkit's fades, so off screen it just jumps
    let live = u.window.isVisible() && !reduce_motion();
    let Some(row) = row else {
        if live {
            NSAnimationContext::beginGrouping();
            NSAnimationContext::currentContext().setDuration(QUICK);
            NSAnimationContext::currentContext().setTimingFunction(Some(&ease_in()));
            pill.animator().setAlphaValue(0.0);
            NSAnimationContext::endGrouping();
        } else {
            pill.setAlphaValue(0.0);
        }
        return;
    };
    let r = u.side.rectOfRow(row as NSInteger);
    let to = NSRect::new(NSPoint::new(r.origin.x + 10.0, r.origin.y + 1.0), NSSize::new((r.size.width - 20.0).max(20.0), r.size.height - 2.0));
    pill.setFillColor(&tone(hue, 1.0, 0.26));
    let from_nowhere = pill.alphaValue() < 0.05;
    let layer = pill.layer();
    let was = layer.as_ref().map(|l| unsafe { l.presentationLayer() }.map_or(l.position(), |p| p.position()));
    pill.setFrame(to);
    if !live {
        pill.setAlphaValue(1.0);
        return;
    }
    if from_nowhere {
        // nothing to travel from: it fades up where it belongs
        NSAnimationContext::beginGrouping();
        NSAnimationContext::currentContext().setDuration(0.16);
        pill.animator().setAlphaValue(1.0);
        NSAnimationContext::endGrouping();
        return;
    }
    pill.setAlphaValue(1.0);
    if let (Some(l), Some(was)) = (layer, was) {
        let now = l.position();
        if (now.y - was.y).abs() > 0.5 {
            l.addAnimation_forKey(&spring("position.y", was.y, now.y, 0.3), Some(&ns("travel")));
        }
    }
}

// the crumbs at the bottom: plain quiet words for the way here, and only where you
// are gets its coloured folder and full-strength text. a row of identical little
// folder icons was just noise
fn paint_path(u: &Ui) {
    let items = u.path.pathItems();
    let h = home();
    let n = items.count();
    let font = NSFont::systemFontOfSize(11.0);
    for i in 0..n {
        let it = items.objectAtIndex(i);
        let Some(p) = it.URL().and_then(|x| x.path()).map(|p| p.to_string()) else { continue };
        let last = i + 1 == n;
        let ink = if last { NSColor::labelColor() } else { NSColor::secondaryLabelColor() };
        let (fk, ck) = unsafe { (objc2_app_kit::NSFontAttributeName, objc2_app_kit::NSForegroundColorAttributeName) };
        let (f, c): (&AnyObject, &AnyObject) = (&font, &ink);
        let attrs = NSDictionary::from_slices(&[fk, ck], &[f, c]);
        let title = it.title();
        let styled = unsafe { objc2_foundation::NSAttributedString::new_with_attributes(&title, &attrs) };
        it.setAttributedTitle(&styled);
        if !last {
            it.setImage(None);
            continue;
        }
        let par = parent_of(&p);
        let ext = ext_of(p.rsplit('/').next().unwrap_or(""));
        if p == "/" || par == "/Volumes" || (!ext.is_empty() && is_package(&p, &ext)) {
            continue;
        }
        let hue = m(|s| hue_in(&s.places, &p));
        if let Some(img) = folder_icon(&p, par != "/" && par != h && par != "/Users", hue, 16.0) {
            it.setImage(Some(&img));
        }
    }
}

fn update_empty(u: &Ui) {
    let (n, all, q, loading, hue) = m(|s| (s.rows.len(), s.all.len(), s.query.trim().to_owned(), s.loading, hue_in(&s.places, &s.cwd)));
    let was_hidden = u.empty.isHidden();
    u.empty.setHidden(n > 0 || loading);
    if n > 0 || loading {
        return;
    }
    // the message drifts up into place rather than blinking on
    if was_hidden && u.window.isVisible() && !reduce_motion() {
        u.empty.setWantsLayer(true);
        if let Some(l) = u.empty.layer() {
            l.addAnimation_forKey(&basic("opacity", 0.0, 1.0, 0.24, &crate::view::ease_out()), Some(&ns("in")));
            l.addAnimation_forKey(&spring("transform.translation.y", -4.0, 0.0, 0.36), Some(&ns("rise")));
        }
    }
    let (title, hint) = if !q.is_empty() {
        let cfg = NSImageSymbolConfiguration::configurationWithPointSize_weight(38.0, unsafe { NSFontWeightRegular })
            .configurationByApplyingConfiguration(&NSImageSymbolConfiguration::configurationWithHierarchicalColor(&tone(hue, 1.0, 1.0)));
        u.empty_icon.setImage(symbol("magnifyingglass").and_then(|i| i.imageWithSymbolConfiguration(&cfg)).as_deref());
        (format!("Nothing matches “{q}”"), "Search looks in this folder first, then everywhere else".to_owned())
    } else {
        u.empty_icon.setImage(folder_icon("", true, hue, 64.0).as_deref());
        if all > 0 {
            (format!("{all} hidden item{}", if all == 1 { "" } else { "s" }), "Press ⇧⌘. to show hidden files".to_owned())
        } else {
            ("This folder is empty".to_owned(), "Drop files here, or press ⇧⌘N for a new folder".to_owned())
        }
    };
    u.empty_title.setStringValue(&ns(&title));
    u.empty_hint.setStringValue(&ns(&hint));
}

fn finish(u: &Ui, generation: u64, got: (Vec<Ent>, i64), keep: &[String], top: bool) {
    let ok = m(|s| {
        if s.load_gen != generation {
            return false;
        }
        s.all = got.0;
        s.stamp = got.1;
        s.loading = false;
        true
    });
    if !ok {
        return;
    }
    if top {
        m(rebuild);
        reload_view(u);
    } else {
        // the same folder read again: show only what changed
        update_rows(u);
    }
    update_empty(u);
    select_paths(u, keep, true);
    if top && keep.is_empty() {
        scroll_top(u);
    }
    if top {
        arrive(u, NAV.with(|n| n.replace(0.0)));
    }
    update_status(u);
}

// read the folder off the main thread. almost every folder comes back in well under
// a frame so we wait a moment for it and show it with no flicker. if a dead network
// mount takes ten seconds, the window stays alive and fills in when it's done.
fn load(u: &Rc<Ui>, keep: Vec<String>, top: bool) {
    let (generation, path) = m(|s| {
        s.load_gen += 1;
        (s.load_gen, s.cwd.clone())
    });
    let slot: Arc<Mutex<Option<(Vec<Ent>, i64)>>> = Arc::default();
    let (tx, rx) = mpsc::channel::<()>();
    {
        let (slot, keep) = (slot.clone(), keep.clone());
        std::thread::spawn(move || {
            let stamp = dir::stamp(&path);
            let ents = dir::list(&path);
            *slot.lock().unwrap() = Some((ents, stamp));
            let _ = tx.send(());
            dispatch2::DispatchQueue::main().exec_async(move || {
                let got = slot.lock().unwrap().take();
                if let (Some(got), Some(u)) = (got, ui()) {
                    finish(&u, generation, got, &keep, top);
                }
            });
        });
    }
    if rx.recv_timeout(Duration::from_millis(40)).is_ok() {
        let got = slot.lock().unwrap().take();
        if let Some(got) = got {
            finish(u, generation, got, &keep, top);
        }
    } else if top {
        m(|s| {
            s.all.clear();
            s.rows.clear();
            s.note = Some("Loading…".into());
            s.loading = true;
        });
        reload_view(u);
        update_empty(u);
        update_status(u);
        m(|s| s.note = None);
    }
}

fn reload(u: &Rc<Ui>) {
    let keep = selected_paths(u);
    load(u, keep, false);
}

// true when the view moved its rows itself; false when it was reloaded and the caller
// may want to fade it in
fn refresh_rows(u: &Ui) -> bool {
    let keep = selected_paths(u);
    let moved = update_rows(u);
    update_empty(u);
    select_paths(u, &keep, false);
    update_status(u);
    moved
}

// works the rows out again (new sort, a filter, files that came or went) and lets the
// view show the difference: what stayed glides to its new place, what's new fades in,
// what's gone fades out. finder just blinks and you go looking for where things went.
fn update_rows(u: &Ui) -> bool {
    let renamed = RENAMED.with(|r| r.borrow_mut().take());
    let (rows, secs) = m(|s| {
        let (r, sc) = (std::mem::take(&mut s.rows), std::mem::take(&mut s.sections));
        rebuild(s);
        (std::mem::replace(&mut s.rows, r), std::mem::replace(&mut s.sections, sc))
    });
    let live = (u.window.isVisible() || TESTING.with(|t| t.get())) && !reduce_motion();
    let tried = if !live {
        Err((rows, secs))
    } else if grid_mode() {
        grid::apply(u, rows, secs, renamed.as_ref())
    } else {
        list_apply(u, rows, secs, renamed.as_ref())
    };
    if TESTING.with(|t| t.get()) {
        eprintln!("snapshot: rows {}", if tried.is_ok() { "animated" } else { "reloaded" });
    }
    match tried {
        Ok(()) => true,
        Err((rows, secs)) => {
            m(|s| {
                s.rows = rows;
                s.sections = secs;
            });
            reload_view(u);
            false
        }
    }
}

// the list's version of the same. a table applies its changes one after the other, so
// this plays the old order forward into the new one: out with what's gone, then walk
// the new order and pull each row up to where it belongs
#[allow(clippy::type_complexity)]
fn list_apply(u: &Ui, rows: Vec<Ent>, secs: Vec<Section>, renamed: Option<&(String, String)>) -> Result<(), (Vec<Ent>, Vec<Section>)> {
    let old: Vec<Ent> = m(|s| s.rows.clone());
    if u.table.numberOfRows() as usize != old.len() || old.len() > 1500 || rows.len() > 1500 {
        return Err((rows, secs));
    }
    let key = |p: &str| -> String {
        match renamed {
            Some((from, to)) if p == from => to.clone(),
            _ => p.to_owned(),
        }
    };
    let was: Vec<String> = old.iter().map(|e| key(&e.path)).collect();
    let now: Vec<&str> = rows.iter().map(|e| e.path.as_str()).collect();
    let was_set: HashSet<&str> = was.iter().map(String::as_str).collect();
    let now_set: HashSet<&str> = now.iter().copied().collect();
    if was_set.len() != was.len() || now_set.len() != now.len() {
        return Err((rows, secs));
    }
    let gone = NSMutableIndexSet::new();
    let mut cur: Vec<&str> = Vec::with_capacity(now.len());
    for (i, p) in was.iter().enumerate() {
        if now_set.contains(p.as_str()) {
            cur.push(p);
        } else {
            gone.addIndex(i);
        }
    }
    enum Op {
        Add(usize),
        Move(usize, usize),
    }
    let mut ops = Vec::new();
    for (i, p) in now.iter().enumerate() {
        if !was_set.contains(p) {
            cur.insert(i, p);
            ops.push(Op::Add(i));
        } else if cur[i] != *p {
            let Some(j) = cur[i..].iter().position(|x| x == p).map(|j| j + i) else { return Err((rows, secs)) };
            let x = cur.remove(j);
            cur.insert(i, x);
            ops.push(Op::Move(j, i));
        }
    }
    if ops.len() > 600 {
        return Err((rows, secs));
    }
    // rows that stayed but whose details changed get drawn again afterwards
    let before: HashMap<&str, &Ent> = was.iter().map(String::as_str).zip(old.iter()).collect();
    let stale = NSMutableIndexSet::new();
    for (i, e) in rows.iter().enumerate() {
        if let Some(o) = before.get(e.path.as_str()) {
            if o.name != e.name || o.size != e.size || o.mtime != e.mtime || o.label != e.label || o.hidden != e.hidden {
                stale.addIndex(i);
            }
        }
    }
    m(|s| {
        s.rows = rows;
        s.sections = secs;
    });
    use objc2_app_kit::NSTableViewAnimationOptions as A;
    u.table.beginUpdates();
    if gone.count() > 0 {
        u.table.removeRowsAtIndexes_withAnimation(&gone, A::EffectFade | A::SlideUp);
    }
    for op in ops {
        match op {
            Op::Add(i) => u.table.insertRowsAtIndexes_withAnimation(&objc2_foundation::NSIndexSet::indexSetWithIndex(i), A::EffectFade | A::SlideDown),
            Op::Move(j, i) => u.table.moveRowAtIndex_toIndex(j as NSInteger, i as NSInteger),
        }
    }
    u.table.endUpdates();
    if stale.count() > 0 {
        let cols = objc2_foundation::NSIndexSet::indexSetWithIndexesInRange(NSRange::new(0, u.cols.len()));
        u.table.reloadDataForRowIndexes_columnIndexes(&stale, &cols);
    }
    Ok(())
}

fn free_space(path: &str) -> String {
    (|| {
        let d = NSFileManager::defaultManager().attributesOfFileSystemForPath_error(&ns(path)).ok()?;
        let n = d.objectForKey(unsafe { objc2_foundation::NSFileSystemFreeSize })?.downcast::<NSNumber>().ok()?;
        Some(format!(
            ", {} available",
            NSByteCountFormatter::stringFromByteCount_countStyle(n.longLongValue(), NSByteCountFormatterCountStyle::File)
        ))
    })()
    .unwrap_or_default()
}

fn is_dark(v: &NSView) -> bool {
    use objc2_app_kit::{NSAppearanceCustomization, NSAppearanceNameAqua, NSAppearanceNameDarkAqua};
    let names = unsafe { NSArray::from_slice(&[NSAppearanceNameAqua, NSAppearanceNameDarkAqua]) };
    v.effectiveAppearance().bestMatchFromAppearancesWithNames(&names).is_some_and(|n| unsafe { &*n == NSAppearanceNameDarkAqua })
}

fn reduce_motion() -> bool {
    NSWorkspace::sharedWorkspace().accessibilityDisplayShouldReduceMotion()
}

// ---------- motion ----------
// how things move in this window, all in one place:
//
// - nothing that answers a click waits. a selection is there the instant you press
// - things that travel ride a spring with no bounce: they land instead of wobbling, and
//   a new move starts from wherever the last one had got to
// - when what's shown changes, it's a real crossfade. the old picture fades out exactly
//   as it was while the new one comes in, and only the new one moves, and not far
// - leaving is quicker than arriving
// - short. an animation you notice is too long

const QUICK: f64 = 0.12;

// for the few things that still go through appkit's animator (the grid's batch moves, a
// scroll): quick off the mark, then a long soft landing
fn settle() -> Retained<CAMediaTimingFunction> {
    CAMediaTimingFunction::functionWithControlPoints(0.25, 1.0, 0.5, 1.0)
}

fn ease_in() -> Retained<CAMediaTimingFunction> {
    CAMediaTimingFunction::functionWithControlPoints(0.4, 0.0, 1.0, 1.0)
}

fn basic(key: &str, from: f64, to: f64, dur: f64, tf: &CAMediaTimingFunction) -> Retained<CABasicAnimation> {
    let a = CABasicAnimation::animationWithKeyPath(Some(&ns(key)));
    unsafe {
        a.setFromValue(Some(&NSNumber::new_f64(from)));
        a.setToValue(Some(&NSNumber::new_f64(to)));
    }
    a.setDuration(dur);
    a.setTimingFunction(Some(tf));
    a
}

// a spring that feels like it takes `feel` seconds. bounce 0 is critically damped:
// the fastest way to arrive without going past
fn spring(key: &str, from: f64, to: f64, feel: f64) -> Retained<CABasicAnimation> {
    let a = objc2_quartz_core::CASpringAnimation::initWithPerceptualDuration_bounce(objc2_quartz_core::CASpringAnimation::alloc(), feel, 0.0);
    a.setKeyPath(Some(&ns(key)));
    unsafe {
        a.setFromValue(Some(&NSNumber::new_f64(from)));
        a.setToValue(Some(&NSNumber::new_f64(to)));
    }
    a.setDuration(a.settlingDuration());
    Retained::into_super(a)
}

// where a layer is right now on screen, mid-animation included
fn shown_scale(l: &CALayer) -> f64 {
    unsafe { l.presentationLayer() }.map_or(1.0, |p| p.transform().m11)
}

// grow or shrink a layer about one point of it, given in the view's own coordinates,
// wherever appkit happened to put the layer's anchor. scale and shift run on the same
// curve, so that point stays exactly where it is the whole way
fn scale_about(l: &CALayer, p: NSPoint, from: f64, to: f64, key: &str, hold: bool, make: &dyn Fn(&str, f64, f64) -> Retained<CABasicAnimation>) {
    let (b, a) = (l.bounds().size, l.anchorPoint());
    let (ax, ay) = (p.x - a.x * b.width, p.y - a.y * b.height);
    for (k, f, t) in [
        ("transform.scale", from, to),
        ("transform.translation.x", ax * (1.0 - from), ax * (1.0 - to)),
        ("transform.translation.y", ay * (1.0 - from), ay * (1.0 - to)),
    ] {
        let anim = make(k, f, t);
        if hold {
            anim.setFillMode(unsafe { objc2_quartz_core::kCAFillModeForwards });
            anim.setRemovedOnCompletion(false);
        }
        l.addAnimation_forKey(&anim, Some(&ns(&format!("{key}.{k}"))));
    }
}

fn drop_scale(l: &CALayer, key: &str) {
    for k in ["transform.scale", "transform.translation.x", "transform.translation.y"] {
        l.removeAnimationForKey(&ns(&format!("{key}.{k}")));
    }
}

// the old picture of this view fades into whatever it shows by the end of this turn of
// the run loop. core animation keeps the old frame itself, so nothing has to be copied
fn crossfade(v: &NSView, dur: f64) {
    let Some(l) = v.layer() else { return };
    let t = objc2_quartz_core::CATransition::new();
    t.setType(unsafe { objc2_quartz_core::kCATransitionFade });
    t.setDuration(dur);
    t.setTimingFunction(Some(&CAMediaTimingFunction::functionWithControlPoints(0.3, 0.0, 0.2, 1.0)));
    l.addAnimation_forKey(&t, Some(&ns("crossfade")));
}

fn moving() -> bool {
    ui().is_some_and(|u| u.window.isVisible()) || TESTING.with(|t| t.get())
}

// new content crossfades in. in the icon view it also comes a little toward you out of
// the folder you opened, or settles back onto the one you came out of. the list drifts
// in a few points from the side it came from. a few percent, not a zoom effect
fn arrive(u: &Ui, dir: f64) {
    let origin = ORIGIN.with(|o| o.take());
    if !moving() {
        return;
    }
    crossfade(&u.stage, if dir == 0.0 { 0.18 } else { 0.24 });
    if reduce_motion() || dir == 0.0 {
        return;
    }
    let view: &NSView = if grid_mode() { &u.grid_scroll } else { &u.list_scroll };
    let Some(layer) = view.layer() else { return };
    if grid_mode() {
        let b = view.bounds().size;
        let centre = NSPoint::new(b.width / 2.0, (b.height - u.grid_scroll.contentInsets().top) / 2.0);
        let (at, from) = if dir > 0.0 { (origin.unwrap_or(centre), 0.96) } else { (grid::selected_point(u).unwrap_or(centre), 1.03) };
        scale_about(&layer, at, from, 1.0, "zoom", false, &|k, f, t| spring(k, f, t, 0.34));
    } else {
        layer.addAnimation_forKey(&spring("transform.translation.x", dir * 14.0, 0.0, 0.3), Some(&ns("slide")));
    }
}

fn go(u: &Rc<Ui>, path: &str, history: bool, select: Option<String>) {
    go_from(u, path, history, select, None);
}

fn go_from(u: &Rc<Ui>, path: &str, history: bool, select: Option<String>, dir: Option<f64>) {
    let path = if path.len() > 1 { path.trim_end_matches('/') } else { path };
    if !Path::new(path).is_dir() {
        return;
    }
    let old = m(|s| s.cwd.clone());
    let under = |inner: &str, outer: &str| inner != outer && (outer == "/" || inner.starts_with(&format!("{outer}/")));
    NAV.with(|n| n.set(dir.unwrap_or(if under(path, &old) { 1.0 } else if under(&old, path) { -1.0 } else { 0.0 })));
    let free = free_space(path);
    m(|s| {
        if history && s.cwd != path {
            let old = std::mem::take(&mut s.cwd);
            s.back.push(old);
            s.fwd.clear();
            if s.back.len() > 300 {
                s.back.remove(0);
            }
        }
        s.cwd = path.to_owned();
        s.query.clear();
        s.note = None;
        s.free = free;
    });
    u.search.setStringValue(&ns(""));
    chrome(u);
    load(u, select.into_iter().collect(), true);
}

fn go_back(u: &Rc<Ui>) {
    let Some((to, from)) = m(|s| {
        let to = s.back.pop()?;
        s.fwd.push(s.cwd.clone());
        Some((to, s.cwd.clone()))
    }) else {
        return;
    };
    go_from(u, &to, false, Some(from), Some(-1.0));
}

fn go_fwd(u: &Rc<Ui>) {
    let Some(to) = m(|s| {
        let to = s.fwd.pop()?;
        s.back.push(s.cwd.clone());
        Some(to)
    }) else {
        return;
    };
    go_from(u, &to, false, None, Some(1.0));
}

fn go_up(u: &Rc<Ui>) {
    let from = m(|s| s.cwd.clone());
    if from != "/" {
        go(u, &parent_of(&from).to_owned(), true, Some(from));
    }
}

// ---------- actions ----------

fn open_targets(u: &Rc<Ui>) {
    let rows = target_rows(u);
    let picked: Vec<Ent> = m(|s| rows.iter().filter_map(|&i| s.rows.get(i).cloned()).collect());
    let ws = NSWorkspace::sharedWorkspace();
    if picked.len() == 1 && picked[0].dir && !ws.isFilePackageAtPath(&ns(&picked[0].path)) {
        if grid_mode() {
            ORIGIN.with(|o| o.set(grid::row_point(u, rows[0])));
        }
        go(u, &picked[0].path, true, None);
        return;
    }
    for e in picked {
        ws.openURL(&file_url(&e.path));
    }
}

#[allow(deprecated)]
fn write_paths(paths: &[String]) {
    let pb = NSPasteboard::generalPasteboard();
    let ty = unsafe { objc2_app_kit::NSFilenamesPboardType };
    let list: Vec<Retained<NSString>> = paths.iter().map(|p| ns(p)).collect();
    unsafe {
        pb.declareTypes_owner(&NSArray::from_slice(&[ty, NSPasteboardTypeString]), None);
        pb.setPropertyList_forType(&NSArray::from_retained_slice(&list), ty);
    }
    pb.setString_forType(&ns(&paths.join("\n")), unsafe { NSPasteboardTypeString });
}

#[allow(deprecated)]
fn read_paths(pb: &NSPasteboard) -> Vec<String> {
    let Some(obj) = pb.propertyListForType(unsafe { objc2_app_kit::NSFilenamesPboardType }) else { return Vec::new() };
    let Ok(arr) = obj.downcast::<NSArray>() else { return Vec::new() };
    (0..arr.count())
        .filter_map(|i| arr.objectAtIndex(i).downcast::<NSString>().ok().map(|s| s.to_string()))
        .filter(|p| Path::new(p).exists())
        .collect()
}

fn copy_text(s: &str) {
    let pb = NSPasteboard::generalPasteboard();
    pb.clearContents();
    pb.setString_forType(&ns(s), unsafe { NSPasteboardTypeString });
}

fn copy_targets(u: &Ui, cut: bool) {
    let paths = target_paths(u);
    if paths.is_empty() {
        return;
    }
    write_paths(&paths);
    let n = paths.len();
    let keep = selected_paths(u);
    m(|s| s.cut = if cut { paths } else { Vec::new() });
    reload_view(u);
    select_paths(u, &keep, false);
    note(u, Some(format!("{} {n} item{}", if cut { "Cut" } else { "Copied" }, if n == 1 { "" } else { "s" })));
}

fn transfer(u: &Ui, srcs: Vec<String>, dest: String, moving: bool) {
    let n = srcs.len();
    note(u, Some(format!("{} {n} item{}…", if moving { "Moving" } else { "Copying" }, if n == 1 { "" } else { "s" })));
    dispatch2::DispatchQueue::global_queue(dispatch2::GlobalQueueIdentifier::QualityOfService(dispatch2::DispatchQoS::UserInitiated))
        .exec_async(move || {
            let fm = NSFileManager::defaultManager();
            let dest = if dest.len() > 1 { dest.trim_end_matches('/').to_owned() } else { String::new() };
            let mut made = Vec::new();
            let mut failed = 0;
            for s in &srcs {
                let (sdir, name) = (parent_of(s), s.rsplit('/').next().unwrap_or(""));
                let same = sdir == if dest.is_empty() { "/" } else { dest.as_str() };
                if moving && same {
                    made.push(s.clone());
                    continue;
                }
                if dest == *s || dest.starts_with(&format!("{s}/")) || name.is_empty() {
                    failed += 1;
                    continue;
                }
                let to = free_name(&dest, name, !moving && same);
                let ok = if moving {
                    fs::rename(s, &to).is_ok() || fm.moveItemAtURL_toURL_error(&file_url(s), &file_url(&to)).is_ok()
                } else {
                    fm.copyItemAtURL_toURL_error(&file_url(s), &file_url(&to)).is_ok()
                };
                if ok { made.push(to) } else { failed += 1 }
            }
            dispatch2::DispatchQueue::main().exec_async(move || {
                let Some(u) = ui() else { return };
                m(|s| s.note = (failed > 0).then(|| format!("{failed} item{} couldn't be {}", if failed == 1 { "" } else { "s" }, if moving { "moved" } else { "copied" })));
                load(&u, made, false);
            });
        });
}

fn paste(u: &Ui) {
    let srcs = read_paths(&NSPasteboard::generalPasteboard());
    if srcs.is_empty() {
        return;
    }
    let (moving, dest) = m(|s| {
        let moving = !s.cut.is_empty() && s.cut == srcs;
        s.cut.clear();
        (moving, s.cwd.clone())
    });
    transfer(u, srcs, dest, moving);
}

fn trash(u: &Rc<Ui>) {
    let paths = target_paths(u);
    if paths.is_empty() {
        return;
    }
    let fm = NSFileManager::defaultManager();
    let failed = paths.iter().filter(|p| fm.trashItemAtURL_resultingItemURL_error(&file_url(p), None).is_err()).count();
    m(|s| s.note = (failed > 0).then(|| format!("{failed} item{} couldn't be moved to the Trash", if failed == 1 { "" } else { "s" })));
    load(u, Vec::new(), false);
}

fn new_folder(u: &Rc<Ui>) {
    let cwd = m(|s| s.cwd.clone());
    let base = if cwd == "/" { String::new() } else { cwd };
    let mut p = format!("{base}/untitled folder");
    let mut n = 2;
    while Path::new(&p).exists() {
        p = format!("{base}/untitled folder {n}");
        n += 1;
    }
    match fs::create_dir(&p) {
        Ok(()) => {
            load(u, vec![p], false);
            later(60, || {
                if let Some(u) = ui() {
                    begin_rename(&u);
                }
            });
        }
        Err(e) => note(u, Some(format!("Can't create a folder here: {e}"))),
    }
}

fn begin_rename(u: &Ui) {
    let rows = target_rows(u);
    let [row] = rows[..] else { return };
    let Some(e) = m(|s| s.rows.get(row).cloned()) else { return };
    pick_rows(u, &[row], true);
    let tf = if grid_mode() {
        grid::name_field(u, row)
    } else {
        u.table
            .viewAtColumn_row_makeIfNecessary(0, row as NSInteger, true)
            .and_then(|v| v.downcast::<NSTableCellView>().ok())
            .and_then(|c| unsafe { c.textField() })
    };
    let Some(tf) = tf else { return };
    m(|s| s.renaming = Some(e.path.clone()));
    tf.setEditable(true);
    if grid_mode() {
        grid::restyle(u);
    }
    if !u.window.makeFirstResponder(Some(&tf)) {
        tf.setEditable(false);
        m(|s| s.renaming = None);
        if grid_mode() {
            grid::reload(u);
        }
        return;
    }
    let stem = if e.dir { e.name.len() } else { e.name.rfind('.').filter(|&d| d > 0).unwrap_or(e.name.len()) };
    if let Some(ed) = tf.currentEditor() {
        ed.setSelectedRange(NSRange::new(0, e.name[..stem].encode_utf16().count()));
    }
}

fn end_rename(tf: &NSTextField, commit: bool) {
    let Some(old) = m(|s| s.renaming.take()) else { return };
    tf.setEditable(false);
    let old_name = old.rsplit('/').next().unwrap_or("").to_owned();
    let new = tf.stringValue().to_string();
    let new = new.trim();
    let mut keep = old.clone();
    let mut msg = None;
    if commit && !new.is_empty() && new != old_name && !new.contains('/') {
        let to = format!("{}/{new}", parent_of(&old).trim_end_matches('/'));
        if Path::new(&to).exists() && to.to_lowercase() != old.to_lowercase() {
            msg = Some(format!("The name “{new}” is already taken"));
        } else if let Err(e) = fs::rename(&old, &to) {
            msg = Some(format!("Couldn't rename: {e}"));
        } else {
            RENAMED.with(|r| *r.borrow_mut() = Some((old.clone(), to.clone())));
            keep = to;
        }
    }
    tf.setStringValue(&ns(keep.rsplit('/').next().unwrap_or("")));
    later(0, move || {
        let Some(u) = ui() else { return };
        focus(&u);
        m(|s| s.note = msg);
        load(&u, vec![keep], false);
    });
}

// ---------- quick look ----------

fn ql_panel(create: bool) -> Option<Retained<AnyObject>> {
    let cls = AnyClass::get(c"QLPreviewPanel")?;
    if !create {
        let exists: bool = unsafe { msg_send![cls, sharedPreviewPanelExists] };
        if !exists {
            return None;
        }
    }
    unsafe { msg_send![cls, sharedPreviewPanel] }
}

fn ql_visible() -> Option<Retained<AnyObject>> {
    let p = ql_panel(false)?;
    let vis: bool = unsafe { msg_send![&*p, isVisible] };
    vis.then_some(p)
}

fn toggle_ql(u: &Ui) {
    if let Some(p) = ql_visible() {
        let _: () = unsafe { msg_send![&*p, orderOut: std::ptr::null::<AnyObject>()] };
        return;
    }
    let paths = target_paths(u);
    if paths.is_empty() {
        return;
    }
    QL.with(|q| *q.borrow_mut() = paths);
    if let Some(p) = ql_panel(true) {
        let _: () = unsafe { msg_send![&*p, makeKeyAndOrderFront: std::ptr::null::<AnyObject>()] };
    }
}

fn refresh_ql(u: &Ui) {
    let Some(p) = ql_visible() else { return };
    let paths = selected_paths(u);
    if paths.is_empty() {
        return;
    }
    let changed = QL.with(|q| {
        let mut q = q.borrow_mut();
        let ch = *q != paths;
        *q = paths;
        ch
    });
    if changed {
        let _: () = unsafe { msg_send![&*p, reloadData] };
    }
}

fn close_ql() {
    if let Some(p) = ql_visible() {
        let _: () = unsafe { msg_send![&*p, orderOut: std::ptr::null::<AnyObject>()] };
    }
}

// ---------- cells ----------

const TAG_VIEW: NSInteger = 77;
const ICON_VIEW: NSInteger = 78;

// `icon` is the size of the picture on the left (0 for none), `x` where the text starts
fn make_cell(tv: &NSTableView, id: &str, icon: f64, x: f64, h: f64, mtm: MainThreadMarker) -> (Retained<NSTableCellView>, bool) {
    let ident = ns(id);
    if let Some(c) = unsafe { tv.makeViewWithIdentifier_owner(&ident, None) }.and_then(|v| v.downcast::<NSTableCellView>().ok()) {
        return (c, false);
    }
    let c = NSTableCellView::initWithFrame(NSTableCellView::alloc(mtm), NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(200.0, h)));
    c.setIdentifier(Some(&ident));
    let tf = NSTextField::labelWithString(&ns(""), mtm);
    tf.setFrame(NSRect::new(NSPoint::new(x, ((h - 17.0) / 2.0).floor()), NSSize::new(200.0 - x - 2.0, 17.0)));
    tf.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewMinYMargin | NSAutoresizingMaskOptions::ViewMaxYMargin);
    tf.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
    if icon > 0.0 {
        let iv = NSImageView::initWithFrame(NSImageView::alloc(mtm), NSRect::new(NSPoint::new(2.0, ((h - icon) / 2.0).floor()), NSSize::new(icon, icon)));
        iv.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinYMargin | NSAutoresizingMaskOptions::ViewMaxYMargin);
        iv.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
        iv.setTag(ICON_VIEW);
        c.addSubview(&iv);
        // the sidebar tiles stay out of the cell's image outlet on purpose: a source list
        // greys out whatever is plugged in there the moment the window loses focus
        if id != "place" {
            unsafe { c.setImageView(Some(&iv)) };
        }
    }
    c.addSubview(&tf);
    unsafe { c.setTextField(Some(&tf)) };
    (c, true)
}

fn side_cell(tv: &NSTableView, row: usize, mtm: MainThreadMarker) -> Option<Retained<NSView>> {
    let p = m(|s| s.places.get(row).cloned())?;
    let (c, fresh) = if p.header { make_cell(tv, "head", 0.0, 2.0, SIDE_ROW, mtm) } else { make_cell(tv, "place", 20.0, 30.0, SIDE_ROW, mtm) };
    let tf = unsafe { c.textField() }?;
    tf.setStringValue(&ns(&p.label));
    if p.header {
        if fresh {
            tf.setFont(Some(&NSFont::systemFontOfSize_weight(11.0, unsafe { NSFontWeightSemibold })));
            tf.setTextColor(Some(&NSColor::tertiaryLabelColor()));
        }
    } else if let Some(iv) = c.viewWithTag(ICON_VIEW).and_then(|v| v.downcast::<NSImageView>().ok()) {
        iv.setImage(chip(p.symbol, p.hue).as_deref());
    }
    Some(Retained::into_super(c))
}

fn file_cell(tv: &NSTableView, col: &NSTableColumn, row: usize, mtm: MainThreadMarker) -> Option<Retained<NSView>> {
    let u = ui()?;
    let which = u.cols.iter().position(|c| std::ptr::eq(&**c, col))?;
    let (e, cut, searching, hue) = m(|s| {
        s.rows.get(row).map(|e| (e.clone(), s.cut.contains(&e.path), !s.query.trim().is_empty(), if e.dir { hue_in(&s.places, &e.path) } else { BLUE }))
    })?;
    // one set of cells per icon size, so nothing has to be re-laid out on the way past
    let (pt, rh) = (icon_pt(), row_h());
    let id = format!("{}{pt}", ["name", "date", "size", "kind"][which]);
    let (c, fresh) = if which == 0 { make_cell(tv, &id, pt, pt + 8.0, rh, mtm) } else { make_cell(tv, &id, 0.0, 2.0, rh, mtm) };
    let tf = unsafe { c.textField() }?;
    if fresh {
        match which {
            0 => {
                unsafe { tf.setDelegate(Some(ProtocolObject::from_ref(&*u.ctl))) };
                let tags = NSImageView::initWithFrame(NSImageView::alloc(mtm), NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(8.0, 8.0)));
                tags.setTag(TAG_VIEW);
                tags.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinXMargin | NSAutoresizingMaskOptions::ViewMinYMargin | NSAutoresizingMaskOptions::ViewMaxYMargin);
                tags.setImageScaling(NSImageScaling::ScaleNone);
                c.addSubview(&tags);
            }
            _ => {
                tf.setTextColor(Some(&NSColor::secondaryLabelColor()));
                // digits all the same width, so a column of sizes or times lines up
                let font = if which == 3 { NSFont::systemFontOfSize(12.0) } else { NSFont::monospacedDigitSystemFontOfSize_weight(12.0, unsafe { NSFontWeightRegular }) };
                tf.setFont(Some(&font));
                if which == 2 {
                    tf.setAlignment(NSTextAlignment::Right);
                }
                if which == 3 {
                    tf.setLineBreakMode(NSLineBreakMode::ByTruncatingMiddle);
                }
            }
        }
    }
    let text = match which {
        0 => {
            if let Some(iv) = unsafe { c.imageView() } {
                iv.setImage(Some(&icon_at(&e, hue, icon_pt())));
            }
            tf.setEditable(false);
            // tag dots sit at the right end of the name and the name stops short of them
            let tags = tags_of(&e);
            let w = c.frame().size.width;
            let dw = if tags.is_empty() { 0.0 } else { tags.len() as f64 * 11.0 - 3.0 };
            tf.setFrame(NSRect::new(
                NSPoint::new(pt + 8.0, ((rh - 17.0) / 2.0).floor()),
                NSSize::new((w - pt - 8.0 - if dw > 0.0 { dw + 12.0 } else { 2.0 }).max(20.0), 17.0),
            ));
            if let Some(tv) = c.viewWithTag(TAG_VIEW).and_then(|v| v.downcast::<NSImageView>().ok()) {
                tv.setHidden(dw == 0.0);
                if dw > 0.0 {
                    tv.setFrame(NSRect::new(NSPoint::new(w - dw - 6.0, ((rh - 8.0) / 2.0).floor()), NSSize::new(dw, 8.0)));
                    tv.setImage(dots(&tags).as_deref());
                }
            }
            e.name.clone()
        }
        1 => u.dates.stringFromDate(&NSDate::dateWithTimeIntervalSince1970(e.mtime as f64)).to_string(),
        2 if e.dir => "--".to_owned(),
        2 if e.size == 0 => "Zero bytes".to_owned(),
        2 => NSByteCountFormatter::stringFromByteCount_countStyle(e.size as i64, NSByteCountFormatterCountStyle::File).to_string(),
        _ if searching && !e.place.is_empty() => e.place.clone(),
        _ => kind_of(&e.path, &e.name, e.dir),
    };
    tf.setStringValue(&ns(&text));
    c.setAlphaValue(if cut || e.hidden { 0.5 } else { 1.0 });
    Some(Retained::into_super(c))
}

// ---------- drag and drop ----------

// the folder a row stands for, if dropping onto it means "put it in there"
fn folder_at(row: usize) -> Option<String> {
    m(|s| s.rows.get(row).filter(|e| e.dir && !e.name.ends_with(".app")).map(|e| e.path.clone()))
}

// what a drop would do: which files, where to, and whether they move or get copied
fn plan_drop(info: &ProtocolObject<dyn NSDraggingInfo>, folder: Option<String>) -> Option<(Vec<String>, String, bool)> {
    let srcs = read_paths(&info.draggingPasteboard());
    if srcs.is_empty() {
        return None;
    }
    let dest = folder.unwrap_or_else(|| m(|s| s.cwd.clone()));
    if srcs.iter().any(|s| *s == dest || dest.starts_with(&format!("{s}/"))) {
        return None;
    }
    let copy_only = info.draggingSourceOperationMask() == NSDragOperation::Copy;
    let same_place = srcs.iter().all(|s| parent_of(s) == dest);
    if same_place && !copy_only {
        return None;
    }
    let dev = |p: &str| fs::metadata(p).map(|m| m.dev()).ok();
    let moving = !copy_only && dev(&srcs[0]) == dev(&dest);
    Some((srcs, dest, moving))
}

fn drop_plan(tv: &NSTableView, info: &ProtocolObject<dyn NSDraggingInfo>, row: NSInteger, op: NSTableViewDropOperation) -> Option<(Vec<String>, String, bool)> {
    let folder = if op == NSTableViewDropOperation::On && row >= 0 { folder_at(row as usize) } else { None };
    if folder.is_none() {
        tv.setDropRow_dropOperation(-1, NSTableViewDropOperation::On);
    }
    plan_drop(info, folder)
}

fn drag_writer(tv: &NSTableView, row: NSInteger) -> Option<Retained<ProtocolObject<dyn NSPasteboardWriting>>> {
    if !is_files(tv) {
        return None;
    }
    let p = m(|s| s.rows.get(row as usize).map(|e| e.path.clone()))?;
    Some(ProtocolObject::from_retained(file_url(&p)))
}

fn accept_drop(tv: &NSTableView, info: &ProtocolObject<dyn NSDraggingInfo>, row: NSInteger, op: NSTableViewDropOperation) -> bool {
    let (true, Some(u)) = (is_files(tv), ui()) else { return false };
    match drop_plan(tv, info, row, op) {
        Some((srcs, dest, moving)) => {
            transfer(&u, srcs, dest, moving);
            true
        }
        None => false,
    }
}

// the selection wears the colour of the place you're in. a run of picked rows reads
// as one block, so where a neighbour carries it on the rounded end is pushed out of
// sight instead of drawn
fn paint_selection(row: &NSTableRowView) {
    let b = row.bounds();
    let hue = ACCENT.with(|a| a.get());
    let rad = 6.0;
    let mut r = NSRect::new(NSPoint::new(b.origin.x + 10.0, b.origin.y), NSSize::new(b.size.width - 20.0, b.size.height));
    let (above, below) = (row.isPreviousRowSelected(), row.isNextRowSelected());
    let (low, high) = if row.isFlipped() { (above, below) } else { (below, above) };
    if low {
        r.origin.y -= rad;
        r.size.height += rad;
    }
    if high {
        r.size.height += rad;
    }
    if row.isEmphasized() { tone(hue, deep(hue), 1.0) } else { tone(hue, 1.0, 0.24) }.setFill();
    NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(r, rad, rad).fill();
}

fn row_view_for(tv: &NSTableView) -> Option<Retained<NSTableRowView>> {
    if !is_files(tv) {
        return None;
    }
    let mtm = MainThreadMarker::new()?;
    let id = ns("row");
    if let Some(v) = unsafe { tv.makeViewWithIdentifier_owner(&id, None) }.and_then(|v| v.downcast::<NSTableRowView>().ok()) {
        return Some(v);
    }
    let r: Retained<Row> = unsafe { msg_send![Row::alloc(mtm), initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(100.0, row_h()))] };
    r.setIdentifier(Some(&id));
    Some(Retained::into_super(r))
}

fn set_zoom(u: &Ui, z: usize) {
    let z = z.min(2);
    if ZOOM.with(|c| c.replace(z)) == z {
        return;
    }
    if REMEMBER.with(|r| r.get()) {
        objc2_foundation::NSUserDefaults::standardUserDefaults().setInteger_forKey(z as NSInteger, &ns("LiftoffFilesZoom"));
    }
    let keep = selected_paths(u);
    u.table.setRowHeight(row_h());
    u.table.reloadData();
    select_paths(u, &keep, true);
    arrive(u, 0.0);
}

// icons or list. the selection comes along
fn set_mode(u: &Ui, mode: u8) {
    let mode = mode.min(1);
    let keep = selected_paths(u);
    grid::peek_close();
    if MODE.with(|c| c.replace(mode)) == mode {
        u.mode_seg.setSelectedSegment(if mode == 1 { 0 } else { 1 });
        return;
    }
    if REMEMBER.with(|r| r.get()) {
        objc2_foundation::NSUserDefaults::standardUserDefaults().setInteger_forKey(mode as NSInteger + 1, &ns("LiftoffFilesMode"));
    }
    u.mode_seg.setSelectedSegment(if mode == 1 { 0 } else { 1 });
    u.list_scroll.setHidden(mode == 1);
    u.grid_scroll.setHidden(mode == 0);
    m(rebuild);
    reload_view(u);
    update_empty(u);
    select_paths(u, &keep, true);
    focus(u);
    update_status(u);
    arrive(u, 0.0);
}

fn sort_by(u: &Ui, col: u8) {
    let (cur, asc) = m(|s| s.sort);
    let asc = if cur == col { !asc } else { col == 0 || col == 3 };
    let key = ["name", "date", "size", "kind"][col.min(3) as usize];
    // the table owns the sort, so its column header and the icon view never disagree
    u.table.setSortDescriptors(&NSArray::from_retained_slice(&[NSSortDescriptor::sortDescriptorWithKey_ascending(Some(&ns(key)), asc)]));
}

fn escape(u: &Ui) {
    close_ql();
    let had_cut = m(|s| !std::mem::take(&mut s.cut).is_empty());
    if had_cut {
        let keep = selected_paths(u);
        reload_view(u);
        select_paths(u, &keep, false);
    }
    note(u, None);
}

// ticks in the menus: which sort, which view, grouped or not
fn validate_item(it: &NSMenuItem) -> bool {
    let Some(action) = it.action() else { return true };
    let tick = |on: bool| it.setState(if on { objc2_app_kit::NSControlStateValueOn } else { objc2_app_kit::NSControlStateValueOff });
    match action.name().to_bytes() {
        b"lfSort:" => tick(m(|s| s.sort.0 as NSInteger) == it.tag()),
        b"lfMode:" => tick(MODE.with(|m| m.get()) as NSInteger == it.tag()),
        b"lfGroup:" => {
            tick(GROUP.with(|g| g.get()));
            return grid_mode();
        }
        _ => {}
    }
    true
}

fn sort_items(mtm: MainThreadMarker, menu: &NSMenu) {
    for (i, title) in ["Name", "Date Modified", "Size", "Kind"].iter().enumerate() {
        let it = item(mtm, title, Some(sel!(lfSort:)), "", NSEventModifierFlags::empty());
        it.setTag(i as NSInteger);
        menu.addItem(&it);
    }
    menu.addItem(&NSMenuItem::separatorItem(mtm));
    menu.addItem(&item(mtm, "Group by Kind", Some(sel!(lfGroup:)), "0", NSEventModifierFlags::Command | NSEventModifierFlags::Control));
}

// the row of colour dots in the right click menu, same control finder uses for it.
// the files are pinned down when the menu opens, not when the dot is clicked.
fn tags_item(u: &Ui, mtm: MainThreadMarker) -> Retained<NSMenuItem> {
    let picked = target_ents(u);
    let have: Vec<Vec<u8>> = picked.iter().map(tag_colours).collect();
    let was: Vec<bool> = TAG_ORDER.iter().map(|n| !have.is_empty() && have.iter().all(|t| t.contains(n))).collect();
    let colors: Vec<Retained<NSColor>> = TAG_ORDER
        .iter()
        .map(|&n| {
            let (r, g, b) = TAG_RGB[n as usize];
            NSColor::colorWithSRGBRed_green_blue_alpha(r, g, b, 1.0)
        })
        .collect();
    let titles: Vec<Retained<NSString>> = TAG_ORDER.iter().map(|&n| ns(&tag_name(n))).collect();
    let armed = Rc::new(Cell::new(false));
    let (armed2, was2) = (armed.clone(), was.clone());
    let changed = block2::RcBlock::new(move |menu: NonNull<NSMenu>| {
        if !armed2.get() {
            return;
        }
        let Some(u) = ui() else { return };
        let items = unsafe { menu.as_ref() }.itemArray();
        for (i, &n) in TAG_ORDER.iter().enumerate().take(items.count()) {
            if (items.objectAtIndex(i).state() == objc2_app_kit::NSControlStateValueOn) != was2[i] {
                armed2.set(false);
                toggle_tag(&u, n, &picked);
                return;
            }
        }
    });
    let pal = NSMenu::paletteMenuWithColors_titles_selectionHandler(
        &NSArray::from_retained_slice(&colors),
        &NSArray::from_retained_slice(&titles),
        Some(&changed),
        mtm,
    );
    pal.setSelectionMode(objc2_app_kit::NSMenuSelectionMode::SelectAny);
    let items = pal.itemArray();
    for (i, on) in was.iter().enumerate().take(items.count()) {
        if *on {
            items.objectAtIndex(i).setState(objc2_app_kit::NSControlStateValueOn);
        }
    }
    armed.set(true);
    let holder = item(mtm, "Tags", None, "", NSEventModifierFlags::empty());
    holder.setSubmenu(Some(&pal));
    holder
}

// a folder you're dragging onto lights up in the place colour, not in graphite
fn paint_drop(row: &NSTableRowView) {
    let b = row.bounds();
    let hue = ACCENT.with(|a| a.get());
    let r = NSRect::new(NSPoint::new(b.origin.x + 11.0, b.origin.y + 1.0), NSSize::new(b.size.width - 22.0, b.size.height - 2.0));
    let shape = NSBezierPath::bezierPathWithRoundedRect_xRadius_yRadius(r, 5.0, 5.0);
    tone(hue, 1.0, 0.22).setFill();
    shape.fill();
    tone(hue, 1.0, 1.0).setStroke();
    shape.setLineWidth(2.0);
    shape.stroke();
}

fn cell_for(tv: &NSTableView, col: Option<&NSTableColumn>, row: NSInteger) -> Option<Retained<NSView>> {
    let mtm = MainThreadMarker::new()?;
    if is_files(tv) { file_cell(tv, col?, row as usize, mtm) } else { side_cell(tv, row as usize, mtm) }
}

fn edit_command(c: &NSControl, sel: Sel) -> bool {
    let Some(u) = ui() else { return false };
    let is_search = std::ptr::eq(c as *const NSControl as *const AnyObject, &*u.search as *const NSSearchField as *const AnyObject);
    let first_row = || {
        if picked(&u).is_empty() && m(|s| !s.rows.is_empty()) {
            pick_rows(&u, &[0], true);
            update_status(&u);
        }
        focus(&u);
    };
    match (sel.name().to_bytes(), is_search) {
        (b"cancelOperation:", false) => {
            if let Some(tf) = c.downcast_ref::<NSTextField>() {
                tf.abortEditing();
                end_rename(tf, false);
            }
            true
        }
        (b"insertNewline:", true) => {
            let q = u.search.stringValue().to_string();
            let q = q.trim();
            let p = if let Some(r) = q.strip_prefix('~') { format!("{}{r}", home()) } else { q.to_owned() };
            if p.starts_with('/') && Path::new(&p).is_dir() {
                go(&u, &p, true, None);
            } else if p.starts_with('/') && Path::new(&p).exists() {
                go(&u, &parent_of(&p).to_owned(), true, Some(p.clone()));
            }
            first_row();
            true
        }
        (b"moveDown:", true) => {
            first_row();
            true
        }
        _ => false,
    }

}

// ---------- keys ----------

fn on_key(e: &NSEvent) -> bool {
    let Some(u) = ui() else { return false };
    let mods = e.modifierFlags();
    let cmd = mods.contains(NSEventModifierFlags::Command);
    match (e.keyCode(), cmd) {
        (36 | 76, false) | (125, true) => open_targets(&u),
        (124, false) => {
            if target_ents(&u).first().is_some_and(|e| e.dir) {
                open_targets(&u);
            }
        }
        (123, false) => go_up(&u),
        (49, false) => toggle_ql(&u),
        (120, false) => begin_rename(&u),
        (53, false) => escape(&u),
        _ => return false,
    }
    true
}

define_class!(
    #[unsafe(super(NSTableRowView))]
    #[thread_kind = MainThreadOnly]
    #[name = "LiftoffFilesRow"]
    struct Row;

    impl Row {
        #[unsafe(method(drawSelectionInRect:))]
        fn draw_selection(&self, _dirty: NSRect) {
            paint_selection(self);
        }

        #[unsafe(method(drawDraggingDestinationFeedbackInRect:))]
        fn draw_drop(&self, _dirty: NSRect) {
            paint_drop(self);
        }
    }
);

// sits on top of the list and lets every click and drop fall straight through it
define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "LiftoffGhostView"]
    struct Ghost;

    impl Ghost {
        #[unsafe(method_id(hitTest:))]
        fn hit(&self, _p: NSPoint) -> Option<Retained<NSView>> {
            None
        }
    }
);

define_class!(
    #[unsafe(super(NSTableView))]
    #[thread_kind = MainThreadOnly]
    #[name = "LiftoffFilesTable"]
    struct FilesTable;

    impl FilesTable {
        #[unsafe(method(keyDown:))]
        fn key_down(&self, e: &NSEvent) {
            if !on_key(e) {
                let _: () = unsafe { msg_send![super(self), keyDown: e] };
            }
        }

        #[unsafe(method(acceptsPreviewPanelControl:))]
        fn ql_accepts(&self, _panel: &AnyObject) -> bool {
            true
        }

        #[unsafe(method(beginPreviewPanelControl:))]
        fn ql_begin(&self, panel: &AnyObject) {
            unsafe {
                let _: () = msg_send![panel, setDataSource: self];
                let _: () = msg_send![panel, setDelegate: self];
            }
        }

        #[unsafe(method(endPreviewPanelControl:))]
        fn ql_end(&self, _panel: &AnyObject) {}

        #[unsafe(method(numberOfPreviewItemsInPreviewPanel:))]
        fn ql_count(&self, _panel: &AnyObject) -> NSInteger {
            QL.with(|q| q.borrow().len() as NSInteger)
        }

        #[unsafe(method_id(previewPanel:previewItemAtIndex:))]
        fn ql_item(&self, _panel: &AnyObject, i: NSInteger) -> Option<Retained<NSURL>> {
            QL.with(|q| q.borrow().get(i as usize).map(|p| file_url(p)))
        }

        #[unsafe(method(previewPanel:handleEvent:))]
        fn ql_event(&self, _panel: &AnyObject, e: &NSEvent) -> bool {
            let arrows = e.r#type() == NSEventType::KeyDown && matches!(e.keyCode(), 125 | 126);
            if arrows {
                let _: () = unsafe { msg_send![super(self), keyDown: e] };
            }
            arrows
        }
    }
);

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "LiftoffFilesController"]
    struct Controller;

    unsafe impl NSObjectProtocol for Controller {}

    unsafe impl NSTableViewDataSource for Controller {
        #[unsafe(method(numberOfRowsInTableView:))]
        fn rows(&self, tv: &NSTableView) -> NSInteger {
            let files = is_files(tv);
            m(|s| if files { s.rows.len() } else { s.places.len() }) as NSInteger
        }

        #[unsafe(method(tableView:sortDescriptorsDidChange:))]
        fn sort_changed(&self, tv: &NSTableView, _old: &NSArray<NSSortDescriptor>) {
            let Some(d) = tv.sortDescriptors().firstObject() else { return };
            let col = match d.key().map(|k| k.to_string()).as_deref() {
                Some("date") => 1,
                Some("size") => 2,
                Some("kind") => 3,
                _ => 0,
            };
            m(|s| s.sort = (col, d.ascending()));
            if let Some(u) = ui() {
                if !refresh_rows(&u) {
                    arrive(&u, 0.0);
                }
            }
        }

        #[unsafe(method_id(tableView:pasteboardWriterForRow:))]
        fn drag_item(&self, tv: &NSTableView, row: NSInteger) -> Option<Retained<ProtocolObject<dyn NSPasteboardWriting>>> {
            drag_writer(tv, row)
        }

        #[unsafe(method(tableView:validateDrop:proposedRow:proposedDropOperation:))]
        fn validate_drop(&self, tv: &NSTableView, info: &ProtocolObject<dyn NSDraggingInfo>, row: NSInteger, op: NSTableViewDropOperation) -> NSDragOperation {
            if !is_files(tv) {
                return NSDragOperation::None;
            }
            match drop_plan(tv, info, row, op) {
                Some((_, _, true)) => NSDragOperation::Move,
                Some(_) => NSDragOperation::Copy,
                None => NSDragOperation::None,
            }
        }

        #[unsafe(method(tableView:acceptDrop:row:dropOperation:))]
        fn accept_drop(&self, tv: &NSTableView, info: &ProtocolObject<dyn NSDraggingInfo>, row: NSInteger, op: NSTableViewDropOperation) -> bool {
            accept_drop(tv, info, row, op)
        }
    }

    unsafe impl NSTableViewDelegate for Controller {
        #[unsafe(method_id(tableView:viewForTableColumn:row:))]
        fn view_for(&self, tv: &NSTableView, col: Option<&NSTableColumn>, row: NSInteger) -> Option<Retained<NSView>> {
            cell_for(tv, col, row)
        }

        #[unsafe(method_id(tableView:rowViewForRow:))]
        fn row_view(&self, tv: &NSTableView, _row: NSInteger) -> Option<Retained<NSTableRowView>> {
            row_view_for(tv)
        }

        #[unsafe(method(tableView:isGroupRow:))]
        fn is_group(&self, tv: &NSTableView, row: NSInteger) -> bool {
            !is_files(tv) && m(|s| s.places.get(row as usize).is_some_and(|p| p.header))
        }

        #[unsafe(method(tableView:shouldSelectRow:))]
        fn should_select(&self, tv: &NSTableView, _row: NSInteger) -> bool {
            is_files(tv)
        }

        #[unsafe(method(tableViewColumnDidResize:))]
        fn column_resized(&self, _n: &NSNotification) {
            let Some(u) = ui() else { return };
            // only a hand drag on a header counts. the table also resizes columns by itself
            let dragging = u.table.headerView().is_some_and(|h| h.resizedColumn() >= 0);
            if dragging && !FITTING.with(|f| f.get()) {
                WANT.with(|w| w.set([u.cols[1].width(), u.cols[2].width(), u.cols[3].width()]));
            }
        }

        #[unsafe(method(tableViewSelectionDidChange:))]
        fn selection_changed(&self, n: &NSNotification) {
            let Some(u) = ui() else { return };
            let files = n.object().is_some_and(|o| std::ptr::eq(&*o as *const AnyObject, &*u.table as *const NSTableView as *const AnyObject));
            if files {
                update_status(&u);
            }
        }

        #[unsafe(method_id(tableView:typeSelectStringForTableColumn:row:))]
        fn type_select(&self, tv: &NSTableView, _col: Option<&NSTableColumn>, row: NSInteger) -> Option<Retained<NSString>> {
            let name = if is_files(tv) { m(|s| s.rows.get(row as usize).map(|e| e.name.clone())) } else { None };
            name.map(|n| ns(&n))
        }
    }

    unsafe impl NSControlTextEditingDelegate for Controller {
        #[unsafe(method(controlTextDidEndEditing:))]
        fn edit_ended(&self, n: &NSNotification) {
            if let Some(tf) = n.object().and_then(|o| o.downcast::<NSTextField>().ok()) {
                end_rename(&tf, true);
            }
        }

        #[unsafe(method(control:textView:doCommandBySelector:))]
        fn edit_command(&self, c: &NSControl, _tv: &NSTextView, sel: Sel) -> bool {
            edit_command(c, sel)
        }
    }

    unsafe impl NSTextFieldDelegate for Controller {}

    unsafe impl NSSearchFieldDelegate for Controller {}

    unsafe impl NSToolbarDelegate for Controller {
        #[unsafe(method_id(toolbar:itemForItemIdentifier:willBeInsertedIntoToolbar:))]
        fn toolbar_item(&self, _t: &NSToolbar, id: &NSToolbarItemIdentifier, _insert: bool) -> Option<Retained<NSToolbarItem>> {
            TOOLBAR.with(|t| t.borrow().get(&id.to_string()).cloned())
        }

        #[unsafe(method_id(toolbarDefaultItemIdentifiers:))]
        fn toolbar_default(&self, _t: &NSToolbar) -> Retained<NSArray<NSToolbarItemIdentifier>> {
            toolbar_ids()
        }

        #[unsafe(method_id(toolbarAllowedItemIdentifiers:))]
        fn toolbar_allowed(&self, _t: &NSToolbar) -> Retained<NSArray<NSToolbarItemIdentifier>> {
            toolbar_ids()
        }
    }

    unsafe impl NSMenuDelegate for Controller {
        #[unsafe(method(menuNeedsUpdate:))]
        fn menu_update(&self, menu: &NSMenu) {
            let (Some(u), Some(mtm)) = (ui(), MainThreadMarker::new()) else { return };
            menu.removeAllItems();
            let on_item = if grid_mode() { grid::on_item() } else { u.table.clickedRow() >= 0 };
            let add = |title: &str, action: Sel| {
                let it = item(mtm, title, Some(action), "", NSEventModifierFlags::empty());
                unsafe { it.setTarget(Some(&u.ctl)) };
                menu.addItem(&it);
            };
            if on_item {
                add("Open", sel!(lfOpen:));
                add("Quick Look", sel!(lfQuickLook:));
                menu.addItem(&NSMenuItem::separatorItem(mtm));
                add("Rename", sel!(lfRename:));
                add("Duplicate", sel!(lfDuplicate:));
                add("Copy", sel!(copy:));
                add("Cut", sel!(cut:));
                add("Copy Path", sel!(lfCopyPath:));
                menu.addItem(&NSMenuItem::separatorItem(mtm));
                add("Move to Trash", sel!(lfTrash:));
                menu.addItem(&NSMenuItem::separatorItem(mtm));
                menu.addItem(&tags_item(&u, mtm));
                menu.addItem(&NSMenuItem::separatorItem(mtm));
            }
            add("New Folder", sel!(lfNewFolder:));
            add("Paste", sel!(paste:));
            menu.addItem(&NSMenuItem::separatorItem(mtm));
            add("Open Terminal Here", sel!(lfTerminal:));
            add("Show in Finder", sel!(lfShowInFinder:));
        }
    }

    unsafe impl NSMenuItemValidation for Controller {
        #[unsafe(method(validateMenuItem:))]
        fn validate(&self, it: &NSMenuItem) -> bool {
            validate_item(it)
        }
    }

    unsafe impl NSWindowDelegate for Controller {
        #[unsafe(method(windowWillClose:))]
        fn will_close(&self, _n: &NSNotification) {
            close_ql();
            let Some(mtm) = MainThreadMarker::new() else { return };
            let app = NSApplication::sharedApplication(mtm);
            if let Some(u) = ui() {
                app.setMainMenu(Some(&u.edit_menu));
            }
            app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
            if !crate::view::is_visible() && !crate::search::is_visible() {
                app.hide(None);
            }
        }

        #[unsafe(method(windowDidBecomeMain:))]
        fn became_main(&self, _n: &NSNotification) {
            let (Some(u), Some(mtm)) = (ui(), MainThreadMarker::new()) else { return };
            NSApplication::sharedApplication(mtm).setMainMenu(Some(&u.menu));
        }

        #[unsafe(method(windowDidResignMain:))]
        fn resigned_main(&self, _n: &NSNotification) {
            let (Some(u), Some(mtm)) = (ui(), MainThreadMarker::new()) else { return };
            NSApplication::sharedApplication(mtm).setMainMenu(Some(&u.edit_menu));
        }

        #[unsafe(method(windowDidBecomeKey:))]
        fn became_key(&self, _n: &NSNotification) {
            check_changed();
            if let Some(u) = ui() {
                grid::restyle(&u);
            }
        }

        #[unsafe(method(windowDidResignKey:))]
        fn resigned_key(&self, _n: &NSNotification) {
            if let Some(u) = ui() {
                grid::restyle(&u);
            }
            grid::peek_close();
        }
    }

    impl Controller {
        #[unsafe(method(lfOpen:))]
        fn a_open(&self, _s: Option<&AnyObject>) {
            if let Some(u) = ui() {
                open_targets(&u);
            }
        }
        #[unsafe(method(lfDouble:))]
        fn a_double(&self, _s: Option<&AnyObject>) {
            if let Some(u) = ui() {
                if u.table.clickedRow() >= 0 {
                    open_targets(&u);
                }
            }
        }
        #[unsafe(method(lfSide:))]
        fn a_side(&self, _s: Option<&AnyObject>) {
            let Some(u) = ui() else { return };
            let r = u.side.clickedRow();
            if r < 0 {
                return;
            }
            if let Some(p) = m(|s| s.places.get(r as usize).filter(|p| !p.header).map(|p| p.path.clone())) {
                go(&u, &p, true, None);
                focus(&u);
            }
        }
        #[unsafe(method(lfNav:))]
        fn a_nav(&self, _s: Option<&AnyObject>) {
            let Some(u) = ui() else { return };
            if u.nav.selectedSegment() == 0 { go_back(&u) } else { go_fwd(&u) }
        }
        #[unsafe(method(lfPath:))]
        fn a_path(&self, _s: Option<&AnyObject>) {
            let Some(u) = ui() else { return };
            if let Some(p) = u.path.clickedPathItem().and_then(|i| i.URL()).and_then(|x| x.path()) {
                go(&u, &p.to_string(), true, None);
            }
        }
        #[unsafe(method(lfSearch:))]
        fn a_search(&self, _s: Option<&AnyObject>) {
            let Some(u) = ui() else { return };
            let q = u.search.stringValue().to_string();
            if m(|s| std::mem::replace(&mut s.query, q.clone()) == q) {
                return;
            }
            // while typing, things that stop matching fade out and the rest close up
            let moved = update_rows(&u);
            update_empty(&u);
            pick_rows(&u, &[], false);
            scroll_top(&u);
            if !moved {
                arrive(&u, 0.0);
            }
            chrome(&u);
            update_status(&u);
        }
        #[unsafe(method(lfBack:))]
        fn a_back(&self, _s: Option<&AnyObject>) {
            if let Some(u) = ui() {
                go_back(&u);
            }
        }
        #[unsafe(method(lfForward:))]
        fn a_fwd(&self, _s: Option<&AnyObject>) {
            if let Some(u) = ui() {
                go_fwd(&u);
            }
        }
        #[unsafe(method(lfUp:))]
        fn a_up(&self, _s: Option<&AnyObject>) {
            if let Some(u) = ui() {
                go_up(&u);
            }
        }
        #[unsafe(method(lfGoto:))]
        fn a_goto(&self, s: Option<&AnyObject>) {
            let Some(u) = ui() else { return };
            let tag = s.and_then(|s| s.downcast_ref::<NSMenuItem>()).map_or(0, |i| i.tag());
            let h = home();
            let p = match tag {
                1 => format!("{h}/Desktop"),
                2 => format!("{h}/Documents"),
                3 => format!("{h}/Downloads"),
                4 => "/Applications".to_owned(),
                _ => h,
            };
            go(&u, &p, true, None);
        }
        #[unsafe(method(lfRename:))]
        fn a_rename(&self, _s: Option<&AnyObject>) {
            if let Some(u) = ui() {
                begin_rename(&u);
            }
        }
        #[unsafe(method(lfNewFolder:))]
        fn a_new_folder(&self, _s: Option<&AnyObject>) {
            if let Some(u) = ui() {
                new_folder(&u);
            }
        }
        #[unsafe(method(lfDuplicate:))]
        fn a_duplicate(&self, _s: Option<&AnyObject>) {
            let Some(u) = ui() else { return };
            let srcs = target_paths(&u);
            if !srcs.is_empty() {
                let dest = m(|s| s.cwd.clone());
                transfer(&u, srcs, dest, false);
            }
        }
        #[unsafe(method(lfTrash:))]
        fn a_trash(&self, _s: Option<&AnyObject>) {
            if let Some(u) = ui() {
                trash(&u);
            }
        }
        #[unsafe(method(lfQuickLook:))]
        fn a_quicklook(&self, _s: Option<&AnyObject>) {
            if let Some(u) = ui() {
                toggle_ql(&u);
            }
        }
        #[unsafe(method(lfToggleHidden:))]
        fn a_hidden(&self, _s: Option<&AnyObject>) {
            let Some(u) = ui() else { return };
            m(|s| s.hidden = !s.hidden);
            refresh_rows(&u);
        }
        #[unsafe(method(lfReload:))]
        fn a_reload(&self, _s: Option<&AnyObject>) {
            if let Some(u) = ui() {
                reload(&u);
            }
        }
        #[unsafe(method(lfCopyPath:))]
        fn a_copy_path(&self, _s: Option<&AnyObject>) {
            let Some(u) = ui() else { return };
            let p = target_paths(&u);
            copy_text(&if p.is_empty() { m(|s| s.cwd.clone()) } else { p.join("\n") });
        }
        #[unsafe(method(lfTerminal:))]
        fn a_terminal(&self, _s: Option<&AnyObject>) {
            let cwd = m(|s| s.cwd.clone());
            let _ = std::process::Command::new("/usr/bin/open").args(["-a", "Terminal", &cwd]).spawn();
        }
        #[unsafe(method(lfShowInFinder:))]
        fn a_finder(&self, _s: Option<&AnyObject>) {
            let Some(u) = ui() else { return };
            let mut p = target_paths(&u);
            if p.is_empty() {
                p.push(m(|s| s.cwd.clone()));
            }
            let urls: Vec<Retained<NSURL>> = p.iter().map(|p| file_url(p)).collect();
            NSWorkspace::sharedWorkspace().activateFileViewerSelectingURLs(&NSArray::from_retained_slice(&urls));
        }
        #[unsafe(method(lfZoom:))]
        fn a_zoom(&self, s: Option<&AnyObject>) {
            let Some(u) = ui() else { return };
            let step = s.and_then(|s| s.downcast_ref::<NSMenuItem>()).map_or(0, |i| i.tag());
            if grid_mode() {
                let z = grid::ZOOM.with(|c| c.get()) as NSInteger + step;
                grid::set_zoom(&u, z.clamp(0, 3) as usize, REMEMBER.with(|r| r.get()));
            } else {
                let z = ZOOM.with(|c| c.get()) as NSInteger + step;
                set_zoom(&u, z.clamp(0, 2) as usize);
            }
        }
        #[unsafe(method(lfMode:))]
        fn a_mode(&self, s: Option<&AnyObject>) {
            let Some(u) = ui() else { return };
            let mode = s.and_then(|s| s.downcast_ref::<NSMenuItem>()).map_or(1, |i| i.tag());
            set_mode(&u, mode as u8);
        }
        #[unsafe(method(lfViewSeg:))]
        fn a_view_seg(&self, _s: Option<&AnyObject>) {
            let Some(u) = ui() else { return };
            set_mode(&u, if u.mode_seg.selectedSegment() == 0 { 1 } else { 0 });
        }
        #[unsafe(method(lfSort:))]
        fn a_sort(&self, s: Option<&AnyObject>) {
            let Some(u) = ui() else { return };
            let col = s.and_then(|s| s.downcast_ref::<NSMenuItem>()).map_or(0, |i| i.tag());
            sort_by(&u, col.clamp(0, 3) as u8);
        }
        #[unsafe(method(lfGroup:))]
        fn a_group(&self, _s: Option<&AnyObject>) {
            let Some(u) = ui() else { return };
            let on = !GROUP.with(|g| g.get());
            GROUP.with(|g| g.set(on));
            if REMEMBER.with(|r| r.get()) {
                objc2_foundation::NSUserDefaults::standardUserDefaults().setBool_forKey(!on, &ns("LiftoffFilesFlat"));
            }
            if !refresh_rows(&u) {
                arrive(&u, 0.0);
            }
        }
        #[unsafe(method(lfFind:))]
        fn a_find(&self, _s: Option<&AnyObject>) {
            if let Some(u) = ui() {
                u.window.makeFirstResponder(Some(&u.search));
            }
        }
        #[unsafe(method(copy:))]
        fn a_copy(&self, _s: Option<&AnyObject>) {
            if let Some(u) = ui() {
                copy_targets(&u, false);
            }
        }
        #[unsafe(method(cut:))]
        fn a_cut(&self, _s: Option<&AnyObject>) {
            if let Some(u) = ui() {
                copy_targets(&u, true);
            }
        }
        #[unsafe(method(paste:))]
        fn a_paste(&self, _s: Option<&AnyObject>) {
            if let Some(u) = ui() {
                paste(&u);
            }
        }
    }
);

// ---------- menus, toolbar ----------

fn item(mtm: MainThreadMarker, title: &str, action: Option<Sel>, key: &str, mods: NSEventModifierFlags) -> Retained<NSMenuItem> {
    let it = unsafe { NSMenuItem::initWithTitle_action_keyEquivalent(NSMenuItem::alloc(mtm), &ns(title), action, &ns(key)) };
    if !key.is_empty() {
        it.setKeyEquivalentModifierMask(mods);
    }
    it
}

fn submenu(mtm: MainThreadMarker, bar: &NSMenu, title: &str) -> Retained<NSMenu> {
    let it = item(mtm, title, None, "", NSEventModifierFlags::empty());
    let sub = NSMenu::initWithTitle(NSMenu::alloc(mtm), &ns(title));
    it.setSubmenu(Some(&sub));
    bar.addItem(&it);
    sub
}

fn add_edit(mtm: MainThreadMarker, bar: &NSMenu, extra: bool) {
    let cmd = NSEventModifierFlags::Command;
    let e = submenu(mtm, bar, "Edit");
    e.addItem(&item(mtm, "Undo", Some(sel!(undo:)), "z", cmd));
    e.addItem(&item(mtm, "Redo", Some(sel!(redo:)), "z", cmd | NSEventModifierFlags::Shift));
    e.addItem(&NSMenuItem::separatorItem(mtm));
    e.addItem(&item(mtm, "Cut", Some(sel!(cut:)), "x", cmd));
    e.addItem(&item(mtm, "Copy", Some(sel!(copy:)), "c", cmd));
    e.addItem(&item(mtm, "Paste", Some(sel!(paste:)), "v", cmd));
    e.addItem(&item(mtm, "Select All", Some(sel!(selectAll:)), "a", cmd));
    if extra {
        e.addItem(&NSMenuItem::separatorItem(mtm));
        e.addItem(&item(mtm, "Copy Path", Some(sel!(lfCopyPath:)), "c", cmd | NSEventModifierFlags::Option));
    }
}

// a menu bar nobody sees. without an edit menu, cmd+c/v/x/a do nothing in any text
// field of a menu-bar-less app, which is why paste never worked in the search bar.
pub fn edit_menu(mtm: MainThreadMarker) -> Retained<NSMenu> {
    let bar = NSMenu::initWithTitle(NSMenu::alloc(mtm), &ns(""));
    submenu(mtm, &bar, "Liftoff");
    add_edit(mtm, &bar, false);
    bar
}

fn full_menu(mtm: MainThreadMarker) -> Retained<NSMenu> {
    let (cmd, shift, opt, ctrl) = (NSEventModifierFlags::Command, NSEventModifierFlags::Shift, NSEventModifierFlags::Option, NSEventModifierFlags::Control);
    let bar = NSMenu::initWithTitle(NSMenu::alloc(mtm), &ns(""));
    let sep = |menu: &NSMenu| menu.addItem(&NSMenuItem::separatorItem(mtm));

    let a = submenu(mtm, &bar, "Liftoff");
    a.addItem(&item(mtm, "Hide Liftoff", Some(sel!(hide:)), "h", cmd));
    a.addItem(&item(mtm, "Hide Others", Some(sel!(hideOtherApplications:)), "h", cmd | opt));
    sep(&a);
    a.addItem(&item(mtm, "Close Files", Some(sel!(performClose:)), "q", cmd));

    let f = submenu(mtm, &bar, "File");
    f.addItem(&item(mtm, "New Folder", Some(sel!(lfNewFolder:)), "n", cmd | shift));
    f.addItem(&item(mtm, "Open", Some(sel!(lfOpen:)), "o", cmd));
    f.addItem(&item(mtm, "Quick Look", Some(sel!(lfQuickLook:)), "y", cmd));
    sep(&f);
    f.addItem(&item(mtm, "Rename", Some(sel!(lfRename:)), "", cmd));
    f.addItem(&item(mtm, "Duplicate", Some(sel!(lfDuplicate:)), "d", cmd));
    f.addItem(&item(mtm, "Move to Trash", Some(sel!(lfTrash:)), "\u{8}", cmd));
    sep(&f);
    f.addItem(&item(mtm, "Close Window", Some(sel!(performClose:)), "w", cmd));

    add_edit(mtm, &bar, true);

    let v = submenu(mtm, &bar, "View");
    for (title, key, mode) in [("as Icons", "1", 1), ("as List", "2", 0)] {
        let it = item(mtm, title, Some(sel!(lfMode:)), key, cmd);
        it.setTag(mode);
        v.addItem(&it);
    }
    sep(&v);
    sort_items(mtm, &submenu(mtm, &v, "Sort By"));
    sep(&v);
    v.addItem(&item(mtm, "Show Hidden Files", Some(sel!(lfToggleHidden:)), ".", cmd | shift));
    v.addItem(&item(mtm, "Reload", Some(sel!(lfReload:)), "r", cmd));
    sep(&v);
    for (title, key, step) in [("Bigger Icons", "+", 1), ("Smaller Icons", "-", -1)] {
        let it = item(mtm, title, Some(sel!(lfZoom:)), key, cmd);
        it.setTag(step);
        v.addItem(&it);
    }
    sep(&v);
    v.addItem(&item(mtm, "Show Sidebar", Some(sel!(toggleSidebar:)), "s", cmd | ctrl));

    let g = submenu(mtm, &bar, "Go");
    g.addItem(&item(mtm, "Back", Some(sel!(lfBack:)), "[", cmd));
    g.addItem(&item(mtm, "Forward", Some(sel!(lfForward:)), "]", cmd));
    g.addItem(&item(mtm, "Enclosing Folder", Some(sel!(lfUp:)), "\u{F700}", cmd));
    sep(&g);
    for (title, key, mods, tag) in [
        ("Home", "h", cmd | shift, 0),
        ("Desktop", "d", cmd | shift, 1),
        ("Documents", "o", cmd | shift, 2),
        ("Downloads", "l", cmd | opt, 3),
        ("Applications", "a", cmd | shift, 4),
    ] {
        let it = item(mtm, title, Some(sel!(lfGoto:)), key, mods);
        it.setTag(tag);
        g.addItem(&it);
    }
    sep(&g);
    g.addItem(&item(mtm, "Find", Some(sel!(lfFind:)), "f", cmd));
    g.addItem(&item(mtm, "Open Terminal Here", Some(sel!(lfTerminal:)), "t", cmd));

    let w = submenu(mtm, &bar, "Window");
    w.addItem(&item(mtm, "Minimise", Some(sel!(performMiniaturize:)), "m", cmd));
    w.addItem(&item(mtm, "Zoom", Some(sel!(performZoom:)), "", cmd));
    bar
}

fn toolbar_ids() -> Retained<NSArray<NSToolbarItemIdentifier>> {
    let own: Vec<Retained<NSString>> = ["nav", "view", "sort", "newfolder", "quicklook", "trash", "search"].iter().map(|s| ns(s)).collect();
    unsafe {
        NSArray::from_slice(&[
            NSToolbarSidebarTrackingSeparatorItemIdentifier,
            &*own[0],
            NSToolbarFlexibleSpaceItemIdentifier,
            &*own[1],
            &*own[2],
            &*own[3],
            &*own[4],
            &*own[5],
            &*own[6],
        ])
    }
}

fn symbol(name: &str) -> Option<Retained<NSImage>> {
    NSImage::imageWithSystemSymbolName_accessibilityDescription(&ns(name), None)
}

// ---------- window ----------

fn build(mtm: MainThreadMarker, persist: bool) -> Rc<Ui> {
    m(|s| s.places = places());
    let ctl: Retained<Controller> = unsafe { msg_send![Controller::alloc(mtm), init] };
    let zero = NSPoint::new(0.0, 0.0);
    let size = NSSize::new(1060.0, 660.0);

    let table: Retained<FilesTable> = unsafe { msg_send![FilesTable::alloc(mtm), initWithFrame: NSRect::new(zero, NSSize::new(820.0, 600.0))] };
    let table: Retained<NSTableView> = Retained::into_super(table);
    FILES_PTR.with(|p| p.set(&*table));
    let specs: [(&str, &str, f64, f64, bool); 4] = [
        ("name", "Name", 340.0, 160.0, true),
        ("date", "Date Modified", 170.0, 110.0, false),
        ("size", "Size", 90.0, 60.0, false),
        ("kind", "Kind", 170.0, 80.0, true),
    ];
    let cols = specs.map(|(id, title, w, min, asc)| {
        let c = NSTableColumn::initWithIdentifier(NSTableColumn::alloc(mtm), &ns(id));
        c.setTitle(&ns(title));
        c.setWidth(w);
        c.setMinWidth(min);
        c.setSortDescriptorPrototype(Some(&NSSortDescriptor::sortDescriptorWithKey_ascending(Some(&ns(id)), asc)));
        if id == "size" {
            c.headerCell().setAlignment(NSTextAlignment::Right);
        }
        table.addTableColumn(&c);
        c
    });
    table.setStyle(NSTableViewStyle::Inset);
    table.setRowSizeStyle(NSTableViewRowSizeStyle::Custom);
    REMEMBER.with(|r| r.set(persist));
    if persist {
        // mode and icon size are stored one up, so "never set" reads as zero
        let d = objc2_foundation::NSUserDefaults::standardUserDefaults();
        ZOOM.with(|c| c.set(d.integerForKey(&ns("LiftoffFilesZoom")).clamp(0, 2) as usize));
        match d.integerForKey(&ns("LiftoffFilesMode")) {
            1 => MODE.with(|c| c.set(0)),
            _ => MODE.with(|c| c.set(1)),
        }
        let gz = d.integerForKey(&ns("LiftoffFilesGridZoom"));
        if gz > 0 {
            grid::ZOOM.with(|c| c.set((gz - 1).clamp(0, 3) as usize));
        }
        GROUP.with(|g| g.set(!d.boolForKey(&ns("LiftoffFilesFlat"))));
    }
    table.setRowHeight(row_h());
    // no stripes: they ran on down the empty half of the window as a ladder of grey bars
    table.setUsesAlternatingRowBackgroundColors(false);
    table.setAllowsMultipleSelection(true);
    table.setAllowsColumnReordering(false);
    table.setAllowsTypeSelect(true);
    table.setColumnAutoresizingStyle(NSTableViewColumnAutoresizingStyle::FirstColumnOnlyAutoresizingStyle);
    table.setDraggingSourceOperationMask_forLocal(NSDragOperation::Copy | NSDragOperation::Move, true);
    table.setDraggingSourceOperationMask_forLocal(NSDragOperation::Copy | NSDragOperation::Move | NSDragOperation::Link | NSDragOperation::Generic, false);
    table.registerForDraggedTypes(&NSArray::from_slice(&[unsafe { NSPasteboardTypeFileURL }]));
    unsafe {
        table.setDataSource(Some(ProtocolObject::from_ref(&*ctl)));
        table.setDelegate(Some(ProtocolObject::from_ref(&*ctl)));
        table.setTarget(Some(&ctl));
        table.setDoubleAction(Some(sel!(lfDouble:)));
    }
    let ctx = NSMenu::initWithTitle(NSMenu::alloc(mtm), &ns(""));
    ctx.setDelegate(Some(ProtocolObject::from_ref(&*ctl)));
    unsafe { table.setMenu(Some(&ctx)) };

    // this pane runs the full width of the window, underneath the floating sidebar, the
    // way finder's does. what has to stay readable is pinned to the safe area. the
    // backdrop and the glow are not, so there is no seam next to the sidebar any more.
    let content = NSView::initWithFrame(NSView::alloc(mtm), NSRect::new(zero, NSSize::new(840.0, size.height)));
    let stage = NSView::initWithFrame(NSView::alloc(mtm), NSRect::new(NSPoint::new(0.0, BAR), NSSize::new(840.0, size.height - BAR)));
    stage.setTranslatesAutoresizingMaskIntoConstraints(false);
    // the zooms scale what's inside past its edges for a moment. that must not spill
    // over the bottom bar
    stage.setWantsLayer(true);
    stage.setClipsToBounds(true);
    content.addSubview(&stage);
    let inside = NSRect::new(zero, NSSize::new(840.0, size.height - BAR));
    let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), inside);
    scroll.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable);
    scroll.setHasVerticalScroller(true);
    scroll.setAutohidesScrollers(true);
    scroll.setBorderType(NSBorderType::NoBorder);
    scroll.setDocumentView(Some(&table));
    stage.addSubview(&scroll);
    let (grid, grid_scroll, grid_ctl) = grid::build(mtm, inside);
    stage.addSubview(&grid_scroll);
    scroll.setHidden(grid_mode());
    grid_scroll.setHidden(!grid_mode());
    // finder just lets the last column fall off the edge when the window gets narrow. squeeze instead
    let clip = scroll.contentView();
    clip.setPostsFrameChangedNotifications(true);
    let fit = block2::RcBlock::new(|_n: std::ptr::NonNull<NSNotification>| {
        if let Some(u) = ui() {
            fit_columns(&u);
        }
    });
    let token = unsafe {
        objc2_foundation::NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
            Some(objc2_app_kit::NSViewFrameDidChangeNotification),
            Some(&clip),
            None,
            &fit,
        )
    };
    std::mem::forget(token);

    let line = NSBox::initWithFrame(NSBox::alloc(mtm), NSRect::new(NSPoint::new(0.0, BAR - 1.0), NSSize::new(840.0, 1.0)));
    line.setBoxType(NSBoxType::Separator);
    line.setTranslatesAutoresizingMaskIntoConstraints(false);
    content.addSubview(&line);
    let path = NSPathControl::initWithFrame(NSPathControl::alloc(mtm), NSRect::new(NSPoint::new(10.0, 3.0), NSSize::new(840.0 - 260.0, 22.0)));
    path.setPathStyle(NSPathStyle::Standard);
    path.setControlSize(NSControlSize::Small);
    path.setFont(Some(&NSFont::systemFontOfSize(11.0)));
    path.setTranslatesAutoresizingMaskIntoConstraints(false);
    path.setContentCompressionResistancePriority_forOrientation(200.0, objc2_app_kit::NSLayoutConstraintOrientation::Horizontal);
    unsafe {
        path.setTarget(Some(&ctl));
        path.setAction(Some(sel!(lfPath:)));
    }
    content.addSubview(&path);
    let status = NSTextField::labelWithString(&ns(""), mtm);
    status.setFrame(NSRect::new(NSPoint::new(840.0 - 250.0, 6.0), NSSize::new(238.0, 15.0)));
    status.setFont(Some(&NSFont::systemFontOfSize(11.0)));
    status.setTextColor(Some(&NSColor::secondaryLabelColor()));
    status.setAlignment(NSTextAlignment::Right);
    status.setLineBreakMode(NSLineBreakMode::ByTruncatingTail);
    status.setTranslatesAutoresizingMaskIntoConstraints(false);
    status.setContentCompressionResistancePriority_forOrientation(900.0, objc2_app_kit::NSLayoutConstraintOrientation::Horizontal);
    status.setContentHuggingPriority_forOrientation(900.0, objc2_app_kit::NSLayoutConstraintOrientation::Horizontal);
    content.addSubview(&status);


    let ghost: Retained<Ghost> = unsafe {
        msg_send![Ghost::alloc(mtm), initWithFrame: NSRect::new(NSPoint::new(240.0, BAR + ((size.height - BAR - 52.0 - 150.0) / 2.0).floor()), NSSize::new(360.0, 150.0))]
    };
    let empty: Retained<NSView> = Retained::into_super(ghost);
    empty.setTranslatesAutoresizingMaskIntoConstraints(false);
    let empty_icon = NSImageView::initWithFrame(NSImageView::alloc(mtm), NSRect::new(NSPoint::new(148.0, 80.0), NSSize::new(64.0, 64.0)));
    empty_icon.setImageScaling(NSImageScaling::ScaleProportionallyDown);
    empty.addSubview(&empty_icon);
    let caption = |y: f64, h: f64, font: &NSFont, color: &NSColor| {
        let l = NSTextField::labelWithString(&ns(""), mtm);
        l.setFrame(NSRect::new(NSPoint::new(0.0, y), NSSize::new(360.0, h)));
        l.setFont(Some(font));
        l.setTextColor(Some(color));
        l.setAlignment(NSTextAlignment::Center);
        l.setLineBreakMode(NSLineBreakMode::ByTruncatingMiddle);
        empty.addSubview(&l);
        l
    };
    let empty_title = caption(44.0, 22.0, &NSFont::systemFontOfSize_weight(16.0, unsafe { NSFontWeightSemibold }), &NSColor::secondaryLabelColor());
    let empty_hint = caption(22.0, 17.0, &NSFont::systemFontOfSize(12.0), &NSColor::tertiaryLabelColor());
    empty.setHidden(true);
    content.addSubview(&empty);

    // everything that has to clear the sidebar hangs off the safe area's left edge, so
    // it follows when the sidebar is dragged wider or tucked away
    let safe = content.safeAreaLayoutGuide();
    let (left, right, top, bottom) = (safe.leadingAnchor(), content.trailingAnchor(), content.topAnchor(), content.bottomAnchor());
    let pins = [
        stage.leadingAnchor().constraintEqualToAnchor(&left),
        stage.trailingAnchor().constraintEqualToAnchor(&right),
        stage.topAnchor().constraintEqualToAnchor(&top),
        stage.bottomAnchor().constraintEqualToAnchor_constant(&bottom, -BAR),
        line.leadingAnchor().constraintEqualToAnchor(&left),
        line.trailingAnchor().constraintEqualToAnchor(&right),
        line.heightAnchor().constraintEqualToConstant(1.0),
        line.bottomAnchor().constraintEqualToAnchor_constant(&bottom, -(BAR - 1.0)),
        path.leadingAnchor().constraintEqualToAnchor_constant(&left, 10.0),
        path.bottomAnchor().constraintEqualToAnchor_constant(&bottom, -3.0),
        path.heightAnchor().constraintEqualToConstant(22.0),
        path.trailingAnchor().constraintLessThanOrEqualToAnchor_constant(&status.leadingAnchor(), -10.0),
        status.trailingAnchor().constraintEqualToAnchor_constant(&right, -12.0),
        status.bottomAnchor().constraintEqualToAnchor_constant(&bottom, -7.0),
        empty.centerXAnchor().constraintEqualToAnchor(&safe.centerXAnchor()),
        empty.centerYAnchor().constraintEqualToAnchor_constant(&content.centerYAnchor(), 12.0),
        empty.widthAnchor().constraintEqualToConstant(360.0),
        empty.heightAnchor().constraintEqualToConstant(150.0),
    ];
    objc2_app_kit::NSLayoutConstraint::activateConstraints(&NSArray::from_retained_slice(&pins));

    let side = NSTableView::initWithFrame(NSTableView::alloc(mtm), NSRect::new(zero, NSSize::new(200.0, size.height)));
    let sc = NSTableColumn::initWithIdentifier(NSTableColumn::alloc(mtm), &ns("place"));
    sc.setWidth(180.0);
    side.addTableColumn(&sc);
    side.setHeaderView(None);
    side.setStyle(NSTableViewStyle::SourceList);
    side.setRowSizeStyle(NSTableViewRowSizeStyle::Custom);
    side.setRowHeight(SIDE_ROW);
    side.setFloatsGroupRows(false);
    side.setRefusesFirstResponder(true);
    // the system's highlight can only jump. this one sits behind the rows and moves
    side.setSelectionHighlightStyle(objc2_app_kit::NSTableViewSelectionHighlightStyle::None);
    let side_pill = NSBox::initWithFrame(NSBox::alloc(mtm), NSRect::new(NSPoint::new(10.0, 0.0), NSSize::new(180.0, SIDE_ROW - 2.0)));
    side_pill.setBoxType(NSBoxType::Custom);
    side_pill.setTitlePosition(objc2_app_kit::NSTitlePosition::NoTitle);
    side_pill.setBorderWidth(0.0);
    side_pill.setCornerRadius(8.0);
    side_pill.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable);
    side_pill.setAlphaValue(0.0);
    side.addSubview_positioned_relativeTo(&side_pill, objc2_app_kit::NSWindowOrderingMode::Below, None);
    unsafe {
        side.setDataSource(Some(ProtocolObject::from_ref(&*ctl)));
        side.setDelegate(Some(ProtocolObject::from_ref(&*ctl)));
        side.setTarget(Some(&ctl));
        side.setAction(Some(sel!(lfSide:)));
    }
    let side_scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), NSRect::new(zero, NSSize::new(200.0, size.height)));
    side_scroll.setHasVerticalScroller(true);
    side_scroll.setAutohidesScrollers(true);
    side_scroll.setBorderType(NSBorderType::NoBorder);
    side_scroll.setDrawsBackground(false);
    side_scroll.setDocumentView(Some(&side));

    let side_vc = NSViewController::new(mtm);
    side_vc.setView(&side_scroll);
    let content_vc = NSViewController::new(mtm);
    content_vc.setView(&content);
    let split = NSSplitViewController::new(mtm);
    let side_item = NSSplitViewItem::sidebarWithViewController(&side_vc);
    side_item.setMinimumThickness(170.0);
    side_item.setMaximumThickness(320.0);
    side_item.setCanCollapse(true);
    split.addSplitViewItem(&side_item);
    let content_item = NSSplitViewItem::splitViewItemWithViewController(&content_vc);
    content_item.setMinimumThickness(420.0);
    // lets the sidebar float on top of this pane instead of sitting next to it
    content_item.setAutomaticallyAdjustsSafeAreaInsets(true);
    split.addSplitViewItem(&content_item);

    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            NSRect::new(zero, size),
            NSWindowStyleMask::Titled
                | NSWindowStyleMask::Closable
                | NSWindowStyleMask::Miniaturizable
                | NSWindowStyleMask::Resizable
                | NSWindowStyleMask::FullSizeContentView,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTabbingMode(NSWindowTabbingMode::Disallowed);
    window.setMinSize(NSSize::new(640.0, 380.0));
    window.setToolbarStyle(NSWindowToolbarStyle::Unified);
    // one surface behind everything: the list, the bottom bar and whatever shows around
    // the sidebar are the same colour, so nothing meets at a line
    window.setBackgroundColor(Some(&NSColor::controlBackgroundColor()));

    let images: Vec<Retained<NSImage>> = ["chevron.left", "chevron.right"].iter().filter_map(|s| symbol(s)).collect();
    let nav = unsafe {
        NSSegmentedControl::segmentedControlWithImages_trackingMode_target_action(
            &NSArray::from_retained_slice(&images),
            NSSegmentSwitchTracking::Momentary,
            Some(&ctl),
            Some(sel!(lfNav:)),
            mtm,
        )
    };
    nav.setSegmentStyle(NSSegmentStyle::Separated);
    let nav_item = NSToolbarItem::initWithItemIdentifier(NSToolbarItem::alloc(mtm), &ns("nav"));
    nav_item.setView(Some(&nav));
    nav_item.setNavigational(true);
    nav_item.setLabel(&ns("Back/Forward"));
    let search_item = NSSearchToolbarItem::initWithItemIdentifier(NSSearchToolbarItem::alloc(mtm), &ns("search"));
    search_item.setPreferredWidthForSearchField(210.0);
    let search = search_item.searchField();
    search.setPlaceholderString(Some(&ns("Search")));
    unsafe {
        search.setDelegate(Some(ProtocolObject::from_ref(&*ctl)));
        search.setTarget(Some(&ctl));
        search.setAction(Some(sel!(lfSearch:)));
    }
    search.setSendsSearchStringImmediately(true);
    let view_images: Vec<Retained<NSImage>> = ["square.grid.2x2", "list.bullet"].iter().filter_map(|s| symbol(s)).collect();
    let mode_seg = unsafe {
        NSSegmentedControl::segmentedControlWithImages_trackingMode_target_action(
            &NSArray::from_retained_slice(&view_images),
            NSSegmentSwitchTracking::SelectOne,
            Some(&ctl),
            Some(sel!(lfViewSeg:)),
            mtm,
        )
    };
    mode_seg.setSelectedSegment(if grid_mode() { 0 } else { 1 });
    let view_item = NSToolbarItem::initWithItemIdentifier(NSToolbarItem::alloc(mtm), &ns("view"));
    view_item.setView(Some(&mode_seg));
    view_item.setLabel(&ns("View"));
    view_item.setToolTip(Some(&ns("Icons or list")));
    let sort_menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &ns("Sort"));
    sort_items(mtm, &sort_menu);
    let sort_item = NSMenuToolbarItem::initWithItemIdentifier(NSMenuToolbarItem::alloc(mtm), &ns("sort"));
    sort_item.setImage(symbol("arrow.up.arrow.down").as_deref());
    sort_item.setMenu(&sort_menu);
    sort_item.setShowsIndicator(true);
    sort_item.setLabel(&ns("Sort"));
    sort_item.setToolTip(Some(&ns("Sort and group")));
    TOOLBAR.with(|t| {
        let mut t = t.borrow_mut();
        t.insert("nav".into(), nav_item);
        t.insert("view".into(), view_item);
        t.insert("sort".into(), Retained::into_super(sort_item));
        t.insert("search".into(), Retained::into_super(search_item));
        for (id, sym, label, action) in [
            ("newfolder", "folder.badge.plus", "New Folder", sel!(lfNewFolder:)),
            ("quicklook", "eye", "Quick Look", sel!(lfQuickLook:)),
            ("trash", "trash", "Delete", sel!(lfTrash:)),
        ] {
            let it = NSToolbarItem::initWithItemIdentifier(NSToolbarItem::alloc(mtm), &ns(id));
            it.setImage(symbol(sym).as_deref());
            it.setLabel(&ns(label));
            it.setToolTip(Some(&ns(label)));
            it.setBordered(true);
            unsafe {
                it.setTarget(Some(&ctl));
                it.setAction(Some(action));
            }
            t.insert(id.into(), it);
        }
    });
    let toolbar = NSToolbar::initWithIdentifier(NSToolbar::alloc(mtm), &ns("LiftoffFiles"));
    toolbar.setDisplayMode(NSToolbarDisplayMode::IconOnly);
    toolbar.setDelegate(Some(ProtocolObject::from_ref(&*ctl)));
    window.setToolbar(Some(&toolbar));
    window.setContentViewController(Some(&split));
    window.setContentSize(size);
    window.center();
    if persist {
        window.setFrameAutosaveName(&ns("LiftoffFiles"));
    }
    window.setDelegate(Some(ProtocolObject::from_ref(&*ctl)));
    if grid_mode() {
        window.setInitialFirstResponder(Some(&grid));
    } else {
        window.setInitialFirstResponder(Some(&table));
    }

    let dates = NSDateFormatter::new();
    dates.setDateStyle(NSDateFormatterStyle::MediumStyle);
    dates.setTimeStyle(NSDateFormatterStyle::ShortStyle);
    dates.setDoesRelativeDateFormatting(true);

    let u = Rc::new(Ui {
        window,
        table,
        side,
        cols,
        path,
        status,
        search,
        nav,
        ctl,
        dates,
        menu: full_menu(mtm),
        edit_menu: edit_menu(mtm),
        empty,
        empty_icon,
        empty_title,
        empty_hint,
        side_pill,
        grid,
        grid_scroll,
        list_scroll: scroll,
        stage,
        mode_seg,
        _grid_ctl: grid_ctl,
    });
    UI.with(|x| *x.borrow_mut() = Some(u.clone()));
    u.side.reloadData();
    u.window.layoutIfNeeded();
    fit_columns(&u);
    u
}

// date, size and kind keep the width you gave them, name takes whatever is left, and
// only when the window is too narrow for that does everything squeeze
fn fit_columns(u: &Ui) {
    let Some(sv) = u.table.enclosingScrollView() else { return };
    if FITTING.with(|f| f.replace(true)) {
        return;
    }
    let pad = PAD.with(|p| {
        if p.get() < 0.0 {
            u.table.sizeToFit();
            p.set((u.table.frame().size.width - u.cols.iter().map(|c| c.width()).sum::<f64>()).max(0.0));
        }
        p.get()
    });
    let avail = sv.contentView().bounds().size.width - pad;
    let want = WANT.with(|w| w.get());
    let mins = [110.0, 60.0, 80.0];
    let (name_min, want_sum, min_sum) = (160.0, want.iter().sum::<f64>(), mins.iter().sum::<f64>());
    let mut name = avail - want_sum;
    let mut rest = want;
    if name < name_min {
        name = name_min;
        let room = (avail - name_min - min_sum).max(0.0);
        let t = if want_sum > min_sum { (room / (want_sum - min_sum)).min(1.0) } else { 0.0 };
        for i in 0..3 {
            rest[i] = (mins[i] + (want[i] - mins[i]).max(0.0) * t).floor();
        }
    }
    u.cols[0].setWidth(name.floor());
    for (c, w) in u.cols[1..].iter().zip(rest) {
        c.setWidth(w);
    }
    FITTING.with(|f| f.set(false));
}

fn check_changed() {
    let Some(u) = ui() else { return };
    let (cwd, stamp, busy) = m(|s| (s.cwd.clone(), s.stamp, s.renaming.is_some()));
    if busy || stamp == 0 {
        return;
    }
    let now = dir::stamp(&cwd);
    if now == 0 {
        go_up(&u);
    } else if now != stamp {
        reload(&u);
    }
}

fn poll() {
    later(1000, || {
        if is_open() {
            check_changed();
            poll();
        } else {
            POLLING.with(|p| p.set(false));
        }
    });
}

pub fn open(path: Option<&str>) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let (u, fresh) = match ui() {
        Some(u) => (u, false),
        None => (build(mtm, true), true),
    };
    let was_open = u.window.isVisible();
    match path {
        Some(p) if Path::new(p).is_dir() => go(&u, p, !fresh, None),
        Some(p) => go(&u, &parent_of(p).to_owned(), !fresh, Some(p.to_owned())),
        None if !was_open => {
            let cwd = m(|s| s.cwd.clone());
            go(&u, &cwd, false, None);
        }
        None => {}
    }
    let app = NSApplication::sharedApplication(mtm);
    let was_front = app.isActive() && !was_open;
    app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
    app.setMainMenu(Some(&u.menu));
    u.window.makeKeyAndOrderFront(None);
    app.activate();
    focus(&u);
    if was_front {
        // going from menu-bar-less to a normal app while already frontmost leaves the old
        // menu bar up until focus leaves and comes back. so make it leave and come back.
        let dock = objc2_app_kit::NSRunningApplication::runningApplicationsWithBundleIdentifier(&ns("com.apple.dock"));
        if let Some(d) = dock.firstObject() {
            d.activateWithOptions(objc2_app_kit::NSApplicationActivationOptions::empty());
            later(160, || {
                let (Some(u), Some(mtm)) = (ui(), MainThreadMarker::new()) else { return };
                NSApplication::sharedApplication(mtm).activate();
                u.window.makeKeyAndOrderFront(None);
                focus(&u);
            });
        }
    }
    if !POLLING.with(|p| p.replace(true)) {
        poll();
    }
}

pub fn toggle() {
    match ui() {
        Some(u) if u.window.isVisible() && u.window.isKeyWindow() => u.window.performClose(None),
        _ => open(None),
    }
}

fn list_check(u: &Ui) -> Vec<String> {
    let mut bad = Vec::new();
    let n = m(|s| s.rows.len());
    if u.table.numberOfRows() as usize != n {
        bad.push(format!("table has {} rows, model {n}", u.table.numberOfRows()));
    }
    let r = u.table.rowsInRect(u.table.visibleRect());
    for i in r.location..r.location + r.length {
        let shown = u
            .table
            .viewAtColumn_row_makeIfNecessary(0, i as NSInteger, false)
            .and_then(|v| v.downcast::<NSTableCellView>().ok())
            .and_then(|c| unsafe { c.textField() })
            .map(|t| t.stringValue().to_string());
        let want = m(|s| s.rows.get(i).map(|e| e.name.clone()));
        if shown.is_some() && shown != want {
            bad.push(format!("row {i} shows {shown:?}, model {want:?}"));
        }
    }
    bad
}

// renders the window into a png without ever putting it on screen, so the layout
// can be checked without hijacking anyone's display
pub fn snapshot(path: &str, out: &str, script: &str) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    TESTING.with(|t| t.set(true));
    let u = build(mtm, false);
    go(&u, path, false, None);
    let spin = |secs: f64| NSRunLoop::currentRunLoop().runUntilDate(&NSDate::dateWithTimeIntervalSinceNow(secs));
    let mut scale = 1.0f64;
    spin(0.4);
    for step in script.split(',').filter(|s| !s.is_empty()) {
        let (cmd, arg) = step.split_once('=').unwrap_or((step, ""));
        eprintln!("snapshot: {cmd} {arg}");
        match cmd {
            "select" => {
                let rows: Vec<usize> = arg.split('+').filter_map(|x| x.parse::<usize>().ok()).collect();
                pick_rows(&u, &rows, false);
                update_status(&u);
            }
            "mode" => set_mode(&u, if arg == "list" { 0 } else { 1 }),
            "gzoom" => grid::set_zoom(&u, arg.parse().unwrap_or(1), false),
            "group" => {
                GROUP.with(|g| g.set(arg != "0"));
                refresh_rows(&u);
            }
            "by" => sort_by(&u, arg.parse().unwrap_or(0)),
            "peek" => {
                let want = arg.to_lowercase();
                let row = arg.parse::<usize>().ok().or_else(|| m(|s| s.rows.iter().position(|e| e.lc.starts_with(&want))));
                grid::peek_test(&u, row.unwrap_or(0));
            }
            // puts the window in the window server so the real display cycle runs: fully
            // transparent, click-through, parked far off every screen. this process is
            // barred from activating, so it can't take focus either
            "live" => {
                u.window.setAlphaValue(0.0);
                u.window.setIgnoresMouseEvents(true);
                u.window.setFrameOrigin(NSPoint::new(-30000.0, -30000.0));
                u.window.orderFrontRegardless();
                spin(0.3);
                eprintln!("snapshot: live, visible={} at {:?}", u.window.isVisible(), u.window.frame().origin);
            }
            // marks everything in the content pane as needing layout and lays it out, the
            // way a window on screen does every frame
            "relayout" => {
                fn mark(v: &NSView) {
                    v.setNeedsLayout(true);
                    let subs = v.subviews();
                    for i in 0..subs.count() {
                        mark(&subs.objectAtIndex(i));
                    }
                }
                mark(&u.stage);
                u.window.layoutIfNeeded();
                eprintln!("snapshot: relayout survived");
            }
            "jump" => {
                grid::jump_to(&u, arg.parse().unwrap_or(0));
                spin(0.5);
            }
            "sort" => {
                let (key, asc) = arg.split_once(':').unwrap_or((arg, "1"));
                u.table.setSortDescriptors(&NSArray::from_retained_slice(&[NSSortDescriptor::sortDescriptorWithKey_ascending(Some(&ns(key)), asc == "1")]));
            }
            "query" => {
                u.search.setStringValue(&ns(arg));
                let _: () = unsafe { msg_send![&*u.ctl, lfSearch: std::ptr::null::<AnyObject>()] };
            }
            "go" => go(&u, arg, true, None),
            "size" => {
                if let Some((w, h)) = arg.split_once('x') {
                    u.window.setContentSize(NSSize::new(w.parse().unwrap_or(1060.0), h.parse().unwrap_or(660.0)));
                    u.window.layoutIfNeeded();
                }
            }
            "light" | "dark" => {
                use objc2_app_kit::{NSAppearance, NSAppearanceCustomization, NSAppearanceNameAqua, NSAppearanceNameDarkAqua};
                let name = unsafe { if cmd == "light" { NSAppearanceNameAqua } else { NSAppearanceNameDarkAqua } };
                u.window.setAppearance(NSAppearance::appearanceNamed(name).as_deref());
                DARK.with(|d| d.set(cmd == "dark"));
                reload_view(&u);
            }
            "back" => go_back(&u),
            "up" => go_up(&u),
            "hidden" => {
                m(|s| s.hidden = !s.hidden);
                refresh_rows(&u);
            }
            "newfolder" => new_folder(&u),
            "rename" => begin_rename(&u),
            "type" => {
                if let Some(ed) = u.window.firstResponder().and_then(|r| r.downcast::<NSTextView>().ok()) {
                    ed.setString(&ns(arg));
                }
            }
            "commit" => focus(&u),
            "duplicate" => {
                let _: () = unsafe { msg_send![&*u.ctl, lfDuplicate: std::ptr::null::<AnyObject>()] };
            }
            "menu" => {
                if let Some(menu) = u.table.menu() {
                    let _: () = unsafe { msg_send![&*u.ctl, menuNeedsUpdate: &*menu] };
                    eprintln!("snapshot: context menu has {} items", menu.numberOfItems());
                }
            }
            "dump" => {
                m(|s| {
                    for e in s.rows.iter().take(12) {
                        eprintln!("  {:<32} dir={} size={} label={} kind={}", e.name, e.dir, e.size, e.label, kind_of(&e.path, &e.name, e.dir));
                    }
                });
            }
            // an off-screen window never has focus, so ask for the focused look by hand
            "emph" if grid_mode() => {
                grid::FORCE_EMPH.with(|f| f.set(true));
                grid::restyle(&u);
            }
            "emph" => {
                for i in selected(&u.table) {
                    if let Some(r) = u.table.rowViewAtRow_makeIfNecessary(i as NSInteger, false) {
                        r.setEmphasized(true);
                    }
                }
            }
            "tag" => toggle_tag(&u, arg.parse().unwrap_or(0), &target_ents(&u)),
            // presses one dot of the palette the way a click would
            "tagclick" => {
                let it = tags_item(&u, mtm);
                if let Some(pal) = it.submenu() {
                    pal.performActionForItemAtIndex(arg.parse().unwrap_or(0));
                }
            }
            // runs the tick logic over the whole menu bar and prints what ended up ticked
            "ticks" => {
                fn walk(menu: &NSMenu, out: &mut Vec<String>) {
                    let items = menu.itemArray();
                    for i in 0..items.count() {
                        let it = items.objectAtIndex(i);
                        if it.action().is_some() {
                            let ok = validate_item(&it);
                            if it.state() == objc2_app_kit::NSControlStateValueOn || !ok {
                                out.push(format!("{}{}", it.title(), if ok { "" } else { " (off)" }));
                            }
                        }
                        if let Some(sub) = it.submenu() {
                            walk(&sub, out);
                        }
                    }
                }
                let mut out = Vec::new();
                walk(&u.menu, &mut out);
                eprintln!("snapshot: ticked {out:?}");
            }
            "tagmenu" => {
                let it = tags_item(&u, mtm);
                let n = it.submenu().map_or(0, |m| m.numberOfItems());
                let on = it.submenu().map_or(0, |m| m.selectedItems().count());
                eprintln!("snapshot: tag palette has {n} dots, {on} switched on");
            }
            "wait" => spin(arg.parse().unwrap_or(1.0)),
            // changes on disk, then the same reload a real change would get
            "touch" => {
                let _ = fs::write(format!("{path}/{arg}"), b"x");
                reload(&u);
            }
            "mkdir" => {
                let _ = fs::create_dir(format!("{path}/{arg}"));
                reload(&u);
            }
            "rm" => {
                let p = format!("{path}/{arg}");
                let _ = fs::remove_file(&p).or_else(|_| fs::remove_dir(&p));
                reload(&u);
            }
            "mv" => {
                if let Some((a, b)) = arg.split_once(':') {
                    let (a, b) = (format!("{path}/{a}"), format!("{path}/{b}"));
                    if fs::rename(&a, &b).is_ok() {
                        RENAMED.with(|r| *r.borrow_mut() = Some((a, b)));
                    }
                    reload(&u);
                }
            }
            // starts the open-a-folder zoom from a known point and reads the layer part way
            // through: that point has to stay put while everything else grows
            "motion" => {
                let p = NSPoint::new(300.0, 200.0);
                ORIGIN.with(|o| o.set(Some(p)));
                let dir: f64 = arg.parse().unwrap_or(1.0);
                arrive(&u, dir);
                // core animation files every transition under "transition", whatever key it was given
                let fading = u.stage.layer().and_then(|l| l.animationKeys()).is_some_and(|k| (0..k.count()).any(|i| k.objectAtIndex(i).to_string() == "transition"));
                eprintln!("snapshot: motion crossfade attached={fading}");
                spin(0.1);
                let view: &NSView = if grid_mode() { &u.grid_scroll } else { &u.list_scroll };
                match view.layer().and_then(|l| unsafe { l.presentationLayer() }) {
                    Some(pl) => {
                        let t = pl.transform();
                        let (b, a) = (pl.bounds().size, pl.anchorPoint());
                        let (ax, ay) = (a.x * b.width, a.y * b.height);
                        let map = |x: f64, y: f64| (ax + t.m11 * (x - ax) + t.m21 * (y - ay) + t.m41, ay + t.m12 * (x - ax) + t.m22 * (y - ay) + t.m42);
                        let (q, far) = (map(p.x, p.y), map(p.x + 200.0, p.y + 100.0));
                        eprintln!(
                            "snapshot: motion anchor ({:.2},{:.2}) scale {:.3} opacity {:.2}: ({},{}) -> ({:.1},{:.1}), ({},{}) -> ({:.1},{:.1})",
                            a.x, a.y, t.m11, pl.opacity(), p.x, p.y, q.0, q.1, p.x + 200.0, p.y + 100.0, far.0, far.1
                        );
                    }
                    None => eprintln!("snapshot: motion has no presentation layer off-screen"),
                }
            }
            // goes to a place and reads the sidebar highlight part way through its trip
            "pill" => {
                let before = u.side_pill.layer().map(|l| l.position().y);
                go(&u, &arg.replace('~', &home()), true, None);
                spin(0.07);
                let l = u.side_pill.layer();
                let shown = l.as_ref().and_then(|l| unsafe { l.presentationLayer() }).map(|p| p.position().y);
                let after = l.map(|l| l.position().y);
                eprintln!("snapshot: pill from {before:?} now at {shown:?} heading for {after:?}");
            }
            "gscroll" => {
                let clip = u.grid_scroll.contentView();
                clip.setBoundsOrigin(NSPoint::new(0.0, arg.parse().unwrap_or(0.0)));
                u.grid_scroll.reflectScrolledClipView(&clip);
            }
            // does what the view shows still add up to the model? counts per section and
            // the file behind every tile or row that's on screen
            "check" => {
                u.window.layoutIfNeeded();
                let bad = if grid_mode() { grid::check(&u) } else { list_check(&u) };
                eprintln!("snapshot: check {}", if bad.is_empty() { "ok".to_owned() } else { bad.join("; ") });
            }
            "scroll" => {
                let want = arg.to_lowercase();
                let row = arg.parse::<usize>().ok().or_else(|| m(|s| s.rows.iter().position(|e| e.lc.starts_with(&want))));
                if let Some(r) = row {
                    let n = u.table.numberOfRows();
                    u.table.scrollRowToVisible((r as NSInteger + 14).min(n - 1));
                    u.table.scrollRowToVisible((r as NSInteger - 4).max(0));
                }
            }
            "scale" => scale = arg.parse().unwrap_or(1.0),
            "zoom" => set_zoom(&u, arg.parse().unwrap_or(0)),
            "bench" => {
                let t = std::time::Instant::now();
                m(rebuild);
                let a = t.elapsed();
                reload_view(&u);
                u.window.layoutIfNeeded();
                if grid_mode() {
                    u.grid.layoutSubtreeIfNeeded();
                }
                eprintln!("snapshot: {} rows, rebuild {a:?}, reload + layout {:?}, tiles built {}", m(|s| s.rows.len()), t.elapsed() - a, u.grid.visibleItems().count());
            }
            "probe" => {
                let f = u.grid_scroll.frame();
                eprintln!(
                    "snapshot: grid scroll {:?} hidden={} doc {:?} visible items={} layer={}",
                    f,
                    u.grid_scroll.isHidden(),
                    u.grid.frame(),
                    u.grid.visibleItems().count(),
                    u.grid.wantsLayer()
                );
                let items = u.grid.visibleItems();
                if items.count() > 0 {
                    let v = items.objectAtIndex(0).view();
                    eprintln!(
                        "snapshot: first tile frame {:?} hidden={} hiddenAncestor={} alpha={} opacity={:?} subviews={} clipBounds {:?} visibleRect {:?}",
                        v.frame(),
                        v.isHidden(),
                        v.isHiddenOrHasHiddenAncestor(),
                        v.alphaValue(),
                        v.layer().map(|l| l.opacity()),
                        v.subviews().count(),
                        u.grid_scroll.contentView().bounds(),
                        u.grid.visibleRect()
                    );
                }
            }
            "where" => {
                let st = u.stage.convertRect_toView(u.stage.bounds(), None);
                let p = u.path.convertRect_toView(u.path.bounds(), None);
                eprintln!(
                    "snapshot: window {} wide. stage x {}..{} y {}..{}. path x {}..{}. safe left {}",
                    u.window.frame().size.width,
                    st.origin.x,
                    st.origin.x + st.size.width,
                    st.origin.y,
                    st.origin.y + st.size.height,
                    p.origin.x,
                    p.origin.x + p.size.width,
                    u.stage.safeAreaInsets().left
                );
            }
            _ => eprintln!("snapshot: unknown step {cmd}"),
        }
        spin(0.25);
    }
    u.window.layoutIfNeeded();
    spin(0.3);
    let Some(frame) = u.window.contentView().and_then(|v| unsafe { v.superview() }) else { return };
    let b = frame.bounds();
    // a rep with more pixels than points makes appkit draw at retina scale
    let canvas = |v: &NSView, b: NSRect| -> Option<Retained<NSBitmapImageRep>> {
        if scale <= 1.0 {
            return v.bitmapImageRepForCachingDisplayInRect(b);
        }
        let rep = unsafe {
            NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
                NSBitmapImageRep::alloc(),
                std::ptr::null_mut(),
                (b.size.width * scale) as NSInteger,
                (b.size.height * scale) as NSInteger,
                8,
                4,
                true,
                false,
                objc2_app_kit::NSDeviceRGBColorSpace,
                0,
                0,
            )
        }?;
        rep.setSize(b.size);
        Some(rep)
    };
    let Some(rep) = canvas(&frame, b) else { return };
    frame.cacheDisplayInRect_toBitmapImageRep(b, &rep);
    let data = unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new()) };
    let ok = data.is_some_and(|d| d.writeToFile_atomically(&ns(out), true));
    let shoot = |v: &NSView, suffix: &str| {
        let b = v.bounds();
        if let Some(rep) = canvas(v, b) {
            v.cacheDisplayInRect_toBitmapImageRep(b, &rep);
            if let Some(d) = unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new()) } {
                d.writeToFile_atomically(&ns(&format!("{out}.{suffix}.png")), true);
            }
        }
    };
    shoot(&u.side, "side");
    shoot(&u.nav, "nav");
    if grid_mode() {
        // the scroll view comes back empty off-screen, the canvas inside it doesn't.
        // so shoot the part of the canvas that's in view
        let vr = u.grid.visibleRect();
        if let Some(rep) = canvas(&u.grid, vr) {
            u.grid.cacheDisplayInRect_toBitmapImageRep(vr, &rep);
            if let Some(d) = unsafe { rep.representationUsingType_properties(NSBitmapImageFileType::PNG, &NSDictionary::new()) } {
                d.writeToFile_atomically(&ns(&format!("{out}.list.png")), true);
            }
        }
    } else {
        shoot(&u.list_scroll, "list");
    }
    let widths: Vec<f64> = u.cols.iter().map(|c| c.width()).collect();
    eprintln!("snapshot: columns {widths:?} table {:?} side {:?}", u.table.frame().size.width, u.side.frame().size);
    eprintln!("snapshot: sidebar highlight alpha {} at {:?}", u.side_pill.alphaValue(), u.side_pill.frame());
    let (rows, sel) = (m(|s| s.rows.len()), picked(&u).len());
    eprintln!("snapshot: sections {:?}", m(|s| s.sections.iter().map(|x| format!("{} {}", x.title, x.len)).collect::<Vec<_>>()));
    eprintln!("snapshot: wrote={ok} {}x{} rows={rows} selected={sel} status={:?}", b.size.width, b.size.height, u.status.stringValue().to_string());
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn helpers() {
        let mut v = vec!["file10.txt", "file2.txt", "file1.txt", "a", "file002b"];
        v.sort_by(|a, b| nat_cmp(a, b));
        assert_eq!(v, ["a", "file1.txt", "file2.txt", "file002b", "file10.txt"]);
        let d = std::env::temp_dir().join(format!("liftoff-test-{}", std::process::id()));
        fs::create_dir_all(&d).unwrap();
        let ds = d.to_str().unwrap();
        fs::write(d.join("x.txt"), "1").unwrap();
        assert_eq!(free_name(ds, "y.txt", false), format!("{ds}/y.txt"));
        assert_eq!(free_name(ds, "x.txt", false), format!("{ds}/x copy.txt"));
        fs::write(d.join("x copy.txt"), "1").unwrap();
        assert_eq!(free_name(ds, "x.txt", true), format!("{ds}/x copy 2.txt"));
        fs::remove_dir_all(&d).unwrap();
        assert_eq!(parent_of("/a/b"), "/a");
        assert_eq!(parent_of("/a"), "/");
    }

    #[test]
    fn colours() {
        let pl = |path: &str, hue| Place { label: String::new(), path: path.into(), symbol: "", hue, header: false };
        let places = vec![pl("/", SILVER), pl("/Users/x", BLUE), pl("/Users/x/Downloads", GREEN), pl("/Users/x/Developer", ORANGE)];
        assert_eq!(hue_in(&places, "/Users/x/Downloads"), GREEN);
        assert_eq!(hue_in(&places, "/Users/x/Downloads/a/b"), GREEN);
        assert_eq!(hue_in(&places, "/Users/x/Downloads2"), BLUE);
        assert_eq!(hue_in(&places, "/Users/x/Developer/liftoff"), ORANGE);
        assert_eq!(hue_in(&places, "/usr/bin"), BLUE);
        assert_eq!(tag_colour("Red\n6"), 6);
        assert_eq!(tag_colour("Work"), 0);
        assert_eq!(tag_colour("Work\n0"), 0);
        assert_eq!(tag_colour("odd\n9"), 0);
        // white text has to stay readable on every selection colour
        let lin = |c: f64| if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
        for (i, &(r, g, b)) in HUES.iter().enumerate() {
            let k = deep(i);
            let lum = 0.2126 * lin(r * k) + 0.7152 * lin(g * k) + 0.0722 * lin(b * k);
            assert!(1.05 / (lum + 0.05) >= 3.3, "hue {i} too bright for white text: {lum}");
        }
    }

    #[test]
    fn grouping() {
        let ent = |name: &str, dir: bool| Ent {
            name: name.into(),
            lc: name.to_lowercase(),
            path: format!("/t/{name}"),
            dir,
            size: 1,
            mtime: 0,
            hidden: false,
            place: String::new(),
            label: 0,
            custom: false,
        };
        m(|s| {
            s.all = vec![ent("b.png", false), ent("zeta", true), ent("a.zip", false), ent("Alpha", true), ent("c.jpg", false), ent("App.app", true)];
            rebuild(s);
            let got: Vec<(&str, usize, usize)> = s.sections.iter().map(|x| (x.title.as_str(), x.start, x.len)).collect();
            assert_eq!(got, [("Folders", 0, 2), ("Applications", 2, 1), ("Pictures", 3, 2), ("Archives", 5, 1)]);
            let names: Vec<&str> = s.rows.iter().map(|e| e.name.as_str()).collect();
            assert_eq!(names, ["Alpha", "zeta", "App.app", "b.png", "c.jpg", "a.zip"]);
        });
        GROUP.with(|g| g.set(false));
        m(|s| {
            rebuild(s);
            assert_eq!(s.sections.len(), 1);
            assert_eq!((s.sections[0].start, s.sections[0].len), (0, 6));
        });
        assert_eq!(category(&ent("x.RS", false)), 6);
        assert_eq!(category(&ent("Photos Library.photoslibrary", true)), 8);
    }

    #[test]
    fn kinds() {
        assert_eq!(kind_of("/usr/lib", "lib", true), "Folder");
        assert_eq!(kind_of("/System/Applications/Calculator.app", "Calculator.app", true), "Application");
        let hosts = kind_of("/etc/hosts", "hosts", false);
        assert!(!hosts.is_empty());
        assert_eq!(kind_of("/nope/x.zzqqy", "x.zzqqy", false), "ZZQQY file");
    }
}
