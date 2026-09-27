//! The traced walker retains synchronous event boundaries; compare the fast
//! walker against it, including all mutable path state, on every tick.
use ttfx::engine::character::CharId;
use ttfx::engine::ctx::{Clock, EngineCtx, NoopHooks};
use ttfx::engine::events::{CallerKey, Event, EventAction};
use ttfx::engine::terminal::TerminalConfig;
use ttfx::utils::easing::Easing;
use ttfx::utils::geometry::Coord;
use ttfx::utils::rng::Rng;

fn context(trace: bool, ease: Option<Easing>, speed: f64, looping: bool, curved: bool) -> EngineCtx {
    let mut ctx =
        EngineCtx::new("x", TerminalConfig::default(), Rng::seeded(1), Clock::virtual_with_frame_rate(60)).unwrap();
    if trace {
        ctx.event_log = Some(Vec::new());
    }
    let motion = &mut ctx.terminal.arena[0].motion;
    motion.new_path(speed, ease, Some(2), 3, looping, "walk").unwrap();
    let path = motion.paths.get_mut("walk").unwrap();
    for coord in [Coord::new(1, 1), Coord::new(1, 1), Coord::new(-7, 9), Coord::new(20, -3)] {
        let control = curved.then(|| vec![Coord::new(4, 7), Coord::new(-2, 3)]);
        path.new_waypoint(coord, control, "").unwrap();
    }
    ctx.register_event(CharId(0), Event::PathHolding, CallerKey::Path("walk".into()), EventAction::SetLayer(7))
        .unwrap();
    ctx.register_event(
        CharId(0),
        Event::PathComplete,
        CallerKey::Path("walk".into()),
        EventAction::SetCoordinate(Coord::new(5, 5)),
    )
    .unwrap();
    ctx.activate_path(&mut NoopHooks, CharId(0), "walk");
    ctx
}

#[test]
fn unobserved_motion_matches_reentrant_walker() {
    for ease in
        [None, Some(Easing::InOutQuad), Some(Easing::InBack), Some(Easing::OutElastic), Some(Easing::InOutBounce)]
    {
        for speed in [0.125, 1.7, 500.0] {
            for looping in [false, true] {
                for curved in [false, true] {
                    let mut fast = context(false, ease, speed, looping, curved);
                    let mut traced = context(true, ease, speed, looping, curved);
                    for tick in 0..800 {
                        // Reactivation must also preserve distance accumulation.
                        if tick == 500 {
                            fast.activate_path(&mut NoopHooks, CharId(0), "walk");
                            traced.activate_path(&mut NoopHooks, CharId(0), "walk");
                        }
                        fast.motion_move(&mut NoopHooks, CharId(0));
                        traced.motion_move(&mut NoopHooks, CharId(0));
                        let a = &fast.terminal.arena[0];
                        let b = &traced.terminal.arena[0];
                        assert_eq!(a.layer, b.layer);
                        assert_eq!(a.motion.current_coord, b.motion.current_coord);
                        assert_eq!(a.motion.previous_coord, b.motion.previous_coord);
                        assert_eq!(a.motion.active_path, b.motion.active_path);
                        assert_eq!(a.motion.completed_path, b.motion.completed_path);
                        let a = a.motion.paths.get("walk").unwrap();
                        let b = b.motion.paths.get("walk").unwrap();
                        assert_eq!(a.current_step(), b.current_step());
                        assert_eq!(a.max_steps, b.max_steps);
                        assert_eq!(a.hold_time_remaining, b.hold_time_remaining);
                        assert_eq!(a.total_distance.to_bits(), b.total_distance.to_bits());
                        assert_eq!(a.last_distance_reached().to_bits(), b.last_distance_reached().to_bits());
                        for (a, b) in a.segments.iter().zip(&b.segments) {
                            assert_eq!(
                                (a.enter_event_triggered, a.exit_event_triggered),
                                (b.enter_event_triggered, b.exit_event_triggered)
                            );
                        }
                        traced.event_log.as_mut().unwrap().clear();
                    }
                }
            }
        }
    }
}
