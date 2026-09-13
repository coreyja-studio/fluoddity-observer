//! Request and dependency evidence for diagnosing public-page latency.
use axum::{extract::MatchedPath, http::Request};
use eyes_subscriber::{
    AggregateFunction as Agg, DashboardItem as Item, DashboardSection as Section, NamedDashboard,
    NamedMetric, NamedMetricBuilder as Metric,
};
use tower_http::trace::MakeSpan;

#[derive(Clone, Copy, Debug)]
pub struct RequestSpan;

impl<B> MakeSpan<B> for RequestSpan {
    fn make_span(&mut self, request: &Request<B>) -> tracing::Span {
        let route = request
            .extensions()
            .get::<MatchedPath>()
            .map_or("unmatched", MatchedPath::as_str);
        // The OAuth callback carries credentials in its query string. Keep
        // route labels at creation, but never record query strings or headers.
        tracing::info_span!(
            "server.request",
            otel.name = %format_args!("{} {route}", request.method()),
            kind = "server",
            http.route = route,
            http.request.method = %request.method(),
            url.path = request.uri().path(),
        )
    }
}

pub fn git_sha() -> Option<&'static str> {
    option_env!("PCG_GIT_SHA").filter(|sha| !sha.is_empty())
}

fn count(id: &str, title: &str, path: &str, value: &str) -> Result<Metric, String> {
    Metric::new(id, Agg::Count, None)?
        .display_name(title)
        .filter_eq(path, value)
}

fn latency(id: &str, title: &str, path: &str, value: &str) -> Result<NamedMetric, String> {
    Metric::new(id, Agg::P95, Some("duration"))?
        .display_name(title)
        .filter_eq(path, value)?
        .unit("µs")
        .build()
}

pub fn metrics() -> Result<Vec<NamedMetric>, String> {
    Ok(vec![
        count("http.requests", "Requests", "semantic_kind", "http.request")?
            .unit("requests")
            .build()?,
        count(
            "http.requests.series",
            "Request volume",
            "semantic_kind",
            "http.request",
        )?
        .unit("requests")
        .time_bucket(300)
        .build()?,
        latency(
            "http.latency.p95",
            "Request latency · p95",
            "semantic_kind",
            "http.request",
        )?,
        count(
            "http.routes",
            "Requests by route",
            "semantic_kind",
            "http.request",
        )?
        .group_by("fields[\"http.route\"]")?
        .unit("requests")
        .build()?,
        latency(
            "threads.latency.p95",
            "Room fetch latency · p95",
            "name",
            "gallery.thread.fetch",
        )?,
        count(
            "threads.cache",
            "Room cache outcomes",
            "fields.event_type",
            "gallery.thread_cache",
        )?
        .group_by("fields[\"cache.outcome\"]")?
        .unit("lookups")
        .build()?,
        latency(
            "bluesky.latency.p95",
            "Bluesky request latency · p95",
            "name",
            "gallery.bluesky.request",
        )?,
        count(
            "bluesky.requests",
            "Bluesky calls by method",
            "name",
            "gallery.bluesky.request",
        )?
        .group_by("fields[\"rpc.method\"]")?
        .unit("requests")
        .build()?,
    ])
}

pub fn dashboard() -> Result<NamedDashboard, String> {
    Ok(
        NamedDashboard::new("gallery-operations", "Gallery operations")?
            .description("Public-page traffic, room cache behavior and Bluesky dependency latency.")
            .default_range_seconds(86_400)
            .section(
                Section::new()
                    .title("At a glance")
                    .item(Item::health_summary())
                    .item(Item::stat("http.requests"))
                    .item(Item::stat("http.latency.p95")),
            )
            .section(
                Section::new()
                    .title("Requests")
                    .item(Item::time_series("http.requests.series"))
                    .item(Item::table("http.routes")),
            )
            .section(
                Section::new()
                    .title("Rooms and Bluesky")
                    .item(Item::stat("threads.latency.p95"))
                    .item(Item::stat("bluesky.latency.p95"))
                    .item(Item::table("threads.cache"))
                    .item(Item::table("bluesky.requests")),
            ),
    )
}

#[cfg(test)]
mod tests;
