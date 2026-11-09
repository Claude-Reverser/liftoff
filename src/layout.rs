use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use objc2::rc::autoreleasepool;
use objc2_foundation::{NSBundle, NSString};

use crate::index::App;

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub fn new_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

#[derive(Clone, Debug)]
pub struct Folder {
    pub id: u64,
    pub name: String,
    pub apps: Vec<String>,
}

#[derive(Clone, Debug)]
pub enum Entry {
    App(String),
    Folder(Folder),
}

fn file() -> PathBuf {
    let h = std::env::var_os("HOME").unwrap_or_default();
    PathBuf::from(h).join("Library/Application Support/Liftoff/layout.tsv")
}

pub fn load() -> Option<Vec<Entry>> {
    let s = fs::read_to_string(file()).ok()?;
    let mut v = Vec::new();
    for line in s.lines() {
        let mut parts = line.split('\t');
        match parts.next() {
            Some("A") => v.push(Entry::App(parts.next()?.to_owned())),
            Some("F") => {
                let name = parts.next()?.to_owned();
                let apps = parts.map(String::from).collect();
                v.push(Entry::Folder(Folder { id: new_id(), name, apps }));
            }
            _ => {}
        }
    }
    Some(v)
}

pub fn save(items: &[Entry]) {
    let mut s = String::new();
    for e in items {
        match e {
            Entry::App(p) => {
                s.push_str("A\t");
                s.push_str(p);
            }
            Entry::Folder(f) => {
                s.push_str("F\t");
                s.push_str(&f.name.replace('\t', " "));
                for p in &f.apps {
                    s.push('\t');
                    s.push_str(p);
                }
            }
        }
        s.push('\n');
    }
    let f = file();
    let _ = fs::create_dir_all(f.parent().unwrap());
    let tmp = f.with_extension("tmp");
    if fs::write(&tmp, s).is_ok() {
        let _ = fs::rename(tmp, f);
    }
}

pub fn normalize(items: &mut Vec<Entry>) {
    for e in items.iter_mut() {
        if let Entry::Folder(f) = e {
            if f.apps.len() == 1 {
                *e = Entry::App(f.apps[0].clone());
            }
        }
    }
    items.retain(|e| !matches!(e, Entry::Folder(f) if f.apps.is_empty()));
}

pub fn reconcile(items: &mut Vec<Entry>, apps: &[App]) -> bool {
    let present: HashSet<&str> = apps.iter().map(|a| a.path.as_str()).collect();
    let mut seen: HashSet<String> = HashSet::new();
    let before = items.len();
    let mut changed = false;
    items.retain_mut(|e| match e {
        Entry::App(p) => {
            let keep = present.contains(p.as_str()) && seen.insert(p.clone());
            changed |= !keep;
            keep
        }
        Entry::Folder(f) => {
            let n = f.apps.len();
            f.apps.retain(|p| present.contains(p.as_str()) && seen.insert(p.clone()));
            changed |= n != f.apps.len();
            true
        }
    });
    normalize(items);
    for a in apps {
        if !seen.contains(&a.path) {
            items.push(Entry::App(a.path.clone()));
            changed = true;
        }
    }
    changed || before != items.len()
}

fn info(path: &str) -> (String, String) {
    autoreleasepool(|_| {
        let Some(b) = NSBundle::bundleWithPath(&NSString::from_str(path)) else {
            return (String::new(), String::new());
        };
        let id = b.bundleIdentifier().map(|s| s.to_string()).unwrap_or_default();
        let cat = b
            .objectForInfoDictionaryKey(&NSString::from_str("LSApplicationCategoryType"))
            .and_then(|o| o.downcast::<NSString>().ok())
            .map(|s| s.to_string())
            .unwrap_or_default();
        (id, cat)
    })
}

fn category_name(cat: &str) -> Option<&'static str> {
    let c = cat.strip_prefix("public.app-category.")?;
    Some(match c {
        "utilities" => "Utilities",
        "developer-tools" => "Developer",
        "productivity" | "business" | "reference" => "Productivity",
        "music" | "video" | "photography" | "graphics-design" => "Creativity",
        "entertainment" | "news" | "lifestyle" | "sports" => "Entertainment",
        "social-networking" => "Social",
        "education" => "Education",
        "finance" => "Finance",
        "health-fitness" | "medical" => "Health",
        "travel" | "weather" => "Travel",
        c if c.ends_with("games") => "Games",
        _ => return None,
    })
}

pub fn auto_name(a: &str, b: &str) -> String {
    let (ca, cb) = (info(a).1, info(b).1);
    category_name(&ca)
        .or_else(|| category_name(&cb))
        .unwrap_or("Folder")
        .to_owned()
}

const MAIN: &[&str] = &[
    "App Store", "Safari", "Mail", "Messages", "FaceTime", "Phone", "Maps", "Photos", "Calendar",
    "Contacts", "Notes", "Reminders", "Music", "Freeform", "Passwords", "FindMy", "Find My",
    "System Settings",
];

const FOLDERS: &[(&str, &[&str])] = &[
    ("Utilities", &[
        "Activity Monitor", "AirPort Utility", "Audio MIDI Setup", "Bluetooth File Exchange",
        "Boot Camp Assistant", "ColorSync Utility", "Console", "Digital Color Meter", "Disk Utility",
        "Grapher", "Migration Assistant", "Screenshot", "Script Editor", "System Information",
        "Terminal", "VoiceOver Utility", "Automator", "Font Book", "Image Capture", "Print Center",
        "Screen Sharing", "Keychain Access", "Apps", "Time Machine", "Mission Control", "Siri",
        "iPhone Mirroring", "Magnifier", "Wireless Diagnostics", "Directory Utility", "Feedback Assistant",
    ]),
    ("Productivity", &[
        "Pages", "Numbers", "Keynote", "TextEdit", "Preview", "Shortcuts", "Clock", "Calculator",
        "Dictionary", "Stickies", "Weather", "Journal", "Voice Memos", "VoiceMemos", "Tips", "Image Playground",
    ]),
    ("Creativity", &[
        "GarageBand", "iMovie", "Final Cut Pro", "Logic Pro", "Photo Booth", "Motion", "Compressor",
        "MainStage", "Pixelmator Pro",
    ]),
    ("Entertainment", &["TV", "Podcasts", "Books", "Chess", "News", "Stocks", "Home", "Games"]),
    ("Developer", &[
        "Xcode", "Instruments", "Simulator", "Reality Composer Pro", "SF Symbols", "Create ML",
        "Accessibility Inspector", "FileMerge",
    ]),
];

pub fn default_layout(apps: &[App]) -> Vec<Entry> {
    let mut main: Vec<(usize, &App)> = Vec::new();
    let mut groups: Vec<(String, Vec<&App>)> =
        FOLDERS.iter().map(|(n, _)| ((*n).to_owned(), Vec::new())).collect();
    groups.push(("Other".into(), Vec::new()));
    let mut third: Vec<&App> = Vec::new();

    for a in apps {
        let (id, cat) = info(&a.path);
        if !id.starts_with("com.apple.") {
            third.push(a);
            continue;
        }
        if let Some(i) = MAIN.iter().position(|m| *m == a.name) {
            main.push((i, a));
            continue;
        }
        let name = FOLDERS
            .iter()
            .find(|(_, list)| list.contains(&a.name.as_str()))
            .map(|(n, _)| (*n).to_owned())
            .or_else(|| category_name(&cat).map(String::from))
            .unwrap_or_else(|| "Other".into());
        match groups.iter_mut().find(|(n, _)| *n == name) {
            Some((_, v)) => v.push(a),
            None => groups.push((name, vec![a])),
        }
    }
    main.sort_by_key(|(i, _)| *i);

    let mut items: Vec<Entry> = main.iter().map(|(_, a)| Entry::App(a.path.clone())).collect();
    let mut loose = Vec::new();
    for (name, v) in groups {
        match v.len() {
            0 => {}
            1 => loose.push(Entry::App(v[0].path.clone())),
            _ => items.push(Entry::Folder(Folder {
                id: new_id(),
                name,
                apps: v.iter().map(|a| a.path.clone()).collect(),
            })),
        }
    }
    items.extend(loose);
    items.extend(third.iter().map(|a| Entry::App(a.path.clone())));
    items
}

#[allow(dead_code)]
pub fn debug_dump(items: &[Entry]) -> HashMap<String, usize> {
    let mut m = HashMap::new();
    for e in items {
        if let Entry::Folder(f) = e {
            m.insert(f.name.clone(), f.apps.len());
        }
    }
    m
}
