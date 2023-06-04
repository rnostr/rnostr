use crate::Result;
use prometheus::{
    Encoder, Histogram, HistogramOpts, IntCounter, IntCounterVec, IntGauge, Opts, Registry,
    TextEncoder,
};

#[derive(Clone, Debug)]
pub struct Metrics {
    pub query_sub: Histogram,        // response time of successful subscriptions
    pub query_db: Histogram,         // individual database query execution time
    pub db_connections: IntGauge,    // database connections in use
    pub write_events: Histogram,     // response time of event writes
    pub sent_events: IntCounterVec,  // count of events sent to clients
    pub connections: IntCounter,     // count of websocket connections
    pub disconnects: IntCounterVec,  // client disconnects
    pub query_aborts: IntCounterVec, // count of queries aborted by server
    pub cmd_req: IntCounter,         // count of REQ commands received
    pub cmd_event: IntCounter,       // count of EVENT commands received
    pub cmd_close: IntCounter,       // count of CLOSE commands received
    pub cmd_auth: IntCounter,        // count of AUTH commands received
}

impl Metrics {
    pub fn render(registry: &Registry) -> Result<Vec<u8>> {
        let mut buffer = vec![];
        let encoder = TextEncoder::new();
        let metric_families = registry.gather();
        encoder.encode(&metric_families, &mut buffer)?;
        Ok(buffer)
    }

    pub fn create() -> Result<(Registry, Metrics)> {
        // setup prometheus registry
        let registry = Registry::new();

        let query_sub = Histogram::with_opts(HistogramOpts::new(
            "nostr_query_seconds",
            "Subscription response times",
        ))?;

        let query_db = Histogram::with_opts(HistogramOpts::new(
            "nostr_filter_seconds",
            "Filter SQL query times",
        ))?;
        let write_events = Histogram::with_opts(HistogramOpts::new(
            "nostr_events_write_seconds",
            "Event writing response times",
        ))?;
        let sent_events = IntCounterVec::new(
            Opts::new("nostr_events_sent_total", "Events sent to clients"),
            vec!["source"].as_slice(),
        )?;
        let connections =
            IntCounter::with_opts(Opts::new("nostr_connections_total", "New connections"))?;
        let db_connections = IntGauge::with_opts(Opts::new(
            "nostr_db_connections",
            "Active database connections",
        ))?;
        let query_aborts = IntCounterVec::new(
            Opts::new("nostr_query_abort_total", "Aborted queries"),
            vec!["reason"].as_slice(),
        )?;
        let cmd_req = IntCounter::with_opts(Opts::new("nostr_cmd_req_total", "REQ commands"))?;
        let cmd_event =
            IntCounter::with_opts(Opts::new("nostr_cmd_event_total", "EVENT commands"))?;
        let cmd_close =
            IntCounter::with_opts(Opts::new("nostr_cmd_close_total", "CLOSE commands"))?;
        let cmd_auth = IntCounter::with_opts(Opts::new("nostr_cmd_auth_total", "AUTH commands"))?;
        let disconnects = IntCounterVec::new(
            Opts::new("nostr_disconnects_total", "Client disconnects"),
            vec!["reason"].as_slice(),
        )?;
        registry.register(Box::new(query_sub.clone()))?;
        registry.register(Box::new(query_db.clone()))?;
        registry.register(Box::new(write_events.clone()))?;
        registry.register(Box::new(sent_events.clone()))?;
        registry.register(Box::new(connections.clone()))?;
        registry.register(Box::new(db_connections.clone()))?;
        registry.register(Box::new(query_aborts.clone()))?;
        registry.register(Box::new(cmd_req.clone()))?;
        registry.register(Box::new(cmd_event.clone()))?;
        registry.register(Box::new(cmd_close.clone()))?;
        registry.register(Box::new(cmd_auth.clone()))?;
        registry.register(Box::new(disconnects.clone()))?;
        let metrics = Metrics {
            query_sub,
            query_db,
            write_events,
            sent_events,
            connections,
            db_connections,
            disconnects,
            query_aborts,
            cmd_req,
            cmd_event,
            cmd_close,
            cmd_auth,
        };
        Ok((registry, metrics))
    }
}
