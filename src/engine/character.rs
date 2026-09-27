//! EffectCharacter, ported from engine/base_character.py, stored in an arena.
//!
//! `CharId` is the arena slot index. `character_id` is the Python-compatible
//! monotonically allocated id — these are NOT the same thing: the Python parser
//! allocates ids for characters that are later overwritten by cursor movement,
//! popped as trailing whitespace, or cropped by the canvas, so surviving
//! characters have id gaps. All canonical orderings sort by `character_id`.

use crate::engine::animation::Animation;
use crate::engine::events::EventHandler;
use crate::engine::motion::Motion;
use crate::utils::geometry::Coord;

/// Arena slot index (dense). Never used for ordering.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CharId(pub u32);

/// Cardinal neighbor slots, upstream's dict keys north/east/south/west.
#[derive(Debug, Clone, Copy, Default)]
pub struct Neighbors {
    pub north: Option<CharId>,
    pub east: Option<CharId>,
    pub south: Option<CharId>,
    pub west: Option<CharId>,
}

#[derive(Debug, Clone)]
pub struct EffectCharacter {
    /// Python-compatible allocation id; canonical ordering key.
    pub character_id: u32,
    pub input_symbol: String,
    pub input_coord: Coord,
    /// Raw input SGR sequences captured at parse time (fg, bg).
    pub input_ansi_fg_sequence: Option<String>,
    pub input_ansi_bg_sequence: Option<String>,
    pub is_visible: bool,
    pub animation: Animation,
    pub motion: Motion,
    pub event_handler: EventHandler,
    pub layer: i64,
    pub is_fill_character: bool,
    pub uses_input_preexisting_colors: bool,
    /// Spanning-tree links (character ids, insertion-ordered — upstream uses a
    /// set, but iteration order is behavior; see plan.md §4.3).
    pub links: Vec<CharId>,
    pub neighbors: Neighbors,
}

impl EffectCharacter {
    pub fn new(character_id: u32, symbol: &str, input_column: i64, input_row: i64) -> Self {
        let input_coord = Coord::new(input_column, input_row);
        EffectCharacter {
            character_id,
            input_symbol: symbol.to_string(),
            input_coord,
            input_ansi_fg_sequence: None,
            input_ansi_bg_sequence: None,
            is_visible: false,
            animation: Animation::new(symbol),
            motion: Motion::new(input_coord),
            event_handler: EventHandler::default(),
            layer: 0,
            is_fill_character: false,
            uses_input_preexisting_colors: false,
            links: Vec::new(),
            neighbors: Neighbors::default(),
        }
    }

    /// EffectCharacter.is_active: active while the animation's active scene is
    /// incomplete OR motion has an active path. Note looping scenes report
    /// complete, so loop-only characters read as inactive (faithful quirk).
    pub fn is_active(&self) -> bool {
        // Movement is a null check; scene completion is a map lookup. Same
        // answer either way, so ask the cheap question first.
        !self.motion.movement_is_complete() || !self.animation.active_scene_is_complete()
    }
}

/// Dense character storage with conservative mutation tracking. Indexed writes
/// mark one slot; mutable slice access marks all slots. This includes mutations
/// made by reentrant effect callbacks without requiring every effect to know
/// about the renderer. Slots are stable and append-only after preprocessing.
#[derive(Debug, Clone)]
pub struct CharacterArena {
    identity: super::revision::Revision,
    characters: Vec<EffectCharacter>,
    dirty: Vec<usize>,
    marked: Vec<bool>,
    layout_dirty: bool,
    pub(crate) playback: super::playback::PlaybackScheduler,
    scene_runtime: super::scene_runtime::SceneRuntime,
    motion_runtime: super::motion_runtime::MotionRuntime,
}

impl From<Vec<EffectCharacter>> for CharacterArena {
    fn from(characters: Vec<EffectCharacter>) -> Self {
        let len = characters.len();
        let mut playback = super::playback::PlaybackScheduler::default();
        playback.resize(len);
        let mut scene_runtime = super::scene_runtime::SceneRuntime::default();
        scene_runtime.resize(len);
        Self {
            identity: super::revision::Revision::default(),
            characters,
            dirty: (0..len).collect(),
            marked: vec![true; len],
            layout_dirty: true,
            playback,
            scene_runtime,
            motion_runtime: super::motion_runtime::MotionRuntime::default(),
        }
    }
}

impl CharacterArena {
    pub(crate) fn identity(&self) -> u64 {
        self.identity.0
    }
    pub fn len(&self) -> usize {
        self.characters.len()
    }
    pub fn is_empty(&self) -> bool {
        self.characters.is_empty()
    }

    /// The renderer only observes visuals/coordinates, never playback counters.
    pub(crate) fn render_slice(&self) -> &[EffectCharacter] {
        &self.characters
    }

    pub(crate) fn begin_update(&mut self, members: &[u64]) {
        self.playback.begin(members, &self.characters, &self.scene_runtime);
    }
    pub(crate) fn finish_update(&mut self, members: &[u64]) {
        self.playback.finish(members, &self.characters, &self.scene_runtime);
    }
    pub(crate) fn begin_sparse_update(&mut self, members: &super::active_characters::ActiveCharacters) {
        self.playback.begin_sparse(members, &self.characters, &self.scene_runtime);
    }
    pub(crate) fn finish_sparse_update(&mut self, members: &super::active_characters::ActiveCharacters) {
        self.playback.finish_sparse(members, &self.characters, &self.scene_runtime);
    }
    pub(crate) fn wake_all(&mut self) {
        self.playback.wake_all(&self.characters, &self.scene_runtime);
    }

    pub(crate) fn set_scene_runtime(&mut self, enabled: bool) {
        self.wake_all();
        for id in 0..self.characters.len() {
            self.scene_runtime.invalidate(id, &self.characters);
        }
        self.scene_runtime.enabled = enabled;
    }

    pub(crate) fn prepare_scene(&mut self, id: usize, allow_loop: bool) -> bool {
        if !self.playback.is_sleeping(id) {
            self.scene_runtime.prepare(id, &self.characters, allow_loop);
        }
        self.scene_runtime.contains(id)
    }

    #[inline]
    pub(crate) fn scene_tick(&mut self, id: usize, allow_idle: bool, motion_done: bool, allow_loop: bool) -> bool {
        let prepared =
            if motion_done { self.scene_runtime.contains(id) } else { self.scene_runtime.contains_stationary(id) };
        if !prepared {
            return false;
        }
        // Direct tick calls from callbacks must settle a pending held interval.
        self.playback.wake(id, &self.characters, &self.scene_runtime);
        let Some((changed, scene, idle)) =
            self.scene_runtime.tick(id, &mut self.characters, &self.motion_runtime, allow_loop)
        else {
            return false;
        };
        if changed {
            self.mark_visual(id);
        }
        if allow_idle && idle > 0 && self.playback.is_current(id) {
            self.playback.sleep(id, scene, idle, super::playback::PlaybackKind::Runtime);
        }
        true
    }

    #[inline]
    pub(crate) fn has_scene(&self, id: usize) -> bool {
        self.characters[id].animation.active_scene.is_some()
    }

    /// Restrict engine edits to animation fields. Path definitions and prepared
    /// motion stay valid; only a changed formatted visual dirties rendering.
    pub(crate) fn animation_edit(&mut self, id: usize) -> AnimationEdit<'_> {
        self.playback.wake(id, &self.characters, &self.scene_runtime);
        self.scene_runtime.invalidate(id, &self.characters);
        let animation = &mut self.characters[id].animation;
        AnimationEdit {
            original_visual: animation.current_character_visual.formatted_symbol.id(),
            animation,
            id,
            dirty: &mut self.dirty,
            marked: &mut self.marked,
        }
    }

    /// Change appearance without invalidating unrelated motion or cell layout.
    pub(crate) fn set_appearance(
        &mut self,
        id: usize,
        symbol: Option<&str>,
        colors: Option<crate::utils::graphics::ColorPair>,
    ) {
        self.playback.wake(id, &self.characters, &self.scene_runtime);
        self.scene_runtime.invalidate(id, &self.characters);
        let ch = &mut self.characters[id];
        let before = ch.animation.current_character_visual.formatted_symbol.id();
        ch.animation.set_appearance(&ch.input_symbol, ch.uses_input_preexisting_colors, symbol, colors);
        if ch.animation.current_character_visual.formatted_symbol.id() != before {
            self.mark_visual(id);
        }
    }

    pub(crate) fn set_appearance_with_palette(
        &mut self,
        id: usize,
        symbol: Option<&str>,
        colors: crate::utils::graphics::ColorPair,
        palette: &mut super::animation::AppearancePalette,
    ) {
        self.playback.wake(id, &self.characters, &self.scene_runtime);
        self.scene_runtime.invalidate(id, &self.characters);
        let ch = &mut self.characters[id];
        let before = ch.animation.current_character_visual.formatted_symbol.id();
        ch.animation.set_appearance_with_palette(
            &ch.input_symbol,
            ch.uses_input_preexisting_colors,
            symbol,
            &colors,
            palette,
        );
        if ch.animation.current_character_visual.formatted_symbol.id() != before {
            self.mark_visual(id);
        }
    }

    pub(crate) fn motion_progress(&self, id: usize) -> Option<(i64, i64, f64, f64)> {
        if let Some(progress) = self.motion_runtime.progress(id) {
            return Some(progress);
        }
        let motion = &self.characters[id].motion;
        let active = motion.active_path.as_ref()?;
        let p = motion.paths.get(active).expect("active path missing");
        Some((p.current_step(), p.max_steps, p.total_distance, p.last_distance_reached()))
    }

    #[inline]
    pub(crate) fn stationary(&self, id: usize) -> bool {
        let motion = &self.characters[id].motion;
        motion.active_path.is_none() && motion.current_coord == motion.previous_coord
    }

    /// Effect predicates that observe geometry/activity do not need deferred
    /// path or scene counters copied back into their public records.
    pub(crate) fn at_first_waypoint(&self, id: usize, path: &str) -> bool {
        let motion = &self.characters[id].motion;
        motion.current_coord == motion.paths.get(path).expect("path missing").waypoints[0].coord
    }

    pub(crate) fn animation_complete(&self, id: usize) -> bool {
        !self.scene_runtime.contains(id) && self.characters[id].animation.active_scene_is_complete()
    }

    pub(crate) fn set_motion_runtime(&mut self, enabled: bool) {
        self.motion_runtime.invalidate_all(&self.characters);
        self.motion_runtime.enabled = enabled;
    }

    #[inline]
    pub(crate) fn motion_tick(&mut self, id: usize) -> bool {
        let Some(coord) = self.motion_runtime.tick(id) else { return false };
        let motion = &mut self.characters[id].motion;
        motion.previous_coord = motion.current_coord;
        motion.current_coord = coord;
        if motion.previous_coord != coord && self.characters[id].is_visible {
            self.mark(id);
        }
        true
    }

    /// The caller has established that segment events are unobserved. Return a
    /// completed path slot for EngineCtx to dispatch holds/completion normally.
    pub(crate) fn motion_step(&mut self, id: usize) -> Option<usize> {
        self.playback.wake(id, &self.characters, &self.scene_runtime);
        self.motion_runtime.invalidate(id, &self.characters);
        let len = self.characters.len();
        let motion = &mut self.characters[id].motion;
        motion.previous_coord = motion.current_coord;
        let active = motion.active_path.as_ref()?;
        let slot = motion.paths.slot(active)?;
        let path = motion.paths.at_mut(slot);
        if path.segments.is_empty() {
            return None;
        }
        let (coord, segment) = path.step_without_events();
        motion.current_coord = coord;
        let completed = path.current_step() == path.max_steps;
        if !completed {
            if let Some(segment) = segment {
                self.motion_runtime.prepare(id, len, slot, path, segment);
            }
        }
        if motion.previous_coord != coord && self.characters[id].is_visible {
            self.mark(id);
        }
        completed.then_some(slot)
    }

    /// These engine observations do not read any deferred scene counters.
    #[inline]
    pub(crate) fn event_handler(&self, id: usize) -> &EventHandler {
        &self.characters[id].event_handler
    }

    /// Return whether movement is needed. Updating previous_coord alone cannot
    /// change rendering or invalidate a stationary scene's playback program.
    #[inline]
    pub(crate) fn settle_stationary_motion(&mut self, id: usize) -> bool {
        let motion = &mut self.characters[id].motion;
        if motion.active_path.is_some() {
            true
        } else {
            motion.previous_coord = motion.current_coord;
            false
        }
    }

    pub(crate) fn active_masks(&self) -> [&[u64]; 3] {
        [&self.playback.sleeping, self.scene_runtime.active_mask(), self.motion_runtime.active_mask()]
    }

    pub(crate) fn is_active(&self, id: usize) -> bool {
        // Held intervals never complete a scene or change active_path. Its
        // activity is therefore observable without materializing tick counters.
        self.scene_runtime.contains(id) || self.characters[id].is_active()
    }

    pub fn push(&mut self, character: EffectCharacter) {
        self.layout_dirty = true;
        self.dirty.push(self.characters.len());
        self.marked.push(true);
        self.characters.push(character);
        self.playback.resize(self.characters.len());
        self.scene_runtime.resize(self.characters.len());
    }

    #[inline]
    fn mark(&mut self, index: usize) {
        self.layout_dirty = true;
        self.mark_visual(index);
    }

    #[inline]
    fn mark_visual(&mut self, index: usize) {
        if !self.marked[index] {
            self.marked[index] = true;
            self.dirty.push(index);
        }
    }

    pub(crate) fn mark_all(&mut self) {
        self.layout_dirty = true;
        self.dirty.clear();
        self.dirty.extend(0..self.characters.len());
        self.marked.fill(true);
    }

    pub(crate) fn dirty_len(&self) -> usize {
        self.dirty.len()
    }

    pub(crate) fn layout_changed(&self) -> bool {
        self.layout_dirty
    }

    pub(crate) fn take_dirty(&mut self) -> Vec<usize> {
        self.layout_dirty = false;
        let dirty = std::mem::take(&mut self.dirty);
        for &index in &dirty {
            self.marked[index] = false;
        }
        dirty
    }

    pub(crate) fn recycle_dirty(&mut self, mut dirty: Vec<usize>) {
        dirty.clear();
        self.dirty = dirty;
    }
}

impl std::ops::Deref for CharacterArena {
    type Target = [EffectCharacter];
    fn deref(&self) -> &Self::Target {
        for id in 0..self.characters.len() {
            self.playback.materialize(id, &self.characters, &self.scene_runtime);
            self.scene_runtime.materialize(id, &self.characters);
            self.motion_runtime.materialize(id, &self.characters);
        }
        &self.characters
    }
}

impl std::ops::DerefMut for CharacterArena {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.wake_all();
        self.motion_runtime.invalidate_all(&self.characters);
        for id in 0..self.characters.len() {
            self.scene_runtime.invalidate(id, &self.characters);
        }
        self.mark_all();
        &mut self.characters
    }
}

impl std::ops::Index<usize> for CharacterArena {
    type Output = EffectCharacter;
    #[inline]
    fn index(&self, index: usize) -> &Self::Output {
        self.playback.materialize(index, &self.characters, &self.scene_runtime);
        self.scene_runtime.materialize(index, &self.characters);
        self.motion_runtime.materialize(index, &self.characters);
        &self.characters[index]
    }
}

impl std::ops::IndexMut<usize> for CharacterArena {
    #[inline]
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        self.playback.wake(index, &self.characters, &self.scene_runtime);
        self.scene_runtime.invalidate(index, &self.characters);
        self.motion_runtime.invalidate(index, &self.characters);
        self.mark(index);
        &mut self.characters[index]
    }
}

/// A private field-restricted edit guard. Dropping it commits only rendering
/// changes; mutable character access remains conservative for public callers.
pub(crate) struct AnimationEdit<'a> {
    animation: &'a mut Animation,
    original_visual: u64,
    id: usize,
    dirty: &'a mut Vec<usize>,
    marked: &'a mut [bool],
}

impl std::ops::Deref for AnimationEdit<'_> {
    type Target = Animation;
    fn deref(&self) -> &Animation {
        self.animation
    }
}

impl std::ops::DerefMut for AnimationEdit<'_> {
    fn deref_mut(&mut self) -> &mut Animation {
        self.animation
    }
}

impl Drop for AnimationEdit<'_> {
    fn drop(&mut self) {
        if self.original_visual != self.animation.current_character_visual.formatted_symbol.id()
            && !self.marked[self.id]
        {
            self.marked[self.id] = true;
            self.dirty.push(self.id);
        }
    }
}
