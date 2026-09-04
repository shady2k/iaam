//! What a report is for: the four names both sides of the answer join on.
//!
//! **Why the list is neither the reports' nor the queue's.** A report says which
//! goal it answers — [`crate::report::confidence::ReportConfidence`] carries one
//! — and the outstanding-work queue in `iaam-app` grades every item by the goals
//! it stands in the way of. That pairing is the whole promise of the vocabulary:
//! a caller holding a report with caveats can ask the queue what closes them, by
//! name, instead of reading the queue whole and guessing which of its entries
//! are about the report in front of him.
//!
//! For several waves the two sides each declared their own enum, with the same
//! four variants and the same four [`ReportGoal::code`] strings, and nothing but
//! the wire joined them. A spelling changed on one side, or a fifth name added
//! to one side, would have reached a client as a filter that matches nothing —
//! and an empty queue reads as «nothing is in your way», which is the one answer
//! this system must never give by accident. So neither side owns the list: it
//! lives here, where both already depend, exactly as [`crate::operation`] does
//! for the names of calls.
//!
//! **This module needs less pleading than that one, not more.**
//! [`crate::operation::OperationKey`] is a vocabulary of symbols the core never
//! resolves — the method, the path and the request schema belong to the
//! transport, and the core holds only the name, in the way
//! [`crate::report::confidence::CaveatKind::see`] holds a path into a response
//! the core never builds. A goal is not like that. All four folds are computed
//! in this crate — [`crate::report::balances`], [`crate::projection::money_flow`],
//! [`crate::returns`], [`crate::reconciliation`] — so the core is naming its own
//! answers, and needs no argument for doing so. What the core does **not** own
//! is the queue that grades work by these names, and that is the only reason the
//! list is not a private detail of the report modules.
//!
//! It is also why the names do not sit in [`crate::report`]: three of the four
//! folds live outside that module, and the confidence register that carries a
//! goal is one of the two sides that must agree about it, not the owner of what
//! they agree on.

/// One report the owner is trying to reach.
///
/// The four are the whole vocabulary, and they are the four answers this
/// workspace computes. A fifth name would be a goal no code produces, and a
/// queue whose goals do not match the reports is worse than a queue with no
/// goals at all, because it would be believed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReportGoal {
    /// What the owner holds, at a date: cash and positions by account.
    /// Folded by [`crate::report::balances`].
    AssetSnapshot,
    /// Where money came from and went, over an interval. Folded by
    /// [`crate::projection::money_flow`].
    MoneyFlow,
    /// What the money earned. Folded by [`crate::returns`].
    Returns,
    /// Whether the journal agrees with what the sources say. Folded by
    /// [`crate::reconciliation`].
    Reconciliation,
}

impl ReportGoal {
    /// Every goal, in the order this vocabulary is published.
    ///
    /// Listed so that a caller enumerating the four cannot publish three: the
    /// discovery catalog names the route that answers each goal, and it walks
    /// this array rather than a list of its own.
    pub const ALL: [Self; 4] = [
        Self::AssetSnapshot,
        Self::MoneyFlow,
        Self::Returns,
        Self::Reconciliation,
    ];

    /// The machine-readable name carried to a caller.
    ///
    /// One spelling, which is the point of the module: an item's `goals`, a
    /// report's confidence register and the discovery catalog's tag on each
    /// report route all publish these strings, and a client joins the three on
    /// them.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::AssetSnapshot => "asset_snapshot",
            Self::MoneyFlow => "money_flow",
            Self::Returns => "returns",
            Self::Reconciliation => "reconciliation",
        }
    }

    /// What this report answers, in one line, for a reader who has only the
    /// code beside it.
    ///
    /// **Not a script** (`iaam-i3nx`). It fixes what has to be conveyed about a
    /// report and leaves the wording to whoever is speaking, which is decision
    /// 0036's settlement for every published question: a caller that says this
    /// in the owner's own register has obeyed it, and one that quotes it at him
    /// has not disobeyed it either.
    ///
    /// **Here rather than beside one of its two publishers, and that is the
    /// whole reason it moved.** The sentences were written for the discovery
    /// catalog, where a cold client reads the four names and decides where to
    /// go. The outstanding-work queue now publishes the same four names beside
    /// what stands in the way of each, and the far end of that response is a
    /// person. A second set of sentences would be two answers to «what is
    /// `returns`?» from one system, differing by however much two authors
    /// differ — which is the defect [`Self::code`] was moved here to prevent,
    /// one column over.
    ///
    /// Prose, and derived from nothing on purpose: the code beside it is the
    /// join, so a sentence that drifts costs a reader a sentence, where a code
    /// that drifts costs a client the join.
    #[must_use]
    pub const fn answers(self) -> &'static str {
        match self {
            Self::AssetSnapshot => {
                "What the owner holds at a date: cash and positions, and the whole."
            }
            Self::MoneyFlow => "Where money came from and where it went, over an interval.",
            Self::Returns => "What the money earned, before tax.",
            Self::Reconciliation => "Whether the journal agrees with what the sources say.",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The codes are the join, so two goals sharing one would silently merge two
    /// filters into one, and a name missing from [`ReportGoal::ALL`] is a goal
    /// the discovery catalog never publishes a route for.
    #[test]
    fn every_goal_is_listed_once_and_named_once() {
        let mut codes: Vec<&str> = ReportGoal::ALL.iter().map(|goal| goal.code()).collect();
        let listed = codes.len();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), listed, "two goals share a name");
        assert_eq!(
            ReportGoal::ALL.map(ReportGoal::code),
            ["asset_snapshot", "money_flow", "returns", "reconciliation"],
            "the four agreed names changed; every client filtering by goal breaks"
        );
    }

    /// The sentence exists for a reader holding the code and nothing else, so
    /// three ways of giving him nothing are refused rather than assumed: a
    /// sentence spelled in this system's own words, a sentence that is the code
    /// again, and one sentence doing duty for two goals.
    ///
    /// Asserted against the codes and against each other, never against four
    /// strings written out here — a list of expected sentences passes by being
    /// edited to whatever the sentences became, which proves nothing about
    /// them.
    #[test]
    fn every_goal_says_what_it_answers_without_using_this_systems_own_words() {
        let mut said: Vec<&str> = Vec::new();
        for goal in ReportGoal::ALL {
            let answers = goal.answers();
            assert!(
                !answers.contains('_') && !answers.contains('/'),
                "«{answers}» shows a wire name to whoever is read it"
            );
            assert!(
                !answers.contains(goal.code()),
                "«{answers}» answers «what is {}?» with «{}»",
                goal.code(),
                goal.code()
            );
            assert!(
                answers.ends_with('.') && answers.split_whitespace().count() > 4,
                "«{answers}» is a label and not a sentence"
            );
            said.push(answers);
        }
        let distinct = {
            let mut sorted = said.clone();
            sorted.sort_unstable();
            sorted.dedup();
            sorted.len()
        };
        assert_eq!(
            distinct,
            said.len(),
            "two of the four reports are described by one sentence: {said:?}"
        );
    }
}
