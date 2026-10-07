struct TileClip { bounds: vec4<f32>, face_level: vec4<f32> };
struct NoiseBand { cell: vec4<i32>, phase_scale: vec4<f32> };
struct NoiseFrame {
    anchor_high: vec4<f32>,
    anchor_low: vec4<f32>,
    bands: array<NoiseBand, 8>,
};
struct Uniforms {
    view_projection: mat4x4<f32>,
    eye: vec4<f32>,
    eye_low: vec4<f32>,
    light: vec4<f32>,
    options: vec4<f32>,
    cartography: vec4<u32>,
    surface: vec4<f32>,
    noise: NoiseFrame,
    clips: array<TileClip, 32>,
};
@group(0) @binding(0) var<uniform> scene: Uniforms;

struct VertexIn {
    @location(0) position: vec3<f32>,
    @location(1) position_low: vec3<f32>,
    @location(2) normal_height: vec4<f32>,
    @location(3) color: vec4<f32>,
    @location(4) fields: vec4<f32>,
    @location(5) morph: vec4<f32>,
    @location(6) shading_normal: vec3<f32>,
    @location(7) parent_normal: vec3<f32>,
    @location(8) water_depth: f32,
    @location(9) biome_a: vec4<f32>,
    @location(10) biome_b: vec4<f32>,
    @location(11) direction_low: vec3<f32>,
};
struct VertexOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) position: vec3<f32>,
    @location(1) normal_height: vec4<f32>,
    @location(2) color: vec4<f32>,
    @location(3) fields: vec4<f32>,
    @location(4) @interpolate(flat) flags: u32,
    @location(5) shading_normal: vec3<f32>,
    @location(6) surface_delta: vec3<f32>,
    @location(7) biome_a: vec4<f32>,
    @location(8) biome_b: vec4<f32>,
};
@vertex fn vs_main(v: VertexIn) -> VertexOut {
    var out: VertexOut;
    var start=v.morph.y;
    if start<0.0 {let id=u32(-start);let record=scene.clips[id%32u].bounds;start=select(-1e9,record.y,record.x==f32(id));}
    let progress=smoothstep(0.0,MORPH_DURATION_SECONDS,scene.light.w-start);
    let offset=mix(v.morph.x,v.morph.w,progress);
    let relative=(v.position-scene.eye.xyz)+(v.position_low-scene.eye_low.xyz)
        + v.normal_height.xyz*offset + v.direction_low*offset;
    out.clip = scene.view_projection * vec4<f32>(relative, 1.0);
    out.position = relative;
    out.normal_height = v.normal_height;
    // The relief legend describes the morphed physical surface, in kilometres,
    // rather than its exaggeration or the final height before a LOD transition.
    if scene.options.x > 4.5 {
        out.normal_height.w += offset * scene.options.y / (1000.0 * scene.surface.x);
    }
    out.color = v.color;
    out.fields = v.fields;
    out.fields.w=mix(v.morph.z,v.water_depth,progress);
    out.flags = u32(v.color.a + 0.5);
    out.shading_normal = normalize(mix(v.parent_normal, v.shading_normal, progress));
    // Interpolate the same morphed physical position used by clip-space.
    // The split camera anchor keeps planetary coordinates out of f32 sums.
    out.surface_delta = ((v.position - scene.noise.anchor_high.xyz)
        + (v.position_low - scene.noise.anchor_low.xyz)
        + v.normal_height.xyz * offset + v.direction_low * offset) * scene.options.y;
    out.biome_a = v.biome_a;
    out.biome_b = v.biome_b;
    return out;
}

fn ramp(x: f32, low: vec3<f32>, high: vec3<f32>) -> vec3<f32> {
    return mix(low, high, clamp(x, 0.0, 1.0));
}

fn relief_palette(height_m: f32) -> vec3<f32> {
    // Fixed metre stops are also printed in the HTML legend. Hillshade and
    // steep-slope accents supplement height; no biome/material noise is used.
    let low = vec3(0.92, 0.76, 0.53);
    let orange = vec3(0.88, 0.34, 0.12);
    let red = vec3(0.65, 0.12, 0.085);
    let summit = vec3(0.98, 0.88, 0.76);
    if height_m < 0.0 { return ramp((height_m + 6000.0) / 6000.0, vec3(0.025, 0.095, 0.16), low); }
    if height_m < 500.0 { return ramp(height_m / 500.0, low, orange); }
    if height_m < 2000.0 { return ramp((height_m - 500.0) / 1500.0, orange, red); }
    return ramp((height_m - 2000.0) / 4000.0, red, summit);
}

fn colored_relief(v: VertexOut, radial: vec3<f32>, water: bool) -> vec3<f32> {
    if water { return vec3(0.035, 0.20, 0.30); }
    let height_m = v.normal_height.w * 1000.0;
    let normal = normalize(v.shading_normal);
    let cosine = clamp(dot(normal, radial), 0.0001, 1.0);
    let correction = (scene.options.y + scene.surface.x * height_m)
        / max(1.0, scene.options.y + height_m);
    // Undo vertical exaggeration for physical slope, as surface_probe does.
    let tangent = (normal - radial * cosine) * correction
        / (cosine * max(1.0, scene.surface.x));
    let physical_normal = normalize(radial + tangent);
    let slope_degrees = atan(length(tangent)) * 57.2957795;
    var light_tangent = vec3(0.4, 0.8, -0.5) - radial * dot(radial, vec3(0.4, 0.8, -0.5));
    if dot(light_tangent, light_tangent) < 0.0001 { light_tangent = cross(radial, vec3(1.0, 0.0, 0.0)); }
    let light = normalize(radial * 0.55 + normalize(light_tangent) * 0.84);
    let departure = dot(physical_normal, light) - dot(radial, light);
    let shade = clamp(0.88 + 3.2 * departure, 0.24, 1.38);
    var color = relief_palette(height_m);
    color = mix(color, vec3(0.68, 0.065, 0.035), 0.45 * smoothstep(20.0, 55.0, slope_degrees));
    color *= shade;
    return mix(color, vec3(0.99, 0.92, 0.78), clamp(departure * 1.2, 0.0, 0.38));
}

fn ink_hash(value: u32) -> u32 {
    var h = (value ^ (value >> 16u)) * 0x7feb352du;
    h = (h ^ (h >> 15u)) * 0x846ca68bu;
    return h ^ (h >> 16u);
}

fn ink_gradient(hash: u32, displacement: vec3<f32>) -> f32 {
    // The twelve equally long cube-edge gradients avoid constant-value
    // plateaus around lattice corners, which looked like square clearings.
    let index = hash % 12u;
    let u = select(displacement.x, displacement.y, index >= 8u);
    let v = select(displacement.z, displacement.y, index < 4u);
    return (select(u, -u, (index & 1u) != 0u)
        + select(v, -v, (index & 2u) != 0u)) * 0.707106781;
}

fn cell_ink(cell: vec3<i32>, displacement: vec3<f32>, seed: u32) -> vec3<f32> {
    let lattice = bitcast<vec3<u32>>(cell);
    let h = ink_hash((lattice.x * 0x8da6b343u) ^ (lattice.y * 0xd8163841u)
        ^ (lattice.z * 0xcb1ab31fu) ^ seed);
    return vec3<f32>(ink_gradient(h, displacement),
        ink_gradient(ink_hash(h ^ 0x68bc21ebu), displacement),
        ink_gradient(ink_hash(h ^ 0x02e5be93u), displacement));
}

// A three-dimensional field on the sphere, not UVs on individual cube faces.
// Gradient noise and quintic interpolation keep cell boundaries C2 without
// a texture lookup. The split anchor still addresses the same world corners.
fn map_noise(q: vec3<f32>, origin: vec3<i32>, seed: u32) -> vec3<f32> {
    let base = origin + vec3<i32>(floor(q));
    let f = fract(q);
    let t = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
    let low = mix(
        mix(cell_ink(base, f, seed), cell_ink(base + vec3<i32>(1, 0, 0), f - vec3(1.0, 0.0, 0.0), seed), t.x),
        mix(cell_ink(base + vec3<i32>(0, 1, 0), f - vec3(0.0, 1.0, 0.0), seed), cell_ink(base + vec3<i32>(1, 1, 0), f - vec3(1.0, 1.0, 0.0), seed), t.x), t.y);
    let high = mix(
        mix(cell_ink(base + vec3<i32>(0, 0, 1), f - vec3(0.0, 0.0, 1.0), seed), cell_ink(base + vec3<i32>(1, 0, 1), f - vec3(1.0, 0.0, 1.0), seed), t.x),
        mix(cell_ink(base + vec3<i32>(0, 1, 1), f - vec3(0.0, 1.0, 1.0), seed), cell_ink(base + vec3<i32>(1, 1, 1), f - vec3(1.0, 1.0, 1.0), seed), t.x), t.y);
    return mix(low, high, t.z) * 1.8;
}

fn noise_axes(p: vec3<f32>) -> vec3<f32> {
    return vec3<f32>(dot(p, vec3(0.36, 0.48, 0.8)),
                     dot(p, vec3(-0.8, 0.6, 0.0)),
                     dot(p, vec3(-0.48, -0.64, 0.6)));
}

fn surface_delta(delta: vec3<f32>) -> vec3<f32> {
    // R*(normalize(physical_point)-normalize(anchor)), without subtracting
    // planetary f32 positions. This also preserves phase on sloping chords.
    let inverse_length = scene.noise.anchor_high.w;
    let anchor = (scene.noise.anchor_high.xyz + scene.noise.anchor_low.xyz) * inverse_length;
    let inv_radius = inverse_length / scene.options.y;
    let z = 2.0 * dot(anchor, delta) + dot(delta, delta) * inv_radius;
    let norm = sqrt(max(1e-12, 1.0 + z * inv_radius));
    return (delta - anchor * (z / (norm + 1.0))) * (inverse_length / norm);
}

// Two small scalar bands avoid expanding the six original three-channel
// domain-warped bands into eight copies in WebGL shader translation.
fn scalar_corner(lattice: vec3<i32>, displacement: vec3<f32>, seed: u32) -> f32 {
    let p = bitcast<vec3<u32>>(lattice);
    let h = ink_hash((p.x * 0x8da6b343u) ^ (p.y * 0xd8163841u) ^ (p.z * 0xcb1ab31fu) ^ seed);
    return ink_gradient(h, displacement);
}
fn scalar_noise(q: vec3<f32>, origin: vec3<i32>, seed: u32) -> f32 {
    let base = origin + vec3<i32>(floor(q));
    let f = fract(q);
    let t = f * f * f * (f * (f * 6.0 - 15.0) + 10.0);
    let a = mix(mix(scalar_corner(base, f, seed), scalar_corner(base + vec3<i32>(1,0,0), f-vec3(1.0,0.0,0.0), seed), t.x),
        mix(scalar_corner(base + vec3<i32>(0,1,0), f-vec3(0.0,1.0,0.0), seed), scalar_corner(base+vec3<i32>(1,1,0),f-vec3(1.0,1.0,0.0),seed),t.x),t.y);
    let b = mix(mix(scalar_corner(base+vec3<i32>(0,0,1),f-vec3(0.0,0.0,1.0),seed), scalar_corner(base+vec3<i32>(1,0,1),f-vec3(1.0,0.0,1.0),seed),t.x),
        mix(scalar_corner(base+vec3<i32>(0,1,1),f-vec3(0.0,1.0,1.0),seed), scalar_corner(base+vec3<i32>(1,1,1),f-vec3(1.0,1.0,1.0),seed),t.x),t.y);
    return mix(a,b,t.z)*1.8;
}

struct Facies { regional: vec3<f32>, local: vec3<f32>, fine: vec3<f32>, grain: f32 };

fn map_facies(delta: vec3<f32>, footprint_m: f32, band_count: u32) -> Facies {
    let q = noise_axes(delta);
    let seed = scene.cartography.x ^ (scene.cartography.y * 0x9e3779b9u);
    let regional_weight = array<f32, 6>(0.55, 0.45, 0.28, 0.15, 0.09, 0.05);
    let local_weight = array<f32, 6>(0.0, 0.0, 0.35, 0.40, 0.32, 0.24);
    let fine_weight = array<f32, 6>(0.0, 0.0, 0.0, 0.28, 0.38, 0.48);
    var field = Facies(vec3(0.0), vec3(0.0), vec3(0.0), 0.0);
    var warp = vec3(0.0);
    for (var band = 0u; band < band_count; band += 1u) {
        let record = scene.noise.bands[band];
        let metres = record.phase_scale.w;
        // A frequency appears continuously once it covers several pixels.
        // The domain warp uses the unfiltered parent field: zoom never shifts
        // an already visible patch by changing the next octave's coordinates.
        if footprint_m < metres * 0.5 {
            let raw = map_noise(q / metres + record.phase_scale.xyz + warp,
                record.cell.xyz, seed ^ (0x713ab51du + (band * 0x318f402bu)));
            let visible = 1.0 - smoothstep(metres * 0.125, metres * 0.5, footprint_m);
            field.regional += raw * (regional_weight[band] * visible);
            field.local += raw * (local_weight[band] * visible);
            field.fine += raw * (fine_weight[band] * visible);
            warp = raw.yzx * 0.72;
        }
    }
    if band_count == 6u {
        let mid = scene.noise.bands[6];
        if footprint_m < 0.25 {
            let raw = scalar_noise(q / 0.5 + mid.phase_scale.xyz, mid.cell.xyz, seed ^ 0x121475cdu);
            let visible = 1.0 - smoothstep(0.0625, 0.25, footprint_m);
            field.local += vec3(raw, raw * 0.7, -raw * 0.45) * (0.10 * visible);
            field.fine += vec3(raw, raw * 0.7, -raw * 0.45) * (0.16 * visible);
            field.grain += raw * (0.05 * visible);
        }
        let micro = scene.noise.bands[7];
        if footprint_m < 0.1 {
            let raw = scalar_noise(q / 0.2 + micro.phase_scale.xyz, micro.cell.xyz, seed ^ 0x872bf623u);
            let visible = 1.0 - smoothstep(0.025, 0.1, footprint_m);
            field.local += vec3(raw, -raw * 0.5, raw * 0.8) * (0.05 * visible);
            field.fine += vec3(raw, -raw * 0.5, raw * 0.8) * (0.12 * visible);
            field.grain += raw * (0.14 * visible);
            if footprint_m < 0.0125 {
                // Small material grains remain readable when the camera is
                // much closer than the 20cm terrain sampling. Derive their
                // world lattice modulo32 from the same anchored band.
                let origin = bitcast<vec3<i32>>(
                    bitcast<vec3<u32>>(micro.cell.xyz) * vec3<u32>(8u));
                let raw_grain = scalar_noise((q / 0.2 + micro.phase_scale.xyz) * 8.0,
                    origin, seed ^ 0x183bc409u);
                let grain_visible = 1.0 - smoothstep(0.003125, 0.0125, footprint_m);
                field.grain += raw_grain * (0.24 * grain_visible);
            }
        }
    }
    return field;
}

fn biome_bias(index: u32, patches: vec3<f32>) -> f32 {
    switch index {
        case 0u: { return dot(patches, vec3(0.95, 0.0, 0.40)); }
        case 1u: { return dot(patches, vec3(-1.0, 0.55, 0.0)); }
        case 2u: { return dot(patches, vec3(0.0, 0.95, 0.45)); }
        case 3u: { return dot(patches, vec3(0.65, -1.0, 0.0)); }
        case 4u: { return dot(patches, vec3(-0.55, 0.0, 0.95)); }
        case 5u: { return dot(patches, vec3(0.0, 0.45, -1.0)); }
        case 6u: { return dot(patches, vec3(0.0, -0.50, -0.40)); }
        default: { return dot(patches, vec3(0.70, -0.55, 0.0)); }
    }
}

fn biome_facies(index: u32, temperature: f32, moisture: f32, slope: f32, field: Facies) -> vec3<f32> {
    let region = smoothstep(-0.36, 0.36, field.regional.x + field.local.y * 0.55);
    let mosaic = smoothstep(-0.30, 0.30, field.regional.y + field.local.x * 0.75);
    let fine = smoothstep(-0.20, 0.20, field.fine.z + field.local.z * 0.25);
    let wet = clamp(moisture + field.regional.z * 0.18 + field.local.y * 0.14, 0.0, 1.0);
    let exposed = smoothstep(25.0, 55.0, slope);
    let tree_cover = pow(1.0 - smoothstep(20.0, 55.0, slope), 2.0);
    let snow_cover = pow(1.0 - smoothstep(25.0, 60.0, slope), 2.0);
    let bare_rock = smoothstep(40.0, 65.0, slope);
    let stone = mix(vec3(0.24, 0.26, 0.25), vec3(0.51, 0.44, 0.33), region);
    var ink = stone;
    switch index {
        case 0u: {
            // Ice remains ice in its cold interior. Warm margins and real
            // steep faces expose debris; no green vegetation is painted here.
            let snow = mix(vec3(0.65, 0.75, 0.76), vec3(0.86, 0.88, 0.83), fine);
            let thaw = smoothstep(-15.0, 0.0, temperature);
            let debris = clamp(exposed * 0.55 + thaw * mosaic * 0.42, 0.0, 0.72);
            ink = mix(mix(vec3(0.30, 0.32, 0.29), stone, fine), snow, (1.0-debris)*snow_cover);
        }
        case 1u: {
            let moss = mix(vec3(0.22, 0.29, 0.18), vec3(0.43, 0.43, 0.25), fine);
            let soil = mix(vec3(0.27, 0.26, 0.21), vec3(0.42, 0.37, 0.28), region);
            ink = mix(soil, moss, clamp(wet * 0.65 + mosaic * 0.50, 0.08, 0.93));
            ink = mix(ink, stone, exposed * 0.62 + (1.0 - mosaic) * 0.15);
        }
        case 2u: {
            let sand = mix(vec3(0.55, 0.37, 0.20), vec3(0.75, 0.61, 0.39), fine);
            let ochre = mix(vec3(0.43, 0.34, 0.25), vec3(0.67, 0.49, 0.29), region);
            ink = mix(ochre, sand, mosaic * 0.76 + 0.12);
            ink = mix(ink, stone, exposed * 0.64 + (1.0 - mosaic) * 0.17);
        }
        case 3u: {
            let grass = mix(vec3(0.20, 0.33, 0.12), vec3(0.41, 0.47, 0.21), fine);
            let dry = mix(vec3(0.41, 0.35, 0.19), vec3(0.56, 0.46, 0.27), region);
            ink = mix(dry, grass, clamp(wet * 0.82 + mosaic * 0.53, 0.05, 0.97));
            ink = mix(ink, stone, exposed * 0.55 + (1.0 - mosaic) * 0.08);
        }
        case 4u: {
            let canopy = mix(vec3(0.045, 0.13, 0.065), vec3(0.19, 0.30, 0.13), fine);
            let glade = mix(vec3(0.25, 0.36, 0.145), vec3(0.43, 0.50, 0.23), region);
            let cover = clamp(0.60 + wet * 0.32 + field.regional.y * 0.34 + field.local.x * 0.28, 0.34, 0.98);
            ink = mix(glade, canopy, cover * tree_cover);
            ink = mix(ink, stone, exposed * 0.70);
        }
        case 5u: {
            let canopy = mix(vec3(0.028, 0.105, 0.06), vec3(0.13, 0.25, 0.11), fine);
            let clearing = mix(vec3(0.18, 0.25, 0.12), vec3(0.32, 0.32, 0.17), region);
            let cover = clamp(0.84 + field.regional.y * 0.24 + field.local.x * 0.20, 0.60, 0.99);
            ink = mix(clearing, canopy, cover * tree_cover);
            ink = mix(ink, stone, exposed * 0.70);
        }
        case 6u: {
            let reeds = mix(vec3(0.17, 0.28, 0.15), vec3(0.36, 0.37, 0.22), fine);
            let peat = mix(vec3(0.075, 0.14, 0.11), vec3(0.24, 0.26, 0.16), region);
            ink = mix(peat, reeds, mosaic * 0.75 + 0.16);
            ink = mix(ink, stone, exposed * 0.65);
        }
        default: {
            let mineral = mix(vec3(0.25, 0.27, 0.26), vec3(0.52, 0.43, 0.31), fine);
            let lichen = mix(vec3(0.23, 0.28, 0.18), vec3(0.39, 0.38, 0.24), region);
            let growth = wet * smoothstep(-10.0, 5.0, temperature) * (1.0 - exposed) * mosaic * 0.32;
            ink = mix(mix(stone, mineral, 0.62), lichen, growth);
        }
    }
    // Every vertical face is exposed mineral. Neither a biome's broad
    // fraction nor its fine pattern can leave trees or snow on that face.
    return mix(ink, stone, bare_rock);
}

fn physical_slope_degrees(v: VertexOut, radial: vec3<f32>) -> f32 {
    let cosine = clamp(dot(normalize(v.shading_normal), radial), 0.0001, 1.0);
    let rendered_tangent = sqrt(max(0.0, 1.0-cosine*cosine)) / cosine;
    let exaggeration = max(1.0, scene.surface.x);
    let height = v.normal_height.w * 1000.0;
    let radial_correction = max(1e-6, scene.options.y + exaggeration*height)
        / max(1e-6, scene.options.y+height);
    return atan(rendered_tangent * radial_correction / exaggeration) * 57.295779513;
}

fn land_ink(v: VertexOut, radial: vec3<f32>, field: Facies) -> vec3<f32> {
    let fractions = array<f32, 8>(v.biome_a.x, v.biome_a.y, v.biome_a.z, v.biome_a.w,
        v.biome_b.x, v.biome_b.y, v.biome_b.z, v.biome_b.w);
    let slope = physical_slope_degrees(v, radial);
    let patches = field.regional * 2.4 + field.local * 1.2;
    var ink = vec3(0.0);
    var total = 0.0;
    for (var biome = 0u; biome < 8u; biome += 1u) {
        if fractions[biome] > 0.0 {
            // Fixed, softly competing patches break an ecotone into fingers
            // and islands. Only terrestrial fractions enter this competition.
            let weight = pow(fractions[biome], 1.35) * exp(clamp(biome_bias(biome, patches), -6.0, 6.0));
            ink += biome_facies(biome, v.fields.x, v.fields.y, slope, field) * weight;
            total += weight;
        }
    }
    return ink / max(total, 1e-12);
}

fn water_ink(depth: f32, style: f32, marine: bool, grain: f32) -> vec3<f32> {
    let d = max(0.0, depth);
    if marine {
        // Bathymetry, including a shallow shelf, reads as a continuous map.
        let lagoon = vec3(0.18, 0.47, 0.46);
        let shelf = vec3(0.055, 0.27, 0.36);
        let deep = vec3(0.015, 0.09, 0.17);
        var ink = mix(lagoon, shelf, 1.0 - exp(-d / 35.0));
        ink = mix(ink, deep, 1.0 - exp(-d / 900.0));
        let shore = 1.0 - smoothstep(0.0, 1.6, d);
        ink = mix(ink, vec3(0.53, 0.64, 0.56), shore * 0.12);
        return ink * (1.0 + grain * 0.04);
    }
    let river = style < -1.5;
    let shallow = select(vec3(0.09, 0.34, 0.31), vec3(0.055, 0.33, 0.40), river);
    let deep = select(vec3(0.035, 0.17, 0.23), vec3(0.030, 0.21, 0.32), river);
    return mix(shallow, deep, 1.0 - exp(-d / select(6.0, 2.5, river))) * (1.0 + grain * 0.025);
}

@fragment fn fs_main(v: VertexOut) -> @location(0) vec4<f32> {
    let radial = normalize(v.normal_height.xyz);
    // Derivatives precede every divergent branch/discard, also on WebGL2.
    let ink_delta = surface_delta(v.surface_delta);
    let footprint_m = max(length(dpdx(ink_delta)), length(dpdy(ink_delta)));
    if v.flags >= 99u {
        if v.flags < 101u && scene.options.z < 0.5 { discard; }
        if v.flags < 101u && scene.eye.w < 10000.0 { discard; }
        if v.flags >= 101u && scene.options.w < 0.5 { discard; }
        let facing = dot(normalize(v.normal_height.xyz), normalize(-v.position));
        if facing < 0.0 { discard; }
        return vec4(v.color.rgb, 1.0);
    }
    let water = (v.flags & 1u) == 1u;
    let p = normalize(v.normal_height.xyz);
    let a = abs(p);
    var face = 0u;
    var uv = vec2<f32>(-p.z, p.y) / p.x;
    if a.x >= a.y && a.x >= a.z {
        if p.x < 0.0 { face = 1u; uv = vec2<f32>(p.z, p.y) / -p.x; }
    } else if a.y >= a.z {
        if p.y > 0.0 { face = 2u; uv = vec2<f32>(p.x, -p.z) / p.y; }
        else { face = 3u; uv = vec2<f32>(p.x, p.z) / -p.y; }
    } else {
        if p.z > 0.0 { face = 4u; uv = vec2<f32>(p.x, p.y) / p.z; }
        else { face = 5u; uv = vec2<f32>(-p.x, p.y) / -p.z; }
    }
    // Keep the actual shoreline even on almost flat beaches. A fixed depth
    // cutoff shifts it by metres and hides the sea during close zooms.
    if water && v.fields.w <= 0.0 { discard; }
    if water && v.fields.z < -1.5 && scene.options.z < 0.5 { discard; }
    var color = v.color.rgb;
    let mode = u32(scene.options.x);
    if mode == 5u { return vec4(colored_relief(v, radial, water), 1.0); }
    if mode == 0u {
        let facies = map_facies(ink_delta, footprint_m, select(6u, 3u, water));
        if water {
            // Existing hydraulic/LOD markers stay unchanged. Marine ink is
            // unambiguously bluer than freshwater ink, including legacy tiles.
            let marine = (v.fields.z > -1.5 && v.fields.z < -0.5)
                || (v.fields.z >= -1.5 && v.color.b > v.color.g * 1.3);
            color = water_ink(v.fields.w, v.fields.z, marine, facies.regional.x * 0.5);
        } else {
            color = land_ink(v, radial, facies);
            // Fine albedo remains visible inside uniform biome patches, where
            // the broader smoothstep transitions have already saturated.
            color *= clamp(1.0 + facies.grain, 0.80, 1.20);
        }
    } else if mode == 1u {
        color = ramp((v.normal_height.w + 2.0) / 8.0, vec3(0.08, 0.20, 0.34), vec3(0.96, 0.91, 0.75));
    } else if mode == 2u {
        color = ramp(v.fields.x / 65.0 + 0.4, vec3(0.09, 0.36, 0.78), vec3(0.95, 0.21, 0.07));
    } else if mode == 3u {
        color = ramp(v.fields.y, vec3(0.72, 0.42, 0.19), vec3(0.04, 0.45, 0.40));
    } else if mode == 4u {
        color = mix(vec3(0.22, 0.25, 0.27), vec3(0.10, 0.77, 0.97), clamp(v.fields.z, 0.0, 1.0));
    }
    let normal = normalize(v.shading_normal);
    let diffuse = max(0.0, dot(normal, normalize(scene.light.xyz)));
    // Restrained hillshade keeps the map legible on steep and unlit slopes.
    let light = select(0.58 + 0.44 * diffuse, 0.82 + 0.18 * diffuse, water);
    var result = color * light;
    if !water {
        // Contrast the real surface slope relative to the local planetary normal.
        let slope_light = diffuse - max(0.0, dot(radial, normalize(scene.light.xyz)));
        result *= clamp(1.0 + 0.85 * slope_light, 0.82, 1.16);
    }
    if water {
        let eye_dir = normalize(-v.position);
        let reflection = reflect(-normalize(scene.light.xyz), radial);
        result += vec3(0.35, 0.48, 0.49) * pow(max(0.0, dot(reflection, eye_dir)), 55.0) * 0.10;
    }
    let limb = pow(1.0 - max(0.0, dot(radial, normalize(-v.position))), 5.0);
    result = mix(result, vec3(0.29, 0.43, 0.51), limb * 0.10);
    return vec4(result, 1.0);
}
