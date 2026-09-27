//! Bulk comparison of immutable visual IDs. Select the CPU path once per terminal,
//! just as memchr amortizes feature detection across searches. Short rows and
//! non-x86 machines retain the standard slice comparison.

pub(crate) fn select_row_comparison() -> fn(&[u64], &[u64]) -> bool {
    if std::env::var_os("TTFX_SIMD").is_some_and(|value| value == "0") {
        return equal_scalar;
    }
    #[cfg(target_arch = "x86_64")]
    if std::is_x86_feature_detected!("avx2") {
        return equal_avx2;
    }
    equal_scalar
}

fn equal_scalar(a: &[u64], b: &[u64]) -> bool {
    a == b
}

#[cfg(target_arch = "x86_64")]
fn equal_avx2(a: &[u64], b: &[u64]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    if a.len() < 16 {
        return equal_scalar(a, b);
    }
    // SAFETY: this function is selected only after runtime AVX2 detection.
    unsafe { equal_avx2_inner(a, b) }
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn equal_avx2_inner(a: &[u64], b: &[u64]) -> bool {
    use std::arch::x86_64::*;
    let mut offset = 0;
    while offset + 4 <= a.len() {
        // SAFETY: equal-length slices; each load stays inside four live u64s.
        // Unaligned loads permit arbitrary row widths and slice offsets.
        let left = unsafe { _mm256_loadu_si256(a.as_ptr().add(offset).cast()) };
        let right = unsafe { _mm256_loadu_si256(b.as_ptr().add(offset).cast()) };
        if _mm256_movemask_epi8(_mm256_cmpeq_epi64(left, right)) != -1 {
            return false;
        }
        offset += 4;
    }
    a[offset..] == b[offset..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comparison_matches_slices_at_every_tail_alignment_and_mismatch() {
        let compare = select_row_comparison();
        for offset in 0..8 {
            for len in 0..260 {
                let a = vec![42; len + offset];
                let mut b = a.clone();
                assert!(compare(&a[offset..], &b[offset..]));
                for index in offset..b.len() {
                    b[index] ^= 1 << (index % 64);
                    assert_eq!(compare(&a[offset..], &b[offset..]), equal_scalar(&a[offset..], &b[offset..]));
                    b[index] = 42;
                }
            }
        }
        assert!(!compare(&[1], &[]));
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
