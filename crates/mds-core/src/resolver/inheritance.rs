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
        Node::Text(_) | Node::EscapedBrace => 0,
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
pub(super) fn check_child_only_blocks(body: &[Node], ctx: &ModuleCtx<'_>) -> Result<(), MdsError> {
    for node in body {
        match node {
            Node::Block(_) => {}
            Node::Text(t) if t.text.trim().is_empty() => {}
            other => {
                let offset = node_offset(other);
                debug_assert!(
                    ctx.source.is_char_boundary(offset),
                    "check_child_only_blocks: offset {offset} is not a UTF-8 char boundary \
                     in source (len={})",
                    ctx.source.len()
                );
                let line_len = ctx.source[offset..]
                    .find('\n')
                    .unwrap_or(ctx.source[offset..].len());
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
        file: Arc::from(ctx.file_str),
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
/// (the root-base file).
///
/// Mirrors the missing-block `debug_assert!`/fallback from `splice_skeleton` so both
/// consumers (splice + validate) have identical coverage. This is the single shared
/// walk that prevents text/messages mode validate paths from drifting (PF-004).
pub(super) fn spliced_regions<'a>(
    skeleton: &'a [Node],
    effective_blocks: &'a IndexMap<String, EffectiveBlock>,
    skeleton_origin: &'a Origin,
) -> Vec<(&'a [Node], &'a Origin)> {
    let mut regions = Vec::with_capacity(skeleton.len());
    for node in skeleton {
        if let Node::Block(skeleton_block) = node {
            if let Some(eff_block) = effective_blocks.get(&skeleton_block.name) {
                // Block body with its own origin (the winning override file's source).
                regions.push((eff_block.node.body.as_slice(), &eff_block.origin));
            } else {
                // Every block in the skeleton must have an effective_blocks entry.
                // A missing entry is a compiler bug (apply_block_overrides was not called).
                debug_assert!(
                    false,
                    "spliced_regions: block '{}' in skeleton has no effective_blocks entry — \
                     this is a compiler bug (apply_block_overrides was not called for this skeleton)",
                    skeleton_block.name
                );
                // Release build: fall back to the skeleton's own default body (same origin
                // as the skeleton since this is the base's own node).
                regions.push((skeleton_block.body.as_slice(), skeleton_origin));
            }
        } else {
            // Non-block skeleton nodes: validated against the skeleton origin.
            regions.push((std::slice::from_ref(node), skeleton_origin));
        }
    }
    regions
}

/// Splice the skeleton body by replacing each `@block` placeholder with its
/// effective body (from the `effective_blocks` override map).
pub(super) fn splice_skeleton(
    skeleton: &[Node],
    effective_blocks: &IndexMap<String, EffectiveBlock>,
    skeleton_origin: &Origin,
) -> Vec<Node> {
    spliced_regions(skeleton, effective_blocks, skeleton_origin)
        .into_iter()
        .flat_map(|(nodes, _)| nodes.iter().cloned())
        .collect()
}
