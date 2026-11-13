use super::*;
use objc2::{AnyThread, ClassType, Message};
use objc2_app_kit::{
    NSAnimatablePropertyContainer, NSAnimationContext, NSButton, NSCollectionElementKindSectionHeader, NSCollectionView, NSCollectionViewDataSource, NSCollectionViewDelegate,
    NSCollectionViewDelegateFlowLayout, NSCollectionViewDropOperation, NSCollectionViewElement, NSCollectionViewFlowLayout,
    NSCollectionViewItem, NSCollectionViewItemHighlightState, NSCollectionViewLayout, NSCollectionViewScrollPosition,
    NSIndexPathNSCollectionViewAdditions, NSTitlePosition,
};
use objc2_foundation::{NSEdgeInsets, NSIndexPath, NSSet};

// finder's icon view, the one good idea in there, minus what makes it miserable:
// icons you can leave lying on top of each other, a grid that has to be told to be a
// grid, previews that show up a minute after you've scrolled past, and not one word
// about what's inside anything. here it's always a grid, it reflows with the window,
// previews are asked for the moment a file gets on screen, every name has its size or
// item count under it, and the folder is split up by what things actually are.

pub(super) const SIZES: [f64; 4] = [64.0, 96.0, 128.0, 192.0];
const GAP: f64 = 4.0;
const EDGE: f64 = 12.0;
const HEAD_H: f64 = 36.0;
const INFO: NSInteger = 81;
const DOTS: NSInteger = 82;
const HEAD_TITLE: NSInteger = 83;
const HEAD_COUNT: NSInteger = 84;
const HEAD_MORE: NSInteger = 85;

thread_local! {
    pub(super) static ZOOM: Cell<usize> = const { Cell::new(1) };
    pub(super) static FORCE_EMPH: Cell<bool> = const { Cell::new(false) };
    static CELL: Cell<(f64, f64)> = const { Cell::new((136.0, 170.0)) };
    static ON_ITEM: Cell<bool> = const { Cell::new(false) };
    static TYPED: RefCell<(String, f64)> = const { RefCell::new((String::new(), 0.0)) };
    static COUNTS: RefCell<HashMap<String, (i64, Option<usize>)>> = RefCell::new(HashMap::new());
    // the file the pointer is resting on
    static HOVER: RefCell<Option<String>> = const { RefCell::new(None) };
    static PEEK: RefCell<Option<Peek>> = const { RefCell::new(None) };
    static PEEK_RUN: Cell<u64> = const { Cell::new(0) };
    static LEAVING: RefCell<Vec<Retained<NSView>>> = const { RefCell::new(Vec::new()) };
}

pub(super) fn icon() -> f64 {
    SIZES[ZOOM.with(|z| z.get()).min(SIZES.len() - 1)]
}

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}

fn locate(s: &Model, row: usize) -> (usize, usize) {
    for (i, x) in s.sections.iter().enumerate() {
        if row >= x.start && row < x.start + x.len {
            return (i, row - x.start);
        }
    }
    (0, row)
}

fn ip_of(row: usize) -> Retained<NSIndexPath> {
    let (sec, item) = m(|s| locate(s, row));
    NSIndexPath::indexPathForItem_inSection(item as NSInteger, sec as NSInteger)
}

fn row_of(ip: &NSIndexPath) -> Option<usize> {
    let (sec, item) = (ip.section() as usize, ip.item() as usize);
    m(|s| s.sections.get(sec).filter(|x| item < x.len).map(|x| x.start + item))
}

fn same(a: &AnyObject, b: &AnyObject) -> bool {
    std::ptr::eq(a, b)
}

fn emphasized() -> bool {
    if FORCE_EMPH.with(|f| f.get()) {
        return true;
    }
    let Some(u) = ui() else { return false };
    u.window.isKeyWindow() && u.window.firstResponder().is_some_and(|r| same(&r, &u.grid))
}

pub(super) fn on_item() -> bool {
    ON_ITEM.with(|o| o.get())
}

// ---------- the cell ----------

fn path_of(item: &NSCollectionViewItem) -> Option<String> {
    item.representedObject().and_then(|o| o.downcast::<NSString>().ok()).map(|p| p.to_string())
}

// how long a highlight takes to come or go. what you clicked is lit the moment you
// press: a selection that fades in reads as lag. the pointer's own plate eases in fast
// and lingers a little on the way out, so sweeping across the grid leaves a soft trail
// instead of a flicker
fn fade(v: &NSView, on: bool, dur: f64) {
    let a = if on { 1.0 } else { 0.0 };
    if dur <= 0.0 {
        // also cuts short a fade still running the other way
        if let Some(l) = v.layer() {
            l.removeAnimationForKey(&ns("opacity"));
        }
        v.setAlphaValue(a);
        return;
    }
    if v.alphaValue() == a {
        return;
    }
    NSAnimationContext::beginGrouping();
    let ctx = NSAnimationContext::currentContext();
    ctx.setDuration(dur);
    ctx.setTimingFunction(Some(&CAMediaTimingFunction::functionWithControlPoints(0.25, 0.1, 0.25, 1.0)));
    v.animator().setAlphaValue(a);
    NSAnimationContext::endGrouping();
}

// one icon, its name, one line of facts. the selected look copies finder (a plate
// behind the icon, a pill behind the name) but in the colour of the place. resting
// the pointer on a tile gives it a faint plate of its own.
fn style(item: &NSCollectionViewItem, animate: bool) {
    let subs = item.view().subviews();
    if subs.count() < 2 {
        return;
    }
    let (Ok(plate), Ok(pill)) = (subs.objectAtIndex(0).downcast::<NSBox>(), subs.objectAtIndex(1).downcast::<NSBox>()) else { return };
    let hl = item.highlightState();
    let picked = (item.isSelected() && hl != NSCollectionViewItemHighlightState::ForDeselection) || hl == NSCollectionViewItemHighlightState::ForSelection;
    let dropping = hl == NSCollectionViewItemHighlightState::AsDropTarget;
    let hovered = !picked && !dropping && HOVER.with(|h| h.borrow().is_some() && *h.borrow() == path_of(item));
    let hue = ACCENT.with(|a| a.get());
    let emph = emphasized();
    let editing = item.textField().is_some_and(|t| t.isEditable());
    plate.setBorderWidth(if dropping { 2.0 } else { 0.0 });
    if dropping {
        plate.setBorderColor(&tone(hue, 1.0, 1.0));
    }
    if dropping || picked || hovered {
        let fill = if dropping || (picked && emph) {
            tone(hue, 1.0, 0.24)
        } else if picked {
            NSColor::labelColor().colorWithAlphaComponent(0.10)
        } else {
            NSColor::labelColor().colorWithAlphaComponent(0.06)
        };
        plate.setFillColor(&fill);
    }
    let named = picked && !editing;
    if named {
        let fill = if emph { tone(hue, deep(hue), 1.0) } else { NSColor::labelColor().colorWithAlphaComponent(0.16) };
        pill.setFillColor(&fill);
    }
    let lit = dropping || picked || hovered;
    // pressed or dropped on: now. hovered: quick in. anything going away: a touch slower
    let (plate_t, pill_t) = if !animate {
        (0.0, 0.0)
    } else {
        (if picked || dropping { 0.0 } else if hovered { 0.08 } else { 0.2 }, if named { 0.0 } else { QUICK })
    };
    fade(&plate, lit, plate_t);
    fade(&pill, named, pill_t);
    if let Some(tf) = item.textField() {
        let ink = if named && emph { NSColor::whiteColor() } else { NSColor::labelColor() };
        tf.setTextColor(Some(&ink));
    }
}

// which tile is under the pointer. only the tile it left and the one it reached are touched
fn hover(cv: &NSCollectionView, e: Option<&NSEvent>) {
    // on the peek itself: nothing changes, so it can be read and clicked
    if let Some(e) = e {
        let on_peek = PEEK.with(|p| {
            p.borrow().as_ref().is_some_and(|p| {
                let at = p.card.convertPoint_fromView(e.locationInWindow(), None);
                let b = p.body;
                at.x >= b.origin.x - 12.0 && at.y >= b.origin.y && at.x < b.origin.x + b.size.width + 12.0 && at.y < b.origin.y + b.size.height
            })
        });
        if on_peek {
            return;
        }
    }
    let now = e
        .and_then(|e| cv.indexPathForItemAtPoint(cv.convertPoint_fromView(e.locationInWindow(), None)))
        .and_then(|ip| m(|s| row_of_in(s, &ip).and_then(|r| s.rows.get(r)).map(|e| e.path.clone())));
    let before = HOVER.with(|h| h.replace(now.clone()));
    if before == now {
        return;
    }
    peek_arm(now.clone());
    let Some(u) = ui() else { return };
    for p in [before, now].into_iter().flatten() {
        if let Some(it) = shown(&u, &p) {
            style(&it, true);
        }
    }
}

// ---------- peeking into a folder ----------

// rest the pointer on a folder and after a moment it opens up: a card comes out of
// it with what's inside. finder makes you open the folder, look, and come back.
// click something in the card to go straight to it.

const PEEK_COLS: usize = 4;
const PEEK_MAX: usize = 12;
const PEEK_CELL: (f64, f64) = (68.0, 62.0);
const PEEK_PAD: f64 = 12.0;
const PEEK_WAIT: u64 = 650;
// the little square that sticks out of the card's edge and points at the folder
const PEEK_NIB: f64 = 14.0;

struct Peek {
    path: String,
    card: Retained<NSView>,
    // the card itself inside `card`, which also holds the pointer
    body: NSRect,
    // where the pointer touches the folder, in `card` coordinates. it grows out of here
    tip: NSPoint,
    ents: Vec<Ent>,
    tile: Option<Retained<NSImageView>>,
}

fn bump_peek() -> u64 {
    PEEK_RUN.with(|c| {
        c.set(c.get() + 1);
        c.get()
    })
}

fn peek_arm(path: Option<String>) {
    let run = bump_peek();
    if PEEK.with(|p| p.borrow().as_ref().is_some_and(|p| Some(&p.path) != path.as_ref())) {
        peek_close();
    }
    let Some(path) = path else { return };
    if PEEK.with(|p| p.borrow().is_some()) {
        return;
    }
    let folder = m(|s| s.rows.iter().find(|e| e.path == path).is_some_and(|e| e.dir && category(e) == 0));
    if folder {
        later(PEEK_WAIT, move || peek_fire(run, path));
    }
}

// still on the same folder after the wait? then read it, off the main thread
fn peek_fire(run: u64, path: String) {
    if PEEK_RUN.with(|c| c.get()) != run {
        return;
    }
    let Some(u) = ui() else { return };
    let still = HOVER.with(|h| h.borrow().as_deref() == Some(path.as_str()));
    if !still || !grid_mode() || !u.window.isKeyWindow() || m(|s| s.renaming.is_some()) {
        return;
    }
    dispatch2::DispatchQueue::global_queue(dispatch2::GlobalQueueIdentifier::QualityOfService(dispatch2::DispatchQoS::UserInitiated)).exec_async(move || {
        let (ents, total) = peek_read(&path);
        dispatch2::DispatchQueue::main().exec_async(move || {
            let still = HOVER.with(|h| h.borrow().as_deref() == Some(path.as_str()));
            if PEEK_RUN.with(|c| c.get()) == run && still {
                peek_show(path, ents, total, true);
            }
        });
    });
}

fn peek_read(path: &str) -> (Vec<Ent>, usize) {
    let mut ents = dir::list(path);
    ents.retain(|e| !e.hidden);
    ents.sort_by(|a, b| b.dir.cmp(&a.dir).then_with(|| nat_cmp(&a.lc, &b.lc)));
    let total = ents.len();
    ents.truncate(PEEK_MAX);
    (ents, total)
}

fn flat_box(mtm: MainThreadMarker, r: NSRect, radius: f64, fill: &NSColor) -> Retained<NSBox> {
    let b = NSBox::initWithFrame(NSBox::alloc(mtm), r);
    b.setBoxType(NSBoxType::Custom);
    b.setTitlePosition(NSTitlePosition::NoTitle);
    b.setBorderWidth(0.0);
    b.setCornerRadius(radius);
    b.setFillColor(fill);
    b.setContentViewMargins(NSSize::new(0.0, 0.0));
    b
}

// side 1: points left, -1: points right, 0: points up
fn triangle(w: f64, h: f64, side: f64, fill: &NSColor) -> Retained<NSImage> {
    let fill = fill.retain();
    let draw = block2::RcBlock::new(move |r: NSRect| -> Bool {
        let (w, h) = (r.size.width, r.size.height);
        let pts = match side {
            s if s > 0.0 => [(0.0, h / 2.0), (w, 0.0), (w, h)],
            s if s < 0.0 => [(w, h / 2.0), (0.0, 0.0), (0.0, h)],
            _ => [(w / 2.0, h), (0.0, 0.0), (w, 0.0)],
        };
        let p = NSBezierPath::bezierPath();
        p.moveToPoint(NSPoint::new(pts[0].0, pts[0].1));
        p.lineToPoint(NSPoint::new(pts[1].0, pts[1].1));
        p.lineToPoint(NSPoint::new(pts[2].0, pts[2].1));
        p.closePath();
        fill.setFill();
        p.fill();
        Bool::YES
    });
    NSImage::imageWithSize_flipped_drawingHandler(NSSize::new(w, h), false, &draw)
}

// it hangs beside the folder like a popover, pointing at it, so the folder and its
// name stay in view. only when there's no room either side does it go underneath
fn peek_show(path: String, ents: Vec<Ent>, total: usize, animate: bool) {
    let (Some(u), Some(mtm)) = (ui(), MainThreadMarker::new()) else { return };
    let Some(item) = shown(&u, &path) else { return };
    let dark = is_dark(&u.grid);
    let n = ents.len();
    let rows = n.div_ceil(PEEK_COLS).max(1) as f64;
    let w = PEEK_PAD * 2.0 + PEEK_COLS as f64 * PEEK_CELL.0;
    let body_h = if n == 0 { 30.0 } else { rows * PEEK_CELL.1 };
    let more = if total > n { 18.0 } else { 0.0 };
    let h = PEEK_PAD + 18.0 + 8.0 + body_h + more + PEEK_PAD - 2.0;

    let tile = item.view().convertRect_toView(item.view().bounds(), Some(&u.stage));
    let stage = u.stage.bounds().size;
    let ceiling = stage.height - u.grid_scroll.contentInsets().top - 8.0;
    let s = icon();
    let mid = tile.origin.x + tile.size.width / 2.0;
    let eye = tile.origin.y + tile.size.height - 8.0 - s / 2.0;
    let reach = s / 2.0 + 12.0;
    let nib = PEEK_NIB * std::f64::consts::FRAC_1_SQRT_2;
    let side = if mid + reach + nib + w <= stage.width - 8.0 {
        1.0
    } else if mid - reach - nib - w >= 8.0 {
        -1.0
    } else {
        0.0
    };
    // card rect (x, y) in the stage, and the point it comes out of
    let (x, y, tip) = if side != 0.0 {
        let x = if side > 0.0 { mid + reach + nib } else { mid - reach - nib - w };
        let y = (eye - h + 34.0).min(ceiling - h).max(8.0);
        let ty = eye.clamp(y + 18.0, y + h - 18.0);
        (x, y, NSPoint::new(if side > 0.0 { x - nib } else { x + w + nib }, ty))
    } else {
        let x = (mid - w / 2.0).clamp(8.0, (stage.width - w - 8.0).max(8.0));
        let y = (eye - s / 2.0 - nib - 4.0 - h).max(8.0);
        (x, y, NSPoint::new(mid.clamp(x + 18.0, x + w - 18.0), y + h + nib))
    };
    // the card view takes in the pointer too: room for it on whichever side it's on
    let frame = match side {
        s if s > 0.0 => rect(x - nib, y, w + nib, h),
        s if s < 0.0 => rect(x, y, w + nib, h),
        _ => rect(x, y, w, h + nib),
    };
    let card = NSView::initWithFrame(NSView::alloc(mtm), NSRect::new(NSPoint::new(frame.origin.x.round(), frame.origin.y.round()), frame.size));
    card.setWantsLayer(true);
    let body = rect(if side > 0.0 { nib } else { 0.0 }, 0.0, w, h);
    let tip = NSPoint::new(tip.x - card.frame().origin.x, tip.y - card.frame().origin.y);
    let fill = if dark { NSColor::colorWithSRGBRed_green_blue_alpha(0.2, 0.2, 0.215, 1.0) } else { NSColor::colorWithSRGBRed_green_blue_alpha(1.0, 1.0, 1.0, 1.0) };
    // the pointer: a triangle in the card's own paint, its base tucked a point under the
    // card's edge so no seam shows. drawn, not a box turned on its corner: appkit's layout
    // engine can't cope with rotated views and took the whole app down with a NaN
    let (pw, ph) = if side != 0.0 { (nib + 1.0, nib * 2.0) } else { (nib * 2.0, nib + 1.0) };
    let (px, py) = match side {
        s if s > 0.0 => (0.0, tip.y - nib),
        s if s < 0.0 => (w - 1.0, tip.y - nib),
        _ => (tip.x - nib, h - 1.0),
    };
    let pointer = NSImageView::initWithFrame(NSImageView::alloc(mtm), rect(px, py, pw, ph));
    pointer.setImageScaling(NSImageScaling::ScaleAxesIndependently);
    pointer.setImage(Some(&triangle(pw, ph, side, &fill)));
    card.addSubview(&pointer);
    card.addSubview(&flat_box(mtm, body, 13.0, &fill));
    let shadow = objc2_app_kit::NSShadow::new();
    shadow.setShadowColor(Some(&NSColor::blackColor().colorWithAlphaComponent(if dark { 0.5 } else { 0.16 })));
    shadow.setShadowBlurRadius(if dark { 18.0 } else { 22.0 });
    shadow.setShadowOffset(NSSize::new(0.0, -5.0));
    card.setShadow(Some(&shadow));
    let inner = NSView::initWithFrame(NSView::alloc(mtm), body);
    card.addSubview(&inner);

    let text = |t: &str, font: &NSFont, color: &NSColor, r: NSRect, align: NSTextAlignment| {
        let l = NSTextField::labelWithString(&ns(t), mtm);
        l.setFont(Some(font));
        l.setTextColor(Some(color));
        l.setAlignment(align);
        l.setLineBreakMode(NSLineBreakMode::ByTruncatingMiddle);
        l.setFrame(r);
        inner.addSubview(&l);
        l
    };
    let name = path.rsplit('/').next().unwrap_or("");
    let count = match total {
        0 => "Empty".to_owned(),
        1 => "1 item".to_owned(),
        t => format!("{t} items"),
    };
    let top = h - PEEK_PAD - 16.0;
    text(name, &NSFont::systemFontOfSize_weight(12.0, unsafe { NSFontWeightSemibold }), &NSColor::labelColor(), rect(PEEK_PAD + 2.0, top, w - PEEK_PAD * 2.0 - 78.0, 16.0), NSTextAlignment::Left);
    text(&count, &NSFont::monospacedDigitSystemFontOfSize_weight(11.0, unsafe { NSFontWeightRegular }), &NSColor::tertiaryLabelColor(), rect(w - PEEK_PAD - 80.0, top, 78.0, 15.0), NSTextAlignment::Right);

    for (i, e) in ents.iter().enumerate() {
        let (col, row) = ((i % PEEK_COLS) as f64, (i / PEEK_COLS) as f64);
        let cx = PEEK_PAD + col * PEEK_CELL.0;
        let cy = top - 8.0 - (row + 1.0) * PEEK_CELL.1;
        let cell = NSView::initWithFrame(NSView::alloc(mtm), rect(cx, cy, PEEK_CELL.0, PEEK_CELL.1));
        let ehue = if e.dir { m(|s| hue_in(&s.places, &e.path)) } else { BLUE };
        let (img, preview) = icon_kind(e, ehue, 32.0);
        let iv = NSImageView::initWithFrame(NSImageView::alloc(mtm), rect(0.0, 0.0, 32.0, 32.0));
        iv.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
        // the tile itself is the frame of reference here: 32pt picture, centred, near the top
        place_icon(&iv, &img, preview, false, PEEK_CELL.0, PEEK_CELL.1 + 4.0, 32.0);
        cell.addSubview(&iv);
        let l = NSTextField::labelWithString(&ns(&e.name), mtm);
        l.setFont(Some(&NSFont::systemFontOfSize(10.0)));
        l.setTextColor(Some(&NSColor::secondaryLabelColor()));
        l.setAlignment(NSTextAlignment::Center);
        l.setLineBreakMode(NSLineBreakMode::ByTruncatingMiddle);
        l.setFrame(rect(2.0, 4.0, PEEK_CELL.0 - 4.0, 13.0));
        cell.addSubview(&l);
        let hit = unsafe { NSButton::buttonWithTitle_target_action(&ns(""), Some(&u._grid_ctl), Some(sel!(lfPeekOpen:)), mtm) };
        hit.setFrame(rect(0.0, 0.0, PEEK_CELL.0, PEEK_CELL.1));
        hit.setTransparent(true);
        hit.setTag(i as NSInteger);
        hit.setToolTip(Some(&ns(&e.name)));
        cell.addSubview(&hit);
        inner.addSubview(&cell);
    }
    if n == 0 {
        text("Nothing in here", &NSFont::systemFontOfSize(11.0), &NSColor::tertiaryLabelColor(), rect(PEEK_PAD, top - 8.0 - 24.0, w - PEEK_PAD * 2.0, 15.0), NSTextAlignment::Center);
    }
    if total > n {
        text(&format!("and {} more", total - n), &NSFont::systemFontOfSize(10.5), &NSColor::tertiaryLabelColor(), rect(PEEK_PAD, PEEK_PAD - 4.0, w - PEEK_PAD * 2.0, 14.0), NSTextAlignment::Center);
    }

    u.stage.addSubview(&card);
    let tile_icon = item.imageView();
    if animate && !reduce_motion() {
        peek_animate(&card, tip, tile_icon.as_deref());
    }
    PEEK.with(|p| *p.borrow_mut() = Some(Peek { path, card, body, tip, ents, tile: tile_icon }));
}

// the folder lifts a hair and the card opens out of the point that touches it, all
// of it in one piece. no confetti of items arriving one by one
fn peek_animate(card: &NSView, tip: NSPoint, tile: Option<&NSImageView>) {
    if let Some(l) = card.layer() {
        l.addAnimation_forKey(&basic("opacity", 0.0, 1.0, 0.14, &crate::view::ease_out()), Some(&ns("in")));
        scale_about(&l, tip, 0.95, 1.0, "grow", false, &|k, f, t| spring(k, f, t, 0.26));
    }
    if let Some(l) = tile.and_then(|t| t.layer()) {
        let b = l.bounds().size;
        let from = shown_scale(&l);
        scale_about(&l, NSPoint::new(b.width / 2.0, b.height / 2.0), from, 1.04, "peek", true, &|k, f, t| spring(k, f, t, 0.3));
    }
}

pub(super) fn peek_close() {
    bump_peek();
    let Some(p) = PEEK.with(|p| p.borrow_mut().take()) else { return };
    if let Some(l) = p.tile.as_ref().and_then(|t| t.layer()) {
        // back down from wherever the lift had got to, so a quick in-and-out never jumps
        let b = l.bounds().size;
        let from = shown_scale(&l);
        drop_scale(&l, "peek");
        scale_about(&l, NSPoint::new(b.width / 2.0, b.height / 2.0), from, 1.0, "unpeek", false, &|k, f, t| spring(k, f, t, 0.24));
    }
    // goes the way it came, quicker
    if let Some(l) = p.card.layer() {
        scale_about(&l, p.tip, 1.0, 0.97, "shrink", true, &|k, f, t| basic(k, f, t, QUICK, &ease_in()));
    }
    NSAnimationContext::beginGrouping();
    NSAnimationContext::currentContext().setDuration(QUICK);
    p.card.animator().setAlphaValue(0.0);
    NSAnimationContext::endGrouping();
    LEAVING.with(|l| l.borrow_mut().push(p.card));
    later(170, || {
        LEAVING.with(|l| {
            for v in l.borrow_mut().drain(..) {
                v.removeFromSuperview();
            }
        })
    });
}

fn peek_open(sender: Option<&AnyObject>) {
    let (Some(u), Some(i)) = (ui(), sender.and_then(|s| s.downcast_ref::<NSButton>()).map(|b| b.tag())) else { return };
    let Some(e) = PEEK.with(|p| p.borrow().as_ref().and_then(|p| p.ents.get(i as usize).cloned())) else { return };
    // the folder's contents grow out of the little icon that was clicked in the card
    if let Some(b) = sender.and_then(|s| s.downcast_ref::<NSButton>()).filter(|_| e.dir && category(&e) == 0) {
        let f = b.bounds();
        let mid = NSPoint::new(f.size.width / 2.0, f.size.height / 2.0 + 8.0);
        ORIGIN.with(|o| o.set(Some(u.grid_scroll.convertPoint_fromView(mid, Some(b)))));
    }
    peek_close();
    if e.dir && category(&e) == 0 {
        go(&u, &e.path, true, None);
        focus(&u);
    } else {
        NSWorkspace::sharedWorkspace().openURL(&file_url(&e.path));
    }
}

// for the off-screen renders: open the peek on a row right now, no waiting, no motion
pub(super) fn peek_test(u: &Ui, row: usize) {
    u.grid.layoutSubtreeIfNeeded();
    let Some(path) = m(|s| s.rows.get(row).map(|e| e.path.clone())) else { return };
    HOVER.with(|h| *h.borrow_mut() = Some(path.clone()));
    let (ents, total) = peek_read(&path);
    eprintln!("snapshot: peek into {} shows {} of {total}", path.rsplit('/').next().unwrap_or(""), ents.len());
    peek_show(path, ents, total, u.window.isVisible());
}

fn cell_hit(v: &CellView, p: NSPoint) -> Option<Retained<NSView>> {
    // while a name is being typed the text field needs its clicks. the rest of the time
    // the whole tile is one target, so a click on the picture or the text all count
    if m(|s| s.renaming.is_some()) {
        return unsafe { msg_send![super(v, NSView::class()), hitTest: p] };
    }
    let f = v.frame();
    let inside = p.x >= f.origin.x && p.x < f.origin.x + f.size.width && p.y >= f.origin.y && p.y < f.origin.y + f.size.height;
    inside.then(|| Retained::into_super(v.retain()))
}

fn build_cell(item: &NSCollectionViewItem) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let (cw, ch) = CELL.with(|c| c.get());
    let root: Retained<CellView> = unsafe { msg_send![CellView::alloc(mtm), initWithFrame: rect(0.0, 0.0, cw, ch)] };
    // every frame in here is set by hand in configure()
    root.setAutoresizesSubviews(false);
    let boxed = |radius: f64| {
        let b = NSBox::initWithFrame(NSBox::alloc(mtm), rect(0.0, 0.0, 10.0, 10.0));
        b.setBoxType(NSBoxType::Custom);
        b.setTitlePosition(NSTitlePosition::NoTitle);
        b.setBorderWidth(0.0);
        b.setCornerRadius(radius);
        b.setFillColor(&NSColor::clearColor());
        b.setContentViewMargins(NSSize::new(0.0, 0.0));
        b
    };
    for radius in [10.0, 5.0] {
        let b = boxed(radius);
        b.setAlphaValue(0.0);
        root.addSubview(&b);
    }
    let iv = NSImageView::initWithFrame(NSImageView::alloc(mtm), rect(0.0, 0.0, 10.0, 10.0));
    iv.setImageScaling(NSImageScaling::ScaleProportionallyUpOrDown);
    root.addSubview(&iv);
    let name = NSTextField::labelWithString(&ns(""), mtm);
    name.setFont(Some(&NSFont::systemFontOfSize(12.0)));
    name.setAlignment(NSTextAlignment::Center);
    name.setMaximumNumberOfLines(2);
    if let Some(c) = name.cell() {
        c.setUsesSingleLineMode(false);
        c.setScrollable(false);
        c.setWraps(true);
        c.setLineBreakMode(NSLineBreakMode::ByWordWrapping);
        c.setTruncatesLastVisibleLine(true);
    }
    root.addSubview(&name);
    let info = NSTextField::labelWithString(&ns(""), mtm);
    info.setTag(INFO);
    info.setFont(Some(&NSFont::monospacedDigitSystemFontOfSize_weight(10.5, unsafe { NSFontWeightRegular })));
    info.setAlignment(NSTextAlignment::Center);
    info.setLineBreakMode(NSLineBreakMode::ByTruncatingMiddle);
    root.addSubview(&info);
    let dots = NSImageView::initWithFrame(NSImageView::alloc(mtm), rect(0.0, 0.0, 8.0, 8.0));
    dots.setTag(DOTS);
    dots.setImageScaling(NSImageScaling::ScaleNone);
    root.addSubview(&dots);
    item.setView(&root);
    item.setImageView(Some(&iv));
    item.setTextField(Some(&name));
}

// a preview keeps its own shape inside the square, with the corners taken off and a
// hairline round it so a dark screenshot doesn't melt into a dark window. an icon
// just fills the square. `swap` cross-fades from whatever was showing.
fn place_icon(iv: &NSImageView, img: &NSImage, preview: bool, swap: bool, cw: f64, ch: f64, s: f64) {
    let sz = img.size();
    let k = (s / sz.width.max(1.0)).min(s / sz.height.max(1.0)).min(1.0);
    let (w, h) = ((sz.width * k).round().max(1.0), (sz.height * k).round().max(1.0));
    iv.setFrame(rect(((cw - w) / 2.0).round(), (ch - 8.0 - s + (s - h) / 2.0).round(), w, h));
    iv.setWantsLayer(true);
    if let Some(l) = iv.layer() {
        l.setCornerRadius(if preview { (s / 22.0).clamp(3.0, 7.0) } else { 0.0 });
        l.setMasksToBounds(preview);
        l.setBorderWidth(if preview { 0.5 } else { 0.0 });
        l.setBorderColor(Some(&objc2_core_graphics::CGColor::new_generic_gray(0.5, 0.4)));
        if swap {
            let t = objc2_quartz_core::CATransition::new();
            t.setDuration(0.22);
            l.addAnimation_forKey(&t, Some(&ns("swap")));
        }
    }
    iv.setImage(Some(img));
}

fn count_text(n: usize) -> String {
    match n {
        0 => "No items".to_owned(),
        1 => "1 item".to_owned(),
        n => format!("{n} items"),
    }
}

// how many things are in a folder, counted off the main thread and remembered until
// the folder changes
fn folder_count(e: &Ent) -> Option<usize> {
    if let Some(got) = COUNTS.with(|c| c.borrow().get(&e.path).filter(|x| x.0 == e.mtime).map(|x| x.1)) {
        return got;
    }
    COUNTS.with(|c| {
        let mut c = c.borrow_mut();
        if c.len() > 20_000 {
            c.clear();
        }
        c.insert(e.path.clone(), (e.mtime, None));
    });
    let (path, mtime) = (e.path.clone(), e.mtime);
    dispatch2::DispatchQueue::global_queue(dispatch2::GlobalQueueIdentifier::QualityOfService(dispatch2::DispatchQoS::Utility)).exec_async(move || {
        let n = fs::read_dir(&path).map(|d| d.flatten().filter(|x| !x.file_name().as_encoded_bytes().starts_with(b".")).count());
        dispatch2::DispatchQueue::main().exec_async(move || {
            let Ok(n) = n else { return };
            COUNTS.with(|c| c.borrow_mut().insert(path.clone(), (mtime, Some(n))));
            let Some(u) = ui() else { return };
            if let Some(item) = shown(&u, &path) {
                if let Some(info) = item.view().viewWithTag(INFO).and_then(|v| v.downcast::<NSTextField>().ok()) {
                    info.setStringValue(&ns(&count_text(n)));
                }
            }
        });
    });
    None
}

fn shown(u: &Ui, path: &str) -> Option<Retained<NSCollectionViewItem>> {
    let items = u.grid.visibleItems();
    (0..items.count()).map(|i| items.objectAtIndex(i)).find(|it| {
        it.representedObject().and_then(|o| o.downcast::<NSString>().ok()).is_some_and(|p| p.to_string() == path)
    })
}

fn configure(u: &Ui, item: &NSCollectionViewItem, row: usize) {
    let Some((e, cut, searching, hue)) =
        m(|s| s.rows.get(row).map(|e| (e.clone(), s.cut.contains(&e.path), !s.query.trim().is_empty(), if e.dir { hue_in(&s.places, &e.path) } else { BLUE })))
    else {
        return;
    };
    let view = item.view();
    let (Some(iv), Some(tf)) = (item.imageView(), item.textField()) else { return };
    let subs = view.subviews();
    if subs.count() < 2 {
        return;
    }
    let (cw, ch) = CELL.with(|c| c.get());
    let s = icon();
    let tag = ns(&e.path);
    let tag: &AnyObject = &tag;
    unsafe { item.setRepresentedObject(Some(tag)) };

    let (img, preview) = icon_kind(&e, hue, s);
    place_icon(&iv, &img, preview, false, cw, ch, s);
    subs.objectAtIndex(0).setFrame(rect(((cw - s) / 2.0).round() - 6.0, ch - s - 14.0, s + 12.0, s + 12.0));

    // tag dots sit in front of the name and the pair is centred as one thing
    let tags = tags_of(&e);
    let dw = if tags.is_empty() { 0.0 } else { tags.len() as f64 * 11.0 - 3.0 + 5.0 };
    unsafe { tf.setDelegate(Some(ProtocolObject::from_ref(&*u.ctl))) };
    tf.setEditable(false);
    tf.setStringValue(&ns(&e.name));
    let max_w = cw - 10.0 - dw;
    let need = tf.cell().map_or(NSSize::new(max_w, 16.0), |c| c.cellSizeForBounds(rect(0.0, 0.0, max_w, 32.0)));
    let (w, h) = (need.width.ceil().clamp(12.0, max_w), need.height.ceil().clamp(15.0, 32.0));
    let top = ch - s - 18.0;
    let x0 = ((cw - w - dw) / 2.0).round();
    tf.setFrame(rect(x0 + dw, top - h, w, h));
    subs.objectAtIndex(1).setFrame(rect(x0 + dw - 4.0, top - h - 1.0, w + 8.0, h + 2.0));
    if let Some(d) = view.viewWithTag(DOTS).and_then(|v| v.downcast::<NSImageView>().ok()) {
        d.setHidden(dw == 0.0);
        if dw > 0.0 {
            d.setFrame(rect(x0 - 2.0, top - 12.0, dw - 5.0, 8.0));
            d.setImage(dots(&tags).as_deref());
        }
    }

    if let Some(info) = view.viewWithTag(INFO).and_then(|v| v.downcast::<NSTextField>().ok()) {
        let text = if searching && !e.place.is_empty() {
            e.place.clone()
        } else if e.dir && category(&e) == 0 {
            folder_count(&e).map(count_text).unwrap_or_default()
        } else if e.dir {
            String::new()
        } else if e.size == 0 {
            "Zero bytes".to_owned()
        } else {
            NSByteCountFormatter::stringFromByteCount_countStyle(e.size as i64, NSByteCountFormatterCountStyle::File).to_string()
        };
        info.setStringValue(&ns(&text));
        info.setTextColor(Some(&NSColor::secondaryLabelColor()));
        info.setFrame(rect(4.0, top - h - 18.0, cw - 8.0, 14.0));
    }
    view.setAlphaValue(if cut || e.hidden { 0.5 } else { 1.0 });
    style(item, false);
}

fn make_item(cv: &NSCollectionView, ip: &NSIndexPath) -> Retained<NSCollectionViewItem> {
    let item = cv.makeItemWithIdentifier_forIndexPath(&ns("item"), ip);
    if let (Some(u), Some(row)) = (ui(), row_of(ip)) {
        configure(&u, &item, row);
    }
    item
}

fn make_header(cv: &NSCollectionView, kind: &NSString, ip: &NSIndexPath) -> Retained<NSView> {
    let v = cv.makeSupplementaryViewOfKind_withIdentifier_forIndexPath(kind, &ns("head"), ip);
    fill_header(cv, &v, ip.section() as usize);
    v
}

// a heading per kind of thing: its name, how many, and a hairline under it. the heading
// sticks to the top while its section scrolls past, so you always know where you are,
// and clicking it lists every section to jump to. that used to be a strip of coloured
// chips on top, which said the same thing as the headings right under it.
fn fill_header(cv: &NSCollectionView, v: &NSView, sec: usize) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let (title, n, many) = m(|s| {
        let (t, n) = s.sections.get(sec).map_or((String::new(), 0), |x| (x.title.clone(), x.len));
        (t, n, s.sections.len() > 1)
    });
    let w = v.bounds().size.width.max(cv.bounds().size.width);
    let field = |tag: NSInteger, font: Retained<NSFont>| -> Retained<NSTextField> {
        if let Some(t) = v.viewWithTag(tag).and_then(|x| x.downcast::<NSTextField>().ok()) {
            return t;
        }
        let t = NSTextField::labelWithString(&ns(""), mtm);
        t.setTag(tag);
        t.setFont(Some(&font));
        v.addSubview(&t);
        t
    };
    let x0 = EDGE + 8.0;
    let t = field(HEAD_TITLE, NSFont::systemFontOfSize_weight(13.0, unsafe { NSFontWeightSemibold }));
    t.setTextColor(Some(&NSColor::labelColor()));
    t.setStringValue(&ns(&title));
    t.sizeToFit();
    t.setFrameOrigin(NSPoint::new(x0, 9.0));
    let c = field(HEAD_COUNT, NSFont::monospacedDigitSystemFontOfSize_weight(12.0, unsafe { NSFontWeightRegular }));
    c.setTextColor(Some(&NSColor::tertiaryLabelColor()));
    c.setStringValue(&ns(&n.to_string()));
    c.sizeToFit();
    let cx = x0 + t.frame().size.width + 4.0;
    c.setFrameOrigin(NSPoint::new(cx, 9.5));

    // a small chevron says the heading opens something, only when there's somewhere to go
    let more = match v.viewWithTag(HEAD_MORE).and_then(|x| x.downcast::<NSImageView>().ok()) {
        Some(iv) => iv,
        None => {
            let iv = NSImageView::initWithFrame(NSImageView::alloc(mtm), rect(0.0, 0.0, 10.0, 10.0));
            iv.setTag(HEAD_MORE);
            let cfg = NSImageSymbolConfiguration::configurationWithPointSize_weight(8.5, unsafe { objc2_app_kit::NSFontWeightBold });
            iv.setImage(symbol("chevron.down").and_then(|i| i.imageWithSymbolConfiguration(&cfg)).as_deref());
            iv.setContentTintColor(Some(&NSColor::tertiaryLabelColor()));
            v.addSubview(&iv);
            iv
        }
    };
    more.setHidden(!many);
    more.setFrameOrigin(NSPoint::new(cx + c.frame().size.width + 3.0, 12.0));

    let subs = v.subviews();
    let line = match (0..subs.count()).find_map(|i| subs.objectAtIndex(i).downcast::<NSBox>().ok()) {
        Some(b) => b,
        None => {
            let b = NSBox::initWithFrame(NSBox::alloc(mtm), rect(0.0, 0.0, 10.0, 1.0));
            b.setBoxType(NSBoxType::Custom);
            b.setTitlePosition(NSTitlePosition::NoTitle);
            b.setBorderWidth(0.0);
            b.setFillColor(&NSColor::separatorColor());
            v.addSubview(&b);
            b
        }
    };
    line.setFrame(rect(x0, 0.0, (w - x0 * 2.0).max(10.0), 1.0));
}

fn header_section(v: &NSView) -> Option<usize> {
    let u = ui()?;
    let kind = unsafe { NSCollectionElementKindSectionHeader };
    let ips = u.grid.indexPathsForVisibleSupplementaryElementsOfKind(kind).allObjects();
    (0..ips.count()).map(|i| ips.objectAtIndex(i)).find_map(|ip| {
        let hv = u.grid.supplementaryViewForElementKind_atIndexPath(kind, &ip)?;
        std::ptr::eq(&*hv, v).then_some(ip.section() as usize)
    })
}

// the jump list opens with the heading you clicked sitting right where the heading is
fn header_menu(v: &NSView) -> bool {
    let (Some(u), Some(mtm), Some(sec)) = (ui(), MainThreadMarker::new(), header_section(v)) else { return false };
    let secs: Vec<(String, usize)> = m(|s| s.sections.iter().map(|x| (x.title.clone(), x.len)).collect());
    if secs.len() < 2 {
        return false;
    }
    let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), &ns(""));
    menu.setAutoenablesItems(false);
    for (i, (title, n)) in secs.iter().enumerate() {
        let it = NSMenuItem::new(mtm);
        it.setTitle(&ns(title));
        it.setTag(i as NSInteger);
        unsafe {
            it.setTarget(Some(&u._grid_ctl));
            it.setAction(Some(sel!(lfJump:)));
        }
        it.setBadge(Some(&objc2_app_kit::NSMenuItemBadge::initWithCount(objc2_app_kit::NSMenuItemBadge::alloc(), *n as NSInteger)));
        it.setState(if i == sec { objc2_app_kit::NSControlStateValueOn } else { objc2_app_kit::NSControlStateValueOff });
        menu.addItem(&it);
    }
    let at = v.viewWithTag(HEAD_TITLE).map_or(NSPoint::new(EDGE, 26.0), |t| {
        let f = t.frame();
        NSPoint::new(f.origin.x - 22.0, f.origin.y + f.size.height + 2.0)
    });
    menu.popUpMenuPositioningItem_atLocation_inView(menu.itemWithTag(sec as NSInteger).as_deref(), at, Some(v));
    true
}

fn jump(sender: Option<&AnyObject>) {
    let tag = sender.and_then(|s| s.downcast_ref::<NSMenuItem>()).map(|i| i.tag());
    if let (Some(u), Some(sec)) = (ui(), tag) {
        jump_to(&u, sec);
    }
}

// scrolls so the section's heading sits at the top, the same place it would stick to
pub(super) fn jump_to(u: &Ui, sec: NSInteger) {
    let ip = NSIndexPath::indexPathForItem_inSection(0, sec);
    // a stuck heading reports where it's stuck, so go by the first tile under it instead
    let Some(at) = u.grid.layoutAttributesForItemAtIndexPath(&ip) else { return };
    let clip = u.grid_scroll.contentView();
    let top = u.grid_scroll.contentInsets().top;
    let want = if sec == 0 { -top } else { at.frame().origin.y - 2.0 - HEAD_H - top };
    let last = (u.grid.frame().size.height - clip.bounds().size.height).max(-top);
    let to = NSPoint::new(0.0, want.min(last).max(-top));
    NSAnimationContext::beginGrouping();
    let ctx = NSAnimationContext::currentContext();
    ctx.setDuration(0.4);
    ctx.setTimingFunction(Some(&settle()));
    clip.animator().setBoundsOrigin(to);
    NSAnimationContext::endGrouping();
    u.grid_scroll.reflectScrolledClipView(&clip);
}

// ---------- input ----------

fn type_select(u: &Ui, e: &NSEvent) -> bool {
    let mods = e.modifierFlags();
    if mods.contains(NSEventModifierFlags::Command) || mods.contains(NSEventModifierFlags::Control) {
        return false;
    }
    let Some(c) = e.charactersIgnoringModifiers().and_then(|s| s.to_string().chars().next()) else { return false };
    if c.is_control() || (0xF700..=0xF8FF).contains(&(c as u32)) {
        return false;
    }
    let now = e.timestamp();
    let typed = TYPED.with(|t| {
        let mut t = t.borrow_mut();
        if now - t.1 > 0.9 {
            t.0.clear();
        }
        t.0.extend(c.to_lowercase());
        t.1 = now;
        t.0.clone()
    });
    if let Some(r) = m(|s| s.rows.iter().position(|e| e.lc.starts_with(&typed))) {
        select_rows(u, &[r], true);
        update_status(u);
    }
    true
}

fn key(e: &NSEvent) -> bool {
    peek_close();
    let Some(u) = ui() else { return false };
    let cmd = e.modifierFlags().contains(NSEventModifierFlags::Command);
    match (e.keyCode(), cmd) {
        (36 | 76, false) | (125, true) => open_targets(&u),
        (49, false) => toggle_ql(&u),
        (120, false) => begin_rename(&u),
        (53, false) => escape(&u),
        _ => return type_select(&u, e),
    }
    true
}

fn clicked(cv: &NSCollectionView, e: &NSEvent) {
    if e.clickCount() != 2 {
        return;
    }
    let Some(u) = ui() else { return };
    if cv.indexPathForItemAtPoint(cv.convertPoint_fromView(e.locationInWindow(), None)).is_some() {
        open_targets(&u);
    }
}

// right click on something that isn't picked picks it, on empty space it drops the
// selection. so the menu is always about exactly what's highlighted
fn menu_for(cv: &NSCollectionView, e: &NSEvent) -> Option<Retained<NSMenu>> {
    let u = ui()?;
    match cv.indexPathForItemAtPoint(cv.convertPoint_fromView(e.locationInWindow(), None)) {
        Some(ip) => {
            if !cv.selectionIndexPaths().containsObject(&ip) {
                cv.setSelectionIndexPaths(&NSSet::from_retained_slice(&[ip]));
            }
            ON_ITEM.with(|o| o.set(true));
        }
        None => {
            cv.setSelectionIndexPaths(&NSSet::new());
            ON_ITEM.with(|o| o.set(false));
        }
    }
    update_status(&u);
    u.table.menu()
}

fn drop_folder(ip: Option<&NSIndexPath>, on: bool) -> Option<String> {
    if !on {
        return None;
    }
    folder_at(row_of(ip?)?)
}

fn validate_drop(info: &ProtocolObject<dyn NSDraggingInfo>, ip: *mut *mut NSIndexPath, op: *mut NSCollectionViewDropOperation) -> NSDragOperation {
    let on = !op.is_null() && unsafe { *op } == NSCollectionViewDropOperation::On;
    let at = if ip.is_null() { None } else { unsafe { (*ip).as_ref() } };
    let folder = drop_folder(at, on);
    if on && folder.is_none() {
        // hovering a file means "into this folder", not "onto that file"
        unsafe { *op = NSCollectionViewDropOperation::Before };
    }
    match plan_drop(info, folder) {
        Some((_, _, true)) => NSDragOperation::Move,
        Some(_) => NSDragOperation::Copy,
        None => NSDragOperation::None,
    }
}

fn accept_drop(info: &ProtocolObject<dyn NSDraggingInfo>, ip: &NSIndexPath, op: NSCollectionViewDropOperation) -> bool {
    let Some(u) = ui() else { return false };
    match plan_drop(info, drop_folder(Some(ip), op == NSCollectionViewDropOperation::On)) {
        Some((srcs, dest, moving)) => {
            transfer(&u, srcs, dest, moving);
            true
        }
        None => false,
    }
}

fn drag_writer(ip: &NSIndexPath) -> Option<Retained<ProtocolObject<dyn NSPasteboardWriting>>> {
    let p = m(|s| row_of_in(s, ip).and_then(|r| s.rows.get(r)).map(|e| e.path.clone()))?;
    Some(ProtocolObject::from_retained(file_url(&p)))
}

fn row_of_in(s: &Model, ip: &NSIndexPath) -> Option<usize> {
    let (sec, item) = (ip.section() as usize, ip.item() as usize);
    s.sections.get(sec).filter(|x| item < x.len).map(|x| x.start + item)
}

fn header_size(cv: &NSCollectionView, sec: NSInteger) -> NSSize {
    let titled = m(|s| s.sections.get(sec as usize).is_some_and(|x| !x.title.is_empty()));
    if !titled {
        return NSSize::new(0.0, 0.0);
    }
    NSSize::new(cv.bounds().size.width, HEAD_H)
}

fn selection_changed() {
    if let Some(u) = ui() {
        update_status(&u);
    }
}

// ---------- classes ----------

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "LiftoffGridCell"]
    struct CellView;

    impl CellView {
        #[unsafe(method_id(hitTest:))]
        fn hit(&self, p: NSPoint) -> Option<Retained<NSView>> {
            cell_hit(self, p)
        }
    }
);

define_class!(
    #[unsafe(super(NSCollectionViewItem))]
    #[thread_kind = MainThreadOnly]
    #[name = "LiftoffGridItem"]
    struct GridItem;

    impl GridItem {
        #[unsafe(method(loadView))]
        fn load_view(&self) {
            build_cell(self);
        }

        #[unsafe(method(setSelected:))]
        fn set_selected(&self, on: bool) {
            let _: () = unsafe { msg_send![super(self), setSelected: on] };
            style(self, true);
        }

        #[unsafe(method(setHighlightState:))]
        fn set_highlight(&self, state: NSCollectionViewItemHighlightState) {
            let _: () = unsafe { msg_send![super(self), setHighlightState: state] };
            style(self, true);
        }
    }
);

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "LiftoffGridHeader"]
    struct Header;

    impl Header {
        // a heading stuck to the top has tiles sliding underneath it, so it needs a floor
        #[unsafe(method(drawRect:))]
        fn draw(&self, _dirty: NSRect) {
            NSColor::controlBackgroundColor().setFill();
            NSBezierPath::fillRect(self.bounds());
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, e: &NSEvent) {
            if !header_menu(self) {
                let _: () = unsafe { msg_send![super(self), mouseDown: e] };
            }
        }
    }

    unsafe impl NSObjectProtocol for Header {}

    unsafe impl NSUserInterfaceItemIdentification for Header {}

    unsafe impl NSCollectionViewElement for Header {}
);

define_class!(
    #[unsafe(super(NSCollectionView))]
    #[thread_kind = MainThreadOnly]
    #[name = "LiftoffGrid"]
    struct Grid;

    impl Grid {
        #[unsafe(method(keyDown:))]
        fn key_down(&self, e: &NSEvent) {
            if !key(e) {
                let _: () = unsafe { msg_send![super(self), keyDown: e] };
            }
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, e: &NSEvent) {
            peek_close();
            let _: () = unsafe { msg_send![super(self), mouseDown: e] };
            clicked(self, e);
        }

        #[unsafe(method_id(menuForEvent:))]
        fn menu_for_event(&self, e: &NSEvent) -> Option<Retained<NSMenu>> {
            menu_for(self, e)
        }

        #[unsafe(method(mouseMoved:))]
        fn mouse_moved(&self, e: &NSEvent) {
            hover(self, Some(e));
        }

        #[unsafe(method(mouseExited:))]
        fn mouse_exited(&self, _e: &NSEvent) {
            hover(self, None);
        }


        #[unsafe(method(becomeFirstResponder))]
        fn become_first(&self) -> bool {
            let ok: bool = unsafe { msg_send![super(self), becomeFirstResponder] };
            later(0, restyle_soon);
            ok
        }

        #[unsafe(method(resignFirstResponder))]
        fn resign_first(&self) -> bool {
            let ok: bool = unsafe { msg_send![super(self), resignFirstResponder] };
            later(0, restyle_soon);
            ok
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
            let arrows = e.r#type() == NSEventType::KeyDown && matches!(e.keyCode(), 123..=126);
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
    #[name = "LiftoffGridController"]
    pub(super) struct GridCtl;

    unsafe impl NSObjectProtocol for GridCtl {}

    unsafe impl NSCollectionViewDataSource for GridCtl {
        #[unsafe(method(numberOfSectionsInCollectionView:))]
        fn sections(&self, _cv: &NSCollectionView) -> NSInteger {
            m(|s| s.sections.len().max(1)) as NSInteger
        }

        #[unsafe(method(collectionView:numberOfItemsInSection:))]
        fn items(&self, _cv: &NSCollectionView, sec: NSInteger) -> NSInteger {
            m(|s| s.sections.get(sec as usize).map_or(0, |x| x.len)) as NSInteger
        }

        #[unsafe(method_id(collectionView:itemForRepresentedObjectAtIndexPath:))]
        fn item(&self, cv: &NSCollectionView, ip: &NSIndexPath) -> Retained<NSCollectionViewItem> {
            make_item(cv, ip)
        }

        #[unsafe(method_id(collectionView:viewForSupplementaryElementOfKind:atIndexPath:))]
        fn header(&self, cv: &NSCollectionView, kind: &NSString, ip: &NSIndexPath) -> Retained<NSView> {
            make_header(cv, kind, ip)
        }
    }

    unsafe impl NSCollectionViewDelegate for GridCtl {
        #[unsafe(method(collectionView:didSelectItemsAtIndexPaths:))]
        fn did_select(&self, _cv: &NSCollectionView, _ips: &NSSet<NSIndexPath>) {
            selection_changed();
        }

        #[unsafe(method(collectionView:didDeselectItemsAtIndexPaths:))]
        fn did_deselect(&self, _cv: &NSCollectionView, _ips: &NSSet<NSIndexPath>) {
            selection_changed();
        }

        #[unsafe(method_id(collectionView:pasteboardWriterForItemAtIndexPath:))]
        fn drag_item(&self, _cv: &NSCollectionView, ip: &NSIndexPath) -> Option<Retained<ProtocolObject<dyn NSPasteboardWriting>>> {
            drag_writer(ip)
        }

        #[unsafe(method(collectionView:validateDrop:proposedIndexPath:dropOperation:))]
        fn validate(
            &self,
            _cv: &NSCollectionView,
            info: &ProtocolObject<dyn NSDraggingInfo>,
            ip: *mut *mut NSIndexPath,
            op: *mut NSCollectionViewDropOperation,
        ) -> NSDragOperation {
            validate_drop(info, ip, op)
        }

        #[unsafe(method(collectionView:acceptDrop:indexPath:dropOperation:))]
        fn accept(&self, _cv: &NSCollectionView, info: &ProtocolObject<dyn NSDraggingInfo>, ip: &NSIndexPath, op: NSCollectionViewDropOperation) -> bool {
            accept_drop(info, ip, op)
        }
    }

    impl GridCtl {
        #[unsafe(method(lfJump:))]
        fn a_jump(&self, s: Option<&AnyObject>) {
            jump(s);
        }

        #[unsafe(method(lfPeekOpen:))]
        fn a_peek_open(&self, s: Option<&AnyObject>) {
            peek_open(s);
        }
    }

    unsafe impl NSCollectionViewDelegateFlowLayout for GridCtl {
        #[unsafe(method(collectionView:layout:referenceSizeForHeaderInSection:))]
        fn head_size(&self, cv: &NSCollectionView, _layout: &NSCollectionViewLayout, sec: NSInteger) -> NSSize {
            header_size(cv, sec)
        }
    }
);

// ---------- what browse.rs calls ----------

fn restyle_soon() {
    if let Some(u) = ui() {
        restyle(&u);
    }
}

pub(super) fn restyle(u: &Ui) {
    let items = u.grid.visibleItems();
    for i in 0..items.count() {
        style(&items.objectAtIndex(i), true);
    }
}

pub(super) fn selected_rows(u: &Ui) -> Vec<usize> {
    let all = u.grid.selectionIndexPaths().allObjects();
    let mut v: Vec<usize> = m(|s| (0..all.count()).filter_map(|i| row_of_in(s, &all.objectAtIndex(i))).collect());
    v.sort_unstable();
    v
}

pub(super) fn select_rows(u: &Ui, rows: &[usize], scroll: bool) {
    let ips: Vec<Retained<NSIndexPath>> = rows.iter().map(|&r| ip_of(r)).collect();
    u.grid.setSelectionIndexPaths(&NSSet::from_retained_slice(&ips));
    if let (true, Some(first)) = (scroll, ips.first()) {
        u.grid.scrollToItemsAtIndexPaths_scrollPosition(&NSSet::from_retained_slice(&[first.clone()]), NSCollectionViewScrollPosition::NearestHorizontalEdge);
    }
}

pub(super) fn reload(u: &Ui) {
    peek_close();
    fit(u);
    u.grid.reloadData();
}

fn refill_headers(u: &Ui) {
    let kind = unsafe { NSCollectionElementKindSectionHeader };
    let ips = u.grid.indexPathsForVisibleSupplementaryElementsOfKind(kind).allObjects();
    for i in 0..ips.count() {
        let ip = ips.objectAtIndex(i);
        if let Some(v) = u.grid.supplementaryViewForElementKind_atIndexPath(kind, &ip) {
            fill_header(&u.grid, &v, ip.section() as usize);
        }
    }
}

// every tile on screen shows what its row holds now
fn refresh_visible(u: &Ui) {
    if m(|s| s.renaming.is_some()) {
        return;
    }
    let items = u.grid.visibleItems();
    for i in 0..items.count() {
        let it = items.objectAtIndex(i);
        if let Some(row) = u.grid.indexPathForItem(&it).and_then(|ip| row_of(&ip)) {
            configure(u, &it, row);
        }
    }
    refill_headers(u);
}

type Rows = (Vec<Ent>, Vec<Section>);

// swaps in new rows as one batch of moves, inserts and deletes instead of a reload, so
// the grid animates from the old arrangement to the new one. sections are matched by
// name. anything this can't express cleanly (a tile changing section into one that's
// being created, say) hands the rows back and the caller reloads.
pub(super) fn apply(u: &Ui, rows: Vec<Ent>, secs: Vec<Section>, renamed: Option<&(String, String)>) -> Result<(), Rows> {
    let old: Vec<(String, Vec<String>)> = m(|s| {
        let key = |p: &str| match renamed {
            Some((from, to)) if p == from => to.clone(),
            _ => p.to_owned(),
        };
        s.sections.iter().map(|x| (x.title.clone(), s.rows[x.start..x.start + x.len].iter().map(|e| key(&e.path)).collect())).collect()
    });
    let new: Vec<(String, Vec<String>)> = secs.iter().map(|x| (x.title.clone(), rows[x.start..x.start + x.len].iter().map(|e| e.path.clone()).collect())).collect();
    // the grid has to be showing exactly the rows we think it is, or the batch won't add up
    let shown_ok = u.grid.numberOfSections() as usize == old.len().max(1)
        && old.iter().enumerate().all(|(i, x)| u.grid.numberOfItemsInSection(i as NSInteger) as usize == x.1.len());
    let total = |v: &[(String, Vec<String>)]| v.iter().map(|x| x.1.len()).sum::<usize>();
    if !shown_ok || old.is_empty() || new.is_empty() || total(&old) > 3000 || total(&new) > 3000 {
        return Err((rows, secs));
    }
    let titles_old: Vec<&str> = old.iter().map(|x| x.0.as_str()).collect();
    let titles_new: Vec<&str> = new.iter().map(|x| x.0.as_str()).collect();
    let uniq = |t: &[&str]| t.iter().collect::<HashSet<_>>().len() == t.len();
    let kept_old: Vec<&str> = titles_old.iter().copied().filter(|t| titles_new.contains(t)).collect();
    let kept_new: Vec<&str> = titles_new.iter().copied().filter(|t| titles_old.contains(t)).collect();
    if !uniq(&titles_old) || !uniq(&titles_new) || kept_old != kept_new {
        return Err((rows, secs));
    }
    let dead_sec: Vec<usize> = (0..old.len()).filter(|&i| !titles_new.contains(&titles_old[i])).collect();
    let born_sec: Vec<usize> = (0..new.len()).filter(|&i| !titles_old.contains(&titles_new[i])).collect();
    let mut at_old: HashMap<&str, (usize, usize)> = HashMap::new();
    for (si, x) in old.iter().enumerate() {
        for (ii, p) in x.1.iter().enumerate() {
            if at_old.insert(p.as_str(), (si, ii)).is_some() {
                return Err((rows, secs));
            }
        }
    }
    let mut at_new: HashMap<&str, (usize, usize)> = HashMap::new();
    for (si, x) in new.iter().enumerate() {
        for (ii, p) in x.1.iter().enumerate() {
            if at_new.insert(p.as_str(), (si, ii)).is_some() {
                return Err((rows, secs));
            }
        }
    }
    let ip = |(s, i): (usize, usize)| NSIndexPath::indexPathForItem_inSection(i as NSInteger, s as NSInteger);
    let (mut dels, mut adds, mut moves) = (Vec::new(), Vec::new(), Vec::new());
    for (p, &o) in &at_old {
        match at_new.get(p) {
            None if !dead_sec.contains(&o.0) => dels.push(ip(o)),
            None => {}
            Some(&n) => {
                if dead_sec.contains(&o.0) || born_sec.contains(&n.0) {
                    return Err((rows, secs));
                }
                if o != n {
                    moves.push((ip(o), ip(n)));
                }
            }
        }
    }
    for (p, &n) in &at_new {
        if !at_old.contains_key(p) && !born_sec.contains(&n.0) {
            adds.push(ip(n));
        }
    }
    peek_close();
    let nothing = dels.is_empty() && adds.is_empty() && moves.is_empty() && dead_sec.is_empty() && born_sec.is_empty();
    let set = |v: &[usize]| {
        let s = NSMutableIndexSet::new();
        for &i in v {
            s.addIndex(i);
        }
        s
    };
    let (dead, born) = (set(&dead_sec), set(&born_sec));
    let dels = NSSet::from_retained_slice(&dels);
    let adds = NSSet::from_retained_slice(&adds);
    let pending = RefCell::new(Some((rows, secs)));
    let install = move || {
        if let Some((r, s)) = pending.borrow_mut().take() {
            m(|x| {
                x.rows = r;
                x.sections = s;
            });
        }
    };
    if nothing {
        // same tiles in the same places: only what they say may have changed
        install();
        refresh_visible(u);
        return Ok(());
    }
    let grid = u.grid.clone();
    let updates = block2::RcBlock::new(move || {
        install();
        if dead.count() > 0 {
            grid.deleteSections(&dead);
        }
        if born.count() > 0 {
            grid.insertSections(&born);
        }
        if dels.count() > 0 {
            grid.deleteItemsAtIndexPaths(&dels);
        }
        if adds.count() > 0 {
            grid.insertItemsAtIndexPaths(&adds);
        }
        for (a, b) in &moves {
            grid.moveItemAtIndexPath_toIndexPath(a, b);
        }
    });
    let done = block2::RcBlock::new(|_ok: Bool| {
        if let Some(u) = ui() {
            refresh_visible(&u);
        }
    });
    NSAnimationContext::beginGrouping();
    let ctx = NSAnimationContext::currentContext();
    ctx.setDuration(0.34);
    ctx.setTimingFunction(Some(&settle()));
    u.grid.animator().performBatchUpdates_completionHandler(Some(&updates), Some(&done));
    NSAnimationContext::endGrouping();
    refresh_visible(u);
    Ok(())
}

// for the off-screen tests: what the grid holds against what the model says
pub(super) fn check(u: &Ui) -> Vec<String> {
    let mut bad = Vec::new();
    let secs: Vec<(String, usize)> = m(|s| s.sections.iter().map(|x| (x.title.clone(), x.len)).collect());
    if u.grid.numberOfSections() as usize != secs.len().max(1) {
        bad.push(format!("grid has {} sections, model {}", u.grid.numberOfSections(), secs.len()));
    }
    for (i, (t, n)) in secs.iter().enumerate() {
        let got = u.grid.numberOfItemsInSection(i as NSInteger) as usize;
        if got != *n {
            bad.push(format!("section {t:?} has {got}, model {n}"));
        }
    }
    let items = u.grid.visibleItems();
    for i in 0..items.count() {
        let it = items.objectAtIndex(i);
        let Some(ip) = u.grid.indexPathForItem(&it) else { continue };
        let want = row_of(&ip).and_then(|r| m(|s| s.rows.get(r).map(|e| e.path.clone())));
        let shown = path_of(&it);
        let name = it.textField().map(|t| t.stringValue().to_string());
        let want_name = want.as_deref().and_then(|p| p.rsplit('/').next()).map(str::to_owned);
        if shown != want || name != want_name {
            bad.push(format!("tile {}/{} shows {:?}, model {:?}", ip.section(), ip.item(), name, want_name));
        }
    }
    let kind = unsafe { NSCollectionElementKindSectionHeader };
    let ips = u.grid.indexPathsForVisibleSupplementaryElementsOfKind(kind).allObjects();
    for i in 0..ips.count() {
        let ip = ips.objectAtIndex(i);
        let title = u
            .grid
            .supplementaryViewForElementKind_atIndexPath(kind, &ip)
            .and_then(|v| v.viewWithTag(HEAD_TITLE))
            .and_then(|t| t.downcast::<NSTextField>().ok())
            .map(|t| t.stringValue().to_string());
        let want = secs.get(ip.section() as usize).map(|x| x.0.clone());
        if title.is_some() && title != want {
            bad.push(format!("heading {} says {title:?}, model {want:?}", ip.section()));
        }
    }
    bad
}

// the middle of a tile's picture, in the scroll view's coordinates
pub(super) fn row_point(u: &Ui, row: usize) -> Option<NSPoint> {
    let item = u.grid.itemAtIndexPath(&ip_of(row))?;
    let f = item.imageView()?.frame();
    let mid = NSPoint::new(f.origin.x + f.size.width / 2.0, f.origin.y + f.size.height / 2.0);
    Some(u.grid_scroll.convertPoint_fromView(mid, Some(&item.view())))
}

pub(super) fn selected_point(u: &Ui) -> Option<NSPoint> {
    let first = *selected_rows(u).first()?;
    u.grid.layoutSubtreeIfNeeded();
    row_point(u, first)
}

pub(super) fn scroll_top(u: &Ui) {
    if m(|s| !s.rows.is_empty()) {
        u.grid.scrollToItemsAtIndexPaths_scrollPosition(&NSSet::from_retained_slice(&[ip_of(0)]), NSCollectionViewScrollPosition::Top);
    }
}

// columns that fill the width exactly, so the grid stays a grid on every row
pub(super) fn fit(u: &Ui) {
    let Some(layout) = u.grid.collectionViewLayout().and_then(|l| l.downcast::<NSCollectionViewFlowLayout>().ok()) else { return };
    let avail = u.grid_scroll.contentView().bounds().size.width - EDGE * 2.0;
    if avail < 40.0 {
        return;
    }
    let s = icon();
    let min_w = (s + 40.0).max(112.0);
    let cols = ((avail + GAP) / (min_w + GAP)).floor().max(1.0);
    let cell = (((avail - (cols - 1.0) * GAP) / cols).floor(), s + 72.0);
    if CELL.with(|c| c.replace(cell)) == cell {
        return;
    }
    layout.setItemSize(NSSize::new(cell.0, cell.1));
    refill_headers(u);
    let items = u.grid.visibleItems();
    for i in 0..items.count() {
        let it = items.objectAtIndex(i);
        if let Some(row) = u.grid.indexPathForItem(&it).and_then(|ip| row_of(&ip)) {
            configure(u, &it, row);
        }
    }
}

// the name label of one tile, stretched out and ready to be typed in
pub(super) fn name_field(u: &Ui, row: usize) -> Option<Retained<NSTextField>> {
    u.grid.layoutSubtreeIfNeeded();
    let item = u.grid.itemAtIndexPath(&ip_of(row))?;
    let tf = item.textField()?;
    let (cw, ch) = CELL.with(|c| c.get());
    tf.setFrame(rect(4.0, ch - icon() - 18.0 - 32.0, cw - 8.0, 32.0));
    if let Some(d) = item.view().viewWithTag(DOTS) {
        d.setHidden(true);
    }
    Some(tf)
}

pub(super) fn set_image(u: &Ui, path: &str, img: &NSImage) {
    if let Some(iv) = shown(u, path).and_then(|it| it.imageView()) {
        let (cw, ch) = CELL.with(|c| c.get());
        place_icon(&iv, img, true, true, cw, ch, icon());
    }
}

pub(super) fn set_zoom(u: &Ui, z: usize, remember: bool) {
    let z = z.min(SIZES.len() - 1);
    let old = ZOOM.with(|c| c.replace(z));
    if old == z {
        return;
    }
    if remember {
        objc2_foundation::NSUserDefaults::standardUserDefaults().setInteger_forKey(z as NSInteger + 1, &ns("LiftoffFilesGridZoom"));
    }
    let keep = selected_paths(u);
    reload(u);
    select_paths(u, &keep, true);
    zoom_motion(u, SIZES[old] / SIZES[z]);
}

// bigger or smaller icons: the old grid fades out as it was while the new one grows
// (or shrinks) into place about the selected tile, so that tile just changes size where
// it stands. with nothing selected it's pinned at the top left corner
fn zoom_motion(u: &Ui, ratio: f64) {
    if !moving() {
        return;
    }
    crossfade(&u.stage, 0.22);
    let Some(l) = u.grid_scroll.layer().filter(|_| !reduce_motion()) else { return };
    let b = u.grid_scroll.bounds().size;
    let corner = NSPoint::new(EDGE, b.height - u.grid_scroll.contentInsets().top);
    let at = selected_point(u).unwrap_or(corner);
    scale_about(&l, at, ratio.clamp(0.6, 1.6), 1.0, "pinch", false, &|k, f, t| spring(k, f, t, 0.32));
}

pub(super) fn build(mtm: MainThreadMarker, frame: NSRect) -> (Retained<NSCollectionView>, Retained<NSScrollView>, Retained<GridCtl>) {
    let ctl: Retained<GridCtl> = unsafe { msg_send![GridCtl::alloc(mtm), init] };
    let layout = NSCollectionViewFlowLayout::new(mtm);
    layout.setMinimumInteritemSpacing(GAP);
    layout.setMinimumLineSpacing(GAP);
    layout.setSectionInset(NSEdgeInsets { top: 6.0, left: EDGE, bottom: 14.0, right: EDGE });
    layout.setSectionHeadersPinToVisibleBounds(true);
    let (cw, ch) = CELL.with(|c| c.get());
    layout.setItemSize(NSSize::new(cw, ch));
    let grid: Retained<Grid> = unsafe { msg_send![Grid::alloc(mtm), initWithFrame: rect(0.0, 0.0, frame.size.width, frame.size.height)] };
    let grid: Retained<NSCollectionView> = Retained::into_super(grid);
    grid.setCollectionViewLayout(Some(&layout));
    grid.setSelectable(true);
    grid.setAllowsMultipleSelection(true);
    grid.setAllowsEmptySelection(true);
    // same backdrop as the list, painted by the grid itself
    grid.setBackgroundColors(Some(&NSArray::from_retained_slice(&[NSColor::controlBackgroundColor()])));
    unsafe {
        grid.registerClass_forItemWithIdentifier(Some(GridItem::class()), &ns("item"));
        grid.registerClass_forSupplementaryViewOfKind_withIdentifier(Some(Header::class()), NSCollectionElementKindSectionHeader, &ns("head"));
        grid.setDataSource(Some(ProtocolObject::from_ref(&*ctl)));
        grid.setDelegate(Some(ProtocolObject::from_ref(&*ctl)));
    }
    let track = unsafe {
        objc2_app_kit::NSTrackingArea::initWithRect_options_owner_userInfo(
            objc2_app_kit::NSTrackingArea::alloc(),
            rect(0.0, 0.0, 0.0, 0.0),
            objc2_app_kit::NSTrackingAreaOptions::MouseMoved
                | objc2_app_kit::NSTrackingAreaOptions::MouseEnteredAndExited
                | objc2_app_kit::NSTrackingAreaOptions::ActiveInKeyWindow
                | objc2_app_kit::NSTrackingAreaOptions::InVisibleRect,
            Some(crate::view::as_obj(&*grid)),
            None,
        )
    };
    grid.addTrackingArea(&track);
    grid.registerForDraggedTypes(&NSArray::from_slice(&[unsafe { NSPasteboardTypeFileURL }]));
    grid.setDraggingSourceOperationMask_forLocal(NSDragOperation::Copy | NSDragOperation::Move, true);
    grid.setDraggingSourceOperationMask_forLocal(NSDragOperation::Copy | NSDragOperation::Move | NSDragOperation::Link | NSDragOperation::Generic, false);

    let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), frame);
    scroll.setAutoresizingMask(NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable);
    scroll.setHasVerticalScroller(true);
    scroll.setAutohidesScrollers(true);
    scroll.setBorderType(NSBorderType::NoBorder);
    scroll.setDocumentView(Some(&grid));
    let clip = scroll.contentView();
    clip.setPostsFrameChangedNotifications(true);
    let refit = block2::RcBlock::new(|_n: NonNull<NSNotification>| {
        if let Some(u) = ui() {
            if grid_mode() {
                fit(&u);
            }
        }
    });
    let token = unsafe {
        objc2_foundation::NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
            Some(objc2_app_kit::NSViewFrameDidChangeNotification),
            Some(&clip),
            None,
            &refit,
        )
    };
    std::mem::forget(token);
    // when the view scrolls the tiles move and the pointer doesn't, so the hover is
    // dropped until the pointer moves again. done by watching the scroll position:
    // overriding scrollWheel would switch off the system's smooth scrolling.
    clip.setPostsBoundsChangedNotifications(true);
    let unhover = block2::RcBlock::new(|_n: NonNull<NSNotification>| {
        if let Some(u) = ui() {
            hover(&u.grid, None);
        }
    });
    let token = unsafe {
        objc2_foundation::NSNotificationCenter::defaultCenter().addObserverForName_object_queue_usingBlock(
            Some(objc2_app_kit::NSViewBoundsDidChangeNotification),
            Some(&clip),
            None,
            &unhover,
        )
    };
    std::mem::forget(token);
    (grid, scroll, ctl)
}
