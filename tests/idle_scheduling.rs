use ttfx::engine::animation::{SyncMetric, VisualParams};
use ttfx::engine::character::CharId;
use ttfx::engine::ctx::{Clock, EffectHooks, EngineCtx, NoopHooks};
use ttfx::engine::events::{CallerKey, EffectCallback, Event, EventAction};
use ttfx::engine::terminal::TerminalConfig;
use ttfx::utils::easing::Easing;
use ttfx::utils::geometry::Coord;
use ttfx::utils::rng::Rng;

fn context(scheduled: bool, mixed: bool, count: usize) -> EngineCtx {
    let config = TerminalConfig {
        canvas_width: count as i64,
        canvas_height: 1,
        ignore_terminal_dimensions: true,
        frame_rate: 0,
        ..Default::default()
    };
    let mut ctx =
        EngineCtx::new(&"X".repeat(count), config, Rng::seeded(7), Clock::virtual_with_frame_rate(60)).unwrap();
    ctx.set_idle_scheduling(scheduled);
    ctx.set_scene_runtime(scheduled);
    ctx.event_log = Some(Vec::new());
    for n in 0..count {
        let id = CharId(n as u32);
        let ch = &mut ctx.terminal.arena[n];
        ch.animation.new_scene(false, None, None, "unused", false);
        let ease = (mixed && n % 19 == 0).then_some(Easing::InOutBack);
        let sync = (mixed && n % 17 == 0).then_some(SyncMetric::Step);
        ch.animation.new_scene(mixed && n % 13 == 0, sync, ease, "hold", false);
        let scene = ch.animation.scenes.get_mut("hold").unwrap();
        let duration = if mixed { [1, 2, 5, 257, 1_000_000_000][n % 5] } else { 20 + n as i64 % 11 };
        for symbol in ["X", "Y", "Z"] {
            scene.add_frame(symbol, duration, VisualParams::default()).unwrap();
        }
        if mixed && n % 11 == 0 {
            ch.motion.new_path(2.0, None, None, 0, false, "move").unwrap();
            ch.motion.paths.get_mut("move").unwrap().new_waypoint(Coord::new(20, 1), None, "").unwrap();
            ctx.activate_path(&mut NoopHooks, id, "move");
        }
        ctx.terminal.set_character_visibility(id, true);
        ctx.activate_scene(&mut NoopHooks, id, "hold");
        ctx.active_characters.insert(id);
    }
    ctx
}

fn assert_state(a: &mut EngineCtx, b: &mut EngineCtx, tick: usize) {
    assert_eq!(
        a.active_characters.iter().collect::<Vec<_>>(),
        b.active_characters.iter().collect::<Vec<_>>(),
        "members at {tick}"
    );
    assert_eq!(a.event_log, b.event_log, "events at {tick}");
    a.event_log.as_mut().unwrap().clear();
    b.event_log.as_mut().unwrap().clear();
    assert_eq!(a.terminal.arena.len(), b.terminal.arena.len());
    for n in 0..a.terminal.arena.len() {
        let x = &a.terminal.arena[n];
        let y = &b.terminal.arena[n];
        assert_eq!(x.motion.current_coord, y.motion.current_coord, "coord {n} at {tick}");
        assert_eq!(x.motion.previous_coord, y.motion.previous_coord, "previous {n} at {tick}");
        assert_eq!(x.motion.active_path, y.motion.active_path);
        assert_eq!(x.layer, y.layer);
        assert_eq!(x.animation.active_scene, y.animation.active_scene);
        assert_eq!(
            x.animation.current_character_visual.formatted_symbol,
            y.animation.current_character_visual.formatted_symbol,
            "visual {n} at {tick}"
        );
        for (name, scene) in x.animation.scenes.iter() {
            let other = y.animation.scenes.get(name).unwrap();
            assert_eq!(scene.frames(), other.frames(), "frames {n}/{name} at {tick}");
            assert_eq!(scene.played_frames(), other.played_frames(), "played {n}/{name} at {tick}");
            assert_eq!(scene.ticks_elapsed(), other.ticks_elapsed(), "ticks {n}/{name} at {tick}");
            assert_eq!(scene.easing_current_step(), other.easing_current_step());
        }
    }
    assert_eq!(a.terminal.get_formatted_output_string(), b.terminal.get_formatted_output_string(), "render at {tick}");
}

#[test]
fn held_frames_match_eager_state_across_retirement_and_timer_wrap() {
    for count in [96, 192] {
        held_frames_match(count);
    }
}

fn held_frames_match(count: usize) {
    let mut fast = context(true, true, count);
    let mut eager = context(false, true, count);
    for tick in 0..900 {
        if tick % 89 == 0 {
            for ctx in [&mut fast, &mut eager] {
                for n in [4, 8, 12, 16] {
                    ctx.terminal.arena[n].animation.scenes.get_mut("hold").unwrap().reset_scene();
                    ctx.activate_scene(&mut NoopHooks, CharId(n as u32), "hold");
                    ctx.active_characters.insert(CharId(n as u32));
                }
            }
        }
        fast.update(&mut NoopHooks);
        eager.update(&mut NoopHooks);
        if tick % 23 == 0 {
            assert_state(&mut fast, &mut eager, tick);
        }
    }
    assert_state(&mut fast, &mut eager, 900);
}

#[derive(Default)]
struct Mutations {
    count: usize,
    reads: Vec<(usize, i64, usize, i64)>,
}
impl EffectHooks for Mutations {
    fn dispatch_callback(&mut self, ctx: &mut EngineCtx, character: CharId, _: &EffectCallback) {
        self.count += 1;
        for n in [16, 80] {
            let scene = ctx.terminal.arena[n].animation.scenes.get("hold").unwrap();
            self.reads.push((n, scene.ticks_elapsed(), scene.frames().start, scene.easing_current_step()));
            match self.count % 8 {
                0 => ctx.terminal.arena[n].animation.set_appearance("X", false, Some("!"), None),
                1 => ctx.terminal.arena[n].animation.scenes.get_mut("hold").unwrap().reset_scene(),
                2 => {
                    let anim = &mut ctx.terminal.arena[n].animation;
                    anim.scenes.remove("unused"); // shifts the scheduled scene's slot
                    anim.scenes.get_mut("hold").unwrap().add_frame("A", 3, VisualParams::default()).unwrap();
                }
                3 => {
                    ctx.step_animation(self, CharId(n as u32));
                    ctx.tick(self, CharId(n as u32));
                }
                4 => {
                    ctx.active_characters.remove(&CharId(n as u32));
                }
                5 => {
                    let anim = &mut ctx.terminal.arena[n].animation;
                    anim.new_scene(false, None, None, "hold", false); // overwrite a scheduled program
                    for symbol in ["B", "C"] {
                        anim.scenes.get_mut("hold").unwrap().add_frame(symbol, 30, VisualParams::default()).unwrap();
                    }
                    ctx.activate_scene(self, CharId(n as u32), "hold");
                }
                _ => {}
            }
        }
        if self.count % 8 == 6 {
            ctx.update(self);
        }
        if self.count % 8 == 7 {
            let id = ctx.terminal.add_character("N", Coord::new(5, 1));
            let anim = &mut ctx.terminal.arena[id.0 as usize].animation;
            anim.new_scene(false, None, None, "hold", false);
            for symbol in ["N", "M"] {
                anim.scenes.get_mut("hold").unwrap().add_frame(symbol, 20, VisualParams::default()).unwrap();
            }
            ctx.activate_scene(self, id, "hold");
            ctx.active_characters.insert(id);
        }
        ctx.activate_scene(self, character, "pulse");
        ctx.active_characters.insert(character);
    }
}

#[test]
fn callbacks_materialize_and_wake_both_sides_of_the_update_cursor() {
    for count in [96, 192] {
        for eased in [false, true] {
            callbacks_match(count, eased);
        }
    }
}

fn callbacks_match(count: usize, eased: bool) {
    let mut fast = context(true, false, count);
    let mut eager = context(false, false, count);
    for ctx in [&mut fast, &mut eager] {
        if eased {
            for n in [16, 80] {
                ctx.terminal.arena[n].animation.scenes.get_mut("hold").unwrap().ease = Some(Easing::OutElastic);
            }
        }
        let anim = &mut ctx.terminal.arena[48].animation;
        anim.new_scene(false, None, None, "pulse", false);
        anim.scenes.get_mut("pulse").unwrap().add_frame("P", 4, VisualParams::default()).unwrap();
        ctx.activate_scene(&mut NoopHooks, CharId(48), "pulse");
        ctx.register_event(
            CharId(48),
            Event::SceneComplete,
            CallerKey::Scene("pulse".into()),
            EventAction::Callback(EffectCallback { id: 0, args: Vec::new() }),
        )
        .unwrap();
    }
    let mut a = Mutations::default();
    let mut b = Mutations::default();
    for tick in 0..180 {
        for ctx in [&mut fast, &mut eager] {
            if tick % 13 == 1 {
                ctx.active_characters.insert(CharId(16));
                ctx.active_characters.insert(CharId(80));
            }
            if tick % 31 == 9 {
                // Whole-slice mutable access must invalidate held state too.
                for ch in ctx.terminal.arena.iter_mut() {
                    ch.layer += 1;
                }
            }
            if tick == 87 {
                ctx.set_idle_scheduling(false);
            }
        }
        if tick == 91 {
            fast.set_idle_scheduling(true);
        }
        fast.update(&mut a);
        eager.update(&mut b);
        assert_eq!(a.count, b.count, "callback count at {tick}");
        assert_eq!(a.reads, b.reads, "callback reads at {tick}");
        assert_state(&mut fast, &mut eager, tick);
    }
}

#[test]
fn eased_holds_survive_curve_changes_appends_and_restarts() {
    for count in [96, 192] {
        let mut fast = context(true, false, count);
        let mut eager = context(false, false, count);
        for tick in 0..900 {
            if tick % 89 == 0 {
                for ctx in [&mut fast, &mut eager] {
                    for n in 0..count {
                        let scene = ctx.terminal.arena[n].animation.scenes.get_mut("hold").unwrap();
                        scene.ease = Some(match tick % 3 {
                            0 => Easing::InOutBack,
                            1 => Easing::OutElastic,
                            _ => Easing::CubicBezier(0.2, -0.8, 0.7, 1.5),
                        });
                        if tick > 0 {
                            scene.add_frame("Q", 7, VisualParams::default()).unwrap();
                        }
                        scene.reset_scene();
                        ctx.activate_scene(&mut NoopHooks, CharId(n as u32), "hold");
                        ctx.active_characters.insert(CharId(n as u32));
                    }
                }
            }
            fast.update(&mut NoopHooks);
            eager.update(&mut NoopHooks);
            if tick % 23 == 0 {
                assert_state(&mut fast, &mut eager, tick);
            }
        }
        assert_state(&mut fast, &mut eager, 900);
    }
}

#[test]
fn eased_single_visual_holds_cross_timer_wrap_and_finish_on_time() {
    for count in [96, 192] {
        let mut fast = context(true, false, count);
        let mut eager = context(false, false, count);
        for ctx in [&mut fast, &mut eager] {
            for n in 0..count {
                let animation = &mut ctx.terminal.arena[n].animation;
                animation.new_scene(false, None, Some(Easing::OutElastic), "hold", false);
                animation.scenes.get_mut("hold").unwrap().add_frame("X", 600, VisualParams::default()).unwrap();
                ctx.activate_scene(&mut NoopHooks, CharId(n as u32), "hold");
            }
        }
        for tick in 0..605 {
            fast.update(&mut NoopHooks);
            eager.update(&mut NoopHooks);
            if tick % 47 == 0 || tick >= 597 {
                assert_state(&mut fast, &mut eager, tick);
            }
        }
        assert!(fast.active_characters.is_empty());
    }
}

#[test]
fn compact_runtime_matches_reference_without_scheduler_and_after_toggles() {
    for count in [96, 192] {
        let mut fast = context(false, true, count);
        fast.set_scene_runtime(true);
        let mut reference = context(false, true, count);
        for tick in 0..900 {
            if tick == 103 || tick == 405 {
                fast.set_scene_runtime(false);
            }
            if tick == 118 || tick == 412 {
                fast.set_scene_runtime(true);
            }
            fast.update(&mut NoopHooks);
            reference.update(&mut NoopHooks);
            if tick % 29 == 0 {
                assert_state(&mut fast, &mut reference, tick);
            }
        }
        assert_state(&mut fast, &mut reference, 900);
    }
}

#[test]
fn arena_clone_preserves_pending_runtime_counters_and_detaches_mutations() {
    let mut fast = context(true, false, 192);
    let mut reference = context(false, false, 192);
    for _ in 0..12 {
        fast.update(&mut NoopHooks);
        reference.update(&mut NoopHooks);
    }
    // Clone before any observing reads materialize the compact/held counters.
    let mut cloned = fast.terminal.arena.clone();
    for n in 0..cloned.len() {
        let scene = cloned[n].animation.scenes.get("hold").unwrap();
        let expected = reference.terminal.arena[n].animation.scenes.get("hold").unwrap();
        assert_eq!(scene.frames(), expected.frames());
        assert_eq!(scene.ticks_elapsed(), expected.ticks_elapsed());
    }
    cloned[16].animation.scenes.get_mut("hold").unwrap().reset_scene();
    assert_eq!(cloned[16].animation.scenes.get("hold").unwrap().ticks_elapsed(), 0);
    assert_eq!(fast.terminal.arena[16].animation.scenes.get("hold").unwrap().ticks_elapsed(), 12);
    for tick in 12..120 {
        fast.update(&mut NoopHooks);
        reference.update(&mut NoopHooks);
        assert_state(&mut fast, &mut reference, tick);
    }
}
