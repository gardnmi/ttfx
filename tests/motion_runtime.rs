use ttfx::engine::animation::{SyncMetric, VisualParams};
use ttfx::engine::character::CharId;
use ttfx::engine::ctx::{Clock, EffectHooks, EngineCtx, NoopHooks};
use ttfx::engine::events::{CallerKey, EffectCallback, Event, EventAction};
use ttfx::engine::terminal::TerminalConfig;
use ttfx::utils::easing::Easing;
use ttfx::utils::geometry::Coord;
use ttfx::utils::rng::Rng;

fn context(enabled: bool, count: usize, ease: Option<Easing>, shape: usize) -> EngineCtx {
    let mut ctx = EngineCtx::new(
        &"X".repeat(count),
        TerminalConfig {
            canvas_width: count.max(50) as i64,
            canvas_height: 12,
            ignore_terminal_dimensions: true,
            ..Default::default()
        },
        Rng::seeded(1),
        Clock::virtual_with_frame_rate(60),
    )
    .unwrap();
    ctx.set_motion_runtime(enabled);
    for n in 0..count {
        let ch = &mut ctx.terminal.arena[n];
        ch.is_visible = true;
        let offset = if shape == 3 { 1_i64 << 53 } else { 0 };
        ch.motion.current_coord = Coord::new(offset + 1, 1);
        ch.motion.new_path(1.0, None, None, 0, false, "unused").unwrap();
        ch.motion.new_path(0.37, ease, None, 3, n % 2 == 0, "walk").unwrap();
        let p = ch.motion.paths.get_mut("walk").unwrap();
        for (x, y) in [(1, 10), (1, 10), (40, 10), (40, 1), (1, 1)] {
            let controls = match shape {
                1 => Some(vec![Coord::new(7, 17)]),
                2 => Some(vec![Coord::new(7, 17), Coord::new(-3, 6)]),
                _ => None,
            };
            p.new_waypoint(Coord::new(offset + x, y), controls, "").unwrap();
        }
        let sync = match n % 5 {
            0 => Some(SyncMetric::Step),
            1 => Some(SyncMetric::Distance),
            _ => None,
        };
        let ease = (n % 5 == 3).then_some(Easing::InOutBack);
        ch.animation.new_scene(n % 5 == 4, sync, ease, "scene", false);
        for symbol in ["X", "λ", "M", "!"] {
            ch.animation.scenes.get_mut("scene").unwrap().add_frame(symbol, 47, VisualParams::default()).unwrap();
        }
        ctx.activate_path(&mut NoopHooks, CharId(n as u32), "walk");
        ctx.activate_scene(&mut NoopHooks, CharId(n as u32), "scene");
        ctx.active_characters.insert(CharId(n as u32));
    }
    ctx
}

fn assert_state(a: &mut EngineCtx, b: &mut EngineCtx) {
    assert_eq!(a.active_characters.iter().collect::<Vec<_>>(), b.active_characters.iter().collect::<Vec<_>>());
    assert_eq!(a.event_log, b.event_log);
    assert_eq!(a.terminal.arena.len(), b.terminal.arena.len());
    for n in 0..a.terminal.arena.len() {
        let x = &a.terminal.arena[n];
        let y = &b.terminal.arena[n];
        assert_eq!(x.motion.current_coord, y.motion.current_coord, "current {n}");
        assert_eq!(x.motion.previous_coord, y.motion.previous_coord, "previous {n}");
        assert_eq!(x.motion.active_path, y.motion.active_path);
        assert_eq!(x.motion.completed_path, y.motion.completed_path);
        assert_eq!(x.layer, y.layer);
        assert_eq!(x.animation.active_scene, y.animation.active_scene);
        assert_eq!(x.animation.current_character_visual, y.animation.current_character_visual);
        for (name, p) in x.motion.paths.iter() {
            let q = y.motion.paths.get(name).unwrap();
            assert_eq!(p.current_step(), q.current_step(), "step {n} {name}");
            assert_eq!(p.last_distance_reached().to_bits(), q.last_distance_reached().to_bits());
            assert_eq!(p.hold_time_remaining, q.hold_time_remaining);
            for (s, t) in p.segments.iter().zip(&q.segments) {
                assert_eq!(
                    (s.enter_event_triggered, s.exit_event_triggered),
                    (t.enter_event_triggered, t.exit_event_triggered)
                );
            }
        }
        for (name, s) in x.animation.scenes.iter() {
            let t = y.animation.scenes.get(name).unwrap();
            assert_eq!(s.frames(), t.frames());
            assert_eq!(s.ticks_elapsed(), t.ticks_elapsed());
            assert_eq!(s.easing_current_step(), t.easing_current_step());
        }
    }
    assert_eq!(a.terminal.get_formatted_output_string(), b.terminal.get_formatted_output_string());
}

#[test]
fn prepared_motion_matches_walker_for_curves_sync_holds_loops_and_large_coordinates() {
    let mut curves = vec![None, Some(Easing::CubicBezier(0.12, 0.8, 0.9, 0.02))];
    for family in ["sine", "quad", "cubic", "quart", "quint", "expo", "circ", "back", "elastic", "bounce"] {
        for direction in ["in", "out", "in_out"] {
            curves.push(Some(Easing::parse(&format!("{direction}_{family}")).unwrap()));
        }
    }
    for ease in curves {
        for shape in 0..4 {
            let mut fast = context(true, 5, ease, shape);
            let mut ordinary = context(false, 5, ease, shape);
            for tick in 0..700 {
                fast.update(&mut NoopHooks);
                ordinary.update(&mut NoopHooks);
                if tick % 7 == 0 || tick > 680 {
                    assert_state(&mut fast, &mut ordinary);
                }
            }
        }
    }
}

#[test]
fn public_edits_clone_growth_map_changes_and_tracing_retire_prepared_progress() {
    let mut fast = context(true, 5, Some(Easing::OutElastic), 0);
    let mut ordinary = context(false, 5, Some(Easing::OutElastic), 0);
    for tick in 0..400 {
        for ctx in [&mut fast, &mut ordinary] {
            match tick {
                17 => ctx.terminal.arena = ctx.terminal.arena.clone(),
                35 => {
                    let p = ctx.terminal.arena[0].motion.paths.get_mut("walk").unwrap();
                    p.set_current_step(5);
                    p.set_last_distance_reached(2.0);
                    p.waypoints[1].coord = Coord::new(-2, 8);
                    p.ease = Some(Easing::InBack);
                }
                51 => {
                    ctx.terminal.arena[1].motion.paths.remove("unused");
                }
                65 => {
                    ctx.terminal.add_character("N", Coord::new(4, 4));
                    for ch in ctx.terminal.arena.iter_mut() {
                        ch.layer += 1;
                    }
                }
                87 => {
                    let replacement = ctx.terminal.arena[2].motion.paths.get("walk").unwrap().clone();
                    ctx.terminal.arena[2].motion.paths.insert("walk", replacement);
                    ctx.activate_path(&mut NoopHooks, CharId(2), "walk");
                }
                113 => ctx.event_log = Some(Vec::new()),
                131 => ctx.event_log = None,
                171 => {
                    let waypoint = ctx.terminal.arena[3].motion.paths.get("walk").unwrap().waypoints[2].key();
                    ctx.register_event(
                        CharId(3),
                        Event::SegmentExited,
                        CallerKey::Waypoint(waypoint),
                        EventAction::SetLayer(91),
                    )
                    .unwrap();
                    ctx.activate_path(&mut NoopHooks, CharId(3), "walk");
                }
                _ => {}
            }
        }
        fast.update(&mut NoopHooks);
        ordinary.update(&mut NoopHooks);
        assert_state(&mut fast, &mut ordinary);
    }
    fast.set_motion_runtime(false);
    assert_state(&mut fast, &mut ordinary);
}

#[derive(Default)]
struct CallbackEdits {
    count: usize,
    observed: Vec<(usize, i64, Coord)>,
}

impl EffectHooks for CallbackEdits {
    fn dispatch_callback(&mut self, ctx: &mut EngineCtx, _: CharId, _: &EffectCallback) {
        self.count += 1;
        for n in [16, 100] {
            // before and after the update cursor
            let ch = &ctx.terminal.arena[n];
            self.observed.push((n, ch.motion.paths.get("walk").unwrap().current_step(), ch.motion.current_coord));
            if self.count % 3 == 0 {
                ctx.terminal.arena[n].motion.paths.remove("unused");
                ctx.activate_path(self, CharId(n as u32), "walk");
            } else {
                ctx.motion_move(self, CharId(n as u32));
                ctx.step_animation(self, CharId(n as u32));
            }
            ctx.active_characters.insert(CharId(n as u32));
        }
        if self.count % 3 == 1 {
            // The emitting non-looping scene is already complete, so nesting
            // cannot recursively re-emit this callback.
            ctx.update(self);
        }
    }
}

#[test]
fn callback_observations_and_nested_updates_preserve_character_order() {
    let mut fast = context(true, 130, None, 0);
    let mut ordinary = context(false, 130, None, 0);
    for ctx in [&mut fast, &mut ordinary] {
        ctx.terminal.arena[64].motion.deactivate_path(None);
        let anim = &mut ctx.terminal.arena[64].animation;
        anim.new_scene(false, None, None, "pulse", false);
        anim.scenes.get_mut("pulse").unwrap().add_frame("P", 9, VisualParams::default()).unwrap();
        ctx.register_event(
            CharId(64),
            Event::SceneComplete,
            CallerKey::Scene("pulse".into()),
            EventAction::Callback(EffectCallback { id: 0, args: Vec::new() }),
        )
        .unwrap();
    }
    let mut a = CallbackEdits::default();
    let mut b = CallbackEdits::default();
    for tick in 0..130 {
        if tick % 13 == 0 {
            for ctx in [&mut fast, &mut ordinary] {
                ctx.activate_scene(&mut NoopHooks, CharId(64), "pulse");
                ctx.active_characters.insert(CharId(64));
            }
        }
        fast.update(&mut a);
        ordinary.update(&mut b);
        assert_eq!(a.observed, b.observed);
        assert_state(&mut fast, &mut ordinary);
    }
    assert!(a.count > 5);
}
