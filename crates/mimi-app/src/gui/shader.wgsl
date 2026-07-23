// mimi cell shader: instanced quads. `vs_bg`/`fs_bg` fill cell backgrounds,
// `vs_glyph`/`fs_glyph` blend glyphs from the R8 atlas.

struct Globals {
    screen: vec2<f32>,
    _pad: vec2<f32>,
};

@group(0) @binding(0) var<uniform> globals: Globals;
@group(1) @binding(0) var atlas_tex: texture_2d<f32>;
@group(1) @binding(1) var atlas_samp: sampler;

fn corner(vi: u32) -> vec2<f32> {
    var corners = array<vec2<f32>, 6>(
        vec2<f32>(0.0, 0.0), vec2<f32>(1.0, 0.0), vec2<f32>(0.0, 1.0),
        vec2<f32>(0.0, 1.0), vec2<f32>(1.0, 0.0), vec2<f32>(1.0, 1.0),
    );
    return corners[vi];
}

fn to_clip(px: vec2<f32>) -> vec4<f32> {
    let ndc = vec2<f32>(
        px.x / globals.screen.x * 2.0 - 1.0,
        1.0 - px.y / globals.screen.y * 2.0,
    );
    return vec4<f32>(ndc, 0.0, 1.0);
}

// ---- Backgrounds ------------------------------------------------------------

struct BgIn {
    @location(0) pos: vec2<f32>,
    @location(1) size: vec2<f32>,
    @location(2) color: vec4<f32>,
};

struct BgOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_bg(@builtin(vertex_index) vi: u32, in: BgIn) -> BgOut {
    var out: BgOut;
    out.clip = to_clip(in.pos + corner(vi) * in.size);
    out.color = in.color;
    return out;
}

@fragment
fn fs_bg(in: BgOut) -> @location(0) vec4<f32> {
    return in.color;
}

// ---- Glyphs -----------------------------------------------------------------

struct GlyphIn {
    @location(0) pos: vec2<f32>,
    @location(1) size: vec2<f32>,
    @location(2) uv_pos: vec2<f32>,
    @location(3) uv_size: vec2<f32>,
    @location(4) color: vec4<f32>,
};

struct GlyphOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@vertex
fn vs_glyph(@builtin(vertex_index) vi: u32, in: GlyphIn) -> GlyphOut {
    let c = corner(vi);
    var out: GlyphOut;
    out.clip = to_clip(in.pos + c * in.size);
    out.uv = in.uv_pos + c * in.uv_size;
    out.color = in.color;
    return out;
}

@fragment
fn fs_glyph(in: GlyphOut) -> @location(0) vec4<f32> {
    let coverage = textureSample(atlas_tex, atlas_samp, in.uv).r;
    return vec4<f32>(in.color.rgb, in.color.a * coverage);
}
