use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2::{define_class, msg_send, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAnimatablePropertyContainer, NSAnimationContext, NSAppearance, NSAppearanceCustomization,
    NSAppearanceNameDarkAqua, NSApplication, NSAutoresizingMaskOptions, NSBackingStoreType,
    NSEvent, NSEventModifierFlags, NSEventPhase, NSFont, NSFontWeightMedium, NSFontWeightRegular,
    NSFontWeightSemibold, NSScreen, NSView, NSVisualEffectBlendingMode, NSVisualEffectMaterial,
    NSVisualEffectState, NSVisualEffectView, NSWindow, NSWindowCollectionBehavior,
    NSWindowStyleMask, NSWorkspace,
};
use objc2_core_foundation::{CFRetained, CFType, CGFloat, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{CGColor, CGImage};
use objc2_foundation::{NSNumber, NSPoint, NSRect, NSString, NSURL};
use objc2_quartz_core::{
    kCAAlignmentCenter, kCAFilterTrilinear, kCAGravityResizeAspect, kCATruncationEnd,
    CABasicAnimation, CALayer, CAMediaTiming, CAMediaTimingFunction, CATextLayer, CATransaction,
    CATransform3D, CATransform3DIdentity,
};

use crate::icons;
use crate::index::{self, App};
use crate::layout::{self, Entry, Folder};

const COLS: usize = 7;
const ROWS: usize = 5;
const PER_PAGE: usize = COLS * ROWS;
const LABEL_H: f64 = 18.0;
const LABEL_GAP: f64 = 7.0;
const EDGE: f64 = 60.0;

#[derive(Clone, Copy, PartialEq, Default)]
struct Geo {
    w: f64,
    h: f64,
    side: f64,
    top: f64,
    bottom: f64,
    cell_w: f64,
    cell_h: f64,
    icon: f64,
}

impl Geo {
    fn new(w: f64, h: f64) -> Self {
        let top = (h * 0.13).round();
        let bottom = (h * 0.10).round();
        let side = (w * 0.10).round();
        let cell_w = (w - 2.0 * side) / COLS as f64;
        let cell_h = (h - top - bottom) / ROWS as f64;
        let icon = (cell_h * 0.62).min(cell_w * 0.62).clamp(48.0, 144.0).round();
        Geo { w, h, side, top, bottom, cell_w, cell_h, icon }
    }
    fn center(&self, pos: usize) -> (f64, f64) {
        let page = pos / PER_PAGE;
        let r = pos % PER_PAGE;
        (
            page as f64 * self.w + self.side + (r % COLS) as f64 * self.cell_w + self.cell_w / 2.0,
            self.top + (r / COLS) as f64 * self.cell_h + self.cell_h / 2.0,
        )
    }
    fn icon_rect(&self) -> CGRect {
        let block = self.icon + LABEL_GAP + LABEL_H;
        rect((self.cell_w - self.icon) / 2.0, ((self.cell_h - block) / 2.0).round(), self.icon, self.icon)
    }
}

#[derive(Clone, Copy, Default)]
struct PanelGeo {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
    cell_h: f64,
    title_h: f64,
    pad: f64,
}

impl PanelGeo {
    fn new(g: &Geo, n: usize) -> Self {
        let rows = n.div_ceil(COLS).max(1) as f64;
        let title_h = 64.0;
        let pad = 24.0;
        let cell_h = g.cell_h.min((g.h * 0.8 - title_h - pad) / rows);
        let w = g.cell_w * COLS as f64 + 2.0 * pad;
        let h = title_h + rows * cell_h + pad;
        PanelGeo { x: ((g.w - w) / 2.0).round(), y: ((g.h - h) / 2.0).round(), w, h, cell_h, title_h, pad }
    }
    fn center(&self, g: &Geo, i: usize) -> (f64, f64) {
        (
            self.pad + (i % COLS) as f64 * g.cell_w + g.cell_w / 2.0,
            self.title_h + (i / COLS) as f64 * self.cell_h + self.cell_h / 2.0,
        )
    }
    fn contains(&self, p: NSPoint) -> bool {
        p.x >= self.x && p.x <= self.x + self.w && p.y >= self.y && p.y <= self.y + self.h
    }
    fn cell(&self, g: &Geo, p: NSPoint) -> Option<usize> {
        let (lx, ly) = (p.x - self.x - self.pad, p.y - self.y - self.title_h);
        if lx < 0.0 || ly < 0.0 || lx >= g.cell_w * COLS as f64 {
            return None;
        }
        Some((ly / self.cell_h) as usize * COLS + (lx / g.cell_w) as usize)
    }
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
    CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
}

pub(crate) fn gray(g: f64, a: f64) -> CFRetained<CGColor> {
    CGColor::new_generic_gray(g, a)
}

pub(crate) fn as_obj<T>(r: &T) -> &AnyObject {
    unsafe { &*(r as *const T as *const AnyObject) }
}

pub(crate) fn ease_out() -> Retained<CAMediaTimingFunction> {
    CAMediaTimingFunction::functionWithControlPoints(0.2, 0.9, 0.3, 1.0)
}

pub(crate) fn no_anim<R>(f: impl FnOnce() -> R) -> R {
    CATransaction::begin();
    CATransaction::setDisableActions(true);
    let r = f();
    CATransaction::commit();
    r
}

pub(crate) fn animated<R>(dur: f64, f: impl FnOnce() -> R) -> R {
    CATransaction::begin();
    CATransaction::setAnimationDuration(dur);
    CATransaction::setAnimationTimingFunction(Some(&ease_out()));
    let r = f();
    CATransaction::commit();
    r
}

// moves a layer to a new frame on a spring with no bounce, starting from wherever it
// is on screen right now, so a run of quick moves (an arrow key held down) is one
// smooth trip instead of a string of hops
pub(crate) fn glide(l: &CALayer, to: CGRect, feel: f64) {
    let from = unsafe { l.presentationLayer() }.map_or(l.position(), |p| p.position());
    no_anim(|| l.setFrame(to));
    let now = l.position();
    for (key, a, b) in [("position.x", from.x, now.x), ("position.y", from.y, now.y)] {
        if (a - b).abs() > 0.5 {
            let sp = objc2_quartz_core::CASpringAnimation::initWithPerceptualDuration_bounce(<objc2_quartz_core::CASpringAnimation as objc2::AnyThread>::alloc(), feel, 0.0);
            sp.setKeyPath(Some(&NSString::from_str(key)));
            unsafe {
                sp.setFromValue(Some(&NSNumber::new_f64(a)));
                sp.setToValue(Some(&NSNumber::new_f64(b)));
            }
            sp.setDuration(sp.settlingDuration());
            l.addAnimation_forKey(&sp, Some(&NSString::from_str(key)));
        }
    }
}

fn pop_in(layer: &CALayer, from_scale: f64, dur: f64) {
    let tf = ease_out();
    let scale = CABasicAnimation::animationWithKeyPath(Some(&NSString::from_str("transform.scale")));
    unsafe {
        scale.setFromValue(Some(&NSNumber::new_f64(from_scale)));
        scale.setToValue(Some(&NSNumber::new_f64(1.0)));
    }
    scale.setDuration(dur);
    scale.setTimingFunction(Some(&tf));
    let fade = CABasicAnimation::animationWithKeyPath(Some(&NSString::from_str("opacity")));
    unsafe {
        fade.setFromValue(Some(&NSNumber::new_f64(0.0)));
        fade.setToValue(Some(&NSNumber::new_f64(1.0)));
    }
    fade.setDuration(dur * 0.8);
    fade.setTimingFunction(Some(&tf));
    layer.addAnimation_forKey(&scale, Some(&NSString::from_str("zoom")));
    layer.addAnimation_forKey(&fade, Some(&NSString::from_str("fade")));
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
enum Key {
    App(String),
    Folder(u64),
}

struct Tile {
    layer: Retained<CALayer>,
    icon: Retained<CALayer>,
    label: Retained<CATextLayer>,
    minis: Vec<Retained<CALayer>>,
}

enum Act {
    None,
    Hide,
    Launch(String),
    Quit,
}

#[derive(Clone, Copy, PartialEq)]
enum Press {
    Main(usize),
    Panel(usize),
    Fav(usize),
}

struct Drag {
    entry: Entry,
    key: Key,
    in_folder: Option<u64>,
    grab: (f64, f64),
    gap: usize,
    merge: Option<usize>,
    last: NSPoint,
    edge_since: Option<Instant>,
    pending: Option<(usize, Instant)>,
}

struct State {
    window: Retained<NSWindow>,
    content: Retained<CALayer>,
    pages: Retained<CALayer>,
    sel_layer: Retained<CALayer>,
    merge_layer: Retained<CALayer>,
    search_bg: Retained<CALayer>,
    search_text: Retained<CATextLayer>,
    dots_layer: Retained<CALayer>,
    dots: Vec<Retained<CALayer>>,
    dim: Retained<CALayer>,
    panel: Retained<CALayer>,
    panel_title: Retained<CATextLayer>,
    panel_sel: Retained<CALayer>,
    drag_layer: Retained<CALayer>,
    label_font: Retained<NSFont>,
    fav_band: Retained<CALayer>,
    fav_caption: Retained<CATextLayer>,
    fav_tiles: Vec<Tile>,
    fav: Vec<String>,

    apps: Vec<App>,
    by_path: HashMap<String, usize>,
    items: Vec<Entry>,
    tiles: HashMap<Key, Tile>,
    icons: HashMap<String, (u64, CFRetained<CGImage>)>,
    usage: HashMap<String, u32>,

    visible: Vec<Key>,
    query: String,
    sel: Option<usize>,
    page: usize,
    offset: f64,
    geo: Geo,
    scale: f64,
    icon_px: usize,

    open: Option<u64>,
    pgeo: PanelGeo,
    psel: Option<usize>,
    renaming: bool,

    swiping: bool,
    vel: f64,
    last_move: Instant,
    prev_x: f64,
    merge_hint: Option<Key>,
    wheel_last: Instant,
    mouse_start: Option<NSPoint>,
    pressed: Option<Press>,
    paging: bool,
    drag: Option<Drag>,
}

thread_local! {
    static ST: RefCell<Option<State>> = const { RefCell::new(None) };
    static VISIBLE: Cell<bool> = const { Cell::new(false) };
    static GEN: Cell<u64> = const { Cell::new(0) };
}

fn bump_gen() -> u64 {
    GEN.with(|g| {
        g.set(g.get() + 1);
        g.get()
    })
}

const FADE_OUT: f64 = 0.2;

fn with<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    ST.with(|s| s.borrow_mut().as_mut().map(f))
}

fn debug() -> bool {
    std::env::var_os("LIFTOFF_DEBUG").is_some()
}

fn key_of(e: &Entry) -> Key {
    match e {
        Entry::App(p) => Key::App(p.clone()),
        Entry::Folder(f) => Key::Folder(f.id),
    }
}

impl State {
    fn searching(&self) -> bool {
        !self.query.is_empty()
    }

    fn base(&self) -> usize {
        if !self.searching() && !self.fav.is_empty() { COLS } else { 0 }
    }

    fn compute_favs(&mut self) {
        let mut v: Vec<(&String, &u32)> =
            self.usage.iter().filter(|(p, c)| **c > 0 && self.by_path.contains_key(*p)).collect();
        v.sort_unstable_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
        self.fav = v.into_iter().take(COLS).map(|(p, _)| p.clone()).collect();
    }

    fn npages(&self) -> usize {
        let n = self.base() + self.visible.len() + usize::from(self.drag.as_ref().is_some_and(|d| d.in_folder.is_none()));
        n.div_ceil(PER_PAGE).max(1)
    }

    fn folder(&self, id: u64) -> Option<&Folder> {
        self.items.iter().find_map(|e| match e {
            Entry::Folder(f) if f.id == id => Some(f),
            _ => None,
        })
    }

    fn folder_mut(&mut self, id: u64) -> Option<&mut Folder> {
        self.items.iter_mut().find_map(|e| match e {
            Entry::Folder(f) if f.id == id => Some(f),
            _ => None,
        })
    }

    fn make_label(&self, text: &str) -> Retained<CATextLayer> {
        let label = CATextLayer::new();
        unsafe {
            label.setString(Some(&NSString::from_str(text)));
            label.setFont(Some(&*(Retained::as_ptr(&self.label_font) as *const CFType)));
            label.setAlignmentMode(kCAAlignmentCenter);
            label.setTruncationMode(kCATruncationEnd);
        }
        label.setFontSize(13.0);
        label.setForegroundColor(Some(&gray(1.0, 1.0)));
        label.setContentsScale(self.scale);
        label
    }

    fn make_icon_layer(&self) -> Retained<CALayer> {
        let l = CALayer::new();
        unsafe {
            l.setContentsGravity(kCAGravityResizeAspect);
            l.setMinificationFilter(kCAFilterTrilinear);
        }
        l
    }

    fn add_tile(&mut self, key: Key) {
        let (icon, label, minis) = match &key {
            Key::App(p) => {
                let icon = self.make_icon_layer();
                if let Some((_, img)) = self.icons.get(p) {
                    unsafe { icon.setContents(Some(as_obj(&**img))) };
                }
                let name = self.by_path.get(p).map(|&i| self.apps[i].name.clone()).unwrap_or_default();
                (icon, self.make_label(&name), Vec::new())
            }
            Key::Folder(id) => {
                let bg = CALayer::new();
                bg.setBackgroundColor(Some(&gray(1.0, 0.2)));
                let minis = (0..9)
                    .map(|_| {
                        let m = self.make_icon_layer();
                        bg.addSublayer(&m);
                        m
                    })
                    .collect();
                let name = self.folder(*id).map(|f| f.name.clone()).unwrap_or_default();
                (bg, self.make_label(&name), minis)
            }
        };
        let layer = CALayer::new();
        layer.addSublayer(&icon);
        layer.addSublayer(&label);
        layer.setHidden(true);
        self.pages.addSublayer(&layer);
        let t = Tile { layer, icon, label, minis };
        self.size_tile(&t);
        self.tiles.insert(key.clone(), t);
        if let Key::Folder(id) = key {
            self.refresh_folder_tile(id);
        }
    }

    fn size_tile(&self, t: &Tile) {
        let g = self.geo;
        let ir = g.icon_rect();
        no_anim(|| {
            t.layer.setBounds(rect(0.0, 0.0, g.cell_w, g.cell_h));
            t.label.setFrame(rect(2.0, ir.origin.y + g.icon + LABEL_GAP, g.cell_w - 4.0, LABEL_H));
            if t.minis.is_empty() {
                t.icon.setFrame(ir);
            } else {
                let bw = (g.icon * 0.82).round();
                let off = ((g.icon - bw) / 2.0).round();
                t.icon.setFrame(rect(ir.origin.x + off, ir.origin.y + off, bw, bw));
                t.icon.setCornerRadius(bw * 0.225);
                let m = bw * 0.27;
                let gap = bw * 0.035;
                let pad = (bw - 3.0 * m - 2.0 * gap) / 2.0;
                for (i, l) in t.minis.iter().enumerate() {
                    let (c, r) = ((i % 3) as f64, (i / 3) as f64);
                    l.setFrame(rect(pad + c * (m + gap), pad + r * (m + gap), m, m));
                }
            }
        });
    }

    fn refresh_folder_tile(&self, id: u64) {
        let (Some(f), Some(t)) = (self.folder(id), self.tiles.get(&Key::Folder(id))) else { return };
        no_anim(|| {
            unsafe { t.label.setString(Some(&NSString::from_str(&f.name))) };
            for (i, m) in t.minis.iter().enumerate() {
                match f.apps.get(i).and_then(|p| self.icons.get(p)) {
                    Some((_, img)) => unsafe {
                        m.setContents(Some(as_obj(&**img)));
                        m.setHidden(false);
                    },
                    None => m.setHidden(true),
                }
            }
        });
    }

    fn sync_tiles(&mut self) {
        let mut want: HashSet<Key> = self.apps.iter().map(|a| Key::App(a.path.clone())).collect();
        for e in &self.items {
            if let Entry::Folder(f) = e {
                want.insert(Key::Folder(f.id));
            }
        }
        self.tiles.retain(|k, t| {
            let keep = want.contains(k);
            if !keep {
                t.layer.removeFromSuperlayer();
            }
            keep
        });
        let missing: Vec<Key> = want.into_iter().filter(|k| !self.tiles.contains_key(k)).collect();
        for k in missing {
            self.add_tile(k);
        }
        let ids: Vec<u64> = self
            .items
            .iter()
            .filter_map(|e| if let Entry::Folder(f) = e { Some(f.id) } else { None })
            .collect();
        for id in ids {
            self.refresh_folder_tile(id);
        }
        while self.fav_tiles.len() < COLS {
            let icon = self.make_icon_layer();
            let label = self.make_label("");
            let layer = CALayer::new();
            layer.addSublayer(&icon);
            layer.addSublayer(&label);
            layer.setHidden(true);
            self.pages.addSublayer(&layer);
            let t = Tile { layer, icon, label, minis: Vec::new() };
            self.size_tile(&t);
            self.fav_tiles.push(t);
        }
    }

    fn layout_favs(&self) {
        let g = self.geo;
        let show = self.base() > 0;
        no_anim(|| {
            self.fav_band.setHidden(!show);
            self.fav_caption.setHidden(!show);
            if show {
                self.fav_band.setFrame(rect(g.side - 14.0, g.top + 2.0, g.cell_w * COLS as f64 + 28.0, g.cell_h - 4.0));
                self.fav_caption.setFrame(rect(g.side - 4.0, g.top - 20.0, 300.0, 16.0));
            }
            for (i, t) in self.fav_tiles.iter().enumerate() {
                match self.fav.get(i).filter(|_| show) {
                    Some(p) => {
                        if let Some((_, img)) = self.icons.get(p) {
                            unsafe { t.icon.setContents(Some(as_obj(&**img))) };
                        }
                        let name = self.by_path.get(p).map(|&i| self.apps[i].name.as_str()).unwrap_or("");
                        unsafe { t.label.setString(Some(&NSString::from_str(name))) };
                        let (x, y) = g.center(i);
                        t.layer.setPosition(CGPoint::new(x, y));
                        t.layer.setHidden(false);
                    }
                    None => t.layer.setHidden(true),
                }
            }
        });
    }

    fn sync_apps(&mut self, new: Vec<App>) -> Vec<(String, u64)> {
        let same = new.len() == self.apps.len()
            && new.iter().zip(&self.apps).all(|(a, b)| a.path == b.path && a.mtime == b.mtime);
        if same {
            return Vec::new();
        }
        let keep: HashSet<&str> = new.iter().map(|a| a.path.as_str()).collect();
        self.icons.retain(|p, _| keep.contains(p.as_str()));
        let jobs = new
            .iter()
            .filter(|a| self.icons.get(&a.path).is_none_or(|(m, _)| *m != a.mtime))
            .map(|a| (a.path.clone(), a.mtime))
            .collect();
        self.by_path = new.iter().enumerate().map(|(i, a)| (a.path.clone(), i)).collect();
        self.apps = new;
        if layout::reconcile(&mut self.items, &self.apps) {
            layout::save(&self.items);
        }
        self.sync_tiles();
        jobs
    }

    fn icon_ready(&mut self, path: String, mtime: u64, img: CFRetained<CGImage>) {
        if let Some(t) = self.tiles.get(&Key::App(path.clone())) {
            unsafe { t.icon.setContents(Some(as_obj(&*img))) };
        }
        // the frequently used row has its own tiles. an icon that lands after the grid
        // opened has to reach those too, or the row sits there blank until next time
        for (i, p) in self.fav.iter().enumerate() {
            if *p == path {
                if let Some(t) = self.fav_tiles.get(i) {
                    no_anim(|| unsafe { t.icon.setContents(Some(as_obj(&*img))) });
                }
            }
        }
        self.icons.insert(path.clone(), (mtime, img));
        let ids: Vec<u64> = self
            .items
            .iter()
            .filter_map(|e| match e {
                Entry::Folder(f) if f.apps.iter().take(9).any(|p| *p == path) => Some(f.id),
                _ => None,
            })
            .collect();
        for id in ids {
            self.refresh_folder_tile(id);
        }
    }

    fn set_geo(&mut self, g: Geo) {
        self.geo = g;
        no_anim(|| {
            for l in [&self.content, &self.dim, &self.drag_layer] {
                l.setFrame(rect(0.0, 0.0, g.w, g.h));
            }
            let sw = (g.w * 0.22).clamp(220.0, 320.0);
            let sh = 30.0;
            self.search_bg.setFrame(rect(((g.w - sw) / 2.0).round(), ((g.top - sh) * 0.5).round(), sw, sh));
            self.search_text.setFrame(rect(10.0, 6.0, sw - 20.0, 20.0));
            for d in self.dots.drain(..) {
                d.removeFromSuperlayer();
            }
        });
        for t in self.tiles.values().chain(self.fav_tiles.iter()) {
            self.size_tile(t);
        }
    }

    fn rebuild_visible(&mut self) {
        if self.searching() {
            let q = self.query.to_lowercase();
            let mut v: Vec<(i32, usize)> = self
                .apps
                .iter()
                .enumerate()
                .filter_map(|(i, a)| {
                    index::score(a, &q).map(|s| {
                        let u = self.usage.get(&a.path).copied().unwrap_or(0);
                        (s + (u.min(45) as i32) * 20, i)
                    })
                })
                .collect();
            v.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            self.visible = v.into_iter().map(|(_, i)| Key::App(self.apps[i].path.clone())).collect();
        } else {
            self.visible = self.items.iter().map(key_of).collect();
        }
    }

    fn refilter(&mut self) {
        self.rebuild_visible();
        self.sel = if self.searching() && !self.visible.is_empty() { Some(0) } else { None };
        self.page = 0;
        self.offset = 0.0;
        no_anim(|| self.layout_grid());
    }

    fn layout_grid(&mut self) {
        let g = self.geo;
        let gap = self.drag.as_ref().filter(|d| d.in_folder.is_none()).map(|d| d.gap);
        let base = self.base();
        let open_apps: HashSet<&String> =
            self.open.and_then(|id| self.folder(id)).map(|f| f.apps.iter().collect()).unwrap_or_default();
        let dragging = self.drag.as_ref().map(|d| d.key.clone());
        let pos: HashMap<&Key, usize> = self
            .visible
            .iter()
            .enumerate()
            .map(|(i, k)| (k, base + if gap.is_some_and(|gp| i >= gp) { i + 1 } else { i }))
            .collect();
        for (k, t) in &self.tiles {
            if Some(k) == dragging.as_ref() || matches!(k, Key::App(p) if open_apps.contains(p)) {
                continue;
            }
            match pos.get(k) {
                Some(&i) => {
                    let (x, y) = g.center(i);
                    if t.layer.isHidden() {
                        no_anim(|| {
                            t.layer.setPosition(CGPoint::new(x, y));
                            t.layer.setHidden(false);
                        });
                    } else {
                        let cur = t.layer.position();
                        if (cur.x - x).abs() > 0.5 || (cur.y - y).abs() > 0.5 {
                            t.layer.setPosition(CGPoint::new(x, y));
                        }
                    }
                }
                None => no_anim(|| t.layer.setHidden(true)),
            }
        }
        no_anim(|| self.pages.setBounds(rect(0.0, 0.0, g.w * self.npages() as f64, g.h)));
        self.place_pages();
        self.update_dots();
        self.update_sel();
        self.update_search();
        self.update_merge();
        self.layout_favs();
    }

    fn place_pages(&self) {
        let mut off = self.offset;
        let last = self.npages() - 1;
        if (self.page == 0 && off > 0.0) || (self.page == last && off < 0.0) {
            off *= 0.3;
        }
        self.pages.setPosition(CGPoint::new(-(self.page as f64) * self.geo.w + off, 0.0));
    }

    fn animate_pages(&mut self) {
        animated(0.38, || {
            self.place_pages();
            self.update_dots();
        });
    }

    fn update_dots(&mut self) {
        let n = self.npages();
        if self.dots.len() != n {
            no_anim(|| {
                for d in self.dots.drain(..) {
                    d.removeFromSuperlayer();
                }
                let g = self.geo;
                let (sz, gap) = (7.5, 10.0);
                let total = n as f64 * sz + (n as f64 - 1.0) * gap;
                let x0 = (g.w - total) / 2.0;
                let y = g.h - g.bottom * 0.55;
                for i in 0..n {
                    let d = CALayer::new();
                    d.setBackgroundColor(Some(&gray(1.0, 1.0)));
                    d.setCornerRadius(sz / 2.0);
                    d.setFrame(rect(x0 + i as f64 * (sz + gap), y, sz, sz));
                    self.dots_layer.addSublayer(&d);
                    self.dots.push(d);
                }
                self.dots_layer.setHidden(n < 2);
            });
        }
        for (i, d) in self.dots.iter().enumerate() {
            d.setOpacity(if i == self.page { 0.95 } else { 0.35 });
        }
    }

    fn highlight_rect(&self, cx: f64, cy: f64, cell_h: f64) -> CGRect {
        let g = self.geo;
        let ir = g.icon_rect();
        let w = (g.icon + 30.0).min(g.cell_w);
        let h = g.icon + LABEL_GAP + LABEL_H + 22.0;
        rect(cx - w / 2.0, cy - cell_h / 2.0 + ir.origin.y - 11.0 + (cell_h - g.cell_h) / 2.0, w, h)
    }

    // the highlight slides from one app to the next instead of blinking across
    fn update_sel(&self) {
        let l = &self.sel_layer;
        match self.sel {
            Some(pos) if self.drag.is_none() => {
                let (x, y) = self.geo.center(pos + self.base());
                let to = self.highlight_rect(x, y, self.geo.cell_h);
                if l.isHidden() {
                    no_anim(|| {
                        l.setFrame(to);
                        l.setHidden(false);
                    });
                } else {
                    glide(l, to, 0.22);
                }
            }
            _ => no_anim(|| l.setHidden(true)),
        }
    }

    fn update_merge(&mut self) {
        let hint = self.drag.as_ref().and_then(|d| d.merge).and_then(|m| self.visible.get(m).cloned());
        if hint != self.merge_hint {
            let ident = unsafe { CATransform3DIdentity };
            animated(0.18, || {
                if let Some(t) = self.merge_hint.as_ref().and_then(|k| self.tiles.get(k)) {
                    t.icon.setTransform(ident);
                }
                if let Some(t) = hint.as_ref().and_then(|k| self.tiles.get(k)) {
                    t.icon.setTransform(CATransform3D::new_scale(1.12, 1.12, 1.0));
                }
            });
            self.merge_hint = hint;
        }
        let target = self.drag.as_ref().and_then(|d| d.merge.map(|m| (m, d.gap)));
        no_anim(|| match target {
            Some((m, gap)) => {
                let slot = self.base() + if m >= gap { m + 1 } else { m };
                let (x, y) = self.geo.center(slot);
                let s = self.geo.icon * 1.12;
                let ir = self.geo.icon_rect();
                let top = y - self.geo.cell_h / 2.0 + ir.origin.y;
                self.merge_layer.setFrame(rect(x - s / 2.0, top + (self.geo.icon - s) / 2.0, s, s));
                self.merge_layer.setCornerRadius(s * 0.24);
                self.merge_layer.setHidden(false);
            }
            None => self.merge_layer.setHidden(true),
        });
    }

    fn update_search(&self) {
        let (s, a) = if self.query.is_empty() { ("Search", 0.55) } else { (self.query.as_str(), 1.0) };
        no_anim(|| {
            unsafe { self.search_text.setString(Some(&NSString::from_str(s))) };
            self.search_text.setForegroundColor(Some(&gray(1.0, a)));
        });
    }

    fn go_page(&mut self, p: usize) {
        self.page = p.min(self.npages() - 1);
        self.offset = 0.0;
        self.animate_pages();
    }

    fn snap(&mut self) {
        let th = self.geo.w * 0.12;
        let flick = self.vel.abs() > 8.0 && self.last_move.elapsed() < Duration::from_millis(90);
        let (fwd, back) = (
            self.offset < -th || (flick && self.vel < 0.0 && self.offset < 0.0),
            self.offset > th || (flick && self.vel > 0.0 && self.offset > 0.0),
        );
        let mut p = self.page;
        if fwd && p + 1 < self.npages() {
            p += 1;
        } else if back && p > 0 {
            p -= 1;
        }
        self.vel = 0.0;
        self.go_page(p);
    }

    fn track(&mut self, d: f64) {
        self.vel = self.vel * 0.5 + d * 0.5;
        self.last_move = Instant::now();
    }

    fn main_cell(&self, p: NSPoint) -> Option<(usize, bool)> {
        let g = self.geo;
        let (lx, ly) = (p.x - g.side, p.y - g.top);
        if lx < 0.0 || ly < 0.0 {
            return None;
        }
        let (col, row) = ((lx / g.cell_w) as usize, (ly / g.cell_h) as usize);
        if col >= COLS || row >= ROWS {
            return None;
        }
        let (cx, cy) = (lx - col as f64 * g.cell_w - g.cell_w / 2.0, ly - row as f64 * g.cell_h);
        let ir = g.icon_rect();
        let icy = cy - ir.origin.y - g.icon / 2.0;
        let centre = cx.abs() < g.icon * 0.38 && icy.abs() < g.icon * 0.38;
        Some((self.page * PER_PAGE + row * COLS + col, centre))
    }

    fn hit(&self, p: NSPoint) -> Option<Press> {
        let raw = self.hit_raw(p)?;
        let base = self.base();
        if raw < base {
            return (raw < self.fav.len()).then_some(Press::Fav(raw));
        }
        (raw - base < self.visible.len()).then_some(Press::Main(raw - base))
    }

    fn hit_raw(&self, p: NSPoint) -> Option<usize> {
        let g = self.geo;
        let (slot, _) = self.main_cell(p)?;
        let (cx, cy) = g.center(slot);
        let cx = cx - self.page as f64 * g.w;
        let ir = g.icon_rect();
        let top = cy - g.cell_h / 2.0 + ir.origin.y;
        let ok = (p.x - cx).abs() <= g.icon / 2.0 + 12.0
            && p.y >= top - 12.0
            && p.y <= top + g.icon + LABEL_GAP + LABEL_H + 12.0;
        ok.then_some(slot)
    }

    fn tile_for_press(&self, pr: Press) -> Option<&Tile> {
        match pr {
            Press::Main(pos) => self.visible.get(pos).and_then(|k| self.tiles.get(k)),
            Press::Panel(i) => {
                let f = self.folder(self.open?)?;
                self.tiles.get(&Key::App(f.apps.get(i)?.clone()))
            }
            Press::Fav(i) => self.fav_tiles.get(i),
        }
    }

    fn press(&mut self, pr: Option<Press>) {
        if let Some(old) = self.pressed.take() {
            if let Some(t) = self.tile_for_press(old) {
                no_anim(|| t.icon.setOpacity(1.0));
            }
        }
        self.pressed = pr;
        if let Some(t) = pr.and_then(|p| self.tile_for_press(p)) {
            no_anim(|| t.icon.setOpacity(0.55));
        }
    }

    fn launch(&mut self, path: String) -> Act {
        if is_running(&path) {
            self.note_use(&path);
        }
        Act::Launch(path)
    }

    fn note_use(&mut self, path: &str) {
        *self.usage.entry(path.to_owned()).or_insert(0) += 1;
        index::save_usage(&self.usage);
    }

    fn activate_pos(&mut self, pos: usize) -> Act {
        match self.visible.get(pos).cloned() {
            Some(Key::App(p)) => self.launch(p),
            Some(Key::Folder(id)) => {
                self.open_folder(id);
                Act::None
            }
            None => Act::None,
        }
    }

    fn arrow(&mut self, code: u16) {
        if let Some(id) = self.open {
            let n = self.folder(id).map_or(0, |f| f.apps.len());
            if n == 0 {
                return;
            }
            let s = self.psel.unwrap_or(0);
            self.psel = Some(match (self.psel, code) {
                (None, _) => 0,
                (_, 123) => s.saturating_sub(1),
                (_, 124) => (s + 1).min(n - 1),
                (_, 125) if s + COLS < n => s + COLS,
                (_, 126) if s >= COLS => s - COLS,
                _ => s,
            });
            self.update_panel_sel();
            return;
        }
        let n = self.visible.len();
        if n == 0 {
            return;
        }
        let new = match self.sel {
            None if code == 123 => return self.go_page(self.page.saturating_sub(1)),
            None if code == 124 => return self.go_page(self.page + 1),
            None => (self.page * PER_PAGE).saturating_sub(self.base()).min(n - 1),
            Some(s) => match code {
                123 => s.saturating_sub(1),
                124 => (s + 1).min(n - 1),
                125 if s + COLS < n => s + COLS,
                126 if s >= COLS => s - COLS,
                _ => s,
            },
        };
        self.sel = Some(new);
        self.update_sel();
        let pg = (new + self.base()) / PER_PAGE;
        if pg != self.page {
            self.go_page(pg);
        }
    }

    fn open_folder(&mut self, id: u64) {
        let Some(n) = self.folder(id).map(|f| f.apps.len()) else { return };
        self.open = Some(id);
        self.psel = None;
        self.renaming = false;
        self.sel = None;
        self.pgeo = PanelGeo::new(&self.geo, n);
        let apps = self.folder(id).unwrap().apps.clone();
        no_anim(|| {
            for p in &apps {
                if let Some(t) = self.tiles.get(&Key::App(p.clone())) {
                    t.layer.removeFromSuperlayer();
                    self.panel.addSublayer(&t.layer);
                    t.layer.setHidden(false);
                }
            }
            self.layout_panel(None);
            self.update_title();
            self.update_sel();
            self.dim.setHidden(false);
            self.panel.setHidden(false);
            self.panel.setOpacity(1.0);
            // it may still be on its way out from the last close
            self.panel.removeAllAnimations();
            self.panel.setTransform(unsafe { CATransform3DIdentity });
        });
        pop_in(&self.panel, 0.92, 0.24);
        animated(0.2, || self.content.setOpacity(0.12));
    }

    fn close_folder(&mut self) {
        let Some(id) = self.open.take() else { return };
        self.renaming = false;
        let apps = self.folder(id).map(|f| f.apps.clone()).unwrap_or_default();
        no_anim(|| {
            for p in &apps {
                if let Some(t) = self.tiles.get(&Key::App(p.clone())) {
                    t.layer.removeFromSuperlayer();
                    self.pages.addSublayer(&t.layer);
                    t.layer.setHidden(true);
                }
            }
            self.dim.setHidden(true);
            self.panel_sel.setHidden(true);
        });
        // the folder sinks back a touch and fades, quicker than it opened
        let away = CAMediaTimingFunction::functionWithControlPoints(0.4, 0.0, 1.0, 1.0);
        CATransaction::begin();
        CATransaction::setAnimationDuration(0.14);
        CATransaction::setAnimationTimingFunction(Some(&away));
        let done = block2::RcBlock::new(|| {
            with(|st| {
                if st.open.is_none() {
                    no_anim(|| {
                        st.panel.setHidden(true);
                        st.panel.setOpacity(1.0);
                        st.panel.setTransform(unsafe { CATransform3DIdentity });
                    });
                }
            });
        });
        // safety: the block is a plain rust closure with nothing borrowed
        unsafe { CATransaction::setCompletionBlock(Some(&done)) };
        self.panel.setOpacity(0.0);
        self.panel.setTransform(CATransform3D::new_scale(0.96, 0.96, 1.0));
        CATransaction::commit();
        animated(0.2, || self.content.setOpacity(1.0));
        let before = self.items.len();
        layout::normalize(&mut self.items);
        if before != self.items.len() || self.items.iter().all(|e| !matches!(e, Entry::Folder(f) if f.id == id)) {
            self.sync_tiles();
        }
        self.refresh_folder_tile(id);
        layout::save(&self.items);
        self.rebuild_visible();
        no_anim(|| self.layout_grid());
    }

    fn layout_panel(&mut self, gap: Option<usize>) {
        let Some(id) = self.open else { return };
        let Some(apps) = self.folder(id).map(|f| f.apps.clone()) else { return };
        let n = apps.len() + usize::from(gap.is_some());
        let pg = PanelGeo::new(&self.geo, n);
        self.pgeo = pg;
        self.panel.setFrame(rect(pg.x, pg.y, pg.w, pg.h));
        self.panel_title.setFrame(rect(pg.pad, 20.0, pg.w - 2.0 * pg.pad, 30.0));
        let dragging = self.drag.as_ref().map(|d| d.key.clone());
        for (i, p) in apps.iter().enumerate() {
            let k = Key::App(p.clone());
            if Some(&k) == dragging.as_ref() {
                continue;
            }
            if let Some(t) = self.tiles.get(&k) {
                let slot = if gap.is_some_and(|g| i >= g) { i + 1 } else { i };
                let (x, y) = pg.center(&self.geo, slot);
                t.layer.setPosition(CGPoint::new(x, y));
            }
        }
        self.update_panel_sel();
    }

    fn update_panel_sel(&self) {
        no_anim(|| match self.psel {
            Some(i) if self.drag.is_none() => {
                let (x, y) = self.pgeo.center(&self.geo, i);
                self.panel_sel.setFrame(self.highlight_rect(x, y, self.geo.cell_h));
                self.panel_sel.setHidden(false);
            }
            _ => self.panel_sel.setHidden(true),
        });
    }

    fn update_title(&self) {
        let name = self.open.and_then(|id| self.folder(id)).map(|f| f.name.clone()).unwrap_or_default();
        let s = if self.renaming { format!("{name}▏") } else { name };
        no_anim(|| unsafe { self.panel_title.setString(Some(&NSString::from_str(&s))) });
    }

    fn title_hit(&self, p: NSPoint) -> bool {
        let pg = self.pgeo;
        self.open.is_some() && pg.contains(p) && p.y < pg.y + pg.title_h
    }

    fn end_rename(&mut self) {
        if !self.renaming {
            return;
        }
        self.renaming = false;
        if let Some(id) = self.open {
            if let Some(f) = self.folder_mut(id) {
                if f.name.trim().is_empty() {
                    f.name = "Folder".into();
                }
            }
            self.refresh_folder_tile(id);
        }
        layout::save(&self.items);
        self.update_title();
    }

    fn begin_drag(&mut self, pr: Press, p: NSPoint) {
        let g = self.geo;
        let (entry, key, in_folder, gap, center) = match pr {
            Press::Fav(_) => return,
            Press::Main(pos) => {
                if pos >= self.items.len() {
                    return;
                }
                let e = self.items.remove(pos);
                let k = key_of(&e);
                let (x, y) = g.center(pos + self.base());
                let c = (x - self.page as f64 * g.w + self.offset, y);
                (e, k, None, pos, c)
            }
            Press::Panel(i) => {
                let Some(id) = self.open else { return };
                let pg = self.pgeo;
                let (x, y) = pg.center(&g, i);
                let Some(f) = self.folder_mut(id) else { return };
                if i >= f.apps.len() {
                    return;
                }
                let path = f.apps.remove(i);
                (Entry::App(path.clone()), Key::App(path), Some(id), i, (pg.x + x, pg.y + y))
            }
        };
        self.press(None);
        self.sel = None;
        self.psel = None;
        if let Some(t) = self.tiles.get(&key) {
            no_anim(|| {
                t.layer.removeFromSuperlayer();
                self.drag_layer.addSublayer(&t.layer);
                t.layer.setPosition(CGPoint::new(center.0, center.1));
                t.layer.setHidden(false);
            });
            animated(0.15, || {
                t.layer.setTransform(CATransform3D::new_scale(1.12, 1.12, 1.0));
                t.layer.setOpacity(0.9);
                t.label.setOpacity(0.0);
            });
        }
        self.drag = Some(Drag {
            entry,
            key,
            in_folder,
            grab: (p.x - center.0, p.y - center.1),
            gap,
            merge: None,
            last: p,
            edge_since: None,
            pending: None,
        });
        if in_folder.is_none() {
            self.rebuild_visible();
            no_anim(|| self.layout_grid());
        }
    }

    fn update_drag(&mut self, p: NSPoint) {
        let Some(d) = self.drag.as_mut() else { return };
        d.last = p;
        let (gx, gy) = d.grab;
        let key = d.key.clone();
        if let Some(t) = self.tiles.get(&key) {
            no_anim(|| t.layer.setPosition(CGPoint::new(p.x - gx, p.y - gy)));
        }
        let g = self.geo;

        if let Some(id) = self.drag.as_ref().unwrap().in_folder {
            if self.pgeo.contains(p) {
                let n = self.folder(id).map_or(0, |f| f.apps.len());
                let cell = self.pgeo.cell(&g, p).unwrap_or(n).min(n);
                if cell != self.drag.as_ref().unwrap().gap {
                    self.drag.as_mut().unwrap().gap = cell;
                    animated(0.25, || self.layout_panel(Some(cell)));
                }
                return;
            }
            self.drag.as_mut().unwrap().in_folder = None;
            self.close_folder();
            let gap = self.visible.len();
            self.drag.as_mut().unwrap().gap = gap;
            no_anim(|| self.layout_grid());
        }

        let at_edge = p.x < EDGE || p.x > g.w - EDGE;
        let d = self.drag.as_mut().unwrap();
        if at_edge {
            if d.edge_since.is_none() {
                d.edge_since = Some(Instant::now());
                schedule_edge_check();
            }
        } else {
            d.edge_since = None;
        }

        let Some((raw, centre)) = self.main_cell(p) else { return };
        let base = self.base();
        let slot = raw.saturating_sub(base);
        let centre = centre && raw >= base;
        let gap = self.drag.as_ref().unwrap().gap;
        let n = self.visible.len();
        let target = if slot < gap { Some(slot) } else if slot > gap { Some(slot - 1) } else { None };
        let can_merge = matches!(key, Key::App(_));
        let merge = target.filter(|&t| centre && can_merge && t < n);
        if merge != self.drag.as_ref().unwrap().merge {
            self.drag.as_mut().unwrap().merge = merge;
            self.update_merge();
        }
        let d = self.drag.as_mut().unwrap();
        let new_gap = slot.min(n);
        if merge.is_some() || centre || new_gap == gap {
            d.pending = None;
            return;
        }
        match d.pending {
            Some((s, t)) if s == new_gap && t.elapsed() >= Duration::from_millis(200) => {
                d.pending = None;
                d.gap = new_gap;
                animated(0.25, || self.layout_grid());
            }
            Some((s, _)) if s == new_gap => {}
            _ => {
                d.pending = Some((new_gap, Instant::now()));
                schedule_drag_tick();
            }
        }
    }

    fn edge_tick(&mut self) -> bool {
        let g = self.geo;
        let Some(d) = self.drag.as_mut() else { return false };
        let Some(since) = d.edge_since else { return false };
        if d.in_folder.is_some() {
            return false;
        }
        if since.elapsed() < Duration::from_millis(550) {
            return true;
        }
        let left = d.last.x < EDGE;
        d.edge_since = Some(Instant::now());
        let np = self.npages();
        let p = if left { self.page.saturating_sub(1) } else { (self.page + 1).min(np - 1) };
        if p != self.page {
            self.go_page(p);
            let last = self.drag.as_ref().unwrap().last;
            let _ = g;
            self.update_drag(last);
        }
        true
    }

    fn end_drag(&mut self) {
        let Some(d) = self.drag.take() else { return };
        let tile_pos = self.tiles.get(&d.key).map(|t| t.layer.position());
        let reparent = |st: &Self, into: &CALayer, to: CGPoint| {
            if let Some(t) = st.tiles.get(&d.key) {
                no_anim(|| {
                    t.layer.removeFromSuperlayer();
                    into.addSublayer(&t.layer);
                    t.layer.setPosition(to);
                });
                animated(0.25, || {
                    t.layer.setTransform(unsafe { CATransform3DIdentity });
                    t.layer.setOpacity(1.0);
                    t.label.setOpacity(1.0);
                });
            }
        };

        if let Some(id) = d.in_folder {
            let Entry::App(path) = d.entry else { return };
            if let Some(f) = self.folder_mut(id) {
                let at = d.gap.min(f.apps.len());
                f.apps.insert(at, path);
            }
            let tp = tile_pos.unwrap_or_default();
            let local = CGPoint::new(tp.x - self.pgeo.x, tp.y - self.pgeo.y);
            reparent(self, &self.panel.clone(), local);
            animated(0.25, || self.layout_panel(None));
            self.refresh_folder_tile(id);
            layout::save(&self.items);
            return;
        }

        let g = self.geo;
        let tp = tile_pos.unwrap_or_default();
        let in_pages = CGPoint::new(tp.x + self.page as f64 * g.w - self.offset, tp.y);
        match (d.merge, d.entry) {
            (Some(t), Entry::App(path)) if t < self.items.len() => {
                match &mut self.items[t] {
                    Entry::Folder(f) => {
                        f.apps.push(path.clone());
                        let id = f.id;
                        self.refresh_folder_tile(id);
                    }
                    Entry::App(other) => {
                        let other = other.clone();
                        let name = layout::auto_name(&other, &path);
                        let id = layout::new_id();
                        self.items[t] = Entry::Folder(Folder { id, name, apps: vec![other, path.clone()] });
                        self.add_tile(Key::Folder(id));
                    }
                }
                if let Some(tl) = self.tiles.get(&d.key) {
                    no_anim(|| {
                        tl.layer.removeFromSuperlayer();
                        self.pages.addSublayer(&tl.layer);
                        tl.layer.setHidden(true);
                        tl.layer.setTransform(unsafe { CATransform3DIdentity });
                        tl.layer.setOpacity(1.0);
                        tl.label.setOpacity(1.0);
                    });
                }
            }
            (_, entry) => {
                let at = d.gap.min(self.items.len());
                self.items.insert(at, entry);
                reparent(self, &self.pages.clone(), in_pages);
            }
        }
        layout::save(&self.items);
        self.rebuild_visible();
        let np = self.npages();
        self.page = self.page.min(np - 1);
        animated(0.25, || self.layout_grid());
        self.update_merge();
    }
}

fn schedule_drag_tick() {
    let when = dispatch2::DispatchTime::try_from(Duration::from_millis(210)).unwrap();
    let _ = dispatch2::DispatchQueue::main().after(when, || {
        with(|st| {
            if let Some(p) = st.drag.as_ref().filter(|d| d.pending.is_some()).map(|d| d.last) {
                st.update_drag(p);
            }
        });
    });
}

fn schedule_edge_check() {
    let when = dispatch2::DispatchTime::try_from(Duration::from_millis(150)).unwrap();
    let _ = dispatch2::DispatchQueue::main().after(when, || {
        if with(|st| st.edge_tick()).unwrap_or(false) {
            schedule_edge_check();
        }
    });
}

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "LiftoffGridView"]
    pub struct GridView;

    impl GridView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }
        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _e: Option<&NSEvent>) -> bool {
            true
        }
        #[unsafe(method(keyDown:))]
        fn key_down(&self, e: &NSEvent) {
            on_key(e);
        }
        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, e: &NSEvent) {
            on_scroll(e);
        }
        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, e: &NSEvent) {
            let p = self.loc(e);
            with(|st| on_mouse_down(st, p));
        }
        #[unsafe(method(mouseDragged:))]
        fn mouse_dragged(&self, e: &NSEvent) {
            let p = self.loc(e);
            with(|st| on_mouse_dragged(st, p));
        }
        #[unsafe(method(mouseUp:))]
        fn mouse_up(&self, e: &NSEvent) {
            let p = self.loc(e);
            let act = with(|st| on_mouse_up(st, p));
            perform(act.unwrap_or(Act::None));
        }
    }
);

impl GridView {
    fn loc(&self, e: &NSEvent) -> NSPoint {
        self.convertPoint_fromView(e.locationInWindow(), None)
    }
}

define_class!(
    #[unsafe(super(NSWindow))]
    #[thread_kind = MainThreadOnly]
    #[name = "LiftoffWindow"]
    pub struct LiftoffWindow;

    impl LiftoffWindow {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key(&self) -> bool {
            true
        }
        #[unsafe(method(canBecomeMainWindow))]
        fn can_become_main(&self) -> bool {
            true
        }
    }
);

fn on_mouse_down(st: &mut State, p: NSPoint) {
    st.mouse_start = Some(p);
    st.paging = false;
    if st.open.is_some() {
        if st.title_hit(p) {
            st.renaming = true;
            st.update_title();
            return;
        }
        st.end_rename();
        if st.pgeo.contains(p) {
            let n = st.open.and_then(|id| st.folder(id)).map_or(0, |f| f.apps.len());
            let hit = st.pgeo.cell(&st.geo, p).filter(|&i| i < n);
            st.press(hit.map(Press::Panel));
        } else {
            st.press(None);
        }
        return;
    }
    let h = st.hit(p);
    st.press(h);
}

fn on_mouse_dragged(st: &mut State, p: NSPoint) {
    if st.drag.is_some() {
        st.update_drag(p);
        return;
    }
    let Some(s) = st.mouse_start else { return };
    let (dx, dy) = (p.x - s.x, p.y - s.y);
    if !st.paging && dx.hypot(dy) > 6.0 {
        if let (Some(pr @ (Press::Main(_) | Press::Panel(_))), false) = (st.pressed, st.searching()) {
            st.begin_drag(pr, p);
            st.update_drag(p);
            return;
        }
        if st.open.is_some() {
            return;
        }
        st.paging = true;
        st.press(None);
        st.vel = 0.0;
        st.prev_x = p.x;
    }
    if st.paging {
        let d = p.x - st.prev_x;
        st.prev_x = p.x;
        st.track(d);
        st.offset = dx;
        no_anim(|| st.place_pages());
    }
}

fn on_mouse_up(st: &mut State, p: NSPoint) -> Act {
    st.mouse_start = None;
    if st.drag.is_some() {
        st.end_drag();
        return Act::None;
    }
    if st.paging {
        st.paging = false;
        st.snap();
        return Act::None;
    }
    let pressed = st.pressed;
    st.press(None);
    if st.open.is_some() {
        if st.renaming {
            return Act::None;
        }
        if !st.pgeo.contains(p) {
            st.close_folder();
            return Act::None;
        }
        if let Some(Press::Panel(i)) = pressed {
            let n = st.open.and_then(|id| st.folder(id)).map_or(0, |f| f.apps.len());
            if st.pgeo.cell(&st.geo, p) == Some(i) && i < n {
                let path = st.folder(st.open.unwrap()).unwrap().apps[i].clone();
                return st.launch(path);
            }
        }
        return Act::None;
    }
    let h = st.hit(p);
    let in_search = {
        let f = st.search_bg.frame();
        p.x >= f.origin.x - 20.0 && p.x <= f.origin.x + f.size.width + 20.0 && p.y <= f.origin.y + f.size.height + 10.0
    };
    match (pressed, h) {
        (Some(Press::Main(a)), Some(Press::Main(b))) if a == b => st.activate_pos(a),
        (Some(Press::Fav(a)), Some(Press::Fav(b))) if a == b => {
            let path = st.fav[a].clone();
            st.launch(path)
        }
        (None, None) if !in_search => Act::Hide,
        _ => Act::None,
    }
}

fn on_key(e: &NSEvent) {
    let code = e.keyCode();
    let cmd = e.modifierFlags().contains(NSEventModifierFlags::Command);
    let chars = e.characters().map(|s| s.to_string()).unwrap_or_default();
    let typed: String = chars
        .chars()
        .filter(|c| !c.is_control() && !('\u{F700}'..='\u{F8FF}').contains(c))
        .collect();
    let act = with(|st| {
        if st.drag.is_some() {
            return Act::None;
        }
        if st.renaming {
            let id = st.open.unwrap();
            match code {
                36 | 76 | 53 => st.end_rename(),
                51 | 117 => {
                    if let Some(f) = st.folder_mut(id) {
                        f.name.pop();
                    }
                    st.update_title();
                }
                _ if !cmd && !typed.is_empty() => {
                    if let Some(f) = st.folder_mut(id) {
                        f.name.push_str(&typed);
                    }
                    st.update_title();
                }
                _ => {}
            }
            return Act::None;
        }
        match code {
            53 => {
                if st.open.is_some() {
                    st.close_folder();
                } else if st.searching() {
                    st.query.clear();
                    st.refilter();
                } else {
                    return Act::Hide;
                }
            }
            36 | 76 => {
                if let Some(id) = st.open {
                    if let Some(path) = st.psel.and_then(|i| st.folder(id)?.apps.get(i).cloned()) {
                        return st.launch(path);
                    }
                } else if let Some(s) = st.sel {
                    return st.activate_pos(s);
                }
            }
            51 | 117 => {
                if cmd {
                    st.query.clear();
                } else {
                    st.query.pop();
                }
                st.refilter();
            }
            123..=126 => st.arrow(code),
            _ if cmd => match chars.as_str() {
                "q" => return Act::Quit,
                "w" | "h" => return Act::Hide,
                _ => {}
            },
            _ => {
                if !typed.is_empty() && !(st.query.is_empty() && typed.trim().is_empty()) {
                    st.close_folder();
                    st.query.push_str(&typed);
                    st.refilter();
                }
            }
        }
        Act::None
    });
    perform(act.unwrap_or(Act::None));
}

fn on_scroll(e: &NSEvent) {
    let (dx, dy) = (e.scrollingDeltaX(), e.scrollingDeltaY());
    with(|st| {
        if st.open.is_some() || st.drag.is_some() {
            return;
        }
        if e.hasPreciseScrollingDeltas() {
            if e.momentumPhase() != NSEventPhase::None {
                return;
            }
            let ph = e.phase();
            if ph.contains(NSEventPhase::Began) {
                st.swiping = true;
                st.offset = 0.0;
                st.vel = 0.0;
                st.press(None);
            }
            if st.swiping && (ph.contains(NSEventPhase::Changed) || ph.contains(NSEventPhase::Began)) {
                let d = if dx.abs() >= dy.abs() { dx } else { dy };
                st.offset += d;
                st.track(d);
                no_anim(|| st.place_pages());
            }
            if st.swiping && (ph.contains(NSEventPhase::Ended) || ph.contains(NSEventPhase::Cancelled)) {
                st.swiping = false;
                st.snap();
            }
        } else {
            let d = if dy.abs() > dx.abs() { dy } else { dx };
            if d == 0.0 || st.wheel_last.elapsed() < Duration::from_millis(250) {
                return;
            }
            st.wheel_last = Instant::now();
            let p = if d < 0.0 { st.page + 1 } else { st.page.saturating_sub(1) };
            st.go_page(p);
        }
    });
}

fn perform(act: Act) {
    match act {
        Act::None => {}
        Act::Hide => hide(),
        Act::Quit => {
            let mtm = MainThreadMarker::new().unwrap();
            NSApplication::sharedApplication(mtm).terminate(None);
        }
        Act::Launch(path) => {
            hide();
            let url = NSURL::fileURLWithPath(&NSString::from_str(&path));
            NSWorkspace::sharedWorkspace().openURL(&url);
        }
    }
}

pub(crate) fn mouse_screen(mtm: MainThreadMarker) -> Option<Retained<NSScreen>> {
    let m = NSEvent::mouseLocation();
    NSScreen::screens(mtm)
        .iter()
        .find(|s| {
            let f = s.frame();
            m.x >= f.origin.x && m.x < f.origin.x + f.size.width && m.y >= f.origin.y && m.y < f.origin.y + f.size.height
        })
        .or_else(|| NSScreen::mainScreen(mtm))
}

fn spawn_icons(jobs: Vec<(String, u64)>, px: usize) {
    icons::render_all(jobs, px, |path, mtime, img| {
        dispatch2::DispatchQueue::main().exec_async(move || {
            with(|st| st.icon_ready(path, mtime, img));
        });
    });
}

pub fn is_visible() -> bool {
    VISIBLE.with(|v| v.get())
}

pub fn toggle() {
    if is_visible() { hide() } else { show() }
}

pub fn show() {
    crate::search::hide();
    let t0 = Instant::now();
    let mtm = MainThreadMarker::new().unwrap();
    let apps = index::scan();
    let t_scan = t0.elapsed();
    let frame = mouse_screen(mtm).map(|s| s.frame());
    let Some((window, jobs, px)) = with(|st| {
        let jobs = st.sync_apps(apps);
        if let Some(f) = frame {
            let g = Geo::new(f.size.width, f.size.height);
            if g != st.geo {
                st.set_geo(g);
            }
        }
        st.query.clear();
        st.compute_favs();
        st.refilter();
        no_anim(|| st.content.setTransform(unsafe { CATransform3DIdentity }));
        pop_in(&st.content, 1.07, 0.26);
        (st.window.clone(), jobs, st.icon_px)
    }) else {
        return;
    };
    spawn_icons(jobs, px);
    if let Some(f) = frame {
        if window.frame() != f {
            window.setFrame_display(f, false);
        }
    }
    VISIBLE.with(|v| v.set(true));
    bump_gen();
    window.setIgnoresMouseEvents(false);
    window.setAlphaValue(0.0);
    window.makeKeyAndOrderFront(None);
    NSApplication::sharedApplication(mtm).activate();
    NSAnimationContext::beginGrouping();
    NSAnimationContext::currentContext().setDuration(0.16);
    window.animator().setAlphaValue(1.0);
    NSAnimationContext::endGrouping();
    if debug() {
        eprintln!("show: scan {:?}, total {:?}", t_scan, t0.elapsed());
    }
}

pub fn hide() {
    if !is_visible() {
        return;
    }
    VISIBLE.with(|v| v.set(false));
    let my_gen = bump_gen();
    let Some(window) = with(|st| {
        st.press(None);
        st.end_drag();
        st.end_rename();
        st.close_folder();
        st.swiping = false;
        st.paging = false;
        animated(FADE_OUT, || st.content.setTransform(CATransform3D::new_scale(1.05, 1.05, 1.0)));
        st.window.clone()
    }) else {
        return;
    };
    window.setIgnoresMouseEvents(true);
    NSAnimationContext::beginGrouping();
    NSAnimationContext::currentContext().setDuration(FADE_OUT);
    window.animator().setAlphaValue(0.0);
    NSAnimationContext::endGrouping();

    let when = dispatch2::DispatchTime::try_from(Duration::from_secs_f64(FADE_OUT + 0.02)).unwrap();
    let _ = dispatch2::DispatchQueue::main().after(when, move || {
        if GEN.with(|g| g.get()) != my_gen || is_visible() {
            return;
        }
        let Some(window) = with(|st| {
            no_anim(|| st.content.setTransform(unsafe { CATransform3DIdentity }));
            st.window.clone()
        }) else {
            return;
        };
        window.orderOut(None);
        if !crate::search::is_visible() && !crate::browse::is_open() {
            let mtm = MainThreadMarker::new().unwrap();
            NSApplication::sharedApplication(mtm).hide(None);
        }
    });
}

pub fn setup(mtm: MainThreadMarker) {
    let t0 = Instant::now();
    let screen = NSScreen::mainScreen(mtm).expect("no screen");
    let frame = screen.frame();
    let scale = NSScreen::screens(mtm).iter().map(|s| s.backingScaleFactor()).fold(1.0, CGFloat::max);
    let max_icon = NSScreen::screens(mtm)
        .iter()
        .map(|s| Geo::new(s.frame().size.width, s.frame().size.height).icon)
        .fold(0.0, f64::max);
    let icon_px = (max_icon * scale).ceil() as usize;

    let window: Retained<LiftoffWindow> = unsafe {
        msg_send![
            LiftoffWindow::alloc(mtm),
            initWithContentRect: frame,
            styleMask: NSWindowStyleMask::Borderless,
            backing: NSBackingStoreType::Buffered,
            defer: false
        ]
    };
    let window: Retained<NSWindow> = Retained::into_super(window);
    unsafe { window.setReleasedWhenClosed(false) };
    window.setOpaque(false);
    window.setHasShadow(false);
    window.setBackgroundColor(Some(&objc2_app_kit::NSColor::clearColor()));
    window.setLevel(objc2_app_kit::NSMainMenuWindowLevel + 2);
    window.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary
            | NSWindowCollectionBehavior::Stationary
            | NSWindowCollectionBehavior::IgnoresCycle,
    );
    if let Some(dark) = NSAppearance::appearanceNamed(unsafe { NSAppearanceNameDarkAqua }) {
        window.setAppearance(Some(&dark));
    }

    let bounds = NSRect::new(NSPoint::new(0.0, 0.0), frame.size);
    let blur = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), bounds);
    blur.setMaterial(NSVisualEffectMaterial::FullScreenUI);
    blur.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
    blur.setState(NSVisualEffectState::Active);
    window.setContentView(Some(&blur));

    let view: Retained<GridView> = unsafe { msg_send![GridView::alloc(mtm), initWithFrame: bounds] };
    view.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable);
    let root = CALayer::new();
    root.setGeometryFlipped(true);
    root.setBackgroundColor(Some(&gray(0.0, 0.22)));
    view.setLayer(Some(&root));
    view.setWantsLayer(true);
    blur.addSubview(&view);
    window.makeFirstResponder(Some(&view));

    let content = CALayer::new();
    root.addSublayer(&content);
    let pages = CALayer::new();
    pages.setAnchorPoint(CGPoint::new(0.0, 0.0));
    content.addSublayer(&pages);
    let sel_layer = CALayer::new();
    sel_layer.setBackgroundColor(Some(&gray(1.0, 0.16)));
    sel_layer.setCornerRadius(18.0);
    sel_layer.setHidden(true);
    pages.addSublayer(&sel_layer);
    let merge_layer = CALayer::new();
    merge_layer.setBackgroundColor(Some(&gray(1.0, 0.28)));
    merge_layer.setHidden(true);
    pages.addSublayer(&merge_layer);

    let search_bg = CALayer::new();
    search_bg.setBackgroundColor(Some(&gray(1.0, 0.13)));
    search_bg.setBorderColor(Some(&gray(1.0, 0.22)));
    search_bg.setBorderWidth(0.5);
    search_bg.setCornerRadius(8.0);
    content.addSublayer(&search_bg);
    let search_text = CATextLayer::new();
    let sfont = NSFont::systemFontOfSize_weight(14.0, unsafe { NSFontWeightRegular });
    unsafe {
        search_text.setFont(Some(&*(Retained::as_ptr(&sfont) as *const CFType)));
        search_text.setAlignmentMode(kCAAlignmentCenter);
        search_text.setTruncationMode(kCATruncationEnd);
    }
    search_text.setFontSize(14.0);
    search_text.setContentsScale(scale);
    search_bg.addSublayer(&search_text);

    let dots_layer = CALayer::new();
    content.addSublayer(&dots_layer);

    let dim = CALayer::new();
    dim.setBackgroundColor(Some(&gray(0.0, 0.25)));
    dim.setHidden(true);
    root.addSublayer(&dim);
    let panel = CALayer::new();
    panel.setBackgroundColor(Some(&gray(0.11, 0.9)));
    panel.setBorderColor(Some(&gray(1.0, 0.12)));
    panel.setBorderWidth(0.5);
    panel.setCornerRadius(28.0);
    panel.setHidden(true);
    root.addSublayer(&panel);
    let panel_sel = CALayer::new();
    panel_sel.setBackgroundColor(Some(&gray(1.0, 0.16)));
    panel_sel.setCornerRadius(18.0);
    panel_sel.setHidden(true);
    panel.addSublayer(&panel_sel);
    let panel_title = CATextLayer::new();
    let tfont = NSFont::systemFontOfSize_weight(24.0, unsafe { NSFontWeightSemibold });
    unsafe {
        panel_title.setFont(Some(&*(Retained::as_ptr(&tfont) as *const CFType)));
        panel_title.setAlignmentMode(kCAAlignmentCenter);
        panel_title.setTruncationMode(kCATruncationEnd);
    }
    panel_title.setFontSize(24.0);
    panel_title.setForegroundColor(Some(&gray(1.0, 0.95)));
    panel_title.setContentsScale(scale);
    panel.addSublayer(&panel_title);

    let drag_layer = CALayer::new();
    root.addSublayer(&drag_layer);

    let fav_band = CALayer::new();
    // a soft shelf for the apps you use most. no outline: the shade alone sets it apart
    fav_band.setBackgroundColor(Some(&gray(1.0, 0.06)));
    fav_band.setCornerRadius(22.0);
    fav_band.setHidden(true);
    pages.insertSublayer_atIndex(&fav_band, 0);
    let fav_caption = CATextLayer::new();
    let cfont = NSFont::systemFontOfSize_weight(12.0, unsafe { NSFontWeightSemibold });
    unsafe {
        fav_caption.setFont(Some(&*(Retained::as_ptr(&cfont) as *const CFType)));
        fav_caption.setString(Some(&NSString::from_str("Frequently used")));
    }
    fav_caption.setFontSize(12.0);
    fav_caption.setForegroundColor(Some(&gray(1.0, 0.55)));
    fav_caption.setContentsScale(scale);
    fav_caption.setHidden(true);
    pages.addSublayer(&fav_caption);

    let label_font = NSFont::systemFontOfSize_weight(13.0, unsafe { NSFontWeightMedium });

    let st = State {
        window,
        content,
        pages,
        sel_layer,
        merge_layer,
        search_bg,
        search_text,
        dots_layer,
        dots: Vec::new(),
        dim,
        panel,
        panel_title,
        panel_sel,
        drag_layer,
        label_font,
        fav_band,
        fav_caption,
        fav_tiles: Vec::new(),
        fav: Vec::new(),
        apps: Vec::new(),
        by_path: HashMap::new(),
        items: layout::load().unwrap_or_default(),
        tiles: HashMap::new(),
        icons: HashMap::new(),
        usage: index::load_usage(),
        visible: Vec::new(),
        query: String::new(),
        sel: None,
        page: 0,
        offset: 0.0,
        geo: Geo::default(),
        scale,
        icon_px,
        open: None,
        pgeo: PanelGeo::default(),
        psel: None,
        renaming: false,
        swiping: false,
        vel: 0.0,
        last_move: Instant::now(),
        prev_x: 0.0,
        merge_hint: None,
        wheel_last: Instant::now(),
        mouse_start: None,
        pressed: None,
        paging: false,
        drag: None,
    };
    let first_run = st.items.is_empty();
    ST.with(|s| *s.borrow_mut() = Some(st));

    let apps = index::scan();
    let jobs = with(|st| {
        st.set_geo(Geo::new(frame.size.width, frame.size.height));
        if first_run {
            st.items = layout::default_layout(&apps);
            layout::save(&st.items);
        }
        let j = st.sync_apps(apps);
        if st.usage.is_empty() {
            seed_usage(st);
        }
        st.compute_favs();
        st.refilter();
        j
    })
    .unwrap_or_default();
    let n = jobs.len();
    let t_ui = t0.elapsed();
    spawn_icons(jobs, icon_px);
    if debug() {
        eprintln!("setup: ui ready in {t_ui:?}, rendering {n} icons @{icon_px}px");
    }
}

fn is_running(path: &str) -> bool {
    NSWorkspace::sharedWorkspace()
        .runningApplications()
        .iter()
        .any(|a| a.bundleURL().and_then(|u| u.path()).is_some_and(|p| p.to_string() == path))
}

fn seed_usage(st: &mut State) {
    for a in NSWorkspace::sharedWorkspace().runningApplications().iter() {
        if a.activationPolicy() != objc2_app_kit::NSApplicationActivationPolicy::Regular {
            continue;
        }
        if let Some(p) = a.bundleURL().and_then(|u| u.path()) {
            let p = p.to_string();
            if st.by_path.contains_key(&p) {
                st.usage.insert(p, 1);
            }
        }
    }
    index::save_usage(&st.usage);
}

pub fn watch_launches() {
    use block2::RcBlock;
    use std::ptr::NonNull;
    let block = RcBlock::new(|n: NonNull<objc2_foundation::NSNotification>| {
        let n = unsafe { n.as_ref() };
        let Some(info) = n.userInfo() else { return };
        let Some(app) = (unsafe { info.objectForKey(objc2_app_kit::NSWorkspaceApplicationKey) }) else { return };
        let Ok(app) = app.downcast::<objc2_app_kit::NSRunningApplication>() else { return };
        let Some(path) = app.bundleURL().and_then(|u| u.path()).map(|p| p.to_string()) else { return };
        with(|st| {
            if st.by_path.contains_key(&path) {
                st.note_use(&path);
            }
        });
    });
    let token = unsafe {
        NSWorkspace::sharedWorkspace().notificationCenter().addObserverForName_object_queue_usingBlock(
            Some(objc2_app_kit::NSWorkspaceDidLaunchApplicationNotification),
            None,
            Some(&objc2_foundation::NSOperationQueue::mainQueue()),
            &block,
        )
    };
    std::mem::forget(token);
}

pub fn usage_snapshot() -> HashMap<String, u32> {
    with(|st| st.usage.clone()).unwrap_or_default()
}

pub fn note_open(path: &str) {
    with(|st| {
        if !st.by_path.contains_key(path) || is_running(path) {
            st.note_use(path);
        }
    });
}

// draws a layer tree into a png with no window on screen. for checking the launcher
// and the search panel without taking over anyone's display. blurs and filters don't
// survive this, so `bg` stands in for whatever would be behind
pub(crate) fn render_layer(layer: &CALayer, w: f64, h: f64, scale: f64, bg: Option<(f64, f64, f64)>, out: &str) -> bool {
    use objc2_core_graphics::{CGColorSpace, CGContext};
    unsafe extern "C" {
        fn CGBitmapContextCreate(data: *mut std::ffi::c_void, width: usize, height: usize, bpc: usize, bpr: usize, space: *const CGColorSpace, info: u32) -> *mut CGContext;
    }
    let (pw, ph) = ((w * scale).round() as usize, (h * scale).round() as usize);
    let Some(cs) = CGColorSpace::new_device_rgb() else { return false };
    let raw = unsafe { CGBitmapContextCreate(std::ptr::null_mut(), pw, ph, 8, 0, &*cs, 2 | (2 << 12)) };
    let Some(ctx) = std::ptr::NonNull::new(raw).map(|p| unsafe { CFRetained::from_raw(p) }) else { return false };
    if let Some(bg) = bg {
        let fill = CGColor::new_srgb(bg.0, bg.1, bg.2, 1.0);
        CGContext::set_fill_color_with_color(Some(&ctx), Some(&fill));
        CGContext::fill_rect(Some(&ctx), CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(pw as f64, ph as f64)));
    }
    // core animation draws top-down into a bottom-up bitmap, so turn the page over first
    CGContext::translate_ctm(Some(&ctx), 0.0, ph as f64);
    CGContext::scale_ctm(Some(&ctx), scale, -scale);
    layer.renderInContext(&ctx);
    let Some(img) = objc2_core_graphics::CGBitmapContextCreateImage(Some(&ctx)) else { return false };
    let rep = objc2_app_kit::NSBitmapImageRep::initWithCGImage(<objc2_app_kit::NSBitmapImageRep as objc2::AnyThread>::alloc(), &img);
    let data = unsafe { rep.representationUsingType_properties(objc2_app_kit::NSBitmapImageFileType::PNG, &objc2_foundation::NSDictionary::new()) };
    data.is_some_and(|d| d.writeToFile_atomically(&NSString::from_str(out), true))
}

// liftoff --snapshot-launcher <out.png> [demo]: the grid as it would open, drawn off
// screen. "demo" keeps only apple's own apps and makes up a frequently used row, for
// pictures that go public without showing what's installed on this mac
pub fn snapshot(out: &str, demo: bool) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    setup(mtm);
    let mut apps = index::scan();
    if demo {
        apps.retain(|a| a.path.starts_with("/System/Applications/") || a.path == "/Applications/Safari.app");
    }
    let frame = NSScreen::mainScreen(mtm).map(|s| s.frame());
    let Some((jobs, px)) = with(|st| {
        let jobs = st.sync_apps(apps);
        if let Some(f) = frame {
            st.set_geo(Geo::new(f.size.width, f.size.height));
        }
        st.query.clear();
        if demo {
            st.usage.clear();
            let made_up = ["/Applications/Safari.app", "Mail", "Messages", "Music", "Notes", "Calendar", "Photos"];
            for (i, a) in made_up.iter().enumerate() {
                let p = if a.starts_with('/') { a.to_string() } else { format!("/System/Applications/{a}.app") };
                st.usage.insert(p, 100 - i as u32);
            }
        }
        st.compute_favs();
        st.refilter();
        (jobs, st.icon_px)
    }) else {
        return;
    };
    spawn_icons(jobs, px);
    let spin = |secs: f64| objc2_foundation::NSRunLoop::currentRunLoop().runUntilDate(&objc2_foundation::NSDate::dateWithTimeIntervalSinceNow(secs));
    spin(2.5);
    let ok = with(|st| {
        let root = st.content.superlayer().unwrap_or_else(|| st.content.clone());
        let b = root.bounds().size;
        let scale = st.window.backingScaleFactor();
        render_layer(&root, b.width, b.height, scale, Some((0.16, 0.18, 0.24)), out)
    });
    eprintln!("snapshot-launcher: wrote={ok:?}");
}
