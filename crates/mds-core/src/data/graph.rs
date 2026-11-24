//! Check the data dependencies of one scope without I/O.

use super::decl::{Entry, Source};
use super::DataLabel;
use crate::limits::{MAX_DATA_ENTRIES_PER_COMPILE, MAX_DATA_LEVELS};
use crate::{sanitize_control_chars_wire, MdsError};

#[derive(Clone, Copy, Debug)]
pub(crate) struct Node<'a> {
    pub(crate) entry: &'a Entry,
    /// The declaring key and the entry's position in that declaration.
    pub(crate) position: (&'a str, usize),
    /// Project-relative label of the module that declares the entry.
    pub(crate) module: &'a str,
    pub(crate) dir: &'a str,
}

impl Node<'_> {
    fn label(&self) -> DataLabel<'_> {
        DataLabel {
            module: self.module,
            entry: &self.entry.name,
            file: None,
        }
    }
}

#[derive(Debug)]
pub(crate) struct Graph<'a> {
    nodes: &'a [Node<'a>],
    replayed: NodeSet,
}

impl<'a> Graph<'a> {
    /// All winning entries in declaration order, replayed entries included.
    pub(crate) fn nodes(&self) -> impl Iterator<Item = Node<'a>> + '_ {
        self.nodes.iter().copied()
    }

    /// Keep declaration order. Do not read entries that replay satisfies.
    pub(crate) fn unreplayed(&self) -> impl Iterator<Item = Node<'a>> + '_ {
        self.nodes
            .iter()
            .copied()
            .enumerate()
            .filter_map(|(index, node)| (!self.replayed.contains(index)).then_some(node))
    }

    /// Append these pairs in module first-resolution order in the collect pass.
    pub(crate) fn frontier(&self) -> impl Iterator<Item = (&'a str, &'a str)> + '_ {
        self.unreplayed()
            .filter(|node| matches!(node.entry.source, Source::Url(_)))
            .map(|node| (node.module, node.entry.name.as_str()))
    }
}

/// Analyze parsed declarations in declaration order. For `@extends`, pass the
/// merged entries and final scope. Check name collisions before this call.
#[must_use = "data graph analysis can fail"]
pub(crate) fn analyze<'a>(
    nodes: &'a [Node<'a>],
    has_frontmatter: impl Fn(&str) -> bool,
    has_runtime: impl Fn(&str) -> bool,
) -> Result<Graph<'a>, MdsError> {
    if nodes.len() > MAX_DATA_ENTRIES_PER_COMPILE {
        return Err(entry_limit());
    }

    // Sort only the lookup table. The graph and frontier keep the input order.
    let mut indices: [usize; MAX_DATA_ENTRIES_PER_COMPILE] = std::array::from_fn(|index| index);
    let indices = &mut indices[..nodes.len()];
    let name_at = |index: usize| nodes[index].entry.name.as_str();
    sort_entries_by_key(indices, |&index| name_at(index))?;

    let mut replayed = NodeSet::default();
    let mut waiting = [None; MAX_DATA_ENTRIES_PER_COMPILE];
    let waiting = &mut waiting[..nodes.len()];
    for (index, node) in nodes.iter().enumerate() {
        let mut deps = NodeSet::default();
        if has_runtime(&node.entry.name) {
            // Replay satisfies the node without use of its outgoing references.
            // The declaration parser already checked the source form.
            replayed.insert(index);
        } else if let Source::Url(refs) = &node.entry.source {
            for path in refs {
                let root = path.split_once('.').map_or(path.as_str(), |(root, _)| root);
                if let Ok(slot) = indices.binary_search_by_key(&root, |&i| name_at(i)) {
                    deps.insert(indices[slot]);
                } else if !has_frontmatter(root) && !has_runtime(root) {
                    return Err(node.label().error(
                        None,
                        &format!(
                            "unknown data reference name \"{}\"",
                            sanitize_control_chars_wire(root)
                        ),
                    ));
                }
            }
        }
        waiting[index] = Some(deps);
    }
    check_levels(nodes, waiting)?;
    Ok(Graph { nodes, replayed })
}

pub(crate) fn entry_limit() -> MdsError {
    MdsError::resource_limit(format!(
        "too many data entries (max {MAX_DATA_ENTRIES_PER_COMPILE} per compile)"
    ))
}

/// Sort at most `MAX_DATA_ENTRIES_PER_COMPILE` items by key. Items with equal keys
/// keep their input order. A longer list is an internal error and stays as it is.
///
/// This is an insertion sort. The lists are short, and the WASM build then
/// carries no general sort code for the data checks.
pub(crate) fn sort_entries_by_key<T, K: Ord>(
    items: &mut [T],
    key: impl Fn(&T) -> K,
) -> Result<(), MdsError> {
    if items.len() > MAX_DATA_ENTRIES_PER_COMPILE {
        return Err(MdsError::internal());
    }
    for end in 1..items.len() {
        // Move the item at `end` down past each item with a greater key.
        for at in (1..=end).rev() {
            if key(&items[at - 1]) <= key(&items[at]) {
                break;
            }
            items.swap(at - 1, at);
        }
    }
    Ok(())
}

fn check_levels(nodes: &[Node<'_>], waiting: &mut [Option<NodeSet>]) -> Result<(), MdsError> {
    // Each pass removes one whole level. At most one pass per node is necessary.
    for level in 1..=nodes.len() {
        let mut ready = NodeSet::default();
        for (index, deps) in waiting.iter_mut().enumerate() {
            if deps.as_ref().is_some_and(NodeSet::is_empty) {
                ready.insert(index);
                *deps = None;
            }
        }
        let Some(blocked) = waiting.iter().position(Option::is_some) else {
            if level > MAX_DATA_LEVELS {
                return Err(MdsError::resource_limit(format!(
                    "data dependency graph exceeds {MAX_DATA_LEVELS} levels"
                )));
            }
            break;
        };
        if ready.is_empty() {
            return Err(nodes[blocked]
                .label()
                .error(None, "blocked by a data dependency cycle"));
        }
        for deps in waiting.iter_mut().flatten() {
            deps.remove_all(&ready);
        }
    }
    Ok(())
}

// The graph uses fixed-size sets. Repeated references cost only one edge each.
const WORDS: usize = MAX_DATA_ENTRIES_PER_COMPILE.div_ceil(u64::BITS as usize);

#[derive(Clone, Copy, Debug, Default)]
struct NodeSet([u64; WORDS]);

impl NodeSet {
    fn insert(&mut self, index: usize) {
        self.0[index / 64] |= 1 << (index % 64);
    }

    fn contains(&self, index: usize) -> bool {
        self.0[index / 64] & (1 << (index % 64)) != 0
    }

    fn is_empty(&self) -> bool {
        self.0.iter().all(|&word| word == 0)
    }

    fn remove_all(&mut self, other: &Self) {
        for (word, other_word) in self.0.iter_mut().zip(other.0) {
            *word &= !other_word;
        }
    }
}

#[cfg(test)]
#[path = "graph_tests.rs"]
mod tests;
