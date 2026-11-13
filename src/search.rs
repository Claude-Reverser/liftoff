use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::time::Duration;

use block2::RcBlock;
use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::{ProtocolObject, Sel};
use objc2::{define_class, msg_send, AnyThread, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{
    NSAnimatablePropertyContainer, NSAnimationContext, NSAppearance, NSAppearanceCustomization,
    NSAppearanceNameDarkAqua, NSApplication, NSAutoresizingMaskOptions, NSBackingStoreType, NSColor,
    NSControl, NSControlTextEditingDelegate, NSEvent, NSEventModifierFlags, NSFocusRingType, NSFont,
    NSFontWeightLight, NSFontWeightMedium, NSFontWeightRegular, NSFontWeightSemibold, NSImage,
    NSImageView, NSPasteboard, NSPasteboardTypeString, NSTextField, NSTextFieldDelegate, NSTextView,
    NSTrackingArea, NSTrackingAreaOptions, NSView, NSVisualEffectBlendingMode, NSVisualEffectMaterial,
    NSVisualEffectState, NSVisualEffectView, NSWindow, NSWindowCollectionBehavior, NSWindowStyleMask,
    NSWorkspace, NSFontAttributeName, NSStringDrawing,
};
use objc2_core_foundation::{CFRetained, CFType, CGPoint, CGRect, CGSize};
use objc2_core_graphics::CGImage;
use objc2_foundation::{
    NSDictionary, NSNumber, NSArray, NSData, NSError, NSJSONReadingOptions, NSJSONSerialization, NSNotification, NSObject,
    NSObjectProtocol, NSPoint, NSRect, NSSize, NSString, NSURLResponse, NSURLSession, NSURL,
};
use objc2_core_graphics::{CGColor, CGMutablePath, CGPath};
use objc2_quartz_core::{
    kCAAlignmentCenter, kCAAlignmentLeft, kCAFilterTrilinear,
    kCAGravityResizeAspect, kCATruncationEnd, kCATruncationMiddle,
    CABasicAnimation, CALayer, CAMediaTiming,
    CAKeyframeAnimation, CAMediaTimingFunction, CAShapeLayer, CATextLayer, CATransaction,
    kCAFillRuleEvenOdd,
};
use objc2::runtime::AnyObject;

use crate::index::{self, App};
use crate::view::{animated, as_obj, ease_out, gray, no_anim};
use crate::{calc, files, icons, view};

const W: f64 = 900.0;
const BAR_H: f64 = 66.0;
const HEAD: f64 = 38.0;
const ROW_H: f64 = 50.0;
const ROWS: usize = 8;
const PAD: f64 = 10.0;
const GAP: f64 = 10.0;
const M: f64 = 50.0;
const EXP_H: f64 = BAR_H + HEAD + ROWS as f64 * ROW_H + PAD + 4.0;
const RADIUS: f64 = 28.0;
const ICON: f64 = 32.0;

const WEB: usize = 0;
const FILES: usize = 1;
const APPS: usize = 2;
const TITLES: [&str; 3] = ["Web", "Files", "Apps"];

fn col_w() -> f64 {
    (W - 2.0 * PAD - 2.0 * GAP) / 3.0
}
fn col_x(c: usize) -> f64 {
    PAD + c as f64 * (col_w() + GAP)
}
fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
    CGRect::new(CGPoint::new(x, y), CGSize::new(w, h))
}

#[derive(Clone)]
enum Do {
    Open(String),
    Web(String),
    Url(String),
}

#[derive(Clone)]
struct Res {
    title: String,
    sub: String,
    icon: Option<String>,
    act: Do,
}

struct Row {
    layer: Retained<CALayer>,
    icon: Retained<CALayer>,
    title: Retained<CATextLayer>,
    sub: Retained<CATextLayer>,
    shown: Option<String>,
}

struct Pane {
    header: Retained<CATextLayer>,
    count: Retained<CATextLayer>,
    empty: Retained<CATextLayer>,
    rows: Vec<Row>,
    items: Vec<Res>,
    top: usize,
}

// the rim. apple put a rainbow around siri and called it intelligence. we put
// one around a search bar that actually finds your files.
//
// cheap on purpose: the ring pieces never move, only their colours cycle, and
// one gaussian over the whole ring does the glow. no per-piece shadows.
struct Glow {
    shadow: Retained<CALayer>,
    holder: Retained<CALayer>,
    lit: Retained<CALayer>,
    cut: Retained<CAShapeLayer>,
    segs: Vec<Retained<CAShapeLayer>>,
    cur: CGRect,
}

const SEGS: usize = 36;
const LAP: f64 = 12.0;
const PALETTE: [(f64, f64, f64); 7] = [
    (0.32, 0.58, 1.00),
    (0.60, 0.42, 1.00),
    (0.98, 0.42, 0.78),
    (1.00, 0.58, 0.36),
    (0.98, 0.82, 0.40),
    (0.30, 0.86, 0.98),
    (0.32, 0.58, 1.00),
];

#[link(name = "CoreImage", kind = "framework")]
unsafe extern "C" {}

fn gaussian(r: f64) -> Option<Retained<AnyObject>> {
    let cls = objc2::runtime::AnyClass::get(c"CIFilter")?;
    let f: Option<Retained<AnyObject>> =
        unsafe { msg_send![cls, filterWithName: &*NSString::from_str("CIGaussianBlur")] };
    let f = f?;
    unsafe {
        let _: () = msg_send![&*f, setValue: &*NSNumber::new_f64(r), forKey: &*NSString::from_str("inputRadius")];
    }
    Some(f)
}

// everything outside the panel, so only light escapes and nothing shows through the glass
fn cut_path(r: CGRect) -> CFRetained<CGPath> {
    let rad = RADIUS.min(r.size.height / 2.0);
    let p = CGMutablePath::new();
    unsafe {
        CGMutablePath::add_rect(Some(&p), std::ptr::null(), rect(0.0, 0.0, W + 2.0 * M, EXP_H + 2.0 * M));
        CGMutablePath::add_rounded_rect(Some(&p), std::ptr::null(), r, rad, rad);
        CFRetained::cast_unchecked(p)
    }
}

// the panel opening, the columns trading places: fast off the mark and a long soft
// landing, no bounce. a search bar that wobbles when you press a key is showing off
fn springy() -> Retained<CAMediaTimingFunction> {
    CAMediaTimingFunction::functionWithControlPoints(0.22, 1.0, 0.36, 1.0)
}

fn rim_path(r: CGRect) -> CFRetained<CGPath> {
    let rad = RADIUS.min(r.size.height / 2.0);
    unsafe { CGPath::with_rounded_rect(r, rad, rad, std::ptr::null()) }
}

// same loop twice so a stroke segment can run off the end and come back round
fn anim(key: &str, from: &AnyObject, to: &AnyObject, dur: f64, tf: &CAMediaTimingFunction) -> Retained<CABasicAnimation> {
    let a = CABasicAnimation::animationWithKeyPath(Some(&NSString::from_str(key)));
    unsafe {
        a.setFromValue(Some(from));
        a.setToValue(Some(to));
    }
    a.setDuration(dur);
    a.setTimingFunction(Some(tf));
    a
}

impl Glow {
    fn new(parent: &CALayer, scale: f64) -> Glow {
        let bounds = rect(0.0, 0.0, W + 2.0 * M, EXP_H + 2.0 * M);
        let shadow = CALayer::new();
        shadow.setFrame(bounds);
        shadow.setShadowColor(Some(&gray(0.0, 1.0)));
        shadow.setShadowOpacity(0.38);
        shadow.setShadowRadius(24.0);
        shadow.setShadowOffset(CGSize::new(0.0, 0.0));
        parent.addSublayer(&shadow);

        let holder = CALayer::new();
        holder.setFrame(bounds);
        holder.setOpacity(0.0);
        parent.addSublayer(&holder);
        let cut = CAShapeLayer::new();
        cut.setFrame(bounds);
        unsafe { cut.setFillRule(kCAFillRuleEvenOdd) };
        cut.setFillColor(Some(&gray(0.0, 1.0)));
        cut.setContentsScale(scale);
        unsafe { holder.setMask(Some(&cut)) };

        let lit = CALayer::new();
        lit.setFrame(bounds);
        let blur = gaussian(11.0);
        let blurred = blur.is_some();
        if let Some(f) = &blur {
            unsafe { lit.setFilters(Some(&NSArray::from_slice(&[&**f]))) };
        }
        holder.addSublayer(&lit);

        let mut segs = Vec::new();
        for i in 0..SEGS {
            let l = CAShapeLayer::new();
            l.setFrame(bounds);
            l.setFillColor(None);
            let (r, g, b) = PALETTE[0];
            l.setStrokeColor(Some(&CGColor::new_srgb(r, g, b, 1.0)));
            l.setLineWidth(if blurred { 6.0 } else { 2.0 });
            l.setContentsScale(scale);
            l.setStrokeStart(i as f64 / SEGS as f64);
            l.setStrokeEnd(((i + 1) as f64 / SEGS as f64 + 0.004).min(1.0));
            lit.addSublayer(&l);
            segs.push(l);
        }
        let g = Glow { shadow, holder, lit, cut, segs, cur: rect(M, M, W, BAR_H) };
        no_anim(|| g.place(g.cur));
        g
    }

    fn place(&self, r: CGRect) {
        let p = rim_path(r);
        self.shadow.setShadowPath(Some(&p));
        self.cut.setPath(Some(&cut_path(r)));
        for l in &self.segs {
            l.setPath(Some(&p));
        }
    }

    fn spin(&self) {
        let cols: Vec<CFRetained<CGColor>> = PALETTE.iter().map(|&(r, g, b)| CGColor::new_srgb(r, g, b, 1.0)).collect();
        let refs: Vec<&AnyObject> = cols.iter().map(|c| as_obj(&**c)).collect();
        let vals = NSArray::from_slice(&refs);
        let key = NSString::from_str("strokeColor");
        for (i, l) in self.segs.iter().enumerate() {
            let a = CAKeyframeAnimation::animationWithKeyPath(Some(&key));
            unsafe { a.setValues(Some(&vals)) };
            a.setDuration(LAP);
            a.setRepeatCount(f32::INFINITY);
            a.setTimeOffset(LAP * (1.0 - i as f64 / SEGS as f64));
            l.addAnimation_forKey(&a, Some(&key));
        }
        let ease = CAMediaTimingFunction::functionWithControlPoints(0.45, 0.0, 0.55, 1.0);
        let b = anim("opacity", &NSNumber::new_f64(0.72), &NSNumber::new_f64(1.0), 3.0, &ease);
        b.setAutoreverses(true);
        b.setRepeatCount(f32::INFINITY);
        self.lit.addAnimation_forKey(&b, Some(&NSString::from_str("breathe")));
    }

    fn morph(&mut self, r: CGRect, dur: f64, tf: &CAMediaTimingFunction) {
        let (f, t) = (rim_path(self.cur), rim_path(r));
        let (fc, tc) = (cut_path(self.cur), cut_path(r));
        let key = NSString::from_str("morph");
        self.shadow.addAnimation_forKey(&anim("shadowPath", as_obj(&*f), as_obj(&*t), dur, tf), Some(&key));
        self.cut.addAnimation_forKey(&anim("path", as_obj(&*fc), as_obj(&*tc), dur, tf), Some(&key));
        for l in &self.segs {
            l.addAnimation_forKey(&anim("path", as_obj(&*f), as_obj(&*t), dur, tf), Some(&key));
        }
        no_anim(|| self.place(r));
        self.cur = r;
    }

    fn flare(&self, peak: f64, rest: f32, dur: f64) {
        let a = anim("opacity", &NSNumber::new_f64(peak), &NSNumber::new_f64(rest as f64), dur, &ease_out());
        self.holder.addAnimation_forKey(&a, Some(&NSString::from_str("flare")));
        no_anim(|| self.holder.setOpacity(rest));
    }

    fn fade(&self, dur: f64) {
        let from = self.holder.opacity();
        let a = anim("opacity", &NSNumber::new_f32(from), &NSNumber::new_f64(0.0), dur, &ease_out());
        self.holder.addAnimation_forKey(&a, Some(&NSString::from_str("flare")));
        no_anim(|| self.holder.setOpacity(0.0));
    }
}

struct S {
    glow: Glow,
    order: [usize; 3],
    answer: Option<String>,
    picked: bool,
    ghost: Retained<NSTextField>,
    font: Retained<NSFont>,
    window: Retained<NSWindow>,
    blur: Retained<NSVisualEffectView>,
    field: Retained<NSTextField>,
    content: Retained<CALayer>,
    sel_layer: Retained<CALayer>,
    panes: Vec<Pane>,
    sel: (usize, usize),
    icons: HashMap<String, CFRetained<CGImage>>,
    pending: HashSet<String>,
    apps: Vec<App>,
    usage: HashMap<String, u32>,
    query: String,
    expanded: bool,
    web_gen: u64,
    suggestions: Vec<String>,
    browser: Option<String>,
    scale: f64,
    wheel: f64,
    _delegate: Retained<FieldDelegate>,
}

thread_local! {
    static SS: RefCell<Option<S>> = const { RefCell::new(None) };
    static VISIBLE: Cell<bool> = const { Cell::new(false) };
    static GEN: Cell<u64> = const { Cell::new(0) };
}

fn with<R>(f: impl FnOnce(&mut S) -> R) -> Option<R> {
    SS.with(|s| s.try_borrow_mut().ok()?.as_mut().map(f))
}

fn bump() -> u64 {
    GEN.with(|g| {
        g.set(g.get() + 1);
        g.get()
    })
}

pub fn is_visible() -> bool {
    VISIBLE.with(|v| v.get())
}

fn later(ms: u64, f: impl FnOnce() + Send + 'static) {
    let when = dispatch2::DispatchTime::try_from(Duration::from_millis(ms)).unwrap();
    let _ = dispatch2::DispatchQueue::main().after(when, f);
}

fn tilde(p: &str) -> String {
    let h = std::env::var("HOME").unwrap_or_default();
    match p.strip_prefix(&h) {
        Some(r) if !h.is_empty() => format!("~{r}"),
        _ => p.to_owned(),
    }
}

fn text_layer(size: f64, weight: f64, alpha: f64, scale: f64) -> Retained<CATextLayer> {
    let t = CATextLayer::new();
    let f = NSFont::systemFontOfSize_weight(size, weight);
    unsafe {
        t.setFont(Some(&*(Retained::as_ptr(&f) as *const CFType)));
        t.setAlignmentMode(kCAAlignmentLeft);
        t.setTruncationMode(kCATruncationEnd);
    }
    t.setFontSize(size);
    t.setForegroundColor(Some(&gray(1.0, alpha)));
    t.setContentsScale(scale);
    t
}

fn set_text(t: &CATextLayer, s: &str) {
    unsafe { t.setString(Some(&NSString::from_str(s))) };
}

fn looks_like_url(q: &str) -> Option<String> {
    if q.contains(char::is_whitespace) {
        return None;
    }
    if q.starts_with("http://") || q.starts_with("https://") {
        return Some(q.to_owned());
    }
    let host = q.split('/').next()?;
    let tld = host.rsplit_once('.')?.1;
    if tld.len() >= 2 && tld.chars().all(|c| c.is_ascii_alphabetic()) && !host.starts_with('.') {
        let common = ["txt", "rs", "md", "pdf", "png", "jpg", "zip", "py", "js", "json", "app", "dmg", "html", "swift", "sh"];
        if !common.contains(&tld) {
            return Some(format!("https://{q}"));
        }
    }
    None
}

fn enc(s: &str) -> String {
    let mut o = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            o.push(b as char);
        } else {
            o.push_str(&format!("%{b:02X}"));
        }
    }
    o
}

impl S {
    fn build_web(&self, q: &str) -> Vec<Res> {
        let mut v = Vec::new();
        if let Some(u) = looks_like_url(q.trim()) {
            v.push(Res { title: q.trim().to_owned(), sub: "Open website".into(), icon: self.browser.clone(), act: Do::Url(u) });
        }
        v.push(Res {
            title: q.trim().to_owned(),
            sub: "Search Google".into(),
            icon: self.browser.clone(),
            act: Do::Web(q.trim().to_owned()),
        });
        for s in &self.suggestions {
            if !s.eq_ignore_ascii_case(q.trim()) {
                v.push(Res { title: s.clone(), sub: "Suggestion".into(), icon: self.browser.clone(), act: Do::Web(s.clone()) });
            }
        }
        v
    }

    fn update(&mut self, q: String) {
        let changed = q != self.query;
        self.query = q;
        let q = self.query.trim().to_owned();
        if q.is_empty() {
            self.answer = None;
            self.place_ghost();
            self.suggestions.clear();
            for p in &mut self.panes {
                p.items.clear();
                p.top = 0;
            }
            self.set_expanded(false);
            self.render(false);
            return;
        }
        if changed {
            self.suggestions.retain(|s| s.to_lowercase().starts_with(&q.to_lowercase()));
        }
        let lc = q.to_lowercase();

        let mut apps: Vec<(i32, &App)> = self
            .apps
            .iter()
            .filter_map(|a| {
                let s = index::score(a, &lc)?;
                let u = self.usage.get(&a.path).map_or(0, |c| (*c as i32 * 20).min(600));
                Some((s + u, a))
            })
            .collect();
        apps.sort_unstable_by(|a, b| b.0.cmp(&a.0).then(a.1.lc.cmp(&b.1.lc)));
        let best_app = apps.first().map(|a| a.0);
        let app_items: Vec<Res> = apps
            .iter()
            .take(40)
            .map(|(_, a)| Res {
                title: a.name.clone(),
                sub: tilde(a.path.rsplit_once('/').map_or("", |x| x.0)),
                icon: Some(a.path.clone()),
                act: Do::Open(a.path.clone()),
            })
            .collect();

        let mut best_file = None;
        let file_items: Vec<Res> = if q.chars().count() >= 2 {
            let app_paths: HashSet<&str> = self.apps.iter().map(|a| a.path.as_str()).collect();
            files::with_index(|i| i.search(&q, &self.usage, 60))
                .unwrap_or_default()
                .into_iter()
                .filter(|h| !app_paths.contains(h.path.as_str()))
                .map(|h| {
                    best_file = Some(best_file.unwrap_or(i32::MIN).max(h.score));
                    let (dir, name) = h.path.rsplit_once('/').unwrap_or(("", &h.path));
                    Res { title: name.to_owned(), sub: tilde(dir), icon: Some(h.path.clone()), act: Do::Open(h.path) }
                })
                .collect()
        } else {
            Vec::new()
        };

        let web = self.build_web(&q);
        let has_url = web.iter().any(|r| matches!(r.act, Do::Url(_)));
        self.panes[WEB].items = web;
        self.panes[FILES].items = file_items;
        self.panes[APPS].items = app_items;
        self.answer = calc::eval(&q);
        self.place_ghost();

        // whoever matched harder goes left. web sits on the right where it belongs.
        let app_u = best_app.map(|s| s / 10 + 100);
        let order = match (app_u, best_file) {
            (Some(a), Some(f)) if f > a => [FILES, APPS, WEB],
            (None, Some(_)) => [FILES, APPS, WEB],
            _ => [APPS, FILES, WEB],
        };
        if order != self.order {
            self.order = order;
            self.layout_cols(self.expanded);
        }
        if changed {
            self.picked = false;
            for p in &mut self.panes {
                p.top = 0;
            }
            self.sel = if has_url {
                (WEB, 0)
            } else {
                order.iter().copied().find(|&p| !self.panes[p].items.is_empty()).map_or((WEB, 0), |p| (p, 0))
            };
        }
        self.clamp_sel();
        self.set_expanded(true);
        self.render(true);
    }

    fn clamp_sel(&mut self) {
        let (p, r) = self.sel;
        let n = self.panes[p].items.len();
        if n == 0 {
            self.sel = (WEB, 0);
        } else if r >= n {
            self.sel = (p, n - 1);
        }
        let (p, r) = self.sel;
        let pane = &mut self.panes[p];
        if r < pane.top {
            pane.top = r;
        } else if r >= pane.top + ROWS {
            pane.top = r + 1 - ROWS;
        }
    }

    fn row_rect(&self, c: usize, i: usize) -> CGRect {
        rect(col_x(self.col(c)), BAR_H + HEAD + i as f64 * ROW_H, col_w(), ROW_H)
    }

    fn render(&mut self, fade: bool) {
        let mut want: Vec<String> = Vec::new();
        no_anim(|| {
            for (c, pane) in self.panes.iter_mut().enumerate() {
                let n = pane.items.len();
                set_text(&pane.count, &if n == 0 { String::new() } else { n.to_string() });
                pane.empty.setHidden(n != 0 || self.query.trim().is_empty());
                if c == WEB {
                    pane.empty.setHidden(true);
                }
                for (i, row) in pane.rows.iter_mut().enumerate() {
                    match pane.items.get(pane.top + i) {
                        Some(r) => {
                            row.layer.setHidden(false);
                            set_text(&row.title, &r.title);
                            set_text(&row.sub, &r.sub);
                            if row.shown != r.icon {
                                row.shown = r.icon.clone();
                                match r.icon.as_ref().and_then(|p| self.icons.get(p)) {
                                    Some(img) => unsafe { row.icon.setContents(Some(as_obj(&**img))) },
                                    None => {
                                        unsafe { row.icon.setContents(None) };
                                        if let Some(p) = &r.icon {
                                            want.push(p.clone());
                                        }
                                    }
                                }
                            }
                        }
                        None => {
                            row.layer.setHidden(true);
                            row.shown = None;
                        }
                    }
                }
            }
        });
        if fade {
            let (p, r) = self.sel;
            let shown = !self.panes[p].items.is_empty() && !self.query.trim().is_empty();
            let row = self.row_rect(p, r - self.panes[p].top);
            let fr = rect(row.origin.x + 4.0, row.origin.y + 1.0, row.size.width - 8.0, row.size.height - 2.0);
            self.place_sel(fr, shown);
        } else {
            no_anim(|| self.sel_layer.setHidden(true));
        }
        for p in want {
            self.request_icon(p);
        }
    }

    fn request_icon(&mut self, path: String) {
        if !self.pending.insert(path.clone()) {
            return;
        }
        let px = (ICON * self.scale).ceil() as usize;
        dispatch2::DispatchQueue::global_queue(dispatch2::GlobalQueueIdentifier::QualityOfService(
            dispatch2::DispatchQoS::UserInitiated,
        ))
        .exec_async(move || {
            let img = icons::render(&path, px);
            dispatch2::DispatchQueue::main().exec_async(move || {
                with(|s| {
                    s.pending.remove(&path);
                    let Some(img) = img else { return };
                    if s.icons.len() > 3000 {
                        s.icons.clear();
                    }
                    no_anim(|| {
                        for pane in &s.panes {
                            for row in &pane.rows {
                                if row.shown.as_deref() == Some(path.as_str()) {
                                    unsafe { row.icon.setContents(Some(as_obj(&*img))) };
                                }
                            }
                        }
                    });
                    s.icons.insert(path, img);
                });
            });
        });
    }

    fn panel_rect(&self, expanded: bool) -> NSRect {
        let h = if expanded { EXP_H } else { BAR_H };
        NSRect::new(NSPoint::new(M, M), NSSize::new(W, h))
    }

    fn set_expanded(&mut self, on: bool) {
        if self.expanded == on {
            return;
        }
        self.expanded = on;
        let r = self.panel_rect(on);
        let (dur, tf) = if on { (0.46, springy()) } else { (0.26, ease_out()) };
        NSAnimationContext::beginGrouping();
        let ctx = NSAnimationContext::currentContext();
        ctx.setDuration(dur);
        ctx.setTimingFunction(Some(&tf));
        self.blur.animator().setFrame(r);
        NSAnimationContext::endGrouping();
        self.glow.morph(rect(r.origin.x, r.origin.y, r.size.width, r.size.height), dur, &tf);
        // the results come in as one piece while the panel opens. no rows flying in one
        // after the other
        animated(if on { 0.2 } else { 0.12 }, || self.content.setOpacity(if on { 1.0 } else { 0.0 }));
        later(340, || {
            with(|s| s.window.invalidateShadow());
        });
    }

    fn col(&self, pane: usize) -> usize {
        self.order.iter().position(|&p| p == pane).unwrap_or(pane)
    }

    fn pane_at(&self, x: f64, slop: f64) -> Option<usize> {
        (0..3).find(|&c| x >= col_x(c) - slop && x < col_x(c) + col_w() + slop).map(|c| self.order[c])
    }

    fn layout_cols(&self, anim: bool) {
        let go = || {
            for (p, pane) in self.panes.iter().enumerate() {
                let x = col_x(self.col(p));
                pane.header.setFrame(rect(x + 12.0, BAR_H + 14.0, 120.0, 16.0));
                pane.count.setFrame(rect(x + col_w() - 72.0, BAR_H + 14.0, 60.0, 16.0));
                pane.empty.setFrame(rect(x, BAR_H + HEAD + 60.0, col_w(), 18.0));
                for (i, row) in pane.rows.iter().enumerate() {
                    row.layer.setFrame(rect(x, BAR_H + HEAD + i as f64 * ROW_H, col_w(), ROW_H));
                }
            }
        };
        if anim {
            CATransaction::begin();
            CATransaction::setAnimationDuration(0.42);
            CATransaction::setAnimationTimingFunction(Some(&springy()));
            go();
            CATransaction::commit();
        } else {
            no_anim(go);
        }
    }

    fn place_ghost(&self) {
        let Some(ans) = &self.answer else {
            self.ghost.setHidden(true);
            return;
        };
        let typed = self.field.stringValue();
        let key: &AnyObject = unsafe { NSFontAttributeName };
        let attrs = NSDictionary::from_slices(&[key.downcast_ref::<NSString>().unwrap()], &[as_obj(&*self.font)]);
        let tw = unsafe { typed.sizeWithAttributes(Some(&attrs)) }.width;
        let f = self.field.frame();
        let x = f.origin.x + 2.0 + tw + 8.0;
        let text = format!("= {ans}");
        self.ghost.setStringValue(&NSString::from_str(&text));
        let room = f.origin.x + f.size.width - x;
        self.ghost.setFrame(NSRect::new(NSPoint::new(x, f.origin.y), NSSize::new(room.max(0.0), f.size.height)));
        self.ghost.setHidden(room < 40.0);
    }

    // the plate glides to the new row on a spring with no bounce, starting from wherever
    // it is on screen, so holding an arrow key makes it run smoothly instead of hopping.
    // when it first turns up it just fades in where it belongs
    fn place_sel(&self, fr: CGRect, shown: bool) {
        let l = &self.sel_layer;
        if !shown {
            no_anim(|| l.setHidden(true));
            return;
        }
        if l.isHidden() {
            no_anim(|| {
                l.setFrame(fr);
                l.setHidden(false);
            });
            l.addAnimation_forKey(&anim("opacity", &NSNumber::new_f64(0.0), &NSNumber::new_f64(1.0), 0.12, &ease_out()), Some(&NSString::from_str("in")));
            return;
        }
        view::glide(l, fr, 0.22);
    }

    fn move_sel(&mut self, d: isize) {
        let (p, r) = self.sel;
        let n = self.panes[p].items.len();
        if n == 0 {
            return;
        }
        self.picked = true;
        self.sel = (p, (r as isize + d).clamp(0, n as isize - 1) as usize);
        self.clamp_sel();
        self.render(true);
    }

    fn cycle(&mut self, d: isize) {
        let (p, r) = self.sel;
        self.picked = true;
        let at = self.col(p) as isize;
        for k in 1..=3 {
            let np = self.order[(at + d * k).rem_euclid(3) as usize];
            if !self.panes[np].items.is_empty() {
                let top = self.panes[np].top;
                let row = (r.saturating_sub(self.panes[p].top) + top).min(self.panes[np].items.len() - 1);
                self.sel = (np, row);
                break;
            }
        }
        self.clamp_sel();
        self.render(true);
    }

    fn hit(&self, p: NSPoint) -> Option<(usize, usize)> {
        if p.y < BAR_H + HEAD {
            return None;
        }
        let i = ((p.y - BAR_H - HEAD) / ROW_H) as usize;
        if i >= ROWS {
            return None;
        }
        let c = self.pane_at(p.x, 0.0)?;
        let pane = &self.panes[c];
        let idx = pane.top + i;
        (idx < pane.items.len()).then_some((c, idx))
    }

    fn scroll(&mut self, p: NSPoint, dy: f64) {
        let Some(c) = self.pane_at(p.x, GAP / 2.0) else { return };
        self.wheel += dy;
        let steps = (self.wheel / ROW_H * 1.6) as isize;
        if steps == 0 {
            return;
        }
        self.wheel = 0.0;
        let pane = &mut self.panes[c];
        let max = pane.items.len().saturating_sub(ROWS) as isize;
        pane.top = (pane.top as isize - steps).clamp(0, max) as usize;
        if self.sel.0 == c {
            let (_, r) = self.sel;
            let top = self.panes[c].top;
            self.sel.1 = r.clamp(top, top + ROWS - 1).min(self.panes[c].items.len().saturating_sub(1));
        }
        self.render(true);
    }

    fn got_suggestions(&mut self, g: u64, q: &str, list: Vec<String>) {
        if g != self.web_gen || q != self.query.trim() {
            return;
        }
        self.suggestions = list.into_iter().take(7).collect();
        let keep = self.sel;
        self.panes[WEB].items = self.build_web(q);
        self.sel = keep;
        self.clamp_sel();
        self.render(true);
    }
}

fn on_text() {
    let Some(q) = with(|s| s.field.stringValue().to_string()) else { return };
    let g = with(|s| {
        s.update(q.clone());
        s.web_gen += 1;
        s.web_gen
    });
    let Some(g) = g else { return };
    let q = q.trim().to_owned();
    if q.is_empty() || calc::eval(&q).is_some() {
        return;
    }
    later(140, move || {
        if with(|s| s.web_gen) == Some(g) {
            fetch_suggestions(q, g);
        }
    });
}

fn fetch_suggestions(q: String, g: u64) {
    let url = format!(
        "https://suggestqueries.google.com/complete/search?client=firefox&ie=utf-8&oe=utf-8&q={}",
        enc(&q)
    );
    let Some(nsurl) = NSURL::URLWithString(&NSString::from_str(&url)) else { return };
    let block = RcBlock::new(move |data: *mut NSData, _r: *mut NSURLResponse, _e: *mut NSError| {
        let Some(data) = (unsafe { data.as_ref() }) else { return };
        let list = autoreleasepool(|_| parse_suggestions(data));
        let q = q.clone();
        dispatch2::DispatchQueue::main().exec_async(move || {
            with(|s| s.got_suggestions(g, &q, list));
        });
    });
    let task = unsafe { NSURLSession::sharedSession().dataTaskWithURL_completionHandler(&nsurl, &block) };
    task.resume();
}

fn parse_suggestions(data: &NSData) -> Vec<String> {
    let Ok(obj) = NSJSONSerialization::JSONObjectWithData_options_error(data, NSJSONReadingOptions::empty()) else {
        return Vec::new();
    };
    let Ok(arr) = obj.downcast::<NSArray>() else { return Vec::new() };
    if arr.count() < 2 {
        return Vec::new();
    }
    let Ok(list) = arr.objectAtIndex(1).downcast::<NSArray>() else { return Vec::new() };
    (0..list.count())
        .filter_map(|i| list.objectAtIndex(i).downcast::<NSString>().ok().map(|s| s.to_string()))
        .collect()
}

fn on_command(sel: Sel) -> bool {
    let name = sel.name().to_bytes();
    let mtm = MainThreadMarker::new().unwrap();
    let ev = NSApplication::sharedApplication(mtm).currentEvent();
    let mods = ev.as_ref().map(|e| e.modifierFlags()).unwrap_or(NSEventModifierFlags::empty());
    match name {
        b"moveUp:" => {
            with(|s| s.move_sel(-1));
        }
        b"moveDown:" => {
            with(|s| s.move_sel(1));
        }
        b"insertTab:" => {
            with(|s| s.cycle(1));
        }
        b"insertBacktab:" => {
            with(|s| s.cycle(-1));
        }
        b"insertNewline:" | b"insertLineBreak:" | b"insertNewlineIgnoringFieldEditor:" => activate(None, mods),
        b"noop:" => {
            if ev.is_some_and(|e| e.keyCode() == 36 || e.keyCode() == 76) {
                activate(None, mods);
            } else {
                return false;
            }
        }
        b"cancelOperation:" => {
            let empty = with(|s| s.field.stringValue().length() == 0).unwrap_or(true);
            if empty {
                hide();
            } else {
                with(|s| s.field.setStringValue(&NSString::from_str("")));
                on_text();
            }
        }
        _ => return false,
    }
    true
}

fn activate(at: Option<(usize, usize)>, mods: NSEventModifierFlags) {
    if at.is_none() {
        if let Some(ans) = with(|s| s.answer.clone().filter(|_| !s.picked)).flatten() {
            copy(&ans);
            hide();
            return;
        }
    }
    let Some(res) = with(|s| {
        let (p, r) = at.unwrap_or(s.sel);
        s.panes[p].items.get(r).cloned()
    })
    .flatten() else {
        return;
    };
    let ws = NSWorkspace::sharedWorkspace();
    let cmd = mods.contains(NSEventModifierFlags::Command);
    let opt = mods.contains(NSEventModifierFlags::Option);
    match res.act {
        Do::Open(p) => {
            let url = NSURL::fileURLWithPath(&NSString::from_str(&p));
            if cmd {
                hide();
                crate::browse::open(Some(&p));
                return;
            } else if opt {
                copy(&p);
            } else {
                view::note_open(&p);
                with(|s| *s.usage.entry(p.clone()).or_insert(0) += 1);
                ws.openURL(&url);
            }
        }
        Do::Web(q) => open_url(&format!("https://www.google.com/search?q={}", enc(&q))),
        Do::Url(u) => open_url(&u),
    }
    hide();
}

fn open_url(u: &str) {
    if let Some(url) = NSURL::URLWithString(&NSString::from_str(u)) {
        NSWorkspace::sharedWorkspace().openURL(&url);
    }
}

fn copy(s: &str) {
    let pb = NSPasteboard::generalPasteboard();
    pb.clearContents();
    pb.setString_forType(&NSString::from_str(s), unsafe { NSPasteboardTypeString });
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "LiftoffSearchDelegate"]
    struct FieldDelegate;

    unsafe impl NSObjectProtocol for FieldDelegate {}

    unsafe impl NSControlTextEditingDelegate for FieldDelegate {
        #[unsafe(method(controlTextDidChange:))]
        fn text_did_change(&self, _n: &NSNotification) {
            on_text();
        }

        #[unsafe(method(control:textView:doCommandBySelector:))]
        fn do_command(&self, _c: &NSControl, _tv: &NSTextView, sel: Sel) -> bool {
            on_command(sel)
        }
    }

    unsafe impl NSTextFieldDelegate for FieldDelegate {}
);

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "LiftoffSearchRoot"]
    struct RootView;

    impl RootView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }
        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, _e: &NSEvent) {
            hide();
        }
    }
);

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "LiftoffSearchGlow"]
    struct GlowView;

    impl GlowView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }
        #[unsafe(method(hitTest:))]
        fn hit_test(&self, _p: NSPoint) -> *mut NSView {
            std::ptr::null_mut()
        }
    }
);

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "LiftoffSearchPanes"]
    struct PaneView;

    impl PaneView {
        #[unsafe(method(isFlipped))]
        fn is_flipped(&self) -> bool {
            true
        }
        #[unsafe(method(acceptsFirstMouse:))]
        fn accepts_first_mouse(&self, _e: Option<&NSEvent>) -> bool {
            true
        }
        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, e: &NSEvent) {
            let p = self.loc(e);
            if let Some(h) = with(|s| s.hit(p)).flatten() {
                with(|s| {
                    s.sel = h;
                    s.render(true);
                });
                activate(Some(h), e.modifierFlags());
            }
        }
        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, e: &NSEvent) {
            let p = self.loc(e);
            with(|s| {
                if let Some(h) = s.hit(p) {
                    if h != s.sel {
                        s.sel = h;
                        s.render(true);
                    }
                }
            });
        }
        #[unsafe(method(scrollWheel:))]
        fn scroll_wheel(&self, e: &NSEvent) {
            let p = self.loc(e);
            let dy = if e.hasPreciseScrollingDeltas() { e.scrollingDeltaY() } else { e.scrollingDeltaY() * ROW_H };
            with(|s| s.scroll(p, dy));
        }
    }
);

impl PaneView {
    fn loc(&self, e: &NSEvent) -> NSPoint {
        self.convertPoint_fromView(e.locationInWindow(), None)
    }
}

fn browser_path() -> Option<String> {
    let u = NSURL::URLWithString(&NSString::from_str("https://example.com"))?;
    NSWorkspace::sharedWorkspace().URLForApplicationToOpenURL(&u)?.path().map(|p| p.to_string())
}

pub fn setup(mtm: MainThreadMarker) {
    let scale = objc2_app_kit::NSScreen::screens(mtm).iter().map(|s| s.backingScaleFactor()).fold(1.0, f64::max);
    let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(W + 2.0 * M, EXP_H + 2.0 * M));
    let window: Retained<view::LiftoffWindow> = unsafe {
        msg_send![
            view::LiftoffWindow::alloc(mtm),
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
    window.setBackgroundColor(Some(&NSColor::clearColor()));
    window.setLevel(objc2_app_kit::NSMainMenuWindowLevel + 2);
    window.setCollectionBehavior(
        NSWindowCollectionBehavior::CanJoinAllSpaces
            | NSWindowCollectionBehavior::FullScreenAuxiliary
            | NSWindowCollectionBehavior::Transient
            | NSWindowCollectionBehavior::IgnoresCycle,
    );
    if let Some(dark) = NSAppearance::appearanceNamed(unsafe { NSAppearanceNameDarkAqua }) {
        window.setAppearance(Some(&dark));
    }

    let root: Retained<RootView> = unsafe { msg_send![RootView::alloc(mtm), initWithFrame: frame] };
    root.setWantsLayer(true);
    window.setContentView(Some(&root));

    let gv: Retained<GlowView> = unsafe { msg_send![GlowView::alloc(mtm), initWithFrame: frame] };
    let glow_root = CALayer::new();
    glow_root.setGeometryFlipped(true);
    gv.setLayer(Some(&glow_root));
    gv.setWantsLayer(true);
    gv.setLayerUsesCoreImageFilters(true);
    root.addSubview(&gv);
    let glow = Glow::new(&glow_root, scale);

    let panel = NSRect::new(NSPoint::new(M, M), NSSize::new(W, BAR_H));
    let blur = NSVisualEffectView::initWithFrame(NSVisualEffectView::alloc(mtm), panel);
    blur.setMaterial(NSVisualEffectMaterial::HUDWindow);
    blur.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
    blur.setState(NSVisualEffectState::Active);
    blur.setWantsLayer(true);
    if let Some(l) = blur.layer() {
        l.setCornerRadius(RADIUS);
        l.setMasksToBounds(true);
        l.setBorderWidth(0.5);
        l.setBorderColor(Some(&gray(1.0, 0.16)));
    }
    root.addSubview(&blur);

    let pv: Retained<PaneView> = unsafe {
        msg_send![PaneView::alloc(mtm), initWithFrame: NSRect::new(NSPoint::new(0.0, 0.0), panel.size)]
    };
    pv.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable);
    let player = CALayer::new();
    player.setGeometryFlipped(true);
    pv.setLayer(Some(&player));
    pv.setWantsLayer(true);
    blur.addSubview(&pv);
    let ta = unsafe {
        NSTrackingArea::initWithRect_options_owner_userInfo(
            NSTrackingArea::alloc(),
            NSRect::ZERO,
            NSTrackingAreaOptions::MouseMoved | NSTrackingAreaOptions::ActiveAlways | NSTrackingAreaOptions::InVisibleRect,
            Some(as_obj(&*pv)),
            None,
        )
    };
    pv.addTrackingArea(&ta);

    if let Some(img) = NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str("magnifyingglass"),
        None,
    ) {
        let iv = NSImageView::imageViewWithImage(&img, mtm);
        iv.setFrame(NSRect::new(NSPoint::new(22.0, (BAR_H - 26.0) / 2.0), NSSize::new(26.0, 26.0)));
        iv.setContentTintColor(Some(&NSColor::colorWithWhite_alpha(1.0, 0.55)));
        iv.setSymbolConfiguration(Some(&objc2_app_kit::NSImageSymbolConfiguration::configurationWithPointSize_weight(
            20.0,
            unsafe { NSFontWeightMedium },
        )));
        iv.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinYMargin);
        blur.addSubview(&iv);
    }

    let field = NSTextField::textFieldWithString(&NSString::from_str(""), mtm);
    field.setFrame(NSRect::new(NSPoint::new(60.0, (BAR_H - 34.0) / 2.0), NSSize::new(W - 80.0, 34.0)));
    field.setBezeled(false);
    field.setBordered(false);
    field.setDrawsBackground(false);
    field.setFocusRingType(NSFocusRingType::None);
    field.setFont(Some(&NSFont::systemFontOfSize_weight(26.0, unsafe { NSFontWeightLight })));
    field.setTextColor(Some(&NSColor::whiteColor()));
    field.setPlaceholderString(Some(&NSString::from_str("Search apps, files and the web")));
    if let Some(c) = field.cell() {
        c.setScrollable(true);
        c.setUsesSingleLineMode(true);
        c.setWraps(false);
    }
    field.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinYMargin | NSAutoresizingMaskOptions::ViewWidthSizable);
    let delegate: Retained<FieldDelegate> = unsafe { msg_send![FieldDelegate::alloc(mtm), init] };
    unsafe { field.setDelegate(Some(ProtocolObject::from_ref(&*delegate))) };
    blur.addSubview(&field);

    let content = CALayer::new();
    content.setFrame(rect(0.0, 0.0, W, EXP_H));
    content.setOpacity(0.0);
    player.addSublayer(&content);
    let sep = CALayer::new();
    sep.setFrame(rect(0.0, BAR_H, W, 0.5));
    sep.setBackgroundColor(Some(&gray(1.0, 0.12)));
    content.addSublayer(&sep);
    for c in 1..3 {
        let d = CALayer::new();
        d.setFrame(rect(col_x(c) - GAP / 2.0, BAR_H + 14.0, 0.5, EXP_H - BAR_H - 28.0));
        d.setBackgroundColor(Some(&gray(1.0, 0.08)));
        content.addSublayer(&d);
    }
    // the selection: a quiet plate of light behind the row, nothing more. it used to be
    // a blue-to-violet gradient with a white rim, which is what every generated ui does
    let sel_layer = CALayer::new();
    sel_layer.setCornerRadius(10.0);
    sel_layer.setBackgroundColor(Some(&gray(1.0, 0.12)));
    sel_layer.setHidden(true);
    content.addSublayer(&sel_layer);

    let order = [APPS, FILES, WEB];
    let mut panes = Vec::new();
    for c in 0..3 {
        let header = text_layer(12.0, unsafe { NSFontWeightSemibold }, 0.55, scale);
        header.setFrame(rect(col_x(c) + 12.0, BAR_H + 14.0, 120.0, 16.0));
        set_text(&header, TITLES[c]);
        content.addSublayer(&header);
        let count = text_layer(11.0, unsafe { NSFontWeightRegular }, 0.3, scale);
        unsafe { count.setAlignmentMode(objc2_quartz_core::kCAAlignmentRight) };
        count.setFrame(rect(col_x(c) + col_w() - 72.0, BAR_H + 14.0, 60.0, 16.0));
        content.addSublayer(&count);
        let empty = text_layer(12.0, unsafe { NSFontWeightRegular }, 0.3, scale);
        unsafe { empty.setAlignmentMode(kCAAlignmentCenter) };
        empty.setFrame(rect(col_x(c), BAR_H + HEAD + 60.0, col_w(), 18.0));
        set_text(&empty, "No results");
        empty.setHidden(true);
        content.addSublayer(&empty);
        let mut rows = Vec::new();
        for i in 0..ROWS {
            let layer = CALayer::new();
            layer.setFrame(rect(col_x(c), BAR_H + HEAD + i as f64 * ROW_H, col_w(), ROW_H));
            layer.setHidden(true);
            let icon = CALayer::new();
            icon.setFrame(rect(10.0, (ROW_H - ICON) / 2.0, ICON, ICON));
            unsafe {
                icon.setContentsGravity(kCAGravityResizeAspect);
                icon.setMinificationFilter(kCAFilterTrilinear);
            }
            layer.addSublayer(&icon);
            let title = text_layer(13.5, unsafe { NSFontWeightMedium }, 0.95, scale);
            title.setFrame(rect(52.0, 8.0, col_w() - 62.0, 18.0));
            layer.addSublayer(&title);
            let sub = text_layer(11.0, unsafe { NSFontWeightRegular }, 0.5, scale);
            unsafe { sub.setTruncationMode(kCATruncationMiddle) };
            sub.setFrame(rect(52.0, 27.0, col_w() - 62.0, 15.0));
            layer.addSublayer(&sub);
            content.addSublayer(&layer);
            rows.push(Row { layer, icon, title, sub, shown: None });
        }
        panes.push(Pane { header, count, empty, rows, items: Vec::new(), top: 0 });
    }

    let font = NSFont::systemFontOfSize_weight(26.0, unsafe { NSFontWeightLight });
    let ghost = NSTextField::labelWithString(&NSString::from_str(""), mtm);
    ghost.setFont(Some(&font));
    ghost.setTextColor(Some(&NSColor::colorWithWhite_alpha(1.0, 0.38)));
    ghost.setDrawsBackground(false);
    ghost.setBezeled(false);
    ghost.setAutoresizingMask(NSAutoresizingMaskOptions::ViewMinYMargin);
    if let Some(c) = ghost.cell() {
        c.setLineBreakMode(objc2_app_kit::NSLineBreakMode::ByTruncatingTail);
    }
    ghost.setHidden(true);
    blur.addSubview(&ghost);

    let s = S {
        order,
        answer: None,
        picked: false,
        ghost,
        font,
        glow,
        window,
        blur,
        field,
        content,
        sel_layer,
        panes,
        sel: (WEB, 0),
        icons: HashMap::new(),
        pending: HashSet::new(),
        apps: index::scan(),
        usage: HashMap::new(),
        query: String::new(),
        expanded: false,
        web_gen: 0,
        suggestions: Vec::new(),
        browser: browser_path(),
        scale,
        wheel: 0.0,
        _delegate: delegate,
    };
    SS.with(|x| *x.borrow_mut() = Some(s));
    with(|s| s.layout_cols(false));
}

pub fn toggle() {
    if is_visible() { hide() } else { show() }
}

pub fn show() {
    let mtm = MainThreadMarker::new().unwrap();
    if view::is_visible() {
        view::hide();
    }
    let screen = view::mouse_screen(mtm).map(|s| s.visibleFrame());
    let Some((window, blur, field)) = with(|s| {
        s.apps = index::scan();
        s.usage = view::usage_snapshot();
        s.browser = browser_path();
        s.field.setStringValue(&NSString::from_str(""));
        s.query.clear();
        s.suggestions.clear();
        for p in &mut s.panes {
            p.items.clear();
            p.top = 0;
        }
        s.expanded = false;
        no_anim(|| s.content.setOpacity(0.0));
        s.render(false);
        (s.window.clone(), s.blur.clone(), s.field.clone())
    }) else {
        return;
    };
    if let Some(f) = screen {
        let ww = W + 2.0 * M;
        let wh = EXP_H + 2.0 * M;
        let top = f.origin.y + f.size.height * 0.80;
        let x = (f.origin.x + (f.size.width - ww) / 2.0).round();
        window.setFrame_display(NSRect::new(NSPoint::new(x, top + M - wh), NSSize::new(ww, wh)), false);
    }
    let full = NSRect::new(NSPoint::new(M, M), NSSize::new(W, BAR_H));
    let start = NSRect::new(NSPoint::new(M + W * 0.04, M + 4.0), NSSize::new(W * 0.92, BAR_H - 6.0));
    blur.setFrame(start);
    let nr = |r: NSRect| rect(r.origin.x, r.origin.y, r.size.width, r.size.height);
    with(|s| {
        s.glow.cur = nr(start);
        no_anim(|| s.glow.place(nr(start)));
        s.glow.spin();
        s.glow.morph(nr(full), 0.5, &springy());
        s.glow.flare(1.0, 0.9, 1.2);
    });
    VISIBLE.with(|v| v.set(true));
    bump();
    window.setIgnoresMouseEvents(false);
    window.setAlphaValue(0.0);
    window.makeKeyAndOrderFront(None);
    window.makeFirstResponder(Some(&field));
    NSApplication::sharedApplication(mtm).activate();
    NSAnimationContext::beginGrouping();
    let ctx = NSAnimationContext::currentContext();
    ctx.setDuration(0.5);
    ctx.setTimingFunction(Some(&springy()));
    window.animator().setAlphaValue(1.0);
    blur.animator().setFrame(full);
    NSAnimationContext::endGrouping();
    later(280, || {
        with(|s| s.window.invalidateShadow());
    });
}

pub fn hide() {
    if !is_visible() {
        return;
    }
    VISIBLE.with(|v| v.set(false));
    let my_gen = bump();
    let Some((window, blur)) = with(|s| (s.window.clone(), s.blur.clone())) else { return };
    let f = blur.frame();
    let shrunk = NSRect::new(
        NSPoint::new(f.origin.x + f.size.width * 0.02, f.origin.y + 3.0),
        NSSize::new(f.size.width * 0.96, f.size.height - 6.0),
    );
    window.setIgnoresMouseEvents(true);
    with(|s| {
        s.glow.morph(rect(shrunk.origin.x, shrunk.origin.y, shrunk.size.width, shrunk.size.height), 0.16, &ease_out());
        s.glow.fade(0.14);
        s.ghost.setHidden(true);
    });
    NSAnimationContext::beginGrouping();
    let ctx = NSAnimationContext::currentContext();
    ctx.setDuration(0.16);
    ctx.setTimingFunction(Some(&ease_out()));
    window.animator().setAlphaValue(0.0);
    blur.animator().setFrame(shrunk);
    NSAnimationContext::endGrouping();
    later(180, move || {
        if GEN.with(|g| g.get()) != my_gen || is_visible() {
            return;
        }
        with(|s| s.window.orderOut(None));
        if !view::is_visible() && !crate::browse::is_open() {
            let mtm = MainThreadMarker::new().unwrap();
            NSApplication::sharedApplication(mtm).hide(None);
        }
    });
}


// liftoff --snapshot-search <out.png> <query> [moves] [demo]: the search panel for a
// query, drawn off screen. moves like "dd" (down) or "r" (next column) pick a row first.
// writes the pieces next to <out> (glow ring, its mask, the shadow, the results, the
// field) because the glass and the glow's blur only exist on a real screen; whoever
// puts them together blurs the ring the same way. "demo" shows only apple's own apps
// and files from $HOME, for pictures that go public
pub fn snapshot(out: &str, query: &str, moves: &str, demo: bool) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    setup(mtm);
    let spin = |secs: f64| objc2_foundation::NSRunLoop::currentRunLoop().runUntilDate(&objc2_foundation::NSDate::dateWithTimeIntervalSinceNow(secs));
    if demo {
        files::start();
        spin(2.0);
        with(|s| {
            s.apps.retain(|a| a.path.starts_with("/System/Applications/") || a.path == "/Applications/Safari.app");
            // whatever browser this mac uses is nobody's business
            s.browser = Some("/Applications/Safari.app".into());
        });
    }
    let ask = || {
        with(|s| {
            s.field.setStringValue(&NSString::from_str(query));
            s.query.clear();
            s.update(query.to_owned());
        });
    };
    ask();
    // suggestions come back from the network and icons from other threads
    spin(3.0);
    ask();
    if demo {
        // the indexer also crawls mounted disks. a public picture only shows $HOME
        let h = std::env::var("HOME").unwrap_or_default();
        with(|s| {
            s.panes[FILES].items.retain(|r| matches!(&r.act, Do::Open(p) if p.starts_with(&h)));
            s.render(true);
        });
    }
    for c in moves.chars() {
        with(|s| match c {
            'd' => s.move_sel(1),
            'u' => s.move_sel(-1),
            'r' => s.cycle(1),
            'l' => s.cycle(-1),
            _ => {}
        });
    }
    spin(1.2);
    let ok = with(|s| {
        let scale = s.scale.max(2.0);
        let panel = s.panel_rect(true);
        s.blur.setFrame(panel);
        no_anim(|| {
            s.content.setOpacity(1.0);
            s.glow.place(rect(panel.origin.x, panel.origin.y, panel.size.width, panel.size.height));
            // the ring's colours normally travel round it. stop them where they'd be at the
            // start of a lap, as plain colours a still picture can hold
            for (i, l) in s.glow.segs.iter().enumerate() {
                let f = (1.0 - i as f64 / SEGS as f64).rem_euclid(1.0) * (PALETTE.len() - 1) as f64;
                let (k, t) = (f.floor() as usize, f.fract());
                let (a, b) = (PALETTE[k], PALETTE[(k + 1).min(PALETTE.len() - 1)]);
                l.setStrokeColor(Some(&CGColor::new_srgb(a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t, a.2 + (b.2 - a.2) * t, 1.0)));
            }
        });
        let (gw, gh) = (W + 2.0 * M, EXP_H + 2.0 * M);
        let mut ok = view::render_layer(&s.glow.lit, gw, gh, scale, None, &format!("{out}.ring.png"));
        ok &= view::render_layer(&s.glow.cut, gw, gh, scale, None, &format!("{out}.cut.png"));
        ok &= view::render_layer(&s.glow.shadow, gw, gh, scale, None, &format!("{out}.shadow.png"));
        ok &= view::render_layer(&s.content, W, EXP_H, scale, None, &format!("{out}.content.png"));
        // the field and the magnifier are ordinary views, drawn the ordinary way
        let views: Vec<(Retained<NSView>, &str)> = {
            let subs = s.blur.subviews();
            let mut v = Vec::new();
            for i in 0..subs.count() {
                let sv = subs.objectAtIndex(i);
                if sv.isKindOfClass(<NSImageView as objc2::ClassType>::class()) {
                    v.push((sv, "icon"));
                } else if std::ptr::eq(&*sv, &**s.field as &NSView) {
                    v.push((sv, "field"));
                }
            }
            v
        };
        for (v, name) in views {
            let b = v.bounds();
            let rep = unsafe {
                objc2_app_kit::NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
                    <objc2_app_kit::NSBitmapImageRep as AnyThread>::alloc(),
                    std::ptr::null_mut(),
                    (b.size.width * scale) as isize,
                    (b.size.height * scale) as isize,
                    8,
                    4,
                    true,
                    false,
                    objc2_app_kit::NSDeviceRGBColorSpace,
                    0,
                    0,
                )
            };
            if let Some(rep) = rep {
                rep.setSize(b.size);
                v.cacheDisplayInRect_toBitmapImageRep(b, &rep);
                if let Some(d) = unsafe { rep.representationUsingType_properties(objc2_app_kit::NSBitmapImageFileType::PNG, &NSDictionary::new()) } {
                    d.writeToFile_atomically(&NSString::from_str(&format!("{out}.{name}.png")), true);
                }
            }
            // where it sits in the panel, measured from the panel's top left
            let f = v.frame();
            eprintln!("snapshot-search: {name} at {} {} size {} {}", f.origin.x, panel.size.height - f.origin.y - f.size.height, f.size.width, f.size.height);
        }
        eprintln!("snapshot-search: window {gw} {gh} panel at {} {} size {} {} radius {RADIUS} scale {scale}", panel.origin.x, panel.origin.y, panel.size.width, panel.size.height);
        ok
    });
    eprintln!("snapshot-search: wrote={ok:?}");
}

#[cfg(test)]
mod tests {
    #[test]
    fn blur_filter_loads() {
        assert!(super::gaussian(10.0).is_some());
    }
}
