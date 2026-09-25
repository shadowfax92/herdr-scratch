//! Bounded adapters for the two private tmux servers and Herdr's read-only
//! socket protocol. No subprocess here may hold up a sweep indefinitely.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};

use super::policy::{Owner, Reason};

const IO_TIMEOUT: Duration = Duration::from_millis(500);
const MAX_OUTPUT: usize = 4 * 1024 * 1024;
const FORMAT: &str = "#{pid}\t#{session_id}\t#{session_name}\t#{session_created}\t#{?session_last_attached,#{session_last_attached},0}\t#{session_activity}\t#{session_attached}\t#{@herdr_source_pane}\t#{HERDR_SOCKET_PATH}\t#{@herdr_env_version}";

#[derive(Clone, Debug)]
/// A validated observation of one Scratch-owned tmux session. The snapshot is
/// only a cleanup candidate; removal rechecks its identity and TTL use markers.
pub(super) struct Session {
    pub socket: PathBuf,
    pub server_pid: u32,
    pub id: String,
    pub name: String,
    pub created: u64,
    pub last_attached: u64,
    pub activity: u64,
    pub attached: bool,
    pub source: String,
    pub herdr_socket: PathBuf,
}

impl Session {
    pub fn key(&self) -> String {
        // A tmux session ID is scoped to one server lifetime. Including its
        // process and creation time prevents stale cache records crossing reuse.
        format!(
            "{}:{}:{}:{}",
            self.socket.display(),
            self.server_pid,
            self.id,
            self.created
        )
    }

    pub fn last_used(&self) -> u64 {
        self.created.max(self.last_attached).max(self.activity)
    }
}

#[derive(Default)]
/// Partial discovery keeps healthy servers useful without treating an unreadable
/// server as empty and discarding its cached terminal identities.
pub(super) struct SessionSnapshot {
    pub sessions: Vec<Session>,
    pub unavailable: Vec<PathBuf>,
    pub errors: Vec<String>,
}

pub(super) fn session_snapshot(state_dir: &Path) -> SessionSnapshot {
    let mut snapshot = SessionSnapshot::default();
    for socket in crate::tmux::server_socket_paths(state_dir) {
        if !socket.exists() {
            continue;
        }
        let output = match tmux(&socket, &["list-sessions", "-F", FORMAT]) {
            Ok(output) => output,
            Err(error) => {
                // One private server may have crashed while the other is
                // healthy. Keep its cached identities and continue the sweep.
                snapshot
                    .errors
                    .push(format!("{}: {error:#}", socket.display()));
                snapshot.unavailable.push(socket);
                continue;
            }
        };
        for line in output.lines() {
            if let Some(session) = parse_session(&socket, line) {
                snapshot.sessions.push(session);
            }
        }
    }
    snapshot
}

#[cfg(test)]
pub(super) fn sessions(state_dir: &Path) -> Result<Vec<Session>> {
    let snapshot = session_snapshot(state_dir);
    if !snapshot.errors.is_empty() {
        bail!("{}", snapshot.errors.join("; "))
    }
    Ok(snapshot.sessions)
}

fn parse_session(socket: &Path, line: &str) -> Option<Session> {
    let f: Vec<_> = line.split('\t').collect();
    if f.len() != 10
        || f[9] != "1"
        || !f[1].starts_with('$')
        || !f[1][1..].chars().all(|c| c.is_ascii_digit())
        || f[1].len() < 2
        || !Path::new(f[8]).is_absolute()
    {
        return None;
    }
    let name: Vec<_> = f[2].split('/').collect();
    if name.len() != 4
        || name[0] != "hs"
        || name[1].is_empty()
        || !name[1]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        || f[7].is_empty()
        || f[7].chars().any(|c| !c.is_ascii_alphanumeric() && c != ':')
        || crate::tmux::session_name(name[1], f[7], f[8]) != f[2]
    {
        return None;
    }
    Some(Session {
        socket: socket.to_path_buf(),
        server_pid: f[0].parse().ok()?,
        id: f[1].into(),
        name: f[2].into(),
        created: f[3].parse().ok()?,
        last_attached: if f[4].is_empty() {
            0
        } else {
            f[4].parse().ok()?
        },
        activity: f[5].parse().ok()?,
        attached: f[6].parse::<u32>().ok()? > 0,
        source: f[7].into(),
        herdr_socket: f[8].into(),
    })
}

#[derive(Debug, Deserialize)]
pub(super) struct Pane {
    pub pane_id: String,
    pub terminal_id: String,
}

/// A missing or malformed snapshot is uncertainty, never a list of zero panes.
pub(super) fn panes(socket: &Path) -> Result<HashMap<String, String>> {
    let reply = herdr(socket, "pane.list", json!({}))?;
    let values = reply
        .pointer("/result/panes")
        .context("Herdr pane list missing")?;
    let panes: Vec<Pane> = serde_json::from_value(values.clone())?;
    if panes
        .iter()
        .any(|p| p.pane_id.is_empty() || p.terminal_id.is_empty())
    {
        bail!("Herdr pane identity missing");
    }
    Ok(panes
        .into_iter()
        .map(|p| (p.pane_id, p.terminal_id))
        .collect())
}

pub(super) fn resolve_owner(
    session: &Session,
    known_terminal: Option<&str>,
    snapshot: Option<&HashMap<String, String>>,
) -> (Owner, Option<String>) {
    let Some(snapshot) = snapshot else {
        return (Owner::Unknown, None);
    };
    if let Some(terminal) = known_terminal {
        // Terminal identity survives workspace moves; public pane IDs do not.
        let owner = if snapshot.values().any(|id| id == terminal) {
            Owner::Live
        } else {
            Owner::Closed
        };
        return (owner, Some(terminal.to_owned()));
    }
    if let Some(terminal) = snapshot.get(&session.source) {
        return (Owner::Live, Some(terminal.clone()));
    }
    // Older sessions have only inherited pane IDs. Ask Herdr to resolve that
    // caller context, including move aliases, before declaring an owner dead.
    match herdr(
        &session.herdr_socket,
        "pane.current",
        json!({"caller_pane_id":session.source}),
    ) {
        Ok(reply)
            if reply.pointer("/error/code").and_then(Value::as_str) == Some("pane_not_found") =>
        {
            (Owner::Closed, None)
        }
        Ok(reply) => match reply
            .pointer("/result/pane")
            .cloned()
            .and_then(|v| serde_json::from_value::<Pane>(v).ok())
        {
            Some(pane) if !pane.terminal_id.is_empty() => (Owner::Live, Some(pane.terminal_id)),
            _ => (Owner::Unknown, None),
        },
        Err(_) => (Owner::Unknown, None),
    }
}

pub(super) fn remove(session: &Session, reason: Reason) -> Result<bool> {
    // The final attachment/use/identity checks run in tmux's command queue,
    // rather than trusting the earlier client-side snapshot. No shell is run.
    // Fields interpolated below are validated identifiers or integers.
    let mut guards = vec![
        format!("#{{==:#{{pid}},{}}}", session.server_pid),
        format!("#{{==:#{{session_created}},{}}}", session.created),
        format!("#{{==:#{{session_name}},{}}}", session.name),
        format!("#{{==:#{{@herdr_source_pane}},{}}}", session.source),
    ];
    if reason == Reason::Expired {
        guards.extend([
            "#{==:#{session_attached},0}".into(),
            format!(
                "#{{==:#{{?session_last_attached,#{{session_last_attached}},0}},{}}}",
                session.last_attached
            ),
            format!("#{{==:#{{session_activity}},{}}}", session.activity),
        ]);
    }
    let filter = guards
        .into_iter()
        .reduce(|a, b| format!("#{{&&:{a},{b}}}"))
        .context("missing cleanup guard")?;
    tmux(
        &session.socket,
        &[
            "if-shell",
            "-F",
            "-t",
            &session.id,
            &filter,
            &format!("kill-session -t {}", session.id),
        ],
    )?;
    // Killing a session can stop the otherwise empty private server.
    if !session.socket.exists() {
        return Ok(true);
    }
    match tmux(&session.socket, &["list-sessions", "-F", "#{session_id}"]) {
        Ok(output) => Ok(!output.lines().any(|id| id == session.id)),
        Err(_) if !session.socket.exists() => Ok(true),
        Err(error) => Err(error),
    }
}

fn tmux(socket: &Path, args: &[&str]) -> Result<String> {
    bounded_output(
        Command::new("tmux")
            .arg("-S")
            .arg(socket)
            .args(args)
            .env_remove("TMUX")
            .env_remove("TMUX_PANE"),
    )
}

fn herdr(socket: &Path, method: &str, params: Value) -> Result<Value> {
    let deadline = Instant::now() + IO_TIMEOUT;
    let mut stream = connect(socket, deadline)?;
    let request = format!(
        "{}\n",
        json!({"id":"scratch-cleanup","method":method,"params":params})
    );
    let mut remaining = request.as_bytes();
    while !remaining.is_empty() {
        if Instant::now() >= deadline {
            bail!("Herdr cleanup request timed out")
        }
        match stream.write(remaining) {
            Ok(0) => bail!("Herdr closed while writing request"),
            Ok(n) => remaining = &remaining[n..],
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                wait_io(&stream, libc::POLLOUT, deadline)?
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        }
    }
    let mut line = Vec::new();
    let mut bytes = [0; 8192];
    loop {
        if Instant::now() >= deadline {
            bail!("Herdr cleanup request timed out")
        }
        match stream.read(&mut bytes) {
            Ok(0) => bail!("incomplete Herdr reply"),
            Ok(n) => {
                line.extend_from_slice(&bytes[..n]);
                if line.len() > MAX_OUTPUT {
                    bail!("oversized Herdr reply")
                }
                if let Some(end) = line.iter().position(|b| *b == b'\n') {
                    line.truncate(end);
                    break;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                wait_io(&stream, libc::POLLIN, deadline)?
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        }
    }
    let value: Value = serde_json::from_slice(&line)?;
    if value.get("id").and_then(Value::as_str) != Some("scratch-cleanup") {
        bail!("Herdr response ID mismatch")
    }
    Ok(value)
}

/// std::UnixStream::connect can block on a full listen queue. Use an owned
/// nonblocking socket so the same total deadline covers connect, write and read.
fn connect(path: &Path, deadline: Instant) -> Result<UnixStream> {
    // SAFETY: sockaddr_un is a C plain-data address, filled before connect.
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    let bytes = path.as_os_str().as_bytes();
    if bytes.is_empty() || bytes.contains(&0) || bytes.len() >= address.sun_path.len() {
        bail!("invalid Herdr socket path");
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (destination, source) in address.sun_path.iter_mut().zip(bytes) {
        *destination = *source as libc::c_char
    }
    #[cfg(target_os = "macos")]
    {
        address.sun_len = std::mem::size_of_val(&address) as u8;
    }
    loop {
        if Instant::now() >= deadline {
            bail!("Herdr cleanup connect timed out")
        }
        // SAFETY: socket returns a new descriptor, immediately transferred into
        // UnixStream so all failures/retries close it without leaking handles.
        let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let stream = unsafe { UnixStream::from_raw_fd(fd) };
        // SAFETY: this owned descriptor must not escape into tmux subprocesses.
        if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        stream.set_nonblocking(true)?;
        // SAFETY: address is initialized, correctly aligned and lives for the call.
        let result = unsafe {
            libc::connect(
                fd,
                (&address as *const libc::sockaddr_un).cast(),
                std::mem::size_of_val(&address) as libc::socklen_t,
            )
        };
        if result == 0 {
            return Ok(stream);
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::EINPROGRESS) {
            wait_io(&stream, libc::POLLOUT, deadline)?;
            if let Some(error) = stream.take_error()? {
                return Err(error.into());
            }
            stream
                .peer_addr()
                .context("Herdr connect did not complete")?;
            return Ok(stream);
        }
        if error.kind() != std::io::ErrorKind::WouldBlock
            && error.kind() != std::io::ErrorKind::Interrupted
        {
            return Err(error.into());
        }
        // Linux reports a saturated Unix listen queue as EAGAIN. Retrying uses
        // a fresh socket because failed connect state is not portable.
        drop(stream);
        thread::sleep(Duration::from_millis(5));
    }
}

fn wait_io(stream: &UnixStream, events: libc::c_short, deadline: Instant) -> Result<()> {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            bail!("Herdr cleanup request timed out")
        }
        let mut descriptor = libc::pollfd {
            fd: stream.as_raw_fd(),
            events,
            revents: 0,
        };
        // SAFETY: poll receives one live descriptor and a bounded timeout.
        let result = unsafe {
            libc::poll(
                &mut descriptor,
                1,
                remaining.as_millis().clamp(1, i32::MAX as u128) as i32,
            )
        };
        if result > 0 {
            return Ok(());
        }
        if result < 0 && std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return Err(std::io::Error::last_os_error().into());
        }
    }
}

/// Drain both pipes without blocking; even a stuck tmux socket/client cannot
/// monopolize the worker or prevent stop requests being serviced next sweep.
pub(super) fn bounded_output(command: &mut Command) -> Result<String> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let result = (|| {
        let mut stdout = child.stdout.take().context("child stdout missing")?;
        let mut stderr = child.stderr.take().context("child stderr missing")?;
        for fd in [stdout.as_raw_fd(), stderr.as_raw_fd()] {
            // SAFETY: fcntl changes flags only on these owned pipe descriptors.
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
            {
                return Err(std::io::Error::last_os_error().into());
            }
        }
        let deadline = Instant::now() + IO_TIMEOUT;
        let (mut out, mut err) = (Vec::new(), Vec::new());
        loop {
            drain(&mut stdout, &mut out)?;
            drain(&mut stderr, &mut err)?;
            if let Some(status) = child.try_wait()? {
                drain(&mut stdout, &mut out)?;
                drain(&mut stderr, &mut err)?;
                if !status.success() {
                    bail!(
                        "tmux request failed: {}",
                        String::from_utf8_lossy(&err).trim()
                    )
                }
                return String::from_utf8(out).context("tmux response is not UTF-8");
            }
            if Instant::now() >= deadline {
                bail!("cleanup command timed out")
            }
            thread::sleep(Duration::from_millis(5));
        }
    })();
    if result.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    result
}

fn drain(reader: &mut impl Read, output: &mut Vec<u8>) -> Result<()> {
    let mut bytes = [0; 8192];
    loop {
        match reader.read(&mut bytes) {
            Ok(0) => return Ok(()),
            Ok(n) => {
                output.extend_from_slice(&bytes[..n]);
                if output.len() > MAX_OUTPUT {
                    bail!("cleanup output exceeds bound")
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    struct ListenerFixture {
        listener: UnixListener,
        path: PathBuf,
    }

    impl ListenerFixture {
        fn new(name: &str) -> Self {
            let path = PathBuf::from(format!("/tmp/sc-{}-{name}.sock", std::process::id()));
            let listener = UnixListener::bind(&path).unwrap();
            Self { listener, path }
        }
    }

    impl Drop for ListenerFixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    #[test]
    fn full_herdr_listen_queue_respects_connect_deadline() {
        let fixture = ListenerFixture::new("connect");
        // Bound the queue without accepting; a blocking connect would hang
        // once it fills even if read/write timeouts had already been set.
        assert_eq!(unsafe { libc::listen(fixture.listener.as_raw_fd(), 1) }, 0);
        let mut queued = Vec::new();
        for _ in 0..128 {
            match connect(&fixture.path, Instant::now() + Duration::from_millis(50)) {
                Ok(stream) => queued.push(stream),
                Err(error) => {
                    // macOS rejects a saturated queue immediately; Linux may
                    // leave the connect pending until our deadline instead.
                    let refused = error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|error| error.kind() == std::io::ErrorKind::ConnectionRefused);
                    assert!(
                        refused || error.to_string().contains("timed out"),
                        "{error:#}"
                    );
                    let start = Instant::now();
                    assert!(herdr(&fixture.path, "pane.list", json!({})).is_err());
                    assert!(start.elapsed() < Duration::from_secs(2));
                    return;
                }
            }
        }
        panic!("test listener queue did not fill");
    }

    #[test]
    fn silent_herdr_server_respects_total_request_deadline() {
        let fixture = ListenerFixture::new("read");
        let start = Instant::now();
        let error = herdr(&fixture.path, "pane.list", json!({})).unwrap_err();
        assert!(error.to_string().contains("timed out"), "{error:#}");
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn foreign_or_malformed_tmux_sessions_are_not_owned() {
        let name = crate::tmux::session_name("shell", "w1:p1", "/tmp/herdr.sock");
        let line = format!("10\t$2\t{name}\t1\t2\t3\t0\tw1:p1\t/tmp/herdr.sock\t1");
        assert!(parse_session(Path::new("/tmp/tmux"), &line).is_some());
        assert!(parse_session(Path::new("/tmp/tmux"), &line.replace(&name, "foreign")).is_none());
        assert!(parse_session(
            Path::new("/tmp/tmux"),
            &line.replace("/tmp/herdr.sock", "/tmp/other.sock")
        )
        .is_none());
    }

    #[test]
    fn terminal_identity_survives_pane_move_and_detects_id_reuse() {
        let name = crate::tmux::session_name("shell", "w1:p1", "/tmp/herdr.sock");
        let session = parse_session(
            Path::new("/tmp/tmux"),
            &format!("10\t$2\t{name}\t1\t2\t3\t0\tw1:p1\t/tmp/herdr.sock\t1"),
        )
        .unwrap();
        let moved = HashMap::from([("w2:p8".into(), "term1".into())]);
        assert_eq!(
            resolve_owner(&session, Some("term1"), Some(&moved)).0,
            Owner::Live
        );
        let reused = HashMap::from([("w1:p1".into(), "term2".into())]);
        assert_eq!(
            resolve_owner(&session, Some("term1"), Some(&reused)).0,
            Owner::Closed
        );
        assert_eq!(
            resolve_owner(&session, Some("term1"), None).0,
            Owner::Unknown
        );
    }

    #[test]
    fn hung_command_is_bounded_and_reaped() {
        let start = Instant::now();
        let error = bounded_output(Command::new("sleep").arg("10")).unwrap_err();
        assert!(error.to_string().contains("timed out"));
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
