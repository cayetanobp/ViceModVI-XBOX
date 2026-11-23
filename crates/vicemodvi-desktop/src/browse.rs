//! Source tree, catalog rows and search filtering.

use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

use vicemodvi_core::{
    util::{contains_ignore_case, eq_ignore_case, parse_hex_u64},
    Catalog,
};

pub struct Node {
    pub id: usize,
    pub title: String,
    /// Full path, shown as a tooltip.
    pub path: String,
    /// Path relative to the root, shown under the tree.
    pub display: String,
    pub sources: HashSet<String>,
    pub children: Vec<Node>,
    pub parent: Option<usize>,
    pub folder: bool,
}

pub struct Row {
    pub asset: usize,
    pub source_path: String,
    pub summary: String,
}

pub struct Tree {
    pub root: Node,
    /// Source id to the id of its file node.
    pub source_nodes: HashMap<String, usize>,
    /// Node id to parent node id.
    pub parents: HashMap<usize, usize>,
}

impl Tree {
    pub fn find(&self, id: usize) -> Option<&Node> {
        fn walk(node: &Node, id: usize) -> Option<&Node> {
            if node.id == id {
                return Some(node);
            }
            node.children.iter().find_map(|c| walk(c, id))
        }
        walk(&self.root, id)
    }

    pub fn ancestors(&self, id: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let mut current = id;
        while let Some(&parent) = self.parents.get(&current) {
            out.push(parent);
            current = parent;
        }
        out
    }
}

fn file_name(path: &str) -> String {
    Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.to_string())
}

/// Deepest folder that contains every source.
fn common_folder(catalog: &Catalog) -> Option<String> {
    let first = catalog.sources.first()?;
    let mut root = Path::new(&first.path).parent().map(Path::to_path_buf);
    while let Some(folder) = root {
        let text = folder.to_string_lossy().into_owned();
        let prefix = if text.ends_with(std::path::MAIN_SEPARATOR) { text.clone() } else { format!("{text}{}", std::path::MAIN_SEPARATOR) };
        let all = catalog.sources.iter().all(|s| s.path.len() >= prefix.len() && eq_ignore_case(&s.path[..prefix.len()], &prefix));
        if all {
            return Some(text);
        }
        root = folder.parent().map(Path::to_path_buf);
    }
    None
}

fn relative(root: &str, path: &str) -> String {
    Path::new(path)
        .strip_prefix(root)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_string())
}

/// Builds the folder tree and the catalog rows.
pub fn build(catalog: &Catalog, folder: Option<&Path>) -> (Tree, Vec<Row>) {
    let root_path = folder.map(|f| f.to_string_lossy().into_owned()).or_else(|| common_folder(catalog));
    let title = match &root_path {
        None => "All sources".to_string(),
        Some(root) => {
            let trimmed = root.trim_end_matches(['\\', '/']);
            let name = file_name(trimmed);
            if name.is_empty() { root.clone() } else { name }
        }
    };
    let mut next = 0usize;
    let mut new_node = |title: String, path: String, display: String, folder: bool| {
        next += 1;
        Node { id: next, title, path, display, sources: HashSet::new(), children: Vec::new(), parent: None, folder }
    };
    let mut root = new_node(title.clone(), root_path.clone().unwrap_or_else(|| "All sources".into()), title, true);
    let mut sources: Vec<_> = catalog.sources.iter().collect();
    sources.sort_by_key(|s| s.path.to_uppercase());
    let mut locations = HashMap::new();
    let mut source_nodes = HashMap::new();
    for source in sources {
        root.sources.insert(source.id.clone());
        let location = match &root_path {
            None => source.path.clone(),
            Some(root) => relative(root, &source.path),
        };
        locations.insert(source.id.clone(), location.clone());
        // Walk or create the folder nodes for this source.
        let mut owner = &mut root;
        if let Some(root_text) = &root_path {
            let parts: Vec<&str> = location.split(['\\', '/']).collect();
            let mut directory = PathBuf::from(root_text);
            for part in &parts[..parts.len().saturating_sub(1)] {
                directory.push(part);
                let text = directory.to_string_lossy().into_owned();
                let position = owner.children.iter().position(|c| c.folder && eq_ignore_case(&c.path, &text));
                let index = match position {
                    Some(i) => i,
                    None => {
                        let display = relative(root_text, &text);
                        owner.children.push(new_node(part.to_string(), text, display, true));
                        owner.children.len() - 1
                    }
                };
                owner = &mut owner.children[index];
                owner.sources.insert(source.id.clone());
            }
        }
        let title = if root_path.is_none() { source.path.clone() } else { file_name(&source.path) };
        let mut file = new_node(title, source.path.clone(), location, false);
        file.sources.insert(source.id.clone());
        source_nodes.insert(source.id.clone(), file.id);
        owner.children.push(file);
    }
    let mut parents = HashMap::new();
    fn link(node: &mut Node, parents: &mut HashMap<usize, usize>) {
        for child in &mut node.children {
            child.parent = Some(node.id);
            parents.insert(child.id, node.id);
            link(child, parents);
        }
    }
    link(&mut root, &mut parents);
    let paths: HashMap<&str, &str> = catalog.sources.iter().map(|s| (s.id.as_str(), s.path.as_str())).collect();
    let rows = catalog
        .assets
        .iter()
        .enumerate()
        .map(|(i, asset)| Row {
            asset: i,
            source_path: paths.get(asset.source_id.as_str()).copied().unwrap_or_default().to_string(),
            summary: format!("{} · {}", asset.kind, locations.get(&asset.source_id).map(String::as_str).unwrap_or_default()),
        })
        .collect();
    (Tree { root, source_nodes, parents }, rows)
}

/// Rows of the visible sources that match the type and the search text.
/// The text matches names, ids, source paths, the type, and hexadecimal keys,
/// hashes or offsets.
pub fn filter(catalog: &Catalog, rows: &[Row], visible: &HashSet<String>, kind: Option<&str>, query: &str) -> Vec<usize> {
    let query = query.trim();
    let value = parse_hex_u64(query);
    let key = value.map(|v| format!("{v:016X}"));
    rows.iter()
        .enumerate()
        .filter(|(_, row)| {
            let asset = &catalog.assets[row.asset];
            visible.contains(&asset.source_id)
                && kind.is_none_or(|k| asset.kind == k)
                && (query.is_empty()
                    || contains_ignore_case(&asset.name, query)
                    || contains_ignore_case(&asset.id, query)
                    || contains_ignore_case(&row.source_path, query)
                    || eq_ignore_case(&asset.kind, query)
                    || key.as_ref().is_some_and(|k| {
                        asset.native_key.as_ref() == Some(k)
                            || asset.name_hash.as_ref() == Some(k)
                            || value.is_some_and(|v| v <= i64::MAX as u64 && asset.range.as_ref().map(|r| r.offset) == Some(v as i64))
                    }))
        })
        .map(|(i, _)| i)
        .collect()
}

const INVALID: &[char] = &['"', '<', '>', '|', ':', '*', '?', '\\', '/'];

fn invalid(c: char) -> bool {
    (c as u32) < 32 || INVALID.contains(&c)
}

/// File name stem for an exported asset: its name and its id.
pub fn export_name(name: &str, id: &str) -> String {
    let clean: String = name.chars().map(|c| if c == '.' || invalid(c) { '_' } else { c }).take(80).collect();
    let identity: String = id.chars().map(|c| if invalid(c) { '_' } else { c }).collect();
    format!("{clean}_{identity}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_names_are_file_safe() {
        assert_eq!(export_name("a.b:c", "0:ttpf:1"), "a_b_c_0_ttpf_1");
    }
}
