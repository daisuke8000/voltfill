//! Form fill: match a document's label/value hypotheses to form fields and
//! return exact source values with Evidence. Values always come from the
//! document's own text layer; results are evaluation-only.
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]
pub mod fill;
