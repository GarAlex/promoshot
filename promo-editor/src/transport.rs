//! Playback transport — Stage 1 slice 1.3.
//!
//! One state machine for play / pause / scrub, because this app fixed the
//! *same* scrub bug four times in four players (`LivePreviewView`,
//! `LayersManagementView`, and both resource trim views), each of which had
//! re-implemented the same three booleans slightly differently.
//!
//! ```text
//! Idle ──play──► Playing ──grab──► Scrubbing{resume: true}
//!                   ▲                    │
//!                   └──── release ───────┘   (seek, then resume iff resume)
//!
//! Idle ──grab──► Scrubbing{resume: false} ──release──► Idle (seek only)
//! ```
//!
//! The machine owns *decisions*, not the player: it answers each event with
//! the [`Effect`]s a host should carry out. What a "seek" costs is the host's
//! business; whether one is owed is this crate's.

/// Where the transport is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportState {
    Idle,
    Playing,
    /// The playhead handle is held. The clock must not write the playhead in
    /// this state — that is what made the handle spring back.
    Scrubbing,
}

impl TransportState {
    /// Parses the wire name; unknown values are Idle, which is the safe
    /// resting state rather than a silent "keep playing".
    ///
    /// Deliberately not `FromStr`: this is infallible by design, and a
    /// `Result` would invite callers to treat an unknown name as an error
    /// rather than as "stop".
    pub fn parse(raw: &str) -> Self {
        match raw {
            "playing" => TransportState::Playing,
            "scrubbing" => TransportState::Scrubbing,
            _ => TransportState::Idle,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            TransportState::Idle => "idle",
            TransportState::Playing => "playing",
            TransportState::Scrubbing => "scrubbing",
        }
    }
}

/// What the host should do about an event. Ordered: a release seeks *before*
/// it resumes, so playback never restarts at the old position.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Effect {
    /// Seek the player. `generation` comes back in
    /// [`Transport::seek_completed`] so stale completions can be told apart
    /// from the current one.
    Seek {
        time: f64,
        generation: u64,
    },
    StartPlayback {
        at: f64,
    },
    StopPlayback,
}

/// The transport machine. Holds no player and no clock — only the decision of
/// what should happen next.
#[derive(Debug, Clone, PartialEq)]
pub struct Transport {
    state: TransportState,
    time: f64,
    duration: f64,
    /// Was playback running when the handle was grabbed?
    resume_after_scrub: bool,
    seek_generation: u64,
}

impl Transport {
    pub fn new(duration: f64) -> Self {
        Self {
            state: TransportState::Idle,
            time: 0.0,
            duration: duration.max(0.0),
            resume_after_scrub: false,
            seek_generation: 0,
        }
    }

    /// Rebuilds a machine from state a host is holding on its behalf.
    ///
    /// Stage 1 does not move ownership: the Swift views still keep their
    /// transport in `@State` and hand it over per event, so the rules live
    /// here while the storage stays there. A front end that owns its
    /// `Transport` outright never needs this.
    pub fn restore(
        state: TransportState,
        time: f64,
        duration: f64,
        resume_after_scrub: bool,
        seek_generation: u64,
    ) -> Self {
        let duration = duration.max(0.0);
        Self {
            state,
            time: time.max(0.0).min(duration),
            duration,
            resume_after_scrub,
            seek_generation,
        }
    }

    pub fn state(&self) -> TransportState {
        self.state
    }
    pub fn time(&self) -> f64 {
        self.time
    }
    pub fn duration(&self) -> f64 {
        self.duration
    }
    pub fn is_playing(&self) -> bool {
        self.state == TransportState::Playing
    }
    pub fn is_scrubbing(&self) -> bool {
        self.state == TransportState::Scrubbing
    }
    pub fn seek_generation(&self) -> u64 {
        self.seek_generation
    }
    pub fn resumes_after_scrub(&self) -> bool {
        self.resume_after_scrub
    }

    fn clamp(&self, time: f64) -> f64 {
        time.max(0.0).min(self.duration.max(0.0))
    }

    /// A composition can grow or shrink under the playhead.
    pub fn set_duration(&mut self, duration: f64) {
        self.duration = duration.max(0.0);
        self.time = self.clamp(self.time);
    }

    pub fn play(&mut self) -> Vec<Effect> {
        if self.state == TransportState::Playing {
            return vec![];
        }
        // Playing from the very end restarts, rather than sitting still.
        if self.duration > 0.0 && self.time >= self.duration {
            self.time = 0.0;
        }
        self.state = TransportState::Playing;
        vec![Effect::StartPlayback { at: self.time }]
    }

    pub fn pause(&mut self) -> Vec<Effect> {
        if self.state != TransportState::Playing {
            return vec![];
        }
        self.state = TransportState::Idle;
        vec![Effect::StopPlayback]
    }

    pub fn toggle(&mut self) -> Vec<Effect> {
        if self.is_playing() {
            self.pause()
        } else {
            self.play()
        }
    }

    /// The clock ticked. Returns the effects, and moves the playhead **only**
    /// when the machine is playing.
    ///
    /// Invariants 1 and 2: while scrubbing, a tick must not write the
    /// playhead — including a tick that lands between the grab and the
    /// playback task noticing it was cancelled.
    pub fn tick(&mut self, clock_time: f64) -> Vec<Effect> {
        if self.state != TransportState::Playing {
            return vec![];
        }
        if self.duration > 0.0 && clock_time >= self.duration {
            self.time = self.duration;
            self.state = TransportState::Idle;
            return vec![Effect::StopPlayback];
        }
        self.time = self.clamp(clock_time);
        vec![]
    }

    /// The handle was grabbed. Playback stops for the drag so the clock
    /// cannot fight the finger; whether it resumes is remembered here.
    pub fn begin_scrub(&mut self) -> Vec<Effect> {
        if self.state == TransportState::Scrubbing {
            return vec![];
        }
        self.resume_after_scrub = self.state == TransportState::Playing;
        let was_playing = self.resume_after_scrub;
        self.state = TransportState::Scrubbing;
        if was_playing {
            vec![Effect::StopPlayback]
        } else {
            vec![]
        }
    }

    /// The handle moved. Moves the playhead and nothing else — no seek per
    /// delta, which is what made the handle spring back when the clock wrote
    /// its own time on the next tick.
    pub fn scrub_to(&mut self, time: f64) {
        if self.state != TransportState::Scrubbing {
            return;
        }
        self.time = self.clamp(time);
    }

    /// The handle was released.
    ///
    /// Invariant 3: seek first, then resume — and resume only if playback was
    /// running when the handle was grabbed.
    pub fn end_scrub(&mut self) -> Vec<Effect> {
        if self.state != TransportState::Scrubbing {
            return vec![];
        }
        self.seek_generation += 1;
        let mut effects = vec![Effect::Seek {
            time: self.time,
            generation: self.seek_generation,
        }];
        if self.resume_after_scrub {
            self.resume_after_scrub = false;
            self.state = TransportState::Playing;
            effects.push(Effect::StartPlayback { at: self.time });
        } else {
            self.state = TransportState::Idle;
        }
        effects
    }

    /// Seek without a drag (tapping the timeline, a keyboard jump). Playback
    /// state is unchanged: seeking while playing keeps playing.
    pub fn seek(&mut self, time: f64) -> Vec<Effect> {
        self.time = self.clamp(time);
        self.seek_generation += 1;
        vec![Effect::Seek {
            time: self.time,
            generation: self.seek_generation,
        }]
    }

    /// A seek reported back.
    ///
    /// Invariant 4, and the subtlest of the four: **the generation decides,
    /// not the host's "finished" flag.** AVFoundation reports `finished ==
    /// false` both when a seek was superseded by a newer one *and* when the
    /// item was not ready yet — so treating that flag as failure strands
    /// playback. A completion for the current generation is the current
    /// answer whatever the flag says; an older one is simply not this seek's
    /// business.
    ///
    /// Returns true when this completion is the current one.
    pub fn seek_completed(&self, generation: u64) -> bool {
        generation == self.seek_generation
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn playing_at(time: f64, duration: f64) -> Transport {
        let mut t = Transport::new(duration);
        t.seek(time);
        t.play();
        t
    }

    #[test]
    fn play_pause_and_toggle() {
        let mut t = Transport::new(60.0);
        assert_eq!(t.state(), TransportState::Idle);
        assert_eq!(t.play(), vec![Effect::StartPlayback { at: 0.0 }]);
        assert!(t.is_playing());
        assert_eq!(t.play(), vec![], "playing twice is not an event");
        assert_eq!(t.pause(), vec![Effect::StopPlayback]);
        assert_eq!(t.pause(), vec![], "pausing twice is not an event");
        assert_eq!(t.toggle(), vec![Effect::StartPlayback { at: 0.0 }]);
        assert_eq!(t.toggle(), vec![Effect::StopPlayback]);
    }

    #[test]
    fn ticking_moves_the_playhead_while_playing() {
        let mut t = playing_at(0.0, 60.0);
        assert_eq!(t.tick(1.5), vec![]);
        assert_eq!(t.time(), 1.5);
    }

    /// Invariant 1: while the handle is held, the clock must not write the
    /// playhead. This is the bug, in one assertion.
    #[test]
    fn a_tick_while_scrubbing_never_moves_the_playhead() {
        let mut t = playing_at(10.0, 60.0);
        t.begin_scrub();
        t.scrub_to(42.0);
        assert_eq!(t.tick(11.0), vec![], "a tick mid-drag produces nothing");
        assert_eq!(
            t.time(),
            42.0,
            "the handle keeps the position it was dragged to"
        );
    }

    /// Invariant 2: a tick that lands between the grab and the playback task
    /// noticing it was cancelled must be inert too — same state, so the same
    /// answer, which is exactly why one machine beats four copies.
    #[test]
    fn a_tick_racing_the_grab_is_inert() {
        let mut t = playing_at(10.0, 60.0);
        assert_eq!(t.begin_scrub(), vec![Effect::StopPlayback]);
        // The in-flight tick arrives now, after the grab.
        assert_eq!(t.tick(10.05), vec![]);
        assert_eq!(t.time(), 10.0);
    }

    /// Invariant 3: release seeks first, then resumes — and only if playback
    /// was running when the handle was grabbed.
    #[test]
    fn release_seeks_then_resumes_only_when_it_was_playing() {
        let mut t = playing_at(10.0, 60.0);
        t.begin_scrub();
        t.scrub_to(30.0);
        let effects = t.end_scrub();
        assert_eq!(
            effects,
            vec![
                Effect::Seek {
                    time: 30.0,
                    generation: 2
                },
                Effect::StartPlayback { at: 30.0 },
            ],
            "seek precedes resume, so playback never restarts at the old spot"
        );
        assert!(t.is_playing());

        // Grabbed while paused: seek only, and it stays paused.
        let mut idle = Transport::new(60.0);
        idle.seek(5.0);
        idle.begin_scrub();
        idle.scrub_to(20.0);
        assert_eq!(
            idle.end_scrub(),
            vec![Effect::Seek {
                time: 20.0,
                generation: 2
            }]
        );
        assert_eq!(idle.state(), TransportState::Idle);
    }

    /// Invariant 4: the generation decides which completion is current — not
    /// the host's "finished" flag, which is false both for a superseded seek
    /// and for one whose item was not ready.
    #[test]
    fn a_superseded_seek_completion_is_ignored_not_treated_as_failure() {
        let mut t = Transport::new(60.0);
        let first = match t.seek(10.0)[0] {
            Effect::Seek { generation, .. } => generation,
            _ => unreachable!(),
        };
        let second = match t.seek(20.0)[0] {
            Effect::Seek { generation, .. } => generation,
            _ => unreachable!(),
        };
        assert!(second > first);
        assert!(
            !t.seek_completed(first),
            "the old seek's completion is not this seek's business"
        );
        assert!(
            t.seek_completed(second),
            "the current generation is current however the host flagged it"
        );
    }

    #[test]
    fn playback_stops_and_clamps_at_the_end() {
        let mut t = playing_at(0.0, 10.0);
        assert_eq!(t.tick(10.0), vec![Effect::StopPlayback]);
        assert_eq!(t.time(), 10.0);
        assert_eq!(t.state(), TransportState::Idle);
        // Playing from the end starts over rather than sitting still.
        assert_eq!(t.play(), vec![Effect::StartPlayback { at: 0.0 }]);
        assert_eq!(t.time(), 0.0);
    }

    #[test]
    fn scrubbing_clamps_to_the_composition() {
        let mut t = Transport::new(30.0);
        t.begin_scrub();
        t.scrub_to(-5.0);
        assert_eq!(t.time(), 0.0);
        t.scrub_to(999.0);
        assert_eq!(t.time(), 30.0);
    }

    #[test]
    fn scrub_moves_are_ignored_unless_the_handle_is_held() {
        let mut t = playing_at(5.0, 60.0);
        t.scrub_to(40.0);
        assert_eq!(t.time(), 5.0, "no drag is in progress");
        assert_eq!(
            t.end_scrub(),
            vec![],
            "releasing a handle nobody held does nothing"
        );
    }

    #[test]
    fn seeking_while_playing_keeps_playing() {
        let mut t = playing_at(5.0, 60.0);
        let effects = t.seek(25.0);
        assert_eq!(effects.len(), 1);
        assert!(t.is_playing(), "a seek is not a pause");
        assert_eq!(t.time(), 25.0);
    }

    #[test]
    fn shrinking_the_composition_pulls_the_playhead_back() {
        let mut t = Transport::new(60.0);
        t.seek(50.0);
        t.set_duration(20.0);
        assert_eq!(t.time(), 20.0);
    }

    #[test]
    fn grabbing_twice_keeps_the_first_answer() {
        let mut t = playing_at(10.0, 60.0);
        assert_eq!(t.begin_scrub(), vec![Effect::StopPlayback]);
        assert!(t.resumes_after_scrub());
        assert_eq!(t.begin_scrub(), vec![], "already held");
        assert!(
            t.resumes_after_scrub(),
            "a second grab must not forget that playback was running"
        );
    }
}
