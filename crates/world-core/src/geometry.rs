//! Fixed oriented cubed-sphere coordinates. All public distances use metres.

use serde::{Deserialize, Serialize};

pub const FACE_BASES: [[[f64; 3]; 3]; 6] = [
    [[1.0, 0.0, 0.0], [0.0, 0.0, -1.0], [0.0, 1.0, 0.0]],
    [[-1.0, 0.0, 0.0], [0.0, 0.0, 1.0], [0.0, 1.0, 0.0]],
    [[0.0, 1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, -1.0]],
    [[0.0, -1.0, 0.0], [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]],
    [[0.0, 0.0, 1.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
    [[0.0, 0.0, -1.0], [-1.0, 0.0, 0.0], [0.0, 1.0, 0.0]],
];

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[repr(u8)]
pub enum CubeFace {
    PositiveX,
    NegativeX,
    PositiveY,
    NegativeY,
    PositiveZ,
    NegativeZ,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct TileKey {
    pub face: u8,
    pub level: u8,
    pub x: u32,
    pub y: u32,
}

pub fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

pub fn normalize(a: [f64; 3]) -> [f64; 3] {
    let length = dot(a, a).sqrt();
    if !length.is_finite() || length <= f64::MIN_POSITIVE {
        return [1.0, 0.0, 0.0];
    }
    [a[0] / length, a[1] / length, a[2] / length]
}

pub fn direction(face: u8, u: f64, v: f64) -> [f64; 3] {
    let [n, a, b] = FACE_BASES[usize::from(face)];
    normalize([
        n[0] + u * a[0] + v * b[0],
        n[1] + u * a[1] + v * b[1],
        n[2] + u * a[2] + v * b[2],
    ])
}

/// Dominant coordinate; ties use the lowest face index, including cube corners.
pub fn face_uv(p: [f64; 3]) -> (u8, f64, f64) {
    let p = normalize(p);
    let mut face = 0usize;
    let mut dominant = f64::NEG_INFINITY;
    for (i, [n, _, _]) in FACE_BASES.iter().enumerate() {
        let score = dot(p, *n);
        if score > dominant {
            dominant = score;
            face = i;
        }
    }
    let [_, u, v] = FACE_BASES[face];
    (face as u8, dot(p, u) / dominant, dot(p, v) / dominant)
}

pub fn cell_index(face: u8, x: u32, y: u32, n: u32) -> usize {
    (u64::from(face) * u64::from(n) * u64::from(n) + u64::from(y) * u64::from(n) + u64::from(x))
        as usize
}

pub fn direction_index(p: [f64; 3], n: u32) -> usize {
    let (face, u, v) = face_uv(p);
    let x = (((u + 1.0) * 0.5 * f64::from(n)).floor() as i64).clamp(0, i64::from(n) - 1) as u32;
    let y = (((v + 1.0) * 0.5 * f64::from(n)).floor() as i64).clamp(0, i64::from(n) - 1) as u32;
    cell_index(face, x, y, n)
}

/// Four neighbours, including rotated neighbours on other cube faces.
pub fn neighbours(index: usize, n: u32) -> [usize; 4] {
    let area = n as usize * n as usize;
    let face = (index / area) as u8;
    let local = index % area;
    let x = (local % n as usize) as i32;
    let y = (local / n as usize) as i32;
    let mut result = [0; 4];
    for (i, (dx, dy)) in [(-1, 0), (1, 0), (0, -1), (0, 1)].into_iter().enumerate() {
        let nx = x + dx;
        let ny = y + dy;
        if nx >= 0 && ny >= 0 && nx < n as i32 && ny < n as i32 {
            result[i] = cell_index(face, nx as u32, ny as u32, n);
        } else {
            let u = 2.0 * (f64::from(nx) + 0.5) / f64::from(n) - 1.0;
            let v = 2.0 * (f64::from(ny) + 0.5) / f64::from(n) - 1.0;
            result[i] = direction_index(direction(face, u, v), n);
        }
    }
    result
}

fn triangle_area(a: [f64; 3], b: [f64; 3], c: [f64; 3]) -> f64 {
    2.0 * dot(a, cross(b, c))
        .abs()
        .atan2(1.0 + dot(a, b) + dot(b, c) + dot(c, a))
}

pub fn cell_area(face: u8, x: u32, y: u32, n: u32, radius: f64) -> f64 {
    let u0 = 2.0 * f64::from(x) / f64::from(n) - 1.0;
    let v0 = 2.0 * f64::from(y) / f64::from(n) - 1.0;
    let u1 = 2.0 * f64::from(x + 1) / f64::from(n) - 1.0;
    let v1 = 2.0 * f64::from(y + 1) / f64::from(n) - 1.0;
    let a = direction(face, u0, v0);
    let b = direction(face, u1, v0);
    let c = direction(face, u1, v1);
    let d = direction(face, u0, v1);
    (triangle_area(a, b, c) + triangle_area(a, c, d)) * radius * radius
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sphere_area_and_edge_neighbours_are_correct() {
        let n = 12;
        let mut area = 0.0;
        for face in 0..6 {
            for y in 0..n {
                for x in 0..n {
                    area += cell_area(face, x, y, n, 1.0);
                    let index = cell_index(face, x, y, n);
                    let center = direction(
                        face,
                        2.0 * (f64::from(x) + 0.5) / f64::from(n) - 1.0,
                        2.0 * (f64::from(y) + 0.5) / f64::from(n) - 1.0,
                    );
                    assert_eq!(direction_index(center, n), index);
                    for other in neighbours(index, n) {
                        assert!(neighbours(other, n).contains(&index));
                    }
                }
            }
        }
        assert!((area - 4.0 * std::f64::consts::PI).abs() < 1e-11);
    }
}
