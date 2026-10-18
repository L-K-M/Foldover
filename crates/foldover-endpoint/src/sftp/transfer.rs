//! Copies where at least one side is an SFTP server. Data streams through
//! Foldover; the target receives it under a temporary name, the content
//! digest is checked against the plan, and only then is it placed.

use std::cell::RefCell;
use std::io;

use foldover_net::sftp::{Attributes, FileKind};
use foldover_types::{EntryContent, EntryKind, EntryVersion, ScanControl, SyncPath};

use super::{DEFAULT_DIRECTORY, DEFAULT_FILE, PERMISSION_BITS, SftpEndpoint, write_error};
use crate::{ApplyError, Endpoint, Expected, ForeignMetadata, ObservedEntry, RunScope, ScanError};

/// What a source provides for one entry.
enum Source<'a> {
    Local(&'a crate::LocalEndpoint, crate::LocalEntry),
    Sftp(&'a SftpEndpoint, Attributes),
}

impl Source<'_> {
    fn metadata(&self) -> ForeignMetadata {
        match self {
            Self::Local(_, entry) => crate::LocalEndpoint::foreign_metadata(entry),
            Self::Sftp(_, attributes) => SftpEndpoint::foreign_metadata(attributes),
        }
    }

    fn link_target(&self, path: &SyncPath) -> Result<Vec<u8>, ApplyError> {
        match self {
            Self::Local(_, entry) => entry
                .link_target()
                .map(<[u8]>::to_vec)
                .ok_or_else(|| ApplyError::Unsupported(path.clone())),
            Self::Sftp(endpoint, _) => Ok(endpoint.read_link(path)?),
        }
    }

    /// Push the file's data; a failure carries the endpoint's error out
    /// through `error`, since the sink only speaks `io::Error`.
    fn push(
        &self,
        path: &SyncPath,
        control: &mut impl FnMut() -> ScanControl,
        sink: &mut dyn FnMut(&[u8]) -> io::Result<()>,
        error: &RefCell<Option<ApplyError>>,
    ) -> io::Result<()> {
        let pushed = match self {
            Self::Local(endpoint, entry) => endpoint
                .read_entry(entry, control, &mut |chunk| sink(chunk))
                .map(|_| ()),
            Self::Sftp(endpoint, attributes) => endpoint
                .read_file(path, attributes, control, sink)
                .map(|_| ())
                .map_err(|source| match source {
                    ScanError::Changed(_) => ApplyError::SourceChanged(path.clone()),
                    source => ApplyError::Scan(source),
                }),
        };
        pushed.map_err(|failure| {
            *error.borrow_mut() = Some(failure);
            io::Error::other("source failed")
        })
    }
}

/// Make `to`'s `path` hold `from`'s entry and return the version stored.
pub(crate) fn copy(
    to: &Endpoint,
    from: &Endpoint,
    path: &SyncPath,
    planned: &ObservedEntry,
    current: Expected<'_>,
    run: &RunScope,
    control: &mut impl FnMut() -> ScanControl,
) -> Result<EntryVersion, ApplyError> {
    let source = source(from, path, planned, run, control)?;
    let kind = planned.version().kind();
    let expected = planned.version().content();
    match to {
        Endpoint::Local(local) => {
            let destination = local.receiving(path, current, run, control)?;
            match kind {
                EntryKind::Directory => {
                    if destination.is_some() {
                        return Err(ApplyError::Unsupported(path.clone()));
                    }
                    local.receive_directory(path, source.metadata(), run, control)
                }
                EntryKind::Symlink => {
                    let target = source.link_target(path)?;
                    local.receive_link(path, destination.as_ref(), &target, run, control)
                }
                EntryKind::File => {
                    let failure = RefCell::new(None);
                    let received = local.receive_file(
                        path,
                        destination.as_ref(),
                        source.metadata(),
                        expected,
                        run,
                        |sink| source.push(path, control, sink, &failure),
                    );
                    received.map_err(|error| failure.into_inner().unwrap_or(error))
                }
            }
        }
        Endpoint::Sftp(remote) => {
            let destination = receiving(remote, path, current, run, control)?;
            send(
                remote,
                &source,
                path,
                kind,
                expected,
                destination.as_ref(),
                run,
                control,
            )
        }
    }
}

/// Give `to`'s `path` `from`'s synced metadata, leaving content alone.
pub(crate) fn update_metadata(
    to: &Endpoint,
    from: &Endpoint,
    path: &SyncPath,
    planned: &ObservedEntry,
    current: &ObservedEntry,
    run: &RunScope,
    control: &mut impl FnMut() -> ScanControl,
) -> Result<EntryVersion, ApplyError> {
    if planned.version().content() != current.version().content() {
        return Err(ApplyError::Unsupported(path.clone()));
    }
    let metadata = source(from, path, planned, run, control)?.metadata();
    match to {
        Endpoint::Local(local) => local.receive_metadata(path, current, metadata, run, control),
        Endpoint::Sftp(remote) => {
            let attributes = receiving(remote, path, Expected::Present(current), run, control)?
                .ok_or_else(|| ApplyError::DestinationChanged(path.clone()))?;
            if attributes.kind() != FileKind::Symlink {
                remote.update_metadata(
                    path,
                    current.version().kind(),
                    metadata,
                    run.options().metadata(),
                )?;
            }
            remote.metadata_version(path, current.version().content(), run.options().metadata())
        }
    }
}

fn source<'a>(
    from: &'a Endpoint,
    path: &SyncPath,
    planned: &ObservedEntry,
    run: &RunScope,
    control: &mut impl FnMut() -> ScanControl,
) -> Result<Source<'a>, ApplyError> {
    Ok(match from {
        Endpoint::Local(local) => {
            Source::Local(local, local.source_entry(path, planned, run, control)?)
        }
        Endpoint::Sftp(remote) => Source::Sftp(
            remote,
            remote
                .verified(path, planned, run.options().metadata(), control)
                .map_err(|error| match error {
                    ApplyError::DestinationChanged(_) | ApplyError::Scan(ScanError::Changed(_)) => {
                        ApplyError::SourceChanged(path.clone())
                    }
                    error => error,
                })?,
        ),
    })
}

/// What an SFTP `path` holds before receiving: nothing, or the verified
/// entry the plan expected.
fn receiving(
    remote: &SftpEndpoint,
    path: &SyncPath,
    current: Expected<'_>,
    run: &RunScope,
    control: &mut impl FnMut() -> ScanControl,
) -> Result<Option<Attributes>, ApplyError> {
    match current {
        Expected::Absent => match remote.attributes(path)? {
            None => Ok(None),
            Some(_) => Err(ApplyError::DestinationChanged(path.clone())),
        },
        Expected::Present(current) => remote
            .verified(path, current, run.options().metadata(), control)
            .map(Some)
            .map_err(|error| match error {
                ApplyError::Scan(ScanError::Changed(_)) => {
                    ApplyError::DestinationChanged(path.clone())
                }
                error => error,
            }),
    }
}

#[allow(clippy::too_many_arguments)]
fn send(
    remote: &SftpEndpoint,
    source: &Source<'_>,
    path: &SyncPath,
    kind: EntryKind,
    expected: EntryContent,
    destination: Option<&Attributes>,
    run: &RunScope,
    control: &mut impl FnMut() -> ScanControl,
) -> Result<EntryVersion, ApplyError> {
    let options = run.options();
    let policy = options.metadata();
    let metadata = source.metadata();
    let synced_mode = |default: u32| match policy.permissions() {
        foldover_types::Propagation::Sync => metadata.permissions & PERMISSION_BITS,
        foldover_types::Propagation::Ignore => default,
    };
    match kind {
        EntryKind::Directory => {
            if destination.is_some() {
                return Err(ApplyError::Unsupported(path.clone()));
            }
            remote
                .make_directory(path, synced_mode(DEFAULT_DIRECTORY))
                .map_err(|source| match remote.attributes(path) {
                    Ok(Some(_)) => ApplyError::DestinationChanged(path.clone()),
                    _ => write_error(path, source),
                })?;
            remote.placed_version(path, None, policy, control)
        }
        EntryKind::Symlink => {
            let target = source.link_target(path)?;
            let staged = remote
                .stage_link(path.as_bytes(), &target)
                .map_err(|source| write_error(path, source))?;
            remote.place(&staged, path, destination, run)?;
            remote.placed_version(path, None, policy, control)
        }
        EntryKind::File => {
            let failure = RefCell::new(None);
            let digest = RefCell::new(blake3::Hasher::new());
            let staged = remote
                .upload(
                    path.as_bytes(),
                    synced_mode(DEFAULT_FILE),
                    options.durability(),
                    |sink| {
                        source.push(
                            path,
                            control,
                            &mut |chunk| {
                                digest.borrow_mut().update(chunk);
                                sink(chunk)
                            },
                            &failure,
                        )
                    },
                )
                .map_err(|source| failure.take().unwrap_or_else(|| write_error(path, source)))?;
            let digest =
                foldover_types::Digest::from_bytes(*digest.into_inner().finalize().as_bytes());
            let applied = remote.set_metadata(&staged, EntryKind::File, metadata, policy);
            let version = applied.and_then(|()| remote.staged_version(&staged, digest, policy));
            let version = match version {
                Ok(version) if version.content() == expected => version,
                Ok(_) => {
                    remote.discard(&staged);
                    return Err(ApplyError::ContentNotStored(path.clone()));
                }
                Err(source) => {
                    remote.discard(&staged);
                    return Err(write_error(path, source));
                }
            };
            remote.place(&staged, path, destination, run)?;
            Ok(version)
        }
    }
}
