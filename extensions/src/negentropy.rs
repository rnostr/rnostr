//! [NIP-77](https://github.com/nostr-protocol/nips/blob/master/77.md) negentropy set reconciliation.
//!
//! `NEG-OPEN` snapshots the `(created_at, id)` pairs a filter matches, then
//! `NEG-MSG` rounds reconcile that snapshot against the client's own set. The
//! relay is always the non-initiator: it answers every message the client
//! sends and lets the client decide when the exchange is finished.

use actix::{ActorFutureExt as _, AsyncContext as _, WrapFuture as _};
use negentropy::{Id, Negentropy, NegentropyStorageVector};
use nostr_relay::{
    db::{Db, Filter},
    duration::NonZeroDuration,
    message::{ClientMessage, IncomingMessage, NegOpen, OutgoingMessage, NEG_MSG},
    Error, Extension, ExtensionMessageResult, Session, Setting, SettingUpdate,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::HashMap,
    num::NonZeroUsize,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, OnceLock,
    },
    time::{Duration, Instant},
};
use tokio::sync::{watch, OwnedSemaphorePermit, Semaphore};
use tracing::{debug, warn};

/// Negentropy rejects a frame size limit between 1 and 4095.
const MIN_FRAME_SIZE_LIMIT: u64 = 4096;
const OPEN_ADMISSION_TIMEOUT: Duration = Duration::from_millis(100);

#[derive(Deserialize, Serialize, Debug, Clone)]
#[serde(default, deny_unknown_fields)]
pub struct NegentropySetting {
    #[serde(deserialize_with = "nostr_relay::setting::deserialize_scalar")]
    pub enabled: bool,

    /// Largest event set a single `NEG-OPEN` may reconcile over; a filter
    /// matching more is rejected rather than reconciled against a subset.
    #[serde(deserialize_with = "nostr_relay::setting::deserialize_scalar")]
    pub max_records: usize,

    /// Concurrent reconciliations and in-flight jobs, fixed per connection.
    /// Opens wait up to 100 ms for a job slot; excess open sessions are refused.
    #[serde(deserialize_with = "nostr_relay::setting::deserialize_scalar")]
    pub max_sessions: usize,

    /// Positive relay-wide job limit, fixed at startup; changes require restart.
    /// Opens over this relay-wide limit are refused; rounds wait until admitted or cancelled.
    #[serde(deserialize_with = "nostr_relay::setting::deserialize_scalar")]
    pub max_jobs: NonZeroUsize,

    /// Maximum snapshot scan duration, also capped by `data.db_query_timeout`.
    pub scan_timeout: NonZeroDuration,

    /// Binary payload bytes; wire size is 2x plus JSON overhead.
    /// `0` disables splitting; otherwise at least 4096.
    #[serde(deserialize_with = "nostr_relay::setting::deserialize_scalar")]
    pub frame_size_limit: u64,

    /// Client idle time, excluding queued or running work. Reloads apply
    /// to existing reconciliations on the next sweep.
    #[serde(deserialize_with = "deserialize_idle_timeout")]
    pub idle_timeout: Option<NonZeroDuration>,

    /// Ceiling on snapshot records across every connection, where
    /// `max_records` and `max_sessions` bound only one. `0` disables it.
    #[serde(deserialize_with = "nostr_relay::setting::deserialize_scalar")]
    pub max_total_records: usize,
}

impl Default for NegentropySetting {
    fn default() -> Self {
        Self {
            enabled: false,
            max_records: 50_000,
            max_sessions: 4,
            max_jobs: NonZeroUsize::new(16).unwrap(),
            scan_timeout: NonZeroDuration::new(Duration::from_secs(5)).unwrap(),
            frame_size_limit: 65536,
            idle_timeout: NonZeroDuration::new(Duration::from_secs(300)),
            // ~40 MiB of snapshots across the whole relay.
            max_total_records: 1_000_000,
        }
    }
}

fn deserialize_idle_timeout<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<NonZeroDuration>, D::Error> {
    #[derive(Deserialize)]
    struct Timeout(#[serde(deserialize_with = "nostr_relay::duration::deserialize")] Duration);
    Option::<Timeout>::deserialize(deserializer)
        .map(|timeout| timeout.and_then(|timeout| NonZeroDuration::new(timeout.0)))
}

/// Snapshot records held in memory across every connection.
#[derive(Clone, Default)]
struct RecordBudget(Arc<AtomicUsize>);

const MEMORY_LIMIT: &str = "blocked: relay is at its reconciliation memory limit";

/// Charges snapshot records against the budget until dropped.
struct RecordLease {
    budget: Arc<AtomicUsize>,
    records: usize,
}

impl Drop for RecordLease {
    fn drop(&mut self) {
        self.budget.fetch_sub(self.records, Ordering::Relaxed);
    }
}

impl RecordBudget {
    /// Reserve `records`, or `None` when that would exceed `max`.
    fn acquire(&self, records: usize, max: usize) -> Option<RecordLease> {
        self.0
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
                current
                    .checked_add(records)
                    .filter(|next| max == 0 || *next <= max)
            })
            .ok()?;
        Some(RecordLease {
            budget: self.0.clone(),
            records,
        })
    }

    /// Advisory check; `acquire` still enforces the budget.
    fn is_full(&self, max: usize) -> bool {
        max != 0 && self.0.load(Ordering::Relaxed) >= max
    }
}

/// A reconciliation in progress, keyed by its subscription id.
struct Reconciliation {
    negentropy: Negentropy<'static, NegentropyStorageVector>,
    _lease: RecordLease,
}

/// Dropping the entry signals cancellation.
struct Entry {
    cancel: watch::Sender<()>,
    last_active: Instant,
    ready: Option<Reconciliation>,
}

type JobResult = Result<(Reconciliation, OutgoingMessage), OutgoingMessage>;

#[derive(Clone, Copy)]
enum JobKind {
    Open,
    Round,
}

enum Admission {
    Granted {
        _permit: OwnedSemaphorePermit,
        _connection: OwnedSemaphorePermit,
    },
    Queued {
        jobs: Arc<Semaphore>,
        connection: Arc<Semaphore>,
        kind: JobKind,
    },
}

impl Admission {
    fn new(
        kind: JobKind,
        jobs: &Arc<Semaphore>,
        connection: &Arc<Semaphore>,
        id: &str,
    ) -> Result<Self, OutgoingMessage> {
        if matches!(kind, JobKind::Open) {
            if let Ok(connection) = connection.clone().try_acquire_owned() {
                let permit = Self::try_open_slot(jobs, id)?;
                return Ok(Self::Granted {
                    _permit: permit,
                    _connection: connection,
                });
            }
        }
        Ok(Self::Queued {
            jobs: jobs.clone(),
            connection: connection.clone(),
            kind,
        })
    }

    fn try_open_slot(
        jobs: &Arc<Semaphore>,
        id: &str,
    ) -> Result<OwnedSemaphorePermit, OutgoingMessage> {
        jobs.clone()
            .try_acquire_owned()
            .map_err(|_| neg_err(id, "blocked: reconciliation work limit reached"))
    }
}

struct Job {
    id: String,
    cancelled: watch::Receiver<()>,
    work: Box<dyn FnOnce() -> JobResult + Send>,
    admission: Admission,
}

impl Job {
    /// Wait for a slot if admission was deferred.
    async fn admit(&mut self) -> Result<(), OutgoingMessage> {
        let Admission::Queued {
            jobs,
            connection,
            kind,
        } = &self.admission
        else {
            return Ok(());
        };
        let (connection, permit) = tokio::select! {
            biased;
            _ = self.cancelled.changed() => {
                return Err(neg_err(&self.id, "closed: reconciliation cancelled"));
            }
            permits = async {
                let connection = if matches!(kind, JobKind::Open) {
                    tokio::time::timeout(OPEN_ADMISSION_TIMEOUT, connection.clone().acquire_owned())
                        .await
                        .map_err(|_| neg_err(&self.id, "blocked: reconciliation connection work limit reached"))?
                } else {
                    connection.clone().acquire_owned().await
                }.expect("connection semaphore is never closed");
                let permit = if matches!(kind, JobKind::Open) {
                    Admission::try_open_slot(jobs, &self.id)?
                } else {
                    jobs.clone().acquire_owned().await.expect("job semaphore is never closed")
                };
                Ok((connection, permit))
            } => permits?,
        };
        self.admission = Admission::Granted {
            _permit: permit,
            _connection: connection,
        };
        Ok(())
    }

    fn run(self) -> JobResult {
        let Self {
            id,
            cancelled,
            work,
            admission: _permit,
        } = self;
        if cancelled.has_changed().is_err() {
            return Err(neg_err(&id, "closed: reconciliation cancelled"));
        }
        work()
    }
}

/// Per-connection reconciliation state, stored on the session.
struct Sessions {
    open: HashMap<String, Entry>,
    jobs: Arc<Semaphore>,
    max_sessions: usize,
    idle_sweep: Option<actix::SpawnHandle>,
}

impl Sessions {
    fn new(max_sessions: usize) -> Self {
        let max_sessions = max_sessions.min(Semaphore::MAX_PERMITS);
        Self {
            open: HashMap::new(),
            jobs: Arc::new(Semaphore::new(max_sessions)),
            max_sessions,
            idle_sweep: None,
        }
    }

    fn close(&mut self, id: &str) {
        self.open.remove(id);
    }

    fn prepare_open(&mut self, id: &str) -> Result<(), OutgoingMessage> {
        self.close(id);
        if self.open.len() >= self.max_sessions {
            return Err(neg_err(id, "blocked: too many concurrent reconciliations"));
        }
        Ok(())
    }

    fn insert(&mut self, id: &str) -> watch::Receiver<()> {
        let (cancel, cancelled) = watch::channel(());
        self.open.insert(
            id.to_owned(),
            Entry {
                cancel,
                last_active: Instant::now(),
                ready: None,
            },
        );
        cancelled
    }

    fn take_round(
        &mut self,
        id: &str,
    ) -> Result<(Reconciliation, watch::Receiver<()>), OutgoingMessage> {
        let Some(entry) = self.open.get_mut(id) else {
            return Err(neg_err(id, "closed: no such reconciliation"));
        };
        let Some(ready) = entry.ready.take() else {
            self.close(id);
            return Err(neg_err(id, "blocked: reconciliation is busy"));
        };
        entry.last_active = Instant::now();
        Ok((ready, entry.cancel.subscribe()))
    }

    fn complete(
        &mut self,
        id: &str,
        cancelled: &watch::Receiver<()>,
        result: JobResult,
    ) -> Option<OutgoingMessage> {
        let entry = self.open.get_mut(id)?;
        if !cancelled.same_channel(&entry.cancel.subscribe()) {
            return None;
        }
        match result {
            Ok((ready, out)) => {
                entry.ready = Some(ready);
                entry.last_active = Instant::now();
                Some(out)
            }
            Err(out) => {
                self.close(id);
                Some(out)
            }
        }
    }

    fn expire(&mut self, timeout: Duration) -> Vec<OutgoingMessage> {
        let now = Instant::now();
        let mut replies = Vec::new();
        self.open.retain(|id, entry| {
            let expired = entry.ready.is_some() && now.duration_since(entry.last_active) > timeout;
            if expired {
                debug!(sub_id = id, "reconciliation timed out");
                replies.push(neg_err(id, "closed: reconciliation timed out"));
            }
            !expired
        });
        replies
    }
}

#[derive(Clone)]
pub struct Negentropies {
    setting: NegentropySetting,
    db: Arc<Db>,
    budget: RecordBudget,
    jobs: Arc<OnceLock<(NonZeroUsize, Arc<Semaphore>)>>,
}

impl Negentropies {
    pub fn new(db: Arc<Db>) -> Self {
        Self {
            setting: NegentropySetting::default(),
            db,
            budget: RecordBudget::default(),
            jobs: Arc::default(),
        }
    }

    fn jobs(&self) -> &Arc<Semaphore> {
        &self
            .jobs
            .get()
            .expect("negentropy settings initialized before handling messages")
            .1
    }

    #[cfg(test)]
    fn jobs_in_use(&self) -> usize {
        let slots = self.jobs();
        self.jobs.get().unwrap().0.get() - slots.available_permits()
    }

    fn sessions<'a>(&self, session: &'a mut Session) -> &'a mut Sessions {
        if session.get::<Sessions>().is_none() {
            session.set(Sessions::new(self.setting.max_sessions));
        }
        session
            .get_mut::<Sessions>()
            .expect("initialized reconciliation state")
    }

    /// Snapshot the filter as sealed negentropy storage plus its record count,
    /// or `None` when it matches more than `max_records` events. Sealing sorts
    /// by `(timestamp, id)`, the protocol's order, which differs from rnostr's
    /// own index order for events sharing a timestamp.
    fn snapshot(
        &self,
        filter: Filter,
        timeout: Duration,
        cancelled: &watch::Receiver<()>,
    ) -> Result<Option<(NegentropyStorageVector, usize)>, Error> {
        let start = Instant::now();
        #[cfg(feature = "search")]
        let filter = {
            let mut filter = filter;
            filter.build_words();
            filter
        };
        let reader = self.db.reader()?;
        let mut iter = self.db.iter::<Vec<u8>, _>(&reader, &filter)?;
        let remaining = timeout
            .checked_sub(start.elapsed())
            .ok_or(nostr_relay::db::Error::ScanTimeout)?;
        let scan_cancelled = cancelled.clone();
        iter.scan_time_with(remaining, 256, move || {
            if scan_cancelled.has_changed().is_err() {
                return Err(nostr_relay::db::Error::Message(
                    "reconciliation cancelled".to_owned(),
                ));
            }
            Ok(())
        });
        let (pairs, complete, _) = iter.index_pairs(self.setting.max_records)?;
        if start.elapsed() > timeout {
            return Err(nostr_relay::db::Error::ScanTimeout.into());
        }
        if cancelled.has_changed().is_err() {
            return Err(Error::Str("reconciliation cancelled"));
        }
        if !complete {
            return Ok(None);
        }
        // Deduplication retains capacity, so charge the original count.
        let records = pairs.len();
        let mut storage = NegentropyStorageVector::with_capacity(records);
        for (created_at, id) in pairs {
            storage
                .insert(created_at, Id::from_byte_array(id))
                .map_err(|err| Error::Message(err.to_string()))?;
        }
        storage
            .seal()
            .map_err(|err| Error::Message(err.to_string()))?;
        Ok(Some((storage, records)))
    }

    fn open(&self, session: &mut Session, open: &NegOpen) -> Result<Job, OutgoingMessage> {
        let sub_id = open.id.as_str();
        let setting = session.app.setting.read();
        let timeout = self.effective_scan_timeout(setting.data.db_query_timeout);
        #[cfg(feature = "search")]
        let search_enabled = setting
            .get_extension::<crate::search::SearchSetting>()
            .is_some_and(|setting| setting.enabled);
        #[cfg(not(feature = "search"))]
        let search_enabled = false;
        drop(setting);
        let sessions = self.sessions(session);
        sessions.prepare_open(sub_id)?;
        if open.filter.search.is_some() && !search_enabled {
            return Err(neg_err(
                sub_id,
                "unsupported: search reconciliation is unavailable",
            ));
        }
        let initial = decode_hex(&open.message).map_err(|reason| neg_err(sub_id, &reason))?;
        // Replacements have already released their old lease.
        if self.budget.is_full(self.setting.max_total_records) {
            return Err(neg_err(sub_id, MEMORY_LIMIT));
        }
        let admission = Admission::new(JobKind::Open, self.jobs(), &sessions.jobs, sub_id)?;
        let cancelled = sessions.insert(sub_id);
        let worker = self.clone();
        let id = sub_id.to_owned();
        let worker_cancelled = cancelled.clone();
        let filter = open.filter.clone();
        Ok(Job {
            id: id.clone(),
            cancelled,
            admission,
            work: Box::new(move || {
                let snapshot = worker
                    .snapshot(filter, timeout, &worker_cancelled)
                    .map_err(|err| match err {
                        Error::Db(nostr_relay::db::Error::ScanTimeout) => {
                            neg_err(&id, "blocked: reconciliation scan timed out")
                        }
                        _ => neg_err(&id, &format!("error: {err}")),
                    })?;
                let Some((storage, records)) = snapshot else {
                    let reason = format!(
                        "blocked: too many records for reconciliation (max {})",
                        worker.setting.max_records
                    );
                    return Err(neg_err(&id, &reason));
                };
                let lease = worker
                    .budget
                    .acquire(records, worker.setting.max_total_records)
                    .ok_or_else(|| neg_err(&id, MEMORY_LIMIT))?;
                let mut negentropy = Negentropy::owned(storage, worker.setting.frame_size_limit)
                    .map_err(|err| neg_err(&id, &format!("error: {err}")))?;
                let out = negentropy
                    .reconcile(&initial)
                    .map_err(|err| neg_err(&id, &format!("invalid: {err}")))?;
                Ok((
                    Reconciliation {
                        negentropy,
                        _lease: lease,
                    },
                    neg_msg(&id, &out),
                ))
            }),
        })
    }

    fn effective_scan_timeout(&self, db_timeout: Option<NonZeroDuration>) -> Duration {
        Duration::from(db_timeout.map_or(self.setting.scan_timeout, |timeout| {
            timeout.min(self.setting.scan_timeout)
        }))
    }

    fn reconcile(
        &self,
        session: &mut Session,
        sub_id: &str,
        message: &str,
    ) -> Result<Job, OutgoingMessage> {
        let sessions = self.sessions(session);
        let (mut reconciliation, cancelled) = sessions.take_round(sub_id)?;
        let query = match decode_hex(message) {
            Ok(query) => query,
            Err(reason) => {
                sessions.close(sub_id);
                return Err(neg_err(sub_id, &reason));
            }
        };
        let id = sub_id.to_owned();
        Ok(Job {
            id: id.clone(),
            cancelled,
            admission: Admission::new(JobKind::Round, self.jobs(), &sessions.jobs, sub_id)?,
            work: Box::new(move || {
                let out = reconciliation
                    .negentropy
                    .reconcile(&query)
                    .map_err(|err| neg_err(&id, &format!("invalid: {err}")))?;
                Ok((reconciliation, neg_msg(&id, &out)))
            }),
        })
    }

    fn schedule(mut job: Job, session: &mut Session, ctx: &mut <Session as actix::Actor>::Context) {
        Self::start_idle_sweep(session, ctx);
        let id = job.id.clone();
        let cancelled = job.cancelled.clone();
        let future = async move {
            match job.admit().await {
                Ok(()) => actix_web::rt::task::spawn_blocking(move || job.run()).await,
                Err(out) => Ok(Err(out)),
            }
        }
        .into_actor(session)
        .map(move |result, session, ctx| {
            let result = result.unwrap_or_else(|err| {
                Err(neg_err(
                    &id,
                    &format!("error: reconciliation worker failed: {err}"),
                ))
            });
            if let Some(out) = Self::complete(session, &id, &cancelled, result) {
                let app = session.app.clone();
                let extensions = app.extensions.read();
                if extensions.call_outgoing(&out, session, ctx) {
                    ctx.text(out.0);
                } else {
                    extensions.close_reconciliation(&id, session);
                }
            }
        });
        ctx.spawn(future);
    }

    fn complete(
        session: &mut Session,
        id: &str,
        cancelled: &watch::Receiver<()>,
        result: JobResult,
    ) -> Option<OutgoingMessage> {
        session
            .get_mut::<Sessions>()?
            .complete(id, cancelled, result)
    }

    /// Poll live timeouts while reconciliations remain open.
    fn start_idle_sweep(session: &mut Session, ctx: &mut <Session as actix::Actor>::Context) {
        let sessions = session
            .get_mut::<Sessions>()
            .expect("accepted reconciliation");
        if sessions.idle_sweep.is_some() {
            return;
        }
        sessions.idle_sweep = Some(ctx.run_interval(Duration::from_secs(1), |session, ctx| {
            let timeout = session
                .app
                .setting
                .read()
                .get_extension::<NegentropySetting>()
                .and_then(|setting| setting.idle_timeout);
            if let Some(timeout) = timeout {
                expire_idle(session, ctx, timeout.into());
            }
            if let Some(handle) = session
                .get_mut::<Sessions>()
                .filter(|sessions| sessions.open.is_empty())
                .and_then(|sessions| sessions.idle_sweep.take())
            {
                ctx.cancel_future(handle);
            }
        }));
    }
}

/// Drop reconciliations idle longer than `timeout`, telling the client which
/// went away so it does not wait on a session the relay has forgotten.
fn expire_idle(
    session: &mut Session,
    ctx: &mut <Session as actix::Actor>::Context,
    timeout: Duration,
) {
    let Some(sessions) = session.get_mut::<Sessions>() else {
        return;
    };
    for out in sessions.expire(timeout) {
        ctx.text(out);
    }
}

#[cfg(test)]
fn sessions_mut(session: &mut Session) -> &mut Sessions {
    session
        .get_mut::<Sessions>()
        .expect("just-inserted session state")
}

fn close(session: &mut Session, sub_id: &str) {
    if let Some(sessions) = session.get_mut::<Sessions>() {
        sessions.close(sub_id);
    }
}

fn neg_msg(sub_id: &str, message: &[u8]) -> OutgoingMessage {
    OutgoingMessage(json!([NEG_MSG, sub_id, hex::encode(message)]).to_string())
}

fn neg_err(sub_id: &str, reason: &str) -> OutgoingMessage {
    OutgoingMessage::neg_err(sub_id, reason)
}

fn decode_hex(raw: &str) -> Result<Vec<u8>, String> {
    let decoded =
        hex::decode(raw).map_err(|err| format!("invalid: malformed negentropy message: {err}"))?;
    input::validate(&decoded)
        .map_err(|err| format!("invalid: malformed negentropy message: {err}"))?;
    Ok(decoded)
}

impl Extension for Negentropies {
    fn name(&self) -> &'static str {
        "negentropy"
    }

    fn connected(&self, session: &mut Session, _: &mut <Session as actix::Actor>::Context) {
        self.sessions(session);
    }

    fn close_reconciliation(&self, id: &str, session: &mut Session) {
        close(session, id);
    }

    fn setting<'a>(&'a mut self, setting: &mut Setting) -> nostr_relay::Result<SettingUpdate<'a>> {
        let mut parsed: NegentropySetting = setting.parse_extension(self.name())?;

        if parsed.frame_size_limit != 0 && parsed.frame_size_limit < MIN_FRAME_SIZE_LIMIT {
            warn!(
                frame_size_limit = parsed.frame_size_limit,
                "negentropy frame_size_limit must be 0 or at least {MIN_FRAME_SIZE_LIMIT}; raising it"
            );
            parsed.frame_size_limit = MIN_FRAME_SIZE_LIMIT;
        }

        if parsed.max_jobs.get() > Semaphore::MAX_PERMITS {
            warn!("negentropy max_jobs exceeds semaphore capacity; lowering it");
            parsed.max_jobs = NonZeroUsize::new(Semaphore::MAX_PERMITS).unwrap();
        }
        if let Some((max_jobs, _)) = self.jobs.get() {
            if parsed.max_jobs != *max_jobs {
                warn!("negentropy max_jobs changes require a restart; keeping the startup limit");
                parsed.max_jobs = *max_jobs;
            }
        }
        setting.set_extension(parsed.clone());
        if parsed.enabled {
            setting.add_nip(77);
        } else {
            setting.information.supported_nips.retain(|nip| *nip != 77);
        }
        Ok(Box::new(move || {
            self.jobs.get_or_init(|| {
                (
                    parsed.max_jobs,
                    Arc::new(Semaphore::new(parsed.max_jobs.get())),
                )
            });
            self.setting = parsed;
        }))
    }

    fn message(
        &self,
        msg: ClientMessage,
        session: &mut Session,
        ctx: &mut <Session as actix::Actor>::Context,
    ) -> ExtensionMessageResult {
        let job = match &msg.msg {
            IncomingMessage::NegOpen(_) | IncomingMessage::NegMsg(..) if !self.setting.enabled => {
                return msg.reject("closed: reconciliation disabled");
            }
            IncomingMessage::NegOpen(open) => self.open(session, open),
            IncomingMessage::NegMsg(id, message) => self.reconcile(session, id, message),
            IncomingMessage::NegClose(_) => return ExtensionMessageResult::Ignore,
            _ => return ExtensionMessageResult::Continue(msg),
        };
        match job {
            Ok(job) => {
                Self::schedule(job, session, ctx);
                ExtensionMessageResult::Ignore
            }
            Err(out) => out.into(),
        }
    }
}

mod input {
    //! Validate V1 framing before handing untrusted input to negentropy 0.5.
    //! Its ID-list decoder reserves memory from the declared count before checking
    //! the payload length. This allocation-free pass bounds that count by bytes
    //! actually present, independently of snapshot and response size limits.

    use negentropy::{FINGERPRINT_SIZE, ID_SIZE, PROTOCOL_VERSION};

    pub(super) fn validate(mut input: &[u8]) -> Result<(), &'static str> {
        let version = take(&mut input, 1)?[0];
        if u64::from(version) != PROTOCOL_VERSION {
            // Let the library handle invalid versions and version negotiation;
            // neither path decodes ranges or allocates from a client count.
            return Ok(());
        }

        while !input.is_empty() {
            varint(&mut input)?; // Timestamp delta.
            let prefix_len = varint(&mut input)?;
            if prefix_len > ID_SIZE as u64 {
                return Err("ID prefix is too long");
            }
            take(&mut input, prefix_len as usize)?;
            match varint(&mut input)? {
                0 => {} // Skip.
                1 => {
                    take(&mut input, FINGERPRINT_SIZE)?;
                }
                2 => {
                    let count = varint(&mut input)?;
                    if count > (input.len() / ID_SIZE) as u64 {
                        return Err("ID count exceeds the remaining message");
                    }
                    // The length check also makes the cast and product safe.
                    take(&mut input, count as usize * ID_SIZE)?;
                }
                _ => return Err("unknown range mode"),
            }
        }
        Ok(())
    }

    fn take<'a>(input: &mut &'a [u8], count: usize) -> Result<&'a [u8], &'static str> {
        if count > input.len() {
            return Err("message ends prematurely");
        }
        let (head, tail) = input.split_at(count);
        *input = tail;
        Ok(head)
    }

    fn varint(input: &mut &[u8]) -> Result<u64, &'static str> {
        let mut value = 0u64;
        loop {
            let byte = take(input, 1)?[0];
            value = value
                .checked_mul(128)
                .and_then(|value| value.checked_add(u64::from(byte & 0x7f)))
                .ok_or("varint overflows u64")?;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn rejects_counts_without_corresponding_ids() {
            // Previously this nine-byte frame attempted a 17.7 GB allocation.
            assert!(validate(&[0x61, 0, 0, 2, 0x81, 0x80, 0x80, 0x80, 0]).is_err());
            assert!(validate(&[0x61, 0, 0, 2, 1]).is_err());
            let mut short_id = vec![0x61, 0, 0, 2, 1];
            short_id.extend([0; ID_SIZE - 1]);
            assert!(validate(&short_id).is_err());
        }

        #[test]
        fn rejects_truncated_and_overflowing_fields() {
            for query in [
                &[][..],
                &[0x61, 0x80],
                &[0x61, 0, 0x80],
                &[0x61, 0, 33],
                &[0x61, 0, 1],
                &[0x61, 0, 0],
                &[0x61, 0, 0, 1],
                &[0x61, 0, 0, 2, 0x80],
                &[0x61, 0, 0, 3],
            ] {
                assert!(validate(query).is_err(), "accepted {query:?}");
            }
            let mut overflow = vec![0x61, 0, 0, 2];
            overflow.extend([0xff; 10]);
            overflow.push(0x7f);
            assert!(validate(&overflow).is_err());
        }

        #[test]
        fn accepts_valid_ranges_and_checks_later_counts() {
            let mut query = vec![0x61, 1, 0, 0, 1, 0, 1];
            query.extend([0; FINGERPRINT_SIZE]);
            query.extend([0, 0, 2, 1]);
            query.extend([0; ID_SIZE]);
            assert!(validate(&query).is_ok());
            query.extend([0, 0, 2, 1]);
            assert!(validate(&query).is_err());
            assert!(validate(&[0x61]).is_ok());
            assert!(validate(&[0x61, 0, 0, 2, 0]).is_ok());
            assert!(validate(&[0x62, 0xff]).is_ok());
        }

        #[test]
        fn validates_multi_round_library_messages() {
            use negentropy::{Id, Negentropy, NegentropyStorageVector};

            let storage = |range: std::ops::Range<u64>| {
                let mut storage = NegentropyStorageVector::new();
                for index in range {
                    let mut id = [0; ID_SIZE];
                    id[24..].copy_from_slice(&index.to_be_bytes());
                    // Shared timestamps force ID prefixes in range boundaries.
                    storage.insert(index / 3, Id::from_byte_array(id)).unwrap();
                }
                storage.seal().unwrap();
                storage
            };
            let mut relay = Negentropy::owned(storage(0..300), 4096).unwrap();
            let mut client = Negentropy::owned(storage(100..400), 4096).unwrap();
            let mut query = client.initiate().unwrap();
            let mut have = Vec::new();
            let mut need = Vec::new();
            let mut rounds = 0;
            loop {
                rounds += 1;
                assert!(rounds < 100, "reconciliation did not converge");
                validate(&query).unwrap();
                let reply = relay.reconcile(&query).unwrap();
                validate(&reply).unwrap();
                match client
                    .reconcile_with_ids(&reply, &mut have, &mut need)
                    .unwrap()
                {
                    Some(next) => query = next,
                    None => break,
                }
            }
            assert!(rounds > 1);
            assert_eq!(have.len(), 100);
            assert_eq!(need.len(), 100);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::create_test_app;
    use actix_web::web;
    use actix_web_actors::ws;
    use anyhow::Result;
    use futures_util::{Sink, SinkExt as _, Stream, StreamExt as _};
    use negentropy::NegentropyStorageVector;
    use nostr_relay::db::{
        now,
        secp256k1::{rand::thread_rng, Keypair},
        Event,
    };
    use nostr_relay::{
        create_web_app,
        message::{NEG_CLOSE, NEG_ERR, NEG_OPEN},
        setting::SettingWrapper,
    };
    use serde_json::Value;

    /// A client-side WebSocket, as `TestServer::ws_at` returns it.
    trait Ws:
        Sink<ws::Message, Error = ws::ProtocolError>
        + Stream<Item = std::result::Result<ws::Frame, ws::ProtocolError>>
        + Unpin
    {
    }
    impl<T> Ws for T where
        T: Sink<ws::Message, Error = ws::ProtocolError>
            + Stream<Item = std::result::Result<ws::Frame, ws::ProtocolError>>
            + Unpin
    {
    }

    /// `["NEG-MSG"|"NEG-ERR", sub_id, payload]`.
    type Reply = (String, String, String);

    async fn send(framed: &mut impl Ws, msg: Value) -> Result<()> {
        Ok(framed
            .send(ws::Message::Text(msg.to_string().into()))
            .await?)
    }

    async fn recv<T: serde::de::DeserializeOwned>(framed: &mut impl Ws) -> Result<T> {
        match framed.next().await.unwrap()? {
            ws::Frame::Text(text) => Ok(serde_json::from_slice(&text)?),
            frame => Err(anyhow::anyhow!("invalid frame type: {frame:?}")),
        }
    }

    /// Open `sub` over `filter` and run rounds until `client` is satisfied.
    /// Returns the sorted ids only the client has, the sorted ids only the
    /// relay has, and the byte size of every relay reply.
    async fn drive(
        framed: &mut impl Ws,
        sub: &str,
        filter: Value,
        client: &mut Negentropy<'static, NegentropyStorageVector>,
    ) -> Result<(Vec<Id>, Vec<Id>, Vec<usize>)> {
        let initial = client.initiate()?;
        send(framed, json!([NEG_OPEN, sub, filter, hex::encode(initial)])).await?;
        let (mut have, mut need, mut sizes) = (Vec::new(), Vec::new(), Vec::new());
        loop {
            let res: Reply = recv(framed).await?;
            assert_eq!(res.0, NEG_MSG, "unexpected message: {res:?}");
            assert_eq!(res.1, sub);
            let reply = hex::decode(&res.2)?;
            sizes.push(reply.len());
            match client.reconcile_with_ids(&reply, &mut have, &mut need)? {
                Some(next) => send(framed, json!([NEG_MSG, sub, hex::encode(next)])).await?,
                None => break,
            }
        }
        have.sort();
        need.sort();
        Ok((have, need, sizes))
    }

    fn ids(events: &[Event]) -> Vec<Id> {
        let mut ids: Vec<Id> = events
            .iter()
            .map(|e| Id::from_byte_array(*e.id()))
            .collect();
        ids.sort();
        ids
    }

    /// The next frame an actor under test writes, raw.
    async fn next_frame<B: AsRef<[u8]>, E: std::fmt::Debug>(
        stream: &mut (impl Stream<Item = std::result::Result<B, E>> + Unpin),
    ) -> Result<Vec<u8>> {
        let frame = actix_rt::time::timeout(Duration::from_secs(5), stream.next()).await?;
        Ok(frame.unwrap().unwrap().as_ref().to_vec())
    }

    type BoundaryStream =
        futures_util::stream::LocalBoxStream<'static, Result<bytes::Bytes, actix_web::Error>>;

    /// Run the session actor `start` returns; yields the frames it writes.
    fn actor(
        start: impl FnOnce(&mut <Session as actix::Actor>::Context) -> Session + 'static,
    ) -> BoundaryStream {
        ws::WebsocketContext::with_factory(futures_util::stream::pending(), start).boxed_local()
    }

    impl RecordBudget {
        fn used(&self) -> usize {
            self.0.load(Ordering::Relaxed)
        }
    }

    fn contains(frame: &[u8], needle: &str) -> bool {
        frame
            .windows(needle.len())
            .any(|part| part == needle.as_bytes())
    }

    fn neg_open(sub: &str, filter: Value, message: &str) -> NegOpen {
        NegOpen {
            id: sub.to_owned(),
            filter: serde_json::from_value(filter).unwrap(),
            message: message.to_owned(),
        }
    }

    async fn spawn(mut job: Job) -> Result<JobResult> {
        match job.admit().await {
            Ok(()) => Ok(actix_web::rt::task::spawn_blocking(move || job.run()).await?),
            Err(out) => Ok(Err(out)),
        }
    }

    async fn run_job(
        session: &mut Session,
        job: Result<Job, OutgoingMessage>,
    ) -> Result<OutgoingMessage> {
        let job = match job {
            Ok(job) => job,
            Err(out) => return Ok(out),
        };
        let id = job.id.clone();
        let cancelled = job.cancelled.clone();
        let result = spawn(job).await?;
        Ok(Negentropies::complete(session, &id, &cancelled, result)
            .expect("current reconciliation"))
    }

    #[actix_rt::test]
    async fn toml_zero_disables_expiry_without_disabling_negentropy() -> Result<()> {
        use std::io::Write as _;
        let app = create_test_app("neg-toml")?;
        let mut extension = Negentropies::new(app.db.clone());
        for (line, expected) in [
            ("idle_timeout = 0", None),
            ("idle_timeout = \"0s\"", None),
            ("idle_timeout = \"2s\"", Some(Duration::from_secs(2))),
            ("", Some(Duration::from_secs(300))),
        ] {
            let mut file = tempfile::Builder::new().suffix(".toml").tempfile()?;
            writeln!(file, "[negentropy]\nenabled = true\n{line}")?;
            let setting: SettingWrapper = nostr_relay::Setting::read(file.path(), None)?.into();
            extension.setting(&mut setting.write()).unwrap()();
            assert!(extension.setting.enabled, "{line}");
            assert_eq!(extension.setting.idle_timeout.map(Duration::from), expected);
            assert!(setting.read().information.supported_nips.contains(&77));
        }
        let setting: NegentropySetting = serde_json::from_value(json!({"idle_timeout": null}))?;
        assert!(setting.idle_timeout.is_none());
        Ok(())
    }

    #[actix_rt::test]
    async fn job_admission_is_global_and_connection_limits_survive_reload() -> Result<()> {
        let app = create_test_app("neg-jobs")?;
        app.db.batch_put(test_events(1)?)?;
        let mut extension = Negentropies::new(app.db.clone());
        configure(
            &mut extension,
            &app.setting,
            json!({"enabled": true, "max_jobs": 1, "max_sessions": 1}),
        );
        let app = web::Data::new(app);
        let mut first = Session::new("first".to_owned(), app.clone());
        let mut second = Session::new("second".to_owned(), app);
        let values = neg_open("x", json!({}), "61");
        let job = extension.open(&mut first, &values).unwrap();
        let blocked = extension.open(&mut second, &values).err().unwrap();
        assert!(blocked.0.contains("work limit"));
        let blocked = extension
            .open(&mut first, &neg_open("y", json!({}), "61"))
            .err()
            .unwrap();
        assert!(blocked.0.contains("concurrent reconciliations"));
        close(&mut first, "x");
        let blocked = extension.open(&mut second, &values).err().unwrap();
        assert!(blocked.0.contains("work limit"));
        drop(job);
        assert_eq!(extension.jobs_in_use(), 0);
        let job = extension.open(&mut second, &values);
        assert!(run_job(&mut second, job).await?.0.contains(NEG_MSG));
        assert_eq!(extension.jobs_in_use(), 0);
        configure(
            &mut extension,
            &second.app.setting,
            json!({"enabled": true, "max_jobs": 1, "max_sessions": 0}),
        );
        let round = extension.reconcile(&mut second, "x", "61");
        assert!(run_job(&mut second, round).await?.0.contains(NEG_MSG));
        let opening = extension.open(&mut first, &neg_open("y", json!({}), "61"));
        assert!(run_job(&mut first, opening).await?.0.contains(NEG_MSG));
        let mut zero = Session::new("zero".to_owned(), second.app.clone());
        let blocked = extension.open(&mut zero, &values).err().unwrap();
        assert!(blocked.0.contains("concurrent reconciliations"));
        configure(
            &mut extension,
            &second.app.setting,
            json!({"enabled": true, "max_jobs": 1, "max_sessions": 2}),
        );
        let blocked = extension.open(&mut zero, &values).err().unwrap();
        assert!(blocked.0.contains("concurrent reconciliations"));
        let blocked = extension
            .open(&mut first, &neg_open("z", json!({}), "61"))
            .err()
            .unwrap();
        assert!(blocked.0.contains("concurrent reconciliations"));
        let mut new = Session::new("new".to_owned(), second.app.clone());
        for id in ["x", "y"] {
            let opening = extension.open(&mut new, &neg_open(id, json!({}), "61"));
            assert!(run_job(&mut new, opening).await?.0.contains(NEG_MSG));
        }
        assert_eq!(extension.budget.used(), 4);
        Ok(())
    }

    #[actix_rt::test]
    async fn relay_job_limit_is_fixed_at_startup() -> Result<()> {
        let app = create_test_app("neg-fixed-jobs")?;
        let mut extension = Negentropies::new(app.db.clone());
        assert!(extension.jobs.get().is_none());
        let clone = extension.clone();
        drop(extension.setting(&mut Setting::default())?);
        assert!(extension.jobs.get().is_none());
        configure(
            &mut extension,
            &app.setting,
            json!({"enabled": true, "max_jobs": 2}),
        );
        let pool = extension.jobs().clone();
        assert!(Arc::ptr_eq(&pool, clone.jobs()));
        let _first = pool.clone().try_acquire_owned().unwrap();
        let _second = pool.clone().try_acquire_owned().unwrap();
        for limit in [1, 3] {
            configure(
                &mut extension,
                &app.setting,
                json!({"enabled": true, "max_jobs": limit}),
            );
            assert!(Arc::ptr_eq(&pool, extension.jobs()));
            assert_eq!(extension.setting.max_jobs.get(), 2);
            assert_eq!(
                app.setting
                    .read()
                    .get_extension::<NegentropySetting>()
                    .unwrap()
                    .max_jobs
                    .get(),
                2
            );
            assert!(extension.jobs().clone().try_acquire_owned().is_err());
        }
        Ok(())
    }

    fn configure(extension: &mut Negentropies, setting: &SettingWrapper, value: Value) {
        setting.write().extra.insert("negentropy".to_owned(), value);
        extension.setting(&mut setting.write()).unwrap()();
    }

    fn configured_extension(app: &nostr_relay::App) -> Negentropies {
        let mut extension = Negentropies::new(app.db.clone());
        extension.setting(&mut app.setting.write()).unwrap()();
        extension
    }

    fn session_with_event(name: &str, settings: Value) -> Result<(Negentropies, Session)> {
        let app = create_test_app(name)?;
        app.db.batch_put(test_events(1)?)?;
        let mut extension = Negentropies::new(app.db.clone());
        configure(&mut extension, &app.setting, settings);
        Ok((
            extension,
            Session::new("test".to_owned(), web::Data::new(app)),
        ))
    }

    struct BoundaryProbe;
    impl Extension for BoundaryProbe {
        fn name(&self) -> &'static str {
            "boundary-probe"
        }

        fn message(
            &self,
            msg: ClientMessage,
            session: &mut Session,
            _: &mut <Session as actix::Actor>::Context,
        ) -> ExtensionMessageResult {
            if matches!(&msg.msg, IncomingMessage::Unknown(command, _) if command == "BARRIER") {
                return OutgoingMessage::notice("barrier").into();
            }
            let reject = session
                .app
                .setting
                .read()
                .extra
                .get("probe_reject")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if reject && msg.msg.reconciliation_id().is_some() {
                return msg.reject("blocked: rejected by extension");
            }
            let payload = match &msg.msg {
                IncomingMessage::NegOpen(open) => Some(open.message.as_str()),
                IncomingMessage::NegMsg(_, payload) => Some(payload.as_str()),
                _ => None,
            };
            match payload {
                Some("62") => return OutgoingMessage::notice("handled").into(),
                Some("63") => return ExtensionMessageResult::Ignore,
                _ => {}
            }
            ExtensionMessageResult::Continue(msg)
        }
    }

    fn boundary_app(name: &str) -> Result<(web::Data<nostr_relay::App>, Negentropies)> {
        let app = create_test_app(name)?;
        app.db.batch_put(test_events(1)?)?;
        app.setting.write().limitation.max_message_length = 64;
        let mut extension = Negentropies::new(app.db.clone());
        configure(
            &mut extension,
            &app.setting,
            json!({"enabled": true, "max_jobs": 1, "max_sessions": 1, "idle_timeout": null}),
        );
        Ok((
            web::Data::new(
                app.add_extension(BoundaryProbe)?
                    .add_extension(extension.clone())?,
            ),
            extension,
        ))
    }

    fn boundary_connection(
        app: web::Data<nostr_relay::App>,
    ) -> (tokio::sync::mpsc::UnboundedSender<String>, BoundaryStream) {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel::<String>();
        let messages = futures_util::stream::unfold(receiver, |mut receiver| async move {
            receiver
                .recv()
                .await
                .map(|text| (Ok(ws::Message::Text(text.into())), receiver))
        });
        let stream = actor(move |ctx| {
            ctx.add_stream(messages);
            Session::new("test".to_owned(), app)
        });
        (sender, stream)
    }

    async fn open_ready_or_pending(
        extension: &Negentropies,
        sender: &tokio::sync::mpsc::UnboundedSender<String>,
        stream: &mut BoundaryStream,
        pending: bool,
    ) -> Result<Option<OwnedSemaphorePermit>> {
        sender.send(json!([NEG_OPEN, "x", {}, "61"]).to_string())?;
        assert!(contains(&next_frame(stream).await?, NEG_MSG));
        if !pending {
            return Ok(None);
        }
        let held = extension.jobs().clone().try_acquire_owned()?;
        sender.send(json!([NEG_MSG, "x", "61"]).to_string())?;
        sender.send(json!(["BARRIER"]).to_string())?;
        assert!(contains(&next_frame(stream).await?, "barrier"));
        Ok(Some(held))
    }

    #[actix_rt::test]
    async fn reconciliation_rejections_close_state_for_every_command() -> Result<()> {
        let (app, extension) = boundary_app("neg-command-errors")?;
        for command in [NEG_OPEN, NEG_MSG, NEG_CLOSE] {
            for failure in 0..6 {
                for pending in [false, true] {
                    let (sender, mut stream) = boundary_connection(app.clone());
                    let held =
                        open_ready_or_pending(&extension, &sender, &mut stream, pending).await?;
                    let valid = match command {
                        NEG_OPEN => json!([command, "x", {}, "61"]),
                        NEG_MSG => json!([command, "x", "61"]),
                        _ => json!([command, "x"]),
                    };
                    let request = match failure {
                        0 => format!(r#"["{command}","x",invalid"#),
                        1 => match command {
                            NEG_OPEN => json!([command, "x", {"kinds": "bad"}, "61"]).to_string(),
                            NEG_MSG => json!([command, "x", 123]).to_string(),
                            _ => json!([command, "x", 123]).to_string(),
                        },
                        2 => format!("{}{}", valid, " ".repeat(64)),
                        3 => {
                            app.setting.write().limitation.max_subid_length = 0;
                            valid.to_string()
                        }
                        4 => {
                            app.setting
                                .write()
                                .extra
                                .insert("probe_reject".to_owned(), json!(true));
                            valid.to_string()
                        }
                        _ => match command {
                            NEG_OPEN => json!([command, "x", {}]).to_string(),
                            NEG_MSG => json!([command, "x"]).to_string(),
                            _ => json!([command, "x", "extra"]).to_string(),
                        },
                    };
                    sender.send(request)?;
                    let frame = next_frame(&mut stream).await?;
                    assert!(
                        contains(
                            &frame,
                            if failure == 3 {
                                r#"["NOTICE","#
                            } else {
                                r#"["NEG-ERR","x","#
                            }
                        ),
                        "{command}, failure {failure}, pending {pending}"
                    );
                    assert!(
                        actix_rt::time::timeout(Duration::from_millis(10), stream.next())
                            .await
                            .is_err()
                    );
                    assert_eq!(extension.budget.used(), 0);
                    drop(held);
                    app.setting.write().limitation.max_subid_length = 100;
                    app.setting.write().extra.remove("probe_reject");
                    sender.send(json!([NEG_OPEN, "x", {}, "61"]).to_string())?;
                    assert!(contains(&next_frame(&mut stream).await?, NEG_MSG));
                    sender.send(json!([NEG_MSG, "x", "61"]).to_string())?;
                    assert!(contains(&next_frame(&mut stream).await?, NEG_MSG));
                    assert_eq!(extension.budget.used(), 1);
                }
            }
        }
        Ok(())
    }

    #[actix_rt::test]
    async fn handled_messages_preserve_rounds_but_close_replacements() -> Result<()> {
        let (app, extension) = boundary_app("neg-handled-state")?;
        for command in [NEG_OPEN, NEG_MSG] {
            for payload in ["62", "63"] {
                for pending in [false, true] {
                    let (sender, mut stream) = boundary_connection(app.clone());
                    let held =
                        open_ready_or_pending(&extension, &sender, &mut stream, pending).await?;
                    let request = if command == NEG_OPEN {
                        json!([command, "x", {}, payload])
                    } else {
                        json!([command, "x", payload])
                    };
                    sender.send(request.to_string())?;
                    if payload == "62" {
                        assert!(contains(&next_frame(&mut stream).await?, "handled"));
                    }
                    sender.send(json!(["BARRIER"]).to_string())?;
                    assert!(contains(&next_frame(&mut stream).await?, "barrier"));
                    drop(held);
                    if pending && command == NEG_MSG {
                        assert!(contains(&next_frame(&mut stream).await?, NEG_MSG));
                    } else {
                        assert!(
                            actix_rt::time::timeout(Duration::from_millis(10), stream.next())
                                .await
                                .is_err()
                        );
                    }
                    assert_eq!(extension.budget.used(), usize::from(command == NEG_MSG));
                    sender.send(json!([NEG_MSG, "x", "61"]).to_string())?;
                    let frame = next_frame(&mut stream).await?;
                    assert!(contains(
                        &frame,
                        if command == NEG_MSG {
                            NEG_MSG
                        } else {
                            "closed: no such reconciliation"
                        }
                    ));
                    drop(stream);
                    assert_eq!(extension.budget.used(), 0);
                }
            }
        }
        Ok(())
    }

    #[actix_rt::test]
    async fn disabling_on_reload_preserves_accepted_work_and_rejects_new_work() -> Result<()> {
        let (app, extension) = boundary_app("neg-disable-reload")?;
        let settings = |enabled| json!({"enabled": enabled, "max_jobs": 1, "max_sessions": 1, "idle_timeout": null});
        for reenable in [false, true] {
            reload(&app, "negentropy", settings(true));
            let (sender, mut stream) = boundary_connection(app.clone());
            let held = open_ready_or_pending(&extension, &sender, &mut stream, true).await?;
            let mut invalid = app.setting.read().clone();
            invalid.extra.insert(
                "negentropy".to_owned(),
                json!({"enabled": false, "scan_timeout": 0}),
            );
            assert!(app
                .extensions
                .write()
                .call_setting(&app.setting, invalid)
                .is_err());
            assert!(
                app.setting
                    .read()
                    .get_extension::<NegentropySetting>()
                    .unwrap()
                    .enabled
            );
            assert!(app.setting.read().information.supported_nips.contains(&77));
            sender.send(json!([NEG_OPEN, "probe", {}, "61"]).to_string())?;
            assert!(contains(
                &next_frame(&mut stream).await?,
                "concurrent reconciliations"
            ));
            reload(&app, "negentropy", settings(false));
            assert!(!app.setting.read().information.supported_nips.contains(&77));
            drop(held);
            assert!(contains(&next_frame(&mut stream).await?, NEG_MSG));
            assert_eq!(extension.budget.used(), 1);
            sender.send(json!([NEG_OPEN, "new", {}, "61"]).to_string())?;
            assert!(contains(
                &next_frame(&mut stream).await?,
                "closed: reconciliation disabled"
            ));
            if reenable {
                reload(&app, "negentropy", settings(true));
            }
            assert_eq!(
                app.setting.read().information.supported_nips.contains(&77),
                reenable
            );
            sender.send(json!([NEG_MSG, "x", "61"]).to_string())?;
            let frame = next_frame(&mut stream).await?;
            assert!(contains(
                &frame,
                if reenable {
                    NEG_MSG
                } else {
                    "closed: reconciliation disabled"
                }
            ));
            assert_eq!(extension.budget.used(), usize::from(reenable));
            drop(stream);
            assert_eq!(extension.budget.used(), 0);
        }
        Ok(())
    }

    #[actix_rt::test]
    async fn oversized_fragments_release_reconciliation_state() -> Result<()> {
        use actix_codec::Decoder;
        use actix_http::ws::Item;
        use bytes::{Bytes, BytesMut};

        let (app, extension) = boundary_app("neg-fragment-overflow")?;
        app.setting.write().limitation.max_message_length = 63;
        for command in [NEG_OPEN, NEG_MSG] {
            for pending in [false, true] {
                for id in ["x".to_owned(), "z".repeat(128)] {
                    let mut session = Session::new("test".to_owned(), app.clone());
                    let opening = extension.open(&mut session, &neg_open("x", json!({}), "61"));
                    run_job(&mut session, opening).await?;
                    let held =
                        pending.then(|| extension.jobs().clone().try_acquire_owned().unwrap());
                    let round =
                        pending.then(|| extension.reconcile(&mut session, "x", "61").unwrap());
                    let request = if command == NEG_OPEN {
                        json!([command, id, {}, "é".repeat(80)])
                    } else {
                        json!([command, id, "é".repeat(80)])
                    }
                    .to_string();
                    let mut stream = actor(move |ctx| {
                        if let Some(round) = round {
                            Negentropies::schedule(round, &mut session, ctx);
                        }
                        let count = request.len().div_ceil(16);
                        for (index, chunk) in request.as_bytes().chunks(16).enumerate() {
                            let chunk = Bytes::copy_from_slice(chunk);
                            let item = match index {
                                0 => Item::FirstText(chunk),
                                _ if index + 1 == count => Item::Last(chunk),
                                _ => Item::Continue(chunk),
                            };
                            actix::StreamHandler::handle(
                                &mut session,
                                Ok(ws::Message::Continuation(item)),
                                ctx,
                            );
                        }
                        session
                    });
                    let frame = next_frame(&mut stream).await?;
                    if id == "x" {
                        assert!(contains(
                            &frame,
                            r#"["NEG-ERR","x","invalid: message too long"]"#
                        ));
                        assert!(
                            actix_rt::time::timeout(Duration::from_millis(10), stream.next())
                                .await
                                .is_err()
                        );
                        assert_eq!(extension.budget.used(), 0);
                    } else {
                        let mut decoder = actix_http::ws::Codec::new().client_mode();
                        let mut bytes = BytesMut::from(frame.as_slice());
                        assert_eq!(
                            decoder.decode(&mut bytes)?,
                            Some(ws::Frame::Text(Bytes::from(
                                OutgoingMessage::notice("payload reached size limit.").0
                            )))
                        );
                        assert_eq!(
                            decoder.decode(&mut bytes)?,
                            Some(ws::Frame::Close(Some(ws::CloseCode::Size.into())))
                        );
                        assert!(bytes.is_empty());
                        assert!(
                            actix_rt::time::timeout(Duration::from_secs(5), stream.next())
                                .await?
                                .is_none()
                        );
                    }
                    drop(stream);
                    drop(held);
                    assert_eq!(extension.budget.used(), 0);
                    assert_eq!(extension.jobs_in_use(), 0);
                }
            }
        }
        Ok(())
    }

    #[actix_rt::test]
    async fn revoking_read_permission_rejects_rounds_but_allows_close() -> Result<()> {
        let app = create_test_app("neg-auth-revoked")?;
        app.db.batch_put(test_events(1)?)?;
        app.setting.write().extra = serde_json::from_value(json!({
            "auth": {"enabled": true},
            "negentropy": {"enabled": true}
        }))?;
        let extension = Negentropies::new(app.db.clone());
        let app = web::Data::new(
            app.add_extension(crate::Auth::new())?
                .add_extension(extension.clone())?
                .add_extension(BoundaryProbe)?,
        );
        let (sender, mut stream) = boundary_connection(app.clone());
        assert!(contains(&next_frame(&mut stream).await?, "AUTH"));
        for message in [
            json!([NEG_OPEN, "x", {}, "61"]),
            json!([NEG_OPEN, "y", {}, "61"]),
            json!([NEG_MSG, "x", "61"]),
        ] {
            sender.send(message.to_string())?;
            assert!(contains(&next_frame(&mut stream).await?, NEG_MSG));
        }
        reload(
            &app,
            "auth",
            json!({"enabled": true, "req": {"ip_blacklist": ["test"]}}),
        );
        sender.send(json!([NEG_CLOSE, "y"]).to_string())?;
        sender.send(json!(["BARRIER"]).to_string())?;
        assert!(contains(&next_frame(&mut stream).await?, "barrier"));
        assert_eq!(extension.budget.used(), 1);
        sender.send(json!([NEG_MSG, "x", "61"]).to_string())?;
        let frame = next_frame(&mut stream).await?;
        assert!(contains(&frame, NEG_ERR));
        assert!(contains(&frame, "restricted: ip in blacklist"));
        assert_eq!(extension.budget.used(), 0);
        assert_eq!(extension.jobs_in_use(), 0);
        Ok(())
    }

    #[actix_rt::test]
    async fn read_permission_is_rechecked_before_reconciliation_replies() -> Result<()> {
        let app = create_test_app("neg-revoked-reply")?;
        app.db.batch_put(test_events(1)?)?;
        app.setting.write().extra = serde_json::from_value(json!({
            "auth": {"enabled": true},
            "negentropy": {"enabled": true, "max_jobs": 1, "idle_timeout": null}
        }))?;
        let extension = Negentropies::new(app.db.clone());
        let app = web::Data::new(
            app.add_extension(crate::Auth::new())?
                .add_extension(extension.clone())?,
        );
        for round in [false, true] {
            for (permission, reason) in [
                (json!({"ip_whitelist": ["test"]}), None),
                (json!({"pubkey_whitelist": []}), Some("auth-required:")),
                (json!({"ip_blacklist": ["test"]}), Some("restricted:")),
            ] {
                reload(&app, "auth", json!({"enabled": true}));
                let mut session = Session::new("test".to_owned(), app.clone());
                let opening = extension.open(&mut session, &neg_open("x", json!({}), "61"));
                let mut job = if round {
                    run_job(&mut session, opening).await?;
                    extension.reconcile(&mut session, "x", "61").unwrap()
                } else {
                    opening.unwrap()
                };
                let (started, running) = tokio::sync::oneshot::channel();
                let (release, gate) = std::sync::mpsc::channel();
                let work = job.work;
                job.work = Box::new(move || {
                    started.send(()).unwrap();
                    gate.recv_timeout(Duration::from_secs(30)).unwrap();
                    work()
                });
                let (address_sender, address) = tokio::sync::oneshot::channel();
                let mut stream = actor(move |ctx| {
                    address_sender.send(ctx.address()).unwrap();
                    Negentropies::schedule(job, &mut session, ctx);
                    session
                });
                assert!(contains(&next_frame(&mut stream).await?, "AUTH"));
                tokio::select! {
                    result = running => result?,
                    frame = stream.next() => panic!("reply before releasing the worker: {frame:?}"),
                }
                address
                    .await?
                    .do_send(OutgoingMessage::notice("actor responsive"));
                assert!(contains(
                    &next_frame(&mut stream).await?,
                    "actor responsive"
                ));
                reload(&app, "auth", json!({"enabled": true, "req": permission}));
                release.send(())?;
                let frame = next_frame(&mut stream).await?;
                if let Some(reason) = reason {
                    assert!(contains(&frame, r#"["NEG-ERR","x","#));
                    assert!(contains(&frame, reason));
                    assert!(!contains(&frame, NEG_MSG));
                } else {
                    assert!(contains(&frame, r#"["NEG-MSG","x","#));
                }
                assert_eq!(extension.budget.used(), usize::from(reason.is_none()));
                assert_eq!(extension.jobs_in_use(), 0);
                drop(stream);
                assert_eq!(extension.budget.used(), 0);
            }
        }
        Ok(())
    }

    #[actix_rt::test]
    async fn closed_jobs_remain_charged_to_their_connection() -> Result<()> {
        for replace in [false, true] {
            let app = web::Data::new(create_test_app("neg-connection-jobs")?);
            let extension = configured_extension(&app);
            let mut first = Session::new("first".to_owned(), app.clone());
            let mut second = Session::new("second".to_owned(), app);
            let mut jobs = Vec::new();
            for _ in 0..extension.setting.max_sessions {
                jobs.push(
                    extension
                        .open(&mut first, &neg_open("x", json!({}), "61"))
                        .unwrap(),
                );
                if !replace {
                    close(&mut first, "x");
                }
            }
            let waiting = extension.open(&mut first, &neg_open("x", json!({}), "61"));
            let refused = run_job(&mut first, waiting).await?;
            assert!(refused.0.contains("connection work limit"));
            let other = extension.open(&mut second, &neg_open("x", json!({}), "61"));
            assert!(run_job(&mut second, other).await?.0.contains(NEG_MSG));
            for cancel in [true, false] {
                let mut next = extension
                    .open(&mut first, &neg_open("x", json!({}), "61"))
                    .unwrap();
                let mut admission = Box::pin(next.admit());
                assert!(futures_util::poll!(&mut admission).is_pending());
                if cancel {
                    close(&mut first, "x");
                    assert!(admission.await.unwrap_err().0.contains("cancelled"));
                } else {
                    jobs.pop();
                    admission.await.unwrap();
                    assert!(run_job(&mut first, Ok(next)).await?.0.contains(NEG_MSG));
                }
            }
        }
        Ok(())
    }

    #[test]
    fn uncapped_snapshots_count_toward_a_reenabled_budget() {
        let budget = RecordBudget::default();
        let capped = budget.acquire(2, 3).unwrap();
        let uncapped = budget.acquire(2, 0).unwrap();
        assert_eq!(budget.used(), 4);
        assert!(budget.acquire(1, 3).is_none());
        drop(uncapped);
        let next = budget.acquire(1, 3).unwrap();
        drop((capped, next));
        assert_eq!(budget.used(), 0);
    }

    #[actix_rt::test]
    async fn invalid_startup_fields_are_rejected() -> Result<()> {
        for (field, invalid) in [
            ("max_jobs", json!(0)),
            ("scan_timeout", json!(0)),
            ("scan_timeout", json!("0s")),
            ("max_records", json!("bad")),
            ("max_sessions", json!(-1)),
            ("max_total_records", json!("bad")),
            ("frame_size_limit", json!("bad")),
            ("idle_timeout", json!("bad")),
        ] {
            let app = create_test_app("neg-invalid-startup")?;
            let extension = Negentropies::new(app.db.clone());
            let mut settings = json!({"enabled": true, "max_jobs": 2, "max_records": 7});
            settings[field] = invalid;
            app.setting
                .write()
                .extra
                .insert("negentropy".to_owned(), settings);
            let shared = app.setting.clone();
            let error = app
                .add_extension(extension.clone())
                .err()
                .expect("invalid startup");
            assert!(error.to_string().contains("negentropy"), "{error}");
            assert!(extension.jobs.get().is_none());
            assert!(!shared.read().information.supported_nips.contains(&77));
            assert!(shared.read().get_extension::<NegentropySetting>().is_none());
        }
        Ok(())
    }

    #[actix_rt::test]
    async fn stale_completions_never_restore_replaced_or_closed_entries() -> Result<()> {
        let (extension, mut session) = session_with_event("neg-generations", json!({}))?;
        let values = neg_open("x", json!({}), "61");
        let old = extension.open(&mut session, &values).unwrap();
        let cancelled = old.cancelled.clone();
        let result = spawn(old).await?;
        assert_eq!(extension.budget.used(), 1);
        let replacement = extension.open(&mut session, &values);
        assert!(Negentropies::complete(&mut session, "x", &cancelled, result).is_none());
        assert_eq!(extension.budget.used(), 0);
        run_job(&mut session, replacement).await?;
        let round = extension.reconcile(&mut session, "x", "61").unwrap();
        let cancelled = round.cancelled.clone();
        let result = spawn(round).await?;
        close(&mut session, "x");
        assert!(Negentropies::complete(&mut session, "x", &cancelled, result).is_none());
        assert_eq!(extension.budget.used(), 0);
        assert_eq!(extension.jobs_in_use(), 0);
        Ok(())
    }

    #[actix_rt::test]
    async fn overlapping_rounds_close_the_busy_reconciliation() -> Result<()> {
        let (extension, mut session) = session_with_event("neg-busy", json!({}))?;
        let opening = extension.open(&mut session, &neg_open("x", json!({}), "61"));
        run_job(&mut session, opening).await?;
        let round = extension.reconcile(&mut session, "x", "61").unwrap();
        let error = extension.reconcile(&mut session, "x", "61").err().unwrap();
        assert!(error.0.contains("blocked: reconciliation is busy"));
        assert!(round.cancelled.has_changed().is_err());
        drop(round);
        assert_eq!(extension.budget.used(), 0);
        assert_eq!(extension.jobs_in_use(), 0);
        Ok(())
    }

    #[actix_rt::test]
    async fn sparse_scans_timeout_without_returning_a_partial_snapshot() -> Result<()> {
        let app = create_test_app("neg-scan-deadline")?;
        let key = Keypair::new_global(&mut thread_rng());
        let events = (0..300)
            .map(|i| {
                Event::create(
                    &key,
                    now(),
                    1,
                    vec![
                        vec!["t".to_owned(), "busy".to_owned()],
                        vec!["x".to_owned(), "busy".to_owned()],
                    ],
                    i.to_string(),
                )
            })
            .collect::<std::result::Result<Vec<_>, _>>()?;
        app.db.batch_put(events)?;
        let filter = json!({"#t": ["busy"], "authors": ["00".repeat(32)]});
        let construction_filter = json!({"#t": ["busy"], "#x": ["busy"], "kinds": [2]});
        let parsed: Filter = serde_json::from_value(filter.clone())?;
        {
            let reader = app.db.reader()?;
            let (pairs, complete, stats) = app
                .db
                .iter::<Vec<u8>, _>(&reader, &parsed)?
                .index_pairs(1)?;
            assert!(pairs.is_empty() && complete);
            assert!(stats.scan_index >= 300);
            let parsed: Filter = serde_json::from_value(construction_filter.clone())?;
            let mut iter = app.db.iter::<Vec<u8>, _>(&reader, &parsed)?;
            assert_eq!(iter.stats().scan_index, 0);
            iter.scan_time(Duration::ZERO, 1);
            assert!(matches!(
                iter.next(),
                Some(Err(nostr_relay::db::Error::ScanTimeout))
            ));
            assert!(iter.stats().scan_index < 300);
            let mut iter = app.db.iter::<Vec<u8>, _>(&reader, &parsed)?;
            let (cancel, cancelled) = watch::channel(());
            drop(cancel);
            iter.scan_time_with(Duration::from_secs(60), 1, move || {
                cancelled
                    .has_changed()
                    .map(|_| ())
                    .map_err(|_| nostr_relay::db::Error::Message("cancelled".to_owned()))
            });
            assert!(matches!(
                iter.next(),
                Some(Err(nostr_relay::db::Error::Message(_)))
            ));
            assert!(iter.stats().scan_index < 300);
        }
        let mut extension = configured_extension(&app);
        assert_eq!(
            extension.effective_scan_timeout(None),
            Duration::from_secs(5)
        );
        assert_eq!(
            extension.effective_scan_timeout(NonZeroDuration::new(Duration::from_secs(1))),
            Duration::from_secs(1)
        );
        assert_eq!(
            extension.effective_scan_timeout(NonZeroDuration::new(Duration::from_secs(60))),
            Duration::from_secs(5)
        );
        extension.setting.scan_timeout = NonZeroDuration::new(Duration::from_nanos(1)).unwrap();
        let mut session = Session::new("test".to_owned(), web::Data::new(app));
        for filter in [filter, construction_filter] {
            let job = extension.open(&mut session, &neg_open("x", filter, "61"));
            let out = run_job(&mut session, job).await?;
            assert!(out.0.contains("blocked: reconciliation scan timed out"));
        }
        assert_eq!(extension.budget.used(), 0);
        assert_eq!(extension.jobs_in_use(), 0);
        assert!(sessions_mut(&mut session).open.is_empty());
        Ok(())
    }

    /// A completed reconciliation "x" on the returned session, a scan from
    /// another connection holding the relay's only job slot, and a round on
    /// "x" queued behind it.
    async fn queued_round(name: &str) -> Result<(Negentropies, Session, Job, Job)> {
        let (extension, mut session) = session_with_event(name, json!({"max_jobs": 1}))?;
        let mut other = Session::new("other".to_owned(), session.app.clone());
        let opening = extension.open(&mut session, &neg_open("x", json!({}), "61"));
        assert!(run_job(&mut session, opening).await?.0.contains(NEG_MSG));
        let scan = extension
            .open(&mut other, &neg_open("x", json!({}), "61"))
            .unwrap();
        let round = extension.reconcile(&mut session, "x", "61").unwrap();
        assert!(matches!(round.admission, Admission::Queued { .. }));
        Ok((extension, session, scan, round))
    }

    #[actix_rt::test]
    async fn rounds_wait_for_a_job_slot_instead_of_closing() -> Result<()> {
        let (extension, mut session, scan, round) = queued_round("neg-queued-round").await?;
        let mut stream = actor(move |ctx| {
            Negentropies::schedule(round, &mut session, ctx);
            session
        });
        assert!(
            actix_rt::time::timeout(Duration::from_millis(100), stream.next())
                .await
                .is_err()
        );
        assert_eq!(extension.jobs_in_use(), 1);
        drop(scan);
        let frame = next_frame(&mut stream).await?;
        assert!(contains(&frame, NEG_MSG));
        assert_eq!(extension.jobs_in_use(), 0);
        assert_eq!(extension.budget.used(), 1);
        Ok(())
    }

    #[actix_rt::test]
    async fn a_queued_round_is_dropped_once_its_reconciliation_closes() -> Result<()> {
        let (extension, mut session, scan, round) = queued_round("neg-queued-cancel").await?;
        let mut stream = actor(move |ctx| {
            Negentropies::schedule(round, &mut session, ctx);
            close(&mut session, "x");
            session
        });
        assert!(
            actix_rt::time::timeout(Duration::from_millis(100), stream.next())
                .await
                .is_err()
        );
        assert_eq!(extension.jobs_in_use(), 1);
        assert_eq!(extension.budget.used(), 0);
        drop(scan);
        Ok(())
    }

    #[actix_rt::test]
    async fn idle_expiry_pauses_until_work_completes() -> Result<()> {
        let timeout = Duration::from_secs(1);
        for round in [false, true] {
            let (extension, mut session) =
                session_with_event("neg-pending-idle", json!({"max_jobs": 1}))?;
            let opening = extension.open(&mut session, &neg_open("x", json!({}), "61"));
            let (job, blocker) = if round {
                run_job(&mut session, opening).await?;
                let blocker = extension.jobs().clone().try_acquire_owned()?;
                (
                    extension.reconcile(&mut session, "x", "61").unwrap(),
                    Some(blocker),
                )
            } else {
                (opening.unwrap(), None)
            };
            let cancelled = job.cancelled.clone();
            let sessions = sessions_mut(&mut session);
            sessions.open.get_mut("x").unwrap().last_active =
                Instant::now() - Duration::from_secs(5);
            assert!(sessions.expire(timeout).is_empty());
            assert!(cancelled.has_changed().is_ok());
            drop(blocker);
            let result = spawn(job).await?;
            let sessions = sessions_mut(&mut session);
            assert!(sessions.expire(timeout).is_empty());
            assert!(sessions
                .complete("x", &cancelled, result)
                .unwrap()
                .0
                .contains(NEG_MSG));
            assert!(sessions.expire(timeout).is_empty());
            sessions.open.get_mut("x").unwrap().last_active =
                Instant::now() - Duration::from_secs(5);
            assert_eq!(sessions.expire(timeout).len(), 1);
            assert!(cancelled.has_changed().is_err());
            assert_eq!(extension.jobs_in_use(), 0);
            assert_eq!(extension.budget.used(), 0);
        }
        Ok(())
    }

    #[actix_rt::test]
    async fn worker_panic_releases_job_and_snapshot_and_reports_error() -> Result<()> {
        let (extension, mut session) = session_with_event("neg-worker-panic", json!({}))?;
        let opening = extension.open(&mut session, &neg_open("x", json!({}), "61"));
        run_job(&mut session, opening).await?;
        let mut round = extension.reconcile(&mut session, "x", "61").unwrap();
        let work = round.work;
        round.work = Box::new(move || {
            // Retain the snapshot until unwinding, just as a panicking decoder would.
            let _snapshot = work;
            panic!("controlled worker failure");
        });
        let mut stream = actor(move |ctx| {
            Negentropies::schedule(round, &mut session, ctx);
            session
        });
        let frame = next_frame(&mut stream).await?;
        assert!(contains(&frame, NEG_ERR));
        assert_eq!(extension.jobs_in_use(), 0);
        assert_eq!(extension.budget.used(), 0);
        Ok(())
    }

    #[actix_rt::test]
    async fn disconnect_cancels_pending_rounds() -> Result<()> {
        let (extension, mut session) = session_with_event("neg-pending-disconnect", json!({}))?;
        let opening = extension.open(&mut session, &neg_open("x", json!({}), "61"));
        run_job(&mut session, opening).await?;
        let mut job = extension.reconcile(&mut session, "x", "61").unwrap();
        drop(session);
        assert!(job.cancelled.has_changed().is_err());
        assert!(job.admit().await.is_err());
        drop(job);
        assert_eq!(extension.jobs_in_use(), 0);
        assert_eq!(extension.budget.used(), 0);
        Ok(())
    }

    /// Build the client side of a reconciliation over `events`.
    fn client(events: &[Event]) -> Result<Negentropy<'static, NegentropyStorageVector>> {
        let mut storage = NegentropyStorageVector::new();
        for event in events {
            storage.insert(event.created_at(), Id::from_byte_array(*event.id()))?;
        }
        storage.seal()?;
        // `initiate()` marks this side as the initiator.
        Ok(Negentropy::owned(storage, 0)?)
    }

    fn test_events(count: u64) -> Result<Vec<Event>> {
        let mut rng = thread_rng();
        let key_pair = Keypair::new_global(&mut rng);
        let start = now();
        (0..count)
            .map(|index| {
                Ok(Event::create(
                    &key_pair,
                    start + index,
                    1000,
                    vec![],
                    format!("test {index}"),
                )?)
            })
            .collect()
    }

    /// Start a relay holding `events`, written straight to the database: a
    /// per-event EVENT/OK round trip would dominate these tests' runtime.
    async fn start_relay(
        name: &str,
        setting: &str,
        events: &[Event],
    ) -> Result<actix_test::TestServer> {
        let app = create_test_app(name)?;
        {
            let mut w = app.setting.write();
            w.extra = serde_json::from_str(setting)?;
        }
        let db = app.db.clone();
        db.batch_put(events.to_vec())?;
        #[cfg(feature = "search")]
        let app = app.add_extension(crate::Search::new())?;
        let app = app.add_extension(Negentropies::new(db))?;
        let app = web::Data::new(app);
        Ok(actix_test::start(move || create_web_app(app.clone())))
    }

    #[actix_rt::test]
    async fn idle_sweep_stops_when_empty_and_restarts_on_open() -> Result<()> {
        let app = create_test_app("neg-sweep-restart")?;
        let mut extension = configured_extension(&app);
        let mut stream = actor(move |ctx| {
            let mut session = Session::new("test".to_owned(), web::Data::new(app));
            extension.connected(&mut session, ctx);
            assert!(sessions_mut(&mut session).idle_sweep.is_none());
            for enabled in [false, true] {
                configure(
                    &mut extension,
                    &session.app.setting,
                    json!({"enabled": enabled, "idle_timeout": "1s"}),
                );
                let text =
                    json!([NEG_OPEN, "bad", {}, if enabled { "zz" } else { "61" }]).to_string();
                let result = extension.message(
                    ClientMessage::new(0, text.clone(), serde_json::from_str(&text).unwrap()),
                    &mut session,
                    ctx,
                );
                assert!(matches!(
                    result,
                    ExtensionMessageResult::Reject(..) | ExtensionMessageResult::Stop(_)
                ));
                assert!(sessions_mut(&mut session).idle_sweep.is_none());
            }
            let job = extension
                .open(&mut session, &neg_open("x", json!({}), "61"))
                .unwrap();
            assert!(sessions_mut(&mut session).idle_sweep.is_none());
            Negentropies::schedule(job, &mut session, ctx);
            assert!(sessions_mut(&mut session).idle_sweep.is_some());
            close(&mut session, "x");
            ctx.run_later(Duration::from_millis(1100), move |session, ctx| {
                assert!(sessions_mut(session).idle_sweep.is_none());
                let job = extension
                    .open(session, &neg_open("again", json!({}), "61"))
                    .unwrap();
                Negentropies::schedule(job, session, ctx);
                assert!(sessions_mut(session).idle_sweep.is_some());
                ctx.text("sweep restarted");
            });
            session
        });
        assert!(contains(&next_frame(&mut stream).await?, "sweep restarted"));
        Ok(())
    }

    fn reload(app: &nostr_relay::App, section: &str, setting: Value) {
        let mut candidate = app.setting.read().clone();
        candidate.extra.insert(section.to_owned(), setting);
        app.extensions
            .write()
            .call_setting(&app.setting, candidate)
            .unwrap();
    }

    #[actix_rt::test]
    async fn disabling_idle_timeout_preserves_existing_snapshots() -> Result<()> {
        check_idle_timeout_reload(Value::Null).await
    }

    #[actix_rt::test]
    async fn lengthening_idle_timeout_preserves_existing_snapshots() -> Result<()> {
        check_idle_timeout_reload(json!("60s")).await
    }

    async fn check_idle_timeout_reload(updated: Value) -> Result<()> {
        let app = create_test_app("neg-timeout-reload")?;
        app.db.batch_put(test_events(1)?)?;
        app.setting.write().extra.insert(
            "negentropy".to_owned(),
            json!({"enabled": true, "idle_timeout": "1s"}),
        );
        let extension = Negentropies::new(app.db.clone());
        let budget = extension.budget.clone();
        let app = web::Data::new(app.add_extension(extension)?);
        let actor_app = app.clone();
        let mut stream = actor(move |ctx| {
            let mut session = Session::new("127.0.0.1".to_owned(), actor_app.clone());
            let text = r#"["NEG-OPEN","sync",{},"61"]"#;
            let out = actor_app.extensions.read().call_message(
                ClientMessage::new(0, text.to_owned(), serde_json::from_str(text).unwrap()),
                &mut session,
                ctx,
            );
            assert!(out.is_none());
            session
                .get_mut::<Sessions>()
                .unwrap()
                .open
                .get_mut("sync")
                .unwrap()
                .last_active = Instant::now() - Duration::from_secs(5);
            session
        });
        let opening = next_frame(&mut stream).await?;
        assert!(contains(&opening, NEG_MSG));
        assert_eq!(budget.used(), 1);
        // Poll the actor before reloading; no additional NEG messages arrive.
        assert!(
            actix_rt::time::timeout(Duration::from_millis(50), stream.next())
                .await
                .is_err()
        );
        reload(
            &app,
            "negentropy",
            json!({"enabled": true, "idle_timeout": updated}),
        );
        // Let two sweep ticks pass. Neither may use the old 1s timeout.
        assert!(
            actix_rt::time::timeout(Duration::from_millis(2100), stream.next())
                .await
                .is_err()
        );
        assert_eq!(budget.used(), 1);
        // The watcher must remain alive so expiry can be enabled again.
        reload(
            &app,
            "negentropy",
            json!({"enabled": true, "idle_timeout": "1s"}),
        );
        let frame = next_frame(&mut stream).await?;
        let expected = neg_err("sync", "closed: reconciliation timed out").0;
        assert_eq!(&frame[2..], expected.as_bytes());
        assert_eq!(budget.used(), 0);
        Ok(())
    }

    #[actix_rt::test]
    async fn a_full_budget_refuses_before_scanning() -> Result<()> {
        let (extension, mut session) =
            session_with_event("neg-budget-precheck", json!({"max_total_records": 1}))?;

        let job = extension.open(&mut session, &neg_open("sync", json!({}), "61"));
        let out = run_job(&mut session, job).await?;
        let res: Value = serde_json::from_str(&out.0)?;
        assert_eq!(res[0], NEG_MSG, "{res:?}");

        let Err(out) = extension.open(&mut session, &neg_open("other", json!({}), "61")) else {
            panic!("a full budget should refuse before scanning");
        };
        let res: Value = serde_json::from_str(&out.0)?;
        assert_eq!(res[0], NEG_ERR);
        assert_eq!(res[2], MEMORY_LIMIT);

        extension.close_reconciliation("sync", &mut session);
        let job = extension.open(&mut session, &neg_open("sync", json!({}), "61"));
        let out = run_job(&mut session, job).await?;
        let res: Value = serde_json::from_str(&out.0)?;
        assert_eq!(res[0], NEG_MSG, "{res:?}");
        Ok(())
    }

    #[actix_rt::test]
    async fn malformed_requests_close_sessions_and_release_the_budget() -> Result<()> {
        let app = create_test_app("neg-invalid-cleanup")?;
        app.db.batch_put(test_events(1)?)?;
        let mut extension = Negentropies::new(app.db.clone());
        configure(
            &mut extension,
            &app.setting,
            json!({"enabled": true, "max_sessions": 1, "max_total_records": 1}),
        );
        let mut session = Session::new("127.0.0.1".to_owned(), web::Data::new(app));
        let malformed = [NEG_OPEN, NEG_MSG].into_iter().flat_map(|command| {
            ["not hex", "00", "610000028180808000"].map(|message| (command, message))
        });
        let open = neg_open("sync", json!({}), "61");
        for payload in ["61", "zz"] {
            let job = extension.reconcile(&mut session, "ghost", payload);
            let out = run_job(&mut session, job).await?;
            let res: Value = serde_json::from_str(&out.0)?;
            assert_eq!(res[0], NEG_ERR);
            assert_eq!(res[2], "closed: no such reconciliation");
        }
        for (command, request) in malformed {
            let job = extension.open(&mut session, &open);
            let out = run_job(&mut session, job).await?;
            let res: Value = serde_json::from_str(&out.0)?;
            assert_eq!(
                res[0], NEG_MSG,
                "opening before {command} {request}: {res:?}"
            );

            let job = if command == NEG_OPEN {
                extension.close_reconciliation("sync", &mut session);
                extension.open(&mut session, &neg_open("sync", json!({}), request))
            } else {
                extension.reconcile(&mut session, "sync", request)
            };
            let out = run_job(&mut session, job).await?;
            let res: Value = serde_json::from_str(&out.0)?;
            assert_eq!(res[0], NEG_ERR, "{command} {request}: {res:?}");
            assert!(res[2].as_str().unwrap().starts_with("invalid:"));

            let job = extension.reconcile(&mut session, "sync", "61");
            let out = run_job(&mut session, job).await?;
            let res: Value = serde_json::from_str(&out.0)?;
            assert_eq!(res[0], NEG_ERR);
            assert_eq!(res[2], "closed: no such reconciliation");

            // A different id proves both the session slot and its one-record
            // budget were released, rather than reclaimed by a replacement.
            let job = extension.open(&mut session, &neg_open("released", json!({}), "61"));
            let out = run_job(&mut session, job).await?;
            let res: Value = serde_json::from_str(&out.0)?;
            assert_eq!(res[0], NEG_MSG, "budget after {request}: {res:?}");
            close(&mut session, "released");
        }
        Ok(())
    }

    #[cfg(feature = "search")]
    fn search_events() -> Result<Vec<Event>> {
        let key_pair = Keypair::new_global(&mut thread_rng());
        let mut events = Vec::new();
        for content in ["nostr alpha", "unrelated", "nostr beta"] {
            let mut event = Event::create(&key_pair, now(), 1, vec![], content.to_owned())?;
            event.build_note_words();
            events.push(event);
        }
        Ok(events)
    }

    #[cfg(feature = "search")]
    #[actix_rt::test]
    async fn search_reconciles_the_same_events_as_req() -> Result<()> {
        let events = search_events()?;
        for search_first in [true, false] {
            let app = create_test_app("neg-search-order")?;
            app.db.batch_put(events.clone())?;
            app.setting.write().extra = serde_json::from_value(json!({
                "negentropy": {"enabled": true}, "search": {"enabled": true}
            }))?;
            let extension = Negentropies::new(app.db.clone());
            let app = if search_first {
                app.add_extension(crate::Search::new())?
                    .add_extension(extension)?
            } else {
                app.add_extension(extension)?
                    .add_extension(crate::Search::new())?
            };
            let app = web::Data::new(app);
            let server_app = app.clone();
            let mut srv = actix_test::start(move || create_web_app(server_app.clone()));
            let mut framed = srv.ws_at("/").await.unwrap();
            for (stage, (enabled, matching)) in
                [(true, 2), (false, 0), (true, 2)].into_iter().enumerate()
            {
                if stage > 0 {
                    let file = tempfile::Builder::new().suffix(".toml").tempfile()?;
                    std::fs::write(
                        file.path(),
                        format!("[negentropy]\nenabled = true\n[search]\nenabled = {enabled}\n"),
                    )?;
                    app.reload(file.path())?;
                }
                let filter = json!({"search": "nostr", "kinds": [1]});
                send(&mut framed, json!(["REQ", "read", filter])).await?;
                let mut expected = Vec::new();
                loop {
                    let res: Value = recv(&mut framed).await?;
                    if res[0] == "EOSE" {
                        break;
                    }
                    assert_eq!(res[0], "EVENT");
                    let event: Event = serde_json::from_value(res[2].clone())?;
                    expected.push(Id::from_byte_array(*event.id()));
                }
                assert_eq!(
                    expected.len(),
                    matching,
                    "search first: {search_first}, stage: {stage}"
                );

                if !enabled {
                    send(&mut framed, json!([NEG_OPEN, "sync", filter, "61"])).await?;
                    let res: Reply = recv(&mut framed).await?;
                    assert_eq!(res.0, NEG_ERR);
                    assert_eq!(res.1, "sync");
                    assert!(res.2.starts_with("unsupported:"));
                    continue;
                }
                let (have, need, _) = drive(&mut framed, "sync", filter, &mut client(&[])?).await?;
                assert!(have.is_empty());
                expected.sort();
                assert_eq!(
                    need, expected,
                    "search first: {search_first}, stage: {stage}"
                );
            }
        }
        Ok(())
    }

    #[actix_rt::test]
    async fn neg_open_obeys_the_req_filter_limitations() -> Result<()> {
        let mut srv = start_relay(
            "neg-limitation",
            r#"{"negentropy": {"enabled": true}}"#,
            &[],
        )
        .await?;
        let mut framed = srv.ws_at("/").await.unwrap();

        send(
            &mut framed,
            json!([NEG_OPEN, "bad", {"ids": ["abcd"]}, "61"]),
        )
        .await?;
        let res: Reply = recv(&mut framed).await?;
        assert_eq!(res.0, NEG_ERR);
        assert_eq!(res.1, "bad");
        assert!(res.2.starts_with("invalid:"), "got {}", res.2);

        send(
            &mut framed,
            json!([NEG_OPEN, "full", {"ids": ["00".repeat(32)]}, "61"]),
        )
        .await?;
        let res: Reply = recv(&mut framed).await?;
        assert_eq!(res.0, NEG_MSG, "got {res:?}");
        Ok(())
    }

    #[actix_rt::test]
    async fn reconciles_missing_events_both_ways() -> Result<()> {
        // The relay holds events 0..8, the client holds 4..12. Each side is
        // missing four events the other has.
        let all = test_events(12)?;
        let mut srv = start_relay(
            "neg-roundtrip",
            r#"{"negentropy": {"enabled": true}}"#,
            &all[0..8],
        )
        .await?;
        let mut framed = srv.ws_at("/").await.unwrap();
        let mut client = client(&all[4..12])?;
        let (have, need, _) =
            drive(&mut framed, "sub", json!({"kinds": [1000]}), &mut client).await?;
        // `have` is what the client holds and the relay does not (events 8..12),
        // `need` is what the relay holds and the client does not (events 0..4).
        assert_eq!(have, ids(&all[8..12]));
        assert_eq!(need, ids(&all[0..4]));
        Ok(())
    }

    #[actix_rt::test]
    async fn identical_sets_reconcile_with_no_differences() -> Result<()> {
        let all = test_events(6)?;
        let mut srv = start_relay(
            "neg-identical",
            r#"{"negentropy": {"enabled": true}}"#,
            &all,
        )
        .await?;
        let mut framed = srv.ws_at("/").await.unwrap();
        let mut client = client(&all)?;
        let (have, need, sizes) =
            drive(&mut framed, "same", json!({"kinds": [1000]}), &mut client).await?;
        assert_eq!(sizes.len(), 1, "reconciliation should finish in one round");
        assert!(have.is_empty());
        assert!(need.is_empty());
        Ok(())
    }

    #[actix_rt::test]
    async fn rejects_a_set_larger_than_max_records() -> Result<()> {
        let all = test_events(5)?;
        let mut srv = start_relay(
            "neg-max-records",
            r#"{"negentropy": {"enabled": true, "max_records": 3}}"#,
            &all,
        )
        .await?;
        let mut framed = srv.ws_at("/").await.unwrap();

        let initial = hex::encode(client(&all)?.initiate()?);
        send(
            &mut framed,
            json!(["NEG-OPEN", "big", {"kinds": [1000]}, initial]),
        )
        .await?;
        let res: Reply = recv(&mut framed).await?;
        assert_eq!(res.0, NEG_ERR);
        assert_eq!(res.1, "big");
        assert_eq!(
            res.2,
            "blocked: too many records for reconciliation (max 3)"
        );
        Ok(())
    }

    #[actix_rt::test]
    async fn neg_close_forgets_the_reconciliation() -> Result<()> {
        let all = test_events(4)?;
        let mut srv =
            start_relay("neg-close", r#"{"negentropy": {"enabled": true}}"#, &all).await?;
        let mut framed = srv.ws_at("/").await.unwrap();

        let mut client = client(&all[0..2])?;
        let initial = client.initiate()?;
        send(
            &mut framed,
            json!(["NEG-OPEN", "bye", {"kinds": [1000]}, hex::encode(&initial)]),
        )
        .await?;
        let res: Reply = recv(&mut framed).await?;
        assert_eq!(res.0, NEG_MSG);

        send(&mut framed, json!(["NEG-CLOSE", "bye"])).await?;

        // The next NEG-MSG for that id must be unknown to the relay.
        send(&mut framed, json!(["NEG-MSG", "bye", "61"])).await?;
        let res: Reply = recv(&mut framed).await?;
        assert_eq!(res.0, NEG_ERR);
        assert_eq!(res.2, "closed: no such reconciliation");
        Ok(())
    }

    /// Expiry driven only by incoming NEG messages would pin the snapshot for
    /// the life of a quiet connection -- the case `idle_timeout` exists for.
    #[actix_rt::test]
    async fn idle_reconciliations_expire_without_client_traffic() -> Result<()> {
        let all = test_events(4)?;
        let mut srv = start_relay(
            "neg-idle",
            r#"{"negentropy": {"enabled": true, "idle_timeout": "1s"}}"#,
            &all,
        )
        .await?;
        let mut framed = srv.ws_at("/").await.unwrap();

        let mut client = client(&all[0..2])?;
        let initial = client.initiate()?;
        send(
            &mut framed,
            json!(["NEG-OPEN", "idle", {"kinds": [1000]}, hex::encode(&initial)]),
        )
        .await?;
        let res: Reply = recv(&mut framed).await?;
        assert_eq!(res.0, NEG_MSG);

        // Send nothing: the sweep must close it and say so unprompted.
        let res: Reply = recv(&mut framed).await?;
        assert_eq!(res.0, NEG_ERR);
        assert_eq!(res.1, "idle");
        assert_eq!(res.2, "closed: reconciliation timed out");

        send(&mut framed, json!(["NEG-MSG", "idle", "61"])).await?;
        let res: Reply = recv(&mut framed).await?;
        assert_eq!(res.0, NEG_ERR);
        assert_eq!(res.2, "closed: no such reconciliation");
        Ok(())
    }

    /// `max_records` and `max_sessions` bound one connection; this bounds the
    /// relay.
    #[actix_rt::test]
    async fn a_snapshot_over_the_relay_budget_is_refused() -> Result<()> {
        let all = test_events(4)?;
        let mut srv = start_relay(
            "neg-budget",
            r#"{"negentropy": {"enabled": true, "max_total_records": 2}}"#,
            &all,
        )
        .await?;
        let mut framed = srv.ws_at("/").await.unwrap();

        let initial = hex::encode(client(&all[0..2])?.initiate()?);
        let open = |sub: &str, filter: Value| json!(["NEG-OPEN", sub, filter, initial]);

        // Four events, a ceiling of two: refused rather than truncated.
        send(&mut framed, open("big", json!({"kinds": [1000]}))).await?;
        let res: Reply = recv(&mut framed).await?;
        assert_eq!(res.0, NEG_ERR);
        assert_eq!(
            res.2,
            "blocked: relay is at its reconciliation memory limit"
        );

        // One inside the ceiling is accepted...
        let one = json!({"kinds": [1000], "since": all[3].created_at()});
        send(&mut framed, open("small", one.clone())).await?;
        let res: Reply = recv(&mut framed).await?;
        assert_eq!(res.0, NEG_MSG, "got {res:?}");

        // ...and closing it repays the budget.
        send(&mut framed, json!(["NEG-CLOSE", "small"])).await?;
        send(&mut framed, open("again", one)).await?;
        let res: Reply = recv(&mut framed).await?;
        assert_eq!(res.0, NEG_MSG, "got {res:?}");
        Ok(())
    }

    #[actix_rt::test]
    async fn disabled_extension_returns_neg_errors() -> Result<()> {
        let app = create_test_app("neg-disabled")?;
        let extension = Negentropies::new(app.db.clone());
        let mut stream = actor(move |ctx| {
            let mut session = Session::new("test".to_owned(), web::Data::new(app));
            let mut extensions = nostr_relay::Extensions::default();
            extensions.add(extension);
            for value in [
                json!([NEG_OPEN, "open", {}, "61"]),
                json!([NEG_MSG, "round", "61"]),
            ] {
                let text = value.to_string();
                let message =
                    ClientMessage::new(0, text.clone(), serde_json::from_str(&text).unwrap());
                assert!(extensions
                    .call_message(message, &mut session, ctx)
                    .is_none());
            }
            assert!(session.get::<Sessions>().is_none());
            session
        });
        let frame = next_frame(&mut stream).await?;
        assert!(contains(
            &frame,
            &neg_err("open", "closed: reconciliation disabled").0
        ));
        assert!(contains(
            &frame,
            &neg_err("round", "closed: reconciliation disabled").0
        ));
        Ok(())
    }

    /// A client with an empty set opens with a five-byte message asking for
    /// every matching id; `frame_size_limit` keeps the answer from arriving as
    /// one enormous frame. The protocol minimum limit lets a small event count
    /// force several rounds.
    #[actix_rt::test]
    async fn an_empty_client_set_gets_a_bounded_response() -> Result<()> {
        let limit = MIN_FRAME_SIZE_LIMIT as usize;
        let all = test_events(300)?;
        let mut srv = start_relay(
            "neg-amplification",
            r#"{"negentropy": {"enabled": true, "frame_size_limit": 4096}}"#,
            &all,
        )
        .await?;
        let mut framed = srv.ws_at("/").await.unwrap();

        let mut client = client(&[])?;
        let (have, need, sizes) =
            drive(&mut framed, "flood", json!({"kinds": [1000]}), &mut client).await?;
        assert!(sizes.iter().all(|size| *size <= limit), "{sizes:?}");
        // More than one round proves the response was actually split rather
        // than fitting under the limit by accident.
        assert!(sizes.len() > 1, "expected a split response, got {sizes:?}");
        assert!(have.is_empty());
        assert_eq!(need, ids(&all));
        Ok(())
    }

    #[test]
    fn frame_size_limit_below_the_protocol_minimum_is_raised() {
        let setting: SettingWrapper = nostr_relay::setting::Setting::default().into();
        {
            let mut w = setting.write();
            w.extra = serde_json::from_str(
                r#"{"negentropy": {"enabled": true, "frame_size_limit": 1024}}"#,
            )
            .unwrap();
        }

        let mut ext = Negentropies::new(Arc::new(
            nostr_relay::db::Db::open(crate::temp_data_path("neg-frame").unwrap().path()).unwrap(),
        ));
        ext.setting(&mut setting.write()).unwrap()();
        assert_eq!(ext.setting.frame_size_limit, MIN_FRAME_SIZE_LIMIT);
        assert!(setting.read().information.supported_nips.contains(&77));
    }
}
