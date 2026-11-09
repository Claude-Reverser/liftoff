use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

#[derive(Clone, Debug)]
pub struct App {
    pub path: String,
    pub name: String,
    pub lc: String,
    pub initials: String,
    pub mtime: u64,
}

fn roots() -> Vec<PathBuf> {
    let mut v = vec![
        PathBuf::from("/Applications"),
        PathBuf::from("/System/Applications"),
    ];
    if let Some(h) = std::env::var_os("HOME") {
        v.push(Path::new(&h).join("Applications"));
    }
    v
}

// apple. apple. APPLE.
// you have a trillion dollars and a campus shaped like a fucking donut and
// you STILL can't list the apps in /Applications without it shitting itself.
// mds, mds_stores, mdworker_shared, corespotlightd, spotlightknowledged --
// FIVE daemons, a private database, an xpc protocol nobody's allowed to see,
// and half the time the launcher just doesn't load. what the fuck were you doing.
//
// and then you deleted launchpad. the one thing that worked. replaced it with a
// spotlight tab that forgets your apps exist. absolute clowns.
//
// here's the whole "indexer". it's a for loop. readdir + stat, ~150 bundles,
// under a millisecond, runs every single time the window opens so it can never
// be stale. no daemon, no db, no reindex, no "Index is read-only" bullshit.
// that's it. that was the hard part. it took you guys a decade to fuck it up.
pub fn scan() -> Vec<App> {
    let mut out = Vec::with_capacity(256);
    let mut seen = HashSet::with_capacity(256);
    for r in roots() {
        walk(&r, 0, &mut out, &mut seen);
    }
    out.sort_unstable_by(|a, b| a.lc.cmp(&b.lc));
    out
}

// follows symlinks cause safari lives in a cryptex now. of course it does.
fn walk(dir: &Path, depth: u32, out: &mut Vec<App>, seen: &mut HashSet<String>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let fname = e.file_name();
        let fname = fname.to_string_lossy();
        if fname.starts_with('.') {
            continue;
        }
        let p = e.path();
        if let Some(stem) = fname.strip_suffix(".app") {
            let Ok(md) = fs::metadata(&p) else { continue };
            if !md.is_dir() || stem == "Liftoff" {
                continue;
            }
            let lc = stem.to_lowercase();
            if !seen.insert(lc.clone()) {
                continue;
            }
            let mtime = md
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs());
            out.push(App {
                path: p.to_string_lossy().into_owned(),
                name: stem.to_owned(),
                initials: initials(stem),
                lc,
                mtime,
            });
        } else if depth < 2 {
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                walk(&p, depth + 1, out, seen);
            }
        }
    }
}

fn initials(name: &str) -> String {
    let mut s = String::new();
    let mut prev: Option<char> = None;
    for c in name.chars() {
        let boundary = match prev {
            None => true,
            Some(p) => {
                !p.is_alphanumeric() && c.is_alphanumeric()
                    || (p.is_lowercase() && c.is_uppercase())
            }
        };
        if boundary && c.is_alphanumeric() {
            s.extend(c.to_lowercase());
        }
        prev = Some(c);
    }
    s
}

pub fn score(app: &App, q: &str) -> Option<i32> {
    let n = app.lc.as_str();
    if n == q {
        return Some(10_000);
    }
    if n.starts_with(q) {
        return Some(9_000 - n.len() as i32);
    }
    if let Some(pos) = n
        .match_indices(q)
        .map(|(i, _)| i)
        .find(|&i| i > 0 && !n.as_bytes()[i - 1].is_ascii_alphanumeric())
    {
        return Some(8_000 - pos as i32);
    }
    if app.initials.starts_with(q) {
        return Some(7_000 - app.initials.len() as i32);
    }
    if let Some(pos) = n.find(q) {
        return Some(6_000 - pos as i32);
    }
    let mut gaps = 0i32;
    let mut it = n.chars();
    let mut first = true;
    for qc in q.chars() {
        let mut skipped = 0;
        loop {
            match it.next() {
                Some(c) if c == qc => break,
                Some(_) => skipped += 1,
                None => return None,
            }
        }
        if !first {
            gaps += skipped;
        }
        first = false;
    }
    Some(4_000 - gaps * 10 - n.len() as i32)
}

fn usage_file() -> PathBuf {
    let h = std::env::var_os("HOME").unwrap_or_default();
    Path::new(&h).join("Library/Application Support/Liftoff/usage.tsv")
}

pub fn load_usage() -> HashMap<String, u32> {
    let mut m = HashMap::new();
    if let Ok(s) = fs::read_to_string(usage_file()) {
        for line in s.lines() {
            if let Some((p, c)) = line.rsplit_once('\t') {
                if let Ok(c) = c.parse() {
                    m.insert(p.to_owned(), c);
                }
            }
        }
    }
    m
}

pub fn save_usage(m: &HashMap<String, u32>) {
    let f = usage_file();
    let _ = fs::create_dir_all(f.parent().unwrap());
    let mut s = String::with_capacity(m.len() * 64);
    for (p, c) in m {
        s.push_str(p);
        s.push('\t');
        s.push_str(&c.to_string());
        s.push('\n');
    }
    let tmp = f.with_extension("tmp");
    if fs::write(&tmp, s).is_ok() {
        let _ = fs::rename(tmp, f);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn app(n: &str) -> App {
        App { path: n.into(), name: n.into(), lc: n.to_lowercase(), initials: initials(n), mtime: 0 }
    }
    #[test]
    fn ranking() {
        let vsc = app("Visual Studio Code");
        let saf = app("Safari");
        let set = app("System Settings");
        assert!(score(&saf, "s").unwrap() > score(&set, "s").unwrap());
        assert!(score(&vsc, "vsc").is_some());
        assert!(score(&vsc, "code").unwrap() > score(&vsc, "vsc").unwrap());
        assert!(score(&set, "settings").is_some());
        assert!(score(&saf, "sfr").is_some());
        assert!(score(&saf, "xyz").is_none());
        assert_eq!(initials("Visual Studio Code"), "vsc");
        assert_eq!(initials("IntelliJ IDEA"), "iji");
    }
    #[test]
    fn scan_is_fast() {
        let t = std::time::Instant::now();
        let a = scan();
        let dt = t.elapsed();
        eprintln!("scanned {} apps in {:?}", a.len(), dt);
        assert!(a.len() > 20);
    }
}
