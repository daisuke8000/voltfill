//! Extraction-specific observer compatibility over the Jev execution boundary.
use super::{
    Limits, Request, Response,
    multipart::{Body, PngImage},
};
pub use jev_client::transport::*;

pub struct RequestScope<'a> {
    pub field_id: &'a str,
    pub page: u32,
}
pub struct AttemptContext<'a> {
    pub slot: u32,
    pub field_id: &'a str,
    pub page: u32,
    pub question_ids: &'a [String],
    pub input: &'a InputSummary,
}
pub trait AttemptObserver {
    fn waiting(&mut self, _duration_ms: u64) -> Result<(), Failure> {
        Ok(())
    }
    fn before_send(
        &mut self,
        _body: &Body,
        _budget: &AttemptBudget,
        _timeout_cap_ms: u64,
        _context: &AttemptContext<'_>,
    ) -> Result<(), Failure> {
        Ok(())
    }
    fn after_send(
        &mut self,
        _attempt: &Attempt,
        _response: Option<&Response>,
        _budget: &AttemptBudget,
        _context: &AttemptContext<'_>,
    ) -> Result<(), Failure> {
        Ok(())
    }
}
pub struct NoopObserver;
impl AttemptObserver for NoopObserver {}

#[allow(clippy::too_many_arguments)]
pub fn execute<T: Transport, C: Clock>(
    request: &Request,
    images: &[PngImage],
    boundary: &str,
    secret: Option<&Secret>,
    policy: Policy,
    limits: Limits,
    transport: &mut T,
    clock: &mut C,
) -> Execution {
    execute_observed(
        request,
        images,
        boundary,
        secret,
        policy,
        limits,
        transport,
        clock,
        &mut AttemptBudget::default(),
        &mut NoopObserver,
    )
}
/// The single retry implementation used by RemoteEvaluator and its callers.
#[allow(clippy::too_many_arguments)]
pub fn execute_observed<T: Transport, C: Clock, O: AttemptObserver>(
    request: &Request,
    images: &[PngImage],
    boundary: &str,
    secret: Option<&Secret>,
    policy: Policy,
    limits: Limits,
    transport: &mut T,
    clock: &mut C,
    budget: &mut AttemptBudget,
    observer: &mut O,
) -> Execution {
    execute_in_scope(
        request,
        images,
        boundary,
        secret,
        policy,
        limits,
        transport,
        clock,
        budget,
        observer,
        RequestScope {
            field_id: "",
            page: 0,
        },
    )
}
#[allow(clippy::too_many_arguments)]
pub fn execute_in_scope<T: Transport, C: Clock, O: AttemptObserver>(
    request: &Request,
    images: &[PngImage],
    boundary: &str,
    secret: Option<&Secret>,
    policy: Policy,
    limits: Limits,
    transport: &mut T,
    clock: &mut C,
    budget: &mut AttemptBudget,
    observer: &mut O,
    scope: RequestScope<'_>,
) -> Execution {
    jev_client::transport::execute_observed(
        request,
        images,
        boundary,
        secret,
        policy,
        limits,
        transport,
        clock,
        budget,
        &mut ScopedObserver {
            inner: observer,
            scope,
        },
    )
}

struct ScopedObserver<'a, 's, O> {
    inner: &'a mut O,
    scope: RequestScope<'s>,
}
impl<O: AttemptObserver> jev_client::transport::AttemptObserver for ScopedObserver<'_, '_, O> {
    fn waiting(&mut self, duration_ms: u64) -> Result<(), Failure> {
        self.inner.waiting(duration_ms)
    }
    fn before_send(
        &mut self,
        body: &Body,
        budget: &AttemptBudget,
        timeout_cap_ms: u64,
        context: &jev_client::transport::AttemptContext<'_>,
    ) -> Result<(), Failure> {
        self.inner.before_send(
            body,
            budget,
            timeout_cap_ms,
            &AttemptContext {
                slot: context.slot,
                field_id: self.scope.field_id,
                page: self.scope.page,
                question_ids: context.question_ids,
                input: context.input,
            },
        )
    }
    fn after_send(
        &mut self,
        attempt: &Attempt,
        response: Option<&Response>,
        budget: &AttemptBudget,
        context: &jev_client::transport::AttemptContext<'_>,
    ) -> Result<(), Failure> {
        self.inner.after_send(
            attempt,
            response,
            budget,
            &AttemptContext {
                slot: context.slot,
                field_id: self.scope.field_id,
                page: self.scope.page,
                question_ids: context.question_ids,
                input: context.input,
            },
        )
    }
}
