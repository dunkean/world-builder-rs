//! Canonical unresolved glacial basins and their wet margins.
//!
//! This is a landform reconstruction, not an additional global water budget.
//! Proposals have fixed spherical addresses, curved multi-lobed beds, and one
//! horizontal hydraulic plane each. The cache changes cost, never geography.

use crate::{
    geometry::{cross, dot, normalize},
    hash64, random, World,
};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex, OnceLock},
};

const PROPOSAL_SPACING_M: f64 = 1_050.0;
const CACHE_SPACING_M: f64 = 4_096.0;
const MAX_SUPPORT_M: f64 = 2_300.0;
const SUPPORT_PHI: f64 = 0.65;
const CACHE_BUCKET_LIMIT: usize = 1_024;
const PROPOSAL_CACHE_LIMIT: usize = 65_536;
const MICRO_LAKE_ID_BASE: u64 = 1 << 40;
const MICRO_LAKE_ID_MASK: u64 = (1 << 40) - 1;

/// Fields must describe the frozen natural terrain, excluding these basins and
/// river carving. `roughness` is an atlas slope proxy in metres/metre.
#[derive(Clone, Copy, Debug, Default)]
pub struct LocalWaterEnvironment {
    pub natural_height_m: f64,
    pub province_weight: f64,
    pub moisture: f64,
    pub temperature_c: f64,
    pub roughness: f64,
    /// Existing marine, reservoir, or channel ownership; one excludes a basin.
    pub protected_water_weight: f64,
}

#[derive(Clone, Copy, Debug)]
pub struct LocalWaterSample {
    pub height_m: f64,
    pub level_m: f64,
    pub id: u64,
    pub wetland_weight: f64,
    pub lake: bool,
}

#[derive(Default)]
struct Cache {
    buckets: HashMap<[i64; 3], Arc<OnceLock<Arc<[Arc<Basin>]>>>>,
    insertion_order: VecDeque<[i64; 3]>,
    proposals: HashMap<[i64; 3], Arc<OnceLock<Option<Arc<Basin>>>>>,
    proposal_order: VecDeque<[i64; 3]>,
}

/// A bounded, reconstructible cache. Direct Atlas clones start empty. World
/// stores it in an Arc, so World clones share it until invalidate_geometry().
#[derive(Default)]
pub struct LocalWaterAtlas {
    cache: Mutex<Cache>,
}

impl Clone for LocalWaterAtlas {
    fn clone(&self) -> Self {
        Self::default()
    }
}

impl std::fmt::Debug for LocalWaterAtlas {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalWaterAtlas").finish_non_exhaustive()
    }
}

#[derive(Clone, Copy, Debug)]
struct Lobe {
    x: f64,
    y: f64,
    a: f64,
    b: f64,
    cosine: f64,
    sine: f64,
}

#[derive(Clone, Debug)]
struct Basin {
    id: u64,
    seed: u64,
    center: [f64; 3],
    east: [f64; 3],
    north: [f64; 3],
    lobes: [Lobe; 6],
    lobe_count: usize,
    length_m: f64,
    minor_m: f64,
    support_m: f64,
    level_m: f64,
    depth_m: f64,
    wetland_strength: f64,
}

fn smooth(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn signed_key(seed: u64, key: [i64; 3]) -> u64 {
    hash64(
        seed ^ (key[0] as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15)
            ^ (key[1] as u64).wrapping_mul(0xbf58_476d_1ce4_e5b9)
            ^ (key[2] as u64).wrapping_mul(0x94d0_49bb_1331_11eb),
    )
}

fn planar_noise(seed: u64, x: f64, y: f64) -> f64 {
    let ix = x.floor() as i64;
    let iy = y.floor() as i64;
    let tx = smooth(x - ix as f64);
    let ty = smooth(y - iy as f64);
    let mut value = 0.0;
    for dy in 0..2 {
        for dx in 0..2 {
            let weight = if dx == 0 { 1.0 - tx } else { tx } * if dy == 0 { 1.0 - ty } else { ty };
            value += weight * (2.0 * random(signed_key(seed, [ix + dx, iy + dy, 0])) - 1.0);
        }
    }
    value
}

fn smooth_min(a: f64, b: f64) -> f64 {
    const WIDTH: f64 = 0.20;
    let h = (WIDTH - (a - b).abs()).max(0.0) / WIDTH;
    a.min(b) - 0.25 * h * h * WIDTH
}

/// Optional cold-path diagnostic. No per-point logging or changed decisions.
fn profile_freeze(id: u64, outcome: &str, factors: [f64; 5]) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        static ENABLED: OnceLock<bool> = OnceLock::new();
        if *ENABLED.get_or_init(|| std::env::var_os("WB_PROFILE_LOCAL_WATER").is_some()) {
            eprintln!(
                "local_water_freeze {id} {outcome} {:.6} {:.6} {:.6} {:.6} {:.6}",
                factors[0], factors[1], factors[2], factors[3], factors[4]
            );
        }
    }
    #[cfg(target_arch = "wasm32")]
    let _ = (id, outcome, factors);
}

impl Basin {
    fn proposal(planet_seed: u64, key: [i64; 3], radius_m: f64) -> Option<Self> {
        let seed = signed_key(planet_seed ^ 0x81a7_2b43_3ed0_9675, key);
        let raw: [f64; 3] = std::array::from_fn(|i| {
            (key[i] as f64 + random(seed.wrapping_add(i as u64 * 173 + 11))) * PROPOSAL_SPACING_M
        });
        // Only a thin spherical shell proposes objects. Projection of a raw
        // point into the sphere is included in every cache bucket's full halo.
        if (dot(raw, raw).sqrt() - radius_m).abs() > PROPOSAL_SPACING_M * 0.55 {
            return None;
        }
        let center = normalize(raw);
        let axis = if center[1].abs() < 0.9 {
            [0.0, 1.0, 0.0]
        } else {
            [1.0, 0.0, 0.0]
        };
        let u = normalize(cross(axis, center));
        let v = cross(center, u);
        // A broad directional field makes neighbouring scoured troughs tend
        // to agree, with substantial object-specific variation.
        let heading = 2.8 * crate::noise(planet_seed ^ 0x7738, center, radius_m / 35_000.0)
            + 1.1 * (random(seed ^ 0xc731) - 0.5);
        let (sine, cosine) = heading.sin_cos();
        let east = std::array::from_fn(|i| u[i] * cosine + v[i] * sine);
        let north = std::array::from_fn(|i| v[i] * cosine - u[i] * sine);
        let size = random(seed ^ 0x694a);
        let diameter = if size < 0.20 {
            50.0 * 3.6_f64.powf(size / 0.20)
        } else if size < 0.65 {
            180.0 * 3.9_f64.powf((size - 0.20) / 0.45)
        } else if size < 0.95 {
            700.0 * 2.4_f64.powf((size - 0.65) / 0.30)
        } else {
            1_680.0 * (3_000.0 / 1_680.0_f64).powf((size - 0.95) / 0.05)
        };
        let length = diameter * 0.5;
        let minor = length / (1.6 + 2.0 * random(seed ^ 0xa951));
        let blank = Lobe {
            x: 0.0,
            y: 0.0,
            a: 1.0,
            b: 1.0,
            cosine: 1.0,
            sine: 0.0,
        };
        let mut lobes = [blank; 6];
        let count = 4 + usize::from(random(seed ^ 0x15a3) > 0.55);
        let bend = (random(seed ^ 0xef77) - 0.5) * minor * 1.7;
        for (i, lobe) in lobes[..count].iter_mut().enumerate() {
            let s = -0.67 + 1.34 * i as f64 / (count - 1) as f64;
            let phase = random(seed.wrapping_add(591 + i as u64 * 193));
            let angle = 0.35 * (2.0 * phase - 1.0) + bend / length * s;
            let (sine, cosine) = angle.sin_cos();
            *lobe = Lobe {
                x: length * s,
                y: bend * (1.0 - s * s) + minor * 0.18 * (2.0 * phase - 1.0),
                a: length * (0.37 + 0.09 * phase),
                b: minor * (0.60 + 0.55 * random(seed.wrapping_add(877 + i as u64 * 311))),
                sine,
                cosine,
            };
        }
        // A shorter side arm gives bays and branching outlines, rather than
        // concentric circular depressions or repeated ellipse silhouettes.
        if random(seed ^ 0x6882) > 0.40 {
            let side = if random(seed ^ 0x95a1) > 0.5 {
                1.0
            } else {
                -1.0
            };
            let angle = side * (0.5 + random(seed ^ 0x29ab));
            let (sine, cosine) = angle.sin_cos();
            lobes[count] = Lobe {
                x: length * (random(seed ^ 0x79f1) - 0.5) * 0.65,
                y: bend + side * minor * 0.65,
                a: length * 0.33,
                b: minor * 0.52,
                sine,
                cosine,
            };
        }
        let lobe_count = count + usize::from(random(seed ^ 0x6882) > 0.40);
        let extent = lobes[..lobe_count]
            .iter()
            .map(|l| l.x.hypot(l.y) + l.a.max(l.b))
            .fold(0.0_f64, f64::max);
        let scale = length / extent;
        for lobe in &mut lobes[..lobe_count] {
            lobe.x *= scale;
            lobe.y *= scale;
            lobe.a *= scale;
            lobe.b *= scale;
        }
        let length_m = length;
        let minor_m = minor * scale;
        // Bound the warped union, including every smooth-min subtraction and
        // the worst boundary-noise value. The acceleration sphere cannot clip
        // a contour, even for a six-lobed shape at its maximum size.
        let expansion = (1.0 + SUPPORT_PHI + 0.05 * (lobe_count - 1) as f64 + 0.10).sqrt();
        let support_m = (lobes[..lobe_count]
            .iter()
            .map(|l| l.x.hypot(l.y) + expansion * l.a.max(l.b))
            .fold(0.0_f64, f64::max)
            + (length_m * 0.045).hypot(minor_m * 0.13)
            + 1.0)
            * 1.03;
        if support_m >= MAX_SUPPORT_M {
            return None;
        }
        Some(Self {
            id: MICRO_LAKE_ID_BASE | (seed & MICRO_LAKE_ID_MASK),
            seed,
            center,
            east,
            north,
            lobes,
            lobe_count,
            length_m,
            minor_m,
            support_m,
            level_m: 0.0,
            depth_m: (0.6 + diameter * 0.0022 + random(seed ^ 0x566e) * 1.1).clamp(0.65, 8.0),
            wetland_strength: 0.0,
        })
    }

    fn direction_at(&self, radius_m: f64, x: f64, y: f64) -> [f64; 3] {
        normalize(std::array::from_fn(|i| {
            self.center[i] + (self.east[i] * x + self.north[i] * y) / radius_m
        }))
    }

    fn shape_xy(&self, x: f64, y: f64) -> f64 {
        let x = x + self.length_m
            * 0.045
            * planar_noise(
                self.seed ^ 0xbe71,
                x / (self.length_m * 0.40),
                y / (self.length_m * 0.40),
            );
        let y = y + self.minor_m
            * 0.13
            * planar_noise(
                self.seed ^ 0xa6e9,
                x / (self.length_m * 0.27),
                y / (self.minor_m * 0.50),
            );
        let mut phi = f64::INFINITY;
        for lobe in &self.lobes[..self.lobe_count] {
            let dx = x - lobe.x;
            let dy = y - lobe.y;
            let u = (dx * lobe.cosine + dy * lobe.sine) / lobe.a;
            let v = (dy * lobe.cosine - dx * lobe.sine) / lobe.b;
            phi = smooth_min(phi, u * u + v * v - 1.0);
        }
        phi + 0.10
            * planar_noise(
                self.seed ^ 0xe16c,
                x / (self.length_m * 0.23),
                y / (self.minor_m * 0.35),
            )
    }

    fn shape_at(&self, p: [f64; 3], radius_m: f64) -> Option<(f64, f64, f64)> {
        let delta: [f64; 3] = std::array::from_fn(|i| (p[i] - self.center[i]) * radius_m);
        if dot(delta, delta) > self.support_m * self.support_m {
            return None;
        }
        let x = dot(delta, self.east);
        let y = dot(delta, self.north);
        let phi = self.shape_xy(x, y);
        (phi < SUPPORT_PHI).then_some((phi, x, y))
    }

    fn freeze<F>(&mut self, radius_m: f64, density: f64, environment: &F) -> bool
    where
        F: Fn([f64; 3]) -> LocalWaterEnvironment,
    {
        let draw = random(self.seed ^ 0xdc15);
        // All remaining habitat factors lie in [0,1], so this is exactly the
        // same decision as the full probability test, without a terrain call.
        if draw >= density.clamp(0.0, 1.0) {
            profile_freeze(self.id, "cheap_density", [0.0; 5]);
            return false;
        }
        let anchor = self.lobes[(self.lobe_count - 1) / 2];
        let center = environment(self.direction_at(radius_m, anchor.x, anchor.y));
        let moisture = smooth((center.moisture - 0.30) / 0.30);
        let cold = smooth((18.0 - center.temperature_c) / 16.0)
            * smooth((center.temperature_c + 24.0) / 14.0);
        let gentle = smooth((0.060 - center.roughness) / 0.045);
        let altitude = smooth((center.natural_height_m - 35.0) / 45.0)
            * smooth((1_650.0 - center.natural_height_m) / 950.0);
        let suitability = density.clamp(0.0, 1.0)
            * center.province_weight.clamp(0.0, 1.0)
            * moisture
            * cold
            * gentle
            * altitude;
        let factors = [center.province_weight, moisture, cold, gentle, altitude];
        if center.protected_water_weight > 0.05 {
            profile_freeze(self.id, "protected_center", factors);
            return false;
        }
        if suitability <= 0.0 || draw >= suitability {
            profile_freeze(
                self.id,
                if center.province_weight <= 0.0 {
                    "province_zero"
                } else {
                    "habitat_draw"
                },
                factors,
            );
            return false;
        }
        // Check actual rim terrain instead of laying a remote water plane over
        // a sloping hillside. The callbacks never run under the cache mutex.
        let mut rim_min = f64::INFINITY;
        let mut rim_max = f64::NEG_INFINITY;
        let mut outer_min = f64::INFINITY;
        let mut outer_max = f64::NEG_INFINITY;
        // Every lobe, including a side arm, supplies its own probe origins.
        // Cover the actual shore and the outer blending fringe, not only the
        // first contour visible from the lake's central bay.
        for lobe in &self.lobes[..self.lobe_count] {
            let e = environment(self.direction_at(radius_m, lobe.x, lobe.y));
            if e.protected_water_weight > 0.05 || !e.natural_height_m.is_finite() {
                profile_freeze(self.id, "protected_probe", factors);
                return false;
            }
            for i in 0..4 {
                let theta = lobe.sine.atan2(lobe.cosine) + std::f64::consts::FRAC_PI_2 * i as f64;
                let (sine, cosine) = theta.sin_cos();
                for contour in [0.0, SUPPORT_PHI] {
                    let mut lo = 0.0;
                    let mut hi = self.support_m * 2.0;
                    for _ in 0..15 {
                        let mid = 0.5 * (lo + hi);
                        if self.shape_xy(lobe.x + mid * cosine, lobe.y + mid * sine) < contour {
                            lo = mid;
                        } else {
                            hi = mid;
                        }
                    }
                    let e = environment(self.direction_at(
                        radius_m,
                        lobe.x + hi * cosine,
                        lobe.y + hi * sine,
                    ));
                    if e.protected_water_weight > 0.05 || !e.natural_height_m.is_finite() {
                        profile_freeze(self.id, "protected_probe", factors);
                        return false;
                    }
                    if contour == 0.0 {
                        rim_min = rim_min.min(e.natural_height_m);
                        rim_max = rim_max.max(e.natural_height_m);
                    } else {
                        outer_min = outer_min.min(e.natural_height_m);
                        outer_max = outer_max.max(e.natural_height_m);
                    }
                }
            }
        }
        if !center.natural_height_m.is_finite()
            || center.natural_height_m - rim_min > self.depth_m + 3.0
        {
            profile_freeze(self.id, "rim_depth", factors);
            return false;
        }
        let rim_tolerance = 0.5 + self.length_m * 0.02;
        if rim_max.max(outer_max) - rim_min.min(outer_min) > rim_tolerance {
            profile_freeze(self.id, "rim_range", factors);
            return false;
        }
        self.level_m = (center.natural_height_m - self.depth_m * 0.32)
            .min(rim_min - 0.35)
            .min(outer_min - 0.15);
        self.wetland_strength = (0.55 + 0.45 * moisture) * center.province_weight.sqrt();
        profile_freeze(self.id, "accepted", factors);
        true
    }
}

impl LocalWaterAtlas {
    fn frozen_proposal<F>(
        &self,
        world: &World,
        key: [i64; 3],
        environment: &F,
    ) -> Option<Arc<Basin>>
    where
        F: Fn([f64; 3]) -> LocalWaterEnvironment,
    {
        // Do not retain the many cubic addresses outside the proposal shell.
        // Descriptor construction is cheap; centre/rim validation is cached.
        let mut proposal = Basin::proposal(world.config.seed, key, world.config.radius_m)?;
        let slot = {
            let mut cache = self
                .cache
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if let Some(slot) = cache.proposals.get(&key) {
                Arc::clone(slot)
            } else {
                while cache.proposals.len() >= PROPOSAL_CACHE_LIMIT {
                    if let Some(old) = cache.proposal_order.pop_front() {
                        cache.proposals.remove(&old);
                    }
                }
                let slot = Arc::new(OnceLock::new());
                cache.proposal_order.push_back(key);
                cache.proposals.insert(key, Arc::clone(&slot));
                slot
            }
        };
        // Cache rejection as well as accepted geometry. Overlapping buckets
        // and concurrent requests share validation without holding the mutex.
        slot.get_or_init(|| {
            proposal
                .freeze(
                    world.config.radius_m,
                    world.config.regional_lake_density,
                    environment,
                )
                .then(|| Arc::new(proposal))
        })
        .clone()
    }

    fn neighbourhood<F>(&self, world: &World, p: [f64; 3], environment: &F) -> Arc<[Arc<Basin>]>
    where
        F: Fn([f64; 3]) -> LocalWaterEnvironment,
    {
        let radius = world.config.radius_m;
        let key = std::array::from_fn(|i| (p[i] * radius / CACHE_SPACING_M).floor() as i64);
        let slot = {
            let mut cache = self
                .cache
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if let Some(slot) = cache.buckets.get(&key) {
                Arc::clone(slot)
            } else {
                while cache.buckets.len() >= CACHE_BUCKET_LIMIT {
                    if let Some(old) = cache.insertion_order.pop_front() {
                        cache.buckets.remove(&old);
                    }
                }
                let slot = Arc::new(OnceLock::new());
                cache.insertion_order.push_back(key);
                cache.buckets.insert(key, Arc::clone(&slot));
                slot
            }
        };
        // Concurrent requests share the same cold computation. The callback
        // runs outside the mutex and must exclude local water and river caches.
        Arc::clone(slot.get_or_init(|| {
            let halo = MAX_SUPPORT_M + PROPOSAL_SPACING_M * 0.55 + 1.0;
            let lo: [i64; 3] = std::array::from_fn(|i| {
                ((key[i] as f64 * CACHE_SPACING_M - halo) / PROPOSAL_SPACING_M).floor() as i64
            });
            let hi: [i64; 3] = std::array::from_fn(|i| {
                (((key[i] + 1) as f64 * CACHE_SPACING_M + halo) / PROPOSAL_SPACING_M).floor() as i64
            });
            let mut objects = Vec::new();
            for z in lo[2]..=hi[2] {
                for y in lo[1]..=hi[1] {
                    for x in lo[0]..=hi[0] {
                        if let Some(basin) = self.frozen_proposal(world, [x, y, z], environment) {
                            objects.push(basin);
                        }
                    }
                }
            }
            // One stable traversal order also makes blend arithmetic independent
            // of the cache bucket, tile request order, and neighbouring cube faces.
            objects.sort_unstable_by_key(|b| b.id);
            let objects: Arc<[Arc<Basin>]> = objects.into();
            objects
        }))
    }
}

/// Reconstruct a small basin or its dry wetland margin. Habitat is decided at
/// the object's frozen centre and rim, so a province boundary cannot cut a lake.
/// Existing macro water must retain priority in the caller.
pub fn sample<F>(
    world: &World,
    position: [f64; 3],
    current: LocalWaterEnvironment,
    environment: F,
) -> Option<LocalWaterSample>
where
    F: Fn([f64; 3]) -> LocalWaterEnvironment,
{
    if world.config.regional_lake_density <= 0.0 {
        return None;
    }
    let p = normalize(position);
    let atlas = world
        .local_water_cache
        .get_or_init(|| Arc::new(LocalWaterAtlas::default()));
    let objects = atlas.neighbourhood(world, p, &environment);
    #[derive(Clone, Copy)]
    struct View {
        phi: f64,
        weight: f64,
        height_m: f64,
        level_m: f64,
        id: u64,
    }
    const EMPTY: View = View {
        phi: f64::INFINITY,
        weight: 0.0,
        height_m: 0.0,
        level_m: 0.0,
        id: 0,
    };
    let mut views = [EMPTY; 64];
    let mut count = 0;
    let mut wetland = 0.0_f64;
    for basin in objects.iter() {
        let Some((phi, x, y)) = basin.shape_at(p, world.config.radius_m) else {
            continue;
        };
        let weight = 1.0 - smooth(phi / SUPPORT_PHI);
        let floor_variation = 0.80
            + 0.20
                * planar_noise(
                    basin.seed ^ 0x819e,
                    x / (basin.length_m * 0.31),
                    y / (basin.minor_m * 0.48),
                );
        let profile = basin.level_m + 0.18 - basin.depth_m * smooth(-phi / 0.75) * floor_variation
            + basin.depth_m * 0.55 * smooth(phi / SUPPORT_PHI);
        let view = View {
            phi,
            weight,
            height_m: current.natural_height_m * (1.0 - weight) + profile * weight,
            level_m: basin.level_m,
            id: basin.id,
        };
        wetland = wetland.max(weight * basin.wetland_strength);
        if count < views.len() {
            views[count] = view;
            count += 1;
        } else {
            // The generous fixed buffer bounds per-point work even for an
            // unusually crowded proposal set. Keep the strongest supports.
            let weakest = (0..count)
                .max_by(|&a, &b| {
                    views[a]
                        .phi
                        .total_cmp(&views[b].phi)
                        .then(views[a].id.cmp(&views[b].id))
                })
                .unwrap();
            if view.phi < views[weakest].phi {
                views[weakest] = view;
            }
        }
    }
    let owner = *views[..count]
        .iter()
        .min_by(|a, b| a.phi.total_cmp(&b.phi).then(a.id.cmp(&b.id)))?;
    let natural = current.natural_height_m;
    let mut height_delta = (owner.height_m - natural) * owner.weight;
    let mut weight_sum = owner.weight;
    for view in &views[..count] {
        if view.id == owner.id {
            continue;
        }
        let gap = (view.phi - owner.phi).max(0.0);
        let influence = 1.0 - smooth(gap / 0.18);
        let weight = influence * view.weight;
        height_delta += (view.height_m - natural) * weight;
        weight_sum += weight;
    }
    let mut height_m = natural + height_delta / weight_sum.max(f64::MIN_POSITIVE);
    // At an ownership equality the crest is above both horizontal planes.
    // Continuous support leaves a dry divide instead of a water-level cliff.
    let mut ridge_lift = 0.0_f64;
    for view in &views[..count] {
        if view.id == owner.id || (view.level_m - owner.level_m).abs() <= 0.01 {
            continue;
        }
        let gap = (view.phi - owner.phi).max(0.0);
        let influence = 1.0 - smooth(gap / 0.18);
        let support = 1.0 - smooth(view.phi.max(owner.phi) / SUPPORT_PHI);
        let crest = view.level_m.max(owner.level_m) + 0.25;
        // A remote third plane with zero influence must not raise another
        // pair's crest when its support first enters the neighbourhood.
        ridge_lift = ridge_lift.max((crest - height_m).max(0.0) * influence * support);
    }
    height_m += ridge_lift;
    Some(LocalWaterSample {
        height_m,
        level_m: owner.level_m,
        id: owner.id,
        wetland_weight: wetland.clamp(0.0, 1.0),
        lake: owner.phi < 0.0 && height_m < owner.level_m,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::WorldConfig;

    fn world() -> World {
        World::generate(WorldConfig {
            resolution: 8,
            erosion_steps: 0,
            regional_lake_density: 1.0,
            ..Default::default()
        })
        .unwrap()
    }

    fn environment(_: [f64; 3]) -> LocalWaterEnvironment {
        LocalWaterEnvironment {
            natural_height_m: 120.0,
            province_weight: 1.0,
            moisture: 0.90,
            temperature_c: 1.0,
            roughness: 0.002,
            protected_water_weight: 0.0,
        }
    }

    fn point(x: f64, y: f64, radius: f64) -> [f64; 3] {
        normalize([1.0, y / radius, x / radius])
    }

    #[test]
    fn lake_planes_and_ids_are_fixed_across_cache_and_sampling_order() {
        let world = world();
        let mut levels = HashMap::<u64, f64>::new();
        let mut saved = Vec::new();
        let mut wet = 0;
        for y in 0..=80 {
            for x in 0..=80 {
                let p = point(
                    (x as f64 - 40.0) * 125.0,
                    (y as f64 - 40.0) * 125.0,
                    world.config.radius_m,
                );
                let s = sample(&world, p, environment(p), environment);
                if let Some(s) = s {
                    assert!((MICRO_LAKE_ID_BASE..(MICRO_LAKE_ID_BASE << 1)).contains(&s.id));
                    assert!(s.height_m.is_finite());
                    assert!(s.level_m - s.height_m < 12.0);
                    if s.lake {
                        wet += 1;
                        if let Some(level) = levels.insert(s.id, s.level_m) {
                            assert_eq!(level.to_bits(), s.level_m.to_bits());
                        }
                    }
                    saved.push((p, s));
                }
            }
        }
        assert!(
            wet > 30,
            "a humid flat province should contain resolved small lakes"
        );
        assert!(
            levels.len() > 8,
            "the province should have multiple distinct basins"
        );
        eprintln!(
            "local_water flat 10 km window: {wet} wet samples / 6561, {} lake IDs",
            levels.len()
        );
        *world.local_water_cache.get().unwrap().cache.lock().unwrap() = Cache::default();
        for (p, old) in saved.into_iter().rev() {
            let new = sample(&world, p, environment(p), environment).unwrap();
            assert_eq!(old.id, new.id);
            assert_eq!(old.height_m.to_bits(), new.height_m.to_bits());
            assert_eq!(old.level_m.to_bits(), new.level_m.to_bits());
            assert_eq!(old.lake, new.lake);
        }
    }

    #[test]
    fn dry_warm_steep_and_protected_provinces_remain_dry() {
        for kind in 0..4 {
            let world = world();
            let fields = |p| {
                let mut e = environment(p);
                match kind {
                    0 => e.moisture = 0.15,
                    1 => e.temperature_c = 25.0,
                    2 => e.roughness = 0.12,
                    _ => e.protected_water_weight = 1.0,
                }
                e
            };
            for y in 0..=20 {
                for x in 0..=20 {
                    let p = point(
                        (x as f64 - 10.0) * 500.0,
                        (y as f64 - 10.0) * 500.0,
                        world.config.radius_m,
                    );
                    assert!(sample(&world, p, fields(p), fields).is_none());
                }
            }
        }
    }

    #[test]
    fn bucket_edges_and_province_edges_do_not_cut_existing_objects() {
        let world = world();
        let radius = world.config.radius_m;
        let atlas = world
            .local_water_cache
            .get_or_init(|| Arc::new(LocalWaterAtlas::default()));
        let objects = atlas.neighbourhood(&world, point(0.0, 0.0, radius), &environment);
        let basin = objects.iter().find(|b| b.length_m > 250.0).unwrap();
        let p = basin.center;
        let mut current = environment(p);
        current.province_weight = 0.0;
        let normal = sample(&world, p, environment(p), environment).unwrap();
        let beyond_province = sample(&world, p, current, environment).unwrap();
        assert_eq!(normal.id, beyond_province.id);
        assert_eq!(
            normal.height_m.to_bits(),
            beyond_province.height_m.to_bits()
        );
        let mut max_jump = 0.0_f64;
        for y in -100..=100 {
            let py = y as f64 * 50.0;
            let at = |offset: f64| {
                let z = CACHE_SPACING_M + offset;
                normalize([(radius * radius - py * py - z * z).sqrt(), py, z])
            };
            let a = at(-0.001);
            let b = at(0.001);
            let a = sample(&world, a, environment(a), environment).map_or(120.0, |s| s.height_m);
            let b = sample(&world, b, environment(b), environment).map_or(120.0, |s| s.height_m);
            max_jump = max_jump.max((a - b).abs());
        }
        assert!(
            max_jump < 0.01,
            "2 mm pairs across a cache edge jumped {max_jump} m"
        );
    }

    #[test]
    fn different_lake_planes_have_a_dry_continuous_divide() {
        let world = world();
        let radius = world.config.radius_m;
        let fields = |p: [f64; 3]| {
            let mut e = environment(p);
            e.natural_height_m += p[2] * radius * 0.0012
                + p[1] * radius * 0.0004
                + 1.2 * crate::noise(7_771, p, radius / 700.0);
            e
        };
        let mut wet_plane_changes = 0;
        let mut checked = 0;
        let mut max_height_jump = 0.0_f64;
        for y in 0..=80 {
            let py = (y as f64 - 40.0) * 125.0;
            let mut previous_x = -5_000.0;
            let p = point(previous_x, py, radius);
            let mut previous = sample(&world, p, fields(p), fields);
            for x in 1..=160 {
                let px = (x as f64 - 80.0) * 62.5;
                let p = point(px, py, radius);
                let current = sample(&world, p, fields(p), fields);
                if let (Some(a), Some(b)) = (previous, current) {
                    if a.id != b.id && (a.level_m - b.level_m).abs() > 0.01 {
                        let mut lo = previous_x;
                        let mut hi = px;
                        for _ in 0..24 {
                            let mid = (lo + hi) * 0.5;
                            let p = point(mid, py, radius);
                            if sample(&world, p, fields(p), fields).is_some_and(|s| s.id == a.id) {
                                lo = mid;
                            } else {
                                hi = mid;
                            }
                        }
                        let pa = point(lo, py, radius);
                        let pb = point(hi, py, radius);
                        if let (Some(a), Some(b)) = (
                            sample(&world, pa, fields(pa), fields),
                            sample(&world, pb, fields(pb), fields),
                        ) {
                            checked += 1;
                            max_height_jump = max_height_jump.max((a.height_m - b.height_m).abs());
                            wet_plane_changes += usize::from(a.lake && b.lake);
                        }
                    }
                }
                previous_x = px;
                previous = current;
            }
        }
        eprintln!("local_water sloped 10 km window: {checked} owner divides, {wet_plane_changes} wet plane jumps, max ground jump {max_height_jump:.9} m");
        assert!(checked > 20);
        assert_eq!(
            wet_plane_changes, 0,
            "horizontal planes must be separated by dry terrain"
        );
        assert!(max_height_jump < 0.001);
    }

    #[test]
    fn macro_lake_and_sea_keep_their_beds_planes_and_ids() {
        for marine in [false, true] {
            let mut world = world();
            world.config.detail_m = 0.0;
            world.config.coast_detail_ratio = 0.0;
            world.config.fjord_strength = 0.0;
            world.config.archipelago_density = 0.0;
            world.lakes.clear();
            for cell in &mut world.cells {
                cell.height_m = if marine { -20.0 } else { 120.0 };
                cell.water_m = if marine { 0.0 } else { 145.0 };
                cell.ocean = marine;
                cell.lake = !marine;
                cell.lake_id = (!marine).then_some(42);
                cell.biome = if marine {
                    crate::Biome::Ocean
                } else {
                    crate::Biome::Lake
                };
                cell.discharge = 0.0;
                cell.receiver = None;
            }
            world.invalidate_geometry();
            let mut disabled = world.clone();
            disabled.config.regional_lake_density = 0.0;
            disabled.invalidate_geometry();
            for i in 0..256 {
                let y = 1.0 - 2.0 * (i as f64 + 0.5) / 256.0;
                let angle = i as f64 * 2.399_963_229_728_653;
                let r = (1.0 - y * y).sqrt();
                let p = [r * angle.cos(), y, r * angle.sin()];
                let a = world.sample(p);
                let b = disabled.sample(p);
                assert_eq!(a.height_m.to_bits(), b.height_m.to_bits());
                assert_eq!(a.water_m.to_bits(), b.water_m.to_bits());
                assert_eq!(a.water_body_id, b.water_body_id);
                assert_eq!(a.water_body_id, Some(if marine { 0 } else { 42 }));
                assert_eq!(a.lake, !marine);
                assert_eq!(a.ocean, marine);
            }
        }
    }

    #[test]
    fn concurrent_cold_bucket_is_frozen_once_outside_the_mutex() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Barrier,
        };
        let baseline = world();
        let p = point(0.0, 0.0, baseline.config.radius_m);
        let serial_calls = AtomicUsize::new(0);
        let serial = |p| {
            serial_calls.fetch_add(1, Ordering::Relaxed);
            if let Some(atlas) = baseline.local_water_cache.get() {
                assert!(atlas.cache.try_lock().is_ok());
            }
            environment(p)
        };
        let _ = sample(&baseline, p, environment(p), serial);
        let expected = serial_calls.load(Ordering::Relaxed);
        assert!(expected > 100);

        let world = world();
        let calls = AtomicUsize::new(0);
        let start = Barrier::new(4);
        std::thread::scope(|scope| {
            for _ in 0..4 {
                scope.spawn(|| {
                    start.wait();
                    let callback = |p| {
                        calls.fetch_add(1, Ordering::Relaxed);
                        environment(p)
                    };
                    let _ = sample(&world, p, environment(p), callback);
                });
            }
        });
        assert_eq!(calls.load(Ordering::Relaxed), expected);
    }

    #[test]
    fn overlapping_buckets_share_accepted_and_rejected_proposals() {
        use std::sync::{
            atomic::{AtomicUsize, Ordering},
            Barrier,
        };
        let mut baseline = world();
        baseline.config.regional_lake_density = 0.55;
        let points = [
            point(0.0, 0.0, baseline.config.radius_m),
            point(5_000.0, 0.0, baseline.config.radius_m),
        ];
        let serial_calls = AtomicUsize::new(0);
        let serial = |p| {
            serial_calls.fetch_add(1, Ordering::Relaxed);
            environment(p)
        };
        let atlas = baseline
            .local_water_cache
            .get_or_init(|| Arc::new(LocalWaterAtlas::default()));
        let saved = points.map(|p| atlas.neighbourhood(&baseline, p, &serial));
        let expected = serial_calls.load(Ordering::Relaxed);
        {
            let mut cache = atlas.cache.lock().unwrap();
            assert!(cache
                .proposals
                .values()
                .any(|slot| matches!(slot.get(), Some(None))));
            cache.buckets.clear();
            cache.insertion_order.clear();
        }
        for (index, p) in points.into_iter().enumerate() {
            let reused = atlas.neighbourhood(&baseline, p, &|_| {
                panic!("accepted and rejected proposals should both be retained")
            });
            assert_eq!(saved[index].len(), reused.len());
            for (a, b) in saved[index].iter().zip(reused.iter()) {
                assert!(Arc::ptr_eq(a, b));
            }
        }

        let mut world = world();
        world.config.regional_lake_density = 0.55;
        let calls = AtomicUsize::new(0);
        let start = Barrier::new(2);
        std::thread::scope(|scope| {
            for p in points {
                let calls = &calls;
                let start = &start;
                let world = &world;
                scope.spawn(move || {
                    start.wait();
                    let atlas = world
                        .local_water_cache
                        .get_or_init(|| Arc::new(LocalWaterAtlas::default()));
                    let _ = atlas.neighbourhood(world, p, &|p| {
                        calls.fetch_add(1, Ordering::Relaxed);
                        environment(p)
                    });
                });
            }
        });
        assert_eq!(calls.load(Ordering::Relaxed), expected);
    }

    #[test]
    fn neighbouring_support_entries_leave_height_continuous() {
        let world = world();
        let radius = world.config.radius_m;
        let fields = |p: [f64; 3]| {
            let mut e = environment(p);
            e.natural_height_m += p[2] * radius * 0.001 + p[1] * radius * 0.0002;
            e
        };
        let supports = |p| {
            let atlas = world
                .local_water_cache
                .get_or_init(|| Arc::new(LocalWaterAtlas::default()));
            atlas
                .neighbourhood(&world, p, &fields)
                .iter()
                .filter(|b| b.shape_at(p, radius).is_some())
                .map(|b| b.id)
                .collect::<Vec<_>>()
        };
        let mut checked = 0;
        let mut max_jump = 0.0_f64;
        for y in -10..=10 {
            let py = y as f64 * 400.0;
            let mut previous_x = -5_000.0;
            let mut previous = supports(point(previous_x, py, radius));
            for x in 1..=160 {
                let px = (x as f64 - 80.0) * 62.5;
                let current = supports(point(px, py, radius));
                if current != previous {
                    let mut lo = previous_x;
                    let mut hi = px;
                    for _ in 0..24 {
                        let mid = (lo + hi) * 0.5;
                        if supports(point(mid, py, radius)) == previous {
                            lo = mid;
                        } else {
                            hi = mid;
                        }
                    }
                    let pa = point(lo, py, radius);
                    let pb = point(hi, py, radius);
                    let a = sample(&world, pa, fields(pa), fields)
                        .map_or(fields(pa).natural_height_m, |s| s.height_m);
                    let b = sample(&world, pb, fields(pb), fields)
                        .map_or(fields(pb).natural_height_m, |s| s.height_m);
                    max_jump = max_jump.max((a - b).abs());
                    checked += 1;
                }
                previous_x = px;
                previous = current;
            }
        }
        eprintln!(
            "local_water support entries: {checked} boundaries, max ground jump {max_jump:.9} m"
        );
        assert!(checked > 50);
        assert!(
            max_jump < 0.0001,
            "a neighbouring dry support introduced a jump"
        );
    }
}
