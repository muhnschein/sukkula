//! LocalSend v2 (F-LS), over the upstream `localsend` crate.
//!
//! Placeholder: the adapter answers every call with `Unavailable` until it
//! is implemented. See `crate::adapter::Adapter` for the contract and
//! `crate::ctx` for the receive path every adapter follows.

use std::sync::Arc;

use sukkula_core::Protocol;

use crate::adapter::{Adapter, BoxFuture, Outgoing, unavailable};
use crate::api::{ErrorInfo, SendTarget, TransferId};
use crate::ctx::Ctx;

/// The adapter.
#[must_use]
pub fn adapter(ctx: Arc<Ctx>) -> Arc<dyn Adapter> {
    Arc::new(LocalSendAdapter { _ctx: ctx })
}

struct LocalSendAdapter {
    _ctx: Arc<Ctx>,
}

impl Adapter for LocalSendAdapter {
    fn protocol(&self) -> Protocol {
        Protocol::LocalSend
    }

    fn start_receiving(&self) -> BoxFuture<'_, Result<(), ErrorInfo>> {
        Box::pin(async { Err(unavailable("LocalSend")) })
    }

    fn stop_receiving(&self) -> BoxFuture<'_, ()> {
        Box::pin(async {})
    }

    fn send(
        &self,
        _target: SendTarget,
        _items: Vec<Outgoing>,
    ) -> BoxFuture<'_, Result<TransferId, ErrorInfo>> {
        Box::pin(async { Err(unavailable("LocalSend")) })
    }
}
