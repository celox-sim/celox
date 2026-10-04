#![allow(dead_code)]
#[path="../../crates/ir/src/term.rs"] mod ir;
#[path="../../crates/solver/src/kernel.rs"] mod kernel;
extern crate self as hwverify_ir;
pub use ir::*;
#[derive(Clone,Debug,PartialEq,Eq)] enum V { B(bool), W(u64), M(Vec<u64>) }
fn w(v:V)->u64 {if let V::W(x)=v{x}else{panic!("word")}}
fn b(v:V)->bool {if let V::B(x)=v{x}else{panic!("bool")}}
fn mask(n:u32)->u64 {if n==64{u64::MAX}else{(1<<n)-1}}
fn signed(v:u64,n:u32)->i128 {if v&(1u64<<(n-1))!=0 {v as i128-(1i128<<n)}else{v as i128}}
fn eval(t:&Term,x:u64,y:u64,p:bool,m:&[u64])->V {
 let n=&t.0;let e=|i:usize|eval(&n.args[i],x,y,p,m);
 let width=if let Sort::Bv(n)=n.sort{n}else{0};
 match n.op.as_str(){
  "@x"=>V::W(x),"@y"=>V::W(y),"@p"=>V::B(p),"@m"=>V::M(m.to_vec()),"true"=>V::B(true),"false"=>V::B(false),
  "="=>V::B(e(0)==e(1)),"not"=>V::B(!b(e(0))),"and"=>V::B(b(e(0))&&b(e(1))),"or"=>V::B(b(e(0))||b(e(1))),"xor"=>V::B(b(e(0))^b(e(1))),"=>"=>V::B(!b(e(0))||b(e(1))),"ite"=>if b(e(0)){e(1)}else{e(2)},
  "bvnot"=>V::W(!w(e(0))&mask(width)),
  "bvadd"=>V::W(w(e(0)).wrapping_add(w(e(1)))&mask(width)),"bvsub"=>V::W(w(e(0)).wrapping_sub(w(e(1)))&mask(width)),"bvmul"=>V::W(w(e(0)).wrapping_mul(w(e(1)))&mask(width)),
  "bvand"=>V::W(w(e(0))&w(e(1))),"bvor"=>V::W(w(e(0))|w(e(1))),"bvxor"=>V::W(w(e(0))^w(e(1))),
  "bvshl"=>{let c=w(e(1));V::W(if c>=width as u64 {0}else{w(e(0)).wrapping_shl(c as u32)&mask(width)})},
  "bvlshr"=>{let c=w(e(1));V::W(if c>=width as u64 {0}else{w(e(0))>>c})},
  "bvult"=>V::B(w(e(0))<w(e(1))),"bvule"=>V::B(w(e(0))<=w(e(1))),
  "bvslt"|"bvsle"=>{let Sort::Bv(n)=t.0.args[0].0.sort else{panic!()}; let a=signed(w(e(0)),n);let c=signed(w(e(1)),n);V::B(if t.0.op=="bvslt"{a<c}else{a<=c})},
  "concat"=>{let Sort::Bv(n)=t.0.args[1].0.sort else{panic!()};V::W((w(e(0))<<n)|w(e(1)))},
  "store"=>{let V::M(mut a)=e(0) else{panic!()};a[w(e(1)) as usize]=w(e(2));V::M(a)},
  "select"=>{let V::M(a)=e(0) else{panic!()};V::W(a[w(e(1)) as usize])},
  o if o.starts_with("(_ bv")=>V::W(o[5..].split_whitespace().next().unwrap().parse().unwrap()),
  o if o.starts_with("(_ extract ")=>{let ns=o[11..].trim_end_matches(')').split_whitespace().map(|s|s.parse::<u32>().unwrap()).collect::<Vec<_>>();V::W((w(e(0))>>ns[1])&mask(ns[0]-ns[1]+1))},
  o if o.starts_with("(_ zero_extend ")=>e(0),
  o if o.starts_with("(_ sign_extend ")=>{let Sort::Bv(n)=t.0.args[0].0.sort else{panic!()}; V::W((signed(w(e(0)),n) as u64)&mask(width))},
  o if o.starts_with("(as const ")=>{let Sort::Mem(a,_)=n.sort else{panic!()};V::M(vec![w(e(0));1<<a])},
  o=>panic!("unsupported {o}")
 }
}
struct R(u64);impl R{fn next(&mut self,n:usize)->usize{self.0^=self.0<<13;self.0^=self.0>>7;self.0^=self.0<<17;self.0 as usize % n}}
fn word(r:&mut R,n:u32,d:usize)->Term {
 if d==0{return match r.next(4){0=>var("x".into(),Sort::Bv(n)),1=>var("y".into(),Sort::Bv(n)),_=>bv(n,r.next(20) as u64)}}
 let a=word(r,n,d-1);let c=word(r,n,d-1);let k=r.next(13);
 if k==10 {return ite(pred(r,n,d-1),a,c)}
 if k==11{return node(Sort::Bv(n),"bvnot",vec![a])}
 if k==12{return a}
 node(Sort::Bv(n),["bvadd","bvsub","bvmul","bvand","bvor","bvxor","bvshl","bvlshr","bvadd","bvsub"][k],vec![a,c])
}
fn pred(r:&mut R,n:u32,d:usize)->Term {
 if d==0 {let a=word(r,n,0);let c=word(r,n,0);return node(Sort::Bool,["=","bvult","bvule","bvslt","bvsle"][r.next(5)],vec![a,c])}
 match r.next(8){0=>not(pred(r,n,d-1)),1=>and(pred(r,n,d-1),pred(r,n,d-1)),2=>node(Sort::Bool,"or",vec![pred(r,n,d-1),pred(r,n,d-1)]),3=>node(Sort::Bool,"=>",vec![pred(r,n,d-1),pred(r,n,d-1)]),4=>var("p".into(),Sort::Bool),_=>node(Sort::Bool,["=","bvult","bvule","bvslt","bvsle"][r.next(5)],vec![word(r,n,d-1),word(r,n,d-1)])}
}
fn main(){
 let mut r=R(0x831421fab);let mut comparisons=0u64;let mut closures=0;
 for n in [1,2,3,4,8,32,64] {
  let vals:Vec<u64>=if n<=4{(0..1u64<<n).collect()}else{vec![0,1,2,3,(1u64<<(n-1))-1,1u64<<(n-1),mask(n)-1,mask(n)]};
  for i in 0..240 {
   let t=if i%2==0{pred(&mut r,n,3)}else{word(&mut r,n,3)};
   let assumptions=match i%5{0=>vec![],1=>vec![eq(var("x".into(),Sort::Bv(n)),bv(n,(i%16) as u64))],2=>vec![pred(&mut r,n,2)],3=>vec![eq(var("x".into(),Sort::Bv(n)),var("y".into(),Sort::Bv(n))),eq(var("y".into(),Sort::Bv(n)),var("x".into(),Sort::Bv(n)))],_=>vec![not(pred(&mut r,n,2))]};
   let simple=kernel::simplify(&t,&assumptions);
   let obligation=if t.0.sort==Sort::Bool{t.clone()}else{not(eq(t.clone(),word(&mut r,n,2)))};
   let full=assumptions.iter().fold(obligation,|a,c|and(c.clone(),a));
   let proof=kernel::refute(&full);if proof.closed{closures+=1;}
   for &x in &vals{for &y in &vals{for p in [false,true]{
    if assumptions.iter().all(|a|b(eval(a,x,y,p,&[]))){assert_eq!(eval(&t,x,y,p,&[]),eval(&simple,x,y,p,&[]),"simplify width={n} case={i} x={x} y={y} p={p} original={t:?} simplified={simple:?} assumptions={assumptions:?}");comparisons+=1;}
    if proof.closed{assert!(!b(eval(&full,x,y,p,&[])),"FALSE UNSAT width={n} case={i} x={x} y={y} p={p} term={full:?}");}
   }}}
  }
 }
 // Deterministic arithmetic identities, extensions, extraction, 64-bit concat.
 for n in [1,2,3,4,8,32,64] {
  let vals:Vec<u64>=if n<=4{(0..1u64<<n).collect()}else{vec![0,1,(1u64<<(n-1))-1,1u64<<(n-1),mask(n)]};
  let x=var("x".into(),Sort::Bv(n));let y=var("y".into(),Sort::Bv(n));
  let sub=node(Sort::Bv(n),"bvsub",vec![x.clone(),x.clone()]);
  let zero_law=not(eq(sub.clone(),bv(n,0)));
  assert!(kernel::refute(&zero_law).closed,"subtraction identity should close");
  let terms=vec![sub,node(Sort::Bv(n),"bvadd",vec![x.clone(),bv(n,mask(n))]),node(Sort::Bv(n),"bvshl",vec![x.clone(),bv(n,n as u64)]),node(Sort::Bv(n),"bvlshr",vec![x.clone(),bv(n,n as u64)])];
  for xv in &vals {for yv in &vals {
   let assumptions=vec![eq(x.clone(),bv(n,*xv)),eq(y.clone(),bv(n,*yv))];
   for t in &terms {assert_eq!(eval(t,*xv,*yv,false,&[]),eval(&kernel::simplify(t,&assumptions),*xv,*yv,false,&[]));comparisons+=1;}
   for target in [n,64] {for op in ["zero_extend","sign_extend"] {
    let t=node(Sort::Bv(target),format!("(_ {op} {})",target-n),vec![x.clone()]);
    assert_eq!(eval(&t,*xv,*yv,false,&[]),eval(&kernel::simplify(&t,&assumptions),*xv,*yv,false,&[]));comparisons+=1;
   }}
   for lo in [0,n/2,n-1] {
    let t=node(Sort::Bv(n-lo),format!("(_ extract {} {lo})",n-1),vec![x.clone()]);
    assert_eq!(eval(&t,*xv,*yv,false,&[]),eval(&kernel::simplify(&t,&assumptions),*xv,*yv,false,&[]));comparisons+=1;
   }
   if n<=32 {
    let t=node(Sort::Bv(n*2),"concat",vec![x.clone(),y.clone()]);
    assert_eq!(eval(&t,*xv,*yv,false,&[]),eval(&kernel::simplify(&t,&assumptions),*xv,*yv,false,&[]));comparisons+=1;
   }
  }}
 }
 // Uninterpreted memory aliases and both ITE branches; enumerate all 2-bit memories.
 let m=var("m".into(),Sort::Mem(2,2));let x=var("x".into(),Sort::Bv(2));let y=var("y".into(),Sort::Bv(2));
 for c in 0..4{for d in 0..4{
  let st=node(Sort::Mem(2,2),"store",vec![node(Sort::Mem(2,2),"store",vec![m.clone(),x.clone(),bv(2,c)]),y.clone(),bv(2,d)]);
  let raw=node(Sort::Bv(2),"select",vec![st,x.clone()]);
  for assumptions in [vec![],vec![eq(x.clone(),y.clone())],vec![not(eq(x.clone(),y.clone()))]]{
   let simple=kernel::simplify(&raw,&assumptions);
   for bits in 0..256 {let mem=(0..4).map(|i|(bits>>(2*i))&3).collect::<Vec<_>>();for xv in 0..4{for yv in 0..4{
    if assumptions.iter().all(|a|b(eval(a,xv,yv,false,&mem))){assert_eq!(eval(&raw,xv,yv,false,&mem),eval(&simple,xv,yv,false,&mem),"memory c={c} d={d} x={xv} y={yv}");comparisons+=1;}
   }}}
  }
 }}
 println!("PASS independent kernel semantics: {comparisons} comparisons, {closures} closed formulas checked; exhaustive widths 1..4 and memory aliases, boundary samples widths 8/32/64");
}
