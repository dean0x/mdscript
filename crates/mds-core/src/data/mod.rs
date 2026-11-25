//! Data source input boundaries.

mod convert;
pub(crate) mod decl;
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the request identity is the first caller")
)]
pub(crate) mod digest;
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the step engine is the first caller")
)]
pub(crate) mod encode;
pub(crate) mod graph;
pub(crate) mod path;
mod refs;
pub(crate) mod resolve;
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the step engine is the first caller")
)]
pub(crate) mod url;

pub(crate) use convert::{convert, Accept, DataLabel};

#[cfg(test)]
mod convert_tests;
#[cfg(test)]
mod corpus_tests;
#[cfg(test)]
mod digest_tests;
#[cfg(test)]
mod encode_tests;
#[cfg(test)]
mod url_tests;
