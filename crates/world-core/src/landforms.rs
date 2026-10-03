//! Seeded geographic provinces. Objects live in the atlas domain and are
//! transported by the same fixed coastal map as the terrain, at every LOD.
use crate::{
    geometry::{cross, dot, neighbours, normalize},
    random, World,
};
use serde::Serialize;
use std::{collections::HashMap, sync::Arc};

#[derive(Clone, Debug, Serialize)]
pub struct FeatureDestination {
    pub id: String,
    pub label: String,
    pub direction: [f64; 3],
    pub altitude_m: f64,
    pub kind: String,
}

#[derive(Clone, Debug)]
pub struct Province {
    /// Display/world coordinates, including inverse coastal transport.
    pub direction: [f64; 3],
    pub radius_m: f64,
    pub strength: f64,
}

#[derive(Debug)]
struct Frame {
    center: [f64; 3],
    east: [f64; 3],
    north: [f64; 3],
    radius: f64,
}
impl Frame {
    fn new(center: [f64; 3], radius: f64) -> Self {
        let axis = if center[1].abs() < 0.95 {
            [0.0, 1.0, 0.0]
        } else {
            [1.0, 0.0, 0.0]
        };
        let east = normalize(cross(axis, center));
        Self {
            center,
            east,
            north: cross(center, east),
            radius,
        }
    }
    fn point(&self, x: f64, y: f64) -> [f64; 3] {
        normalize(std::array::from_fn(|i| {
            self.center[i] + (x * self.east[i] + y * self.north[i]) / self.radius
        }))
    }
    fn local(&self, p: [f64; 3]) -> [f64; 2] {
        let d = dot(p, self.center).max(0.1);
        [
            dot(p, self.east) * self.radius / d,
            dot(p, self.north) * self.radius / d,
        ]
    }
}
#[derive(Debug)]
struct Trough {
    a: [f64; 2],
    b: [f64; 2],
    width: f64,
    floor: f64,
}
#[derive(Debug)]
struct Island {
    at: [f64; 2],
    long: f64,
    short: f64,
    angle: f64,
    peak: f64,
    seed: u64,
}
#[derive(Debug)]
enum Kind {
    Fjord(Vec<Trough>),
    Archipelago(Vec<Island>),
    Shield,
}
#[derive(Debug)]
struct District {
    frame: Frame,
    radius: f64,
    strength: f64,
    kind: Kind,
    visit: [f64; 3],
}

#[derive(Debug)]
pub(crate) struct Landforms {
    districts: Vec<District>,
    buckets: HashMap<[i16; 3], Vec<usize>>,
}
const BUCKET: f64 = 0.08;
fn bucket(p: [f64; 3]) -> [i16; 3] {
    p.map(|v| (v / BUCKET).floor() as i16)
}
fn smooth(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}
fn distance(a: [f64; 3], b: [f64; 3], r: f64) -> f64 {
    dot(
        std::array::from_fn(|i| a[i] - b[i]),
        std::array::from_fn(|i| a[i] - b[i]),
    )
    .sqrt()
        * r
}

fn safe_trough(world: &World, frame: &Frame, a: [f64; 2], b: [f64; 2], width: f64) -> bool {
    let midpoint = frame.point((a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5);
    // Gnomonic projection contracts chord lengths. Clearance is 1-Lipschitz,
    // so the complete capsule stays outside physical reservoirs and controls.
    let support = (b[0] - a[0]).hypot(b[1] - a[1]) * 0.5 + width + 3_000.0;
    crate::lake_geometry::geographic_clearance(world, midpoint) > support
}

impl Landforms {
    fn compile(world: &World) -> Self {
        let r = world.config.radius_m;
        let scale = (r / 1_737_000.0).min(1.0);
        let n = world.config.resolution;
        let stride = (world.cells.len() / 40_000).max(1);
        let mut coast = Vec::new();
        let mut shields = Vec::new();
        for i in (0..world.cells.len()).step_by(stride) {
            let c = &world.cells[i];
            if c.ocean || c.lake {
                continue;
            }
            let near = neighbours(i, n);
            if c.height_m < 900.0 {
                if let Some(j) = near.iter().copied().find(|&j| world.cells[j].ocean) {
                    coast.push((
                        random(world.config.seed ^ (i as u64).wrapping_mul(193)),
                        i,
                        j,
                    ));
                }
            }
            let slope = near
                .iter()
                .map(|&j| {
                    f64::from((world.cells[j].height_m - c.height_m).abs()) / c.area_m2.sqrt()
                })
                .fold(0.0, f64::max);
            let (temperature, moisture) = crate::annual_cell_climate(world, c);
            if c.position[1].abs() > 0.45
                && temperature > -18.0
                && temperature < 16.0
                && moisture > 0.45
                && (50.0..1000.0).contains(&c.height_m)
                && c.discharge < 80.0
                && slope < 0.025
                && crate::lake_geometry::geographic_clearance(world, c.position) > 8_000.0
            {
                shields.push((
                    random(world.config.seed ^ 0x71ac ^ (i as u64).wrapping_mul(271)),
                    i,
                ));
            }
        }
        coast.sort_by(|a, b| a.0.total_cmp(&b.0));
        shields.sort_by(|a, b| a.0.total_cmp(&b.0));
        let mut districts: Vec<District> = Vec::new();
        if world.config.fjord_strength > 0.0 {
            for &(_, i, j) in &coast {
                let c = &world.cells[i];
                if c.position[1].abs() < 0.72 {
                    continue;
                }
                if districts.iter().any(|d| {
                    matches!(d.kind, Kind::Fjord(_))
                        && distance(d.frame.center, c.position, r) < 90_000.0 * scale
                }) {
                    continue;
                }
                let seed = world.config.seed ^ 0xf107d ^ (i as u64).wrapping_mul(811);
                let frame = Frame::new(c.position, r);
                let sea = frame.local(world.cells[j].position);
                let length = (sea[0] * sea[0] + sea[1] * sea[1]).sqrt().max(1.0);
                let outward = [sea[0] / length, sea[1] / length];
                let strike = [-outward[1], outward[0]];
                let mut troughs = Vec::new();
                let mut visit = frame.center;
                for root in 0..3u64 {
                    let shift = (root as f64 - 1.0) * 12_000.0 * scale;
                    let offset = std::array::from_fn::<_, 2, _>(|k| strike[k] * shift);
                    // Start at the actual frozen shore, including coarse atlases
                    // whose cell centre can lie tens of kilometres inland.
                    let limit = (c.area_m2.sqrt() * 3.0).max(45_000.0 * scale);
                    let at = |along: f64| {
                        frame.point(
                            offset[0] + outward[0] * along,
                            offset[1] + outward[1] * along,
                        )
                    };
                    let mut bracket = None;
                    let mut previous = (-limit, crate::sampler::atlas_height(world, at(-limit)));
                    for step in 1..=24 {
                        let along = -limit + 2.0 * limit * step as f64 / 24.0;
                        let h = crate::sampler::atlas_height(world, at(along));
                        if previous.1 >= 0.0 && h < 0.0 {
                            bracket = Some((previous.0, along));
                            break;
                        }
                        previous = (along, h);
                    }
                    let Some((mut land, mut sea)) = bracket else {
                        continue;
                    };
                    for _ in 0..14 {
                        let mid = (land + sea) * 0.5;
                        if crate::sampler::atlas_height(world, at(mid)) >= 0.0 {
                            land = mid
                        } else {
                            sea = mid
                        }
                    }
                    let shore = (land + sea) * 0.5;
                    let origin = std::array::from_fn::<_, 2, _>(|k| offset[k] + outward[k] * shore);
                    let inland = (14_000.0 + random(seed ^ root * 119) * 17_000.0) * scale;
                    let width = (450.0 + 700.0 * random(seed ^ root * 199 + 17))
                        * scale
                        * world.config.fjord_strength.sqrt();
                    let mut points = Vec::new();
                    for k in 0..=7 {
                        let t = k as f64 / 7.0;
                        let along = -6_000.0 * scale + t * (inland + 6_000.0 * scale);
                        let bend = (std::f64::consts::PI * t).sin()
                            * (random(seed ^ root * 373 + k as u64 * 61) - 0.5)
                            * 4_000.0
                            * scale;
                        points.push(std::array::from_fn(|a| {
                            origin[a] - outward[a] * along + strike[a] * bend
                        }));
                    }
                    if !points
                        .windows(2)
                        .all(|pair| safe_trough(world, &frame, pair[0], pair[1], width))
                    {
                        continue;
                    }
                    for pair in points.windows(2) {
                        troughs.push(Trough {
                            a: pair[0],
                            b: pair[1],
                            width,
                            floor: -(80.0 + 180.0 * random(seed ^ root * 33))
                                * world.config.fjord_strength,
                        })
                    }
                    if root == 1 {
                        visit = frame.point(points[4][0], points[4][1]);
                    }
                    for branch in 0..4u64 {
                        let start = points[2 + branch as usize];
                        let side = if branch % 2 == 0 { 1.0 } else { -1.0 };
                        let branch_length =
                            (4_000.0 + random(seed ^ branch * 197 + root * 177) * 8_000.0) * scale;
                        let end = std::array::from_fn(|a| {
                            start[a] + (side * strike[a] - outward[a] * 0.55) * branch_length
                        });
                        let branch_width =
                            width * (0.30 + 0.30 * random(seed ^ branch * 971 + root * 311));
                        if !safe_trough(world, &frame, start, end, branch_width) {
                            continue;
                        }
                        troughs.push(Trough {
                            a: start,
                            b: end,
                            width: branch_width,
                            floor: -(30.0 + 90.0 * random(seed ^ branch * 661))
                                * world.config.fjord_strength,
                        });
                    }
                }
                if troughs.is_empty() {
                    continue;
                }
                let radius = troughs
                    .iter()
                    .map(|t| {
                        let a = (t.a[0] * t.a[0] + t.a[1] * t.a[1]).sqrt();
                        let b = (t.b[0] * t.b[0] + t.b[1] * t.b[1]).sqrt();
                        a.max(b) + t.width * 1.2
                    })
                    .fold(0.0, f64::max);
                districts.push(District {
                    frame,
                    radius,
                    strength: world.config.fjord_strength,
                    kind: Kind::Fjord(troughs),
                    visit,
                });
                if districts
                    .iter()
                    .filter(|d| matches!(d.kind, Kind::Fjord(_)))
                    .count()
                    >= 28
                {
                    break;
                }
            }
        }
        if world.config.archipelago_density > 0.0 {
            for &(_, i, j) in &coast {
                let c = &world.cells[i];
                if districts.iter().any(|d| {
                    matches!(d.kind, Kind::Archipelago(_))
                        && distance(d.frame.center, c.position, r) < 160_000.0 * scale
                }) {
                    continue;
                }
                let seed = world.config.seed ^ 0xa7c11 ^ (i as u64).wrapping_mul(613);
                let shoreline = Frame::new(c.position, r);
                let sea = shoreline.local(world.cells[j].position);
                let length = (sea[0] * sea[0] + sea[1] * sea[1]).sqrt().max(1.0);
                let outward = [sea[0] / length, sea[1] / length];
                let strike = [-outward[1], outward[0]];
                let center =
                    shoreline.point(outward[0] * 22_000.0 * scale, outward[1] * 22_000.0 * scale);
                let frame = Frame::new(center, r);
                let mut islands = Vec::new();
                let mut visit = center;
                for k in 0..70u64 {
                    if random(seed ^ k * 891 + 7) > world.config.archipelago_density {
                        continue;
                    }
                    let major = k < 9;
                    let along = (if major {
                        (k as f64 - 4.0) * 16_000.0
                    } else {
                        (random(seed ^ k * 739) - 0.5) * 135_000.0
                    }) * scale;
                    let bend = (along / (80_000.0 * scale)).powi(2) * 18_000.0 * scale;
                    let across = (random(seed ^ k * 433 + 13) - 0.5) * 26_000.0 * scale + bend;
                    let at = std::array::from_fn(|a| strike[a] * along + outward[a] * across);
                    let p = frame.point(at[0], at[1]);
                    let h = crate::sampler::atlas_height(world, p);
                    if !(-2400.0..-5.0).contains(&h) {
                        continue;
                    }
                    let long = if major {
                        2_500.0 + random(seed ^ k * 271) * 5_500.0
                    } else {
                        90.0 + random(seed ^ k * 271).powi(2) * 2200.0
                    };
                    let short = long * (0.25 + 0.4 * random(seed ^ k * 73)) * scale;
                    let peak = if major {
                        180.0 + random(seed ^ k * 277) * 1000.0
                    } else {
                        15.0 + random(seed ^ k * 277) * 180.0
                    } * scale;
                    if crate::lake_geometry::geographic_clearance(world, p) < 2.2 * long * scale {
                        continue;
                    }
                    islands.push(Island {
                        at,
                        long: long * scale,
                        short,
                        angle: strike[1].atan2(strike[0]) + 0.6 * (random(seed ^ k * 907) - 0.5),
                        peak: peak.min(short * 0.5),
                        seed: seed ^ k * 193,
                    });
                    if major {
                        visit = p;
                    }
                }
                if islands.is_empty() {
                    continue;
                }
                districts.push(District {
                    frame,
                    radius: 115_000.0 * scale,
                    strength: world.config.archipelago_density,
                    kind: Kind::Archipelago(islands),
                    visit,
                });
                if districts
                    .iter()
                    .filter(|d| matches!(d.kind, Kind::Archipelago(_)))
                    .count()
                    >= 36
                {
                    break;
                }
            }
        }
        if world.config.regional_lake_density > 0.0 {
            for &(_, i) in &shields {
                let p = world.cells[i].position;
                if districts.iter().any(|d| {
                    matches!(d.kind, Kind::Shield)
                        && distance(d.frame.center, p, r) < 180_000.0 * scale
                }) {
                    continue;
                }
                let radius =
                    (80_000.0 + random(world.config.seed ^ i as u64 ^ 0x773) * 70_000.0) * scale;
                districts.push(District {
                    frame: Frame::new(p, r),
                    radius,
                    strength: 1.0,
                    kind: Kind::Shield,
                    visit: p,
                });
                if districts
                    .iter()
                    .filter(|d| matches!(d.kind, Kind::Shield))
                    .count()
                    >= 32
                {
                    break;
                }
            }
        }
        let mut buckets: HashMap<[i16; 3], Vec<usize>> = HashMap::new();
        for (i, d) in districts.iter().enumerate() {
            let support = if matches!(d.kind, Kind::Shield) {
                d.radius * 1.05 + 2_500.0
            } else {
                d.radius + 5_000.0 * scale
            };
            let extent = support * 1.2 / r;
            let low = bucket(d.frame.center.map(|v| v - extent));
            let high = bucket(d.frame.center.map(|v| v + extent));
            for x in low[0]..=high[0] {
                for y in low[1]..=high[1] {
                    for z in low[2]..=high[2] {
                        buckets.entry([x, y, z]).or_default().push(i)
                    }
                }
            }
        }
        Self { districts, buckets }
    }
}
fn catalogue(world: &World) -> &Landforms {
    world
        .landform_cache
        .get_or_init(|| Arc::new(Landforms::compile(world)))
}

/// Called in atlas coordinates after the frozen continuous reconstruction.
pub(crate) fn terrain(world: &World, q: [f64; 3], natural: f64) -> f64 {
    let plan = catalogue(world);
    let Some(indices) = plan.buckets.get(&bucket(q)) else {
        return natural;
    };
    apply_districts(world, q, natural, plan, indices)
}

fn apply_districts(
    world: &World,
    q: [f64; 3],
    natural: f64,
    plan: &Landforms,
    indices: &[usize],
) -> f64 {
    let mut height = natural;
    // Raise islands first, then carve connected glacial troughs through the
    // resulting terrain. An overlapping island must not dam a fjord, and the
    // carving support must blend back to that raised terrain continuously.
    for pass in 0..2 {
        for &i in indices {
            let d = &plan.districts[i];
            if matches!(d.kind, Kind::Fjord(_)) != (pass == 1) {
                continue;
            }
            if dot(q, d.frame.center) <= 0.0 {
                continue;
            }
            let [x, y] = d.frame.local(q);
            if x * x + y * y > d.radius * d.radius {
                continue;
            }
            match &d.kind {
                Kind::Fjord(troughs) => {
                    for t in troughs {
                        let reach = t.width;
                        if x < t.a[0].min(t.b[0]) - reach
                            || x > t.a[0].max(t.b[0]) + reach
                            || y < t.a[1].min(t.b[1]) - reach
                            || y > t.a[1].max(t.b[1]) + reach
                        {
                            continue;
                        }
                        let vx = t.b[0] - t.a[0];
                        let vy = t.b[1] - t.a[1];
                        let along = (((x - t.a[0]) * vx + (y - t.a[1]) * vy)
                            / (vx * vx + vy * vy).max(1.0))
                        .clamp(0.0, 1.0);
                        let radial = ((x - t.a[0] - along * vx).powi(2)
                            + (y - t.a[1] - along * vy).powi(2))
                        .sqrt()
                            / reach;
                        if radial < 1.0 {
                            height = height.min(
                                t.floor
                                    + (height - t.floor).max(0.0) * smooth((radial - 0.30) / 0.70),
                            );
                        }
                    }
                }
                Kind::Archipelago(islands) => {
                    for island in islands {
                        let dx = x - island.at[0];
                        let dy = y - island.at[1];
                        if dx.abs() > island.long * 2.2 || dy.abs() > island.long * 2.2 {
                            continue;
                        }
                        let (s, c) = island.angle.sin_cos();
                        let u = (dx * c + dy * s) / island.long;
                        let v = (-dx * s + dy * c) / island.short;
                        let angle = v.atan2(u);
                        let outline = 1.0
                            + 0.17 * (3.0 * angle + random(island.seed) * 6.28).sin()
                            + 0.12 * (7.0 * angle + random(island.seed ^ 0x937) * 6.28).sin();
                        let radial = (u * u + v * v).sqrt() / outline;
                        if radial < 1.6 {
                            let support = 1.0 - smooth((radial - 1.0) / 0.60);
                            let texture = 1.0
                                + 0.30
                                    * crate::noise(
                                        island.seed,
                                        q,
                                        world.config.radius_m / island.short * 4.0,
                                    );
                            let target = island.peak * (1.0 - radial * radial) * texture;
                            height = height.max(natural + (target - natural) * support);
                        }
                    }
                }
                Kind::Shield => {}
            }
        }
    }
    height
}

/// Province gating in atlas coordinates, with a halo covering microbasin support.
pub(crate) fn lake_province_weight(world: &World, q: [f64; 3]) -> f64 {
    let plan = catalogue(world);
    let Some(indices) = plan.buckets.get(&bucket(q)) else {
        return 0.0;
    };
    indices
        .iter()
        .filter_map(|&i| {
            let d = &plan.districts[i];
            matches!(d.kind, Kind::Shield).then(|| {
                let radial = distance(q, d.frame.center, world.config.radius_m) / d.radius;
                d.strength * (1.0 - smooth((radial - 0.65) / 0.40))
            })
        })
        .fold(0.0, f64::max)
}

pub fn lake_provinces(world: &World) -> Vec<Province> {
    catalogue(world)
        .districts
        .iter()
        .filter(|d| matches!(d.kind, Kind::Shield))
        .map(|d| Province {
            direction: crate::coast::inverse(&world.config, d.frame.center),
            radius_m: d.radius,
            strength: d.strength,
        })
        .collect()
}

pub(crate) fn near_lake_province(world: &World, q: [f64; 3]) -> bool {
    let plan = catalogue(world);
    let Some(indices) = plan.buckets.get(&bucket(q)) else {
        return false;
    };
    indices.iter().any(|&i| {
        let d = &plan.districts[i];
        matches!(d.kind, Kind::Shield)
            && distance(q, d.frame.center, world.config.radius_m) < d.radius * 1.05 + 2_500.0
    })
}

pub(crate) fn detail_multiplier(world: &World, q: [f64; 3]) -> f64 {
    // Scoured shield districts retain their broad relief, with subdued fine
    // bedrock undulations that can actually hold shallow pools and wetlands.
    1.0 - 0.96 * lake_province_weight(world, q)
}

pub fn destinations(world: &World) -> Vec<FeatureDestination> {
    let mut result = Vec::new();
    let mut attempts = [0_u8; 3];
    // Sampling happens only after the province catalogue is initialized. It can
    // reuse that catalogue while compiling the shared river geometry once.
    let observe = |q| {
        let p = crate::coast::inverse(&world.config, q);
        (p[1].abs() <= 1.54_f64.sin()).then(|| (p, world.sample(p)))
    };
    let scale = (world.config.radius_m / 1_737_000.0).min(1.0);
    for (i, d) in catalogue(world).districts.iter().enumerate() {
        let (slot, kind, label) = match d.kind {
            Kind::Fjord(_) => (0, "fjord", "Fjords polaires"),
            Kind::Archipelago(_) => (1, "archipelago", "Archipel et îlots"),
            Kind::Shield => (2, "lake_district", "Pays des lacs et marais"),
        };
        if result.iter().any(|f: &FeatureDestination| f.kind == kind) || attempts[slot] >= 8 {
            continue;
        }
        attempts[slot] += 1;
        let selected = match &d.kind {
            Kind::Fjord(troughs) => {
                let marine = |q| {
                    observe(q)
                        .filter(|(_, s)| s.ocean && s.water_m > s.height_m + 0.2)
                        .map(|(p, _)| p)
                };
                let point = marine(d.visit).or_else(|| {
                    // Each generated root has seven main segments and four
                    // branches. Prefer its inland segments over the open sea.
                    troughs
                        .iter()
                        .enumerate()
                        .find_map(|(j, t)| {
                            (j % 11 >= 2).then(|| {
                                marine(
                                    d.frame
                                        .point((t.a[0] + t.b[0]) * 0.5, (t.a[1] + t.b[1]) * 0.5),
                                )
                            })?
                        })
                        .or_else(|| {
                            troughs.iter().find_map(|t| {
                                marine(
                                    d.frame
                                        .point((t.a[0] + t.b[0]) * 0.5, (t.a[1] + t.b[1]) * 0.5),
                                )
                            })
                        })
                });
                point.map(|p| (p, 22_000.0 * scale))
            }
            Kind::Archipelago(islands) => {
                // Verify emerged islands, then frame a central group rather
                // than the last island created at the far end of the chain.
                let mut emerged: Vec<_> = islands
                    .iter()
                    .filter(|island| island.long >= 1_500.0 * scale)
                    .take(24)
                    .filter(|island| {
                        observe(d.frame.point(island.at[0], island.at[1]))
                            .is_some_and(|(_, s)| !s.ocean && !s.lake && s.height_m > 0.0)
                    })
                    .collect();
                if emerged.len() < 2 {
                    None
                } else {
                    let center = std::array::from_fn::<_, 2, _>(|a| {
                        emerged.iter().map(|island| island.at[a]).sum::<f64>()
                            / emerged.len() as f64
                    });
                    let squared_distance = |island: &Island| {
                        (island.at[0] - center[0]).powi(2) + (island.at[1] - center[1]).powi(2)
                    };
                    emerged.sort_by(|a, b| squared_distance(a).total_cmp(&squared_distance(b)));
                    let group = &emerged[..emerged.len().min(3)];
                    let center = std::array::from_fn::<_, 2, _>(|a| {
                        group.iter().map(|island| island.at[a]).sum::<f64>() / group.len() as f64
                    });
                    let extent = group
                        .iter()
                        .map(|island| {
                            (island.at[0] - center[0]).hypot(island.at[1] - center[1]) + island.long
                        })
                        .fold(0.0_f64, f64::max);
                    observe(d.frame.point(center[0], center[1]))
                        .filter(|(_, s)| !s.lake)
                        .map(|(p, _)| (p, (extent * 2.4).clamp(25_000.0 * scale, 90_000.0 * scale)))
                }
            }
            Kind::Shield => {
                let basin = |q| {
                    observe(q)
                        .filter(|(_, s)| {
                            s.lake
                                && !s.ocean
                                && s.water_m > s.height_m + 0.2
                                && s.water_body_id
                                    .is_some_and(|id| ((1_u64 << 40)..(1_u64 << 41)).contains(&id))
                        })
                        .map(|(p, _)| p)
                };
                let point = basin(d.visit).or_else(|| {
                    // At most 288 local probes per province; no global scan.
                    // Half the proposal spacing resolves common medium basins.
                    let step = 525.0_f64.min(d.radius * 0.06).max(50.0);
                    for ring in 1_i32..=8 {
                        for y in -ring..=ring {
                            for x in -ring..=ring {
                                if x.abs() != ring && y.abs() != ring {
                                    continue;
                                }
                                let local = [f64::from(x) * step, f64::from(y) * step];
                                if local[0].hypot(local[1]) > d.radius * 0.55 {
                                    continue;
                                }
                                if let Some(p) = basin(d.frame.point(local[0], local[1])) {
                                    return Some(p);
                                }
                            }
                        }
                    }
                    None
                });
                point.map(|p| (p, 8_000.0 * scale))
            }
        };
        let Some((direction, altitude)) = selected else {
            continue;
        };
        result.push(FeatureDestination {
            id: format!("{kind}-{i}"),
            label: label.into(),
            kind: kind.into(),
            direction,
            altitude_m: altitude.min(world.config.radius_m * 0.1),
        });
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn destination_fixture(kind: Kind, density: f64) -> World {
        let marine = matches!(kind, Kind::Archipelago(_));
        let mut world = World::generate(crate::WorldConfig {
            resolution: 8,
            erosion_steps: 0,
            detail_m: 0.0,
            coast_detail_ratio: 0.0,
            fjord_strength: 0.0,
            archipelago_density: 0.0,
            regional_lake_density: density,
            ..Default::default()
        })
        .unwrap();
        world.lakes.clear();
        for cell in &mut world.cells {
            cell.height_m = if marine { -100.0 } else { 300.0 };
            cell.water_m = if marine { 0.0 } else { 300.0 };
            cell.ocean = marine;
            cell.lake = false;
            cell.lake_id = None;
            cell.discharge = 0.0;
            cell.receiver = None;
            cell.temperature_c = 8.0;
            cell.moisture = 0.85;
        }
        world.invalidate_geometry();
        let center = [0.0, 0.0, 1.0];
        let key = bucket(center);
        let mut buckets = HashMap::new();
        for x in -1..=1 {
            for y in -1..=1 {
                for z in -1..=1 {
                    buckets.insert([key[0] + x, key[1] + y, key[2] + z], vec![0]);
                }
            }
        }
        world
            .landform_cache
            .set(Arc::new(Landforms {
                districts: vec![District {
                    frame: Frame::new(center, world.config.radius_m),
                    radius: 80_000.0,
                    strength: 1.0,
                    kind,
                    visit: center,
                }],
                buckets,
            }))
            .unwrap();
        world
    }

    #[test]
    fn fjord_destination_falls_back_from_a_dry_visit_to_the_actual_inlet() {
        let world = destination_fixture(
            Kind::Fjord(vec![Trough {
                a: [8_000.0, 0.0],
                b: [16_000.0, 0.0],
                width: 800.0,
                floor: -80.0,
            }]),
            0.0,
        );
        assert!(!world.sample([0.0, 0.0, 1.0]).ocean);
        let features = destinations(&world);
        assert_eq!(features.len(), 1);
        assert_eq!(features[0].kind, "fjord");
        assert!(world.sample(features[0].direction).ocean);
        assert_ne!(features[0].direction, [0.0, 0.0, 1.0]);
    }

    #[test]
    fn lake_destination_requires_an_actual_microbasin() {
        let dry = destination_fixture(Kind::Shield, 0.0);
        assert!(destinations(&dry).is_empty());
        let wet = destination_fixture(Kind::Shield, 1.0);
        let features = destinations(&wet);
        assert_eq!(features.len(), 1);
        assert_eq!(features[0].kind, "lake_district");
        let sample = wet.sample(features[0].direction);
        assert!(sample.lake && !sample.ocean && sample.water_m > sample.height_m + 0.2);
        assert!(((1_u64 << 40)..(1_u64 << 41)).contains(&sample.water_body_id.unwrap()));
        assert_eq!(features[0].altitude_m, 8_000.0);
    }

    #[test]
    fn archipelago_destination_frames_several_emerged_islands() {
        let islands = [-12_000.0, 0.0, 12_000.0]
            .into_iter()
            .map(|x| Island {
                at: [x, 0.0],
                long: 2_500.0,
                short: 1_200.0,
                angle: 0.0,
                peak: 200.0,
                seed: x.to_bits(),
            })
            .collect();
        let world = destination_fixture(Kind::Archipelago(islands), 0.0);
        let features = destinations(&world);
        assert_eq!(features.len(), 1);
        assert_eq!(features[0].kind, "archipelago");
        assert_eq!(features[0].direction, [0.0, 0.0, 1.0]);
        assert!((features[0].altitude_m - 34_800.0).abs() < 1e-6);
        let frame = Frame::new(features[0].direction, world.config.radius_m);
        for x in [-12_000.0, 0.0, 12_000.0] {
            assert!(!world.sample(frame.point(x, 0.0)).ocean);
        }
        assert!(world.sample(frame.point(6_000.0, 0.0)).ocean);
    }

    #[test]
    fn disabled_provinces_preserve_the_exact_base_field() {
        let world = World::generate(crate::WorldConfig {
            resolution: 32,
            erosion_steps: 0,
            fjord_strength: 0.0,
            archipelago_density: 0.0,
            regional_lake_density: 0.0,
            ..Default::default()
        })
        .unwrap();
        assert!(destinations(&world).is_empty());
        assert_eq!(terrain(&world, [1.0, 0.0, 0.0], 123.45), 123.45);
    }

    #[test]
    fn overlapping_islands_cannot_dam_fjords_or_break_their_outer_banks() {
        let world = World::generate(crate::WorldConfig {
            resolution: 8,
            erosion_steps: 0,
            ..Default::default()
        })
        .unwrap();
        let r = world.config.radius_m;
        let center = [1.0, 0.0, 0.0];
        let frame = Frame::new(center, r);
        let points = [-4000.0, -2000.0, 0.0, 2000.0, 4000.0];
        let mut buckets = HashMap::new();
        for x in points {
            for y in [-350.001, -349.999, 0.0, 349.999, 350.001] {
                buckets.insert(bucket(frame.point(x, y)), vec![0, 1]);
            }
        }
        let districts = vec![
            District {
                frame: Frame::new(center, r),
                radius: 5000.0,
                strength: 1.0,
                kind: Kind::Fjord(vec![Trough {
                    a: [-4000.0, 0.0],
                    b: [4000.0, 0.0],
                    width: 350.0,
                    floor: -70.0,
                }]),
                visit: center,
            },
            District {
                frame: Frame::new(center, r),
                radius: 8000.0,
                strength: 1.0,
                kind: Kind::Archipelago(vec![Island {
                    at: [0.0, 0.0],
                    long: 3000.0,
                    short: 1200.0,
                    angle: 0.0,
                    peak: 500.0,
                    seed: 193,
                }]),
                visit: center,
            },
        ];
        world
            .landform_cache
            .set(Arc::new(Landforms { districts, buckets }))
            .unwrap();
        for x in points {
            assert!(terrain(&world, frame.point(x, 0.0), -10.0) < 0.0);
        }
        assert!(terrain(&world, frame.point(0.0, 350.001), -10.0) > 200.0);
        let inside = terrain(&world, frame.point(0.0, 349.999), -10.0);
        let outside = terrain(&world, frame.point(0.0, 350.001), -10.0);
        assert!((inside - outside).abs() < 0.01);
    }

    #[test]
    fn seasonal_presentation_does_not_select_different_geographic_provinces() {
        let world = World::generate(crate::WorldConfig {
            resolution: 64,
            seed: 2386084681,
            erosion_steps: 0,
            ..Default::default()
        })
        .unwrap();
        // Build the winter cache first, then the annual cache independently.
        let winter = world.seasonal_world(0.83).unwrap();
        let cold = lake_provinces(&winter);
        let annual = lake_provinces(&world);
        assert!(!annual.is_empty());
        assert_eq!(annual.len(), cold.len());
        for (a, b) in annual.iter().zip(cold) {
            assert_eq!(a.direction, b.direction);
            assert_eq!(a.radius_m.to_bits(), b.radius_m.to_bits());
        }
        for d in annual.iter().take(2) {
            let frame = Frame::new(d.direction, world.config.radius_m);
            for y in -2..=2 {
                for x in -2..=2 {
                    let p = frame.point(f64::from(x) * 1000.0, f64::from(y) * 1000.0);
                    let cold = winter.sample(p);
                    let normal = world.sample(p);
                    assert_eq!(cold.height_m.to_bits(), normal.height_m.to_bits());
                    assert_eq!(cold.water_m.to_bits(), normal.water_m.to_bits());
                    assert_eq!(cold.water_body_id, normal.water_body_id);
                }
            }
        }
    }
    #[test]
    fn geographic_features_are_addressed_independently_of_visit_order() {
        let world = World::generate(crate::WorldConfig {
            resolution: 64,
            seed: 2386084681,
            erosion_steps: 0,
            ..Default::default()
        })
        .unwrap();
        let plan = catalogue(&world);
        assert!(plan
            .districts
            .iter()
            .any(|d| matches!(d.kind, Kind::Fjord(_))));
        assert!(plan
            .districts
            .iter()
            .any(|d| matches!(d.kind, Kind::Archipelago(_))));
        for d in &plan.districts {
            let a = terrain(&world, d.visit, 100.0);
            let _ = terrain(&world, [-1.0, 0.0, 0.0], -10.0);
            assert_eq!(a.to_bits(), terrain(&world, d.visit, 100.0).to_bits());
        }
    }

    #[test]
    fn spatial_buckets_do_not_cut_features_at_their_boundaries() {
        let world = World::generate(crate::WorldConfig {
            resolution: 64,
            seed: 2386084681,
            erosion_steps: 0,
            ..Default::default()
        })
        .unwrap();
        let plan = catalogue(&world);
        let all: Vec<usize> = (0..plan.districts.len()).collect();
        for d in &plan.districts {
            for y in -2..=2 {
                for x in -2..=2 {
                    let q = d
                        .frame
                        .point(f64::from(x) * d.radius * 0.6, f64::from(y) * d.radius * 0.6);
                    let base = crate::sampler::atlas_height(&world, q);
                    assert_eq!(
                        terrain(&world, q, base).to_bits(),
                        apply_districts(&world, q, base, plan, &all).to_bits(),
                        "district index omitted a feature at {q:?}"
                    );
                }
            }
        }
    }
}
