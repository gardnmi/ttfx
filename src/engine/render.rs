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

/// Incremental cell membership, stored in parallel arrays. Moving characters
/// unlink/relink in O(1). Only touched cells need their painter winner recomputed.
#[derive(Default)]
pub(crate) struct CellGrid {
    pub(crate) dense: bool,
    pub(crate) revision: u64,
    last_geometry: Option<[i64; 6]>,
    pub(crate) dirty_rows: Vec<bool>,
    geometry: Option<[i64; 6]>,
    heads: Vec<u32>,
    cells: Vec<usize>,
    next: Vec<u32>,
    prev: Vec<u32>,
    layers: Vec<i64>,
    character_ids: Vec<u32>,
    visuals: Vec<u64>,
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
        // A held frame can leave every cell unchanged. Keep the current mode
        // and winners instead of rebuilding sparse membership after a dense
        // frame. Revision also covers API calls that consume arena dirtiness.
        if self.last_geometry == Some(geometry) && arena.dirty_len() == 0 {
            return (width, height);
        }
        self.last_geometry = Some(geometry);
        self.revision = self.revision.checked_add(1).expect("render revision exhausted");
        // Like memchr's ineffective-prefilter fallback, avoid bookkeeping when
        // most characters are changing. Invalidate membership so switching back
        // to sparse updates will rebuild a correct grid once.
        self.dense = arena.dirty_len() > 256 && arena.dirty_len() > arena.len() / 3;
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
            self.cells.clear();
            self.next.clear();
            self.prev.clear();
            self.layers.clear();
            self.character_ids.clear();
            self.visuals.clear();
            self.dirty.clear();
            self.marked.clear();
            self.marked.resize(count, false);
            winners.clear();
            winners.resize(count, NONE);
            visual_ids.clear();
            visual_ids.resize(count, 0);
            arena.mark_all();
        }
        self.cells.resize(arena.len(), OFFSCREEN);
        self.next.resize(arena.len(), NONE);
        self.prev.resize(arena.len(), NONE);
        self.layers.resize(arena.len(), 0);
        self.character_ids.resize(arena.len(), 0);
        self.visuals.resize(arena.len(), 0);
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
            let old = self.cells[id];
            let visual = ch.animation.current_character_visual.formatted_symbol.id();
            if old != cell {
                if old != OFFSCREEN {
                    let prev = self.prev[id];
                    let next = self.next[id];
                    if prev == NONE {
                        self.heads[old] = next;
                    } else {
                        self.next[prev as usize] = next;
                    }
                    if next != NONE {
                        self.prev[next as usize] = prev;
                    }
                    self.mark(old);
                }
                if cell != OFFSCREEN {
                    let head = self.heads[cell];
                    self.next[id] = head;
                    self.prev[id] = NONE;
                    if head != NONE {
                        self.prev[head as usize] = id as u32;
                    }
                    self.heads[cell] = id as u32;
                    self.mark(cell);
                }
                self.cells[id] = cell;
            } else if cell != OFFSCREEN
                && (self.visuals[id] != visual
                    || self.layers[id] != ch.layer
                    || self.character_ids[id] != ch.character_id)
            {
                self.mark(cell);
            }
            self.visuals[id] = visual;
            self.layers[id] = ch.layer;
            self.character_ids[id] = ch.character_id;
        }
        arena.recycle_dirty(changed);
        for &cell in &self.dirty {
            let mut candidate = self.heads[cell];
            let mut winner = NONE;
            while candidate != NONE {
                let index = candidate as usize;
                if winner == NONE
                    || (self.layers[index], self.character_ids[index])
                        > (self.layers[winner as usize], self.character_ids[winner as usize])
                {
                    winner = candidate;
                }
                candidate = self.next[index];
            }
            winners[cell] = winner;
            let visual = if winner == NONE { 0 } else { self.visuals[winner as usize] };
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
