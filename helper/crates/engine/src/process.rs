//! Owned subprocess groups. Cancellation reaps writers before temporary audio is removed.
use futures_util::future::BoxFuture;
use scribe_common::{Error, Result, Settings};
use serde_json::Value;
#[cfg(unix)]
use std::os::unix::process::CommandExt;
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, BufReader},
    process::{Child, Command},
};
use tokio_util::sync::CancellationToken;

pub fn cancelled() -> Error {
    Error::new(499, "Cancelled. Temporary audio is removed.")
}
fn io_error(message: &str) -> Error {
    Error::internal(message)
}

struct ProcessGroup(u32);
impl ProcessGroup {
    fn signal(&self, hard: bool) {
        #[cfg(unix)]
        unsafe {
            libc::kill(
                -(self.0 as i32),
                if hard { libc::SIGKILL } else { libc::SIGTERM },
            );
        }
    }
}
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        self.signal(true);
    }
}

fn command(args: &[String], directory: &Path) -> Result<Command> {
    let program = args
        .first()
        .ok_or_else(|| io_error("The downloader command was empty."))?;
    let mut command = Command::new(program);
    command
        .args(&args[1..])
        .current_dir(directory)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_remove("AZURE_SPEECH_KEY")
        .env_remove("HELPER_TOKEN")
        .kill_on_drop(true);
    #[cfg(unix)]
    {
        command.as_std_mut().process_group(0);
    }
    Ok(command)
}

async fn read_bounded<R: AsyncRead + Unpin>(
    mut source: R,
    limit: usize,
    tail: bool,
) -> Result<Vec<u8>> {
    let mut result = Vec::new();
    let mut block = [0u8; 8192];
    let mut warned = false;
    loop {
        let count = source
            .read(&mut block)
            .await
            .map_err(|_| io_error("The downloader output could not be read."))?;
        if count == 0 {
            if tail && warned && !has_warnings(&result) {
                result.splice(..0, b"WARNING:\n".iter().copied());
            }
            return Ok(result);
        }
        result.extend_from_slice(&block[..count]);
        if tail {
            warned |= has_warnings(&result);
        }
        if result.len() > limit {
            if tail {
                result.drain(..result.len() - limit);
            } else {
                return Err(io_error("The downloader returned oversized metadata."));
            }
        }
    }
}

async fn stop(child: &mut Child, group: &ProcessGroup) {
    group.signal(false);
    if tokio::time::timeout(Duration::from_secs(5), child.wait())
        .await
        .is_err()
    {
        group.signal(true);
        let _ = child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
    }
    // Descendants may outlive a parent that exited normally and retain pipes.
    group.signal(true);
}

pub fn downloader_error(detail: &[u8], message: &str) -> Error {
    let text = String::from_utf8_lossy(detail).to_lowercase();
    if [
        "not a bot",
        "bot verification",
        "confirm you're",
        "confirm you’re",
    ]
    .iter()
    .any(|part| text.contains(part))
    {
        return Error::bad_request(
            "YouTube requested sign-in or bot verification. Check the optional Brave login setting and try again.",
        );
    }
    if [
        "private video",
        "video unavailable",
        "video has been removed",
        "this video has been removed",
        "not available in your country",
        "not available in your region",
        "this video is no longer available",
    ]
    .iter()
    .any(|part| text.contains(part))
    {
        return Error::new(
            410,
            "This video is private, removed, or unavailable in your region.",
        );
    }
    if text.contains("sign in") || text.contains("cookies") {
        return Error::bad_request(
            "YouTube requested sign-in. Check the optional Brave login setting and try again.",
        );
    }
    if text.contains("requested format is not available") {
        return Error::bad_request(
            "YouTube did not provide the requested audio format. Check the downloader JavaScript runtime and try again.",
        );
    }
    if text.contains("javascript") || text.contains("js runtime") || text.contains("challenge") {
        return Error::bad_request(format!(
            "{message} Check that a current Node.js or Deno runtime and yt-dlp[default] are installed."
        ));
    }
    Error::bad_request(message)
}

pub async fn run_process(
    args: &[String],
    directory: &Path,
    message: &str,
    timeout: Duration,
    cancel: &CancellationToken,
    fail_on_warnings: bool,
) -> Result<Vec<u8>> {
    if cancel.is_cancelled() {
        return Err(cancelled());
    }
    let mut child = command(args, directory)?
        .spawn()
        .map_err(|_| io_error(message))?;
    let group = ProcessGroup(child.id().ok_or_else(|| io_error(message))?);
    let stdout = child.stdout.take().ok_or_else(|| io_error(message))?;
    let stderr = child.stderr.take().ok_or_else(|| io_error(message))?;
    let out_task = tokio::spawn(read_bounded(stdout, 64 * 1024 * 1024, false));
    let err_task = tokio::spawn(read_bounded(stderr, 64 * 1024, true));
    let status = tokio::select! {
        biased;
        _ = cancel.cancelled() => Err(cancelled()),
        result = tokio::time::timeout(timeout, child.wait()) => match result {
            Ok(Ok(status)) => Ok(status),
            Ok(Err(_)) => Err(io_error(message)),
            Err(_) => Err(Error::bad_request(format!("{message} The operation timed out; temporary audio will be deleted."))),
        }
    };
    stop(&mut child, &group).await;
    let stdout = tokio::time::timeout(Duration::from_secs(5), out_task)
        .await
        .map_err(|_| io_error(message))?
        .map_err(|_| io_error(message))?;
    let stderr = tokio::time::timeout(Duration::from_secs(5), err_task)
        .await
        .map_err(|_| io_error(message))?
        .map_err(|_| io_error(message))??;
    let status = status?;
    if !status.success() {
        return Err(downloader_error(&stderr, message));
    }
    if fail_on_warnings && has_warnings(&stderr) {
        return Err(Error::bad_request(
            "YouTube reported an incomplete channel listing. Already queued videos remain in your library.",
        ));
    }
    stdout
}

fn has_warnings(value: &[u8]) -> bool {
    let text = String::from_utf8_lossy(value).to_ascii_lowercase();
    text.contains("warning:") || text.contains("error:")
}

pub fn ytdlp_arguments(settings: &Settings) -> Result<Vec<String>> {
    let local = settings.root.join(".venv/bin/yt-dlp");
    let executable = if local.is_file() {
        local
    } else {
        which::which("yt-dlp")
            .map_err(|_| Error::new(503, "yt-dlp is required. Run setup again."))?
    };
    let mut args = vec![executable.to_string_lossy().into_owned()];
    args.extend(
        [
            "--ignore-config",
            "--no-cache-dir",
            "--no-playlist",
            "--no-warnings",
            "--quiet",
            "--socket-timeout",
            "30",
            "--retries",
            "3",
            "--fragment-retries",
            "3",
            "--extractor-retries",
            "3",
        ]
        .map(String::from),
    );
    if !settings.ffmpeg_location.is_empty() {
        args.extend(["--ffmpeg-location".into(), settings.ffmpeg_location.clone()]);
    }
    let runtime = settings.javascript_runtime();
    if let Some(runtime) = runtime {
        args.extend(["--js-runtimes".into(), runtime]);
    }
    if !settings.cookies_browser.is_empty() {
        args.extend([
            "--cookies-from-browser".into(),
            settings.cookies_browser.clone(),
        ]);
    }
    Ok(args)
}

pub async fn metadata(
    settings: &Settings,
    url: &str,
    directory: &Path,
    cancel: &CancellationToken,
) -> Result<Value> {
    let mut args = ytdlp_arguments(settings)?;
    args.extend([
        "--format".into(),
        "bestaudio".into(),
        "--dump-single-json".into(),
        "--skip-download".into(),
        url.into(),
    ]);
    let output = run_process(
        &args,
        directory,
        "Could not read this YouTube video.",
        Duration::from_secs(180),
        cancel,
        false,
    )
    .await?;
    let value: Value = serde_json::from_slice(&output)
        .map_err(|_| Error::bad_request("YouTube returned unreadable video information."))?;
    if !value.is_object() {
        return Err(Error::bad_request(
            "YouTube returned unreadable video information.",
        ));
    }
    Ok(value)
}

/// Callback returns false at a verified per-tab date cutoff. Always reap the
/// listing process before releasing the private directory, including on error.
pub async fn stream_tab<F>(
    settings: &Settings,
    url: &str,
    cancel: &CancellationToken,
    mut entry: F,
) -> Result<()>
where
    F: FnMut(Value) -> BoxFuture<'static, Result<bool>>,
{
    let parsed = url::Url::parse(url)
        .map_err(|_| Error::bad_request("Use a supported YouTube channel upload tab."))?;
    let tab = parsed
        .path()
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("");
    if parsed.scheme() != "https"
        || parsed.host_str() != Some("www.youtube.com")
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.port().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
        || !["videos", "shorts", "streams"].contains(&tab)
    {
        return Err(Error::bad_request(
            "Use a supported YouTube channel upload tab.",
        ));
    }
    let work = settings.data_dir.join("work");
    std::fs::create_dir_all(&work)?;
    let temporary = tempfile::Builder::new()
        .prefix("channel-stream-")
        .tempdir_in(work)?;
    let mut args = ytdlp_arguments(settings)?;
    args.retain(|part| part != "--no-playlist" && part != "--no-warnings");
    args.extend(
        [
            "--yes-playlist",
            "--flat-playlist",
            "--lazy-playlist",
            "--dump-json",
            "--skip-download",
            "--no-progress",
            url,
        ]
        .map(String::from),
    );
    let mut child = command(&args, temporary.path())?
        .spawn()
        .map_err(|_| Error::bad_request("The channel downloader could not start."))?;
    let group = ProcessGroup(
        child
            .id()
            .ok_or_else(|| io_error("The channel downloader could not start."))?,
    );
    let mut stdout = BufReader::new(
        child
            .stdout
            .take()
            .ok_or_else(|| io_error("Channel output unavailable."))?,
    );
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io_error("Channel output unavailable."))?;
    let err_task = tokio::spawn(read_bounded(stderr, 65536, true));
    let mut count = 0;
    let mut early = false;
    let result:Result<()>=async {
        loop {
            let mut line=Vec::new();
            let mut limited=(&mut stdout).take(1024*1024+1);
            let read=tokio::select! {
                biased; _=cancel.cancelled()=>return Err(cancelled()),
                read=tokio::time::timeout(Duration::from_secs(180),limited.read_until(b'\n',&mut line))=>read,
            }.map_err(|_|Error::bad_request("Channel discovery stopped responding. Already queued videos remain in your library."))?.map_err(|_|io_error("Channel output could not be read."))?;
            if read==0 {break;}
            if line.len()>1024*1024 {return Err(Error::bad_request("YouTube returned an oversized channel entry."));}
            if line.iter().all(u8::is_ascii_whitespace) {continue;}
            let value:Value=serde_json::from_slice(&line).map_err(|_|Error::bad_request("YouTube returned an unreadable channel entry."))?;
            if !value.is_object() {return Err(Error::bad_request("YouTube returned an unreadable channel entry."));}
            count+=1;
            if !entry(value).await? {early=true;break;}
        }
        if !early {
            let status=tokio::time::timeout(Duration::from_secs(10),child.wait()).await.map_err(|_|io_error("The channel downloader did not finish."))??;
            if !status.success() {return Err(Error::new(502,"channel-process-failed"));}
        }
        Ok(())
    }.await;
    stop(&mut child, &group).await;
    let detail = tokio::time::timeout(Duration::from_secs(5), err_task)
        .await
        .map_err(|_| io_error("The channel downloader could not stop cleanly."))?
        .map_err(|_| io_error("The channel downloader could not stop cleanly."))??;
    if result
        .as_ref()
        .err()
        .is_some_and(|error| error.message == "channel-process-failed")
    {
        let text = String::from_utf8_lossy(&detail).to_lowercase();
        if count == 0
            && text.contains(&format!("this channel does not have a {tab} tab"))
            && !["sign in", "not a bot", "bot verification"]
                .iter()
                .any(|part| text.contains(part))
        {
            return Err(Error::new(424, format!("This channel has no {tab} tab.")));
        }
        return Err(downloader_error(
            &detail,
            "Channel discovery could not finish. Already queued videos remain in your library.",
        ));
    }
    result?;
    if has_warnings(&detail) {
        return Err(Error::bad_request(
            "YouTube reported an incomplete channel listing. Already queued videos remain in your library.",
        ));
    }
    Ok(())
}

pub fn words(args: &[&str]) -> Vec<String> {
    args.iter().map(|part| (*part).to_owned()).collect()
}
pub fn executable(settings: &Settings, name: &str) -> Result<PathBuf> {
    settings
        .executable(name)
        .ok_or_else(|| Error::new(503, format!("{name} is required. Run setup again.")))
}
