//! Retention decisions are independent of process I/O. Unknown ownership is
//! deliberately different from a confirmed closed terminal.

use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Owner {
    Live,
    Closed,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Reason {
    SourceClosed,
    Expired,
}

pub(super) fn decide(
    owner: Owner,
    attached: bool,
    last_used: u64,
    now: u64,
    ttl_seconds: u64,
) -> Option<Reason> {
    if owner == Owner::Closed {
        Some(Reason::SourceClosed)
    } else if !attached && now.saturating_sub(last_used) >= ttl_seconds {
        Some(Reason::Expired)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closed_source_releases_even_recent_or_attached_jobs() {
        assert_eq!(
            decide(Owner::Closed, true, 100, 101, 86400),
            Some(Reason::SourceClosed)
        );
    }

    #[test]
    fn ttl_is_since_last_use_and_only_expires_hidden_sessions() {
        assert_eq!(
            decide(Owner::Live, false, 100, 86500, 86400),
            Some(Reason::Expired)
        );
        assert_eq!(decide(Owner::Live, false, 101, 86500, 86400), None);
        assert_eq!(decide(Owner::Live, true, 100, 86500, 86400), None);
    }

    #[test]
    fn unavailable_owner_is_not_closed_but_independent_ttl_still_applies() {
        assert_eq!(decide(Owner::Unknown, false, 100, 101, 86400), None);
        assert_eq!(
            decide(Owner::Unknown, false, 100, 86500, 86400),
            Some(Reason::Expired)
        );
    }

    #[test]
    fn clock_rollback_does_not_expire_sessions() {
        assert_eq!(decide(Owner::Live, false, 200, 100, 86400), None);
    }
}
