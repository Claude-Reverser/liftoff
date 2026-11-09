mod calc;
mod files;
mod icons;
mod index;
mod layout;
mod search;
mod view;

use std::ffi::c_void;

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, MainThreadMarker, MainThreadOnly};
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate};
use objc2_foundation::{NSNotification, NSObject, NSObjectProtocol};

#[repr(C)]
struct EventTypeSpec {
    class: u32,
    kind: u32,
}
#[repr(C)]
#[derive(Clone, Copy)]
struct EventHotKeyID {
    signature: u32,
    id: u32,
}

type Handler = extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> i32;

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn GetApplicationEventTarget() -> *mut c_void;
    fn InstallEventHandler(
        target: *mut c_void,
        handler: Handler,
        num_types: u32,
        list: *const EventTypeSpec,
        user_data: *mut c_void,
        out_ref: *mut *mut c_void,
    ) -> i32;
    fn GetEventParameter(
        event: *mut c_void,
        name: u32,
        desired: u32,
        actual: *mut u32,
        size: usize,
        actual_size: *mut usize,
        data: *mut c_void,
    ) -> i32;
    fn RegisterEventHotKey(
        key_code: u32,
        modifiers: u32,
        id: EventHotKeyID,
        target: *mut c_void,
        options: u32,
        out_ref: *mut *mut c_void,
    ) -> i32;
}

const K_EVENT_CLASS_KEYBOARD: u32 = u32::from_be_bytes(*b"keyb");
const K_EVENT_HOTKEY_PRESSED: u32 = 5;
const CMD: u32 = 1 << 8;
const SHIFT: u32 = 1 << 9;
const OPT: u32 = 1 << 11;
const CTRL: u32 = 1 << 12;

const SEARCH_ID: u32 = 1000;

extern "C" fn on_hotkey(_: *mut c_void, event: *mut c_void, _: *mut c_void) -> i32 {
    let mut id = EventHotKeyID { signature: 0, id: 0 };
    unsafe {
        GetEventParameter(
            event,
            u32::from_be_bytes(*b"----"),
            u32::from_be_bytes(*b"hkid"),
            std::ptr::null_mut(),
            std::mem::size_of::<EventHotKeyID>(),
            std::ptr::null_mut(),
            &mut id as *mut _ as *mut c_void,
        );
    }
    if id.id >= SEARCH_ID { search::toggle() } else { view::toggle() }
    0
}

fn parse_hotkey(s: &str) -> Option<(u32, u32)> {
    let mut mods = 0;
    let mut key = None;
    for part in s.to_lowercase().split('+').map(str::trim) {
        match part {
            "cmd" | "command" => mods |= CMD,
            "opt" | "option" | "alt" => mods |= OPT,
            "ctrl" | "control" => mods |= CTRL,
            "shift" => mods |= SHIFT,
            k => key = keycode(k),
        }
    }
    key.map(|k| (k, mods))
}

fn keycode(k: &str) -> Option<u32> {
    const LETTERS: &[(char, u32)] = &[
        ('a', 0), ('s', 1), ('d', 2), ('f', 3), ('h', 4), ('g', 5), ('z', 6), ('x', 7), ('c', 8),
        ('v', 9), ('b', 11), ('q', 12), ('w', 13), ('e', 14), ('r', 15), ('y', 16), ('t', 17),
        ('o', 31), ('u', 32), ('i', 34), ('p', 35), ('l', 37), ('j', 38), ('k', 40), ('n', 45),
        ('m', 46),
    ];
    Some(match k {
        "space" => 49,
        "f1" => 122, "f2" => 120, "f3" => 99, "f4" => 118, "f5" => 96, "f6" => 97,
        "f7" => 98, "f8" => 100, "f9" => 101, "f10" => 109, "f11" => 103, "f12" => 111,
        "launchpad" => 131,
        _ if k.chars().count() == 1 => {
            let c = k.chars().next()?;
            LETTERS.iter().find(|(l, _)| *l == c)?.1
        }
        _ => return None,
    })
}

fn config_hotkeys(file: &str, defaults: &[&str]) -> Vec<String> {
    let h = std::env::var_os("HOME").unwrap_or_default();
    let p = std::path::Path::new(&h).join("Library/Application Support/Liftoff").join(file);
    match std::fs::read_to_string(&p) {
        Ok(s) => s.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).map(String::from).collect(),
        Err(_) => {
            let mut def = String::from("# one hotkey per line, e.g. opt+space, cmd+shift+l, f4\n");
            for d in defaults {
                def.push_str(d);
                def.push('\n');
            }
            let _ = std::fs::create_dir_all(p.parent().unwrap());
            let _ = std::fs::write(&p, def);
            defaults.iter().map(|d| d.to_string()).collect()
        }
    }
}

fn register_hotkeys() {
    unsafe {
        let target = GetApplicationEventTarget();
        let spec = EventTypeSpec { class: K_EVENT_CLASS_KEYBOARD, kind: K_EVENT_HOTKEY_PRESSED };
        let mut h = std::ptr::null_mut();
        InstallEventHandler(target, on_hotkey, 1, &spec, std::ptr::null_mut(), &mut h);
        let launcher = config_hotkeys("hotkeys", &["opt+space", "launchpad"]);
        let finder = config_hotkeys("search-hotkeys", &["cmd+space"]);
        let all = launcher.iter().enumerate().map(|(i, h)| (i as u32, h)).chain(
            finder.iter().enumerate().map(|(i, h)| (SEARCH_ID + i as u32, h)),
        );
        for (i, hk) in all {
            let Some((code, mods)) = parse_hotkey(hk) else {
                eprintln!("liftoff: can't parse hotkey {hk:?}");
                continue;
            };
            let id = EventHotKeyID { signature: u32::from_be_bytes(*b"LFTO"), id: i };
            let mut r = std::ptr::null_mut();
            let err = RegisterEventHotKey(code, mods, id, target, 0, &mut r);
            if err != 0 {
                eprintln!("liftoff: hotkey {hk:?} unavailable (err {err})");
            }
        }
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "LiftoffDelegate"]
    struct Delegate;

    unsafe impl NSObjectProtocol for Delegate {}

    unsafe impl NSApplicationDelegate for Delegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _n: &NSNotification) {
            let mtm = MainThreadMarker::new().unwrap();
            view::setup(mtm);
            view::watch_launches();
            search::setup(mtm);
            files::start();
            register_hotkeys();
            if std::env::args().any(|a| a == "--show") {
                view::show();
            }
            if std::env::args().any(|a| a == "--search") {
                search::show();
            }
        }

        #[unsafe(method(applicationShouldHandleReopen:hasVisibleWindows:))]
        fn should_handle_reopen(&self, _app: &NSApplication, _visible: bool) -> bool {
            view::toggle();
            false
        }

        #[unsafe(method(applicationDidResignActive:))]
        fn did_resign_active(&self, _n: &NSNotification) {
            view::hide();
            search::hide();
        }
    }
);

fn main() {
    let mtm = MainThreadMarker::new().expect("must run on main thread");
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    let delegate: Retained<Delegate> = unsafe { msg_send![Delegate::alloc(mtm), init] };
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    app.run();
}
