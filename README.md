# gendtree

A terminal (TUI) dependency-graph browser for **Gentoo Portage**.

Explore the dependency tree — deps of deps of deps — of every package in your
`@world` set, or search for any package (installed or not) and walk its tree
interactively. You can also flip a package around to see its *reverse*
dependencies (what depends on it).

## Requirements

Any Gentoo system. That's it:

- **Portage** (provides the `portage` Python module — already installed on Gentoo).
- Python 3 with `curses` (part of the standard library).

No pip installs, no external libraries. If you launch it with a Python that
doesn't have the `portage` module, gendtree automatically re-execs into one that
does.

## Usage

```sh
./gendtree.py
```

or

```sh
python3 gendtree.py
```

It opens on your `@world` set. Move to a package and press `enter`/`→` to load
and reveal its dependencies; keep expanding to go deeper.

## Keys

| Key | Action |
| --- | --- |
| `↑`/`↓`, `k`/`j` | move cursor |
| `PgUp`/`PgDn` | page up / down |
| `g` / `G` | jump to top / bottom |
| `space` / `→` / `l` | open the dependency tree (deps of deps) |
| `←` / `h` | collapse, or move to parent |
| `enter` | reverse deps — what depends on the selected package |
| `E` | expand the whole subtree (bounded depth) |
| `/` | search: show any package's dep tree (not just `@world`) |
| `w` | back to the `@world` view |
| `?` | help |
| `q` | quit |

## Legend

- `▸` collapsed / `▾` expanded / `↺` cycle (dependency loops back to an ancestor)
- `↩` a reverse-dependency subtree root
- dim text = the dependency atom is **not installed**

## How it works

- The `@world` set is read from `$EROOT/var/lib/portage/world`.
- Dependencies come from installed-package metadata (`RDEPEND`, `DEPEND`,
  `BDEPEND`, `PDEPEND`), reduced against the package's actual `USE` flags, so you
  see the deps that really apply to your system. Packages not installed fall
  back to ebuild metadata.
- Reverse dependencies are computed by indexing all installed packages the first
  time you request them (a one-off pass; progress is shown at the bottom).
