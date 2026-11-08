use crate::{
    performance::Timings,
    process::{self, cancelled, run_process},
};
use scribe_common::{
    API_VERSION, Error, MODEL, Result, Settings, atomic_json, atomic_write, build_document,
    create_transient_json, extract_description_links, publication_date, read_bytes_optional,
    read_json_optional, render_document, write_exports,
};
use serde_json::{Value, json};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

pub const SAMPLE_RATE: u64 = 16000;
pub const SAMPLE_WIDTH: u64 = 2;
pub type Update = Arc<dyn Fn(Value) -> Result<()> + Send + Sync>;
pub type RunFuture = futures_util::future::BoxFuture<'static, Result<()>>;
pub trait PipelineRunner: Send + Sync {
    fn run(
        &self,
        settings: Settings,
        job: Value,
        update: Update,
        result_dir: PathBuf,
        work_root: PathBuf,
        cancel: CancellationToken,
    ) -> RunFuture;
}
#[derive(Default)]
pub struct Pipeline;
impl PipelineRunner for Pipeline {
    fn run(
        &self,
        settings: Settings,
        job: Value,
        update: Update,
        result_dir: PathBuf,
        work_root: PathBuf,
        cancel: CancellationToken,
    ) -> RunFuture {
        Box::pin(run(settings, job, update, result_dir, work_root, cancel))
    }
}

fn checkpoint_chunk_end(chunk: &Value, index: usize, offset: f64) -> Option<f64> {
    let start = chunk["offset_ms"]
        .as_f64()
        .filter(|v| v.is_finite() && *v >= 0.)?;
    let duration = chunk["duration_ms"]
        .as_f64()
        .filter(|v| v.is_finite() && *v > 0.)?;
    if chunk["index"].as_u64() != Some(index as u64 + 1)
        || !chunk["text"].is_string()
        || (start - offset).abs() > 0.001
    {
        return None;
    }
    let end = offset + duration;
    end.is_finite().then_some(end)
}

fn checkpoint_text(chunks: &[Value]) -> String {
    chunks
        .iter()
        .filter_map(|chunk| chunk["text"].as_str())
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Verify a final JSON-last commit whose final job-state update may have been
/// interrupted. This is deliberately read-only and never infers completion
/// from a partial document or from a merely successful preview restoration.
pub fn completed_checkpoint(job: &Value, result_dir: &Path) -> Result<bool> {
    let Some(document) = read_json_optional(&result_dir.join("transcript.json"))? else {
        return Ok(false);
    };
    let Some(total) = job["total_chunks"].as_u64().filter(|value| *value > 0) else {
        return Ok(false);
    };
    let Some(chunks) = document["chunks"].as_array() else {
        return Ok(false);
    };
    if document["complete"] != true
        || job["video_id"].as_str().is_none_or(str::is_empty)
        || job["url"].as_str().is_none_or(str::is_empty)
        || document["video_id"] != job["video_id"]
        || document["source_url"] != job["url"]
        || document["completed_chunks"].as_u64() != Some(total)
        || document["total_chunks"].as_u64() != Some(total)
        || chunks.len() as u64 != total
    {
        return Ok(false);
    }
    let mut end = 0.;
    for (index, chunk) in chunks.iter().enumerate() {
        let Some(next) = checkpoint_chunk_end(chunk, index, end) else {
            return Ok(false);
        };
        end = next;
    }
    if document["full_text"].as_str() != Some(checkpoint_text(chunks).as_str()) {
        return Ok(false);
    }
    let positive = |value: &Value| value.as_f64().filter(|v| v.is_finite() && *v > 0.);
    let (Some(job_audio), Some(document_audio)) = (
        positive(&job["metadata"]["audio_duration_seconds"]),
        positive(&document["audio_duration_seconds"]),
    ) else {
        return Ok(false);
    };
    if (end - job_audio * 1000.).abs() > 1.
        || (end - document_audio * 1000.).abs() > 1.
        || (job_audio - document_audio).abs() > 0.001
    {
        return Ok(false);
    }
    let source = &job["metadata"]["duration_seconds"];
    let recorded_source = &document["video_duration_seconds"];
    if source.is_null() && recorded_source.is_null() {
        // yt-dlp can omit source duration; the pipeline already checks ffprobe
        // before it creates a final commit. Decoded coverage is still required.
        return Ok(true);
    }
    let (Some(source), Some(recorded_source)) = (positive(source), positive(recorded_source))
    else {
        return Ok(false);
    };
    Ok((source - recorded_source).abs() <= 0.001 && job_audio + 3. >= source)
}

/// Restore private, derived previews after an interruption. JSON checkpoints
/// remain the source of truth; this never authorizes more audio processing or
/// changes a partial document into a complete one.
pub fn restore_partial_exports(job: &Value, result_dir: &Path) -> Result<()> {
    let checkpoint = read_json_optional(&result_dir.join("transcript.json"))?;
    let invalid = || {
        Error::bad_request(
            "The saved partial transcript could not be verified. Its original checkpoints were retained.",
        )
    };
    let mut document = if let Some(value) = checkpoint.as_ref() {
        if !value.is_object()
            || value["video_id"] != job["video_id"]
            || !value["complete"].is_boolean()
            || !value["full_text"].is_string()
            || !value["chunks"].is_array()
        {
            return Err(invalid());
        }
        // A completed commit is authoritative, including its exact existing
        // export bytes. Recovery must never demote or regenerate it.
        if value["complete"] == true {
            return Ok(());
        }
        value.clone()
    } else {
        build_document(job, &[], false)
    };
    let mut chunks = document["chunks"].as_array().ok_or_else(invalid)?.clone();
    let validate = |chunk: &Value, index: usize, offset: f64| {
        checkpoint_chunk_end(chunk, index, offset).ok_or_else(invalid)
    };
    let mut offset = 0.;
    for (index, chunk) in chunks.iter().enumerate() {
        offset = validate(chunk, index, offset)?;
    }
    if document["full_text"].as_str() != Some(checkpoint_text(&chunks).as_str()) {
        return Err(invalid());
    }
    // Read only sequential named checkpoints through common's no-follow IO.
    // A missing index ends the recoverable contiguous prefix. Later files stay
    // intact for inspection; no gaps or mismatched checkpoints are invented.
    let mut index = 0;
    let mut journal_offset = 0.;
    while let Some(chunk) = read_json_optional(
        &result_dir
            .join("chunks")
            .join(format!("{:06}.json", index + 1)),
    )? {
        journal_offset = validate(&chunk, index, journal_offset)?;
        if let Some(saved) = chunks.get(index) {
            if saved != &chunk {
                return Err(invalid());
            }
        } else {
            chunks.push(chunk);
        }
        index += 1;
    }
    if chunks.is_empty() {
        return Ok(());
    }
    document["full_text"] = json!(checkpoint_text(&chunks));
    document["completed_chunks"] = json!(chunks.len());
    document["total_chunks"] = json!(
        document["total_chunks"]
            .as_u64()
            .unwrap_or(0)
            .max(chunks.len() as u64)
    );
    document["chunks"] = json!(chunks);
    if checkpoint.as_ref() != Some(&document) {
        atomic_json(&result_dir.join("transcript.json"), &document)?;
    }
    for format in ["md", "txt"] {
        let content = render_document(&document, format)?;
        let path = result_dir.join(format!("transcript.{format}"));
        if read_bytes_optional(&path)?.as_deref() != Some(content.as_bytes()) {
            atomic_write(&path, content.as_bytes())?;
        }
    }
    Ok(())
}
fn apply(job: &mut Value, update: &Update, patch: Value) -> Result<()> {
    update(patch.clone())?;
    if let (Some(job), Some(patch)) = (job.as_object_mut(), patch.as_object()) {
        for (k, v) in patch {
            job.insert(k.clone(), v.clone());
        }
    }
    Ok(())
}
pub fn azure_definition() -> Value {
    json!({"enhancedMode":{"enabled":true,"model":MODEL,"modelOptions":{"timestamps":"segment","transcribeStyle":"verbatim"}},"diarization":{"enabled":false},"profanityFilterMode":"None"})
}
pub fn normalize_response(
    response: &Value,
    offset_ms: f64,
    duration_ms: f64,
    index: usize,
    speed: f64,
) -> Result<Value> {
    let invalid = || {
        Error::bad_request(
            "Azure returned an unexpected transcript format; no complete document was marked ready.",
        )
    };
    let phrases = response
        .get("combinedPhrases")
        .and_then(Value::as_array)
        .ok_or_else(invalid)?;
    let reported=response.get("durationMilliseconds").and_then(Value::as_f64).filter(|v|v.is_finite()&&*v>=0.).ok_or_else(||Error::bad_request("Azure did not report a valid processed audio duration; completeness could not be verified."))?;
    if reported + (duration_ms * 0.01).min(1000.) < duration_ms {
        return Err(Error::bad_request(
            "Azure reported processing less than the complete audio chunk. Earlier completed chunks are retained; the job is not complete.",
        ));
    }
    let texts: Vec<&str> = phrases
        .iter()
        .map(|entry| {
            entry
                .get("text")
                .and_then(Value::as_str)
                .ok_or_else(invalid)
        })
        .collect::<Result<_>>()?;
    let empty = vec![];
    let segments_in = match response.get("phrases") {
        None => &empty,
        Some(v) => v.as_array().ok_or_else(invalid)?,
    };
    let mut segments = vec![];
    for phrase in segments_in {
        let text = phrase
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(invalid)?;
        let mut segment = json!({"text":text});
        for (source, target) in [
            ("offsetMilliseconds", "offset_ms"),
            ("durationMilliseconds", "duration_ms"),
        ] {
            if let Some(value) = phrase
                .get(source)
                .and_then(Value::as_f64)
                .filter(|v| v.is_finite())
            {
                segment[target] = json!(
                    (value
                        + if source == "offsetMilliseconds" {
                            offset_ms
                        } else {
                            0.
                        })
                        * speed
                );
            }
        }
        if let Some(locale) = phrase.get("locale").filter(|v| !v.is_null()) {
            segment["locale"] = locale.clone();
        }
        segments.push(segment);
    }
    if texts.join("").trim().is_empty()
        && segments
            .iter()
            .any(|v| !v["text"].as_str().unwrap_or("").trim().is_empty())
    {
        return Err(invalid());
    }
    Ok(
        json!({"index":index,"offset_ms":offset_ms*speed,"duration_ms":duration_ms*speed,"azure_duration_ms":reported,"text":texts.join("\n"),"text_source":"Azure combinedPhrases","segments":segments}),
    )
}
pub fn probe_duration(probe: &Value) -> Result<f64> {
    // ffprobe can omit stream duration for containers that only expose a format duration.
    let candidates = [
        probe.pointer("/streams/0/duration"),
        probe.pointer("/format/duration"),
    ];
    for candidate in candidates.into_iter().flatten() {
        if candidate.is_null()
            || candidate
                .as_str()
                .is_some_and(|s| s.is_empty() || s == "N/A")
        {
            continue;
        }
        let parsed = candidate
            .as_f64()
            .or_else(|| candidate.as_str().and_then(|s| s.parse::<f64>().ok()));
        return parsed
            .filter(|v| v.is_finite() && *v >= 0.)
            .ok_or_else(|| Error::bad_request("The source audio duration could not be verified."));
    }
    Err(Error::bad_request(
        "The source audio duration was unavailable; completeness could not be verified.",
    ))
}
pub fn chunk_layout(frames: u64, maximum: u64) -> Result<Vec<(u64, u64)>> {
    if maximum == 0 || maximum > 900 * SAMPLE_RATE {
        return Err(Error::bad_request("Choose a valid audio chunk length."));
    }
    let mut layout = vec![];
    let mut offset = 0;
    while offset < frames {
        let mut count = maximum.min(frames - offset);
        let tail = frames - offset - count;
        if tail > 0 && tail <= SAMPLE_RATE.min(maximum / 500) {
            count = if count + tail <= 900 * SAMPLE_RATE {
                count + tail
            } else {
                (count + tail) / 2
            };
        }
        layout.push((offset, count));
        offset += count;
    }
    Ok(layout)
}
pub fn wav_header(frames: u64) -> Result<Vec<u8>> {
    let bytes = u32::try_from(frames * SAMPLE_WIDTH)
        .map_err(|_| Error::bad_request("The audio chunk is too large."))?;
    let mut data = vec![];
    data.extend(b"RIFF");
    data.extend((bytes + 36).to_le_bytes());
    data.extend(b"WAVEfmt ");
    data.extend(16u32.to_le_bytes());
    data.extend(1u16.to_le_bytes());
    data.extend(1u16.to_le_bytes());
    data.extend((SAMPLE_RATE as u32).to_le_bytes());
    data.extend((SAMPLE_RATE * SAMPLE_WIDTH).to_le_bytes()[..4].iter());
    data.extend(2u16.to_le_bytes());
    data.extend(16u16.to_le_bytes());
    data.extend(b"data");
    data.extend(bytes.to_le_bytes());
    Ok(data)
}
pub fn prepare_wav_chunk(
    pcm: &Path,
    audio: &Path,
    offset: u64,
    count: u64,
) -> Result<(PathBuf, f64)> {
    let start = Instant::now();
    let mut source = File::open(pcm)?;
    source.seek(SeekFrom::Start(offset * SAMPLE_WIDTH))?;
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut target = options.open(audio)?;
    target.write_all(&wav_header(count)?)?;
    let copied = std::io::copy(&mut source.take(count * SAMPLE_WIDTH), &mut target)?;
    if copied != count * SAMPLE_WIDTH {
        return Err(Error::bad_request(
            "The temporary audio ended unexpectedly; the transcript is incomplete.",
        ));
    }
    target.flush()?;
    Ok((audio.to_owned(), start.elapsed().as_secs_f64()))
}
fn speech_client(probe: bool) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(if probe { 10 } else { 30 }))
        .read_timeout(Duration::from_secs(if probe { 25 } else { 900 }))
        .timeout(Duration::from_secs(if probe { 35 } else { 1050 }))
        .pool_max_idle_per_host(1)
        .pool_idle_timeout(Duration::from_secs(60))
        .build()
        .map_err(|_| Error::internal("The Azure connection could not be prepared."))
}
async fn request_speech(
    client: &reqwest::Client,
    settings: &Settings,
    payload: Vec<u8>,
    cancel: &CancellationToken,
    probe: bool,
) -> (Result<Value>, Value) {
    let overall = Instant::now();
    let audio_bytes = payload.len();
    let mut attempts = vec![];
    let endpoint = format!(
        "{}/speechtotext/transcriptions:transcribe?api-version={API_VERSION}",
        settings.speech_endpoint.trim_end_matches('/')
    );
    let result = async {
        let prepared = crate::transfer::PreparedMultipart::new(
            payload,
            if probe { "connection-test.wav" } else { "audio.wav" },
            azure_definition(),
        ).await?;
        for attempt in 0..if probe { 1 } else { 4 } {
            let (body, observer) = prepared.attempt();
            let start = Instant::now();
            let response = tokio::select! {
                biased;
                _ = cancel.cancelled() => Err(cancelled()),
                result = client.post(&endpoint)
                    .header("Ocp-Apim-Subscription-Key", &settings.speech_key)
                    .header(reqwest::header::CONTENT_TYPE, prepared.content_type())
                    .header(reqwest::header::CONTENT_LENGTH, prepared.length())
                    .body(body).send() => result.map_err(|error| {
                        if error.is_timeout() {
                            Error::bad_request("Azure transcription timed out. Completed transcript chunks are retained.")
                        } else {
                            Error::bad_request("Could not reach Azure Speech. Check your internet connection and resource endpoint.")
                        }
                    }),
            };
            let elapsed = start.elapsed().as_secs_f64();
            let status = response.as_ref().ok().map(|r| r.status().as_u16());
            let mut metrics = observer.snapshot(start);
            if let Some(fields) = metrics.as_object_mut() {
                fields.extend(json!({
                    "request_seconds": elapsed,
                    "response_headers_seconds": response.as_ref().ok().map(|_|elapsed),
                    "upload_seconds": Value::Null,
                    "response_wait_seconds": Value::Null,
                    "status_code": status,
                }).as_object().into_iter().flatten().map(|(key,value)|(key.clone(),value.clone())));
            }
            // Record even cancelled/network-failed attempts. A stream that did
            // not deliver all prepared bytes must not be reported complete.
            attempts.push(metrics);
            let response = response?;
            let status = response.status().as_u16();
            if status == 200 {
                return tokio::select! {
                    biased;
                    _ = cancel.cancelled() => Err(cancelled()),
                    result = response.json::<Value>() => result.map_err(|_| Error::bad_request("Azure returned an unreadable response.")),
                };
            }
            if [401, 403].contains(&status) {
                return Err(Error::bad_request("Azure rejected the Speech key or resource access. Check the key belongs to this resource and has access to MAI-Transcribe-2."));
            }
            if [429, 500, 502, 503, 504].contains(&status) && attempt < 3 && !probe {
                let delay = response.headers().get("retry-after")
                    .and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<f64>().ok())
                    .filter(|v| v.is_finite()).unwrap_or(2f64.powi(attempt + 1)).clamp(1., 30.);
                // Drain only small error bodies so a reusable connection is
                // retained without trusting or exposing arbitrary error text.
                let mut response = response;
                let mut drained = 0usize;
                loop {
                    let chunk = tokio::select! {
                        biased;
                        _ = cancel.cancelled() => return Err(cancelled()),
                        chunk = response.chunk() => chunk,
                    };
                    match chunk {
                        Ok(Some(bytes)) if drained + bytes.len() <= 64 * 1024 => drained += bytes.len(),
                        _ => break,
                    }
                }
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => return Err(cancelled()),
                    _ = tokio::time::sleep(Duration::from_secs_f64(delay)) => {},
                }
                continue;
            }
            if status == 429 {
                return Err(Error::bad_request("Azure Speech is rate-limited or out of quota. Partial text is retained; try again later."));
            }
            return Err(Error::bad_request(format!("Azure rejected this MAI-Transcribe-2 request (HTTP {status}). Check the Speech resource, region and model access.")));
        }
        Err(Error::bad_request("Azure transcription failed after retries."))
    }.await;
    (
        result,
        json!({"audio_bytes":audio_bytes,"attempts":attempts,"total_seconds":overall.elapsed().as_secs_f64()}),
    )
}
pub async fn test_connection(settings: &Settings) -> Result<()> {
    let frames = SAMPLE_RATE * 3 / 4;
    let mut wav = wav_header(frames)?;
    wav.resize(wav.len() + (frames * SAMPLE_WIDTH) as usize, 0);
    let (response, _) = request_speech(
        &speech_client(true)?,
        settings,
        wav,
        &CancellationToken::new(),
        true,
    )
    .await;
    let response = response?;
    normalize_response(&response, 0., 750., 1, 1.)?;
    Ok(())
}
pub async fn run(
    settings: Settings,
    mut job: Value,
    update: Update,
    result_dir: PathBuf,
    work_root: PathBuf,
    cancel: CancellationToken,
) -> Result<()> {
    let mut timings = Timings::new(job["created_at"].as_str());
    apply(&mut job, &update, json!({"performance":timings.snapshot()}))?;
    let result = run_inner(
        &settings,
        &mut job,
        &update,
        &result_dir,
        &work_root,
        &cancel,
        &mut timings,
    )
    .await;
    if result.is_err() {
        let started = Instant::now();
        // Preserve the original processing/cancellation error if storage is
        // unavailable. Durable JSON/chunk journals remain recoverable later.
        let _ = restore_partial_exports(&job, &result_dir);
        timings.add("write", started.elapsed().as_secs_f64());
    }
    timings.finish(if result.is_ok() {
        "completed"
    } else if result.as_ref().err().is_some_and(|e| e.status == 499) {
        "cancelled"
    } else {
        "failed"
    });
    if result.is_ok() {
        apply(
            &mut job,
            &update,
            json!({"status":"completed","progress":100,"document_available":true,"error":Value::Null,"message":"Complete transcript ready. Temporary audio deleted.","performance":timings.snapshot()}),
        )?;
    } else {
        let _ = update(json!({"performance":timings.snapshot()}));
    }
    result
}
async fn run_inner(
    settings: &Settings,
    job: &mut Value,
    update: &Update,
    result_dir: &Path,
    work_root: &Path,
    cancel: &CancellationToken,
    timings: &mut Timings,
) -> Result<()> {
    std::fs::create_dir_all(work_root)?;
    let temporary = tempfile::Builder::new()
        .prefix(&format!("{}-", job["id"].as_str().unwrap_or("video")))
        .tempdir_in(work_root)?;
    let work = temporary.path();
    let mut pending: Option<tokio::task::JoinHandle<Result<(PathBuf, f64)>>> = None;
    let result=async {
        apply(job,update,json!({"status":"downloading","progress":3,"message":"Reading video details…"}))?;
        let started=Instant::now();let info=process::metadata(settings,job["url"].as_str().unwrap_or(""),work,cancel).await?;timings.add("metadata",started.elapsed().as_secs_f64());
        if info["is_live"].as_bool()==Some(true)||["is_live","is_upcoming","post_live"].contains(&info["live_status"].as_str().unwrap_or("")){return Err(Error::bad_request("This is a live, upcoming, or still-processing stream. Try again after the finished recording is available."));}
        if info["id"]!=job["video_id"] {return Err(Error::bad_request("YouTube returned a different video; download was stopped."));}
        let released=publication_date(&info["release_date"]);
        let mut metadata=json!({"channel":info["channel"].as_str().or(info["uploader"].as_str()).unwrap_or(""),"published_date":if released.is_empty(){publication_date(&info["upload_date"])}else{released},"duration_seconds":info["duration"],"model":MODEL});
        let links=extract_description_links(&info["description"]);if !links.is_empty(){metadata["description_links"]=json!(links);}
        apply(job,update,json!({"title":info["title"].as_str().or(job["video_id"].as_str()).unwrap_or("YouTube transcript"),"metadata":metadata,"message":"Downloading temporary audio…","progress":7}))?;
        let info_path=work.join("source-info.json");create_transient_json(&info_path,&info)?;
        let mut args=process::ytdlp_arguments(settings)?;
        args.extend(["--format".into(),"bestaudio".into(),"--no-simulate".into(),"--no-mtime".into(),"--output".into(),work.join("source.%(ext)s").to_string_lossy().into_owned(),"--print".into(),"after_move:filepath".into(),"--load-info-json".into(),info_path.to_string_lossy().into_owned()]);
        let started=Instant::now();let output=run_process(&args,work,"Could not download the video's audio.",Duration::from_secs(6*3600),cancel,false).await?;timings.add("download",started.elapsed().as_secs_f64());
        std::fs::remove_file(info_path)?;
        let source=String::from_utf8_lossy(&output).lines().rfind(|line|!line.trim().is_empty()).map(PathBuf::from).ok_or_else(||Error::bad_request("YouTube did not produce an audio file."))?.canonicalize()?;
        if source.parent()!=Some(work.canonicalize()?.as_path())||!source.is_file(){return Err(Error::bad_request("The downloaded audio path was invalid."));}
        apply(job,update,json!({"status":"preparing","progress":18,"message":"Preparing the entire audio track…"}))?;
        let ffmpeg=process::executable(settings,"ffmpeg")?;let ffprobe=process::executable(settings,"ffprobe")?;
        let mut args=vec![ffprobe.to_string_lossy().into_owned()];args.extend(process::words(&["-v","error","-select_streams","a:0","-show_entries","stream=duration:format=duration","-of","json"]));args.push(source.to_string_lossy().into_owned());
        let started=Instant::now();let probe:Value=serde_json::from_slice(&run_process(&args,work,"Could not inspect the downloaded audio.",Duration::from_secs(60),cancel,false).await?)?;timings.add("probe",started.elapsed().as_secs_f64());
        let source_duration=probe_duration(&probe)?;
        let speed=settings.audio_speed;if !speed.is_finite()||!(1.0..=2.0).contains(&speed){return Err(Error::bad_request("Choose an audio speed between1 and2."));}
        let pcm=work.join("decoded.pcm");let mut args=vec![ffmpeg.to_string_lossy().into_owned()];args.extend(process::words(&["-hide_banner","-loglevel","error","-nostdin","-xerror","-i"]));args.push(source.to_string_lossy().into_owned());args.extend(process::words(&["-map","0:a:0","-vn"]));
        if speed!=1. {args.extend(["-af".into(),format!("atempo={speed}")]);}
        args.extend(process::words(&["-ac","1","-ar","16000","-c:a","pcm_s16le","-f","s16le"]));args.push(pcm.to_string_lossy().into_owned());
        let started=Instant::now();run_process(&args,work,"Could not decode the complete audio track.",Duration::from_secs_f64((source_duration*0.25).max(300.)),cancel,false).await?;timings.add("decode",started.elapsed().as_secs_f64());
        std::fs::remove_file(source)?;let bytes=std::fs::metadata(&pcm)?.len();if bytes==0||bytes%SAMPLE_WIDTH!=0{return Err(Error::bad_request("The decoded audio is empty or incomplete."));}
        let frames=bytes/SAMPLE_WIDTH;let audio_duration=frames as f64/SAMPLE_RATE as f64*speed;
        if audio_duration+2.5<source_duration||metadata["duration_seconds"].as_f64().is_some_and(|v|v>audio_duration+3.){return Err(Error::bad_request("The downloaded audio is shorter than the complete video. No incomplete transcript will be marked complete."));}
        metadata["audio_duration_seconds"]=json!(audio_duration);if speed!=1.{metadata["audio_speed"]=json!(speed);}
        let layout=chunk_layout(frames,settings.chunk_seconds as u64*SAMPLE_RATE)?;let total=layout.len();
        apply(job,update,json!({"metadata":metadata,"total_chunks":total,"completed_chunks":0,"status":"transcribing","progress":25,"message":format!("Transcribing chunk1 of {total} with {MODEL}…")}))?;
        let prepare=|index:usize|{let pcm=pcm.clone();let audio=work.join(format!("chunk-{}.wav",index+1));let(offset,count)=layout[index];tokio::task::spawn_blocking(move||prepare_wav_chunk(&pcm,&audio,offset,count))};
        pending=Some(prepare(0));let client=speech_client(false)?;let mut chunks=vec![];let mut offset_frames=0;
        for (index,(offset,count)) in layout.iter().copied().enumerate(){
            if cancel.is_cancelled(){return Err(cancelled());}
            let (audio,seconds)=pending.take().ok_or_else(||Error::internal("The next audio chunk was not prepared."))?.await.map_err(|_|Error::internal("Could not prepare the next audio chunk."))??;timings.add("chunk_prepare",seconds);
            pending=if index+1<total{Some(prepare(index+1))}else{None};
            apply(job,update,json!({"message":format!("Transcribing chunk {} of {total} with {MODEL}…",index+1),"progress":25+70*index/total}))?;
            let audio_read=audio.clone();let payload=tokio::task::spawn_blocking(move||std::fs::read(audio_read)).await.map_err(|_|Error::internal("Could not read temporary audio."))??;
            let started=Instant::now();let (response,mut metrics)=request_speech(&client,settings,payload,cancel,false).await;timings.add("speech",started.elapsed().as_secs_f64());metrics["chunk"]=json!(index+1);timings.requests.push(metrics);
            let chunk=normalize_response(&response?,offset as f64/SAMPLE_RATE as f64*1000.,count as f64/SAMPLE_RATE as f64*1000.,index+1,speed)?;
            let started=Instant::now();atomic_json(&result_dir.join("chunks").join(format!("{:06}.json",index+1)),&chunk)?;chunks.push(chunk);
            atomic_json(&result_dir.join("transcript.json"),&build_document(job,&chunks,false))?;
            apply(job,update,json!({"completed_chunks":chunks.len(),"progress":25+70*chunks.len()/total,"performance":timings.snapshot()}))?;
            timings.add("write",started.elapsed().as_secs_f64());
            offset_frames=offset+count;std::fs::remove_file(audio)?;tokio::task::yield_now().await;
        }
        if offset_frames!=frames{return Err(Error::bad_request("Not all audio samples were processed; the transcript is incomplete."));}
        apply(job,update,json!({"message":"Writing the complete transcript…","progress":98}))?;
        let started=Instant::now();write_exports(result_dir,&build_document(job,&chunks,true))?;timings.add("write",started.elapsed().as_secs_f64());Ok(())
    }.await;
    // A blocking prefetch cannot be aborted. Join it before the tempdir dies.
    if let Some(task) = pending {
        let _ = task.await;
    }
    let start = Instant::now();
    let cleanup = temporary.close();
    timings.add("cleanup", start.elapsed().as_secs_f64());
    result?;
    cleanup?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn cancelled_before_first_poll_records_zero_delivered_bytes_without_a_request() {
        let temporary = tempfile::tempdir().unwrap();
        let settings = Settings {
            root: temporary.path().into(),
            data_dir: temporary.path().join("data"),
            speech_key: "offline-test-key".into(),
            speech_endpoint: "http://127.0.0.1:1".into(),
            helper_token: "offline-token".into(),
            ffmpeg_location: String::new(),
            cookies_browser: String::new(),
            js_runtime: String::new(),
            chunk_seconds: 600,
            audio_speed: 1.25,
        };
        let cancel = CancellationToken::new();
        cancel.cancel();
        let (result, metrics) = request_speech(
            &speech_client(false).unwrap(),
            &settings,
            vec![0; 1024],
            &cancel,
            false,
        )
        .await;
        assert_eq!(result.unwrap_err().status, 499);
        let attempts = metrics["attempts"].as_array().unwrap();
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0]["body_stream_bytes"], 0);
        assert_eq!(attempts[0]["body_stream_complete"], false);
        assert!(attempts[0]["body_stream_first_poll_seconds"].is_null());
        assert!(attempts[0]["response_headers_seconds"].is_null());
        assert!(!metrics.to_string().contains("offline-test-key"));
    }
    #[test]
    fn coverage_and_tail_layout() {
        for frames in [1, 16000, 9_600_001, 14_400_001, 30_000_003] {
            let chunks = chunk_layout(frames, 9_600_000).unwrap();
            assert_eq!(chunks.iter().map(|(_, n)| n).sum::<u64>(), frames);
            assert!(chunks.windows(2).all(|p| p[0].0 + p[0].1 == p[1].0));
        }
        assert!(chunk_layout(1, 0).is_err());
    }
    #[test]
    fn speed_maps_all_timestamps_back_to_original_video() {
        let response = json!({"durationMilliseconds":1000,"combinedPhrases":[{"text":"All words"}],"phrases":[{"text":"All words","offsetMilliseconds":100,"durationMilliseconds":200}]});
        let normalized = normalize_response(&response, 2000., 1000., 2, 1.25).unwrap();
        assert_eq!(normalized["offset_ms"], 2500.);
        assert_eq!(normalized["segments"][0]["offset_ms"], 2625.);
        assert_eq!(normalized["duration_ms"], 1250.);
        assert!(normalize_response(&response, 0., 2000., 1, 1.).is_err());
    }
    #[test]
    fn wav_chunk_retains_every_sample() {
        let temp = tempfile::tempdir().unwrap();
        let pcm = temp.path().join("all.pcm");
        let samples: Vec<u8> = (0..64000).map(|n| (n % 251) as u8).collect();
        std::fs::write(&pcm, &samples).unwrap();
        let mut found = vec![];
        for (i, (offset, count)) in chunk_layout(32000, 10000)
            .unwrap()
            .iter()
            .copied()
            .enumerate()
        {
            let (path, _) =
                prepare_wav_chunk(&pcm, &temp.path().join(format!("{i}.wav")), offset, count)
                    .unwrap();
            let bytes = std::fs::read(path).unwrap();
            assert_eq!(&bytes[..4], b"RIFF");
            found.extend_from_slice(&bytes[44..]);
        }
        assert_eq!(found, samples);
    }
}
