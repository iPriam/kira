//! The devflow verb set and (once ported) their implementations.
//!
//! Port target: kira-zig `kira_devflow/src/commands.zig`.

/// Every verb devflow accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    Status,
    Commit,
    Push,
    PrScope,
    OpenForkPr,
    RequestReviews,
    WaitCi,
    CiFailures,
    CiRunners,
    RerunCi,
    ReviewFindings,
    WaitReviews,
    ResolveThread,
    Land,
    Sync,
    OpenUpstreamPr,
    NextVersion,
    ReleasePrep,
    Release,
}

/// All verbs, in help order.
pub const ALL: [Verb; 19] = [
    Verb::Status,
    Verb::Commit,
    Verb::Push,
    Verb::PrScope,
    Verb::OpenForkPr,
    Verb::RequestReviews,
    Verb::WaitCi,
    Verb::CiFailures,
    Verb::CiRunners,
    Verb::RerunCi,
    Verb::ReviewFindings,
    Verb::WaitReviews,
    Verb::ResolveThread,
    Verb::Land,
    Verb::Sync,
    Verb::OpenUpstreamPr,
    Verb::NextVersion,
    Verb::ReleasePrep,
    Verb::Release,
];

impl Verb {
    pub fn parse(text: &str) -> Option<Self> {
        ALL.iter().copied().find(|verb| verb.label() == text)
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::Commit => "commit",
            Self::Push => "push",
            Self::PrScope => "pr-scope",
            Self::OpenForkPr => "open-fork-pr",
            Self::RequestReviews => "request-reviews",
            Self::WaitCi => "wait-ci",
            Self::CiFailures => "ci-failures",
            Self::CiRunners => "ci-runners",
            Self::RerunCi => "rerun-ci",
            Self::ReviewFindings => "review-findings",
            Self::WaitReviews => "wait-reviews",
            Self::ResolveThread => "resolve-thread",
            Self::Land => "land",
            Self::Sync => "sync",
            Self::OpenUpstreamPr => "open-upstream-pr",
            Self::NextVersion => "next-version",
            Self::ReleasePrep => "release-prep",
            Self::Release => "release",
        }
    }
}
