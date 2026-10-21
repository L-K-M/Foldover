//! Where one side of a pair lives: a local folder, a folder on an SFTP
//! server, or a folder another Mac shares. All are kept as the bytes a
//! person chose:
//!
//! ```text
//! /Users/alice/Photos                       local, absolute
//! sftp://alice@nas.local:2222/srv/photos    SFTP, absolute path
//! sftp://backup/~/photos                    SFTP, below the login's home
//! peer://<device: 64 hex>/<share: 32 hex>   shared by a paired Mac
//! ```

use crate::{DeviceId, InvalidPair, LocalRoot, MAX_LOCAL_ROOT_BYTES, ShareId};

const SFTP_SCHEME: &[u8] = b"sftp://";
const PEER_SCHEME: &[u8] = b"peer://";
const HOME: &[u8] = b"/~";
const SSH_PORT: u16 = 22;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum PairRoot {
    Local(LocalRoot),
    Sftp(SftpRoot),
    Peer(PeerRoot),
}

impl PairRoot {
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, InvalidPair> {
        if bytes.starts_with(SFTP_SCHEME) {
            return Ok(Self::Sftp(SftpRoot::from_bytes(bytes)?));
        }
        if bytes.starts_with(PEER_SCHEME) {
            return Ok(Self::Peer(PeerRoot::from_bytes(bytes)?));
        }
        Ok(Self::Local(LocalRoot::from_bytes(bytes)?))
    }

    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Local(root) => root.as_bytes(),
            Self::Sftp(root) => &root.locator,
            Self::Peer(root) => &root.locator,
        }
    }
}

/// A folder a paired Mac shares with this one (ADR 0031).
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct PeerRoot {
    locator: Vec<u8>,
    device: DeviceId,
    share: ShareId,
}

impl PeerRoot {
    pub fn new(device: DeviceId, share: ShareId) -> Self {
        Self {
            locator: format!("peer://{device}/{share}").into_bytes(),
            device,
            share,
        }
    }

    fn from_bytes(locator: Vec<u8>) -> Result<Self, InvalidPair> {
        let rest =
            std::str::from_utf8(&locator[PEER_SCHEME.len()..]).map_err(|_| InvalidPair::Root)?;
        let (device, share) = rest.split_once('/').ok_or(InvalidPair::Root)?;
        let device = device.parse().map_err(|_| InvalidPair::Root)?;
        let share = share.parse().map_err(|_| InvalidPair::Root)?;
        Ok(Self::new(device, share))
    }

    pub fn device(&self) -> DeviceId {
        self.device
    }

    pub fn share(&self) -> ShareId {
        self.share
    }
}

impl From<LocalRoot> for PairRoot {
    fn from(root: LocalRoot) -> Self {
        Self::Local(root)
    }
}

/// A folder on an SFTP server, reached with the user's SSH configuration.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct SftpRoot {
    locator: Vec<u8>,
    user: Option<String>,
    host: String,
    port: Option<u16>,
    path: Vec<u8>,
}

impl SftpRoot {
    fn from_bytes(locator: Vec<u8>) -> Result<Self, InvalidPair> {
        if locator.len() > MAX_LOCAL_ROOT_BYTES || locator.contains(&b'\0') {
            return Err(InvalidPair::Root);
        }
        let rest = &locator[SFTP_SCHEME.len()..];
        let slash = rest
            .iter()
            .position(|byte| *byte == b'/')
            .ok_or(InvalidPair::Root)?;
        let (authority, path) = rest.split_at(slash);
        let authority = std::str::from_utf8(authority).map_err(|_| InvalidPair::Root)?;
        let (user, address) = match authority.rsplit_once('@') {
            Some((user, address)) => (Some(user), address),
            None => (None, authority),
        };
        let (host, port) = split_port(address)?;
        let name = |text: &str| {
            !text.is_empty()
                && !text.starts_with('-')
                && text
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
        };
        let address = |text: &str| {
            name(text)
                || (text.len() > 2
                    && text.starts_with('[')
                    && text.ends_with(']')
                    && text[1..text.len() - 1]
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() || byte == b':'))
        };
        if !address(host) || user.is_some_and(|user| !name(user)) {
            return Err(InvalidPair::Root);
        }
        // `/~` and `/~/…` name the login's home folder.
        let path = match path.strip_prefix(HOME) {
            Some([]) => b"~".to_vec(),
            Some(below) if below.starts_with(b"/") => [b"~".as_slice(), below].concat(),
            _ => path.to_vec(),
        };
        Ok(Self {
            user: user.map(str::to_owned),
            host: host
                .trim_start_matches('[')
                .trim_end_matches(']')
                .to_owned(),
            port,
            path,
            locator,
        })
    }

    pub fn user(&self) -> Option<&str> {
        self.user.as_deref()
    }

    pub fn host(&self) -> &str {
        &self.host
    }

    pub fn port(&self) -> Option<u16> {
        self.port
    }

    /// Absolute, or `~` and `~/…` below the login's home folder.
    pub fn path(&self) -> &[u8] {
        &self.path
    }

    /// The folder at absolute `path` on the same server, as the same user:
    /// `sftp://alice@nas.local/srv` at `/srv/photos` is
    /// `sftp://alice@nas.local/srv/photos`. A folder named `~` at the
    /// server's top cannot be named: `/~` means the home folder.
    pub fn at(&self, path: &[u8]) -> Result<Self, InvalidPair> {
        let names_home = path
            .strip_prefix(HOME)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with(b"/"));
        if !path.starts_with(b"/") || names_home {
            return Err(InvalidPair::Root);
        }
        let rest = &self.locator[SFTP_SCHEME.len()..];
        let slash = rest
            .iter()
            .position(|byte| *byte == b'/')
            .ok_or(InvalidPair::Root)?;
        Self::from_bytes([SFTP_SCHEME, &rest[..slash], path].concat())
    }

    /// The locator's text: `sftp://alice@nas.local/srv/photos`.
    pub fn as_bytes(&self) -> &[u8] {
        &self.locator
    }

    /// Whether one folder holds the other, judged from the locators alone:
    /// the same server and port, and nested paths. A home-relative path
    /// and an absolute one are not compared.
    pub fn overlaps(&self, other: &Self) -> bool {
        let nested = |outer: &[u8], inner: &[u8]| {
            inner.strip_prefix(outer).is_some_and(|rest| {
                rest.is_empty() || rest.starts_with(b"/") || outer.ends_with(b"/")
            })
        };
        self.host.eq_ignore_ascii_case(&other.host)
            && self.port.unwrap_or(SSH_PORT) == other.port.unwrap_or(SSH_PORT)
            && (nested(&self.path, &other.path) || nested(&other.path, &self.path))
    }
}

fn split_port(address: &str) -> Result<(&str, Option<u16>), InvalidPair> {
    let port_at = if address.starts_with('[') {
        address.find("]:").map(|end| end + 1)
    } else {
        address.rfind(':')
    };
    let Some(colon) = port_at else {
        return Ok((address, None));
    };
    let port = address[colon + 1..]
        .parse::<u16>()
        .ok()
        .filter(|port| *port > 0)
        .ok_or(InvalidPair::Root)?;
    Ok((&address[..colon], Some(port)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_roots_name_a_device_and_its_share_exactly() {
        let device = "ab".repeat(32);
        let share = "cd".repeat(16);
        let text = format!("peer://{device}/{share}");
        let PairRoot::Peer(root) = PairRoot::from_bytes(text.clone().into_bytes()).unwrap() else {
            panic!("not a peer root");
        };
        assert_eq!(root.device().to_string(), device);
        assert_eq!(root.share().to_string(), share);
        assert_eq!(PairRoot::Peer(root).as_bytes(), text.as_bytes());

        for bad in [
            format!("peer://{device}"),
            format!("peer://{device}/{share}/sub"),
            format!("peer://{}/{share}", "AB".repeat(32)),
            format!("peer://{device}/"),
            "peer://".to_owned(),
        ] {
            assert!(
                PairRoot::from_bytes(bad.clone().into_bytes()).is_err(),
                "{bad}"
            );
        }
    }

    fn sftp(text: &str) -> Result<SftpRoot, InvalidPair> {
        match PairRoot::from_bytes(text.as_bytes().to_vec())? {
            PairRoot::Sftp(root) => Ok(root),
            PairRoot::Local(_) | PairRoot::Peer(_) => panic!("not an SFTP root"),
        }
    }

    #[test]
    fn sftp_folders_on_the_same_server_keep_its_user_and_port() {
        let root = sftp("sftp://alice@nas.local:2222/~/photos").unwrap();
        let below = root.at(b"/home/alice/photos/2026").unwrap();
        assert_eq!(
            below.as_bytes(),
            b"sftp://alice@nas.local:2222/home/alice/photos/2026"
        );
        assert_eq!(below.path(), b"/home/alice/photos/2026");
        assert_eq!(
            root.at(b"/").unwrap().as_bytes(),
            b"sftp://alice@nas.local:2222/"
        );
        assert!(root.at(b"relative").is_err());
        // `/~` in a locator names the home folder, not a folder named `~`.
        assert!(root.at(b"/~").is_err());
        assert!(root.at(b"/~/photos").is_err());
        assert!(root.at(b"/~photos").is_ok());
        assert!(root.at(b"/a\0b").is_err());
    }

    #[test]
    fn sftp_locators_name_user_host_port_and_path() {
        let root = sftp("sftp://alice@nas.local:2222/srv/photos").unwrap();
        assert_eq!(
            (root.user(), root.host(), root.port(), root.path()),
            (
                Some("alice"),
                "nas.local",
                Some(2222),
                b"/srv/photos".as_slice()
            )
        );
        assert_eq!(root.locator, b"sftp://alice@nas.local:2222/srv/photos");
        assert_eq!(sftp("sftp://backup/~/photos").unwrap().path(), b"~/photos");
        assert_eq!(sftp("sftp://backup/~").unwrap().path(), b"~");
        let literal = sftp("sftp://[fe80::1]:22/data").unwrap();
        assert_eq!((literal.host(), literal.port()), ("fe80::1", Some(22)));

        for invalid in [
            "sftp://nas",
            "sftp:///path",
            "sftp://-oProxyCommand=x/path",
            "sftp://nas:0/path",
            "sftp://nas:port/path",
            "sftp://us er@nas/path",
            "sftp://nas/pa\0th",
        ] {
            assert!(sftp(invalid).is_err(), "{invalid}");
        }
        assert!(matches!(
            PairRoot::from_bytes(b"/local".to_vec()),
            Ok(PairRoot::Local(_))
        ));
        assert!(PairRoot::from_bytes(b"relative".to_vec()).is_err());
    }

    #[test]
    fn overlap_needs_the_same_server_and_nested_paths() {
        let photos = sftp("sftp://nas/srv/photos").unwrap();
        assert!(photos.overlaps(&sftp("sftp://alice@NAS:22/srv/photos/2026").unwrap()));
        assert!(photos.overlaps(&sftp("sftp://nas/srv").unwrap()));
        assert!(!photos.overlaps(&sftp("sftp://nas/srv/photos-old").unwrap()));
        assert!(!photos.overlaps(&sftp("sftp://nas:2222/srv/photos").unwrap()));
        assert!(!photos.overlaps(&sftp("sftp://other/srv/photos").unwrap()));
    }
}
