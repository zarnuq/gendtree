//! Search, navigation controls, and package details surrounding the views.

use eframe::egui::{self, Align, Key, Layout, RichText, Ui};

use crate::db::{ALL_KINDS, Id};

use super::{App, KINDS, Root, Sel, View};

impl App {
    pub(super) fn top_bar(&mut self, ui: &mut Ui) {
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
            if ui
                .add(egui::Button::new("@world").selected(self.root == Root::World))
                .clicked()
                && self.root != Root::World
            {
                self.set_root(Root::World, Vec::new());
            }
            ui.separator();
            let mut view = self.view;
            ui.selectable_value(&mut view, View::Tree, "Tree")
                .on_hover_text("Packages as boxes, joined to what they depend on");
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
            if search.lost_focus()
                && ui.input(|i| i.key_pressed(Key::Enter))
                && let Some(cp) = self.results.first().cloned()
                && let Some(id) = self.db.as_ref().unwrap().best_for_cp(&cp)
            {
                self.set_root(Root::Pkg(id), Vec::new());
            }

            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.checkbox(&mut self.installed_only, "Installed only");
                ui.separator();
                for &(bit, letter, name, var, color) in KINDS.iter().rev() {
                    let mut on = self.kinds & bit != 0;
                    let text = RichText::new(format!("{letter}  {name}")).color(color);
                    if ui
                        .toggle_value(&mut on, text)
                        .on_hover_text(format!("Show {var} dependencies"))
                        .changed()
                    {
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

    pub(super) fn breadcrumb(&mut self, ui: &mut Ui) {
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
                let text = if i == self.focus {
                    RichText::new(text).strong()
                } else {
                    RichText::new(text)
                };
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

    pub(super) fn search_view(&mut self, ui: &mut Ui) {
        let db = self.db.as_ref().unwrap();
        if self.results_for != self.query {
            self.results = db.search(&self.query, 300);
            self.results_for = self.query.clone();
        }
        ui.label(
            RichText::new(format!(
                "{} matches — click one to explore its dependencies",
                self.results.len()
            ))
            .weak(),
        );
        ui.separator();
        let mut pick = None;
        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show(ui, |ui| {
                for cp in &self.results {
                    let Some(id) = db.best_for_cp(cp) else {
                        continue;
                    };
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

    pub(super) fn details(&mut self, ui: &mut Ui) {
        let Some(sel) = self.current() else {
            let db = self.db.as_ref().unwrap();
            ui.add_space(8.0);
            ui.heading("@world");
            ui.label(format!(
                "{} packages you installed explicitly.",
                db.world.len()
            ));
            ui.add_space(8.0);
            ui.label(RichText::new(match self.view {
                View::Tree => "Click a package to open its dependencies out to the right, and click it \
                               again to close them. Every package opens on its own, however often it appears.",
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
                ui.label(
                    RichText::new("No installed or available package matches this atom.").weak(),
                );
                return;
            }
        };

        // Collect everything first so `self` is free for the buttons below.
        let db = self.db.as_mut().unwrap();
        let n_deps = db.deps(id).len();
        let db = self.db.as_ref().unwrap();
        let p = &db.pkgs[id];
        let m = db.meta(id);
        let rdeps: Vec<(Id, u8)> = db
            .rdeps(id)
            .iter()
            .copied()
            .filter(|(_, k)| k & self.kinds != 0)
            .collect();
        let chain = if p.installed() && !db.world_ids.contains(&id) {
            db.why_installed(id, self.kinds)
        } else {
            None
        };
        let newer = db
            .newest_available(id)
            .filter(|&n| p.installed() && db.pkgs[n].ver > p.ver);

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

    pub(super) fn status_bar(&self, ui: &mut Ui) {
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
                    View::Tree => "⬆⬇ move   ➡ expand   ⬅ collapse   drag to pan   Enter explore from here   Backspace previous view   / search",
                    View::Columns => "⬆⬇ move   ➡ open   ⬅ back   Enter explore from here   Backspace previous view   / search",
                }).weak());
            });
        });
    }
}

fn kind_chips_ui(ui: &mut Ui, kinds: u8) {
    for &(bit, letter, name, _, color) in &KINDS {
        if kinds & bit != 0 {
            ui.label(RichText::new(letter).monospace().color(color))
                .on_hover_text(name);
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
            ui.label(
                RichText::new("The coloured strip on a package is its dependency type.").weak(),
            );
            ui.horizontal(|ui| {
                ui.label(RichText::new("12").small().weak());
                ui.label("dependencies it has; click to show them");
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new("⬆").small().weak());
                ui.label("outline: a cycle, already further up the branch");
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
    if u == 0 {
        format!("{b} B")
    } else {
        format!("{v:.1} {}", units[u])
    }
}
