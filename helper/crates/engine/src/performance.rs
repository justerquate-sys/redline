use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::Instant};

pub struct Timings {
    start: Instant,
    started_at: String,
    finished_at: Option<String>,
    outcome: String,
    elapsed: Option<f64>,
    queue: Option<f64>,
    stages: BTreeMap<String, f64>,
    pub requests: Vec<Value>,
}
impl Timings {
    pub fn new(created: Option<&str>) -> Self {
        Self {
            start: Instant::now(),
            started_at: scribe_common::now(),
            finished_at: None,
            outcome: "running".into(),
            elapsed: None,
            queue: created
                .and_then(|value| DateTime::parse_from_rfc3339(value).ok())
                .map(|created| {
                    ((Utc::now() - created.with_timezone(&Utc)).num_milliseconds() as f64 / 1000.)
                        .max(0.)
                }),
            stages: BTreeMap::new(),
            requests: vec![],
        }
    }
    pub fn add(&mut self, stage: &str, seconds: f64) {
        *self.stages.entry(stage.to_owned()).or_default() += seconds;
    }
    pub fn finish(&mut self, outcome: &str) {
        self.elapsed = Some(self.start.elapsed().as_secs_f64());
        self.finished_at = Some(scribe_common::now());
        self.outcome = outcome.into();
    }
    pub fn snapshot(&self) -> Value {
        json!({"schema_version":1,"started_at":self.started_at,"finished_at":self.finished_at,"outcome":self.outcome,"queue_seconds":self.queue,"processing_seconds":self.elapsed.unwrap_or_else(||self.start.elapsed().as_secs_f64()),"stages_seconds":self.stages,"overlapping_stages":["chunk_prepare","speech"],"speech_requests":self.requests})
    }
}
