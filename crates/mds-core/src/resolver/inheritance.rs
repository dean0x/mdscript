//! Pure helper functions for `@extends` / `@block` template inheritance.
//!
//! These are free functions extracted from `resolver.rs` that operate only on
//! shared types (`Origin`, `EffectiveBlock`, `ModuleCtx`, `Node`) without
//! requiring any `&mut self` access.

use std::collections::HashSet;
use std::sync::Arc;

use indexmap::IndexMap;

use crate::ast::Node;
use crate::error::MdsError;

use super::{EffectiveBlock, ModuleCtx, Origin};

/// Build the initial `effective_blocks` map for a non-extending module.
///
/// Filters `body` for `@block` nodes whose names appear in `block_names`, then
/// wraps each in an `EffectiveBlock` stamped with the module's `origin`.
pub(super) fn seed_effective_blocks(
    body: &[Node],
    block_names: &HashSet<String>,
    origin: &Origin,
) -> IndexMap<String, EffectiveBlock> {
    body.iter()
        .filter_map(|n| {
            if let Node::Block(b) = n {
                block_names.contains(&b.name).then(|| {
                    (
                        b.name.clone(),
                        EffectiveBlock {
                            node: Arc::new(b.clone()),
                            origin: origin.clone(),
                        },
                    )
                })
            } else {
                None
            }
        })
        .collect()
}

/// Return the source offset of `node`, or `0` for node types that carry no offset.
pub(super) fn node_offset(node: &Node) -> usize {
    match node {
        Node::Text(_) => 0,
        Node::EscapedBrace { offset } => *offset,
        Node::Interpolation(i) => i.offset,
        Node::If(b) => b.offset,
        Node::For(b) => b.offset,
        Node::Define(b) => b.offset,
        Node::Import(i) => match i {
            crate::ast::ImportDirective::Alias { offset, .. }
            | crate::ast::ImportDirective::Merge { offset, .. }
            | crate::ast::ImportDirective::Selective { offset, .. } => *offset,
        },
        Node::Export(_) => 0,
        Node::Include(i) => i.offset,
        Node::Message(m) => m.offset,
        Node::Block(b) => b.offset,
    }
}

/// Validate that the body of a child-only extending template contains only
/// `@block` overrides and optional whitespace-only text nodes.
///
/// Returns `Err(mds::extends)` on the first stray node.
///
/// A non-boundary offset degrades to a zero-length span (#220).
pub(super) fn check_child_only_blocks(body: &[Node], ctx: &ModuleCtx<'_>) -> Result<(), MdsError> {
    for node in body {
        match node {
            Node::Block(_) => {}
            Node::Text(t) if t.text.trim().is_empty() => {}
            other => {
                let offset = node_offset(other);
                // Degrade rather than slice on a non-boundary offset (#220): a
                // bad offset here is a defect in `node_offset` pairing, not
                // something a template can produce. See `line_len_at`.
                let line_len = super::line_len_at(ctx.source, offset);
                return Err(MdsError::extends_error_at(
                    "an extending template may contain only @block overrides",
                    ctx.file_str,
                    ctx.source,
                    offset,
                    line_len,
                ));
            }
        }
    }
    Ok(())
}

/// Clone `parent_blocks` and apply the `@block` overrides from `body`.
///
/// Clones the parent map first so the cached parent entry is never mutated
/// (diamond-inheritance correctness, F5).  Returns `Err(mds::extends)` if a
/// child block name is not present in the parent map (E4: unknown override).
///
/// Each override entry is stamped with the CURRENT `ctx`'s `Origin` (the overriding
/// file). Inherited entries keep their existing origin (the file where the winning
/// definition last came from). This ensures diagnostics attribute to the correct file
/// (Risk #1 from the plan: origin must follow the winning override).
///
/// **Perf rule:** `override_origin` is built ONCE outside the loop and `Arc::clone`d
/// into each stamped entry — never `Arc::from(ctx.source)` inside the loop (P3).
pub(super) fn apply_block_overrides(
    parent_blocks: &IndexMap<String, EffectiveBlock>,
    body: &[Node],
    ctx: &ModuleCtx<'_>,
) -> Result<IndexMap<String, EffectiveBlock>, MdsError> {
    let mut blocks = parent_blocks.clone();

    // Build the override origin ONCE — O(1) Arc bumps per override node.
    let override_origin = Origin {
        file: Arc::from(ctx.key),
        display: Arc::from(ctx.file_str),
        source: Arc::from(ctx.source),
    };

    for node in body {
        if let Node::Block(b) = node {
            // Decision #6 / F4/E4: child may only override blocks declared by the root base.
            if !blocks.contains_key(&b.name) {
                return Err(MdsError::extends_error_at(
                    "only the root template may declare @block placeholders",
                    ctx.file_str,
                    ctx.source,
                    b.offset,
                    b.name.len(),
                ));
            }
            // Most-derived wins; stamp with current file's origin.
            blocks.insert(
                b.name.clone(),
                EffectiveBlock {
                    node: Arc::new(b.clone()),
                    origin: override_origin.clone(),
                },
            );
        }
    }
    Ok(blocks)
}

/// Iterate over spliced regions of the skeleton, each paired with its `Origin`.
///
/// A `Node::Block` placeholder in the skeleton yields the effective block's body
/// nodes and the block's own `Origin` (the file whose offsets those nodes index into).
/// Any other skeleton node yields a single-element slice and the `skeleton_origin`
/// (the root-base file), so between-block spacing (skeleton `Text` nodes) is preserved
/// (decision #9, F11). The regions, in order, are the child's whole inherited body.
///
/// Every skeleton `@block` is required to have an `effective_blocks` entry, and that
/// requirement is enforced in release builds too (#220). This is the single shared walk
/// behind validation and every evaluation mode (text, messages, source-mapped), so they
/// cannot drift apart (PF-004).
///
/// # Panics
///
/// Panics when a skeleton `@block` has no `effective_blocks` entry. That pairing is
/// built from the skeleton itself, so only a defect in the override-map construction
/// can produce it — no template input can.
pub(super) fn spliced_regions<'a>(
    skeleton: &'a [Node],
    effective_blocks: &'a IndexMap<String, EffectiveBlock>,
    skeleton_origin: &'a Origin,
) -> Vec<(&'a [Node], &'a Origin)> {
    let mut regions = Vec::with_capacity(skeleton.len());
    for node in skeleton {
        if let Node::Block(skeleton_block) = node {
            let eff_block = effective_blocks.get(&skeleton_block.name);
            // Enforced in release too, not `debug_assert!` (#220): the map is built
            // from this very skeleton by `seed_effective_blocks` /
            // `apply_block_overrides`, so a skeleton `@block` with no entry can only
            // be a defect in that pairing — no template input can produce it. The
            // old release fallback spliced the base default in silence, i.e. dropped
            // a child's override from the compiled output with no diagnostic.
            assert!(
                eff_block.is_some(),
                "skeleton @block has no effective_blocks entry: the override map was not \
                 built for this skeleton, so a child's @block override would be silently \
                 dropped from the compiled output"
            );
            if let Some(eff_block) = eff_block {
                // Block body with its own origin (the winning override file's source).
                regions.push((eff_block.node.body.as_slice(), &eff_block.origin));
            }
        } else {
            // Non-block skeleton nodes: validated against the skeleton origin.
            regions.push((std::slice::from_ref(node), skeleton_origin));
        }
    }
    regions
}
