//! The one order a merged event log replays in.
//!
#![doc = include_str!("../docs/modules/merge.md")]

/// The sort key that turns a union of event batches into a sequence.
///
/// Ordered by declaration: timestamp first, then the store that wrote the
/// event, then that store's own event id. Every field is a property of the
/// event itself, so the key — and therefore the order — is a function of the
/// set, never of the delivery schedule.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct MergeKey {
    ts: String,
    origin_uuid: String,
    origin_id: i64,
}

impl MergeKey {
    pub fn new(ts: &str, origin_uuid: &str, origin_id: i64) -> Self {
        Self {
            ts: canonical_ts(ts),
            origin_uuid: origin_uuid.to_string(),
            origin_id,
        }
    }
}

/// Puts a merged event stream into replay order.
///
/// Generic over the event type so the ordering has exactly one definition:
/// a second copy living next to a consumer would drift, and the copy that
/// drifts is the one whose convergence property then proves nothing.
pub fn sort_for_replay<T>(events: &mut [T], key: impl FnMut(&T) -> MergeKey) {
    events.sort_by_cached_key(key);
}

/// Digits the fractional-seconds field is padded to before comparison.
const FRAC_DIGITS: usize = 9;

/// Rewrites a timestamp so a plain string compare is a chronological compare.
///
/// The store writes `strftime('%Y-%m-%dT%H:%M:%fZ')`, which is fixed width and
/// already sorts correctly. A batch from somewhere else need not be: `…:23.7Z`
/// sorts *after* `…:23.737Z` byte for byte, because `Z` is greater than `3`.
/// Padding the fraction to a fixed width removes that trap without parsing —
/// the order then carries no date-library semantics and has no failure mode for
/// a timestamp nothing can parse.
///
/// Anything not of the form `<datetime>[.<digits>]Z` is returned untouched. It
/// still sorts deterministically, which is what convergence needs; whether it
/// sorts *causally* is what the transition guard is there to find out.
pub fn canonical_ts(ts: &str) -> String {
    let Some(body) = ts.strip_suffix('Z') else {
        return ts.to_string();
    };
    let (head, frac) = match body.split_once('.') {
        None => (body, ""),
        Some((head, frac)) => (head, frac),
    };
    if !frac.chars().all(|c| c.is_ascii_digit()) {
        return ts.to_string();
    }
    let mut padded = frac.to_string();
    padded.truncate(FRAC_DIGITS);
    while padded.len() < FRAC_DIGITS {
        padded.push('0');
    }
    format!("{head}.{padded}Z")
}

/// Every edge the task state machine has, as the guarded `UPDATE`s spell them.
///
/// The list is the closure of the guards in `db` and `cmd`: `assign` matches
/// only `ready`, `claim` only `assigned`, `complete` only `running`; the
/// release commands match `assigned`/`running`; `cancel` matches anything
/// non-terminal; and the dependency sweeps move a task between `pending` and
/// `ready` in both directions. `done` and `cancelled` are terminal and appear
/// on no left-hand side, which is what makes a resurrection detectable.
///
/// Creation is deliberately absent: it has no origin state, so it is not an
/// edge. `is_birth_state` covers the states a task may be born into.
pub const TRANSITIONS: &[(&str, &str)] = &[
    ("pending", "ready"),
    ("pending", "cancelled"),
    ("ready", "pending"),
    ("ready", "assigned"),
    ("ready", "cancelled"),
    ("assigned", "running"),
    ("assigned", "pending"),
    ("assigned", "cancelled"),
    ("running", "done"),
    ("running", "pending"),
    ("running", "cancelled"),
];

/// Whether the state machine has an edge from `from` to `to`.
///
/// This is a question about the *pair*, not about where the projection happens
/// to be. A replay that only checks continuity — that the task is in `from` —
/// accepts an event recording a move the machine cannot make, and applies it,
/// because the projection is in the state the event claims. That is precisely
/// the illegal `pending -> assigned` hop class found in pre-fidelity logs, and
/// merging such a ledger would carry it into a healthy store.
pub fn is_legal_transition(from: &str, to: &str) -> bool {
    TRANSITIONS.contains(&(from, to))
}

/// Whether a task in this state holds an open assignment.
///
/// The states, not the exits: `assigned` and `running` are the two a claim
/// spans, and everything else is outside it. Consumers that ask "does this move
/// release the claim?" should ask it of the ORIGIN — leaving a held state
/// releases it, whatever the destination, with `assigned` -> `running` the one
/// edge that stays inside. Enumerating the destinations instead is what left
/// replay with no arm for a historic `running` -> `ready` hop and an assignment
/// row open forever behind it.
pub fn holds_assignment(state: &str) -> bool {
    matches!(state, "assigned" | "running")
}

/// Whether a task may come into existence already in this state.
pub fn is_birth_state(state: &str) -> bool {
    matches!(state, "pending" | "ready")
}

/// The sentence every refusal ends with, so the reader knows where to look.
///
/// A merged order is only as causal as the clocks that fed it, so the first
/// question a refusal has to answer is "whose clock is wrong?" — which is why
/// the refusals name both stores and both timestamps rather than only the event
/// that failed.
pub const SKEW_HINT: &str =
    "Merged events replay in (timestamp, origin store, event id) order, so \
     if those two timestamps came from different machines, compare their clocks \
     before suspecting the log.";

#[cfg(test)]
mod tests {
    use super::*;

    /// A same-millisecond burst keeps its store's order.
    ///
    /// A store's own events must stay in the order that store wrote them, even
    /// when a whole burst of them shares one millisecond.
    ///
    /// This is the third key earning its place. Without it a task's `ready` and
    /// `assigned` — written in the same millisecond by the same store, which is
    /// ordinary — tie completely, and the sequence they replay in becomes a
    /// property of whatever fed the sort rather than of the log.
    /// `assigned` -> `running` is the only edge that stays inside a claim.
    ///
    /// `import`'s release arm is keyed on the origin state and excepts exactly
    /// that one destination. If the machine ever grows a second edge between
    /// two held states, that arm silently starts closing a live claim — so the
    /// assumption is asserted here, next to the table it is an assumption about.
    #[test]
    fn the_only_move_between_two_held_states_is_the_claim() {
        let inner: Vec<&(&str, &str)> = TRANSITIONS
            .iter()
            .filter(|(f, t)| holds_assignment(f) && holds_assignment(t))
            .collect();
        assert_eq!(inner, vec![&("assigned", "running")]);
    }

    #[test]
    fn same_millisecond_events_from_one_store_keep_their_original_order() {
        let ts = "2026-01-01T00:00:00.001Z";
        let store = "11111111-1111-4111-8111-111111111111";
        let mut evs: Vec<i64> = vec![5, 2, 4, 1, 3];
        sort_for_replay(&mut evs, |id| MergeKey::new(ts, store, *id));
        assert_eq!(
            evs,
            vec![1, 2, 3, 4, 5],
            "same-store, same-millisecond events lost their original order"
        );
    }

    /// The whole key, in priority order, and the same on every machine.
    #[test]
    fn timestamp_beats_store_which_beats_event_id() {
        let (early, late) = ("2026-01-01T00:00:00.001Z", "2026-01-01T00:00:00.002Z");
        let (a, b) = (
            "aaaaaaaa-0000-4000-8000-000000000000",
            "bbbbbbbb-0000-4000-8000-000000000000",
        );

        // A late event with the lowest id from the first store still sorts last.
        assert!(MergeKey::new(early, b, 999) < MergeKey::new(late, a, 1));
        // Same instant: the store breaks the tie, whatever the ids say.
        assert!(MergeKey::new(early, a, 999) < MergeKey::new(early, b, 1));
        // Same instant and same store: the event id breaks the tie.
        assert!(MergeKey::new(early, a, 1) < MergeKey::new(early, a, 2));
    }

    /// A shorter fractional part must not sort after a longer one.
    ///
    /// Byte for byte `…00.7Z` sorts *after* `…00.737Z`, because `Z` outranks
    /// `3`, while 700ms is plainly earlier than 737ms. A ledger written by
    /// anything but this store can carry either form.
    #[test]
    fn a_short_fraction_sorts_before_a_longer_one_at_the_same_instant() {
        let store = "11111111-1111-4111-8111-111111111111";
        assert!("2026-01-01T00:00:00.7Z" > "2026-01-01T00:00:00.737Z");
        assert!(
            MergeKey::new("2026-01-01T00:00:00.7Z", store, 1)
                < MergeKey::new("2026-01-01T00:00:00.737Z", store, 2),
            "700ms is earlier than 737ms, whatever a byte compare says"
        );
        assert!(
            MergeKey::new("2026-01-01T00:00:00Z", store, 1)
                < MergeKey::new("2026-01-01T00:00:00.001Z", store, 2),
            "a missing fraction is the start of the second, not the end of it"
        );
        // Unparseable stays put rather than becoming a special case.
        assert_eq!(canonical_ts("not a timestamp"), "not a timestamp");
    }

    /// The edge table refuses what the guards refuse.
    ///
    /// It is the state machine, so the moves the machine cannot make have to be
    /// absent — including every resurrection of a terminal task.
    #[test]
    fn the_edge_table_refuses_the_moves_the_guards_refuse() {
        assert!(is_legal_transition("ready", "assigned"));
        assert!(is_legal_transition("assigned", "running"));
        assert!(is_legal_transition("running", "done"));

        // The hop that a continuity-only check lets through.
        assert!(!is_legal_transition("pending", "assigned"));
        assert!(!is_legal_transition("ready", "running"));
        assert!(!is_legal_transition("ready", "done"));
        // Terminal is terminal.
        for to in ["pending", "ready", "assigned", "running", "cancelled"] {
            assert!(!is_legal_transition("done", to), "done -> {to}");
            assert!(!is_legal_transition("cancelled", to), "cancelled -> {to}");
        }
        // A state nothing has ever written is not quietly allowed.
        assert!(!is_legal_transition("ready", "archived"));
    }
}
