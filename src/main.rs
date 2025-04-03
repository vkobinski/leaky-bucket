use axum::{
    extract::{Request, State},
    middleware::{from_fn_with_state, Next},
    response::Response,
    routing::get,
    Router,
};
use std::{
    collections::VecDeque,
    hash::{Hash, Hasher},
    sync::Arc,
    time::{Duration, Instant},
};
use strum::IntoEnumIterator;
use strum_macros::{EnumIter, EnumString, EnumVariantNames};
use tokio::sync::{oneshot, Mutex};

//
// RateLimitPolicy and helper enums
//

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UserCategory {
    PF,
    PJ,
}

impl Default for UserCategory {
    fn default() -> Self {
        Self::PJ
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ParticipantCategory {
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
}

impl Default for ParticipantCategory {
    fn default() -> Self {
        Self::A
    }
}

#[derive(Debug, Clone, Copy, EnumIter, PartialEq, Eq, Hash)]
pub enum RateLimitPolicy {
    EntriesReadUserAntiscan(UserCategory),
    EntriesReadUserAntiscanV2(UserCategory),
    EntriesReadParticipantAntiscan(ParticipantCategory),
    EntriesStatisticsRead,
    EntriesWrite,
    EntriesUpdate,
    ClaimsRead,
    ClaimsWrite,
    ClaimsListWithRole,
    ClaimsListWithoutRole,
    SyncVerificationsWrite,
    CidsFilesWrite,
    CidsFilesRead,
    CidsEventsList,
    CidsEntriesRead,
    InfractionReportsRead,
    InfractionReportsWrite,
    InfractionReportsListWithRole,
    InfractionReportsListWithoutRole,
    KeysCheck,
    RefundsRead,
    RefundsWrite,
    RefundListWithRole,
    RefundListWithoutRole,
    FraudMarkersRead,
    FraudMarkersWrite,
    PersonsStatisticsRead,
    PoliciesRead,
    PoliciesList,
}

impl RateLimitPolicy {
    /// Returns (leak_rate, capacity)
    pub fn config(&self) -> (f64, f64) {
        match *self {
            RateLimitPolicy::EntriesReadUserAntiscan(category)
            | RateLimitPolicy::EntriesReadUserAntiscanV2(category) => match category {
                UserCategory::PF => (2.0 / 60.0, 100.0),
                UserCategory::PJ => (20.0 / 60.0, 1000.0),
            },
            RateLimitPolicy::EntriesReadParticipantAntiscan(category) => match category {
                ParticipantCategory::A => (25000.0 / 60.0, 50000.0),
                ParticipantCategory::B => (20000.0 / 60.0, 40000.0),
                ParticipantCategory::C => (15000.0 / 60.0, 30000.0),
                ParticipantCategory::D => (8000.0 / 60.0, 16000.0),
                ParticipantCategory::E => (2500.0 / 60.0, 5000.0),
                ParticipantCategory::F => (250.0 / 60.0, 500.0),
                ParticipantCategory::G => (25.0 / 60.0, 250.0),
                ParticipantCategory::H => (2.0 / 60.0, 50.0),
            },
            RateLimitPolicy::EntriesStatisticsRead => (600.0 / 60.0, 600.0),
            RateLimitPolicy::EntriesWrite => (1200.0 / 60.0, 36000.0),
            RateLimitPolicy::EntriesUpdate => (600.0 / 60.0, 600.0),
            RateLimitPolicy::ClaimsRead => (600.0 / 60.0, 18000.0),
            RateLimitPolicy::ClaimsWrite => (1200.0 / 60.0, 36000.0),
            RateLimitPolicy::ClaimsListWithRole => (40.0 / 60.0, 200.0),
            RateLimitPolicy::ClaimsListWithoutRole => (10.0 / 60.0, 50.0),
            RateLimitPolicy::SyncVerificationsWrite => (10.0 / 60.0, 50.0),
            RateLimitPolicy::CidsFilesWrite => (40.0 / 86_400.0, 200.0),
            RateLimitPolicy::CidsFilesRead => (10.0 / 60.0, 50.0),
            RateLimitPolicy::CidsEventsList => (20.0 / 60.0, 100.0),
            RateLimitPolicy::CidsEntriesRead => (1200.0 / 60.0, 36000.0),
            RateLimitPolicy::InfractionReportsRead => (600.0 / 60.0, 18000.0),
            RateLimitPolicy::InfractionReportsWrite => (1200.0 / 60.0, 36000.0),
            RateLimitPolicy::InfractionReportsListWithRole => (40.0 / 60.0, 200.0),
            RateLimitPolicy::InfractionReportsListWithoutRole => (10.0 / 60.0, 50.0),
            RateLimitPolicy::KeysCheck => (70.0 / 60.0, 70.0),
            RateLimitPolicy::RefundsRead => (1200.0 / 60.0, 36000.0),
            RateLimitPolicy::RefundsWrite => (2400.0 / 60.0, 72000.0),
            RateLimitPolicy::RefundListWithRole => (40.0 / 60.0, 200.0),
            RateLimitPolicy::RefundListWithoutRole => (10.0 / 60.0, 50.0),
            RateLimitPolicy::FraudMarkersRead => (600.0 / 60.0, 18000.0),
            RateLimitPolicy::FraudMarkersWrite => (1200.0 / 60.0, 36000.0),
            RateLimitPolicy::PersonsStatisticsRead => (12000.0 / 60.0, 36000.0),
            RateLimitPolicy::PoliciesRead => (60.0 / 60.0, 200.0),
            RateLimitPolicy::PoliciesList => (6.0 / 60.0, 20.0),
        }
    }
}

//
// Bucket and LeakyBucket with FIFO waiting queue
//

#[derive(Debug)]
struct Bucket {
    tokens: f64,
    last_updated: Instant,
    policy: RateLimitPolicy,
    // FIFO queue of waiting tasks
    waiters: VecDeque<oneshot::Sender<()>>,
}

impl Bucket {
    fn new(policy: RateLimitPolicy) -> Self {
        let (_, capacity) = policy.config();
        Self {
            tokens: capacity,
            last_updated: Instant::now(),
            policy,
            waiters: VecDeque::new(),
        }
    }

    /// Replenish tokens based on elapsed time and notify waiting tasks.
    async fn refill(&mut self) {
        let (leak_rate, capacity) = self.policy.config();
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_updated).as_secs_f64();
        self.last_updated = now;

        // Add tokens but do not exceed capacity.
        self.tokens = (self.tokens + elapsed * leak_rate).min(capacity);

        // While tokens are available and there are waiting tasks, consume a token and notify one waiter.
        while self.tokens >= 1.0 {
            if let Some(waiter) = self.waiters.pop_front() {
                // Consume token for this waiter.
                self.tokens -= 1.0;
                // Notify the waiting task.
                let _ = waiter.send(());
            } else {
                break;
            }
        }
    }
}

/// The overall rate limiter.
/// We store one bucket per policy in an Arc so that each bucket is independently locked.
#[derive(Debug)]
struct LeakyBucket {
    buckets: Vec<Arc<Mutex<Bucket>>>,
}

impl LeakyBucket {
    fn new() -> Self {
        let buckets = RateLimitPolicy::iter()
            .map(|policy| Arc::new(Mutex::new(Bucket::new(policy))))
            .collect();
        Self { buckets }
    }

    async fn acquire(&self, policy: RateLimitPolicy) {
        // Find the matching bucket
        let bucket = {
            let mut found_bucket = None;
            for b in &self.buckets {
                let bucket_guard = b.lock().await;
                if bucket_guard.policy.config() == policy.config() {
                    found_bucket = Some(b.clone());
                    break;
                }
            }
            found_bucket.expect("Bucket for policy should exist")
        };

        loop {
            let mut b = bucket.lock().await;
            if b.tokens >= 1.0 {
                b.tokens -= 1.0;
                return;
            } else {
                let (sender, receiver) = oneshot::channel();
                b.waiters.push_back(sender);
                drop(b);
                let _ = receiver.await;
            }
        }
    }

    async fn refill_all(&self) {
        for bucket in &self.buckets {
            let mut b = bucket.lock().await;
            b.refill().await;
        }
    }
}

//
// Axum middleware and server
//

// Map a URL (path & method) to a policy.
fn url_to_policy(path: &str, method: &str) -> RateLimitPolicy {
    match (path, method) {
        ("/claims", "POST") => RateLimitPolicy::ClaimsWrite,
        ("/claims", "GET") => RateLimitPolicy::ClaimsRead,
        _ => RateLimitPolicy::EntriesReadUserAntiscan(UserCategory::PJ),
    }
}

// This middleware calls `acquire` on the appropriate bucket.
// It loops until the token is acquired.
#[axum::debug_middleware]
async fn rate_limiter_middleware(
    State(state): State<Arc<LeakyBucket>>,
    req: Request,
    next: Next,
) -> Response {
    let path = req.uri().path();
    let method = req.method().as_str();
    let policy = url_to_policy(path, method);

    state.acquire(policy).await;
    next.run(req).await
}

// A simple handler.
async fn hello_handler() -> &'static str {
    "Hello, World!"
}

#[tokio::main]
async fn main() {
    let bucket = Arc::new(LeakyBucket::new());

    // Spawn a background task to periodically refill all buckets.
    let bucket_clone = bucket.clone();
    tokio::spawn(async move {
        loop {
            bucket_clone.refill_all().await;
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });

    let app = Router::new()
        .route("/", get(hello_handler))
        .route("/claims", get(hello_handler))
        .layer(from_fn_with_state(bucket.clone(), rate_limiter_middleware))
        // Optionally attach state to handlers.
        .with_state(bucket);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();

    axum::serve(listener, app.into_make_service())
        .await
        .unwrap();
}
