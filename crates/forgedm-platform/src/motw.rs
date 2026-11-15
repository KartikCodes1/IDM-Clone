//! Mark-of-the-Web: Zone.Identifier and IAttachmentExecute (ARCHITECTURE 5.6, ADR-015).
//!
//! Two mechanisms are combined:
//!
//! * a **direct write** of the `Zone.Identifier` alternate data stream ([`write_zone_identifier`]),
//!   which needs no COM and is used for empty files, for IAE failures, and while the IAE worker
//!   is poisoned;
//! * [`MotwService`], a dedicated single-threaded apartment (STA) worker that calls the shell's
//!   `IAttachmentExecute` so Windows policy and antivirus providers see the file.
//!
//! URLs recorded in the stream are sanitised by [`zone_source_url`] (PLAN D8): `scheme://host[:port]/path`
//! without userinfo, query or fragment, because IAE and browsers otherwise persist signed tokens.
//!
//! Unsafe is confined to the COM calls at the bottom of this file; each block has a `SAFETY`
//! comment and COM objects and the apartment are released by RAII.
#![allow(unsafe_code)] // IAttachmentExecute COM FFI; every block has a SAFETY comment.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoCreateInstance,
    CoInitializeEx, CoUninitialize,
};
use windows::Win32::UI::Shell::{AttachmentServices, IAttachmentExecute};
use windows::core::{GUID, HSTRING, IUnknown};

use crate::error::{PlatformError, unwrap_hresult};
use crate::wide::verbatim_path;

/// Name of the alternate data stream that carries the zone.
const ZONE_STREAM_SUFFIX: &str = ":Zone.Identifier";
/// Zone id for the Internet zone.
pub const INTERNET_ZONE_ID: u32 = 3;
/// Value recorded when a URL is not a usable http(s) URL.
pub const ABOUT_INTERNET: &str = "about:internet";
/// Longest sanitised or raw URL (bytes) that is recorded; longer values become [`ABOUT_INTERNET`].
pub const MAX_URL_LEN: usize = 2048;
/// How long a caller waits for IAE before the worker is poisoned.
pub const IAE_TIMEOUT: Duration = Duration::from_secs(60);
/// Fixed IAE client GUID identifying ForgeDM to the shell (generated once, never changes).
pub const FORGEDM_CLIENT_GUID: GUID = GUID::from_u128(0x7b3f_1c52_9e4a_4d18_a6c7_2f8e_5b0d_91a3);

const ERROR_SHARING_VIOLATION: i32 = 32;
const ERROR_ACCESS_DENIED: u32 = 5;
const ERROR_NOT_SUPPORTED: i32 = 50;
const ERROR_INVALID_NAME: i32 = 123;
const ERROR_DISK_FULL: i32 = 112;
const ERROR_HANDLE_DISK_FULL: i32 = 39;
/// `INET_E_SECURITY_PROBLEM` (Urlmon.h): the shell refused the file for security reasons.
const INET_E_SECURITY_PROBLEM: u32 = 0x800C_000E;

// ---------------------------------------------------------------------------------------------
// URL sanitiser
// ---------------------------------------------------------------------------------------------

/// Reduces `url` to the form recorded in Mark-of-the-Web (PLAN D8).
///
/// Only `http` and `https` URLs with a host are kept, as `scheme://host[:port]/path` (scheme
/// and host lower-cased, an empty path becomes `/`). Userinfo is always removed. The query and
/// fragment are removed unless `record_full` is true. Anything else, or a result longer than
/// [`MAX_URL_LEN`], becomes [`ABOUT_INTERNET`].
///
/// # Errors
/// [`PlatformError::InvalidInput`] when `url` contains a control character (CR and LF would
/// let a caller inject extra stream lines).
pub fn zone_source_url(url: &str, record_full: bool) -> Result<String, PlatformError> {
    reject_control_chars(url)?;
    if url.len() > MAX_URL_LEN * 4 {
        return Ok(ABOUT_INTERNET.to_owned());
    }
    let Some((scheme, rest)) = split_scheme(url) else {
        return Ok(ABOUT_INTERNET.to_owned());
    };
    let (authority, tail) = match rest.find(['/', '?', '#']) {
        Some(i) => rest.split_at(i),
        None => (rest, ""),
    };
    let host_port = authority.rsplit('@').next().unwrap_or("");
    if !valid_host_port(host_port) {
        return Ok(ABOUT_INTERNET.to_owned());
    }
    let (path, suffix) = match tail.find(['?', '#']) {
        Some(i) => tail.split_at(i),
        None => (tail, ""),
    };
    let path = if path.is_empty() { "/" } else { path };
    let keep = if record_full { suffix } else { "" };
    let out = format!("{scheme}://{}{path}{keep}", host_port.to_ascii_lowercase());
    Ok(if out.len() > MAX_URL_LEN {
        ABOUT_INTERNET.to_owned()
    } else {
        out
    })
}

fn split_scheme(url: &str) -> Option<(&'static str, &str)> {
    let (scheme, rest) = url.split_once("://")?;
    if scheme.eq_ignore_ascii_case("http") {
        Some(("http", rest))
    } else if scheme.eq_ignore_ascii_case("https") {
        Some(("https", rest))
    } else {
        None
    }
}

fn valid_host_port(host_port: &str) -> bool {
    !host_port.is_empty()
        && !host_port.starts_with(':')
        && !host_port
            .chars()
            .any(|c| c.is_whitespace() || c == '\\' || c == '@')
}

fn reject_control_chars(value: &str) -> Result<(), PlatformError> {
    // Cc plus the Unicode line and paragraph separators some text readers treat as line breaks.
    if value
        .chars()
        .any(|c| c.is_control() || c == '\u{2028}' || c == '\u{2029}')
    {
        Err(PlatformError::InvalidInput(
            "URL contains a control character",
        ))
    } else {
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Direct Zone.Identifier write
// ---------------------------------------------------------------------------------------------

/// Contents of the Zone.Identifier stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ZoneInfo {
    /// Security zone (3 = Internet).
    pub zone_id: u32,
    /// Sanitised download URL (see [`zone_source_url`]).
    pub host_url: String,
    /// Sanitised referrer, omitted from the stream when `None`.
    pub referrer_url: Option<String>,
}

impl ZoneInfo {
    fn validate(&self) -> Result<(), PlatformError> {
        reject_control_chars(&self.host_url)?;
        if let Some(referrer) = &self.referrer_url {
            reject_control_chars(referrer)?;
        }
        Ok(())
    }

    fn render(&self) -> String {
        let mut text = format!("[ZoneTransfer]\r\nZoneId={}\r\n", self.zone_id);
        if let Some(referrer) = &self.referrer_url {
            text.push_str(&format!("ReferrerUrl={referrer}\r\n"));
        }
        text.push_str(&format!("HostUrl={}\r\n", self.host_url));
        text
    }
}

/// Writes `<path>:Zone.Identifier` directly and flushes it (`FlushFileBuffers`).
///
/// The stream is replaced if present, so redoing the write after a crash is idempotent. The
/// base file must already exist: it is never created as a side effect.
///
/// # Errors
/// [`PlatformError::InvalidInput`] for CR/LF or other control characters in the URLs;
/// [`PlatformError::Unsupported`] when the volume has no alternate data streams;
/// [`PlatformError::SharingViolation`], [`PlatformError::DiskFull`] or [`PlatformError::Io`]
/// for other failures.
pub fn write_zone_identifier(path: &Path, info: &ZoneInfo) -> Result<(), PlatformError> {
    info.validate()?;
    let base = verbatim_path(path)?;
    fs::metadata(&base)?;
    let mut stream = base.into_os_string();
    stream.push(ZONE_STREAM_SUFFIX);
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(PathBuf::from(stream))
        .map_err(map_stream_error)?;
    file.write_all(info.render().as_bytes())
        .map_err(map_stream_error)?;
    file.sync_all().map_err(map_stream_error)
}

fn map_stream_error(err: io::Error) -> PlatformError {
    match err.raw_os_error() {
        Some(ERROR_INVALID_NAME | ERROR_NOT_SUPPORTED) => {
            PlatformError::Unsupported("file system has no alternate data streams")
        }
        Some(ERROR_SHARING_VIOLATION) => PlatformError::SharingViolation,
        Some(ERROR_DISK_FULL | ERROR_HANDLE_DISK_FULL) => PlatformError::DiskFull,
        _ => PlatformError::Io(err),
    }
}

// ---------------------------------------------------------------------------------------------
// IAttachmentExecute abstraction
// ---------------------------------------------------------------------------------------------

/// Input to one IAttachmentExecute call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IaeRequest {
    /// File to mark.
    pub path: PathBuf,
    /// Sanitised source URL (`SetSource`).
    pub source: String,
    /// Sanitised referrer (`SetReferrer`), when known.
    pub referrer: Option<String>,
}

/// Why an IAttachmentExecute call did not mark the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IaeFailure {
    /// Group policy refused the file.
    BlockedByPolicy,
    /// Any other failure, with the HRESULT (or Win32 code) as unsigned.
    Failed(u32),
}

/// Backend that applies Mark-of-the-Web through the shell. Injectable so tests can fake it.
///
/// `apply` runs on the service's STA worker thread, one call at a time.
pub trait AttachmentExecutor: Send + 'static {
    /// Marks `request.path`.
    ///
    /// # Errors
    /// An [`IaeFailure`] describing the refusal or error.
    fn apply(&self, request: &IaeRequest) -> Result<(), IaeFailure>;
}

/// Result of [`MotwService::protect`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotwOutcome {
    /// IAttachmentExecute marked the file.
    Marked,
    /// The Zone.Identifier stream was written directly.
    MarkedDirect,
    /// Group policy refused the file.
    BlockedByPolicy,
    /// An antivirus provider removed the file.
    RemovedByAntivirus,
    /// The volume cannot hold alternate data streams.
    UnsupportedFs,
}

// ---------------------------------------------------------------------------------------------
// MotwService
// ---------------------------------------------------------------------------------------------

struct Job {
    request: IaeRequest,
    reply: Sender<Result<(), IaeFailure>>,
    state: Arc<Mutex<JobState>>,
}

#[derive(Default)]
struct JobState {
    finished: bool,
    abandoned: bool,
}

/// Dedicated STA worker running IAttachmentExecute (never blocks other downloads: callers
/// wait only on their own reply and fall back to the direct write on timeout).
pub struct MotwService {
    jobs: Option<Sender<Job>>,
    worker: Option<JoinHandle<()>>,
    poisoned: Arc<Mutex<bool>>,
    timeout: Duration,
}

impl MotwService {
    /// Starts the service with the real shell backend and the 60 s [`IAE_TIMEOUT`].
    ///
    /// # Errors
    /// [`PlatformError::MotwFailed`] when the worker thread cannot be spawned.
    pub fn start() -> Result<Self, PlatformError> {
        Self::with_executor(ShellAttachmentExecutor, IAE_TIMEOUT)
    }

    /// Starts the service with an injected backend and call timeout.
    ///
    /// # Errors
    /// [`PlatformError::MotwFailed`] when the worker thread cannot be spawned.
    pub fn with_executor<E: AttachmentExecutor>(
        executor: E,
        timeout: Duration,
    ) -> Result<Self, PlatformError> {
        let (jobs, inbox) = mpsc::channel::<Job>();
        let poisoned = Arc::new(Mutex::new(false));
        let worker_poisoned = Arc::clone(&poisoned);
        let worker = thread::Builder::new()
            .name("forgedm-motw-sta".to_owned())
            .spawn(move || worker_loop(&executor, &inbox, &worker_poisoned))
            .map_err(|e| PlatformError::MotwFailed {
                api: "thread::spawn",
                code: u32::try_from(e.raw_os_error().unwrap_or(0)).unwrap_or(0),
            })?;
        Ok(Self {
            jobs: Some(jobs),
            worker: Some(worker),
            poisoned,
            timeout,
        })
    }

    /// True while a timed-out IAE call is still stuck on the worker.
    #[must_use]
    pub fn is_poisoned(&self) -> bool {
        *lock(&self.poisoned)
    }

    /// Marks `path` as downloaded from `info`, preferring IAE and falling back to the direct write.
    ///
    /// Zero-byte files, a poisoned worker, an IAE timeout and IAE failures with the file still
    /// present all use the direct write ([`MotwOutcome::MarkedDirect`]).
    ///
    /// # Errors
    /// [`PlatformError::InvalidInput`] for control characters in the URLs, or the error of the
    /// direct write when it is needed and fails (other than an unsupported volume, which is
    /// the [`MotwOutcome::UnsupportedFs`] outcome).
    pub fn protect(&self, path: &Path, info: &ZoneInfo) -> Result<MotwOutcome, PlatformError> {
        info.validate()?;
        let verbatim = verbatim_path(path)?;
        if fs::metadata(&verbatim)?.len() == 0 || self.is_poisoned() {
            return direct_outcome(path, info);
        }
        let request = IaeRequest {
            path: verbatim.clone(),
            source: info.host_url.clone(),
            referrer: info.referrer_url.clone(),
        };
        match self.run_iae(request) {
            Some(Ok(())) if verbatim.exists() => Ok(MotwOutcome::Marked),
            Some(Err(IaeFailure::BlockedByPolicy)) => Ok(MotwOutcome::BlockedByPolicy),
            Some(_) if !verbatim.exists() => Ok(MotwOutcome::RemovedByAntivirus),
            // A timeout or dead worker says nothing about who removed the file.
            None if !verbatim.exists() => {
                Err(PlatformError::Io(io::Error::from(io::ErrorKind::NotFound)))
            }
            _ => direct_outcome(path, info),
        }
    }

    /// Sends one job and waits for it; `None` means timeout or a dead worker.
    fn run_iae(&self, request: IaeRequest) -> Option<Result<(), IaeFailure>> {
        let (reply, answer) = mpsc::channel();
        let state = Arc::new(Mutex::new(JobState::default()));
        let job = Job {
            request,
            reply,
            state: Arc::clone(&state),
        };
        self.jobs.as_ref()?.send(job).ok()?;
        match answer.recv_timeout(self.timeout) {
            Ok(result) => Some(result),
            Err(RecvTimeoutError::Disconnected) => None,
            Err(RecvTimeoutError::Timeout) => {
                // Decide under the poison lock so a job finishing right now cannot leave the
                // flag set after the worker has already cleared it.
                let mut poisoned = lock(&self.poisoned);
                let mut job_state = lock(&state);
                if job_state.finished {
                    drop(job_state);
                    drop(poisoned);
                    return answer.try_recv().ok();
                }
                job_state.abandoned = true;
                *poisoned = true;
                tracing::warn!("IAttachmentExecute timed out; worker poisoned");
                None
            }
        }
    }
}

impl Drop for MotwService {
    fn drop(&mut self) {
        self.jobs = None;
        if let Some(worker) = self.worker.take() {
            if *lock(&self.poisoned) {
                // A stuck COM call cannot be cancelled; detach rather than hang shutdown.
                tracing::warn!("MOTW worker is stuck; detaching it");
            } else if worker.join().is_err() {
                tracing::warn!("MOTW worker panicked");
            }
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn direct_outcome(path: &Path, info: &ZoneInfo) -> Result<MotwOutcome, PlatformError> {
    match write_zone_identifier(path, info) {
        Ok(()) => Ok(MotwOutcome::MarkedDirect),
        Err(PlatformError::Unsupported(_)) => Ok(MotwOutcome::UnsupportedFs),
        Err(other) => Err(other),
    }
}

fn worker_loop<E: AttachmentExecutor>(
    executor: &E,
    inbox: &mpsc::Receiver<Job>,
    poisoned: &Mutex<bool>,
) {
    let _apartment = Apartment::enter();
    while let Ok(job) = inbox.recv() {
        {
            // An abandoned job set the poison flag when its caller timed out; the worker is idle
            // again here, so skipping the job must also clear it (same lock order as run_iae).
            let mut flag = lock(poisoned);
            if lock(&job.state).abandoned {
                *flag = false;
                continue;
            }
        }
        let result = executor.apply(&job.request);
        let mut flag = lock(poisoned);
        lock(&job.state).finished = true;
        *flag = false;
        drop(flag);
        // The caller may have gone away; nothing else to do with the answer then.
        let _ = job.reply.send(result);
    }
}

// ---------------------------------------------------------------------------------------------
// COM
// ---------------------------------------------------------------------------------------------

/// RAII single-threaded COM apartment for the current thread.
struct Apartment {
    initialised: bool,
}

impl Apartment {
    fn enter() -> Self {
        // SAFETY: plain FFI call with no pointers; the matching CoUninitialize runs in Drop on
        // this same thread, and only when initialisation succeeded (S_OK or S_FALSE).
        let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
        if hr.is_err() {
            tracing::warn!(hresult = unwrap_hresult(hr.0), "CoInitializeEx failed");
        }
        Self {
            initialised: hr.is_ok(),
        }
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        if self.initialised {
            // SAFETY: balances the successful CoInitializeEx in `enter` on the same thread.
            unsafe { CoUninitialize() };
        }
    }
}

/// The real backend: `IAttachmentExecute` from the shell, created per call on the STA thread.
struct ShellAttachmentExecutor;

impl AttachmentExecutor for ShellAttachmentExecutor {
    fn apply(&self, request: &IaeRequest) -> Result<(), IaeFailure> {
        shell_save(request)
    }
}

fn failed(err: &windows::core::Error) -> IaeFailure {
    IaeFailure::Failed(unwrap_hresult(err.code().0))
}

fn shell_save(request: &IaeRequest) -> Result<(), IaeFailure> {
    // SAFETY: CoCreateInstance receives a valid CLSID reference and no outer unknown; the
    // returned interface is released when `attachment` drops at the end of this function.
    let attachment: IAttachmentExecute =
        unsafe { CoCreateInstance(&AttachmentServices, None::<&IUnknown>, CLSCTX_INPROC_SERVER) }
            .map_err(|e| failed(&e))?;
    let path = HSTRING::from(request.path.as_os_str());
    let source = HSTRING::from(request.source.as_str());
    // SAFETY: every pointer passed below comes from a live HSTRING/GUID that outlives the call,
    // and `attachment` is a valid interface on this (STA) thread.
    unsafe {
        attachment
            .SetClientGuid(&FORGEDM_CLIENT_GUID)
            .map_err(|e| failed(&e))?;
        attachment.SetLocalPath(&path).map_err(|e| failed(&e))?;
        attachment.SetSource(&source).map_err(|e| failed(&e))?;
        if let Some(referrer) = &request.referrer {
            let referrer = HSTRING::from(referrer.as_str());
            attachment.SetReferrer(&referrer).map_err(|e| failed(&e))?;
        }
        attachment
            .CheckPolicy()
            .map_err(|e| iae_failure_from_hresult(e.code().0, true))?;
        attachment
            .Save()
            .map_err(|e| iae_failure_from_hresult(e.code().0, false))
    }
}

/// Maps an IAttachmentExecute HRESULT to an [`IaeFailure`].
///
/// `INET_E_SECURITY_PROBLEM` and access denied mean the shell refused the file, from either
/// call. Any other `CheckPolicy` failure is also a refusal (the documented contract treats
/// anything but `S_OK`/`S_FALSE` as "disable"), so it must never fall through to a direct write
/// that reports success. Other `Save` failures stay [`IaeFailure::Failed`].
#[must_use]
pub fn iae_failure_from_hresult(hresult: i32, from_check_policy: bool) -> IaeFailure {
    match unwrap_hresult(hresult) {
        INET_E_SECURITY_PROBLEM | ERROR_ACCESS_DENIED => IaeFailure::BlockedByPolicy,
        _ if from_check_policy => IaeFailure::BlockedByPolicy,
        other => IaeFailure::Failed(other),
    }
}
