//! Integration evidence for learned chart residuals over the original globe.
//! All fixtures are analytic CPU payloads; no learned inference runs here.

use std::collections::HashMap;
use std::sync::OnceLock;

use world_core::geometry::{cross, direction, dot, normalize, FACE_BASES};
use world_core::hybrid::{
    chart_direction, chart_plane, HybridField, HybridPatch, MAX_RESIDUAL_M, NATIVE_MPP, PATCH_CORE,
    PATCH_SIDE,
};
use world_core::{World, WorldConfig};

const EARTH_RADIUS_M: f64 = 6_371_000.0;

#[test]
fn full_learned_heights_retain_large_valleys_and_cubic_sampling_has_no_pixel_slope_jump() {
    use world_core::hybrid::SurfaceKind;
    let mut terrain = patch(4, 0, 0, 0.0);
    for row in 0..PATCH_SIDE {
        for column in 0..PATCH_SIDE {
            let x = (column as f64 - 0.5) * NATIVE_MPP;
            terrain.values[row * PATCH_SIDE + column] = (1800.0 + 650.0 * (x / 900.0).sin()) as f32;
        }
    }
    let learned = HybridField::with_kind(
        "full".into(),
        1,
        EARTH_RADIUS_M,
        vec![terrain],
        SurfaceKind::Height,
    )
    .unwrap();
    let height = |x| {
        learned.protected_delta(
            chart_direction(4, x, 3840.0, EARTH_RADIUS_M),
            700.0,
            None,
            None,
            0.0,
            false,
            false,
        ) + 700.0
    };
    assert!((height(3100.0) - 700.0).abs() > 500.0);
    let position = 100.5 * NATIVE_MPP;
    let h = 0.01;
    let left = (height(position) - height(position - h)) / h;
    let right = (height(position + h) - height(position)) / h;
    assert!(
        (left - right).abs() < 0.001,
        "pixel slope jump {left} vs {right}"
    );
    // Native data centres stay exact, rather than being blurred away.
    let expected = (1800.0 + 650.0 * (position / 900.0).sin()) as f32 as f64;
    assert!((height(position) - expected).abs() < 1e-6);
    assert_eq!(
        learned.protected_delta([0.0, 0.0, 1.0], 20.0, None, None, 0.0, true, false),
        0.0
    );
}

#[test]
fn fine_learned_field_fades_into_learned_preview_without_returning_to_flat_parent() {
    use world_core::hybrid::SurfaceKind;
    let mut preview = patch(4, 0, 0, 1700.0);
    preview.level = 3;
    let native = patch(4, 1, 1, 2100.0);
    let learned = HybridField::with_kind(
        "full".into(),
        2,
        EARTH_RADIUS_M,
        vec![preview, native],
        SurfaceKind::Height,
    )
    .unwrap();
    for x in [7650.0, 7680.0, 7695.0, 8000.0] {
        let p = chart_direction(4, x, 11000.0, EARTH_RADIUS_M);
        let height = 700.0 + learned.protected_delta(p, 700.0, None, None, 0.0, false, false);
        assert!(
            (1699.999999..=2100.000001).contains(&height),
            "height={height} at x={x}"
        );
    }
    // Both resolutions can own the same x/y address without overwriting it.
    let mut duplicate_address = patch(4, 1, 1, 1500.0);
    duplicate_address.level = 3;
    assert!(HybridField::with_kind(
        "full".into(),
        3,
        EARTH_RADIUS_M,
        vec![patch(4, 1, 1, 1800.0), duplicate_address],
        SurfaceKind::Height
    )
    .is_ok());
}

fn patch(face: u8, x: i32, y: i32, value: f32) -> HybridPatch {
    HybridPatch {
        face,
        level: 0,
        x,
        y,
        values: vec![value; PATCH_SIDE * PATCH_SIDE],
    }
}

fn field(radius: f64, patches: Vec<HybridPatch>) -> HybridField {
    HybridField::new("integration-world".into(), 7, radius, patches).unwrap()
}

fn all_charts() -> &'static HybridField {
    static FIELD: OnceLock<HybridField> = OnceLock::new();
    FIELD.get_or_init(|| {
        // Four patches per axis cover every positive-weight chart hemisphere
        // at the smallest supported radius, without touching outer feathers.
        let values = [10.0, -10.0, 20.0, -20.0, 30.0, -30.0];
        let mut patches = Vec::new();
        for face in 0..6 {
            for y in -2..2 {
                for x in -2..2 {
                    patches.push(patch(face, x, y, values[face as usize]));
                }
            }
        }
        field(10_000.0, patches)
    })
}

fn point_key(point: [f64; 3]) -> [i64; 3] {
    point.map(|v| (v * 1e12).round() as i64)
}

#[test]
fn conformal_charts_round_trip_and_keep_pole_sample_spacing_bounded() {
    for face in 0..6 {
        for (u, v) in [(0.0, 0.0), (1.0, -1.0), (-1.3, 0.7), (0.2, 1.1)] {
            let original = direction(face, u, v);
            let plane = chart_plane(face, original, EARTH_RADIUS_M);
            let recovered = chart_direction(face, plane[0], plane[1], EARTH_RADIUS_M);
            for axis in 0..3 {
                assert!((original[axis] - recovered[axis]).abs() < 5e-16);
            }
            let center = chart_direction(face, plane[0], plane[1], EARTH_RADIUS_M);
            let mut lengths = Vec::new();
            for offset in [[NATIVE_MPP, 0.0], [0.0, NATIVE_MPP]] {
                let adjacent = chart_direction(
                    face,
                    plane[0] + offset[0],
                    plane[1] + offset[1],
                    EARTH_RADIUS_M,
                );
                lengths.push(
                    adjacent
                        .iter()
                        .zip(center)
                        .map(|(a, b)| (a - b).powi(2))
                        .sum::<f64>()
                        .sqrt()
                        * EARTH_RADIUS_M,
                );
            }
            assert!(lengths.iter().all(|&m| m > 20.0 && m <= 30.0001));
            assert!((lengths[0] - lengths[1]).abs() < 0.001);
        }
        assert!(
            (all_charts().raw_delta(FACE_BASES[face as usize][0])
                - [10.0, -10.0, 20.0, -20.0, 30.0, -30.0][face as usize])
                .abs()
                < 1e-10
        );
    }
}

#[test]
fn all_twelve_cube_edges_and_eight_corners_share_the_same_learned_surface() {
    let field = all_charts();
    let mut edges = HashMap::<([i64; 3], [i64; 3]), Vec<Vec<[f64; 3]>>>::new();
    let mut corners = HashMap::<[i64; 3], Vec<[f64; 3]>>::new();
    for face in 0..6 {
        for fixed in [-1.0, 1.0] {
            for axis in 0..2 {
                let points: Vec<_> = (0..17)
                    .map(|i| {
                        let varying = i as f64 / 8.0 - 1.0;
                        if axis == 0 {
                            direction(face, fixed, varying)
                        } else {
                            direction(face, varying, fixed)
                        }
                    })
                    .collect();
                let mut endpoints = [point_key(points[0]), point_key(points[16])];
                endpoints.sort();
                edges
                    .entry((endpoints[0], endpoints[1]))
                    .or_default()
                    .push(points);
            }
            for v in [-1.0, 1.0] {
                let p = direction(face, fixed, v);
                corners.entry(point_key(p)).or_default().push(p);
            }
        }
    }
    assert_eq!(edges.len(), 12);
    assert_eq!(corners.len(), 8);
    for pair in edges.values() {
        assert_eq!(pair.len(), 2);
        let reversed = point_key(pair[0][0]) != point_key(pair[1][0]);
        for i in 0..17 {
            let a = pair[0][i];
            let b = pair[1][if reversed { 16 - i } else { i }];
            assert!((field.raw_delta(a) - field.raw_delta(b)).abs() < 1e-10);
        }
        let center = pair[0][8];
        let tangent = normalize(cross(center, pair[0][0]));
        let nearby = |sign: f64| {
            normalize(std::array::from_fn(|i| {
                center[i] + sign * 1e-7 * tangent[i]
            }))
        };
        assert!((field.raw_delta(nearby(-1.0)) - field.raw_delta(nearby(1.0))).abs() < 0.001);
    }
    for points in corners.values() {
        assert_eq!(points.len(), 3);
        for &point in &points[1..] {
            assert_eq!(field.raw_delta(points[0]), field.raw_delta(point));
        }
        // At a three-face overlap, equal geometric weights must average the
        // three distinct chart values rather than switch to the dominant face.
        let p = points[0];
        let expected = (10.0 * p[0].signum() + 20.0 * p[1].signum() + 30.0 * p[2].signum()) / 3.0;
        assert!((field.raw_delta(p) - expected).abs() < 1e-10);
    }
}

#[test]
fn canonical_pixel_owners_override_conflicting_halos_independently_of_patch_order() {
    let mut a = patch(4, 0, 0, 10.0);
    let mut b = patch(4, 1, 0, 20.0);
    for row in 0..PATCH_SIDE {
        a.values[row * PATCH_SIDE + PATCH_SIDE - 1] = 80.0;
        b.values[row * PATCH_SIDE] = -80.0;
    }
    let forward = field(EARTH_RADIUS_M, vec![a.clone(), b.clone()]);
    let backward = field(EARTH_RADIUS_M, vec![b, a]);
    let json = serde_json::to_string(&forward).unwrap();
    let restored: HybridField = serde_json::from_str(&json).unwrap();
    for pixel in [255.5, 255.75, 256.0, 256.25, 256.5] {
        let p = chart_direction(4, pixel * NATIVE_MPP, 128.5 * NATIVE_MPP, EARTH_RADIUS_M);
        let expected = 10.0 + (pixel - 255.5) * 10.0;
        assert!((forward.raw_delta(p) - expected).abs() < 1e-8);
        assert_eq!(forward.raw_delta(p), backward.raw_delta(p));
        assert_eq!(forward.raw_delta(p), restored.raw_delta(p));
    }
}

#[test]
fn missing_patch_boundaries_and_diagonal_holes_fade_with_zero_first_second_derivatives() {
    let isolated = field(EARTH_RADIUS_M, vec![patch(4, 0, 0, 80.0)]);
    let incomplete_quartet = field(
        EARTH_RADIUS_M,
        vec![
            patch(4, 0, 0, 80.0),
            patch(4, 1, 0, 80.0),
            patch(4, 0, 1, 80.0),
        ],
    );
    let boundary = PATCH_CORE as f64 * NATIVE_MPP;
    for (field, center, inward) in [
        (&isolated, [0.0, 128.5 * NATIVE_MPP], [1.0, 0.0]),
        (&incomplete_quartet, [boundary, boundary], [-1.0, -1.0]),
    ] {
        let sample = |distance: f64| {
            field.raw_delta(chart_direction(
                4,
                center[0] + distance * inward[0],
                center[1] + distance * inward[1],
                EARTH_RADIUS_M,
            ))
        };
        let step = 0.25;
        assert!(sample(0.0).abs() < 1e-12);
        assert!(sample(-step).abs() < 1e-12);
        let first_derivative = (sample(step) - sample(-step)) / (2.0 * step);
        let second_derivative = (sample(step) - 2.0 * sample(0.0) + sample(-step)) / step.powi(2);
        assert!(first_derivative.abs() < 1e-5);
        assert!(second_derivative.abs() < 1e-5);
        assert!(sample(step / 2.0).abs() < sample(step).abs() / 6.0);
    }
    assert_eq!(
        isolated.raw_delta(chart_direction(4, -100.0, 4000.0, EARTH_RADIUS_M)),
        0.0
    );
    assert_eq!(
        field(EARTH_RADIUS_M, vec![]).raw_delta([0.0, 1.0, 0.0]),
        0.0
    );
}

#[test]
fn direct_deserialization_rejects_invalid_payloads_and_rebuilds_valid_lookup() {
    let valid = field(EARTH_RADIUS_M, vec![patch(4, 0, 0, 40.0)]);
    let wire = serde_json::to_value(&valid).unwrap();
    let mut invalid = Vec::new();
    for (key, value) in [
        ("world_id", serde_json::json!("")),
        ("radius_m", serde_json::json!(9999.0)),
        ("revision", serde_json::json!(9_007_199_254_740_992_u64)),
    ] {
        let mut bad = wire.clone();
        bad[key] = value;
        invalid.push(bad);
    }
    let mut bad = wire.clone();
    bad["patches"][0]["face"] = serde_json::json!(6);
    invalid.push(bad);
    let mut bad = wire.clone();
    bad["patches"][0]["values"] = serde_json::json!([0.0]);
    invalid.push(bad);
    let mut bad = wire.clone();
    bad["patches"][0]["values"][0] = serde_json::json!(81.0);
    invalid.push(bad);
    let mut bad = wire.clone();
    bad["patches"]
        .as_array_mut()
        .unwrap()
        .push(wire["patches"][0].clone());
    invalid.push(bad);
    for bad in invalid {
        assert!(serde_json::from_value::<HybridField>(bad.clone()).is_err());
        assert!(HybridField::from_json(&bad.to_string()).is_err());
    }
    let restored: HybridField = serde_json::from_value(wire).unwrap();
    let p = chart_direction(4, 128.5 * NATIVE_MPP, 128.5 * NATIVE_MPP, EARTH_RADIUS_M);
    assert!((restored.raw_delta(p) - 40.0).abs() < 1e-10);
}

#[test]
fn adversarial_residuals_cannot_cross_the_authoritative_sea_or_reservoir_level() {
    let p = chart_direction(4, 128.5 * NATIVE_MPP, 128.5 * NATIVE_MPP, EARTH_RADIUS_M);
    for sign in [-1.0, 1.0] {
        let field = field(
            EARTH_RADIUS_M,
            vec![patch(4, 0, 0, (sign * MAX_RESIDUAL_M) as f32)],
        );
        for height in [0.0, 0.0001, 0.2, 1.0, 30.0, 100.0, 1000.0] {
            for river in [0.0, 0.5, 1.0] {
                for level in [None, Some(0.0), Some(height - 0.1), Some(height + 1.0)] {
                    let delta =
                        field.protected_delta(p, height, level, Some(1 << 52), river, false, false);
                    assert!(delta.is_finite() && delta.abs() <= MAX_RESIDUAL_M);
                    if height > 0.0 {
                        assert!(height + delta > 0.0);
                    }
                    if let Some(level) = level {
                        if height <= level {
                            assert_eq!(delta, 0.0);
                        } else {
                            assert!(height + delta > level);
                        }
                    }
                    if river == 1.0 || height == 0.0 {
                        assert_eq!(delta, 0.0);
                    }
                }
            }
        }
        assert_eq!(
            field.protected_delta(p, -30.0, Some(0.0), Some(0), 0.0, true, false),
            0.0
        );
        assert_eq!(
            field.protected_delta(p, 30.0, Some(40.0), Some(17), 0.0, false, true),
            0.0
        );
        assert_eq!(
            field.protected_delta(p, f64::NAN, None, None, 0.0, false, false),
            0.0
        );
    }
}

#[test]
fn learned_thirty_meter_samples_are_interpolated_without_inventing_submeter_bands() {
    let mut p = patch(4, 0, 0, 0.0);
    for row in 0..PATCH_SIDE {
        for column in 0..PATCH_SIDE {
            p.values[row * PATCH_SIDE + column] = 0.25 * column as f32 + 0.125 * row as f32 - 48.0;
        }
    }
    let field = field(EARTH_RADIUS_M, vec![p]);
    let base_x = 128.5 * NATIVE_MPP;
    let y = 128.5 * NATIVE_MPP;
    let sample =
        |offset: f64| field.raw_delta(chart_direction(4, base_x + offset, y, EARTH_RADIUS_M));
    let center = sample(0.0);
    for offset in [0.2, 0.4, 1.0, 15.0, 30.0] {
        assert!((sample(offset) - center - offset / NATIVE_MPP * 0.25).abs() < 1e-8);
    }
}

#[test]
fn original_procedural_terrain_retains_twenty_centimeter_sampling_beneath_the_native_model() {
    let world = World::generate(WorldConfig {
        radius_m: EARTH_RADIUS_M,
        resolution: 8,
        erosion_steps: 0,
        surface_detail_min_spacing_m: 0.2,
        detail_m: 1400.0,
        ..Default::default()
    })
    .unwrap();
    let mut coarse = world.clone();
    coarse.config.surface_detail_min_spacing_m = NATIVE_MPP;
    let peak = world
        .cells
        .iter()
        .filter(|c| !c.ocean && !c.lake)
        .max_by(|a, b| a.height_m.total_cmp(&b.height_m))
        .unwrap();
    let tangent = normalize(cross(peak.position, [0.0, 1.0, 0.0]));
    let mut distinct = 0;
    for step in -16..=16 {
        let angle = step as f64 * 0.2 / EARTH_RADIUS_M;
        let p = normalize(std::array::from_fn(|i| {
            peak.position[i] * angle.cos() + tangent[i] * angle.sin()
        }));
        let fine = world.sample(p);
        let low = coarse.sample(p);
        assert_eq!(fine.height_m.to_bits(), world.sample(p).height_m.to_bits());
        assert!(fine.height_m.is_finite());
        distinct += usize::from(fine.height_m.to_bits() != low.height_m.to_bits());
    }
    assert!(
        distinct > 16,
        "Original procedural bands below30m were lost"
    );
    // Actual canonical samples must remain untouched underwater, including
    // oceans, real reservoirs and river reaches from the generated world.
    let mut checked_wet = 0;
    for cell in world.cells.iter().step_by(7) {
        let sample = world.sample(cell.position);
        if sample.ocean || sample.lake || sample.river >= 1.0 {
            assert_eq!(
                all_charts().protected_delta(
                    sample.direction,
                    f64::from(sample.height_m),
                    sample.water_level_m.map(f64::from),
                    sample.water_body_id,
                    f64::from(sample.river),
                    sample.ocean,
                    sample.lake,
                ),
                0.0
            );
            checked_wet += 1;
        }
    }
    assert!(checked_wet > 10);
    assert!((dot(tangent, peak.position)).abs() < 1e-12);
}
