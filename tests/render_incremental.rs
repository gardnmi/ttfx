use std::rc::Rc;
use ttfx::engine::animation::CharacterVisual;
use ttfx::engine::terminal::{Terminal, TerminalConfig};
use ttfx::utils::geometry::Coord;

fn reference(t: &Terminal) -> String {
    let width = t.visible_right.max(0) as usize;
    let height = t.visible_top.max(0) as usize;
    let mut cells = vec![None; width * height];
    for ch in t.arena.iter().filter(|ch| ch.is_visible) {
        let row = ch.motion.current_coord.row + t.canvas_row_offset;
        let col = ch.motion.current_coord.column + t.canvas_column_offset;
        if row < t.visible_bottom.max(1) || row > t.visible_top || col < t.visible_left.max(1) || col > t.visible_right
        {
            continue;
        }
        let cell: &mut Option<&ttfx::engine::character::EffectCharacter> =
            &mut cells[(row as usize - 1) * width + col as usize - 1];
        if cell.is_none_or(|old| (ch.layer, ch.character_id) > (old.layer, old.character_id)) {
            *cell = Some(ch);
        }
    }
    (0..height)
        .rev()
        .map(|row| {
            cells[row * width..(row + 1) * width]
                .iter()
                .map(|cell| cell.map_or(" ", |ch| ch.animation.current_character_visual.formatted_symbol.as_str()))
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn cached_frames_match_full_repaint_through_mutation_overlap_and_geometry_changes() {
    let mut t = Terminal::new(
        "AB\nCD",
        TerminalConfig { canvas_width: 17, canvas_height: 7, ignore_terminal_dimensions: true, ..Default::default() },
    )
    .unwrap();
    for i in 0..32 {
        t.add_character("λ", Coord::new(i % 17 + 1, i % 7 + 1));
    }
    let mut random = 913u64;
    for frame in 0..1600 {
        random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
        let id = (random >> 32) as usize % t.arena.len();
        match frame % 8 {
            0 => {
                let visible = !t.arena[id].is_visible;
                t.set_character_visibility(ttfx::engine::character::CharId(id as u32), visible);
            }
            1 => {
                t.arena[id].motion.current_coord = Coord::new((random % 23) as i64 - 2, ((random >> 8) % 11) as i64 - 2)
            }
            2 => t.arena[id].layer = (random % 11) as i64 - 5,
            3 => t.arena[id].animation.set_appearance(
                "x",
                false,
                Some(if random & 1 == 0 { "界" } else { "long symbol" }),
                None,
            ),
            4 => {
                let vis = CharacterVisual::plain("new");
                Rc::make_mut(&mut t.arena[id].animation.current_character_visual).formatted_symbol =
                    vis.formatted_symbol;
            }
            5 => t.canvas_column_offset = (random % 3) as i64 - 1,
            6 => {
                for ch in t.arena.iter_mut() {
                    ch.layer = (ch.layer + 1) % 7;
                }
            }
            _ => {
                t.visible_right = (random % 21) as i64;
                t.visible_top = ((random >> 8) % 9) as i64;
            }
        }
        let expected = reference(&t);
        assert_eq!(t.get_formatted_output_string(), expected, "frame {frame}");
        if frame % 7 == 0 {
            t.update_terminal_state();
        }
        assert_eq!(t.get_formatted_output_string(), expected, "unchanged frame {frame}");
    }
}

#[test]
fn switching_between_dense_and_sparse_updates_keeps_cached_rows_current() {
    let input = ("ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcd\n").repeat(15);
    let mut t = Terminal::new(
        &input,
        TerminalConfig { canvas_width: 40, canvas_height: 15, ignore_terminal_dimensions: true, ..Default::default() },
    )
    .unwrap();
    for id in t.input_characters.clone() {
        t.set_character_visibility(id, true);
    }
    for frame in 0..80 {
        if frame % 4 == 0 {
            for ch in t.arena.iter_mut() {
                ch.motion.current_coord.column = ch.motion.current_coord.column % 40 + 1;
            }
        } else {
            let id = frame % t.arena.len();
            t.arena[id].animation.set_appearance("x", false, Some("λ"), None);
        }
        if frame % 3 == 0 {
            t.update_terminal_state();
        }
        assert_eq!(t.get_formatted_output_string(), reference(&t));
        assert_eq!(t.get_formatted_output_string(), reference(&t));
    }
}

#[test]
fn vectored_frame_output_handles_interrupts_and_partial_writes() {
    use std::io::{self, IoSlice, Write};
    use ttfx::engine::terminal::FrameOutput;
    struct ShortWriter {
        bytes: Vec<u8>,
        interrupted: bool,
        calls: usize,
    }
    impl Write for ShortWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let n = bytes.len().min(3);
            self.bytes.extend_from_slice(&bytes[..n]);
            Ok(n)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
        fn write_vectored(&mut self, buffers: &[IoSlice<'_>]) -> io::Result<usize> {
            self.calls += 1;
            if !self.interrupted {
                self.interrupted = true;
                return Err(io::ErrorKind::Interrupted.into());
            }
            let mut left = 5;
            for buffer in buffers {
                let n = buffer.len().min(left);
                self.bytes.extend_from_slice(&buffer[..n]);
                left -= n;
                if left == 0 {
                    break;
                }
            }
            Ok(5 - left)
        }
    }
    let mut t = Terminal::new(
        "A界\nBC",
        TerminalConfig { canvas_width: 8, canvas_height: 70, ignore_terminal_dimensions: true, ..Default::default() },
    )
    .unwrap();
    for id in t.input_characters.clone() {
        t.set_character_visibility(id, true);
    }
    t.get_formatted_output_string(); // first dense rebuild
    t.arena[0].layer += 1; // request a sparse update to exercise vectored rows
    let snapshot = t.get_formatted_output_string();
    let mut expected = Vec::new();
    t.print_frame(&mut expected, &FrameOutput::Contiguous(snapshot)).unwrap();
    let mut writer = ShortWriter { bytes: Vec::new(), interrupted: false, calls: 0 };
    t.print_frame(&mut writer, &FrameOutput::CachedRows).unwrap();
    assert_eq!(writer.bytes, expected);
    assert!(writer.calls > 2);
}

#[test]
fn dense_repaint_clips_boundaries_and_resolves_overlaps_like_checked_reference() {
    let mut t = Terminal::new(
        "",
        TerminalConfig { canvas_width: 17, canvas_height: 7, ignore_terminal_dimensions: true, ..Default::default() },
    )
    .unwrap();
    for n in 0..600 {
        t.add_character(if n % 2 == 0 { "X" } else { "λ" }, Coord::new(1, 1));
    }
    for (right, top) in [(17, 7), (1, 1), (0, 7), (17, 0), (-1, -1), (23, 11)] {
        for offset in [-3, 0, 4] {
            t.visible_right = right;
            t.visible_top = top;
            t.visible_left = -1;
            t.visible_bottom = -2;
            t.canvas_column_offset = offset;
            t.canvas_row_offset = offset;
            // Whole-arena mutation forces a dense rebuild in every geometry.
            for (n, ch) in t.arena.iter_mut().enumerate() {
                ch.is_visible = n % 5 != 0;
                ch.layer = (n % 7) as i64 - 3;
                ch.character_id = (n / 2) as u32; // also exercise exact painter ties
                ch.motion.current_coord = Coord::new((n % 27) as i64 - 3, (n % 15) as i64 - 2);
                if offset == 0 && n % 31 == 0 {
                    ch.motion.current_coord = Coord::new(i64::MAX, i64::MIN);
                }
            }
            assert_eq!(t.get_formatted_output_string(), reference(&t), "canvas {right}x{top}, offset {offset}");
        }
    }
}

#[test]
fn stationary_playback_reuses_layout_until_geometry_or_painter_state_changes() {
    use ttfx::engine::animation::VisualParams;
    use ttfx::engine::character::CharId;
    use ttfx::engine::ctx::{Clock, EngineCtx, NoopHooks};
    use ttfx::utils::rng::Rng;
    let mut ctx = EngineCtx::new(
        &("X".repeat(40) + "\n").repeat(15),
        TerminalConfig { canvas_width: 40, canvas_height: 15, ignore_terminal_dimensions: true, ..Default::default() },
        Rng::seeded(9),
        Clock::virtual_with_frame_rate(60),
    )
    .unwrap();
    let mut hooks = NoopHooks;
    for id in ctx.terminal.input_characters.clone() {
        let animation = &mut ctx.terminal.arena[id.0 as usize].animation;
        animation.new_scene(false, None, None, "colors", false);
        let scene = animation.scenes.get_mut("colors").unwrap();
        for symbol in ["A", "λ", "long", "界"].into_iter().cycle().take(32) {
            scene.add_frame(symbol, 3, VisualParams::default()).unwrap();
        }
        ctx.activate_scene(&mut hooks, id, "colors");
        ctx.terminal.set_character_visibility(id, true);
        ctx.active_characters.insert(id);
    }
    for frame in 0..100 {
        match frame {
            10 => ctx.terminal.arena[0].motion.current_coord = ctx.terminal.arena[1].motion.current_coord,
            20 => ctx.terminal.arena[0].layer = 9,
            30 => ctx.terminal.set_character_visibility(CharId(0), false),
            40 => ctx.terminal.canvas_column_offset = 1,
            50 => ctx.terminal.visible_right = 35,
            60 => {
                let id = ctx.terminal.add_character("new", Coord::new(5, 5));
                ctx.terminal.set_character_visibility(id, true);
            }
            70 => ctx.terminal.arena[1].character_id = u32::MAX,
            _ => {}
        }
        ctx.update(&mut hooks);
        let expected = reference(&ctx.terminal);
        assert_eq!(ctx.terminal.get_formatted_output_string(), expected, "frame {frame}");
        if frame % 4 == 0 {
            ctx.terminal.update_terminal_state();
        }
        assert_eq!(ctx.terminal.get_formatted_output_string(), expected);
    }
}
