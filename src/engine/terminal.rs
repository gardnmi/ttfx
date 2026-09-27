//! Terminal: config, canvas assembly, character queries, renderer, tty output.
//! Ported from engine/terminal.py. Unlike upstream (which builds two Terminals
//! per run), this single Terminal owns both the simulation and the tty side.

use std::collections::HashMap;
use std::io::Write;
use std::time::Instant;

use crate::engine::animation::ExistingColorHandling;
use crate::engine::canvas::{Anchor, Canvas};
use crate::engine::character::{CharId, CharacterArena, EffectCharacter};
use crate::engine::error::EngineError;
use crate::engine::input::{ColorFrequency, Preprocessor};
use crate::utils::ansi;
use crate::utils::geometry::Coord;
use crate::utils::graphics::Color;
use crate::utils::rng::Rng;

const EMPTY_RENDER_CELL: u32 = u32::MAX;
const NOT_VISIBLE: usize = usize::MAX;

/// Output prepared for the current animation frame. Cached rows belong to the
/// terminal and must be consumed before the next frame is prepared.
pub enum FrameOutput {
    Contiguous(String),
    CachedRows,
}

#[derive(Debug, Clone)]
pub struct TerminalConfig {
    pub tab_width: i64,
    pub xterm_colors: bool,
    pub no_color: bool,
    pub terminal_background_color: Color,
    pub existing_color_handling: ExistingColorHandling,
    pub wrap_text: bool,
    pub frame_rate: i64,
    pub canvas_width: i64,
    pub canvas_height: i64,
    pub anchor_canvas: Anchor,
    pub anchor_text: Anchor,
    pub ignore_terminal_dimensions: bool,
    pub reuse_canvas: bool,
    pub no_eol: bool,
    pub no_restore_cursor: bool,
}

impl Default for TerminalConfig {
    fn default() -> Self {
        TerminalConfig {
            tab_width: 4,
            xterm_colors: false,
            no_color: false,
            terminal_background_color: Color::from_hex("000000").unwrap(),
            existing_color_handling: ExistingColorHandling::Ignore,
            wrap_text: false,
            frame_rate: 60,
            canvas_width: -1,
            canvas_height: -1,
            anchor_canvas: Anchor::Sw,
            anchor_text: Anchor::Sw,
            ignore_terminal_dimensions: false,
            reuse_canvas: false,
            no_eol: false,
            no_restore_cursor: false,
        }
    }
}

/// CharacterSort (argutils.CharacterSort).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharacterSort {
    Random,
    TopToBottomLeftToRight,
    BottomToTopRightToLeft,
    BottomToTopLeftToRight,
    TopToBottomRightToLeft,
    OutsideRowToMiddle,
    MiddleRowToOutside,
}

/// CharacterGroup (argutils.CharacterGroup).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CharacterGroup {
    ColumnLeftToRight,
    ColumnRightToLeft,
    RowTopToBottom,
    RowBottomToTop,
    DiagonalBottomLeftToTopRight,
    DiagonalTopRightToBottomLeft,
    DiagonalTopLeftToBottomRight,
    DiagonalBottomRightToTopLeft,
    CenterToOutside,
    OutsideToCenter,
}

/// ColorSort (argutils.ColorSort).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorSort {
    LeastToMost,
    MostToLeast,
    Random,
}

/// Which character populations to include in a query (the four bool kwargs).
#[derive(Debug, Clone, Copy)]
pub struct CharacterFilter {
    pub input_chars: bool,
    pub inner_fill_chars: bool,
    pub outer_fill_chars: bool,
    pub added_chars: bool,
}

impl Default for CharacterFilter {
    fn default() -> Self {
        CharacterFilter { input_chars: true, inner_fill_chars: false, outer_fill_chars: false, added_chars: false }
    }
}

/// The input-coordinate index retains HashMap access while recording mutable
/// access and replacement for effect caches. A mutable borrow may perform any
/// number of map edits; Rust excludes a simultaneous frame update.
#[derive(Debug, Clone, Default)]
pub struct InputCoordinateMap {
    entries: HashMap<Coord, CharId>,
    dense: std::cell::OnceCell<Option<DenseInputCoordinates>>,
    revision: super::revision::Revision,
}

impl From<HashMap<Coord, CharId>> for InputCoordinateMap {
    fn from(entries: HashMap<Coord, CharId>) -> Self {
        Self { entries, dense: std::cell::OnceCell::new(), revision: super::revision::Revision::default() }
    }
}

/// Dense input text permits direct coordinate lookup. Sparse or enormous
/// coordinate domains keep the HashMap; storage is bounded independently of
/// user-provided coordinates.
#[derive(Debug, Clone)]
struct DenseInputCoordinates {
    left: i64,
    bottom: i64,
    width: usize,
    height: usize,
    cells: Vec<Option<CharId>>,
}

impl DenseInputCoordinates {
    fn build(entries: &HashMap<Coord, CharId>) -> Option<Self> {
        let left = entries.keys().map(|c| c.column).min()?;
        let right = entries.keys().map(|c| c.column).max()?;
        let bottom = entries.keys().map(|c| c.row).min()?;
        let top = entries.keys().map(|c| c.row).max()?;
        let width = usize::try_from(right.checked_sub(left)?.checked_add(1)?).ok()?;
        let height = usize::try_from(top.checked_sub(bottom)?.checked_add(1)?).ok()?;
        let len = width.checked_mul(height)?;
        if len > 1_048_576 || len > entries.len().saturating_mul(4) {
            return None;
        }
        let mut cells = vec![None; len];
        for (coord, &id) in entries {
            cells[(coord.column - left) as usize * height + (coord.row - bottom) as usize] = Some(id);
        }
        Some(Self { left, bottom, width, height, cells })
    }

    fn get(&self, coord: Coord) -> Option<CharId> {
        let column = usize::try_from(coord.column.checked_sub(self.left)?).ok()?;
        let row = usize::try_from(coord.row.checked_sub(self.bottom)?).ok()?;
        if column >= self.width || row >= self.height {
            return None;
        }
        self.cells[column * self.height + row]
    }
}

impl InputCoordinateMap {
    /// Visit only populated input cells inside the exact column-major ellipse.
    /// The dense index lets beams skip empty off-canvas spans before walking
    /// cells. Sparse maps retain the ordinary coordinate iterator.
    pub(crate) fn for_each_in_circle(&self, center: Coord, diameter: i64, mut visit: impl FnMut(CharId)) {
        let Some(dense) = self.dense.get_or_init(|| DenseInputCoordinates::build(&self.entries)) else {
            for coord in crate::utils::geometry::coords_in_circle(center, diameter) {
                if let Some(&id) = self.entries.get(&coord) {
                    visit(id);
                }
            }
            return;
        };
        if diameter <= 0 {
            return;
        }
        let left = center.column.saturating_sub(diameter).max(dense.left);
        let right = center.column.saturating_add(diameter).min(dense.left + (dense.width - 1) as i64);
        let top = dense.bottom + (dense.height - 1) as i64;
        let a_squared = (diameter as f64).powf(2.0);
        let b_squared = (diameter as f64 / 2.0).powf(2.0);
        for column in left..=right {
            let rows =
                crate::utils::geometry::circle_column_y_range(column, center.column, center.row, a_squared, b_squared);
            let bottom = (*rows.start()).max(dense.bottom);
            let top = (*rows.end()).min(top);
            if bottom > top {
                continue;
            }
            let start = (column - dense.left) as usize * dense.height + (bottom - dense.bottom) as usize;
            let end = start + (top - bottom) as usize + 1;
            for &id in dense.cells[start..end].iter().flatten() {
                visit(id);
            }
        }
    }

    fn lookup(&self, coord: Coord) -> Option<CharId> {
        match self.dense.get_or_init(|| DenseInputCoordinates::build(&self.entries)) {
            Some(dense) => dense.get(coord),
            None => self.entries.get(&coord).copied(),
        }
    }
}

impl std::ops::Deref for InputCoordinateMap {
    type Target = HashMap<Coord, CharId>;
    fn deref(&self) -> &Self::Target {
        &self.entries
    }
}

impl std::ops::DerefMut for InputCoordinateMap {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.revision = super::revision::Revision::default();
        self.dense.take();
        &mut self.entries
    }
}

impl<'a> IntoIterator for &'a InputCoordinateMap {
    type Item = (&'a Coord, &'a CharId);
    type IntoIter = std::collections::hash_map::Iter<'a, Coord, CharId>;
    fn into_iter(self) -> Self::IntoIter {
        self.entries.iter()
    }
}

impl<'a> IntoIterator for &'a mut InputCoordinateMap {
    type Item = (&'a Coord, &'a mut CharId);
    type IntoIter = std::collections::hash_map::IterMut<'a, Coord, CharId>;
    fn into_iter(self) -> Self::IntoIter {
        self.revision = super::revision::Revision::default();
        self.dense.take();
        self.entries.iter_mut()
    }
}

impl IntoIterator for InputCoordinateMap {
    type Item = (Coord, CharId);
    type IntoIter = std::collections::hash_map::IntoIter<Coord, CharId>;
    fn into_iter(self) -> Self::IntoIter {
        self.entries.into_iter()
    }
}

impl PartialEq for InputCoordinateMap {
    fn eq(&self, other: &Self) -> bool {
        self.entries == other.entries
    }
}

impl Eq for InputCoordinateMap {}

pub struct Terminal {
    pub config: TerminalConfig,
    pub canvas: Canvas,
    pub arena: CharacterArena,
    next_character_id: u32,
    pub input_colors_frequency: ColorFrequency,
    terminal_dimensions: (i64, i64),
    resize_seen_at: Option<Instant>,
    /// Pre-wrap input line lengths — all `compute_layout` needs from the input,
    /// so a resize can re-derive the geometry without re-preprocessing.
    input_line_lengths: Vec<i64>,
    pub canvas_column_offset: i64,
    pub canvas_row_offset: i64,
    pub visible_top: i64,
    pub visible_bottom: i64,
    pub visible_right: i64,
    pub visible_left: i64,
    pub input_characters: Vec<CharId>,
    pub added_characters: Vec<CharId>,
    pub character_by_input_coord: InputCoordinateMap,
    pub inner_fill_characters: Vec<CharId>,
    pub outer_fill_characters: Vec<CharId>,
    visible_characters: Vec<CharId>,
    visible_positions: Vec<usize>,
    render_grid: crate::engine::render::CellGrid,
    render_cells: Vec<u32>,
    render_visual_ids: Vec<u64>,
    previous_visual_ids: Vec<u64>,
    rendered_rows: Vec<crate::engine::render::CachedRow>,
    row_scratch: Vec<u8>,
    rendered_width: usize,
    rendered_revision: Option<u64>,
    row_changes: fn(&[u64], &[u64]) -> u64,
    pub terminal_state: Vec<String>,
    output_buffer: String,
    move_cursor_to_top: String,
    frame_rate: i64,
    last_time_printed: Instant,
}

fn ordered_buckets(
    characters: Vec<CharId>,
    first_key: i64,
    last_key: i64,
    mut key: impl FnMut(CharId) -> i64,
) -> Vec<Vec<CharId>> {
    if first_key > last_key {
        return Vec::new();
    }
    let bucket_count = last_key
        .checked_sub(first_key)
        .and_then(|span| span.checked_add(1))
        .and_then(|span| usize::try_from(span).ok())
        .expect("terminal canvas is too large");
    let expected_bucket_len = characters.len() / bucket_count;
    let mut buckets: Vec<Vec<CharId>> = (0..bucket_count).map(|_| Vec::with_capacity(expected_bucket_len)).collect();
    for id in characters {
        let character_key = key(id);
        if first_key <= character_key && character_key <= last_key {
            buckets[(character_key - first_key) as usize].push(id);
        }
    }
    buckets.into_iter().filter(|bucket| !bucket.is_empty()).collect()
}

impl Terminal {
    pub fn new(input_data: &str, config: TerminalConfig) -> Result<Self, EngineError> {
        let input_data = if input_data.is_empty() { "No Input." } else { input_data };
        let mut arena: Vec<EffectCharacter> = Vec::new();
        let mut next_character_id: u32 = 0;
        let mut input_colors_frequency = ColorFrequency::default();

        let preprocessed_lines = Preprocessor {
            arena: &mut arena,
            next_character_id: &mut next_character_id,
            input_colors_frequency: &mut input_colors_frequency,
            config: &config,
        }
        .preprocess(input_data)?;

        let input_line_lengths: Vec<i64> = preprocessed_lines.iter().map(|l| l.len() as i64).collect();
        let terminal_dimensions = get_terminal_dimensions();
        let layout = compute_layout(&config, &input_line_lengths, terminal_dimensions.0, terminal_dimensions.1);
        let mut canvas = Canvas::new(layout.canvas_height, layout.canvas_width);
        let Layout {
            column_offset: canvas_column_offset,
            row_offset: canvas_row_offset,
            visible_top,
            visible_bottom,
            visible_right,
            visible_left,
            ..
        } = layout;

        let input_characters = setup_input_characters(&config, &mut canvas, &mut arena, preprocessed_lines)?
            .into_iter()
            .filter(|&id| {
                let coord = arena[id.0 as usize].input_coord;
                coord.row <= canvas.top && coord.column <= canvas.right
            })
            .collect::<Vec<_>>();

        let mut character_by_input_coord: HashMap<Coord, CharId> = HashMap::new();
        for &id in &input_characters {
            character_by_input_coord.insert(arena[id.0 as usize].input_coord, id);
        }

        let frame_rate = config.frame_rate;
        let arena_len = arena.len();
        let move_cursor_to_top = format!(
            "{}{}{}",
            ansi::DEC_RESTORE_CURSOR,
            ansi::DEC_SAVE_CURSOR,
            ansi::move_cursor_up(visible_top.max(0) as usize)
        );
        let mut terminal = Terminal {
            config,
            canvas,
            arena: arena.into(),
            next_character_id,
            input_colors_frequency,
            terminal_dimensions,
            resize_seen_at: None,
            input_line_lengths,
            canvas_column_offset,
            canvas_row_offset,
            visible_top,
            visible_bottom,
            visible_right,
            visible_left,
            input_characters,
            added_characters: Vec::new(),
            character_by_input_coord: character_by_input_coord.into(),
            inner_fill_characters: Vec::new(),
            outer_fill_characters: Vec::new(),
            visible_characters: Vec::new(),
            visible_positions: vec![NOT_VISIBLE; arena_len],
            render_grid: crate::engine::render::CellGrid::default(),
            render_cells: Vec::new(),
            render_visual_ids: Vec::new(),
            previous_visual_ids: Vec::new(),
            rendered_rows: Vec::new(),
            row_scratch: Vec::new(),
            rendered_width: 0,
            rendered_revision: None,
            row_changes: crate::engine::render::select_row_comparison(),
            terminal_state: Vec::new(),
            output_buffer: String::new(),
            move_cursor_to_top,
            frame_rate,
            last_time_printed: Instant::now(),
        };
        terminal.make_fill_characters();
        terminal.setup_character_neighbors();
        terminal.update_terminal_state();
        Ok(terminal)
    }

    /// Terminal._make_fill_characters: row-major from (1,1), fresh space chars
    /// for unoccupied canvas coords, split inner/outer by the text bounds.
    fn make_fill_characters(&mut self) {
        for row in 1..=self.canvas.top {
            for column in 1..=self.canvas.right {
                let coord = Coord::new(column, row);
                if !self.character_by_input_coord.contains_key(&coord) {
                    let mut fill = EffectCharacter::new(self.next_character_id, " ", column, row);
                    fill.is_fill_character = true;
                    fill.animation.no_color = self.config.no_color;
                    fill.animation.use_xterm_colors = self.config.xterm_colors;
                    fill.animation.existing_color_handling = self.config.existing_color_handling;
                    fill.uses_input_preexisting_colors = false;
                    self.next_character_id += 1;
                    let id = CharId(self.arena.len() as u32);
                    self.arena.push(fill);
                    self.character_by_input_coord.insert(coord, id);
                    if self.canvas.text_left <= column
                        && column <= self.canvas.text_right
                        && self.canvas.text_bottom <= row
                        && row <= self.canvas.text_top
                    {
                        self.inner_fill_characters.push(id);
                    } else {
                        self.outer_fill_characters.push(id);
                    }
                }
            }
        }
    }

    fn setup_character_neighbors(&mut self) {
        let coords: Vec<(Coord, CharId)> = self.character_by_input_coord.iter().map(|(&c, &id)| (c, id)).collect();
        for (coord, id) in coords {
            let n = self.character_by_input_coord.get(&Coord::new(coord.column, coord.row + 1)).copied();
            let e = self.character_by_input_coord.get(&Coord::new(coord.column + 1, coord.row)).copied();
            let s = self.character_by_input_coord.get(&Coord::new(coord.column, coord.row - 1)).copied();
            let w = self.character_by_input_coord.get(&Coord::new(coord.column - 1, coord.row)).copied();
            let ch = &mut self.arena[id.0 as usize];
            ch.neighbors.north = n;
            ch.neighbors.east = e;
            ch.neighbors.south = s;
            ch.neighbors.west = w;
        }
    }

    /// Terminal.add_character: registered only in added_characters, not in
    /// character_by_input_coord or the neighbor map.
    pub fn add_character(&mut self, symbol: &str, coord: Coord) -> CharId {
        let mut ch = EffectCharacter::new(self.next_character_id, symbol, coord.column, coord.row);
        ch.animation.no_color = self.config.no_color;
        ch.animation.use_xterm_colors = self.config.xterm_colors;
        ch.animation.existing_color_handling = self.config.existing_color_handling;
        ch.uses_input_preexisting_colors = false;
        self.next_character_id += 1;
        let id = CharId(self.arena.len() as u32);
        self.arena.push(ch);
        self.added_characters.push(id);
        id
    }

    pub fn get_character_by_input_coord(&self, coord: Coord) -> Option<CharId> {
        self.character_by_input_coord.lookup(coord)
    }

    /// Dirty edits invalidate immediately. A caller that renders those edits
    /// advances the renderer revision, so consuming dirtiness cannot hide them.
    /// Replacing/cloning the arena or coordinate map also changes this token.
    pub(crate) fn lighting_stamp(&self) -> Option<(u64, u64, u64)> {
        (self.arena.dirty_len() == 0).then_some((
            self.arena.identity(),
            self.character_by_input_coord.revision.0,
            self.render_grid.revision,
        ))
    }

    pub fn set_character_visibility(&mut self, id: CharId, is_visible: bool) {
        let arena_index = id.0 as usize;
        if self.arena[arena_index].is_visible == is_visible {
            return;
        }
        self.arena[arena_index].is_visible = is_visible;
        self.visible_positions.resize(self.arena.len(), NOT_VISIBLE);
        if is_visible {
            self.visible_positions[arena_index] = self.visible_characters.len();
            self.visible_characters.push(id);
        } else {
            let position = std::mem::replace(&mut self.visible_positions[arena_index], NOT_VISIBLE);
            self.visible_characters.swap_remove(position);
            if position < self.visible_characters.len() {
                let moved = self.visible_characters[position];
                self.visible_positions[moved.0 as usize] = position;
            }
        }
    }

    /// Terminal.get_input_colors. Equal-count ties keep insertion order
    /// (Python's stable sort over dict keys).
    pub fn get_input_colors(&self, rng: &mut Rng, sort: ColorSort) -> Vec<Color> {
        let mut colors: Vec<(Color, i64)> = self.input_colors_frequency.0.clone();
        match sort {
            ColorSort::MostToLeast => {
                // Python: sorted(keys, key=count, reverse=True) — reverse of a
                // stable ascending sort reverses tie order too; replicate by
                // sorting descending with stable tie order = insertion order.
                colors.sort_by(|a, b| b.1.cmp(&a.1));
            }
            ColorSort::LeastToMost => {
                colors.sort_by(|a, b| a.1.cmp(&b.1));
            }
            ColorSort::Random => {
                rng.shuffle(&mut colors);
            }
        }
        colors.into_iter().map(|(c, _)| c).collect()
    }

    pub fn collect_characters(&self, filter: CharacterFilter) -> Vec<CharId> {
        let capacity = if filter.input_chars { self.input_characters.len() } else { 0 }
            + if filter.inner_fill_chars { self.inner_fill_characters.len() } else { 0 }
            + if filter.outer_fill_chars { self.outer_fill_characters.len() } else { 0 }
            + if filter.added_chars { self.added_characters.len() } else { 0 };
        let mut all: Vec<CharId> = Vec::with_capacity(capacity);
        if filter.input_chars {
            all.extend(&self.input_characters);
        }
        if filter.inner_fill_chars {
            all.extend(&self.inner_fill_characters);
        }
        if filter.outer_fill_chars {
            all.extend(&self.outer_fill_characters);
        }
        if filter.added_chars {
            all.extend(&self.added_characters);
        }
        all
    }

    /// Terminal.get_characters with all sort variants.
    pub fn get_characters(&self, rng: &mut Rng, filter: CharacterFilter, sort: CharacterSort) -> Vec<CharId> {
        let mut all = self.collect_characters(filter);
        // default sort: (-row, column), stable
        all.sort_by_key(|&id| {
            let c = self.arena[id.0 as usize].input_coord;
            (-c.row, c.column)
        });
        match sort {
            CharacterSort::Random => rng.shuffle(&mut all),
            CharacterSort::TopToBottomLeftToRight => {}
            CharacterSort::BottomToTopRightToLeft => all.reverse(),
            CharacterSort::BottomToTopLeftToRight | CharacterSort::TopToBottomRightToLeft => {
                all.sort_by_key(|&id| {
                    let c = self.arena[id.0 as usize].input_coord;
                    (c.row, c.column)
                });
                if sort == CharacterSort::TopToBottomRightToLeft {
                    all.reverse();
                }
            }
            CharacterSort::OutsideRowToMiddle | CharacterSort::MiddleRowToOutside => {
                // upstream: alternate pop(0)/pop(-1)
                let mut deque: std::collections::VecDeque<CharId> = all.into();
                let mut interleaved = Vec::with_capacity(deque.len());
                let mut from_front = true;
                while let Some(id) = if from_front { deque.pop_front() } else { deque.pop_back() } {
                    interleaved.push(id);
                    from_front = !from_front;
                }
                all = interleaved;
                if sort == CharacterSort::MiddleRowToOutside {
                    all.reverse();
                }
            }
        }
        all
    }

    /// Terminal.get_characters_grouped with all grouping variants.
    pub fn get_characters_grouped(&self, filter: CharacterFilter, grouping: CharacterGroup) -> Vec<Vec<CharId>> {
        let mut all = self.collect_characters(filter);
        all.sort_by_key(|&id| {
            let c = self.arena[id.0 as usize].input_coord;
            (c.row, c.column)
        });
        let coord = |id: &CharId| self.arena[id.0 as usize].input_coord;
        match grouping {
            CharacterGroup::ColumnLeftToRight | CharacterGroup::ColumnRightToLeft => {
                let mut columns = ordered_buckets(all, 0, self.canvas.right, |id| coord(&id).column);
                if grouping == CharacterGroup::ColumnRightToLeft {
                    columns.reverse();
                }
                columns
            }
            CharacterGroup::RowBottomToTop | CharacterGroup::RowTopToBottom => {
                let mut rows = ordered_buckets(all, 0, self.canvas.top, |id| coord(&id).row);
                if grouping == CharacterGroup::RowTopToBottom {
                    rows.reverse();
                }
                rows
            }
            CharacterGroup::DiagonalBottomLeftToTopRight | CharacterGroup::DiagonalTopRightToBottomLeft => {
                let mut diagonals = ordered_buckets(all, 0, self.canvas.top + self.canvas.right, |id| {
                    let c = coord(&id);
                    c.row + c.column
                });
                if grouping == CharacterGroup::DiagonalTopRightToBottomLeft {
                    diagonals.reverse();
                }
                diagonals
            }
            CharacterGroup::DiagonalTopLeftToBottomRight | CharacterGroup::DiagonalBottomRightToTopLeft => {
                let mut diagonals = ordered_buckets(
                    all,
                    self.canvas.left - self.canvas.top,
                    self.canvas.right - self.canvas.bottom,
                    |id| {
                        let c = coord(&id);
                        c.column - c.row
                    },
                );
                if grouping == CharacterGroup::DiagonalBottomRightToTopLeft {
                    diagonals.reverse();
                }
                diagonals
            }
            CharacterGroup::CenterToOutside | CharacterGroup::OutsideToCenter => {
                let max_distance = all
                    .iter()
                    .map(|&id| {
                        let c = coord(&id);
                        (c.column - self.canvas.text_center.column).abs() + (c.row - self.canvas.text_center.row).abs()
                    })
                    .max();
                let dense_limit = all.len().saturating_mul(4).max(256);
                let mut groups = if max_distance
                    .and_then(|distance| usize::try_from(distance).ok())
                    .is_some_and(|distance| distance <= dense_limit)
                {
                    ordered_buckets(all, 0, max_distance.unwrap(), |id| {
                        let c = coord(&id);
                        (c.column - self.canvas.text_center.column).abs() + (c.row - self.canvas.text_center.row).abs()
                    })
                } else {
                    // Out-of-canvas added characters can have sparse, arbitrarily
                    // large distances; avoid allocating through the largest key.
                    let mut distances: HashMap<i64, Vec<CharId>> = HashMap::new();
                    for id in all {
                        let c = coord(&id);
                        let distance = (c.column - self.canvas.text_center.column).abs()
                            + (c.row - self.canvas.text_center.row).abs();
                        distances.entry(distance).or_default().push(id);
                    }
                    let mut distances: Vec<(i64, Vec<CharId>)> = distances.into_iter().collect();
                    distances.sort_by_key(|&(distance, _)| distance);
                    distances.into_iter().map(|(_, group)| group).collect()
                };
                if grouping == CharacterGroup::OutsideToCenter {
                    groups.reverse();
                }
                groups
            }
        }
    }

    /// Paint the visible characters into the reusable cell buffer using the
    /// canonical (layer, character_id) painter order (plan.md §4.3).
    fn update_render_cells(&mut self) -> (usize, usize) {
        self.render_grid.update(
            &mut self.arena,
            [
                self.visible_left,
                self.visible_right,
                self.visible_bottom,
                self.visible_top,
                self.canvas_column_offset,
                self.canvas_row_offset,
            ],
            &mut self.render_cells,
            &mut self.render_visual_ids,
        )
    }

    /// Terminal._update_terminal_state: materialize the row-oriented state
    /// exposed by the upstream API. Frame output uses the cell buffer directly
    /// so the hot path does not copy every rendered byte through these rows.
    pub fn update_terminal_state(&mut self) {
        let (width, height) = self.update_render_cells();

        self.terminal_state.resize_with(height, String::new);
        self.terminal_state.truncate(height);
        let arena = &self.arena;
        for (row_index, row) in self.terminal_state.iter_mut().enumerate() {
            row.clear();
            if row.capacity() < width {
                row.reserve(width);
            }
            for &cell in &self.render_cells[row_index * width..(row_index + 1) * width] {
                if cell == EMPTY_RENDER_CELL {
                    row.push(' ');
                } else {
                    row.push_str(
                        arena.render_slice()[cell as usize]
                            .animation
                            .current_character_visual
                            .formatted_symbol
                            .as_str(),
                    );
                }
            }
        }
    }

    /// get_formatted_output_string: refresh + emit top row first.
    pub(crate) fn prepare_frame_output(&mut self) -> FrameOutput {
        let (width, height) = self.update_render_cells();
        let unchanged = self.rendered_revision == Some(self.render_grid.revision);
        if unchanged {
            if !self.render_grid.dense {
                return FrameOutput::CachedRows;
            }
            if !self.output_buffer.is_empty() {
                return FrameOutput::Contiguous(std::mem::take(&mut self.output_buffer));
            }
        }
        self.rendered_revision = Some(self.render_grid.revision);
        if self.render_grid.dense {
            let minimum_capacity = width
                .checked_mul(height)
                .and_then(|cells| cells.checked_add(height.saturating_sub(1)))
                .expect("terminal canvas is too large");
            let mut out = std::mem::take(&mut self.output_buffer).into_bytes();
            out.clear();
            if out.capacity() < minimum_capacity {
                out.reserve(minimum_capacity);
            }

            for row in (0..height).rev() {
                if row + 1 < height {
                    out.push(b'\n');
                }
                for &cell in &self.render_cells[row * width..(row + 1) * width] {
                    if cell == EMPTY_RENDER_CELL {
                        out.push(b' ');
                    } else {
                        self.arena.render_slice()[cell as usize]
                            .animation
                            .current_character_visual
                            .formatted_symbol
                            .append_to(&mut out);
                    }
                }
            }
            // SAFETY: only complete UTF-8 symbols, spaces, and newlines were appended.
            return FrameOutput::Contiguous(unsafe { String::from_utf8_unchecked(out) });
        }
        if self.rendered_width != width || self.rendered_rows.len() != height {
            self.previous_visual_ids.clear();
            self.previous_visual_ids.resize(width * height, u64::MAX);
            self.rendered_rows.clear();
            self.rendered_rows.resize_with(height, crate::engine::render::CachedRow::default);
            self.rendered_width = width;
        }
        let arena = &self.arena;
        for row_index in (0..height).rev() {
            if !self.render_grid.dirty_rows[row_index] {
                continue;
            }
            self.render_grid.dirty_rows[row_index] = false;
            let start = row_index * width;
            let ids = &self.render_visual_ids[start..start + width];
            let previous = &mut self.previous_visual_ids[start..start + width];
            let row = &mut self.rendered_rows[row_index];
            let changes = (self.row_changes)(ids, previous);
            if changes != 0 {
                row.rebuild(
                    &self.render_cells[start..start + width],
                    changes,
                    arena.render_slice(),
                    &mut self.row_scratch,
                );
                previous.copy_from_slice(ids);
            }
        }
        FrameOutput::CachedRows
    }

    pub(crate) fn recycle_frame(&mut self, output: FrameOutput) {
        if let FrameOutput::Contiguous(output) = output {
            self.recycle_output_string(output);
        }
    }

    pub(crate) fn recycle_output_string(&mut self, output: String) {
        // Retain bytes as well as capacity so unchanged dense frames can emit
        // the prior serialization. Preparing changed content still clears it.
        if output.capacity() > self.output_buffer.capacity() {
            self.output_buffer = output;
        }
    }

    /// Compatibility helper for callers that need an owned snapshot.
    pub fn get_formatted_output_string(&mut self) -> String {
        match self.prepare_frame_output() {
            FrameOutput::Contiguous(output) => output,
            FrameOutput::CachedRows => {
                let mut out = std::mem::take(&mut self.output_buffer).into_bytes();
                out.clear();
                for (i, row) in self.rendered_rows.iter().rev().enumerate() {
                    if i != 0 {
                        out.push(b'\n');
                    }
                    out.extend_from_slice(row);
                }
                // SAFETY: rows contain only complete UTF-8 symbols.
                unsafe { String::from_utf8_unchecked(out) }
            }
        }
    }

    pub(crate) fn frame_bytes(&self, frame: &FrameOutput) -> usize {
        match frame {
            FrameOutput::Contiguous(output) => output.len(),
            FrameOutput::CachedRows => {
                self.rendered_rows.iter().map(|row| row.len()).sum::<usize>()
                    + self.rendered_rows.len().saturating_sub(1)
            }
        }
    }

    pub(crate) fn write_frame_data(&self, out: &mut impl Write, frame: &FrameOutput) -> std::io::Result<()> {
        match frame {
            FrameOutput::Contiguous(output) => out.write_all(output.as_bytes()),
            FrameOutput::CachedRows => {
                use std::io::IoSlice;
                // A bounded stack batch stays below platform iovec limits and
                // avoids joining/copying potentially gigabytes of repeated rows.
                let mut slices: [IoSlice<'_>; 128] = std::array::from_fn(|_| IoSlice::new(&[]));
                let mut used = 0;
                for (i, row) in self.rendered_rows.iter().rev().enumerate() {
                    if i != 0 {
                        slices[used] = IoSlice::new(b"\n");
                        used += 1;
                    }
                    slices[used] = IoSlice::new(row);
                    used += 1;
                    if used >= 126 {
                        crate::engine::render::write_all_vectored(out, &mut slices[..used])?;
                        used = 0;
                    }
                }
                crate::engine::render::write_all_vectored(out, &mut slices[..used])
            }
        }
    }

    /// Whether a resize has landed, settled, and actually moved something.
    ///
    /// Settled: dragging a window edge emits a SIGWINCH per step, and rebuilding
    /// for each one pins the animation at its opening frames for the whole drag.
    /// Each signal restarts a quiet window; the old canvas keeps animating until
    /// it expires, so the wait costs nothing on screen.
    ///
    /// Moved something: a new terminal size is not enough. With an input-sized
    /// canvas and no anchor offsets most resizes leave every rendered cell
    /// exactly where it was, and restarting for those is pure loss. Explicitly
    /// ignored dimensions are fixed by definition.
    pub fn resize_settled(&mut self) -> bool {
        resize_settled(&mut self.resize_seen_at, &self.config, &self.input_line_lengths, self.terminal_dimensions)
    }

    /// After a resize: go back to the top of the area this run allocated, wipe
    /// it, and leave the cursor there so the rebuilt canvas takes the same rows
    /// instead of scrolling a second one into the terminal.
    pub fn reset_canvas_area(&self, out: &mut impl Write) -> std::io::Result<()> {
        out.write_all(ansi::DEC_RESTORE_CURSOR.as_bytes())?;
        if self.visible_top > 0 {
            out.write_all(ansi::move_cursor_up(self.visible_top as usize).as_bytes())?;
        }
        out.write_all(ansi::CLEAR_TO_END_OF_SCREEN.as_bytes())?;
        Ok(())
    }

    // --- tty side (upstream's second Terminal instance) ---

    pub fn prep_canvas(&mut self, out: &mut impl Write) -> std::io::Result<()> {
        out.write_all(ansi::HIDE_CURSOR.as_bytes())?;
        if self.config.reuse_canvas {
            self.write_move_cursor_to_top(out)?;
        }
        for _ in 0..self.visible_top {
            let blank = " ".repeat(self.visible_right.max(0) as usize);
            out.write_all(blank.as_bytes())?;
            out.write_all(b"\n")?;
        }
        out.write_all(ansi::DEC_SAVE_CURSOR.as_bytes())?;
        Ok(())
    }

    pub fn restore_cursor(&self, out: &mut impl Write, end_symbol: &str) -> std::io::Result<()> {
        let end_symbol = if self.config.no_eol { "" } else { end_symbol };
        if !self.config.no_restore_cursor {
            out.write_all(ansi::SHOW_CURSOR.as_bytes())?;
        }
        out.write_all(end_symbol.as_bytes())?;
        Ok(())
    }

    pub fn print_frame(&mut self, out: &mut impl Write, frame: &FrameOutput) -> std::io::Result<()> {
        self.write_move_cursor_to_top(out)?;
        self.write_frame_data(out, frame)?;
        out.flush()
    }

    fn write_move_cursor_to_top(&self, out: &mut impl Write) -> std::io::Result<()> {
        out.write_all(self.move_cursor_to_top.as_bytes())
    }

    /// Terminal.enforce_framerate: sleep off the remainder; timestamp taken
    /// AFTER the sleep (drift accumulates, faithfully).
    pub fn enforce_framerate(&mut self) {
        if self.frame_rate == 0 {
            return;
        }
        let frame_delay = 1.0 / self.frame_rate as f64;
        let elapsed = self.last_time_printed.elapsed().as_secs_f64();
        if elapsed < frame_delay {
            std::thread::sleep(std::time::Duration::from_secs_f64(frame_delay - elapsed));
        }
        self.last_time_printed = Instant::now();
    }
}

/// [`Terminal::resize_settled`] for a run described only by its settings,
/// input line lengths and starting dimensions - what the assembly engine's
/// runs are, as far as Rust can see them.
pub fn resize_settled(
    seen_at: &mut Option<Instant>,
    config: &TerminalConfig,
    line_lengths: &[i64],
    dimensions: (i64, i64),
) -> bool {
    const QUIET: std::time::Duration = std::time::Duration::from_millis(50);

    if crate::take_terminal_resize() {
        *seen_at = Some(Instant::now());
    }
    match *seen_at {
        Some(seen) if seen.elapsed() >= QUIET => *seen_at = None,
        _ => return false,
    }
    if config.ignore_terminal_dimensions {
        return false;
    }
    let (width, height) = get_terminal_dimensions();
    if (width, height) == dimensions {
        return false;
    }
    compute_layout(config, line_lengths, width, height)
        != compute_layout(config, line_lengths, dimensions.0, dimensions.1)
}

/// shutil.get_terminal_size semantics: COLUMNS/LINES env vars win; else query
/// the tty; on failure (80, 24).
pub fn get_terminal_dimensions() -> (i64, i64) {
    let env_dim = |name: &str| -> Option<i64> { std::env::var(name).ok()?.parse::<i64>().ok() };
    let columns = env_dim("COLUMNS");
    let lines = env_dim("LINES");
    if let (Some(c), Some(l)) = (columns, lines) {
        return (c, l);
    }
    match terminal_size::terminal_size() {
        Some((terminal_size::Width(w), terminal_size::Height(h))) => {
            (columns.unwrap_or(w as i64), lines.unwrap_or(h as i64))
        }
        None => (columns.unwrap_or(80), lines.unwrap_or(24)),
    }
}

/// Everything about the drawing area that is derived from the terminal size.
/// A resize only matters if recomputing this yields something different, so it
/// is factored out of Terminal::new rather than inlined there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Layout {
    canvas_height: i64,
    canvas_width: i64,
    column_offset: i64,
    row_offset: i64,
    visible_top: i64,
    visible_bottom: i64,
    visible_right: i64,
    visible_left: i64,
}

fn compute_layout(config: &TerminalConfig, line_lengths: &[i64], terminal_width: i64, terminal_height: i64) -> Layout {
    let (canvas_height, canvas_width) = get_canvas_dimensions(config, line_lengths, terminal_width, terminal_height);
    let canvas = Canvas::new(canvas_height, canvas_width);
    let (mut width, mut height) = (terminal_width, terminal_height);
    let (column_offset, row_offset) = if !config.ignore_terminal_dimensions {
        calc_canvas_offsets(config, &canvas, width, height)
    } else {
        width = canvas.right;
        height = canvas.top;
        (0, 0)
    };
    Layout {
        canvas_height,
        canvas_width,
        column_offset,
        row_offset,
        visible_top: std::cmp::min(canvas.top + row_offset, height),
        visible_bottom: std::cmp::max(canvas.bottom + row_offset, 1),
        visible_right: std::cmp::min(canvas.right + column_offset, width),
        visible_left: std::cmp::max(canvas.left + column_offset, 1),
    }
}

/// Terminal._get_canvas_dimensions -> (height, width).
fn get_canvas_dimensions(
    config: &TerminalConfig,
    line_lengths: &[i64],
    terminal_width: i64,
    terminal_height: i64,
) -> (i64, i64) {
    let canvas_width = if config.canvas_width > 0 {
        config.canvas_width
    } else if config.canvas_width == 0 {
        terminal_width
    } else {
        let input_width = line_lengths.iter().copied().max().unwrap_or(0);
        if config.ignore_terminal_dimensions {
            input_width
        } else {
            std::cmp::min(terminal_width, input_width)
        }
    };
    let canvas_height = if config.canvas_height > 0 {
        config.canvas_height
    } else if config.canvas_height == 0 {
        terminal_height
    } else {
        let input_height = line_lengths.len() as i64;
        if config.ignore_terminal_dimensions {
            input_height
        } else if config.wrap_text {
            std::cmp::min(wrapped_line_count(line_lengths, canvas_width), terminal_height)
        } else {
            std::cmp::min(terminal_height, input_height)
        }
    };
    (canvas_height, canvas_width)
}

fn wrapped_line_count(line_lengths: &[i64], width: i64) -> i64 {
    let mut count: i64 = 0;
    for &length in line_lengths {
        let mut remaining = length;
        while remaining > width {
            count += 1;
            remaining -= width;
        }
        count += 1;
    }
    count
}

/// Terminal._wrap_lines.
fn wrap_lines(lines: Vec<Vec<CharId>>, width: i64) -> Vec<Vec<CharId>> {
    let mut wrapped: Vec<Vec<CharId>> = Vec::new();
    for line in lines {
        let mut current = line;
        while current.len() as i64 > width {
            let rest = current.split_off(width as usize);
            wrapped.push(current);
            current = rest;
        }
        wrapped.push(current);
    }
    wrapped
}

fn calc_canvas_offsets(
    config: &TerminalConfig,
    canvas: &Canvas,
    terminal_width: i64,
    terminal_height: i64,
) -> (i64, i64) {
    use crate::engine::canvas::Anchor::*;
    use crate::utils::pycompat::floor_div;
    let mut column_offset = 0;
    let mut row_offset = 0;
    match config.anchor_canvas {
        S | N | C => column_offset = floor_div(terminal_width, 2) - floor_div(canvas.width, 2),
        Se | E | Ne => column_offset = terminal_width - canvas.width,
        _ => {}
    }
    match config.anchor_canvas {
        W | E | C => row_offset = floor_div(terminal_height, 2) - floor_div(canvas.height, 2),
        Nw | N | Ne => row_offset = terminal_height - canvas.height,
        _ => {}
    }
    (column_offset, row_offset)
}

/// Terminal._setup_input_characters: wrap, assign 1-based bottom-up coords,
/// drop plain spaces (they become fill), anchor, and keep in-canvas chars.
fn setup_input_characters(
    config: &TerminalConfig,
    canvas: &mut Canvas,
    arena: &mut Vec<EffectCharacter>,
    preprocessed_lines: Vec<Vec<CharId>>,
) -> Result<Vec<CharId>, EngineError> {
    let formatted_lines =
        if config.wrap_text { wrap_lines(preprocessed_lines, canvas.right) } else { preprocessed_lines };
    let input_height = formatted_lines.len() as i64;
    let mut input_characters: Vec<CharId> = Vec::new();
    for (row, line) in formatted_lines.iter().enumerate() {
        for (column0, &id) in line.iter().enumerate() {
            let column = column0 as i64 + 1;
            let ch = &mut arena[id.0 as usize];
            ch.input_coord = Coord::new(column, input_height - row as i64);
            if ch.input_symbol != " " || ch.animation.input_fg_color.is_some() || ch.animation.input_bg_color.is_some()
            {
                input_characters.push(id);
            }
        }
    }
    canvas.anchor_text(arena, input_characters, config.anchor_text).map_err(EngineError::Other)
}

#[cfg(test)]
mod dense_output_cache_tests {
    use super::*;

    #[test]
    fn coordinate_index_and_clipped_ellipses_follow_map_mutations() {
        let entries: HashMap<_, _> = (-3..=9)
            .flat_map(|x| (-5..=6).map(move |y| (Coord::new(x, y), CharId(((x + 3) * 12 + y + 5) as u32))))
            .collect();
        let mut map = InputCoordinateMap::from(entries);
        for round in 0..6 {
            for x in -7..=14 {
                for y in -8..=10 {
                    let c = Coord::new(x, y);
                    assert_eq!(map.lookup(c), map.entries.get(&c).copied());
                }
            }
            for center in [Coord::new(-5, -8), Coord::new(0, 0), Coord::new(8, 9)] {
                for diameter in 0..30 {
                    let expected: Vec<_> = crate::utils::geometry::coords_in_circle(center, diameter)
                        .filter_map(|c| map.entries.get(&c).copied())
                        .collect();
                    let mut actual = Vec::new();
                    map.for_each_in_circle(center, diameter, |id| actual.push(id));
                    assert_eq!(actual, expected);
                }
            }
            match round {
                0 => {
                    map.remove(&Coord::new(0, 0));
                }
                1 => {
                    map.insert(Coord::new(10, 7), CharId(u32::MAX));
                }
                2 => {
                    for (_, id) in &mut map {
                        id.0 = id.0.wrapping_add(1);
                    }
                }
                3 => {
                    let mut copy = map.clone();
                    copy.clear();
                    assert_eq!(copy.lookup(Coord::new(1, 1)), None);
                }
                4 => {
                    map = HashMap::from([(Coord::new(2, 2), CharId(19))]).into();
                }
                _ => {
                    map.clear();
                }
            }
        }
        for entries in [
            HashMap::from([(Coord::new(i64::MIN, 0), CharId(1)), (Coord::new(i64::MAX, 0), CharId(2))]),
            HashMap::from([(Coord::new(0, 0), CharId(1)), (Coord::new(100000, 100000), CharId(2))]),
        ] {
            let map = InputCoordinateMap::from(entries);
            for (&coord, &id) in &map {
                assert_eq!(map.lookup(coord), Some(id));
            }
            assert!(map.dense.get().unwrap().is_none());
        }
    }

    #[test]
    fn recycled_dense_frames_track_consumed_mutations_and_geometry() {
        let mut terminal = Terminal::new(
            &"A".repeat(600),
            TerminalConfig {
                canvas_width: 600,
                canvas_height: 1,
                ignore_terminal_dimensions: true,
                ..Default::default()
            },
        )
        .unwrap();
        for id in terminal.input_characters.clone() {
            terminal.set_character_visibility(id, true);
        }
        for iteration in 0..8 {
            if iteration == 3 {
                // Consume mutation marks through the public snapshot API before
                // preparing output. A cached dense string must still change.
                for ch in terminal.arena.iter_mut() {
                    ch.animation.set_appearance("A", false, Some("B"), None);
                }
                terminal.update_terminal_state();
            }
            if iteration == 6 {
                terminal.visible_right -= 1;
            }
            let frame = terminal.prepare_frame_output();
            let mut bytes = Vec::new();
            terminal.write_frame_data(&mut bytes, &frame).unwrap();
            assert_eq!(bytes, vec![if iteration < 3 { b'A' } else { b'B' }; if iteration < 6 { 600 } else { 599 }]);
            terminal.recycle_frame(frame);
        }
    }
}
