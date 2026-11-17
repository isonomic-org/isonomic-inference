// Copyright 2026 Anud Inc.
// SPDX-License-Identifier: GPL-3.0-or-later

//! Worker lifecycle state and the restart policy numbers.

/// Lifecycle state of a supervised worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    Stopped,
    Starting,
    Healthy,
    /// Circuit breaker tripped: more than 5 restarts in 10 minutes.
    Degraded,
}

impl ServiceState {
    /// Stable string form.
    pub fn as_str(&self) -> &'static str {
        match self {
            ServiceState::Stopped => "stopped",
            ServiceState::Starting => "starting",
            ServiceState::Healthy => "healthy",
            ServiceState::Degraded => "degraded",
        }
    }

    /// Inverse of `as_str`.
    pub fn parse(value: &str) -> Option<ServiceState> {
        match value {
            "stopped" => Some(ServiceState::Stopped),
            "starting" => Some(ServiceState::Starting),
            "healthy" => Some(ServiceState::Healthy),
            "degraded" => Some(ServiceState::Degraded),
            _ => None,
        }
    }
}

/// Exponential backoff for restarts: 1s → 2s → 4s → … capped at 60s.
pub fn restart_backoff_secs(attempt: u32) -> u64 {
    let raw = 1u64.checked_shl(attempt).unwrap_or(u64::MAX);
    raw.min(60)
}

/// Circuit breaker: more than 5 restarts inside a 10-minute window → Degraded.
pub fn should_trip_breaker(restarts_in_window: u32) -> bool {
    restarts_in_window > 5
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_then_caps_at_60() {
        assert_eq!(restart_backoff_secs(0), 1);
        assert_eq!(restart_backoff_secs(1), 2);
        assert_eq!(restart_backoff_secs(2), 4);
        assert_eq!(restart_backoff_secs(5), 32);
        assert_eq!(restart_backoff_secs(6), 60);
        assert_eq!(restart_backoff_secs(63), 60);
        assert_eq!(restart_backoff_secs(64), 60); // overflow guard
    }

    #[test]
    fn breaker_trips_after_five() {
        assert!(!should_trip_breaker(5));
        assert!(should_trip_breaker(6));
    }

    #[test]
    fn state_strings_round_trip() {
        for state in [
            ServiceState::Stopped,
            ServiceState::Starting,
            ServiceState::Healthy,
            ServiceState::Degraded,
        ] {
            assert_eq!(ServiceState::parse(state.as_str()), Some(state));
        }
        assert_eq!(ServiceState::Stopped.as_str(), "stopped");
        assert_eq!(ServiceState::Degraded.as_str(), "degraded");
        assert_eq!(ServiceState::parse("nonsense"), None);
    }
}
