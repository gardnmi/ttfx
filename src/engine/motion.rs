//! Waypoint, Segment, Path, Motion — state from engine/motion.py. The stepping
//! logic that fires events (Path.step, Motion.move, activate_path) lives on
//! EngineCtx (ctx.rs) so actions run inline at upstream emission points.

use std::cell::Cell;
use std::rc::Rc;

use crate::engine::events::WaypointKey;
use crate::utils::easing::Easing;
use crate::utils::geometry::{self, Coord};
use crate::utils::ordered_map::OrderedMap;
use crate::utils::pycompat::round_half_even;

/// Waypoints are cloned constantly — into segments, into origin segments on
/// every path activation, and into event keys — so both owned fields are
/// reference counted and a clone is two refcount bumps.
#[derive(Debug, Clone, PartialEq)]
pub struct Waypoint {
    pub waypoint_id: Rc<str>,
    pub coord: Coord,
    pub bezier_control: Option<Rc<[Coord]>>,
}

impl Waypoint {
    pub fn key(&self) -> WaypointKey {
        WaypointKey {
            coord: self.coord,
            waypoint_id: self.waypoint_id.clone(),
            bezier_control: self.bezier_control.clone(),
        }
    }
}

/// A span between two of the path's waypoints, held as indices into
/// `Path::waypoints` (or `Path::ORIGIN` for the synthetic activation origin).
/// Upstream keeps two Waypoint objects per segment; copying them made a
/// segment 112 bytes, and binarypath alone builds half a million of them.
#[derive(Debug, Clone, Copy)]
pub struct Segment {
    pub start: u32,
    pub end: u32,
    pub distance: f64,
    pub enter_event_triggered: bool,
    pub exit_event_triggered: bool,
}

impl Segment {
    pub fn new(start: u32, end: u32, distance: f64) -> Self {
        Segment { start, end, distance, enter_event_triggered: false, exit_event_triggered: false }
    }
}

/// Independent paths may share waypoint definitions. Ordinary paths retain
/// their inline Vec; only explicit templates allocate shared storage. Mutable
/// access detaches shared definitions while preserving normal Vec operations.
#[derive(Debug, Clone)]
pub struct Waypoints(WaypointStorage);

#[derive(Debug, Clone)]
enum WaypointStorage {
    Owned(Vec<Waypoint>),
    Shared(Rc<Vec<Waypoint>>),
}

impl Waypoints {
    pub(crate) fn share(&mut self) {
        if let WaypointStorage::Owned(entries) = &mut self.0 {
            self.0 = WaypointStorage::Shared(Rc::new(std::mem::take(entries)));
        }
    }
}
impl From<Vec<Waypoint>> for Waypoints {
    fn from(waypoints: Vec<Waypoint>) -> Self {
        Self(WaypointStorage::Owned(waypoints))
    }
}
impl std::ops::Deref for Waypoints {
    type Target = Vec<Waypoint>;
    fn deref(&self) -> &Self::Target {
        match &self.0 {
            WaypointStorage::Owned(entries) => entries,
            WaypointStorage::Shared(entries) => entries,
        }
    }
}
impl std::ops::DerefMut for Waypoints {
    fn deref_mut(&mut self) -> &mut Self::Target {
        match &mut self.0 {
            WaypointStorage::Owned(entries) => entries,
            WaypointStorage::Shared(entries) => Rc::make_mut(entries),
        }
    }
}
impl<'a> IntoIterator for &'a Waypoints {
    type Item = &'a Waypoint;
    type IntoIter = std::slice::Iter<'a, Waypoint>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
impl<'a> IntoIterator for &'a mut Waypoints {
    type Item = &'a mut Waypoint;
    type IntoIter = std::slice::IterMut<'a, Waypoint>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter_mut()
    }
}
impl IntoIterator for Waypoints {
    type Item = Waypoint;
    type IntoIter = std::vec::IntoIter<Waypoint>;
    fn into_iter(self) -> Self::IntoIter {
        match self.0 {
            WaypointStorage::Owned(entries) => entries.into_iter(),
            WaypointStorage::Shared(entries) => Rc::unwrap_or_clone(entries).into_iter(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Path {
    /// Shared with the key in `Motion::paths`, so the id is stored once.
    pub path_id: Rc<str>,
    pub speed: f64,
    pub ease: Option<Easing>,
    pub layer: Option<i64>,
    pub hold_time: i64,
    pub loop_: bool,
    pub segments: Vec<Segment>,
    pub waypoints: Waypoints,
    pub total_distance: f64,
    current_step: Cell<i64>,
    pub max_steps: i64,
    pub hold_time_remaining: i64,
    last_distance_reached: Cell<f64>,
    /// Only this distance survives reactivation; the segment is already
    /// stored at segments[0].
    pub origin_segment_distance: Option<f64>,
    /// The synthetic origin remains separate from the indexed waypoints.
    pub origin_waypoint: Option<Waypoint>,
}

impl Path {
    pub fn new(
        path_id: &str,
        speed: f64,
        ease: Option<Easing>,
        layer: Option<i64>,
        hold_time: i64,
        loop_: bool,
    ) -> Result<Self, String> {
        if speed <= 0.0 {
            return Err(format!("Path speed must be greater than 0. Received: {speed}"));
        }
        Ok(Path {
            path_id: Rc::from(path_id),
            speed,
            ease,
            layer,
            hold_time,
            loop_,
            segments: Vec::new(),
            // Allocate on first insertion; a template can replace this empty
            // definition without first allocating an unused waypoint buffer.
            waypoints: Vec::new().into(),
            total_distance: 0.0,
            current_step: Cell::new(0),
            max_steps: 0,
            hold_time_remaining: hold_time,
            last_distance_reached: Cell::new(0.0),
            origin_segment_distance: None,
            origin_waypoint: None,
        })
    }

    /// Segment endpoint index for the activation origin.
    pub const ORIGIN: u32 = u32::MAX;

    pub fn current_step(&self) -> i64 {
        self.current_step.get()
    }

    pub fn set_current_step(&mut self, step: i64) {
        self.current_step.set(step);
    }

    pub fn last_distance_reached(&self) -> f64 {
        self.last_distance_reached.get()
    }

    pub fn set_last_distance_reached(&mut self, distance: f64) {
        self.last_distance_reached.set(distance);
    }

    /// The arena materializes deferred counters before returning a public path
    /// reference. Only the runtime may change these through a shared reference.
    pub(crate) fn materialize_progress(&self, step: i64, distance: f64) {
        self.current_step.set(step);
        self.last_distance_reached.set(distance);
    }

    /// The waypoint a segment endpoint refers to.
    pub fn waypoint_at(&self, index: u32) -> &Waypoint {
        if index == Path::ORIGIN {
            self.origin_waypoint.as_ref().expect("origin segment without an origin waypoint")
        } else {
            &self.waypoints[index as usize]
        }
    }

    /// Path.new_waypoint: auto-id like scenes; duplicate explicit id errors.
    pub fn new_waypoint(
        &mut self,
        coord: Coord,
        bezier_control: Option<Vec<Coord>>,
        waypoint_id: &str,
    ) -> Result<Waypoint, String> {
        let waypoint_id: Rc<str> = if waypoint_id.is_empty() {
            let mut current_id = self.waypoints.len();
            loop {
                let candidate = current_id.to_string();
                if !self.waypoints.iter().any(|w| *w.waypoint_id == *candidate) {
                    break Rc::from(candidate);
                }
                current_id += 1;
            }
        } else {
            if self.waypoints.iter().any(|w| *w.waypoint_id == *waypoint_id) {
                return Err(format!("duplicate waypoint id: {waypoint_id}"));
            }
            Rc::from(waypoint_id)
        };
        // Python: empty tuple bezier_control is falsy -> None
        let bezier_control = bezier_control.filter(|v| !v.is_empty()).map(Rc::from);
        let waypoint = Waypoint { waypoint_id, coord, bezier_control };
        self.add_waypoint_to_path(waypoint.clone());
        Ok(waypoint)
    }

    /// Path._add_waypoint_to_path.
    fn add_waypoint_to_path(&mut self, waypoint: Waypoint) {
        // Most paths contain a single waypoint. Avoid Vec's default four-item
        // first allocation while allowing unused/template paths to stay empty.
        if self.waypoints.is_empty() {
            self.waypoints.reserve_exact(1);
        }
        self.waypoints.push(waypoint);
        if self.waypoints.len() < 2 {
            return;
        }
        let prev = &self.waypoints[self.waypoints.len() - 2];
        let waypoint = &self.waypoints[self.waypoints.len() - 1];
        let distance_from_previous = match &waypoint.bezier_control {
            Some(control) => geometry::find_length_of_bezier_curve(prev.coord, control, waypoint.coord),
            None => geometry::find_length_of_line(prev.coord, waypoint.coord, true),
        };
        self.total_distance += distance_from_previous;
        if self.segments.is_empty() {
            self.segments.reserve_exact(1);
        }
        let end = (self.waypoints.len() - 1) as u32;
        self.segments.push(Segment::new(end - 1, end, distance_from_previous));
        self.max_steps = round_half_even(self.total_distance / self.speed);
    }

    /// Event-free stepping keeps the path borrowed through the whole walk. The
    /// engine uses its reentrant walker instead whenever segment events are
    /// observed (including tracing). Preserve operation order for exact parity.
    pub(crate) fn step_without_events(&mut self) -> (Coord, Option<usize>) {
        if self.max_steps == 0 || self.current_step() >= self.max_steps || self.total_distance == 0.0 {
            return (self.waypoint_at(self.segments.last().expect("path has no segments").end).coord, None);
        }
        self.set_current_step(self.current_step() + 1);
        let ratio = self.current_step() as f64 / self.max_steps as f64;
        let factor = self.ease.map_or(ratio, |ease| ease.ease(ratio));
        let mut distance = factor * self.total_distance;
        self.set_last_distance_reached(distance);
        let mut active = self.segments.len() - 1;
        let mut found = false;
        for (index, segment) in self.segments.iter_mut().enumerate() {
            segment.enter_event_triggered = true;
            if distance <= segment.distance {
                active = index;
                found = true;
                break;
            }
            distance -= segment.distance;
            segment.exit_event_triggered = true;
        }
        let segment = &self.segments[active];
        if !found {
            distance += segment.distance;
        }
        let t = if segment.distance == 0.0 {
            0.0
        } else if self.ease.is_some() {
            distance / segment.distance
        } else {
            (distance / segment.distance).min(1.0)
        };
        let start = self.waypoint_at(segment.start);
        let end = self.waypoint_at(segment.end);
        let coord = match &end.bezier_control {
            Some(control) => geometry::find_coord_on_bezier_curve(start.coord, control, end.coord, t),
            None => geometry::find_coord_on_line(start.coord, end.coord, t),
        };
        (coord, Some(active))
    }

    pub fn query_waypoint(&self, waypoint_id: &str) -> Result<&Waypoint, String> {
        self.waypoints
            .iter()
            .find(|w| *w.waypoint_id == *waypoint_id)
            .ok_or_else(|| format!("waypoint not found: {waypoint_id}"))
    }
}

/// engine/motion.py Motion: per-character movement state. `active_path` and
/// `completed_path` are path ids (upstream holds object references; Path
/// equality is by id).
#[derive(Debug, Clone)]
pub struct Motion {
    pub paths: OrderedMap<Path>,
    pub current_coord: Coord,
    pub previous_coord: Coord,
    pub active_path: Option<Rc<str>>,
    pub completed_path: Option<Rc<str>>,
}

impl Motion {
    pub fn new(input_coord: Coord) -> Self {
        Motion {
            paths: OrderedMap::new(),
            current_coord: input_coord,
            previous_coord: Coord::new(-1, -1),
            active_path: None,
            completed_path: None,
        }
    }

    pub fn set_coordinate(&mut self, coord: Coord) {
        self.current_coord = coord;
    }

    /// Motion.new_path: auto-id probing; duplicate explicit id errors.
    pub fn new_path(
        &mut self,
        speed: f64,
        ease: Option<Easing>,
        layer: Option<i64>,
        hold_time: i64,
        loop_: bool,
        path_id: &str,
    ) -> Result<String, String> {
        let path_id = if path_id.is_empty() {
            let mut current_id = self.paths.len();
            loop {
                let candidate = current_id.to_string();
                if !self.paths.contains_key(&candidate) {
                    break candidate;
                }
                current_id += 1;
            }
        } else {
            if self.paths.contains_key(path_id) {
                return Err(format!("duplicate path id: {path_id}"));
            }
            path_id.to_string()
        };
        let path = Path::new(&path_id, speed, ease, layer, hold_time, loop_)?;
        let key = Rc::clone(&path.path_id);
        self.paths.insert(key, path);
        Ok(path_id)
    }

    pub fn movement_is_complete(&self) -> bool {
        self.active_path.is_none()
    }

    /// Motion.deactivate_path: None clears unconditionally; otherwise only
    /// clears when the given path is the active one.
    pub fn deactivate_path(&mut self, path_id: Option<&str>) {
        match path_id {
            None => self.active_path = None,
            Some(id) => {
                if self.active_path.as_deref() == Some(id) {
                    self.active_path = None;
                }
            }
        }
    }
}

#[cfg(test)]
mod sharing_tests {
    use super::*;
    use crate::engine::character::CharId;
    use crate::engine::ctx::{Clock, EngineCtx, NoopHooks};
    use crate::engine::terminal::TerminalConfig;
    use crate::utils::rng::Rng;

    fn context(shared: bool) -> EngineCtx {
        let mut ctx =
            EngineCtx::new("xy", TerminalConfig::default(), Rng::seeded(1), Clock::virtual_with_frame_rate(60))
                .unwrap();
        for id in 0..2 {
            if id == 1 && shared {
                let path = ctx.terminal.arena[0].motion.paths.get_mut("route").unwrap();
                path.waypoints.share();
                let copy = path.clone();
                ctx.terminal.arena[1].motion.paths.insert("route", copy);
            } else {
                let motion = &mut ctx.terminal.arena[id].motion;
                motion.new_path(0.25, Some(Easing::OutElastic), None, 3, true, "route").unwrap();
                let path = motion.paths.get_mut("route").unwrap();
                for coord in [Coord::new(8, 2), Coord::new(-3, 7), Coord::new(8, 2)] {
                    path.new_waypoint(coord, None, "").unwrap();
                }
            }
        }
        for id in 0..2 {
            ctx.activate_path(&mut NoopHooks, CharId(id), "route");
        }
        ctx
    }

    #[test]
    fn shared_routes_keep_playback_origins_and_mutations_independent() {
        let mut shared = context(true);
        let mut independent = context(false);
        for tick in 0..700 {
            for ctx in [&mut shared, &mut independent] {
                if tick == 70 {
                    let path = ctx.terminal.arena[1].motion.paths.get_mut("route").unwrap();
                    path.waypoints[0].coord = Coord::new(4, 9);
                    path.waypoints[1].bezier_control = Some(Rc::from([Coord::new(2, 4)]));
                }
                if tick == 100 {
                    let path = ctx.terminal.arena[1].motion.paths.get_mut("route").unwrap();
                    path.new_waypoint(Coord::new(9, -1), None, "extra").unwrap();
                    for waypoint in &mut path.waypoints {
                        waypoint.coord.row += 1;
                    }
                }
                if tick == 140 {
                    ctx.terminal.arena[1].motion.paths.get_mut("route").unwrap().segments[0].exit_event_triggered =
                        false;
                }
                if tick == 180 {
                    ctx.activate_path(&mut NoopHooks, CharId(1), "route");
                }
                ctx.motion_move(&mut NoopHooks, CharId(0));
                if tick % 3 == 0 {
                    ctx.motion_move(&mut NoopHooks, CharId(1));
                }
            }
            for id in 0..2 {
                let a = &shared.terminal.arena[id].motion;
                let b = &independent.terminal.arena[id].motion;
                assert_eq!((a.current_coord, a.previous_coord), (b.current_coord, b.previous_coord));
                assert_eq!(a.active_path, b.active_path);
                assert_eq!(a.completed_path, b.completed_path);
                let a = a.paths.get("route").unwrap();
                let b = b.paths.get("route").unwrap();
                assert_eq!(
                    (a.current_step(), a.max_steps, a.hold_time_remaining),
                    (b.current_step(), b.max_steps, b.hold_time_remaining)
                );
                assert_eq!(a.last_distance_reached().to_bits(), b.last_distance_reached().to_bits());
                assert_eq!(a.total_distance.to_bits(), b.total_distance.to_bits());
                assert_eq!(a.origin_waypoint, b.origin_waypoint);
                assert_eq!(a.waypoints.as_slice(), b.waypoints.as_slice());
                for (a, b) in a.segments.iter().zip(&b.segments) {
                    assert_eq!(
                        (a.enter_event_triggered, a.exit_event_triggered),
                        (b.enter_event_triggered, b.exit_event_triggered)
                    );
                }
            }
        }
    }
}
