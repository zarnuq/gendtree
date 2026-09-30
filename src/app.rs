//! The egui front end: a dependency browser with two views, a collapsible
//! tree and columns (each column lists the dependencies of the package
//! selected in the column to its left), plus a details panel.

use std::collections::HashSet;
use std::sync::mpsc::{self, Receiver};
use std::sync::Arc;
use std::time::Duration;

use eframe::egui::{
    self, text::LayoutJob, text::TextWrapping, Align, Align2, Color32, CornerRadius, FontId, Key, Layout, RichText,
    Sense, Stroke, TextFormat, Ui,
};

use crate::db::{Db, Dep, Id, ALL_KINDS, BDEP, DEP, IDEP, PDEP, RDEP};

const COL_W: f32 = 280.0;
const ROW_H: f32 = 40.0;
const TREE_ROW_H: f32 = 26.0;
const INDENT: f32 = 16.0;

/// (bit, letter, short name, portage variable, colour)
const KINDS: [(u8, &str, &str, &str, Color32); 5] = [
    (RDEP, "R", "runtime", "RDEPEND", Color32::from_rgb(0x4c, 0xaf, 0x50)),
    (DEP, "D", "build", "DEPEND", Color32::from_rgb(0x42, 0xa5, 0xf5)),
    (BDEP, "B", "build tool", "BDEPEND", Color32::from_rgb(0xff, 0xa7, 0x26)),
    (PDEP, "P", "post", "PDEPEND", Color32::from_rgb(0xab, 0x47, 0xbc)),
    (IDEP, "I", "install", "IDEPEND", Color32::from_rgb(0x26, 0xa6, 0x9a)),
];

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Sel {
    Pkg(Id),
    /// A dependency that matches no known package.
    Atom(String),
}

impl Sel {
    fn of(d: &Dep) -> Sel {
        d.target.map_or_else(|| Sel::Atom(d.atom.clone()), Sel::Pkg)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Root {
    World,
    Pkg(Id),
}

struct Column {
    parent: Option<Id>,
    items: Vec<Dep>,
    /// Root plus everything selected to the left; these would be cycles.
    ancestors: Vec<Id>,
}

#[derive(Clone, Copy, PartialEq)]
enum View {
    Tree,
    Columns,
}

/// One visible line of the tree view.
struct TreeRow {
    depth: usize,
    dep: Dep,
    /// Selections from the root down to and including this row.
    path: Vec<Sel>,
    /// The package already appears higher up; it isn't expanded again here.
    dup: bool,
    /// Number of dependencies shown under the current filters.
    children: usize,
    open: bool,
}

pub struct App {
    loading: Option<Receiver<Result<Db, String>>>,
    db: Option<Db>,
    error: Option<String>,

    view: View,
    root: Root,
    /// `path[i]` is the selection in column `i`; in the tree, the path from
    /// the root to the selected row.
    path: Vec<Sel>,
    /// The column the keyboard is acting on.
    focus: usize,
    history: Vec<(Root, Vec<Sel>, usize)>,
    /// Open tree nodes, by their path from the root.
    expanded: HashSet<(Root, Vec<Sel>)>,

    kinds: u8,
    /// The kind filter of the view that isn't showing; each view keeps its own.
    other_kinds: u8,
    installed_only: bool,
    query: String,
    results: Vec<String>,
    results_for: String,

    scroll_to_sel: bool,
    reveal_col: Option<usize>,
    focus_search: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> App {
        let (tx, rx) = mpsc::channel();
        let ctx = cc.egui_ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(Db::load());
            ctx.request_repaint();
        });
        App {
            loading: Some(rx),
            db: None,
            error: None,
            view: View::Tree,
            root: Root::World,
            path: Vec::new(),
            focus: 0,
            history: Vec::new(),
            expanded: HashSet::new(),
            // The tree starts with runtime deps only; build deps roughly double it.
            kinds: RDEP | PDEP,
            other_kinds: ALL_KINDS,
            installed_only: false,
            query: String::new(),
            results: Vec::new(),
            results_for: String::new(),
            scroll_to_sel: false,
            reveal_col: None,
            focus_search: false,
        }
    }

    fn set_root(&mut self, root: Root, path: Vec<Sel>) {
        self.history.push((self.root, std::mem::take(&mut self.path), self.focus));
        self.root = root;
        self.focus = path.len().saturating_sub(1);
        self.path = path;
        self.expand_to_selection();
        self.query.clear();
        self.scroll_to_sel = true;
        self.reveal_col = Some(self.focus + 1);
    }

    fn go_back(&mut self) {
        if let Some((root, path, focus)) = self.history.pop() {
            self.root = root;
            self.path = path;
            self.focus = focus;
            self.scroll_to_sel = true;
        }
    }

    fn select(&mut self, col: usize, sel: Sel) {
        self.path.truncate(col);
        self.path.push(sel);
        self.focus = col;
        self.reveal_col = Some(col + 1);
    }

    /// The package the details panel describes.
    fn current(&self) -> Option<Sel> {
        match self.path.get(self.focus).or(self.path.last()) {
            Some(s) => Some(s.clone()),
            None => match self.root {
                Root::Pkg(id) => Some(Sel::Pkg(id)),
                Root::World => None,
            },
        }
    }

    /// Build the columns for the current root and path, trimming any part of
    /// the path that is no longer visible (e.g. after changing a filter).
    fn columns(&mut self) -> Vec<Column> {
        let (kinds, installed_only) = (self.kinds, self.installed_only);
        let db = self.db.as_mut().unwrap();
        let (parent, first) = match self.root {
            Root::World => (None, db.world.clone()),
            Root::Pkg(id) => (Some(id), db.deps(id)),
        };
        let mut ancestors: Vec<Id> = parent.into_iter().collect();
        let filter = |db: &Db, items: &Arc<Vec<Dep>>| -> Vec<Dep> {
            items.iter().filter(|d| shown(db, d, kinds, installed_only)).cloned().collect()
        };
        let mut cols = vec![Column { parent, items: filter(db, &first), ancestors: ancestors.clone() }];
        let mut keep = 0;
        for (i, sel) in self.path.iter().enumerate() {
            if !cols[i].items.iter().any(|d| Sel::of(d) == *sel) {
                break;
            }
            keep = i + 1;
            let Sel::Pkg(id) = *sel else { break };
            if ancestors.contains(&id) {
                break;
            }
            ancestors.push(id);
            let deps = db.deps(id);
            cols.push(Column { parent: Some(id), items: filter(db, &deps), ancestors: ancestors.clone() });
        }
        self.path.truncate(keep);
        self.focus = self.focus.min(cols.len() - 1);
        cols
    }

    /// Open every tree node above the selection so it is visible.
    fn expand_to_selection(&mut self) {
        for i in 1..self.path.len() {
            self.expanded.insert((self.root, self.path[..i].to_vec()));
        }
    }

    fn set_view(&mut self, view: View) {
        if view == self.view {
            return;
        }
        self.view = view;
        std::mem::swap(&mut self.kinds, &mut self.other_kinds);
        self.focus = self.path.len().saturating_sub(1);
        self.expand_to_selection();
        self.scroll_to_sel = true;
        self.reveal_col = Some(self.focus + 1);
    }

    /// Flatten the open part of the tree into rows, and trim the selection to
    /// the deepest row that is still visible.
    fn tree_rows(&mut self) -> Vec<TreeRow> {
        let (kinds, installed_only, root) = (self.kinds, self.installed_only, self.root);
        let db = self.db.as_mut().unwrap();
        let (top, mut seen) = match root {
            Root::World => (db.world.clone(), HashSet::new()),
            Root::Pkg(id) => (db.deps(id), HashSet::from([id])),
        };
        let mut rows = Vec::new();
        let mut walk = Walk { db, expanded: &self.expanded, root, kinds, installed_only, seen: &mut seen, rows: &mut rows };
        walk.add(&top, &mut Vec::new(), 0);

        while !self.path.is_empty() && !rows.iter().any(|r| r.path == self.path) {
            self.path.pop();
        }
        self.focus = self.path.len().saturating_sub(1);
        rows
    }

    // ------------------------------------------------------------------ //
    // Keyboard
    // ------------------------------------------------------------------ //

    fn handle_keys(&mut self, ctx: &egui::Context, cols: &[Column], rows: &[TreeRow]) {
        if ctx.egui_wants_keyboard_input() {
            if ctx.input(|i| i.key_pressed(Key::Escape)) {
                self.query.clear();
                ctx.memory_mut(|m| m.surrender_focus(egui::Id::new("search")));
            }
            return;
        }
        match self.view {
            View::Tree => self.tree_keys(ctx, rows),
            View::Columns => self.column_keys(ctx, cols),
        }
        let pressed = |k: Key| ctx.input(|i| i.key_pressed(k));
        if pressed(Key::Enter) {
            if let Some(Sel::Pkg(id)) = self.path.get(self.focus).cloned() {
                self.set_root(Root::Pkg(id), Vec::new());
            }
        }
        if pressed(Key::Backspace) {
            self.go_back();
        }
        if pressed(Key::Slash) || ctx.input(|i| i.modifiers.command && i.key_pressed(Key::F)) {
            self.focus_search = true;
        }
    }

    /// Up/down/home/end/page keys: the row to move to in a list of `n`.
    fn step_key(ctx: &egui::Context, cur: Option<usize>, n: usize) -> Option<usize> {
        let pressed = |k: Key| ctx.input(|i| i.key_pressed(k));
        let delta: isize = if pressed(Key::ArrowDown) || pressed(Key::J) {
            1
        } else if pressed(Key::ArrowUp) || pressed(Key::K) {
            -1
        } else if pressed(Key::PageDown) {
            15
        } else if pressed(Key::PageUp) {
            -15
        } else if pressed(Key::Home) || pressed(Key::G) && !ctx.input(|i| i.modifiers.shift) {
            isize::MIN / 2
        } else if pressed(Key::End) || pressed(Key::G) {
            isize::MAX / 2
        } else {
            return None;
        };
        if n == 0 {
            return None;
        }
        Some(match cur {
            Some(c) => (c as isize + delta).clamp(0, n as isize - 1) as usize,
            None => 0,
        })
    }

    fn tree_keys(&mut self, ctx: &egui::Context, rows: &[TreeRow]) {
        let pressed = |k: Key| ctx.input(|i| i.key_pressed(k));
        let cur = rows.iter().position(|r| r.path == self.path);
        let mut goto = Self::step_key(ctx, cur, rows.len());

        if pressed(Key::ArrowRight) || pressed(Key::L) {
            match cur.map(|c| (c, &rows[c])) {
                // Jump to where the package is shown in full.
                Some((_, r)) if r.dup => goto = rows.iter().position(|x| !x.dup && x.dep.target == r.dep.target),
                Some((c, r)) if r.open => goto = Some(c + 1),
                Some((_, r)) if r.children > 0 => {
                    self.expanded.insert((self.root, r.path.clone()));
                }
                Some(_) => {}
                None => goto = (!rows.is_empty()).then_some(0),
            }
        }
        if pressed(Key::ArrowLeft) || pressed(Key::H) {
            match cur.map(|c| &rows[c]) {
                Some(r) if r.open => {
                    self.expanded.remove(&(self.root, r.path.clone()));
                }
                _ if self.path.len() > 1 => {
                    self.path.pop();
                    self.scroll_to_sel = true;
                }
                _ => {}
            }
        }
        if let Some(i) = goto {
            self.path = rows[i].path.clone();
            self.scroll_to_sel = true;
        }
        self.focus = self.path.len().saturating_sub(1);
    }

    fn column_keys(&mut self, ctx: &egui::Context, cols: &[Column]) {
        let pressed = |k: Key| ctx.input(|i| i.key_pressed(k));
        let col = &cols[self.focus];
        let cur = self.path.get(self.focus).and_then(|s| col.items.iter().position(|d| Sel::of(d) == *s));
        if let Some(i) = Self::step_key(ctx, cur, col.items.len()) {
            let sel = Sel::of(&col.items[i]);
            self.path.truncate(self.focus);
            self.path.push(sel);
            self.scroll_to_sel = true;
        }

        if pressed(Key::ArrowRight) || pressed(Key::L) {
            if let Some(next) = cols.get(self.focus + 1) {
                if let Some(first) = next.items.first() {
                    self.focus += 1;
                    if self.path.len() <= self.focus {
                        self.path.push(Sel::of(first));
                    }
                    self.scroll_to_sel = true;
                    self.reveal_col = Some(self.focus + 1);
                }
            }
        }
        if (pressed(Key::ArrowLeft) || pressed(Key::H)) && self.focus > 0 {
            self.focus -= 1;
            self.path.truncate(self.focus + 1);
            self.scroll_to_sel = true;
        }
    }

    // ------------------------------------------------------------------ //
    // Panels
    // ------------------------------------------------------------------ //

    fn top_bar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("gendtree").strong().size(16.0));
            ui.add_space(8.0);
            if ui
                .add_enabled(!self.history.is_empty(), egui::Button::new("⏴ Back"))
                .on_hover_text("Previous view (Backspace)")
                .clicked()
            {
                self.go_back();
            }
            if ui.add(egui::Button::new("@world").selected(self.root == Root::World)).clicked()
                && self.root != Root::World
            {
                self.set_root(Root::World, Vec::new());
            }
            ui.separator();
            let mut view = self.view;
            ui.selectable_value(&mut view, View::Tree, "Tree").on_hover_text("Expandable tree of dependencies");
            ui.selectable_value(&mut view, View::Columns, "Columns")
                .on_hover_text("One column per level, like a file browser");
            self.set_view(view);
            ui.separator();
            let search = ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .id(egui::Id::new("search"))
                    .hint_text("Search all packages…  ( / )")
                    .desired_width(300.0),
            );
            if std::mem::take(&mut self.focus_search) {
                search.request_focus();
            }
            if search.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                if let Some(cp) = self.results.first().cloned() {
                    if let Some(id) = self.db.as_ref().unwrap().best_for_cp(&cp) {
                        self.set_root(Root::Pkg(id), Vec::new());
                    }
                }
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.checkbox(&mut self.installed_only, "Installed only");
                ui.separator();
                for &(bit, letter, name, var, color) in KINDS.iter().rev() {
                    let mut on = self.kinds & bit != 0;
                    let text = RichText::new(format!("{letter}  {name}")).color(color);
                    if ui.toggle_value(&mut on, text).on_hover_text(format!("Show {var} dependencies")).changed() {
                        self.kinds ^= bit;
                        if self.kinds == 0 {
                            self.kinds = ALL_KINDS & !bit;
                        }
                    }
                }
                ui.label(RichText::new("Show:").weak());
            });
        });
    }

    fn breadcrumb(&mut self, ui: &mut Ui) {
        let db = self.db.as_ref().unwrap();
        let mut clicked: Option<usize> = None;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let root_label = match self.root {
                Root::World => "@world".to_string(),
                Root::Pkg(id) => db.pkgs[id].cp.clone(),
            };
            if ui.link(RichText::new(root_label).strong()).clicked() {
                clicked = Some(0);
            }
            for (i, sel) in self.path.iter().enumerate() {
                ui.label(RichText::new("›").weak());
                let text = match sel {
                    Sel::Pkg(id) => db.pkgs[*id].name().to_string(),
                    Sel::Atom(a) => a.clone(),
                };
                let text = if i == self.focus { RichText::new(text).strong() } else { RichText::new(text) };
                if ui.link(text).clicked() {
                    clicked = Some(i + 1);
                }
            }
        });
        if let Some(n) = clicked {
            self.path.truncate(n);
            self.focus = n.saturating_sub(1);
            self.scroll_to_sel = true;
        }
    }

    fn columns_view(&mut self, ui: &mut Ui, cols: &[Column]) {
        let db = self.db.as_ref().unwrap();
        let mut action: Option<(usize, Sel, bool)> = None;
        let scroll_to_sel = std::mem::take(&mut self.scroll_to_sel);
        let reveal = self.reveal_col.take();
        let height = ui.available_height();

        egui::ScrollArea::horizontal().id_salt("columns").auto_shrink(false).show(ui, |ui| {
            ui.horizontal_top(|ui| {
                for (ci, col) in cols.iter().enumerate() {
                    let r = ui.allocate_ui_with_layout(
                        egui::vec2(COL_W, height),
                        Layout::top_down(Align::Min),
                        |ui| {
                            ui.set_width(COL_W);
                            column_header(ui, db, col);
                            ui.separator();
                            egui::ScrollArea::vertical()
                                .id_salt(("col", ci, col.parent))
                                .auto_shrink(false)
                                .show(ui, |ui| {
                                    if col.items.is_empty() {
                                        ui.add_space(12.0);
                                        ui.vertical_centered(|ui| {
                                            let msg = if col.parent.is_some() {
                                                "No dependencies\n(with the current filters)"
                                            } else {
                                                "Empty"
                                            };
                                            ui.label(RichText::new(msg).weak());
                                        });
                                    }
                                    let selected = self.path.get(ci);
                                    for d in &col.items {
                                        let sel = Sel::of(d);
                                        let is_sel = selected == Some(&sel);
                                        let cycle = d.target.is_some_and(|t| col.ancestors.contains(&t));
                                        let resp = dep_row(ui, db, d, is_sel, ci == self.focus, cycle);
                                        if is_sel && ci == self.focus && scroll_to_sel {
                                            resp.scroll_to_me(None);
                                        }
                                        if resp.double_clicked() {
                                            action = Some((ci, sel, true));
                                        } else if resp.clicked() {
                                            action = Some((ci, sel, false));
                                        }
                                    }
                                });
                        },
                    );
                    if reveal == Some(ci) {
                        ui.scroll_to_rect(r.response.rect, None);
                    }
                    ui.separator();
                }
            });
        });

        if let Some((ci, sel, make_root)) = action {
            match (make_root, &sel) {
                (true, Sel::Pkg(id)) => self.set_root(Root::Pkg(*id), Vec::new()),
                _ => self.select(ci, sel),
            }
        }
    }

    fn tree_view(&mut self, ui: &mut Ui, rows: &[TreeRow]) {
        let db = self.db.as_ref().unwrap();
        if rows.is_empty() {
            ui.add_space(12.0);
            ui.label(RichText::new("No dependencies (with the current filters)").weak());
            return;
        }
        let scroll_to_sel = std::mem::take(&mut self.scroll_to_sel);
        let sel = rows.iter().position(|r| r.path == self.path);
        let mut toggle: Option<usize> = None;
        let mut pick: Option<(usize, bool)> = None;

        egui::ScrollArea::vertical().id_salt(("tree", self.root)).auto_shrink(false).show_viewport(ui, |ui, view| {
            ui.set_height(TREE_ROW_H * rows.len() as f32);
            let area = ui.max_rect();
            let row_rect = |i: usize| {
                egui::Rect::from_min_size(
                    egui::pos2(area.left(), area.top() + i as f32 * TREE_ROW_H),
                    egui::vec2(area.width(), TREE_ROW_H),
                )
            };
            let first = (view.min.y / TREE_ROW_H).floor().max(0.0) as usize;
            let last = ((view.max.y / TREE_ROW_H).ceil() as usize + 1).min(rows.len());
            for i in first..last {
                let (resp, on_toggle) = tree_row(ui, db, row_rect(i), &rows[i], sel == Some(i), self.root);
                if on_toggle {
                    toggle = Some(i);
                } else if resp.double_clicked() {
                    pick = Some((i, true));
                } else if resp.clicked() {
                    pick = Some((i, false));
                }
            }
            if let (true, Some(i)) = (scroll_to_sel, sel) {
                ui.scroll_to_rect(row_rect(i), None);
            }
        });

        if let Some(i) = toggle {
            let key = (self.root, rows[i].path.clone());
            if !self.expanded.remove(&key) {
                self.expanded.insert(key);
            }
        }
        match pick {
            Some((i, true)) if rows[i].dep.target.is_some() => {
                self.set_root(Root::Pkg(rows[i].dep.target.unwrap()), Vec::new());
            }
            Some((i, _)) => {
                self.path = rows[i].path.clone();
                self.focus = self.path.len() - 1;
            }
            None => {}
        }
    }

    fn search_view(&mut self, ui: &mut Ui) {
        let db = self.db.as_ref().unwrap();
        if self.results_for != self.query {
            self.results = db.search(&self.query, 300);
            self.results_for = self.query.clone();
        }
        ui.label(RichText::new(format!("{} matches — click one to explore its dependencies", self.results.len())).weak());
        ui.separator();
        let mut pick = None;
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            for cp in &self.results {
                let Some(id) = db.best_for_cp(cp) else { continue };
                let p = &db.pkgs[id];
                let resp = ui
                    .horizontal(|ui| {
                        let mut name = RichText::new(p.name()).strong().size(14.0);
                        if !p.installed() {
                            name = name.weak();
                        }
                        let r = ui.add(egui::Button::new(name).frame(false));
                        ui.label(RichText::new(p.category()).weak());
                        ui.label(RichText::new(&p.ver.full).small().weak());
                        if p.installed() {
                            ui.label(RichText::new("installed").small().color(KINDS[0].4));
                        }
                        ui.label(RichText::new(&db.meta(id).description).weak());
                        r
                    })
                    .inner;
                if resp.clicked() {
                    pick = Some(id);
                }
            }
        });
        if let Some(id) = pick {
            self.set_root(Root::Pkg(id), Vec::new());
        }
    }

    fn details(&mut self, ui: &mut Ui) {
        let Some(sel) = self.current() else {
            let db = self.db.as_ref().unwrap();
            ui.add_space(8.0);
            ui.heading("@world");
            ui.label(format!("{} packages you installed explicitly.", db.world.len()));
            ui.add_space(8.0);
            ui.label(RichText::new(match self.view {
                View::Tree => "Select a package to see what it depends on. Click ⏵ or press ➡ \
                               to open a package's dependencies beneath it.",
                View::Columns => "Select a package to see what it depends on. Each column to the right \
                                  lists the dependencies of the package selected to its left.",
            }).weak());
            ui.add_space(8.0);
            legend(ui, self.view);
            return;
        };
        let id = match sel {
            Sel::Pkg(id) => id,
            Sel::Atom(a) => {
                ui.heading("Unresolved dependency");
                ui.monospace(&a);
                ui.label(RichText::new("No installed or available package matches this atom.").weak());
                return;
            }
        };

        // Collect everything first so `self` is free for the buttons below.
        let db = self.db.as_mut().unwrap();
        let n_deps = db.deps(id).len();
        let db = self.db.as_ref().unwrap();
        let p = &db.pkgs[id];
        let m = db.meta(id);
        let rdeps: Vec<(Id, u8)> = db.rdeps(id).iter().copied().filter(|(_, k)| k & self.kinds != 0).collect();
        let chain = if p.installed() && !db.world_ids.contains(&id) { db.why_installed(id, self.kinds) } else { None };
        let newer = db.newest_available(id).filter(|&n| p.installed() && db.pkgs[n].ver > p.ver);

        let mut open_root: Option<Id> = None;
        let mut show_chain = false;

        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.add_space(6.0);
            ui.label(RichText::new(p.name()).size(20.0).strong());
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(p.category()).weak());
                ui.label(RichText::new(&p.ver.full).monospace());
                if p.installed() {
                    ui.label(RichText::new("installed").color(KINDS[0].4));
                } else {
                    ui.label(RichText::new("not installed").weak());
                }
            });
            if !m.description.is_empty() {
                ui.add_space(4.0);
                ui.label(&m.description);
            }
            if let Some(url) = m.homepage.split_whitespace().next() {
                ui.hyperlink(url);
            }
            ui.add_space(6.0);
            egui::Grid::new("meta").num_columns(2).spacing([12.0, 4.0]).show(ui, |ui| {
                let mut row = |k: &str, v: String| {
                    if !v.is_empty() {
                        ui.label(RichText::new(k).weak());
                        ui.label(v);
                        ui.end_row();
                    }
                };
                row("Slot", m.slot.clone());
                row("Repository", db.repo(id).to_string());
                row("License", m.license.clone());
                row("Size", m.size.map(human_size).unwrap_or_default());
                row("Dependencies", n_deps.to_string());
                if let Some(n) = newer {
                    row("Newer", db.pkgs[n].ver.full.clone());
                }
            });
            if m.approximate {
                ui.label(RichText::new("Read from the ebuild (repo has no metadata cache); may be incomplete.").small().weak());
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if ui.button("Explore from here").on_hover_text("Make this the root of the tree (Enter)").clicked() {
                    open_root = Some(id);
                }
                if ui.button("Copy atom").clicked() {
                    ui.ctx().copy_text(format!("={}", p.cpv));
                }
            });

            if p.installed() {
                ui.add_space(10.0);
                ui.label(RichText::new("Why is this installed?").strong());
                if db.world_ids.contains(&id) {
                    ui.label("It's in your @world set: you asked for it explicitly.");
                } else if let Some(chain) = &chain {
                    ui.label(RichText::new("Pulled in by:").weak());
                    for (i, &c) in chain.iter().enumerate() {
                        let indent = "   ".repeat(i);
                        let arrow = if i == 0 { "@world ›" } else { "└" };
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("{indent}{arrow}")).weak().monospace());
                            let name = RichText::new(db.pkgs[c].cp.as_str());
                            if c == id {
                                ui.label(name.strong());
                            } else if ui.link(name).clicked() {
                                open_root = Some(c);
                            }
                        });
                    }
                    if ui.button("Show this path in the tree").clicked() {
                        show_chain = true;
                    }
                } else {
                    ui.label(RichText::new(
                        "Nothing in @world needs it (through the dependency types shown). \
                         It may belong to @system, or be a leftover that `emerge --depclean` would remove.",
                    ).weak());
                }

                ui.add_space(10.0);
                ui.label(RichText::new(format!("Required by ({})", rdeps.len())).strong());
                if rdeps.is_empty() {
                    ui.label(RichText::new("No installed package depends on it.").weak());
                }
                for &(r, k) in &rdeps {
                    ui.horizontal(|ui| {
                        kind_chips_ui(ui, k);
                        if ui.link(db.pkgs[r].cp.as_str()).on_hover_text("Explore from this package").clicked() {
                            open_root = Some(r);
                        }
                    });
                }
            }

            if !m.use_flags.is_empty() {
                ui.add_space(10.0);
                ui.label(RichText::new("USE flags").strong());
                let enabled: std::collections::HashSet<&str> = m.use_flags.iter().map(String::as_str).collect();
                ui.horizontal_wrapped(|ui| {
                    for f in &m.iuse {
                        let f = f.trim_start_matches(['+', '-']);
                        if enabled.contains(f) {
                            ui.label(RichText::new(f).color(KINDS[0].4));
                        } else {
                            ui.label(RichText::new(format!("-{f}")).weak());
                        }
                    }
                });
            }
        });

        if show_chain {
            if let Some(chain) = chain {
                self.set_root(Root::World, chain.into_iter().map(Sel::Pkg).collect());
            }
        } else if let Some(r) = open_root {
            self.set_root(Root::Pkg(r), Vec::new());
        }
    }

    fn status_bar(&self, ui: &mut Ui) {
        let db = self.db.as_ref().unwrap();
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!(
                "{} installed · {} available · {} in @world",
                db.installed_count,
                db.available_count(),
                db.world.len()
            )).weak());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(match self.view {
                    View::Tree => "⬆⬇ move   ➡ expand   ⬅ collapse   Enter explore from here   Backspace previous view   / search",
                    View::Columns => "⬆⬇ move   ➡ open   ⬅ back   Enter explore from here   Backspace previous view   / search",
                }).weak());
            });
        });
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        if let Some(rx) = &self.loading {
            match rx.try_recv() {
                Ok(Ok(db)) => {
                    self.db = Some(db);
                    self.loading = None;
                }
                Ok(Err(e)) => {
                    self.error = Some(e);
                    self.loading = None;
                }
                Err(_) => {
                    egui::CentralPanel::default().show(ui, |ui| {
                        ui.centered_and_justified(|ui| {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                ui.label("Reading the Portage database…");
                            });
                        });
                    });
                    ui.ctx().request_repaint_after(Duration::from_millis(100));
                    return;
                }
            }
        }
        if let Some(e) = &self.error {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.centered_and_justified(|ui| ui.label(RichText::new(e).color(ui.visuals().error_fg_color)));
            });
            return;
        }

        let searching = !self.query.trim().is_empty();
        let rows = |app: &mut App| if app.view == View::Tree { app.tree_rows() } else { Vec::new() };
        let cols = self.columns();
        let tree = rows(self);
        self.handle_keys(ui.ctx(), &cols, &tree);
        let cols = self.columns();
        let tree = rows(self);

        egui::Panel::top("top").show(ui, |ui| {
            ui.add_space(4.0);
            self.top_bar(ui);
            ui.add_space(2.0);
        });
        egui::Panel::bottom("status").show(ui, |ui| self.status_bar(ui));
        egui::Panel::right("details").resizable(true).default_size(360.0).min_size(260.0).show(ui, |ui| {
            self.details(ui);
        });
        egui::CentralPanel::default().show(ui, |ui| {
            if searching {
                self.search_view(ui);
            } else {
                self.breadcrumb(ui);
                ui.add_space(2.0);
                match self.view {
                    View::Tree => self.tree_view(ui, &tree),
                    View::Columns => self.columns_view(ui, &cols),
                }
            }
        });
    }
}

// --------------------------------------------------------------------------- //
// Widgets
// --------------------------------------------------------------------------- //

/// Whether the kind and installed filters let a dependency through.
fn shown(db: &Db, d: &Dep, kinds: u8, installed_only: bool) -> bool {
    (d.kinds == 0 || d.kinds & kinds != 0) && (!installed_only || d.target.is_some_and(|t| db.pkgs[t].installed()))
}

/// Depth-first walk of the open part of the tree.
struct Walk<'a> {
    db: &'a mut Db,
    expanded: &'a HashSet<(Root, Vec<Sel>)>,
    root: Root,
    kinds: u8,
    installed_only: bool,
    /// Packages already given a row; later rows for them are `dup`.
    seen: &'a mut HashSet<Id>,
    rows: &'a mut Vec<TreeRow>,
}

impl Walk<'_> {
    fn add(&mut self, items: &[Dep], path: &mut Vec<Sel>, depth: usize) {
        for d in items {
            if !shown(self.db, d, self.kinds, self.installed_only) {
                continue;
            }
            path.push(Sel::of(d));
            let dup = d.target.is_some_and(|t| !self.seen.insert(t));
            let deps = d.target.filter(|_| !dup).map(|t| self.db.deps(t));
            let children = deps.as_ref().map_or(0, |deps| {
                deps.iter().filter(|c| shown(self.db, c, self.kinds, self.installed_only)).count()
            });
            let open = children > 0 && self.expanded.contains(&(self.root, path.clone()));
            self.rows.push(TreeRow { depth, dep: d.clone(), path: path.clone(), dup, children, open });
            if let (true, Some(deps)) = (open, deps) {
                self.add(&deps, path, depth + 1);
            }
            path.pop();
        }
    }
}

/// Draws one tree row; returns its response and whether the expand arrow was clicked.
fn tree_row(ui: &mut Ui, db: &Db, rect: egui::Rect, r: &TreeRow, selected: bool, root: Root) -> (egui::Response, bool) {
    let resp = ui.interact(rect, ui.id().with(&r.path), Sense::click());
    if !ui.is_rect_visible(rect) {
        return (resp, false);
    }
    let v = ui.visuals();
    let painter = ui.painter_at(rect);
    let inner = rect.shrink2(egui::vec2(3.0, 1.0));
    let bg = if selected {
        v.selection.bg_fill
    } else if resp.hovered() {
        v.widgets.hovered.weak_bg_fill
    } else {
        Color32::TRANSPARENT
    };
    painter.rect_filled(inner, CornerRadius::same(5), bg);

    let strong = if selected { v.selection.stroke.color } else { v.strong_text_color() };
    let weak = if selected { v.selection.stroke.color.gamma_multiply(0.75) } else { v.weak_text_color() };

    // Faint guide lines, one per level, under the parent's arrow.
    let guide = v.widgets.noninteractive.bg_stroke.color;
    for level in 0..r.depth {
        let x = rect.left() + 14.0 + level as f32 * INDENT;
        painter.vline(x, rect.y_range(), Stroke::new(1.0, guide));
    }

    // Expand arrow.
    let x0 = rect.left() + 8.0 + r.depth as f32 * INDENT;
    let mid = rect.center().y;
    let has_arrow = r.children > 0;
    let arrow_hit = egui::Rect::from_x_y_ranges(rect.left()..=x0 + 14.0, rect.y_range());
    if has_arrow {
        let c = egui::pos2(x0 + 6.0, mid);
        let hot = resp.hover_pos().is_some_and(|p| arrow_hit.contains(p));
        let color = if hot { strong } else { weak };
        let pts = if r.open {
            vec![c + egui::vec2(-4.0, -2.0), c + egui::vec2(4.0, -2.0), c + egui::vec2(0.0, 3.0)]
        } else {
            vec![c + egui::vec2(-2.0, -4.0), c + egui::vec2(3.0, 0.0), c + egui::vec2(-2.0, 4.0)]
        };
        painter.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
    }

    // One small dot in the colour of the main dependency kind.
    if let Some(&(_, _, _, _, color)) = KINDS.iter().find(|k| r.dep.kinds & k.0 != 0) {
        painter.circle_filled(egui::pos2(x0 + 20.0, mid), 3.0, color.gamma_multiply(0.85));
    }

    // Right-hand side: `⬆` for a repeat, or the child count while collapsed.
    let mut right = inner.right() - 8.0;
    let marker = if r.dup {
        Some(("⬆".to_string(), FontId::proportional(11.0)))
    } else if has_arrow && !r.open {
        Some((r.children.to_string(), FontId::proportional(11.0)))
    } else {
        None
    };
    if let Some((text, font)) = marker {
        let m = painter.text(egui::pos2(right, mid), Align2::RIGHT_CENTER, text, font, weak);
        right = m.left() - 8.0;
    }

    let installed = r.dep.target.is_some_and(|t| db.pkgs[t].installed());
    let (name, color) = match r.dep.target {
        None => (r.dep.atom.clone(), v.error_fg_color),
        Some(t) => (db.pkgs[t].name().to_string(), if installed && !r.dup { strong } else { weak }),
    };
    let left = x0 + 30.0;
    let mut job = LayoutJob::single_section(name, TextFormat {
        font_id: FontId::proportional(14.0),
        color,
        italics: !installed && r.dep.target.is_some(),
        ..Default::default()
    });
    job.wrap = TextWrapping::truncate_at_width((right - left).max(20.0));
    let galley = painter.layout_job(job);
    painter.galley(egui::pos2(left, mid - galley.size().y / 2.0), galley, color);

    let mut tip = match r.dep.target {
        Some(t) => format!("{}\n{}  ·  {}", r.dep.atom, db.pkgs[t].category(), db.pkgs[t].ver.full),
        None => format!("{}\nNo matching package", r.dep.atom),
    };
    let kinds: Vec<&str> = KINDS.iter().filter(|k| r.dep.kinds & k.0 != 0).map(|k| k.2).collect();
    if !kinds.is_empty() {
        tip += &format!("\n{} dependency", kinds.join(", "));
    }
    if !installed && r.dep.target.is_some() {
        tip += "\nNot installed";
    }
    if let (true, Some(t)) = (r.dup, r.dep.target) {
        let ancestors = &r.path[..r.path.len() - 1];
        tip += if root == Root::Pkg(t) || ancestors.contains(&Sel::Pkg(t)) {
            "\nDependency cycle: this package is above it in the tree"
        } else {
            "\nAlready shown higher up (➡ jumps there)"
        };
    }
    let toggled = has_arrow && resp.clicked() && resp.interact_pointer_pos().is_some_and(|p| arrow_hit.contains(p));
    (resp.on_hover_text(tip), toggled)
}

fn column_header(ui: &mut Ui, db: &Db, col: &Column) {
    ui.add_space(4.0);
    match col.parent {
        None => {
            ui.label(RichText::new("@world").strong());
            ui.label(RichText::new(format!("{} packages you installed", col.items.len())).small().weak());
        }
        Some(p) => {
            let pkg = &db.pkgs[p];
            ui.add(egui::Label::new(RichText::new(format!("{} needs", pkg.name())).strong()).truncate());
            ui.label(RichText::new(format!("{} dependencies", col.items.len())).small().weak());
        }
    }
}

fn dep_row(ui: &mut Ui, db: &Db, d: &Dep, selected: bool, focused: bool, cycle: bool) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), Sense::click());
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let v = ui.visuals();
    let bg = if selected && focused {
        v.selection.bg_fill
    } else if selected {
        v.selection.bg_fill.gamma_multiply(0.45)
    } else if resp.hovered() {
        v.widgets.hovered.weak_bg_fill
    } else {
        Color32::TRANSPARENT
    };
    let painter = ui.painter_at(rect);
    let inner = rect.shrink2(egui::vec2(3.0, 1.0));
    painter.rect_filled(inner, CornerRadius::same(6), bg);

    let strong = if selected && focused { v.selection.stroke.color } else { v.strong_text_color() };
    let weak = if selected && focused { v.selection.stroke.color.gamma_multiply(0.75) } else { v.weak_text_color() };

    let (name, sub, installed) = match d.target {
        Some(t) => {
            let p = &db.pkgs[t];
            (p.name().to_string(), format!("{}  ·  {}", p.category(), p.ver.full), p.installed())
        }
        None => (d.atom.clone(), "no matching package".to_string(), false),
    };
    let name_color = if d.target.is_none() {
        v.error_fg_color
    } else if installed {
        strong
    } else {
        weak
    };

    // Right-hand side: dependency kind chips, then a chevron / cycle marker.
    let mut x = inner.right() - 8.0;
    let mid = inner.center().y;
    let marker = if cycle {
        Some(("cycle", weak))
    } else if d.target.is_some() {
        Some(("›", weak))
    } else {
        None
    };
    if let Some((text, color)) = marker {
        let size = if text == "›" { 18.0 } else { 10.0 };
        let r = painter.text(egui::pos2(x, mid), Align2::RIGHT_CENTER, text, FontId::proportional(size), color);
        x = r.left() - 6.0;
    }
    for &(bit, letter, _, _, color) in KINDS.iter().rev() {
        if d.kinds & bit == 0 {
            continue;
        }
        let chip = egui::Rect::from_center_size(egui::pos2(x - 8.0, mid), egui::vec2(16.0, 16.0));
        painter.rect(chip, CornerRadius::same(4), color.gamma_multiply(0.2), Stroke::new(1.0, color), egui::StrokeKind::Inside);
        painter.text(chip.center(), Align2::CENTER_CENTER, letter, FontId::monospace(10.0), color);
        x = chip.left() - 3.0;
    }

    let text_w = (x - inner.left() - 14.0).max(20.0);
    let line = |text: String, size: f32, color: Color32, italic: bool| {
        let mut job = LayoutJob::single_section(text, TextFormat { font_id: FontId::proportional(size), color, italics: italic, ..Default::default() });
        job.wrap = TextWrapping::truncate_at_width(text_w);
        painter.layout_job(job)
    };
    let g1 = line(name, 14.0, name_color, !installed && d.target.is_some());
    let g2 = line(sub, 11.0, weak, false);
    let left = inner.left() + 10.0;
    painter.galley(egui::pos2(left, inner.top() + 4.0), g1, name_color);
    painter.galley(egui::pos2(left, inner.bottom() - 17.0), g2, weak);

    let tip = if cycle {
        format!("{}\nAlready open to the left (dependency cycle)", d.atom)
    } else if !installed && d.target.is_some() {
        format!("{}\nNot installed", d.atom)
    } else {
        d.atom.clone()
    };
    resp.on_hover_text(tip)
}

fn kind_chips_ui(ui: &mut Ui, kinds: u8) {
    for &(bit, letter, name, _, color) in &KINDS {
        if kinds & bit != 0 {
            ui.label(RichText::new(letter).monospace().color(color)).on_hover_text(name);
        }
    }
}

fn legend(ui: &mut Ui, view: View) {
    ui.label(RichText::new("Legend").strong());
    for &(_, letter, name, var, color) in &KINDS {
        ui.horizontal(|ui| {
            ui.label(RichText::new(letter).monospace().color(color));
            ui.label(format!("{name} dependency ({var})"));
        });
    }
    ui.horizontal(|ui| {
        ui.label(RichText::new("pkg").italics().weak());
        ui.label("not installed");
    });
    ui.horizontal(|ui| {
        ui.label(RichText::new("pkg").color(ui.visuals().error_fg_color));
        ui.label("no package matches");
    });
    match view {
        View::Tree => {
            ui.horizontal(|ui| {
                ui.label(RichText::new("⬆").small().weak());
                ui.label("already shown higher up");
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new("12").small().weak());
                ui.label("dependencies inside, when collapsed");
            });
        }
        View::Columns => {
            ui.horizontal(|ui| {
                ui.label(RichText::new("cycle").small().weak());
                ui.label("already open to the left");
            });
        }
    }
}

fn human_size(b: u64) -> String {
    let units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = b as f64;
    let mut u = 0;
    while v >= 1024.0 && u < units.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 { format!("{b} B") } else { format!("{v:.1} {}", units[u]) }
}
