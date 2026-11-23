//! The approval gate: every request waits here for the user's answer, and nothing but an answer
//! the user gave, a timed permission the user gave, or nothing at all lets one through.
//!
//! The gate knows connections and requests by number only. A timed permission belongs to the
//! connection it was given on, never to the name an agent calls itself, so another process that
//! says the same name gets nothing from it. The clock is injected, so the tests walk through the
//! minutes without waiting.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// How long a request waits for its answer before it is denied as timed out.
pub const ANSWER_WITHIN: Duration = Duration::from_secs(60);
/// How many requests wait at once; the next one is refused at once.
pub const QUEUE_LIMIT: usize = 5;
/// How long the card must have been fully shown before its answers count, so a click or a key
/// press aimed at whatever was there a moment earlier approves nothing.
pub const ARM_AFTER: Duration = Duration::from_millis(600);
/// How long a timed permission lasts.
pub const GRANT_FOR: Duration = Duration::from_secs(10 * 60);
/// How many captures a timed permission sends in any minute; the next one is asked again.
pub const GRANT_RATE: usize = 12;
const RATE_WINDOW: Duration = Duration::from_secs(60);
/// How often a waiting request looks at the clock while nothing wakes it.
const WAIT_STEP: Duration = Duration::from_millis(250);

pub type ConnectionId = u64;
pub type RequestId = u64;

/// Where the gate reads the time.
pub trait Clock: Send + Sync {
    fn now(&self) -> Instant;
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// The user's answer on the card.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Answer {
    Allow,
    Deny,
    /// Allows this request and gives its connection a timed permission.
    AllowTimed,
}

/// How an answer reached the core. Another process can post clicks and key presses into the
/// session; the window server marks a posted event with its sender's process, and the HID state
/// holds only the keys a keyboard holds down, so the app hands the gate what it read there.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Evidence {
    /// A click on the card; `hardware` when its event's source process was 0.
    Click { hardware: bool },
    /// A key press; `held` when the HID state held its key and modifiers down.
    Key { held: bool },
}

impl Evidence {
    fn genuine(self) -> bool {
        match self {
            Evidence::Click { hardware } => hardware,
            Evidence::Key { held } => held,
        }
    }
}

/// What became of a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    Allow,
    AllowTimed,
    Deny,
    Timeout,
    /// The screen locked or the Mac slept while it waited.
    Locked,
    /// Its connection closed, or it was withdrawn.
    Gone,
}

impl Decision {
    pub fn allows(self) -> bool {
        matches!(self, Decision::Allow | Decision::AllowTimed)
    }
}

/// What the gate says to a new request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    /// The connection's timed permission covers it: it goes without a card.
    Granted,
    /// It holds a place in the queue; the card shows it once [`Gate::open`] is called.
    Queued(RequestId),
    Locked,
    QueueFull,
}

/// Why an answer was not taken. The request goes on waiting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused {
    /// No such request waits (answered, timed out or withdrawn).
    NotWaiting,
    /// The card shows another request.
    NotShown,
    /// The card has not been fully shown for [`ARM_AFTER`] yet.
    NotArmed,
    /// The click or the key did not come from the user's hardware.
    Synthetic,
}

#[derive(Clone, Debug)]
struct Pending {
    id: RequestId,
    connection: ConnectionId,
    pick: bool,
    /// Shown on the card; until then it only holds its place while the app prepares it.
    open: bool,
    deadline: Instant,
}

#[derive(Clone, Debug)]
struct Grant {
    until: Instant,
    uses: VecDeque<Instant>,
}

#[derive(Default)]
struct State {
    next: RequestId,
    queue: Vec<Pending>,
    decided: HashMap<RequestId, Decision>,
    shown: Option<RequestId>,
    /// When the card began to show `shown` fully.
    visible_since: Option<Instant>,
    grants: HashMap<ConnectionId, Grant>,
    locked: bool,
    /// The connections whose requests were refused while the screen was locked, one entry per
    /// request, for the note after the unlock.
    missed: Vec<ConnectionId>,
}

impl State {
    fn decide(&mut self, id: RequestId, decision: Decision) {
        self.queue.retain(|p| p.id != id);
        self.decided.insert(id, decision);
        if self.shown == Some(id) {
            self.shown = None;
            self.visible_since = None;
        }
    }

    /// The card goes on with the oldest open request when the one it showed left.
    fn reselect(&mut self) {
        if self.shown.is_some_and(|id| self.queue.iter().any(|p| p.id == id && p.open)) {
            return;
        }
        self.shown = self.queue.iter().find(|p| p.open).map(|p| p.id);
        self.visible_since = None;
    }

    /// Times out what waited too long and drops ended permissions. Returns whether anything
    /// changed.
    fn expire(&mut self, now: Instant) -> bool {
        let late: Vec<RequestId> = self.queue.iter().filter(|p| p.open && p.deadline <= now).map(|p| p.id).collect();
        for id in &late {
            self.decide(*id, Decision::Timeout);
        }
        let before = self.grants.len();
        self.grants.retain(|_, g| g.until > now);
        for grant in self.grants.values_mut() {
            while grant.uses.front().is_some_and(|t| now.duration_since(*t) >= RATE_WINDOW) {
                grant.uses.pop_front();
            }
        }
        if !late.is_empty() {
            self.reselect();
        }
        !late.is_empty() || self.grants.len() != before
    }

    fn end_all(&mut self, decision: Decision) {
        let ids: Vec<RequestId> = self.queue.iter().map(|p| p.id).collect();
        for id in ids {
            self.decide(id, decision);
        }
        self.shown = None;
        self.visible_since = None;
        self.grants.clear();
    }
}

/// One request as the card shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct PendingView {
    pub id: RequestId,
    pub connection: ConnectionId,
    pub pick: bool,
    pub left: Duration,
}

/// The gate at one moment, for the card, the menu bar and Settings.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct View {
    /// The open requests, oldest first.
    pub queue: Vec<PendingView>,
    pub shown: Option<RequestId>,
    /// The timed permissions with the time each has left.
    pub grants: Vec<(ConnectionId, Duration)>,
    pub locked: bool,
}

pub struct Gate {
    clock: Arc<dyn Clock>,
    state: Mutex<State>,
    changed: Condvar,
    notify: Box<dyn Fn() + Send + Sync>,
}

impl Gate {
    /// `notify` runs after every change the card, the menu bar or Settings shows, outside the
    /// gate's lock.
    pub fn new(clock: Arc<dyn Clock>, notify: impl Fn() + Send + Sync + 'static) -> Gate {
        Gate {
            clock,
            state: Mutex::new(State { next: 1, ..State::default() }),
            changed: Condvar::new(),
            notify: Box::new(notify),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Runs `change` under the lock, wakes the waiting requests and tells the pages when it says
    /// something changed.
    fn change<T>(&self, change: impl FnOnce(&mut State, Instant) -> (T, bool)) -> T {
        let now = self.clock.now();
        let (value, changed) = {
            let mut state = self.state();
            let expired = state.expire(now);
            let (value, changed) = change(&mut state, now);
            (value, changed || expired)
        };
        if changed {
            self.changed.notify_all();
            (self.notify)();
        }
        value
    }

    /// Takes a new request of `connection`. `pick` is a request whose area the user selects,
    /// which a timed permission never covers.
    pub fn admit(&self, connection: ConnectionId, pick: bool) -> Admission {
        self.change(|state, now| {
            if state.locked {
                state.missed.push(connection);
                return (Admission::Locked, false);
            }
            if !pick {
                if let Some(grant) = state.grants.get_mut(&connection) {
                    if grant.uses.len() < GRANT_RATE {
                        grant.uses.push_back(now);
                        return (Admission::Granted, false);
                    }
                }
            }
            if state.queue.len() >= QUEUE_LIMIT {
                return (Admission::QueueFull, false);
            }
            let id = state.next;
            state.next += 1;
            state.queue.push(Pending { id, connection, pick, open: false, deadline: now + ANSWER_WITHIN });
            (Admission::Queued(id), false)
        })
    }

    /// Puts a queued request on the card, once the app has prepared what it would send; its 60
    /// seconds start now. A request that ended meanwhile (the screen locked, its connection
    /// closed) gives its decision instead.
    pub fn open(&self, id: RequestId) -> Result<(), Decision> {
        self.change(|state, now| {
            if let Some(decision) = state.decided.remove(&id) {
                return (Err(decision), false);
            }
            let Some(pending) = state.queue.iter_mut().find(|p| p.id == id) else {
                return (Err(Decision::Gone), false);
            };
            pending.open = true;
            pending.deadline = now + ANSWER_WITHIN;
            state.reselect();
            (Ok(()), true)
        })
    }

    /// Takes a request off the queue without an answer (it could not be prepared, or the agent
    /// cancelled it).
    pub fn withdraw(&self, id: RequestId) {
        self.change(|state, _| {
            let was = state.queue.iter().any(|p| p.id == id);
            state.queue.retain(|p| p.id != id);
            state.decided.remove(&id);
            if was {
                state.decided.insert(id, Decision::Gone);
                state.reselect();
            }
            ((), was)
        })
    }

    /// Shows request `id` on the card (‹ ›); its answers count [`ARM_AFTER`] after the card shows
    /// it fully.
    pub fn show(&self, id: RequestId) -> bool {
        self.change(|state, _| {
            if state.shown == Some(id) || !state.queue.iter().any(|p| p.id == id && p.open) {
                return (false, false);
            }
            state.shown = Some(id);
            state.visible_since = None;
            (true, true)
        })
    }

    /// Shows the next (`forward`) or the previous open request (⌥⇧⌘→ ←), wrapping around.
    pub fn step(&self, forward: bool) -> Option<RequestId> {
        let next = {
            let state = self.state();
            let open: Vec<RequestId> = state.queue.iter().filter(|p| p.open).map(|p| p.id).collect();
            let at = state.shown.and_then(|id| open.iter().position(|x| *x == id))?;
            let n = open.len();
            open[if forward { (at + 1) % n } else { (at + n - 1) % n }]
        };
        self.show(next);
        Some(next)
    }

    /// What the card shows fully at this moment, as the toast page reports it: `None` while the
    /// card is hidden, moving or fading.
    pub fn visible(&self, shown: Option<RequestId>) {
        let now = self.clock.now();
        let mut state = self.state();
        match shown {
            Some(id) if state.shown == Some(id) => {
                if state.visible_since.is_none() {
                    state.visible_since = Some(now);
                }
            }
            _ => state.visible_since = None,
        }
    }

    /// The user answered request `id`. Only the request the card shows, after it has shown fully
    /// for [`ARM_AFTER`], with input from the user's own hardware, is answered.
    pub fn answer(&self, id: RequestId, answer: Answer, evidence: Evidence) -> Result<Decision, Refused> {
        self.change(|state, now| {
            let Some(pending) = state.queue.iter().find(|p| p.id == id && p.open).cloned() else {
                return (Err(Refused::NotWaiting), false);
            };
            if state.shown != Some(id) {
                return (Err(Refused::NotShown), false);
            }
            if !state.visible_since.is_some_and(|since| now.duration_since(since) >= ARM_AFTER) {
                return (Err(Refused::NotArmed), false);
            }
            if !evidence.genuine() {
                return (Err(Refused::Synthetic), false);
            }
            let decision = match answer {
                Answer::Allow => Decision::Allow,
                Answer::Deny => Decision::Deny,
                Answer::AllowTimed => Decision::AllowTimed,
            };
            state.decide(id, decision);
            if decision == Decision::AllowTimed {
                let uses = if pending.pick { VecDeque::new() } else { VecDeque::from([now]) };
                state.grants.insert(pending.connection, Grant { until: now + GRANT_FOR, uses });
            }
            state.reselect();
            (Ok(decision), true)
        })
    }

    /// Waits for request `id`'s decision; a request nobody answers times out.
    pub fn wait(&self, id: RequestId) -> Decision {
        loop {
            let now = self.clock.now();
            let mut state = self.state();
            if state.expire(now) {
                drop(state);
                self.changed.notify_all();
                (self.notify)();
                continue;
            }
            if let Some(decision) = state.decided.remove(&id) {
                return decision;
            }
            let Some(deadline) = state.queue.iter().find(|p| p.id == id).map(|p| p.deadline) else {
                return Decision::Gone;
            };
            let step = deadline.saturating_duration_since(now).clamp(Duration::from_millis(1), WAIT_STEP);
            drop(self.changed.wait_timeout(state, step).unwrap_or_else(|p| p.into_inner()));
        }
    }

    /// The screen locked: every waiting request is refused, every timed permission ends, and new
    /// requests are refused until it unlocks.
    pub fn lock_screen(&self) {
        self.change(|state, _| {
            let changed = !state.locked || !state.queue.is_empty() || !state.grants.is_empty();
            state.locked = true;
            state.end_all(Decision::Locked);
            ((), changed)
        })
    }

    /// The screen unlocked. Returns the connections whose requests were refused meanwhile, one
    /// entry per request, which the app tells the user about once.
    pub fn unlock_screen(&self) -> Vec<ConnectionId> {
        self.change(|state, _| {
            let changed = state.locked;
            state.locked = false;
            (std::mem::take(&mut state.missed), changed)
        })
    }

    /// The Mac is going to sleep: nobody is there to answer, and every timed permission ends.
    pub fn sleep(&self) {
        self.change(|state, _| {
            let changed = !state.queue.is_empty() || !state.grants.is_empty();
            state.end_all(Decision::Locked);
            ((), changed)
        })
    }

    /// A connection closed: its requests end and its timed permission with them.
    pub fn disconnect(&self, connection: ConnectionId) {
        self.change(|state, _| {
            let ids: Vec<RequestId> = state.queue.iter().filter(|p| p.connection == connection).map(|p| p.id).collect();
            let had = state.grants.remove(&connection).is_some();
            for id in &ids {
                state.decide(*id, Decision::Gone);
            }
            state.reselect();
            ((), had || !ids.is_empty())
        })
    }

    /// The user took a timed permission back.
    pub fn revoke(&self, connection: ConnectionId) -> bool {
        self.change(|state, _| {
            let had = state.grants.remove(&connection).is_some();
            (had, had)
        })
    }

    /// The time left of `connection`'s timed permission.
    pub fn grant_left(&self, connection: ConnectionId) -> Option<Duration> {
        let now = self.clock.now();
        let state = self.state();
        state.grants.get(&connection).filter(|g| g.until > now).map(|g| g.until - now)
    }

    pub fn is_locked(&self) -> bool {
        self.state().locked
    }

    pub fn view(&self) -> View {
        let now = self.clock.now();
        let mut state = self.state();
        state.expire(now);
        View {
            queue: state
                .queue
                .iter()
                .filter(|p| p.open)
                .map(|p| PendingView {
                    id: p.id,
                    connection: p.connection,
                    pick: p.pick,
                    left: p.deadline.saturating_duration_since(now),
                })
                .collect(),
            shown: state.shown,
            grants: state.grants.iter().map(|(c, g)| (*c, g.until.saturating_duration_since(now))).collect(),
            locked: state.locked,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeClock {
        base: Instant,
        offset: Mutex<Duration>,
    }

    impl FakeClock {
        fn advance(&self, by: Duration) {
            *self.offset.lock().unwrap() += by;
        }
    }

    impl Clock for FakeClock {
        fn now(&self) -> Instant {
            self.base + *self.offset.lock().unwrap()
        }
    }

    const CLICK: Evidence = Evidence::Click { hardware: true };

    fn gate() -> (Gate, Arc<FakeClock>) {
        let clock = Arc::new(FakeClock { base: Instant::now(), offset: Mutex::new(Duration::ZERO) });
        (Gate::new(clock.clone(), || {}), clock)
    }

    /// A request of `connection` on the card, shown fully for long enough to be answered.
    fn shown(gate: &Gate, clock: &FakeClock, connection: ConnectionId) -> RequestId {
        let Admission::Queued(id) = gate.admit(connection, false) else { panic!("not queued") };
        gate.open(id).unwrap();
        gate.show(id);
        gate.visible(Some(id));
        clock.advance(ARM_AFTER);
        id
    }

    #[test]
    fn a_denied_request_is_never_allowed() {
        let (gate, clock) = gate();
        let id = shown(&gate, &clock, 1);
        assert_eq!(gate.answer(id, Answer::Deny, CLICK), Ok(Decision::Deny));
        assert_eq!(gate.wait(id), Decision::Deny);
        assert_eq!(gate.answer(id, Answer::Allow, CLICK), Err(Refused::NotWaiting));
        assert_eq!(gate.admit(1, false), Admission::Queued(2), "a denial gives no permission");
    }

    #[test]
    fn an_unanswered_request_times_out_after_60_seconds() {
        let (gate, clock) = gate();
        let id = shown(&gate, &clock, 1);
        clock.advance(ANSWER_WITHIN);
        assert_eq!(gate.wait(id), Decision::Timeout);
        assert_eq!(gate.answer(id, Answer::Allow, CLICK), Err(Refused::NotWaiting));
        assert!(gate.view().queue.is_empty());
    }

    #[test]
    fn the_sixth_waiting_request_is_refused() {
        let (gate, _) = gate();
        for _ in 0..QUEUE_LIMIT {
            assert!(matches!(gate.admit(1, false), Admission::Queued(_)));
        }
        assert_eq!(gate.admit(2, false), Admission::QueueFull);
    }

    #[test]
    fn a_locked_screen_refuses_and_ends_what_waits() {
        let (gate, clock) = gate();
        let id = shown(&gate, &clock, 1);
        let Admission::Queued(preparing) = gate.admit(1, false) else { panic!() };
        gate.lock_screen();
        assert_eq!(gate.wait(id), Decision::Locked);
        assert_eq!(gate.open(preparing), Err(Decision::Locked), "one being prepared never reaches the card");
        assert_eq!(gate.admit(2, false), Admission::Locked);
        assert_eq!(gate.unlock_screen(), vec![2]);
        assert!(gate.unlock_screen().is_empty(), "the note comes once");
    }

    #[test]
    fn answers_count_only_after_the_card_showed_fully_for_600_ms() {
        let (gate, clock) = gate();
        let Admission::Queued(id) = gate.admit(1, false) else { panic!() };
        gate.open(id).unwrap();
        assert_eq!(gate.answer(id, Answer::Allow, CLICK), Err(Refused::NotArmed), "never shown");
        gate.visible(Some(id));
        clock.advance(ARM_AFTER - Duration::from_millis(1));
        assert_eq!(gate.answer(id, Answer::Allow, CLICK), Err(Refused::NotArmed));
        gate.visible(None);
        gate.visible(Some(id));
        clock.advance(Duration::from_millis(2));
        assert_eq!(gate.answer(id, Answer::Allow, CLICK), Err(Refused::NotArmed), "hiding starts the wait again");
        clock.advance(ARM_AFTER);
        assert_eq!(gate.answer(id, Answer::Allow, CLICK), Ok(Decision::Allow));
    }

    #[test]
    fn posted_clicks_and_keys_answer_nothing() {
        let (gate, clock) = gate();
        let id = shown(&gate, &clock, 1);
        assert_eq!(gate.answer(id, Answer::Allow, Evidence::Click { hardware: false }), Err(Refused::Synthetic));
        assert_eq!(gate.answer(id, Answer::AllowTimed, Evidence::Key { held: false }), Err(Refused::Synthetic));
        assert_eq!(gate.view().queue.len(), 1, "the request goes on waiting");
        assert_eq!(gate.grant_left(1), None);
    }

    #[test]
    fn only_the_request_on_the_card_is_answered() {
        let (gate, clock) = gate();
        let first = shown(&gate, &clock, 1);
        let Admission::Queued(second) = gate.admit(2, false) else { panic!() };
        gate.open(second).unwrap();
        assert_eq!(gate.answer(second, Answer::Allow, CLICK), Err(Refused::NotShown));
        assert_eq!(gate.step(true), Some(second));
        assert_eq!(gate.answer(second, Answer::Allow, CLICK), Err(Refused::NotArmed), "the step starts the wait again");
        assert_eq!(gate.view().shown, Some(second));
        assert!(gate.view().queue.iter().any(|p| p.id == first));
    }

    #[test]
    fn a_timed_permission_belongs_to_its_connection_only() {
        let (gate, clock) = gate();
        let id = shown(&gate, &clock, 1);
        assert_eq!(gate.answer(id, Answer::AllowTimed, CLICK), Ok(Decision::AllowTimed));
        assert_eq!(gate.admit(1, false), Admission::Granted);
        // A second connection whose agent says the same name is a different connection.
        assert!(matches!(gate.admit(2, false), Admission::Queued(_)));
    }

    #[test]
    fn a_timed_permission_still_asks_for_a_selection_and_past_12_a_minute() {
        let (gate, clock) = gate();
        let id = shown(&gate, &clock, 1);
        gate.answer(id, Answer::AllowTimed, CLICK).unwrap();
        assert!(matches!(gate.admit(1, true), Admission::Queued(_)), "a selection is always asked");
        for _ in 1..GRANT_RATE {
            assert_eq!(gate.admit(1, false), Admission::Granted);
        }
        assert!(matches!(gate.admit(1, false), Admission::Queued(_)), "the 13th in a minute is asked");
        clock.advance(RATE_WINDOW);
        assert_eq!(gate.admit(1, false), Admission::Granted);
    }

    #[test]
    fn a_timed_permission_ends_after_10_minutes_and_on_lock_sleep_disconnect_and_revoke() {
        type End = fn(&Gate, &FakeClock);
        let ends: [(&str, End); 5] = [
            ("10 minutes", |_, clock| clock.advance(GRANT_FOR)),
            ("lock", |gate, _| {
                gate.lock_screen();
                gate.unlock_screen();
            }),
            ("sleep", |gate, _| gate.sleep()),
            ("disconnect", |gate, _| gate.disconnect(1)),
            ("revoke", |gate, _| {
                gate.revoke(1);
            }),
        ];
        for (name, end) in ends {
            let (gate, clock) = gate();
            let id = shown(&gate, &clock, 1);
            gate.answer(id, Answer::AllowTimed, CLICK).unwrap();
            assert!(gate.grant_left(1).is_some());
            end(&gate, &clock);
            assert_eq!(gate.grant_left(1), None, "{name}");
            assert!(matches!(gate.admit(1, false), Admission::Queued(_)), "{name}: asked again");
        }
    }

    #[test]
    fn a_closed_connection_ends_its_waiting_requests() {
        let (gate, clock) = gate();
        let id = shown(&gate, &clock, 1);
        gate.disconnect(1);
        assert_eq!(gate.wait(id), Decision::Gone);
        assert_eq!(gate.view().shown, None);
    }
}
