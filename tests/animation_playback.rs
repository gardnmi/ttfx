use std::rc::Rc;

use ttfx::engine::animation::{Animation, CharacterVisual, ExistingColorHandling, Scene, VisualParams};
use ttfx::utils::ansi::ColorCode;
use ttfx::utils::graphics::{Color, ColorPair, Gradient};

fn scene(looping: bool) -> Scene {
    Scene::new("test", looping, None, None, false, false)
}

#[test]
fn unequal_gradients_and_symbols_follow_python_distribution() {
    // Obtained from the pinned Python reference's apply_gradient_to_symbols.
    type GradientCase = (u8, u8, u8, &'static [(char, u8, u8)]);
    let cases: &[GradientCase] = &[
        (5, 2, 3, &[('A', 1, 20), ('A', 2, 20), ('B', 3, 20), ('B', 4, 21), ('C', 5, 21)]),
        (2, 5, 7, &[('A', 1, 20), ('B', 1, 20), ('C', 1, 21), ('D', 1, 21), ('E', 1, 22), ('F', 2, 23), ('G', 2, 24)]),
        (1, 1, 5, &[('A', 1, 20), ('B', 1, 20), ('C', 1, 20), ('D', 1, 20), ('E', 1, 20)]),
        (7, 3, 2, &[('A', 1, 20), ('A', 2, 20), ('A', 3, 20), ('A', 4, 21), ('B', 5, 21), ('B', 6, 22), ('B', 7, 22)]),
    ];
    for &(fg_count, bg_count, symbol_count, expected) in cases {
        let fg = Gradient { spectrum: (1..=fg_count).map(Color::from_xterm).collect() };
        let bg = Gradient { spectrum: (20..20 + bg_count).map(Color::from_xterm).collect() };
        let symbols: Vec<String> = (b'A'..b'A' + symbol_count).map(|c| (c as char).to_string()).collect();
        let mut scene = scene(false);
        scene.apply_gradient_to_symbols(&symbols, 2, Some(&fg), Some(&bg)).unwrap();
        assert_eq!(scene.all_frames.len(), expected.len());
        assert_eq!(scene.easing_total_steps, expected.len() as i64 * 2);
        for (frame, &(symbol, foreground, background)) in scene.all_frames.iter().zip(expected) {
            let visual = &frame.character_visual;
            let colors = visual.colors.unwrap();
            assert_eq!(visual.symbol, symbol.to_string());
            assert_eq!(colors.fg_color.unwrap(), Color::from_xterm(foreground));
            assert_eq!(colors.bg_color.unwrap(), Color::from_xterm(background));
        }
        for &(symbol, _, _) in expected {
            for _ in 0..2 {
                assert_eq!(scene.get_next_visual().symbol, symbol.to_string());
            }
        }
        assert!(scene.frames().is_empty());
    }
}

#[test]
fn reset_restores_partially_played_frames_and_looping_order() {
    for looping in [false, true] {
        let mut scene = scene(looping);
        for (symbol, duration) in [("A", 2), ("B", 3), ("C", 1)] {
            scene.add_frame(symbol, duration, VisualParams::default()).unwrap();
        }
        for symbol in ["A", "A", "B"] {
            assert_eq!(scene.get_next_visual().symbol, symbol);
        }
        scene.reset_scene();
        assert_eq!(scene.ticks_elapsed(), 0);
        assert!(scene.played_frames().is_empty());
        for symbol in ["A", "A", "B", "B", "B", "C"] {
            assert_eq!(scene.get_next_visual().symbol, symbol);
        }
        if looping {
            assert_eq!(scene.get_next_visual().symbol, "A");
        } else {
            assert!(scene.frames().is_empty());
        }
    }
}

#[test]
fn appearance_changes_preserve_shared_visuals_and_weak_observers() {
    let mut animation = Animation::new("original");
    let original = Rc::clone(&animation.current_character_visual);
    animation.set_appearance("input", false, Some("λ"), None);
    assert_eq!(original.formatted_symbol.as_str(), "original");
    assert_eq!(animation.current_character_visual.formatted_symbol.as_str(), "λ");

    let observer = Rc::downgrade(&animation.current_character_visual);
    animation.set_appearance("input", false, Some("replacement"), None);
    assert!(observer.upgrade().is_none());
    assert_eq!(animation.current_character_visual.formatted_symbol.as_str(), "replacement");
}

#[test]
fn reused_appearances_reset_styles_and_follow_color_mode_changes() {
    let mut animation = Animation::new("input");
    animation.current_character_visual = Rc::new(CharacterVisual::new(
        "styled",
        VisualParams {
            bold: true,
            dim: true,
            italic: true,
            underline: true,
            blink: true,
            reverse: true,
            hidden: true,
            strike: true,
            ..Default::default()
        },
    ));
    let colors = ColorPair::new(Some(Color::from_hex("Fa0088").unwrap()), Some(Color::from_hex("0A0B0C").unwrap()));
    animation.set_appearance("input", false, Some("字"), Some(colors));
    let expected = CharacterVisual::new(
        "字",
        VisualParams {
            colors: Some(colors),
            fg_color_code: Some(ColorCode::Rgb("Fa0088".into())),
            bg_color_code: Some(ColorCode::Rgb("0A0B0C".into())),
            ..Default::default()
        },
    );
    assert_eq!(*animation.current_character_visual, expected);
    assert_eq!(
        animation.current_character_visual.formatted_symbol.as_str(),
        "\x1b[38;2;250;0;136m\x1b[48;2;10;11;12m字\x1b[0m"
    );

    animation.use_xterm_colors = true;
    animation.set_appearance("input", false, Some("x"), Some(ColorPair::new(Some(Color::from_xterm(1)), None)));
    assert_eq!(animation.current_character_visual.formatted_symbol.as_str(), "\x1b[38;5;1mx\x1b[0m");
    assert_eq!(animation.current_character_visual.bg_color_code, None);

    animation.no_color = true;
    animation.set_appearance("input", false, Some("z"), Some(colors));
    assert_eq!(animation.current_character_visual.formatted_symbol.as_str(), "z");
    assert_eq!(animation.current_character_visual.colors, Some(colors));
    assert_eq!(animation.current_character_visual.fg_color_code, None);

    animation.no_color = false;
    animation.existing_color_handling = ExistingColorHandling::Always;
    animation.input_bold = true;
    animation.input_fg_color = Some(Color::from_xterm(196));
    animation.input_bg_color = Some(Color::from_xterm(7));
    animation.set_appearance("input", true, Some("m"), Some(colors));
    assert_eq!(
        animation.current_character_visual.formatted_symbol.as_str(),
        "\x1b[1m\x1b[38;5;196m\x1b[48;5;7mm\x1b[0m"
    );

    let long_symbol = "🥟".repeat(40);
    animation.set_appearance("input", false, Some(&long_symbol), None);
    assert_eq!(animation.current_character_visual.formatted_symbol.as_str(), long_symbol);
    animation.set_appearance("input", false, None, None);
    assert_eq!(animation.current_character_visual.formatted_symbol.as_str(), "input");
}

#[test]
fn formatted_symbols_append_across_inline_and_heap_boundaries() {
    for length in 0..=96 {
        for suffix in ["", "λ漢💫"] {
            let text = format!("{}{suffix}", "a".repeat(length));
            let visual = CharacterVisual::plain(&text);
            for capacity in [0, 1, 31, 32, 63, 64, 128] {
                for prefix_length in 0..8 {
                    let prefix = "!".repeat(prefix_length);
                    let mut bytes = Vec::with_capacity(capacity);
                    bytes.extend_from_slice(prefix.as_bytes());
                    visual.formatted_symbol.append_to(&mut bytes);
                    visual.formatted_symbol.append_to(&mut bytes);
                    assert_eq!(String::from_utf8(bytes).unwrap(), format!("{prefix}{text}{text}"));
                }
            }
        }
    }
}

#[test]
fn shared_scene_visuals_preserve_independent_playback_and_mutation() {
    let mut first = scene(false);
    let mut second = scene(false);
    first.add_frame("λ", 2, VisualParams::default()).unwrap();
    second.add_frame("λ", 5, VisualParams::default()).unwrap();
    assert!(Rc::ptr_eq(&first.all_frames[0].character_visual, &second.all_frames[0].character_visual));
    first.get_next_visual();
    first.get_next_visual();
    assert!(first.frames().is_empty());
    assert_eq!(second.ticks_elapsed(), 0);
    Rc::make_mut(&mut second.all_frames.make_mut()[0].character_visual).symbol = "changed".into();
    assert_eq!(first.all_frames[0].character_visual.symbol, "λ");
}

#[test]
fn cached_scene_visuals_distinguish_modes_styles_and_color_representation() {
    let mut retained = Vec::new();
    for no_color in [false, true] {
        for xterm in [false, true] {
            for color in [Color::from_hex("FF0000").unwrap(), Color::from_hex("ff0000").unwrap(), Color::from_xterm(9)]
            {
                for style in 0..256u16 {
                    let params = VisualParams {
                        bold: style & 1 != 0,
                        dim: style & 2 != 0,
                        italic: style & 4 != 0,
                        underline: style & 8 != 0,
                        blink: style & 16 != 0,
                        reverse: style & 32 != 0,
                        hidden: style & 64 != 0,
                        strike: style & 128 != 0,
                        colors: Some(ColorPair::new(Some(color), None)),
                        // add_frame must ignore explicitly supplied codes.
                        fg_color_code: Some(ColorCode::Xterm(123)),
                        bg_color_code: Some(ColorCode::Xterm(234)),
                    };
                    let mut animation = Animation::new("x");
                    animation.no_color = no_color;
                    animation.use_xterm_colors = xterm;
                    let mut expected = params.clone();
                    expected.fg_color_code = animation.get_color_code(Some(&color));
                    expected.bg_color_code = None;
                    let expected = CharacterVisual::new("界", expected);
                    let mut scene = Scene::new("test", false, None, None, no_color, xterm);
                    for _ in 0..2 {
                        scene.add_frame("界", 1, params.clone()).unwrap();
                        assert_eq!(*scene.all_frames.last().unwrap().character_visual, expected);
                    }
                    retained.push(scene);
                }
            }
        }
    }
}

#[test]
fn scene_cache_does_not_keep_visuals_alive_after_teardown() {
    let observer = {
        let mut scene = scene(false);
        scene.add_frame("teardown", 1, VisualParams::default()).unwrap();
        Rc::downgrade(&scene.all_frames[0].character_visual)
    };
    assert!(observer.upgrade().is_none());
}

#[test]
fn scene_cache_applies_preexisting_overrides_before_lookup() {
    let red = ColorPair::new(Some(Color::from_hex("ff0000").unwrap()), None);
    let blue = ColorPair::new(Some(Color::from_hex("0000ff").unwrap()), None);
    let mut overridden = scene(false);
    overridden.preexisting_colors = Some(red);
    overridden.preexisting_bold = true;
    overridden.add_frame("x", 1, VisualParams { colors: Some(blue), ..Default::default() }).unwrap();
    let mut explicit = scene(false);
    explicit.add_frame("x", 1, VisualParams { colors: Some(red), bold: true, ..Default::default() }).unwrap();
    assert_eq!(overridden.all_frames[0].character_visual, explicit.all_frames[0].character_visual);
    let len = explicit.all_frames.len();
    assert!(explicit.add_frame("x", 0, VisualParams::default()).is_err());
    assert_eq!(explicit.all_frames.len(), len);
}

#[test]
fn scene_programs_share_definitions_but_keep_playback_and_append_independent() {
    let mut a = scene(false);
    let mut b = scene(false);
    for (symbol, duration) in [("A", 2), ("λ", 3), ("C", 1)] {
        a.add_frame(symbol, duration, VisualParams::default()).unwrap();
        b.add_frame(symbol, duration, VisualParams::default()).unwrap();
    }
    a.activate().unwrap();
    b.activate().unwrap();
    assert_eq!(a.all_frames.as_ptr(), b.all_frames.as_ptr());
    a.get_next_visual();
    assert_eq!(a.ticks_elapsed(), 1);
    assert_eq!(b.ticks_elapsed(), 0);
    b.add_frame("D", 2, VisualParams::default()).unwrap();
    assert_eq!(a.all_frames.len(), 3);
    assert_eq!(b.all_frames.len(), 4);
    assert_ne!(a.all_frames.as_ptr(), b.all_frames.as_ptr());
    for symbol in ["A", "λ", "λ", "λ", "C"] {
        assert_eq!(a.get_next_visual().symbol, symbol);
    }
    a.reset_scene();
    assert_eq!(a.get_next_visual().symbol, "A");
}

#[test]
fn long_duration_scenes_do_not_need_a_per_tick_allocation() {
    let mut scene = scene(false);
    scene.add_frame("A", 1_000_000_000, VisualParams::default()).unwrap();
    scene.add_frame("B", 3, VisualParams::default()).unwrap();
    for _ in 0..3 {
        assert_eq!(scene.get_next_visual().symbol, "A");
    }
    assert_eq!(scene.ticks_elapsed(), 3);
    assert_eq!(scene.easing_total_steps, 1_000_000_003);
}

#[test]
fn repeated_appearance_respects_color_modes_and_bold_overrides() {
    let colors = ColorPair::new(Some(Color::from_xterm(196)), Some(Color::from_xterm(7)));
    let mut animation = Animation::new("λ");
    for (no_color, xterm, expected) in [
        (false, false, "\x1b[38;2;255;0;0m\x1b[48;2;192;192;192mλ\x1b[0m"),
        (true, false, "λ"),
        (false, true, "\x1b[38;5;196m\x1b[48;5;7mλ\x1b[0m"),
    ] {
        animation.no_color = no_color;
        animation.use_xterm_colors = xterm;
        for _ in 0..3 {
            animation.set_appearance("λ", false, None, Some(colors));
            assert_eq!(animation.current_character_visual.formatted_symbol.as_str(), expected);
        }
    }
    animation.existing_color_handling = ExistingColorHandling::Always;
    animation.input_bold = true;
    animation.set_appearance("λ", true, None, Some(colors));
    assert_eq!(animation.current_character_visual.formatted_symbol.as_str(), "\x1b[1mλ\x1b[0m");
    animation.input_bold = false;
    animation.set_appearance("λ", true, None, Some(colors));
    assert_eq!(animation.current_character_visual.formatted_symbol.as_str(), "λ");
}
