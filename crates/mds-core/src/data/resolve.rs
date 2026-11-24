//! Check scope declarations and bind local data through one read and conversion path.

use std::sync::Arc;

use indexmap::IndexMap;
use serde_yaml_ng::Mapping;

use super::{convert, decl, graph, path, Accept, DataLabel};
use crate::ast::{ImportDirective, Node};
use crate::evaluator::budget::{MemBudget, Part};
use crate::limits::MAX_DATA_FILE_BYTES;
use crate::resolver::FrontmatterImport;
use crate::scope::VarLayer;
use crate::{FileSystem, MdsError, Value};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum Mode {
    #[default]
    NoReads,
    Collect,
    Read,
}

#[derive(Default)]
pub(crate) struct State {
    pub(crate) mode: Mode,
    pub(crate) files: IndexMap<String, Arc<Value>>,
    pub(crate) entries: usize,
    pub(crate) bytes: usize,
}

pub(crate) struct Declarations<'a> {
    pub(crate) module: &'a str,
    pub(crate) mapping: &'a Mapping,
    pub(crate) imports: &'a [FrontmatterImport],
    pub(crate) body: &'a [Node],
    pub(crate) runtime: &'a VarLayer,
    pub(crate) is_md: bool,
}

impl Declarations<'_> {
    /// Keep declarations and the graph live only during this operation.
    pub(crate) fn with_graph<T>(
        self,
        mem: &mut MemBudget,
        use_graph: impl FnOnce(usize, &graph::Graph<'_>, &mut MemBudget) -> Result<T, MdsError>,
    ) -> Result<T, MdsError> {
        let mark = mem.mark();
        let before = mem.buffers;
        let entries = self.mapping.get("data").map_or_else(
            || Ok(Vec::new()),
            |field| decl::parse_declarations(field, self.module, mem),
        )?;
        let declaration_bytes = mem.buffers - before;
        let result = (|| {
            let has_frontmatter = |name: &str| {
                !(matches!(name, "data" | "imports" | crate::lexer::INDENTED_DIRECTIVES)
                    || self.is_md && name == "type")
                    && self.mapping.contains_key(name)
            };
            decl::check_collisions(&entries, |name| {
                self.mapping.contains_key(name)
                    || self.imports.iter().any(|import| {
                        matches!(import, FrontmatterImport::Alias { alias, .. } if alias == name)
                    })
                    || self.body.iter().any(|node| {
                        matches!(node, Node::Import(ImportDirective::Alias { alias, .. }) if alias == name)
                    })
            })?;
            mem.charge(
                Part::Temp,
                entries.len().checked_mul(size_of::<graph::Node<'_>>()),
            )?;
            let nodes: Vec<_> = entries
                .iter()
                .map(|entry| graph::Node {
                    entry,
                    module: self.module,
                })
                .collect();
            let graph = graph::analyze(&nodes, has_frontmatter, |name| {
                self.runtime.contains_key(name)
            })?;
            use_graph(entries.len(), &graph, mem)
        })();
        mem.release_to(mark);
        mem.release_buffers(declaration_bytes);
        result
    }
}

pub(crate) fn canonical_file(
    fs: &dyn FileSystem,
    dir: &str,
    written: &str,
    label: DataLabel<'_>,
) -> Result<(String, Accept), MdsError> {
    let key = path::normalize(fs, dir, written)?;
    let accept = if key.ends_with(".json") {
        Accept::Json
    } else if key.ends_with(".yaml") || key.ends_with(".yml") {
        Accept::Yaml
    } else {
        return Err(label.error(
            None,
            "resolved data file must have a .json, .yaml or .yml extension",
        ));
    };
    // A memo entry has one parser. Two YAML spellings have the same format.
    if written.ends_with(".json") != matches!(accept, Accept::Json) {
        return Err(label.error(None, "resolved data file has a different format"));
    }
    Ok((key, accept))
}

pub(crate) fn read_file(
    fs: &dyn FileSystem,
    key: &str,
    accept: Accept,
    label: DataLabel<'_>,
    mem: &mut MemBudget,
    admit_bytes: impl FnOnce(usize) -> Result<(), MdsError>,
) -> Result<Value, MdsError> {
    let mark = mem.mark();
    let result = (|| {
        // The backend bounds the returned text. Reserve one read buffer first.
        mem.charge(Part::Temp, Some(MAX_DATA_FILE_BYTES as usize))?;
        let text = fs.read_data(key)?;
        mem.release_to(mark);
        mem.charge(Part::Temp, Some(text.capacity()))?;
        admit_bytes(text.len())?;
        convert(text.as_bytes(), accept, label, mem)
    })();
    mem.release_to(mark);
    result
}
