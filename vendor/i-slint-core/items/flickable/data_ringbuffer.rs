// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0

//! This module contains a simple ringbuffer to store time and delta tuples.
//! It is used in the flickable to determine the initial velocity of the animation.

use crate::Coord;
use crate::animations::Instant;
use crate::lengths::{LogicalPx, LogicalVector};
use core::time::Duration;
use euclid::Vector2D;

/// Input timestamps have millisecond precision and may describe UI-thread delivery
/// rather than physical motion. A batch drained across one clock tick is not a
/// reliable fling sample. Require at least two 240 Hz intervals (rounded down);
/// a full five-sample history at that rate still covers about 16 milliseconds.
const MIN_VELOCITY_SAMPLE_DURATION: Duration = Duration::from_millis(8);

/// Simple ringbuffer storing time and delta tuples
#[derive(Debug)]
pub(crate) struct VelocityRingBuffer<const N: usize> {
    /// Pointing to the next free element
    curr_index: usize,
    /// Indicates if the buffer is full
    full: bool,
    values: [(Instant, Vector2D<Coord, LogicalPx>); N],
}

impl<const N: usize> Default for VelocityRingBuffer<N> {
    fn default() -> Self {
        // Placeholder timestamps; `curr_index`/`full` track which entries are real.
        Self { curr_index: 0, full: false, values: [(Instant::default(), Vector2D::default()); N] }
    }
}

impl<const N: usize> VelocityRingBuffer<N> {
    /// Indicates if the buffer is empty
    pub fn empty(&self) -> bool {
        !(self.full || self.curr_index > 0)
    }

    /// Add a new element to the ringbuffer
    pub fn push(&mut self, time: Instant, value: LogicalVector) {
        if self.curr_index < self.values.len() {
            self.values[self.curr_index] = (time, value);
        }
        self.curr_index += 1;
        if self.curr_index >= N {
            self.full = true;
            self.curr_index = 0;
        }
    }

    /// Index of the most recent added value
    fn latest_index(&self) -> usize {
        if self.curr_index > 0 { self.curr_index - 1 } else { N - 1 }
    }

    fn len(&self) -> usize {
        if self.full { N } else { self.curr_index }
    }

    /// Returns the last time value added to the buffer if not empty otherwise None
    pub fn last_time(&self) -> Option<Instant> {
        if !self.empty() { Some(self.values[self.latest_index()].0) } else { None }
    }

    pub fn mean_velocity(&self) -> LogicalVector {
        let len = self.len();
        if len < 2 {
            return Default::default();
        }

        let oldest_index = if self.full { self.curr_index } else { 0 };
        let newest_index = self.latest_index();
        let duration = self.values[newest_index].0.duration_since(self.values[oldest_index].0);
        if duration < MIN_VELOCITY_SAMPLE_DURATION {
            return Default::default();
        }

        // The oldest recorded delta happened before the oldest timestamp in the covered time span,
        // so it does not belong to the average velocity between oldest and newest.
        let mut total_delta = LogicalVector::default();
        let mut index = (oldest_index + 1) % N;
        for _ in 1..len {
            total_delta += self.values[index].1;
            index = (index + 1) % N;
        }

        (total_delta.cast::<f32>() / duration.as_secs_f32()).cast::<Coord>()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::animations::Instant;
    use core::time::Duration;

    #[test]
    fn test_empty_buffer() {
        let buffer: VelocityRingBuffer<5> = VelocityRingBuffer::default();
        assert!(buffer.empty());
        assert_eq!(buffer.curr_index, 0);
        assert!(!buffer.full);
        assert_eq!(buffer.last_time(), None);
        assert_eq!(buffer.mean_velocity(), Vector2D::default());
    }

    #[test]
    fn test_push_single_element() {
        let mut buffer: VelocityRingBuffer<5> = VelocityRingBuffer::default();
        let time = Instant::default();
        let delta = Vector2D::new(10.0, 20.0);

        buffer.push(time, delta);

        assert!(!buffer.empty());
        assert_eq!(buffer.curr_index, 1);
        assert!(!buffer.full);
        assert_eq!(buffer.latest_index(), 0);
        assert_eq!(buffer.last_time(), Some(time));
        assert_eq!(buffer.mean_velocity(), Vector2D::default());
    }

    /// Buffer not complete full
    #[test]
    fn test_push_two_elements() {
        let mut buffer: VelocityRingBuffer<5> = VelocityRingBuffer::default();
        let time = Instant::default();

        buffer.push(time, Vector2D::new(10.0, 20.0));
        buffer.push(time + Duration::from_millis(100), Vector2D::new(13.0, -5.0));

        assert!(!buffer.empty());
        assert_eq!(buffer.curr_index, 2);
        assert!(!buffer.full);
        assert_eq!(buffer.latest_index(), 1);
        assert_eq!(buffer.last_time(), Some(time + Duration::from_millis(100)));

        assert_eq!(buffer.mean_velocity(), Vector2D::new(130.0, -50.0));
    }

    #[test]
    fn test_push_until_full() {
        let mut buffer: VelocityRingBuffer<5> = VelocityRingBuffer::default();
        let base_time = Instant::default();

        // Push elements to fill the buffer
        for i in 0..5 {
            let time = base_time + Duration::from_millis(i * 100);
            buffer.push(time, Vector2D::new(1.0, -2.0));
        }

        assert!(!buffer.empty());
        assert_eq!(buffer.curr_index, 0);
        assert!(buffer.full);
        assert_eq!(buffer.last_time(), Some(base_time + Duration::from_millis(400)));
        assert_eq!(buffer.latest_index(), 4);

        assert_eq!(buffer.mean_velocity(), Vector2D::new(10.0, -20.0));
    }

    #[test]
    fn test_push_beyond_capacity() {
        const CAP: usize = 5;
        let mut buffer: VelocityRingBuffer<CAP> = VelocityRingBuffer::default();
        let base_time = Instant::default();

        // Push more than capacity
        for i in 0..(CAP + 2) {
            let time = base_time + Duration::from_millis(i as u64 * 100);
            buffer.push(time, Vector2D::new(1.0, 2.0));
        }

        assert!(!buffer.empty());
        assert!(buffer.full);
        assert_eq!(buffer.curr_index, 2);
        assert_eq!(buffer.latest_index(), 1);
        assert_eq!(buffer.last_time(), Some(base_time + Duration::from_millis(600)));

        assert_eq!(buffer.mean_velocity(), Vector2D::new(10.0, 20.0));
    }

    #[test]
    fn test_push_beyond_capacity_wrap_back() {
        const CAP: usize = 5;
        let mut buffer: VelocityRingBuffer<CAP> = VelocityRingBuffer::default();
        let base_time = Instant::default();

        // Push more than capacity
        for i in 0..CAP {
            let time = base_time + Duration::from_millis(i as u64 * 100);
            buffer.push(time, Vector2D::new(3.0, -2.0));
        }

        assert!(!buffer.empty());
        assert!(buffer.full);
        assert_eq!(buffer.curr_index, 0);
        assert_eq!(buffer.latest_index(), CAP - 1);
        assert_eq!(buffer.last_time(), Some(base_time + Duration::from_millis(400)));

        assert_eq!(buffer.mean_velocity(), Vector2D::new(30.0, -20.0));
    }

    #[test]
    fn test_delivery_batch_does_not_start_a_fling() {
        // The last event crossing a millisecond tick must not turn a queued
        // 100 px drag into an 80,000 px/s release velocity.
        for times in [[100, 100, 100, 100, 100], [100, 100, 100, 100, 101]] {
            let mut buffer: VelocityRingBuffer<5> = VelocityRingBuffer::default();
            for time in times {
                buffer.push(Instant(time), Vector2D::new(0.0, -20.0));
            }
            assert_eq!(buffer.mean_velocity(), Vector2D::default());
        }
    }

    #[test]
    fn test_minimum_sample_duration() {
        let mut buffer: VelocityRingBuffer<5> = VelocityRingBuffer::default();
        buffer.push(Instant(100), Vector2D::new(0.0, -20.0));
        buffer.push(Instant(107), Vector2D::new(0.0, -20.0));
        assert_eq!(buffer.mean_velocity(), Vector2D::default());

        buffer.push(Instant(108), Vector2D::new(0.0, -20.0));
        assert_eq!(buffer.mean_velocity(), Vector2D::new(0.0, -5000.0));
    }

    #[test]
    fn test_regular_motion_preserves_velocity_and_direction() {
        // Full histories at approximately 240, 120 and 60 Hz remain usable.
        for interval in [4, 8, 16] {
            let mut buffer: VelocityRingBuffer<5> = VelocityRingBuffer::default();
            for index in 0..5 {
                buffer.push(Instant(100 + index * interval), Vector2D::new(12.0, -20.0));
            }
            let expected = Vector2D::new(12_000.0 / interval as f32, -20_000.0 / interval as f32);
            assert!((buffer.mean_velocity() - expected).square_length() < 0.001);
        }
    }

    #[test]
    fn test_slow_motion_and_reversal_use_recent_history() {
        let mut buffer: VelocityRingBuffer<5> = VelocityRingBuffer::default();
        for index in 0..10 {
            buffer.push(Instant(100 + index * 100), Vector2D::new(0.0, -1.0));
        }
        assert_eq!(buffer.mean_velocity(), Vector2D::new(0.0, -10.0));

        // Once the older deltas leave the ring, the reversed gesture has the
        // same speed in the opposite direction, regardless of gesture length.
        for index in 10..15 {
            buffer.push(Instant(100 + index * 100), Vector2D::new(0.0, 1.0));
        }
        assert_eq!(buffer.mean_velocity(), Vector2D::new(0.0, 10.0));
    }
}
