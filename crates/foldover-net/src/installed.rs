use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use core_foundation::base::TCFType;
use core_foundation::data::CFData;
use core_foundation::url::CFURL;
use nix::sys::socket::{getsockopt, sockopt::LocalPeerToken};
use security_framework::os::macos::code_signing::{
    Flags, GuestAttributes, SecCode, SecRequirement, SecStaticCode,
};
use tokio::io::AsyncReadExt;
use tokio::net::UnixStream;
use tokio::process::Command;
use tokio::sync::{OnceCell, Semaphore};

use crate::code_identity::CodeIdentity;

const APP_IDENTIFIER: &str = "ch.lkmc.foldover";
const CLI_IDENTIFIER: &str = "ch.lkmc.foldover.cli";
const AGENT_IDENTIFIER: &str = "ch.lkmc.foldover.agent";
const APP_EXECUTABLE: &str = "Contents/MacOS/Foldover";
const CLI_EXECUTABLE: &str = "Contents/Library/LaunchServices/foldover";
const AGENT_EXECUTABLE: &str = "Contents/Library/LaunchServices/foldoverd";
const CODESIGN: &str = "/usr/bin/codesign";
const MAX_SIGNING_OUTPUT: u64 = 64 * 1024;
const IDENTITY_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_NATIVE_CHECKS: usize = 4;
const FORBIDDEN_ENTITLEMENTS: &[&str] = &[
    "com.apple.security.get-task-allow",
    "com.apple.security.cs.debugger",
    "com.apple.security.cs.allow-jit",
    "com.apple.security.cs.allow-unsigned-executable-memory",
    "com.apple.security.cs.allow-dyld-environment-variables",
    "com.apple.security.cs.disable-library-validation",
    "com.apple.security.cs.disable-executable-page-protection",
];

static INSTALLATION: OnceCell<Option<Arc<Installation>>> = OnceCell::const_new();
static NATIVE_CHECKS: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(MAX_NATIVE_CHECKS)));

struct Installation {
    bundle: PathBuf,
    bundle_requirement: String,
    peer_requirement: String,
}

#[derive(Clone, Copy)]
enum Role {
    App,
    Cli,
    Agent,
}

/// Pin the verified installation before accepting requests. Standalone/debug
/// builds retain public health access but cannot authorize privileged peers.
pub(super) async fn prepare() {
    INSTALLATION
        .get_or_init(|| async {
            match load().await {
                Ok(installation) => Some(Arc::new(installation)),
                Err(error) => {
                    eprintln!("net: privileged IPC unavailable: {error}");
                    None
                }
            }
        })
        .await;
}

pub(super) async fn require(stream: &UnixStream) -> io::Result<()> {
    let token = getsockopt(stream, LocalPeerToken).map_err(io::Error::from)?;
    let token = token
        .val
        .into_iter()
        .flat_map(u32::to_ne_bytes)
        .collect::<Vec<_>>();
    prepare().await;
    let installation = INSTALLATION
        .get()
        .and_then(Option::as_ref)
        .ok_or_else(denied)?
        .clone();
    native(move || {
        let data = CFData::from_buffer(&token);
        let mut attributes = GuestAttributes::new();
        attributes.set_audit_token(data.as_concrete_TypeRef());
        let guest = SecCode::copy_guest_with_attribues(None, &attributes, Flags::NONE)
            .map_err(|_| denied())?;
        let requirement = installation
            .peer_requirement
            .parse::<SecRequirement>()
            .map_err(|_| denied())?;
        guest
            .check_validity(Flags::NONE, &requirement)
            .map_err(|_| denied())?;
        // Do not reload trust from a replacement bundle while this process runs.
        check_static(&installation.bundle, &installation.bundle_requirement)
    })
    .await
}

async fn load() -> io::Result<Installation> {
    let executable = std::fs::canonicalize(std::env::current_exe()?)?;
    let (bundle, role) = locate(&executable)?;
    let app = identity(&bundle, APP_IDENTIFIER).await?;
    let cli = identity(&bundle.join(CLI_EXECUTABLE), CLI_IDENTIFIER).await?;
    let agent = identity(&bundle.join(AGENT_EXECUTABLE), AGENT_IDENTIFIER).await?;
    let own = match role {
        Role::App => &app,
        Role::Cli => &cli,
        Role::Agent => &agent,
    };
    let own_requirement = requirement(&[&own.hash]);
    let bundle_requirement = requirement(&[&app.hash]);
    let cli_requirement = requirement(&[&cli.hash]);
    let agent_requirement = requirement(&[&agent.hash]);
    let peer_requirement = match role {
        Role::Agent => requirement(&[&app.hash, &cli.hash]),
        Role::App | Role::Cli => requirement(&[&agent.hash]),
    };
    let verified_bundle = bundle.clone();
    let verified_requirement = bundle_requirement.clone();
    native(move || {
        check_static(&verified_bundle, &verified_requirement)?;
        check_static(&verified_bundle.join(CLI_EXECUTABLE), &cli_requirement)?;
        check_static(&verified_bundle.join(AGENT_EXECUTABLE), &agent_requirement)?;
        let own =
            SecCode::for_self(Flags::NONE).map_err(|error| security_error("self lookup", error))?;
        let required = own_requirement
            .parse::<SecRequirement>()
            .map_err(|error| security_error("self requirement", error))?;
        own.check_validity(Flags::NONE, &required)
            .map_err(|error| security_error("self validity", error))
    })
    .await?;
    Ok(Installation {
        bundle,
        bundle_requirement,
        peer_requirement,
    })
}

fn locate(executable: &Path) -> io::Result<(PathBuf, Role)> {
    for (suffix, role) in [
        (APP_EXECUTABLE, Role::App),
        (CLI_EXECUTABLE, Role::Cli),
        (AGENT_EXECUTABLE, Role::Agent),
    ] {
        let suffix = Path::new(suffix);
        if !executable.ends_with(suffix) {
            continue;
        }
        let mut bundle = executable.to_owned();
        for _ in suffix.components() {
            bundle.pop();
        }
        if bundle
            .extension()
            .is_some_and(|extension| extension == "app")
        {
            return Ok((bundle, role));
        }
    }
    Err(denied())
}

async fn identity(path: &Path, identifier: &str) -> io::Result<CodeIdentity> {
    if std::fs::canonicalize(path)? != path {
        return Err(denied());
    }
    let mut child = Command::new(CODESIGN)
        .args(["--display", "--verbose=4"])
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let stderr = child.stderr.take().ok_or_else(denied)?;
    let read = async move {
        let mut bytes = Vec::new();
        stderr
            .take(MAX_SIGNING_OUTPUT + 1)
            .read_to_end(&mut bytes)
            .await?;
        if bytes.len() as u64 > MAX_SIGNING_OUTPUT {
            return Err(denied());
        }
        String::from_utf8(bytes).map_err(|_| denied())
    };
    let (status, output) = tokio::time::timeout(IDENTITY_TIMEOUT, async {
        tokio::try_join!(child.wait(), read)
    })
    .await
    .map_err(|_| denied())??;
    if !status.success() {
        return Err(denied());
    }
    let identity = CodeIdentity::parse(&output)?;
    if identity.identifier != identifier || !identity.hardened {
        return Err(denied());
    }
    Ok(identity)
}

fn requirement(hashes: &[&str]) -> String {
    let hashes = hashes
        .iter()
        .map(|hash| format!("cdhash H\"{hash}\""))
        .collect::<Vec<_>>()
        .join(" or ");
    let mut expression = format!("({hashes})");
    for entitlement in FORBIDDEN_ENTITLEMENTS {
        expression.push_str(&format!(" and !(entitlement[\"{entitlement}\"] exists)"));
    }
    expression
}

fn check_static(path: &Path, requirement: &str) -> io::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(denied());
    }
    let url = CFURL::from_path(path, metadata.is_dir()).ok_or_else(denied)?;
    let code = SecStaticCode::from_path(&url, Flags::NONE)
        .map_err(|error| security_error("installation lookup", error))?;
    let requirement = requirement
        .parse::<SecRequirement>()
        .map_err(|error| security_error("installation requirement", error))?;
    code.check_validity(
        Flags::STRICT_VALIDATE | Flags::CHECK_NESTED_CODE | Flags::NO_NETWORK_ACCESS,
        &requirement,
    )
    .map_err(|error| security_error("installation validity", error))
}

fn security_error(operation: &'static str, error: security_framework::base::Error) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("Code {operation} failed ({})", error.code()),
    )
}

async fn native<T: Send + 'static>(
    operation: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> io::Result<T> {
    tokio::time::timeout(IDENTITY_TIMEOUT, async {
        let permit = Arc::clone(&NATIVE_CHECKS)
            .acquire_owned()
            .await
            .map_err(|_| denied())?;
        tokio::task::spawn_blocking(move || {
            // A timeout cannot cancel Security.framework; retain its budget slot.
            let _permit = permit;
            operation()
        })
        .await
        .map_err(|_| denied())?
    })
    .await
    .map_err(|_| denied())?
}

fn denied() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "Installed peer identity is unavailable or no longer valid",
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apple_parser_accepts_generated_single_and_multiple_identity_requirements() {
        let hashes = [
            "0123456789abcdef0123456789abcdef01234567",
            "abcdef0123456789abcdef0123456789abcdef01",
        ];
        for count in 1..=hashes.len() {
            let expression = requirement(&hashes[..count]);
            assert!(
                expression.parse::<SecRequirement>().is_ok(),
                "Apple rejected generated requirement: {expression}",
            );
        }
    }
}
