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
    cell::RefCell,
    collections::{HashMap, VecDeque},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex, OnceLock,
    },
};

/// The physical guard used by the province and macro-water exclusion halos.
pub(crate) const LOCAL_WATER_SUPPORT_M: f64 = 2_300.0;
const MAX_SUPPORT_M: f64 = LOCAL_WATER_SUPPORT_M;
const CACHE_SPACING_M: f64 = 4_096.0;

#[derive(Clone, Copy, Debug)]
struct Tier {
    spacing_m: f64,
    bucket_m: f64,
    support_m: f64,
    diameter_min_m: f64,
    diameter_max_m: f64,
    rate: f64,
}

// All four populations exist at every query resolution. A small bucket limits
// the cold footprint of a sparse metre-scale query; it does not set geography.
const TIERS: [Tier; 4] = [
    Tier {
        spacing_m: 1_050.0,
        bucket_m: CACHE_SPACING_M,
        support_m: MAX_SUPPORT_M,
        diameter_min_m: 180.0,
        diameter_max_m: 3_000.0,
        rate: 1.0,
    },
    Tier {
        spacing_m: 400.0,
        bucket_m: 1_024.0,
        support_m: 480.0,
        diameter_min_m: 120.0,
        diameter_max_m: 620.0,
        rate: 0.88,
    },
    Tier {
        spacing_m: 160.0,
        bucket_m: 512.0,
        support_m: 130.0,
        diameter_min_m: 35.0,
        diameter_max_m: 170.0,
        rate: 0.65,
    },
    Tier {
        spacing_m: 60.0,
        bucket_m: 192.0,
        support_m: 43.0,
        diameter_min_m: 10.0,
        diameter_max_m: 55.0,
        rate: 0.30,
    },
];

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Address {
    tier: u8,
    cell: [i64; 3],
}
const SUPPORT_PHI: f64 = 0.65;
const CACHE_BUCKET_LIMIT: usize = 1_024;
const PROPOSAL_CACHE_LIMIT: usize = 65_536;
const CACHE_SHARDS: usize = 8;
const BUCKET_TIER_LIMITS: [usize; 4] = [384, 256, 256, 128];
const PROPOSAL_TIER_LIMITS: [usize; 4] = [32_768, 16_384, 8_192, 8_192];
const MICRO_LAKE_ID_BASE: u64 = 1 << 40;
const MICRO_LAKE_ID_MASK: u64 = (1 << 38) - 1;

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
    /// Conservative distance inside a protected primitive, measured in metres.
    /// The caller must guarantee the same primitive at every point within this
    /// distance. Zero disables the exact interior acceleration.
    pub protected_inner_m: f64,
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
    buckets: HashMap<Address, BucketSlot>,
    insertion_order: [VecDeque<Address>; 4],
    bucket_counts: [usize; 4],
    proposals: HashMap<Address, ProposalSlot>,
    proposal_order: [VecDeque<Address>; 4],
    proposal_counts: [usize; 4],
    stats: LocalWaterCacheStats,
}

/// Cost counters, separated by physical scale. They do not affect decisions.
#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub struct LocalWaterCacheStats {
    pub bucket_builds: [usize; 4],
    pub proposal_builds: [usize; 4],
    pub environment_calls: [usize; 4],
    pub freeze_attempts: [usize; 4],
    pub retained_buckets: [usize; 4],
    pub retained_proposals: [usize; 4],
    pub retained_basins: [usize; 4],
    pub bucket_query_count: usize,
    /// Optional aggregate times across all worker threads, including waits.
    pub bucket_lookup_ns: u64,
    pub proposal_cache_ns: [u64; 4],
    pub shell_scan_ns: [u64; 4],
    pub descriptor_ns: [u64; 4],
    pub freeze_ns: [u64; 4],
}

pub fn cache_stats(world: &World) -> LocalWaterCacheStats {
    let Some(atlas) = world.local_water_cache.get() else {
        return LocalWaterCacheStats::default();
    };
    let mut stats = LocalWaterCacheStats {
        bucket_query_count: atlas.bucket_query_count.load(Ordering::Relaxed),
        ..Default::default()
    };
    for shard in &atlas.caches {
        let cache = shard.lock().unwrap_or_else(|poison| poison.into_inner());
        stats.bucket_lookup_ns += cache.stats.bucket_lookup_ns;
        for tier in 0..4 {
            stats.bucket_builds[tier] += cache.stats.bucket_builds[tier];
            stats.proposal_builds[tier] += cache.stats.proposal_builds[tier];
            stats.environment_calls[tier] += cache.stats.environment_calls[tier];
            stats.freeze_attempts[tier] += cache.stats.freeze_attempts[tier];
            stats.proposal_cache_ns[tier] += cache.stats.proposal_cache_ns[tier];
            stats.shell_scan_ns[tier] += cache.stats.shell_scan_ns[tier];
            stats.descriptor_ns[tier] += cache.stats.descriptor_ns[tier];
            stats.freeze_ns[tier] += cache.stats.freeze_ns[tier];
        }
        for address in cache.buckets.keys() {
            stats.retained_buckets[address.tier as usize] += 1;
        }
        for (address, slot) in &cache.proposals {
            let tier = address.tier as usize;
            stats.retained_proposals[tier] += 1;
            if let Some(Some(proposal)) = slot.get() {
                if matches!(proposal.frozen.get(), Some(Some(_))) {
                    stats.retained_basins[tier] += 1;
                }
            }
        }
    }
    stats
}

/// A bounded, reconstructible cache. Direct Atlas clones start empty. World
/// stores it in an Arc, so World clones share it until invalidate_geometry().
#[derive(Default)]
pub struct LocalWaterAtlas {
    caches: [Mutex<Cache>; CACHE_SHARDS],
    bucket_query_count: AtomicUsize,
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
    proposal_probability: f64,
    wetland_strength: f64,
}

#[derive(Default)]
struct NeighbourhoodFront {
    // Retain the owner, so allocator reuse cannot alias a stale atlas address.
    owner: Option<Arc<LocalWaterAtlas>>,
    entries: VecDeque<FrontEntry>,
}
struct FrontEntry {
    keys: [Address; 4],
    active: [bool; 4],
    values: [Arc<[Arc<Proposal>]>; 4],
}
thread_local! {
    static NEIGHBOURHOOD_FRONT: RefCell<NeighbourhoodFront> = RefCell::new(NeighbourhoodFront::default());
}

struct CostTimer {
    #[cfg(not(target_arch = "wasm32"))]
    started: Option<std::time::Instant>,
}
impl CostTimer {
    fn start() -> Self {
        Self {
            #[cfg(not(target_arch = "wasm32"))]
            started: {
                static ENABLED: OnceLock<bool> = OnceLock::new();
                (*ENABLED.get_or_init(|| std::env::var_os("WB_PROFILE_LOCAL_WATER_COST").is_some()))
                    .then(std::time::Instant::now)
            },
        }
    }
    fn elapsed_ns(&self) -> u64 {
        #[cfg(not(target_arch = "wasm32"))]
        return self.started.map_or(0, |start| {
            start.elapsed().as_nanos().min(u64::MAX as u128) as u64
        });
        #[cfg(target_arch = "wasm32")]
        0
    }
}

type BucketSlot = Arc<OnceLock<Arc<[Arc<Proposal>]>>>;
type ProposalSlot = Arc<OnceLock<Option<Arc<Proposal>>>>;

struct Candidate {
    address: Address,
    seed: u64,
    center: [f64; 3],
    cache_shard: usize,
}

struct Proposal {
    basin: Basin,
    frozen: OnceLock<Option<Arc<Basin>>>,
    cache_shard: usize,
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

fn cache_shard(tier: u8, physical: [f64; 3]) -> usize {
    // Nearby slots share one lock, while distant hydraulic workers can build
    // independent neighbourhoods concurrently. This only routes cache entries;
    // neither the shard boundary nor its hash enters the physical field.
    let tile_m = TIERS[tier as usize].bucket_m * 4.0;
    let cell = physical.map(|coordinate| (coordinate / tile_m).floor() as i64);
    (signed_key(0x9a6b_27e1_33fc_0715 ^ tier as u64, cell) as usize) % CACHE_SHARDS
}

fn bucket_cache_shard(key: Address) -> usize {
    cache_shard(
        key.tier,
        key.cell
            .map(|coordinate| coordinate as f64 * TIERS[key.tier as usize].bucket_m),
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

/// Shared geographic lake fields, with nested wet swales at the smaller
/// scales. These continuous fields bias proposals, never multiply their final
/// bed or hydraulic plane at a query point.
fn lake_field_probability(seed: u64, p: [f64; 3], radius_m: f64, tier: u8) -> f64 {
    let broad = crate::noise(seed ^ 0x41af_885d, p, radius_m / 4_800.0);
    let patch = crate::noise(seed ^ 0x26e3_044b, p, radius_m / 1_150.0);
    let field = smooth((0.18 + 0.78 * broad + 0.22 * patch) / 0.50);
    if tier < 2 {
        0.12 + 0.88 * field
    } else {
        // Near a coherent contour of a warped geographical field, pools form
        // narrow strings; elsewhere only sparse residual basins survive.
        let swale = crate::noise(seed ^ 0xc51d_1193, p, radius_m / 430.0);
        let corridor = 1.0 - smooth((swale.abs() - 0.08) / 0.28);
        (0.025 + 0.975 * field) * (0.10 + 0.90 * corridor)
    }
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
    fn proposal_center(
        planet_seed: u64,
        address: Address,
        radius_m: f64,
    ) -> Option<(u64, [f64; 3])> {
        let tier = TIERS[address.tier as usize];
        // Reject whole cubic cells outside the thin physical shell before
        // hashing a jittered centre. The millimetre margin only covers floating
        // arithmetic; it cannot remove any proposal admitted below.
        let mut near_squared = 0.0;
        let mut far_squared = 0.0;
        for coordinate in address.cell {
            let low = coordinate as f64 * tier.spacing_m;
            let high = (coordinate + 1) as f64 * tier.spacing_m;
            near_squared += low.abs().min(high.abs()).powi(2);
            far_squared += low.abs().max(high.abs()).powi(2);
        }
        if near_squared > (radius_m + tier.spacing_m * 0.55 + 0.001).powi(2)
            || far_squared < (radius_m - tier.spacing_m * 0.55 - 0.001).max(0.0).powi(2)
        {
            return None;
        }
        // Retain the old coarse hash namespace, and give each added scale its
        // own namespace before truncating the stable 40-bit body identifier.
        let namespace = if address.tier == 0 {
            0
        } else {
            hash64(0x6315_c923_7b0a_228d ^ address.tier as u64)
        };
        let seed = signed_key(
            planet_seed ^ 0x81a7_2b43_3ed0_9675 ^ namespace,
            address.cell,
        );
        let raw: [f64; 3] = std::array::from_fn(|i| {
            (address.cell[i] as f64 + random(seed.wrapping_add(i as u64 * 173 + 11)))
                * tier.spacing_m
        });
        if (dot(raw, raw).sqrt() - radius_m).abs() > tier.spacing_m * 0.55 {
            return None;
        }
        Some((seed, normalize(raw)))
    }

    fn proposal(
        planet_seed: u64,
        address: Address,
        radius_m: f64,
        seed: u64,
        center: [f64; 3],
        proposal_probability: f64,
    ) -> Option<Self> {
        let tier = TIERS[address.tier as usize];
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
        // Logarithmic physical ranges, with a long upper tail at the largest
        // scale. The denser scales never enlarge a distant pond at coarse LOD.
        let exponent = if address.tier == 0 {
            size.powf(1.45)
        } else {
            size
        };
        let diameter =
            tier.diameter_min_m * (tier.diameter_max_m / tier.diameter_min_m).powf(exponent);
        let length = diameter * 0.5;
        let fissure = random(seed ^ 0x732b) < 0.32;
        let aspect = if fissure {
            2.8 + 3.4 * random(seed ^ 0xa951)
        } else {
            1.25 + 2.0 * random(seed ^ 0xa951)
        };
        let minor = length / aspect;
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
        if support_m >= tier.support_m || support_m >= MAX_SUPPORT_M {
            return None;
        }
        Some(Self {
            id: MICRO_LAKE_ID_BASE | ((address.tier as u64) << 38) | (seed & MICRO_LAKE_ID_MASK),
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
            proposal_probability,
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
        let probability = density.clamp(0.0, 1.0) * self.proposal_probability;
        if draw >= probability {
            profile_freeze(self.id, "cheap_density", [0.0; 5]);
            return false;
        }
        let anchor = self.lobes[(self.lobe_count - 1) / 2];
        let center = environment(self.direction_at(radius_m, anchor.x, anchor.y));
        let moisture = smooth((center.moisture - 0.30) / 0.30);
        // Relict glacial basins also hold summer water in temperate shields.
        let cold = smooth((24.0 - center.temperature_c) / 20.0)
            * smooth((center.temperature_c + 25.0) / 15.0);
        let gentle = smooth((0.060 - center.roughness) / 0.045);
        let altitude = smooth((center.natural_height_m - 35.0) / 45.0)
            * smooth((1_650.0 - center.natural_height_m) / 950.0);
        let suitability = probability
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
    fn bucket_slots(&self, keys: [Address; 4], active: [bool; 4]) -> [Option<BucketSlot>; 4] {
        let shards = keys.map(bucket_cache_shard);
        let mut slots = std::array::from_fn(|_| None);
        self.bucket_query_count.fetch_add(1, Ordering::Relaxed);
        // At most one shard lock is held. No callback, descriptor build or
        // OnceLock wait can block unrelated hydraulic workers behind it.
        for shard in 0..CACHE_SHARDS {
            if !(0..4).any(|tier| active[tier] && shards[tier] == shard) {
                continue;
            }
            let timer = CostTimer::start();
            let mut cache = self.caches[shard]
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            for tier in 0..4 {
                if !active[tier] || shards[tier] != shard {
                    continue;
                }
                let key = keys[tier];
                if let Some(slot) = cache.buckets.get(&key) {
                    slots[tier] = Some(Arc::clone(slot));
                    continue;
                }
                while cache.bucket_counts[tier] >= BUCKET_TIER_LIMITS[tier] / CACHE_SHARDS {
                    if let Some(old) = cache.insertion_order[tier].pop_front() {
                        if cache.buckets.remove(&old).is_some() {
                            cache.bucket_counts[tier] -= 1;
                        }
                    }
                }
                let slot = Arc::new(OnceLock::new());
                cache.insertion_order[tier].push_back(key);
                cache.buckets.insert(key, Arc::clone(&slot));
                cache.bucket_counts[tier] += 1;
                cache.stats.bucket_builds[tier] += 1;
                debug_assert!(cache.buckets.len() <= CACHE_BUCKET_LIMIT / CACHE_SHARDS);
                slots[tier] = Some(slot);
            }
            cache.stats.bucket_lookup_ns += timer.elapsed_ns();
        }
        slots
    }

    fn proposal_slots(&self, candidates: &[Candidate]) -> Vec<ProposalSlot> {
        // A sparse hydraulic query can touch dozens of shell proposals. Batch
        // the cache bookkeeping so parallel river-bank queries do not contend
        // for the same mutex once per accepted or cheaply rejected address.
        let mut groups: [Vec<usize>; CACHE_SHARDS] = std::array::from_fn(|_| Vec::new());
        for (index, candidate) in candidates.iter().enumerate() {
            groups[candidate.cache_shard].push(index);
        }
        let mut slots = vec![None; candidates.len()];
        for (shard, group) in groups.iter().enumerate() {
            let Some(&first) = group.first() else {
                continue;
            };
            let timer = CostTimer::start();
            let mut cache = self.caches[shard]
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            for &index in group {
                let candidate = &candidates[index];
                let address = candidate.address;
                if let Some(slot) = cache.proposals.get(&address) {
                    slots[index] = Some(Arc::clone(slot));
                    continue;
                }
                let tier = address.tier as usize;
                while cache.proposal_counts[tier] >= PROPOSAL_TIER_LIMITS[tier] / CACHE_SHARDS {
                    if let Some(old) = cache.proposal_order[tier].pop_front() {
                        if cache.proposals.remove(&old).is_some() {
                            cache.proposal_counts[tier] -= 1;
                        }
                    }
                }
                let slot = Arc::new(OnceLock::new());
                cache.proposal_order[tier].push_back(address);
                cache.proposals.insert(address, Arc::clone(&slot));
                cache.proposal_counts[tier] += 1;
                cache.stats.proposal_builds[tier] += 1;
                debug_assert!(cache.proposals.len() <= PROPOSAL_CACHE_LIMIT / CACHE_SHARDS);
                slots[index] = Some(slot);
            }
            cache.stats.proposal_cache_ns[candidates[first].address.tier as usize] +=
                timer.elapsed_ns();
        }
        slots.into_iter().map(Option::unwrap).collect()
    }

    fn proposal(
        world: &World,
        candidate: &Candidate,
        slot: &ProposalSlot,
    ) -> Option<Arc<Proposal>> {
        slot.get_or_init(|| {
            let draw = random(candidate.seed ^ 0xdc15);
            let density = world.config.regional_lake_density.clamp(0.0, 1.0);
            let rate = TIERS[candidate.address.tier as usize].rate;
            // All later geographic/habitat factors are <=1. These are upper
            // bounds on the same draw, so density is applied exactly once.
            if draw >= density * rate {
                return None;
            }
            let probability = rate
                * lake_field_probability(
                    world.config.seed,
                    candidate.center,
                    world.config.radius_m,
                    candidate.address.tier,
                );
            if draw >= density * probability {
                return None;
            }
            let basin = Basin::proposal(
                world.config.seed,
                candidate.address,
                world.config.radius_m,
                candidate.seed,
                candidate.center,
                probability,
            )?;
            Some(Arc::new(Proposal {
                basin,
                frozen: OnceLock::new(),
                cache_shard: candidate.cache_shard,
            }))
        })
        .clone()
    }

    fn freeze_proposal<F>(
        &self,
        world: &World,
        descriptor: &Proposal,
        environment: &F,
    ) -> Option<Arc<Basin>>
    where
        F: Fn([f64; 3]) -> LocalWaterEnvironment,
    {
        descriptor
            .frozen
            .get_or_init(|| {
                let timer = CostTimer::start();
                let mut basin = descriptor.basin.clone();
                let calls = std::cell::Cell::new(0usize);
                let fields = |p| {
                    calls.set(calls.get() + 1);
                    environment(p)
                };
                let accepted = basin.freeze(
                    world.config.radius_m,
                    world.config.regional_lake_density,
                    &fields,
                );
                let tier = ((basin.id >> 38) & 3) as usize;
                let mut cache = self.caches[descriptor.cache_shard]
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                cache.stats.environment_calls[tier] += calls.get();
                cache.stats.freeze_attempts[tier] += 1;
                cache.stats.freeze_ns[tier] += timer.elapsed_ns();
                accepted.then(|| Arc::new(basin))
            })
            .clone()
    }

    fn descriptor_bucket(
        &self,
        world: &World,
        key: Address,
        slot: &BucketSlot,
    ) -> Arc<[Arc<Proposal>]> {
        Arc::clone(slot.get_or_init(|| {
            let scan_timer = CostTimer::start();
            let radius = world.config.radius_m;
            let tier = TIERS[key.tier as usize];
            // Include spherical projection and every possible physical support.
            let halo = tier.support_m + tier.spacing_m * 0.55 + 1.0;
            let lo: [i64; 3] = std::array::from_fn(|i| {
                ((key.cell[i] as f64 * tier.bucket_m - halo) / tier.spacing_m).floor() as i64
            });
            let hi: [i64; 3] = std::array::from_fn(|i| {
                (((key.cell[i] + 1) as f64 * tier.bucket_m + halo) / tier.spacing_m).floor() as i64
            });
            let mut candidates = Vec::new();
            for z in lo[2]..=hi[2] {
                for y in lo[1]..=hi[1] {
                    for x in lo[0]..=hi[0] {
                        let address = Address {
                            tier: key.tier,
                            cell: [x, y, z],
                        };
                        if let Some((seed, center)) =
                            Basin::proposal_center(world.config.seed, address, radius)
                        {
                            candidates.push(Candidate {
                                address,
                                seed,
                                center,
                                cache_shard: cache_shard(
                                    key.tier,
                                    address
                                        .cell
                                        .map(|coordinate| coordinate as f64 * tier.spacing_m),
                                ),
                            });
                        }
                    }
                }
            }
            let scan_ns = scan_timer.elapsed_ns();
            let proposal_slots = self.proposal_slots(&candidates);
            let descriptor_timer = CostTimer::start();
            let mut objects = Vec::new();
            for (candidate, proposal_slot) in candidates.iter().zip(&proposal_slots) {
                let Some(proposal) = Self::proposal(world, candidate, proposal_slot) else {
                    continue;
                };
                // The maximum halo discovers candidates; their own support
                // controls which descriptors can actually reach this bucket.
                let distance_squared = (0..3)
                    .map(|i| {
                        let coordinate = proposal.basin.center[i] * radius;
                        let low = key.cell[i] as f64 * tier.bucket_m;
                        let high = (key.cell[i] + 1) as f64 * tier.bucket_m;
                        (coordinate - coordinate.clamp(low, high)).powi(2)
                    })
                    .sum::<f64>();
                if distance_squared <= proposal.basin.support_m.powi(2) {
                    objects.push(proposal);
                }
            }
            objects.sort_unstable_by_key(|p| p.basin.id);
            let descriptor_ns = descriptor_timer.elapsed_ns();
            if scan_ns > 0 || descriptor_ns > 0 {
                let mut cache = self.caches[bucket_cache_shard(key)]
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner());
                cache.stats.shell_scan_ns[key.tier as usize] += scan_ns;
                cache.stats.descriptor_ns[key.tier as usize] += descriptor_ns;
            }
            objects.into()
        }))
    }

    fn neighbourhoods(
        self: &Arc<Self>,
        world: &World,
        p: [f64; 3],
        active: [bool; 4],
    ) -> [Arc<[Arc<Proposal>]>; 4] {
        let keys = std::array::from_fn(|tier| Address {
            tier: tier as u8,
            cell: std::array::from_fn(|i| {
                (p[i] * world.config.radius_m / TIERS[tier].bucket_m).floor() as i64
            }),
        });
        if let Some(values) = NEIGHBOURHOOD_FRONT.with(|front| {
            let mut front = front.borrow_mut();
            if !front
                .owner
                .as_ref()
                .is_some_and(|owner| Arc::ptr_eq(owner, self))
            {
                front.owner = Some(Arc::clone(self));
                front.entries.clear();
            }
            front
                .entries
                .iter()
                .find(|entry| entry.keys == keys && entry.active == active)
                .map(|entry| entry.values.clone())
        }) {
            return values;
        }
        let slots = self.bucket_slots(keys, active);
        let values = std::array::from_fn(|tier| match &slots[tier] {
            Some(slot) => self.descriptor_bucket(world, keys[tier], slot),
            None => Arc::from([]),
        });
        NEIGHBOURHOOD_FRONT.with(|front| {
            let mut front = front.borrow_mut();
            if front.entries.len() >= 8 {
                front.entries.pop_front();
            }
            front.entries.push_back(FrontEntry {
                keys,
                active,
                values: values.clone(),
            });
        });
        values
    }

    #[cfg(test)]
    fn tier_neighbourhood(
        self: &Arc<Self>,
        world: &World,
        p: [f64; 3],
        tier: usize,
    ) -> Arc<[Arc<Proposal>]> {
        Arc::clone(&self.neighbourhoods(world, p, [true; 4])[tier])
    }

    #[cfg(test)]
    fn neighbourhood<F>(
        self: &Arc<Self>,
        world: &World,
        p: [f64; 3],
        environment: &F,
    ) -> Vec<Arc<Basin>>
    where
        F: Fn([f64; 3]) -> LocalWaterEnvironment,
    {
        let mut objects: Vec<_> = self
            .neighbourhoods(world, p, [true; 4])
            .iter()
            .flat_map(|tier| tier.iter())
            .filter_map(|proposal| self.freeze_proposal(world, proposal, environment))
            .collect();
        objects.sort_unstable_by_key(|b| b.id);
        objects
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
    let radius_m = world.config.radius_m;
    let active = TIERS.map(|tier| {
        // A touching proposal's spherical centre is at most R asin(S/R)
        // away. Freeze first checks a lobe anchor, displaced from that centre
        // by less than the same distance. If the protected primitive covers
        // both paths, freeze must reject it; no descriptor can affect p.
        // The extra metre covers floating arithmetic and stays conservative.
        let anchor_reach_m = 2.0 * radius_m * (tier.support_m / radius_m).asin() + 1.0;
        current.protected_inner_m <= anchor_reach_m
    });
    if active.iter().all(|&active| !active) {
        return None;
    }
    let p = normalize(position);
    let atlas = world
        .local_water_cache
        .get_or_init(|| Arc::new(LocalWaterAtlas::default()));
    let objects = atlas.neighbourhoods(world, p, active);
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
    for proposal in objects.iter().flat_map(|tier| tier.iter()) {
        if matches!(proposal.frozen.get(), Some(None)) {
            continue;
        }
        let Some((phi, x, y)) = proposal.basin.shape_at(p, world.config.radius_m) else {
            continue;
        };
        // The fixed physical contour exists independently of sampling density.
        // Only a descriptor touching this point needs its canonical habitat/
        // rim validation. Sparse catalogue queries never freeze a whole tile.
        let Some(basin) = atlas.freeze_proposal(world, proposal, &environment) else {
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
            // Basin excavation never adds a raised ring to its natural rim.
            height_m: current.natural_height_m * (1.0 - weight)
                + profile.min(current.natural_height_m) * weight,
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
    // Buckets from different physical scales have different object lists. A
    // stable global order keeps additions bit-identical at shared positions.
    views[..count].sort_unstable_by_key(|view| view.id);
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
            protected_inner_m: 0.0,
        }
    }

    fn point(x: f64, y: f64, radius: f64) -> [f64; 3] {
        normalize([1.0, y / radius, x / radius])
    }

    fn clear_caches(atlas: &LocalWaterAtlas, proposals: bool) {
        for shard in &atlas.caches {
            let mut cache = shard.lock().unwrap();
            if proposals {
                *cache = Cache::default();
            } else {
                cache.buckets.clear();
                cache.insertion_order.iter_mut().for_each(VecDeque::clear);
                cache.bucket_counts = [0; 4];
            }
        }
        NEIGHBOURHOOD_FRONT.with(|front| *front.borrow_mut() = NeighbourhoodFront::default());
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
        clear_caches(world.local_water_cache.get().unwrap(), true);
        for (p, old) in saved.into_iter().rev() {
            let new = sample(&world, p, environment(p), environment).unwrap();
            assert_eq!(old.id, new.id);
            assert_eq!(old.height_m.to_bits(), new.height_m.to_bits());
            assert_eq!(old.level_m.to_bits(), new.level_m.to_bits());
            assert_eq!(old.lake, new.lake);
        }
    }

    #[test]
    fn metre_samples_resolve_existing_pools_and_preserve_every_shared_point() {
        use std::collections::HashSet;
        let world = world();
        let radius = world.config.radius_m;
        // A fixed dense geographic patch of seed 42, distinct from the sparse
        // origin window. Selecting no window from the query LOD is essential.
        let origin = [4_000.0, -2_000.0];
        let mut previous = None::<(usize, Vec<Option<LocalWaterSample>>)>;
        let mut observed = Vec::new();
        for segments in [32usize, 64, 128, 256] {
            let side = segments + 1;
            let mut grid = Vec::with_capacity(side * side);
            let mut ids = HashSet::new();
            let mut pools = HashSet::new();
            for y in 0..side {
                for x in 0..side {
                    let p = point(
                        origin[0] + (x as f64 / segments as f64 - 0.5) * 1_000.0,
                        origin[1] + (y as f64 / segments as f64 - 0.5) * 1_000.0,
                        radius,
                    );
                    let s = sample(&world, p, environment(p), environment);
                    if let Some(s) = s.filter(|s| s.lake) {
                        ids.insert(s.id);
                        if (s.id >> 38) & 3 == 3 {
                            pools.insert(s.id);
                        }
                    }
                    grid.push(s);
                }
            }
            if let Some((old_segments, old)) = previous.take() {
                for y in 0..=old_segments {
                    for x in 0..=old_segments {
                        assert_same_sample(
                            old[y * (old_segments + 1) + x],
                            grid[(y * 2) * side + x * 2],
                        );
                    }
                }
            }
            observed.push((ids.len(), pools.len()));
            previous = Some((segments, grid));
        }
        eprintln!("local_water flat 1 km nested lake/pool counts: {observed:?}");
        assert!(
            observed.last().unwrap().1 >= 3,
            "metre samples must resolve several 10–55 m pools"
        );
        assert!(
            observed.last().unwrap().1 > observed.first().unwrap().1,
            "sub-grid pools should be revealed by resolving existing geography"
        );
        // Eviction/reconstruction must preserve the fine field in reverse order.
        clear_caches(world.local_water_cache.get().unwrap(), true);
        let (segments, grid) = previous.unwrap();
        let side = segments + 1;
        for index in (0..grid.len()).step_by(49).rev() {
            // Use the same coordinate expression to make the compared physical
            // position bit-identical, as a renderer's nested vertices are.
            let p = point(
                origin[0] + ((index % side) as f64 / segments as f64 - 0.5) * 1_000.0,
                origin[1] + ((index / side) as f64 / segments as f64 - 0.5) * 1_000.0,
                radius,
            );
            assert_same_sample(grid[index], sample(&world, p, environment(p), environment));
        }
    }

    fn assert_same_sample(a: Option<LocalWaterSample>, b: Option<LocalWaterSample>) {
        match (a, b) {
            (None, None) => {}
            (Some(a), Some(b)) => {
                assert_eq!(a.id, b.id);
                assert_eq!(a.height_m.to_bits(), b.height_m.to_bits());
                assert_eq!(a.level_m.to_bits(), b.level_m.to_bits());
                assert_eq!(a.wetland_weight.to_bits(), b.wetland_weight.to_bits());
                assert_eq!(a.lake, b.lake);
            }
            _ => panic!("the same position changed basin support"),
        }
    }

    #[test]
    fn protected_interior_guard_skips_only_canonically_rejected_basins() {
        let world = world();
        let radius = world.config.radius_m;
        // A spherical half-space has an exact 1-Lipschitz inner distance under
        // the chord metric used by the physical support. The reference caller
        // disables only acceleration, retaining the identical protected field.
        let fields = |p: [f64; 3]| {
            let mut e = environment(p);
            let inner_m = -p[2] * radius;
            if inner_m > 0.0 {
                e.protected_water_weight = 1.0;
                e.protected_inner_m = inner_m;
            }
            e
        };
        let deep = point(-450.0, 0.0, radius);
        assert!(sample(&world, deep, fields(deep), fields).is_none());
        let guarded_stats = cache_stats(&world);
        assert_eq!(guarded_stats.bucket_builds[2..], [0, 0]);
        let mut reference = fields(deep);
        reference.protected_inner_m = 0.0;
        assert_same_sample(
            sample(&world, deep, fields(deep), fields),
            sample(&world, deep, reference, fields),
        );
        // The front cache must include the active scales: the same keys can
        // first be queried from an interior point and then with guard disabled.
        assert!(cache_stats(&world).bucket_builds[2..]
            .iter()
            .all(|&count| count > 0));

        let mut wet = 0;
        for y in -15..=15 {
            for x in -100..=50 {
                let p = point(x as f64 * 50.0, y as f64 * 100.0, radius);
                let current = fields(p);
                let mut reference = current;
                reference.protected_inner_m = 0.0;
                let unguarded = sample(&world, p, reference, fields);
                wet += usize::from(unguarded.is_some_and(|s| s.lake));
                assert_same_sample(unguarded, sample(&world, p, current, fields));
            }
        }
        assert!(
            wet > 20,
            "the comparison must include surviving dry-side lakes"
        );
        for tier in TIERS {
            let margin = 2.0 * radius * (tier.support_m / radius).asin() + 1.0;
            for offset in [-0.0001, 0.0001] {
                let z = -(margin + offset) / radius;
                let p = [(1.0 - z * z).sqrt(), 0.0, z];
                let current = fields(p);
                let mut reference = current;
                reference.protected_inner_m = 0.0;
                assert_same_sample(
                    sample(&world, p, reference, fields),
                    sample(&world, p, current, fields),
                );
            }
        }
    }

    #[test]
    fn thread_front_cache_keeps_worlds_and_geometry_invalidations_separate() {
        let mut a = world();
        let b = world();
        let radius = a.config.radius_m;
        let descriptors =
            Arc::new(LocalWaterAtlas::default()).tier_neighbourhood(&a, point(0.0, 0.0, radius), 0);
        let basin = &descriptors.first().unwrap().basin;
        let anchor = basin.lobes[(basin.lobe_count - 1) / 2];
        let p = basin.direction_at(radius, anchor.x, anchor.y);
        let higher = |p| {
            let mut e = environment(p);
            e.natural_height_m += 90.0;
            e
        };
        let low = sample(&a, p, environment(p), environment);
        let high = sample(&b, p, higher(p), higher);
        assert!((high.unwrap().level_m - low.unwrap().level_m - 90.0).abs() < 1e-9);
        assert_same_sample(low, sample(&a, p, environment(p), environment));
        a.invalidate_geometry();
        assert_same_sample(high, sample(&a, p, higher(p), higher));
    }

    #[test]
    fn all_tier_density_rejects_are_cached_before_any_terrain_callback() {
        let mut world = world();
        world.config.regional_lake_density = 0.0;
        let radius = world.config.radius_m;
        let atlas = Arc::new(LocalWaterAtlas::default());
        let fields = |_| panic!("zero-density candidates must never query the environment");
        for x in [-500.0, 0.0, 500.0] {
            assert!(atlas
                .neighbourhood(&world, point(x, 0.0, radius), &fields)
                .is_empty());
        }
        let mut cached_by_tier = [0usize; 4];
        for shard in &atlas.caches {
            let cache = shard.lock().unwrap();
            for (address, slot) in &cache.proposals {
                assert!(matches!(slot.get(), Some(None)));
                cached_by_tier[address.tier as usize] += 1;
            }
        }
        assert!(cached_by_tier.iter().all(|&count| count > 10));
        clear_caches(&atlas, false);
        assert!(atlas
            .neighbourhood(&world, point(0.0, 0.0, radius), &fields)
            .is_empty());
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
        let radius = baseline.config.radius_m;
        let descriptors = Arc::new(LocalWaterAtlas::default()).tier_neighbourhood(
            &baseline,
            point(0.0, 0.0, radius),
            0,
        );
        let basin = &descriptors
            .first()
            .expect("a coarse descriptor in the flat fixture")
            .basin;
        let anchor = basin.lobes[(basin.lobe_count - 1) / 2];
        let p = basin.direction_at(radius, anchor.x, anchor.y);
        let serial_calls = AtomicUsize::new(0);
        let serial = |p| {
            serial_calls.fetch_add(1, Ordering::Relaxed);
            if let Some(atlas) = baseline.local_water_cache.get() {
                assert!(atlas.caches.iter().all(|shard| shard.try_lock().is_ok()));
            }
            environment(p)
        };
        let _ = sample(&baseline, p, environment(p), serial);
        let expected = serial_calls.load(Ordering::Relaxed);
        assert!(expected > 20);

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
    fn sparse_query_validates_only_objects_whose_physical_support_reaches_it() {
        let world = world();
        let radius = world.config.radius_m;
        let atlas = Arc::new(LocalWaterAtlas::default());
        let coarse = atlas.tier_neighbourhood(&world, point(0.0, 0.0, radius), 0);
        let basin = &coarse.first().unwrap().basin;
        let anchor = basin.lobes[(basin.lobe_count - 1) / 2];
        let p = basin.direction_at(radius, anchor.x, anchor.y);
        let mut candidates = 0;
        let mut touching = 0;
        for tier in 0..TIERS.len() {
            for descriptor in atlas.tier_neighbourhood(&world, p, tier).iter() {
                candidates += 1;
                touching += usize::from(descriptor.basin.shape_at(p, radius).is_some());
            }
        }
        assert!(
            candidates > touching * 4,
            "a sparse query must not need the whole neighborhood"
        );
        world.local_water_cache.set(atlas).unwrap();
        assert!(sample(&world, p, environment(p), environment).is_some());
        let stats = cache_stats(&world);
        assert_eq!(stats.freeze_attempts.iter().sum::<usize>(), touching);
        let calls = stats.environment_calls;
        let _ = sample(&world, p, environment(p), environment);
        assert_eq!(cache_stats(&world).environment_calls, calls);
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
        for shard in &atlas.caches {
            let cache = shard.lock().unwrap();
            assert!(cache
                .proposals
                .values()
                .any(|slot| matches!(slot.get(), Some(None))));
        }
        clear_caches(&atlas, false);
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
