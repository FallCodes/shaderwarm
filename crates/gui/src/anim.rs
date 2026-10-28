//! Tiny animation toolkit: easing curves, reveal timelines and interruptible hover fades.
//! Nothing here owns a clock; callers pass the frame time in.

use std::time::{Duration, Instant};

/// Quartic ease-out: fast start, soft landing. The snappy curve for entrances.
pub fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(4)
}

/// Progress of something that started at `start` and lasts `duration`, after `delay`.
pub fn progress(now: Instant, start: Instant, delay: Duration, duration: Duration) -> f32 {
    let elapsed = now.saturating_duration_since(start).saturating_sub(delay);
    (elapsed.as_secs_f32() / duration.as_secs_f32()).clamp(0.0, 1.0)
}

pub fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// Hover in 110 ms, out in 70 ms (exits faster than entrances). Interruptible: a new
/// target starts from wherever the previous fade had got to.
#[derive(Debug, Clone, Copy)]
pub struct Fade {
    on: bool,
    from: f32,
    since: Instant,
}

const FADE_IN: Duration = Duration::from_millis(110);
const FADE_OUT: Duration = Duration::from_millis(70);

impl Fade {
    /// Starts at rest (0) and heads for `on`.
    pub fn new(on: bool, now: Instant) -> Self {
        Self { on: false, from: 0.0, since: now }.retarget(on, now)
    }

    pub fn retarget(self, on: bool, now: Instant) -> Self {
        if on == self.on {
            return self;
        }
        Self { on, from: self.value(now), since: now }
    }

    pub fn value(&self, now: Instant) -> f32 {
        let dur = if self.on { FADE_IN } else { FADE_OUT };
        let t = ease_out(progress(now, self.since, Duration::ZERO, dur));
        if self.on {
            lerp(self.from, 1.0, t)
        } else {
            lerp(self.from, 0.0, t)
        }
    }

    pub fn settled(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.since) >= FADE_IN.max(FADE_OUT)
    }

    pub fn is_on(&self) -> bool {
        self.on
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn easing_bounds() {
        assert_eq!(ease_out(0.0), 0.0);
        assert_eq!(ease_out(1.0), 1.0);
        assert!(ease_out(0.5) > 0.5, "ease-out front-loads motion");
    }

    #[test]
    fn fade_is_interruptible() {
        let t0 = Instant::now();
        let f = Fade::new(true, t0);
        let mid = t0 + Duration::from_millis(40);
        let v = f.value(mid);
        assert!(v > 0.0 && v < 1.0);
        let back = f.retarget(false, mid);
        assert!((back.value(mid) - v).abs() < 1e-6, "reversal starts from the current value");
        assert_eq!(back.value(mid + Duration::from_millis(200)), 0.0);
        assert!(back.settled(mid + Duration::from_millis(200)));
    }
}
