//! Recover a word reduction from reductions of transposed bit columns.
//! Verify every driver and reader before replacing any path.
use super::*;
use celox_design::{BinaryOp, UnaryOp};

pub(super) struct ColumnReduction<A> {
    pub source: A,
    bases: Vec<usize>,
    width: usize,
    op: BinaryOp,
    pub outputs: Vec<(usize, usize)>,
}

impl<A: Copy + Eq + Hash> ColumnReduction<A> {
    pub fn words(&self) -> Vec<VarAtomBase<A>> {
        self.bases
            .iter()
            .map(|&base| VarAtomBase::new(self.source, base, base + self.width - 1))
            .collect()
    }

    pub fn reduce(&self, arena: &mut SLTNodeArena<A>) -> Result<NodeId, SLTNodeFactsError> {
        let mut level = self
            .words()
            .iter()
            .map(|word| {
                arena.alloc(SLTNode::Input {
                    variable: word.id,
                    access: word.access,
                    signed: false,
                    index: Vec::new(),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        while level.len() > 1 {
            level = level
                .chunks(2)
                .map(|pair| match pair {
                    [a, b] => arena.alloc(SLTNode::Binary(*a, self.op, *b)),
                    _ => Ok(pair[0]),
                })
                .collect::<Result<Vec<_>, _>>()?;
        }
        Ok(level[0])
    }
}

#[allow(clippy::too_many_arguments)]
pub(super) fn match_columns<A: Copy + Eq + Hash>(
    array: A,
    rows: usize,
    paths: &[LogicPath<A>],
    arena: &SLTNodeArena<A>,
    drivers: &HashMap<A, Vec<usize>>,
    readers: &HashMap<A, Vec<usize>>,
    ordered: &HashSet<usize>,
    plain: &[bool],
    widths: &HashMap<A, usize>,
) -> Option<ColumnReduction<A>> {
    let users = readers.get(&array)?;
    let bits = drivers.get(&array)?;
    if rows < 2 || users.len() < 2 || !bits.len().is_multiple_of(rows) {
        return None;
    }
    let mut mapping = vec![None; bits.len()];
    for &index in bits {
        let path = &paths[index];
        let dest = path.target.var()?.access;
        if dest.lsb != dest.msb
            || !plain[index]
            || ordered.contains(&index)
            || !path.order_before.is_empty()
            || !path.comb_capture_enable_sites.is_empty()
            || path.comb_capture_enable_always
        {
            return None;
        }
        let SLTNode::Input {
            variable,
            signed: false,
            index,
            access,
        } = arena.get(path.expr)
        else {
            return None;
        };
        if !index.is_empty()
            || path.sources.iter().any(|atom| atom.id != *variable)
            || access.lsb != access.msb
            || mapping
                .get_mut(dest.lsb)?
                .replace((*variable, access.lsb))
                .is_some()
        {
            return None;
        }
    }
    let (source, _) = mapping[0]?;
    let width = *widths.get(&source)?;
    if width < 2 || rows.checked_mul(width)? != bits.len() {
        return None;
    }
    let bases = mapping[..rows]
        .iter()
        .map(|cell| Some(cell.as_ref()?.1 / width * width))
        .collect::<Option<Vec<_>>>()?;
    if bases
        .iter()
        .any(|&base| base.checked_add(width - 1).is_none())
    {
        return None;
    }
    let mut lanes = Vec::with_capacity(width);
    for column in mapping.chunks_exact(rows) {
        let lane = column[0]?.1 % width;
        for (&cell, &base) in column.iter().zip(&bases) {
            if cell != Some((source, base.checked_add(lane)?)) {
                return None;
            }
        }
        lanes.push(lane);
    }
    let mut operation = None;
    let mut outputs = Vec::with_capacity(users.len());
    for &user in users {
        let SLTNode::Unary(op, operand) = arena.get(paths[user].expr) else {
            return None;
        };
        let op = match op {
            UnaryOp::And => BinaryOp::And,
            UnaryOp::Or => BinaryOp::Or,
            UnaryOp::Xor => BinaryOp::Xor,
            _ => return None,
        };
        if operation.replace(op).is_some_and(|other| other != op) {
            return None;
        }
        let SLTNode::Input {
            variable,
            signed: false,
            index,
            access,
        } = arena.get(*operand)
        else {
            return None;
        };
        if !index.is_empty()
            || *variable != array
            || !access.lsb.is_multiple_of(rows)
            || access.msb.checked_sub(access.lsb)? != rows - 1
        {
            return None;
        }
        outputs.push((user, *lanes.get(access.lsb / rows)?));
    }
    Some(ColumnReduction {
        source,
        bases,
        width,
        op: operation?,
        outputs,
    })
}
