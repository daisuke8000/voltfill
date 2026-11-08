//! A work budget with an optional wall-clock deadline.
//!
//! Callers count work in units (bytes of text processed, fragments
//! examined) and spend them before doing the work. The deadline is checked
//! at least once every [`CHECK_INTERVAL`] units and on every
//! [`Budget::check_deadline`] call, so a long loop stops soon after the
//! deadline instead of running to completion.
use std::time::Instant;

/// Work units between deadline checks.
pub const CHECK_INTERVAL: u64 = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum BudgetError {
    #[error("work budget exceeded")]
    WorkExceeded,
    #[error("deadline exceeded")]
    DeadlineExceeded,
}

/// Remaining work units and the deadline.
#[derive(Debug, Clone)]
pub struct Budget {
    remaining: u64,
    deadline: Option<Instant>,
    until_check: u64,
}

impl Budget {
    #[must_use]
    pub fn new(max_work: u64, deadline: Option<Instant>) -> Self {
        Self {
            remaining: max_work,
            deadline,
            until_check: CHECK_INTERVAL,
        }
    }

    /// Spend `units`, checking the deadline once enough units have passed.
    ///
    /// # Errors
    /// [`BudgetError::WorkExceeded`] when fewer than `units` remain and
    /// [`BudgetError::DeadlineExceeded`] once the deadline has passed.
    pub fn spend(&mut self, units: u64) -> Result<(), BudgetError> {
        self.remaining = self
            .remaining
            .checked_sub(units)
            .ok_or(BudgetError::WorkExceeded)?;
        if units >= self.until_check {
            self.until_check = CHECK_INTERVAL;
            self.check_deadline()
        } else {
            self.until_check -= units;
            Ok(())
        }
    }

    /// Spend one unit per byte of `text`.
    ///
    /// # Errors
    /// As [`Budget::spend`].
    pub fn spend_bytes(&mut self, text: &str) -> Result<(), BudgetError> {
        self.spend(u64::try_from(text.len()).unwrap_or(u64::MAX))
    }

    /// # Errors
    /// [`BudgetError::DeadlineExceeded`] once the deadline has passed.
    pub fn check_deadline(&self) -> Result<(), BudgetError> {
        match self.deadline {
            Some(deadline) if Instant::now() >= deadline => Err(BudgetError::DeadlineExceeded),
            _ => Ok(()),
        }
    }

    /// Units not spent yet.
    #[must_use]
    pub fn remaining(&self) -> u64 {
        self.remaining
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn spending_past_the_budget_or_deadline_fails() {
        let mut budget = Budget::new(10, None);
        assert_eq!(budget.spend(4), Ok(()));
        assert_eq!(budget.spend_bytes("abcdef"), Ok(()));
        assert_eq!(budget.remaining(), 0);
        assert_eq!(budget.spend(1), Err(BudgetError::WorkExceeded));
        let mut late = Budget::new(u64::MAX, Some(Instant::now()));
        assert_eq!(late.check_deadline(), Err(BudgetError::DeadlineExceeded));
        // Small spends check the deadline once per interval.
        for _ in 0..CHECK_INTERVAL - 1 {
            assert_eq!(late.spend(1), Ok(()));
        }
        assert_eq!(late.spend(1), Err(BudgetError::DeadlineExceeded));
        let mut early = Budget::new(u64::MAX, Some(Instant::now() + Duration::from_secs(60)));
        assert_eq!(early.spend(CHECK_INTERVAL * 2), Ok(()));
    }
}
