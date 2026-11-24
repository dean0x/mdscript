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

#[derive(Default)]
pub(crate) struct State {
    pub(crate) compile: bool,
    pub(crate) pending: bool,
    pub(crate) files: IndexMap<String, Arc<Value>>,
    pub(crate) entries: usize,
    pub(crate) bytes: usize,
}

/// Per-file declarations and names, in a shared chain from child to base.
pub(crate) type Chain = Option<Arc<Declarations>>;

pub(crate) struct DeclarationSite<'a> {
    pub(crate) key: &'a str,
    pub(crate) module: &'a str,
    pub(crate) dir: &'a str,
    pub(crate) is_md: bool,
    pub(crate) preflight: bool,
}

pub(crate) enum FrontmatterNames<'a> {
    Parsed {
        mapping: Option<&'a Mapping>,
        is_md: bool,
    },
    Preflight {
        merged: bool,
    },
}

pub(crate) struct Declarations {
    base: Chain,
    key: String,
    module: String,
    dir: String,
    entries: Vec<decl::Entry>,
    keys: Vec<String>,
    aliases: Vec<String>,
    has_data: bool,
}

impl Declarations {
    /// Charge this link as a temporary. Its caller owns any retained charge.
    pub(crate) fn extend(
        base: &Chain,
        site: DeclarationSite<'_>,
        mapping: Option<&Mapping>,
        imports: &[FrontmatterImport],
        body: &[Node],
        mem: &mut MemBudget,
    ) -> Result<Chain, MdsError> {
        let mut next = base.as_deref();
        for _ in 0..=crate::limits::MAX_MODULE_COUNT {
            let Some(source) = next else { break };
            if source.key == site.key {
                return Err(MdsError::circular_import(site.module.to_owned()));
            }
            next = source.base.as_deref();
        }
        if next.is_some() {
            return Err(crate::error::module_count_exceeded());
        }
        let keys = || {
            mapping
                .filter(|_| site.preflight)
                .into_iter()
                .flatten()
                .filter_map(|(key, _)| {
                    let name = key.as_str()?;
                    is_scope_key(name, site.is_md).then_some(name)
                })
        };
        let aliases = || {
            imports
                .iter()
                .filter_map(|import| match import {
                    FrontmatterImport::Alias { alias, .. } => Some(alias.as_str()),
                    _ => None,
                })
                .chain(body.iter().filter_map(|node| match node {
                    Node::Import(ImportDirective::Alias { alias, .. }) => Some(alias.as_str()),
                    _ => None,
                }))
        };
        let data = mapping.and_then(|mapping| mapping.get("data"));
        if data.is_none() && keys().next().is_none() && aliases().next().is_none() {
            return Ok(base.clone());
        }
        let names = keys().chain(aliases()).try_fold(0usize, |sum, name| {
            sum.checked_add(2 * size_of::<String>())?
                .checked_add(name.len())
        });
        let overhead = size_of::<Self>() + 2 * size_of::<usize>();
        mem.charge(
            Part::Temp,
            names.and_then(|n| {
                n.checked_add(overhead)?
                    .checked_add(site.key.len())?
                    .checked_add(site.module.len())?
                    .checked_add(site.dir.len())
            }),
        )?;
        let before = mem.buffers;
        let entries = data.map_or_else(
            || Ok(Vec::new()),
            |field| decl::parse_declarations(field, site.module, mem),
        )?;
        mem.transfer(Part::Buffers, Part::Temp, mem.buffers - before);
        Ok(Some(Arc::new(Self {
            base: base.clone(),
            key: site.key.to_owned(),
            module: site.module.to_owned(),
            dir: site.dir.to_owned(),
            entries,
            keys: sorted_names(keys()),
            aliases: sorted_names(aliases()),
            has_data: data.is_some() || Self::contains_data(base),
        })))
    }

    pub(crate) fn contains_data(chain: &Chain) -> bool {
        chain.as_ref().is_some_and(|source| source.has_data)
    }

    /// Validate the winning entries against the variables and aliases of this scope.
    pub(crate) fn with_graph<T>(
        chain: &Chain,
        frontmatter: FrontmatterNames<'_>,
        runtime: &VarLayer,
        mem: &mut MemBudget,
        use_graph: impl FnOnce(usize, &graph::Graph<'_>, &mut MemBudget) -> Result<T, MdsError>,
    ) -> Result<Option<T>, MdsError> {
        let mut use_graph = Some(use_graph);
        let mut output = None;
        Self::check_graph(
            chain,
            frontmatter,
            runtime,
            mem,
            &mut |count, graph, mem| {
                let use_graph = use_graph.take().ok_or_else(MdsError::internal)?;
                output = Some(use_graph(count, graph, mem)?);
                Ok(())
            },
        )?;
        Ok(output)
    }

    /// The body of `with_graph`. It is not generic, so the WASM build carries one
    /// copy of the graph check for all callers.
    #[inline(never)]
    fn check_graph(
        chain: &Chain,
        frontmatter: FrontmatterNames<'_>,
        runtime: &VarLayer,
        mem: &mut MemBudget,
        use_graph: &mut dyn FnMut(usize, &graph::Graph<'_>, &mut MemBudget) -> Result<(), MdsError>,
    ) -> Result<(), MdsError> {
        if !Self::contains_data(chain) {
            return Ok(());
        }
        let mut sources = [None; crate::limits::MAX_MODULE_COUNT + 1];
        let mut next = chain.as_deref();
        let mut count = 0;
        for slot in &mut sources {
            let Some(source) = next else { break };
            *slot = Some(source);
            next = source.base.as_deref();
            count += 1;
        }
        if next.is_some() {
            return Err(crate::error::module_count_exceeded());
        }
        let sources = &sources[..count];
        let mark = mem.mark();
        let result = (|| {
            let mut entries = IndexMap::new();
            for source in sources.iter().rev().flatten() {
                for (ordinal, entry) in source.entries.iter().enumerate() {
                    let node = graph::Node {
                        entry,
                        position: (&source.key, ordinal),
                        module: &source.module,
                        dir: &source.dir,
                    };
                    if let Some(previous) = entries.get_mut(entry.name.as_str()) {
                        *previous = node;
                    } else {
                        if entries.len() == crate::limits::MAX_DATA_ENTRIES_PER_COMPILE {
                            return Err(graph::entry_limit());
                        }
                        mem.charge(
                            Part::Temp,
                            Some(4 * size_of::<(&str, graph::Node<'_>, usize)>()),
                        )?;
                        entries.insert(entry.name.as_str(), node);
                    }
                }
            }
            let has_frontmatter = |name: &str| match frontmatter {
                FrontmatterNames::Parsed { mapping, is_md } => {
                    is_scope_key(name, is_md)
                        && mapping.is_some_and(|mapping| mapping.contains_key(name))
                }
                FrontmatterNames::Preflight { merged } => {
                    (!merged || !crate::resolver::RESERVED_MERGE_KEYS.contains(&name))
                        && sources
                            .iter()
                            .flatten()
                            .any(|source| has_name(&source.keys, name))
                }
            };
            decl::check_collisions(entries.values().map(|node| node.entry), |name| {
                has_frontmatter(name)
                    || sources
                        .iter()
                        .flatten()
                        .any(|source| has_name(&source.aliases, name))
            })?;
            mem.charge(
                Part::Temp,
                entries.len().checked_mul(size_of::<graph::Node<'_>>()),
            )?;
            let nodes: Vec<_> = entries.into_values().collect();
            let graph = graph::analyze(&nodes, has_frontmatter, |name| runtime.contains_key(name))?;
            use_graph(nodes.len(), &graph, mem)
        })();
        mem.release_to(mark);
        result
    }
}

fn is_scope_key(name: &str, is_md: bool) -> bool {
    !(matches!(name, "data" | "imports" | crate::lexer::INDENTED_DIRECTIVES)
        || is_md && name == "type")
}

fn sorted_names<'a>(names: impl Iterator<Item = &'a str>) -> Vec<String> {
    let mut names: Vec<_> = names.map(str::to_owned).collect();
    names.sort_unstable();
    names
}

fn has_name(names: &[String], name: &str) -> bool {
    names.binary_search_by(|key| key.as_str().cmp(name)).is_ok()
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
        let display = crate::source_path::display_path_for(fs, key);
        convert(
            text.as_bytes(),
            accept,
            DataLabel {
                file: Some(&display),
                ..label
            },
            mem,
        )
    })();
    mem.release_to(mark);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::NativeFs;

    #[test]
    fn native_data_parse_error_names_the_project_relative_file() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("item.yml");
        let fs = NativeFs::new();
        let dir = fs.anchor_base_dir(root.path().to_str().unwrap()).unwrap();
        let label = DataLabel {
            module: "entry.mds",
            entry: "item",
            file: None,
        };
        std::fs::write(&path, "[").unwrap();
        let (key, accept) = canonical_file(&fs, &dir, "./item.yml", label).unwrap();
        let read = || {
            read_file(&fs, &key, accept, label, &mut MemBudget::default(), |_| {
                Ok(())
            })
        };
        let error = read().unwrap_err().serialize();
        assert_eq!(error.code, "mds::yaml");
        assert!(
            error
                .message
                .contains(r#"data file "item.yml", entry "item""#),
            "{}",
            error.message
        );
        assert!(!error.message.contains(&key), "{}", error.message);
        std::fs::write(&path, "42").unwrap();
        assert_eq!(read().unwrap(), Value::Number(42.0));
    }
}
