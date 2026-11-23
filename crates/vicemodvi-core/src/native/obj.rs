//! Bounded Wavefront OBJ reader for polygon geometry and UVs.
//!
//! Polygons are triangulated by ear clipping after checking that they are
//! planar and simple. Faces with and without UVs go to separate meshes, and
//! each mesh gets one vertex per distinct position/UV pair.

use std::collections::HashMap;

use super::model::{Mesh, Model, Vec2, Vec3};
use crate::{Error, Result};

pub const MAX_CHARACTERS: usize = 32 * 1024 * 1024;
const MAX_LINE_BYTES: usize = 1024 * 1024;
const MAX_SOURCE_VERTICES: usize = 1_000_000;
const MAX_OUTPUT_VERTICES: usize = 2_000_000;
const MAX_TRIANGLES: usize = 2_000_000;
const MAX_MESHES: usize = 1024;
const MAX_CORNERS: usize = 256;
const MAX_WORK: u64 = 50_000_000;
const MAX_COORDINATE: f32 = 1_000_000.0;

fn fail(message: &str) -> Error {
    Error::invalid(message)
}

/// A mesh being built: one per run of faces in the same group with the
/// same UV presence.
struct Part {
    name: String,
    has_uv: bool,
    corners: HashMap<(usize, Option<usize>), i32>,
    positions: Vec<Vec3>,
    tex_coords: Vec<Vec2>,
    indices: Vec<i32>,
}

#[derive(Default)]
struct Reader {
    positions: Vec<Vec3>,
    uvs: Vec<Vec2>,
    normals: usize,
    parts: Vec<Part>,
    open_part: Option<usize>,
    group: String,
    output_vertices: usize,
    triangles: usize,
    work: u64,
}

/// Parses OBJ text into a model named `name`.
pub fn parse(text: &str, name: &str) -> Result<Model> {
    if text.encode_utf16().count() > MAX_CHARACTERS {
        return Err(fail("OBJ text exceeds the 64 MiB text-memory limit."));
    }
    let mut reader = Reader { group: "default".into(), ..Reader::default() };
    for (number, line) in lines(text).enumerate() {
        reader.line(line, number == 0).map_err(|error| Error::invalid(format!("OBJ line {}: {error}", number + 1)))?;
    }
    reader.finish(name)
}

impl Reader {
    fn line(&mut self, raw: &str, first: bool) -> Result<()> {
        if raw.len() > MAX_LINE_BYTES {
            return Err(fail("Line is too long."));
        }
        let content = raw.split('#').next().unwrap_or_default().trim();
        let content = if first { content.trim_start_matches('\u{FEFF}') } else { content };
        let fields: Vec<&str> = content.split_whitespace().collect();
        match fields.split_first() {
            Some((&"v", values)) => self.vertex(values),
            Some((&"vt", values)) => self.uv(values),
            Some((&"vn", values)) => self.normal(values),
            Some((&("o" | "g"), words)) => {
                self.group = clean_name(&words.join(" "), "default");
                self.open_part = None;
                Ok(())
            }
            Some((&"f", corners)) => self.face(corners),
            _ => Ok(()),
        }
    }

    fn vertex(&mut self, values: &[&str]) -> Result<()> {
        // x y z, x y z w, x y z r g b, or x y z w r g b.
        if !matches!(values.len(), 3 | 4 | 6 | 7) {
            return Err(fail("Expected a 3D vertex."));
        }
        if self.positions.len() >= MAX_SOURCE_VERTICES {
            return Err(fail("Vertex limit exceeded."));
        }
        let numbers = values.iter().map(|v| number(v)).collect::<Result<Vec<f32>>>()?;
        let mut point = [numbers[0], numbers[1], numbers[2]];
        if matches!(values.len(), 4 | 7) {
            let w = numbers[3];
            if w == 0.0 {
                return Err(fail("Zero homogeneous coordinate."));
            }
            point = point.map(|v| v / w);
        }
        if !point.iter().all(|v| v.is_finite() && v.abs() <= MAX_COORDINATE) {
            return Err(fail("Vertex is outside the supported finite range."));
        }
        self.positions.push(point);
        Ok(())
    }

    fn uv(&mut self, values: &[&str]) -> Result<()> {
        if !(1..=3).contains(&values.len()) || self.uvs.len() >= MAX_SOURCE_VERTICES {
            return Err(fail("Invalid texture coordinate count."));
        }
        let numbers = values.iter().map(|v| number(v)).collect::<Result<Vec<f32>>>()?;
        self.uvs.push([numbers[0], numbers.get(1).copied().unwrap_or(0.0)]);
        Ok(())
    }

    fn normal(&mut self, values: &[&str]) -> Result<()> {
        self.normals += 1;
        if values.len() != 3 || self.normals > MAX_SOURCE_VERTICES {
            return Err(fail("Invalid normal count."));
        }
        values.iter().try_for_each(|v| number(v).map(drop))
    }

    fn face(&mut self, fields: &[&str]) -> Result<()> {
        if !(3..=MAX_CORNERS).contains(&fields.len()) {
            return Err(fail("Face must have 3 to 256 corners."));
        }
        let mut corners = Vec::with_capacity(fields.len());
        let mut has_uv = true;
        for field in fields {
            let parts: Vec<&str> = field.split('/').collect();
            if parts.len() > 3 {
                return Err(fail("Invalid face index."));
            }
            let position = index(parts[0], self.positions.len())?;
            let uv = match parts.get(1) {
                Some(text) if !text.is_empty() => Some(index(text, self.uvs.len())?),
                _ => {
                    has_uv = false;
                    None
                }
            };
            if let Some(normal) = parts.get(2) {
                index(normal, self.normals)?;
            }
            corners.push((position, uv));
        }
        let mut distinct: Vec<usize> = corners.iter().map(|c| c.0).collect();
        distinct.sort_unstable();
        distinct.dedup();
        if distinct.len() != corners.len() {
            return Err(fail("Face repeats a vertex."));
        }
        let points: Vec<Vec3> = corners.iter().map(|c| self.positions[c.0]).collect();
        let triangles = triangulate(&points, &mut self.work)?;
        self.triangles += triangles.len() / 3;
        if self.triangles > MAX_TRIANGLES {
            return Err(fail("Triangle limit exceeded."));
        }
        let part = self.part(has_uv)?;
        let mut mapped = Vec::with_capacity(corners.len());
        for &(position, uv) in &corners {
            let key = (position, if has_uv { uv } else { None });
            let existing = self.parts[part].corners.get(&key).copied();
            let at = match existing {
                Some(at) => at,
                None => {
                    self.output_vertices += 1;
                    if self.output_vertices > MAX_OUTPUT_VERTICES {
                        return Err(fail("UV-seam vertex limit exceeded."));
                    }
                    let target = &mut self.parts[part];
                    let at = target.positions.len() as i32;
                    target.corners.insert(key, at);
                    target.positions.push(self.positions[position]);
                    if let (true, Some(uv)) = (has_uv, uv) {
                        target.tex_coords.push(self.uvs[uv]);
                    }
                    at
                }
            };
            mapped.push(at);
        }
        self.parts[part].indices.extend(triangles.iter().map(|&corner| mapped[corner]));
        Ok(())
    }

    /// The current part, or a new one when the UV presence changes.
    fn part(&mut self, has_uv: bool) -> Result<usize> {
        if let Some(open) = self.open_part.filter(|&i| self.parts[i].has_uv == has_uv) {
            return Ok(open);
        }
        if self.parts.len() >= MAX_MESHES {
            return Err(fail("Mesh limit exceeded."));
        }
        self.parts.push(Part { name: self.group.clone(), has_uv, corners: HashMap::new(), positions: Vec::new(), tex_coords: Vec::new(), indices: Vec::new() });
        self.open_part = Some(self.parts.len() - 1);
        Ok(self.parts.len() - 1)
    }

    fn finish(self, name: &str) -> Result<Model> {
        if self.parts.is_empty() || self.triangles == 0 {
            return Err(fail("OBJ contains no supported polygon faces."));
        }
        let (mut low, mut high) = ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]);
        for point in self.parts.iter().flat_map(|p| &p.positions) {
            for axis in 0..3 {
                low[axis] = low[axis].min(point[axis]);
                high[axis] = high[axis].max(point[axis]);
            }
        }
        let meshes = self
            .parts
            .into_iter()
            .map(|part| Mesh {
                name: part.name,
                tex_coords: part.has_uv.then_some(part.tex_coords),
                positions: part.positions,
                indices: part.indices,
                vertex_colors: None,
                shader_index: None,
            })
            .collect();
        let mut model = Model::new(clean_name(name, "Imported OBJ"), meshes, low, high);
        model.presentation_note = Some("Imported OBJ geometry; external material libraries and texture paths are not loaded.".into());
        Ok(model)
    }
}

/// Splits on \r\n, \r, \n, form feed, NEL, line separator and paragraph
/// separator.
fn lines(text: &str) -> impl Iterator<Item = &str> {
    let mut rest = Some(text);
    std::iter::from_fn(move || {
        let current = rest?;
        let Some((at, separator)) = current.char_indices().find(|(_, c)| matches!(c, '\r' | '\n' | '\u{000C}' | '\u{0085}' | '\u{2028}' | '\u{2029}')) else {
            rest = None;
            return Some(current);
        };
        let mut next = at + separator.len_utf8();
        if separator == '\r' && current[next..].starts_with('\n') {
            next += 1;
        }
        rest = Some(&current[next..]);
        Some(&current[..at])
    })
}

fn clean_name(value: &str, fallback: &str) -> String {
    let name: String = value.chars().filter(|c| !c.is_control()).take(256).collect();
    if name.trim().is_empty() {
        fallback.to_string()
    } else {
        name
    }
}

fn number(text: &str) -> Result<f32> {
    text.parse::<f32>()
        .ok()
        .filter(|v| v.is_finite() && v.abs() <= MAX_COORDINATE)
        .ok_or_else(|| fail("Expected a finite number within the supported range."))
}

/// Resolves a 1-based or negative (relative) OBJ index.
fn index(text: &str, available: usize) -> Result<usize> {
    let value = text.parse::<i32>().ok().filter(|&v| v != 0).ok_or_else(|| fail("Face index must be a nonzero integer."))? as i64;
    let resolved = if value > 0 { value - 1 } else { available as i64 + value };
    if !(0..available as i64).contains(&resolved) {
        return Err(fail("Face index is outside the available source list."));
    }
    Ok(resolved as usize)
}

fn minus(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

fn dot(a: Vec3, b: Vec3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn spend(work: &mut u64) -> Result<()> {
    *work += 1;
    if *work > MAX_WORK {
        return Err(fail("Polygon processing limit exceeded."));
    }
    Ok(())
}

/// Triangulates a planar simple polygon. Returns corner indices, three per
/// triangle, wound like the polygon.
fn triangulate(points: &[Vec3], work: &mut u64) -> Result<Vec<usize>> {
    let n = points.len();
    let origin = points[0];
    let extent = points.iter().map(|&p| dot(minus(p, origin), minus(p, origin)).sqrt()).fold(0f32, f32::max);
    // Newell-style normal from the fan around the first corner.
    let normal = (0..n).fold([0f32; 3], |sum, i| {
        let c = cross(minus(points[i], origin), minus(points[(i + 1) % n], origin));
        [sum[0] + c[0], sum[1] + c[1], sum[2] + c[2]]
    });
    let length_squared = dot(normal, normal);
    if extent <= 0.0 || length_squared < extent.powi(4) * 1e-14 {
        return Err(fail("Degenerate polygon."));
    }
    let length = length_squared.sqrt();
    let normal = normal.map(|v| v / length);
    if points.iter().any(|&p| dot(minus(p, origin), normal).abs() > extent * 0.00001) {
        return Err(fail("Nonplanar polygon cannot be triangulated faithfully."));
    }
    // Project onto the plane that drops the dominant normal axis.
    let [ax, ay, az] = normal.map(f32::abs);
    let dropped = if ax > ay { if ax > az { 0 } else { 2 } } else if ay > az { 1 } else { 2 };
    let flat: Vec<[f64; 2]> = points
        .iter()
        .map(|p| match dropped {
            0 => [p[1] as f64, p[2] as f64],
            1 => [p[0] as f64, p[2] as f64],
            _ => [p[0] as f64, p[1] as f64],
        })
        .collect();
    let turn = |a: usize, b: usize, c: usize| (flat[b][0] - flat[a][0]) * (flat[c][1] - flat[a][1]) - (flat[b][1] - flat[a][1]) * (flat[c][0] - flat[a][0]);
    let epsilon = (extent as f64 * extent as f64 * 1e-12).max(1e-24);
    let area: f64 = (1..n - 1).map(|i| turn(0, i, i + 1)).sum();
    if area.abs() <= epsilon {
        return Err(fail("Degenerate projected polygon."));
    }
    let winding = area.signum();
    check_simple(&flat, &turn, work)?;

    let mut ring: Vec<usize> = (0..n).collect();
    let mut triangles = Vec::with_capacity((n - 2) * 3);
    while ring.len() > 3 {
        let r = ring.len();
        let mut clipped = None;
        for i in 0..r {
            let (a, b, c) = (ring[(i + r - 1) % r], ring[i], ring[(i + 1) % r]);
            if turn(a, b, c) * winding <= epsilon {
                continue;
            }
            let mut blocked = false;
            for &d in &ring {
                spend(work)?;
                if d != a && d != b && d != c && [(a, b), (b, c), (c, a)].iter().all(|&(p, q)| turn(p, q, d) * winding >= -epsilon) {
                    blocked = true;
                    break;
                }
            }
            if !blocked {
                clipped = Some(i);
                triangles.extend_from_slice(&[a, b, c]);
                break;
            }
        }
        let i = clipped.ok_or_else(|| fail("Polygon has no supported nondegenerate triangulation."))?;
        ring.remove(i);
    }
    if turn(ring[0], ring[1], ring[2]) * winding <= epsilon {
        return Err(fail("Degenerate final triangle."));
    }
    triangles.extend_from_slice(&ring);
    Ok(triangles)
}

/// Rejects polygons whose non-adjacent edges touch or cross.
fn check_simple(flat: &[[f64; 2]], turn: &impl Fn(usize, usize, usize) -> f64, work: &mut u64) -> Result<()> {
    let n = flat.len();
    let span = |i: usize, j: usize, axis: usize| (flat[i][axis].min(flat[j][axis]), flat[i][axis].max(flat[j][axis]));
    for a in 0..n {
        for b in a + 1..n {
            spend(work)?;
            let (a2, b2) = ((a + 1) % n, (b + 1) % n);
            if a == b2 || a2 == b {
                continue;
            }
            let apart = (0..2).any(|axis| {
                let (lo1, hi1) = span(a, a2, axis);
                let (lo2, hi2) = span(b, b2, axis);
                hi1 < lo2 || hi2 < lo1
            });
            if !apart && turn(a, a2, b) * turn(a, a2, b2) <= 0.0 && turn(b, b2, a) * turn(b, b2, a2) <= 0.0 {
                return Err(fail("Self-intersecting polygon."));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quads_triangulate_and_uv_runs_split() {
        let obj = "v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nvt 0 0\nvt 1 0\nvt 1 1\nvt 0 1\no quad\nf 1/1 2/2 3/3 4/4\nf 1 2 3\n";
        let model = parse(obj, "test").unwrap();
        assert_eq!(model.meshes.len(), 2);
        assert_eq!(model.meshes[0].indices.len(), 6);
        assert_eq!(model.meshes[0].name, "quad");
        assert!(model.meshes[1].tex_coords.is_none());
    }

    #[test]
    fn errors_name_the_line() {
        let error = parse("v 0 0 0\nf 1 2 3\n", "x").unwrap_err().to_string();
        assert!(error.starts_with("OBJ line 2:"), "{error}");
    }

    #[test]
    fn concave_polygons_and_bowties() {
        // An L shape needs a reflex corner.
        let l = "v 0 0 0\nv 2 0 0\nv 2 1 0\nv 1 1 0\nv 1 2 0\nv 0 2 0\nf 1 2 3 4 5 6\n";
        assert_eq!(parse(l, "l").unwrap().meshes[0].indices.len(), 12);
        let bowtie = "v 0 0 0\nv 1 1 0\nv 1 0 0\nv 0 1 0\nf 1 2 3 4\n";
        assert!(parse(bowtie, "b").is_err());
    }
}
