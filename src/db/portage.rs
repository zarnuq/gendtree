//! Portage metadata formats and repository configuration read from disk.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::atom::split_cpv;

use super::{DEP_KEYS, Meta};

// --------------------------------------------------------------------------- //
// Metadata loaders
// --------------------------------------------------------------------------- //

pub(super) fn load_vdb_meta(dir: &Path) -> Meta {
    let read = |k: &str| {
        fs::read_to_string(dir.join(k))
            .map(|s| s.trim().to_string())
            .unwrap_or_default()
    };
    Meta {
        slot: read("SLOT"),
        description: read("DESCRIPTION"),
        homepage: read("HOMEPAGE"),
        license: read("LICENSE"),
        keywords: read("KEYWORDS"),
        use_flags: read("USE").split_whitespace().map(String::from).collect(),
        iuse: read("IUSE").split_whitespace().map(String::from).collect(),
        repo: read("repository"),
        size: read("SIZE").parse().ok(),
        deps: DEP_KEYS.map(|(k, _)| read(k)),
        approximate: false,
    }
}

fn meta_from_vars(vars: &HashMap<String, String>) -> Meta {
    let get = |k: &str| vars.get(k).cloned().unwrap_or_default();
    Meta {
        slot: get("SLOT"),
        description: get("DESCRIPTION"),
        homepage: get("HOMEPAGE"),
        license: get("LICENSE"),
        keywords: get("KEYWORDS"),
        iuse: get("IUSE").split_whitespace().map(String::from).collect(),
        deps: DEP_KEYS.map(|(k, _)| get(k)),
        ..Meta::default()
    }
}

pub(super) fn load_cache_meta(f: &Path) -> Meta {
    let text = fs::read_to_string(f).unwrap_or_default();
    let vars = text
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    meta_from_vars(&vars)
}

pub(super) fn load_ebuild_meta(f: &Path, cpv: &str) -> Meta {
    let mut vars = HashMap::new();
    // Variables ebuilds commonly reference.
    if let Some((cp, ver)) = split_cpv(cpv) {
        let pn = cp
            .split_once('/')
            .map_or(cp.as_str(), |(_, n)| n)
            .to_string();
        let pv = ver.full.split("-r").next().unwrap_or("").to_string();
        vars.insert("P".into(), format!("{pn}-{pv}"));
        vars.insert("PN".into(), pn);
        vars.insert("PV".into(), pv);
    }
    let text = fs::read_to_string(f).unwrap_or_default();
    parse_shell_vars(&text, &mut vars);
    let mut m = meta_from_vars(&vars);
    if m.slot.is_empty() {
        m.slot = "0".into();
    }
    m.approximate = true;
    m
}

/// A deliberately small reader for `NAME="value"` / `NAME+="value"`
/// assignments at the start of a line, with `$VAR` / `${VAR}` expansion of
/// previously seen variables. Good enough for make.conf and simple ebuilds.
pub(super) fn parse_shell_vars(text: &str, vars: &mut HashMap<String, String>) {
    let mut lines = text.lines();
    while let Some(line) = lines.next() {
        let line = line.trim_start();
        let line = line.strip_prefix("export ").unwrap_or(line);
        let name_len = line
            .bytes()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == b'_')
            .count();
        if name_len == 0 {
            continue;
        }
        let (name, rest) = line.split_at(name_len);
        let (append, rest) = match rest.strip_prefix("+=") {
            Some(r) => (true, r),
            None => match rest.strip_prefix('=') {
                Some(r) => (false, r),
                None => continue,
            },
        };
        let raw = match rest.chars().next() {
            Some(q @ ('"' | '\'')) => {
                let mut buf = rest[1..].to_string();
                loop {
                    if let Some(end) = find_close(&buf, q) {
                        buf.truncate(end);
                        break;
                    }
                    match lines.next() {
                        Some(l) => {
                            buf.push('\n');
                            buf.push_str(l);
                        }
                        None => break,
                    }
                }
                if q == '"' { expand(&buf, vars) } else { buf }
            }
            _ => expand(rest.split_whitespace().next().unwrap_or(""), vars),
        };
        let value = match (append, vars.get(name)) {
            (true, Some(old)) => format!("{old} {raw}"),
            _ => raw,
        };
        vars.insert(name.to_string(), value);
    }
}

fn find_close(s: &str, q: char) -> Option<usize> {
    let mut escaped = false;
    for (i, c) in s.char_indices() {
        if escaped {
            escaped = false;
        } else if c == '\\' && q == '"' {
            escaped = true;
        } else if c == q {
            return Some(i);
        }
    }
    None
}

fn expand(s: &str, vars: &HashMap<String, String>) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('$') {
        out.push_str(&rest[..i]);
        rest = &rest[i + 1..];
        let (name, after) = if let Some(r) = rest.strip_prefix('{') {
            match r.find('}') {
                Some(end) => (&r[..end], &r[end + 1..]),
                None => ("", r),
            }
        } else {
            let n = rest
                .bytes()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == b'_')
                .count();
            (&rest[..n], &rest[n..])
        };
        // Ignore anything fancier than a plain name (e.g. ${PV//./_}).
        if name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') {
            out.push_str(vars.get(name).map_or("", String::as_str));
        }
        rest = after;
    }
    out.push_str(rest);
    out
}

// --------------------------------------------------------------------------- //
// Config helpers
// --------------------------------------------------------------------------- //

pub(super) fn read_dir_sorted(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir)
        .map(|r| r.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    v.sort();
    v
}

/// A config path that may be a file or a directory of files.
pub(super) fn files_in(path: &str) -> Vec<PathBuf> {
    let p = Path::new(path);
    if p.is_dir() {
        read_dir_sorted(p)
            .into_iter()
            .filter(|f| f.is_file())
            .collect()
    } else if p.is_file() {
        vec![p.to_path_buf()]
    } else {
        Vec::new()
    }
}

/// (name, location) of every configured repository.
pub(super) fn repos() -> Vec<(String, PathBuf)> {
    let mut locs: Vec<(String, PathBuf)> = Vec::new();
    let mut files = files_in("/usr/share/portage/config/repos.conf");
    files.extend(files_in("/etc/portage/repos.conf"));
    for f in files {
        let Ok(text) = fs::read_to_string(&f) else {
            continue;
        };
        let mut section = String::new();
        for line in text.lines().map(str::trim) {
            if let Some(s) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
                section = s.trim().to_string();
            } else if let Some((k, v)) = line.split_once('=')
                && k.trim() == "location"
                && section != "DEFAULT"
            {
                let loc = PathBuf::from(v.trim());
                match locs.iter_mut().find(|(n, _)| *n == section) {
                    Some(e) => e.1 = loc,
                    None => locs.push((section.clone(), loc)),
                }
            }
        }
    }
    locs.retain(|(_, l)| l.is_dir());
    locs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_vars() {
        let mut vars = HashMap::new();
        parse_shell_vars(
            "DEPEND=\"a/b\nc/d\"\nRDEPEND=\"${DEPEND} e/f\"\nIUSE='+x y'\nRDEPEND+=\" g/h\"\nSLOT=0\n",
            &mut vars,
        );
        assert_eq!(vars["RDEPEND"], "a/b\nc/d e/f  g/h");
        assert_eq!(vars["IUSE"], "+x y");
        assert_eq!(vars["SLOT"], "0");
    }
}
