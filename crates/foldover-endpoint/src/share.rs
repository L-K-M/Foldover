//! Network shares holding local roots: mounting one again after it went
//! away, e.g. when the Mac slept or left the network.

use std::io;

/// Mount the share at `url`, e.g. `smb://alice@nas.local/Photos`, with
/// the user's saved credentials and no dialog.
pub fn mount_share(url: &str) -> io::Result<()> {
    foldover_os::mount_share(url).map(|_| ())
}
