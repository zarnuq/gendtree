#!/usr/bin/env python3
"""
gendtree - a TUI dependency-graph browser for Gentoo Portage.

Browse the dependency tree (deps of deps) of every package in your @world set,
or search for any package and explore its dependency tree interactively.

Runs on any Gentoo system: it uses only the Python standard library (curses)
for the interface and the `portage` module that ships with Portage for data.
If the interpreter you launch it with lacks the `portage` module, it will
automatically re-exec into one that has it.

Keys (also shown with '?'):
  up/down, k/j     move cursor
  PgUp/PgDn        page up / down
  g / G            jump to top / bottom
  right / l / enter  expand node (load & show its dependencies)
  left / h         collapse node, or move to its parent
  space            toggle expand/collapse
  E                expand this whole subtree (bounded)
  /                search: show the dep tree of any package (not just @world)
  w                return to the @world view
  r                toggle reverse deps (what depends on this) for a node
  q                quit
"""

import os
import sys


def _ensure_portage_interpreter():
    """Re-exec into a python that has the `portage` module, if needed."""
    try:
        import portage  # noqa: F401
        return
    except ImportError:
        pass

    if os.environ.get("_GENDTREE_REEXEC"):
        sys.exit(
            "gendtree: could not import the 'portage' module.\n"
            "This tool must run on a Gentoo system with Portage installed."
        )

    import glob
    import shutil

    # Portage installs its module under /usr/lib/pythonX.Y/site-packages/portage
    versions = []
    for path in glob.glob("/usr/lib/python3*/site-packages/portage"):
        ver = os.path.basename(os.path.dirname(os.path.dirname(path)))  # python3.14
        versions.append(ver)
    # Prefer newest.
    versions.sort(reverse=True)

    env = dict(os.environ, _GENDTREE_REEXEC="1")
    for ver in versions:
        exe = shutil.which(ver)
        if exe:
            os.execve(exe, [exe] + sys.argv, env)

    sys.exit(
        "gendtree: could not import the 'portage' module and found no\n"
        "python interpreter that provides it. Is Portage installed?"
    )


_ensure_portage_interpreter()

import curses  # noqa: E402
import portage  # noqa: E402
from portage.dep import use_reduce  # noqa: E402


# --------------------------------------------------------------------------- #
# Data backend
# --------------------------------------------------------------------------- #
class PortageBackend:
    """Thin wrapper over the portage databases with small caches."""

    _DEP_KEYS = ("RDEPEND", "DEPEND", "BDEPEND", "PDEPEND")

    def __init__(self):
        self.root = str(portage.root)
        self.vardb = portage.db[self.root]["vartree"].dbapi   # installed pkgs
        self.portdb = portage.db[self.root]["porttree"].dbapi  # ebuild repo
        self._dep_cache = {}       # cpv -> sorted list of child cpvs
        self._rdep_cache = {}      # cpv -> sorted list of parent cpvs
        self._reverse_index = None  # cpv -> set(parent cpv)

    # -- resolution -------------------------------------------------------- #
    def best_installed(self, atom):
        """Return newest installed cpv matching atom, or None."""
        try:
            matches = self.vardb.match(atom)
        except Exception:
            return None
        return matches[-1] if matches else None

    def best_ebuild(self, atom):
        """Return newest ebuild cpv matching atom, or None."""
        try:
            matches = self.portdb.match(atom)
        except Exception:
            return None
        return matches[-1] if matches else None

    def resolve(self, atom):
        """Resolve a user query/atom to a cpv (installed preferred)."""
        return self.best_installed(atom) or self.best_ebuild(atom)

    def is_installed(self, cpv):
        return bool(self.vardb.cpv_exists(cpv))

    # -- forward deps ------------------------------------------------------ #
    def deps_of(self, cpv):
        """Return sorted list of (atom, child_cpv|None) for a package.

        Uses installed metadata when the package is installed, else the ebuild
        metadata. USE-conditional deps are reduced with the package's USE flags.
        """
        if cpv in self._dep_cache:
            return self._dep_cache[cpv]

        installed = self.is_installed(cpv)
        db = self.vardb if installed else self.portdb
        keys = self._DEP_KEYS + ("USE", "EAPI")
        try:
            md = dict(zip(keys, db.aux_get(cpv, keys)))
        except Exception:
            self._dep_cache[cpv] = []
            return []

        use = md.get("USE", "").split()
        eapi = md.get("EAPI") or None
        atoms = {}
        for key in self._DEP_KEYS:
            depstr = md.get(key)
            if not depstr:
                continue
            try:
                tokens = use_reduce(depstr, uselist=use, eapi=eapi, flat=True)
            except Exception:
                try:
                    tokens = use_reduce(depstr, uselist=use, flat=True)
                except Exception:
                    continue
            for tok in tokens:
                if tok in ("||", "(", ")") or tok.startswith("!"):
                    continue
                atoms.setdefault(tok, None)

        result = []
        for atom in atoms:
            child = self.best_installed(atom)
            if child is None and not installed:
                child = self.best_ebuild(atom)
            result.append((atom, child))
        # Sort by resolved package name (fall back to atom text).
        result.sort(key=lambda t: (t[1] or t[0]).lower())
        self._dep_cache[cpv] = result
        return result

    # -- reverse deps ------------------------------------------------------ #
    def _build_reverse_index(self, progress=None):
        """Index, over all installed packages, who depends on whom."""
        index = {}
        all_cpvs = self.vardb.cpv_all()
        total = len(all_cpvs)
        for i, cpv in enumerate(all_cpvs):
            if progress and i % 25 == 0:
                progress(i, total)
            for _atom, child in self.deps_of(cpv):
                if child is not None:
                    index.setdefault(child, set()).add(cpv)
        self._reverse_index = index

    def rdeps_of(self, cpv, progress=None):
        """Return sorted list of (label, parent_cpv) that depend on cpv."""
        if cpv in self._rdep_cache:
            return self._rdep_cache[cpv]
        if self._reverse_index is None:
            self._build_reverse_index(progress=progress)
        parents = sorted(self._reverse_index.get(cpv, ()), key=str.lower)
        result = [(p, p) for p in parents]
        self._rdep_cache[cpv] = result
        return result

    # -- world ------------------------------------------------------------- #
    def world_atoms(self):
        """Return sorted list of atom strings from the @world set."""
        world_file = os.path.join(self.root, "var/lib/portage/world")
        atoms = []
        try:
            with open(world_file, encoding="utf-8") as fh:
                for line in fh:
                    line = line.strip()
                    if line and not line.startswith("#"):
                        atoms.append(line)
        except OSError:
            pass
        atoms.sort(key=str.lower)
        return atoms


# --------------------------------------------------------------------------- #
# Tree model
# --------------------------------------------------------------------------- #
class Node:
    __slots__ = ("atom", "cpv", "depth", "parent", "children",
                 "expanded", "cyclic", "reverse", "loading_failed", "is_last")

    def __init__(self, atom, cpv, depth, parent, reverse=False, is_last=True):
        self.atom = atom            # atom text used to reach this node
        self.cpv = cpv              # resolved cpv, or None if unresolved
        self.depth = depth
        self.parent = parent
        self.children = None        # None = not loaded yet
        self.expanded = False
        self.cyclic = False         # would repeat an ancestor -> not expandable
        self.reverse = reverse      # this node shows reverse deps of its cpv
        self.loading_failed = False
        self.is_last = is_last      # last child of its parent (for tree guides)

    def guide_flags(self):
        """Return the ancestors' is_last flags from depth 1 down to this node.

        Used to draw the │ / space columns that connect tree branches.
        """
        flags = []
        node = self
        while node is not None and node.depth >= 1:
            flags.append(node.is_last)
            node = node.parent
        flags.reverse()
        return flags

    def ancestor_cpvs(self):
        seen = set()
        node = self.parent
        while node is not None:
            if node.cpv:
                seen.add(node.cpv)
            node = node.parent
        return seen

    def is_expandable(self):
        return self.cpv is not None and not self.cyclic


# --------------------------------------------------------------------------- #
# TUI
# --------------------------------------------------------------------------- #
class App:
    HELP_LINES = [
        "  gendtree — Gentoo dependency-tree browser",
        "",
        "  MOVE",
        "    ↑ ↓  /  k j       move cursor",
        "    PgUp / PgDn       page up / down",
        "    g / G             jump to top / bottom",
        "",
        "  EXPLORE",
        "    space  /  → l     open the dependency tree (deps of deps)",
        "    ← h               collapse, or jump to parent",
        "    enter             reverse deps: what depends on this package",
        "    E                 expand the whole subtree (bounded depth)",
        "",
        "  FIND",
        "    /                 search any package's dep tree (not just @world)",
        "    w                 back to the @world view",
        "",
        "    ?                 toggle this help          q   quit",
        "",
        "  LEGEND",
        "    ▸ ▾  collapsed / open      ↺  dependency cycle",
        "    ↩    reverse-dep root      dim  not installed",
        "    red tree lines = reverse deps (what depends on this)",
        "",
        "  Press any key to close help.",
    ]

    def __init__(self, stdscr, backend):
        self.scr = stdscr
        self.backend = backend
        self.roots = []          # top-level Nodes
        self.title = ""
        self.cursor = 0          # index into flattened visible list
        self.top = 0             # scroll offset
        self.flat = []           # cached flattened visible nodes
        self.status = ""
        self.show_help = False
        self._init_colors()
        self.load_world()

    # -- setup ------------------------------------------------------------- #
    # Semantic color-pair ids.
    C_CATEGORY = 1
    C_ACCENT = 2
    C_ALERT = 3
    C_REVERSE = 4
    C_VERSION = 5
    C_GUIDE = 6
    C_BAR = 7
    C_SELECT = 8
    C_NAME = 9

    def _init_colors(self):
        self.has_color = False
        try:
            curses.start_color()
            curses.use_default_colors()
            curses.init_pair(self.C_CATEGORY, curses.COLOR_CYAN, -1)
            curses.init_pair(self.C_ACCENT, curses.COLOR_YELLOW, -1)
            curses.init_pair(self.C_ALERT, curses.COLOR_RED, -1)
            curses.init_pair(self.C_REVERSE, curses.COLOR_GREEN, -1)
            curses.init_pair(self.C_VERSION, curses.COLOR_MAGENTA, -1)
            curses.init_pair(self.C_GUIDE, curses.COLOR_BLUE, -1)
            curses.init_pair(self.C_BAR, curses.COLOR_BLACK, curses.COLOR_CYAN)
            curses.init_pair(self.C_SELECT, curses.COLOR_WHITE, curses.COLOR_BLUE)
            curses.init_pair(self.C_NAME, curses.COLOR_WHITE, -1)
            self.has_color = True
        except curses.error:
            pass

    def _c(self, n):
        return curses.color_pair(n) if self.has_color else 0

    # -- data / roots ------------------------------------------------------ #
    def load_world(self):
        self.title = "@world  (%s packages)" % 0
        nodes = []
        for atom in self.backend.world_atoms():
            cpv = self.backend.resolve(atom)
            nodes.append(Node(atom, cpv, 0, None))
        self.roots = nodes
        self.title = "@world  (%d packages)" % len(nodes)
        self.cursor = 0
        self.top = 0
        self._reflow()
        self.status = "Loaded @world. Press '?' for help."

    def add_search_root(self, query):
        cpv = self.backend.resolve(query)
        if cpv is None:
            self.status = "No package found for '%s'." % query
            return
        node = Node(query, cpv, 0, None)
        # Prepend so the newly searched package is on top.
        self.roots.insert(0, node)
        self.title = "search: %s  +  @world" % query
        self.cursor = 0
        self.top = 0
        self._reflow()
        self.status = "Showing %s. Press enter/right to expand." % cpv

    # -- flatten / navigation --------------------------------------------- #
    def _reflow(self):
        flat = []

        def walk(node):
            flat.append(node)
            if node.expanded and node.children:
                for child in node.children:
                    walk(child)

        for root in self.roots:
            walk(root)
        self.flat = flat
        if self.cursor >= len(flat):
            self.cursor = max(0, len(flat) - 1)

    def current(self):
        if 0 <= self.cursor < len(self.flat):
            return self.flat[self.cursor]
        return None

    # -- expand / collapse ------------------------------------------------- #
    def _load_children(self, node):
        if node.children is not None:
            return
        if not node.is_expandable():
            node.children = []
            return
        ancestors = node.ancestor_cpvs()
        children = []
        if node.reverse:
            pairs = self.backend.rdeps_of(node.cpv, progress=self._progress)
        else:
            pairs = self.backend.deps_of(node.cpv)
        for i, (atom, child_cpv) in enumerate(pairs):
            child = Node(atom, child_cpv, node.depth + 1, node,
                         reverse=node.reverse, is_last=(i == len(pairs) - 1))
            if child_cpv is not None and child_cpv in ancestors:
                child.cyclic = True
            children.append(child)
        node.children = children

    def expand(self, node):
        if not node.is_expandable():
            name = node.cpv or node.atom
            if node.cyclic:
                self.status = "%s already appears higher up (cycle)." % name
            else:
                self.status = "%s is not installed — no dependencies to show." % name
            return
        self._load_children(node)
        node.expanded = True
        if not node.children:
            self.status = "%s has no dependencies." % (node.cpv or node.atom)
        self._reflow()

    def collapse(self, node):
        node.expanded = False
        self._reflow()

    def expand_subtree(self, node, max_depth=6, max_nodes=2000):
        count = [0]

        def rec(n, depth):
            if depth <= 0 or count[0] >= max_nodes:
                return
            if not n.is_expandable():
                return
            self._load_children(n)
            n.expanded = True
            for child in n.children:
                count[0] += 1
                if count[0] >= max_nodes:
                    break
                rec(child, depth - 1)

        rec(node, max_depth)
        self._reflow()
        self.status = "Expanded subtree (%d nodes loaded)." % count[0]

    # -- progress callback for slow reverse-dep indexing ------------------- #
    def _progress(self, i, total):
        h, w = self.scr.getmaxyx()
        pct = int(i * 100 / total) if total else 0
        bar_w = max(10, min(30, w - 40))
        filled = int(bar_w * pct / 100)
        bar = "█" * filled + "░" * (bar_w - filled)
        msg = " indexing reverse deps  %s  %3d%% " % (bar, pct)
        try:
            self.scr.addstr(h - 1, 0, msg.ljust(w - 1)[:w - 1],
                            self._c(self.C_ACCENT) | curses.A_BOLD)
            self.scr.refresh()
        except curses.error:
            pass

    # -- rendering --------------------------------------------------------- #
    @staticmethod
    def _split_cpv(cpv):
        parts = portage.catpkgsplit(cpv)
        if not parts:
            return None
        cat, pn, ver, rev = parts
        if rev and rev != "r0":
            ver = "%s-%s" % (ver, rev)
        return cat, pn, ver

    def draw(self):
        self.scr.erase()
        h, w = self.scr.getmaxyx()
        if self.show_help:
            self._draw_help(h, w)
            self.scr.refresh()
            return

        self._draw_header(w)

        # Reserve: header (row 0), status (row h-2), keybind bar (row h-1).
        body_h = h - 3
        self._clamp_scroll(body_h)

        if not self.flat:
            self.scr.addstr(2, 2, "(empty)", curses.A_DIM)

        for row in range(body_h):
            idx = self.top + row
            if idx >= len(self.flat):
                break
            node = self.flat[idx]
            self._draw_node(row + 1, w, node, idx == self.cursor)

        self._draw_status(h, w)
        self._draw_keybar(h, w)
        self.scr.refresh()

    def _seg(self, y, x, w, text, attr):
        """Draw clipped text at (y, x); return the new x. Never raises."""
        if x >= w - 1 or not text:
            return x
        s = text[:max(0, w - 1 - x)]
        try:
            self.scr.addstr(y, x, s, attr)
        except curses.error:
            pass
        return x + len(s)

    def _draw_header(self, w):
        bar = self._c(self.C_BAR)
        self.scr.addstr(0, 0, " " * (w - 1), bar)
        x = self._seg(0, 1, w, " gendtree ", bar | curses.A_BOLD)
        x = self._seg(0, x + 1, w, "│", bar)
        self._seg(0, x + 1, w, " " + self.title, bar)

    # Persistent bottom keybind bar.
    KEYBINDS = [
        ("↑↓", "move"), ("space", "open"), ("↵", "rdeps"), ("←", "back"),
        ("E", "expand"), ("/", "search"), ("w", "world"), ("?", "help"),
        ("q", "quit"),
    ]

    def _draw_status(self, h, w):
        """Transient status message on the line above the keybind bar."""
        y = h - 2
        if self.status:
            self._seg(y, 0, w, self.status[:w - 1],
                      self._c(self.C_ACCENT))
        if self.flat:
            pos = "[%d/%d] " % (self.cursor + 1, len(self.flat))
            self._seg(y, w - 1 - len(pos), w, pos, curses.A_DIM)

    def _draw_keybar(self, h, w):
        """Always-visible keybinding bar at the very bottom."""
        y = h - 1
        bar = self._c(self.C_BAR)
        self.scr.addstr(y, 0, " " * (w - 1), bar)
        x = 1
        for key, desc in self.KEYBINDS:
            x = self._seg(y, x, w, key, bar | curses.A_BOLD)
            x = self._seg(y, x, w, " " + desc + "  ", bar)

    def _draw_node(self, y, w, node, selected):
        sel = selected and self.has_color
        if sel:
            self.scr.addstr(y, 0, " " * (w - 1), self._c(self.C_SELECT))

        def seg(x, text, attr):
            if sel:
                attr = self._c(self.C_SELECT) | curses.A_BOLD
            elif selected:
                attr = attr | curses.A_REVERSE
            return self._seg(y, x, w, text, attr)

        # Reverse-dep branches get red guide lines to make the mode obvious.
        if node.reverse:
            guide = self._c(self.C_ALERT)
        else:
            guide = self._c(self.C_GUIDE) | curses.A_DIM
        x = 0

        # Tree-branch guides (│  ├─ └─) for everything below the roots.
        flags = node.guide_flags()
        if flags:
            for ancestor_last in flags[:-1]:
                x = seg(x, "    " if ancestor_last else "│   ", guide)
            x = seg(x, "└── " if flags[-1] else "├── ", guide)

        # Expansion glyph.
        if node.cyclic:
            x = seg(x, "↺ ", self._c(self.C_ALERT) | curses.A_BOLD)
        elif node.is_expandable():
            g = "▾ " if node.expanded else "▸ "
            x = seg(x, g, self._c(self.C_ACCENT) | curses.A_BOLD)
        else:
            x = seg(x, "· ", guide)

        # Reverse-dep root marker.
        if node.reverse and node.depth == 0:
            x = seg(x, "↩ ", self._c(self.C_REVERSE) | curses.A_BOLD)

        # Package label: colorize category / name / version.
        parts = self._split_cpv(node.cpv) if node.cpv else None
        if parts:
            cat, pn, ver = parts
            x = seg(x, cat + "/", self._c(self.C_CATEGORY))
            x = seg(x, pn, self._c(self.C_NAME) | curses.A_BOLD)
            x = seg(x, "-" + ver, self._c(self.C_VERSION) | curses.A_DIM)
        elif node.cpv:
            x = seg(x, node.cpv, self._c(self.C_NAME))
        else:
            x = seg(x, node.atom, self._c(self.C_ALERT) | curses.A_DIM)
            x = seg(x, "  (not installed)", curses.A_DIM)

    def _draw_help(self, h, w):
        self.scr.addstr(0, 0, " " * (w - 1), self._c(self.C_BAR))
        self._seg(0, 1, w, " gendtree — help ",
                  self._c(self.C_BAR) | curses.A_BOLD)
        section_heads = {"MOVE", "EXPLORE", "FIND", "LEGEND"}
        for i, line in enumerate(self.HELP_LINES, start=2):
            if i >= h - 1:
                break
            stripped = line.strip()
            if i == 2:  # title line
                attr = self._c(self.C_NAME) | curses.A_BOLD
            elif stripped in section_heads:
                attr = self._c(self.C_ACCENT) | curses.A_BOLD
            else:
                attr = 0
            try:
                self.scr.addstr(i, 2, line[:w - 3], attr)
            except curses.error:
                pass

    def _clamp_scroll(self, body_h):
        if self.cursor < self.top:
            self.top = self.cursor
        elif self.cursor >= self.top + body_h:
            self.top = self.cursor - body_h + 1
        if self.top < 0:
            self.top = 0

    # -- input ------------------------------------------------------------- #
    def prompt(self, label):
        h, w = self.scr.getmaxyx()
        curses.echo()
        curses.curs_set(1)
        try:
            self.scr.addstr(h - 1, 0, (" " * (w - 1)))
            self.scr.addstr(h - 1, 0, label,
                            self._c(self.C_ACCENT) | curses.A_BOLD)
            self.scr.refresh()
            raw = self.scr.getstr(h - 1, len(label), 200)
        except curses.error:
            raw = b""
        finally:
            curses.noecho()
            curses.curs_set(0)
        try:
            return raw.decode("utf-8", "replace").strip()
        except Exception:
            return ""

    def run(self):
        curses.curs_set(0)
        while True:
            self.draw()
            try:
                ch = self.scr.getch()
            except KeyboardInterrupt:
                break

            if self.show_help:
                self.show_help = False
                continue

            body_h = self.scr.getmaxyx()[0] - 3

            if ch in (ord("q"), 27):  # q / ESC
                break
            elif ch in (curses.KEY_DOWN, ord("j")):
                self.cursor = min(self.cursor + 1, len(self.flat) - 1)
            elif ch in (curses.KEY_UP, ord("k")):
                self.cursor = max(self.cursor - 1, 0)
            elif ch == curses.KEY_NPAGE:
                self.cursor = min(self.cursor + body_h, len(self.flat) - 1)
            elif ch == curses.KEY_PPAGE:
                self.cursor = max(self.cursor - body_h, 0)
            elif ch == ord("g"):
                self.cursor = 0
            elif ch == ord("G"):
                self.cursor = len(self.flat) - 1
            elif ch in (curses.KEY_ENTER, 10, 13):
                # Enter -> show what depends on the hovered package.
                self._toggle_reverse()
            elif ch in (curses.KEY_RIGHT, ord("l")):
                node = self.current()
                if node:
                    if node.expanded:
                        self.cursor = min(self.cursor + 1, len(self.flat) - 1)
                    else:
                        self.expand(node)
            elif ch in (curses.KEY_LEFT, ord("h")):
                node = self.current()
                if node:
                    if node.expanded:
                        self.collapse(node)
                    elif node.parent is not None:
                        # Move cursor to parent.
                        try:
                            self.cursor = self.flat.index(node.parent)
                        except ValueError:
                            pass
            elif ch == ord(" "):
                node = self.current()
                if node:
                    if node.expanded:
                        self.collapse(node)
                    else:
                        self.expand(node)
            elif ch == ord("E"):
                node = self.current()
                if node:
                    self.status = "Expanding subtree..."
                    self.draw()
                    self.expand_subtree(node)
            elif ch == ord("/"):
                query = self.prompt("search package: ")
                if query:
                    self.add_search_root(query)
            elif ch == ord("w"):
                self.load_world()
            elif ch == ord("r"):
                self._toggle_reverse()
            elif ch == ord("?"):
                self.show_help = True

    def _toggle_reverse(self):
        node = self.current()
        if not node or node.cpv is None:
            self.status = "Select an installed package first."
            return
        # Create a reverse-dep root for this package at the top.
        rnode = Node(node.atom, node.cpv, 0, None, reverse=True)
        self.roots.insert(0, rnode)
        self.title = "reverse deps: %s" % node.cpv
        self.cursor = 0
        self.top = 0
        self._reflow()
        self.status = ("Reverse deps of %s. First expand indexes all installed "
                       "packages (may take a moment)." % node.cpv)


def main():
    backend = PortageBackend()

    def _run(stdscr):
        App(stdscr, backend).run()

    curses.wrapper(_run)


if __name__ == "__main__":
    main()
