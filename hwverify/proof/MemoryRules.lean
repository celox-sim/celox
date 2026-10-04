import Std

/- A typed model of the four memory smart-constructor rules in src/ir.rs.
   Address syntax is abstract: syntactic equality implies equal interpretation;
   unequal syntax need not imply unequal addresses. Memory sizes are unrestricted. -/
namespace Hwverify.MemoryRules

mutual
  inductive Value (A W : Type) where
    | atom : W → Value A W
    | read : Memory A W → A → Value A W
    | branch : A → A → Value A W → Value A W → Value A W
  inductive Memory (A W : Type) where
    | atom : Nat → Memory A W
    | constant : Value A W → Memory A W
    | store : Memory A W → A → Value A W → Memory A W
end

variable {A W D : Type} [DecidableEq D]

mutual
  def evalValue (address : A → D) (base : Nat → D → W) : Value A W → W
    | .atom w => w
    | .read m a => evalMemory address base m (address a)
    | .branch a b yes no =>
      if address a = address b then evalValue address base yes else evalValue address base no
  def evalMemory (address : A → D) (base : Nat → D → W) : Memory A W → D → W
    | .atom i => base i
    | .constant v => fun _ => evalValue address base v
    | .store m a v => fun x =>
      if x = address a then evalValue address base v else evalMemory address base m x
end

variable [DecidableEq A]

/-- Matches memory_read, including the constant-read rule at budget zero. -/
def memoryRead (m : Memory A W) (a : A) : Nat → Value A W
  | 0 => match m with
    | .constant v => v
    | _ => .read m a
  | n + 1 => match m with
    | .store old b v =>
      if b = a then v else .branch a b v (memoryRead old a n)
    | .constant v => v
    | _ => .read m a

/-- Matches memory_write: eliminate exactly one preceding same-syntax store. -/
def memoryWrite (m : Memory A W) (a : A) (v : Value A W) : Memory A W :=
  match m with
  | .store old b _ => if b = a then .store old a v else .store m a v
  | _ => .store m a v

theorem memoryRead_sound (address : A → D) (base : Nat → D → W)
    (m : Memory A W) (a : A) (budget : Nat) :
    evalValue address base (memoryRead m a budget) = evalMemory address base m (address a) := by
  induction budget generalizing m with
  | zero => cases m <;> rfl
  | succ n ih =>
    cases m with
    | atom i => rfl
    | constant v => rfl
    | store old b v =>
      by_cases same : b = a
      · subst b
        simp [memoryRead, evalMemory]
      · simp only [memoryRead, ite_eq_right same, evalValue, evalMemory]
        rw [ih]

theorem memoryWrite_sound (address : A → D) (base : Nat → D → W)
    (m : Memory A W) (a : A) (v : Value A W) :
    evalMemory address base (memoryWrite m a v) = evalMemory address base (.store m a v) := by
  cases m with
  | atom i => rfl
  | constant w => rfl
  | store old b w =>
    by_cases same : b = a
    · subst b
      funext x
      by_cases hit : x = address a
      · simp [memoryWrite, evalMemory, hit]
      · simp [memoryWrite, evalMemory, hit]
    · simp only [memoryWrite, ite_eq_right same]

/-- A concrete aliasing counterexample: distinct syntax can denote one address. -/
example :
    evalValue (fun _ : Nat => 0) (fun _ _ : Nat => (3 : Nat))
      (memoryRead (.store (.atom 0) 1 (.atom 7)) 2 1) = 7 := by decide

/-- Budget exhaustion retains a read; it does not forget previous writes. -/
example :
    evalValue (fun x : Nat => x) (fun _ _ : Nat => (3 : Nat))
      (memoryRead (.store (.atom 0) 1 (.atom 7)) 1 0) = 7 := by decide

/-- A wrong no-alias rewrite fails even though address syntax differs. -/
theorem reject_noalias :
    evalValue (fun _ : Nat => 0) (fun _ _ : Nat => (3 : Nat))
      (.read (.store (.atom 0) 1 (.atom 7)) 2) ≠
    evalValue (fun _ : Nat => 0) (fun _ _ : Nat => (3 : Nat))
      (.read (.atom 0) 2) := by decide

/-- Retaining an older same-address write instead of the last write is wrong. -/
theorem reject_first_write_wins :
    evalMemory (fun x : Nat => x) (fun _ _ : Nat => (0 : Nat))
      (memoryWrite (.store (.atom 0) 1 (.atom 3)) 1 (.atom 7)) 1 ≠
    evalMemory (fun x : Nat => x) (fun _ _ : Nat => (0 : Nat))
      (.store (.atom 0) 1 (.atom 3)) 1 := by decide

#print axioms memoryRead_sound
#print axioms memoryWrite_sound
#print axioms reject_noalias
#print axioms reject_first_write_wins
end Hwverify.MemoryRules
