//! The conformance runner: the Rust implementation, held to the same language-neutral
//! fixtures in `libs/ops/conformance/cases` that Go is held to.
//!
//! That is the point. Go passing its own tests proves nothing about Rust; both passing the
//! *same* cases is what keeps two implementations honest without shared code. A case this
//! runner does not recognise is a failure, not a skip, so a fixture added for one language
//! cannot be silently ignored by the other.
//!
//! Needs a PostgreSQL in `OPS_TEST_DATABASE_URL`. The suite creates and drops a database
//! per case, which is the cheapest isolation PostgreSQL offers.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::postgres::PgConnectOptions;
use sqlx::{Connection, PgConnection, PgPool};
use tokio::sync::Mutex;
use uuid::Uuid;

use myota_ops::{
    hash_request, Backoff, Event, IdempotencyOutcome, Job, OpsError, Store, StoreOptions,
};

const CASES_DIRECTORY: &str = "../conformance/cases";

#[derive(Debug, Deserialize)]
struct Case {
    name: String,
    #[allow(dead_code)]
    description: Option<String>,
    steps: Vec<Step>,
    expect: HashMap<String, Value>,
}

// The fixtures are shared with Go and use camelCase keys. Without this every
// multi-word field silently falls back to its default, which is how three cases
// "passed" their steps while testing nothing.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Step {
    op: String,
    #[serde(default)]
    events: Vec<FixtureEvent>,
    #[serde(default)]
    jobs: Vec<FixtureJob>,
    #[serde(default)]
    count: i64,
    #[serde(default)]
    claimers: i64,
    #[serde(default)]
    max_attempts: i32,
    #[serde(default)]
    outcome: String,
    #[serde(default)]
    key: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    response: String,
    #[serde(default)]
    status: i32,
    #[serde(default)]
    expect_error: bool,
    #[serde(default)]
    lease_expires: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FixtureEvent {
    id: String,
    subject: String,
    #[serde(rename = "type")]
    event_type: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FixtureJob {
    id: String,
    kind: String,
}

/// Everything one case observes.
struct Harness {
    store: Store,
    pool: PgPool,
    /// A store whose lease has already lapsed when a claim returns — how the
    /// lease-expiry case is expressed without sleeping.
    expiring_store: Store,
    symbol_of: HashMap<Uuid, String>,
    id_of: HashMap<String, Uuid>,
    published: Vec<String>,
    claimed: Vec<String>,
    attempts: Vec<i32>,
    outcomes: Vec<String>,
}

impl Harness {
    fn id(&mut self, symbol: &str) -> Uuid {
        if let Some(id) = self.id_of.get(symbol) {
            return *id;
        }
        let id = Uuid::new_v4();
        self.id_of.insert(symbol.to_string(), id);
        self.symbol_of.insert(id, symbol.to_string());
        id
    }
}

/// test_database_url returns the admin DSN, or None when the suite should skip.
fn test_database_url() -> Option<String> {
    std::env::var("OPS_TEST_DATABASE_URL")
        .ok()
        .filter(|value| !value.is_empty())
}

/// Creates a database and returns a pool connected to it.
async fn fresh_pool() -> PgPool {
    let admin_url = test_database_url().expect("checked by the caller");
    let mut admin = PgConnection::connect(&admin_url)
        .await
        .expect("connecting to the test server");

    let name = format!("ops_rs_{}", &Uuid::new_v4().to_string()[..8]);
    sqlx::query(&format!("CREATE DATABASE {name}"))
        .execute(&mut admin)
        .await
        .expect("creating a test database");
    let _ = admin.close().await;

    let options = PgConnectOptions::from_str(&admin_url)
        .expect("parsing the test DSN")
        .database(&name);
    let pool = PgPool::connect_with(options)
        .await
        .expect("connecting to the test database");

    myota_ops::migrate(&pool)
        .await
        .expect("migrating the test database");
    pool
}

async fn build_harness() -> Harness {
    let pool = fresh_pool().await;
    let store = Store::new(pool.clone());
    let expiring_store = Store::with_options(
        pool.clone(),
        StoreOptions::default()
            .owner("expiring")
            .lease_duration(std::time::Duration::from_nanos(1)),
    );
    Harness {
        store,
        pool,
        expiring_store,
        symbol_of: HashMap::new(),
        id_of: HashMap::new(),
        published: Vec::new(),
        claimed: Vec::new(),
        attempts: Vec::new(),
        outcomes: Vec::new(),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn conformance() {
    let Some(_) = test_database_url() else {
        eprintln!("skipping: OPS_TEST_DATABASE_URL is not set");
        return;
    };

    let mut entries: Vec<_> = std::fs::read_dir(CASES_DIRECTORY)
        .expect("reading the cases directory")
        .filter_map(std::result::Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
        .collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);

    assert!(
        !entries.is_empty(),
        "{CASES_DIRECTORY} holds no cases, so this suite asserts nothing"
    );

    let mut failures = Vec::new();
    for entry in entries {
        let body = std::fs::read_to_string(entry.path()).expect("reading a case");
        let case: Case = serde_json::from_str(&body).unwrap_or_else(|error| {
            panic!("{} is not a valid case: {error}", entry.path().display())
        });

        if let Err(message) = run_case(&case).await {
            failures.push(format!("{}: {message}", case.name));
        }
    }

    assert!(
        failures.is_empty(),
        "conformance failures:\n{}",
        failures.join("\n")
    );
}

/// Runs one case and returns a description of the first thing that did not hold.
async fn run_case(case: &Case) -> Result<(), String> {
    let mut harness = build_harness().await;

    for (index, step) in case.steps.iter().enumerate() {
        match apply(&mut harness, step).await {
            Ok(()) => {
                if step.expect_error {
                    return Err(format!(
                        "step {} ({}) was expected to fail and did not",
                        index + 1,
                        step.op
                    ));
                }
            }
            Err(error) => {
                if step.expect_error {
                    continue;
                }
                return Err(format!("step {} ({}): {error}", index + 1, step.op));
            }
        }
    }

    let observed = observe(&harness).await?;
    for (key, want) in &case.expect {
        let Some(got) = observed.get(key) else {
            return Err(format!(
                "expectation {key:?} is not an outcome this runner produces"
            ));
        };
        if got != want {
            return Err(format!("{key} = {got}, want {want}"));
        }
    }
    Ok(())
}

async fn observe(harness: &Harness) -> Result<HashMap<String, Value>, String> {
    let mut observed = HashMap::new();
    observed.insert("published".into(), json!(harness.published));
    observed.insert("publishedCount".into(), json!(harness.published.len()));
    observed.insert(
        "publishedDistinct".into(),
        json!(distinct(&harness.published)),
    );
    observed.insert("claimed".into(), json!(harness.claimed));
    observed.insert("claimedCount".into(), json!(harness.claimed.len()));
    observed.insert("claimedDistinct".into(), json!(distinct(&harness.claimed)));
    observed.insert("claimedAttempts".into(), json!(harness.attempts));
    observed.insert("idempotencyOutcomes".into(), json!(harness.outcomes));

    let (remaining,): (i64,) = sqlx::query_as("SELECT count(*) FROM ops.outbox_event")
        .fetch_one(&harness.pool)
        .await
        .map_err(|error| error.to_string())?;
    observed.insert("remainingEvents".into(), json!(remaining));

    let (unpublished,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM ops.outbox_event WHERE published_at IS NULL")
            .fetch_one(&harness.pool)
            .await
            .map_err(|error| error.to_string())?;
    observed.insert("remainingUnpublished".into(), json!(unpublished));

    let (dead_letters,): (i64,) = sqlx::query_as("SELECT count(*) FROM ops.dead_letter")
        .fetch_one(&harness.pool)
        .await
        .map_err(|error| error.to_string())?;
    observed.insert("deadLetters".into(), json!(dead_letters));

    let mut states = serde_json::Map::new();
    for (symbol, id) in &harness.id_of {
        let row: Option<(String,)> = sqlx::query_as("SELECT state FROM ops.job WHERE id = $1")
            .bind(id)
            .fetch_optional(&harness.pool)
            .await
            .map_err(|error| error.to_string())?;
        if let Some((state,)) = row {
            states.insert(symbol.clone(), Value::String(state));
        }
    }
    observed.insert("jobStates".into(), Value::Object(states));

    Ok(observed)
}

fn distinct(values: &[String]) -> bool {
    let mut seen = std::collections::HashSet::with_capacity(values.len());
    values.iter().all(|value| seen.insert(value))
}

async fn apply(harness: &mut Harness, step: &Step) -> Result<(), String> {
    let database = |error: OpsError| error.to_string();

    match step.op.as_str() {
        "outbox.write" | "outbox.write_rollback" => {
            let rollback = step.op.ends_with("rollback");
            let mut prepared = Vec::with_capacity(step.events.len());
            for event in &step.events {
                let id = harness.id(&event.id);
                prepared.push(Event {
                    id,
                    event_type: event.event_type.clone(),
                    subject: event.subject.clone(),
                    payload: json!({}),
                    time: None,
                });
            }

            // `begin` directly rather than a library helper: the transaction has to be
            // the caller's, because the point is that the event commits with the change.
            let mut transaction = harness
                .pool
                .begin()
                .await
                .map_err(|error| error.to_string())?;
            let mut failure: Option<String> = None;
            for event in &prepared {
                if let Err(error) = myota_ops::enqueue(&mut transaction, event).await {
                    failure = Some(error.to_string());
                    break;
                }
            }

            if rollback || failure.is_some() {
                let _ = transaction.rollback().await;
            } else if let Err(error) = transaction.commit().await {
                failure = Some(error.to_string());
            }

            match failure {
                // The rollback case deliberately fails the transaction; that is the point.
                _ if rollback => Ok(()),
                None => Ok(()),
                Some(message) => Err(message),
            }
        }

        "outbox.relay" => {
            harness.store.relay_drain(100).await.map_err(database)?;
            let feed = harness
                .store
                .read_events(0, 500, &[])
                .await
                .map_err(database)?;
            harness.published = feed
                .events
                .iter()
                .map(|event| {
                    harness
                        .symbol_of
                        .get(&event.id())
                        .cloned()
                        .unwrap_or_else(|| event.id().to_string())
                })
                .collect();
            Ok(())
        }

        "job.enqueue" => {
            let mut prepared = Vec::with_capacity(step.jobs.len());
            for job in &step.jobs {
                let id = harness.id(&job.id);
                let mut built = Job::new(job.kind.clone(), json!({})).with_id(id);
                if step.max_attempts > 0 {
                    built = built.with_max_attempts(step.max_attempts);
                }
                prepared.push(built);
            }
            for job in prepared {
                harness.store.enqueue_job(job).await.map_err(database)?;
            }
            Ok(())
        }

        "job.claim" => {
            let claimers = step.claimers.max(1);
            let count = step.count.max(1);
            let claimed: Arc<Mutex<Vec<Job>>> = Arc::new(Mutex::new(Vec::new()));

            if claimers == 1 {
                let store = if step.lease_expires {
                    harness.expiring_store.clone()
                } else {
                    harness.store.clone()
                };
                let jobs = store.claim(count).await.map_err(database)?;
                for job in jobs {
                    harness.claimed.push(harness.symbol_of[&job.id].clone());
                    harness.attempts.push(job.attempt);
                }
                return Ok(());
            }

            let mut handles = Vec::with_capacity(claimers as usize);
            for slot in 0..claimers {
                let store = Store::with_options(
                    harness.pool.clone(),
                    StoreOptions::default().owner(format!("claim-{slot}")),
                );
                let claimed = claimed.clone();
                handles.push(tokio::spawn(async move {
                    let jobs = store.claim(count).await?;
                    let mut guard = claimed.lock().await;
                    guard.extend(jobs);
                    Ok::<(), OpsError>(())
                }));
            }
            for handle in handles {
                handle
                    .await
                    .map_err(|error| error.to_string())?
                    .map_err(database)?;
            }

            let mut guard = claimed.lock().await;
            for job in guard.drain(..) {
                harness.claimed.push(harness.symbol_of[&job.id].clone());
                harness.attempts.push(job.attempt);
            }
            Ok(())
        }

        "job.settle" => {
            let Some(symbol) = harness.claimed.last().cloned() else {
                return Err("job.settle with nothing claimed".into());
            };
            let id = harness.id_of[&symbol];
            match step.outcome.as_str() {
                "complete" => harness.store.complete(id).await.map_err(database),
                "fail" => {
                    harness
                        .store
                        .fail(id, "conformance: deliberate failure")
                        .await
                        .map_err(database)?;
                    // Clear the backoff so the next claim is immediate: the case is about
                    // the attempt count, not about waiting.
                    sqlx::query("UPDATE ops.job SET available_at = now() WHERE id = $1")
                        .bind(id)
                        .execute(&harness.pool)
                        .await
                        .map_err(|error| error.to_string())?;
                    Ok(())
                }
                other => Err(format!("unknown job.settle outcome {other:?}")),
            }
        }

        "idempotency.request" => {
            let hash = hash_request(step.body.as_bytes());
            match harness
                .store
                .claim_idempotency("conformance", &step.key, &hash, 0)
                .await
            {
                Err(OpsError::IdempotencyConflict(_)) => {
                    harness.outcomes.push("conflict".into());
                    Ok(())
                }
                Err(OpsError::IdempotencyInFlight(_)) => {
                    harness.outcomes.push("in_flight".into());
                    Ok(())
                }
                Err(error) => Err(database(error)),
                Ok((IdempotencyOutcome::Replay, stored)) => {
                    harness.outcomes.push("replay".into());
                    if stored.is_none() {
                        return Err("a replay returned no stored response".into());
                    }
                    Ok(())
                }
                Ok((IdempotencyOutcome::Proceed, _)) => {
                    harness.outcomes.push("proceed".into());
                    harness
                        .store
                        .save_idempotent_response(
                            "conformance",
                            &step.key,
                            step.status,
                            step.response.as_bytes(),
                        )
                        .await
                        .map_err(database)
                }
            }
        }

        other => Err(format!("unknown op {other:?}")),
    }
}

/// Keeps `Backoff` referenced so the export is exercised from a test rather than only by
/// the Go side.
#[test]
fn backoff_is_bounded() {
    assert_eq!(Backoff::delay(0).num_seconds(), 0);
    assert_eq!(Backoff::delay(1).num_seconds(), 2);
    assert_eq!(Backoff::delay(3).num_seconds(), 8);
    assert_eq!(Backoff::delay(8).num_seconds(), 60);
    assert_eq!(Backoff::delay(20).num_seconds(), 60);
}
