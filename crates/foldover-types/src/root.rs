//! Where one side of a pair lives: a local folder, or a folder on an SFTP
//! server. Both are kept as the bytes a person chose:
//!
//! ```text
//! /Users/alice/Photos                       local, absolute
//! sftp://alice@nas.local:2222/srv/photos    SFTP, absolute path
//! sftp://backup/~/photos                    SFTP, below the login's home
//! ```

use crate::{InvalidPair, LocalRoot, MAX_LOCAL_ROOT_BYTES};

const SFTP_SCHEME: &[u8] = b"sftp://";
const HOME: &[u8] = b"/~";
const SSH_PORT: u16 = 22;

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum PairRoot {
    Local(LocalRoot),
    Sftp(SftpRoot),
}

impl PairRoot {
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self, InvalidPair> {
        if bytes.starts_with(SFTP_SCHEME) {
            return Ok(Self::Sftp(SftpRoot::from_bytes(bytes)?));
        }
        Ok(Self::Local(LocalRoot::from_bytes(bytes)?))
    }

    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Local(root) => root.as_bytes(),
            Self::Sftp(root) => &root.locator,
        }
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

    fn sftp(text: &str) -> Result<SftpRoot, InvalidPair> {
        match PairRoot::from_bytes(text.as_bytes().to_vec())? {
            PairRoot::Sftp(root) => Ok(root),
            PairRoot::Local(_) => panic!("not an SFTP root"),
        }
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
