use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use objc2::rc::autoreleasepool;
use objc2_app_kit::NSWorkspace;
use objc2_core_foundation::{CFRetained, CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreateImage, CGColorSpace, CGContext, CGImage, CGInterpolationQuality,
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
        let img = ws.iconForFile(&NSString::from_str(path));
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
