//! Copies where at least one side is an SFTP server or another Mac's
//! folder. Data streams through Foldover; the target receives it under a
//! temporary name, the content digest is checked against the plan, and
//! only then is it placed.

use std::cell::RefCell;
use std::io;

use foldover_fs::{NativeMetadata, PARTIAL_MIN_BYTES, Resume};
use foldover_net::sftp::{Attributes, FileKind};
use foldover_proto::peer::IncomingKind;
use foldover_types::{EntryContent, EntryKind, EntryVersion, ScanControl, SyncPath};

use super::{DEFAULT_DIRECTORY, DEFAULT_FILE, PERMISSION_BITS, SftpEndpoint, write_error};
use crate::local::{Archiving, Carried};
use crate::{ApplyError, Endpoint, Expected, ForeignMetadata, ObservedEntry, RunScope, ScanError};

/// What a source provides for one entry.
enum Source<'a> {
    Local(&'a crate::LocalEndpoint, crate::LocalEntry),
    Sftp(&'a SftpEndpoint, Attributes),
    Peer(crate::remote::PeerSource),
}

impl Source<'_> {
    fn metadata(&self) -> ForeignMetadata {
        match self {
            Self::Local(_, entry) => crate::LocalEndpoint::foreign_metadata(entry),
            Self::Sftp(_, attributes) => SftpEndpoint::foreign_metadata(attributes),
            Self::Peer(source) => source.metadata(),
        }
    }

    /// What a copy to or from another Mac carries besides data, mode and
    /// time; None where the source keeps none of it, as on SFTP servers.
    fn native(&self, run: &RunScope) -> Result<Option<NativeMetadata>, ApplyError> {
        match self {
            Self::Local(endpoint, entry) if entry.link_target().is_none() => endpoint
                .native_metadata(entry, run.options().metadata())
                .map(Some),
            Self::Local(..) | Self::Sftp(..) => Ok(None),
            Self::Peer(source) => Ok(source.native().cloned()),
        }
    }

    fn link_target(&self, path: &SyncPath) -> Result<Vec<u8>, ApplyError> {
        match self {
            Self::Local(_, entry) => entry
                .link_target()
                .map(<[u8]>::to_vec)
                .ok_or_else(|| ApplyError::Unsupported(path.clone())),
            Self::Sftp(endpoint, _) => Ok(endpoint.read_link(path)?),
            Self::Peer(source) => source
                .link_target()
                .map(<[u8]>::to_vec)
                .ok_or_else(|| ApplyError::Unsupported(path.clone())),
        }
    }

    /// Push the file's data from byte `from`; a failure carries the
    /// endpoint's error out through `error`, since the sink only speaks
    /// `io::Error`. Another Mac was asked for that offset when the
    /// source was opened.
    fn push(
        &self,
        path: &SyncPath,
        from: u64,
        run: &RunScope,
        control: &mut impl FnMut() -> ScanControl,
        sink: &mut dyn FnMut(&[u8]) -> io::Result<()>,
        error: &RefCell<Option<ApplyError>>,
    ) -> io::Result<()> {
        // PLAN.md §4.9: the pair's bandwidth limit paces the data as it
        // goes. The wait asks the control too, so a cancel is not held
        // up; the reader asks it between chunks, never during a wait.
        let control = RefCell::new(control);
        let mut checked = || (*control.borrow_mut())();
        let mut paced = |chunk: &[u8]| {
            if let Some(pace) = run.pace()
                && pace.wait(chunk.len() as u64, &mut || (*control.borrow_mut())())
                    == ScanControl::Cancel
            {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
            }
            sink(chunk)
        };
        let (control, sink) = (&mut checked, &mut paced);
        let pushed = match self {
            Self::Local(endpoint, entry) => {
                endpoint.read_entry_from(entry, from, control, &mut |chunk| sink(chunk))
            }
            Self::Sftp(endpoint, attributes) => endpoint
                .read_file_from(path, attributes, from, control, sink)
                .map_err(|source| match source {
                    ScanError::Changed(_) => ApplyError::SourceChanged(path.clone()),
                    source => ApplyError::Scan(source),
                }),
            Self::Peer(source) => source.push(path, control, sink),
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
    if let Endpoint::Sftp(_) = to {
        super::check_durability(run.options().durability(), path)?;
    }
    // PLAN.md §4.9: a large file continues after what a broken-off copy
    // kept at the destination, unless the run keeps nothing there.
    let kept = match run.archiving() {
        Archiving::Kept => kept(to, planned),
        Archiving::Off => 0,
    };
    let source = source(from, path, planned, run, control, kept)?;
    let kind = planned.version().kind();
    let expected = planned.version().content();
    match to {
        #[cfg(feature = "simulation")]
        Endpoint::Memory(_) => Err(ApplyError::Unsupported(path.clone())),
        Endpoint::Local(local) => {
            let destination = local.receiving(path, current, run, control)?;
            match kind {
                EntryKind::Directory => {
                    if destination.is_some() {
                        return Err(ApplyError::Unsupported(path.clone()));
                    }
                    let native = source.native(run)?;
                    let carried = Carried {
                        metadata: source.metadata(),
                        native: native.as_ref(),
                    };
                    local.receive_directory(path, carried, run, control)
                }
                EntryKind::Symlink => {
                    let target = source.link_target(path)?;
                    local.receive_link(path, destination.as_ref(), &target, run, control)
                }
                EntryKind::File => {
                    let EntryContent::File(content) = expected else {
                        return Err(ApplyError::Unsupported(path.clone()));
                    };
                    let native = source.native(run)?;
                    let failure = RefCell::new(None);
                    let carried = Carried {
                        metadata: source.metadata(),
                        native: native.as_ref(),
                    };
                    let resume = Resume {
                        content,
                        from: kept,
                    };
                    let received = local.receive_file(
                        path,
                        destination.as_ref(),
                        carried,
                        resume,
                        run,
                        |sink| source.push(path, kept, run, control, sink, &failure),
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
        Endpoint::Peer(remote) => {
            let kind = match kind {
                EntryKind::File => match expected {
                    EntryContent::File(digest) => IncomingKind::File {
                        content: digest.as_bytes().to_vec(),
                        from: kept,
                    },
                    _ => return Err(ApplyError::Unsupported(path.clone())),
                },
                EntryKind::Directory => IncomingKind::Folder,
                EntryKind::Symlink => IncomingKind::Link {
                    target: source.link_target(path)?,
                },
            };
            let native = source.native(run)?;
            let failure = RefCell::new(None);
            let carried = Carried {
                metadata: source.metadata(),
                native: native.as_ref(),
            };
            let received = remote.receive(path, current, kind, carried, run, |sink| {
                source.push(path, kept, run, control, sink, &failure)
            });
            received.map_err(|error| failure.into_inner().unwrap_or(error))
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
    if let Endpoint::Sftp(_) = to {
        super::check_durability(run.options().durability(), path)?;
    }
    if planned.version().content() != current.version().content() {
        return Err(ApplyError::Unsupported(path.clone()));
    }
    // Another Mac sends only the metadata, not the data with it.
    let (metadata, native) = match from {
        Endpoint::Peer(remote) => remote.source_metadata(path, planned, run)?,
        _ => {
            let source = source(from, path, planned, run, control, 0)?;
            let native = match to {
                Endpoint::Sftp(_) => None,
                _ => source.native(run)?,
            };
            (source.metadata(), native)
        }
    };
    match to {
        #[cfg(feature = "simulation")]
        Endpoint::Memory(_) => Err(ApplyError::Unsupported(path.clone())),
        Endpoint::Peer(remote) => {
            let carried = Carried {
                metadata,
                native: native.as_ref(),
            };
            remote.receive_metadata(path, current, carried, run)
        }
        Endpoint::Local(local) => {
            let carried = Carried {
                metadata,
                native: native.as_ref(),
            };
            local.receive_metadata(path, current, carried, run, control)
        }
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

/// How much of `planned` a broken-off copy kept at `to`. Resuming only
/// saves time, so a failure to find out starts the copy over.
fn kept(to: &Endpoint, planned: &ObservedEntry) -> u64 {
    let EntryContent::File(content) = planned.version().content() else {
        return 0;
    };
    if planned.size() < PARTIAL_MIN_BYTES {
        return 0;
    }
    let kept = match to {
        #[cfg(feature = "simulation")]
        Endpoint::Memory(_) => Ok(0),
        Endpoint::Local(local) => local.partial_length(&content),
        Endpoint::Peer(remote) => remote.partial_length(&content),
        // Pipelined SFTP writes may land out of order, leaving holes no
        // digest there would catch; SFTP copies start over.
        Endpoint::Sftp(_) => Ok(0),
    };
    kept.unwrap_or(0).min(planned.size())
}

/// `from`'s entry, a peer's read starting at byte `kept`.
fn source<'a>(
    from: &'a Endpoint,
    path: &SyncPath,
    planned: &ObservedEntry,
    run: &RunScope,
    control: &mut impl FnMut() -> ScanControl,
    kept: u64,
) -> Result<Source<'a>, ApplyError> {
    Ok(match from {
        #[cfg(feature = "simulation")]
        Endpoint::Memory(_) => return Err(ApplyError::Unsupported(path.clone())),
        Endpoint::Local(local) => {
            Source::Local(local, local.source_entry(path, planned, run, control)?)
        }
        // Every destination checks a file's digest as it lands, so
        // hashing it here first would only download it twice; the read
        // checks its stat stays put.
        Endpoint::Sftp(remote) if planned.version().kind() == EntryKind::File => {
            let attributes = remote
                .attributes(path)?
                .filter(|attributes| {
                    attributes.kind() == FileKind::File && attributes.size == Some(planned.size())
                })
                .ok_or_else(|| ApplyError::SourceChanged(path.clone()))?;
            Source::Sftp(remote, attributes)
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
        Endpoint::Peer(remote) => Source::Peer(remote.read(path, planned, run, kept)?),
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
                            0,
                            run,
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
