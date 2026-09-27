//! Persistent playback for non-looping, unsynced scenes.
//!
//! The public Scene remains the construction/introspection API. Ordinary ticks
//! use a dense 32-byte cursor instead of chasing each character's scene map.
//! Reads materialize counters and writes retire the cursor before exposing a
//! mutable character. Immutable frame programs are shared with the Scene.
use std::cell::Cell;
use std::rc::Rc;

use super::animation::{Frame, FrameStorage, Scene};
use super::character::EffectCharacter;
use crate::utils::easing::Easing;

#[derive(Clone, Copy, Debug, Default)]
struct Cursor {
    ticks: i64,
    duration: i64,
    head: u32,
    end: u32,
    played: u32,
    shown: u32,
}

#[derive(Clone, Debug)]
struct Program {
    frames: Rc<[Frame]>,
    scene: usize,
    eased: Option<Rc<EasedProgram>>,
    stationary: bool,
}

#[derive(Clone, Copy, Debug)]
struct EasedRun {
    frame: u32,
    end: u32,
}

#[derive(Debug)]
struct EasedProgram {
    runs: Box<[EasedRun]>,
}

#[derive(Clone, Debug)]
struct EasedShape {
    ease: Easing,
    total: i64,
    duration: i64,
    uses: u8,
    program: Option<Rc<EasedProgram>>,
}

/// Uniform-frame easing depends only on the curve, total, and frame duration.
/// Keep at most sixteen plans, including plans still used by active cursors.
/// Unusual/large/diverse shapes continue through the ordinary scalar path.
#[derive(Clone, Debug, Default)]
struct EasedPrograms {
    shapes: Vec<EasedShape>,
    last: usize,
    next: usize,
}

impl EasedPrograms {
    fn get(&mut self, scene: &Scene, frames: usize) -> Option<Rc<EasedProgram>> {
        let ease = scene.ease?;
        let duration = scene.uniform_frame_duration()?;
        let total = scene.easing_total_steps;
        if !(1..=65_536).contains(&total) || duration <= 0 || ((total - 1) / duration) as usize >= frames {
            return None;
        }
        let matches = |shape: &EasedShape| shape.ease == ease && shape.total == total && shape.duration == duration;
        let slot = if self.shapes.get(self.last).is_some_and(matches) {
            self.last
        } else if let Some(slot) = self.shapes.iter().position(matches) {
            slot
        } else {
            let shape = EasedShape { ease, total, duration, uses: 0, program: None };
            if self.shapes.len() < 16 {
                self.shapes.push(shape);
                self.shapes.len() - 1
            } else {
                let slot = (0..16)
                    .map(|offset| (self.next + offset) % 16)
                    .find(|&slot| self.shapes[slot].program.as_ref().is_none_or(|plan| Rc::strong_count(plan) == 1))?;
                self.shapes[slot] = shape;
                self.next = (slot + 1) % 16;
                slot
            }
        };
        self.last = slot;
        let shape = &mut self.shapes[slot];
        if shape.program.is_none() {
            shape.uses += 1;
            if shape.uses < 8 {
                return None;
            }
            let mut runs: Box<[_]> = (0..total)
                .map(|step| EasedRun { frame: scene.eased_runtime_frame(ease, step) as u32, end: step as u32 + 1 })
                .collect();
            for step in (0..runs.len() - 1).rev() {
                if runs[step].frame == runs[step + 1].frame {
                    runs[step].end = runs[step + 1].end;
                }
            }
            shape.program = Some(Rc::new(EasedProgram { runs }));
        }
        shape.program.clone()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct SceneRuntime {
    cursors: Vec<Cell<Cursor>>,
    programs: Vec<Option<Program>>,
    members: Vec<u64>,
    count: usize,
    stationary_members: Vec<u64>,
    stationary_count: usize,
    eased_programs: EasedPrograms,
    pub(crate) enabled: bool,
}

impl Default for SceneRuntime {
    fn default() -> Self {
        Self {
            cursors: Vec::new(),
            programs: Vec::new(),
            members: Vec::new(),
            count: 0,
            stationary_members: Vec::new(),
            stationary_count: 0,
            eased_programs: EasedPrograms::default(),
            enabled: std::env::var_os("TTFX_SCENE_RUNTIME").is_none_or(|value| value != "0"),
        }
    }
}

impl SceneRuntime {
    pub(crate) fn resize(&mut self, len: usize) {
        self.cursors.resize_with(len, Cell::default);
        self.programs.resize_with(len, || None);
        self.members.resize(len.div_ceil(64), 0);
        self.stationary_members.resize(len.div_ceil(64), 0);
    }

    #[inline]
    pub(crate) fn contains(&self, id: usize) -> bool {
        self.count != 0 && self.members[id / 64] & (1 << (id % 64)) != 0
    }

    #[inline]
    pub(crate) fn contains_stationary(&self, id: usize) -> bool {
        self.stationary_count != 0 && self.stationary_members[id / 64] & (1 << (id % 64)) != 0
    }

    pub(crate) fn prepare(&mut self, id: usize, characters: &[EffectCharacter]) {
        if !self.enabled || self.contains(id) {
            return;
        }
        let ch = &characters[id];
        let stationary = ch.motion.active_path.is_none() && ch.motion.previous_coord == ch.motion.current_coord;
        let anim = &ch.animation;
        let Some(slot) = anim.active_scene.as_ref().and_then(|active| anim.scenes.handle_slot(active)) else {
            return;
        };
        let scene = anim.scenes.at(slot);
        if scene.is_looping || scene.sync.is_some() {
            return;
        }
        let FrameStorage::Shared(frames) = &scene.all_frames else {
            return;
        };
        let remaining = scene.frames();
        if remaining.is_empty() || remaining.end > frames.len() || remaining.end >= u32::MAX as usize {
            return;
        }
        let eased = if scene.ease.is_some() {
            let Some(plan) = self.eased_programs.get(scene, frames.len()) else { return };
            Some(plan)
        } else {
            None
        };
        let (ticks, duration) = if eased.is_some() {
            (scene.easing_current_step(), scene.easing_total_steps)
        } else {
            (scene.ticks_elapsed(), frames[remaining.start].duration)
        };
        let played = scene.played_frames().end;
        if duration <= 0 || ticks < 0 || ticks >= duration || played > u32::MAX as usize {
            return;
        }
        self.cursors[id].set(Cursor {
            ticks,
            duration,
            head: remaining.start as u32,
            end: remaining.end as u32,
            played: played as u32,
            // The visual may have been changed independently, or the previous
            // tick may have retired a frame. Compare once on the first tick.
            shown: u32::MAX,
        });
        self.programs[id] = Some(Program { frames: Rc::clone(frames), scene: slot, eased, stationary });
        self.members[id / 64] |= 1 << (id % 64);
        self.count += 1;
        if stationary {
            self.stationary_members[id / 64] |= 1 << (id % 64);
            self.stationary_count += 1;
        }
    }

    #[inline]
    pub(crate) fn materialize(&self, id: usize, characters: &[EffectCharacter]) {
        if self.contains(id) {
            self.materialize_cached(id, characters);
        }
    }

    // Keep the public arena accessors small enough to inline on motion-only
    // paths. The actual hot scene loop never needs to materialize its counters.
    #[inline(never)]
    fn materialize_cached(&self, id: usize, characters: &[EffectCharacter]) {
        let program = self.programs[id].as_ref().unwrap();
        let cursor = self.cursors[id].get();
        let scene = characters[id].animation.scenes.at(program.scene);
        if program.eased.is_some() {
            scene.set_eased_cursor(cursor.ticks);
        } else {
            scene.set_plain_cursor(cursor.head as usize, cursor.played as usize, cursor.ticks);
        }
    }

    #[inline]
    pub(crate) fn invalidate(&mut self, id: usize, characters: &[EffectCharacter]) {
        if self.contains(id) {
            self.invalidate_cached(id, characters);
        }
    }

    #[inline(never)]
    fn invalidate_cached(&mut self, id: usize, characters: &[EffectCharacter]) {
        self.materialize_cached(id, characters);
        if self.programs[id].as_ref().unwrap().stationary {
            self.stationary_members[id / 64] &= !(1 << (id % 64));
            self.stationary_count -= 1;
        }
        self.programs[id] = None;
        self.members[id / 64] &= !(1 << (id % 64));
        self.count -= 1;
    }

    /// Same bound as the scheduler: at most one intermediate frame retirement.
    pub(crate) fn advance_held_ticks(&self, id: usize, count: i64) {
        let program = self.programs[id].as_ref().expect("scheduled runtime missing");
        let mut cursor = self.cursors[id].get();
        cursor.ticks += count;
        debug_assert!(cursor.ticks <= cursor.duration);
        if program.eased.is_some() {
            debug_assert!(cursor.ticks < cursor.duration);
        } else if cursor.ticks == cursor.duration {
            debug_assert!(cursor.head + 1 < cursor.end);
            cursor.head += 1;
            cursor.played = cursor.head;
            cursor.ticks = 0;
            cursor.duration = program.frames[cursor.head as usize].duration;
        }
        self.cursors[id].set(cursor);
    }

    /// None hands completion and unusual mutated durations to the ordinary
    /// engine. Some returns whether rendering changed, the scene slot, and the
    /// number of subsequent ticks which can only hold the current visual.
    #[inline]
    pub(crate) fn tick(&mut self, id: usize, characters: &mut [EffectCharacter]) -> Option<(bool, usize, i64)> {
        let program = self.programs[id].as_ref()?;
        let mut cursor = self.cursors[id].get();
        if let Some(eased) = &program.eased {
            if cursor.ticks + 1 == cursor.duration {
                self.invalidate(id, characters);
                return None;
            }
            let run = eased.runs[cursor.ticks as usize];
            let mut changed = false;
            if cursor.shown != run.frame {
                let next = &program.frames[run.frame as usize].character_visual;
                let current = &mut characters[id].animation.current_character_visual;
                if !Rc::ptr_eq(current, next) {
                    *current = Rc::clone(next);
                    changed = true;
                }
                cursor.shown = run.frame;
            }
            cursor.ticks += 1;
            let idle = i64::from(run.end).min(cursor.duration - 1) - cursor.ticks;
            self.cursors[id].set(cursor);
            return Some((changed, program.scene, idle));
        }
        if cursor.duration <= 0 || (cursor.head + 1 == cursor.end && cursor.ticks + 1 == cursor.duration) {
            self.invalidate(id, characters);
            return None;
        }
        let mut changed = false;
        if cursor.shown != cursor.head {
            let next = &program.frames[cursor.head as usize].character_visual;
            let current = &mut characters[id].animation.current_character_visual;
            if !Rc::ptr_eq(current, next) {
                *current = Rc::clone(next);
                changed = true;
            }
            cursor.shown = cursor.head;
        }
        cursor.ticks += 1;
        if cursor.ticks == cursor.duration {
            cursor.head += 1;
            cursor.played = cursor.head;
            cursor.ticks = 0;
            cursor.duration = program.frames[cursor.head as usize].duration;
        }
        let idle = if cursor.ticks > 0 {
            cursor.duration - cursor.ticks - i64::from(cursor.head + 1 == cursor.end)
        } else {
            0
        };
        self.cursors[id].set(cursor);
        Some((changed, program.scene, idle))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::animation::VisualParams;

    fn eased_scene(total: i64) -> Scene {
        let mut scene = Scene::new("test", false, None, Some(Easing::OutElastic), false, false);
        scene.add_frame("X", total, VisualParams::default()).unwrap();
        scene
    }

    #[test]
    fn active_easing_plans_are_bounded_and_fall_back_when_full() {
        let mut cache = EasedPrograms::default();
        let mut active = Vec::new();
        for total in 100..116 {
            let scene = eased_scene(total);
            for _ in 0..7 {
                assert!(cache.get(&scene, 1).is_none());
            }
            active.push(cache.get(&scene, 1).unwrap());
        }
        let extra = eased_scene(116);
        for _ in 0..20 {
            assert!(cache.get(&extra, 1).is_none());
        }
        active.pop();
        for _ in 0..7 {
            assert!(cache.get(&extra, 1).is_none());
        }
        assert!(cache.get(&extra, 1).is_some());
        assert!(cache.get(&eased_scene(1_000_000_000), 1).is_none());
    }

    #[test]
    fn compact_plans_preserve_exact_frame_indices_and_curve_reversals() {
        let mut cache = EasedPrograms::default();
        for ease in [Easing::Linear, Easing::OutElastic, Easing::InOutBack, Easing::CubicBezier(0.2, -0.8, 0.7, 1.5)] {
            for duration in [1, 3, 11] {
                let mut scene = Scene::new("test", false, None, Some(ease), false, false);
                for symbol in ["A", "B", "C", "D"] {
                    scene.add_frame(symbol, duration, VisualParams::default()).unwrap();
                }
                for _ in 0..7 {
                    cache.get(&scene, 4);
                }
                let plan = cache.get(&scene, 4).unwrap();
                for (step, run) in plan.runs.iter().enumerate() {
                    let tick = super::super::animation::Scene::eased_runtime_frame(&scene, ease, step as i64);
                    assert_eq!(run.frame as usize, tick);
                    assert!(run.end as usize > step);
                    for held in step..run.end as usize {
                        assert_eq!(plan.runs[held].frame, run.frame);
                    }
                    if (run.end as usize) < plan.runs.len() {
                        assert_ne!(plan.runs[run.end as usize].frame, run.frame);
                    }
                }
            }
        }
    }
}
