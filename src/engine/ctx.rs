//! EngineCtx: the mutable engine world (terminal + arena + rng + clock +
//! active characters) and every stepping routine that can fire events.
//!
//! Python executes event actions synchronously at the emission point, deep in
//! the middle of Path.step / Motion.move / Animation.step_animation, and those
//! actions reentrantly mutate the same structures being stepped. To preserve
//! that observable ordering (plan.md §4.2), all stepping logic lives here as
//! EngineCtx methods that hold only short-lived borrows: state is re-fetched
//! by id after every emission point, and segment walks are index-based so
//! reentrant list mutation behaves like Python list iteration.

use std::rc::Rc;
use std::time::Instant;

use crate::engine::active_characters::ActiveCharacters;
use crate::engine::animation::{EasedTicksCache, SyncMetric};
use crate::engine::character::CharId;
use crate::engine::error::EngineError;
use crate::engine::events::{CallerKey, CallerRef, EffectCallback, Event, EventAction};
use crate::engine::motion::Waypoint;
use crate::engine::motion::{Path, Segment};
use crate::engine::playback::PlaybackKind;
use crate::engine::terminal::{Terminal, TerminalConfig};
use crate::utils::geometry::{self, Coord};
use crate::utils::pycompat::round_half_even;
use crate::utils::rng::Rng;

thread_local! {
    /// The synthetic origin waypoint's id, shared by every activation.
    static ORIGIN_WAYPOINT_ID: Rc<str> = Rc::from("origin");
}

/// Virtual/real clock (plan.md §4.7). Matrix reads wall time, thunderstorm
/// reads monotonic time; the parity harness swaps in the virtual variant.
#[derive(Debug)]
pub enum Clock {
    Real {
        start: Instant,
        wall_start: f64,
    },
    /// Virtual time advancing a fixed dt per emitted frame.
    Virtual {
        now: f64,
        dt: f64,
    },
}

impl Clock {
    pub fn real() -> Self {
        let wall_start =
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
        Clock::Real { start: Instant::now(), wall_start }
    }

    pub fn virtual_with_frame_rate(frame_rate: i64) -> Self {
        let dt = if frame_rate > 0 { 1.0 / frame_rate as f64 } else { 1.0 / 60.0 };
        Clock::Virtual { now: 0.0, dt }
    }

    /// time.time() analog.
    pub fn now_wall(&self) -> f64 {
        match self {
            Clock::Real { start, wall_start } => wall_start + start.elapsed().as_secs_f64(),
            Clock::Virtual { now, .. } => *now,
        }
    }

    /// time.monotonic() analog.
    pub fn now_monotonic(&self) -> f64 {
        match self {
            Clock::Real { start, .. } => start.elapsed().as_secs_f64(),
            Clock::Virtual { now, .. } => *now,
        }
    }

    /// Advance virtual time by one frame; no-op for the real clock.
    pub fn advance_frame(&mut self) {
        if let Clock::Virtual { now, dt } = self {
            *now += *dt;
        }
    }
}

/// Effect-side hook for CALLBACK actions. The effect struct and the EngineCtx
/// are disjoint ownership trees, so the callback may freely recurse into
/// engine calls with the provided ctx.
pub trait EffectHooks {
    fn dispatch_callback(&mut self, ctx: &mut EngineCtx, character: CharId, callback: &EffectCallback);
}

/// Hooks implementation for engine-internal use (no effect callbacks registered).
pub struct NoopHooks;
impl EffectHooks for NoopHooks {
    fn dispatch_callback(&mut self, _ctx: &mut EngineCtx, _character: CharId, _callback: &EffectCallback) {}
}

pub struct EngineCtx {
    pub terminal: Terminal,
    pub rng: Rng,
    pub clock: Clock,
    /// BaseEffectIterator.active_characters — canonical ascending-id order
    /// (CharId order == character_id order by construction).
    pub active_characters: ActiveCharacters,
    active_character_scratch: Vec<u64>,
    active_members_scratch: Vec<u64>,
    sparse_character_scratch: Vec<CharId>,
    idle_scheduling: bool,
    nested_updates: usize,
    eased_ticks: EasedTicksCache,
    pub preexisting_colors_present: bool,
    /// When Some, every event emission appends a trace line (test harness).
    pub event_log: Option<Vec<String>>,
}

impl EngineCtx {
    pub fn new(input_data: &str, config: TerminalConfig, rng: Rng, clock: Clock) -> Result<Self, EngineError> {
        let terminal = Terminal::new(input_data, config)?;
        let preexisting_colors_present = terminal.input_characters.iter().any(|&id| {
            let ch = &terminal.arena[id.0 as usize];
            ch.animation.input_fg_color.is_some() || ch.animation.input_bg_color.is_some()
        });
        Ok(EngineCtx {
            terminal,
            rng,
            clock,
            active_characters: ActiveCharacters::new(),
            active_character_scratch: Vec::new(),
            active_members_scratch: Vec::new(),
            sparse_character_scratch: Vec::new(),
            idle_scheduling: std::env::var_os("TTFX_SCHEDULER").is_none_or(|value| value != "0"),
            nested_updates: 0,
            eased_ticks: EasedTicksCache::default(),
            preexisting_colors_present,
            event_log: None,
        })
    }

    // ------------------------------------------------------------------
    // event dispatch (EventHandler._handle_event)
    // ------------------------------------------------------------------

    /// Whether an emission of `event` on `id` can have any observable effect —
    /// false lets hot emission sites skip building the CallerKey entirely.
    #[inline]
    fn observes_event(&self, id: CharId, event: Event) -> bool {
        self.event_log.is_some() || self.terminal.arena.event_handler(id.0 as usize).subscribes(event)
    }

    /// Execute all actions registered for (event, caller) on `id`, in
    /// registration order, inline and reentrantly. The action list is indexed
    /// per iteration because a callback may append more actions to it.
    pub fn handle_event(&mut self, hooks: &mut dyn EffectHooks, id: CharId, event: Event, caller: CallerRef<'_>) {
        if self.event_log.is_some() {
            let character_id = self.terminal.arena[id.0 as usize].character_id;
            let event_name = match event {
                Event::SegmentEntered => "SEGMENT_ENTERED",
                Event::SegmentExited => "SEGMENT_EXITED",
                Event::PathActivated => "PATH_ACTIVATED",
                Event::PathComplete => "PATH_COMPLETE",
                Event::PathHolding => "PATH_HOLDING",
                Event::SceneActivated => "SCENE_ACTIVATED",
                Event::SceneComplete => "SCENE_COMPLETE",
            };
            let caller_label = match caller {
                CallerRef::Path(pid) => format!("path:{pid}"),
                CallerRef::Waypoint(wp) => format!("wp:{}", wp.waypoint_id),
                CallerRef::Scene(sid) => format!("scene:{sid}"),
            };
            self.event_log
                .as_mut()
                .unwrap()
                .push(format!("EVENT char={character_id} {event_name} caller={caller_label}"));
        }
        let Some(entry_index) = self.terminal.arena[id.0 as usize].event_handler.actions_index(event, caller) else {
            return;
        };
        let mut action_index = 0;
        loop {
            let action = {
                let handler = &self.terminal.arena[id.0 as usize].event_handler;
                let actions = handler.actions(entry_index);
                if action_index >= actions.len() {
                    break;
                }
                actions[action_index].clone()
            };
            match action {
                EventAction::ActivatePath(path_id) => self.activate_path(hooks, id, &path_id),
                EventAction::ActivateScene(scene_id) => self.activate_scene(hooks, id, &scene_id),
                EventAction::DeactivatePath(target) => {
                    self.terminal.arena[id.0 as usize].motion.deactivate_path(target.as_deref());
                }
                EventAction::DeactivateScene(target) => {
                    self.deactivate_scene(id, target.as_deref());
                }
                EventAction::ResetAppearance => {
                    let ch = &mut self.terminal.arena[id.0 as usize];
                    let input_symbol = ch.input_symbol.clone();
                    let uses = ch.uses_input_preexisting_colors;
                    ch.animation.set_appearance(&input_symbol, uses, Some(&input_symbol.clone()), None);
                }
                EventAction::SetLayer(layer) => {
                    self.terminal.arena[id.0 as usize].layer = layer;
                }
                EventAction::SetCoordinate(coord) => {
                    self.terminal.arena[id.0 as usize].motion.current_coord = coord;
                }
                EventAction::Callback(cb) => {
                    hooks.dispatch_callback(self, id, &cb);
                }
            }
            action_index += 1;
        }
    }

    /// EventHandler.register_event: resolves/validates existence for id-based
    /// callers and targets, rejects duplicates.
    pub fn register_event(
        &mut self,
        id: CharId,
        event: Event,
        caller: CallerKey,
        action: EventAction,
    ) -> Result<(), String> {
        {
            let ch = &self.terminal.arena[id.0 as usize];
            match &caller {
                CallerKey::Path(pid) => {
                    if !ch.motion.paths.contains_key(pid) {
                        return Err(format!("path not found: {pid}"));
                    }
                }
                CallerKey::Scene(sid) => {
                    if !ch.animation.scenes.contains_key(sid) {
                        return Err(format!("scene not found: {sid}"));
                    }
                }
                CallerKey::Waypoint(_) => {}
            }
            match &action {
                EventAction::ActivatePath(pid) | EventAction::DeactivatePath(Some(pid)) => {
                    if !ch.motion.paths.contains_key(pid) {
                        return Err(format!("path not found: {pid}"));
                    }
                }
                EventAction::ActivateScene(sid) | EventAction::DeactivateScene(Some(sid)) => {
                    if !ch.animation.scenes.contains_key(sid) {
                        return Err(format!("scene not found: {sid}"));
                    }
                }
                _ => {}
            }
        }
        self.terminal.arena[id.0 as usize].event_handler.push(event, caller, action)
    }

    // ------------------------------------------------------------------
    // motion (Motion.activate_path / Path.step / Motion.move)
    // ------------------------------------------------------------------

    /// Motion.activate_path.
    pub fn activate_path(&mut self, hooks: &mut dyn EffectHooks, id: CharId, path_id: &str) {
        let (current_coord, first_waypoint) = {
            let ch = &self.terminal.arena[id.0 as usize];
            let path = ch.motion.paths.get(path_id).expect("activate_path: path not found");
            assert!(!path.waypoints.is_empty(), "activate_path: empty path {path_id}");
            (ch.motion.current_coord, path.waypoints[0].clone())
        };
        let distance_to_first_waypoint = match &first_waypoint.bezier_control {
            Some(control) => geometry::find_length_of_bezier_curve(current_coord, control, first_waypoint.coord),
            None => geometry::find_length_of_line(current_coord, first_waypoint.coord, true),
        };
        let new_origin_segment = Segment::new(Path::ORIGIN, 0, distance_to_first_waypoint);
        let layer = {
            let ch = &mut self.terminal.arena[id.0 as usize];
            ch.motion.active_path = ch.motion.paths.shared_key(path_id);
            let path = ch.motion.paths.get_mut(path_id).unwrap();
            path.origin_waypoint = Some(Waypoint {
                waypoint_id: ORIGIN_WAYPOINT_ID.with(Rc::clone),
                coord: current_coord,
                bezier_control: None,
            });
            path.total_distance += distance_to_first_waypoint;
            if let Some(distance) = path.origin_segment_distance {
                path.total_distance -= distance;
                path.segments[0] = new_origin_segment;
            } else {
                path.segments.reserve_exact(1);
                path.segments.insert(0, new_origin_segment);
            }
            path.origin_segment_distance = Some(distance_to_first_waypoint);
            path.set_current_step(0);
            path.hold_time_remaining = path.hold_time;
            path.max_steps = round_half_even(path.total_distance / path.speed);
            for segment in path.segments.iter_mut() {
                segment.enter_event_triggered = false;
                segment.exit_event_triggered = false;
            }
            path.layer
        };
        if let Some(layer) = layer {
            self.terminal.arena[id.0 as usize].layer = layer;
        }
        if self.observes_event(id, Event::PathActivated) {
            self.handle_event(hooks, id, Event::PathActivated, CallerRef::Path(path_id));
        }
    }

    /// Path.step on the given path of `id`. Index-based segment walk with
    /// re-borrow per access so reentrant mutation behaves like Python.
    ///
    /// The path's slot is resolved once and re-resolved after every emission,
    /// since only a reentrant action can move or drop it.
    fn path_step(&mut self, hooks: &mut dyn EffectHooks, id: CharId, path_id: &str) -> Coord {
        let mut slot =
            self.terminal.arena[id.0 as usize].motion.paths.slot(path_id).expect("path_step: path removed mid-step");
        macro_rules! path {
            () => {
                self.terminal.arena[id.0 as usize].motion.paths.at(slot)
            };
        }
        macro_rules! path_mut {
            () => {
                self.terminal.arena[id.0 as usize].motion.paths.at_mut(slot)
            };
        }
        macro_rules! resolve_slot {
            () => {
                slot = self.terminal.arena[id.0 as usize]
                    .motion
                    .paths
                    .slot(path_id)
                    .expect("path_step: path removed mid-step")
            };
        }

        let mut distance_to_travel = {
            let p = path_mut!();
            if p.max_steps == 0 || p.current_step() >= p.max_steps || p.total_distance == 0.0 {
                return p.waypoint_at(p.segments.last().expect("path has no segments").end).coord;
            }
            p.set_current_step(p.current_step() + 1);
            let ratio = p.current_step() as f64 / p.max_steps as f64;
            let distance_factor = match &p.ease {
                Some(ease) => ease.ease(ratio),
                None => ratio,
            };
            let distance = distance_factor * p.total_distance;
            p.set_last_distance_reached(distance);
            distance
        };

        let mut active_segment_index: Option<usize> = None;
        let mut i = 0usize;
        loop {
            let (seg_distance, enter_triggered, exit_triggered) = {
                let p = path!();
                if i >= p.segments.len() {
                    break;
                }
                let seg = &p.segments[i];
                (seg.distance, seg.enter_event_triggered, seg.exit_event_triggered)
            };
            if distance_to_travel <= seg_distance {
                active_segment_index = Some(i);
                if !enter_triggered {
                    if self.observes_event(id, Event::SegmentEntered) {
                        let seg_end_key = {
                            let p = path!();
                            p.waypoint_at(p.segments[i].end).key()
                        };
                        path_mut!().segments[i].enter_event_triggered = true;
                        self.handle_event(hooks, id, Event::SegmentEntered, CallerRef::Waypoint(&seg_end_key));
                        resolve_slot!();
                    } else {
                        path_mut!().segments[i].enter_event_triggered = true;
                    }
                }
                break;
            }
            distance_to_travel -= seg_distance;
            if !enter_triggered || !exit_triggered {
                let observes =
                    self.observes_event(id, Event::SegmentEntered) || self.observes_event(id, Event::SegmentExited);
                if !observes {
                    let seg = &mut path_mut!().segments[i];
                    seg.enter_event_triggered = true;
                    seg.exit_event_triggered = true;
                } else {
                    let seg_end_key = {
                        let p = path!();
                        p.waypoint_at(p.segments[i].end).key()
                    };
                    if !enter_triggered {
                        path_mut!().segments[i].enter_event_triggered = true;
                        self.handle_event(hooks, id, Event::SegmentEntered, CallerRef::Waypoint(&seg_end_key));
                        resolve_slot!();
                    }
                    if !exit_triggered {
                        path_mut!().segments[i].exit_event_triggered = true;
                        self.handle_event(hooks, id, Event::SegmentExited, CallerRef::Waypoint(&seg_end_key));
                        resolve_slot!();
                    }
                }
            }
            i += 1;
        }
        // Python for-else: overshoot past the last waypoint re-adds the final
        // segment's distance and travels beyond it (eased overshoot).
        let active_segment_index = match active_segment_index {
            Some(idx) => idx,
            None => {
                let p = path!();
                let idx = p.segments.len() - 1;
                distance_to_travel += p.segments[idx].distance;
                idx
            }
        };

        let p = path!();
        let seg = &p.segments[active_segment_index];
        let seg_distance = seg.distance;
        let t = if seg_distance == 0.0 {
            0.0
        } else if p.ease.is_some() {
            distance_to_travel / seg_distance // unclamped: eased overshoot goes past the waypoint
        } else {
            (distance_to_travel / seg_distance).min(1.0)
        };
        let start = p.waypoint_at(seg.start);
        let end = p.waypoint_at(seg.end);
        match &end.bezier_control {
            Some(control) => geometry::find_coord_on_bezier_curve(start.coord, control, end.coord, t),
            None => geometry::find_coord_on_line(start.coord, end.coord, t),
        }
    }

    /// Motion.move.
    pub fn motion_move(&mut self, hooks: &mut dyn EffectHooks, id: CharId) {
        // Admission checked subscriptions. Public edits invalidate the cursor;
        // tracing can be enabled directly, so it must bypass prepared stepping.
        if self.event_log.is_none() && self.terminal.arena.motion_tick(id.0 as usize) {
            return;
        }
        let slot = if !self.observes_event(id, Event::SegmentEntered) && !self.observes_event(id, Event::SegmentExited)
        {
            let Some(slot) = self.terminal.arena.motion_step(id.0 as usize) else { return };
            slot
        } else {
            let motion = &mut self.terminal.arena[id.0 as usize].motion;
            motion.previous_coord = motion.current_coord;
            let Some(path_id) = motion
                .active_path
                .as_ref()
                .filter(|pid| !motion.paths.get(pid).is_none_or(|p| p.segments.is_empty()))
                .cloned()
            else {
                return;
            };
            let new_coord = self.path_step(hooks, id, &path_id);
            let motion = &mut self.terminal.arena[id.0 as usize].motion;
            motion.current_coord = new_coord;
            // Segment callbacks can replace the active path synchronously.
            let active =
                motion.active_path.as_ref().expect("active path cleared mid-move (would be an upstream crash)");
            motion.paths.slot(active).expect("active path missing")
        };
        let active_path_id = self.terminal.arena[id.0 as usize].motion.active_path.as_ref().unwrap().clone();
        let (current_step, max_steps, hold_time, hold_time_remaining, loop_, segment_count) = {
            let p = self.terminal.arena[id.0 as usize].motion.paths.at(slot);
            (p.current_step(), p.max_steps, p.hold_time, p.hold_time_remaining, p.loop_, p.segments.len())
        };
        if current_step == max_steps {
            if hold_time != 0 && hold_time_remaining == hold_time {
                if self.observes_event(id, Event::PathHolding) {
                    self.handle_event(hooks, id, Event::PathHolding, CallerRef::Path(&active_path_id));
                }
                self.terminal.arena[id.0 as usize]
                    .motion
                    .paths
                    .get_mut(&active_path_id)
                    .unwrap()
                    .hold_time_remaining -= 1;
                return;
            }
            if hold_time_remaining != 0 {
                self.terminal.arena[id.0 as usize].motion.paths.at_mut(slot).hold_time_remaining -= 1;
                return;
            }
            if loop_ && segment_count > 1 {
                self.terminal.arena[id.0 as usize].motion.deactivate_path(Some(&active_path_id));
                self.activate_path(hooks, id, &active_path_id);
            } else {
                {
                    let motion = &mut self.terminal.arena[id.0 as usize].motion;
                    motion.completed_path = Some(active_path_id.clone());
                    motion.deactivate_path(Some(&active_path_id));
                }
                if self.observes_event(id, Event::PathComplete) {
                    self.handle_event(hooks, id, Event::PathComplete, CallerRef::Path(&active_path_id));
                }
            }
        }
    }

    /// Motion.chain_paths.
    pub fn chain_paths(&mut self, id: CharId, paths: &[String], loop_: bool) -> Result<(), String> {
        if paths.len() < 2 {
            return Ok(());
        }
        for i in 1..paths.len() {
            self.register_event(
                id,
                Event::PathComplete,
                CallerKey::Path(paths[i - 1].clone()),
                EventAction::ActivatePath(paths[i].clone()),
            )?;
        }
        if loop_ {
            self.register_event(
                id,
                Event::PathComplete,
                CallerKey::Path(paths[paths.len() - 1].clone()),
                EventAction::ActivatePath(paths[0].clone()),
            )?;
        }
        Ok(())
    }

    // ------------------------------------------------------------------
    // animation (Animation.activate_scene / step_animation)
    // ------------------------------------------------------------------

    /// Animation.activate_scene: does NOT reset playback (resume semantics).
    pub fn activate_scene(&mut self, hooks: &mut dyn EffectHooks, id: CharId, scene_id: &str) {
        {
            let mut animation = self.terminal.arena.animation_edit(id.0 as usize);
            let visual = animation
                .scenes
                .get_mut(scene_id)
                .expect("activate_scene: scene not found")
                .activate()
                .expect("activate_scene: empty scene");
            animation.active_scene = animation.scenes.handle(scene_id);
            animation.active_scene_current_step = 0;
            animation.current_character_visual = visual;
        }
        if self.observes_event(id, Event::SceneActivated) {
            self.handle_event(hooks, id, Event::SceneActivated, CallerRef::Scene(scene_id));
        }
        // Admit playback on its first tick, after effect setup/visibility/event
        // mutations, instead of constructing and immediately retiring a cursor.
    }

    /// Animation.deactivate_scene.
    pub fn deactivate_scene(&mut self, id: CharId, scene_id: Option<&str>) {
        let mut animation = self.terminal.arena.animation_edit(id.0 as usize);
        match scene_id {
            None => animation.active_scene = None,
            Some(sid) => {
                if animation.active_scene.as_deref() == Some(sid) {
                    animation.active_scene = None;
                }
            }
        }
    }

    /// Animation.step_animation.
    ///
    /// Nothing between here and complete_scene_if_finished can add or remove a
    /// scene, so the active scene's slot is resolved once and reused instead of
    /// looking the id up again at every step.
    pub fn step_animation(&mut self, hooks: &mut dyn EffectHooks, id: CharId) {
        if !self.terminal.arena.has_scene(id.0 as usize) {
            return;
        }
        // Take the mutable arena boundary once. No callback can run until the
        // scene has been stepped, so its slot and flags remain valid here.
        let stationary = self.terminal.arena.stationary(id.0 as usize);
        let mut edit = self.terminal.arena.animation_edit(id.0 as usize);
        let anim = &mut *edit;
        let Some(active) = &anim.active_scene else { return };
        let scene_slot = anim.scenes.handle_slot(active).expect("active scene missing");
        let scene = anim.scenes.at_mut(scene_slot);
        if scene.frames().is_empty() {
            return;
        }
        let prepare_moving = !stationary && !scene.is_looping && scene.sync.is_none();
        if let Some(sync) = scene.sync {
            drop(edit);
            self.step_synced_scene(id, scene_slot, sync);
        } else if let Some(ease) = scene.ease {
            drop(edit);
            self.step_eased_scene(id, scene_slot, ease);
        } else {
            let head = scene.step_frame();
            replace_visual(&mut anim.current_character_visual, &scene.all_frames[head].character_visual);
            if !scene.is_looping && !scene.frames().is_empty() {
                let ticks = scene.ticks_elapsed();
                // A completed path can leave previous_coord one step behind.
                // Let the next ordinary tick settle it before skipping motion.
                let idle = if ticks > 0 && stationary {
                    let frames = scene.frames();
                    scene.all_frames[head].duration - ticks - i64::from(frames.start + 1 == frames.end)
                } else {
                    0
                };
                drop(edit);
                if prepare_moving {
                    self.terminal.arena.prepare_scene(id.0 as usize);
                }
                if self.idle_scheduling
                    && self.nested_updates == 0
                    && idle > 0
                    && self.terminal.arena.playback.is_current(id.0 as usize)
                {
                    let kind = if self.terminal.arena.prepare_scene(id.0 as usize) {
                        PlaybackKind::Runtime
                    } else {
                        PlaybackKind::Plain
                    };
                    self.terminal.arena.playback.sleep(id.0 as usize, scene_slot, idle, kind);
                }
                // An incomplete non-looping scene emits no completion event.
                return;
            }
            drop(edit);
        }

        self.complete_scene_if_finished(hooks, id, scene_slot);
        if prepare_moving {
            self.terminal.arena.prepare_scene(id.0 as usize);
        }
    }

    /// Animation._step_synced_scene + _synced_scene_frame_index.
    fn step_synced_scene(&mut self, id: CharId, scene_slot: usize, sync: SyncMetric) {
        let active_path_state = self.terminal.arena.motion_progress(id.0 as usize);
        let mut edit = self.terminal.arena.animation_edit(id.0 as usize);
        let anim = &mut *edit;
        let scene = anim.scenes.at_mut(scene_slot);
        match active_path_state {
            None => {
                // no active path: jump to final frame and force-complete
                let last = scene.frames().end - 1;
                replace_visual(&mut anim.current_character_visual, &scene.all_frames[last].character_visual);
                scene.finish_frames();
            }
            Some((current_step, max_steps, total_distance, last_distance_reached)) => {
                let final_frame_index = scene.frames().len() as i64 - 1;
                let progress_ratio = match sync {
                    SyncMetric::Step => current_step.max(1) as f64 / max_steps.max(1) as f64,
                    SyncMetric::Distance => {
                        let total = total_distance.max(1.0);
                        let remaining = (total_distance - last_distance_reached).max(1.0);
                        let reached = (total - remaining).max(1.0);
                        reached / total
                    }
                };
                let frame_index =
                    round_half_even(final_frame_index as f64 * progress_ratio).min(final_frame_index).max(0);
                let frame = scene.frames().start + frame_index as usize;
                replace_visual(&mut anim.current_character_visual, &scene.all_frames[frame].character_visual);
            }
        }
    }

    /// Animation._step_eased_scene (+ _ease_animation).
    fn step_eased_scene(&mut self, id: CharId, scene_slot: usize, ease: crate::utils::easing::Easing) {
        let allow_idle =
            self.idle_scheduling && self.nested_updates == 0 && self.terminal.arena.playback.is_current(id.0 as usize);
        let allow_idle = allow_idle && self.terminal.arena.stationary(id.0 as usize);
        let mut edit = self.terminal.arena.animation_edit(id.0 as usize);
        let anim = &mut *edit;
        let scene = anim.scenes.at_mut(scene_slot);
        let (frame, idle) = scene.step_eased(ease, &mut self.eased_ticks, allow_idle);
        replace_visual(&mut anim.current_character_visual, &scene.all_frames[frame].character_visual);
        drop(edit);
        if idle > 0 {
            let kind = if self.terminal.arena.prepare_scene(id.0 as usize) {
                PlaybackKind::Runtime
            } else {
                PlaybackKind::Eased
            };
            self.terminal.arena.playback.sleep(id.0 as usize, scene_slot, idle, kind);
        }
    }

    /// Animation._complete_scene_if_finished: fires SCENE_COMPLETE every tick
    /// for looping scenes, faithfully.
    fn complete_scene_if_finished(&mut self, hooks: &mut dyn EffectHooks, id: CharId, scene_slot: usize) {
        {
            // The stepping above cannot clear active_scene, so the slot still
            // holds it and active_scene_is_complete reduces to its scene test.
            let anim = &self.terminal.arena[id.0 as usize].animation;
            let scene = anim.scenes.at(scene_slot);
            if !(scene.frames().is_empty() || scene.is_looping) {
                return;
            }
        }
        {
            let mut anim = self.terminal.arena.animation_edit(id.0 as usize);
            let scene = anim.scenes.at_mut(scene_slot);
            if !scene.is_looping {
                scene.reset_scene();
                anim.active_scene = None;
            }
        }
        if self.observes_event(id, Event::SceneComplete) {
            let scene_id = Rc::clone(self.terminal.arena[id.0 as usize].animation.scenes.key_at(scene_slot));
            self.handle_event(hooks, id, Event::SceneComplete, CallerRef::Scene(&scene_id));
        }
    }

    // ------------------------------------------------------------------
    // ticking (EffectCharacter.tick / BaseEffectIterator.update / frame)
    // ------------------------------------------------------------------

    /// EffectCharacter.tick: motion first, then animation.
    pub fn tick(&mut self, hooks: &mut dyn EffectHooks, id: CharId) {
        if self.terminal.arena.scene_tick(id.0 as usize, self.idle_scheduling && self.nested_updates == 0, false) {
            return;
        }
        let moving = self.terminal.arena.settle_stationary_motion(id.0 as usize);
        if moving {
            self.motion_move(hooks, id);
            if self.terminal.arena.scene_tick(id.0 as usize, false, true) {
                return;
            }
        }
        self.step_animation(hooks, id);
        if !moving {
            self.terminal.arena.prepare_scene(id.0 as usize);
        }
    }

    /// Allows comparison with the ordinary object-based playback implementation.
    pub fn set_scene_runtime(&mut self, enabled: bool) {
        self.terminal.arena.set_scene_runtime(enabled);
    }

    /// Compare prepared motion against the ordinary event-free path walker.
    pub fn set_motion_runtime(&mut self, enabled: bool) {
        self.terminal.arena.set_motion_runtime(enabled);
    }

    /// Allows a deterministic reference run without held-frame scheduling.
    pub fn set_idle_scheduling(&mut self, enabled: bool) {
        self.terminal.arena.wake_all();
        self.idle_scheduling = enabled;
    }

    /// Tick the original active snapshot in ascending order, restoring sleepers
    /// invalidated by callbacks before their turn, then prune inactive members.
    pub fn update(&mut self, hooks: &mut dyn EffectHooks) {
        // Nested updates preserve the original snapshot semantics. Settle every
        // outer sleeper first; its wake is replayed in the outer snapshot too.
        if self.terminal.arena.playback.updating() {
            self.terminal.arena.wake_all();
            self.nested_updates += 1;
            let snapshot: Vec<_> = self.active_characters.iter().collect();
            for id in snapshot {
                self.tick(hooks, id);
            }
            let arena = &self.terminal.arena;
            self.active_characters.retain(|id| arena.is_active(id.0 as usize));
            self.nested_updates -= 1;
            return;
        }
        if self.active_characters.len() < 128 {
            self.update_sparse(hooks);
            return;
        }
        let mut members = std::mem::take(&mut self.active_members_scratch);
        self.active_characters.copy_words_into(&mut members);
        self.terminal.arena.begin_update(&members);
        let mut snapshot = std::mem::take(&mut self.active_character_scratch);
        snapshot.clear();
        snapshot.extend(
            members
                .iter()
                .enumerate()
                .map(|(i, word)| word & !self.terminal.arena.playback.sleeping.get(i).copied().unwrap_or(0)),
        );
        for word in 0..snapshot.len() {
            while snapshot[word] != 0 {
                let bit = snapshot[word].trailing_zeros() as usize;
                snapshot[word] &= snapshot[word] - 1;
                let id = word * 64 + bit;
                self.terminal.arena.playback.set_cursor(id);
                self.tick(hooks, CharId(id as u32));
                // A callback may wake a higher slot after snapshot filtering.
                // Reinsert only original members which have not ticked yet.
                for awakened in self.terminal.arena.playback.woken.drain(..) {
                    if awakened > id {
                        let word = awakened / 64;
                        let bit = 1 << (awakened % 64);
                        if members.get(word).is_some_and(|members| members & bit != 0) {
                            snapshot[word] |= bit;
                        }
                    }
                }
            }
        }
        let arena = &self.terminal.arena;
        self.active_characters.retain_unless_masked(&arena.playback.sleeping, |id| arena.is_active(id.0 as usize));
        self.active_characters.copy_words_into(&mut members);
        self.terminal.arena.finish_update(&members);
        self.active_members_scratch = members;
        self.active_character_scratch = snapshot;
    }

    /// A few high-numbered characters should cost O(active), not require a
    /// bitmap snapshot spanning every earlier arena slot. Keeping sleepers in
    /// this small snapshot also naturally handles callbacks that wake them.
    fn update_sparse(&mut self, hooks: &mut dyn EffectHooks) {
        self.terminal.arena.begin_sparse_update(&self.active_characters);
        let mut snapshot = std::mem::take(&mut self.sparse_character_scratch);
        snapshot.clear();
        snapshot.extend(self.active_characters.iter());
        for &id in &snapshot {
            self.terminal.arena.playback.set_cursor(id.0 as usize);
            if !self.terminal.arena.playback.is_sleeping(id.0 as usize) {
                self.tick(hooks, id);
            }
        }
        let arena = &self.terminal.arena;
        self.active_characters.retain_unless_masked(&arena.playback.sleeping, |id| arena.is_active(id.0 as usize));
        self.terminal.arena.finish_sparse_update(&self.active_characters);
        self.sparse_character_scratch = snapshot;
    }

    /// BaseEffectIterator.frame: enforce framerate (real clock only), then the
    /// formatted output string; advances the virtual clock by one frame.
    pub fn frame(&mut self) -> crate::engine::terminal::FrameOutput {
        if matches!(self.clock, Clock::Real { .. }) && self.terminal.config.frame_rate != 0 {
            self.terminal.enforce_framerate();
        }
        self.clock.advance_frame();
        self.terminal.prepare_frame_output()
    }
}

/// Rc's default clone_from still increments and decrements reference counts.
/// A held frame can keep its existing owner without touching a shared cache line.
#[inline]
fn replace_visual(
    current: &mut Rc<crate::engine::animation::CharacterVisual>,
    next: &Rc<crate::engine::animation::CharacterVisual>,
) {
    if !Rc::ptr_eq(current, next) {
        *current = Rc::clone(next);
    }
}
