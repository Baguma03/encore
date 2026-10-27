//! The system "Open file" dialog, through the XDG Desktop Portal `FileChooser` interface, with
//! `zenity` or `kdialog` as a fallback when no portal answers.

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use log::{debug, warn};
use percent_encoding::percent_decode_str;
use zbus::blocking::{connection, Connection, MessageIterator};
use zbus::message::Type as MessageType;
use zbus::zvariant::{ObjectPath, OwnedObjectPath, OwnedValue, Value};
use zbus::MatchRule;

const PORTAL_BUS: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const FILE_CHOOSER_IFACE: &str = "org.freedesktop.portal.FileChooser";
const REQUEST_IFACE: &str = "org.freedesktop.portal.Request";
/// How long the portal (including D-Bus activation) has to accept the request. The dialog
/// itself stays open as long as the user needs.
const CALL_TIMEOUT: Duration = Duration::from_secs(10);
/// `FileChooser` filter entry type for a glob pattern (1 is a MIME type).
const GLOB: u32 = 0;

/// A named file filter of glob patterns, such as `("Lyrics", &["*.lrc", "*.txt"])`.
pub type Filter<'a> = (&'a str, &'a [&'a str]);

/// How a file pick ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PickResult {
    Picked(PathBuf),
    Cancelled,
    /// No dialog could be shown, or it returned something other than a local file.
    Unavailable(String),
}

/// How long closing a request keeps retrying while the portal answers that the request does
/// not exist yet: right after `OpenFile` returns, the backend may not have created its side of
/// the request.
const CLOSE_RETRY: Duration = Duration::from_secs(5);

/// The portal request of a picker, shared between the picker and its thread.
enum Request {
    /// No portal dialog is open (yet, or any more).
    Idle,
    /// The portal dialog is open on this connection and request handle.
    Open(Connection, OwnedObjectPath),
    /// The picker was dropped; a dialog opened from now on is closed at once.
    Abandoned,
}

/// An "Open file" dialog running on a background thread.
pub struct FilePicker {
    rx: Receiver<PickResult>,
    finished: bool,
    request: Arc<Mutex<Request>>,
}

impl FilePicker {
    /// Opens the dialog with `title`, offering `filters` (the first one selected), and returns
    /// at once. `on_ready` runs on the background thread once a result is available, which is
    /// where to request a repaint.
    pub fn open<F>(title: &str, filters: &[Filter<'_>], on_ready: F) -> Self
    where
        F: FnOnce() + Send + 'static,
    {
        let title = title.to_owned();
        let filters = owned_filters(filters);
        let request = Arc::new(Mutex::new(Request::Idle));
        let (tx, rx) = mpsc::channel();
        let thread_request = Arc::clone(&request);
        let spawned = thread::Builder::new()
            .name("file-picker".into())
            .spawn(move || {
                let result = pick(&title, &filters, &thread_request);
                debug!("file picker result: {result:?}");
                // The receiver is gone when the picker was dropped; nobody is waiting then.
                let _ = tx.send(result);
                on_ready();
            });
        if let Err(e) = spawned {
            warn!("file picker thread failed to start: {e}");
        }
        Self {
            rx,
            finished: false,
            request,
        }
    }

    /// The result once the dialog has closed, returned exactly once; `None` while it is open
    /// and after the result has been taken.
    pub fn poll(&mut self) -> Option<PickResult> {
        if self.finished {
            return None;
        }
        let result = match self.rx.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return None,
            Err(TryRecvError::Disconnected) => {
                PickResult::Unavailable("file picker thread ended without a result".into())
            }
        };
        self.finished = true;
        Some(result)
    }

    /// Whether the dialog is still open, i.e. `poll` has not returned a result yet.
    pub fn is_pending(&self) -> bool {
        !self.finished
    }
}

impl Drop for FilePicker {
    /// Closes the portal dialog when it is still open, on a separate thread as closing may wait
    /// for the portal.
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let Ok(mut request) = self.request.lock() else {
            return;
        };
        if let Request::Open(conn, handle) = std::mem::replace(&mut *request, Request::Abandoned) {
            let _ = thread::Builder::new()
                .name("file-picker-close".into())
                .spawn(move || close_request(&conn, &handle));
        }
    }
}

/// Filters as owned `(name, patterns)` pairs, dropping filters without patterns.
fn owned_filters(filters: &[Filter<'_>]) -> Vec<(String, Vec<String>)> {
    filters
        .iter()
        .filter(|(_, patterns)| !patterns.is_empty())
        .map(|(name, patterns)| {
            let patterns = patterns.iter().map(|p| (*p).to_owned()).collect();
            ((*name).to_owned(), patterns)
        })
        .collect()
}

/// Filters in the portal's `a(sa(us))` form: each pattern tagged as a glob.
fn portal_filters(filters: &[(String, Vec<String>)]) -> Vec<(String, Vec<(u32, String)>)> {
    filters
        .iter()
        .map(|(name, patterns)| {
            let entries = patterns.iter().map(|p| (GLOB, p.clone())).collect();
            (name.clone(), entries)
        })
        .collect()
}

/// Runs the portal dialog, falling back to `zenity` or `kdialog` when the portal is missing.
fn pick(title: &str, filters: &[(String, Vec<String>)], request: &Mutex<Request>) -> PickResult {
    let portal_error = match pick_with_portal(title, filters, request) {
        Ok(result) => return result,
        Err(e) => e,
    };
    debug!("file chooser portal unavailable: {portal_error}");
    match pick_with_helper(title, filters) {
        Some(result) => result,
        None => PickResult::Unavailable(format!("no file chooser portal: {portal_error}")),
    }
}

/// Errors are those that mean the portal could not show the dialog at all.
fn pick_with_portal(
    title: &str,
    filters: &[(String, Vec<String>)],
    request: &Mutex<Request>,
) -> zbus::Result<PickResult> {
    let conn = connection::Builder::session()?
        .method_timeout(CALL_TIMEOUT)
        .build()?;
    let (handle, responses) = start_open_file(&conn, title, filters)?;
    {
        let mut request = request
            .lock()
            .map_err(|_| zbus::Error::Failure("file picker state poisoned".into()))?;
        if matches!(*request, Request::Abandoned) {
            close_request(&conn, &handle);
            return Ok(PickResult::Cancelled);
        }
        *request = Request::Open(conn.clone(), handle.clone());
    }
    let result = await_response(responses);
    if let Ok(mut request) = request.lock() {
        if matches!(*request, Request::Open(..)) {
            *request = Request::Idle;
        }
    }
    Ok(result)
}

/// Calls `OpenFile` and returns the request handle with an iterator over its `Response`
/// signals. The subscription is made before the call, on the handle the portal specification
/// derives from the unique bus name and `handle_token`, so a fast response is not missed.
fn start_open_file(
    conn: &Connection,
    title: &str,
    filters: &[(String, Vec<String>)],
) -> zbus::Result<(OwnedObjectPath, MessageIterator)> {
    let token = handle_token();
    let unique = conn
        .unique_name()
        .ok_or_else(|| zbus::Error::Failure("session bus connection has no unique name".into()))?;
    let expected = request_path(unique.as_str(), &token);
    let mut responses = response_signals(conn, &expected)?;

    let portal_filters = portal_filters(filters);
    let mut options: HashMap<&str, Value<'_>> = HashMap::new();
    options.insert("handle_token", Value::from(token.as_str()));
    options.insert("modal", Value::from(true));
    options.insert("multiple", Value::from(false));
    if let Some(first) = portal_filters.first() {
        options.insert("current_filter", Value::from(first.clone()));
    }
    if !portal_filters.is_empty() {
        options.insert("filters", Value::from(portal_filters));
    }

    let handle: OwnedObjectPath = conn
        .call_method(
            Some(PORTAL_BUS),
            PORTAL_PATH,
            Some(FILE_CHOOSER_IFACE),
            "OpenFile",
            &("", title, options),
        )?
        .body()
        .deserialize()?;
    // Portals older than version 0.9 ignore `handle_token` and pick their own handle.
    if handle.as_str() != expected {
        responses = response_signals(conn, &handle)?;
    }
    Ok((handle, responses))
}

/// Subscribes to the `Response` signal of the request at `path`.
fn response_signals(conn: &Connection, path: &str) -> zbus::Result<MessageIterator> {
    let rule = MatchRule::builder()
        .msg_type(MessageType::Signal)
        .interface(REQUEST_IFACE)?
        .member("Response")?
        .path(path)?
        .build();
    MessageIterator::for_match_rule(rule, conn, Some(4))
}

/// Waits for the first `Response` signal and decodes it.
fn await_response(mut responses: MessageIterator) -> PickResult {
    let Some(message) = responses.next() else {
        return PickResult::Unavailable("session bus closed before the dialog answered".into());
    };
    let body = match message {
        Ok(message) => message.body(),
        Err(e) => return PickResult::Unavailable(format!("reading portal response: {e}")),
    };
    match body.deserialize::<(u32, HashMap<String, OwnedValue>)>() {
        Ok((code, results)) => response_result(code, &results),
        Err(e) => PickResult::Unavailable(format!("decoding portal response: {e}")),
    }
}

/// The pick a `Response` signal reports: code 0 is success, 1 cancelled by the user and 2
/// ended some other way.
fn response_result(code: u32, results: &HashMap<String, OwnedValue>) -> PickResult {
    match code {
        0 => {
            let uris: Vec<String> = results
                .get("uris")
                .and_then(|v| Vec::<String>::try_from(v.try_clone().ok()?).ok())
                .unwrap_or_default();
            match uris.first() {
                Some(uri) => match uri_to_path(uri) {
                    Some(path) => PickResult::Picked(path),
                    None => PickResult::Unavailable(format!("not a local file: {uri}")),
                },
                None => PickResult::Unavailable("portal returned no file".into()),
            }
        }
        1 => PickResult::Cancelled,
        _ => PickResult::Unavailable(format!("file chooser ended with code {code}")),
    }
}

/// Closes an open portal request, which dismisses its dialog. Returns whether the portal
/// accepted the call, retrying for up to `CLOSE_RETRY` while the request does not exist yet.
fn close_request(conn: &Connection, handle: &ObjectPath<'_>) -> bool {
    let deadline = Instant::now() + CLOSE_RETRY;
    loop {
        let error =
            match conn.call_method(Some(PORTAL_BUS), handle, Some(REQUEST_IFACE), "Close", &()) {
                Ok(_) => return true,
                Err(e) => e,
            };
        let unknown = matches!(
            &error,
            zbus::Error::MethodError(name, ..) if name.as_str() == "org.freedesktop.DBus.Error.UnknownObject"
        );
        if !unknown || Instant::now() >= deadline {
            debug!("closing portal request {handle}: {error}");
            return false;
        }
        thread::sleep(Duration::from_millis(100));
    }
}

/// A request token unique within this process, as `handle_token` requires: ASCII letters,
/// digits and underscores.
fn handle_token() -> String {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    format!(
        "encore_{}_{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    )
}

/// The request object path the portal uses for `token` from the connection named `unique`:
/// the unique name without its leading `:` and with `.` replaced by `_`.
fn request_path(unique: &str, token: &str) -> String {
    let sender = unique.trim_start_matches(':').replace('.', "_");
    format!("{PORTAL_PATH}/request/{sender}/{token}")
}

/// The local path of a `file://` URI, percent-decoded. Other schemes and remote hosts give
/// `None`.
pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let path = match rest.strip_prefix("localhost") {
        Some(path) => path,
        None => rest,
    };
    if !path.starts_with('/') {
        return None;
    }
    let decoded = percent_decode_str(path).collect::<Vec<u8>>();
    #[cfg(unix)]
    {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        Some(PathBuf::from(OsString::from_vec(decoded)))
    }
    #[cfg(not(unix))]
    {
        Some(PathBuf::from(String::from_utf8(decoded).ok()?))
    }
}

/// Runs `zenity` or `kdialog`, whichever is on `PATH`; `None` when neither is.
fn pick_with_helper(title: &str, filters: &[(String, Vec<String>)]) -> Option<PickResult> {
    let mut command = if on_path("zenity") {
        let mut command = Command::new("zenity");
        command.args(["--file-selection", "--title", title]);
        for (name, patterns) in filters {
            command.arg(format!("--file-filter={name} | {}", patterns.join(" ")));
        }
        command
    } else if on_path("kdialog") {
        let mut command = Command::new("kdialog");
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        command.arg("--title").arg(title).arg("--getopenfilename");
        command.arg(home);
        if !filters.is_empty() {
            command.arg(kdialog_filter(filters));
        }
        command
    } else {
        return None;
    };
    let output = match command.output() {
        Ok(output) => output,
        Err(e) => return Some(PickResult::Unavailable(format!("running file dialog: {e}"))),
    };
    // Both tools exit with 1 when the dialog is cancelled.
    match output.status.code() {
        Some(0) => {
            let path = String::from_utf8_lossy(&output.stdout)
                .trim_end()
                .to_owned();
            if path.is_empty() {
                Some(PickResult::Cancelled)
            } else {
                Some(PickResult::Picked(PathBuf::from(path)))
            }
        }
        Some(1) => Some(PickResult::Cancelled),
        _ => Some(PickResult::Unavailable(format!(
            "file dialog failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))),
    }
}

/// `kdialog`'s filter argument: `Name (*.a *.b)` entries separated by `|`.
fn kdialog_filter(filters: &[(String, Vec<String>)]) -> String {
    filters
        .iter()
        .map(|(name, patterns)| format!("{name} ({})", patterns.join(" ")))
        .collect::<Vec<_>>()
        .join("|")
}

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const LYRICS: [Filter<'static>; 2] = [("Lyrics", &["*.lrc", "*.txt"]), ("Empty", &[])];

    #[test]
    fn file_uris_become_decoded_paths() {
        assert_eq!(
            uri_to_path("file:///home/u/Music/A%20B/01%20%C3%A9.lrc"),
            Some(PathBuf::from("/home/u/Music/A B/01 é.lrc"))
        );
        assert_eq!(
            uri_to_path("file://localhost/srv/x.lrc"),
            Some(PathBuf::from("/srv/x.lrc"))
        );
        assert_eq!(uri_to_path("file:///a%25b"), Some(PathBuf::from("/a%b")));
    }

    #[test]
    fn non_local_uris_are_rejected() {
        assert_eq!(uri_to_path("file://host/x.lrc"), None);
        assert_eq!(uri_to_path("https://example.com/x.lrc"), None);
        assert_eq!(uri_to_path("/plain/path"), None);
    }

    #[test]
    fn filters_drop_empty_entries_and_tag_globs() {
        let owned = owned_filters(&LYRICS);
        assert_eq!(
            owned,
            vec![(
                "Lyrics".to_owned(),
                vec!["*.lrc".to_owned(), "*.txt".to_owned()]
            )]
        );
        assert_eq!(
            portal_filters(&owned),
            vec![(
                "Lyrics".to_owned(),
                vec![(GLOB, "*.lrc".to_owned()), (GLOB, "*.txt".to_owned())]
            )]
        );
        assert_eq!(kdialog_filter(&owned), "Lyrics (*.lrc *.txt)");
    }

    #[test]
    fn filters_encode_as_portal_signature() {
        let value = Value::from(portal_filters(&owned_filters(&LYRICS)));
        assert_eq!(value.value_signature().to_string(), "a(sa(us))");
    }

    #[test]
    fn request_path_follows_portal_convention() {
        assert_eq!(
            request_path(":1.42", "encore_7_0"),
            "/org/freedesktop/portal/desktop/request/1_42/encore_7_0"
        );
        let a = handle_token();
        let b = handle_token();
        assert_ne!(a, b);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
    }

    #[test]
    fn responses_decode_to_results() {
        let mut results = HashMap::new();
        results.insert(
            "uris".to_owned(),
            OwnedValue::try_from(Value::from(vec!["file:///m/a%20b.lrc"])).unwrap(),
        );
        assert_eq!(
            response_result(0, &results),
            PickResult::Picked(PathBuf::from("/m/a b.lrc"))
        );
        assert_eq!(response_result(1, &HashMap::new()), PickResult::Cancelled);
        assert!(matches!(
            response_result(0, &HashMap::new()),
            PickResult::Unavailable(_)
        ));
        assert!(matches!(
            response_result(2, &HashMap::new()),
            PickResult::Unavailable(_)
        ));
    }

    /// Asks the session's portal to open the dialog, checks it accepts the request on the
    /// expected handle, then closes the dialog again.
    #[test]
    #[ignore = "opens a dialog through the desktop portal"]
    fn portal_accepts_and_closes_request() {
        let conn = connection::Builder::session()
            .unwrap()
            .method_timeout(CALL_TIMEOUT)
            .build()
            .unwrap();
        let unique = conn.unique_name().unwrap().to_string();
        let (handle, _responses) =
            start_open_file(&conn, "Encore portal test", &owned_filters(&LYRICS)).unwrap();
        assert!(handle
            .as_str()
            .starts_with(&request_path(&unique, "encore_")));
        assert!(close_request(&conn, &handle));
    }

    /// Drops a picker right after opening it, which closes its dialog as soon as the portal
    /// shows it.
    #[test]
    #[ignore = "opens a dialog through the desktop portal"]
    fn dropping_picker_closes_dialog() {
        drop(FilePicker::open("Encore portal drop test", &LYRICS, || {}));
        thread::sleep(CLOSE_RETRY);
    }

    /// Opens the dialog and waits for a pick, for trying the dialog by hand.
    #[test]
    #[ignore = "waits for a file to be picked in a dialog"]
    fn pick_interactively() {
        let (tx, rx) = mpsc::channel();
        let mut picker = FilePicker::open("Pick a lyrics file", &LYRICS, move || {
            let _ = tx.send(());
        });
        rx.recv().unwrap();
        let result = picker.poll().unwrap();
        println!("{result:?}");
        assert!(picker.poll().is_none());
    }
}
