//! cs-actor integration — back a route with a BEAM-style actor.
//!
//! An [`ActorHandler`] is a [`Service`] that, on every request,
//!
//! 1. constructs a [`WebMessage`] envelope (request + reply
//!    oneshot),
//! 2. sends it to a target [`ActorRef`] via the actor's mailbox,
//!    and
//! 3. awaits the reply (with a configurable timeout — slow
//!    actors return 504).
//!
//! The target actor consumes `Message::User`, downcasts the
//! payload to `WebMessage`, builds a `Response`, and ships it
//! through `reply`. [`spawn_handler_actor`] wraps that loop so
//! users don't write the receive boilerplate.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use cs_actor::{ActorRef, ActorSystem, Message, Payload};
use futures_util::future::BoxFuture;
use http::StatusCode;
use tokio::sync::oneshot;

use crate::{response, ArcService, Request, Response, Service};

/// Envelope sent to an actor by an [`ActorHandler`]. The reply
/// slot is a `Mutex<Option<…>>` because tokio's `oneshot::Sender`
/// is `Send` but not `Sync` — the Mutex lets the envelope cross
/// the `Arc<dyn Any + Send + Sync>` payload boundary.
pub struct WebMessage {
    pub req: Request,
    reply: Mutex<Option<oneshot::Sender<Response>>>,
}

impl WebMessage {
    /// Build a new envelope from a request and a reply channel.
    /// Used by integration points that ship a `WebMessage` to a
    /// downstream actor (cs-runtime's payload bridge, or third-
    /// party adapters that wrap a non-tower transport).
    pub fn new(req: Request, reply: oneshot::Sender<Response>) -> Self {
        Self {
            req,
            reply: Mutex::new(Some(reply)),
        }
    }

    /// Take the reply sender out of the envelope. Returns `None`
    /// on the second call — actors that reply twice silently
    /// drop the second response.
    pub fn take_reply(&self) -> Option<oneshot::Sender<Response>> {
        self.reply.lock().expect("reply lock poisoned").take()
    }

    /// Convenience: ship a [`Response`] back through the reply
    /// slot. Returns true if the reply was sent (the requester
    /// was still waiting), false if it was already taken or the
    /// requester dropped.
    pub fn reply_with(&self, resp: Response) -> bool {
        if let Some(tx) = self.take_reply() {
            tx.send(resp).is_ok()
        } else {
            false
        }
    }
}

/// Service that delegates each request to an actor.
pub struct ActorHandler {
    target: ActorRef,
    timeout: Duration,
}

impl ActorHandler {
    /// Build a handler that ships requests to `target` and waits
    /// up to `timeout` for the reply. Pick `timeout` based on
    /// expected handler latency — too short, requests fail with
    /// 504; too long, slow handlers tie up server task slots.
    pub fn new(target: ActorRef, timeout: Duration) -> Self {
        Self { target, timeout }
    }

    /// Wrap as an [`ArcService`] suitable for [`crate::Router`].
    pub fn into_service(self) -> ArcService {
        Arc::new(self)
    }
}

impl Service for ActorHandler {
    fn call(&self, req: Request) -> BoxFuture<'static, Response> {
        let (tx, rx) = oneshot::channel();
        let envelope = Arc::new(WebMessage::new(req, tx));
        let payload: Payload = envelope;
        let send_result = self.target.send(payload);
        let timeout = self.timeout;
        Box::pin(async move {
            if let Err(e) = send_result {
                return response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    format!("actor send failed: {e}"),
                );
            }
            match tokio::time::timeout(timeout, rx).await {
                Ok(Ok(resp)) => resp,
                Ok(Err(_)) => response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "actor dropped reply channel",
                ),
                Err(_) => response(StatusCode::GATEWAY_TIMEOUT, "actor reply timeout"),
            }
        })
    }
}

/// Spawn a long-running actor that runs a request → response
/// closure on every received [`WebMessage`]. Returns the
/// [`ActorRef`] so the caller can build an [`ActorHandler`]
/// against it.
///
/// Closures run inside the actor's task — they cannot block,
/// but they can `await` freely (each tokio yield is the same as
/// the `(yield)` Scheme primop on the BEAM side).
pub fn spawn_handler_actor<F, Fut>(system: &ActorSystem, body: F) -> ActorRef
where
    F: Fn(Request) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = Response> + Send + 'static,
{
    let body = Arc::new(body);
    system.spawn_async(move |mut actor| {
        let body = Arc::clone(&body);
        async move {
            while let Some(msg) = actor.receive_async().await {
                let Message::User(payload) = msg else {
                    // Exit / Down — actor terminates on Exit;
                    // Down has been handled by cs-actor's
                    // monitor/link machinery already.
                    break;
                };
                let Ok(envelope) = payload.downcast::<WebMessage>() else {
                    // Foreign payload — drop it; the unrelated
                    // sender already lost the receiver.
                    continue;
                };
                // Request body is `Bytes` (cheap clone) and parts
                // are small, so cloning matches the BEAM
                // copy-on-send semantics without measurable cost.
                let req = envelope.req.clone();
                let resp = body(req).await;
                envelope.reply_with(resp);
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ok;
    use bytes::Bytes;
    use cs_actor::ActorSystem;

    fn req(path: &str) -> Request {
        http::Request::builder()
            .uri(path)
            .body(Bytes::new())
            .unwrap()
    }

    // These tests need a real multi-thread tokio runtime so the
    // actor task can run alongside the test task. The actor
    // system itself owns its runtime; we drive the assertion side
    // via `Handle::current()` from within the system's runtime.

    #[test]
    fn actor_handler_round_trip() {
        let system = ActorSystem::new();
        let actor_ref = spawn_handler_actor(&system, |r| async move {
            ok(format!("you asked for {}", r.uri().path()))
        });
        let svc = ActorHandler::new(actor_ref, Duration::from_secs(2)).into_service();

        // Run the assertion inside the actor system's runtime so
        // tokio::time::timeout has a reactor.
        let resp = block_on_system(&system, async move { svc.call(req("/hello")).await });
        assert_eq!(resp.status(), StatusCode::OK);
        assert_eq!(resp.body(), &Bytes::from_static(b"you asked for /hello"));
    }

    #[test]
    fn actor_handler_timeout_returns_504() {
        let system = ActorSystem::new();
        let actor_ref = spawn_handler_actor(&system, |_| async {
            tokio::time::sleep(Duration::from_secs(5)).await;
            ok("never")
        });
        let svc = ActorHandler::new(actor_ref, Duration::from_millis(50)).into_service();
        let resp = block_on_system(&system, async move { svc.call(req("/slow")).await });
        assert_eq!(resp.status(), StatusCode::GATEWAY_TIMEOUT);
    }

    #[test]
    fn actor_handler_concurrent_requests() {
        let system = ActorSystem::new();
        let actor_ref = spawn_handler_actor(&system, |r| async move {
            tokio::task::yield_now().await;
            ok(format!("{}", r.uri().path()))
        });
        let svc = ActorHandler::new(actor_ref, Duration::from_secs(2)).into_service();

        let results = block_on_system(&system, async move {
            let mut handles = Vec::new();
            for i in 0..16 {
                let svc = Arc::clone(&svc);
                let path = format!("/req-{i}");
                handles.push(tokio::spawn(async move {
                    let r = svc.call(req(&path)).await;
                    (r.status(), r.body().clone())
                }));
            }
            let mut out = Vec::new();
            for h in handles {
                out.push(h.await.unwrap());
            }
            out
        });
        for (i, (status, body)) in results.into_iter().enumerate() {
            assert_eq!(status, StatusCode::OK);
            let expected = format!("/req-{i}");
            assert_eq!(body, Bytes::from(expected));
        }
    }

    /// Run a future on the actor system's runtime. cs-actor's
    /// `ActorSystem::new()` owns its tokio runtime; tests need to
    /// piggyback on it so spawned actors and the test driver
    /// share the same reactor.
    fn block_on_system<F: std::future::Future + Send>(system: &ActorSystem, fut: F) -> F::Output
    where
        F::Output: Send,
    {
        // We can't expose ActorSystem's runtime handle directly,
        // but we can spawn a task and block on its JoinHandle via
        // a stdlib channel from a fresh single-threaded runtime.
        // For tests, spinning up a sibling runtime is fine.
        let driver = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("build sibling runtime");
        let _ = system; // hold the system alive; actors spawn into its runtime
        driver.block_on(fut)
    }
}
