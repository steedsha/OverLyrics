//! Source-accurate preparation shared by replay and live rendering.
use oly_contracts::{AmllLine,LyricDoc,LineDisplayOptions,doc_to_lines};
pub fn prepare_lines(doc:&LyricDoc)->Vec<AmllLine> {
 let mut lines=doc_to_lines(doc,LineDisplayOptions{show_translation:true,show_romanization:true});
 oly_contracts::optimize_lyric_lines(&mut lines,&Default::default());
 lines
}
#[cfg(test)] mod tests {
 use super::*;
 #[test] fn runtime_preparation_advances_the_row_but_not_its_words() {
 let doc:LyricDoc=serde_json::from_value(serde_json::json!({"lines":[{"text":"First","start":1000,"end":2000,"words":[{"text":"First","start":1000,"end":2000}]},{"text":"Next","start":4000,"end":5000,"words":[{"text":"Next","start":4000,"end":5000}]}]})).unwrap();
 let lines=prepare_lines(&doc);
 assert_eq!(lines[0].start_time,400.);assert_eq!(lines[1].start_time,3400.);
 assert_eq!(lines[1].words[0].start_time,4000);
 }
}

use super::model::{Row,Box4};
use oly_core::timeline::{Timeline,LineWindow,SeekDetector,TimelineSnapshot,TimelineDiff};
/// Visual main/BG lyric group mirroring AMLL `LyricLineGroup`.
#[derive(Clone,Debug,PartialEq)]
pub struct LyricGroup {
 pub main_row:usize,
 pub bg_row:Option<usize>,
 pub start_ms:f64,
 pub end_ms:f64,
 pub is_bg_first:bool,
 pub is_duet:bool,
}
/// Timeline IDs are visual main/BG groups, never translation `_srcIndex` IDs.
pub struct LyricTimeline { timeline:Timeline, seek_detector:SeekDetector, groups:Vec<LyricGroup>, row_groups:Vec<usize>, first_rows:Vec<usize> }
pub struct Frame {pub focal:usize,pub active:Vec<bool>,pub highlighted:Vec<bool>,pub groups:Vec<usize>,pub latest:usize}
pub struct TimelineFrame {
 pub snapshot:TimelineSnapshot,
 pub diff:TimelineDiff,
 pub latest_group:usize,
 pub active_groups:Vec<bool>,
 pub highlighted_groups:Vec<bool>,
}
impl Default for LyricTimeline {fn default()->Self{Self::new(&[])}}
impl LyricTimeline {
 pub fn new(rows:&[Row])->Self {
  let mut bounds:Vec<LineWindow>=Vec::new();let mut row_groups=Vec::new();let mut first_rows=Vec::new();let mut groups:Vec<LyricGroup>=Vec::new();
  for (i,row) in rows.iter().enumerate(){
   if !row.is_bg || groups.is_empty() || groups.last().is_some_and(|g|g.bg_row.is_some()){
    bounds.push(LineWindow::new(row.start_ms,row.end_ms));first_rows.push(i);
    groups.push(LyricGroup{main_row:i,bg_row:None,start_ms:row.start_ms,end_ms:row.end_ms,is_bg_first:false,is_duet:row.is_duet});
   } else if let Some(g)=groups.last_mut() {
    g.bg_row=Some(i);
    let main_word_start=rows[g.main_row].words.first().map(|w|w.1 as f64).unwrap_or(rows[g.main_row].start_ms);
    let bg_word_start=row.words.first().map(|w|w.1 as f64).unwrap_or(row.start_ms);
    g.is_bg_first=bg_word_start<main_word_start;
    g.start_ms=g.start_ms.min(row.start_ms);g.end_ms=g.end_ms.max(row.end_ms);
   }
   let g=bounds.len()-1;bounds[g].start_ms=bounds[g].start_ms.min(row.start_ms);bounds[g].end_ms=bounds[g].end_ms.max(row.end_ms);row_groups.push(g);
  }
  Self{timeline:Timeline::new(bounds),seek_detector:SeekDetector::new(),groups,row_groups,first_rows}
 }
 pub fn groups(&self)->&[LyricGroup] {&self.groups}
 pub fn group_start_interval(&self,group:usize)->Option<f64> {
  if group>0 && group<self.groups.len() {Some(self.groups[group].start_ms-self.groups[group-1].start_ms)}else{None}
 }
 pub fn reset_seek(&mut self) {self.seek_detector.reset();}
 pub fn sync_at(&mut self,time:f64,is_playing:bool,wall_ms:f64,force_seek:bool)->TimelineFrame {
  let seek=force_seek || self.seek_detector.detect(time,is_playing,wall_ms);
  let diff=self.timeline.sync(time,seek);
  let s=self.timeline.snapshot().clone();
  let latest_group=s.latest_highlighted_index.unwrap_or(s.scroll_to_index);
  let active_groups=(0..self.groups.len()).map(|g|s.highlighted.contains(&g)||(g>=s.scroll_to_index&&g<latest_group)).collect();
  let highlighted_groups=(0..self.groups.len()).map(|g|s.highlighted.contains(&g)).collect();
  TimelineFrame{snapshot:s,diff,latest_group,active_groups,highlighted_groups}
 }
 pub fn sync(&mut self,time:f64)->Frame {
  self.timeline.sync(time,false);let s=self.timeline.snapshot();let latest=s.latest_highlighted_index.unwrap_or(s.scroll_to_index);
  Frame{focal:self.first_rows.get(s.scroll_to_index).copied().unwrap_or(0),latest,
   groups:self.row_groups.clone(),
   active:self.row_groups.iter().map(|g|s.highlighted.contains(g)||(*g>=s.scroll_to_index&&*g<latest)).collect(),
   highlighted:self.row_groups.iter().map(|g|s.highlighted.contains(g)).collect()}
 }
}
/// AMLL reserves 15% opposite margin when a chart contains duet vocals.
pub fn row_bounds(column:Box4,align_right:bool,is_duet:bool,has_duet:bool)->(Box4,bool){
 let right=align_right^is_duet;let width=column.w*if has_duet{0.85}else{1.0};
 (Box4::new(column.x+if right{column.w-width}else{0.},column.y,width,column.h),right)
}

/// Inputs for AMLL's manual-browsing auto-resume check.
#[derive(Clone, Copy, Debug)]
pub struct AutoResumeInput {
    pub is_direct: bool,
    pub can_resume_after_idle: bool,
    pub focus_on_interlude: bool,
    pub prev_scroll_to: usize,
    pub prev_focus_interlude: bool,
    pub prev_end_of_song: bool,
    pub scroll_to_index: usize,
    pub is_end_of_song: bool,
    pub has_bottom_content: bool,
    pub playing_contains_target: bool,
    pub target_in_render_range: bool,
}

/// Evaluates AMLL's joint condition before ending manual scroll lock.
pub fn can_auto_resume_browsing(input: AutoResumeInput) -> bool {
    if input.is_direct || !input.can_resume_after_idle || input.focus_on_interlude {
        return false;
    }
    let focus_changed = input.prev_scroll_to != input.scroll_to_index
        || (input.prev_focus_interlude && !input.focus_on_interlude)
        || (!input.prev_end_of_song && input.is_end_of_song && input.has_bottom_content);
    let target_active =
        (input.is_end_of_song && input.has_bottom_content) || input.playing_contains_target;
    focus_changed && target_active && input.target_in_render_range
}
#[cfg(test)] mod group_tests {
 use super::*;
 fn row(bg:bool,start:f64,end:f64)->Row {Row{text:"x".into(),words:vec![("x".into(),start as i64,end as i64)],start_ms:start,end_ms:end,is_bg:bg,is_duet:false,word_timed:true,group:0,translation:String::new(),romanization:String::new()}}
 #[test] fn background_never_steals_focus_and_overlap_lingers_until_next_group(){
 let rows=[row(false,0.,1000.),row(true,0.,1000.),row(false,500.,1500.),row(false,3000.,4000.)];
 let mut timeline=LyricTimeline::new(&rows);
 assert_eq!(timeline.sync(600.).focal,0);
 let frame=timeline.sync(1100.);assert_eq!(frame.focal,0);assert!(frame.active[0]&&frame.active[1]&&frame.active[2]);
 assert_eq!(timeline.sync(3100.).focal,3);
 assert_eq!(timeline.sync(600.).focal,0);
 }
 #[test] fn duet_side_is_mirrored_in_both_alignment_preferences(){
 let column=Box4::new(100.,0.,600.,500.);
 for align in [false,true]{let(a,ar)=row_bounds(column,align,false,true);let(b,br)=row_bounds(column,align,true,true);assert_ne!(ar,br);assert_eq!(a.w,510.);assert_eq!(b.w,510.);assert_eq!(if ar{a.right()}else{b.right()},column.right());}
 }
}

#[cfg(test)] mod duet_projection_tests {
 use super::*;
 #[test] fn real_document_singer_sides_and_background_survive_runtime_projection(){
 let doc:LyricDoc=serde_json::from_value(serde_json::json!({"lines":[
 {"text":"Left","singer":"a","start":1000,"end":2000},
 {"text":"Right","singer":"b","bg":"Harmony","start":4000,"end":5000},
 {"text":"Left again","singer":"a","start":6000,"end":7000}]})).unwrap();
 let mut lines=prepare_lines(&doc);
 let bg=lines.iter_mut().find(|l|l.is_bg).unwrap();bg.is_duet=false;
 let rows=super::super::model::rows_from(&lines);
 assert!(!rows[0].is_duet);assert!(rows[1].is_duet);assert!(rows[2].is_bg&&rows[2].is_duet);assert!(!rows[3].is_duet);
 }


 #[test]
 fn is_bg_first_detects_early_harmony_and_post_harmony() {
  let rows = [
   Row {
    text: "Main A".into(),
    words: vec![("Main A".into(), 1000, 2000)],
    start_ms: 400.0,
    end_ms: 2000.0,
    is_bg: false,
    is_duet: false,
    word_timed: true,
    group: 0,
    translation: String::new(),
    romanization: String::new(),
   },
   Row {
    text: "Early BG".into(),
    words: vec![("Early BG".into(), 800, 1500)],
    start_ms: 200.0,
    end_ms: 1500.0,
    is_bg: true,
    is_duet: false,
    word_timed: true,
    group: 0,
    translation: String::new(),
    romanization: String::new(),
   },
   Row {
    text: "Main B".into(),
    words: vec![("Main B".into(), 3000, 4000)],
    start_ms: 2400.0,
    end_ms: 4000.0,
    is_bg: false,
    is_duet: false,
    word_timed: true,
    group: 1,
    translation: String::new(),
    romanization: String::new(),
   },
   Row {
    text: "Late BG".into(),
    words: vec![("Late BG".into(), 3500, 4500)],
    start_ms: 2900.0,
    end_ms: 4500.0,
    is_bg: true,
    is_duet: false,
    word_timed: true,
    group: 1,
    translation: String::new(),
    romanization: String::new(),
   },
  ];
  let timeline = LyricTimeline::new(&rows);
  assert_eq!(timeline.groups().len(), 2);
  assert!(timeline.groups()[0].is_bg_first);
  assert!(!timeline.groups()[1].is_bg_first);
  assert!((timeline.group_start_interval(1).unwrap() - 2200.0).abs() < 1e-6);
 }

 #[test]
 fn synthetic_duet_retains_per_row_translations_and_detects_duet_interlude() {
  let doc: LyricDoc = serde_json::from_value(serde_json::json!({
   "lines": [
    { "text": "Lead line", "trans": "主唱译文", "singer": "v1", "bg": "Echo", "bgTrans": "和声译文", "start": 1000, "end": 4000 },
    { "text": "Duet bridge", "trans": "合唱译文", "singer": "v2", "start": 20000, "end": 24000 }
   ]
  })).unwrap();
  let lines = prepare_lines(&doc);
  let rows = super::super::model::rows_from(&lines);
  let mut timeline = LyricTimeline::new(&rows);

  let both_translated = timeline.groups().iter().any(|g| {
   g.bg_row.is_some_and(|bi| {
    !rows[g.main_row].translation.is_empty() && !rows[bi].translation.is_empty()
   })
  });
  assert!(both_translated, "main and bg rows must keep their own translations");

  let step = timeline.sync_at(10_000.0, true, 1_000.0, true);
  assert!(step.diff.is_time_jumped);
  let interlude = step
   .snapshot
   .active_interlude
   .expect("gap [4000, 20000) should expose an interlude");
  assert!(interlude.end_ms - interlude.start_ms >= 7_000.0);
  let next_group_idx = ((interlude.anchor_line_index + 1).max(0) as usize)
   .min(timeline.groups().len() - 1);
  assert!(
   timeline.groups()[next_group_idx].is_duet,
   "bridge after interlude belongs to duet vocalist so dots follow duet side"
  );

  let interlude_policy = oly_core::scroll::pos_y_spring_policy(
   false,
   true,
   timeline.group_start_interval(next_group_idx),
   false,
  );
  let normal_policy = oly_core::scroll::pos_y_spring_policy(false, false, Some(200.0), false);
  assert!(normal_policy.stiffness > interlude_policy.stiffness);
 }

 #[test]
 fn synthetic_chart_switches_interlude_and_bottom_focus() {
  let doc: LyricDoc = serde_json::from_value(serde_json::json!({
   "credits": ["Sample Writer"],
   "lines": [
    { "text": "Intro ends here", "start": 8000, "end": 11000 },
    { "text": "Closing line", "start": 12000, "end": 15000 }
   ]
  })).unwrap();
  assert!(
   !oly_contracts::credit_names(&doc).is_empty(),
   "synthetic chart has songwriter credits for bottom line"
  );
  let lines = prepare_lines(&doc);
  let rows = super::super::model::rows_from(&lines);
  let mut timeline = LyricTimeline::new(&rows);
  let mut focus = oly_core::timeline::FocusController::new();
  let group_count = timeline.groups().len();

  let intro = timeline.sync_at(100.0, true, 16.0, false);
  assert!(intro.snapshot.focus_on_interlude);
  assert_eq!(
   focus.resolve(&intro.snapshot, group_count, false, true, true),
   oly_core::timeline::FocalTarget::Interlude(-1)
  );

  let exit_time = timeline.groups()[0].start_ms + 700.0;
  let first_line = timeline.sync_at(exit_time, true, 1000.0, true);
  assert!(!first_line.snapshot.focus_on_interlude);
  assert_eq!(
   focus.resolve(&first_line.snapshot, group_count, false, true, false),
   oly_core::timeline::FocalTarget::Line(first_line.snapshot.scroll_to_index)
  );

  let end_time = timeline.groups().last().unwrap().end_ms + 500.0;
  let outro = timeline.sync_at(end_time, true, 2000.0, true);
  assert!(outro.snapshot.is_end_of_song);
  assert_eq!(
   focus.resolve(&outro.snapshot, group_count, false, true, false),
   oly_core::timeline::FocalTarget::Bottom
  );
 }

 #[test]
 fn joint_auto_resume_requires_idle_non_interlude_changed_active_visible_target() {
  let base = AutoResumeInput {
   is_direct: false,
   can_resume_after_idle: true,
   focus_on_interlude: false,
   prev_scroll_to: 3,
   prev_focus_interlude: false,
   prev_end_of_song: false,
   scroll_to_index: 4,
   is_end_of_song: false,
   has_bottom_content: true,
   playing_contains_target: true,
   target_in_render_range: true,
  };
  assert!(can_auto_resume_browsing(base));
  assert!(!can_auto_resume_browsing(AutoResumeInput { is_direct: true, ..base }));
  assert!(!can_auto_resume_browsing(AutoResumeInput { can_resume_after_idle: false, ..base }));
  assert!(!can_auto_resume_browsing(AutoResumeInput { focus_on_interlude: true, ..base }));
  assert!(!can_auto_resume_browsing(AutoResumeInput { scroll_to_index: 3, ..base }));
  assert!(!can_auto_resume_browsing(AutoResumeInput { playing_contains_target: false, ..base }));
  assert!(!can_auto_resume_browsing(AutoResumeInput { target_in_render_range: false, ..base }));
  assert!(can_auto_resume_browsing(AutoResumeInput {
   scroll_to_index: 3,
   prev_focus_interlude: true,
   ..base
  }));
  assert!(can_auto_resume_browsing(AutoResumeInput {
   scroll_to_index: 3,
   is_end_of_song: true,
   playing_contains_target: false,
   ..base
  }));
 }
}

#[cfg(test)] mod live_preroll_regression {
 use super::*;
 #[test] fn prepared_live_rows_focus_next_line_before_its_first_word() {
 let doc:LyricDoc=serde_json::from_value(serde_json::json!({"lines":[
 {"text":"One","start":1000,"end":2000,"words":[{"text":"One","start":1000,"end":2000}]},
 {"text":"Two","start":4000,"end":5000,"words":[{"text":"Two","start":4000,"end":5000}]}]})).unwrap();
 let rows=super::super::model::rows_from(&prepare_lines(&doc));let mut timeline=LyricTimeline::new(&rows);
 let f=timeline.sync_at(3500.,true,0.,true);
 assert_eq!(f.snapshot.scroll_to_index,1);assert!(f.highlighted_groups[1]);assert_eq!(rows[1].words[0].1,4000);
 }
}
