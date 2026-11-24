//! Data source input boundaries.

mod convert;
pub(crate) mod decl;
pub(crate) mod graph;
pub(crate) mod path;
mod refs;
pub(crate) mod resolve;

pub(crate) use convert::{convert, Accept, DataLabel};

#[cfg(test)]
mod convert_tests;
#[cfg(test)]
mod corpus_tests;
