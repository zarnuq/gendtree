//! The egui front end: a dependency browser with two views, a tree of package
//! nodes joined by lines, and columns (each column lists the dependencies of the package
//! selected in the column to its left), plus a details panel.

mod columns;
mod panels;
mod tree;

#[cfg(test)]
mod tests;

use columns::Column;
use tree::TreeRow;

use std::collections::HashSet;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use eframe::egui::{self, Color32, Key, RichText, Ui};

use crate::db::{ALL_KINDS, BDEP, DEP, Db, Dep, IDEP, Id, PDEP, RDEP};

/// (bit, letter, short name, portage variable, colour)
const KINDS: [(u8, &str, &str, &str, Color32); 5] = [
    (
        RDEP,
        "R",
        "runtime",
        "RDEPEND",
        Color32::from_rgb(0x4c, 0xaf, 0x50),
    ),
    (
        DEP,
        "D",
        "build",
        "DEPEND",
        Color32::from_rgb(0x42, 0xa5, 0xf5),
    ),
    (
        BDEP,
        "B",
        "build tool",
        "BDEPEND",
        Color32::from_rgb(0xff, 0xa7, 0x26),
    ),
    (
        PDEP,
        "P",
        "post",
        "PDEPEND",
        Color32::from_rgb(0xab, 0x47, 0xbc),
    ),
    (
        IDEP,
        "I",
        "install",
        "IDEPEND",
        Color32::from_rgb(0x26, 0xa6, 0x9a),
    ),
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

#[derive(Clone, Copy, PartialEq)]
enum View {
    Tree,
    Columns,
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
        self.history
            .push((self.root, std::mem::take(&mut self.path), self.focus));
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
        if pressed(Key::Enter)
            && let Some(Sel::Pkg(id)) = self.path.get(self.focus).cloned()
        {
            self.set_root(Root::Pkg(id), Vec::new());
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
                ui.centered_and_justified(|ui| {
                    ui.label(RichText::new(e).color(ui.visuals().error_fg_color))
                });
            });
            return;
        }

        let searching = !self.query.trim().is_empty();
        let rows = |app: &mut App| {
            if app.view == View::Tree {
                app.tree_rows()
            } else {
                Vec::new()
            }
        };
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
        egui::Panel::right("details")
            .resizable(true)
            .default_size(360.0)
            .min_size(260.0)
            .show(ui, |ui| {
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

/// Whether the kind and installed filters let a dependency through.
fn shown(db: &Db, d: &Dep, kinds: u8, installed_only: bool) -> bool {
    (d.kinds == 0 || d.kinds & kinds != 0)
        && (!installed_only || d.target.is_some_and(|t| db.pkgs[t].installed()))
}
