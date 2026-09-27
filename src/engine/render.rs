//! Find changed blocks of immutable visual IDs. Select the CPU path once per
//! terminal. Short rows and non-x86 machines use ordinary slice comparisons;
//! rows wider than the bounded block mask use complete row serialization.

pub(crate) fn select_row_comparison() -> fn(&[u64], &[u64]) -> u64 {
    if std::env::var_os("TTFX_SIMD").is_some_and(|value| value == "0") {
        return changed_scalar;
    }
    #[cfg(target_arch = "x86_64")]
    if std::is_x86_feature_detected!("avx2") {
        return changed_avx2;
    }
    changed_scalar
}

// One bit per eight cells. Oversized rows use the ordinary full-row path;
// u64::MAX is also a conservative full-rebuild result for unequal slice sizes.
fn changed_scalar(a: &[u64], b: &[u64]) -> u64 {
    if a.len() != b.len() || a.len() > ROW_BLOCK * 64 {
        return if a == b { 0 } else { u64::MAX };
    }
    a.chunks(ROW_BLOCK)
        .zip(b.chunks(ROW_BLOCK))
        .enumerate()
        .fold(0, |mask, (block, (a, b))| mask | (u64::from(a != b) << block))
}

#[cfg(target_arch = "x86_64")]
fn changed_avx2(a: &[u64], b: &[u64]) -> u64 {
    if a.len() != b.len() || a.len() < ROW_BLOCK || a.len() > ROW_BLOCK * 64 {
        return changed_scalar(a, b);
    }
    // SAFETY: this function is selected only after runtime AVX2 detection.
    unsafe { changed_avx2_inner(a, b) }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn changed_avx2_inner(a: &[u64], b: &[u64]) -> u64 {
    use std::arch::x86_64::*;
    let mut offset = 0;
    let mut block = 0;
    let mut mask = 0;
    while offset + ROW_BLOCK <= a.len() {
        let mut different = _mm256_setzero_si256();
        for part in (0..ROW_BLOCK).step_by(4) {
            // SAFETY: the caller checked equal lengths. The complete block
            // contains each four-u64 load; unaligned row starts are supported.
            let left = unsafe { _mm256_loadu_si256(a.as_ptr().add(offset + part).cast()) };
            let right = unsafe { _mm256_loadu_si256(b.as_ptr().add(offset + part).cast()) };
            different = _mm256_or_si256(different, _mm256_xor_si256(left, right));
        }
        mask |= u64::from(_mm256_testz_si256(different, different) == 0) << block;
        block += 1;
        offset += ROW_BLOCK;
    }
    if offset < a.len() {
        mask |= u64::from(a[offset..] != b[offset..]) << block;
    }
    mask
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comparison_matches_slices_at_every_tail_alignment_and_mismatch() {
        let compare = select_row_comparison();
        for offset in 0..8 {
            for len in (0..260).chain([511, 512, 513, 1023, 1024, 1025, 2048]) {
                let a = vec![42; len + offset];
                let mut b = a.clone();
                assert_eq!(compare(&a[offset..], &b[offset..]), 0);
                for index in offset..b.len() {
                    b[index] ^= 1 << (index % 64);
                    let expected = if len > ROW_BLOCK * 64 { u64::MAX } else { 1 << ((index - offset) / ROW_BLOCK) };
                    assert_eq!(compare(&a[offset..], &b[offset..]), expected);
                    assert_eq!(changed_scalar(&a[offset..], &b[offset..]), expected);
                    b[index] = 42;
                }
                for index in (offset..b.len()).step_by(13) {
                    b[index] ^= 1;
                }
                assert_eq!(compare(&a[offset..], &b[offset..]), changed_scalar(&a[offset..], &b[offset..]));
            }
        }
        assert_eq!(compare(&[1], &[]), u64::MAX);
    }
}

const NONE: u32 = u32::MAX;
const OFFSCREEN: usize = usize::MAX;

/// Store the membership fields touched together during a move together.
#[derive(Clone, Copy)]
struct CellMember {
    cell: usize,
    layer: i64,
    visual: u64,
    next: u32,
    prev: u32,
    character_id: u32,
}
impl Default for CellMember {
    fn default() -> Self {
        Self { cell: OFFSCREEN, layer: 0, visual: 0, next: NONE, prev: NONE, character_id: 0 }
    }
}

/// Incremental cell membership. Moving characters
/// unlink/relink in O(1). Only touched cells need their painter winner recomputed.
#[derive(Default)]
pub(crate) struct CellGrid {
    pub(crate) dense: bool,
    pub(crate) revision: u64,
    last_geometry: Option<[i64; 6]>,
    stable_layout: usize,
    pub(crate) dirty_rows: Vec<bool>,
    geometry: Option<[i64; 6]>,
    heads: Vec<u32>,
    members: Vec<CellMember>,
    dirty: Vec<usize>,
    marked: Vec<bool>,
}

impl CellGrid {
    pub(crate) fn update(
        &mut self,
        arena: &mut crate::engine::character::CharacterArena,
        geometry: [i64; 6],
        winners: &mut Vec<u32>,
        visual_ids: &mut Vec<u64>,
    ) -> (usize, usize) {
        let [left, right, bottom, top, column_offset, row_offset] = geometry;
        let width = right.max(0) as usize;
        let height = top.max(0) as usize;
        let count = width.checked_mul(height).expect("terminal canvas is too large");
        let layout_unchanged = self.last_geometry == Some(geometry) && !arena.layout_changed();
        self.stable_layout = if layout_unchanged { self.stable_layout.saturating_add(1) } else { 0 };
        // A held frame can leave every cell unchanged. Keep the current mode
        // and winners instead of rebuilding sparse membership after a dense
        // frame. Revision also covers API calls that consume arena dirtiness.
        if self.last_geometry == Some(geometry) && arena.dirty_len() == 0 {
            return (width, height);
        }
        self.last_geometry = Some(geometry);
        self.revision = self.revision.checked_add(1).expect("render revision exhausted");
        // An animation can change visuals while cell membership and painter
        // order stay fixed. Update only the affected winners without revisiting
        // coordinates, layers, or collision lists.
        if layout_unchanged && self.geometry == Some(geometry) {
            let changed = arena.take_dirty();
            for &id in &changed {
                let visual = arena.render_slice()[id].animation.current_character_visual.formatted_symbol.id();
                self.members[id].visual = visual;
                let cell = self.members[id].cell;
                if cell != OFFSCREEN && winners[cell] == id as u32 && visual_ids[cell] != visual {
                    visual_ids[cell] = visual;
                    self.dirty_rows[cell / width] = true;
                }
            }
            arena.recycle_dirty(changed);
            self.dense = false;
            return (width, height);
        }
        // Like memchr's ineffective-prefilter fallback, avoid bookkeeping when
        // most characters are changing. Invalidate membership so switching back
        // to sparse updates will rebuild a correct grid once.
        self.dense = self.stable_layout < 2 && arena.dirty_len() > 256 && arena.dirty_len() > arena.len() / 3;
        if self.dense {
            self.geometry = None;
            winners.resize(count, NONE);
            winners.fill(NONE);
            for (id, ch) in arena.render_slice().iter().enumerate() {
                let row = ch.motion.current_coord.row + row_offset;
                let col = ch.motion.current_coord.column + column_offset;
                if !ch.is_visible || row < bottom.max(1) || row > top || col < left.max(1) || col > right {
                    continue;
                }
                let cell = (row - 1) as usize * width + (col - 1) as usize;
                let old = winners[cell];
                if old == NONE
                    || (ch.layer, ch.character_id)
                        > (arena.render_slice()[old as usize].layer, arena.render_slice()[old as usize].character_id)
                {
                    winners[cell] = id as u32;
                }
            }
            let changed = arena.take_dirty();
            arena.recycle_dirty(changed);
            return (width, height);
        }
        if self.geometry != Some(geometry) {
            self.geometry = Some(geometry);
            self.dirty_rows.clear();
            self.dirty_rows.resize(height, true);
            self.heads.clear();
            self.heads.resize(count, NONE);
            self.members.clear();
            self.dirty.clear();
            self.marked.clear();
            self.marked.resize(count, false);
            winners.clear();
            winners.resize(count, NONE);
            visual_ids.clear();
            visual_ids.resize(count, 0);
            arena.mark_all();
        }
        self.members.resize(arena.len(), CellMember::default());
        let changed = arena.take_dirty();
        for &id in &changed {
            let ch = &arena.render_slice()[id];
            let row = ch.motion.current_coord.row + row_offset;
            let col = ch.motion.current_coord.column + column_offset;
            let cell = if ch.is_visible && row >= bottom.max(1) && row <= top && col >= left.max(1) && col <= right {
                (row - 1) as usize * width + (col - 1) as usize
            } else {
                OFFSCREEN
            };
            let old = self.members[id].cell;
            let visual = ch.animation.current_character_visual.formatted_symbol.id();
            if old != cell {
                if old != OFFSCREEN {
                    let prev = self.members[id].prev;
                    let next = self.members[id].next;
                    if prev == NONE {
                        self.heads[old] = next;
                    } else {
                        self.members[prev as usize].next = next;
                    }
                    if next != NONE {
                        self.members[next as usize].prev = prev;
                    }
                    if winners[old] == id as u32 {
                        self.mark(old);
                    }
                }
                if cell != OFFSCREEN {
                    let head = self.heads[cell];
                    self.members[id].next = head;
                    self.members[id].prev = NONE;
                    if head != NONE {
                        self.members[head as usize].prev = id as u32;
                    }
                    self.heads[cell] = id as u32;
                }
                self.members[id].cell = cell;
            } else if cell != OFFSCREEN
                && (self.members[id].layer != ch.layer || self.members[id].character_id != ch.character_id)
            {
                self.mark(cell);
            }
            self.members[id].visual = visual;
            self.members[id].layer = ch.layer;
            self.members[id].character_id = ch.character_id;
            if cell != OFFSCREEN && !self.marked[cell] {
                let winner = winners[cell];
                // A newly linked member is at the head, so it wins exact
                // painter ties just as the membership scan does. Removing a
                // non-winner or changing only its visual needs no scan.
                if old != cell
                    && (winner == NONE
                        || (ch.layer, ch.character_id)
                            >= (self.members[winner as usize].layer, self.members[winner as usize].character_id))
                {
                    winners[cell] = id as u32;
                }
                if winners[cell] == id as u32 && visual_ids[cell] != visual {
                    visual_ids[cell] = visual;
                    self.dirty_rows[cell / width] = true;
                }
            }
        }
        arena.recycle_dirty(changed);
        for &cell in &self.dirty {
            let mut candidate = self.heads[cell];
            let mut winner = NONE;
            while candidate != NONE {
                let index = candidate as usize;
                if winner == NONE
                    || (self.members[index].layer, self.members[index].character_id)
                        > (self.members[winner as usize].layer, self.members[winner as usize].character_id)
                {
                    winner = candidate;
                }
                candidate = self.members[index].next;
            }
            winners[cell] = winner;
            let visual = if winner == NONE { 0 } else { self.members[winner as usize].visual };
            if visual_ids[cell] != visual {
                self.dirty_rows[cell / width] = true;
            }
            visual_ids[cell] = visual;
            self.marked[cell] = false;
        }
        self.dirty.clear();
        (width, height)
    }

    fn mark(&mut self, cell: usize) {
        if !self.marked[cell] {
            self.marked[cell] = true;
            self.dirty.push(cell);
        }
    }
}

const ROW_BLOCK: usize = 8;

/// Cached UTF-8 bytes with eight-cell boundaries. Sparse changed runs are
/// replaced right-to-left, preserving earlier offsets and unchanged bytes.
/// Fully changing rows keep the sequential serialization path.
#[derive(Default)]
pub(crate) struct CachedRow {
    bytes: Vec<u8>,
    offsets: Vec<usize>,
    #[cfg(test)]
    partial_updates: usize,
}

impl std::ops::Deref for CachedRow {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        &self.bytes
    }
}

impl CachedRow {
    pub(crate) fn rebuild(
        &mut self,
        cells: &[u32],
        changes: u64,
        characters: &[crate::engine::character::EffectCharacter],
        scratch: &mut Vec<u8>,
    ) {
        let blocks = cells.len().div_ceil(ROW_BLOCK);
        // Closing a one-block gap avoids shifting the same suffix twice for
        // nearby edits. Distant patches run right-to-left, preserving offsets.
        let mut changes = changes | ((changes << 1) & (changes >> 1));
        if self.offsets.len() != blocks + 1
            || changes == u64::MAX
            || changes.count_ones() as usize > blocks / 2
            || cells.len() < ROW_BLOCK
        {
            self.bytes.clear();
            self.offsets.resize(blocks + 1, 0);
            for (block, cells) in cells.chunks(ROW_BLOCK).enumerate() {
                self.offsets[block] = self.bytes.len();
                for &cell in cells {
                    append_cell(&mut self.bytes, cell, characters);
                }
            }
            *self.offsets.last_mut().unwrap() = self.bytes.len();
            return;
        }
        while changes != 0 {
            let end = 64 - changes.leading_zeros() as usize;
            let mut first = end - 1;
            while first > 0 && changes & (1 << (first - 1)) != 0 {
                first -= 1;
            }
            let run = (u64::MAX >> (64 - (end - first))) << first;
            changes &= !run;
            self.replace_span(cells, first * ROW_BLOCK, (end * ROW_BLOCK).min(cells.len()), characters, scratch);
        }
    }

    fn replace_span(
        &mut self,
        cells: &[u32],
        first: usize,
        end: usize,
        characters: &[crate::engine::character::EffectCharacter],
        scratch: &mut Vec<u8>,
    ) {
        let first_block = first / ROW_BLOCK;
        let end_block = end.div_ceil(ROW_BLOCK);
        let start_byte = self.offsets[first_block];
        #[cfg(test)]
        {
            self.partial_updates += 1;
        }
        let old_end = self.offsets[end_block];
        scratch.clear();
        for (block, cells) in cells[first..end].chunks(ROW_BLOCK).enumerate() {
            self.offsets[first_block + block] = start_byte + scratch.len();
            for &cell in cells {
                append_cell(scratch, cell, characters);
            }
        }
        let new_end = start_byte + scratch.len();
        self.offsets[end_block] = new_end;
        let old_len = self.bytes.len();
        if new_end > old_end {
            let growth = new_end - old_end;
            self.bytes.resize(old_len + growth, 0);
            self.bytes.copy_within(old_end..old_len, new_end);
            for offset in &mut self.offsets[end_block + 1..] {
                *offset += growth;
            }
        } else if new_end < old_end {
            let shrink = old_end - new_end;
            self.bytes.copy_within(old_end..old_len, new_end);
            self.bytes.truncate(old_len - shrink);
            for offset in &mut self.offsets[end_block + 1..] {
                *offset -= shrink;
            }
        }
        self.bytes[start_byte..new_end].copy_from_slice(scratch);
    }
}

#[inline]
fn append_cell(out: &mut Vec<u8>, cell: u32, characters: &[crate::engine::character::EffectCharacter]) {
    if cell == NONE {
        out.push(b' ');
    } else {
        characters[cell as usize].animation.current_character_visual.formatted_symbol.append_to(out);
    }
}

#[cfg(test)]
mod cached_row_tests {
    use super::*;
    use crate::engine::character::EffectCharacter;

    #[test]
    fn partial_rows_match_fresh_utf8_output_through_growth_shrink_and_resize() {
        let symbols = ["", "A", "λ", "界", "🙂", "\x1b[38;2;255;1;32mZ\x1b[0m", &"🦀".repeat(50)];
        let characters: Vec<_> =
            symbols.iter().enumerate().map(|(id, symbol)| EffectCharacter::new(id as u32, symbol, 1, 1)).collect();
        let mut row = CachedRow::default();
        let mut scratch = Vec::new();
        let mut state = 125u64;
        for width in [0, 1, 15, 16, 65, 200, 31, 511, 512, 513, 1023, 1024, 1025, 2048, 0] {
            let mut cells = vec![NONE; width];
            let mut previous = vec![u64::MAX; width];
            for step in 0..400 {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                if width != 0 {
                    let col = step % width;
                    cells[col] = if step % 8 == 0 { NONE } else { (step % symbols.len()) as u32 };
                    if step % 31 == 0 {
                        for (col, cell) in cells.iter_mut().enumerate() {
                            *cell = ((state as usize + col) % symbols.len()) as u32;
                        }
                    }
                }
                let ids: Vec<_> = cells.iter().map(|&cell| if cell == NONE { 0 } else { cell as u64 + 1 }).collect();
                row.rebuild(&cells, changed_scalar(&ids, &previous), &characters, &mut scratch);
                let mut expected = Vec::new();
                for (col, &cell) in cells.iter().enumerate() {
                    if col % ROW_BLOCK == 0 {
                        assert_eq!(
                            row.offsets[col / ROW_BLOCK],
                            expected.len(),
                            "width {width}, step {step}, column {col}"
                        );
                    }
                    let text = if cell == NONE {
                        " "
                    } else {
                        characters[cell as usize].animation.current_character_visual.formatted_symbol.as_str()
                    };
                    expected.extend_from_slice(text.as_bytes());
                }
                assert_eq!(row.bytes, expected, "width {width}, step {step}");
                assert_eq!(row.offsets.last(), Some(&expected.len()));
                previous = ids;
            }
        }
        assert!(row.partial_updates > 500, "test must exercise actual in-place replacements");
    }
}

pub(crate) fn write_all_vectored(
    out: &mut impl std::io::Write,
    mut slices: &mut [std::io::IoSlice<'_>],
) -> std::io::Result<()> {
    while !slices.is_empty() {
        // Empty rows can occur on zero-width canvases. Do not treat an empty
        // leading batch as a failed write.
        while slices.first().is_some_and(|slice| slice.is_empty()) {
            slices = &mut slices[1..];
        }
        if slices.is_empty() {
            break;
        }
        match out.write_vectored(slices) {
            Ok(0) => return Err(std::io::ErrorKind::WriteZero.into()),
            Ok(written) => std::io::IoSlice::advance_slices(&mut slices, written),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}
