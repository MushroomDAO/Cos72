//! Pure task state machine + field validation (docs/agent/architecture.md
//! 「系统骨架」`core/`: "纯类型 + 状态机转移函数（无 IO）"). No sqlx, no axum,
//! no `KernelPort` — `store::tasks` and `http::tasks` are the only callers,
//! and both only ever see [`TaskStatus`]/[`TaskAction`]/[`next_status`], never
//! duplicate the transition table themselves.

use std::fmt;

/// `tasks.status` (docs/agent/spec.md「状态机」「任务 tasks.status」).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskStatus {
    Open,
    Claimed,
    Submitted,
    Completed,
}

impl TaskStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Claimed => "claimed",
            Self::Submitted => "submitted",
            Self::Completed => "completed",
        }
    }

    /// `None` for anything outside the migration's own `CHECK(status IN
    /// (...))` set — a row read back out of `tasks` should never fail this,
    /// but callers (`store::tasks`) still get a `Result`, not a panic.
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "open" => Some(Self::Open),
            "claimed" => Some(Self::Claimed),
            "submitted" => Some(Self::Submitted),
            "completed" => Some(Self::Completed),
            _ => None,
        }
    }

    /// All four values, in a fixed order — the exhaustive transition-table
    /// test iterates this so "4 states" stays true even if a fifth status is
    /// ever added (the test would then need a fifth arm added deliberately,
    /// rather than silently covering only 3).
    #[must_use]
    pub const fn all() -> [Self; 4] {
        [Self::Open, Self::Claimed, Self::Submitted, Self::Completed]
    }
}

impl fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The two T1.2.1-scope actions (docs/agent/tasks.md T1.2.1「明确不做」:
/// no cancel/unclaim/deadline actions this task).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskAction {
    Claim,
    Submit,
}

impl TaskAction {
    #[must_use]
    pub const fn all() -> [Self; 2] {
        [Self::Claim, Self::Submit]
    }
}

/// A structural transition was rejected. Does NOT cover the actor check
/// ("is this member the claimer?") — that is `store::tasks::SubmitOutcome::
/// NotClaimer`, checked separately by `store::tasks::submit_task` after the
/// structural transition is known to be legal (docs/agent/spec.md「状态机」
/// submit precondition table: wrong status is 409, wrong actor is 403 — two
/// different failure modes, kept as two different variants so a caller
/// cannot accidentally conflate them).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidTransition;

/// The single source of truth for which `(status, action)` pairs are legal
/// (docs/agent/spec.md「状态机」table). `store::tasks` uses this same
/// function to decide which CAS `UPDATE ... WHERE status = ?` to attempt —
/// it never hand-rolls a second copy of this table in SQL.
///
/// # Errors
/// [`InvalidTransition`] for any pair not in spec.md's table.
pub fn next_status(
    current: TaskStatus,
    action: TaskAction,
) -> Result<TaskStatus, InvalidTransition> {
    match (current, action) {
        (TaskStatus::Open, TaskAction::Claim) => Ok(TaskStatus::Claimed),
        (TaskStatus::Claimed, TaskAction::Submit) => Ok(TaskStatus::Submitted),
        _ => Err(InvalidTransition),
    }
}

/// `title`: 1..=200 chars after trimming, non-empty (docs/agent/spec.md「标识
/// 与校验」).
#[must_use]
pub fn is_valid_title(title: &str) -> bool {
    let trimmed = title.trim();
    !trimmed.is_empty() && trimmed.chars().count() <= 200
}

/// `description` / `evidence`: 0..=4000 chars (docs/agent/spec.md「标识与校
/// 验」) — empty is allowed (unlike title).
#[must_use]
pub fn is_valid_long_text(text: &str) -> bool {
    text.chars().count() <= 4000
}

/// `reward_points`: integer 1..=1,000,000 ⚖️ (docs/agent/spec.md「标识与校
/// 验」). The migration's own `CHECK` is the backstop; this is the fast,
/// pre-write rejection so an out-of-range request never reaches the store.
#[must_use]
pub fn is_valid_reward_points(points: i64) -> bool {
    (1..=1_000_000).contains(&points)
}

/// `member` / `publisher`: `^[a-z0-9][a-z0-9_-]{0,63}$` (docs/agent/spec.md
/// 「标识与校验」) — written by hand rather than pulling in a `regex`
/// dependency for one fixed, tiny pattern.
#[must_use]
pub fn is_valid_actor(s: &str) -> bool {
    let mut chars = s.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_lowercase() || first.is_ascii_digit()) {
        return false;
    }
    let rest: Vec<char> = chars.collect();
    rest.len() <= 63
        && rest
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_' || *c == '-')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// docs/agent/tasks.md T1.2.1 验收命令 #1: 4 states × {claim, submit} —
    /// all 8 combinations — the allowed set is EXACTLY the two rows in
    /// spec.md's table, everything else must be [`InvalidTransition`].
    ///
    /// 变异: temporarily add an arm `(TaskStatus::Completed, TaskAction::Claim)
    /// => Ok(TaskStatus::Claimed)` to `next_status` (spec.md 明确 completed 是
    /// 终态) — this test goes red because the exhaustive table below no
    /// longer matches the implementation's behavior for that pair (verified
    /// by hand for this PR; see PR body).
    #[test]
    fn transition_table_is_exhaustive() {
        use TaskAction::{Claim, Submit};
        use TaskStatus::{Claimed, Completed, Open, Submitted};

        type Case = (
            (TaskStatus, TaskAction),
            Result<TaskStatus, InvalidTransition>,
        );

        let expected: &[Case] = &[
            ((Open, Claim), Ok(Claimed)),
            ((Open, Submit), Err(InvalidTransition)),
            ((Claimed, Claim), Err(InvalidTransition)),
            ((Claimed, Submit), Ok(Submitted)),
            ((Submitted, Claim), Err(InvalidTransition)),
            ((Submitted, Submit), Err(InvalidTransition)),
            ((Completed, Claim), Err(InvalidTransition)),
            ((Completed, Submit), Err(InvalidTransition)),
        ];

        // Guard the guard: the fixture itself must actually be the full 4×2
        // cross product, so this test cannot silently shrink if someone
        // trims the `expected` list instead of extending `TaskStatus::all()`.
        assert_eq!(
            expected.len(),
            TaskStatus::all().len() * TaskAction::all().len()
        );

        for &((status, action), want) in expected {
            let got = next_status(status, action);
            assert_eq!(got, want, "next_status({status:?}, {action:?})");
        }
    }

    #[test]
    fn actor_pattern_accepts_and_rejects() {
        for ok in ["a", "a1", "jason", "a-b_c9", &"a".repeat(64)] {
            assert!(is_valid_actor(ok), "{ok:?} should be valid");
        }
        for bad in ["", "Jason", "_a", "-a", &"a".repeat(65), "a b", "a.b"] {
            assert!(!is_valid_actor(bad), "{bad:?} should be invalid");
        }
    }

    #[test]
    fn reward_points_bounds() {
        assert!(!is_valid_reward_points(0));
        assert!(is_valid_reward_points(1));
        assert!(is_valid_reward_points(1_000_000));
        assert!(!is_valid_reward_points(1_000_001));
    }

    #[test]
    fn title_bounds() {
        assert!(!is_valid_title(""));
        assert!(!is_valid_title("   "));
        assert!(is_valid_title("a"));
        assert!(is_valid_title(&"a".repeat(200)));
        assert!(!is_valid_title(&"a".repeat(201)));
    }
}
