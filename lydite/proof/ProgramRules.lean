import Std

/- The mathematical invariant/ranking argument behind program_contract.
   This does not certify Rust's construction or SMT translation of obligations. -/
namespace Lydite.ProgramRules

variable {S : Type}

def run (step : S → S) : Nat → S → S
  | 0, s => s
  | n + 1, s => run step n (step s)

/-- The checker uses an unsigned finite word; its natural-number value is a rank.
    An invariant plus strict decrease on every active step gives termination and
    the postcondition, without executing or unrolling the program. -/
theorem total_correctness (step : S → S) (inv terminal post : S → Prop)
    (rank : S → Nat)
    (preserves : ∀ s, inv s → ¬ terminal s → inv (step s))
    (decreases : ∀ s, inv s → ¬ terminal s → rank (step s) < rank s)
    (finishes : ∀ s, inv s → terminal s → post s)
    (initial : S) (initial_inv : inv initial) :
    ∃ n, n ≤ rank initial ∧ terminal (run step n initial) ∧ post (run step n initial) := by
  have all : ∀ r s, rank s = r → inv s →
      ∃ n, n ≤ r ∧ terminal (run step n s) ∧ post (run step n s) := by
    intro r
    induction r using Nat.strongRecOn with
    | ind r ih =>
      intro s hr hi
      by_cases ht : terminal s
      · exact ⟨0, Nat.zero_le _, ht, finishes s hi ht⟩
      · have hd := decreases s hi ht
        have smaller : rank (step s) < r := by omega
        obtain ⟨n, hn, ht', hp⟩ := ih (rank (step s)) smaller (step s) rfl (preserves s hi ht)
        exact ⟨n+1, by omega, ht', hp⟩
  exact all (rank initial) initial rfl initial_inv

/-- General microstep safety transfer, allowing any finite number of stutters.
    Each implementation transition either corresponds to a spec step or leaves
    the spec state unchanged. -/
theorem lift_invariant {I : Type} (specStep : S → S) (implStep : I → I)
    (commit : I → Bool) (relation : S → I → Prop) (inv : S → Prop)
    (preserves : ∀ s, inv s → inv (specStep s))
    (refines : ∀ s i, relation s i →
      relation (if commit i then specStep s else s) (implStep i))
    (s : S) (i : I) (hr : relation s i) (hi : inv s) :
    relation (if commit i then specStep s else s) (implStep i) ∧
    inv (if commit i then specStep s else s) := by
  constructor
  · exact refines s i hr
  · cases hc : commit i <;> simp only [Bool.false_eq_true, ↓reduceIte]
    · exact hi
    · exact preserves s hi

#print axioms total_correctness
#print axioms lift_invariant
end Lydite.ProgramRules
