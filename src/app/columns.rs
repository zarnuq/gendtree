//! Column traversal, keyboard navigation, and package rows.

use std::sync::Arc;

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Key, Layout, RichText, Sense, Stroke,
    TextFormat, Ui, text::LayoutJob, text::TextWrapping,
};

use crate::db::{Db, Dep, Id};

use super::{App, KINDS, Root, Sel, shown};

const COL_W: f32 = 280.0;
const ROW_H: f32 = 40.0;
pub(super) struct Column {
    pub(super) parent: Option<Id>,
    items: Vec<Dep>,
    /// Root plus everything selected to the left; these would be cycles.
    pub(super) ancestors: Vec<Id>,
}

impl App {
    /// Build the columns for the current root and path, trimming any part of
    /// the path that is no longer visible (e.g. after changing a filter).
    pub(super) fn columns(&mut self) -> Vec<Column> {
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
                .filter(|d| shown(db, d, kinds, installed_only))
                .cloned()
                .collect()
        };
        let mut cols = vec![Column {
            parent,
            items: filter(db, &first),
            ancestors: ancestors.clone(),
        }];
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
            cols.push(Column {
                parent: Some(id),
                items: filter(db, &deps),
                ancestors: ancestors.clone(),
            });
        }
        self.path.truncate(keep);
        self.focus = self.focus.min(cols.len() - 1);
        cols
    }

    pub(super) fn column_keys(&mut self, ctx: &egui::Context, cols: &[Column]) {
        let pressed = |k: Key| ctx.input(|i| i.key_pressed(k));
        let col = &cols[self.focus];
        let cur = self
            .path
            .get(self.focus)
            .and_then(|s| col.items.iter().position(|d| Sel::of(d) == *s));
        if let Some(i) = Self::step_key(ctx, cur, col.items.len()) {
            let sel = Sel::of(&col.items[i]);
            self.path.truncate(self.focus);
            self.path.push(sel);
            self.scroll_to_sel = true;
        }

        if (pressed(Key::ArrowRight) || pressed(Key::L))
            && let Some(next) = cols.get(self.focus + 1)
            && let Some(first) = next.items.first()
        {
            self.focus += 1;
            if self.path.len() <= self.focus {
                self.path.push(Sel::of(first));
            }
            self.scroll_to_sel = true;
            self.reveal_col = Some(self.focus + 1);
        }
        if (pressed(Key::ArrowLeft) || pressed(Key::H)) && self.focus > 0 {
            self.focus -= 1;
            self.path.truncate(self.focus + 1);
            self.scroll_to_sel = true;
        }
    }

    pub(super) fn columns_view(&mut self, ui: &mut Ui, cols: &[Column]) {
        let db = self.db.as_ref().unwrap();
        let mut action: Option<(usize, Sel, bool)> = None;
        let scroll_to_sel = std::mem::take(&mut self.scroll_to_sel);
        let reveal = self.reveal_col.take();
        let height = ui.available_height();

        egui::ScrollArea::horizontal()
            .id_salt("columns")
            .auto_shrink(false)
            .show(ui, |ui| {
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
                                            let cycle = d
                                                .target
                                                .is_some_and(|t| col.ancestors.contains(&t));
                                            let resp =
                                                dep_row(ui, db, d, is_sel, ci == self.focus, cycle);
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
}

fn column_header(ui: &mut Ui, db: &Db, col: &Column) {
    ui.add_space(4.0);
    match col.parent {
        None => {
            ui.label(RichText::new("@world").strong());
            ui.label(
                RichText::new(format!("{} packages you installed", col.items.len()))
                    .small()
                    .weak(),
            );
        }
        Some(p) => {
            let pkg = &db.pkgs[p];
            ui.add(
                egui::Label::new(RichText::new(format!("{} needs", pkg.name())).strong())
                    .truncate(),
            );
            ui.label(
                RichText::new(format!("{} dependencies", col.items.len()))
                    .small()
                    .weak(),
            );
        }
    }
}

fn dep_row(
    ui: &mut Ui,
    db: &Db,
    d: &Dep,
    selected: bool,
    focused: bool,
    cycle: bool,
) -> egui::Response {
    let (rect, resp) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), Sense::click());
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

    let strong = if selected && focused {
        v.selection.stroke.color
    } else {
        v.strong_text_color()
    };
    let weak = if selected && focused {
        v.selection.stroke.color.gamma_multiply(0.75)
    } else {
        v.weak_text_color()
    };

    let (name, sub, installed) = match d.target {
        Some(t) => {
            let p = &db.pkgs[t];
            (
                p.name().to_string(),
                format!("{}  ·  {}", p.category(), p.ver.full),
                p.installed(),
            )
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
        let r = painter.text(
            egui::pos2(x, mid),
            Align2::RIGHT_CENTER,
            text,
            FontId::proportional(size),
            color,
        );
        x = r.left() - 6.0;
    }
    for &(bit, letter, _, _, color) in KINDS.iter().rev() {
        if d.kinds & bit == 0 {
            continue;
        }
        let chip = egui::Rect::from_center_size(egui::pos2(x - 8.0, mid), egui::vec2(16.0, 16.0));
        painter.rect(
            chip,
            CornerRadius::same(4),
            color.gamma_multiply(0.2),
            Stroke::new(1.0, color),
            egui::StrokeKind::Inside,
        );
        painter.text(
            chip.center(),
            Align2::CENTER_CENTER,
            letter,
            FontId::monospace(10.0),
            color,
        );
        x = chip.left() - 3.0;
    }

    let text_w = (x - inner.left() - 14.0).max(20.0);
    let line = |text: String, size: f32, color: Color32, italic: bool| {
        let mut job = LayoutJob::single_section(
            text,
            TextFormat {
                font_id: FontId::proportional(size),
                color,
                italics: italic,
                ..Default::default()
            },
        );
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
