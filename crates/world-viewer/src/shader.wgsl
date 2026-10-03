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
    @location(5) morph: vec4<f32>,
    @location(6) shading_normal: vec3<f32>,
    @location(7) parent_normal: vec3<f32>,
    @location(8) water_depth: f32,
};
struct VertexOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) position: vec3<f32>,
    @location(1) normal_height: vec4<f32>,
    @location(2) color: vec4<f32>,
    @location(3) fields: vec4<f32>,
    @location(4) @interpolate(flat) flags: u32,
    @location(5) shading_normal: vec3<f32>,
};
@vertex fn vs_main(v: VertexIn) -> VertexOut {
    var out: VertexOut;
    var start=v.morph.y;
    if start<0.0 {let id=u32(-start);let record=scene.clips[id%32u].bounds;start=select(-1e9,record.y,record.x==f32(id));}
    let progress=smoothstep(0.0,MORPH_DURATION_SECONDS,scene.light.w-start);
    let offset=mix(v.morph.x,v.morph.w,progress);
    let relative=(v.position-scene.eye.xyz)+(v.position_low-scene.eye_low.xyz)+v.normal_height.xyz*offset;
    out.clip = scene.view_projection * vec4<f32>(relative, 1.0);
    out.position = relative;
    out.normal_height = v.normal_height;
    out.color = v.color;
    out.fields = v.fields;
    out.fields.w=mix(v.morph.z,v.water_depth,progress);
    out.flags = u32(v.color.a + 0.5);
    out.shading_normal = normalize(mix(v.parent_normal, v.shading_normal, progress));
    return out;
}

fn ramp(x: f32, low: vec3<f32>, high: vec3<f32>) -> vec3<f32> {
    return mix(low, high, clamp(x, 0.0, 1.0));
}

@fragment fn fs_main(v: VertexOut) -> @location(0) vec4<f32> {
    if v.flags >= 99u {
        if v.flags < 101u && scene.options.z < 0.5 { discard; }
        if v.flags < 101u && scene.eye.w < 10000.0 { discard; }
        if v.flags >= 101u && scene.options.w < 0.5 { discard; }
        let facing = dot(normalize(v.normal_height.xyz), normalize(-v.position));
        if facing < 0.0 { discard; }
        return vec4(v.color.rgb, 1.0);
    }
    let lod = f32(v.flags / 2u);
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
    if water && v.fields.w <= 0.01 { discard; }
    if water && v.fields.z < -1.5 && scene.options.z < 0.5 { discard; }
    var color = v.color.rgb;
    if scene.options.x < 0.5 && lod > 0.0 && scene.options.z > 0.5 {
        color = mix(color, vec3(0.05, 0.34, 0.44), smoothstep(0.6, 0.9, v.fields.z));
    }
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
    let radial = normalize(v.normal_height.xyz);
    let normal = normalize(v.shading_normal);
    let diffuse = max(0.0, dot(normal, normalize(scene.light.xyz)));
    let ambient = 0.33;
    let light = ambient + 0.72 * diffuse;
    var result = color * light;
    if !water {
        // Contrast the real surface slope relative to the local planetary normal.
        let slope_light = diffuse - max(0.0, dot(radial, normalize(scene.light.xyz)));
        result *= clamp(1.0 + 2.5 * slope_light, 0.65, 1.25);
    }
    if water {
        let eye_dir = normalize(-v.position);
        let reflection = reflect(-normalize(scene.light.xyz), radial);
        result += vec3(0.55, 0.75, 0.85) * pow(max(0.0, dot(reflection, eye_dir)), 65.0) * 0.55;
    }
    let limb = pow(1.0 - max(0.0, dot(radial, normalize(-v.position))), 5.0);
    result = mix(result, vec3(0.29, 0.52, 0.70), limb * 0.23);
    return vec4(result, 1.0);
}
