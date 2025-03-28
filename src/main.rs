use axum::{
    extract::{Request, State},
    middleware::{from_fn_with_state, Next},
    response::Response,
    routing::get,
    Router,
};
use tokio::sync::Mutex;

use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use strum::IntoEnumIterator;
use strum_macros::EnumIter;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub enum UserCategory {
    PF,
    #[default]
    PJ,
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub enum ParticipantCategory {
    #[default]
    A,
    B,
    C,
    D,
    E,
    F,
    G,
    H,
}

#[derive(Debug, Clone, Copy, EnumIter, PartialEq)]
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

#[derive(Debug)]
struct Bucket {
    tokens: f64,
    last_updated: Instant,
    policy: RateLimitPolicy,
}

impl Bucket {
    fn new(policy: RateLimitPolicy) -> Self {
        Self {
            tokens: policy.config().1,
            last_updated: Instant::now(),
            policy,
        }
    }
}

#[derive(Debug)]
struct LeakyBucket {
    last_updated: Instant,
    buckets: Vec<Bucket>,
}

impl LeakyBucket {
    fn new() -> Self {
        let buckets = RateLimitPolicy::iter().map(|p| Bucket::new(p)).collect();

        Self {
            last_updated: Instant::now(),
            buckets,
        }
    }

    pub async fn main_task(&mut self) {
        self.replenish_buckets();
    }

    pub fn replenish_buckets(&mut self) {
        let buckets = &mut self.buckets;

        buckets.iter_mut().for_each(|b| {
            let (leak_rate, capacity) = b.policy.config();
            let now = Instant::now();
            let elapsed = now.duration_since(b.last_updated).as_secs_f64();
            b.last_updated = now;

            b.tokens = (b.tokens + elapsed * leak_rate).min(capacity);
        });
    }

    async fn acquire(&mut self, policy: RateLimitPolicy) -> Duration {
        let bucket = self
            .buckets
            .iter_mut()
            .find(|p| p.policy.config() == policy.config())
            .unwrap();

        let (rate, capacity) = bucket.policy.config();

        if capacity / bucket.tokens <= 0.75 {
            let delay = Duration::from_secs_f64(rate * 0.75);
            tokio::time::sleep(delay).await;
        }

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Duration::from_secs(0)
        } else {
            let missing = 1.0 - bucket.tokens;
            let wait_time = missing / bucket.policy.config().0;
            Duration::from_secs_f64(wait_time)
        }
    }
}

fn url_to_policy(path: &str, method: &str) -> RateLimitPolicy {
    match (path, method) {
        ("/claims", "POST") => RateLimitPolicy::ClaimsWrite,
        ("/claims", "GET") => RateLimitPolicy::ClaimsRead,
        _ => RateLimitPolicy::EntriesReadUserAntiscan(UserCategory::PJ),
    }
}

#[axum::debug_middleware]
async fn rate_limiter_middleware(
    State(state): State<Arc<Mutex<LeakyBucket>>>,
    req: Request,
    next: Next,
) -> Response {
    let path = req.uri().path();
    let method = req.method().to_string();
    let policy = url_to_policy(path, &method);

    let now = Instant::now();

    loop {
        let wait_time = {
            let mut bucket_guard = state.lock().await;
            bucket_guard.acquire(policy).await
        };

        if wait_time.is_zero() {
            break;
        } else {
            tokio::time::sleep(wait_time).await;
        }
    }

    next.run(req).await
}

async fn hello_handler() -> &'static str {
    "Hello, World!"
}

#[tokio::main]
async fn main() {
    let bucket = Arc::new(Mutex::new(LeakyBucket::new()));

    let bucket_clone = bucket.clone();
    tokio::spawn(async move {
        loop {
            {
                bucket_clone.lock().await.main_task().await;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });

    let app = Router::new()
        .route("/", get(hello_handler))
        .route("/claims", get(hello_handler))
        .layer(from_fn_with_state(bucket.clone(), rate_limiter_middleware))
        .with_state(bucket);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();

    axum::serve(listener, app.into_make_service())
        .await
        .unwrap();
}
