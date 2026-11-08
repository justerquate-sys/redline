//! Instrument delivery to the HTTP transport, never remote acknowledgement.
//!
//! Reqwest's multipart encoder prepares immutable byte segments once. Each
//! attempt borrows cheap slices of those same segments, so backpressure can
//! limit polling to64KiB pieces without copying the complete audio or changing
//! multipart boundaries between retries.
use bytes::Bytes;
use futures_util::{Stream, StreamExt};
use parking_lot::Mutex;
use scribe_common::{Error, Result};
use serde_json::{Value, json};
use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Instant,
};

const DELIVERY_CHUNK_BYTES: usize = 64 * 1024;
pub(crate) const TIMING_NOTE: &str = "Body stream timings measure multipart bytes yielded to the HTTP transport under polling/backpressure, not socket flush, server acknowledgement, or Azure processing time. Complete means all prepared bytes were delivered. EOF time is null if the transport stops polling at Content-Length; duration then ends at the last byte.";

pub(crate) struct PreparedMultipart {
    content_type: String,
    segments: Arc<[Bytes]>,
    length: u64,
}
impl PreparedMultipart {
    pub(crate) async fn new(
        audio: Vec<u8>,
        filename: &'static str,
        definition: Value,
    ) -> Result<Self> {
        let part = reqwest::multipart::Part::bytes(audio)
            .file_name(filename)
            .mime_str("audio/wav")
            .map_err(|_| Error::internal("Could not prepare temporary audio."))?;
        let form = reqwest::multipart::Form::new()
            .part("audio", part)
            .text("definition", definition.to_string());
        let content_type = format!("multipart/form-data; boundary={}", form.boundary());
        let mut stream = Box::pin(form.into_stream());
        let mut segments = Vec::new();
        let mut length = 0u64;
        while let Some(segment) = stream.next().await {
            let segment =
                segment.map_err(|_| Error::internal("Could not prepare temporary audio."))?;
            length = length
                .checked_add(segment.len() as u64)
                .ok_or_else(|| Error::bad_request("The temporary audio request is too large."))?;
            segments.push(segment);
        }
        Ok(Self {
            content_type,
            segments: segments.into(),
            length,
        })
    }
    pub(crate) fn content_type(&self) -> &str {
        &self.content_type
    }
    pub(crate) fn length(&self) -> u64 {
        self.length
    }
    pub(crate) fn attempt(&self) -> (reqwest::Body, DeliveryObserver) {
        let (stream, observer) = self.stream();
        (reqwest::Body::wrap_stream(stream), observer)
    }
    fn stream(&self) -> (DeliveryStream, DeliveryObserver) {
        let observer = DeliveryObserver {
            state: Arc::new(Mutex::new(DeliveryState::default())),
            expected: self.length,
        };
        (
            DeliveryStream {
                segments: self.segments.clone(),
                segment: 0,
                offset: 0,
                observer: observer.clone(),
            },
            observer,
        )
    }
}

#[derive(Default)]
struct DeliveryState {
    first_poll: Option<Instant>,
    last_byte: Option<Instant>,
    eof: Option<Instant>,
    bytes: u64,
}
#[derive(Clone)]
pub(crate) struct DeliveryObserver {
    state: Arc<Mutex<DeliveryState>>,
    expected: u64,
}
impl DeliveryObserver {
    pub(crate) fn snapshot(&self, request_start: Instant) -> Value {
        let state = self.state.lock();
        let relative =
            |value: Option<Instant>| value.map(|v| v.duration_since(request_start).as_secs_f64());
        let duration = state
            .first_poll
            .zip(state.eof.or(state.last_byte))
            .map(|(first, last)| last.duration_since(first).as_secs_f64());
        json!({
            "body_stream_first_poll_seconds":relative(state.first_poll),
            "body_stream_last_byte_seconds":relative(state.last_byte),
            "body_stream_eof_seconds":relative(state.eof),
            "body_stream_seconds":duration,
            "body_stream_bytes":state.bytes,
            "body_stream_expected_bytes":self.expected,
            "body_stream_complete":state.bytes==self.expected,
            "body_stream_eof_polled":state.eof.is_some(),
            "timing_note":TIMING_NOTE,
        })
    }
}
struct DeliveryStream {
    segments: Arc<[Bytes]>,
    segment: usize,
    offset: usize,
    observer: DeliveryObserver,
}
impl Stream for DeliveryStream {
    type Item = std::result::Result<Bytes, std::io::Error>;
    fn poll_next(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let instant = Instant::now();
        let mut state = this.observer.state.lock();
        state.first_poll.get_or_insert(instant);
        while let Some(segment) = this.segments.get(this.segment) {
            if this.offset == segment.len() {
                this.segment += 1;
                this.offset = 0;
                continue;
            }
            let end = segment.len().min(this.offset + DELIVERY_CHUNK_BYTES);
            let bytes = segment.slice(this.offset..end);
            this.offset = end;
            state.bytes += bytes.len() as u64;
            state.last_byte = Some(instant);
            return Poll::Ready(Some(Ok(bytes)));
        }
        state.eof.get_or_insert(instant);
        Poll::Ready(None)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn multipart_replay_is_byte_identical_bounded_and_each_attempt_has_its_own_clock() {
        let audio = vec![117; 3 * DELIVERY_CHUNK_BYTES + 7];
        let prepared = PreparedMultipart::new(audio.clone(), "audio.wav", json!({"test":true}))
            .await
            .unwrap();
        let (mut first, one) = prepared.stream();
        let start = Instant::now();
        assert!(one.snapshot(start)["body_stream_first_poll_seconds"].is_null());
        let mut collected = vec![];
        while let Some(bytes) = first.next().await {
            let bytes = bytes.unwrap();
            assert!(bytes.len() <= DELIVERY_CHUNK_BYTES);
            collected.extend_from_slice(&bytes);
        }
        let metrics = one.snapshot(start);
        assert_eq!(metrics["body_stream_complete"], true);
        assert_eq!(metrics["body_stream_bytes"], prepared.length());
        assert!(
            metrics["body_stream_eof_seconds"].as_f64().unwrap()
                >= metrics["body_stream_last_byte_seconds"].as_f64().unwrap()
        );
        assert!(collected.windows(audio.len()).any(|bytes| bytes == audio));
        let (mut second, two) = prepared.stream();
        assert_eq!(two.snapshot(Instant::now())["body_stream_bytes"], 0);
        let mut replay = vec![];
        while let Some(bytes) = second.next().await {
            replay.extend_from_slice(&bytes.unwrap());
        }
        assert_eq!(replay, collected);
        assert_eq!(one.snapshot(start), metrics);
    }
    #[tokio::test]
    async fn dropped_partial_stream_is_never_claimed_complete() {
        let prepared =
            PreparedMultipart::new(vec![1; 2 * DELIVERY_CHUNK_BYTES], "audio.wav", json!({}))
                .await
                .unwrap();
        let start = Instant::now();
        let (mut stream, observer) = prepared.stream();
        stream.next().await.unwrap().unwrap();
        drop(stream);
        let metrics = observer.snapshot(start);
        assert_eq!(metrics["body_stream_complete"], false);
        assert!(metrics["body_stream_eof_seconds"].is_null());
        assert!(metrics["body_stream_bytes"].as_u64().unwrap() > 0);
        assert!(metrics["body_stream_bytes"].as_u64().unwrap() < prepared.length());
    }

    #[tokio::test]
    async fn complete_delivery_and_explicit_eof_poll_are_distinct_observations() {
        let prepared = PreparedMultipart::new(vec![1; 64], "audio.wav", json!({}))
            .await
            .unwrap();
        let start = Instant::now();
        let (mut stream, observer) = prepared.stream();
        while observer.snapshot(start)["body_stream_bytes"] != prepared.length() {
            stream.next().await.unwrap().unwrap();
        }
        let delivered = observer.snapshot(start);
        assert_eq!(delivered["body_stream_complete"], true);
        assert_eq!(delivered["body_stream_eof_polled"], false);
        assert!(delivered["body_stream_eof_seconds"].is_null());
        assert!(stream.next().await.is_none());
        let eof = observer.snapshot(start);
        assert_eq!(eof["body_stream_eof_polled"], true);
        assert_eq!(eof["body_stream_bytes"], delivered["body_stream_bytes"]);
    }
}
