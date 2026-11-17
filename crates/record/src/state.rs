//! The recorder's state machine: idle → countdown → recording ⇄ paused → finishing → one outcome.
//!
//! It is pure: it knows no stream, file or clock, so every rule can be tested. Each method is
//! one transition and refuses with [`RecordError::InvalidState`] when it does not fit the current
//! state, so a double click, a late shortcut or a stale bar can never start a second take or
//! drop one. A take that leaves `idle` ends exactly once, through [`Machine::finish`].

use crate::{RecordError, RecordingId, RecordingOutcome, RecordingState, Result};

/// What the finishing step was asked to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ending {
    /// Stop and keep the file.
    Save,
    /// Stop and move the file to the Trash; only after the user confirmed it.
    Discard,
    /// The stream or the writer failed, or the disk ran low; keep what was written.
    Fault,
}

#[derive(Clone, Debug, PartialEq)]
enum Phase {
    Idle,
    Countdown { remaining: u32, total: u32 },
    Recording,
    Paused { discard_asked: bool },
    Finishing(Ending),
}

#[derive(Clone, Debug)]
pub struct Machine {
    phase: Phase,
    id: Option<RecordingId>,
    elapsed_ms: u64,
    muted: bool,
    /// Whether the take records a microphone.
    microphone: bool,
    /// Set once the take reached `recording`, so it can never get there a second time.
    started: bool,
}

impl Default for Machine {
    fn default() -> Self {
        Machine { phase: Phase::Idle, id: None, elapsed_ms: 0, muted: false, microphone: false, started: false }
    }
}

impl Machine {
    pub fn new() -> Machine {
        Machine::default()
    }

    /// The state as the UI sees it.
    pub fn state(&self) -> RecordingState {
        let id = || self.id.clone().unwrap_or_else(|| RecordingId(String::new()));
        match &self.phase {
            Phase::Idle => RecordingState::Idle,
            Phase::Countdown { remaining, total } => {
                RecordingState::Countdown { id: id(), remaining_seconds: *remaining, total_seconds: *total }
            }
            Phase::Recording => RecordingState::Recording {
                id: id(),
                elapsed_ms: self.elapsed_ms,
                microphone: self.microphone,
                microphone_muted: self.muted,
            },
            Phase::Paused { discard_asked } => RecordingState::Paused {
                id: id(),
                elapsed_ms: self.elapsed_ms,
                microphone: self.microphone,
                microphone_muted: self.muted,
                discard_asked: *discard_asked,
            },
            Phase::Finishing(_) => RecordingState::Finishing { id: id() },
        }
    }

    pub fn id(&self) -> Option<&RecordingId> {
        self.id.as_ref()
    }

    pub fn is_idle(&self) -> bool {
        self.phase == Phase::Idle
    }

    /// Whether a take is recording or paused, so its file is being written.
    pub fn is_capturing(&self) -> bool {
        matches!(self.phase, Phase::Recording | Phase::Paused { .. })
    }

    pub fn is_finishing(&self) -> bool {
        matches!(self.phase, Phase::Finishing(_))
    }

    pub fn microphone_muted(&self) -> bool {
        self.muted
    }

    fn name(&self) -> &'static str {
        match self.phase {
            Phase::Idle => "idle",
            Phase::Countdown { .. } => "counting down",
            Phase::Recording => "recording",
            Phase::Paused { .. } => "paused",
            Phase::Finishing(_) => "finishing",
        }
    }

    fn refuse<T>(&self, action: &'static str) -> Result<T> {
        if self.phase == Phase::Idle && matches!(action, "pause" | "resume" | "stop" | "discard" | "mute") {
            return Err(RecordError::NotRecording);
        }
        if self.phase != Phase::Idle && action == "begin" {
            return Err(RecordError::Busy);
        }
        Err(RecordError::InvalidState { action, state: self.name() })
    }

    /// Idle → countdown. A countdown of 0 seconds still passes through here; the caller calls
    /// [`Machine::started`] at once.
    pub fn begin(&mut self, id: RecordingId, countdown_seconds: u32, microphone_muted: bool) -> Result<()> {
        if self.phase != Phase::Idle {
            return self.refuse("begin");
        }
        *self = Machine {
            phase: Phase::Countdown { remaining: countdown_seconds, total: countdown_seconds },
            id: Some(id),
            elapsed_ms: 0,
            muted: microphone_muted,
            microphone: false,
            started: false,
        };
        Ok(())
    }

    /// Says whether the take records a microphone, for the control bar's microphone button.
    pub fn set_has_microphone(&mut self, microphone: bool) {
        if !self.is_idle() {
            self.microphone = microphone;
        }
    }

    /// One second of the countdown passed; returns the seconds left.
    pub fn countdown_tick(&mut self) -> Result<u32> {
        match &mut self.phase {
            Phase::Countdown { remaining, .. } => {
                *remaining = remaining.saturating_sub(1);
                Ok(*remaining)
            }
            _ => self.refuse("count down"),
        }
    }

    /// Countdown → recording, once the stream runs. A take reaches `recording` once only.
    pub fn started(&mut self) -> Result<()> {
        match self.phase {
            Phase::Countdown { .. } if !self.started => {
                self.phase = Phase::Recording;
                self.started = true;
                Ok(())
            }
            _ => self.refuse("start recording"),
        }
    }

    /// Countdown → idle: nothing was recorded, and the take ends as cancelled.
    pub fn cancel_countdown(&mut self) -> Result<RecordingOutcome> {
        match self.phase {
            Phase::Countdown { .. } => {
                let id = self.id.clone().unwrap_or_else(|| RecordingId(String::new()));
                *self = Machine::default();
                Ok(RecordingOutcome::Cancelled { id })
            }
            _ => self.refuse("cancel the countdown"),
        }
    }

    pub fn pause(&mut self) -> Result<()> {
        match self.phase {
            Phase::Recording => {
                self.phase = Phase::Paused { discard_asked: false };
                Ok(())
            }
            _ => self.refuse("pause"),
        }
    }

    pub fn resume(&mut self) -> Result<()> {
        match self.phase {
            Phase::Paused { .. } => {
                self.phase = Phase::Recording;
                Ok(())
            }
            _ => self.refuse("resume"),
        }
    }

    /// Recording or paused → paused with the question in the bar. Returns whether the take was
    /// recording, so the caller pauses the stream as well.
    pub fn ask_discard(&mut self) -> Result<bool> {
        match self.phase {
            Phase::Recording => {
                self.phase = Phase::Paused { discard_asked: true };
                Ok(true)
            }
            Phase::Paused { .. } => {
                self.phase = Phase::Paused { discard_asked: true };
                Ok(false)
            }
            _ => self.refuse("discard"),
        }
    }

    /// The user kept the take: the question goes away and the take stays paused.
    pub fn keep(&mut self) -> Result<()> {
        match self.phase {
            Phase::Paused { discard_asked: true } => {
                self.phase = Phase::Paused { discard_asked: false };
                Ok(())
            }
            _ => self.refuse("keep the take"),
        }
    }

    /// Paused with the question asked → finishing towards the Trash. Without the question
    /// first, a discard is refused, so nothing is thrown away by a single click.
    pub fn confirm_discard(&mut self) -> Result<()> {
        match self.phase {
            Phase::Paused { discard_asked: true } => {
                self.phase = Phase::Finishing(Ending::Discard);
                Ok(())
            }
            _ => self.refuse("discard"),
        }
    }

    /// Recording or paused → finishing towards a saved file.
    pub fn stop(&mut self) -> Result<()> {
        match self.phase {
            Phase::Recording | Phase::Paused { .. } => {
                self.phase = Phase::Finishing(Ending::Save);
                Ok(())
            }
            _ => self.refuse("stop"),
        }
    }

    /// The stream or the writer failed. During the countdown nothing was written, so the take
    /// ends at once (`Some` outcome); while recording, the take goes to finishing to keep what
    /// was written. A fault while finishing changes nothing: the finishing step reports it.
    pub fn fault(&mut self, code: &str, message: &str) -> Result<Option<RecordingOutcome>> {
        match self.phase {
            Phase::Countdown { .. } => {
                let id = self.id.clone().unwrap_or_else(|| RecordingId(String::new()));
                *self = Machine::default();
                Ok(Some(RecordingOutcome::Failed {
                    id,
                    code: code.to_string(),
                    message: message.to_string(),
                    partial: None,
                }))
            }
            Phase::Recording | Phase::Paused { .. } => {
                self.phase = Phase::Finishing(Ending::Fault);
                Ok(None)
            }
            Phase::Finishing(_) => Ok(None),
            Phase::Idle => self.refuse("fail"),
        }
    }

    /// What the finishing step was asked to do, while finishing.
    pub fn ending(&self) -> Option<Ending> {
        match self.phase {
            Phase::Finishing(ending) => Some(ending),
            _ => None,
        }
    }

    /// Finishing → idle with the take's one outcome. The outcome must fit the ending (a save
    /// ends saved or failed, a discard ends discarded or failed, a fault ends failed or saved
    /// after a full disk) and name the same take, so a take can never end twice or as
    /// something it was not.
    pub fn finish(&mut self, outcome: RecordingOutcome) -> Result<RecordingOutcome> {
        let Phase::Finishing(ending) = self.phase else {
            return self.refuse("finish");
        };
        let (outcome_id, fits) = match &outcome {
            RecordingOutcome::Saved { result } => (&result.id, ending != Ending::Discard),
            RecordingOutcome::Discarded { id, .. } => (id, ending == Ending::Discard),
            RecordingOutcome::Failed { id, .. } => (id, true),
            RecordingOutcome::Cancelled { id } => (id, false),
        };
        if !fits || Some(outcome_id) != self.id.as_ref() {
            return Err(RecordError::InvalidState { action: "end the take this way", state: self.name() });
        }
        *self = Machine::default();
        Ok(outcome)
    }

    pub fn set_elapsed(&mut self, elapsed_ms: u64) {
        if self.is_capturing() {
            self.elapsed_ms = elapsed_ms;
        }
    }

    pub fn set_muted(&mut self, muted: bool) -> Result<()> {
        match self.phase {
            Phase::Countdown { .. } | Phase::Recording | Phase::Paused { .. } => {
                self.muted = muted;
                Ok(())
            }
            _ => self.refuse("mute"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RecordingResult, VideoCodec};
    use glimpsfy_capture::PhysicalSize;

    fn id() -> RecordingId {
        RecordingId("rec-1".into())
    }

    fn saved(id: RecordingId) -> RecordingOutcome {
        RecordingOutcome::Saved {
            result: RecordingResult {
                id,
                path: "/tmp/a.mp4".into(),
                duration_ms: 1000,
                size: PhysicalSize { width: 2, height: 2 },
                file_bytes: 1,
                tracks: vec![],
                codec: VideoCodec::H264,
                fps: 30,
                created_at_ms: 0,
                stopped_because: None,
            },
        }
    }

    fn failed(id: RecordingId) -> RecordingOutcome {
        RecordingOutcome::Failed { id, code: "encoder".into(), message: "x".into(), partial: None }
    }

    fn recording() -> Machine {
        let mut m = Machine::new();
        m.begin(id(), 3, false).unwrap();
        m.started().unwrap();
        m
    }

    /// Every transition from every state: only the listed ones succeed.
    #[test]
    #[allow(clippy::type_complexity)]
    fn refuses_every_transition_that_does_not_fit() {
        type Step = fn(&mut Machine) -> bool;
        let steps: [(&str, Step); 10] = [
            ("begin", |m| m.begin(RecordingId("other".into()), 0, false).is_ok()),
            ("tick", |m| m.countdown_tick().is_ok()),
            ("started", |m| m.started().is_ok()),
            ("cancel", |m| m.cancel_countdown().is_ok()),
            ("pause", |m| m.pause().is_ok()),
            ("resume", |m| m.resume().is_ok()),
            ("ask", |m| m.ask_discard().is_ok()),
            ("keep", |m| m.keep().is_ok()),
            ("confirm", |m| m.confirm_discard().is_ok()),
            ("stop", |m| m.stop().is_ok()),
        ];
        let states: [(&str, fn() -> Machine, &[&str]); 6] = [
            ("idle", Machine::new, &["begin"]),
            (
                "countdown",
                || {
                    let mut m = Machine::new();
                    m.begin(id(), 3, false).unwrap();
                    m
                },
                &["tick", "started", "cancel"],
            ),
            ("recording", recording, &["pause", "ask", "stop"]),
            (
                "paused",
                || {
                    let mut m = recording();
                    m.pause().unwrap();
                    m
                },
                &["resume", "ask", "stop"],
            ),
            (
                "asked",
                || {
                    let mut m = recording();
                    m.ask_discard().unwrap();
                    m
                },
                &["resume", "ask", "keep", "confirm", "stop"],
            ),
            (
                "finishing",
                || {
                    let mut m = recording();
                    m.stop().unwrap();
                    m
                },
                &[],
            ),
        ];
        for (state, make, allowed) in states {
            for (step, run) in steps {
                let mut m = make();
                let before = m.state();
                let ok = run(&mut m);
                assert_eq!(ok, allowed.contains(&step), "{step} from {state}");
                if !ok {
                    assert_eq!(m.state(), before, "a refused {step} from {state} changed the state");
                }
            }
        }
    }

    #[test]
    fn never_reaches_recording_twice() {
        let mut m = recording();
        assert!(m.started().is_err());
        m.pause().unwrap();
        assert!(m.started().is_err());
        m.resume().unwrap();
        assert!(m.resume().is_err());
        assert!(matches!(m.begin(RecordingId("second".into()), 0, false), Err(RecordError::Busy)));
        assert_eq!(m.id(), Some(&id()));
    }

    #[test]
    fn a_discard_needs_the_question_first() {
        let mut m = recording();
        assert!(m.confirm_discard().is_err());
        m.pause().unwrap();
        assert!(m.confirm_discard().is_err());
        m.ask_discard().unwrap();
        m.keep().unwrap();
        assert!(m.confirm_discard().is_err(), "keeping the take withdraws the question");
        m.ask_discard().unwrap();
        m.confirm_discard().unwrap();
        assert_eq!(m.ending(), Some(Ending::Discard));
        assert!(m.finish(saved(id())).is_err(), "a discarded take cannot end as saved");
        let out = m.finish(RecordingOutcome::Discarded { id: id(), trashed: true }).unwrap();
        assert!(matches!(out, RecordingOutcome::Discarded { trashed: true, .. }));
    }

    /// Stop, discard and failure each leave exactly one outcome, and a second one is refused.
    #[test]
    #[allow(clippy::type_complexity)]
    fn every_take_ends_exactly_once() {
        let paths: [(&str, fn(&mut Machine), RecordingOutcome); 4] = [
            ("stop", |m| m.stop().unwrap(), saved(id())),
            (
                "discard",
                |m| {
                    m.ask_discard().unwrap();
                    m.confirm_discard().unwrap();
                },
                RecordingOutcome::Discarded { id: id(), trashed: true },
            ),
            (
                "fault",
                |m| {
                    assert!(m.fault("encoder", "x").unwrap().is_none());
                },
                failed(id()),
            ),
            ("failed save", |m| m.stop().unwrap(), failed(id())),
        ];
        for (name, end, outcome) in paths {
            let mut m = recording();
            end(&mut m);
            assert!(m.is_finishing(), "{name}");
            assert!(
                m.finish(RecordingOutcome::Saved { result: wrong_id() }).is_err(),
                "{name}: another take's outcome"
            );
            assert_eq!(m.finish(outcome.clone()).unwrap(), outcome, "{name}");
            assert!(m.is_idle(), "{name}");
            assert!(m.finish(outcome).is_err(), "{name}: a second outcome");
        }

        let mut m = Machine::new();
        m.begin(id(), 3, false).unwrap();
        assert!(matches!(m.fault("capture", "x").unwrap(), Some(RecordingOutcome::Failed { partial: None, .. })));
        assert!(m.is_idle());
        m.begin(id(), 3, false).unwrap();
        assert!(matches!(m.cancel_countdown().unwrap(), RecordingOutcome::Cancelled { .. }));
        assert!(m.cancel_countdown().is_err());
    }

    fn wrong_id() -> RecordingResult {
        match saved(RecordingId("other".into())) {
            RecordingOutcome::Saved { result } => result,
            _ => unreachable!(),
        }
    }
}
