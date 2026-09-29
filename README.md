# gendtree

A graphical dependency browser for **Gentoo Portage**, shipped as a single
binary.

Start from your `@world` set, or search for any package (installed or not), and
walk its dependencies column by column: each column lists what the package
selected in the column to its left depends on. A side panel shows details for
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

| Key | Action |
| --- | --- |
| `↑`/`↓`, `k`/`j`, `PgUp`/`PgDn`, `g`/`G` | move within a column |
| `→` / `l` | step into the dependencies of the selection |
| `←` / `h` | step back a column |
| `Enter` or double-click | *explore from here*: make the package the root |
| `Backspace` | previous view |
| `/` or `Ctrl+F` | search all packages; `Enter` picks the top result |
| `Esc` | leave the search box |

The **Show** toggles in the top bar filter by dependency type. Each row has
chips for its types: `R` runtime (RDEPEND), `D` build (DEPEND), `B` build tool
(BDEPEND), `P` post (PDEPEND) and `I` install (IDEPEND). **Installed only**
hides packages that aren't installed.

In the columns, italic dim names are not installed, red names are atoms that no
package matches, and `cycle` marks a package that is already open to the left.

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
