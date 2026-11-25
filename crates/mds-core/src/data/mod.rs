//! Data source input boundaries.

mod convert;
pub(crate) mod decl;
pub(crate) mod digest;
pub(crate) mod encode;
pub(crate) mod graph;
pub(crate) mod path;
pub(crate) mod policy;
mod refs;
pub(crate) mod request;
pub(crate) mod resolve;
pub(crate) mod step;
pub(crate) mod url;

pub(crate) use convert::{convert, Accept, DataInput, DataLabel};

#[cfg(test)]
mod convert_tests;
#[cfg(test)]
mod corpus_tests;
#[cfg(test)]
mod digest_tests;
#[cfg(test)]
mod encode_tests;
#[cfg(test)]
mod policy_tests;
#[cfg(test)]
mod request_tests;
#[cfg(test)]
mod step_tests;
#[cfg(test)]
mod url_tests;
