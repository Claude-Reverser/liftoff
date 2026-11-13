use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use objc2::rc::autoreleasepool;
use objc2_app_kit::NSWorkspace;
use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreateImage, CGBitmapContextGetBytesPerRow, CGBitmapContextGetData, CGColorSpace, CGContext,
    CGImage, CGInterpolationQuality,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

unsafe extern "C" {
    fn CGBitmapContextCreate(
        data: *mut c_void,
        width: usize,
        height: usize,
        bits_per_component: usize,
        bytes_per_row: usize,
        space: *const CGColorSpace,
        bitmap_info: u32,
    ) -> *mut CGContext;
}

const PREMUL_FIRST: u32 = 2;
const BYTE_ORDER_32_LITTLE: u32 = 2 << 12;

pub fn render(path: &str, px: usize) -> Option<CFRetained<CGImage>> {
    autoreleasepool(|_| {
        let ws = NSWorkspace::sharedWorkspace();
        // safari in /Applications is a link into the system cryptex now. ask for the icon
        // of the link and you get the little alias arrow stamped on it
        let real = std::fs::canonicalize(path).ok().and_then(|p| p.to_str().map(str::to_owned)).unwrap_or_else(|| path.to_owned());
        let img = ws.iconForFile(&NSString::from_str(&real));
        let mut rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(px as f64, px as f64));
        let src = unsafe { img.CGImageForProposedRect_context_hints(&mut rect, None, None) }?;
        let cs = CGColorSpace::new_device_rgb()?;
        let ctx = unsafe {
            CGBitmapContextCreate(
                std::ptr::null_mut(),
                px,
                px,
                8,
                0,
                &*cs,
                PREMUL_FIRST | BYTE_ORDER_32_LITTLE,
            )
        };
        let ctx = unsafe { CFRetained::from_raw(NonNull::new(ctx)?) };
        CGContext::set_interpolation_quality(Some(&ctx), CGInterpolationQuality::High);
        let f = px as f64;
        let shadow = objc2_core_graphics::CGColor::new_generic_gray(0.0, 0.35);
        CGContext::set_shadow_with_color(Some(&ctx), CGSize::new(0.0, -f * 0.018), f * 0.035, Some(&shadow));
        let r = CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(f, f));
        CGContext::draw_image(Some(&ctx), r, Some(&src));
        CGBitmapContextCreateImage(Some(&ctx))
    })
}

fn hsl(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let (mx, mn) = (r.max(g).max(b), r.min(g).min(b));
    let l = (mx + mn) / 2.0;
    if mx - mn < 1e-5 {
        return (0.0, 0.0, l);
    }
    let d = mx - mn;
    let s = if l > 0.5 { d / (2.0 - mx - mn) } else { d / (mx + mn) };
    let h = if mx == r {
        (g - b) / d + if g < b { 6.0 } else { 0.0 }
    } else if mx == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h / 6.0, s, l)
}

fn rgb(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let f = |mut t: f32| {
        t = t.rem_euclid(1.0);
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    (f(h + 1.0 / 3.0), f(h), f(h - 1.0 / 3.0))
}

// repaints an icon in one colour and keeps its shading: every pixel keeps how light
// it was and takes the hue and saturation of `tint`. the folder icon follows the
// system accent, so on graphite every folder on the machine is the colour of a
// tuesday in february. this is how they get their colour back.
pub fn tinted(path: &str, px: usize, tint: (f64, f64, f64)) -> Option<CFRetained<CGImage>> {
    autoreleasepool(|_| {
        let img = NSWorkspace::sharedWorkspace().iconForFile(&NSString::from_str(path));
        let mut rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(px as f64, px as f64));
        let src = unsafe { img.CGImageForProposedRect_context_hints(&mut rect, None, None) }?;
        let cs = CGColorSpace::new_device_rgb()?;
        let ctx = unsafe { CGBitmapContextCreate(std::ptr::null_mut(), px, px, 8, 0, &*cs, PREMUL_FIRST | BYTE_ORDER_32_LITTLE) };
        let ctx = unsafe { CFRetained::from_raw(NonNull::new(ctx)?) };
        CGContext::set_interpolation_quality(Some(&ctx), CGInterpolationQuality::High);
        let f = px as f64;
        CGContext::draw_image(Some(&ctx), CGRect::new(CGPoint::new(0.0, 0.0), CGSize::new(f, f)), Some(&src));
        let data = CGBitmapContextGetData(Some(&ctx)) as *mut u8;
        let stride = CGBitmapContextGetBytesPerRow(Some(&ctx));
        if data.is_null() || stride < px * 4 {
            return None;
        }
        let buf = unsafe { std::slice::from_raw_parts_mut(data, stride * px) };
        let (th, ts, tl) = hsl(tint.0 as f32, tint.1 as f32, tint.2 as f32);
        // where the icon sits on the light scale, so its middle lands on the tint itself
        let (mut sum, mut n) = (0.0f32, 0.0f32);
        for y in 0..px {
            for x in 0..px {
                let i = y * stride + x * 4;
                let a = buf[i + 3] as f32 / 255.0;
                if a > 0.5 {
                    let (b, g, r) = (buf[i] as f32 / 255.0 / a, buf[i + 1] as f32 / 255.0 / a, buf[i + 2] as f32 / 255.0 / a);
                    sum += (r.max(g).max(b) + r.min(g).min(b)) / 2.0;
                    n += 1.0;
                }
            }
        }
        let shift = if n > 0.0 { tl - sum / n } else { 0.0 };
        for y in 0..px {
            for x in 0..px {
                let i = y * stride + x * 4;
                let a = buf[i + 3] as f32 / 255.0;
                if a <= 0.0 {
                    continue;
                }
                let (b, g, r) = (buf[i] as f32 / 255.0 / a, buf[i + 1] as f32 / 255.0 / a, buf[i + 2] as f32 / 255.0 / a);
                let l = ((r.max(g).max(b) + r.min(g).min(b)) / 2.0).min(1.0);
                let (r, g, b) = rgb(th, ts, (l + shift).clamp(0.04, 0.96));
                buf[i] = (b * a * 255.0).round().clamp(0.0, 255.0) as u8;
                buf[i + 1] = (g * a * 255.0).round().clamp(0.0, 255.0) as u8;
                buf[i + 2] = (r * a * 255.0).round().clamp(0.0, 255.0) as u8;
            }
        }
        CGBitmapContextCreateImage(Some(&ctx))
    })
}

pub fn render_all<F>(jobs: Vec<(String, u64)>, px: usize, done: F)
where
    F: Fn(String, u64, CFRetained<CGImage>) + Send + Sync + 'static,
{
    if jobs.is_empty() {
        return;
    }
    let jobs = Arc::new(jobs);
    let next = Arc::new(AtomicUsize::new(0));
    let done = Arc::new(done);
    let n = std::thread::available_parallelism().map_or(4, |n| n.get()).min(8);
    for _ in 0..n {
        let (jobs, next, done) = (jobs.clone(), next.clone(), done.clone());
        std::thread::spawn(move || {
            loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some((path, mtime)) = jobs.get(i) else { break };
                if let Some(img) = render(path, px) {
                    done(path.clone(), *mtime, img);
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{hsl, rgb};
    #[test]
    fn hsl_round_trip() {
        for c in [(0.26f32, 0.58f32, 1.0f32), (1.0, 0.36, 0.36), (0.28, 0.80, 0.45), (0.5, 0.5, 0.5), (0.98, 0.76, 0.18)] {
            let (h, s, l) = hsl(c.0, c.1, c.2);
            let (r, g, b) = rgb(h, s, l);
            assert!((r - c.0).abs() < 0.01 && (g - c.1).abs() < 0.01 && (b - c.2).abs() < 0.01, "{c:?} -> {r} {g} {b}");
        }
    }
}
