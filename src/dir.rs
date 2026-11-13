use std::ffi::{c_char, c_void, CString};
use std::fs;
use std::os::fd::AsRawFd;
use std::os::macos::fs::MetadataExt as _;
use std::os::unix::fs::MetadataExt as _;

// finder stats every file one at a time and then asks three daemons what they think
// about each one. getattrlistbulk hands back names + type + mtime + size for a whole
// directory in a couple of syscalls. it's been in the kernel since 10.10. use it.

#[derive(Clone, Debug)]
pub struct Ent {
    pub name: String,
    pub lc: String,
    pub path: String,
    pub dir: bool,
    pub size: u64,
    pub mtime: i64,
    pub hidden: bool,
    pub place: String,
    // finder's colour label, 0 = none. rides along in the same syscall for free
    pub label: u8,
    // someone pasted their own icon onto this one, so keep our paint off it
    pub custom: bool,
}

#[repr(C)]
struct AttrList {
    bitmapcount: u16,
    reserved: u16,
    commonattr: u32,
    volattr: u32,
    dirattr: u32,
    fileattr: u32,
    forkattr: u32,
}

unsafe extern "C" {
    fn getattrlistbulk(dirfd: i32, alist: *const AttrList, buf: *mut c_void, size: usize, options: u64) -> i32;
    fn getxattr(path: *const c_char, name: *const c_char, value: *mut c_void, size: usize, position: u32, options: i32) -> isize;
    fn setxattr(path: *const c_char, name: *const c_char, value: *const c_void, size: usize, position: u32, options: i32) -> i32;
    fn removexattr(path: *const c_char, name: *const c_char, options: i32) -> i32;
}

const CMN_NAME: u32 = 0x1;
const CMN_OBJTYPE: u32 = 0x8;
const CMN_MODTIME: u32 = 0x400;
const CMN_FNDRINFO: u32 = 0x4000;
const CMN_FLAGS: u32 = 0x4_0000;
const CMN_RETURNED: u32 = 0x8000_0000;
const FILE_DATALENGTH: u32 = 0x200;
const VDIR: u32 = 2;
const VLNK: u32 = 5;
const UF_HIDDEN: u32 = 0x8000;
const FI_COLOR: u16 = 0x000E;
const FI_CUSTOM_ICON: u16 = 0x0400;
const TAGS: &std::ffi::CStr = c"com.apple.metadata:_kMDItemUserTags";

// the 16 bit finder flags sit at byte 8 of the 32 byte finder info, big endian,
// because this struct was designed for a 68k mac
fn finder_bits(fi: &[u8]) -> (u8, bool) {
    if fi.len() < 10 {
        return (0, false);
    }
    let f = u16::from_be_bytes([fi[8], fi[9]]);
    (((f & FI_COLOR) >> 1) as u8, f & FI_CUSTOM_ICON != 0)
}

fn xattr(path: &str, name: &std::ffi::CStr, cap: usize) -> Option<Vec<u8>> {
    let c = CString::new(path).ok()?;
    let mut buf = vec![0u8; cap];
    let n = unsafe { getxattr(c.as_ptr(), name.as_ptr(), buf.as_mut_ptr() as *mut c_void, cap, 0, 0) };
    if n < 0 {
        return None;
    }
    buf.truncate(n as usize);
    Some(buf)
}

// raw binary plist of the tag list, "Name\nColourNumber" per entry
pub fn tag_blob(path: &str) -> Option<Vec<u8>> {
    xattr(path, TAGS, 16 * 1024)
}

pub fn set_tag_blob(path: &str, blob: Option<&[u8]>) -> bool {
    let Ok(c) = CString::new(path) else { return false };
    match blob {
        Some(b) => unsafe { setxattr(c.as_ptr(), TAGS.as_ptr(), b.as_ptr() as *const c_void, b.len(), 0, 0) == 0 },
        None => unsafe { removexattr(c.as_ptr(), TAGS.as_ptr(), 0) == 0 },
    }
}

fn join(dir: &str, name: &str) -> String {
    let mut p = String::with_capacity(dir.len() + 1 + name.len());
    if dir != "/" {
        p.push_str(dir);
    }
    p.push('/');
    p.push_str(name);
    p
}

fn bulk(dir: &str) -> Option<Vec<Ent>> {
    let f = fs::File::open(dir).ok()?;
    let fd = f.as_raw_fd();
    let al = AttrList {
        bitmapcount: 5,
        reserved: 0,
        commonattr: CMN_RETURNED | CMN_NAME | CMN_OBJTYPE | CMN_MODTIME | CMN_FNDRINFO | CMN_FLAGS,
        volattr: 0,
        dirattr: 0,
        fileattr: FILE_DATALENGTH,
        forkattr: 0,
    };
    let mut buf = vec![0u8; 256 * 1024];
    let mut out = Vec::new();
    loop {
        let n = unsafe { getattrlistbulk(fd, &al, buf.as_mut_ptr() as *mut c_void, buf.len(), 0) };
        if n < 0 {
            return None;
        }
        if n == 0 {
            break;
        }
        let u32_at = |i: usize| u32::from_ne_bytes(buf[i..i + 4].try_into().unwrap());
        let i64_at = |i: usize| i64::from_ne_bytes(buf[i..i + 8].try_into().unwrap());
        let mut p = 0usize;
        for _ in 0..n {
            let len = u32_at(p) as usize;
            if len < 24 || p + len > buf.len() {
                return None;
            }
            let mut q = p + 4;
            let (rc, rf) = (u32_at(q), u32_at(q + 12));
            q += 20;
            let mut name = "";
            if rc & CMN_NAME != 0 {
                let off = u32_at(q) as i32;
                let l = u32_at(q + 4) as usize;
                let start = (q as isize + off as isize) as usize;
                if l > 0 && start + l <= buf.len() {
                    name = std::str::from_utf8(&buf[start..start + l - 1]).unwrap_or("");
                }
                q += 8;
            }
            let mut ty = 0;
            if rc & CMN_OBJTYPE != 0 {
                ty = u32_at(q);
                q += 4;
            }
            let mut mtime = 0;
            if rc & CMN_MODTIME != 0 {
                mtime = i64_at(q);
                q += 16;
            }
            let (mut label, mut custom) = (0, false);
            if rc & CMN_FNDRINFO != 0 && q + 32 <= p + len {
                (label, custom) = finder_bits(&buf[q..q + 32]);
                q += 32;
            }
            let mut flags = 0;
            if rc & CMN_FLAGS != 0 {
                flags = u32_at(q);
                q += 4;
            }
            let mut size = 0u64;
            if rf & FILE_DATALENGTH != 0 {
                size = i64_at(q).max(0) as u64;
            }
            p += len;
            if name.is_empty() {
                continue;
            }
            let path = join(dir, name);
            let mut is_dir = ty == VDIR;
            if ty == VLNK {
                if let Ok(md) = fs::metadata(&path) {
                    is_dir = md.is_dir();
                    size = if is_dir { 0 } else { md.len() };
                }
            }
            out.push(Ent {
                lc: name.to_lowercase(),
                hidden: name.starts_with('.') || flags & UF_HIDDEN != 0,
                name: name.to_owned(),
                path,
                dir: is_dir,
                size: if is_dir { 0 } else { size },
                mtime,
                place: String::new(),
                label,
                custom,
            });
        }
    }
    Some(out)
}

fn slow(dir: &str) -> Vec<Ent> {
    let Ok(rd) = fs::read_dir(dir) else { return Vec::new() };
    rd.flatten().filter_map(|e| ent_of(&join(dir, &e.file_name().to_string_lossy()))).collect()
}

pub fn ent_of(path: &str) -> Option<Ent> {
    let link = fs::symlink_metadata(path).ok()?;
    let md = if link.file_type().is_symlink() { fs::metadata(path).unwrap_or(link.clone()) } else { link.clone() };
    let name = if path == "/" { "/" } else { path.rsplit('/').next()? };
    let dir = md.is_dir();
    let (label, custom) = xattr(path, c"com.apple.FinderInfo", 32).map_or((0, false), |fi| finder_bits(&fi));
    Some(Ent {
        name: name.to_owned(),
        lc: name.to_lowercase(),
        path: path.to_owned(),
        dir,
        size: if dir { 0 } else { md.len() },
        mtime: md.mtime(),
        hidden: name.starts_with('.') || link.st_flags() & UF_HIDDEN != 0,
        place: String::new(),
        label,
        custom,
    })
}

pub fn list(dir: &str) -> Vec<Ent> {
    bulk(dir).unwrap_or_else(|| slow(dir))
}

pub fn stamp(dir: &str) -> i64 {
    fs::metadata(dir).map(|m| m.mtime() * 1_000_000_000 + m.mtime_nsec()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn bulk_matches_readdir() {
        let d = std::env::temp_dir().join(format!("liftoff-dir-{}", std::process::id()));
        fs::create_dir_all(d.join("sub")).unwrap();
        fs::write(d.join("a.txt"), "hello").unwrap();
        fs::write(d.join(".hidden"), "").unwrap();
        fs::write(d.join("ünï cödé.bin"), vec![0u8; 3000]).unwrap();
        std::os::unix::fs::symlink(d.join("sub"), d.join("link")).unwrap();
        let ds = d.to_str().unwrap();
        let b = bulk(ds).expect("getattrlistbulk failed");
        let by: HashMap<&str, &Ent> = b.iter().map(|e| (e.name.as_str(), e)).collect();
        assert_eq!(b.len(), 5);
        assert!(by["sub"].dir && by["link"].dir && !by["a.txt"].dir);
        assert_eq!(by["a.txt"].size, 5);
        assert_eq!(by["ünï cödé.bin"].size, 3000);
        assert!(by[".hidden"].hidden && !by["a.txt"].hidden);
        assert!(by["a.txt"].mtime > 1_600_000_000);
        assert_eq!(by["a.txt"].path, format!("{ds}/a.txt"));
        let s = slow(ds);
        assert_eq!(s.len(), 5);
        for e in &s {
            let o = by[e.name.as_str()];
            assert_eq!((e.dir, e.size, e.mtime, e.hidden, e.label, e.custom), (o.dir, o.size, o.mtime, o.hidden, o.label, o.custom), "{}", e.name);
        }
        fs::remove_dir_all(&d).unwrap();
    }

    #[test]
    fn finder_flags() {
        let mut fi = [0u8; 32];
        assert_eq!(finder_bits(&fi), (0, false));
        fi[9] = 6 << 1;
        assert_eq!(finder_bits(&fi), (6, false));
        fi[8] = 0x04;
        assert_eq!(finder_bits(&fi), (6, true));
        assert_eq!(finder_bits(&fi[..4]), (0, false));
    }

    #[test]
    fn bulk_is_faster() {
        let home = std::env::var("HOME").unwrap();
        for d in ["/usr/bin", "/System/Library/Frameworks", "/Applications", &home, &format!("{home}/Downloads")] {
            let (_, _) = (bulk(d), slow(d));
            let best = |f: &dyn Fn() -> usize| {
                (0..7).map(|_| {
                    let t = std::time::Instant::now();
                    let n = f();
                    (t.elapsed(), n)
                })
                .min()
                .unwrap()
            };
            let (ta, a) = best(&|| bulk(d).map_or(0, |v| v.len()));
            let (tb, b) = best(&|| slow(d).len());
            eprintln!("{d}: bulk {a} in {ta:?}, readdir+stat {b} in {tb:?}");
            assert_eq!(a, b);
        }
    }
}
