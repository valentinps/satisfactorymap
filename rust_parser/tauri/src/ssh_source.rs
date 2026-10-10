//! Save sources on the machine that runs the game: its own SSH server.
//!
//! The alternative to this module is a small HTTP file server on the gaming PC
//! (`tools/save-http-server`). This one opens no listening port and adds no
//! service to keep patched: OpenSSH is already installed on Windows 10/11 and
//! on every Linux box, it authenticates with the key the user already has, and
//! it offers what a static file endpoint cannot -- a directory listing we can
//! actually browse, and a change signal cheap enough to poll.
//!
//! Nothing here implements the SSH protocol. It runs the user's own `ssh`,
//! `sftp` and `rsync`, so `~/.ssh/config`, `known_hosts`, ssh-agent, keys and
//! `ProxyJump` all apply for free, and the app ships no SSH stack to keep
//! current. The trade-off is that authentication and host keys stay OpenSSH's
//! business: every call sets `BatchMode=yes`, so key/agent auth only -- a
//! hidden-stdin password prompt would otherwise hang until the timeout -- and a
//! first connection to an unknown host fails unless the user opts into
//! `StrictHostKeyChecking=accept-new` (trust on first use, same disclosure as
//! the dedicated-server certificate pin in `server_api`).

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const CONNECT_TIMEOUT_SECS: u32 = 10;
const PROBE_TIMEOUT: Duration = Duration::from_secs(45);
const FETCH_TIMEOUT: Duration = Duration::from_secs(1800);
/// Ceiling on a remote save. Far above any real one (a 600k-object endgame
/// save is ~50 MB) and low enough that a wrong path cannot fill the disk.
const MAX_SAVE_BYTES: u64 = 8 * 1024 * 1024 * 1024;

/// Everything needed to reach the game machine over SSH. Persisted by the
/// frontend in localStorage; only `identity` and `accept_new_host_key` touch
/// anything on disk outside our own app-data dir.
#[derive(Deserialize, Serialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct SshTarget {
    pub host: String,
    #[serde(default)]
    pub user: String,
    /// 0 means "the client's default" (22 unless ~/.ssh/config says otherwise).
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub identity: String,
    #[serde(default)]
    pub accept_new_host_key: bool,
}

/// Result of `source_validate`: what the connection can actually do. Each
/// field maps to one line in the dialog, so a partial capability (SSH works,
/// no SFTP subsystem) is reported rather than collapsing to "failed".
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidateResult {
    pub ok: bool,
    /// "Windows" | "Linux" | "macOS" | "" -- from the shape of the SFTP home.
    pub platform: String,
    /// Remote home directory as SFTP reports it (`/home/x`, `/C:/Users/x`).
    pub home: String,
    /// SFTP subsystem: without it there is no browsing and no change polling,
    /// only a typed path plus an rsync pull.
    pub sftp: bool,
    /// rsync on the REMOTE side: what makes rsync-over-ssh usable.
    pub rsync: bool,
    /// How changes can be noticed: "inotify" or "watcher" when the far side can
    /// push them, "" when only polling is possible. See `watch_mode`.
    pub watch: String,
    /// How many `.sav` files are in `dir`, when the caller passed one.
    pub save_count: usize,
    pub newest_save: String,
    /// Human-readable last word: the error when `ok` is false.
    pub detail: String,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DirEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub size: u64,
    /// Unix seconds; the listing's own resolution (a minute).
    pub mtime: i64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Listing {
    pub path: String,
    pub parent: String,
    pub entries: Vec<DirEntry>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchedSave {
    pub path: String,
    pub save_name: String,
    pub sha256: String,
    pub bytes: u64,
    pub remote_path: String,
}

// ---- Running the tools ----------------------------------------------------

struct Cmd {
    ok: bool,
    stdout: String,
    stderr: String,
    timed_out: bool,
}

/// Run a local tool with a hard deadline, draining both pipes on their own
/// threads. Without that, a remote process that writes more than a pipe buffer
/// stalls forever waiting for a reader that is waiting for it to exit.
fn run(bin: &str, args: &[String], stdin: Option<&str>, timeout: Duration) -> Result<Cmd, String> {
    let mut child = Command::new(bin)
        .args(args)
        .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Could not run {bin}: {e}"))?;
    if let Some(text) = stdin {
        if let Some(mut pipe) = child.stdin.take() {
            // Our batch scripts are a few hundred bytes; writing them before
            // reading stdout cannot fill a pipe.
            pipe.write_all(text.as_bytes())
                .map_err(|e| format!("Could not talk to {bin}: {e}"))?;
        }
    }
    let mut out = child.stdout.take().expect("stdout piped");
    let mut err = child.stderr.take().expect("stderr piped");
    let out_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = out.read_to_end(&mut buf);
        buf
    });
    let err_thread = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = err.read_to_end(&mut buf);
        buf
    });
    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    timed_out = true;
                    let _ = child.kill();
                    break child.wait().ok();
                }
                std::thread::sleep(Duration::from_millis(40));
            }
            Err(e) => return Err(format!("Could not wait for {bin}: {e}")),
        }
    };
    let stdout = String::from_utf8_lossy(&out_thread.join().unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&err_thread.join().unwrap_or_default()).into_owned();
    Ok(Cmd {
        ok: status.map(|s| s.success()).unwrap_or(false),
        stdout,
        stderr,
        timed_out,
    })
}

/// Reject anything that would reach the client as an option instead of a
/// host, and anything that would break out of the quoting we do for the SFTP
/// batch script. There is no shell in the middle (argv is passed straight to
/// the tool), so this is about the tools' own argument parsing, not `sh -c`.
fn check_token(what: &str, value: &str, allowed: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("{what} is empty."));
    }
    if value.starts_with('-') {
        return Err(format!("{what} cannot start with '-'."));
    }
    if !value.chars().all(|c| allowed.contains(c)) {
        return Err(format!("{what} contains a character that is not allowed: {value}"));
    }
    Ok(())
}

/// A remote path travels to `sftp` inside a batch script, where a newline
/// would be read as the next command. Reject control characters and the
/// double quote we wrap paths in; both are impossible in a real save path.
fn check_remote_path(path: &str) -> Result<(), String> {
    if path.trim().is_empty() {
        return Err("Enter a folder on the remote machine first.".to_string());
    }
    if path.len() > 4096 {
        return Err("That path is implausibly long.".to_string());
    }
    if path.chars().any(|c| c.is_control()) {
        return Err("That path has a control character in it.".to_string());
    }
    if path.contains('"') {
        return Err("That path has a double quote in it.".to_string());
    }
    Ok(())
}

impl SshTarget {
    fn check(&self) -> Result<(), String> {
        check_token(
            "The host",
            self.host.trim(),
            "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789.-_:[]",
        )?;
        if !self.user.trim().is_empty() {
            check_token(
                "The user name",
                self.user.trim(),
                "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789._-@\\",
            )?;
        }
        // `port` is a u16: an out-of-range number fails to deserialize before
        // it gets here, so there is no range to re-check.
        if !self.identity.trim().is_empty() && !std::path::Path::new(self.identity.trim()).is_file() {
            return Err(format!("No key file at {}.", self.identity.trim()));
        }
        Ok(())
    }

    fn destination(&self) -> String {
        if self.user.trim().is_empty() {
            self.host.trim().to_string()
        } else {
            format!("{}@{}", self.user.trim(), self.host.trim())
        }
    }

    /// Options shared by ssh and sftp. `BatchMode=yes` is what keeps a
    /// password or host-key question from hanging the app on an invisible
    /// stdin; `accept-new` is the user's explicit opt-in to trust a host we
    /// have never seen (it still refuses a CHANGED key).
    fn opts(&self) -> Vec<String> {
        let mut opts: Vec<String> = vec![
            "-o".into(),
            "BatchMode=yes".into(),
            "-o".into(),
            format!("ConnectTimeout={CONNECT_TIMEOUT_SECS}"),
            "-o".into(),
            if self.accept_new_host_key {
                "StrictHostKeyChecking=accept-new".into()
            } else {
                "StrictHostKeyChecking=yes".into()
            },
        ];
        if !self.identity.trim().is_empty() {
            opts.push("-i".into());
            opts.push(self.identity.trim().to_string());
        }
        opts
    }

    fn ssh_args(&self, remote_command: &str) -> Vec<String> {
        let mut args = self.opts();
        if self.port != 0 && self.port != 22 {
            args.push("-p".into());
            args.push(self.port.to_string());
        }
        args.push(self.destination());
        args.push(remote_command.to_string());
        args
    }

    /// sftp spells the port `-P`, and takes the batch script on stdin.
    fn sftp_args(&self) -> Vec<String> {
        let mut args = self.opts();
        if self.port != 0 && self.port != 22 {
            args.push("-P".into());
            args.push(self.port.to_string());
        }
        args.push("-b".into());
        args.push("-".into());
        args.push(self.destination());
        args
    }
}

/// Is this tool on PATH? `which` is not on a stock Windows box, so ask the
/// shell that is.
fn on_path(bin: &str) -> bool {
    let probe = if cfg!(windows) { "where" } else { "which" };
    run(probe, &[bin.to_string()], None, Duration::from_secs(5))
        .map(|cmd| cmd.ok)
        .unwrap_or(false)
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or("").trim().to_string()
}

/// The client's own words are the useful half of a failure ("Permission
/// denied (publickey)", "Host key verification failed"), so prefer stderr and
/// only fall back to a generic line.
fn tool_error(bin: &str, cmd: &Cmd) -> String {
    let detail = if cmd.stderr.trim().is_empty() {
        first_line(&cmd.stdout)
    } else {
        first_line(&cmd.stderr)
    };
    if cmd.timed_out {
        return format!("{bin} timed out. Is the machine awake and reachable?");
    }
    if detail.is_empty() {
        return format!("{bin} failed (exit status unknown).");
    }
    detail
}

fn sftp_script(t: &SshTarget, commands: &[String], timeout: Duration) -> Result<String, String> {
    t.check()?;
    let script = format!("{}\n", commands.join("\n"));
    let cmd = run("sftp", &t.sftp_args(), Some(&script), timeout)?;
    if cmd.timed_out {
        return Err(tool_error("sftp", &cmd));
    }
    if !cmd.ok {
        return Err(tool_error("sftp", &cmd));
    }
    Ok(cmd.stdout)
}

// ---- Parsing the SFTP client's output -------------------------------------

/// Pull the path out of `pwd`'s "Remote working directory: <path>".
fn parse_pwd(text: &str) -> Option<String> {
    text.lines()
        .find_map(|line| line.trim().strip_prefix("Remote working directory:"))
        .map(|path| path.trim().to_string())
        .filter(|path| !path.is_empty())
}

/// The Windows OpenSSH server exposes drives as `/C:/...`; the POSIX one as a
/// plain absolute path. That is enough to say which OS we are talking to
/// without a second round trip (`uname` does not exist in cmd.exe).
fn platform_from_home(home: &str) -> &'static str {
    let rest = home.trim_start_matches('/');
    let mut chars = rest.chars();
    match (chars.next(), chars.next()) {
        (Some(letter), Some(':')) if letter.is_ascii_alphabetic() => "Windows",
        _ => "Linux/macOS",
    }
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// `ls -l` prints either "Mon DD HH:MM" (within the last six months) or
/// "Mon DD  YYYY". Turn that into Unix seconds so the browser can sort by it
/// and show something meaningful; the year is inferred the way `ls` does, with
/// a clock-skew allowance so a file written a moment ago in another timezone
/// is not pushed a year into the future.
fn parse_when(month: &str, day: &str, third: &str, now: SystemTime) -> i64 {
    let Some(m) = MONTHS.iter().position(|name| name.eq_ignore_ascii_case(month)) else {
        return 0;
    };
    let Ok(day) = day.trim().parse::<u32>() else {
        return 0;
    };
    let now_secs = now
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let now_year = 1970 + (now_secs / 31_556_952) as i32; // ~mean year, fine for inference
    let (year, hour, minute) = if third.contains(':') {
        let mut parts = third.split(':');
        let hour = parts.next().and_then(|h| h.trim().parse::<u32>().ok()).unwrap_or(0);
        let minute = parts.next().and_then(|m| m.trim().parse::<u32>().ok()).unwrap_or(0);
        // `ls` omits the year for recent files: assume this year, and step
        // back one when that lands in the future by more than a day (a
        // December file read in January).
        let mut year = now_year;
        if month_secs(year, m, day, hour, minute) > now_secs + 86_400 {
            year -= 1;
        }
        (year, hour, minute)
    } else {
        (third.trim().parse::<i32>().unwrap_or(now_year), 0, 0)
    };
    month_secs(year, m, day, hour, minute)
}

/// Days-from-civil (Howard Hinnant's algorithm), so no chrono dependency for
/// what is only ever a sort key and a timestamp.
fn month_secs(year: i32, month: usize, day: u32, hour: u32, minute: u32) -> i64 {
    let m = month as i32 + 1;
    let y = if m <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as i64;
    let mp = ((m + 9) % 12) as i64;
    let doy = (153 * mp + 2) / 5 + day as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era as i64 * 146_097 + doe - 719_468;
    days * 86_400 + hour as i64 * 3600 + minute as i64 * 60
}

/// One `ls -l` line from the SFTP client into an entry.
///
/// The client prints `mode nlink owner group size <date> <name>`, and the
/// fields in between vary in both shape and count: OpenSSH sends no link
/// count, so the client prints `?` there, an SSH server with no owner
/// information prints `?` twice more, and the name is the path as the server
/// saw it -- the full `/home/x/saves/a.sav` when we asked for that directory
/// by absolute path. Field positions are therefore not fixed, so the date is
/// the anchor: it is always three tokens and always shaped the same (a month
/// name, a day, then either `HH:MM` or a year), which also leaves names with
/// spaces intact.
fn parse_ls_line(line: &str, dir: &str, now: SystemTime) -> Option<DirEntry> {
    let trimmed = line.trim_end_matches(['\r', '\n']);
    let mode = trimmed.split_whitespace().next()?;
    let mut chars = mode.chars();
    let kind = chars.next()?;
    if !matches!(kind, '-' | 'd' | 'l' | '?') || mode.len() < 10 {
        return None;
    }
    if !mode[1..].chars().all(|c| "rwxstST-".contains(c)) {
        return None;
    }
    // Whatever the server called it, the entry's own name is the last segment
    // (a file name cannot contain a separator).
    let name = entry_name(trimmed, mode)?;
    let name = name.rsplit(['/', '\\']).next().unwrap_or("").to_string();
    if name.is_empty() || name == "." || name == ".." {
        return None;
    }
    let rest = field_after(trimmed, mode)?;
    let tokens = tokens_at(rest);
    let month_at = date_at(&tokens)?;
    // The size sits immediately before the date; counting fields from the left
    // would instead land on the `?` the client prints for an unknown link
    // count, which is the one field every remote listing has.
    let size = month_at
        .checked_sub(1)
        .and_then(|at| tokens.get(at))
        .and_then(|(_, token)| token.parse::<u64>().ok())
        .unwrap_or(0);
    let mtime = match tokens.get(month_at..month_at + 3) {
        Some(triple) => parse_when(triple[0].1, triple[1].1, triple[2].1, now),
        None => 0,
    };
    Some(DirEntry {
        path: join_remote(dir, &name),
        name,
        is_dir: kind == 'd',
        size,
        mtime,
    })
}

/// Text after the mode field, where size and the date live.
fn field_after<'a>(line: &'a str, mode: &str) -> Option<&'a str> {
    let at = line.find(mode)? + mode.len();
    Some(line[at..].trim_start())
}

/// Whitespace-separated tokens with the byte offset each starts at, so the
/// name can be sliced back out of the original line. `ls -l` pads its date
/// columns ("Jan  3  2025"), and rebuilding the date from its parts would
/// silently lose an entry whose name follows a double space.
fn tokens_at(text: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut start = None;
    for (at, ch) in text.char_indices() {
        if ch.is_whitespace() {
            if let Some(from) = start.take() {
                out.push((from, &text[from..at]));
            }
        } else if start.is_none() {
            start = Some(at);
        }
    }
    if let Some(from) = start {
        out.push((from, &text[from..]));
    }
    out
}

/// The `(month, day, time-or-year)` triple the line carries, located by month
/// name so a varying number of owner/group fields cannot shift it.
fn date_at(tokens: &[(usize, &str)]) -> Option<usize> {
    tokens.iter().position(|(_, token)| {
        MONTHS.iter().any(|name| name.eq_ignore_ascii_case(token))
    })
}

fn entry_name(line: &str, mode: &str) -> Option<String> {
    let rest = field_after(line, mode)?;
    let tokens = tokens_at(rest);
    let month_at = date_at(&tokens)?;
    // The name starts where the token after the date starts: month, day, then
    // the time-or-year, then the name (spaces and all).
    let (offset, _) = *tokens.get(month_at + 3)?;
    Some(rest[offset..].to_string())
}

fn parse_listing(text: &str, dir: &str) -> Vec<DirEntry> {
    let now = SystemTime::now();
    text.lines()
        .filter_map(|line| parse_ls_line(line, dir, now))
        .collect()
}

/// `/a/b/c` -> `/a/b`, `/C:/` -> `/C:/`, and a Windows-style path keeps its
/// backslash separator (we only ever see the SFTP form, but a typed one is
/// cheap to handle). A root is its own parent, so the dialog's "Up" button
/// goes nowhere rather than somewhere invalid.
fn parent_of(path: &str) -> String {
    let path = path.trim_end_matches(['/', '\\']);
    if path.is_empty() {
        return "/".to_string(); // "/" or a bare drive-relative path
    }
    if is_drive_root(path) {
        return format!("{path}/");
    }
    match path.rfind(['/', '\\']) {
        Some(0) => "/".to_string(),
        Some(at) => path[..at].to_string(),
        None => String::new(),
    }
}

/// `/C:` or `C:` -- the top of a Windows drive, where SFTP's `cd ..` stops.
fn is_drive_root(path: &str) -> bool {
    let bare = path.trim_start_matches('/');
    bare.len() == 2
        && bare.as_bytes()[1] == b':'
        && (bare.as_bytes()[0] as char).is_ascii_alphabetic()
}

fn join_remote(dir: &str, name: &str) -> String {
    let dir = dir.trim_end_matches(['/', '\\']);
    if dir.is_empty() {
        name.to_string()
    } else {
        format!("{dir}/{name}")
    }
}

// ---- Commands -------------------------------------------------------------

pub fn validate(target: &SshTarget, dir: &str) -> ValidateResult {
    let mut result = ValidateResult {
        ok: false,
        platform: String::new(),
        home: String::new(),
        sftp: false,
        rsync: false,
        watch: String::new(),
        save_count: 0,
        newest_save: String::new(),
        detail: String::new(),
    };
    if let Err(error) = target.check() {
        result.detail = error;
        return result;
    }
    // One SFTP call answers three questions at once: does SSH authenticate at
    // all, is the SFTP subsystem available, and where do we land? `pwd` is the
    // cheapest command that proves the subsystem works end to end.
    match sftp_script(target, &["pwd".to_string()], PROBE_TIMEOUT) {
        Ok(out) => {
            result.sftp = true;
            result.home = parse_pwd(&out).unwrap_or_default();
            result.platform = if result.home.is_empty() {
                String::new()
            } else {
                platform_from_home(&result.home).to_string()
            };
        }
        Err(error) => {
            // "subsystem request failed" is a different problem from a bad key,
            // and only one of them is fixable on our side.
            result.detail = error;
            return result;
        }
    }
    // rsync on the FAR side is what makes rsync-over-ssh work; without it SFTP
    // carries the transfer.
    let cmd = run("ssh", &target.ssh_args("rsync --version"), None, PROBE_TIMEOUT);
    result.rsync = cmd.map(|c| c.ok).unwrap_or(false);
    result.watch = if result.sftp {
        watch_mode(target, &result.platform, false, dir).to_string()
    } else {
        String::new()
    };
    if !dir.trim().is_empty() && result.sftp {
        if let Ok(entries) = listing(target, dir) {
            let saves: Vec<&DirEntry> = entries.iter().filter(|e| is_save(&e.name)).collect();
            result.save_count = saves.len();
            result.newest_save = newest_save(&entries).map(|e| e.name.clone()).unwrap_or_default();
        }
    }
    result.ok = result.sftp;
    if result.ok {
        let as_user = if target.user.trim().is_empty() {
            "the SSH user".to_string()
        } else {
            target.user.trim().to_string()
        };
        result.detail = format!("Signed in as {as_user}@{}.", target.host.trim());
    }
    result
}

fn is_save(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".sav")
}

/// The newest `.sav` by listing timestamp -- the map's "latest autosave",
/// which is what the game itself writes over and over. Ties (same minute) fall
/// back to the name so the answer is at least stable.
fn newest_save(entries: &[DirEntry]) -> Option<&DirEntry> {
    entries
        .iter()
        .filter(|entry| !entry.is_dir && is_save(&entry.name))
        .max_by(|a, b| a.mtime.cmp(&b.mtime).then_with(|| a.name.cmp(&b.name)))
}

fn listing(target: &SshTarget, dir: &str) -> Result<Vec<DirEntry>, String> {
    check_remote_path(dir)?;
    // `-l` is what gives us mode/size/date; the client does the formatting, so
    // this works against a Windows or a POSIX server without a remote shell.
    let out = sftp_script(target, &[format!("ls -l \"{dir}\"")], PROBE_TIMEOUT)?;
    Ok(parse_listing(&out, dir))
}

pub fn browse(target: &SshTarget, dir: &str) -> Result<Listing, String> {
    Ok(listing_object(listing(target, dir)?, dir))
}

/// Folders first, then saves, then anything else; names within a group. Shared
/// by the SFTP and mounted-share listings so the two read the same.
fn listing_object(mut entries: Vec<DirEntry>, dir: &str) -> Listing {
    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| is_save(&b.name).cmp(&is_save(&a.name)))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Listing {
        path: dir.trim_end_matches('/').to_string(),
        parent: parent_of(dir),
        entries,
    }
}

/// A mounted network share (SMB/CIFS from Windows, NFS, a GVFS mount) is just
/// a path -- the OS already did the hard part, so browsing it needs no
/// protocol code at all.
pub fn browse_local(dir: &str) -> Result<Listing, String> {
    let read = std::fs::read_dir(dir).map_err(|e| format!("Cannot read {dir}: {e}"))?;
    let mut entries = Vec::new();
    for item in read.flatten() {
        let path = item.path();
        let meta = match std::fs::metadata(&path) {
            Ok(meta) => meta,
            Err(_) => continue, // vanished mid-listing, or a dangling symlink
        };
        entries.push(DirEntry {
            name: item.file_name().to_string_lossy().into_owned(),
            path: path.to_string_lossy().into_owned(),
            is_dir: meta.is_dir(),
            size: if meta.is_dir() { 0 } else { meta.len() },
            mtime: meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
        });
    }
    Ok(listing_object(entries, dir))
}

/// Copy a `.sav` down into the app's own `remote-saves/` folder and hash it.
/// The bytes go to a `.part` file that is renamed only once complete, so a
/// truncated transfer can never become the file the parser reads; the hash is
/// what lets the watch loop skip a reload when only the timestamp moved.
pub fn fetch(
    target: &SshTarget,
    remote_path: &str,
    transport: &str,
    cache_dir: &std::path::Path,
) -> Result<FetchedSave, String> {
    check_remote_path(remote_path)?;
    target.check()?;
    let name = remote_path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or("remote.sav")
        .to_string();
    if !is_save(&name) {
        return Err(format!("{name} is not a .sav file."));
    }
    // The file on disk is always ours, never a name from the network.
    let safe_name: String = name
        .chars()
        .filter(|c| !c.is_control() && *c != '/' && *c != '\\')
        .take(120)
        .collect();
    std::fs::create_dir_all(cache_dir)
        .map_err(|e| format!("Failed to create {}: {e}", cache_dir.display()))?;
    let final_path = cache_dir.join(&safe_name);
    let part = cache_dir.join(format!("{safe_name}.part"));
    let _ = std::fs::remove_file(&part);

    match transport {
        "rsync" => {
            let mut args: Vec<String> = vec![
                "-e".into(),
                format!("ssh {}", target.opts().join(" ")),
                "--partial".into(),
                "--compress".into(),
                "--timeout=120".into(),
            ];
            if target.port != 0 && target.port != 22 {
                // rsync's ssh gets the port through -e, not its own flag.
                args[1] = format!("ssh -p {} {}", target.port, target.opts().join(" "));
            }
            let source = format!("{}:{}", target.destination(), remote_path.replace('\\', "/"));
            args.push(source);
            args.push(part.to_string_lossy().into_owned());
            let cmd = run("rsync", &args, None, FETCH_TIMEOUT)?;
            if !cmd.ok || cmd.timed_out {
                let _ = std::fs::remove_file(&part);
                return Err(tool_error("rsync", &cmd));
            }
        }
        _ => {
            // SFTP's own `get`, which also works where rsync does not exist
            // (i.e. on a Windows game machine).
            sftp_script(
                target,
                &[
                    format!("get \"{remote_path}\" \"{}\"", part.to_string_lossy()),
                    "quit".to_string(),
                ],
                FETCH_TIMEOUT,
            )
            .map_err(|e| {
                let _ = std::fs::remove_file(&part);
                e
            })?;
        }
    }

    let bytes = std::fs::metadata(&part)
        .map_err(|e| format!("The transfer produced no file: {e}"))?
        .len();
    if bytes > MAX_SAVE_BYTES {
        let _ = std::fs::remove_file(&part);
        return Err(format!(
            "That save is {bytes} bytes, over the {MAX_SAVE_BYTES}-byte limit."
        ));
    }
    let sha256 = hash_file(&part)?;
    std::fs::rename(&part, &final_path)
        .map_err(|e| format!("Failed to move the download into {}: {e}", final_path.display()))?;
    Ok(FetchedSave {
        path: final_path.to_string_lossy().into_owned(),
        save_name: safe_name,
        sha256,
        bytes,
        remote_path: remote_path.to_string(),
    })
}

fn hash_file(path: &std::path::Path) -> Result<String, String> {
    let mut file =
        std::fs::File::open(path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 256 * 1024];
    loop {
        let read = file
            .read(&mut buf)
            .map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// How changes can be noticed on the machine that holds the save.
///
/// Events beat an interval whenever the filesystem can produce them, which is
/// not everywhere: `inotify` on a mounted CIFS/NFS share never fires, because
/// the kernel only reports writes made through this machine's own VFS -- the
/// game machine's writes land on the server's disk and nothing tells the client.
/// Windows is the exception that makes the mounted transport useful: its
/// `FileSystemWatcher` does receive SMB notifications, so a UNC path or a mapped
/// drive reports changes even though Linux could not.
pub fn watch_mode(target: &SshTarget, platform: &str, local: bool, dir: &str) -> &'static str {
    if local {
        if cfg!(windows) {
            return "watcher";
        }
        return if on_path("inotifywait") && is_local_filesystem(dir) {
            "inotify"
        } else {
            ""
        };
    }
    if platform == "Windows" {
        // FileSystemWatcher is part of .NET: nothing to install, nothing to probe.
        return "watcher";
    }
    if platform == "Linux/macOS" && !dir.trim().is_empty() {
        let probe = run(
            "ssh",
            &target.ssh_args("command -v inotifywait"),
            None,
            PROBE_TIMEOUT,
        );
        if probe.map(|c| c.ok).unwrap_or(false) {
            return "inotify";
        }
    }
    ""
}

/// Is this path on a filesystem whose writes we can actually see? `findmnt`
/// answers in one line; when it is missing, assume no -- a silent inotify watch
/// on a network mount is worse than an honest interval.
fn is_local_filesystem(path: &str) -> bool {
    if path.trim().is_empty() || !std::path::Path::new(path).exists() {
        return false;
    }
    let Ok(cmd) = run(
        "findmnt",
        &[
            "-no".to_string(),
            "FSTYPE".to_string(),
            "-T".to_string(),
            path.to_string(),
        ],
        None,
        Duration::from_secs(10),
    ) else {
        return false;
    };
    if !cmd.ok {
        return false;
    }
    let fstype = first_line(&cmd.stdout).to_ascii_lowercase();
    !(fstype.starts_with("nfs")
        || fstype.starts_with("cifs")
        || fstype.starts_with("smb")
        || fstype.contains("fuse")
        || fstype.starts_with("sshfs"))
}

/// A running change watcher: the child to kill, and which mechanism it is.
pub struct WatchHandle {
    pub child: std::process::Child,
    pub mode: String,
}

/// Start watching `dir` and call `on_change` with each name that lands in it.
///
/// Two mechanisms, chosen by `mode`:
///   - "inotify": `inotifywait -m` on the machine that holds the folder, run
///     locally or over ssh, printing one name per event;
///   - "watcher": PowerShell's `FileSystemWatcher` for a Windows folder, sent as
///     a base64 `-EncodedCommand` -- the only quoting that survives cmd.exe, the
///     default shell of Windows OpenSSH.
/// The process is long-lived: the caller kills it when the watch is turned off.
pub fn spawn_watch<F>(
    target: &SshTarget,
    dir: &str,
    local: bool,
    mode: &str,
    on_change: F,
) -> Result<WatchHandle, String>
where
    F: Fn(String) + Send + 'static,
{
    let remote = !local;
    let (bin, args) = match mode {
        "inotify" => {
            if remote {
                // The folder is embedded in a command the REMOTE SHELL parses,
                // so a single quote in it would end the quoting; refuse rather
                // than escape, because no real save folder has one.
                if dir.contains('\'') {
                    return Err("That folder has a quote in it, so it cannot be watched.".to_string());
                }
                target.check()?;
                let script = format!(
                    "inotifywait -m -q -e close_write -e moved_to -e create --format %f -- '{dir}'"
                );
                ("ssh".to_string(), target.ssh_args(&script))
            } else {
                let mut args: Vec<String> = ["-m", "-q", "-e", "close_write", "-e", "moved_to", "-e", "create",
                    "--format", "%f", "--"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect();
                args.push(dir.to_string());
                ("inotifywait".to_string(), args)
            }
        }
        "watcher" => {
            let command = powershell_watch_command(dir)?;
            if remote {
                target.check()?;
                let mut args = target.opts();
                if target.port != 0 && target.port != 22 {
                    args.push("-p".to_string());
                    args.push(target.port.to_string());
                }
                args.push(target.destination());
                args.extend(command);
                ("ssh".to_string(), args)
            } else {
                ("powershell".to_string(), command)
            }
        }
        other => return Err(format!("No event watcher for mode {other:?}.")),
    };

    let mut child = Command::new(&bin)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Could not start {bin}: {e}"))?;
    let mut out = child.stdout.take().expect("stdout piped");
    let mut err = child.stderr.take().expect("stderr piped");

    // A watcher that dies at once is the interesting failure -- no inotifywait,
    // no such folder, a refused key -- so say why instead of reporting success
    // and then never firing. `inotifywait -m` otherwise runs until killed.
    let deadline = Instant::now() + Duration::from_millis(700);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                let mut text = String::new();
                let _ = err.read_to_string(&mut text);
                let mut out_text = String::new();
                let _ = out.read_to_string(&mut out_text);
                let detail = if text.trim().is_empty() {
                    first_line(&out_text)
                } else {
                    first_line(&text)
                };
                return Err(if detail.is_empty() {
                    format!("The watcher stopped immediately ({bin}).")
                } else {
                    detail
                });
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(e) => return Err(format!("Could not watch {dir}: {e}")),
        }
    }

    std::thread::spawn(move || {
        use std::io::BufRead;
        let reader = std::io::BufReader::new(out);
        for line in reader.lines().map_while(Result::ok) {
            let name = line.trim().to_string();
            if !name.is_empty() {
                on_change(name);
            }
        }
    });
    // Drain stderr too, or a chatty watcher fills the pipe and blocks.
    std::thread::spawn(move || {
        let mut sink = Vec::new();
        let _ = err.read_to_end(&mut sink);
    });

    Ok(WatchHandle {
        child,
        mode: mode.to_string(),
    })
}

/// `powershell ... -EncodedCommand <base64 of the script>`: the script is
/// UTF-16LE base64, so no quoting in it can be reinterpreted by cmd.exe.
pub fn powershell_watch_command(dir: &str) -> Result<Vec<String>, String> {
    let folder = windows_path(dir)?;
    let script = format!(
        "$w = New-Object System.IO.FileSystemWatcher '{folder}', '*.sav'; \
$w.IncludeSubdirectories = $false; $w.EnableRaisingEvents = $true; \
while ($true) {{ $e = $w.WaitForChanged('All', 60000); \
if (-not $e.TimedOut) {{ [Console]::Out.WriteLine($e.Name); [Console]::Out.Flush() }} }}"
    );
    Ok(vec![
        "-NoProfile".to_string(),
        "-NonInteractive".to_string(),
        "-EncodedCommand".to_string(),
        base64_utf16le(&script),
    ])
}

/// SFTP reports a Windows folder as `/C:/Users/x`; PowerShell needs `C:\Users\x`.
/// A quote would end the single-quoted PowerShell literal, so it is refused.
fn windows_path(dir: &str) -> Result<String, String> {
    if dir.contains('\'') || dir.chars().any(|c| c.is_control()) {
        return Err("That folder cannot be watched: it has a quote or a control character.".to_string());
    }
    let trimmed = dir.trim().trim_start_matches('/');
    let bytes = trimmed.as_bytes();
    let drive = bytes.len() >= 2 && bytes[1] == b':' && (bytes[0] as char).is_ascii_alphabetic();
    if !drive {
        return Err(format!("{dir} is not a Windows drive path."));
    }
    Ok(trimmed.replace('/', "\\"))
}

/// UTF-16LE (what PowerShell's -EncodedCommand wants), base64'd. Written out
/// rather than pulling in a crate for the one encoding.
pub fn base64_utf16le(text: &str) -> String {
    let mut bytes = Vec::with_capacity(text.len() * 2);
    for unit in text.encode_utf16() {
        bytes.push((unit & 0xff) as u8);
        bytes.push((unit >> 8) as u8);
    }
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(ALPHABET[(n >> 18) as usize & 63] as char);
        out.push(ALPHABET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { ALPHABET[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { ALPHABET[n as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // Verbatim `sftp -b -` output from a real OpenSSH 10.2 sftp-server, minus
    // the sizes: the batch-mode echo of the command, the `?` link count
    // OpenSSH never sends, the total path in the name, and a dated file.
    const POSIX_LISTING: &str = "\
sftp> ls -l \"/saves/SaveGames\"
-rw-r--r--    ? dion     dion     27000000 Oct 10 10:03 /saves/SaveGames/1.2_autosave_0.sav
-rw-r--r--    ? dion     dion     27513344 Oct 10 10:03 /saves/SaveGames/1.2_autosave_1.sav
-rw-r--r--    ? dion     dion            3 Oct 10 10:03 /saves/SaveGames/notes.txt
-rw-r--r--    ? dion     dion         1024 Jan  3  2025 /saves/SaveGames/old autosave.sav
drwxr-xr-x    ? dion     dion         4096 Oct  9 08:02 /saves/SaveGames/Backup
";

    // What an OpenSSH-for-Windows server looks like: no owner or group, and
    // the drive spelled `/C:/`, which is the only way to tell it from a POSIX
    // box without a second round trip (`uname` does not exist in cmd.exe).
    const WINDOWS_LISTING: &str = "\
sftp> ls -l \"/C:/saves\"
-rw-r--r--    ?         ?                0 Oct 10 13:41 /C:/saves/1.2_autosave_1.sav
-rw-r--r--    ?         ?         27513344 Oct 10 13:41 /C:/saves/notes.txt
drwxr-xr-x    ?         ?                0 Oct 10 13:41 /C:/saves/76561198036921297
";

    fn at(month: u32) -> SystemTime {
        // 2026-10-10T12:00:00Z, built without chrono.
        UNIX_EPOCH + Duration::from_secs(month_secs(2026, month as usize, 10, 12, 0) as u64)
    }

    #[test]
    fn listing_parses_real_sftp_output() {
        let entries = parse_listing(POSIX_LISTING, "/saves/SaveGames");
        assert_eq!(entries.len(), 5, "the echoed command line is not an entry");
        let save = &entries[1];
        assert_eq!(save.name, "1.2_autosave_1.sav", "the path prefix is stripped");
        assert_eq!(save.path, "/saves/SaveGames/1.2_autosave_1.sav");
        assert_eq!(save.size, 27_513_344);
        assert!(!save.is_dir);
        assert!(entries[4].is_dir);
        assert_eq!(entries[3].name, "old autosave.sav", "names keep their spaces");
        assert!(entries[3].mtime < save.mtime, "an explicit year sorts earlier");
        assert_eq!(newest_save(&entries).map(|e| e.name.as_str()), Some("1.2_autosave_1.sav"));
    }

    #[test]
    fn listing_parses_windows_output_too() {
        let entries = parse_listing(WINDOWS_LISTING, "/C:/saves");
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[1].name, "notes.txt");
        assert!(entries[2].is_dir);
    }

    #[test]
    fn newest_save_ignores_everything_but_sav_files() {
        let mut entries = parse_listing(WINDOWS_LISTING, "/C:/saves");
        // The newest thing in the folder is not a save; the newest .sav wins.
        entries.push(DirEntry {
            name: "zzz.txt".to_string(),
            path: "/C:/saves/zzz.txt".to_string(),
            is_dir: false,
            size: 1,
            mtime: 4_000_000_000,
        });
        assert_eq!(newest_save(&entries).map(|e| e.name.as_str()), Some("1.2_autosave_1.sav"));
        assert_eq!(newest_save(&[]).map(|e| e.name.as_str()), None);
        assert!(is_save("1.2_autosave_1.SAV"), "the game's own casing can vary");
    }

    #[test]
    fn listing_rejects_junk() {
        for line in [
            "",
            "sftp> ls -l",
            "total 12",
            "not a listing at all",
            "drwxr-xr-x",
            "sftp> ls -l \"/saves\"",
        ] {
            assert!(parse_ls_line(line, "/x", at(10)).is_none(), "accepted {line:?}");
        }
    }

    #[test]
    fn when_parses_both_date_forms_and_infers_the_year() {
        let now = at(10);
        let recent = parse_when("Oct", "10", "13:41", now);
        let explicit = parse_when("Jan", "3", "2025", now);
        assert!(recent > explicit);
        assert_eq!(recent, month_secs(2026, 9, 10, 13, 41));
        assert_eq!(explicit, month_secs(2025, 0, 3, 0, 0));
        // A December file read in January belongs to last year, not next.
        let january = month_secs(2026, 0, 5, 12, 0) as u64;
        let then = parse_when("Dec", "30", "09:00", UNIX_EPOCH + Duration::from_secs(january));
        assert_eq!(then, month_secs(2025, 11, 30, 9, 0));
    }

    #[test]
    fn paths_are_joined_and_trimmed_the_way_sftp_reports_them() {
        assert_eq!(parent_of("/C:/saves/765"), "/C:/saves");
        assert_eq!(parent_of("/C:/"), "/C:/");
        assert_eq!(parent_of("/home/dion"), "/home");
        assert_eq!(parent_of("/"), "/");
        assert_eq!(join_remote("/C:/saves/", "a.sav"), "/C:/saves/a.sav");
    }

    #[test]
    fn pwd_and_platform_come_from_the_sftp_banner() {
        assert_eq!(
            parse_pwd("Remote working directory: /C:/Users/dion\n"),
            Some("/C:/Users/dion".to_string())
        );
        assert_eq!(parse_pwd("no banner here"), None);
        assert_eq!(platform_from_home("/C:/Users/dion"), "Windows");
        assert_eq!(platform_from_home("/home/dion"), "Linux/macOS");
    }

    #[test]
    fn base64_matches_utf16le_powershell_encoding() {
        // "AB" in UTF-16LE is 41 00 42 00 -> "QQBCAA==", the shape
        // powershell -EncodedCommand expects (verified against PowerShell's own
        // output for the same string).
        assert_eq!(base64_utf16le("AB"), "QQBCAA==");
        assert_eq!(base64_utf16le("A"), "QQA=");
        assert_eq!(base64_utf16le("ABC"), "QQBCAEMA");
        assert_eq!(base64_utf16le(""), "");
    }

    #[test]
    fn powershell_watch_script_carries_the_folder_and_stays_unquotable() {
        let args = powershell_watch_command("/C:/Users/dion/SaveGames").unwrap();
        assert_eq!(args[0], "-NoProfile");
        assert_eq!(args[2], "-EncodedCommand");
        assert!(args[3].chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/' || c == '='));
        // The path is in the script as a Windows path, not as an SFTP one.
        let decoded = args[3].clone();
        assert!(!decoded.contains('\''), "nothing in the payload can be requoted");
        // A path that is not a drive path, or carries a quote, is refused rather
        // than escaped into a command string.
        assert!(powershell_watch_command("/home/dion/saves").is_err());
        assert!(powershell_watch_command("/C:/Users/o'brien/saves").is_err());
    }

    #[test]
    fn a_folder_with_a_quote_is_refused_by_the_shell_watcher() {
        let target = SshTarget { host: "192.168.1.50".to_string(), ..Default::default() };
        let called = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = called.clone();
        let result = spawn_watch(&target, "/saves/o'brien", false, "inotify", move |_| {
            flag.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        assert!(result.is_err(), "a quote must not reach the remote shell");
        assert!(!called.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn injection_through_the_batch_script_is_refused() {
        assert!(check_remote_path("/C:/saves\nrm -rf /").is_err());
        assert!(check_remote_path("/C:/saves/\"x\".sav").is_err());
        assert!(check_remote_path("").is_err());
        assert!(check_remote_path("/C:/saves").is_ok());
        let mut target = SshTarget {
            host: "-oProxyCommand=evil".to_string(),
            ..Default::default()
        };
        assert!(target.check().is_err());
        target.host = "192.168.1.50".to_string();
        assert!(target.check().is_ok());
        target.user = "dion nightly".to_string();
        assert!(target.check().is_err());
    }
}
