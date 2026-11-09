use libc::{pid_t, waitpid, ECHILD, WNOHANG};
use std::{io, os::unix::process::ExitStatusExt, process::ExitStatus};

const MAX_DRAIN_ATTEMPTS: usize = 64;

pub(super) type ProcessId = pid_t;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum DrainOutcome {
    ChildrenRemain,
    NoChildren,
    BudgetExhausted,
}

pub(super) struct ChildReaper {
    handoff_pid: ProcessId,
    handoff_status: Option<ExitStatus>,
}

impl ChildReaper {
    pub(super) fn new(handoff_pid: ProcessId) -> Self {
        Self {
            handoff_pid,
            handoff_status: None,
        }
    }

    pub(super) fn drain(&mut self) -> io::Result<DrainOutcome> {
        self.drain_with(|status| {
            let result = unsafe { waitpid(-1, status, WNOHANG) };
            if result < 0 {
                Err(io::Error::last_os_error())
            } else {
                Ok(result)
            }
        })
    }

    pub(super) fn handoff_exited(&self) -> bool {
        self.handoff_status.is_some()
    }

    pub(super) fn take_handoff_status(&mut self) -> io::Result<ExitStatus> {
        self.handoff_status
            .take()
            .ok_or_else(missing_handoff_status)
    }

    fn drain_with(
        &mut self,
        mut wait: impl FnMut(&mut i32) -> io::Result<ProcessId>,
    ) -> io::Result<DrainOutcome> {
        for _ in 0..MAX_DRAIN_ATTEMPTS {
            let mut status = 0;
            match wait(&mut status) {
                Ok(0) => return Ok(DrainOutcome::ChildrenRemain),
                Ok(pid) => {
                    self.record_wait_status(pid, status);
                }
                Err(error) if error.raw_os_error() == Some(ECHILD) => {
                    return Ok(DrainOutcome::NoChildren);
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }
        Ok(DrainOutcome::BudgetExhausted)
    }

    fn record_wait_status(&mut self, pid: ProcessId, status: i32) {
        debug_assert!(pid > 0, "waitpid returned a non-child PID");
        if pid != self.handoff_pid || self.handoff_status.is_some() {
            return;
        }
        self.handoff_status = Some(ExitStatus::from_raw(status));
    }
}

fn missing_handoff_status() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "initial handoff exit status was not collected",
    )
}

#[cfg(test)]
mod tests {
    use super::{ChildReaper, DrainOutcome, ProcessId};
    use libc::{ECHILD, EINTR, EIO, SIGTERM};
    use std::{collections::VecDeque, io, os::unix::process::ExitStatusExt, process::ExitStatus};

    struct WaitStep {
        result: io::Result<ProcessId>,
        status: i32,
    }

    fn run_steps(
        reaper: &mut ChildReaper,
        steps: impl IntoIterator<Item = WaitStep>,
    ) -> io::Result<DrainOutcome> {
        let mut steps = steps.into_iter().collect::<VecDeque<_>>();
        reaper.drain_with(|status| {
            let step = steps.pop_front().expect("test wait sequence was exhausted");
            *status = step.status;
            step.result
        })
    }

    fn step(pid: ProcessId, status: i32) -> WaitStep {
        WaitStep {
            result: Ok(pid),
            status,
        }
    }

    fn error(errno: i32) -> WaitStep {
        WaitStep {
            result: Err(io::Error::from_raw_os_error(errno)),
            status: 0,
        }
    }

    #[test]
    fn drain_collects_handoff_once_and_discards_other_child_statuses() {
        let mut reaper = ChildReaper::new(42);
        let outcome = run_steps(
            &mut reaper,
            [step(51, 7 << 8), step(42, 23 << 8), step(0, 0)],
        )
        .unwrap();

        assert_eq!(outcome, DrainOutcome::ChildrenRemain);
        assert!(reaper.handoff_exited());
        assert_eq!(reaper.take_handoff_status().unwrap().code(), Some(23));
        assert!(reaper.take_handoff_status().is_err());
    }

    #[test]
    fn drain_preserves_signal_derived_exit_status() {
        let mut reaper = ChildReaper::new(42);
        let outcome = run_steps(&mut reaper, [step(42, SIGTERM), error(ECHILD)]).unwrap();

        assert_eq!(outcome, DrainOutcome::NoChildren);
        let status = reaper.take_handoff_status().unwrap();
        assert_eq!(status.code(), None);
        assert_eq!(status.signal(), Some(SIGTERM));
    }

    #[test]
    fn drain_retries_interrupted_wait_and_propagates_other_errors() {
        let mut reaper = ChildReaper::new(42);
        let outcome = run_steps(&mut reaper, [error(EINTR), error(ECHILD)]).unwrap();
        assert_eq!(outcome, DrainOutcome::NoChildren);

        let mut reaper = ChildReaper::new(42);
        let error = run_steps(&mut reaper, [error(EIO)]).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(EIO));
    }

    #[test]
    fn echild_is_distinct_from_no_exited_child_and_missing_handoff_is_an_error() {
        let mut reaper = ChildReaper::new(42);
        assert_eq!(
            run_steps(&mut reaper, [step(0, 0)]).unwrap(),
            DrainOutcome::ChildrenRemain
        );
        assert!(!reaper.handoff_exited());
        assert_eq!(
            run_steps(&mut reaper, [error(ECHILD)]).unwrap(),
            DrainOutcome::NoChildren
        );
        assert!(reaper.take_handoff_status().is_err());
    }

    #[test]
    fn drain_yields_after_a_bounded_number_of_wait_attempts() {
        let mut reaper = ChildReaper::new(42);
        let mut attempts = 0;
        let outcome = reaper
            .drain_with(|_| {
                attempts += 1;
                Ok(51)
            })
            .unwrap();

        assert_eq!(outcome, DrainOutcome::BudgetExhausted);
        assert_eq!(attempts, 64);
    }

    #[test]
    fn reused_handoff_pid_is_treated_as_another_child_after_handoff_exit() {
        let mut reaper = ChildReaper::new(42);
        let outcome = run_steps(
            &mut reaper,
            [step(42, 23 << 8), step(42, 7 << 8), error(ECHILD)],
        )
        .unwrap();

        assert_eq!(outcome, DrainOutcome::NoChildren);
        assert_eq!(reaper.take_handoff_status().unwrap().code(), Some(23));
    }

    #[test]
    fn exit_status_conversion_uses_unix_wait_status() {
        let status = ExitStatus::from_raw(9 << 8);
        assert_eq!(status.code(), Some(9));
    }
}
