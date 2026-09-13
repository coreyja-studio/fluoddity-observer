use super::*;
use crate::threads::ThreadFetcher;
use axum::{Json, Router, body::Body, extract::State, http::StatusCode, routing::get};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use tower::ServiceExt;
use tracing::{
    Subscriber,
    field::{Field, Visit},
    instrument::WithSubscriber,
};
use tracing_subscriber::{
    Layer,
    layer::{Context, SubscriberExt},
    registry::LookupSpan,
};

#[derive(Debug)]
struct Record {
    name: &'static str,
    target: &'static str,
    fields: BTreeMap<String, Value>,
    ancestors: Vec<&'static str>,
}

#[derive(Default)]
struct Fields(BTreeMap<String, Value>);
impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.insert(field.name().into(), json!(value));
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0.insert(field.name().into(), json!(value));
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0
            .insert(field.name().into(), json!(format!("{value:?}")));
    }
}

#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<Record>>>);
impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for Capture {
    fn on_close(&self, id: tracing::Id, ctx: Context<'_, S>) {
        let span = ctx.span(&id).unwrap();
        self.0.lock().unwrap().push(Record {
            name: "span_closed",
            target: span.metadata().target(),
            fields: BTreeMap::from([("span.name".into(), json!(span.name()))]),
            ancestors: vec![],
        });
    }
    fn on_new_span(
        &self,
        attrs: &tracing::span::Attributes<'_>,
        id: &tracing::Id,
        ctx: Context<'_, S>,
    ) {
        let mut fields = Fields::default();
        attrs.record(&mut fields);
        let ancestors = ctx
            .span(id)
            .unwrap()
            .scope()
            .from_root()
            .map(|span| span.name())
            .collect();
        self.0.lock().unwrap().push(Record {
            name: attrs.metadata().name(),
            target: attrs.metadata().target(),
            fields: fields.0,
            ancestors,
        });
    }
    fn on_event(&self, event: &tracing::Event<'_>, ctx: Context<'_, S>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        let ancestors = ctx
            .event_scope(event)
            .map(|scope| scope.from_root().map(|span| span.name()).collect())
            .unwrap_or_default();
        self.0.lock().unwrap().push(Record {
            name: event.metadata().name(),
            target: event.metadata().target(),
            fields: fields.0,
            ancestors,
        });
    }
}

async fn bluesky(status: StatusCode) -> (String, Arc<AtomicUsize>, tokio::task::JoinHandle<()>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let routes = Router::new()
        .route(
            "/xrpc/com.atproto.identity.resolveHandle",
            get(|State(calls): State<Arc<AtomicUsize>>| async move {
                calls.fetch_add(1, Ordering::Relaxed);
                Json(json!({"did":"did:plc:test"}))
            }),
        )
        .route(
            "/xrpc/app.bsky.feed.getPostThread",
            get(move |State(calls): State<Arc<AtomicUsize>>| async move {
                calls.fetch_add(1, Ordering::Relaxed);
                (
                    status,
                    Json(json!({"thread":{"post":{
                        "uri":"at://did:plc:test/app.bsky.feed.post/root",
                        "author":{"did":"did:plc:test","handle":"test.bsky"},
                        "record":{"text":"A room"}
                    }}})),
                )
            }),
        )
        .with_state(calls.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(listener, routes).await.unwrap();
    });
    (url, calls, task)
}

#[tokio::test]
async fn request_traces_link_cache_decisions_and_bluesky_calls_without_credentials() {
    let (url, calls, mock) = bluesky(StatusCode::OK).await;
    let fetcher = Arc::new(ThreadFetcher::test_client(url));
    let router = Router::new()
        .route(
            "/room/{author}/{rkey}",
            get(|State(fetcher): State<Arc<ThreadFetcher>>| async move {
                fetcher
                    .fetch("test.bsky", "root", "did:plc:test", "test.bsky")
                    .await
                    .unwrap()
                    .unwrap();
                StatusCode::OK
            }),
        )
        .layer(
            tower_http::trace::TraceLayer::new_for_http()
                .make_span_with(RequestSpan)
                .on_response(cja::server::trace::Tracer),
        )
        .with_state(fetcher);
    let capture = Capture::default();
    let subscriber = tracing_subscriber::registry().with(capture.clone());
    async {
        for _ in 0..2 {
            let request = Request::builder()
                .uri("/room/test.bsky/root?code=query-secret")
                .header("authorization", "Bearer header-secret")
                .header("cookie", "cookie-secret")
                .body(Body::empty())
                .unwrap();
            let response = router.clone().oneshot(request).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            drop(response);
        }
    }
    .with_subscriber(subscriber)
    .await;
    mock.abort();
    assert_eq!(
        calls.load(Ordering::Relaxed),
        2,
        "second request must reuse the cache"
    );
    let records = capture.0.lock().unwrap();
    let requests: Vec<_> = records
        .iter()
        .filter(|r| r.name == "server.request")
        .collect();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        records
            .iter()
            .filter(|r| r.fields.get("span.name") == Some(&json!("server.request")))
            .count(),
        2
    );
    assert_eq!(
        records
            .iter()
            .filter(|r| r.fields.get("span.name") == Some(&json!("gallery.bluesky.request")))
            .count(),
        2
    );
    for request in requests {
        assert_eq!(request.fields["http.route"], "/room/{author}/{rkey}");
        assert_eq!(request.fields["otel.name"], "GET /room/{author}/{rkey}");
    }
    let cache: Vec<_> = records
        .iter()
        .filter_map(|r| r.fields.get("cache.outcome"))
        .collect();
    assert_eq!(cache, [&json!("miss"), &json!("hit")]);
    let dependencies: Vec<_> = records
        .iter()
        .filter(|r| r.name == "gallery.bluesky.request")
        .collect();
    assert_eq!(dependencies.len(), 2);
    for dependency in dependencies {
        assert!(dependency.ancestors.contains(&"server.request"));
        assert!(dependency.ancestors.contains(&"gallery.thread.fetch"));
        assert!(dependency.fields.contains_key("rpc.method"));
    }
    let statuses: Vec<_> = records
        .iter()
        .filter(|r| r.target == "cja::server::trace" && r.fields.contains_key("status"))
        .collect();
    assert_eq!(statuses.len(), 2);
    for status in statuses {
        assert_eq!(status.fields["status"], 200);
    }
    let evidence = format!("{records:?}");
    for secret in ["query-secret", "header-secret", "cookie-secret"] {
        assert!(!evidence.contains(secret));
    }
}

#[tokio::test]
async fn dependency_failures_and_missing_threads_retain_their_status_evidence() {
    for (status, failed) in [
        (StatusCode::NOT_FOUND, false),
        (StatusCode::INTERNAL_SERVER_ERROR, true),
    ] {
        let (url, _, mock) = bluesky(status).await;
        let fetcher = ThreadFetcher::test_client(url);
        let capture = Capture::default();
        let subscriber = tracing_subscriber::registry().with(capture.clone());
        let result = fetcher
            .fetch("did:plc:test", "root", "did:plc:test", "test.bsky")
            .with_subscriber(subscriber)
            .await;
        mock.abort();
        assert_eq!(result.is_err(), failed);
        if !failed {
            assert!(result.unwrap().is_none());
        }
        let records = capture.0.lock().unwrap();
        assert!(records.iter().any(|r| r.fields.get("http.response.status_code") == Some(&json!(status.as_u16()))));
        if failed {
            assert!(records.iter().any(|r| r.fields.contains_key("error")));
        }
    }
}

#[test]
fn operations_dashboard_references_declared_metrics() {
    let metrics = metrics().unwrap();
    let dashboard = serde_json::to_value(dashboard().unwrap()).unwrap();
    assert_eq!(metrics.len(), 8);
    let names: Vec<_> = metrics.iter().map(|m| m.id.as_str()).collect();
    let mut references = 0;
    for section in dashboard["sections"].as_array().unwrap() {
        for item in section["items"].as_array().unwrap() {
            if let Some(id) = item.get("query_id").and_then(Value::as_str) {
                assert!(names.contains(&id), "{id}");
                references += 1;
            }
        }
    }
    assert_eq!(references, 8);
}
