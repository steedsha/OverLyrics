//! Compact control geometry and side-window placement, independent of the GPU.
use super::model::Box4;
#[derive(Clone,Copy,Debug,PartialEq)] pub enum PanelSide { Left, Right }
pub fn side_panel(host:Box4,area:Box4,width:f32)->(Box4,PanelSide){
 side_panel_sized(host,area,width,650.)
}
pub fn side_panel_sized(host:Box4,area:Box4,width:f32,height:f32)->(Box4,PanelSide){
 let width=width.min(area.w).max(1.);let h=height.min(area.h).max(1.);
 let left=host.x-12.-width;let right=host.right()+12.;
 let side=if left>=area.x {PanelSide::Left}else{PanelSide::Right};
 let x=match side{PanelSide::Left=>left,PanelSide::Right=>right}.clamp(area.x,(area.right()-width).max(area.x));
 (Box4::new(x,host.y.clamp(area.y,(area.bottom()-h).max(area.y)),width,h),side)
}
pub fn compact_cover_radius(configured:f32)->f32{(configured*11./20.).clamp(0.,11.)}
pub fn fold_visible(cursor:Option<(f32,f32)>,w:f32,h:f32)->bool{
 cursor.is_some_and(|(x,y)|x>=0.&&x<w&&y>=h-24.&&y<h)
}
pub fn fold_hit_box(w:f32,h:f32)->Box4{
 Box4::new((w*0.5-42.).max(0.),(h-22.).max(0.),84.0_f32.min(w),22.0_f32.min(h))
}
pub fn switch_track(row:Box4)->Box4{Box4::new(row.right()-50.,row.y+((row.h-24.)*0.5).max(0.),42.,24.)}
pub fn switch_fraction(track:Box4,x:f32)->f32{
 let d=(track.h-2.).max(1.);((x-track.x-1.-d/2.)/(track.w-d-2.).max(1.)).clamp(0.,1.)
}
pub fn switch_knob(track:Box4,fraction:f32,press:f32)->Box4{
 let d=(track.h-2.).max(1.);let w=(d+4.*press.clamp(0.,1.)).min(track.w-2.);
 Box4::new(track.x+1.+(track.w-w-2.).max(0.)*fraction.clamp(0.,1.),track.y+1.,w,d)
}
#[derive(Clone,Copy)]pub struct SwitchDrag{pub index:usize,pub track:Box4,pub origin:f32,pub fraction:f32,pub moved:bool}
impl SwitchDrag{pub fn update(&mut self,x:f32){self.moved|=(x-self.origin).abs()>2.;if self.moved{self.fraction=switch_fraction(self.track,x);}}}
 #[cfg(test)]mod tests{
 use super::*;
 #[test]fn side_window_prefers_left_and_flips_at_work_area_edge(){
 let area=Box4::new(-1920.,0.,1920.,1040.);
 let(left,side)=side_panel(Box4::new(-1000.,100.,380.,650.),area,380.);assert_eq!(side,PanelSide::Left);assert_eq!(left.right()+12.,-1000.);
 let(right,side)=side_panel(Box4::new(-1900.,100.,380.,650.),area,380.);assert_eq!(side,PanelSide::Right);assert_eq!(right.x,-1508.);
 let(clamped,_)=side_panel(Box4::new(0.,800.,380.,650.),Box4::new(0.,0.,600.,900.),380.);assert!(clamped.right()<=600.&&clamped.bottom()<=900.);
 }
 #[test]fn switch_drags_reach_both_track_ends(){let t=Box4::new(100.,10.,42.,24.);let left=switch_knob(t,0.,0.);let right=switch_knob(t,1.,0.);assert_eq!(left.x,t.x+1.);assert_eq!(right.right(),t.right()-1.);assert_eq!(switch_fraction(t,left.centre().0),0.);assert_eq!(switch_fraction(t,right.centre().0),1.);let mut drag=SwitchDrag{index:0,track:t,origin:right.centre().0,fraction:1.,moved:false};drag.update(left.centre().0);assert!(drag.moved);assert_eq!(drag.fraction,0.);}
 #[test]fn compact_artwork_and_fold_follow_reference(){assert_eq!(compact_cover_radius(20.),11.);assert_eq!(compact_cover_radius(48.),11.);assert!(!fold_visible(None,380.,650.));assert!(!fold_visible(Some((190.,600.)),380.,650.));assert!(fold_visible(Some((190.,640.)),380.,650.));let fb=fold_hit_box(380.,650.);assert!(fb.contains(190.,644.));assert!(fb.w>32.&&fb.y>=626.);}
}
