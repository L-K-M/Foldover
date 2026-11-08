//! Headless test-only peer-backend qualification fixture for a second Mac.
//!
//! Runs the production `PeerLink`/`PeerHost`/`serve_folder` stack with a
//! generated identity persisted under the fixture root, one expected peer
//! device, and one offered share. It never calls the production identity
//! loader, the catalog, or the Keychain, and it only touches files inside
//! the fixture root the caller proves it owns.
//!
//! ```text
//! peer_qualification FIXTURE_ROOT OWNER_UUID EXPECTED_DEVICE SHARE_ID BIND_IP TTL_SECONDS
//! ```
//!
//! The caller creates the fixture root: an absolute mode-700 directory
//! named `foldover-perf-peer-OWNER_UUID` with `control/` and `shared/`
//! inside and a `.qualification-owner` marker holding the owner UUID.
//! `shared/` is exposed only through a `LocalRoot`; `control/` holds the
//! fixture's private state and is never served.
//!
//! The marker is a fixture convention, not a sandbox against another process
//! running as the same user. Use a private root that no other process modifies.
//!
//! The operator reads `control/status.json` over authenticated SSH, compares
//! the six-digit codes on both Macs, and writes `control/confirm.json`.
//! The fixture confirms only the exact expected pending device and code.
//! It stops when `control/stop` appears or the TTL expires, then returns
//! from `main` so `Drop for PeerLink` closes the transport channels. No
//! signal handler is installed: a signal would terminate the process
//! without running `Drop`.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use foldover_engine::{
    Answer, Decision, FolderCache, NotRecorded, PeerHost, PeerIdentity, PeerLink, PendingPairing,
    Presence, serve_folder,
};
use foldover_proto::peer::{OfferedShare, PeerCall, PeerReply};
use foldover_types::{DeviceId, LocalRoot, ShareId};
use serde::{Deserialize, Serialize};

/// The fixture directory is named `foldover-perf-peer-` plus the owner UUID.
const FIXTURE_PREFIX: &str = "foldover-perf-peer-";
/// The marker file holding the owner UUID, exact aside from trailing spaces.
const OWNER_MARKER: &str = ".qualification-owner";
/// Private fixture state, never served.
const CONTROL_DIR: &str = "control";
/// The only directory exposed to a paired Mac, through a `LocalRoot`.
const SHARED_DIR: &str = "shared";
const IDENTITY_FILE: &str = "identity.json";
const STATE_FILE: &str = "state.json";
const NETWORK_FILE: &str = "network.json";
const PAIRED_FILE: &str = "paired.json";
const CONFIRM_FILE: &str = "confirm.json";
const STATUS_FILE: &str = "status.json";
const STOP_FILE: &str = "stop";
const IDENTITY_FORMAT: &str = "foldover-qual-identity-v1";
const LINK_NAME: &str = "Foldover Qualification Fixture";
const SHARE_LABEL: &str = "Qualification Share";
/// State files hold a private key or pairing codes; keep them private.
const PRIVATE_MODE: u32 = 0o600;
/// The fixture root is private to the caller's user.
const FIXTURE_MODE: u32 = 0o700;
/// The bounded run length.
const MAX_TTL_SECONDS: u64 = 3_600;
/// One pairing window is at most ten minutes.
const MAX_PAIRING_WINDOW: Duration = Duration::from_secs(600);
/// How often the control files are polled and the status refreshed.
const POLL_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Debug)]
struct Failure(String);

impl Failure {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for Failure {}

impl From<io::Error> for Failure {
    fn from(error: io::Error) -> Self {
        Self(error.to_string())
    }
}

impl From<serde_json::Error> for Failure {
    fn from(error: serde_json::Error) -> Self {
        Self(error.to_string())
    }
}

impl From<foldover_engine::LinkError> for Failure {
    fn from(error: foldover_engine::LinkError) -> Self {
        Self(error.to_string())
    }
}

type Result<T> = std::result::Result<T, Failure>;

/// The persisted secret identity. The hex string is the exact
/// `to_secret_bytes` encoding; a corrupt file is never replaced.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct IdentityFile {
    format: String,
    identity: String,
}

/// The owner, expected device, and share this fixture was created for.
/// Changed values are never accepted as the previous trust.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ExpectedState {
    owner: String,
    device: String,
    share: String,
}

/// The transport address the fixture bound on an earlier run.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct NetworkState {
    address: String,
}

/// The device both people confirmed, recorded by `PeerHost::paired`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct PinRecord {
    device: String,
    name: String,
    paired_ms: u64,
}

/// The operator's answer to a pending pairing code.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
struct ConfirmRequest {
    owner: String,
    device: String,
    code: u32,
}

/// What the operator reads about the running fixture.
#[derive(Clone, Debug, Serialize)]
struct Status {
    device: String,
    address: String,
    pairing: Option<String>,
    pending: Vec<PendingRecord>,
    paired: bool,
    shutdown: bool,
    /// The last actionable refusal, e.g. a mismatched confirm request.
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
struct PendingRecord {
    device: String,
    name: String,
    code: u32,
}

/// The fixture's side of the link. It records the one expected device and
/// serves the one known share to that device only.
struct FixtureHost {
    expected: DeviceId,
    share: String,
    root: LocalRoot,
    control: PathBuf,
    folders: FolderCache,
    /// Set only after `PeerHost::paired` persisted the confirmed device.
    confirmed: AtomicBool,
}

impl PeerHost for FixtureHost {
    fn paired(&self, device: DeviceId, name: String) -> std::result::Result<(), NotRecorded> {
        if device != self.expected {
            eprintln!("peer_qualification: refusing to record unexpected device {device}");
            return Err(NotRecorded);
        }
        if self.confirmed.load(Ordering::SeqCst) {
            return Ok(());
        }
        let record = PinRecord {
            device: device.to_string(),
            name,
            paired_ms: now_ms(),
        };
        if let Err(error) = write_json_atomic(&self.control.join(PAIRED_FILE), &record) {
            eprintln!("peer_qualification: could not record the paired Mac {device}: {error}");
            return Err(NotRecorded);
        }
        self.confirmed.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn serve(&self, device: DeviceId, call: PeerCall, answer: &mut Answer) {
        // No lock is held while the folder is served.
        let allowed = device == self.expected && self.confirmed.load(Ordering::SeqCst);
        match call {
            PeerCall::Shares => {
                let reply = if allowed {
                    PeerReply::Shares {
                        shares: vec![OfferedShare {
                            id: self.share.clone(),
                            name: SHARE_LABEL.to_owned(),
                        }],
                    }
                } else {
                    refused("This Mac shares nothing with that device")
                };
                let _ = answer.reply(&reply);
            }
            PeerCall::Folder { share, op } => {
                if !allowed || share != self.share {
                    let _ = answer.reply(&refused("That folder is not shared with this Mac"));
                    return;
                }
                // `None` state: the fixture's own state subtree is never
                // listed, read, or changed for another Mac.
                serve_folder(&self.root, *op, &self.folders, answer, None);
            }
        }
    }
}

fn refused(reason: &str) -> PeerReply {
    PeerReply::Refused {
        reason: reason.to_owned(),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Stopped {
    StopFile,
    Ttl,
}

impl fmt::Display for Stopped {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StopFile => formatter.write_str("stop file"),
            Self::Ttl => formatter.write_str("ttl"),
        }
    }
}

/// Lexically normalize an absolute path, resolving `.` and `..` without
/// touching symlinks. The caller still compares this with the canonical
/// path, so a symlinked component is refused.
fn normalize_absolute(path: &Path) -> Result<PathBuf> {
    if !path.is_absolute() {
        return Err(Failure::new(format!(
            "The fixture root must be an absolute path, not {}",
            path.display()
        )));
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir => normalized.push("/"),
            Component::Prefix(_) => {
                return Err(Failure::new(
                    "The fixture root must be a POSIX absolute path",
                ));
            }
            Component::CurDir => {}
            Component::Normal(part) => normalized.push(part),
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(Failure::new(
                        "The fixture root must not climb above the filesystem root",
                    ));
                }
            }
        }
    }
    if normalized.as_os_str().is_empty() {
        normalized.push("/");
    }
    Ok(normalized)
}

/// The owner UUID, lowercased. Only the canonical 8-4-4-4-12 shape is
/// accepted; the version and variant bits are not constrained.
fn validate_owner(text: &str) -> Result<String> {
    let owner = text.trim();
    let bytes = owner.as_bytes();
    if bytes.len() != 36 {
        return Err(Failure::new(format!(
            "The owner UUID {owner:?} is not 36 characters"
        )));
    }
    for (index, byte) in bytes.iter().enumerate() {
        match index {
            8 | 13 | 18 | 23 => {
                if *byte != b'-' {
                    return Err(Failure::new(format!(
                        "The owner UUID {owner:?} has no hyphen at position {index}"
                    )));
                }
            }
            _ => {
                if !byte.is_ascii_hexdigit() {
                    return Err(Failure::new(format!(
                        "The owner UUID {owner:?} has a non-hex digit at position {index}"
                    )));
                }
            }
        }
    }
    Ok(owner.to_ascii_lowercase())
}

/// Check the fixture root the caller prepared: a physical, non-symlink
/// directory named for the owner, mode 700, with the marker and the two
/// owned subdirectories. Nothing outside the root is touched.
fn validate_fixture(root: &Path, owner: &str) -> Result<PathBuf> {
    let normalized = normalize_absolute(root)?;
    let canonical = fs::canonicalize(&normalized).map_err(|error| {
        Failure::new(format!(
            "The fixture root {} is unavailable: {error}",
            normalized.display()
        ))
    })?;
    if canonical != normalized {
        return Err(Failure::new(format!(
            "The fixture root {} must be a physical path, not a symlink to {}",
            normalized.display(),
            canonical.display()
        )));
    }
    let name = normalized
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| Failure::new("The fixture root has no directory name"))?;
    let expected_name = format!("{FIXTURE_PREFIX}{owner}");
    // UUIDs are case-insensitive; the marker and the mode are the real checks.
    if !name.eq_ignore_ascii_case(&expected_name) {
        return Err(Failure::new(format!(
            "The fixture root is named {name}, not {expected_name}"
        )));
    }
    let metadata = fs::metadata(&normalized).map_err(|error| {
        Failure::new(format!(
            "The fixture root {} is unreadable: {error}",
            normalized.display()
        ))
    })?;
    if !metadata.is_dir() {
        return Err(Failure::new(format!(
            "The fixture root {} is not a directory",
            normalized.display()
        )));
    }
    let mode = metadata.permissions().mode() & 0o777;
    if mode != FIXTURE_MODE {
        return Err(Failure::new(format!(
            "The fixture root {} has mode {mode:o}, not {FIXTURE_MODE:o}",
            normalized.display()
        )));
    }
    let link_metadata = fs::symlink_metadata(&normalized).map_err(|error| {
        Failure::new(format!(
            "The fixture root {} is unreadable: {error}",
            normalized.display()
        ))
    })?;
    if link_metadata.file_type().is_symlink() {
        return Err(Failure::new(format!(
            "The fixture root {} must not be a symlink",
            normalized.display()
        )));
    }
    for directory in [CONTROL_DIR, SHARED_DIR] {
        let path = normalized.join(directory);
        let metadata = fs::symlink_metadata(&path).map_err(|error| {
            Failure::new(format!(
                "The fixture directory {} is unavailable: {error}",
                path.display()
            ))
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(Failure::new(format!(
                "The fixture directory {} must be a real directory",
                path.display()
            )));
        }
    }
    let control = normalized.join(CONTROL_DIR);
    let shared = normalized.join(SHARED_DIR);
    if control == shared || control.starts_with(&shared) || shared.starts_with(&control) {
        return Err(Failure::new(
            "The fixture control and shared directories must be separate",
        ));
    }
    let marker = normalized.join(OWNER_MARKER);
    let metadata = fs::symlink_metadata(&marker).map_err(|error| {
        Failure::new(format!(
            "The owner marker {} is unavailable: {error}",
            marker.display()
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(Failure::new(format!(
            "The owner marker {} must be a real file",
            marker.display()
        )));
    }
    let bytes = fs::read(&marker)?;
    let text = String::from_utf8(bytes).map_err(|_| {
        Failure::new(format!(
            "The owner marker {} is not UTF-8",
            marker.display()
        ))
    })?;
    if !text.trim().eq_ignore_ascii_case(owner) {
        return Err(Failure::new(format!(
            "The owner marker {} holds {:?}, not {owner}",
            marker.display(),
            text.trim()
        )));
    }
    Ok(normalized)
}

fn hex_encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(DIGITS[usize::from(byte >> 4)] as char);
        text.push(DIGITS[usize::from(byte & 0x0f)] as char);
    }
    text
}

fn hex_decode(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    let (pairs, remainder) = text.as_bytes().as_chunks::<2>();
    if !remainder.is_empty() {
        return None;
    }
    let mut decoded = Vec::with_capacity(pairs.len());
    for pair in pairs {
        let high = hex_value(pair[0])?;
        let low = hex_value(pair[1])?;
        decoded.push(high * 16 + low);
    }
    Some(decoded)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Write a private file with `create_new`, so an existing identity is never
/// replaced. The caller falls back to reading the winner's file when a
/// concurrent creator won the race.
fn write_private_create_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true).mode(PRIVATE_MODE);
    let mut file = options.open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Write a private JSON file atomically: a temporary file in the same
/// directory, then a rename, so a reader never sees a partial document.
fn write_json_atomic(path: &Path, value: &impl Serialize) -> Result<()> {
    let json = serde_json::to_vec(value)?;
    let mut temporary = path.as_os_str().to_owned();
    temporary.push(".tmp");
    let temporary = PathBuf::from(temporary);
    {
        let mut options = OpenOptions::new();
        options
            .write(true)
            .create(true)
            .truncate(true)
            .mode(PRIVATE_MODE);
        let mut file = options.open(&temporary).map_err(|error| {
            Failure::new(format!("Writing {} failed: {error}", temporary.display()))
        })?;
        file.write_all(&json)?;
        file.sync_all()?;
    }
    fs::rename(&temporary, path)
        .map_err(|error| Failure::new(format!("Replacing {} failed: {error}", path.display())))
}

/// Load the saved identity, or create it once. A corrupt file is an error:
/// the fixture never regenerates an identity, which would change its device
/// ID and break every pin recorded against it.
fn load_or_create_identity(control: &Path) -> Result<PeerIdentity> {
    let path = control.join(IDENTITY_FILE);
    if !path.exists() {
        if [STATE_FILE, NETWORK_FILE, PAIRED_FILE]
            .iter()
            .any(|name| control.join(name).exists())
        {
            return Err(Failure::new(
                "The saved identity is missing from an existing fixture; refusing to generate a different identity",
            ));
        }
        let identity = PeerIdentity::generate()
            .map_err(|error| Failure::new(format!("Generating a peer identity failed: {error}")))?;
        let secret = identity
            .to_secret_bytes()
            .map_err(|error| Failure::new(format!("Encoding the peer identity failed: {error}")))?;
        let file = IdentityFile {
            format: IDENTITY_FORMAT.to_owned(),
            identity: hex_encode(&secret),
        };
        let json = serde_json::to_vec(&file)?;
        match write_private_create_new(&path, &json) {
            Ok(()) => return Ok(identity),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(Failure::new(format!(
                    "Saving {} failed: {error}",
                    path.display()
                )));
            }
        }
    }
    load_identity(&path)
}

fn load_identity(path: &Path) -> Result<PeerIdentity> {
    let bytes = fs::read(path)?;
    let file: IdentityFile = serde_json::from_slice(&bytes).map_err(|_| {
        Failure::new(format!(
            "The saved peer identity {} is corrupt; not regenerating it",
            path.display()
        ))
    })?;
    if file.format != IDENTITY_FORMAT {
        return Err(Failure::new(format!(
            "The saved peer identity {} has unknown format {:?}",
            path.display(),
            file.format
        )));
    }
    let secret = hex_decode(&file.identity).ok_or_else(|| {
        Failure::new(format!(
            "The saved peer identity {} is not hex; not regenerating it",
            path.display()
        ))
    })?;
    PeerIdentity::from_secret_bytes(&secret).map_err(|error| {
        Failure::new(format!(
            "The saved peer identity {} is invalid: {error}",
            path.display()
        ))
    })
}

/// Persist the owner, expected device, and share on the first run; verify
/// them on every later run. Changed values are not accepted as previous
/// trust.
fn load_or_create_expected(
    control: &Path,
    owner: &str,
    device: DeviceId,
    share: ShareId,
) -> Result<()> {
    let path = control.join(STATE_FILE);
    let desired = ExpectedState {
        owner: owner.to_owned(),
        device: device.to_string(),
        share: share.to_string(),
    };
    if !path.exists() {
        if [NETWORK_FILE, PAIRED_FILE]
            .iter()
            .any(|name| control.join(name).exists())
        {
            return Err(Failure::new(
                "The expected owner/device/share state is missing from an existing fixture; refusing to replace its trust scope",
            ));
        }
        let json = serde_json::to_vec(&desired)?;
        match write_private_create_new(&path, &json) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(Failure::new(format!(
                    "Saving {} failed: {error}",
                    path.display()
                )));
            }
        }
    }
    let bytes = fs::read(&path)?;
    let saved: ExpectedState = serde_json::from_slice(&bytes).map_err(|_| {
        Failure::new(format!(
            "The saved expected state {} is corrupt; not replacing it",
            path.display()
        ))
    })?;
    if saved != desired {
        return Err(Failure::new(format!(
            "The fixture already expects owner {}, device {}, share {}; refusing changed values as previous trust",
            saved.owner, saved.device, saved.share
        )));
    }
    Ok(())
}

/// The transport address saved by an earlier run, if any.
fn saved_network(control: &Path) -> Result<Option<SocketAddr>> {
    let path = control.join(NETWORK_FILE);
    if !path.exists() {
        if control.join(PAIRED_FILE).exists() {
            return Err(Failure::new(
                "The saved transport address is missing from a paired fixture; refusing to choose a different port",
            ));
        }
        return Ok(None);
    }
    let bytes = fs::read(&path)?;
    let saved: NetworkState = serde_json::from_slice(&bytes).map_err(|_| {
        Failure::new(format!(
            "The saved network state {} is corrupt; not replacing it",
            path.display()
        ))
    })?;
    saved.address.parse::<SocketAddr>().map(Some).map_err(|_| {
        Failure::new(format!(
            "The saved network state {} holds an invalid address {:?}",
            path.display(),
            saved.address
        ))
    })
}

/// The device recorded by a real pairing, if any. Only the expected device
/// is ever accepted.
fn load_paired(control: &Path, expected: DeviceId) -> Result<Option<DeviceId>> {
    let path = control.join(PAIRED_FILE);
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(&path)?;
    let record: PinRecord = serde_json::from_slice(&bytes).map_err(|_| {
        Failure::new(format!(
            "The recorded pin {} is corrupt; not trusting it",
            path.display()
        ))
    })?;
    let device = DeviceId::from_str(&record.device).map_err(|_| {
        Failure::new(format!(
            "The recorded pin {} holds an invalid device ID",
            path.display()
        ))
    })?;
    if device != expected {
        return Err(Failure::new(format!(
            "The recorded pin {} is for {device}, not the expected {expected}",
            path.display()
        )));
    }
    Ok(Some(device))
}

/// The operator's answer to a pending code. Only the exact expected
/// pending device and code is confirmed; anything else is refused and left
/// in place for the operator to see.
#[derive(Clone, Debug, Eq, PartialEq)]
enum ConfirmDecision {
    None,
    Confirm(DeviceId),
    Refused(String),
}

fn read_confirm(
    control: &Path,
    owner: &str,
    expected: DeviceId,
    pending: &[PendingPairing],
) -> ConfirmDecision {
    let path = control.join(CONFIRM_FILE);
    if !path.exists() {
        return ConfirmDecision::None;
    }
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            return ConfirmDecision::Refused(format!("Reading {} failed: {error}", path.display()));
        }
    };
    let request: ConfirmRequest = match serde_json::from_slice(&bytes) {
        Ok(request) => request,
        Err(error) => {
            return ConfirmDecision::Refused(format!(
                "{} is not a valid confirm request: {error}",
                path.display()
            ));
        }
    };
    if !request.owner.eq_ignore_ascii_case(owner) {
        return ConfirmDecision::Refused(format!(
            "{} names owner {:?}, not {owner}",
            path.display(),
            request.owner
        ));
    }
    let device = match DeviceId::from_str(&request.device) {
        Ok(device) => device,
        Err(_) => {
            return ConfirmDecision::Refused(format!(
                "{} names an invalid device ID {:?}",
                path.display(),
                request.device
            ));
        }
    };
    if device != expected {
        return ConfirmDecision::Refused(format!(
            "{} names device {device}, not the expected {expected}",
            path.display()
        ));
    }
    let Some(code) = pending
        .iter()
        .find(|entry| entry.device == expected)
        .map(|entry| entry.code)
    else {
        return ConfirmDecision::Refused(format!(
            "{} arrived before {expected} asked to pair",
            path.display()
        ));
    };
    if request.code != code {
        return ConfirmDecision::Refused(format!(
            "{} shows code {}, not the {code} both Macs show",
            path.display(),
            request.code
        ));
    }
    ConfirmDecision::Confirm(device)
}

enum FixtureState {
    Running,
    Shutdown,
}

fn write_status(
    control: &Path,
    link: &PeerLink,
    expected: DeviceId,
    state: FixtureState,
    error: Option<String>,
) -> Result<()> {
    let status = Status {
        device: link.device().to_string(),
        address: link.address()?.to_string(),
        pairing: link.pairing_address().map(|address| address.to_string()),
        pending: link
            .pending()
            .into_iter()
            .map(|entry| PendingRecord {
                device: entry.device.to_string(),
                name: entry.name,
                code: entry.code,
            })
            .collect(),
        paired: link.is_paired(expected),
        shutdown: matches!(state, FixtureState::Shutdown),
        error,
    };
    write_json_atomic(&control.join(STATUS_FILE), &status)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
        })
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let [
        root_argument,
        owner_argument,
        device_argument,
        share_argument,
        bind_argument,
        ttl_argument,
    ] = arguments.as_slice()
    else {
        return Err(Failure::new(
            "usage: peer_qualification FIXTURE_ROOT OWNER_UUID EXPECTED_DEVICE SHARE_ID BIND_IP TTL_SECONDS",
        ));
    };
    let owner = validate_owner(owner_argument)?;
    let expected = DeviceId::from_str(device_argument).map_err(|error| {
        Failure::new(format!(
            "The expected device ID {device_argument:?} is invalid: {error}"
        ))
    })?;
    let share = ShareId::from_str(share_argument).map_err(|error| {
        Failure::new(format!(
            "The share ID {share_argument:?} is invalid: {error}"
        ))
    })?;
    let bind_ip = bind_argument.parse::<Ipv4Addr>().map_err(|_| {
        Failure::new(format!(
            "The bind IP {bind_argument:?} is not an IPv4 address"
        ))
    })?;
    let ttl_seconds = ttl_argument.parse::<u64>().map_err(|_| {
        Failure::new(format!(
            "The TTL {ttl_argument:?} is not a whole number of seconds"
        ))
    })?;
    if ttl_seconds == 0 || ttl_seconds > MAX_TTL_SECONDS {
        return Err(Failure::new(format!(
            "The TTL must be 1 to {MAX_TTL_SECONDS} seconds, not {ttl_seconds}"
        )));
    }

    let root = validate_fixture(Path::new(root_argument), &owner)?;
    let control = root.join(CONTROL_DIR);
    let shared = root.join(SHARED_DIR);

    let identity = load_or_create_identity(&control)?;
    let own_device = DeviceId::from_bytes(*identity.id().as_bytes());
    load_or_create_expected(&control, &owner, expected, share)?;

    // Restart on the same transport port, so the operator's recorded
    // address stays valid. A saved address on another IP is refused.
    let saved = saved_network(&control)?;
    let bind = match saved {
        Some(address) => {
            if address.ip() != IpAddr::V4(bind_ip) {
                return Err(Failure::new(format!(
                    "The fixture last bound {address}, not {bind_ip}; start it on the same LAN address",
                )));
            }
            address
        }
        None => SocketAddr::new(IpAddr::V4(bind_ip), 0),
    };
    let paired_device = load_paired(&control, expected)?;
    let host = Arc::new(FixtureHost {
        expected,
        share: share.to_string(),
        root: LocalRoot::from_bytes(shared.as_os_str().as_encoded_bytes().to_vec()).map_err(
            |error| {
                Failure::new(format!(
                    "The shared directory is not a usable root: {error}"
                ))
            },
        )?,
        control: control.clone(),
        folders: FolderCache::default(),
        confirmed: AtomicBool::new(paired_device.is_some()),
    });
    let link = PeerLink::start(
        identity,
        LINK_NAME.to_owned(),
        bind,
        Presence::Unlisted,
        paired_device,
        host,
    )
    .map_err(|error| Failure::new(format!("Starting the peer link failed: {error}")))?;
    let address = link.address()?;
    if saved.is_none() {
        write_json_atomic(
            &control.join(NETWORK_FILE),
            &NetworkState {
                address: address.to_string(),
            },
        )?;
    }
    println!("ready {own_device} {address}");
    io::stdout().flush().ok();

    let ttl = Duration::from_secs(ttl_seconds);
    link.open_pairing(MAX_PAIRING_WINDOW.min(ttl))
        .map_err(|error| Failure::new(format!("Opening the pairing window failed: {error}")))?;
    let deadline = Instant::now() + ttl;
    let mut last_error: Option<String> = None;
    let mut stopped = Stopped::Ttl;
    loop {
        if control.join(STOP_FILE).exists() {
            stopped = Stopped::StopFile;
            break;
        }
        if Instant::now() >= deadline {
            break;
        }
        match read_confirm(&control, &owner, expected, &link.pending()) {
            ConfirmDecision::None => {}
            ConfirmDecision::Confirm(device) => match link.decide(device, Decision::Confirm) {
                Ok(()) => {
                    if let Err(error) = fs::remove_file(control.join(CONFIRM_FILE)) {
                        last_error = Some(format!("Consuming the confirm request failed: {error}"));
                    } else {
                        last_error = None;
                    }
                }
                Err(error) => {
                    last_error = Some(format!("Confirming {device} failed: {error}"));
                }
            },
            ConfirmDecision::Refused(reason) => last_error = Some(reason),
        }
        // A window never outlives the TTL; reopen one while time remains.
        if link.pairing_address().is_none() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            if let Err(error) = link.open_pairing(MAX_PAIRING_WINDOW.min(remaining)) {
                last_error = Some(format!("Reopening the pairing window failed: {error}"));
            }
        }
        write_status(
            &control,
            &link,
            expected,
            FixtureState::Running,
            last_error.clone(),
        )?;
        tokio::time::sleep(POLL_INTERVAL).await;
    }

    // Close the window, report the final state, then return: dropping the
    // `Arc<PeerLink>` runs its `Drop` and closes the transport channels.
    link.close_pairing();
    write_status(
        &control,
        &link,
        expected,
        FixtureState::Shutdown,
        last_error,
    )?;
    println!("stopped: {stopped}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    const OWNER: &str = "0d3f0d3f-0d3f-4d3f-8d3f-0d3f0d3f0d3f";
    const DEVICE: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const SHARE: &str = "0123456789abcdef0123456789abcdef";
    const OTHER_OWNER: &str = "00000000-0000-4000-8000-000000000000";

    /// A fixture root as the caller prepares it, under a canonical base so
    /// the physical-path check holds on systems with symlinked temp dirs.
    fn prepared_fixture() -> (tempfile::TempDir, PathBuf) {
        let temporary = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(temporary.path()).unwrap();
        let root = base.join(format!("{FIXTURE_PREFIX}{OWNER}"));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(FIXTURE_MODE)).unwrap();
        fs::create_dir(root.join(CONTROL_DIR)).unwrap();
        fs::create_dir(root.join(SHARED_DIR)).unwrap();
        fs::write(root.join(OWNER_MARKER), format!("{OWNER}\n")).unwrap();
        (temporary, root)
    }

    fn control(root: &Path) -> PathBuf {
        root.join(CONTROL_DIR)
    }

    #[test]
    fn owner_uuid_is_validated() {
        assert_eq!(validate_owner(OWNER).unwrap(), OWNER);
        assert_eq!(validate_owner(&OWNER.to_uppercase()).unwrap(), OWNER);
        assert_eq!(validate_owner(&format!(" {OWNER} ")).unwrap(), OWNER);
        for text in [
            "",
            "0d3f0d3f",
            "0d3f0d3f0d3f4d3f8d3f0d3f0d3f0d3f",
            "0d3f0d3f-0d3f-4d3f-8d3f-0d3f0d3f0d3g",
            "0d3f0d3f-0d3f-4d3f-8d3f-0d3f0d3f0d3f-",
        ] {
            assert!(validate_owner(text).is_err(), "accepted {text:?}");
        }
    }

    #[test]
    fn identifiers_and_ttl_are_validated() {
        assert_eq!(DeviceId::from_str(DEVICE).unwrap().to_string(), DEVICE);
        assert_eq!(ShareId::from_str(SHARE).unwrap().to_string(), SHARE);
        assert!(DeviceId::from_str(&DEVICE.to_uppercase()).is_err());
        assert!(DeviceId::from_str(&DEVICE[..63]).is_err());
        assert!(ShareId::from_str(&SHARE[..31]).is_err());
        assert!(ShareId::from_str(&SHARE.to_uppercase()).is_err());
        assert_eq!(
            Ipv4Addr::from_str("192.0.2.1").unwrap(),
            Ipv4Addr::new(192, 0, 2, 1)
        );
        assert!(Ipv4Addr::from_str("::1").is_err());
        assert!(Ipv4Addr::from_str("192.0.2.256").is_err());
        for ttl in ["0", "3601", "tomorrow", "-1"] {
            let parsed = ttl.parse::<u64>();
            let accepted =
                matches!(parsed, Ok(seconds) if seconds > 0 && seconds <= MAX_TTL_SECONDS);
            assert!(!accepted, "accepted ttl {ttl:?}");
        }
        assert!(matches!("3600".parse::<u64>(), Ok(3600)));
    }

    #[test]
    fn absolute_paths_are_normalized_without_symlinks() {
        assert_eq!(
            normalize_absolute(Path::new("/a/./b/")).unwrap(),
            Path::new("/a/b")
        );
        assert_eq!(
            normalize_absolute(Path::new("/a/../b")).unwrap(),
            Path::new("/b")
        );
        assert!(normalize_absolute(Path::new("relative/path")).is_err());
        assert!(normalize_absolute(Path::new("/a/../../b")).is_err());
    }

    #[test]
    fn fixture_validation_accepts_a_prepared_root() {
        let (_temporary, root) = prepared_fixture();
        assert_eq!(validate_fixture(&root, OWNER).unwrap(), root);
        assert_eq!(
            validate_fixture(&root.join("control/.."), OWNER).unwrap(),
            root
        );
    }

    #[test]
    fn fixture_name_matches_the_owner_case_insensitively() {
        let temporary = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(temporary.path()).unwrap();
        let root = base.join(format!("{FIXTURE_PREFIX}{}", OWNER.to_uppercase()));
        fs::create_dir(&root).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(FIXTURE_MODE)).unwrap();
        fs::create_dir(root.join(CONTROL_DIR)).unwrap();
        fs::create_dir(root.join(SHARED_DIR)).unwrap();
        fs::write(root.join(OWNER_MARKER), format!("{OWNER}\n")).unwrap();
        assert_eq!(validate_fixture(&root, OWNER).unwrap(), root);
    }

    #[test]
    fn fixture_validation_rejects_wrong_marker_name_mode_or_symlink() {
        let (temporary, root) = prepared_fixture();
        fs::write(root.join(OWNER_MARKER), format!("{OTHER_OWNER}\n")).unwrap();
        assert!(validate_fixture(&root, OWNER).is_err());
        fs::write(root.join(OWNER_MARKER), format!("{OWNER}\n")).unwrap();

        fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(validate_fixture(&root, OWNER).is_err());
        fs::set_permissions(&root, fs::Permissions::from_mode(FIXTURE_MODE)).unwrap();

        let misnamed = temporary.path().join("foldover-perf-peer-other");
        fs::create_dir(&misnamed).unwrap();
        assert!(validate_fixture(&misnamed, OWNER).is_err());

        let link = temporary.path().join("fixture-link");
        std::os::unix::fs::symlink(&root, &link).unwrap();
        assert!(validate_fixture(&link, OWNER).is_err());

        fs::remove_dir(root.join(CONTROL_DIR)).unwrap();
        assert!(validate_fixture(&root, OWNER).is_err());
    }

    #[test]
    fn identity_is_created_once_and_corrupt_state_is_refused() {
        let (_temporary, root) = prepared_fixture();
        let control = control(&root);
        let first = load_or_create_identity(&control).unwrap();
        let first_device = DeviceId::from_bytes(*first.id().as_bytes());
        let second = load_or_create_identity(&control).unwrap();
        assert_eq!(first_device, DeviceId::from_bytes(*second.id().as_bytes()));

        let path = control.join(IDENTITY_FILE);
        let bytes = fs::read(&path).unwrap();
        let mut file: IdentityFile = serde_json::from_slice(&bytes).unwrap();
        file.identity = "not hex".to_owned();
        fs::write(&path, serde_json::to_vec(&file).unwrap()).unwrap();
        let error = load_or_create_identity(&control).err().unwrap().to_string();
        assert!(error.contains("not hex"), "unexpected error: {error}");
        // The corrupt file is left in place, not replaced.
        assert!(path.exists());

        fs::write(&path, b"not json").unwrap();
        assert!(load_or_create_identity(&control).is_err());
    }

    #[test]
    fn missing_restart_state_is_never_silently_recreated() {
        let (_temporary, root) = prepared_fixture();
        let control = control(&root);
        let device = DeviceId::from_str(DEVICE).unwrap();
        let share = ShareId::from_str(SHARE).unwrap();
        load_or_create_identity(&control).unwrap();
        load_or_create_expected(&control, OWNER, device, share).unwrap();
        fs::remove_file(control.join(IDENTITY_FILE)).unwrap();
        assert!(load_or_create_identity(&control).is_err());
        assert!(!control.join(IDENTITY_FILE).exists());
        write_json_atomic(
            &control.join(NETWORK_FILE),
            &NetworkState {
                address: "192.0.2.1:45678".to_owned(),
            },
        )
        .unwrap();
        fs::remove_file(control.join(STATE_FILE)).unwrap();
        assert!(load_or_create_expected(&control, OWNER, device, share).is_err());
        assert!(!control.join(STATE_FILE).exists());
        fs::write(control.join(PAIRED_FILE), b"{}").unwrap();
        fs::remove_file(control.join(NETWORK_FILE)).unwrap();
        assert!(saved_network(&control).is_err());
    }

    #[test]
    fn expected_state_is_stable_and_changed_values_are_refused() {
        let (_temporary, root) = prepared_fixture();
        let control = control(&root);
        let device = DeviceId::from_str(DEVICE).unwrap();
        let share = ShareId::from_str(SHARE).unwrap();
        load_or_create_expected(&control, OWNER, device, share).unwrap();
        load_or_create_expected(&control, OWNER, device, share).unwrap();
        assert!(load_or_create_expected(&control, OTHER_OWNER, device, share).is_err());
        let other_share = ShareId::from_str("ab".repeat(16).as_str()).unwrap();
        assert!(load_or_create_expected(&control, OWNER, device, other_share).is_err());
        let other_device = DeviceId::from_str("ab".repeat(32).as_str()).unwrap();
        assert!(load_or_create_expected(&control, OWNER, other_device, share).is_err());
    }

    #[test]
    fn network_address_round_trips_and_pins_are_checked() {
        let (_temporary, root) = prepared_fixture();
        let control = control(&root);
        assert!(saved_network(&control).unwrap().is_none());
        let address = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 1)), 45_678);
        write_json_atomic(
            &control.join(NETWORK_FILE),
            &NetworkState {
                address: address.to_string(),
            },
        )
        .unwrap();
        assert_eq!(saved_network(&control).unwrap(), Some(address));

        let device = DeviceId::from_str(DEVICE).unwrap();
        assert!(load_paired(&control, device).unwrap().is_none());
        let record = PinRecord {
            device: device.to_string(),
            name: "Studio".to_owned(),
            paired_ms: 1,
        };
        write_json_atomic(&control.join(PAIRED_FILE), &record).unwrap();
        assert_eq!(load_paired(&control, device).unwrap(), Some(device));
        let other = DeviceId::from_str("ab".repeat(32).as_str()).unwrap();
        assert!(load_paired(&control, other).is_err());
    }

    #[test]
    fn confirm_requires_the_exact_pending_device_and_code() {
        let (_temporary, root) = prepared_fixture();
        let control = control(&root);
        let expected = DeviceId::from_str(DEVICE).unwrap();
        let other = DeviceId::from_str("ab".repeat(32).as_str()).unwrap();
        let pending = vec![PendingPairing {
            device: expected,
            name: "Studio".to_owned(),
            code: 123_456,
        }];
        let request = ConfirmRequest {
            owner: OWNER.to_owned(),
            device: expected.to_string(),
            code: 123_456,
        };
        write_json_atomic(&control.join(CONFIRM_FILE), &request).unwrap();
        assert_eq!(
            read_confirm(&control, OWNER, expected, &pending),
            ConfirmDecision::Confirm(expected)
        );
        // A matching request is not consumed by reading; the caller removes it.
        assert!(control.join(CONFIRM_FILE).exists());

        let wrong_code = ConfirmRequest {
            code: 654_321,
            ..request.clone()
        };
        write_json_atomic(&control.join(CONFIRM_FILE), &wrong_code).unwrap();
        assert!(matches!(
            read_confirm(&control, OWNER, expected, &pending),
            ConfirmDecision::Refused(_)
        ));

        let wrong_device = ConfirmRequest {
            device: other.to_string(),
            ..request.clone()
        };
        write_json_atomic(&control.join(CONFIRM_FILE), &wrong_device).unwrap();
        assert!(matches!(
            read_confirm(&control, OWNER, expected, &pending),
            ConfirmDecision::Refused(_)
        ));

        let wrong_owner = ConfirmRequest {
            owner: OTHER_OWNER.to_owned(),
            ..request.clone()
        };
        write_json_atomic(&control.join(CONFIRM_FILE), &wrong_owner).unwrap();
        assert!(matches!(
            read_confirm(&control, OWNER, expected, &pending),
            ConfirmDecision::Refused(_)
        ));

        write_json_atomic(&control.join(CONFIRM_FILE), &request).unwrap();
        assert!(matches!(
            read_confirm(&control, OWNER, expected, &[]),
            ConfirmDecision::Refused(_)
        ));
        assert_eq!(
            read_confirm(&control, OWNER, expected, &pending),
            ConfirmDecision::Confirm(expected)
        );
        fs::remove_file(control.join(CONFIRM_FILE)).unwrap();
        assert_eq!(
            read_confirm(&control, OWNER, expected, &pending),
            ConfirmDecision::None
        );
    }

    #[test]
    fn hex_round_trips() {
        let bytes = [0u8, 1, 15, 16, 127, 128, 255];
        let text = hex_encode(&bytes);
        assert_eq!(text, "00010f107f80ff");
        assert_eq!(hex_decode(&text).unwrap(), bytes);
        assert!(hex_decode("0").is_none());
        assert!(hex_decode("zz").is_none());
    }
}
