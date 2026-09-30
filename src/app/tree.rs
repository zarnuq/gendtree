//! Tree traversal, navigation, layout, and package-node rendering.

use std::collections::HashSet;

use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Key, Sense, Stroke, TextFormat, Ui,
    scroll_area::{DragScroll, ScrollSource},
    text::{LayoutJob, TextWrapping},
};

use crate::db::{Db, Dep};

use super::{App, KINDS, Root, Sel, shown};

const NODE_W: f32 = 210.0;
const NODE_H: f32 = 42.0;
/// Horizontal room between levels, for the edges.
const GAP_X: f32 = 64.0;
const GAP_Y: f32 = 10.0;
const TREE_PAD: f32 = 20.0;

/// One visible line of the tree view.
pub(super) struct TreeRow {
    depth: usize,
    dep: Dep,
    /// Selections from the root down to and including this row.
    pub(super) path: Vec<Sel>,
    /// The package is its own ancestor on this branch (a dependency cycle),
    /// so it isn't expanded again here.
    pub(super) cycle: bool,
    /// Number of dependencies shown under the current filters.
    pub(super) children: usize,
    pub(super) open: bool,
}

impl App {
    /// Flatten the open part of the tree into rows, and trim the selection to
    /// the deepest row that is still visible.
    pub(super) fn tree_rows(&mut self) -> Vec<TreeRow> {
        let (kinds, installed_only, root) = (self.kinds, self.installed_only, self.root);
        let db = self.db.as_mut().unwrap();
        let top = match root {
            Root::World => db.world.clone(),
            Root::Pkg(id) => db.deps(id),
        };
        let mut rows = Vec::new();
        let mut walk = Walk {
            db,
            expanded: &self.expanded,
            root,
            kinds,
            installed_only,
            rows: &mut rows,
        };
        walk.add(&top, &mut Vec::new(), 0);

        while !self.path.is_empty() && !rows.iter().any(|r| r.path == self.path) {
            self.path.pop();
        }
        self.focus = self.path.len().saturating_sub(1);
        rows
    }

    pub(super) fn tree_keys(&mut self, ctx: &egui::Context, rows: &[TreeRow]) {
        let pressed = |k: Key| ctx.input(|i| i.key_pressed(k));
        let cur = rows.iter().position(|r| r.path == self.path);
        let mut goto = Self::step_key(ctx, cur, rows.len());

        if pressed(Key::ArrowRight) || pressed(Key::L) {
            match cur.map(|c| (c, &rows[c])) {
                // Jump to the copy further up the branch.
                Some((_, r)) if r.cycle => match cycle_origin(rows, r) {
                    Some(j) => goto = Some(j),
                    None => {
                        self.path.clear();
                        self.scroll_to_sel = true;
                    }
                },
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
                // From a top-level node this goes to the root.
                _ if !self.path.is_empty() => {
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

    pub(super) fn tree_view(&mut self, ui: &mut Ui, rows: &[TreeRow]) {
        let db = self.db.as_ref().unwrap();
        let scroll_to_sel = std::mem::take(&mut self.scroll_to_sel);
        let sel = rows.iter().position(|r| r.path == self.path);
        let lay = TreeLayout::new(rows);
        // Rows from the root down to the selection; their edges are highlighted.
        let mut on_path = HashSet::new();
        let mut cur = sel;
        while let Some(i) = cur {
            on_path.insert(i);
            cur = lay.parent[i];
        }

        let mut toggle: Option<usize> = None;
        let mut jump: Option<usize> = None;
        // `None` is the root node.
        let mut pick: Option<(Option<usize>, bool)> = None;

        egui::ScrollArea::both()
            .id_salt(("tree", self.root))
            .auto_shrink(false)
            .scroll_source(ScrollSource {
                drag: DragScroll::Always,
                ..Default::default()
            })
            .show_viewport(ui, |ui, view| {
                ui.set_min_size(lay.size);
                let origin = ui.max_rect().min.to_vec2();
                let visible = view.expand(40.0);
                let v = ui.visuals().clone();
                let painter = ui.painter().clone();

                // Edges first, so nodes sit on top. Each parent gets one comb:
                // a stub out of it, a vertical trunk, and a stub into each child.
                let quiet = Stroke::new(1.0, v.widgets.noninteractive.bg_stroke.color);
                let hot = Stroke::new(2.5, v.selection.bg_fill);
                let mut kids: Vec<Vec<usize>> = vec![Vec::new(); rows.len() + 1];
                for (i, p) in lay.parent.iter().enumerate() {
                    kids[p.unwrap_or(rows.len())].push(i);
                }
                let parent_rect = |p: usize| {
                    if p == rows.len() {
                        lay.root
                    } else {
                        lay.nodes[p]
                    }
                };
                let trunk_x = |from: egui::Rect| from.right() + GAP_X / 2.0;
                for (p, kids) in kids.iter().enumerate() {
                    let (Some(&first), Some(&last)) = (kids.first(), kids.last()) else {
                        continue;
                    };
                    let from = parent_rect(p);
                    let x = trunk_x(from);
                    let (top, bottom) = (
                        lay.nodes[first].center().y.min(from.center().y),
                        lay.nodes[last].center().y.max(from.center().y),
                    );
                    let span =
                        egui::Rect::from_x_y_ranges(from.right()..=x + GAP_X / 2.0, top..=bottom);
                    if !visible.intersects(span) {
                        continue;
                    }
                    painter.hline(
                        from.right() + origin.x..=x + origin.x,
                        from.center().y + origin.y,
                        quiet,
                    );
                    painter.vline(x + origin.x, top + origin.y..=bottom + origin.y, quiet);
                    for &c in kids {
                        let y = lay.nodes[c].center().y;
                        if (visible.top()..=visible.bottom()).contains(&y) {
                            painter.hline(
                                x + origin.x..=lay.nodes[c].left() + origin.x,
                                y + origin.y,
                                quiet,
                            );
                        }
                    }
                }
                // The way from the root to the selection, on top.
                for &i in &on_path {
                    let from = lay.parent[i].map_or(lay.root, |p| lay.nodes[p]);
                    let (a, b) = (from.right_center(), lay.nodes[i].left_center());
                    let x = trunk_x(from);
                    let pts = vec![a, egui::pos2(x, a.y), egui::pos2(x, b.y), b];
                    painter.add(egui::Shape::line(
                        pts.into_iter().map(|p| p + origin).collect(),
                        hot,
                    ));
                }

                if root_node(ui, db, lay.root.translate(origin), self.root, sel.is_none()).clicked()
                {
                    pick = Some((None, false));
                }
                if rows.is_empty() {
                    painter.text(
                        lay.root.right_center() + origin + egui::vec2(24.0, 0.0),
                        Align2::LEFT_CENTER,
                        "No dependencies (with the current filters)",
                        FontId::proportional(13.0),
                        v.weak_text_color(),
                    );
                }
                for (i, r) in rows.iter().enumerate() {
                    let rect = lay.nodes[i];
                    if !visible.intersects(rect) {
                        continue;
                    }
                    let (resp, knob) = tree_node(ui, db, rect.translate(origin), r, sel == Some(i));
                    if knob {
                        if r.cycle {
                            jump = Some(i)
                        } else {
                            toggle = Some(i)
                        }
                    } else if resp.double_clicked() {
                        pick = Some((Some(i), true));
                    } else if resp.clicked() {
                        // Clicking a package selects it and opens or closes it.
                        pick = Some((Some(i), false));
                        if r.children > 0 {
                            toggle = Some(i);
                        }
                    }
                }
                if scroll_to_sel {
                    let r = sel.map_or(lay.root, |i| lay.nodes[i]);
                    ui.scroll_to_rect(r.translate(origin).expand(24.0), None);
                }
            });

        if let Some(i) = toggle {
            let key = (self.root, rows[i].path.clone());
            if !self.expanded.remove(&key) {
                self.expanded.insert(key);
            }
        }
        if let Some(i) = jump {
            match cycle_origin(rows, &rows[i]) {
                Some(j) => self.path = rows[j].path.clone(),
                None => self.path.clear(),
            }
            self.focus = self.path.len().saturating_sub(1);
            self.scroll_to_sel = true;
        }
        match pick {
            Some((Some(i), true)) if rows[i].dep.target.is_some() => {
                self.set_root(Root::Pkg(rows[i].dep.target.unwrap()), Vec::new());
            }
            Some((Some(i), _)) => {
                self.path = rows[i].path.clone();
                self.focus = self.path.len() - 1;
            }
            Some((None, _)) => {
                self.path.clear();
                self.focus = 0;
            }
            None => {}
        }
    }
}

/// Depth-first walk of the open part of the tree.
struct Walk<'a> {
    db: &'a mut Db,
    expanded: &'a HashSet<(Root, Vec<Sel>)>,
    root: Root,
    kinds: u8,
    installed_only: bool,
    rows: &'a mut Vec<TreeRow>,
}

impl Walk<'_> {
    fn add(&mut self, items: &[Dep], path: &mut Vec<Sel>, depth: usize) {
        for d in items {
            if !shown(self.db, d, self.kinds, self.installed_only) {
                continue;
            }
            path.push(Sel::of(d));
            // Every occurrence is its own subtree; only a package that is
            // already one of its own ancestors stops, or the tree would be infinite.
            let cycle = d.target.is_some_and(|t| {
                self.root == Root::Pkg(t) || path[..path.len() - 1].contains(&Sel::Pkg(t))
            });
            let deps = d.target.filter(|_| !cycle).map(|t| self.db.deps(t));
            let children = deps.as_ref().map_or(0, |deps| {
                deps.iter()
                    .filter(|c| shown(self.db, c, self.kinds, self.installed_only))
                    .count()
            });
            let open = children > 0 && self.expanded.contains(&(self.root, path.clone()));
            self.rows.push(TreeRow {
                depth,
                dep: d.clone(),
                path: path.clone(),
                cycle,
                children,
                open,
            });
            if let (true, Some(deps)) = (open, deps) {
                self.add(&deps, path, depth + 1);
            }
            path.pop();
        }
    }
}

/// The row a cycle leads back to: the same package further up the branch, or
/// `None` when that is the root.
fn cycle_origin(rows: &[TreeRow], r: &TreeRow) -> Option<usize> {
    let t = Sel::Pkg(r.dep.target?);
    let k = r.path[..r.path.len() - 1].iter().position(|s| *s == t)?;
    rows.iter().position(|x| x.path == r.path[..=k])
}

/// Where each node of the tree view goes: the root on the left, each level of
/// dependencies one column further right. Every collapsed node gets its own
/// row, and an open node sits beside its children (centred on them, but never
/// far below the first, so a big fan-out doesn't push it off screen). Each
/// subtree keeps to its own rows, so they never overlap.
struct TreeLayout {
    root: egui::Rect,
    nodes: Vec<egui::Rect>,
    /// The row each row hangs from; `None` for the root's children.
    parent: Vec<Option<usize>>,
    size: egui::Vec2,
}

impl TreeLayout {
    fn new(rows: &[TreeRow]) -> TreeLayout {
        let n = rows.len();
        let mut parent = vec![None; n];
        let mut stack: Vec<usize> = Vec::new();
        for (i, r) in rows.iter().enumerate() {
            stack.truncate(r.depth);
            parent[i] = stack.last().copied();
            stack.push(i);
        }

        let mut slot = vec![0.0f32; n];
        let mut leaves = 0.0;
        for (i, r) in rows.iter().enumerate() {
            if !r.open {
                slot[i] = leaves;
                leaves += 1.0;
            }
        }
        // Children come after their parent, so walking backwards settles every
        // child before the parent that is centred on them.
        let mut span: Vec<Option<(f32, f32)>> = vec![None; n];
        let mut root_span: Option<(f32, f32)> = None;
        let beside = |(lo, hi): (f32, f32)| ((lo + hi) / 2.0).min(lo + 2.0);
        for i in (0..n).rev() {
            if let (true, Some(s)) = (rows[i].open, span[i]) {
                slot[i] = beside(s);
            }
            let s = match parent[i] {
                Some(p) => &mut span[p],
                None => &mut root_span,
            };
            *s = Some(s.map_or((slot[i], slot[i]), |(lo, hi)| {
                (lo.min(slot[i]), hi.max(slot[i]))
            }));
        }

        let at = |col: usize, slot: f32| {
            egui::Rect::from_min_size(
                egui::pos2(
                    TREE_PAD + col as f32 * (NODE_W + GAP_X),
                    TREE_PAD + slot * (NODE_H + GAP_Y),
                ),
                egui::vec2(NODE_W, NODE_H),
            )
        };
        let root = at(0, root_span.map_or(0.0, beside));
        let nodes: Vec<egui::Rect> = rows
            .iter()
            .zip(&slot)
            .map(|(r, &s)| at(r.depth + 1, s))
            .collect();
        let right = nodes
            .iter()
            .fold(root.right() + if n == 0 { 320.0 } else { 0.0 }, |m, r| {
                m.max(r.right())
            });
        let bottom = nodes.iter().fold(root.bottom(), |m, r| m.max(r.bottom()));
        TreeLayout {
            root,
            nodes,
            parent,
            size: egui::vec2(right + TREE_PAD + 12.0, bottom + TREE_PAD),
        }
    }
}

/// A node's box: a coloured strip on the left, the name, and a quieter line
/// under it.
struct Card<'a> {
    fill: Color32,
    stroke: Stroke,
    accent: Option<Color32>,
    name: &'a str,
    name_color: Color32,
    italics: bool,
    sub: &'a str,
    sub_color: Color32,
}

impl Card<'_> {
    fn paint(&self, painter: &egui::Painter, rect: egui::Rect) {
        painter.rect(
            rect,
            CornerRadius::same(7),
            self.fill,
            self.stroke,
            egui::StrokeKind::Inside,
        );
        if let Some(c) = self.accent {
            let strip = egui::Rect::from_min_size(rect.min, egui::vec2(4.0, rect.height()));
            painter.rect_filled(
                strip,
                CornerRadius {
                    nw: 7,
                    sw: 7,
                    ne: 0,
                    se: 0,
                },
                c,
            );
        }
        let width = rect.width() - 28.0;
        let line = |text: &str, size: f32, color: Color32, italics: bool| {
            let mut job = LayoutJob::single_section(
                text.to_string(),
                TextFormat {
                    font_id: FontId::proportional(size),
                    color,
                    italics,
                    ..Default::default()
                },
            );
            job.wrap = TextWrapping::truncate_at_width(width);
            painter.layout_job(job)
        };
        let left = rect.left() + 12.0;
        painter.galley(
            egui::pos2(left, rect.top() + 5.0),
            line(self.name, 14.0, self.name_color, self.italics),
            self.name_color,
        );
        painter.galley(
            egui::pos2(left, rect.bottom() - 18.0),
            line(self.sub, 11.0, self.sub_color, false),
            self.sub_color,
        );
    }
}

/// Fill, border and text colours for a node in the given state.
fn node_colors(ui: &Ui, selected: bool, hovered: bool) -> (Color32, Stroke, Color32, Color32) {
    let v = ui.visuals();
    if selected {
        let text = v.selection.stroke.color;
        (
            v.selection.bg_fill,
            Stroke::new(1.0, v.selection.bg_fill),
            text,
            text.gamma_multiply(0.75),
        )
    } else {
        let stroke = if hovered {
            v.widgets.hovered.bg_stroke
        } else {
            v.widgets.noninteractive.bg_stroke
        };
        (
            v.faint_bg_color,
            stroke,
            v.strong_text_color(),
            v.weak_text_color(),
        )
    }
}

fn root_node(ui: &mut Ui, db: &Db, rect: egui::Rect, root: Root, selected: bool) -> egui::Response {
    let resp = ui.interact(rect, ui.id().with("root-node"), Sense::click());
    if !ui.is_rect_visible(rect) {
        return resp;
    }
    let (fill, mut stroke, strong, weak) = node_colors(ui, selected, resp.hovered());
    stroke.width = 2.0;
    let (name, sub) = match root {
        Root::World => (
            "@world".to_string(),
            format!("{} packages you installed", db.world.len()),
        ),
        Root::Pkg(id) => {
            let p = &db.pkgs[id];
            (
                p.name().to_string(),
                format!("{}  ·  {}", p.category(), p.ver.full),
            )
        }
    };
    let card = Card {
        fill,
        stroke,
        accent: None,
        name: &name,
        name_color: strong,
        italics: false,
        sub: &sub,
        sub_color: weak,
    };
    card.paint(ui.painter(), rect);
    match root {
        Root::World => resp,
        Root::Pkg(id) => resp.on_hover_text(&db.pkgs[id].cpv),
    }
}

/// Draws one package node; returns its response and whether its knob (the
/// expand/collapse button, or the jump button on a repeat) was clicked.
fn tree_node(
    ui: &mut Ui,
    db: &Db,
    rect: egui::Rect,
    r: &TreeRow,
    selected: bool,
) -> (egui::Response, bool) {
    let resp = ui.interact(rect, ui.id().with(("node", &r.path)), Sense::click());

    // The knob sits on the right edge: the number of dependencies while
    // collapsed, a minus while open, `⬆` on a repeat.
    let knob_text = if r.cycle {
        Some("⬆".to_string())
    } else if r.children > 0 && !r.open {
        Some(r.children.to_string())
    } else if r.open {
        Some(String::new())
    } else {
        None
    };
    let knob = knob_text.as_ref().map(|t| {
        let w = (t.chars().count() as f32 * 7.0 + 12.0).max(20.0);
        let rect = egui::Rect::from_center_size(rect.right_center(), egui::vec2(w, 20.0));
        let tip = if r.cycle {
            "Jump to it further up this branch"
        } else if r.open {
            "Hide its dependencies"
        } else {
            "Show its dependencies"
        };
        (
            rect,
            ui.interact(rect, ui.id().with(("knob", &r.path)), Sense::click())
                .on_hover_text(tip),
        )
    });
    if !ui.is_rect_visible(rect) {
        return (resp, false);
    }

    let (fill, stroke, strong, weak) = node_colors(ui, selected, resp.hovered());
    let installed = r.dep.target.is_some_and(|t| db.pkgs[t].installed());
    let (name, sub) = match r.dep.target {
        None => (r.dep.atom.clone(), "no matching package".to_string()),
        Some(t) => {
            let p = &db.pkgs[t];
            let note = if r.cycle {
                "  ·  cycle"
            } else if !installed {
                "  ·  not installed"
            } else {
                ""
            };
            (p.name().to_string(), format!("{}{note}", p.category()))
        }
    };
    let name_color = if r.dep.target.is_none() {
        ui.visuals().error_fg_color
    } else if installed && !r.cycle {
        strong
    } else {
        weak
    };
    let accent = KINDS
        .iter()
        .find(|k| r.dep.kinds & k.0 != 0)
        .map(|k| k.4.gamma_multiply(if r.cycle { 0.4 } else { 0.9 }));
    // Repeats are outlines only, so the full node stands out.
    let fill = if r.cycle && !selected {
        Color32::TRANSPARENT
    } else {
        fill
    };
    let card = Card {
        fill,
        stroke,
        accent,
        name: &name,
        name_color,
        italics: !installed && r.dep.target.is_some(),
        sub: &sub,
        sub_color: weak,
    };
    let painter = ui.painter();
    card.paint(painter, rect);

    let mut clicked = false;
    if let (Some(text), Some((krect, kresp))) = (knob_text, &knob) {
        let v = ui.visuals();
        let hot = kresp.hovered();
        let kfill = if hot {
            v.widgets.hovered.weak_bg_fill
        } else {
            v.widgets.inactive.weak_bg_fill
        };
        let kstroke = if hot {
            v.widgets.hovered.bg_stroke
        } else {
            v.widgets.noninteractive.bg_stroke
        };
        let kcolor = if hot {
            v.strong_text_color()
        } else {
            v.text_color()
        };
        painter.rect(
            *krect,
            CornerRadius::same(10),
            kfill,
            kstroke,
            egui::StrokeKind::Inside,
        );
        if text.is_empty() {
            let c = krect.center();
            painter.line_segment(
                [c - egui::vec2(4.0, 0.0), c + egui::vec2(4.0, 0.0)],
                Stroke::new(1.5, kcolor),
            );
        } else {
            painter.text(
                krect.center(),
                Align2::CENTER_CENTER,
                text,
                FontId::proportional(11.0),
                kcolor,
            );
        }
        clicked = kresp.clicked();
    }

    let mut tip = match r.dep.target {
        Some(t) => format!(
            "{}\n{}  ·  {}",
            r.dep.atom,
            db.pkgs[t].category(),
            db.pkgs[t].ver.full
        ),
        None => format!("{}\nNo matching package", r.dep.atom),
    };
    let kinds: Vec<&str> = KINDS
        .iter()
        .filter(|k| r.dep.kinds & k.0 != 0)
        .map(|k| k.2)
        .collect();
    if !kinds.is_empty() {
        tip += &format!("\n{} dependency", kinds.join(", "));
    }
    if !installed && r.dep.target.is_some() {
        tip += "\nNot installed";
    }
    if r.cycle {
        tip += "\nDependency cycle: this package is further up this branch (⬆ or ➡ jumps there)";
    }
    (resp.on_hover_text(tip), clicked)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(path: &[usize], open: bool) -> TreeRow {
        TreeRow {
            depth: path.len() - 1,
            dep: Dep {
                atom: String::new(),
                target: path.last().copied(),
                kinds: 0,
            },
            path: path.iter().copied().map(Sel::Pkg).collect(),
            cycle: false,
            children: usize::from(open),
            open,
        }
    }

    #[test]
    fn empty_tree_keeps_root_and_room_for_empty_message() {
        let layout = TreeLayout::new(&[]);

        assert!(layout.nodes.is_empty());
        assert!(layout.parent.is_empty());
        assert_eq!(layout.root.min, egui::pos2(TREE_PAD, TREE_PAD));
        assert_eq!(layout.root.size(), egui::vec2(NODE_W, NODE_H));
        assert!(layout.size.x >= layout.root.right() + 320.0);
        assert!(layout.size.y > layout.root.bottom());
    }

    #[test]
    fn nested_branches_keep_their_parents_and_do_not_overlap() {
        let rows = vec![
            row(&[0], true),
            row(&[0, 1], true),
            row(&[0, 1, 2], false),
            row(&[0, 1, 3], false),
            row(&[0, 4], false),
            row(&[5], true),
            row(&[5, 6], false),
            row(&[5, 7], false),
            row(&[8], false),
        ];
        let layout = TreeLayout::new(&rows);

        assert_eq!(
            layout.parent,
            vec![
                None,
                Some(0),
                Some(1),
                Some(1),
                Some(0),
                None,
                Some(5),
                Some(5),
                None
            ]
        );
        for (i, node) in layout.nodes.iter().enumerate() {
            let parent = layout.parent[i].map_or(layout.root, |p| layout.nodes[p]);
            assert!(node.left() > parent.right());
            assert!(node.left() >= 0.0 && node.top() >= 0.0);
            assert!(node.right() < layout.size.x && node.bottom() < layout.size.y);
            for other in &layout.nodes[i + 1..] {
                assert!(!node.intersects(*other));
            }
        }
        assert!(layout.root.right() < layout.size.x);
        assert!(layout.root.bottom() < layout.size.y);
    }

    #[test]
    fn large_fanout_keeps_parent_near_first_child() {
        let mut rows = vec![row(&[0], true)];
        rows.extend((1..=20).map(|id| row(&[0, id], false)));
        let layout = TreeLayout::new(&rows);

        let first_child = layout.nodes[1].center().y;
        let parent = layout.nodes[0].center().y;
        assert!(parent >= first_child);
        assert!(parent <= first_child + 2.0 * (NODE_H + GAP_Y));
        assert!(layout.nodes.last().unwrap().bottom() < layout.size.y);
    }

    #[test]
    fn cycle_origin_uses_the_matching_ancestor_on_its_own_branch() {
        let mut rows = vec![
            row(&[0], true),
            row(&[0, 2], false),
            row(&[1], true),
            row(&[1, 2], true),
            row(&[1, 2, 3], true),
            row(&[1, 2, 3, 2], false),
        ];
        rows[5].cycle = true;

        assert_eq!(cycle_origin(&rows, &rows[5]), Some(3));
    }

    #[test]
    fn cycle_back_to_root_has_no_ancestor_row() {
        let mut rows = vec![row(&[1], true), row(&[1, 0], false)];
        rows[1].cycle = true;

        assert_eq!(cycle_origin(&rows, &rows[1]), None);
    }
}
