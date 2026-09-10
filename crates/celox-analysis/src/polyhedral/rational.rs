use super::{Error, Result};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct Q {
    pub n: i128,
    pub d: i128,
}

pub(super) fn gcd(mut a: i128, mut b: i128) -> i128 {
    debug_assert!(a >= 0 && b >= 0);
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

impl Q {
    pub const ZERO: Self = Self { n: 0, d: 1 };
    pub const ONE: Self = Self { n: 1, d: 1 };
    pub fn integer(n: i128) -> Self {
        Self { n, d: 1 }
    }
    pub fn new(n: i128, d: i128) -> Result<Self> {
        if d == 0 {
            return Err(Error::Invalid("zero rational denominator"));
        }
        let magnitude = n.checked_abs().ok_or(Error::ArithmeticOverflow)?;
        let denominator = d.checked_abs().ok_or(Error::ArithmeticOverflow)?;
        let divisor = gcd(magnitude, denominator);
        let n = n / divisor;
        Ok(Self {
            n: if d < 0 {
                n.checked_neg().ok_or(Error::ArithmeticOverflow)?
            } else {
                n
            },
            d: denominator / divisor,
        })
    }
    pub fn neg(self) -> Result<Self> {
        Ok(Self {
            n: self.n.checked_neg().ok_or(Error::ArithmeticOverflow)?,
            d: self.d,
        })
    }
    pub fn add(self, rhs: Self) -> Result<Self> {
        let common = gcd(self.d, rhs.d);
        let left = self
            .n
            .checked_mul(rhs.d / common)
            .ok_or(Error::ArithmeticOverflow)?;
        let right = rhs
            .n
            .checked_mul(self.d / common)
            .ok_or(Error::ArithmeticOverflow)?;
        Self::new(
            left.checked_add(right).ok_or(Error::ArithmeticOverflow)?,
            self.d
                .checked_mul(rhs.d / common)
                .ok_or(Error::ArithmeticOverflow)?,
        )
    }
    pub fn sub(self, rhs: Self) -> Result<Self> {
        self.add(rhs.neg()?)
    }
    pub fn mul(self, rhs: Self) -> Result<Self> {
        let g1 = gcd(
            self.n.checked_abs().ok_or(Error::ArithmeticOverflow)?,
            rhs.d,
        );
        let g2 = gcd(
            rhs.n.checked_abs().ok_or(Error::ArithmeticOverflow)?,
            self.d,
        );
        Self::new(
            (self.n / g1)
                .checked_mul(rhs.n / g2)
                .ok_or(Error::ArithmeticOverflow)?,
            (self.d / g2)
                .checked_mul(rhs.d / g1)
                .ok_or(Error::ArithmeticOverflow)?,
        )
    }
    pub fn div(self, rhs: Self) -> Result<Self> {
        self.mul(Self::new(rhs.d, rhs.n)?)
    }
}

/// Reduced row echelon form; the final column is the right-hand side.
pub(super) fn rref(rows: &mut [Vec<Q>], columns: usize) -> Result<Option<Vec<usize>>> {
    let mut pivots = Vec::new();
    for column in 0..columns {
        let Some(pivot) = (pivots.len()..rows.len()).find(|&r| rows[r][column].n != 0) else {
            continue;
        };
        let target = pivots.len();
        rows.swap(target, pivot);
        let divisor = rows[target][column];
        for value in &mut rows[target][column..=columns] {
            *value = value.div(divisor)?;
        }
        let (before, remaining) = rows.split_at_mut(target);
        let (pivot, after) = remaining.split_first_mut().unwrap();
        for row in before.iter_mut().chain(after) {
            let factor = row[column];
            if factor.n == 0 {
                continue;
            }
            for (value, &pivot) in row[column..=columns]
                .iter_mut()
                .zip(&pivot[column..=columns])
            {
                *value = value.sub(factor.mul(pivot)?)?;
            }
        }
        pivots.push(column);
    }
    if rows
        .iter()
        .any(|row| row[..columns].iter().all(|x| x.n == 0) && row[columns].n != 0)
    {
        return Ok(None);
    }
    Ok(Some(pivots))
}

pub(super) fn integers(row: &[Q]) -> Result<Vec<i128>> {
    let mut denominator = 1i128;
    for value in row {
        denominator = (denominator / gcd(denominator, value.d))
            .checked_mul(value.d)
            .ok_or(Error::ArithmeticOverflow)?;
    }
    let mut out = row
        .iter()
        .map(|q| {
            q.n.checked_mul(denominator / q.d)
                .ok_or(Error::ArithmeticOverflow)
        })
        .collect::<Result<Vec<_>>>()?;
    let mut divisor = 0;
    for &value in &out {
        divisor = gcd(
            divisor,
            value.checked_abs().ok_or(Error::ArithmeticOverflow)?,
        );
    }
    if divisor > 1 {
        for value in &mut out {
            *value /= divisor;
        }
    }
    Ok(out)
}

pub(super) fn nullspace(rows: &[Vec<i64>], columns: usize) -> Result<Vec<Vec<i128>>> {
    let mut matrix = rows
        .iter()
        .map(|row| {
            row.iter()
                .map(|&x| Q::integer(i128::from(x)))
                .chain([Q::ZERO])
                .collect()
        })
        .collect::<Vec<Vec<Q>>>();
    let pivots =
        rref(&mut matrix, columns)?.ok_or(Error::Invalid("inconsistent homogeneous system"))?;
    let mut basis = Vec::new();
    for column in 0..columns {
        if pivots.contains(&column) {
            continue;
        }
        let mut vector = vec![Q::ZERO; columns];
        vector[column] = Q::ONE;
        for (r, &pivot) in pivots.iter().enumerate() {
            vector[pivot] = matrix[r][column].neg()?;
        }
        basis.push(integers(&vector)?);
    }
    Ok(basis)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_arithmetic_reduces_before_multiplying() {
        let a = Q::new(i128::MAX - 1, 2).unwrap();
        assert_eq!(a.mul(Q::new(2, i128::MAX - 1).unwrap()).unwrap(), Q::ONE);
        assert_eq!(
            Q::new(-3, 4).unwrap().add(Q::new(1, 2).unwrap()).unwrap(),
            Q::new(-1, 4).unwrap()
        );
        assert!(Q::new(i128::MIN, 1).is_err());
        assert!(Q::integer(i128::MAX).add(Q::ONE).is_err());
    }
    #[test]
    fn independent_directions_include_skew_complements() {
        assert_eq!(nullspace(&[vec![1, 0]], 2).unwrap(), vec![vec![0, 1]]);
        assert_eq!(nullspace(&[vec![2, 1]], 2).unwrap(), vec![vec![-1, 2]]);
        assert!(nullspace(&[vec![1, 0], vec![2, 1]], 2).unwrap().is_empty());
    }
}
