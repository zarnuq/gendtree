//! A read-only view of the Portage databases, read straight from disk:
//!
//! - installed packages: `/var/db/pkg/<cat>/<pf>/` (one file per metadata key;
//!   USE conditionals in the *DEPEND files are already evaluated by Portage)
//! - available packages: each repo's `metadata/md5-cache/<cat>/<pf>`, or for
//!   repos without a cache, a best-effort parse of the `.ebuild` itself.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use crate::atom::{Atom, Version, split_cpv};
use crate::depstr;

mod portage;

use portage::{
    files_in, load_cache_meta, load_ebuild_meta, load_vdb_meta, parse_shell_vars, read_dir_sorted,
    repos,
};

pub type Id = usize;

/// Dependency kinds, as bit flags.
pub const RDEP: u8 = 1;
pub const DEP: u8 = 2;
pub const BDEP: u8 = 4;
pub const PDEP: u8 = 8;
pub const IDEP: u8 = 16;
pub const ALL_KINDS: u8 = RDEP | DEP | BDEP | PDEP | IDEP;
pub const DEP_KEYS: [(&str, u8); 5] = [
    ("RDEPEND", RDEP),
    ("DEPEND", DEP),
    ("BDEPEND", BDEP),
    ("PDEPEND", PDEP),
    ("IDEPEND", IDEP),
];

const VDB: &str = "/var/db/pkg";
const WORLD: &str = "/var/lib/portage/world";

enum Source {
    Installed(PathBuf),
    Cache(PathBuf),
    Ebuild(PathBuf),
}

pub struct Pkg {
    pub cp: String,
    pub cpv: String,
    pub ver: Version,
    pub repo: String,
    src: Source,
    meta: OnceLock<Meta>,
}

impl Pkg {
    pub fn installed(&self) -> bool {
        matches!(self.src, Source::Installed(_))
    }
    pub fn name(&self) -> &str {
        self.cp.split_once('/').map_or(&self.cp, |(_, n)| n)
    }
    pub fn category(&self) -> &str {
        self.cp.split_once('/').map_or("", |(c, _)| c)
    }
}

#[derive(Default)]
pub struct Meta {
    pub slot: String,
    pub description: String,
    pub homepage: String,
    pub license: String,
    pub keywords: String,
    /// Enabled USE flags (installed packages only).
    pub use_flags: Vec<String>,
    pub iuse: Vec<String>,
    pub repo: String,
    pub size: Option<u64>,
    deps: [String; 5],
    /// Metadata was scraped from an ebuild, not a cache.
    pub approximate: bool,
}

#[derive(Clone, Debug)]
pub struct Dep {
    pub atom: String,
    pub target: Option<Id>,
    /// Bitmask of RDEP | DEP | ...
    pub kinds: u8,
}

#[derive(Default)]
pub struct Db {
    pub pkgs: Vec<Pkg>,
    installed: HashMap<String, Vec<Id>>,
    available: HashMap<String, Vec<Id>>,
    pub world: Arc<Vec<Dep>>,
    pub world_ids: HashSet<Id>,
    rdeps: HashMap<Id, Vec<(Id, u8)>>,
    dep_cache: HashMap<Id, Arc<Vec<Dep>>>,
    use_on: HashSet<String>,
    use_off: HashSet<String>,
    /// Every known `cat/pn`, sorted.
    names: Vec<String>,
    pub installed_count: usize,
}

impl Db {
    pub fn load() -> Result<Db, String> {
        let mut db = Db::default();

        // Installed packages.
        let cats = fs::read_dir(VDB)
            .map_err(|e| format!("cannot read {VDB}: {e}\nIs this a Gentoo system?"))?;
        for cat in cats.flatten() {
            let cat_name = cat.file_name().to_string_lossy().into_owned();
            for pf in read_dir_sorted(&cat.path()) {
                let pf_name = pf.file_name().unwrap().to_string_lossy().into_owned();
                if pf_name.starts_with(['-', '.']) {
                    continue; // e.g. "-MERGING-foo"
                }
                db.add(
                    format!("{cat_name}/{pf_name}"),
                    String::new(),
                    Source::Installed(pf),
                );
            }
        }
        db.installed_count = db.pkgs.len();

        // Available packages from every configured repo.
        for (repo, loc) in repos() {
            let cache = loc.join("metadata/md5-cache");
            if cache.is_dir() {
                for cat in read_dir_sorted(&cache) {
                    let cat_name = cat.file_name().unwrap().to_string_lossy().into_owned();
                    for f in read_dir_sorted(&cat) {
                        let pf = f.file_name().unwrap().to_string_lossy().into_owned();
                        if pf.starts_with('.') {
                            continue;
                        }
                        db.add(format!("{cat_name}/{pf}"), repo.clone(), Source::Cache(f));
                    }
                }
            } else {
                for cat in read_dir_sorted(&loc) {
                    let cat_name = cat.file_name().unwrap().to_string_lossy().into_owned();
                    if !(cat_name.contains('-') || cat_name == "virtual") || !cat.is_dir() {
                        continue;
                    }
                    for pn in read_dir_sorted(&cat) {
                        for f in read_dir_sorted(&pn) {
                            let fname = f.file_name().unwrap().to_string_lossy().into_owned();
                            if let Some(pf) = fname.strip_suffix(".ebuild") {
                                db.add(format!("{cat_name}/{pf}"), repo.clone(), Source::Ebuild(f));
                            }
                        }
                    }
                }
            }
        }

        for list in db.installed.values_mut().chain(db.available.values_mut()) {
            list.sort_by(|a, b| db.pkgs[*a].ver.cmp(&db.pkgs[*b].ver));
        }
        let mut names: Vec<String> = db
            .installed
            .keys()
            .chain(db.available.keys())
            .cloned()
            .collect();
        names.sort_unstable();
        names.dedup();
        db.names = names;

        // Global USE from make.conf, used to evaluate deps of uninstalled packages.
        let mut make_conf = HashMap::new();
        for f in files_in("/etc/portage/make.conf") {
            if let Ok(text) = fs::read_to_string(f) {
                parse_shell_vars(&text, &mut make_conf);
            }
        }
        for flag in make_conf
            .get("USE")
            .map(String::as_str)
            .unwrap_or("")
            .split_whitespace()
        {
            match flag.strip_prefix('-') {
                Some(f) => {
                    db.use_on.remove(f);
                    db.use_off.insert(f.to_string());
                }
                None => {
                    db.use_off.remove(flag);
                    db.use_on.insert(flag.to_string());
                }
            }
        }

        // @world
        let mut world = Vec::new();
        if let Ok(text) = fs::read_to_string(WORLD) {
            for line in text.lines().map(str::trim) {
                if line.is_empty() || line.starts_with('#') {
                    continue;
                }
                let target = db.resolve(line);
                if let Some(t) = target {
                    db.world_ids.insert(t);
                }
                world.push(Dep {
                    atom: line.to_string(),
                    target,
                    kinds: 0,
                });
            }
        }
        world.sort_by_cached_key(|d| db.sort_key(d));
        db.world = Arc::new(world);

        db.index_reverse_dependencies();
        Ok(db)
    }

    fn index_reverse_dependencies(&mut self) {
        // Reverse dependency index over installed packages.
        for id in 0..self.installed_count {
            for d in self.deps(id).iter() {
                if let Some(t) = d.target.filter(|t| self.pkgs[*t].installed()) {
                    self.rdeps.entry(t).or_default().push((id, d.kinds));
                }
            }
        }
        for list in self.rdeps.values_mut() {
            list.sort_by(|a, b| self.pkgs[a.0].cp.cmp(&self.pkgs[b.0].cp));
        }
    }

    /// Build an installed package graph for tests without reading Portage data.
    #[cfg(test)]
    pub(crate) fn for_test(deps: Vec<Vec<Dep>>, mut world: Vec<Dep>) -> Db {
        let mut db = Db::default();
        for (id, deps) in deps.into_iter().enumerate() {
            db.add(
                format!("test/pkg{id}-1"),
                String::new(),
                Source::Installed(PathBuf::new()),
            );
            db.pkgs[id].meta = OnceLock::from(Meta {
                slot: "0".into(),
                ..Meta::default()
            });
            db.dep_cache.insert(id, Arc::new(deps));
        }
        db.installed_count = db.pkgs.len();
        db.names = db.installed.keys().cloned().collect();
        db.names.sort_unstable();
        db.world_ids = world.iter().filter_map(|dep| dep.target).collect();
        world.sort_by_cached_key(|dep| db.sort_key(dep));
        db.world = Arc::new(world);
        db.index_reverse_dependencies();
        db
    }

    fn add(&mut self, cpv: String, repo: String, src: Source) {
        let Some((cp, ver)) = split_cpv(&cpv) else {
            return;
        };
        let id = self.pkgs.len();
        let map = if matches!(src, Source::Installed(_)) {
            &mut self.installed
        } else {
            &mut self.available
        };
        map.entry(cp.clone()).or_default().push(id);
        self.pkgs.push(Pkg {
            cp,
            cpv,
            ver,
            repo,
            src,
            meta: OnceLock::new(),
        });
    }

    pub fn meta(&self, id: Id) -> &Meta {
        let p = &self.pkgs[id];
        p.meta.get_or_init(|| match &p.src {
            Source::Installed(dir) => load_vdb_meta(dir),
            Source::Cache(f) => load_cache_meta(f),
            Source::Ebuild(f) => load_ebuild_meta(f, &p.cpv),
        })
    }

    pub fn repo(&self, id: Id) -> &str {
        let p = &self.pkgs[id];
        if p.installed() {
            &self.meta(id).repo
        } else {
            &p.repo
        }
    }

    fn best_in(&self, list: Option<&Vec<Id>>, atom: &Atom) -> Option<Id> {
        let mut candidates = list?.iter().rev().copied().filter(|&id| {
            atom.matches_version(&self.pkgs[id].ver)
                && (atom.slot.is_none() || atom.matches_slot(&self.meta(id).slot))
        });
        let first = candidates.next()?;
        // Prefer a keyworded version over a live (9999) ebuild.
        std::iter::once(first)
            .chain(candidates)
            .find(|&id| self.keyworded(id))
            .or(Some(first))
    }

    /// Live ebuilds carry no KEYWORDS; installed packages always count.
    fn keyworded(&self, id: Id) -> bool {
        self.pkgs[id].installed() || !self.meta(id).keywords.trim().is_empty()
    }

    fn resolve_installed(&self, atom: &str) -> Option<Id> {
        let a = Atom::parse(atom)?;
        self.best_in(self.installed.get(&a.cp), &a)
    }

    /// Newest installed match, else newest available match.
    pub fn resolve(&self, atom: &str) -> Option<Id> {
        let a = Atom::parse(atom)?;
        self.best_in(self.installed.get(&a.cp), &a)
            .or_else(|| self.best_in(self.available.get(&a.cp), &a))
    }

    /// The package to show for a bare `cat/pn`.
    pub fn best_for_cp(&self, cp: &str) -> Option<Id> {
        if let Some(l) = self.installed.get(cp) {
            return l.last().copied();
        }
        let l = self.available.get(cp)?;
        l.iter()
            .rev()
            .copied()
            .find(|&i| self.keyworded(i))
            .or(l.last().copied())
    }

    /// The newest keyworded (non-live) available version of this package.
    pub fn newest_available(&self, id: Id) -> Option<Id> {
        self.available
            .get(&self.pkgs[id].cp)?
            .iter()
            .rev()
            .copied()
            .find(|&i| self.keyworded(i))
    }

    pub fn sort_key(&self, d: &Dep) -> String {
        match d.target {
            Some(t) => self.pkgs[t].cp.to_lowercase(),
            None => d.atom.to_lowercase(),
        }
    }

    /// The (deduplicated, sorted) direct dependencies of a package.
    pub fn deps(&mut self, id: Id) -> Arc<Vec<Dep>> {
        if let Some(d) = self.dep_cache.get(&id) {
            return d.clone();
        }
        let d = Arc::new(self.compute_deps(id));
        self.dep_cache.insert(id, d.clone());
        d
    }

    fn compute_deps(&self, id: Id) -> Vec<Dep> {
        let meta = self.meta(id);
        let use_flags: HashSet<String> = if self.pkgs[id].installed() {
            meta.use_flags.iter().cloned().collect()
        } else {
            meta.iuse
                .iter()
                .filter_map(|f| {
                    let (default_on, name) = match f.strip_prefix('+') {
                        Some(n) => (true, n),
                        None => (false, f.trim_start_matches('-')),
                    };
                    let on =
                        self.use_on.contains(name) || (default_on && !self.use_off.contains(name));
                    on.then(|| name.to_string())
                })
                .collect()
        };
        let satisfied = |alt: &[String]| alt.iter().all(|a| self.resolve_installed(a).is_some());

        let mut out: Vec<Dep> = Vec::new();
        let mut index: HashMap<String, usize> = HashMap::new();
        for (k, (_, bit)) in DEP_KEYS.iter().enumerate() {
            let mut atoms = Vec::new();
            depstr::flatten(
                &depstr::parse(&meta.deps[k]),
                &use_flags,
                &satisfied,
                &mut atoms,
            );
            for atom in atoms {
                let target = self.resolve(&atom);
                let key = match target {
                    Some(t) => self.pkgs[t].cp.clone(),
                    None => atom.clone(),
                };
                if target == Some(id) {
                    continue;
                }
                match index.get(&key) {
                    Some(&i) => out[i].kinds |= bit,
                    None => {
                        index.insert(key, out.len());
                        out.push(Dep {
                            atom,
                            target,
                            kinds: *bit,
                        });
                    }
                }
            }
        }
        out.sort_by_cached_key(|d| self.sort_key(d));
        out
    }

    /// Installed packages that depend on `id`, with the kinds of dependency.
    pub fn rdeps(&self, id: Id) -> &[(Id, u8)] {
        self.rdeps.get(&id).map_or(&[], Vec::as_slice)
    }

    /// Shortest chain `world pkg -> ... -> id` through installed dependencies
    /// of the given kinds.
    pub fn why_installed(&self, id: Id, kinds: u8) -> Option<Vec<Id>> {
        let mut prev: HashMap<Id, Id> = HashMap::new();
        let mut queue = VecDeque::from([id]);
        let mut seen = HashSet::from([id]);
        while let Some(cur) = queue.pop_front() {
            if self.world_ids.contains(&cur) {
                let mut chain = vec![cur];
                let mut n = cur;
                while let Some(&child) = prev.get(&n) {
                    chain.push(child);
                    n = child;
                }
                return Some(chain);
            }
            for &(parent, k) in self.rdeps(cur) {
                if k & kinds != 0 && seen.insert(parent) {
                    prev.insert(parent, cur);
                    queue.push_back(parent);
                }
            }
        }
        None
    }

    /// Package names (`cat/pn`) matching a query, best matches first.
    pub fn search(&self, query: &str, limit: usize) -> Vec<String> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        let mut hits: Vec<(u8, bool, &String)> = self
            .names
            .iter()
            .filter_map(|cp| {
                let lc = cp.to_lowercase();
                let pn = lc.split_once('/').map_or(lc.as_str(), |(_, n)| n);
                let rank = if pn == q || lc == q {
                    0
                } else if pn.starts_with(&q) {
                    1
                } else if pn.contains(&q) {
                    2
                } else if lc.contains(&q) {
                    3
                } else {
                    return None;
                };
                Some((rank, !self.installed.contains_key(cp), cp))
            })
            .collect();
        hits.sort();
        hits.into_iter()
            .take(limit)
            .map(|(_, _, cp)| cp.clone())
            .collect()
    }

    pub fn available_count(&self) -> usize {
        self.pkgs.len() - self.installed_count
    }
}

#[cfg(test)]
mod live {
    use super::*;

    /// `cargo test --release -- --ignored --nocapture` on a Gentoo box.
    #[test]
    #[ignore]
    fn load_real_db() {
        let t = std::time::Instant::now();
        let mut db = Db::load().unwrap();
        println!(
            "loaded in {:?}: {} installed, {} available, {} world",
            t.elapsed(),
            db.installed_count,
            db.available_count(),
            db.world.len()
        );
        let unresolved: Vec<_> = db
            .world
            .iter()
            .filter(|d| d.target.is_none())
            .map(|d| d.atom.clone())
            .collect();
        println!("unresolved world atoms: {unresolved:?}");
        let mut missing = 0;
        for id in 0..db.installed_count {
            for d in db.deps(id).iter() {
                if d.target.is_none() {
                    missing += 1;
                    if missing <= 15 {
                        println!("  {} -> unresolved {}", db.pkgs[id].cpv, d.atom);
                    }
                }
            }
        }
        println!("unresolved installed deps: {missing}");
        let glibc = db.best_for_cp("sys-libs/glibc").unwrap();
        println!("glibc required by {}", db.rdeps(glibc).len());
        let zlib = db.best_for_cp("sys-libs/zlib").unwrap();
        println!(
            "why zlib: {:?}",
            db.why_installed(zlib, ALL_KINDS).map(|c| c
                .iter()
                .map(|&i| db.pkgs[i].cpv.clone())
                .collect::<Vec<_>>())
        );
        let t = std::time::Instant::now();
        let hits = db.search("firefox", 10);
        let fx = db.best_for_cp("www-client/firefox").unwrap_or(0);
        println!(
            "search {:?} {:?} deps={}",
            t.elapsed(),
            hits,
            db.deps(fx).len()
        );
    }
}
