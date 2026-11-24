//! Data source input boundaries.

mod convert;
pub(crate) mod decl;
#[cfg_attr(not(test), expect(dead_code, reason = "S8 and S9 call the data graph"))]
pub(crate) mod graph;
pub(crate) mod path;
mod refs;

pub(crate) use convert::DataLabel;
#[cfg_attr(
    not(test),
    expect(
        unused_imports,
        reason = "the data resolver calls the converter in a later phase"
    )
)]
pub(crate) use convert::{convert, Accept};

#[cfg(test)]
mod convert_tests;
#[cfg(test)]
mod corpus_tests;
