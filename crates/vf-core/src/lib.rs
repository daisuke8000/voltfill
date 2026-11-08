//! Form fill: match a document's label/value hypotheses to form fields and
//! return exact source values with Evidence. Values always come from the
//! document itself: its text layer, or with [`acroform`] the `/V` of its
//! AcroForm fields; results are evaluation-only. [`assist`] optionally lets
//! Jev choose among an ambiguous field's own candidates.
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]
pub mod acroform;
pub mod assist;
pub mod fill;
