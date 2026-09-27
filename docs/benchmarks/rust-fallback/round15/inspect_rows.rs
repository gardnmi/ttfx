use clap::Parser;
use std::rc::Rc;
use ttfx::cli::Cli;
use ttfx::engine::animation::CharacterVisual;
use ttfx::engine::ctx::{Clock, EngineCtx};
use ttfx::engine::terminal::TerminalConfig;
use ttfx::utils::rng::Rng;

fn main() {
 let input=std::fs::read_to_string("docs/benchmarks/rust-fallback/pr/input.txt").unwrap();
 println!("effect,frames,changed_rows,changed_cells,full_row_cells,span_cells,same_length_cells,same_length_rows");
 for name in std::env::args().skip(1) {
  let cli=Cli::try_parse_from(["ttfx",name.as_str()]).unwrap();
  let mut effect=cli.effect.unwrap().build_effect();
  let mut ctx=EngineCtx::new(&input,TerminalConfig { canvas_width:200,canvas_height:50,frame_rate:0,ignore_terminal_dimensions:true,..Default::default() },Rng::seeded(1),Clock::virtual_with_frame_rate(60)).unwrap();
  effect.build(&mut ctx).unwrap();
  let (mut frames,mut rows,mut changed,mut full,mut spans,mut same,mut same_rows)=(0u64,0u64,0u64,0u64,0u64,0u64,0u64);
  let mut previous:Vec<Option<Rc<CharacterVisual>>>=Vec::new();
  let mut winners=Vec::new();
  while let Some(_frame)=effect.next_frame(&mut ctx) {
   frames+=1;
   let term=&ctx.terminal;
   let width=term.visible_right.max(0) as usize;
   let height=term.visible_top.max(0) as usize;
   previous.resize(width*height,None);
   winners.resize(width*height,usize::MAX); winners.fill(usize::MAX);
   let chars:&[_]=&term.arena;
   for (id,ch) in chars.iter().enumerate() {
    let row=ch.motion.current_coord.row+term.canvas_row_offset;
    let col=ch.motion.current_coord.column+term.canvas_column_offset;
    if !ch.is_visible || row<term.visible_bottom.max(1) || row>term.visible_top || col<term.visible_left.max(1) || col>term.visible_right { continue; }
    let cell=(row as usize-1)*width+col as usize-1;
    let old=winners[cell];
    if old==usize::MAX || (ch.layer,ch.character_id)>(chars[old].layer,chars[old].character_id) { winners[cell]=id; }
   }
   for row in 0..height {
    let mut first=width; let mut last=0; let mut all_same=true;
    for col in 0..width {
     let cell=row*width+col;
     let visual=(winners[cell]!=usize::MAX).then(||&chars[winners[cell]].animation.current_character_visual);
     let bytes=visual.map_or(" ",|v|v.formatted_symbol.as_str());
     let old=previous[cell].as_ref().map_or(" ",|v|v.formatted_symbol.as_str());
     if bytes!=old {
      first=first.min(col);last=col;changed+=1;
      if bytes.len()==old.len() { same+=1; } else { all_same=false; }
     }
     previous[cell]=visual.cloned();
    }
    if first<width { rows+=1;full+=width as u64;spans+=(last-first+1) as u64;if all_same { same_rows+=1; } }
   }
  }
  println!("{name},{frames},{rows},{changed},{full},{spans},{same},{same_rows}");
 }
}
