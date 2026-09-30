# gendtree

A graphical dependency browser for **Gentoo Portage**, shipped as a single
binary.

Start from your `@world` set, or search for any package (installed or not), and
walk its dependencies as a tree of package boxes joined by lines, or column by
column, where each column lists what the package selected in the column to its
left depends on.
A side panel shows details for
the selected package, including **why it is installed** (the shortest chain of
dependencies back to `@world`) and **what requires it**.

## Build

```sh
cargo build --release
./target/release/gendtree
```

The result is one self-contained binary with no Python and no Portage API
dependency. It only needs libc at link time; OpenGL and Wayland/X11 are loaded
at runtime.

## Using it

**Tree** and **Columns** in the top bar switch between the two views. The
selection carries over.

| Key | Tree | Columns |
| --- | --- | --- |
| `↑`/`↓`, `k`/`j`, `PgUp`/`PgDn`, `g`/`G` | move | move within a column |
| `→` / `l` | expand; if open, go to the first dependency | step into the dependencies of the selection |
| `←` / `h` | collapse; if closed, go to the parent | step back a column |
| drag | pan around the tree | |

| Key | Action |
| --- | --- |
| `Enter` or double-click | *explore from here*: make the package the root |
| `Backspace` | previous view |
| `/` or `Ctrl+F` | search all packages; `Enter` picks the top result |
| `Esc` | leave the search box |

The **Show** toggles in the top bar filter by dependency type: `R` runtime
(RDEPEND), `D` build (DEPEND), `B` build tool (BDEPEND), `P` post (PDEPEND) and
`I` install (IDEPEND). Each view keeps its own filter; the tree starts with
runtime and post dependencies only, to stay small. **Installed only** hides
packages that aren't installed.

Italic dim names are not installed, and red names are atoms that no package
matches.

The tree grows left to right. The root (`@world` or the package you're
exploring) sits on the left, and each package is a box joined by a line to the
package that needs it. Each box shows the name, the category, and a strip
coloured by its main dependency type. The button on a box's right edge shows how
many dependencies it has. Click a box (or its button, or press `→`) to open its
dependencies out to the right, and click it again to fold them away. The lines
from the root to the selected package are highlighted.

Everything starts collapsed, and each box opens on its own: a library that many
packages need appears under each of them, and each copy can be opened separately.
The only exception is a dependency cycle, where a package would end up under
itself. There it is drawn as a dimmed outline whose `⬆` button jumps to the copy
further up the branch. Double-click a box to explore from it.

In the columns, each row has chips for its dependency types, and `cycle` marks a
package that is already open to the left.

## How it works

gendtree reads Portage's on-disk databases directly:

- **Installed packages**: `/var/db/pkg/<cat>/<pf>/`. Portage has already
  evaluated the USE conditionals in these `*DEPEND` files, so you see the deps
  that really apply to your system.
- **Available packages**: each repo in `repos.conf`, via
  `metadata/md5-cache`. For overlays without a cache, it reads variable
  assignments straight out of the `.ebuild`. That parse is best effort and is
  flagged as approximate in the UI.
- **USE flags for packages that aren't installed**: the `USE` value in
  `make.conf` plus the ebuild's `IUSE` defaults. Profile USE isn't applied.
- **`|| ( … )` groups** resolve to the first alternative that is installed,
  falling back to the first alternative.
- **Atoms** resolve to the newest installed match, else the newest available
  one. Keyworded versions win over live `9999` ebuilds. Masks and
  `ACCEPT_KEYWORDS` are not evaluated, and sub-slots are ignored.
- **The reverse-dependency index** is built over all installed packages at
  startup, which takes about 0.1 s with a warm cache.

`gendtree.py` is the older curses TUI version, which uses the `portage` Python
module.

## Development

`src/app.rs` owns shared navigation state and frame orchestration. The tree,
columns, and surrounding panels live in `src/app/{tree,columns,panels}.rs`.
`src/db.rs` handles package resolution and dependency indexing; metadata and
Portage configuration readers live in `src/db/portage.rs`.

```sh
cargo test
cargo clippy --all-targets -- -D warnings
```

The regular tests use in-memory package graphs and run without Gentoo. On a
Gentoo system, `cargo test -- --ignored` also exercises the local Portage database.
