use super::geometry::{Polyhedron, difference, evaluate};
use super::integer::{Constraint, lex_min};
use super::rational::{Q, integers, nullspace};
use super::{AccessKind, Affine, Error, Region, Result, Work};
use std::collections::BTreeSet;
use std::ops::Range;

#[derive(Clone, Debug)]
pub struct ScheduleOptions {
    pub max_coefficient: i64,
    pub max_constant: i64,
    pub max_work: usize,
    pub max_statements: usize,
    pub max_dimensions: usize,
    pub input_reuse: bool,
}

impl Default for ScheduleOptions {
    fn default() -> Self {
        Self {
            max_coefficient: 8,
            max_constant: 32,
            max_work: 2_000_000,
            max_statements: 8,
            max_dimensions: 4,
            input_reuse: true,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct ScheduleStats {
    pub dependence_polyhedra: usize,
    pub dependence_vertices: usize,
    pub integer_constraints: usize,
    pub work: usize,
}

#[derive(Clone, Debug)]
pub struct Schedule {
    /// Outer index: time dimension. Inner index: statement.
    pub rows: Vec<Vec<Affine>>,
    /// Maximal bands proved componentwise nonnegative before removing any
    /// dependence carried by the band. Only dimensions within one band may
    /// be tiled together.
    pub bands: Vec<Range<usize>>,
    /// Minimized maximum dependence/reuse distance for each affine row.
    /// Scalar SCC splitters have no distance objective.
    pub distances: Vec<Option<i64>>,
    pub stats: ScheduleStats,
}

#[derive(Clone)]
struct Dependence {
    source: usize,
    target: usize,
    ordering: bool,
    polyhedron: Polyhedron,
    vertices: Vec<Vec<Q>>,
}

fn validate(region: &Region, options: &ScheduleOptions, work: &mut Work) -> Result<()> {
    if region.statements.is_empty() || region.statements.len() > options.max_statements {
        return Err(Error::Invalid("statement count"));
    }
    if options.max_coefficient < 1 || options.max_constant < 0 {
        return Err(Error::Invalid("coefficient bounds"));
    }
    let time_dimensions = region.statements[0].original_schedule.len();
    let mut objects = std::collections::BTreeMap::new();
    for statement in &region.statements {
        let dimensions = statement.domain.bounds.len();
        if dimensions > options.max_dimensions
            || time_dimensions == 0
            || statement.original_schedule.len() != time_dimensions
        {
            return Err(Error::Invalid("iteration or schedule dimensions"));
        }
        if statement
            .domain
            .constraints
            .iter()
            .chain(&statement.original_schedule)
            .chain(
                statement
                    .accesses
                    .iter()
                    .flat_map(|access| &access.subscripts),
            )
            .any(|expression| expression.coefficients.len() != dimensions)
        {
            return Err(Error::Invalid("expression dimension mismatch"));
        }
        for access in &statement.accesses {
            if let Some(previous) = objects.insert(access.object, access.subscripts.len())
                && previous != access.subscripts.len()
            {
                return Err(Error::Invalid("inconsistent memory element partitions"));
            }
        }
        let rows = statement
            .original_schedule
            .iter()
            .map(|row| row.coefficients.clone())
            .collect::<Vec<_>>();
        if !nullspace(&rows, dimensions)?.is_empty() {
            return Err(Error::Invalid("original schedule is not injective"));
        }
    }
    // Do not invent an order between simultaneous statement instances.
    for source in 0..region.statements.len() {
        for target in source + 1..region.statements.len() {
            let a = &region.statements[source];
            let b = &region.statements[target];
            let mut poly = Polyhedron::new(a.domain.bounds.len() + b.domain.bounds.len());
            poly.add_domain(&a.domain, 0)?;
            poly.add_domain(&b.domain, a.domain.bounds.len())?;
            for (left, right) in a.original_schedule.iter().zip(&b.original_schedule) {
                poly.equalities.push(difference(left, right));
            }
            if !poly.vertices(work)?.is_empty() {
                return Err(Error::Invalid("original statement times overlap"));
            }
        }
    }
    Ok(())
}

fn dependences(region: &Region, input_reuse: bool, work: &mut Work) -> Result<Vec<Dependence>> {
    let mut output = Vec::new();
    let mut seen = BTreeSet::new();
    for (source, a) in region.statements.iter().enumerate() {
        for (target, b) in region.statements.iter().enumerate() {
            for left in &a.accesses {
                for right in &b.accesses {
                    if left.object != right.object {
                        continue;
                    }
                    let ordering =
                        left.kind == AccessKind::Write || right.kind == AccessKind::Write;
                    if !ordering && !input_reuse {
                        continue;
                    }
                    let mut poly = Polyhedron::new(a.domain.bounds.len() + b.domain.bounds.len());
                    poly.add_domain(&a.domain, 0)?;
                    poly.add_domain(&b.domain, a.domain.bounds.len())?;
                    for (l, r) in left.subscripts.iter().zip(&right.subscripts) {
                        poly.equalities.push(difference(l, r));
                    }
                    // The union of these disjoint pieces represents strict
                    // lexicographic order in the original program. RAW, WAR,
                    // and WAW are all retained; last-writer elimination is an
                    // optional future precision improvement, not assumed.
                    for (l, r) in a.original_schedule.iter().zip(&b.original_schedule) {
                        let delta = difference(l, r);
                        let mut earlier = delta.clone();
                        earlier[poly.dimensions] = earlier[poly.dimensions].sub(Q::ONE)?;
                        poly.inequalities.push(earlier);
                        let vertices = poly.vertices(work)?;
                        if !vertices.is_empty()
                            && seen.insert((source, target, ordering, vertices.clone()))
                        {
                            output.push(Dependence {
                                source,
                                target,
                                ordering,
                                polyhedron: poly.clone(),
                                vertices,
                            });
                        }
                        poly.inequalities.pop();
                        poly.equalities.push(delta);
                    }
                }
            }
        }
    }
    Ok(output)
}

fn complements(region: &Region, rows: &[Vec<Affine>]) -> Result<Vec<Vec<Vec<i128>>>> {
    region
        .statements
        .iter()
        .enumerate()
        .map(|(s, statement)| {
            nullspace(
                &rows
                    .iter()
                    .map(|row| row[s].coefficients.clone())
                    .collect::<Vec<_>>(),
                statement.domain.bounds.len(),
            )
        })
        .collect()
}

fn synthesize_row(
    region: &Region,
    dependencies: &[Dependence],
    complements: &[Vec<Vec<i128>>],
    options: &ScheduleOptions,
    stats: &mut ScheduleStats,
    work: &mut Work,
) -> Result<Option<(Vec<Affine>, i64)>> {
    let mut starts = Vec::new();
    let mut variables = 1; // The common distance bound is the first objective.
    let mut magnitude = 0i128;
    for statement in &region.statements {
        starts.push(variables);
        variables += statement.domain.bounds.len() + 1;
        for range in &statement.domain.bounds {
            magnitude = magnitude
                .checked_add(
                    i128::from(range.start)
                        .abs()
                        .max(i128::from(range.end).abs()),
                )
                .ok_or(Error::ArithmeticOverflow)?;
        }
    }
    let maximum_distance = magnitude
        .checked_mul(i128::from(options.max_coefficient))
        .and_then(|v| v.checked_mul(2))
        .and_then(|v| v.checked_add(2 * i128::from(options.max_constant)))
        .ok_or(Error::ArithmeticOverflow)?;
    let mut bounds = vec![(
        0,
        i64::try_from(maximum_distance).map_err(|_| Error::ArithmeticOverflow)?,
    )];
    let mut order = vec![0];
    let mut clauses = Vec::new();
    for (s, statement) in region.statements.iter().enumerate() {
        let d = statement.domain.bounds.len();
        bounds.extend((0..d).map(|_| {
            (
                0,
                if complements[s].is_empty() {
                    0
                } else {
                    options.max_coefficient
                },
            )
        }));
        bounds.push((0, options.max_constant));
        // Prefer preserving outer axes when dependence-distance objectives
        // tie. This is a deterministic tie break, not a legality heuristic.
        order.extend((0..d).rev().map(|i| starts[s] + i));
        let mut clause = Vec::new();
        for vector in &complements[s] {
            for sign in [-1i128, 1] {
                let mut coefficients = vec![0; variables];
                for (i, &coefficient) in vector.iter().enumerate() {
                    coefficients[starts[s] + i] = coefficient
                        .checked_mul(sign)
                        .ok_or(Error::ArithmeticOverflow)?;
                }
                clause.push(Constraint {
                    coefficients,
                    constant: -1,
                });
            }
        }
        if !clause.is_empty() {
            clauses.push(clause);
        }
    }
    for (s, statement) in region.statements.iter().enumerate() {
        order.push(starts[s] + statement.domain.bounds.len());
    }
    let mut constraints = BTreeSet::new();
    for dependency in dependencies {
        let source_dimensions = region.statements[dependency.source].domain.bounds.len();
        let target_dimensions = region.statements[dependency.target].domain.bounds.len();
        for point in &dependency.vertices {
            let mut delta = vec![Q::ZERO; variables];
            for (i, &coordinate) in point[..source_dimensions].iter().enumerate() {
                let slot = starts[dependency.source] + i;
                delta[slot] = delta[slot].sub(coordinate)?;
            }
            for (i, &coordinate) in point[source_dimensions..].iter().enumerate() {
                let slot = starts[dependency.target] + i;
                delta[slot] = delta[slot].add(coordinate)?;
            }
            let source_constant = starts[dependency.source] + source_dimensions;
            let target_constant = starts[dependency.target] + target_dimensions;
            delta[source_constant] = delta[source_constant].sub(Q::ONE)?;
            delta[target_constant] = delta[target_constant].add(Q::ONE)?;
            if dependency.ordering {
                constraints.insert(Constraint {
                    coefficients: integers(&delta)?,
                    constant: 0,
                });
            }
            let mut upper = delta.iter().map(|&q| q.neg()).collect::<Result<Vec<_>>>()?;
            upper[0] = Q::ONE;
            constraints.insert(Constraint {
                coefficients: integers(&upper)?,
                constant: 0,
            });
            if !dependency.ordering {
                delta[0] = Q::ONE;
                constraints.insert(Constraint {
                    coefficients: integers(&delta)?,
                    constant: 0,
                });
            }
        }
    }
    stats.integer_constraints += constraints.len();
    let Some(solution) = lex_min(
        &constraints.into_iter().collect::<Vec<_>>(),
        &clauses,
        bounds,
        &order,
        work,
    )?
    else {
        return Ok(None);
    };
    let rows = region
        .statements
        .iter()
        .enumerate()
        .map(|(s, statement)| {
            let start = starts[s];
            let end = start + statement.domain.bounds.len();
            Affine::new(solution[start..end].to_vec(), solution[end])
        })
        .collect();
    Ok(Some((rows, solution[0])))
}

fn residual(
    mut dependencies: Vec<Dependence>,
    rows: &[Vec<Affine>],
    work: &mut Work,
) -> Result<Vec<Dependence>> {
    let mut output = Vec::new();
    for mut dependency in dependencies.drain(..) {
        if !dependency.ordering {
            output.push(dependency);
            continue;
        }
        for row in rows {
            dependency
                .polyhedron
                .equalities
                .push(difference(&row[dependency.source], &row[dependency.target]));
        }
        dependency.vertices = dependency.polyhedron.vertices(work)?;
        if !dependency.vertices.is_empty() {
            output.push(dependency);
        }
    }
    Ok(output)
}

fn splitter(region: &Region, dependencies: &[Dependence]) -> Option<Vec<Affine>> {
    let n = region.statements.len();
    let mut reachable = vec![vec![false; n]; n];
    for (i, row) in reachable.iter_mut().enumerate() {
        row[i] = true;
    }
    for dependency in dependencies.iter().filter(|d| d.ordering) {
        reachable[dependency.source][dependency.target] = true;
    }
    for k in 0..n {
        for i in 0..n {
            for j in 0..n {
                reachable[i][j] |= reachable[i][k] && reachable[k][j];
            }
        }
    }
    let mut component = vec![usize::MAX; n];
    let mut components = 0;
    for i in 0..n {
        if component[i] != usize::MAX {
            continue;
        }
        for j in i..n {
            if reachable[i][j] && reachable[j][i] {
                component[j] = components;
            }
        }
        components += 1;
    }
    if !dependencies
        .iter()
        .any(|d| d.ordering && component[d.source] != component[d.target])
    {
        return None;
    }
    let mut edges = vec![vec![false; components]; components];
    for d in dependencies.iter().filter(|d| d.ordering) {
        if component[d.source] != component[d.target] {
            edges[component[d.source]][component[d.target]] = true;
        }
    }
    let mut rank = vec![usize::MAX; components];
    for next in 0..components {
        let node = (0..components).find(|&i| {
            rank[i] == usize::MAX && (0..components).all(|j| !edges[j][i] || rank[j] != usize::MAX)
        })?;
        rank[node] = next;
    }
    Some(
        region
            .statements
            .iter()
            .enumerate()
            .map(|(s, statement)| {
                Affine::constant(statement.domain.bounds.len(), rank[component[s]] as i64)
            })
            .collect(),
    )
}

pub fn schedule(region: &Region, options: &ScheduleOptions) -> Result<Schedule> {
    let mut work = Work {
        remaining: options.max_work,
        used: 0,
    };
    validate(region, options, &mut work)?;
    let mut dependencies = dependences(region, options.input_reuse, &mut work)?;
    let mut result = Schedule {
        rows: Vec::new(),
        bands: Vec::new(),
        distances: Vec::new(),
        stats: ScheduleStats {
            dependence_polyhedra: dependencies.len(),
            dependence_vertices: dependencies.iter().map(|d| d.vertices.len()).sum(),
            ..Default::default()
        },
    };
    let mut band_start = 0;
    loop {
        work.spend()?;
        let complements = complements(region, &result.rows)?;
        let complete = complements.iter().all(Vec::is_empty);
        if !complete
            && let Some((row, distance)) = synthesize_row(
                region,
                &dependencies,
                &complements,
                options,
                &mut result.stats,
                &mut work,
            )?
        {
            result.rows.push(row);
            result.distances.push(Some(distance));
            continue;
        }
        if band_start != result.rows.len() {
            result.bands.push(band_start..result.rows.len());
            dependencies = residual(dependencies, &result.rows[band_start..], &mut work)?;
            band_start = result.rows.len();
            continue;
        }
        if complete && dependencies.iter().all(|d| !d.ordering) {
            break;
        }
        let Some(row) = splitter(region, &dependencies) else {
            return Err(Error::NoSchedule);
        };
        dependencies = residual(dependencies, std::slice::from_ref(&row), &mut work)?;
        result.rows.push(row);
        result.distances.push(None);
        band_start = result.rows.len();
    }
    verify_with_work(region, &result, &mut work)?;
    result.stats.work = work.used;
    Ok(result)
}

/// An independent legality check, including band permutability. It does not
/// trust the integer solver's result or its distance objective.
pub fn verify_schedule(region: &Region, schedule: &Schedule, max_work: usize) -> Result<()> {
    let mut work = Work {
        remaining: max_work,
        used: 0,
    };
    validate(region, &ScheduleOptions::default(), &mut work)?;
    verify_with_work(region, schedule, &mut work)
}

fn verify_with_work(region: &Region, schedule: &Schedule, work: &mut Work) -> Result<()> {
    if schedule.rows.iter().any(|row| {
        row.len() != region.statements.len()
            || row
                .iter()
                .zip(&region.statements)
                .any(|(row, s)| row.coefficients.len() != s.domain.bounds.len())
    }) {
        return Err(Error::Invalid("candidate schedule dimensions"));
    }
    if complements(region, &schedule.rows)?
        .iter()
        .any(|space| !space.is_empty())
    {
        return Err(Error::Invalid("candidate schedule is not injective"));
    }
    let mut previous_end = 0;
    for band in &schedule.bands {
        if band.start < previous_end || band.start >= band.end || band.end > schedule.rows.len() {
            return Err(Error::Invalid("candidate band ranges"));
        }
        previous_end = band.end;
    }
    for dependency in dependences(region, false, work)? {
        for band in &schedule.bands {
            let mut poly = dependency.polyhedron.clone();
            for row in &schedule.rows[..band.start] {
                poly.equalities
                    .push(difference(&row[dependency.source], &row[dependency.target]));
            }
            let vertices = poly.vertices(work)?;
            for row in &schedule.rows[band.clone()] {
                let delta = difference(&row[dependency.source], &row[dependency.target]);
                for point in &vertices {
                    if evaluate(&delta, point)?.n < 0 {
                        return Err(Error::Invalid("candidate band reverses a dependence"));
                    }
                }
            }
        }
        let mut poly = dependency.polyhedron;
        let mut vertices = dependency.vertices;
        for row in &schedule.rows {
            if vertices.is_empty() {
                break;
            }
            let delta = difference(&row[dependency.source], &row[dependency.target]);
            for point in &vertices {
                if evaluate(&delta, point)?.n < 0 {
                    return Err(Error::Invalid("candidate reverses a dependence"));
                }
            }
            poly.equalities.push(delta);
            vertices = poly.vertices(work)?;
        }
        if !vertices.is_empty() {
            return Err(Error::Invalid("candidate leaves a dependence unordered"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::polyhedral::{Access, Domain, Statement};
    fn access(object: usize, coefficients: &[i64], constant: i64, kind: AccessKind) -> Access {
        Access {
            object,
            subscripts: vec![Affine::new(coefficients.to_vec(), constant)],
            kind,
        }
    }
    fn shifted_region() -> Region {
        use AccessKind::{Read, Write};
        Region {
            statements: vec![
                Statement {
                    domain: Domain::rectangular(vec![0..16]),
                    original_schedule: vec![Affine::constant(1, 0), Affine::axis(1, 0)],
                    accesses: vec![access(0, &[1], 0, Read), access(1, &[1], 0, Write)],
                },
                Statement {
                    domain: Domain::rectangular(vec![1..15]),
                    original_schedule: vec![Affine::constant(1, 1), Affine::axis(1, 0)],
                    accesses: vec![
                        access(1, &[1], -1, Read),
                        access(1, &[1], 1, Read),
                        access(2, &[1], 0, Write),
                    ],
                },
            ],
        }
    }
    fn jacobi() -> Region {
        use AccessKind::{Read, Write};
        Region {
            statements: vec![
                Statement {
                    domain: Domain::rectangular(vec![0..4, 1..15]),
                    original_schedule: vec![
                        Affine::axis(2, 0),
                        Affine::constant(2, 0),
                        Affine::axis(2, 1),
                    ],
                    accesses: vec![
                        access(0, &[0, 1], -1, Read),
                        access(0, &[0, 1], 0, Read),
                        access(0, &[0, 1], 1, Read),
                        access(1, &[0, 1], 0, Write),
                    ],
                },
                Statement {
                    domain: Domain::rectangular(vec![0..4, 1..15]),
                    original_schedule: vec![
                        Affine::axis(2, 0),
                        Affine::constant(2, 1),
                        Affine::axis(2, 1),
                    ],
                    accesses: vec![access(1, &[0, 1], 0, Read), access(0, &[0, 1], 0, Write)],
                },
            ],
        }
    }
    #[test]
    fn synthesizes_a_producer_consumer_shift_before_fusion() {
        let region = shifted_region();
        let result = schedule(&region, &ScheduleOptions::default()).unwrap();
        assert_eq!(
            result.rows[0],
            vec![Affine::new(vec![1], 0), Affine::new(vec![1], 1)]
        );
        assert_eq!(
            result.rows[1],
            vec![Affine::constant(1, 0), Affine::constant(1, 1)]
        );
        assert_eq!(result.bands, vec![0..1]);
        assert_eq!(result.distances[0], Some(2));
    }
    #[test]
    fn derives_the_published_imperfect_jacobi_skew_and_relative_shift() {
        let result = schedule(&jacobi(), &ScheduleOptions::default()).unwrap();
        assert_eq!(result.rows[0], vec![Affine::new(vec![1, 0], 0); 2]);
        assert_eq!(
            result.rows[1],
            vec![Affine::new(vec![2, 1], 0), Affine::new(vec![2, 1], 1)]
        );
        assert_eq!(result.bands, vec![0..2]);
    }
    #[test]
    fn dependence_constraints_choose_different_axes_for_different_statements() {
        use AccessKind::{Read, Write};
        let mut region = Region {
            statements: Vec::new(),
        };
        for s in 0..2 {
            region.statements.push(Statement {
                domain: Domain::rectangular(vec![0..8, 0..8]),
                original_schedule: vec![
                    Affine::constant(2, s as i64),
                    Affine::axis(2, 0),
                    Affine::axis(2, 1),
                ],
                accesses: vec![Access {
                    object: 0,
                    subscripts: if s == 0 {
                        vec![Affine::axis(2, 0), Affine::axis(2, 1)]
                    } else {
                        vec![Affine::axis(2, 1), Affine::axis(2, 0)]
                    },
                    kind: if s == 0 { Write } else { Read },
                }],
            });
        }
        let result = schedule(&region, &ScheduleOptions::default()).unwrap();
        assert_eq!(result.rows[0], vec![Affine::axis(2, 0), Affine::axis(2, 1)]);
        assert_eq!(result.rows[1], vec![Affine::axis(2, 1), Affine::axis(2, 0)]);
    }
    #[test]
    fn verifier_rejects_an_illegal_fusion_and_an_illegal_tile_band() {
        let region = shifted_region();
        let mut result = schedule(&region, &ScheduleOptions::default()).unwrap();
        result.rows[0][1].constant = 0;
        assert!(verify_schedule(&region, &result, 1_000_000).is_err());
        let region = jacobi();
        let original = Schedule {
            rows: (0..3)
                .map(|i| {
                    region
                        .statements
                        .iter()
                        .map(|s| s.original_schedule[i].clone())
                        .collect()
                })
                .collect(),
            bands: vec![0..3],
            distances: Vec::new(),
            stats: ScheduleStats::default(),
        };
        assert!(verify_schedule(&region, &original, 1_000_000).is_err());
        let mut legal = original;
        legal.bands.clear();
        verify_schedule(&region, &legal, 1_000_000).unwrap();
    }
    #[test]
    fn work_limits_and_malformed_models_return_errors() {
        let mut options = ScheduleOptions {
            max_work: 0,
            ..Default::default()
        };
        assert!(matches!(
            schedule(&shifted_region(), &options),
            Err(Error::WorkLimit)
        ));
        options.max_work = 100_000;
        let mut region = shifted_region();
        region.statements[1].original_schedule[0].constant = 0;
        assert!(matches!(
            schedule(&region, &options),
            Err(Error::Invalid(_))
        ));
    }
}
