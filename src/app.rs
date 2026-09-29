//! The egui front end: a column browser (each column lists the dependencies
//! of the package selected in the column to its left) plus a details panel.

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

/// (bit, letter, short name, portage variable, colour)
const KINDS: [(u8, &str, &str, &str, Color32); 5] = [
    (RDEP, "R", "runtime", "RDEPEND", Color32::from_rgb(0x4c, 0xaf, 0x50)),
    (DEP, "D", "build", "DEPEND", Color32::from_rgb(0x42, 0xa5, 0xf5)),
    (BDEP, "B", "build tool", "BDEPEND", Color32::from_rgb(0xff, 0xa7, 0x26)),
    (PDEP, "P", "post", "PDEPEND", Color32::from_rgb(0xab, 0x47, 0xbc)),
    (IDEP, "I", "install", "IDEPEND", Color32::from_rgb(0x26, 0xa6, 0x9a)),
];

#[derive(Clone, PartialEq)]
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

#[derive(Clone, Copy, PartialEq)]
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

pub struct App {
    loading: Option<Receiver<Result<Db, String>>>,
    db: Option<Db>,
    error: Option<String>,

    root: Root,
    /// `path[i]` is the selection in column `i`.
    path: Vec<Sel>,
    /// The column the keyboard is acting on.
    focus: usize,
    history: Vec<(Root, Vec<Sel>, usize)>,

    kinds: u8,
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
            root: Root::World,
            path: Vec::new(),
            focus: 0,
            history: Vec::new(),
            kinds: RDEP | DEP | BDEP | PDEP | IDEP,
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
            items
                .iter()
                .filter(|d| {
                    (d.kinds == 0 || d.kinds & kinds != 0)
                        && (!installed_only || d.target.is_some_and(|t| db.pkgs[t].installed()))
                })
                .cloned()
                .collect()
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

    // ------------------------------------------------------------------ //
    // Keyboard
    // ------------------------------------------------------------------ //

    fn handle_keys(&mut self, ctx: &egui::Context, cols: &[Column]) {
        if ctx.egui_wants_keyboard_input() {
            if ctx.input(|i| i.key_pressed(Key::Escape)) {
                self.query.clear();
                ctx.memory_mut(|m| m.surrender_focus(egui::Id::new("search")));
            }
            return;
        }
        let pressed = |k: Key| ctx.input(|i| i.key_pressed(k));
        let col = &cols[self.focus];
        let cur = self.path.get(self.focus).and_then(|s| col.items.iter().position(|d| Sel::of(d) == *s));

        let step = |delta: isize| -> Option<usize> {
            if col.items.is_empty() {
                return None;
            }
            let n = col.items.len() as isize;
            Some(match cur {
                Some(c) => (c as isize + delta).clamp(0, n - 1) as usize,
                None => 0,
            })
        };
        let mv = if pressed(Key::ArrowDown) || pressed(Key::J) {
            step(1)
        } else if pressed(Key::ArrowUp) || pressed(Key::K) {
            step(-1)
        } else if pressed(Key::PageDown) {
            step(15)
        } else if pressed(Key::PageUp) {
            step(-15)
        } else if pressed(Key::Home) || pressed(Key::G) && !ctx.input(|i| i.modifiers.shift) {
            step(isize::MIN / 2)
        } else if pressed(Key::End) || pressed(Key::G) {
            step(isize::MAX / 2)
        } else {
            None
        };
        if let Some(i) = mv {
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
            ui.label(RichText::new(
                "Select a package to see what it depends on. Each column to the right \
                 lists the dependencies of the package selected to its left.",
            ).weak());
            ui.add_space(8.0);
            legend(ui);
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
                ui.label(RichText::new(
                    "↑↓ move   → open   ← back   Enter explore from here   Backspace previous view   / search",
                ).weak());
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
        let cols = self.columns();
        self.handle_keys(ui.ctx(), &cols);
        let cols = self.columns();

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
                self.columns_view(ui, &cols);
            }
        });
    }
}

// --------------------------------------------------------------------------- //
// Widgets
// --------------------------------------------------------------------------- //

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

fn legend(ui: &mut Ui) {
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
    ui.horizontal(|ui| {
        ui.label(RichText::new("cycle").small().weak());
        ui.label("already open to the left");
    });
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
