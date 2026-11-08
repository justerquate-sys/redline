//! Optimistic two-phase state for one action target (a video's like state, a
//! channel's subscription, a playlist's membership). The UI shows `desired`
//! at once; at most one request is in flight; a failure rolls back to the
//! last state YouTube confirmed. Pure (no bridge), unit-tested.

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Toggle<S: Copy + PartialEq> {
    /// Last state YouTube confirmed (or the page reported).
    pub confirmed: S,
    /// What the user asked for (shown).
    pub desired: S,
    /// Request id and the state it asks for.
    pub inflight: Option<(u32, S)>,
}

impl<S: Copy + PartialEq> Toggle<S> {
    pub fn new(s: S) -> Self {
        Toggle { confirmed: s, desired: s, inflight: None }
    }

    pub fn shown(&self) -> S {
        self.desired
    }

    pub fn busy(&self) -> bool {
        self.inflight.is_some()
    }

    /// YouTube says what the state is now (a quiet check, not an answer to a change): taken unless the user has
    /// asked for something meanwhile. True when it changes what is shown.
    pub fn learn(&mut self, s: S) -> bool {
        if self.busy() || self.desired != self.confirmed || self.desired == s {
            return false;
        }
        self.confirmed = s;
        self.desired = s;
        true
    }

    /// The user asks for `s`. Returns the state to request now, if any (a
    /// change while a request is in flight is sent when it completes).
    pub fn want(&mut self, s: S) -> Option<S> {
        self.desired = s;
        (self.inflight.is_none() && s != self.confirmed).then_some(s)
    }

    pub fn sent(&mut self, id: u32, s: S) {
        self.inflight = Some((id, s));
    }

    pub fn owns(&self, id: u32) -> bool {
        self.inflight.is_some_and(|(i, _)| i == id)
    }

    /// The response for `id`. Success confirms its state and returns the next
    /// state to request if the user changed their mind meanwhile; failure
    /// rolls `desired` back to the confirmed state.
    pub fn done(&mut self, id: u32, ok: bool) -> Option<S> {
        let (i, s) = self.inflight?;
        if i != id {
            return None;
        }
        self.inflight = None;
        if ok {
            self.confirmed = s;
            (self.desired != s).then_some(self.desired)
        } else {
            self.desired = self.confirmed;
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn optimistic_then_confirmed() {
        let mut t = Toggle::new(false);
        assert_eq!(t.want(true), Some(true));
        t.sent(1, true);
        assert!(t.shown() && t.busy());
        assert_eq!(t.done(1, true), None);
        assert!(t.confirmed && t.shown() && !t.busy());
    }
    #[test]
    fn a_quiet_check_never_overrides_the_user() {
        let mut t = Toggle::new(false);
        assert!(t.learn(true));
        assert!(t.confirmed && t.shown());
        assert!(!t.learn(true), "nothing changes");
        t.want(false);
        t.sent(1, false);
        assert!(!t.learn(true), "in flight");
        assert!(!t.shown());
        t.done(1, true);
        assert!(!t.shown() && !t.confirmed);
        // Changed, not sent yet (a queued change of mind).
        let mut t = Toggle::new(false);
        t.confirmed = true;
        assert!(!t.learn(true) && !t.shown());
    }
    #[test]
    fn failure_rolls_back() {
        let mut t = Toggle::new(false);
        t.want(true);
        t.sent(1, true);
        assert_eq!(t.done(2, false), None, "other ids are ignored");
        assert!(t.shown());
        assert_eq!(t.done(1, false), None);
        assert!(!t.shown() && !t.confirmed);
    }
    #[test]
    fn change_of_mind_while_in_flight() {
        let mut t = Toggle::new(0u8);
        assert_eq!(t.want(1), Some(1));
        t.sent(7, 1);
        assert_eq!(t.want(2), None, "queued");
        assert_eq!(t.shown(), 2);
        assert_eq!(t.done(7, true), Some(2));
        t.sent(8, 2);
        assert_eq!(t.done(8, false), None);
        assert_eq!((t.confirmed, t.shown()), (1, 1), "rolls back to the last confirmed state");
        // Asking for the confirmed state sends nothing.
        assert_eq!(t.want(1), None);
        // Back and forth while in flight ends where it started: nothing more to send.
        t.want(0);
        t.sent(9, 0);
        t.want(1);
        assert_eq!(t.done(9, true), Some(1));
    }
}
