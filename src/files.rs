use std::collections::HashMap;
use std::ffi::{c_void, CStr};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, RwLock};

// spotlight: 5 daemons, a database the size of a small country, and it still
// can't find a file you saved 10 seconds ago. "indexing..." for 3 days after
// every update. mds eating 200% cpu so it can tell you a pdf exists. fuck off.
//
// this one: walk outward. downloads/documents/desktop first (searchable in ms),
// then the rest of $HOME, then icloud + external drives. all cores, one flat
// buffer, fsevents tells us which dirs changed and we rescan just those.
// no content indexing, no metadata soup, no "Spotlight is reindexing your mac".

const DIR: u8 = 1;
const DEAD: u8 = 2;
const TIER_SHIFT: u8 = 2;

#[derive(Clone, Copy)]
struct Item {
    off: u32,
    len: u16,
    name: u16,
    depth: u8,
    flags: u8,
}

#[derive(Default)]
pub struct Index {
    buf: Vec<u8>,
    items: Vec<Item>,
    dead: usize,
}

pub struct Hit {
    pub path: String,
    pub score: i32,
}

impl Index {
    fn push(&mut self, path: &[u8], name: usize, depth: u8, dir: bool, tier: u8) {
        if path.len() > u16::MAX as usize {
            return;
        }
        let off = self.buf.len() as u32;
        self.buf.extend_from_slice(path);
        self.items.push(Item {
            off,
            len: path.len() as u16,
            name: name as u16,
            depth,
            flags: (if dir { DIR } else { 0 }) | (tier << TIER_SHIFT),
        });
    }

    fn path(&self, it: &Item) -> &[u8] {
        &self.buf[it.off as usize..it.off as usize + it.len as usize]
    }

    fn append(&mut self, other: Index) {
        let base = self.buf.len() as u32;
        self.buf.extend_from_slice(&other.buf);
        self.items.extend(other.items.into_iter().map(|mut i| {
            i.off += base;
            i
        }));
        self.dead += other.dead;
    }

    fn compact(&mut self) {
        let mut out = Index::default();
        out.buf.reserve(self.buf.len());
        out.items.reserve(self.items.len() - self.dead);
        for it in &self.items {
            if it.flags & DEAD == 0 {
                let p = self.path(it);
                out.push(p, it.name as usize, it.depth, it.flags & DIR != 0, it.flags >> TIER_SHIFT);
            }
        }
        *self = out;
    }

    pub fn len(&self) -> usize {
        self.items.len() - self.dead
    }

    pub fn search(&self, query: &str, usage: &HashMap<String, u32>, limit: usize) -> Vec<Hit> {
        let q = query.trim().to_ascii_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        let terms: Vec<&[u8]> = q.split_whitespace().map(str::as_bytes).collect();
        let whole = q.as_bytes();
        let n = self.items.len();
        let threads = if n > 40_000 { std::thread::available_parallelism().map_or(4, |n| n.get()).min(8) } else { 1 };
        let chunk = n.div_ceil(threads).max(1);
        let mut hits: Vec<(i32, u32)> = std::thread::scope(|s| {
            let hs: Vec<_> = self
                .items
                .chunks(chunk)
                .enumerate()
                .map(|(ci, items)| {
                    let terms = &terms;
                    s.spawn(move || {
                        let mut top: Vec<(i32, u32)> = Vec::with_capacity(limit + 1);
                        for (j, it) in items.iter().enumerate() {
                            if it.flags & DEAD != 0 {
                                continue;
                            }
                            let p = self.path(it);
                            let Some(sc) = score(p, it.name as usize, it.depth, whole, terms) else { continue };
                            let sc = sc + [150, 0, -60, -60][(it.flags >> TIER_SHIFT) as usize & 3];
                            if top.len() == limit && sc <= top[limit - 1].0 {
                                continue;
                            }
                            let i = (ci * chunk + j) as u32;
                            let at = top.partition_point(|x| x.0 >= sc);
                            top.insert(at, (sc, i));
                            top.truncate(limit);
                        }
                        top
                    })
                })
                .collect();
            hs.into_iter().flat_map(|h| h.join().unwrap()).collect()
        });
        let mut out: Vec<Hit> = hits
            .drain(..)
            .map(|(sc, i)| {
                let it = &self.items[i as usize];
                let path = String::from_utf8_lossy(self.path(it)).into_owned();
                let boost = usage.get(&path).map_or(0, |c| (*c as i32 * 40).min(300));
                Hit { path, score: sc + boost }
            })
            .collect();
        out.sort_unstable_by(|a, b| b.score.cmp(&a.score).then_with(|| a.path.len().cmp(&b.path.len())));
        out.truncate(limit);
        out
    }
}

fn find_ci(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.len() > hay.len() {
        return None;
    }
    let f = needle[0];
    (0..=hay.len() - needle.len())
        .find(|&i| hay[i].to_ascii_lowercase() == f && hay[i..i + needle.len()].eq_ignore_ascii_case(needle))
}

fn word_start(s: &[u8], i: usize) -> bool {
    i == 0 || {
        let (p, c) = (s[i - 1], s[i]);
        !p.is_ascii_alphanumeric() || (p.is_ascii_lowercase() && c.is_ascii_uppercase())
    }
}

fn score(path: &[u8], name_at: usize, depth: u8, whole: &[u8], terms: &[&[u8]]) -> Option<i32> {
    let name = &path[name_at..];
    let stem = name.iter().rposition(|&b| b == b'.').filter(|&i| i > 0).map_or(name, |i| &name[..i]);
    let base = if stem.eq_ignore_ascii_case(whole) || name.eq_ignore_ascii_case(whole) {
        1000
    } else if let Some(pos) = find_ci(name, whole) {
        if pos == 0 {
            900
        } else if word_start(name, pos) {
            800
        } else {
            650
        }
    } else if terms.len() > 1 {
        let last = terms[terms.len() - 1];
        find_ci(name, last)?;
        for t in &terms[..terms.len() - 1] {
            find_ci(path, t)?;
        }
        450
    } else {
        return None;
    };
    Some(base - depth as i32 * 12 - name.len() as i32 / 2)
}

fn skip_name(name: &str) -> bool {
    name.starts_with('.')
        || matches!(
            name,
            "node_modules" | "__pycache__" | "site-packages" | "bower_components" | "DerivedData" | "Icon\r"
        )
}

fn is_bundle(name: &str) -> bool {
    let Some((_, ext)) = name.rsplit_once('.') else { return false };
    matches!(
        ext,
        "app" | "framework" | "bundle" | "plugin" | "kext" | "appex" | "xpc" | "photoslibrary"
            | "musiclibrary" | "tvlibrary" | "xcodeproj" | "xcworkspace" | "xcassets" | "lproj"
            | "sparsebundle" | "pkg" | "mpkg" | "playground" | "rtfd" | "pages" | "numbers" | "key"
            | "logicx" | "band" | "fcpbundle" | "imovielibrary" | "dSYM" | "xcarchive" | "docc"
            | "mlmodelc" | "scptd" | "wdgt" | "prefPane" | "saver" | "qlgenerator" | "mdimporter"
    )
}

fn descend(dir: &Path, name: &str) -> bool {
    !is_bundle(name) && !(name == "target" && dir.join("CACHEDIR.TAG").exists())
}

struct Root {
    path: PathBuf,
    depth: u8,
    tier: u8,
    skip_top: &'static [&'static str],
}

fn roots() -> Vec<Root> {
    let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
    let mut v = Vec::new();
    for d in ["Downloads", "Documents", "Desktop"] {
        v.push(Root { path: home.join(d), depth: 1, tier: 0, skip_top: &[] });
    }
    let icloud = home.join("Library/Mobile Documents/com~apple~CloudDocs");
    v.push(Root {
        path: home,
        depth: 0,
        tier: 1,
        skip_top: &["Library", "Applications", "Downloads", "Documents", "Desktop"],
    });
    if icloud.is_dir() {
        v.push(Root { path: icloud, depth: 1, tier: 2, skip_top: &[] });
    }
    if let Ok(rd) = fs::read_dir("/Volumes") {
        for e in rd.flatten() {
            if e.file_type().is_ok_and(|t| t.is_dir()) && !e.file_name().to_string_lossy().starts_with('.') {
                v.push(Root { path: e.path(), depth: 1, tier: 3, skip_top: &[] });
            }
        }
    }
    v
}

fn read_dir_into(dir: &Path, depth: u8, tier: u8, skip_top: &[&str], out: &mut Index, subdirs: &mut Vec<(PathBuf, u8)>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    let prefix = dir.as_os_str().as_encoded_bytes();
    for e in rd.flatten() {
        let fname = e.file_name();
        let Some(name) = fname.to_str() else { continue };
        if skip_name(name) || skip_top.contains(&name) {
            continue;
        }
        let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
        let mut p = Vec::with_capacity(prefix.len() + 1 + name.len());
        p.extend_from_slice(prefix);
        p.push(b'/');
        p.extend_from_slice(name.as_bytes());
        out.push(&p, prefix.len() + 1, depth, is_dir, tier);
        if is_dir && descend(dir, name) {
            subdirs.push((e.path(), depth.saturating_add(1)));
        }
    }
}

fn crawl(start: Vec<(PathBuf, u8)>, tier: u8, skip_top: &'static [&'static str]) -> Index {
    let top: Option<PathBuf> = if skip_top.is_empty() { None } else { start.first().map(|s| s.0.clone()) };
    struct Q {
        jobs: Mutex<Vec<(PathBuf, u8)>>,
        cv: Condvar,
        pending: AtomicUsize,
    }
    let q = Arc::new(Q { pending: AtomicUsize::new(start.len()), jobs: Mutex::new(start), cv: Condvar::new() });
    let n = std::thread::available_parallelism().map_or(4, |n| n.get()).min(8);
    let hs: Vec<_> = (0..n)
        .map(|_| {
            let q = q.clone();
            let top = top.clone();
            std::thread::spawn(move || {
                let mut out = Index::default();
                let mut sub = Vec::new();
                loop {
                    let job = {
                        let mut j = q.jobs.lock().unwrap();
                        loop {
                            if let Some(x) = j.pop() {
                                break Some(x);
                            }
                            if q.pending.load(Ordering::Acquire) == 0 {
                                break None;
                            }
                            j = q.cv.wait(j).unwrap();
                        }
                    };
                    let Some((dir, depth)) = job else { break };
                    let skip: &[&str] = if top.as_ref() == Some(&dir) { skip_top } else { &[] };
                    read_dir_into(&dir, depth, tier, skip, &mut out, &mut sub);
                    if !sub.is_empty() {
                        q.pending.fetch_add(sub.len(), Ordering::AcqRel);
                        q.jobs.lock().unwrap().append(&mut sub);
                        q.cv.notify_all();
                    }
                    if q.pending.fetch_sub(1, Ordering::AcqRel) == 1 {
                        let _g = q.jobs.lock().unwrap();
                        q.cv.notify_all();
                    }
                }
                out
            })
        })
        .collect();
    let mut idx = Index::default();
    for h in hs {
        idx.append(h.join().unwrap());
    }
    idx
}

fn build_tier(tier: u8) -> Index {
    let mut idx = Index::default();
    for r in roots().into_iter().filter(|r| r.tier == tier) {
        idx.append(crawl(vec![(r.path, r.depth)], r.tier, r.skip_top));
    }
    idx
}

pub fn build() -> Index {
    let mut idx = Index::default();
    for t in 0..4 {
        idx.append(build_tier(t));
    }
    idx
}

static INDEX: OnceLock<RwLock<Index>> = OnceLock::new();

pub fn with_index<R>(f: impl FnOnce(&Index) -> R) -> Option<R> {
    let lock = INDEX.get()?;
    Some(f(&lock.read().unwrap()))
}

// figure out where in our roots a changed dir lives. None = we don't care about it.
fn locate(dir: &str) -> Option<(u8, u8, &'static [&'static str])> {
    for r in roots() {
        let rp = r.path.to_str()?;
        let rest = if dir == rp {
            ""
        } else if let Some(rest) = dir.strip_prefix(rp).and_then(|s| s.strip_prefix('/')) {
            rest
        } else {
            continue;
        };
        let mut depth = r.depth;
        let mut parent = r.path.clone();
        for (i, c) in rest.split('/').filter(|c| !c.is_empty()).enumerate() {
            if skip_name(c) || (i == 0 && r.skip_top.contains(&c)) || !descend(&parent, c) {
                return None;
            }
            parent.push(c);
            depth = depth.saturating_add(1);
        }
        return Some((depth, r.tier, if rest.is_empty() { r.skip_top } else { &[] }));
    }
    None
}

fn under(p: &[u8], dir: &[u8]) -> bool {
    p.len() > dir.len() && p.starts_with(dir) && p[dir.len()] == b'/'
}

fn apply(changed: Vec<(String, bool)>) {
    let Some(lock) = INDEX.get() else { return };
    let mut kill: Vec<u32> = Vec::new();
    let mut add = Index::default();
    {
        let idx = lock.read().unwrap();
        for (dir, deep) in &changed {
            let Some((depth, tier, skip_top)) = locate(dir) else { continue };
            let db = dir.as_bytes();
            let path = Path::new(dir);
            if !path.is_dir() || *deep {
                for (i, it) in idx.items.iter().enumerate() {
                    if it.flags & DEAD == 0 && under(idx.path(it), db) {
                        kill.push(i as u32);
                    }
                }
                if path.is_dir() {
                    add.append(crawl(vec![(path.to_path_buf(), depth)], tier, skip_top));
                }
                continue;
            }
            let mut old: HashMap<&[u8], (u32, bool)> = HashMap::new();
            for (i, it) in idx.items.iter().enumerate() {
                if it.flags & DEAD != 0 {
                    continue;
                }
                let p = idx.path(it);
                if it.name as usize == db.len() + 1 && under(p, db) {
                    old.insert(&p[it.name as usize..], (i as u32, it.flags & DIR != 0));
                }
            }
            let mut fresh = Index::default();
            let mut sub = Vec::new();
            read_dir_into(path, depth, tier, skip_top, &mut fresh, &mut sub);
            let mut new_dirs = Vec::new();
            for it in &fresh.items {
                let p = fresh.path(it);
                let name = &p[it.name as usize..];
                let is_dir = it.flags & DIR != 0;
                match old.remove(name) {
                    Some((_, d)) if d == is_dir => {}
                    other => {
                        if let Some((i, _)) = other {
                            kill.push(i);
                        }
                        add.push(p, it.name as usize, it.depth, is_dir, tier);
                        if is_dir {
                            let full = PathBuf::from(String::from_utf8_lossy(p).into_owned());
                            if sub.iter().any(|(s, _)| *s == full) {
                                new_dirs.push((full, it.depth.saturating_add(1)));
                            }
                        }
                    }
                }
            }
            for (_, (i, was_dir)) in old {
                kill.push(i);
                if was_dir {
                    let gone = idx.path(&idx.items[i as usize]).to_vec();
                    for (j, it) in idx.items.iter().enumerate() {
                        if it.flags & DEAD == 0 && under(idx.path(it), &gone) {
                            kill.push(j as u32);
                        }
                    }
                }
            }
            if !new_dirs.is_empty() {
                add.append(crawl(new_dirs, tier, &[]));
            }
        }
    }
    if kill.is_empty() && add.items.is_empty() {
        return;
    }
    let mut idx = lock.write().unwrap();
    for i in kill {
        let it = &mut idx.items[i as usize];
        if it.flags & DEAD == 0 {
            it.flags |= DEAD;
            idx.dead += 1;
        }
    }
    idx.append(add);
    if idx.dead > idx.items.len() / 4 {
        idx.compact();
    }
}

type FsCallback = extern "C" fn(*const c_void, *mut c_void, usize, *mut c_void, *const u32, *const u64);

#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    fn FSEventStreamCreate(
        alloc: *const c_void,
        cb: FsCallback,
        ctx: *const c_void,
        paths: *const c_void,
        since: u64,
        latency: f64,
        flags: u32,
    ) -> *mut c_void;
    fn FSEventStreamSetDispatchQueue(s: *mut c_void, q: *const c_void);
    fn FSEventStreamStart(s: *mut c_void) -> bool;
}

const SINCE_NOW: u64 = u64::MAX;
const MUST_SCAN_SUBDIRS: u32 = 0x1;
const ROOT_CHANGED: u32 = 0x20;

extern "C" fn on_events(_: *const c_void, _: *mut c_void, n: usize, paths: *mut c_void, flags: *const u32, _: *const u64) {
    let paths = paths as *const *const std::ffi::c_char;
    let mut changed: Vec<(String, bool)> = Vec::new();
    let mut rebuild = false;
    for i in 0..n {
        let f = unsafe { *flags.add(i) };
        let Ok(p) = unsafe { CStr::from_ptr(*paths.add(i)) }.to_str() else { continue };
        let p = p.trim_end_matches('/');
        if f & ROOT_CHANGED != 0 {
            rebuild = true;
            continue;
        }
        if locate(p).is_none() {
            continue;
        }
        let deep = f & MUST_SCAN_SUBDIRS != 0;
        if let Some(e) = changed.iter_mut().find(|(d, _)| d == p) {
            e.1 |= deep;
        } else {
            changed.push((p.to_owned(), deep));
        }
    }
    if rebuild {
        let fresh = build();
        if let Some(l) = INDEX.get() {
            *l.write().unwrap() = fresh;
        }
        return;
    }
    if !changed.is_empty() {
        apply(changed);
    }
}

pub fn start() {
    std::thread::spawn(|| {
        let t = std::time::Instant::now();
        let _ = INDEX.set(RwLock::new(Index::default()));
        for tier in 0..4 {
            let part = build_tier(tier);
            let lock = INDEX.get().unwrap();
            lock.write().unwrap().append(part);
            if std::env::var_os("LIFTOFF_DEBUG").is_some() {
                eprintln!("files: tier {tier} done, {} paths, {:?}", lock.read().unwrap().len(), t.elapsed());
            }
        }
        watch();
    });
}

fn watch() {
    use objc2_foundation::{NSArray, NSString};
    let ps: Vec<_> = roots().iter().filter_map(|r| r.path.to_str().map(NSString::from_str)).collect();
    let arr = NSArray::from_retained_slice(&ps);
    let q = dispatch2::DispatchQueue::new("liftoff.fsevents", None);
    unsafe {
        let s = FSEventStreamCreate(
            std::ptr::null(),
            on_events,
            std::ptr::null(),
            objc2::rc::Retained::as_ptr(&arr) as *const c_void,
            SINCE_NOW,
            1.0,
            0x2 | 0x4,
        );
        if s.is_null() {
            eprintln!("liftoff: fsevents said no");
            return;
        }
        FSEventStreamSetDispatchQueue(s, &*q as *const _ as *const c_void);
        FSEventStreamStart(s);
    }
    std::mem::forget(q);
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ranks_and_finds() {
        let mut idx = Index::default();
        let items = [
            "/Users/x/Documents/report.pdf",
            "/Users/x/Documents/old/report-2019.pdf",
            "/Users/x/Developer/liftoff/src/view.rs",
            "/Users/x/Developer/liftoff/README.md",
            "/Users/x/Downloads/MyReportCard.png",
        ];
        for p in items {
            let name = p.rfind('/').unwrap() + 1;
            let depth = p.matches('/').count() as u8 - 3;
            idx.push(p.as_bytes(), name, depth, false, 1);
        }
        let u = HashMap::new();
        let r = idx.search("report", &u, 10);
        assert_eq!(r[0].path, items[0]);
        assert_eq!(r.len(), 3);
        let r = idx.search("liftoff view", &u, 10);
        assert_eq!(r[0].path, items[2]);
        assert!(idx.search("zzz", &u, 10).is_empty());
        let r = idx.search("readme", &u, 10);
        assert_eq!(r[0].path, items[3]);
    }

    #[test]
    fn crawl_home() {
        let t = std::time::Instant::now();
        let idx = build();
        let dt = t.elapsed();
        let t = std::time::Instant::now();
        let r = idx.search("readme", &HashMap::new(), 30);
        eprintln!("indexed {} in {:?}, {} bytes; search {:?} -> {} hits", idx.len(), dt, idx.buf.len(), t.elapsed(), r.len());
        assert!(idx.len() > 100);
    }
}
