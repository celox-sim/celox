use super::rational::{Q, integers, rref};
use super::{Affine, Domain, Error, Result, Work};
use std::collections::BTreeSet;

#[derive(Clone, Debug)]
pub(super) struct Polyhedron {
    pub dimensions: usize,
    /// Affine rows: coefficients followed by the constant, interpreted >= 0.
    pub inequalities: Vec<Vec<Q>>,
    pub equalities: Vec<Vec<Q>>,
}

impl Polyhedron {
    pub fn new(dimensions: usize) -> Self {
        Self {
            dimensions,
            inequalities: Vec::new(),
            equalities: Vec::new(),
        }
    }

    pub fn add_domain(&mut self, domain: &Domain, offset: usize) -> Result<()> {
        for (axis, bound) in domain.bounds.iter().enumerate() {
            let mut lower = vec![Q::ZERO; self.dimensions + 1];
            lower[offset + axis] = Q::ONE;
            lower[self.dimensions] = Q::integer(-i128::from(bound.start));
            let mut upper = vec![Q::ZERO; self.dimensions + 1];
            upper[offset + axis] = Q::integer(-1);
            upper[self.dimensions] = Q::integer(i128::from(bound.end) - 1);
            self.inequalities.extend([lower, upper]);
        }
        for constraint in &domain.constraints {
            let mut row = vec![Q::ZERO; self.dimensions + 1];
            for (i, &coefficient) in constraint.coefficients.iter().enumerate() {
                row[offset + i] = Q::integer(i128::from(coefficient));
            }
            row[self.dimensions] = Q::integer(i128::from(constraint.constant));
            self.inequalities.push(row);
        }
        Ok(())
    }

    /// Eliminate equalities first, then enumerate full-rank intersections of
    /// faces. Every caller supplies finite bounds for every original variable.
    /// Rational vertices conservatively cover all integer dependence points.
    pub fn vertices(&self, work: &mut Work) -> Result<Vec<Vec<Q>>> {
        work.spend()?;
        let d = self.dimensions;
        let mut equations = self.equalities.clone();
        for row in &mut equations {
            row[d] = row[d].neg()?;
        }
        let Some(pivots) = rref(&mut equations, d)? else {
            return Ok(Vec::new());
        };
        let free = (0..d)
            .filter(|axis| !pivots.contains(axis))
            .collect::<Vec<_>>();
        let n = free.len();
        let mut coordinates = vec![vec![Q::ZERO; n + 1]; d];
        for (k, &axis) in free.iter().enumerate() {
            coordinates[axis][k] = Q::ONE;
        }
        for (r, &axis) in pivots.iter().enumerate() {
            coordinates[axis][n] = equations[r][d];
            for (k, &column) in free.iter().enumerate() {
                coordinates[axis][k] = equations[r][column].neg()?;
            }
        }
        let mut faces = BTreeSet::new();
        for inequality in &self.inequalities {
            let mut row = vec![Q::ZERO; n + 1];
            row[n] = inequality[d];
            for i in 0..d {
                for (value, &coefficient) in row.iter_mut().zip(&coordinates[i]) {
                    *value = value.add(inequality[i].mul(coefficient)?)?;
                }
            }
            if row[..n].iter().all(|value| value.n == 0) {
                if row[n].n < 0 {
                    return Ok(Vec::new());
                }
            } else {
                faces.insert(integers(&row)?);
            }
        }
        if n == 0 {
            return Ok(vec![coordinates.into_iter().map(|row| row[0]).collect()]);
        }
        let faces = faces
            .into_iter()
            .map(|row| row.into_iter().map(Q::integer).collect::<Vec<_>>())
            .collect::<Vec<_>>();
        if faces.len() < n {
            return Err(Error::Invalid("dependence polyhedron is not bounded"));
        }
        let mut selection = (0..n).collect::<Vec<_>>();
        let mut vertices = BTreeSet::new();
        loop {
            work.spend()?;
            let mut matrix = selection
                .iter()
                .map(|&index| faces[index].clone())
                .collect::<Vec<_>>();
            for row in &mut matrix {
                row[n] = row[n].neg()?;
            }
            if let Some(pivots) = rref(&mut matrix, n)?
                && pivots.len() == n
            {
                let point = matrix.iter().map(|row| row[n]).collect::<Vec<_>>();
                let mut inside = true;
                for face in &faces {
                    if evaluate(face, &point)?.n < 0 {
                        inside = false;
                        break;
                    }
                }
                if inside {
                    vertices.insert(
                        coordinates
                            .iter()
                            .map(|row| evaluate(row, &point))
                            .collect::<Result<Vec<_>>>()?,
                    );
                }
            }
            let Some(k) = (0..n).rev().find(|&k| selection[k] < faces.len() - n + k) else {
                break;
            };
            selection[k] += 1;
            for next in k + 1..n {
                selection[next] = selection[next - 1] + 1;
            }
        }
        Ok(vertices.into_iter().collect())
    }
}

pub(super) fn evaluate(row: &[Q], point: &[Q]) -> Result<Q> {
    let mut value = row[point.len()];
    for (&coefficient, &coordinate) in row.iter().zip(point) {
        value = value.add(coefficient.mul(coordinate)?)?;
    }
    Ok(value)
}

pub(super) fn difference(source: &Affine, target: &Affine) -> Vec<Q> {
    source
        .coefficients
        .iter()
        .map(|&x| Q::integer(-i128::from(x)))
        .chain(
            target
                .coefficients
                .iter()
                .map(|&x| Q::integer(i128::from(x))),
        )
        .chain([Q::integer(
            i128::from(target.constant) - i128::from(source.constant),
        )])
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn work() -> Work {
        Work {
            remaining: 100_000,
            used: 0,
        }
    }
    #[test]
    fn equality_projection_keeps_all_dependence_extrema() {
        let mut poly = Polyhedron::new(2);
        poly.add_domain(&Domain::rectangular(vec![0..8, 0..8]), 0)
            .unwrap();
        poly.equalities
            .push(vec![Q::integer(-1), Q::ONE, Q::integer(-1)]);
        assert_eq!(
            poly.vertices(&mut work()).unwrap(),
            vec![vec![Q::ZERO, Q::ONE], vec![Q::integer(6), Q::integer(7)]]
        );
    }
    #[test]
    fn rational_vertices_are_not_rounded_for_legality() {
        let mut poly = Polyhedron::new(1);
        poly.add_domain(&Domain::rectangular(vec![0..3]), 0)
            .unwrap();
        poly.inequalities.push(vec![Q::integer(-2), Q::integer(3)]);
        assert_eq!(
            poly.vertices(&mut work()).unwrap(),
            vec![vec![Q::ZERO], vec![Q::new(3, 2).unwrap()]]
        );
    }
    #[test]
    fn empty_and_zero_dimensional_domains_are_distinguished() {
        let mut poly = Polyhedron::new(1);
        poly.add_domain(&Domain::rectangular(vec![2..2]), 0)
            .unwrap();
        assert!(poly.vertices(&mut work()).unwrap().is_empty());
        assert_eq!(
            Polyhedron::new(0).vertices(&mut work()).unwrap(),
            vec![vec![]]
        );
    }
}
