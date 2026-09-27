//! Persistent event-free motion cursors and prepared segment geometry.
//!
//! The ordinary walker enters a segment and commits its event flags first.
//! Pure interior steps then use this owned storage without visiting path maps,
//! segments, or waypoints. Coordinates are committed eagerly; path counters are
//! materialized at observation/mutation boundaries. No steps are speculative.

use super::character::EffectCharacter;
use super::motion::Path;
use crate::utils::easing::Easing;
use crate::utils::geometry::Coord;
use crate::utils::pycompat::round_half_even;

// With a nonnegative remaining distance <= 2^52, subtracting an integer no
// larger than that distance is exact: the result stays on the original f64
// lattice, with equal or finer spacing. This permits one prefix subtraction
// instead of repeating earlier integer segment subtractions. Fractional
// prefixes and nonfinite inputs retain the ordinary walker. Overshoot can be
// prepared after the ordinary walker has committed the final segment's flags.
const EXACT_LIMIT: f64 = 4_503_599_627_370_496.0;

#[derive(Debug, Clone, Copy, Default)]
struct Cursor {
    step: i64,
    max_steps: i64,
    distance: f64,
    path: usize,
}

#[derive(Debug, Clone, Copy)]
struct Geometry {
    start: [f64; 2],
    end: [f64; 2],
    control: [f64; 2],
    total: f64,
    distance: f64,
    prefix: f64,
    ease: Option<Easing>,
    first: bool,
    curved: bool,
    last: bool,
    exit_fired: bool,
}

impl Default for Geometry {
    fn default() -> Self {
        Self {
            start: [0.0; 2],
            end: [0.0; 2],
            control: [0.0; 2],
            total: 0.0,
            distance: 0.0,
            prefix: 0.0,
            ease: None,
            first: true,
            curved: false,
            last: false,
            exit_fired: false,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct MotionRuntime {
    cursors: Vec<Cursor>,
    geometry: Vec<Geometry>,
    members: Vec<u64>,
    count: usize,
    point: fn(&Geometry, f64) -> Coord,
    pub(crate) enabled: bool,
}

impl Default for MotionRuntime {
    fn default() -> Self {
        Self {
            cursors: Vec::new(),
            geometry: Vec::new(),
            members: Vec::new(),
            count: 0,
            point: select_point(),
            enabled: std::env::var_os("TTFX_MOTION_RUNTIME").is_none_or(|value| value != "0"),
        }
    }
}

impl MotionRuntime {
    /// Prepared cursors cannot be complete: completion and public mutations
    /// retire them before the ordinary engine changes activity.
    pub(crate) fn active_mask(&self) -> &[u64] {
        &self.members
    }

    #[inline]
    fn contains(&self, id: usize) -> bool {
        self.count != 0 && self.members.get(id / 64).is_some_and(|word| word & (1 << (id % 64)) != 0)
    }

    pub(crate) fn prepare(&mut self, id: usize, len: usize, slot: usize, path: &Path, segment: usize) {
        if !self.enabled || path.max_steps <= 1 || path.current_step() < 0 || path.current_step() >= path.max_steps - 1
        {
            return;
        }
        let seg = &path.segments[segment];
        let end = path.waypoint_at(seg.end);
        let control = match end.bezier_control.as_deref() {
            None | Some([]) => None,
            Some([control]) => Some(*control),
            Some(_) => return,
        };
        let mut prefix = 0.0;
        for previous in &path.segments[..segment] {
            let distance = previous.distance;
            if !distance.is_finite() || distance < 0.0 || distance.fract() != 0.0 || distance > EXACT_LIMIT - prefix {
                return;
            }
            prefix += distance;
        }
        if self.cursors.len() < len {
            self.cursors.resize(len, Cursor::default());
            self.geometry.resize(len, Geometry::default());
            self.members.resize(len.div_ceil(64), 0);
        }
        debug_assert!(!self.contains(id));
        self.cursors[id] = Cursor {
            step: path.current_step(),
            max_steps: path.max_steps,
            distance: path.last_distance_reached(),
            path: slot,
        };
        let floats = |coord: Coord| [coord.column as f64, coord.row as f64];
        self.geometry[id] = Geometry {
            start: floats(path.waypoint_at(seg.start).coord),
            end: floats(end.coord),
            control: control.map_or([0.0; 2], floats),
            total: path.total_distance,
            distance: seg.distance,
            prefix,
            ease: path.ease,
            first: segment == 0,
            curved: control.is_some(),
            last: segment + 1 == path.segments.len(),
            exit_fired: seg.exit_event_triggered,
        };
        self.members[id / 64] |= 1 << (id % 64);
        self.count += 1;
    }

    /// None leaves the step untouched for the ordinary completion/event walker.
    #[inline]
    pub(crate) fn tick(&mut self, id: usize) -> Option<Coord> {
        if !self.contains(id) {
            return None;
        }
        let cursor = &mut self.cursors[id];
        if cursor.step >= cursor.max_steps - 1 {
            return None;
        }
        let geometry = &self.geometry[id];
        let step = cursor.step + 1;
        let ratio = step as f64 / cursor.max_steps as f64;
        let distance = geometry.ease.map_or(ratio, |ease| ease.ease(ratio)) * geometry.total;
        if !distance.is_finite() || (!geometry.first && (distance <= geometry.prefix || distance > EXACT_LIMIT)) {
            return None;
        }
        // The first segment must not introduce an extra subtraction: even
        // signed zero is observable in path progress and downstream rounding.
        let mut remaining = if geometry.first { distance } else { distance - geometry.prefix };
        if !(remaining <= geometry.distance) {
            if !geometry.last || !geometry.exit_fired {
                return None;
            }
            // Preserve the reference's subtract-then-add during overshoot:
            // cancellation need not be exact. Entry/exit flags are already set.
            remaining -= geometry.distance;
            remaining += geometry.distance;
        }
        let t = if geometry.distance == 0.0 {
            0.0
        } else if geometry.ease.is_some() {
            remaining / geometry.distance
        } else {
            (remaining / geometry.distance).min(1.0)
        };
        let coord = (self.point)(geometry, t);
        cursor.step = step;
        cursor.distance = distance;
        Some(coord)
    }

    #[inline]
    pub(crate) fn materialize(&self, id: usize, characters: &[EffectCharacter]) {
        if self.contains(id) {
            self.materialize_cached(id, characters);
        }
    }

    pub(crate) fn progress(&self, id: usize) -> Option<(i64, i64, f64, f64)> {
        if !self.contains(id) {
            return None;
        }
        let cursor = &self.cursors[id];
        Some((cursor.step, cursor.max_steps, self.geometry[id].total, cursor.distance))
    }

    #[inline(never)]
    fn materialize_cached(&self, id: usize, characters: &[EffectCharacter]) {
        let cursor = &self.cursors[id];
        characters[id].motion.paths.at(cursor.path).materialize_progress(cursor.step, cursor.distance);
    }

    #[inline]
    pub(crate) fn invalidate(&mut self, id: usize, characters: &[EffectCharacter]) {
        if self.contains(id) {
            self.materialize_cached(id, characters);
            self.members[id / 64] &= !(1 << (id % 64));
            self.count -= 1;
        }
    }

    pub(crate) fn invalidate_all(&mut self, characters: &[EffectCharacter]) {
        for word in 0..self.members.len() {
            let mut members = self.members[word];
            while members != 0 {
                let id = word * 64 + members.trailing_zeros() as usize;
                members &= members - 1;
                self.materialize_cached(id, characters);
            }
        }
        self.members.fill(0);
        self.count = 0;
    }
}

fn point_scalar(geometry: &Geometry, t: f64) -> Coord {
    let axis = |axis: usize| {
        let start = geometry.start[axis];
        let end = geometry.end[axis];
        if geometry.curved {
            let control = geometry.control[axis];
            let a = (1.0 - t) * start + t * control;
            let b = (1.0 - t) * control + t * end;
            (1.0 - t) * a + t * b
        } else {
            (1.0 - t) * start + t * end
        }
    };
    Coord::new(round_half_even(axis(0)), round_half_even(axis(1)))
}

fn select_point() -> fn(&Geometry, f64) -> Coord {
    if std::env::var_os("TTFX_SIMD").is_some_and(|value| value == "0")
        || std::env::var_os("TTFX_MOTION_SIMD").is_some_and(|value| value == "0")
    {
        return point_scalar;
    }
    #[cfg(target_arch = "x86_64")]
    if std::is_x86_feature_detected!("sse4.1") {
        return point_sse41;
    }
    point_scalar
}

#[cfg(target_arch = "x86_64")]
fn point_sse41(geometry: &Geometry, t: f64) -> Coord {
    // SAFETY: selected only after runtime SSE4.1 detection. Tests use the same
    // feature guard. All loads below read exactly two live f64 array elements.
    unsafe { point_sse41_inner(geometry, t) }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse4.1")]
unsafe fn point_sse41_inner(geometry: &Geometry, t: f64) -> Coord {
    use std::arch::x86_64::*;
    let fraction = _mm_set1_pd(t);
    let inverse = _mm_set1_pd(1.0 - t);
    let start = unsafe { _mm_loadu_pd(geometry.start.as_ptr()) };
    let end = unsafe { _mm_loadu_pd(geometry.end.as_ptr()) };
    let point = if geometry.curved {
        let control = unsafe { _mm_loadu_pd(geometry.control.as_ptr()) };
        let a = _mm_add_pd(_mm_mul_pd(inverse, start), _mm_mul_pd(fraction, control));
        let b = _mm_add_pd(_mm_mul_pd(inverse, control), _mm_mul_pd(fraction, end));
        _mm_add_pd(_mm_mul_pd(inverse, a), _mm_mul_pd(fraction, b))
    } else {
        _mm_add_pd(_mm_mul_pd(inverse, start), _mm_mul_pd(fraction, end))
    };
    // Keep scalar saturation and the existing nonfinite behavior. The upper
    // limit is exclusive: i64::MAX rounds up when represented as f64.
    let in_range = _mm_and_pd(
        _mm_cmpge_pd(point, _mm_set1_pd(i64::MIN as f64)),
        _mm_cmplt_pd(point, _mm_set1_pd(-(i64::MIN as f64))),
    );
    if _mm_movemask_pd(in_range) != 3 {
        return point_scalar(geometry, t);
    }
    let rounded = _mm_round_pd::<{ _MM_FROUND_TO_NEAREST_INT | _MM_FROUND_NO_EXC }>(point);
    Coord::new(_mm_cvttsd_si64(rounded), _mm_cvttsd_si64(_mm_unpackhi_pd(rounded, rounded)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prepared_overshoot_keeps_rounding_and_retires_exit_flags_before_reuse() {
        let mut overshoots = 0;
        for ease in [Easing::OutBack, Easing::OutElastic, Easing::InOutBack, Easing::InOutElastic] {
            let mut reference = Path::new("p", 0.2, Some(ease), None, 0, false).unwrap();
            reference.new_waypoint(Coord::new(-5, 3), None, "a").unwrap();
            reference.new_waypoint(Coord::new(53, -7), None, "b").unwrap();
            let mut character = EffectCharacter::new(0, "X", -5, 3);
            character.motion.paths.insert("p", reference.clone());
            let mut characters = vec![character];
            let mut runtime = MotionRuntime::default();
            runtime.enabled = true;
            for _ in 0..reference.max_steps + 3 {
                let (expected, _) = reference.step_without_events();
                let actual = if let Some(coord) = runtime.tick(0) {
                    overshoots += usize::from(reference.last_distance_reached() > reference.total_distance);
                    coord
                } else {
                    runtime.invalidate(0, &characters);
                    let path = characters[0].motion.paths.at_mut(0);
                    let (coord, segment) = path.step_without_events();
                    if let Some(segment) = segment {
                        runtime.prepare(0, 1, 0, path, segment);
                    }
                    coord
                };
                runtime.materialize(0, &characters);
                let path = characters[0].motion.paths.at(0);
                assert_eq!(actual, expected);
                assert_eq!(path.last_distance_reached().to_bits(), reference.last_distance_reached().to_bits());
                assert_eq!(path.segments[0].exit_event_triggered, reference.segments[0].exit_event_triggered);
            }
        }
        assert!(overshoots > 100);
    }

    #[test]
    fn integer_prefix_preparation_matches_sequential_subtraction_and_event_flags() {
        let mut used = 0;
        for distances in [
            [0.0, 1.0, 2.0, 7.0],
            [31.0, 257.0, 3.0, 101.0],
            [1_125_899_906_842_624.0, 17.0, 4_503_599_627_370_496.0, 3.0],
            [0.125, 17.0, 0.375, 7.0],
        ] {
            for ease in [None, Some(Easing::InOutBack), Some(Easing::OutBounce)] {
                let mut path = Path::new("p", 1.0, ease, None, 0, false).unwrap();
                for x in 0..5 {
                    path.new_waypoint(Coord::new(x * 5, x % 2), None, "").unwrap();
                }
                for (segment, distance) in path.segments.iter_mut().zip(distances) {
                    segment.distance = distance;
                }
                path.total_distance = distances.iter().sum();
                path.max_steps = 257;
                let mut reference = path.clone();
                let mut character = EffectCharacter::new(0, "X", 1, 1);
                character.motion.paths.insert("p", path);
                let mut characters = vec![character];
                let mut runtime = MotionRuntime::default();
                runtime.enabled = true;
                for _ in 0..260 {
                    let (expected, _) = reference.step_without_events();
                    let actual = if let Some(coord) = runtime.tick(0) {
                        used += 1;
                        coord
                    } else {
                        runtime.invalidate(0, &characters);
                        let path = characters[0].motion.paths.at_mut(0);
                        let (coord, segment) = path.step_without_events();
                        if let Some(segment) = segment {
                            runtime.prepare(0, 1, 0, path, segment);
                        }
                        coord
                    };
                    runtime.materialize(0, &characters);
                    assert_eq!(actual, expected);
                    let actual = characters[0].motion.paths.at(0);
                    assert_eq!(actual.current_step(), reference.current_step());
                    assert_eq!(actual.last_distance_reached().to_bits(), reference.last_distance_reached().to_bits());
                    for (a, b) in actual.segments.iter().zip(&reference.segments) {
                        assert_eq!(
                            (a.enter_event_triggered, a.exit_event_triggered),
                            (b.enter_event_triggered, b.exit_event_triggered)
                        );
                    }
                }
            }
        }
        assert!(used > 1500, "the comparison must exercise prepared steps, not just fallback");
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn vector_point_matches_geometry_at_ties_overshoot_and_conversion_limits() {
        if !std::is_x86_feature_detected!("sse4.1") {
            return;
        }
        let values = [i64::MIN, -(1_i64 << 53) - 1, -513, -1, 0, 1, 513, (1_i64 << 53) + 1, i64::MAX];
        let fractions = [-2.0, -0.5, -0.0, 0.0, 0.125, 0.5, 0.75, 1.0, 1.5, 2.0, f64::NAN];
        for &x in &values {
            for &y in &values {
                let start = Coord::new(x, y);
                let end = Coord::new(y, x);
                let control = Coord::new(-7, 9);
                for curved in [false, true] {
                    let geometry = Geometry {
                        start: [x as f64, y as f64],
                        end: [y as f64, x as f64],
                        control: [-7.0, 9.0],
                        curved,
                        ..Geometry::default()
                    };
                    for &t in &fractions {
                        let expected = if curved {
                            crate::utils::geometry::find_coord_on_bezier_curve(start, &[control], end, t)
                        } else {
                            crate::utils::geometry::find_coord_on_line(start, end, t)
                        };
                        assert_eq!(point_scalar(&geometry, t), expected);
                        assert_eq!(point_sse41(&geometry, t), expected, "{x}, {y}, t={t}, curved={curved}");
                    }
                }
            }
        }
    }
}
