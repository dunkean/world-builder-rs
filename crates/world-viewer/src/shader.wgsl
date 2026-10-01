struct TileClip { bounds: vec4<f32>, face_level: vec4<f32> };
struct Uniforms {
    view_projection: mat4x4<f32>,
    eye: vec4<f32>,
    eye_low: vec4<f32>,
    light: vec4<f32>,
    options: vec4<f32>,
    clips: array<TileClip, 32>,
};
@group(0) @binding(0) var<uniform> scene: Uniforms;

struct VertexIn {
    @location(0) position: vec3<f32>,
    @location(1) position_low: vec3<f32>,
    @location(2) normal_height: vec4<f32>,
    @location(3) color: vec4<f32>,
    @location(4) fields: vec4<f32>,
};
struct VertexOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) position: vec3<f32>,
    @location(1) normal_height: vec4<f32>,
    @location(2) color: vec4<f32>,
    @location(3) fields: vec4<f32>,
};
@vertex fn vs_main(v: VertexIn) -> VertexOut {
    var out: VertexOut;
    let relative = (v.position - scene.eye.xyz) + (v.position_low - scene.eye_low.xyz);
    out.clip = scene.view_projection * vec4<f32>(relative, 1.0);
    out.position = v.position;
    out.normal_height = v.normal_height;
    out.color = v.color;
    out.fields = v.fields;
    return out;
}

fn ramp(x: f32, low: vec3<f32>, high: vec3<f32>) -> vec3<f32> {
    return mix(low, high, clamp(x, 0.0, 1.0));
}

@fragment fn fs_main(v: VertexOut) -> @location(0) vec4<f32> {
    if v.color.a >= 99.0 {
        if v.color.a < 101.0 && scene.options.z < 0.5 { discard; }
        if v.color.a >= 101.0 && scene.options.w < 0.5 { discard; }
        let facing = dot(normalize(v.normal_height.xyz), normalize(scene.eye.xyz - v.position));
        if facing < 0.0 { discard; }
        return vec4(v.color.rgb, 1.0);
    }
    let lod = floor(v.color.a / 2.0);
    let water = fract(v.color.a / 2.0) > 0.25;
    let p = normalize(v.position);
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
    for (var i = 0u; i < u32(scene.options.y); i += 1u) {
        let clip = scene.clips[i];
        if u32(clip.face_level.x) == face && clip.face_level.y > lod && uv.x > clip.bounds.x && uv.x < clip.bounds.z && uv.y > clip.bounds.y && uv.y < clip.bounds.w { discard; }
    }
    if water && v.fields.w <= 0.01 { discard; }
    var color = v.color.rgb;
    let mode = u32(scene.options.x);
    if mode == 1u {
        color = ramp((v.normal_height.w + 2.0) / 8.0, vec3(0.08, 0.20, 0.34), vec3(0.96, 0.91, 0.75));
    } else if mode == 2u {
        color = ramp(v.fields.x / 65.0 + 0.4, vec3(0.09, 0.36, 0.78), vec3(0.95, 0.21, 0.07));
    } else if mode == 3u {
        color = ramp(v.fields.y, vec3(0.72, 0.42, 0.19), vec3(0.04, 0.45, 0.40));
    } else if mode == 4u {
        color = mix(vec3(0.22, 0.25, 0.27), vec3(0.10, 0.77, 0.97), clamp(v.fields.z, 0.0, 1.0));
    }
    var geometric = normalize(cross(dpdx(v.position), dpdy(v.position)));
    let radial = normalize(v.normal_height.xyz);
    if dot(geometric, radial) < 0.0 { geometric = -geometric; }
    let normal = normalize(mix(radial, geometric, 0.70));
    let diffuse = max(0.0, dot(normal, normalize(scene.light.xyz)));
    let ambient = 0.33;
    let light = ambient + 0.72 * diffuse;
    var result = color * light;
    if water {
        let eye_dir = normalize(scene.eye.xyz - v.position);
        let reflection = reflect(-normalize(scene.light.xyz), radial);
        result += vec3(0.55, 0.75, 0.85) * pow(max(0.0, dot(reflection, eye_dir)), 65.0) * 0.55;
    }
    let limb = pow(1.0 - max(0.0, dot(radial, normalize(scene.eye.xyz - v.position))), 5.0);
    result = mix(result, vec3(0.29, 0.52, 0.70), limb * 0.23);
    return vec4(result, 1.0);
}
