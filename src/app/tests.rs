use super::*;

fn dep(id: Id, kinds: u8) -> Dep {
    Dep {
        atom: format!("test/pkg{id}"),
        target: Some(id),
        kinds,
    }
}

fn path(ids: &[Id]) -> Vec<Sel> {
    ids.iter().copied().map(Sel::Pkg).collect()
}

fn fixture(deps: Vec<Vec<Dep>>) -> App {
    App {
        loading: None,
        db: Some(Db::for_test(deps, vec![dep(0, 0)])),
        error: None,
        view: View::Tree,
        root: Root::Pkg(0),
        path: Vec::new(),
        focus: 0,
        history: Vec::new(),
        expanded: HashSet::new(),
        kinds: ALL_KINDS,
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

fn press(app: &mut App, key: Key) {
    let (cols, rows) = match app.view {
        View::Tree => (Vec::new(), app.tree_rows()),
        View::Columns => (app.columns(), Vec::new()),
    };
    let input = egui::RawInput {
        events: vec![egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }],
        ..Default::default()
    };
    let mut output = egui::Context::default().run_ui(input, |ui| {
        app.handle_keys(ui.ctx(), &cols, &rows);
    });
    // These input-only frames have no renderer to apply texture updates.
    output.textures_delta.clear();
}

#[test]
fn diamond_dependencies_expand_independently_on_each_branch() {
    let mut app = fixture(vec![
        vec![dep(1, RDEP), dep(2, RDEP)],
        vec![dep(3, RDEP)],
        vec![dep(3, RDEP)],
        vec![dep(4, RDEP)],
        vec![],
    ]);
    for ids in [&[1][..], &[1, 3], &[2]] {
        app.expanded.insert((app.root, path(ids)));
    }

    let rows = app.tree_rows();
    let visible: Vec<_> = rows.iter().map(|r| r.path.clone()).collect();
    assert_eq!(
        visible,
        vec![
            path(&[1]),
            path(&[1, 3]),
            path(&[1, 3, 4]),
            path(&[2]),
            path(&[2, 3])
        ]
    );
    assert!(rows.iter().all(|r| !r.cycle));
    assert!(rows[1].open);
    assert!(!rows[4].open);
    assert_eq!(rows[4].children, 1);

    app.path = path(&[2, 3]);
    press(&mut app, Key::ArrowRight);
    let rows = app.tree_rows();
    assert!(rows.iter().any(|r| r.path == path(&[1, 3, 4])));
    assert!(rows.iter().any(|r| r.path == path(&[2, 3, 4])));
    assert_eq!(app.path, path(&[2, 3]));
}

#[test]
fn cycles_stop_expanding_and_right_arrow_jumps_to_the_ancestor_or_root() {
    for (root, selected, expected) in [
        (Root::World, path(&[0, 1, 0]), path(&[0])),
        (Root::Pkg(0), path(&[1, 0]), Vec::new()),
    ] {
        let mut app = fixture(vec![vec![dep(1, RDEP)], vec![dep(0, RDEP)]]);
        app.root = root;
        app.path = selected.clone();
        app.expand_to_selection();
        // Even an explicitly open cycle must stop at the repeated package.
        app.expanded.insert((root, selected.clone()));

        let rows = app.tree_rows();
        assert_eq!(rows.len(), selected.len());
        let repeat = rows.last().unwrap();
        assert!(repeat.cycle);
        assert_eq!(repeat.children, 0);
        assert!(!repeat.open);

        press(&mut app, Key::ArrowRight);
        assert_eq!(app.path, expected);
        assert_eq!(app.focus, 0);
        assert_eq!(app.current(), Some(Sel::Pkg(0)));
        assert!(app.scroll_to_sel);
    }
}

#[test]
fn collapse_and_kind_filters_trim_selection_to_the_deepest_visible_node() {
    let mut app = fixture(vec![vec![dep(1, RDEP)], vec![dep(2, DEP)], vec![]]);
    app.path = path(&[1, 2]);
    app.expand_to_selection();
    assert_eq!(app.tree_rows().len(), 2);

    app.expanded.clear();
    assert_eq!(app.tree_rows().len(), 1);
    assert_eq!(app.path, path(&[1]));
    assert_eq!(app.focus, 0);

    app.path = path(&[1, 2]);
    app.expand_to_selection();
    app.kinds = RDEP;
    let rows = app.tree_rows();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].children, 0);
    assert_eq!(app.path, path(&[1]));

    app.kinds = DEP;
    assert!(app.tree_rows().is_empty());
    assert!(app.path.is_empty());
    assert_eq!(app.current(), Some(Sel::Pkg(0)));
}

#[test]
fn columns_stop_at_cycles_and_keep_keyboard_focus_within_visible_columns() {
    let mut app = fixture(vec![vec![dep(1, RDEP)], vec![dep(0, RDEP)]]);
    app.set_view(View::Columns);
    app.path = path(&[1, 0, 1]);
    app.focus = 2;

    let cols = app.columns();
    assert_eq!(cols.len(), 2);
    assert_eq!(cols[0].parent, Some(0));
    assert_eq!(cols[1].parent, Some(1));
    assert_eq!(cols[1].ancestors, vec![0, 1]);
    assert_eq!(app.path, path(&[1, 0]));
    assert_eq!(app.focus, 1);

    press(&mut app, Key::ArrowRight);
    assert_eq!(app.path, path(&[1, 0]));
    assert_eq!(app.focus, 1);
    press(&mut app, Key::ArrowLeft);
    assert_eq!(app.path, path(&[1]));
    assert_eq!(app.focus, 0);
    press(&mut app, Key::ArrowRight);
    assert_eq!(app.path, path(&[1, 0]));
    assert_eq!(app.focus, 1);

    app.kinds = DEP;
    assert_eq!(app.columns().len(), 1);
    assert!(app.path.is_empty());
    assert_eq!(app.focus, 0);
}

#[test]
fn switching_views_preserves_the_path_expansion_and_each_views_kind_filter() {
    let mut app = fixture(vec![
        vec![dep(1, RDEP), dep(3, RDEP)],
        vec![dep(2, RDEP)],
        vec![],
        vec![dep(2, RDEP)],
    ]);
    app.path = path(&[1, 2]);
    app.kinds = RDEP | PDEP;
    app.expanded.insert((app.root, path(&[3])));

    app.set_view(View::Columns);
    assert_eq!(app.path, path(&[1, 2]));
    assert_eq!(app.current(), Some(Sel::Pkg(2)));
    assert_eq!(app.kinds, ALL_KINDS);
    assert_eq!(app.other_kinds, RDEP | PDEP);
    assert_eq!(app.reveal_col, Some(2));
    app.kinds = DEP;

    app.set_view(View::Tree);
    assert_eq!(app.kinds, RDEP | PDEP);
    assert_eq!(app.other_kinds, DEP);
    assert_eq!(app.path, path(&[1, 2]));
    assert!(app.expanded.contains(&(app.root, path(&[3]))));
    assert!(app.tree_rows().iter().any(|r| r.path == path(&[1, 2])));

    app.set_view(View::Tree);
    assert_eq!(app.kinds, RDEP | PDEP);
    app.set_view(View::Columns);
    assert_eq!(app.kinds, DEP);
    assert_eq!(app.path, path(&[1, 2]));
}
