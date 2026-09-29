//! Parsing and evaluating dependency strings like
//! `foo? ( a/b ) || ( c/d e/f ) !bar? ( g/h )`.

use std::collections::HashSet;

#[derive(Debug)]
pub enum Node {
    Atom(String),
    All(Vec<Node>),
    AnyOf(Vec<Node>),
    Use { flag: String, negate: bool, kids: Vec<Node> },
}

pub fn parse(s: &str) -> Vec<Node> {
    let toks: Vec<&str> = s.split_whitespace().collect();
    let mut pos = 0;
    parse_seq(&toks, &mut pos)
}

fn parse_seq(toks: &[&str], pos: &mut usize) -> Vec<Node> {
    let mut out = Vec::new();
    while *pos < toks.len() {
        let t = toks[*pos];
        *pos += 1;
        match t {
            ")" => return out,
            "(" => out.push(Node::All(parse_seq(toks, pos))),
            "||" => {
                if toks.get(*pos) == Some(&"(") {
                    *pos += 1;
                }
                out.push(Node::AnyOf(parse_seq(toks, pos)));
            }
            _ if t.ends_with('?') => {
                if toks.get(*pos) == Some(&"(") {
                    *pos += 1;
                }
                let (negate, flag) = match t.strip_prefix('!') {
                    Some(f) => (true, f),
                    None => (false, t),
                };
                out.push(Node::Use {
                    flag: flag.trim_end_matches('?').to_string(),
                    negate,
                    kids: parse_seq(toks, pos),
                });
            }
            _ => out.push(Node::Atom(t.to_string())),
        }
    }
    out
}

/// Reduce a dep tree to a flat list of atoms (blockers dropped).
///
/// USE conditionals are evaluated against `use_flags`. For `|| ( ... )`
/// groups, the first alternative for which `satisfied` holds is taken,
/// falling back to the first alternative.
pub fn flatten(
    nodes: &[Node],
    use_flags: &HashSet<String>,
    satisfied: &dyn Fn(&[String]) -> bool,
    out: &mut Vec<String>,
) {
    for n in nodes {
        match n {
            Node::Atom(a) => {
                if !a.starts_with('!') {
                    out.push(a.clone());
                }
            }
            Node::All(kids) => flatten(kids, use_flags, satisfied, out),
            Node::Use { flag, negate, kids } => {
                if use_flags.contains(flag) != *negate {
                    flatten(kids, use_flags, satisfied, out);
                }
            }
            Node::AnyOf(kids) => {
                let alts: Vec<Vec<String>> = kids
                    .iter()
                    .map(|k| {
                        let mut v = Vec::new();
                        flatten(std::slice::from_ref(k), use_flags, satisfied, &mut v);
                        v
                    })
                    .filter(|v| !v.is_empty())
                    .collect();
                if let Some(pick) = alts.iter().find(|a| satisfied(a)).or(alts.first()) {
                    out.extend(pick.iter().cloned());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flatten_use_and_any() {
        let nodes = parse("a/a pam? ( sys-libs/pam ) !pam? ( virtual/libcrypt:= ) || ( x/one x/two ) !b/block");
        let mut use_flags = HashSet::new();
        use_flags.insert("pam".to_string());
        let mut out = Vec::new();
        flatten(&nodes, &use_flags, &|alt| alt[0] == "x/two", &mut out);
        assert_eq!(out, ["a/a", "sys-libs/pam", "x/two"]);
    }
}
