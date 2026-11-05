//! Programs wayluck starts, and the open files limit: wayluck raises its
//! own, since every window costs it open files, but what it starts gets
//! the limit wayluck itself started with. Some programs misbehave with
//! file descriptors above 1024 (select(), and fixed-size sets of them).

use std::{ffi::OsStr, os::unix::process::CommandExt, process::Command, sync::OnceLock};

use rustix::process::{Resource, Rlimit, getrlimit, setrlimit};

/// The limit wayluck started with, once it raised its own.
static STARTED_WITH: OnceLock<Rlimit> = OnceLock::new();

/// A session's usual limit, 1024, runs out at around a hundred windows:
/// their clients' connections, their buffers, their sync points. Raised as
/// far as the system allows (the hard limit).
pub fn raise_open_files_limit() {
    let before = getrlimit(Resource::Nofile);
    if before.current == before.maximum {
        return;
    }
    let raised = Rlimit {
        current: before.maximum,
        ..before
    };
    match setrlimit(Resource::Nofile, raised) {
        Ok(()) => {
            let _ = STARTED_WITH.set(before);
            tracing::info!(
                "open files: up to {:?} (was {:?})",
                raised.current,
                before.current
            );
        }
        Err(e) => tracing::warn!(
            "can't raise the open files limit ({:?}): {e}",
            before.current
        ),
    }
}

/// A command for program, which runs with the open files limit wayluck
/// started with.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    let mut command = Command::new(program);
    if let Some(&limit) = STARTED_WITH.get() {
        // SAFETY: runs in the child, between fork and exec, where only
        // async-signal-safe calls may be made: setrlimit is one system
        // call, and nothing here allocates or takes a lock.
        unsafe {
            command
                .pre_exec(move || setrlimit(Resource::Nofile, limit).map_err(std::io::Error::from));
        }
    }
    command
}
