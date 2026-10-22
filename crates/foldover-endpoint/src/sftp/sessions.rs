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
use std::panic::{self, AssertUnwindSafe};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use foldover_net::sftp::Session;

/// Opens another session to the same server.
pub(super) type Connect = Box<dyn Fn() -> io::Result<Session> + Send + Sync>;

/// Refusals in a row before the server is taken at its word that it
/// took all the sessions it will; one failure can be transient.
const REFUSALS: usize = 3;

/// Between connect retries, so a refusing server is not hammered.
const RETRY: Duration = Duration::from_millis(200);

pub(super) struct Sessions {
    pool: Mutex<Pool>,
    returned: Condvar,
}

struct Pool {
    idle: Vec<Session>,
    /// Sessions open, idle or borrowed.
    open: usize,
    /// Sessions open at most; a run of refusals lowers it to what the
    /// server took, and a successful connect restores it.
    limit: usize,
    /// What `limit` was asked for, so a recovered server gets it back.
    wanted: usize,
    /// Connects refused in a row; reset by one that succeeds.
    refusals: usize,
    /// Last connect tried under a clamp, so a recovered server is
    /// found again even while the sessions it already took stay alive.
    last_probe: Option<Instant>,
}

impl Sessions {
    /// Start from `first`; at most `limit` sessions are ever open.
    pub(super) fn new(first: Session, limit: usize) -> Self {
        let limit = limit.max(1);
        Self {
            pool: Mutex::new(Pool {
                idle: vec![first],
                open: 1,
                limit,
                wanted: limit,
                refusals: 0,
                last_probe: None,
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
                drop(pool);
                return self.lend(session);
            }
            if let Some(connect) = connect
                && (pool.open < pool.limit
                    || pool.limit < pool.wanted
                        && pool.last_probe.is_none_or(|at| at.elapsed() >= RETRY))
            {
                pool.open += 1;
                pool.last_probe = Some(Instant::now());
                drop(pool);
                match panic::catch_unwind(AssertUnwindSafe(connect)) {
                    Ok(Ok(session)) => {
                        let mut pool = self.lock();
                        pool.refusals = 0;
                        // Grow back one slot per success rather than
                        // straight to wanted: a server that stays
                        // capped costs one probe per RETRY below
                        // wanted, not a burst of refusals on recovery.
                        pool.limit = pool.limit.saturating_add(1).min(pool.wanted);
                        drop(pool);
                        self.returned.notify_one();
                        return self.lend(session);
                    }
                    // The server takes no more: share the ones it took.
                    // One may have come back meanwhile, so look again.
                    Ok(Err(_)) => {
                        pool = self.lock();
                        pool.open -= 1;
                        pool.refusals += 1;
                        if pool.refusals >= REFUSALS {
                            pool.limit = pool.open.max(1);
                        }
                        pool = self.wait_for(pool, RETRY);
                        continue;
                    }
                    Err(payload) => {
                        // Give the slot back before propagating, or the
                        // pool shrinks for good.
                        self.lock().open -= 1;
                        self.returned.notify_one();
                        panic::resume_unwind(payload);
                    }
                }
            }
            pool = if pool.limit < pool.wanted {
                self.wait_for(pool, RETRY)
            } else {
                self.wait(pool)
            };
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

    fn wait_for<'a>(&self, pool: MutexGuard<'a, Pool>, within: Duration) -> MutexGuard<'a, Pool> {
        self.returned
            .wait_timeout(pool, within)
            .unwrap_or_else(PoisonError::into_inner)
            .0
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
        let mut pool = self.sessions.lock();
        // A session that desynchronized refuses every request; give its
        // slot back rather than lend it to the next borrower.
        if session.alive() {
            pool.idle.push(session);
        } else {
            pool.open -= 1;
        }
        self.sessions.returned.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
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
    fn a_recovered_server_grows_its_pool_back() {
        let Some(server) = server() else {
            eprintln!("sftp-server not found; set FOLDOVER_SFTP_SERVER to run SFTP tests");
            return;
        };
        let refusing = Arc::new(AtomicBool::new(true));
        let connect: Connect = {
            let server = server.clone();
            let refusing = refusing.clone();
            Box::new(move || {
                if refusing.load(Ordering::SeqCst) {
                    Err(io::Error::other("too many sessions"))
                } else {
                    spawn(&server)
                }
            })
        };
        let sessions = Sessions::new(spawn(&server).unwrap(), 2);
        let _held = sessions.borrow(Some(&connect));
        std::thread::scope(|threads| {
            let waiter = threads.spawn(|| sessions.borrow(Some(&connect)));
            // Refusals clamp the pool to its one live session.
            for _ in 0..100 {
                let pool = sessions.lock();
                if pool.limit == 1 && pool.refusals >= REFUSALS {
                    break;
                }
                std::thread::sleep(RETRY / 4);
            }
            assert_eq!(sessions.lock().limit, 1);
            // It takes connects again; a probe finds the recovery.
            refusing.store(false, Ordering::SeqCst);
            let _got = waiter.join().unwrap();
        });
        assert_eq!(sessions.lock().limit, 2);
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
            // A few refused connects go by before the cap is learned;
            // hold the session out until they have, or the waiter takes
            // it and the server is never asked again.
            for _ in 0..100 {
                if sessions.lock().refusals >= REFUSALS {
                    break;
                }
                std::thread::sleep(RETRY / 4);
            }
            assert!(
                sessions.lock().refusals >= REFUSALS,
                "the refusal cap was never learned"
            );
            assert!(!waiter.is_finished());
            drop(first);
            assert!(waiter.join().unwrap());
        });
        assert_eq!(sessions.lock().limit, 1);
    }

    #[test]
    fn a_panicking_connect_gives_its_slot_back() {
        let Some(server) = server() else {
            eprintln!("sftp-server not found; set FOLDOVER_SFTP_SERVER to run SFTP tests");
            return;
        };
        let panicking: Connect = Box::new(|| panic!("the connect failed loudly"));
        let sessions = Sessions::new(spawn(&server).unwrap(), 2);
        let _held = sessions.borrow(Some(&panicking));

        let panicked = panic::catch_unwind(AssertUnwindSafe(|| {
            sessions.borrow(Some(&panicking));
        }));
        assert!(panicked.is_err());
        assert_eq!(sessions.lock().open, 1);
    }
}
