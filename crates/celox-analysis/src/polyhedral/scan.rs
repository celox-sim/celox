//! Polyhedral scanning after statement-wise affine transformations. Projection
//! produces integer loop bounds; guards retain exact membership in each
//! statement domain when scanning their union. Tiling is applied to the
//! scattering dimensions before projection, not to a generated syntax tree.

use super::rational::{Q, gcd, nullspace, rref};
use super::{Affine, Error, Region, Result, Schedule, Work, verify_schedule};
use std::collections::BTreeSet;

#[derive(Clone, Debug)]
pub struct Tile {
    pub band: usize,
    pub sizes: Vec<i64>,
}

#[derive(Clone, Debug)]
pub struct Bound {
    pub numerator: Affine,
    pub denominator: i64,
}

impl Bound {
    pub fn evaluate(&self, prefix: &[i64], lower: bool) -> Result<i64> {
        let value = self.numerator.evaluate(prefix)?;
        if self.denominator <= 0 {
            return Err(Error::Invalid("scan divisor"));
        }
        let quotient = value.div_euclid(self.denominator);
        quotient
            .checked_add(i64::from(lower && value.rem_euclid(self.denominator) != 0))
            .ok_or(Error::ArithmeticOverflow)
    }
}

#[derive(Clone, Debug)]
pub struct ScanDimension {
    /// Minimum across statements of each statement's maximum lower bound.
    pub lower: Vec<Vec<Bound>>,
    /// Maximum across statements of each statement's minimum upper bound.
    pub upper: Vec<Vec<Bound>>,
    /// Scalar scattering coordinates can be expanded at compile time.
    pub fixed_values: Option<Vec<i64>>,
}

impl ScanDimension {
    pub fn extent(&self, prefix: &[i64]) -> Result<(i64, i64)> {
        let mut lower = i64::MAX;
        let mut upper = i64::MIN;
        for bounds in &self.lower {
            let mut start = i64::MIN;
            for bound in bounds {
                start = start.max(bound.evaluate(prefix, true)?);
            }
            lower = lower.min(start);
        }
        for bounds in &self.upper {
            let mut end = i64::MAX;
            for bound in bounds {
                end = end.min(bound.evaluate(prefix, false)?);
            }
            upper = upper.max(end);
        }
        Ok((lower, upper))
    }
}

#[derive(Clone, Debug)]
pub struct ScanStatement {
    pub iterators: Vec<Affine>,
    /// Full scattering-domain membership, for deriving an unguarded interior.
    pub domain: Vec<Affine>,
    /// Coordinates fixed for this statement, including scalar phase suffixes.
    pub fixed_coordinates: Vec<Option<i64>>,
    /// Nonnegative tests required before executing this statement. Common
    /// domain constraints already enforced by all union bounds are omitted.
    pub guards: Vec<Affine>,
}

#[derive(Clone, Debug)]
pub struct ScanPlan {
    pub dimensions: Vec<ScanDimension>,
    pub statements: Vec<ScanStatement>,
    /// Inclusive bounds, also used to prove that generated signed-64-bit
    /// affine expressions cannot overflow even in guarded halo iterations.
    pub coordinate_bounds: Vec<(i64, i64)>,
    pub work: usize,
}

impl ScanPlan {
    /// Reference executor for validation. Production adapters emit loops and
    /// statement bodies from this plan instead of interpreting points.
    pub fn for_each_instance(
        &self,
        limit: usize,
        mut visit: impl FnMut(usize, &[i64]),
    ) -> Result<()> {
        fn walk(
            plan: &ScanPlan,
            prefix: &mut Vec<i64>,
            work: &mut Work,
            visit: &mut impl FnMut(usize, &[i64]),
        ) -> Result<()> {
            work.spend()?;
            if prefix.len() == plan.dimensions.len() {
                for (s, statement) in plan.statements.iter().enumerate() {
                    let mut active = true;
                    for guard in &statement.guards {
                        if guard.evaluate(prefix)? < 0 {
                            active = false;
                            break;
                        }
                    }
                    if active {
                        visit(
                            s,
                            &statement
                                .iterators
                                .iter()
                                .map(|i| i.evaluate(prefix))
                                .collect::<Result<Vec<_>>>()?,
                        );
                    }
                }
                return Ok(());
            }
            let dimension = &plan.dimensions[prefix.len()];
            let (lower, upper) = dimension.extent(prefix)?;
            if let Some(values) = &dimension.fixed_values {
                for &value in values.iter().filter(|&&v| lower <= v && v <= upper) {
                    prefix.push(value);
                    walk(plan, prefix, work, visit)?;
                    prefix.pop();
                }
            } else {
                for value in lower..=upper {
                    prefix.push(value);
                    walk(plan, prefix, work, visit)?;
                    prefix.pop();
                }
            }
            Ok(())
        }
        walk(
            self,
            &mut Vec::new(),
            &mut Work {
                remaining: limit,
                used: 0,
            },
            &mut visit,
        )
    }
}

fn narrow(value: i128) -> Result<i64> {
    i64::try_from(value).map_err(|_| Error::ArithmeticOverflow)
}

fn primitive(mut row: Vec<i128>) -> Result<Vec<i128>> {
    let mut divisor = 0;
    for &value in &row {
        divisor = gcd(
            divisor,
            value.checked_abs().ok_or(Error::ArithmeticOverflow)?,
        );
    }
    if divisor > 1 {
        for value in &mut row {
            *value /= divisor;
        }
    }
    Ok(row)
}

fn add_row(rows: &mut BTreeSet<Vec<i128>>, row: Vec<i128>) -> Result<()> {
    let row = primitive(row)?;
    if row[..row.len() - 1].iter().all(|&x| x == 0) && row[row.len() - 1] >= 0 {
        return Ok(());
    }
    rows.insert(row);
    Ok(())
}

fn inverse(schedule: &Schedule, statement: usize, dimensions: usize) -> Result<Vec<Affine>> {
    let time = schedule.rows.len();
    let mut selected = Vec::new();
    let mut rows = Vec::new();
    let mut missing = dimensions;
    for (index, row) in schedule.rows.iter().enumerate() {
        let mut trial = rows.clone();
        trial.push(row[statement].coefficients.clone());
        let next = nullspace(&trial, dimensions)?.len();
        if next < missing {
            selected.push(index);
            rows = trial;
            missing = next;
        }
        if missing == 0 {
            break;
        }
    }
    if missing != 0 {
        return Err(Error::NonIntegralScan);
    }
    let mut out = vec![Affine::constant(time, 0); dimensions];
    for column in 0..=dimensions {
        let mut equations = rows
            .iter()
            .enumerate()
            .map(|(r, row)| {
                row.iter()
                    .map(|&value| Q::integer(i128::from(value)))
                    .chain([if column == dimensions {
                        Q::integer(-i128::from(schedule.rows[selected[r]][statement].constant))
                    } else {
                        Q::integer(i128::from(r == column))
                    }])
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        if rref(&mut equations, dimensions)?.is_none() {
            return Err(Error::NonIntegralScan);
        }
        for axis in 0..dimensions {
            let value = equations[axis][dimensions];
            if value.d != 1 {
                return Err(Error::NonIntegralScan);
            }
            if column == dimensions {
                out[axis].constant = narrow(value.n)?;
            } else {
                out[axis].coefficients[selected[column]] = narrow(value.n)?;
            }
        }
    }
    Ok(out)
}

fn substitute(expression: &Affine, coordinates: &[Affine], dimensions: usize) -> Result<Vec<i128>> {
    let mut row = vec![0i128; dimensions + 1];
    row[dimensions] = i128::from(expression.constant);
    for (&coefficient, coordinate) in expression.coefficients.iter().zip(coordinates) {
        for (slot, value) in row.iter_mut().zip(
            coordinate
                .coefficients
                .iter()
                .copied()
                .chain([coordinate.constant]),
        ) {
            *slot = slot
                .checked_add(
                    i128::from(coefficient)
                        .checked_mul(i128::from(value))
                        .ok_or(Error::ArithmeticOverflow)?,
                )
                .ok_or(Error::ArithmeticOverflow)?;
        }
    }
    Ok(row)
}

fn project(
    rows: &BTreeSet<Vec<i128>>,
    axis: usize,
    work: &mut Work,
) -> Result<BTreeSet<Vec<i128>>> {
    let mut output = BTreeSet::new();
    for row in rows.iter().filter(|row| row[axis] == 0) {
        let mut row = row.clone();
        row.remove(axis);
        add_row(&mut output, row)?;
    }
    for positive in rows.iter().filter(|row| row[axis] > 0) {
        for negative in rows.iter().filter(|row| row[axis] < 0) {
            work.spend()?;
            let p = positive[axis];
            let n = negative[axis]
                .checked_neg()
                .ok_or(Error::ArithmeticOverflow)?;
            let divisor = gcd(p, n);
            let mut row = Vec::new();
            for k in 0..positive.len() {
                if k == axis {
                    continue;
                }
                row.push(
                    positive[k]
                        .checked_mul(n / divisor)
                        .and_then(|x| {
                            negative[k]
                                .checked_mul(p / divisor)
                                .and_then(|y| x.checked_add(y))
                        })
                        .ok_or(Error::ArithmeticOverflow)?,
                );
            }
            add_row(&mut output, row)?;
        }
    }
    Ok(output)
}

/// Generate a scan only after independently verifying schedule legality.
/// Parameters must be specialized and the selected iterator basis unimodular.
pub fn scan(
    region: &Region,
    schedule: &Schedule,
    tile: Option<&Tile>,
    max_work: usize,
) -> Result<ScanPlan> {
    verify_schedule(region, schedule, max_work)?;
    let mut work = Work {
        remaining: max_work,
        used: 0,
    };
    let original_dimensions = schedule.rows.len();
    let band = if let Some(tile) = tile {
        let band = schedule
            .bands
            .get(tile.band)
            .ok_or(Error::Invalid("tile band"))?;
        if tile.sizes.len() != band.len() || tile.sizes.iter().any(|&size| size <= 0) {
            return Err(Error::Invalid("tile sizes"));
        }
        band.clone()
    } else {
        0..0
    };
    let dimensions = original_dimensions + band.len();
    if dimensions == 0 {
        return Err(Error::Invalid("empty scan schedule"));
    }
    let position = |axis: usize| {
        if axis < band.start {
            axis
        } else {
            axis + band.len()
        }
    };
    let mut coordinate_bounds = vec![(i64::MAX, i64::MIN); dimensions];
    let mut fixed = vec![Some(BTreeSet::new()); dimensions];
    for (s, statement) in region.statements.iter().enumerate() {
        for (axis, rows) in schedule.rows.iter().enumerate() {
            let row = &rows[s];
            let mut low = i128::from(row.constant);
            let mut high = low;
            for (&coefficient, range) in row.coefficients.iter().zip(&statement.domain.bounds) {
                if range.start >= range.end {
                    return Err(Error::Invalid("empty scan domain"));
                }
                let (a, b) = if coefficient >= 0 {
                    (range.start, range.end - 1)
                } else {
                    (range.end - 1, range.start)
                };
                low = low
                    .checked_add(i128::from(coefficient) * i128::from(a))
                    .ok_or(Error::ArithmeticOverflow)?;
                high = high
                    .checked_add(i128::from(coefficient) * i128::from(b))
                    .ok_or(Error::ArithmeticOverflow)?;
            }
            let (low, high) = (narrow(low)?, narrow(high)?);
            let slot = position(axis);
            coordinate_bounds[slot].0 = coordinate_bounds[slot].0.min(low);
            coordinate_bounds[slot].1 = coordinate_bounds[slot].1.max(high);
            if low == high {
                if let Some(values) = &mut fixed[slot] {
                    values.insert(low);
                }
            } else {
                fixed[slot] = None;
            }
            if band.contains(&axis) {
                let slot = band.start + axis - band.start;
                let size = tile.unwrap().sizes[axis - band.start];
                let (low, high) = (low.div_euclid(size), high.div_euclid(size));
                coordinate_bounds[slot].0 = coordinate_bounds[slot].0.min(low);
                coordinate_bounds[slot].1 = coordinate_bounds[slot].1.max(high);
                if low == high {
                    if let Some(values) = &mut fixed[slot] {
                        values.insert(low);
                    }
                } else {
                    fixed[slot] = None;
                }
            }
        }
    }
    if coordinate_bounds
        .iter()
        .any(|&(low, high)| low <= i64::MIN / 4 || high >= i64::MAX / 4)
    {
        return Err(Error::ArithmeticOverflow);
    }
    let mut all_domains = Vec::new();
    let mut statements = Vec::new();
    for (s, statement) in region.statements.iter().enumerate() {
        let coordinates = inverse(schedule, s, statement.domain.bounds.len())?;
        let expand = |old: &[i128]| {
            let mut row = vec![0; dimensions + 1];
            for (axis, &value) in old[..original_dimensions].iter().enumerate() {
                row[position(axis)] = value;
            }
            row[dimensions] = old[original_dimensions];
            row
        };
        let mut rows = BTreeSet::new();
        for (axis, range) in statement.domain.bounds.iter().enumerate() {
            let mut lower = Affine::axis(statement.domain.bounds.len(), axis);
            lower.constant = range.start.checked_neg().ok_or(Error::ArithmeticOverflow)?;
            add_row(
                &mut rows,
                expand(&substitute(&lower, &coordinates, original_dimensions)?),
            )?;
            let mut upper = Affine::constant(statement.domain.bounds.len(), range.end - 1);
            upper.coefficients[axis] = -1;
            add_row(
                &mut rows,
                expand(&substitute(&upper, &coordinates, original_dimensions)?),
            )?;
        }
        for constraint in &statement.domain.constraints {
            add_row(
                &mut rows,
                expand(&substitute(constraint, &coordinates, original_dimensions)?),
            )?;
        }
        for (axis, schedule_row) in schedule.rows.iter().enumerate() {
            let mut equality = substitute(&schedule_row[s], &coordinates, original_dimensions)?;
            equality[axis] = equality[axis]
                .checked_sub(1)
                .ok_or(Error::ArithmeticOverflow)?;
            let equality = expand(&equality);
            add_row(&mut rows, equality.clone())?;
            add_row(
                &mut rows,
                equality
                    .into_iter()
                    .map(|v| v.checked_neg().ok_or(Error::ArithmeticOverflow))
                    .collect::<Result<Vec<_>>>()?,
            )?;
        }
        for axis in band.clone() {
            let size = i128::from(tile.unwrap().sizes[axis - band.start]);
            let mut lower = vec![0; dimensions + 1];
            lower[position(axis)] = 1;
            lower[axis] = -size;
            let mut upper = lower.iter().map(|&v| -v).collect::<Vec<_>>();
            upper[dimensions] = size - 1;
            add_row(&mut rows, lower)?;
            add_row(&mut rows, upper)?;
        }
        // Bound all emitted coordinates even for prefixes outside an
        // individual domain. This makes overflow proofs independent of guards.
        for (axis, &(low, high)) in coordinate_bounds.iter().enumerate() {
            let mut lower = vec![0; dimensions + 1];
            lower[axis] = 1;
            lower[dimensions] = -i128::from(low);
            let mut upper = vec![0; dimensions + 1];
            upper[axis] = -1;
            upper[dimensions] = i128::from(high);
            add_row(&mut rows, lower)?;
            add_row(&mut rows, upper)?;
        }
        let iterators = coordinates
            .iter()
            .map(|coordinate| {
                let row = expand(
                    &coordinate
                        .coefficients
                        .iter()
                        .map(|&v| i128::from(v))
                        .chain([i128::from(coordinate.constant)])
                        .collect::<Vec<_>>(),
                );
                to_affine(&row)
            })
            .collect::<Result<Vec<_>>>()?;
        let mut fixed_coordinates = vec![None; dimensions];
        for (axis, row) in schedule.rows.iter().enumerate() {
            if row[s].coefficients.iter().all(|&c| c == 0) {
                fixed_coordinates[position(axis)] = Some(row[s].constant);
                if band.contains(&axis) {
                    fixed_coordinates[axis] = Some(
                        row[s]
                            .constant
                            .div_euclid(tile.unwrap().sizes[axis - band.start]),
                    );
                }
            }
        }
        statements.push(ScanStatement {
            iterators,
            domain: rows
                .iter()
                .map(|row| to_affine(row))
                .collect::<Result<Vec<_>>>()?,
            fixed_coordinates,
            guards: Vec::new(),
        });
        all_domains.push(rows);
    }
    let mut common = all_domains[0].clone();
    for domain in &all_domains[1..] {
        common.retain(|row| domain.contains(row));
    }
    for (statement, domain) in statements.iter_mut().zip(&all_domains) {
        statement.guards = domain
            .difference(&common)
            .map(|row| to_affine(row))
            .collect::<Result<Vec<_>>>()?;
    }
    let mut scan_dimensions = (0..dimensions)
        .map(|axis| ScanDimension {
            lower: Vec::new(),
            upper: Vec::new(),
            fixed_values: fixed[axis]
                .take()
                .map(|values| values.into_iter().collect()),
        })
        .collect::<Vec<_>>();
    for mut domain in all_domains {
        for axis in (0..dimensions).rev() {
            let mut lower = Vec::new();
            let mut upper = Vec::new();
            for row in &domain {
                let coefficient = row[axis];
                if coefficient == 0 {
                    continue;
                }
                let mut numerator = row.clone();
                numerator.remove(axis);
                if coefficient > 0 {
                    numerator = numerator
                        .into_iter()
                        .map(|v| v.checked_neg().ok_or(Error::ArithmeticOverflow))
                        .collect::<Result<Vec<_>>>()?;
                }
                let bound = Bound {
                    numerator: to_affine(&numerator)?,
                    denominator: narrow(
                        coefficient.checked_abs().ok_or(Error::ArithmeticOverflow)?,
                    )?,
                };
                if coefficient > 0 {
                    lower.push(bound);
                } else {
                    upper.push(bound);
                }
            }
            if lower.is_empty() || upper.is_empty() {
                return Err(Error::Invalid("unbounded scan coordinate"));
            }
            scan_dimensions[axis].lower.push(lower);
            scan_dimensions[axis].upper.push(upper);
            domain = project(&domain, axis, &mut work)?;
        }
    }
    // Prove all intermediate products and sums fit the emitter's i64 type,
    // including rounding a negative numerator by a positive denominator.
    for statement in &statements {
        for expression in statement.iterators.iter().chain(&statement.domain) {
            check_extent(expression, &coordinate_bounds, 0)?;
        }
    }
    for dimension in &scan_dimensions {
        for bound in dimension.lower.iter().chain(&dimension.upper).flatten() {
            check_extent(&bound.numerator, &coordinate_bounds, bound.denominator)?;
        }
    }
    Ok(ScanPlan {
        dimensions: scan_dimensions,
        statements,
        coordinate_bounds,
        work: work.used,
    })
}

fn to_affine(row: &[i128]) -> Result<Affine> {
    Ok(Affine::new(
        row[..row.len() - 1]
            .iter()
            .copied()
            .map(narrow)
            .collect::<Result<Vec<_>>>()?,
        narrow(row[row.len() - 1])?,
    ))
}

fn check_extent(expression: &Affine, bounds: &[(i64, i64)], extra: i64) -> Result<()> {
    let mut magnitude = i128::from(expression.constant).abs() + i128::from(extra);
    for (&coefficient, &(low, high)) in expression.coefficients.iter().zip(bounds) {
        magnitude = magnitude
            .checked_add(
                i128::from(coefficient)
                    .abs()
                    .checked_mul(i128::from(low).abs().max(i128::from(high).abs()))
                    .ok_or(Error::ArithmeticOverflow)?,
            )
            .ok_or(Error::ArithmeticOverflow)?;
    }
    if magnitude >= i128::from(i64::MAX) {
        return Err(Error::ArithmeticOverflow);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::polyhedral::{Access, AccessKind, Domain, ScheduleOptions, Statement, schedule};
    #[test]
    fn projected_scans_cover_constrained_domains_under_unimodular_bases() {
        let domain = Domain {
            bounds: vec![-2..3, -1..4],
            constraints: vec![Affine::new(vec![1, 1], 0)],
        };
        let region = Region {
            statements: vec![Statement {
                domain: domain.clone(),
                original_schedule: vec![Affine::axis(2, 0), Affine::axis(2, 1)],
                accesses: vec![],
            }],
        };
        let mut expected = BTreeSet::new();
        for i in -2..3 {
            for j in -1..4 {
                if domain.contains(&[i, j]).unwrap() {
                    expected.insert(vec![i, j]);
                }
            }
        }
        for matrix in [
            [[1, 2], [0, 1]],
            [[0, 1], [1, 0]],
            [[1, -1], [0, 1]],
            [[-1, 0], [0, 1]],
            [[2, 1], [1, 1]],
        ] {
            let schedule = Schedule {
                rows: matrix
                    .into_iter()
                    .map(|row| vec![Affine::new(row.to_vec(), -1)])
                    .collect(),
                bands: vec![0..2],
                distances: vec![],
                stats: Default::default(),
            };
            for sizes in [vec![1, 1], vec![2, 3], vec![11, 11]] {
                let plan =
                    scan(&region, &schedule, Some(&Tile { band: 0, sizes }), 100_000).unwrap();
                let mut visited = BTreeSet::new();
                plan.for_each_instance(100_000, |s, point| {
                    assert_eq!(s, 0);
                    assert!(visited.insert(point.to_vec()));
                })
                .unwrap();
                assert_eq!(visited, expected, "basis {matrix:?}");
            }
        }
    }
    fn example() -> Region {
        Region {
            statements: (0..2)
                .map(|s| Statement {
                    domain: Domain::rectangular(vec![-2..7, 1..12]),
                    original_schedule: vec![
                        Affine::axis(2, 0),
                        Affine::constant(2, s),
                        Affine::axis(2, 1),
                    ],
                    accesses: vec![Access {
                        object: 0,
                        subscripts: vec![Affine::axis(2, 0), Affine::axis(2, 1)],
                        kind: if s == 0 {
                            AccessKind::Write
                        } else {
                            AccessKind::Read
                        },
                    }],
                })
                .collect(),
        }
    }
    #[test]
    fn tiling_visits_every_instance_once_in_a_legal_order_including_tails() {
        let region = example();
        let scheduled = schedule(&region, &ScheduleOptions::default()).unwrap();
        for sizes in [vec![1, 1], vec![4, 3], vec![16, 16]] {
            let plan = scan(
                &region,
                &scheduled,
                Some(&Tile { band: 0, sizes }),
                1_000_000,
            )
            .unwrap();
            let mut visited = BTreeSet::new();
            let mut writes = BTreeSet::new();
            plan.for_each_instance(100_000, |s, point| {
                assert!(region.statements[s].domain.contains(point).unwrap());
                assert!(visited.insert((s, point.to_vec())));
                if s == 0 {
                    writes.insert(point.to_vec());
                } else {
                    assert!(writes.contains(point));
                }
            })
            .unwrap();
            assert_eq!(visited.len(), 2 * 9 * 11);
        }
    }
    #[test]
    fn rejects_nonintegral_inverse_and_invalid_tile_sizes() {
        let region = example();
        let mut scheduled = schedule(&region, &ScheduleOptions::default()).unwrap();
        assert!(
            scan(
                &region,
                &scheduled,
                Some(&Tile {
                    band: 0,
                    sizes: vec![0, 4]
                }),
                1_000_000
            )
            .is_err()
        );
        for row in &mut scheduled.rows[0] {
            for coefficient in &mut row.coefficients {
                *coefficient *= 2;
            }
        }
        assert_eq!(
            scan(&region, &scheduled, None, 1_000_000).unwrap_err(),
            Error::NonIntegralScan
        );
    }
}
