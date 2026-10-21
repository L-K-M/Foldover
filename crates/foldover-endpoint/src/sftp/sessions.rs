//! A server's sessions, so several copies can talk to it at once (PLAN.md
//! §4.9: network 4–8 connections). A borrower takes an idle session, or
//! opens another while fewer than the limit are open, or waits for one
//! to come back. Each borrower holds at most one at a time, so borrowers
//! never wait on each other in a cycle; a server that refuses another
//! connection is shared by the sessions it already took.
//!
//! ```text
//! borrow ─▶ idle? ──yes──▶ use ──drop──▶ back to idle, wake a waiter
//!             └─no─▶ open < limit? ──yes──▶ connect (fails: wait)
//!                         └─no──▶ wait
//! ```

use std::io;
use std::ops::{Deref, DerefMut};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};

use foldover_net::sftp::Session;

/// Opens another session to the same server.
pub(super) type Connect = Box<dyn Fn() -> io::Result<Session> + Send + Sync>;

pub(super) struct Sessions {
    pool: Mutex<Pool>,
    returned: Condvar,
}

struct Pool {
    idle: Vec<Session>,
    /// Sessions open, idle or borrowed.
    open: usize,
    /// Sessions open at most; lowered to what the server took once it
    /// refuses another.
    limit: usize,
}

impl Sessions {
    /// Start from `first`; at most `limit` sessions are ever open.
    pub(super) fn new(first: Session, limit: usize) -> Self {
        Self {
            pool: Mutex::new(Pool {
                idle: vec![first],
                open: 1,
                limit: limit.max(1),
            }),
            returned: Condvar::new(),
        }
    }

    /// A session to use alone until the guard drops; `connect` opens
    /// another when none is idle and the limit allows.
    pub(super) fn borrow(&self, connect: Option<&Connect>) -> Borrowed<'_> {
        let mut pool = self.lock();
        loop {
            if let Some(session) = pool.idle.pop() {
                return self.lend(session);
            }
            if let Some(connect) = connect
                && pool.open < pool.limit
            {
                pool.open += 1;
                drop(pool);
                match connect() {
                    Ok(session) => return self.lend(session),
                    // The server takes no more: share the ones it took.
                    // One may have come back meanwhile, so look again.
                    Err(_) => {
                        pool = self.lock();
                        pool.open -= 1;
                        pool.limit = pool.open.max(1);
                        continue;
                    }
                }
            }
            pool = self.wait(pool);
        }
    }

    fn lend(&self, session: Session) -> Borrowed<'_> {
        Borrowed {
            sessions: self,
            session: Some(session),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Pool> {
        self.pool.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn wait<'a>(&self, pool: MutexGuard<'a, Pool>) -> MutexGuard<'a, Pool> {
        self.returned
            .wait(pool)
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// A borrowed session, back in the pool when dropped.
pub(super) struct Borrowed<'a> {
    sessions: &'a Sessions,
    session: Option<Session>,
}

impl Deref for Borrowed<'_> {
    type Target = Session;

    fn deref(&self) -> &Session {
        self.session.as_ref().expect("held until dropped")
    }
}

impl DerefMut for Borrowed<'_> {
    fn deref_mut(&mut self) -> &mut Session {
        self.session.as_mut().expect("held until dropped")
    }
}

impl Drop for Borrowed<'_> {
    fn drop(&mut self) {
        let Some(session) = self.session.take() else {
            return;
        };
        self.sessions.lock().idle.push(session);
        self.sessions.returned.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::*;

    fn server() -> Option<PathBuf> {
        std::env::var_os("FOLDOVER_SFTP_SERVER")
            .map(PathBuf::from)
            .into_iter()
            .chain(["/usr/libexec/sftp-server", "/usr/lib/openssh/sftp-server"].map(PathBuf::from))
            .find(|path| path.exists())
    }

    fn spawn(server: &Path) -> io::Result<Session> {
        Session::spawn(server, &[OsStr::new("-d"), OsStr::new("/")])
    }

    #[test]
    fn borrowers_get_their_own_sessions_up_to_the_limit() {
        let Some(server) = server() else {
            eprintln!("sftp-server not found; set FOLDOVER_SFTP_SERVER to run SFTP tests");
            return;
        };
        let opened = Arc::new(AtomicUsize::new(1));
        let counted = Arc::clone(&opened);
        let program = server.clone();
        let connect: Connect = Box::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
            spawn(&program)
        });
        let sessions = Sessions::new(spawn(&server).unwrap(), 2);

        let mut first = sessions.borrow(Some(&connect));
        let mut second = sessions.borrow(Some(&connect));
        assert_eq!(opened.load(Ordering::SeqCst), 2);
        // Each works on its own; a handle stays on its session.
        assert!(first.real_path(b"/").is_ok() && second.real_path(b"/").is_ok());

        // A third waits for one to come back rather than opening more.
        std::thread::scope(|threads| {
            let waiter = threads.spawn(|| sessions.borrow(Some(&connect)).real_path(b"/").is_ok());
            std::thread::sleep(Duration::from_millis(100));
            assert!(!waiter.is_finished());
            drop(first);
            assert!(waiter.join().unwrap());
        });
        drop(second);
        assert_eq!(opened.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_server_that_refuses_more_sessions_shares_the_ones_it_took() {
        let Some(server) = server() else {
            eprintln!("sftp-server not found; set FOLDOVER_SFTP_SERVER to run SFTP tests");
            return;
        };
        let refused: Connect = Box::new(|| Err(io::Error::other("too many sessions")));
        let sessions = Sessions::new(spawn(&server).unwrap(), 4);

        let first = sessions.borrow(Some(&refused));
        std::thread::scope(|threads| {
            let waiter = threads.spawn(|| sessions.borrow(Some(&refused)).real_path(b"/").is_ok());
            std::thread::sleep(Duration::from_millis(100));
            assert!(!waiter.is_finished());
            drop(first);
            assert!(waiter.join().unwrap());
        });
        assert_eq!(sessions.lock().limit, 1);
    }
}
