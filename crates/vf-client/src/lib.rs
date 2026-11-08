//! Bounded Jev 1.14 wire adapter. No credentials or network are read automatically.
//! The contract is based on public documentation and synthetic fixtures, not live validation.
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used))]
pub mod http;
pub mod json;
pub mod multipart;
pub mod planning;
pub mod transport;

mod protocol;

pub use protocol::{
    Answer, Asset, Content, Diagnostic, Error, Limits, MODEL, Nullable, Question, QuestionSpec,
    Rect, Request, RequestDraft, Response, ResponseBody, Usage, build_request, parse_request,
    parse_response, supports_images, validate_model,
};

pub use http::{Endpoint, HttpTransport};
pub use multipart::PngImage;
pub use transport::{ExecutionConfig, Policy, RecordingTransport, ResponseRecord, Secret};
