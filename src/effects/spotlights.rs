//! spotlights, ported from effects/effect_spotlights.py.

use clap::Args;

use crate::cli::parse_color;
use crate::effects::common::{
    parse_gradient_direction, parse_gradient_steps, parse_non_negative_float, parse_positive_float,
    parse_positive_float_range, parse_positive_int,
};
use crate::engine::active_characters::ActiveCharacters;
use crate::engine::animation::{Animation, ExistingColorHandling};
use crate::engine::character::CharId;
use crate::engine::ctx::{EffectHooks, EngineCtx};
use crate::engine::effect::Effect;
use crate::engine::error::EngineError;
use crate::engine::events::EffectCallback;
use crate::engine::terminal::{CharacterFilter, CharacterSort};
use crate::utils::easing::Easing;
use crate::utils::geometry::{self, Coord};
use crate::utils::graphics::{Color, ColorPair, Gradient, GradientDirection};
use crate::utils::pycompat::floor_div;

#[derive(Args, Debug, Clone)]
pub struct SpotlightsConfig {
    /// Width of the beam of light as min(width, height) // n of the input text. Values less than 1 are raised to 1.
    #[arg(long = "beam-width-ratio", default_value_t = 2.0, value_parser = parse_positive_float)]
    pub beam_width_ratio: f64,

    /// Distance from the edge of the beam where the brightness begins to fall off, as a percentage of total beam width.
    #[arg(long = "beam-falloff", default_value_t = 0.3, value_parser = parse_non_negative_float)]
    pub beam_falloff: f64,

    /// Duration of the search phase, in frames, before the spotlights converge in the center.
    #[arg(long = "search-duration", default_value_t = 550, value_parser = parse_positive_int)]
    pub search_duration: i64,

    /// Range of speeds for the spotlights during the search phase.
    #[arg(long = "search-speed-range", default_value = "0.35-0.75", value_parser = parse_positive_float_range)]
    pub search_speed_range: (f64, f64),

    /// Number of spotlights to use.
    #[arg(long = "spotlight-count", default_value_t = 3, value_parser = parse_positive_int)]
    pub spotlight_count: i64,

    /// Space separated, unquoted, list of colors for the final color gradient.
    #[arg(long = "final-gradient-stops", num_args = 1.., value_parser = parse_color,
          default_values = ["ab48ff", "e7b2b2", "fffebd"])]
    pub final_gradient_stops: Vec<Color>,

    /// Number of gradient steps to use.
    #[arg(long = "final-gradient-steps", num_args = 1.., value_parser = parse_gradient_steps,
          default_values = ["12"])]
    pub final_gradient_steps: Vec<i64>,

    /// Direction of the final gradient.
    #[arg(long = "final-gradient-direction", default_value = "vertical", value_parser = parse_gradient_direction)]
    pub final_gradient_direction: GradientDirection,
}

/// Exact integer-offset distances, bounded to 2 MiB per effect. Off-canvas
/// offsets outside this table use the original hypot implementation.
#[derive(Default)]
struct DistanceTable {
    width: usize,
    values: Vec<f64>,
}

impl DistanceTable {
    fn new(width: i64, height: i64) -> Self {
        let width = width.clamp(1, 1024) as usize + 1;
        let height = (height.clamp(1, 1024) as usize + 1).min(262_144 / width);
        let mut values = Vec::with_capacity(width * height);
        for row in 0..height {
            for column in 0..width {
                values.push((column as f64).hypot(2.0 * row as f64));
            }
        }
        Self { width, values }
    }

    fn distance(&self, from: Coord, to: Coord) -> f64 {
        let column = (to.column - from.column).unsigned_abs();
        let row = (to.row - from.row).unsigned_abs();
        if self.width != 0 && column < self.width as u64 && row < (self.values.len() / self.width) as u64 {
            self.values[row as usize * self.width + column as usize]
        } else {
            geometry::find_length_of_line(from, to, true)
        }
    }
}

pub struct Spotlights {
    config: SpotlightsConfig,
    illuminated_chars: ActiveCharacters,
    illuminated_scratch: ActiveCharacters,
    character_color_map: Vec<Option<(ColorPair, ColorPair)>>,
    spotlights: Vec<CharId>,
    illuminate_range: i64,
    search_duration: i64,
    searching: bool,
    expanding: bool,
    complete: bool,
    distances: DistanceTable,
    spotlight_coords: Vec<Coord>,
    lighting_cache: bool,
    lighting_stamp: Option<(u64, u64, u64)>,
    lighting_state: Option<(i64, bool, ExistingColorHandling)>,
}

impl Spotlights {
    pub fn new(config: SpotlightsConfig) -> Self {
        Spotlights {
            config,
            illuminated_chars: ActiveCharacters::new(),
            illuminated_scratch: ActiveCharacters::new(),
            character_color_map: Vec::new(),
            spotlights: Vec::new(),
            illuminate_range: 1,
            search_duration: 0,
            searching: true,
            expanding: false,
            complete: false,
            distances: DistanceTable::default(),
            spotlight_coords: Vec::new(),
            lighting_cache: std::env::var_os("TTFX_LIGHTING_CACHE").is_none_or(|value| value != "0"),
            lighting_stamp: None,
            lighting_state: None,
        }
    }

    /// SpotlightsIterator._adjust_color_pair_brightness.
    fn adjust_color_pair_brightness(colors: &ColorPair, brightness_factor: f64) -> ColorPair {
        ColorPair::new(
            colors.fg_color.as_ref().map(|fg| Animation::adjust_color_brightness(fg, brightness_factor)),
            colors.bg_color.as_ref().map(|bg| Animation::adjust_color_brightness(bg, brightness_factor)),
        )
    }

    /// SpotlightsIterator._has_input_colors.
    fn has_input_colors(ctx: &EngineCtx, id: CharId) -> bool {
        let ch = &ctx.terminal.arena[id.0 as usize];
        ch.animation.input_fg_color.is_some() || ch.animation.input_bg_color.is_some()
    }

    /// SpotlightsIterator._is_spotlightable.
    fn is_spotlightable(ctx: &EngineCtx, id: CharId) -> bool {
        ctx.terminal.arena[id.0 as usize].input_symbol != " " || Self::has_input_colors(ctx, id)
    }

    /// SpotlightsIterator._get_expand_color_override.
    fn get_expand_color_override(&self, ctx: &EngineCtx, id: CharId) -> Option<ColorPair> {
        if ctx.terminal.config.existing_color_handling != ExistingColorHandling::Dynamic || !self.expanding {
            return None;
        }
        let ch = &ctx.terminal.arena[id.0 as usize];
        if ch.animation.input_fg_color.is_none() && ch.animation.input_bg_color.is_some() {
            return Some(ColorPair::new(None, ch.animation.input_bg_color.clone()));
        }
        if !Self::has_input_colors(ctx, id) {
            return Some(ColorPair::default());
        }
        None
    }

    /// SpotlightsIterator.make_spotlights.
    fn make_spotlights(&mut self, ctx: &mut EngineCtx, num_spotlights: i64) -> Result<Vec<CharId>, EngineError> {
        let mut spotlights: Vec<CharId> = Vec::new();
        let minimum_distance = floor_div(ctx.terminal.canvas.right, 4);
        for _ in 0..num_spotlights {
            let spawn_coord = ctx.terminal.canvas.random_coord(&mut ctx.rng, true, false);
            let spotlight = ctx.terminal.add_character("O", spawn_coord);
            spotlights.push(spotlight);

            let mut spotlight_target_coords: Vec<Coord> = Vec::new();
            let mut last_coord = ctx.terminal.canvas.random_coord(&mut ctx.rng, false, false);
            spotlight_target_coords.push(last_coord);
            for _ in 0..10 {
                let next_coord = Self::find_coord_at_minimum_distance(ctx, last_coord, minimum_distance);
                spotlight_target_coords.push(next_coord);
                last_coord = next_coord;
            }

            let mut paths: Vec<String> = Vec::new();
            for coord in spotlight_target_coords {
                let speed = ctx.rng.uniform(self.config.search_speed_range.0, self.config.search_speed_range.1);
                let path_id = paths.len().to_string();
                let path_id = {
                    let ch = &mut ctx.terminal.arena[spotlight.0 as usize];
                    ch.motion
                        .new_path(speed, Some(Easing::InOutQuad), None, 0, false, &path_id)
                        .map_err(EngineError::Other)?
                };
                let bezier_control = ctx.terminal.canvas.random_coord(&mut ctx.rng, true, false);
                {
                    let ch = &mut ctx.terminal.arena[spotlight.0 as usize];
                    ch.motion
                        .paths
                        .get_mut(&path_id)
                        .unwrap()
                        .new_waypoint(coord, Some(vec![bezier_control]), "")
                        .map_err(EngineError::Other)?;
                }
                paths.push(path_id);
            }
            ctx.chain_paths(spotlight, &paths, true).map_err(EngineError::Other)?;

            let canvas_center = ctx.terminal.canvas.center;
            let ch = &mut ctx.terminal.arena[spotlight.0 as usize];
            let center_path = ch
                .motion
                .new_path(0.5, Some(Easing::InOutSine), None, 0, false, "center")
                .map_err(EngineError::Other)?;
            ch.motion
                .paths
                .get_mut(&center_path)
                .unwrap()
                .new_waypoint(canvas_center, None, "")
                .map_err(EngineError::Other)?;
        }
        Ok(spotlights)
    }

    /// SpotlightsIterator.find_coord_at_minimum_distance.
    fn find_coord_at_minimum_distance(ctx: &mut EngineCtx, origin_coord: Coord, minimum_distance: i64) -> Coord {
        loop {
            let coord = ctx.terminal.canvas.random_coord(&mut ctx.rng, false, false);
            let distance = geometry::find_length_of_line(origin_coord, coord, false);
            if distance >= minimum_distance as f64 {
                return coord;
            }
        }
    }

    /// SpotlightsIterator.illuminate_chars.
    fn illuminate_chars(&mut self, ctx: &mut EngineCtx, range_: i64) {
        let state = (range_, self.expanding, ctx.terminal.config.existing_color_handling);
        let unchanged = self.lighting_cache
            && self.lighting_stamp.is_some()
            && self.lighting_stamp == ctx.terminal.lighting_stamp()
            && self.lighting_state == Some(state)
            && self.spotlight_coords.len() == self.spotlights.len()
            && self
                .spotlight_coords
                .iter()
                .zip(&self.spotlights)
                .all(|(&coord, id)| coord == ctx.terminal.arena[id.0 as usize].motion.current_coord);
        if unchanged {
            return;
        }
        self.lighting_state = Some(state);
        let mut chars_in_range = std::mem::take(&mut self.illuminated_scratch);
        chars_in_range.clear();
        self.spotlight_coords.clear();
        self.spotlight_coords
            .extend(self.spotlights.iter().map(|id| ctx.terminal.arena[id.0 as usize].motion.current_coord));
        for &current_coord in &self.spotlight_coords {
            for coord in geometry::coords_in_circle(current_coord, range_) {
                if let Some(id) = ctx.terminal.get_character_by_input_coord(coord) {
                    if Self::is_spotlightable(ctx, id) {
                        chars_in_range.insert(id);
                    }
                }
            }
        }
        for id in self.illuminated_chars.iter().filter(|id| !chars_in_range.contains(id)) {
            let expand_override = self.get_expand_color_override(ctx, id);
            let colors = match expand_override {
                None => self.character_color_map[id.0 as usize].as_ref().unwrap().1.clone(),
                Some(overridden) => overridden,
            };
            let ch = &mut ctx.terminal.arena[id.0 as usize];
            let uses_pre = ch.uses_input_preexisting_colors;
            ch.animation.set_appearance(&ch.input_symbol, uses_pre, None, Some(colors));
        }

        for id in &chars_in_range {
            let input_coord = ctx.terminal.arena[id.0 as usize].input_coord;
            let distance = self
                .spotlight_coords
                .iter()
                .map(|&current_coord| self.distances.distance(current_coord, input_coord))
                .fold(f64::INFINITY, f64::min);

            let adjusted_color = if distance > range_ as f64 * (1.0 - self.config.beam_falloff) {
                let brightness_factor = (1.0
                    - (distance - range_ as f64 * (1.0 - self.config.beam_falloff))
                        / (range_ as f64 * self.config.beam_falloff))
                    .max(0.2);
                Self::adjust_color_pair_brightness(
                    &self.character_color_map[id.0 as usize].as_ref().unwrap().0,
                    brightness_factor,
                )
            } else {
                self.character_color_map[id.0 as usize].as_ref().unwrap().0.clone()
            };
            let expand_override = self.get_expand_color_override(ctx, id);
            let colors = match expand_override {
                None => adjusted_color,
                Some(overridden) => overridden,
            };
            let ch = &mut ctx.terminal.arena[id.0 as usize];
            let uses_pre = ch.uses_input_preexisting_colors;
            ch.animation.set_appearance(&ch.input_symbol, uses_pre, None, Some(colors));
        }
        self.illuminated_scratch = std::mem::replace(&mut self.illuminated_chars, chars_in_range);
    }
}

impl EffectHooks for Spotlights {
    fn dispatch_callback(&mut self, _ctx: &mut EngineCtx, _character: CharId, _callback: &EffectCallback) {}
}

impl Effect for Spotlights {
    fn build(&mut self, ctx: &mut EngineCtx) -> Result<(), EngineError> {
        self.lighting_stamp = None;
        self.lighting_state = None;
        // SpotlightsIterator.DYNAMIC_NEUTRAL_GRAY
        let dynamic_neutral_gray = Color::from_hex("#808080").unwrap();
        self.spotlights = self.make_spotlights(ctx, self.config.spotlight_count)?;
        self.distances = DistanceTable::new(ctx.terminal.canvas.right, ctx.terminal.canvas.top);
        let final_gradient =
            Gradient::new(&self.config.final_gradient_stops, &self.config.final_gradient_steps, false, false)
                .map_err(EngineError::Other)?;
        let final_gradient_mapping = final_gradient
            .build_coordinate_color_mapping(
                ctx.terminal.canvas.text_bottom,
                ctx.terminal.canvas.text_top,
                ctx.terminal.canvas.text_left,
                ctx.terminal.canvas.text_right,
                self.config.final_gradient_direction,
            )
            .map_err(EngineError::Other)?;
        self.character_color_map.clear();
        self.character_color_map.resize_with(ctx.terminal.arena.len(), || None);
        let dynamic = ctx.terminal.config.existing_color_handling == ExistingColorHandling::Dynamic;
        let characters = {
            let filter = CharacterFilter::default();
            ctx.terminal.get_characters(&mut ctx.rng, filter, CharacterSort::TopToBottomLeftToRight)
        };
        for &id in &characters {
            let (input_coord, input_fg, input_bg) = {
                let ch = &ctx.terminal.arena[id.0 as usize];
                (ch.input_coord, ch.animation.input_fg_color.clone(), ch.animation.input_bg_color.clone())
            };
            let (bright_pair, dark_pair);
            if dynamic {
                if input_fg.is_some() || input_bg.is_some() {
                    let mut bright_fg = input_fg.clone();
                    if bright_fg.is_none() && input_bg.is_some() {
                        bright_fg = Some(dynamic_neutral_gray.clone());
                    }
                    bright_pair = ColorPair::new(bright_fg.clone(), input_bg.clone());
                    dark_pair = ColorPair::new(
                        bright_fg.as_ref().map(|fg| Animation::adjust_color_brightness(fg, 0.2)),
                        input_bg.as_ref().map(|bg| Animation::adjust_color_brightness(bg, 0.2)),
                    );
                } else {
                    bright_pair = ColorPair::new(Some(dynamic_neutral_gray.clone()), None);
                    dark_pair =
                        ColorPair::new(Some(Animation::adjust_color_brightness(&dynamic_neutral_gray, 0.2)), None);
                }
            } else {
                let color_bright = final_gradient_mapping.get(&input_coord).unwrap().clone();
                dark_pair = ColorPair::new(Some(Animation::adjust_color_brightness(&color_bright, 0.2)), None);
                bright_pair = ColorPair::new(Some(color_bright), None);
            }
            ctx.terminal.set_character_visibility(id, true);
            self.character_color_map[id.0 as usize] = Some((bright_pair, dark_pair.clone()));
            let ch = &mut ctx.terminal.arena[id.0 as usize];
            let input_symbol = ch.input_symbol.clone();
            let uses_pre = ch.uses_input_preexisting_colors;
            ch.animation.set_appearance(&input_symbol, uses_pre, Some(&input_symbol.clone()), Some(dark_pair));
        }
        let smallest_dimension = std::cmp::min(ctx.terminal.canvas.right, ctx.terminal.canvas.top);
        // int(min(smallest // ratio, smallest)) — float floor division then truncation
        self.illuminate_range = std::cmp::max(
            (smallest_dimension as f64 / self.config.beam_width_ratio).floor().min(smallest_dimension as f64) as i64,
            1,
        );
        self.search_duration = self.config.search_duration;
        self.searching = true;
        self.expanding = false;
        self.complete = false;
        for &spotlight in &self.spotlights.clone() {
            ctx.activate_path(self, spotlight, "0");
            ctx.active_characters.insert(spotlight);
        }
        Ok(())
    }

    fn next_frame(&mut self, ctx: &mut EngineCtx) -> Option<crate::engine::terminal::FrameOutput> {
        if !self.complete {
            self.illuminate_chars(ctx, self.illuminate_range);
            if self.searching {
                self.search_duration -= 1;
                if self.search_duration == 0 {
                    for &spotlight in &self.spotlights.clone() {
                        ctx.activate_path(self, spotlight, "center");
                    }
                    self.searching = false;
                }
            }
            if !self
                .spotlights
                .iter()
                .any(|&spotlight| ctx.terminal.arena[spotlight.0 as usize].motion.active_path.is_some())
            {
                while self.spotlights.len() > 1 {
                    self.spotlights.pop();
                }
                self.expanding = true;
                self.illuminate_range += 1;
                let limit = (std::cmp::max(ctx.terminal.canvas.right, ctx.terminal.canvas.top) as f64 / 1.5).floor();
                if self.illuminate_range as f64 > limit {
                    self.complete = true;
                }
            }

            // Extra active characters supplied by library callers can change
            // input appearances after illumination. Their next lighting pass
            // must run even if the beam positions stay fixed.
            let beams_only = ctx.active_characters.iter().all(|id| self.spotlights.contains(&id));
            ctx.update(self);
            let frame = ctx.frame();
            self.lighting_stamp = if beams_only { ctx.terminal.lighting_stamp() } else { None };
            return Some(frame);
        }
        None
    }
}

#[cfg(test)]
mod distance_tests {
    use super::*;

    #[test]
    fn cached_and_fallback_distances_match_hypot_bit_for_bit() {
        let table = DistanceTable::new(200, 50);
        for row in -70..=70 {
            for column in -250..=250 {
                let origin = Coord::new(31, -9);
                let target = Coord::new(origin.column + column, origin.row + row);
                assert_eq!(
                    table.distance(origin, target).to_bits(),
                    geometry::find_length_of_line(origin, target, true).to_bits()
                );
            }
        }
        let bounded = DistanceTable::new(i64::MAX, i64::MAX);
        assert!(bounded.values.len() <= 262_144);
        let empty = DistanceTable::default();
        assert_eq!(empty.distance(Coord::new(0, 0), Coord::new(3, 2)), 5.0);
    }

    #[test]
    fn unchanged_lighting_respects_external_edits_rendering_maps_and_other_animations() {
        use crate::cli::Cli;
        use crate::effects::EffectCommand;
        use crate::engine::animation::VisualParams;
        use crate::engine::ctx::{Clock, NoopHooks};
        use crate::engine::terminal::TerminalConfig;
        use crate::utils::rng::Rng;
        use clap::Parser;

        let build = |cached| {
            let cli = Cli::try_parse_from([
                "ttfx",
                "spotlights",
                "--search-duration",
                "45",
                "--search-speed-range",
                "0.001-0.001",
                "--spotlight-count",
                "1",
            ])
            .unwrap();
            let Some(EffectCommand::Spotlights(config)) = cli.effect else { panic!("wrong effect") };
            let mut effect = Spotlights::new(config);
            effect.lighting_cache = cached;
            let mut ctx = EngineCtx::new(
                "ABCDEFG\nHIJKLMN\nOPQRSTU",
                TerminalConfig {
                    canvas_width: 20,
                    canvas_height: 8,
                    ignore_terminal_dimensions: true,
                    ..Default::default()
                },
                Rng::seeded(17),
                Clock::virtual_with_frame_rate(60),
            )
            .unwrap();
            effect.build(&mut ctx).unwrap();
            (effect, ctx)
        };
        let (mut fast, mut a) = build(true);
        let (mut slow, mut b) = build(false);
        let coord = a.terminal.arena[0].input_coord;
        let mut last_output = String::new();
        let mut repeated = 0;
        let mut reused_lighting = 0;
        for frame in 0..180 {
            for ctx in [&mut a, &mut b] {
                match frame {
                    5 => ctx.terminal.arena[0].animation.set_appearance("A", false, Some("!"), None),
                    9 => {
                        ctx.terminal.arena[1].animation.set_appearance("B", false, Some("?"), None);
                        // Rendering consumes dirty flags but must not hide the edit.
                        ctx.terminal.get_formatted_output_string();
                    }
                    13 => {
                        ctx.terminal.character_by_input_coord.remove(&coord);
                    }
                    17 => {
                        ctx.terminal.character_by_input_coord.insert(coord, CharId(0));
                    }
                    21 => ctx.terminal.arena = ctx.terminal.arena.clone(),
                    25 => ctx.terminal.character_by_input_coord = ctx.terminal.character_by_input_coord.clone(),
                    29 => ctx.terminal.config.existing_color_handling = ExistingColorHandling::Dynamic,
                    33 => {
                        let animation = &mut ctx.terminal.arena[2].animation;
                        animation.new_scene(false, None, None, "external", false);
                        for symbol in ["0", "1", "2"] {
                            animation
                                .scenes
                                .get_mut("external")
                                .unwrap()
                                .add_frame(symbol, 3, VisualParams::default())
                                .unwrap();
                        }
                        ctx.activate_scene(&mut NoopHooks, CharId(2), "external");
                        ctx.active_characters.insert(CharId(2));
                    }
                    _ => {}
                }
            }
            let previous_stamp = a.terminal.lighting_stamp();
            let x = fast.next_frame(&mut a);
            reused_lighting += usize::from(previous_stamp.is_some() && previous_stamp == a.terminal.lighting_stamp());
            let y = slow.next_frame(&mut b);
            assert_eq!(x.is_some(), y.is_some());
            let (Some(x), Some(y)) = (x, y) else { break };
            // Read cached row output through the normal compatibility helper.
            a.terminal.recycle_frame(x);
            b.terminal.recycle_frame(y);
            let x = a.terminal.get_formatted_output_string();
            let y = b.terminal.get_formatted_output_string();
            assert_eq!(x, y, "frame {frame}");
            repeated += usize::from(x == last_output);
            last_output = x;
        }
        assert!(repeated > 10);
        assert!(reused_lighting > 10, "the cached branch must actually be exercised");
    }
}
