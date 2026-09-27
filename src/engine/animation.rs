//! CharacterVisual, Frame, Scene, and Animation, ported from engine/animation.py.
//! Scene/Animation stepping that fires events lives on EngineCtx (ctx.rs);
//! everything here is state plus event-free logic.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::rc::{Rc, Weak};

use crate::utils::ansi::{self, ColorCode};
use crate::utils::easing::Easing;
use crate::utils::graphics::{Color, ColorPair, Gradient};
use crate::utils::hexterm;
use crate::utils::ordered_map::OrderedMap;
use crate::utils::pycompat::round_half_even;

/// Easing depends on the curve and elapsed step, not the character or colors.
/// Share the exact rounded tick indices; keep unusually long scenes on the
/// scalar path so a large duration cannot cause an equally large allocation.
#[derive(Debug)]
struct EasedTicks {
    ease: Easing,
    total: i64,
    uses: u8,
    ticks: Box<[u32]>,
}

impl EasedTicks {
    const MAX_STEPS: i64 = 65_536;

    fn tick(ease: Easing, step: i64, total: i64) -> i64 {
        let ratio = step as f64 / total as f64;
        let factor = ease.ease(ratio);
        let last = (total - 1).max(0);
        round_half_even(factor * last as f64).min(last).max(0)
    }
}

/// Numeric easing plans belong to the engine, keeping ordinary Scene records
/// compact and releasing the plans with the animation. At most 4 MiB of tick
/// indices are retained; very long scenes use scalar evaluation.
#[derive(Debug, Default)]
pub(crate) struct EasedTicksCache {
    shapes: Vec<EasedTicks>,
    last: usize,
    next: usize,
}

impl EasedTicksCache {
    fn get(&mut self, ease: Easing, total: i64) -> Option<&EasedTicks> {
        if !(1..=EasedTicks::MAX_STEPS).contains(&total) {
            return None;
        }
        let matches = |entry: &EasedTicks| entry.ease == ease && entry.total == total;
        let slot = if self.shapes.get(self.last).is_some_and(matches) {
            self.last
        } else if let Some(slot) = self.shapes.iter().position(matches) {
            slot
        } else {
            let shape = EasedTicks { ease, total, uses: 0, ticks: Box::new([]) };
            if self.shapes.len() < 16 {
                self.shapes.push(shape);
                self.shapes.len() - 1
            } else {
                let slot = self.next;
                self.shapes[slot] = shape;
                self.next = (slot + 1) % 16;
                slot
            }
        };
        self.last = slot;
        let shape = &mut self.shapes[slot];
        if shape.ticks.is_empty() {
            // Establish reuse before evaluating a whole curve. With many
            // distinct scenes, eviction should cost a scalar tick rather than
            // rebuilding thousands of unused future values on each miss.
            shape.uses += 1;
            if shape.uses < 8 {
                return None;
            }
            shape.ticks = (0..total).map(|step| EasedTicks::tick(ease, step, total) as u32).collect();
        }
        Some(shape)
    }
}

/// Handling of preexisting SGR colors in the input (TerminalConfig option).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExistingColorHandling {
    Always,
    Dynamic,
    Ignore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncMetric {
    Distance,
    Step,
}

#[cfg(test)]
mod frame_index_tests {
    use super::*;

    #[test]
    fn eased_cache_distinguishes_curves_and_durations_after_eviction() {
        let mut cache = EasedTicksCache::default();
        for total in (1..=24).chain((1..=24).rev()) {
            for ease in [Easing::Linear, Easing::OutElastic, Easing::CubicBezier(0.2, -0.8, 0.7, 1.5)] {
                for _ in 0..7 {
                    cache.get(ease, total);
                }
                let cached = cache.get(ease, total).unwrap();
                let expected: Vec<_> = (0..total).map(|step| EasedTicks::tick(ease, step, total) as u32).collect();
                assert_eq!(&*cached.ticks, expected);
                assert!(cache.shapes.len() <= 16);
            }
        }
        assert!(cache.get(Easing::Linear, EasedTicks::MAX_STEPS + 1).is_none());
        assert!(cache.get(Easing::Linear, 1_000_000_000).is_none());
    }

    #[test]
    fn diverse_eased_scenes_do_not_rebuild_large_tables_on_every_tick() {
        let mut cache = EasedTicksCache::default();
        for _ in 0..32 {
            for total in 60_000..60_032 {
                assert!(cache.get(Easing::InOutSine, total).is_none());
            }
        }
        assert!(cache.shapes.iter().all(|shape| shape.ticks.is_empty()));
    }

    #[test]
    fn duration_lookup_matches_expanded_ticks_after_preparation_and_appends() {
        let mut scene = Scene::new("lookup", false, None, Some(Easing::Linear), false, false);
        let mut expanded = Vec::new();
        for (frame, duration) in [3, 3, 3, 1, 5, 2].into_iter().enumerate() {
            scene.add_frame("X", duration, VisualParams::default()).unwrap();
            expanded.extend(std::iter::repeat_n(frame, duration as usize));
            scene.activate().unwrap();
            for (tick, &expected) in expanded.iter().enumerate() {
                assert_eq!(scene.frame_at_tick(tick as i64), expected, "frame {frame}, tick {tick}");
            }
            scene.get_next_visual();
            scene.reset_scene();
        }
    }
}

#[inline]
fn resolve_color_code(
    color: Option<&Color>,
    no_color: bool,
    use_xterm_colors: bool,
    reusable: Option<ColorCode>,
) -> Option<ColorCode> {
    let color = color?;
    if no_color {
        return None;
    }
    if use_xterm_colors {
        return Some(ColorCode::Xterm(color.xterm_color.unwrap_or_else(|| hexterm::hex_to_xterm(&color.rgb_color))));
    }
    let hex = match reusable {
        Some(ColorCode::Rgb(mut hex)) => {
            color.rgb_color.as_ref().clone_into(&mut hex);
            hex
        }
        _ => color.rgb_color.as_ref().to_owned(),
    };
    Some(ColorCode::Rgb(hex))
}

/// Compare derived codes without allocating their RGB string representation.
fn color_code_matches(code: Option<&ColorCode>, color: Option<&Color>, no_color: bool, xterm: bool) -> bool {
    let Some(color) = color.filter(|_| !no_color) else {
        return code.is_none();
    };
    if xterm {
        matches!(code, Some(ColorCode::Xterm(value))
            if *value == color.xterm_color.unwrap_or_else(|| hexterm::hex_to_xterm(&color.rgb_color)))
    } else {
        matches!(code, Some(ColorCode::Rgb(value)) if value == color.rgb_color.as_ref())
    }
}

thread_local! {
    /// Reused assembly buffer for CharacterVisual::new's SGR string.
    static FORMAT_SCRATCH: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
    /// Live visuals, one per distinct symbol and styling, shared by every
    /// frame that shows them (beams: 227,002 frames, 378 visuals). Weak
    /// references, swept as the table grows, so effects that keep making
    /// new colors do not accumulate dead ones.
    static SHARED_VISUALS: std::cell::RefCell<SharedVisuals> = std::cell::RefCell::new(SharedVisuals {
        table: HashMap::new(),
        sweep_at: SharedVisuals::MIN_SWEEP,
    });
}

struct SharedVisuals {
    table: HashMap<VisualKey, std::rc::Weak<CharacterVisual>>,
    /// Size at which dead entries are dropped; doubles after each sweep.
    sweep_at: usize,
}

impl SharedVisuals {
    const MIN_SWEEP: usize = 1024;

    fn get(&self, symbol: &str, params: &VisualParams) -> Option<Rc<CharacterVisual>> {
        self.table.get(&(symbol, params) as &dyn VisualLookup)?.upgrade()
    }

    fn insert(&mut self, key: VisualKey, visual: &Rc<CharacterVisual>) {
        self.table.insert(key, Rc::downgrade(visual));
        if self.table.len() >= self.sweep_at {
            self.table.retain(|_, weak| weak.strong_count() > 0);
            self.sweep_at = (self.table.len() * 2).max(SharedVisuals::MIN_SWEEP);
        }
    }
}

/// Everything that determines a CharacterVisual, owned, for the share table.
struct VisualKey {
    symbol: Box<str>,
    params: VisualParams,
}

/// The share table is keyed by `dyn VisualLookup`, which the owned key and a
/// borrowed `(&str, &VisualParams)` pair both implement with the same Hash
/// and Eq, so a lookup allocates nothing. Equality is field by field with
/// colors compared by their ColorArg, the equality upstream gives visuals.
trait VisualLookup {
    fn symbol(&self) -> &str;
    fn params(&self) -> &VisualParams;
}

impl VisualLookup for VisualKey {
    fn symbol(&self) -> &str {
        &self.symbol
    }
    fn params(&self) -> &VisualParams {
        &self.params
    }
}

impl VisualLookup for (&str, &VisualParams) {
    fn symbol(&self) -> &str {
        self.0
    }
    fn params(&self) -> &VisualParams {
        self.1
    }
}

impl<'a> std::borrow::Borrow<dyn VisualLookup + 'a> for VisualKey {
    fn borrow(&self) -> &(dyn VisualLookup + 'a) {
        self
    }
}

impl std::hash::Hash for dyn VisualLookup + '_ {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.symbol().hash(state);
        let p = self.params();
        [p.bold, p.dim, p.italic, p.underline, p.blink, p.reverse, p.hidden, p.strike].hash(state);
        p.colors.is_some().hash(state);
        p.colors.as_ref().and_then(|c| c.fg_color).hash(state);
        p.colors.as_ref().and_then(|c| c.bg_color).hash(state);
        p.fg_color_code.hash(state);
        p.bg_color_code.hash(state);
    }
}

impl PartialEq for dyn VisualLookup + '_ {
    fn eq(&self, other: &Self) -> bool {
        let (a, b) = (self.params(), other.params());
        self.symbol() == other.symbol()
            && [a.bold, a.dim, a.italic, a.underline, a.blink, a.reverse, a.hidden, a.strike]
                == [b.bold, b.dim, b.italic, b.underline, b.blink, b.reverse, b.hidden, b.strike]
            && a.colors.is_some() == b.colors.is_some()
            && a.colors.as_ref().and_then(|c| c.fg_color) == b.colors.as_ref().and_then(|c| c.fg_color)
            && a.colors.as_ref().and_then(|c| c.bg_color) == b.colors.as_ref().and_then(|c| c.bg_color)
            && a.fg_color_code == b.fg_color_code
            && a.bg_color_code == b.bg_color_code
    }
}

impl Eq for dyn VisualLookup + '_ {}

impl std::hash::Hash for VisualKey {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        (self as &dyn VisualLookup).hash(state)
    }
}

impl PartialEq for VisualKey {
    fn eq(&self, other: &Self) -> bool {
        (self as &dyn VisualLookup) == (other as &dyn VisualLookup)
    }
}

impl Eq for VisualKey {}

/// Compact value key. Scene color codes are derived, so retaining their String
/// fields in every cache slot would waste space and allocate on hits.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct SceneVisualKey {
    symbol: char,
    styles: u16,
    colors: Option<ColorPair>,
}

thread_local! {
    static SCENE_VISUALS: std::cell::RefCell<std::collections::HashMap<
        SceneVisualKey, Weak<CharacterVisual>, std::hash::BuildHasherDefault<rustc_hash::FxHasher>
    >> = std::cell::RefCell::new(std::collections::HashMap::default());
}

fn scene_visual(symbol: &str, mut params: VisualParams, no_color: bool, xterm: bool) -> Rc<CharacterVisual> {
    params.fg_color_code = None;
    params.bg_color_code = None;
    let mut chars = symbol.chars();
    let single = chars.next().filter(|_| chars.next().is_none());
    let build = |mut params: VisualParams| {
        if let Some(colors) = &params.colors {
            params.fg_color_code = resolve_color_code(colors.fg_color.as_ref(), no_color, xterm, None);
            params.bg_color_code = resolve_color_code(colors.bg_color.as_ref(), no_color, xterm, None);
        }
        Rc::new(CharacterVisual::new(symbol, params))
    };
    let Some(symbol) = single else {
        return build(params);
    };
    let styles = params.bold as u16
        | (params.dim as u16) << 1
        | (params.italic as u16) << 2
        | (params.underline as u16) << 3
        | (params.blink as u16) << 4
        | (params.reverse as u16) << 5
        | (params.hidden as u16) << 6
        | (params.strike as u16) << 7
        | (no_color as u16) << 8
        | (xterm as u16) << 9;
    let key = SceneVisualKey { symbol, styles, colors: params.colors };
    SCENE_VISUALS.with(|cache| {
        let mut cache = cache.borrow_mut();
        if let Some(visual) = cache.get(&key).and_then(Weak::upgrade) {
            return visual;
        }
        // Bound long-lived embedded use. Eviction affects reuse only, never output.
        if cache.len() >= 16384 {
            cache.clear();
        }
        let visual = build(params);
        cache.insert(key, Rc::downgrade(&visual));
        visual
    })
}

/// Appearance updates have no frame program retaining earlier visuals. Keep a
/// bounded set of immutable values alive so repeated row colors and lighting
/// values do not rebuild ANSI strings. Direct mapping makes both lookup and
/// eviction constant-time; a collision only loses reuse.
pub(crate) struct AppearancePalette {
    slots: Vec<Option<(SceneVisualKey, Rc<CharacterVisual>)>>,
    enabled: bool,
}

impl AppearancePalette {
    const CAPACITY: usize = 4096;

    pub(crate) fn new() -> Self {
        Self { slots: Vec::new(), enabled: std::env::var_os("TTFX_APPEARANCE_CACHE").is_none_or(|value| value != "0") }
    }

    fn visual(
        &mut self,
        symbol: &str,
        colors: &ColorPair,
        bold: bool,
        no_color: bool,
        xterm: bool,
    ) -> Option<Rc<CharacterVisual>> {
        if !self.enabled {
            return None;
        }
        let mut chars = symbol.chars();
        let single = chars.next()?;
        if chars.next().is_some() {
            return None;
        }
        let styles = bold as u16 | (no_color as u16) << 8 | (xterm as u16) << 9;
        use std::hash::{Hash, Hasher};
        let mut hash = rustc_hash::FxHasher::default();
        (single, styles, Some(colors)).hash(&mut hash);
        let slot = hash.finish() as usize & (Self::CAPACITY - 1);
        if self.slots.is_empty() {
            self.slots.resize_with(Self::CAPACITY, || None);
        }
        if let Some((previous, visual)) = &self.slots[slot] {
            // Color equality intentionally compares only its constructor
            // argument. RGB/xterm fields remain publicly mutable, and formatting
            // reads those fields, so a palette hit must compare them as well.
            let same_codes = |left: Option<&Color>, right: Option<&Color>| match (left, right) {
                (Some(left), Some(right)) => left.rgb_color == right.rgb_color && left.xterm_color == right.xterm_color,
                (None, None) => true,
                _ => false,
            };
            let old_colors = previous.colors.as_ref().unwrap();
            if previous.symbol == single
                && previous.styles == styles
                && old_colors == colors
                && same_codes(old_colors.fg_color.as_ref(), colors.fg_color.as_ref())
                && same_codes(old_colors.bg_color.as_ref(), colors.bg_color.as_ref())
            {
                return Some(Rc::clone(visual));
            }
        }
        let visual = Rc::new(CharacterVisual::new(
            symbol,
            VisualParams {
                bold,
                colors: Some(*colors),
                fg_color_code: resolve_color_code(colors.fg_color.as_ref(), no_color, xterm, None),
                bg_color_code: resolve_color_code(colors.bg_color.as_ref(), no_color, xterm, None),
                ..Default::default()
            },
        ));
        let key = SceneVisualKey { symbol: single, styles, colors: Some(*colors) };
        self.slots[slot] = Some((key, Rc::clone(&visual)));
        Some(visual)
    }
}

/// Inline capacity for a formatted symbol. A 24-bit foreground and background
/// pair plus a reset is 42 bytes, so all but pathological styling fits.
const INLINE_SYMBOL_CAPACITY: usize = 63;

/// The precomputed ANSI string for one cell, stored inline when it fits.
///
/// The frame writer emits one of these per visible cell — millions of times
/// over a run — and a `str` copy of a couple of dozen bytes is dominated by the
/// memcpy call itself. An inline buffer lets the writer copy a fixed block and
/// then advance by the real length. The common foreground-only
/// case fits in 32 bytes; heavily styled symbols use the full inline buffer.
#[derive(Debug, Clone)]
pub struct FormattedSymbol {
    // Immutable identity follows clones and changes whenever bytes are rebuilt.
    // Zero is reserved for empty render cells.
    id: u64,
    data: SymbolBytes,
}

#[derive(Debug, Clone)]
enum SymbolBytes {
    Inline { bytes: [u8; INLINE_SYMBOL_CAPACITY], len: u8 },
    Heap(Box<str>),
}

impl FormattedSymbol {
    fn new(text: &str) -> Self {
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let id = NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        // Do not permit wraparound to alias a previously cached symbol.
        if id == u64::MAX {
            std::process::abort();
        }
        let data = if text.len() <= INLINE_SYMBOL_CAPACITY {
            let mut bytes = [0u8; INLINE_SYMBOL_CAPACITY];
            bytes[..text.len()].copy_from_slice(text.as_bytes());
            SymbolBytes::Inline { bytes, len: text.len() as u8 }
        } else {
            SymbolBytes::Heap(text.into())
        };
        Self { id, data }
    }

    #[inline]
    pub(crate) fn id(&self) -> u64 {
        self.id
    }

    #[inline]
    pub fn as_str(&self) -> &str {
        match &self.data {
            SymbolBytes::Inline { bytes, len } => {
                // SAFETY: built from a &str prefix, so the range is valid UTF-8.
                unsafe { std::str::from_utf8_unchecked(&bytes[..*len as usize]) }
            }
            SymbolBytes::Heap(text) => text,
        }
    }

    /// Append a fixed-size block, then discard its unused padding.
    #[inline]
    pub fn append_to(&self, out: &mut Vec<u8>) {
        match &self.data {
            SymbolBytes::Inline { bytes, len } => {
                let start = out.len();
                if *len <= 32 {
                    out.extend_from_slice(&bytes[..32]);
                } else {
                    out.extend_from_slice(bytes);
                }
                out.truncate(start + *len as usize);
            }
            SymbolBytes::Heap(text) => out.extend_from_slice(text.as_bytes()),
        }
    }
}

impl PartialEq for FormattedSymbol {
    fn eq(&self, other: &Self) -> bool {
        self.as_str() == other.as_str()
    }
}

/// animation.CharacterVisual with the formatted ANSI string precomputed.
#[derive(Debug, Clone, PartialEq)]
pub struct CharacterVisual {
    pub symbol: String,
    pub bold: bool,
    pub dim: bool, // stored but never emitted, faithfully
    pub italic: bool,
    pub underline: bool,
    pub blink: bool,
    pub reverse: bool,
    pub hidden: bool,
    pub strike: bool,
    pub colors: Option<ColorPair>,
    pub fg_color_code: Option<ColorCode>,
    pub bg_color_code: Option<ColorCode>,
    pub formatted_symbol: FormattedSymbol,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct VisualParams {
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub blink: bool,
    pub reverse: bool,
    pub hidden: bool,
    pub strike: bool,
    pub colors: Option<ColorPair>,
    pub fg_color_code: Option<ColorCode>,
    pub bg_color_code: Option<ColorCode>,
}

impl CharacterVisual {
    pub fn new(symbol: &str, p: VisualParams) -> Self {
        Self::with_symbol(symbol.to_owned(), p)
    }

    fn with_symbol(symbol: String, p: VisualParams) -> Self {
        let mut vis = CharacterVisual {
            symbol,
            bold: p.bold,
            dim: p.dim,
            italic: p.italic,
            underline: p.underline,
            blink: p.blink,
            reverse: p.reverse,
            hidden: p.hidden,
            strike: p.strike,
            colors: p.colors,
            fg_color_code: p.fg_color_code,
            bg_color_code: p.bg_color_code,
            formatted_symbol: FormattedSymbol {
                id: 0,
                data: SymbolBytes::Inline { bytes: [0; INLINE_SYMBOL_CAPACITY], len: 0 },
            },
        };
        // Effects rebuild visuals every frame, so the SGR string is assembled in
        // a reused scratch buffer rather than a fresh allocation per visual.
        FORMAT_SCRATCH.with(|scratch| {
            let mut scratch = scratch.borrow_mut();
            scratch.clear();
            vis.format_symbol_into(&mut scratch);
            vis.formatted_symbol = FormattedSymbol::new(&scratch);
        });
        vis
    }

    pub fn plain(symbol: &str) -> Self {
        CharacterVisual::new(symbol, VisualParams::default())
    }

    /// The one shared visual for this symbol and styling, built on first use.
    /// Visuals are immutable once built and compared by content everywhere,
    /// so sharing is not observable; it just stops every frame of every
    /// character owning its own copy.
    pub fn shared(symbol: &str, params: VisualParams) -> Rc<CharacterVisual> {
        SHARED_VISUALS.with(|shared| {
            if let Some(visual) = shared.borrow().get(symbol, &params) {
                return visual;
            }
            let visual = Rc::new(CharacterVisual::new(symbol, params.clone()));
            shared.borrow_mut().insert(VisualKey { symbol: symbol.into(), params }, &visual);
            visual
        })
    }

    /// SGR emission in upstream's fixed order; `dim` intentionally omitted;
    /// bare symbol when nothing applies.
    fn format_symbol_into(&self, fmt: &mut String) {
        if self.bold {
            fmt.push_str(ansi::BOLD);
        }
        if self.italic {
            fmt.push_str(ansi::ITALIC);
        }
        if self.underline {
            fmt.push_str(ansi::UNDERLINE);
        }
        if self.blink {
            fmt.push_str(ansi::BLINK);
        }
        if self.reverse {
            fmt.push_str(ansi::REVERSE);
        }
        if self.hidden {
            fmt.push_str(ansi::HIDDEN);
        }
        if self.strike {
            fmt.push_str(ansi::STRIKETHROUGH);
        }
        if let Some(code) = &self.fg_color_code {
            ansi::fg(code, fmt);
        }
        if let Some(code) = &self.bg_color_code {
            ansi::bg(code, fmt);
        }
        fmt.push_str(&self.symbol);
        if fmt.len() != self.symbol.len() {
            fmt.push_str(ansi::RESET_ALL);
        }
    }
}

/// animation.Frame. Frames live in Scene.all_frames (stable storage);
/// Scene.frames / Scene.played_frames hold indices into it, preserving the
/// upstream object-identity semantics of frame_index_map.
#[derive(Debug, Clone)]
pub struct Frame {
    pub character_visual: Rc<CharacterVisual>,
    pub duration: i64,
}

/// Mutable construction and compact shared playback use different layouts.
/// Rc<[Frame]> points directly at the frames; Rc<Vec<Frame>> adds a dependent
/// pointer and length load to every animation tick.
#[derive(Debug, Clone)]
pub enum FrameStorage {
    Building(Vec<Frame>),
    Shared(Rc<[Frame]>),
}

impl std::ops::Deref for FrameStorage {
    type Target = [Frame];
    #[inline]
    fn deref(&self) -> &[Frame] {
        match self {
            Self::Building(frames) => frames,
            Self::Shared(frames) => frames,
        }
    }
}

impl FrameStorage {
    pub fn make_mut(&mut self) -> &mut Vec<Frame> {
        if let Self::Shared(frames) = self {
            *self = Self::Building(frames.to_vec());
        }
        match self {
            Self::Building(frames) => frames,
            Self::Shared(_) => unreachable!(),
        }
    }

    fn share(&mut self) -> &Rc<[Frame]> {
        if let Self::Building(frames) = self {
            *self = Self::Shared(std::mem::take(frames).into());
        }
        match self {
            Self::Shared(frames) => frames,
            Self::Building(_) => unreachable!(),
        }
    }
}

/// animation.Scene.
#[derive(Debug, Clone)]
pub struct Scene {
    pub scene_id: String,
    pub is_looping: bool,
    pub sync: Option<SyncMetric>,
    pub ease: Option<Easing>,
    pub no_color: bool,
    pub use_xterm_colors: bool,
    /// Stable frame storage; never reordered.
    pub all_frames: FrameStorage,
    ticks_elapsed: std::cell::Cell<i64>,
    prepared: bool,
    /// Remaining frame queue (indices into all_frames).
    head: std::cell::Cell<usize>,
    end: usize,
    /// Played frames (indices into all_frames).
    played: std::cell::Cell<usize>,
    /// Exclusive cumulative ends for nonuniform durations. Uniform scenes use
    /// division and never allocate this index.
    frame_end_ticks: Vec<i64>,
    uniform_duration: Option<i64>,
    pub easing_total_steps: i64,
    easing_current_step: std::cell::Cell<i64>,
    pub preexisting_colors: Option<ColorPair>,
    pub preexisting_bold: bool,
}

impl Scene {
    pub fn new(
        scene_id: &str,
        is_looping: bool,
        sync: Option<SyncMetric>,
        ease: Option<Easing>,
        no_color: bool,
        use_xterm_colors: bool,
    ) -> Self {
        Scene {
            scene_id: scene_id.to_string(),
            is_looping,
            sync,
            ease,
            no_color,
            use_xterm_colors,
            all_frames: FrameStorage::Building(Vec::new()),
            ticks_elapsed: std::cell::Cell::new(0),
            prepared: false,
            head: std::cell::Cell::new(0),
            end: 0,
            played: std::cell::Cell::new(0),
            frame_end_ticks: Vec::new(),
            uniform_duration: None,
            easing_total_steps: 0,
            easing_current_step: std::cell::Cell::new(0),
            preexisting_colors: None,
            preexisting_bold: false,
        }
    }

    /// Scene.add_frame with the preexisting-color/bold overrides.
    pub fn add_frame(&mut self, symbol: &str, duration: i64, mut params: VisualParams) -> Result<(), String> {
        if let Some(pre) = &self.preexisting_colors {
            params.colors = Some(pre.clone());
        }
        if self.preexisting_bold {
            params.bold = true;
        }
        if duration < 1 {
            return Err(format!("Frame duration must be at least 1. Received: {duration}"));
        }
        let visual = scene_visual(symbol, params, self.no_color, self.use_xterm_colors);
        let frame_index = self.all_frames.len();
        self.all_frames.make_mut().push(Frame { character_visual: visual, duration });
        self.prepared = false;
        self.end = frame_index + 1;
        if frame_index == 0 {
            self.uniform_duration = Some(duration);
        } else if let Some(previous) = self.uniform_duration {
            if previous != duration {
                // A later append can turn a uniform scene into a variable one.
                self.frame_end_ticks.extend((1..=frame_index).map(|index| index as i64 * previous));
                self.uniform_duration = None;
            }
        }
        self.easing_total_steps += duration;
        if self.uniform_duration.is_none() {
            self.frame_end_ticks.push(self.easing_total_steps);
        }
        Ok(())
    }

    /// Scene.activate: first frame's visual, error when empty.
    pub fn activate(&mut self) -> Result<Rc<CharacterVisual>, String> {
        self.prepare();
        if self.frames().is_empty() {
            Err(format!("Scene {} has no frames.", self.scene_id))
        } else {
            Ok(self.all_frames[self.head.get()].character_visual.clone())
        }
    }

    /// Scene.get_next_visual: tick the head frame, retiring it (and looping)
    /// exactly as upstream.
    pub fn get_next_visual(&mut self) -> Rc<CharacterVisual> {
        let head = self.step_frame();
        self.all_frames[head].character_visual.clone()
    }

    pub fn frames(&self) -> std::ops::Range<usize> {
        self.head.get()..self.end
    }
    pub fn played_frames(&self) -> std::ops::Range<usize> {
        0..self.played.get()
    }
    pub fn ticks_elapsed(&self) -> i64 {
        self.ticks_elapsed.get()
    }

    pub(crate) fn uniform_frame_duration(&self) -> Option<i64> {
        self.uniform_duration
    }

    pub(crate) fn eased_runtime_frame(&self, ease: Easing, step: i64) -> usize {
        self.frame_at_tick(EasedTicks::tick(ease, step, self.easing_total_steps))
    }

    pub(crate) fn set_eased_cursor(&self, step: i64) {
        self.easing_current_step.set(step);
    }

    /// Materialize the arena's compact playback cursor before API observation.
    pub(crate) fn set_plain_cursor(&self, head: usize, played: usize, ticks: i64) {
        self.head.set(head);
        self.played.set(played);
        self.ticks_elapsed.set(ticks);
    }

    pub fn easing_current_step(&self) -> i64 {
        self.easing_current_step.get()
    }

    pub fn set_easing_current_step(&mut self, step: i64) {
        self.easing_current_step.set(step);
    }

    pub(crate) fn advance_eased_ticks(&self, count: i64) {
        let next = self.easing_current_step.get() + count;
        debug_assert!(next < self.easing_total_steps);
        self.easing_current_step.set(next);
    }

    /// Returns the visual's frame index and following ticks that hold that same
    /// visual without completing. Curves can reverse or overshoot, so inspect
    /// the exact cached indices rather than assuming monotonic progression.
    pub(crate) fn step_eased(&mut self, ease: Easing, cache: &mut EasedTicksCache, allow_idle: bool) -> (usize, i64) {
        let total = self.easing_total_steps;
        let cached = cache.get(ease, total);
        let step = self.easing_current_step.get();
        let tick = cached
            .and_then(|cached| cached.ticks.get(step as usize))
            .map_or_else(|| EasedTicks::tick(ease, step, total), |&tick| i64::from(tick));
        let frame = self.frame_at_tick(tick);
        let next = step + 1;
        self.easing_current_step.set(next);
        if next == total {
            if self.is_looping {
                self.easing_current_step.set(0);
            } else {
                self.finish_frames();
            }
            return (frame, 0);
        }
        let mut idle = 0;
        if allow_idle && !self.is_looping && (0..total).contains(&next) {
            if let Some(cached) = cached {
                let visual = &self.all_frames[frame].character_visual;
                // The completion tick must take the ordinary event path. The
                // scheduler wheel can hold at most 255 ticks in one interval.
                for future in next..(total - 1).min(next + 255) {
                    let upcoming = self.frame_at_tick(i64::from(cached.ticks[future as usize]));
                    if !Rc::ptr_eq(visual, &self.all_frames[upcoming].character_visual) {
                        break;
                    }
                    idle += 1;
                }
            }
        }
        (frame, idle)
    }

    /// A scheduler interval can include one intermediate retirement, but never
    /// the final frame or a change to the displayed visual.
    pub(crate) fn advance_held_ticks(&self, count: i64) {
        let ticks = self.ticks_elapsed.get() + count;
        let head = self.head.get();
        let duration = self.all_frames[head].duration;
        debug_assert!(ticks <= duration);
        if ticks == duration {
            debug_assert!(head + 1 < self.end);
            self.ticks_elapsed.set(0);
            self.head.set(head + 1);
            self.played.set(head + 1);
        } else {
            self.ticks_elapsed.set(ticks);
        }
    }

    pub(crate) fn step_frame(&mut self) -> usize {
        let head = self.head.get();
        self.ticks_elapsed.set(self.ticks_elapsed.get() + 1);
        if self.ticks_elapsed.get() == self.all_frames[head].duration {
            self.ticks_elapsed.set(0);
            self.head.set(head + 1);
            self.played.set(head + 1);
            if self.is_looping && self.frames().is_empty() {
                self.head.set(0);
                self.played.set(0);
            }
        }
        head
    }

    /// Scene.apply_gradient_to_symbols with the exact cyclic_distribution
    /// generator semantics (repeat factor + overflow-remainder rule).
    pub fn apply_gradient_to_symbols(
        &mut self,
        symbols: &[String],
        duration: i64,
        fg_gradient: Option<&Gradient>,
        bg_gradient: Option<&Gradient>,
    ) -> Result<(), String> {
        fn cyclic_distribution<'a, T, R>(larger: &'a [T], smaller: &'a [R]) -> impl Iterator<Item = (&'a T, &'a R)> {
            let repeat_factor = larger.len() / smaller.len();
            let mut overflow_count = larger.len() % smaller.len();
            let mut overflow_used = false;
            let mut smaller_index = 0usize;
            let mut current_repeat_factor = 0usize;
            larger.iter().map(move |element| {
                if current_repeat_factor >= repeat_factor {
                    if overflow_count > 0 {
                        if overflow_used {
                            smaller_index += 1;
                            current_repeat_factor = 0;
                            overflow_used = false;
                        } else {
                            overflow_used = true;
                            overflow_count -= 1;
                        }
                    } else {
                        smaller_index += 1;
                        current_repeat_factor = 0;
                    }
                }
                current_repeat_factor += 1;
                (element, &smaller[smaller_index])
            })
        }

        let fg_has = fg_gradient.is_some_and(|g| !g.spectrum.is_empty());
        let bg_has = bg_gradient.is_some_and(|g| !g.spectrum.is_empty());
        if fg_gradient.is_none() && bg_gradient.is_none() {
            return Err("Foreground and background gradient are None. At least one gradient must be provided.".into());
        }
        if !fg_has && !bg_has {
            return Err(
                "Foreground and background gradient are empty. At least one gradient must have at least one color."
                    .into(),
            );
        }
        for symbol in symbols {
            if symbol.chars().count() > 1 {
                return Err(format!("Symbol must be a string with a length of 1. Received: `{symbol}`."));
            }
        }
        let color_pairs: Vec<ColorPair> = if fg_has && bg_has {
            let fg = &fg_gradient.unwrap().spectrum;
            let bg = &bg_gradient.unwrap().spectrum;
            if fg.len() >= bg.len() {
                cyclic_distribution(fg, bg).map(|(f, b)| ColorPair::new(Some(*f), Some(*b))).collect()
            } else {
                cyclic_distribution(bg, fg).map(|(b, f)| ColorPair::new(Some(*f), Some(*b))).collect()
            }
        } else if fg_has {
            fg_gradient.unwrap().spectrum.iter().map(|c| ColorPair::new(Some(c.clone()), None)).collect()
        } else {
            bg_gradient.unwrap().spectrum.iter().map(|c| ColorPair::new(None, Some(c.clone()))).collect()
        };

        // Every frame of the scene is known up front; size the stores once
        // instead of letting them double their way up.
        let frame_count = symbols.len().max(color_pairs.len());
        self.all_frames.make_mut().reserve_exact(frame_count);

        if symbols.len() >= color_pairs.len() {
            for (symbol, colors) in cyclic_distribution(symbols, &color_pairs) {
                self.add_frame(symbol, duration, VisualParams { colors: Some(*colors), ..Default::default() })?;
            }
        } else {
            for (colors, symbol) in cyclic_distribution(&color_pairs, symbols) {
                self.add_frame(symbol, duration, VisualParams { colors: Some(*colors), ..Default::default() })?;
            }
        }
        Ok(())
    }

    /// Scene.reset_scene: restore played + remaining frames in original order
    /// (played first), zero tick counters and the easing step.
    pub fn reset_scene(&mut self) {
        // Only the current head can have nonzero ticks; retired and unplayed
        // frames are zero. Reset is therefore O(1), including looping scenes.
        self.ticks_elapsed.set(0);
        self.head.set(0);
        self.end = self.all_frames.len();
        self.played.set(0);
        self.easing_current_step.set(0);
    }

    pub(crate) fn prepare(&mut self) {
        if self.prepared {
            return;
        }
        // Deduplicate complete immutable frame programs, not playback state.
        // Bounded weak slots preserve teardown and make collisions harmless.
        thread_local! {
            static PROGRAMS: std::cell::RefCell<Vec<Option<Weak<[Frame]>>>> =
                std::cell::RefCell::new(vec![None; 4096]);
        }
        let mut hash = rustc_hash::FxHasher::default();
        for frame in self.all_frames.iter() {
            frame.character_visual.formatted_symbol.id().hash(&mut hash);
            frame.duration.hash(&mut hash);
        }
        let slot = hash.finish() as usize & 4095;
        PROGRAMS.with(|programs| {
            let mut programs = programs.borrow_mut();
            if let Some(existing) = programs[slot].as_ref().and_then(Weak::upgrade) {
                if existing.len() == self.all_frames.len()
                    && existing
                        .iter()
                        .zip(self.all_frames.iter())
                        .all(|(a, b)| a.duration == b.duration && Rc::ptr_eq(&a.character_visual, &b.character_visual))
                {
                    self.all_frames = FrameStorage::Shared(existing);
                    return;
                }
            }
            programs[slot] = Some(Rc::downgrade(self.all_frames.share()));
        });
        self.prepared = true;
    }

    pub(crate) fn finish_frames(&mut self) {
        self.played.set(self.all_frames.len());
        self.head.set(self.end);
    }

    pub(crate) fn frame_at_tick(&self, tick: i64) -> usize {
        match self.uniform_duration {
            Some(duration) => (tick / duration) as usize,
            None => self.frame_end_ticks.partition_point(|&end| end <= tick),
        }
    }
}

/// engine/animation.py Animation: per-character animation state.
#[derive(Debug, Clone)]
pub struct Animation {
    pub scenes: OrderedMap<Scene>,
    pub active_scene: Option<crate::utils::ordered_map::MapHandle>,
    pub use_xterm_colors: bool,
    pub no_color: bool,
    pub existing_color_handling: ExistingColorHandling,
    pub input_fg_color: Option<Color>,
    pub input_bg_color: Option<Color>,
    pub input_bold: bool,
    pub active_scene_current_step: i64,
    pub current_character_visual: Rc<CharacterVisual>,
}

impl Animation {
    pub fn new(input_symbol: &str) -> Self {
        Animation {
            scenes: OrderedMap::new(),
            active_scene: None,
            use_xterm_colors: false,
            no_color: false,
            existing_color_handling: ExistingColorHandling::Ignore,
            input_fg_color: None,
            input_bg_color: None,
            input_bold: false,
            active_scene_current_step: 0,
            current_character_visual: CharacterVisual::shared(input_symbol, VisualParams::default()),
        }
    }

    /// Animation._get_color_code (identical logic to Scene's; the upstream
    /// per-instance memo is value-transparent and omitted).
    pub fn get_color_code(&mut self, color: Option<&Color>) -> Option<ColorCode> {
        resolve_color_code(color, self.no_color, self.use_xterm_colors, None)
    }

    /// Animation.new_scene: auto-ids are stringified integers probing upward;
    /// duplicate explicit ids silently overwrite (faithful).
    pub fn new_scene(
        &mut self,
        is_looping: bool,
        sync: Option<SyncMetric>,
        ease: Option<Easing>,
        scene_id: &str,
        uses_input_preexisting_colors: bool,
    ) -> String {
        let scene_id = if scene_id.is_empty() {
            let mut current_id = self.scenes.len();
            loop {
                let candidate = current_id.to_string();
                if !self.scenes.contains_key(&candidate) {
                    break candidate;
                }
                current_id += 1;
            }
        } else {
            scene_id.to_string()
        };
        let (preexisting_colors, preexisting_bold) =
            if self.existing_color_handling == ExistingColorHandling::Always && uses_input_preexisting_colors {
                (Some(ColorPair::new(self.input_fg_color.clone(), self.input_bg_color.clone())), self.input_bold)
            } else {
                (None, false)
            };
        let mut scene = Scene::new(&scene_id, is_looping, sync, ease, self.no_color, self.use_xterm_colors);
        scene.preexisting_colors = preexisting_colors;
        scene.preexisting_bold = preexisting_bold;
        self.scenes.insert(scene_id.clone(), scene);
        scene_id
    }

    /// Animation.active_scene_is_complete: no scene, no remaining frames, or looping.
    pub fn active_scene_is_complete(&self) -> bool {
        match &self.active_scene {
            None => true,
            Some(id) => {
                let scene = self.scenes.get_handle(id).expect("active scene must exist");
                scene.frames().is_empty() || scene.is_looping
            }
        }
    }

    /// Reuse a bounded effect-local palette when a workload deliberately applies
    /// the same colors to many symbols. Ordinary one-off appearance changes keep
    /// their allocation reuse and weak-reference behavior.
    pub(crate) fn set_appearance_with_palette(
        &mut self,
        input_symbol: &str,
        uses_input_preexisting_colors: bool,
        symbol: Option<&str>,
        colors: &ColorPair,
        palette: &mut AppearancePalette,
    ) {
        let input_colors;
        let (resolved, bold) =
            if self.existing_color_handling == ExistingColorHandling::Always && uses_input_preexisting_colors {
                input_colors = ColorPair::new(self.input_fg_color, self.input_bg_color);
                (&input_colors, self.input_bold)
            } else {
                (colors, false)
            };
        if let Some(visual) =
            palette.visual(symbol.unwrap_or(input_symbol), resolved, bold, self.no_color, self.use_xterm_colors)
        {
            self.current_character_visual = visual;
        } else {
            self.set_appearance(input_symbol, uses_input_preexisting_colors, symbol, Some(*colors));
        }
    }

    /// Animation.set_appearance.
    pub fn set_appearance(
        &mut self,
        input_symbol: &str,
        uses_input_preexisting_colors: bool,
        symbol: Option<&str>,
        colors: Option<ColorPair>,
    ) {
        let symbol = symbol.unwrap_or(input_symbol);
        let replacement;
        let mut bold = false;
        let colors = if self.existing_color_handling == ExistingColorHandling::Always && uses_input_preexisting_colors {
            replacement = ColorPair::new(self.input_fg_color, self.input_bg_color);
            bold = self.input_bold;
            &replacement
        } else if let Some(colors) = &colors {
            colors
        } else {
            replacement = ColorPair::default();
            &replacement
        };
        // Repeated lighting/appearance updates often resolve to exactly the
        // current visual. Test the resolved modes too: callers may change
        // no_color or xterm between updates without changing the ColorPair.
        let current = &self.current_character_visual;
        if current.symbol == symbol
            && current.colors.as_ref() == Some(colors)
            && current.bold == bold
            && !(current.dim
                || current.italic
                || current.underline
                || current.blink
                || current.reverse
                || current.hidden
                || current.strike)
            && color_code_matches(
                current.fg_color_code.as_ref(),
                colors.fg_color.as_ref(),
                self.no_color,
                self.use_xterm_colors,
            )
            && color_code_matches(
                current.bg_color_code.as_ref(),
                colors.bg_color.as_ref(),
                self.no_color,
                self.use_xterm_colors,
            )
        {
            return;
        }
        // Appearance-driven effects usually own their visual outright. Reuse
        // its allocation and strings; a scene or caller retaining a strong or
        // weak reference still receives an independent replacement.
        let mut reusable = Rc::get_mut(&mut self.current_character_visual);
        let (symbol_buffer, fg_code, bg_code) = match reusable.as_deref_mut() {
            Some(visual) => {
                let mut buffer = std::mem::take(&mut visual.symbol);
                symbol.clone_into(&mut buffer);
                (buffer, visual.fg_color_code.take(), visual.bg_color_code.take())
            }
            None => (symbol.to_owned(), None, None),
        };
        let fg_code = resolve_color_code(colors.fg_color.as_ref(), self.no_color, self.use_xterm_colors, fg_code);
        let bg_code = resolve_color_code(colors.bg_color.as_ref(), self.no_color, self.use_xterm_colors, bg_code);
        let visual = CharacterVisual::with_symbol(
            symbol_buffer,
            VisualParams {
                bold,
                colors: Some(*colors),
                fg_color_code: fg_code,
                bg_color_code: bg_code,
                ..Default::default()
            },
        );
        match reusable {
            Some(current) => *current = visual,
            None => self.current_character_visual = Rc::new(visual),
        }
    }

    /// Animation.adjust_color_brightness: hand-rolled RGB->HSL->RGB with
    /// round() (banker's) at the end — unlike shift_color_towards's truncation.
    pub fn adjust_color_brightness(color: &Color, brightness: f64) -> Color {
        use crate::utils::pycompat::round_half_even;

        fn hue_to_rgb(lightness_scaled: f64, color_intensity: f64, mut hue_value: f64) -> f64 {
            if hue_value < 0.0 {
                hue_value += 1.0;
            }
            if hue_value > 1.0 {
                hue_value -= 1.0;
            }
            if hue_value < 1.0 / 6.0 {
                return lightness_scaled + (color_intensity - lightness_scaled) * 6.0 * hue_value;
            }
            if hue_value < 1.0 / 2.0 {
                return color_intensity;
            }
            if hue_value < 2.0 / 3.0 {
                return lightness_scaled + (color_intensity - lightness_scaled) * (2.0 / 3.0 - hue_value) * 6.0;
            }
            lightness_scaled
        }

        let (r, g, b) = color.rgb_ints();
        let normalized_red = r as f64 / 255.0;
        let normalized_green = g as f64 / 255.0;
        let normalized_blue = b as f64 / 255.0;

        let max_val = normalized_red.max(normalized_green).max(normalized_blue);
        let min_val = normalized_red.min(normalized_green).min(normalized_blue);
        let mut lightness = (max_val + min_val) / 2.0;

        let lightness_threshold = 0.5;
        let (hue_value, saturation) = if max_val == min_val {
            (0.0, 0.0)
        } else {
            let diff = max_val - min_val;
            let saturation = if lightness > lightness_threshold {
                diff / (2.0 - max_val - min_val)
            } else {
                diff / (max_val + min_val)
            };
            let mut hue_value = if max_val == normalized_red {
                (normalized_green - normalized_blue) / diff + if normalized_green < normalized_blue { 6.0 } else { 0.0 }
            } else if max_val == normalized_green {
                (normalized_blue - normalized_red) / diff + 2.0
            } else {
                (normalized_red - normalized_green) / diff + 4.0
            };
            hue_value /= 6.0;
            (hue_value, saturation)
        };

        lightness = (lightness * brightness).min(1.0).max(0.0);

        let (red, green, blue) = if saturation == 0.0 {
            (lightness, lightness, lightness)
        } else {
            let color_intensity = if lightness < lightness_threshold {
                lightness * (1.0 + saturation)
            } else {
                lightness + saturation - lightness * saturation
            };
            let lightness_scaled = 2.0 * lightness - color_intensity;
            (
                hue_to_rgb(lightness_scaled, color_intensity, hue_value + 1.0 / 3.0),
                hue_to_rgb(lightness_scaled, color_intensity, hue_value),
                hue_to_rgb(lightness_scaled, color_intensity, hue_value - 1.0 / 3.0),
            )
        };

        Color::from_rgb(
            round_half_even(red * 255.0) as u8,
            round_half_even(green * 255.0) as u8,
            round_half_even(blue * 255.0) as u8,
        )
    }
}

#[cfg(test)]
mod shared_visual_tests {
    use super::*;

    fn params(hex: &str) -> VisualParams {
        let color = Color::from_hex(hex).unwrap();
        VisualParams {
            colors: Some(ColorPair::new(Some(color), None)),
            fg_color_code: Some(ColorCode::Rgb(hex.to_string())),
            ..Default::default()
        }
    }

    #[test]
    fn equal_symbol_and_styling_share_one_visual() {
        let a = CharacterVisual::shared("█", params("ff0000"));
        let b = CharacterVisual::shared("█", params("ff0000"));
        assert!(Rc::ptr_eq(&a, &b));
        assert_eq!(a.formatted_symbol.as_str(), "\x1b[38;2;255;0;0m█\x1b[0m");
    }

    #[test]
    fn different_symbol_or_styling_do_not_share() {
        let base = CharacterVisual::shared("█", params("00ff00"));
        assert!(!Rc::ptr_eq(&base, &CharacterVisual::shared("▀", params("00ff00"))));
        assert!(!Rc::ptr_eq(&base, &CharacterVisual::shared("█", params("00ff01"))));
        let mut bold = params("00ff00");
        bold.bold = true;
        assert!(!Rc::ptr_eq(&base, &CharacterVisual::shared("█", bold)));
        // dim is never emitted, but it is still part of the visual
        let mut dim = params("00ff00");
        dim.dim = true;
        assert!(!Rc::ptr_eq(&base, &CharacterVisual::shared("█", dim)));
    }

    #[test]
    fn dropped_visuals_are_swept_out_of_the_table() {
        let before = SHARED_VISUALS.with(|s| s.borrow().table.len());
        for i in 0..(SharedVisuals::MIN_SWEEP * 4) {
            drop(CharacterVisual::shared(&format!("{i}"), VisualParams::default()));
        }
        let after = SHARED_VISUALS.with(|s| s.borrow().table.len());
        assert!(after < before + SharedVisuals::MIN_SWEEP * 2, "table kept growing: {before} -> {after}");
    }

    #[test]
    fn scene_frames_share_visuals_across_scenes() {
        let mut a = Scene::new("a", false, None, None, false, false);
        let mut b = Scene::new("b", false, None, None, false, false);
        a.add_frame("x", 1, params("123456")).unwrap();
        b.add_frame("x", 1, params("123456")).unwrap();
        assert!(Rc::ptr_eq(&a.all_frames[0].character_visual, &b.all_frames[0].character_visual));
    }
}

#[cfg(test)]
mod appearance_palette_tests {
    use super::*;

    #[test]
    fn palette_observes_color_code_edits_even_when_constructor_arguments_match() {
        for xterm in [false, true] {
            let mut palette = AppearancePalette::new();
            palette.enabled = true;
            let mut ordinary = Animation::new("x");
            ordinary.use_xterm_colors = xterm;
            let mut cached = ordinary.clone();
            let mut foreground = Color::from_hex("112233").unwrap();
            let mut background = Color::from_xterm(42);
            for step in 0..20 {
                foreground.rgb_color = Color::from_xterm(100 + step).rgb_color;
                foreground.xterm_color = Some(100 + step);
                background.rgb_color = Color::from_xterm(200 + step).rgb_color;
                background.xterm_color = Some(200 + step);
                let colors = ColorPair::new(Some(foreground), Some(background));
                ordinary.set_appearance("x", false, None, Some(colors));
                cached.set_appearance_with_palette("x", false, None, &colors, &mut palette);
                assert_eq!(ordinary.current_character_visual, cached.current_character_visual);
                assert_eq!(
                    ordinary.current_character_visual.formatted_symbol.as_str(),
                    cached.current_character_visual.formatted_symbol.as_str()
                );
            }
        }
    }

    #[test]
    fn palette_matches_ordinary_appearance_across_modes_eviction_and_mutation() {
        let mut palette = AppearancePalette::new();
        palette.enabled = true;
        let mut ordinary = Animation::new("original");
        let mut cached = ordinary.clone();
        for iteration in 0..10_000 {
            let symbol = ["x", "λ", "界", "🥟", "multiple characters", ""][iteration % 6];
            let colors = ColorPair::new(
                Some(Color::from_hex(&format!("{:06x}", iteration * 199 % 0x1000000)).unwrap()),
                (iteration % 3 == 0).then(|| Color::from_xterm((iteration % 256) as u8)),
            );
            for animation in [&mut ordinary, &mut cached] {
                animation.no_color = iteration % 7 == 0;
                animation.use_xterm_colors = iteration % 5 == 0;
                animation.existing_color_handling =
                    if iteration % 4 == 0 { ExistingColorHandling::Always } else { ExistingColorHandling::Ignore };
                animation.input_bold = iteration % 2 == 0;
                animation.input_fg_color = Some(Color::from_xterm(196));
            }
            let (input, replacement) =
                if iteration % 2 == 0 { ("original input", Some(symbol)) } else { (symbol, None) };
            ordinary.set_appearance(input, true, replacement, Some(colors));
            cached.set_appearance_with_palette(input, true, replacement, &colors, &mut palette);
            assert_eq!(ordinary.current_character_visual, cached.current_character_visual);
            if iteration % 11 == 0 {
                Rc::make_mut(&mut cached.current_character_visual).symbol = "independently changed".into();
                cached.set_appearance_with_palette(input, true, replacement, &colors, &mut palette);
                assert_eq!(ordinary.current_character_visual, cached.current_character_visual);
            }
        }
        assert_eq!(palette.slots.len(), AppearancePalette::CAPACITY);
    }
}
