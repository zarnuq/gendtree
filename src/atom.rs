//! Gentoo package versions and dependency atoms (PMS chapter 3 and 8).

use std::cmp::Ordering;

#[derive(Clone, Debug)]
pub struct Version {
    /// The full version text, including any `-rN`.
    pub full: String,
    nums: Vec<String>,
    letter: Option<u8>,
    /// (rank, number) where alpha=0 beta=1 pre=2 rc=3 p=5 (4 = "no suffix").
    suffixes: Vec<(u8, String)>,
    rev: String,
}

impl Version {
    pub fn parse(s: &str) -> Option<Version> {
        let (main, rev) = match s.rfind("-r") {
            Some(i) if i + 2 < s.len() && s[i + 2..].bytes().all(|c| c.is_ascii_digit()) => {
                (&s[..i], &s[i + 2..])
            }
            _ => (s, "0"),
        };
        let b = main.as_bytes();
        let mut i = 0;
        let mut nums = Vec::new();
        loop {
            let st = i;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            if i == st {
                return None;
            }
            nums.push(main[st..i].to_string());
            if i + 1 < b.len() && b[i] == b'.' && b[i + 1].is_ascii_digit() {
                i += 1;
                continue;
            }
            break;
        }
        let mut letter = None;
        if i < b.len() && b[i].is_ascii_lowercase() {
            letter = Some(b[i]);
            i += 1;
        }
        let mut suffixes = Vec::new();
        while i < b.len() && b[i] == b'_' {
            i += 1;
            let rest = &main[i..];
            let (rank, len) = if rest.starts_with("alpha") {
                (0, 5)
            } else if rest.starts_with("beta") {
                (1, 4)
            } else if rest.starts_with("pre") {
                (2, 3)
            } else if rest.starts_with("rc") {
                (3, 2)
            } else if rest.starts_with('p') {
                (5, 1)
            } else {
                return None;
            };
            i += len;
            let st = i;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            suffixes.push((rank, main[st..i].to_string()));
        }
        if i != b.len() {
            return None;
        }
        Some(Version {
            full: s.to_string(),
            nums,
            letter,
            suffixes,
            rev: rev.to_string(),
        })
    }

    fn cmp_ignoring_rev(&self, o: &Version) -> Ordering {
        let c = cmp_num(&self.nums[0], &o.nums[0]);
        if c != Ordering::Equal {
            return c;
        }
        for i in 1..self.nums.len().max(o.nums.len()) {
            match (self.nums.get(i), o.nums.get(i)) {
                (Some(a), Some(b)) => {
                    let c = if a.starts_with('0') || b.starts_with('0') {
                        a.trim_end_matches('0').cmp(b.trim_end_matches('0'))
                    } else {
                        cmp_num(a, b)
                    };
                    if c != Ordering::Equal {
                        return c;
                    }
                }
                (Some(_), None) => return Ordering::Greater,
                (None, _) => return Ordering::Less,
            }
        }
        let c = self.letter.cmp(&o.letter);
        if c != Ordering::Equal {
            return c;
        }
        for i in 0..self.suffixes.len().max(o.suffixes.len()) {
            match (self.suffixes.get(i), o.suffixes.get(i)) {
                (Some(a), Some(b)) => {
                    let c = a.0.cmp(&b.0).then_with(|| cmp_num(&a.1, &b.1));
                    if c != Ordering::Equal {
                        return c;
                    }
                }
                (Some(a), None) => return if a.0 == 5 { Ordering::Greater } else { Ordering::Less },
                (None, Some(b)) => return if b.0 == 5 { Ordering::Less } else { Ordering::Greater },
                (None, None) => unreachable!(),
            }
        }
        Ordering::Equal
    }
}

impl Ord for Version {
    fn cmp(&self, o: &Version) -> Ordering {
        self.cmp_ignoring_rev(o).then_with(|| cmp_num(&self.rev, &o.rev))
    }
}
impl PartialOrd for Version {
    fn partial_cmp(&self, o: &Version) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}
impl PartialEq for Version {
    fn eq(&self, o: &Version) -> bool {
        self.cmp(o) == Ordering::Equal
    }
}
impl Eq for Version {}

/// Compare two decimal digit strings of arbitrary length numerically.
fn cmp_num(a: &str, b: &str) -> Ordering {
    let a = a.trim_start_matches('0');
    let b = b.trim_start_matches('0');
    a.len().cmp(&b.len()).then_with(|| a.cmp(b))
}

/// Split `cat/pn-ver` into (`cat/pn`, version).
pub fn split_cpv(cpv: &str) -> Option<(String, Version)> {
    let slash = cpv.find('/')?;
    let pf = &cpv[slash + 1..];
    for (i, _) in pf.match_indices('-') {
        if let Some(v) = Version::parse(&pf[i + 1..]) {
            return Some((cpv[..slash + 1 + i].to_string(), v));
        }
    }
    None
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Op {
    Any,
    Lt,
    Le,
    Eq,
    EqGlob,
    Tilde,
    Ge,
    Gt,
}

#[derive(Clone, Debug)]
pub struct Atom {
    pub cp: String,
    op: Op,
    ver: Option<Version>,
    /// For `=cat/pkg-1.2*`: the text "1.2".
    glob: String,
    pub slot: Option<String>,
}

impl Atom {
    /// Parse a dependency atom. Blockers (`!foo`) return None.
    pub fn parse(s: &str) -> Option<Atom> {
        if s.starts_with('!') {
            return None;
        }
        let mut s = s;
        if let Some(i) = s.find('[') {
            s = &s[..i];
        }
        if let Some(i) = s.find("::") {
            s = &s[..i];
        }
        let mut slot = None;
        if let Some(i) = s.find(':') {
            let sl = s[i + 1..].trim_end_matches('=');
            let sl = sl.split_once('/').map_or(sl, |(a, _)| a);
            s = &s[..i];
            if !sl.is_empty() && sl != "*" {
                slot = Some(sl.to_string());
            }
        }
        let ops = [
            (">=", Op::Ge),
            ("<=", Op::Le),
            (">", Op::Gt),
            ("<", Op::Lt),
            ("=", Op::Eq),
            ("~", Op::Tilde),
        ];
        let mut op = Op::Any;
        for (p, o) in ops {
            if let Some(rest) = s.strip_prefix(p) {
                op = o;
                s = rest;
                break;
            }
        }
        if op == Op::Any {
            if !s.contains('/') {
                return None;
            }
            return Some(Atom { cp: s.to_string(), op, ver: None, glob: String::new(), slot });
        }
        let mut glob = String::new();
        if op == Op::Eq {
            if let Some(stripped) = s.strip_suffix('*') {
                s = stripped;
                op = Op::EqGlob;
            }
        }
        let (cp, ver) = split_cpv(s)?;
        if op == Op::EqGlob {
            glob = ver.full.clone();
        }
        Some(Atom { cp, op, ver: Some(ver), glob, slot })
    }

    pub fn matches_version(&self, v: &Version) -> bool {
        let Some(want) = &self.ver else { return true };
        match self.op {
            Op::Any => true,
            Op::Lt => v < want,
            Op::Le => v <= want,
            Op::Eq => v == want,
            Op::Gt => v > want,
            Op::Ge => v >= want,
            Op::Tilde => v.cmp_ignoring_rev(want) == Ordering::Equal,
            Op::EqGlob => {
                // Prefix match, but "1.2*" must not match "1.20".
                v.full.starts_with(&self.glob)
                    && !(self.glob.ends_with(|c: char| c.is_ascii_digit())
                        && v.full[self.glob.len()..].starts_with(|c: char| c.is_ascii_digit()))
            }
        }
    }

    /// `pkg_slot` is the package's SLOT value, e.g. "0" or "3/3.12".
    ///
    /// Sub-slots are ignored: installed packages record the sub-slot they were
    /// built against (`:0/1.2=`), which goes stale until the next rebuild.
    pub fn matches_slot(&self, pkg_slot: &str) -> bool {
        let s = pkg_slot.split_once('/').map_or(pkg_slot, |(s, _)| s);
        self.slot.as_deref().is_none_or(|want| want == s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn version_order() {
        let ordered = [
            "1.0_alpha", "1.0_beta2", "1.0_pre", "1.0_rc1", "1.0", "1.0-r1", "1.0_p1", "1.0a", "1.0.1",
            "1.01", "1.1", "1.10", "2",
        ];
        for w in ordered.windows(2) {
            assert!(v(w[0]) < v(w[1]), "{} < {}", w[0], w[1]);
        }
        assert_eq!(v("1.0"), v("1.0-r0"));
    }

    #[test]
    fn cpv_split() {
        let (cp, ver) = split_cpv("dev-lang/python-3.12.4_p1-r2").unwrap();
        assert_eq!(cp, "dev-lang/python");
        assert_eq!(ver.full, "3.12.4_p1-r2");
        let (cp, _) = split_cpv("x11-libs/gtk+-3.24.41").unwrap();
        assert_eq!(cp, "x11-libs/gtk+");
        let (cp, _) = split_cpv("app-misc/foo-2-bar-1.0").unwrap();
        assert_eq!(cp, "app-misc/foo-2-bar");
    }

    #[test]
    fn atoms() {
        let a = Atom::parse(">=dev-libs/glib-2.80:2[introspection]").unwrap();
        assert_eq!(a.cp, "dev-libs/glib");
        assert!(a.matches_version(&v("2.82")));
        assert!(!a.matches_version(&v("2.78")));
        assert!(a.matches_slot("2"));
        assert!(!a.matches_slot("3"));
        let g = Atom::parse("=dev-lang/python-3.1*").unwrap();
        assert!(g.matches_version(&v("3.1.4")));
        assert!(!g.matches_version(&v("3.12")));
        let t = Atom::parse("~sys-libs/foo-1.0").unwrap();
        assert!(t.matches_version(&v("1.0-r5")));
        assert!(Atom::parse("!sys-apps/foo").is_none());
        assert!(Atom::parse("virtual/libcrypt:=").unwrap().slot.is_none());
    }
}
