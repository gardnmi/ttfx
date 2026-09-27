//! Skip scene ticks that can only increment a held visual's counter.
//!
//! Final retirement, looping, motion, and events stay on the ordinary
//! engine path. Arena reads materialize elapsed counters; mutable access also
//! wakes the character. No references or raw pointers survive a scene mutation.
use std::cell::Cell;

use super::active_characters::ActiveCharacters;
use super::character::{CharId, EffectCharacter};
use super::scene_runtime::SceneRuntime;

const WHEEL_SIZE: usize = 256;

#[derive(Clone, Copy, Debug)]
struct HeldFrame {
    // Scene records are larger than two bytes, so valid Vec scene indices
    // cannot reach either top bit. Keep the playback kind in those bits.
    scene: usize,
    accounted: u64,
    wake_at: u64,
}

const EASED_SCENE: usize = 1 << (usize::BITS - 1);
const RUNTIME_SCENE: usize = 1 << (usize::BITS - 2);
const SCENE_MASK: usize = !(EASED_SCENE | RUNTIME_SCENE);

#[derive(Clone, Copy, Debug)]
pub(crate) enum PlaybackKind {
    Plain,
    Eased,
    Runtime,
}

#[derive(Clone, Debug)]
pub(crate) struct PlaybackScheduler {
    held: Vec<Cell<Option<HeldFrame>>>,
    pub(crate) sleeping: Vec<u64>,
    sleeping_count: usize,
    wheel: [Vec<usize>; WHEEL_SIZE],
    pub(crate) woken: Vec<usize>,
    tick: u64,
    cursor: Option<usize>,
}

impl Default for PlaybackScheduler {
    fn default() -> Self {
        Self {
            held: Vec::new(),
            sleeping: Vec::new(),
            sleeping_count: 0,
            wheel: std::array::from_fn(|_| Vec::new()),
            woken: Vec::new(),
            tick: 0,
            cursor: None,
        }
    }
}

impl PlaybackScheduler {
    pub(crate) fn resize(&mut self, len: usize) {
        self.held.resize_with(len, || Cell::new(None));
        self.sleeping.resize(len.div_ceil(64), 0);
    }

    /// During a callback, higher slots have not received this update's tick yet.
    /// Outside update, every surviving sleeper has received the completed tick.
    #[inline]
    pub(crate) fn materialize(&self, id: usize, characters: &[EffectCharacter], runtime: &SceneRuntime) {
        if self.is_sleeping(id) {
            self.materialize_held(id, characters, runtime);
        }
    }

    #[inline(never)]
    fn materialize_held(&self, id: usize, characters: &[EffectCharacter], runtime: &SceneRuntime) {
        let mut held = self.held[id].get().expect("sleeping bit without playback state");
        let through = if self.cursor.is_some_and(|cursor| id >= cursor) { self.tick - 1 } else { self.tick };
        let through = through.min(held.wake_at - 1);
        if through > held.accounted {
            let count = (through - held.accounted) as i64;
            if held.scene & RUNTIME_SCENE != 0 {
                runtime.advance_held_ticks(id, count);
            } else {
                let scene = characters[id].animation.scenes.at(held.scene & SCENE_MASK);
                if held.scene & EASED_SCENE == 0 {
                    scene.advance_held_ticks(count);
                } else {
                    scene.advance_eased_ticks(count);
                }
            }
            held.accounted = through;
            self.held[id].set(Some(held));
        }
    }

    #[inline]
    pub(crate) fn wake(&mut self, id: usize, characters: &[EffectCharacter], runtime: &SceneRuntime) {
        if self.is_sleeping(id) {
            self.wake_held(id, characters, runtime);
        }
    }

    #[inline(never)]
    fn wake_held(&mut self, id: usize, characters: &[EffectCharacter], runtime: &SceneRuntime) {
        self.materialize_held(id, characters, runtime);
        self.held[id].set(None);
        self.sleeping[id / 64] &= !(1 << (id % 64));
        self.sleeping_count -= 1;
        self.woken.push(id);
    }

    pub(crate) fn wake_all(&mut self, characters: &[EffectCharacter], runtime: &SceneRuntime) {
        if self.sleeping_count == 0 {
            return;
        }
        for word in 0..self.sleeping.len() {
            let mut bits = self.sleeping[word];
            while bits != 0 {
                let bit = bits.trailing_zeros() as usize;
                bits &= bits - 1;
                self.wake(word * 64 + bit, characters, runtime);
            }
        }
    }

    fn wake_removed(&mut self, members: &[u64], characters: &[EffectCharacter], runtime: &SceneRuntime) {
        if self.sleeping_count == 0 {
            return;
        }
        for word in 0..self.sleeping.len() {
            let mut bits = self.sleeping[word] & !members.get(word).copied().unwrap_or(0);
            while bits != 0 {
                let bit = bits.trailing_zeros() as usize;
                bits &= bits - 1;
                self.wake(word * 64 + bit, characters, runtime);
            }
        }
    }

    fn wake_removed_sparse(
        &mut self,
        members: &ActiveCharacters,
        characters: &[EffectCharacter],
        runtime: &SceneRuntime,
    ) {
        if self.sleeping_count == 0 {
            return;
        }
        for word in 0..self.sleeping.len() {
            let mut bits = self.sleeping[word];
            while bits != 0 {
                let id = word * 64 + bits.trailing_zeros() as usize;
                bits &= bits - 1;
                if !members.contains(&CharId(id as u32)) {
                    self.wake(id, characters, runtime);
                }
            }
        }
    }

    #[inline]
    pub(crate) fn is_sleeping(&self, id: usize) -> bool {
        self.sleeping_count != 0 && self.sleeping[id / 64] & (1 << (id % 64)) != 0
    }

    pub(crate) fn updating(&self) -> bool {
        self.cursor.is_some()
    }

    pub(crate) fn begin(&mut self, members: &[u64], characters: &[EffectCharacter], runtime: &SceneRuntime) {
        debug_assert!(!self.updating());
        self.wake_removed(members, characters, runtime);
        self.begin_clock(characters, runtime);
    }

    pub(crate) fn begin_sparse(
        &mut self,
        members: &ActiveCharacters,
        characters: &[EffectCharacter],
        runtime: &SceneRuntime,
    ) {
        debug_assert!(!self.updating());
        self.wake_removed_sparse(members, characters, runtime);
        self.begin_clock(characters, runtime);
    }

    fn begin_clock(&mut self, characters: &[EffectCharacter], runtime: &SceneRuntime) {
        self.tick = self.tick.checked_add(1).expect("animation update counter exhausted");
        self.cursor = Some(0);
        let bucket = self.tick as usize % WHEEL_SIZE;
        let mut due = std::mem::take(&mut self.wheel[bucket]);
        for &id in &due {
            if self.held[id].get().is_some_and(|held| held.wake_at == self.tick) {
                self.wake(id, characters, runtime);
            }
        }
        due.clear();
        self.wheel[bucket] = due;
        // Wakes before the snapshot are already represented by sleeping's mask.
        self.woken.clear();
    }

    pub(crate) fn set_cursor(&mut self, id: usize) {
        self.cursor = Some(id);
    }

    pub(crate) fn finish(&mut self, members: &[u64], characters: &[EffectCharacter], runtime: &SceneRuntime) {
        self.cursor = None;
        self.wake_removed(members, characters, runtime);
        self.woken.clear();
    }

    pub(crate) fn finish_sparse(
        &mut self,
        members: &ActiveCharacters,
        characters: &[EffectCharacter],
        runtime: &SceneRuntime,
    ) {
        self.cursor = None;
        self.wake_removed_sparse(members, characters, runtime);
        self.woken.clear();
    }

    pub(crate) fn is_current(&self, id: usize) -> bool {
        self.cursor == Some(id)
    }

    pub(crate) fn sleep(&mut self, id: usize, scene: usize, remaining: i64, kind: PlaybackKind) {
        if remaining <= 0 {
            return;
        }
        let skip = remaining.min((WHEEL_SIZE - 1) as i64) as u64;
        let wake_at = self.tick.checked_add(skip + 1).expect("animation wake counter exhausted");
        debug_assert!(!self.is_sleeping(id));
        self.sleeping_count += 1;
        debug_assert!(scene < RUNTIME_SCENE);
        let scene = scene
            | match kind {
                PlaybackKind::Plain => 0,
                PlaybackKind::Eased => EASED_SCENE,
                PlaybackKind::Runtime => RUNTIME_SCENE,
            };
        self.held[id].set(Some(HeldFrame { scene, accounted: self.tick, wake_at }));
        self.sleeping[id / 64] |= 1 << (id % 64);
        self.wheel[wake_at as usize % WHEEL_SIZE].push(id);
    }
}
