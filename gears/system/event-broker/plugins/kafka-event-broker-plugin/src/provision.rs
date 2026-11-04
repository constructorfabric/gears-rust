//! `provision` (`docs/DESIGN.md` §5.1): a compare-and-set on the topic's log
//! record under the topic's own transactional id, and the Kafka topic it
//! names.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use event_broker_sdk::backend::support::topic_uuid;
use event_broker_sdk::backend::{BackendError, LogClaim, Provisioning, TopicSpec};
use rdkafka::admin::{AlterConfig, NewTopic, ResourceSpecifier, TopicReplication};
use rdkafka::error::{KafkaError, RDKafkaErrorCode};
use rdkafka::producer::{FutureProducer, FutureRecord, Producer};
use tokio::sync::watch;
use uuid::Uuid;

use crate::clients::{self, Admin};
use crate::error::{self, Operation};
use crate::fence;
use crate::names;
use crate::records::{LogRecord, log_key};
use crate::shared::{KnownTopic, Shared};

const OP: Operation = Operation::Provision;

/// The most partitions one topic may have on this backend.
pub const MAX_PARTITIONS: u32 = 10_000;

/// The longest segment the plugin asks for: Kafka's own default.
const MAX_SEGMENT: Duration = Duration::from_hours(24 * 7);

/// What the Kafka topic of `spec` is configured with: every setting the
/// plugin owns, so the full set can replace what the topic holds.
pub fn topic_configs(spec: &TopicSpec, min_insync_replicas: u16) -> BTreeMap<&'static str, String> {
    let mut configs = BTreeMap::from([
        ("cleanup.policy", "delete".to_owned()),
        ("message.timestamp.type", "LogAppendTime".to_owned()),
        ("min.insync.replicas", min_insync_replicas.to_string()),
        (
            "retention.ms",
            spec.retention
                .max_age
                .map_or_else(|| "-1".to_owned(), |age| millis(age).to_string()),
        ),
        (
            "retention.bytes",
            spec.retention.max_bytes.map_or_else(
                || "-1".to_owned(),
                |bytes| i64::try_from(bytes).unwrap_or(i64::MAX).to_string(),
            ),
        ),
    ]);
    // A segment goes only once its newest record is past the age bound, so
    // the segment length is how long an event may outlive it.
    if let Some(age) = spec.retention.max_age {
        configs.insert(
            "segment.ms",
            millis(age.min(MAX_SEGMENT)).max(1).to_string(),
        );
    }
    configs
}

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// How long a call waits for its provisioning job before answering
/// `Pending`: well inside any caller's own timeout.
const ANSWER_WITHIN: Duration = Duration::from_secs(2);

/// How many request timeouts a job's `init_transactions` may retry for while
/// the coordinator is not ready (a broker's first transaction creates its
/// transaction state log).
const OPEN_BUDGET_TIMEOUTS: u32 = 6;

/// One topic's provisioning attempt, run by a task of the instance's own.
pub struct Job {
    spec: TopicSpec,
    claim: LogClaim,
    answer: watch::Receiver<Option<Result<Provisioning, BackendError>>>,
}

/// The jobs of an instance, by topic uuid.
pub type Jobs = Mutex<HashMap<Uuid, Job>>;

/// Claims the topic's log for `claim` and creates its Kafka topic;
/// `LogConflict` when the stored log id is not one the claim accepts.
///
/// The work runs as a task of the instance, not in the caller's future: a
/// caller that gives up (the gear bounds startup provisioning) cancels
/// nothing half-done, and the next call with the same spec and claim picks up
/// the same attempt. A call answers `Pending` once it has waited
/// [`ANSWER_WITHIN`]; a call for another claim while an attempt runs answers
/// `Pending` too, and starts its own once that one has ended.
pub async fn provision(
    shared: &Arc<Shared>,
    spec: &TopicSpec,
    claim: &LogClaim,
) -> Result<Provisioning, BackendError> {
    if spec.partitions == 0 || spec.partitions > MAX_PARTITIONS {
        return Err(BackendError::invalid_config(format!(
            "a topic has between 1 and {MAX_PARTITIONS} partitions on the kafka backend"
        )));
    }
    shared.check_running(OP)?;
    let topic = topic_uuid(&spec.topic);
    let mut answer = {
        let mut jobs = shared
            .provisions
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match jobs.get(&topic) {
            Some(job) if job.spec == *spec && job.claim == *claim => job.answer.clone(),
            Some(job) if job.answer.borrow().is_none() => return Ok(Provisioning::Pending),
            _ => {
                let answer = spawn_job(shared, topic, spec.clone(), *claim);
                jobs.insert(
                    topic,
                    Job {
                        spec: spec.clone(),
                        claim: *claim,
                        answer: answer.clone(),
                    },
                );
                answer
            }
        }
    };
    let answered = tokio::time::timeout(ANSWER_WITHIN, async {
        answer
            .wait_for(Option::is_some)
            .await
            .map(|value| value.clone())
    })
    .await;
    let result = match answered {
        Err(_) => return Ok(Provisioning::Pending),
        Ok(Ok(Some(result))) => result,
        Ok(_) => Err(BackendError::unavailable(
            "provision: the provisioning attempt ended without an answer",
        )),
    };
    // The answer is handed out once; the next call starts afresh.
    let mut jobs = shared
        .provisions
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    if jobs
        .get(&topic)
        .is_some_and(|job| job.answer.same_channel(&answer))
    {
        jobs.remove(&topic);
    }
    result
}

fn spawn_job(
    shared: &Arc<Shared>,
    topic: Uuid,
    spec: TopicSpec,
    claim: LogClaim,
) -> watch::Receiver<Option<Result<Provisioning, BackendError>>> {
    let (tx, rx) = watch::channel(None);
    let shared = Arc::clone(shared);
    tokio::spawn(async move {
        let result = match shared.enter_owned(OP) {
            Ok(in_flight) => {
                let result = attempt(&shared, topic, &spec, &claim).await;
                drop(in_flight);
                result
            }
            Err(err) => Err(err),
        };
        // Every caller gave up waiting: the next one starts afresh.
        drop(tx.send(Some(result)));
    });
    rx
}

/// One provisioning attempt, start to end.
async fn attempt(
    shared: &Shared,
    topic: Uuid,
    spec: &TopicSpec,
    claim: &LogClaim,
) -> Result<Provisioning, BackendError> {
    shared.ensure_internal(OP).await?;
    let name = names::event_topic(&shared.settings.topic_prefix, topic);
    let id = names::log_transaction(&shared.settings.topic_prefix, topic);
    // Fences every other provisioning of this topic until it is dropped.
    let budget = shared.settings.request_timeout * OPEN_BUDGET_TIMEOUTS;
    let producer = fence::open_producer(shared, &id, OP, budget).await?;
    let (provisioning, partitions) =
        claimed(shared, spec, claim, &name, producer.producer()).await?;
    if let Provisioning::Ready { .. } = provisioning {
        shared.remember(
            topic,
            KnownTopic {
                name: Arc::from(name),
                partitions,
            },
        );
    }
    Ok(provisioning)
}

async fn claimed(
    shared: &Shared,
    spec: &TopicSpec,
    claim: &LogClaim,
    name: &str,
    producer: &Arc<FutureProducer>,
) -> Result<(Provisioning, u32), BackendError> {
    let topic = topic_uuid(&spec.topic);
    let key = log_key(topic);
    let mut logs = shared.logs(OP).await?;
    let stored = logs.get(&key).and_then(LogRecord::decode);
    if !claim.accepts(stored.as_ref().map(LogRecord::log_id)) {
        return Err(BackendError::log_conflict(OP.name()));
    }
    let admin = shared.admin(OP)?;
    let Some(partitions) = ready_partitions(shared, &admin, name).await? else {
        create(shared, &admin, spec, name).await?;
        return Ok((Provisioning::Pending, 0));
    };
    if partitions == 0 {
        return Ok((Provisioning::Pending, 0));
    }
    reconfigure(shared, &admin, spec, name).await?;
    let record = LogRecord::new(spec.topic.as_ref(), claim.new, partitions);
    if stored.as_ref() != Some(&record) {
        let value = record.encode();
        write_record(shared, producer, &key, &value)
            .await
            .map_err(|e| error::map(OP, &e))?;
        logs.put(key, value);
    }
    Ok((Provisioning::Ready { partitions }, partitions))
}

/// `None` when the Kafka topic does not exist; `Some(0)` while a partition
/// has no leader; otherwise its partition count.
async fn ready_partitions(
    shared: &Shared,
    admin: &Arc<Admin>,
    name: &str,
) -> Result<Option<u32>, BackendError> {
    let admin = Arc::clone(admin);
    let topic = name.to_owned();
    let timeout = shared.settings.request_timeout;
    let metadata = clients::detached(move || admin.inner().fetch_metadata(Some(&topic), timeout))
        .await
        .map_err(|_| BackendError::Internal(format!("{OP}: a metadata request was cancelled")))?
        .map_err(|e| error::map(OP, &e))?;
    let Some(found) = metadata.topics().iter().find(|t| t.name() == name) else {
        return Ok(None);
    };
    match found.error().map(RDKafkaErrorCode::from) {
        None => {}
        Some(RDKafkaErrorCode::UnknownTopicOrPartition | RDKafkaErrorCode::UnknownTopic) => {
            return Ok(None);
        }
        Some(RDKafkaErrorCode::LeaderNotAvailable) => return Ok(Some(0)),
        Some(code) => return Err(error::map_code(OP, code)),
    }
    let partitions = found.partitions();
    if partitions.is_empty()
        || partitions
            .iter()
            .any(|p| p.leader() < 0 || p.error().is_some())
    {
        return Ok(Some(0));
    }
    Ok(Some(u32::try_from(partitions.len()).unwrap_or(u32::MAX)))
}

async fn create(
    shared: &Shared,
    admin: &Admin,
    spec: &TopicSpec,
    name: &str,
) -> Result<(), BackendError> {
    let configs = topic_configs(spec, shared.settings.min_insync_replicas);
    let mut new = NewTopic::new(
        name,
        i32::try_from(spec.partitions).unwrap_or(i32::MAX),
        TopicReplication::Fixed(i32::from(shared.settings.replication_factor)),
    );
    for (config, value) in &configs {
        new = new.set(config, value);
    }
    let results = admin
        .create_topics([&new], &shared.admin_options())
        .await
        .map_err(|e| error::map(OP, &e))?;
    for result in results {
        match result {
            Ok(_) | Err((_, RDKafkaErrorCode::TopicAlreadyExists)) => {}
            Err((_, code)) => return Err(error::map_code(OP, code)),
        }
    }
    Ok(())
}

/// Brings the topic's configuration to `spec`'s when it differs: the
/// non-incremental `AlterConfigs` replaces the whole set, so it carries every
/// setting the plugin owns.
async fn reconfigure(
    shared: &Shared,
    admin: &Admin,
    spec: &TopicSpec,
    name: &str,
) -> Result<(), BackendError> {
    let wanted = topic_configs(spec, shared.settings.min_insync_replicas);
    let described = admin
        .describe_configs([&ResourceSpecifier::Topic(name)], &shared.admin_options())
        .await
        .map_err(|e| error::map(OP, &e))?;
    let current = match described.into_iter().next() {
        Some(Ok(resource)) => resource,
        Some(Err(code)) => return Err(error::map_code(OP, code)),
        None => {
            return Err(BackendError::Internal(format!(
                "{OP}: no topic configuration returned"
            )));
        }
    };
    let entries = current.entry_map();
    let differs = wanted.iter().any(|(config, value)| {
        entries.get(config).and_then(|entry| entry.value.as_deref()) != Some(value.as_str())
    });
    if !differs {
        return Ok(());
    }
    let mut alter = AlterConfig::new(ResourceSpecifier::Topic(name));
    for (config, value) in &wanted {
        alter = alter.set(config, value);
    }
    let results = admin
        .alter_configs([&alter], &shared.admin_options())
        .await
        .map_err(|e| error::map(OP, &e))?;
    for result in results {
        if let Err((_, code)) = result {
            return Err(error::map_code(OP, code));
        }
    }
    Ok(())
}

/// The log record, in a transaction of the topic's own producer.
async fn write_record(
    shared: &Shared,
    producer: &Arc<FutureProducer>,
    key: &[u8],
    value: &[u8],
) -> Result<(), KafkaError> {
    let timeout = shared.settings.request_timeout;
    let transaction = fence::Transaction::begin(producer, timeout)?;
    let sent = async {
        let record = FutureRecord::to(&shared.logs_topic)
            .partition(0)
            .key(key)
            .payload(value);
        producer
            .send_result(record)
            .map_err(|(err, _)| err)?
            .await
            .map_err(|_| KafkaError::Canceled)?
            .map_err(|(err, _)| err)?;
        Ok::<_, KafkaError>(())
    }
    .await;
    if let Err(err) = sent {
        fence::abort(producer, timeout).await;
        transaction.end();
        return Err(err);
    }
    let committed = fence::blocking(producer, move |p| p.commit_transaction(timeout)).await;
    if committed.is_err() {
        fence::abort(producer, timeout).await;
    }
    transaction.end();
    committed
}
