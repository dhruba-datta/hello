#include <metal_stdlib>
using namespace metal;

struct Uniforms {
    float2 resolution;
    float time;
    float width;
    float height;
    float opacity;
    float scale;
    float softness;
    float average;
    float peak;
    float processing;
    float post_processing;
    float capturing;
    float queued_count;
    float line_style;
    float line_count;
    float line_curvature;
    float line_speed;
    float line_sharpness;
    float line_glow;
    float sphere_depth;
    float light_angle;
    float sphere_outline;
    float completion;
    float recording_flash;
    float indicator_style;
};

struct VertexOutput {
    float4 position [[position]];
    float2 uv;
};

vertex VertexOutput indicator_vertex(uint vertex_id [[vertex_id]]) {
    const float2 positions[] = {
        float2(-1.0, -1.0),
        float2( 3.0, -1.0),
        float2(-1.0,  3.0),
    };
    VertexOutput output;
    output.position = float4(positions[vertex_id], 0.0, 1.0);
    output.uv = positions[vertex_id] * float2(0.5, -0.5) + 0.5;
    return output;
}

float rounded_box(float2 point, float2 half_size, float radius) {
    float2 q = abs(point) - half_size + radius;
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - radius;
}

float coverage(float distance, float softness) {
    return smoothstep(softness, -softness, distance);
}

float glow(float distance, float radius) {
    float outside = max(distance, 0.0);
    return exp2(-outside * outside / max(radius * radius, 0.001));
}

void composite(thread float4 &destination, float3 color, float alpha) {
    alpha = clamp(alpha, 0.0, 1.0);
    destination.rgb = color * alpha + destination.rgb * (1.0 - alpha);
    destination.a = alpha + destination.a * (1.0 - alpha);
}

void screen(thread float4 &destination, float3 color, float amount) {
    amount = clamp(amount, 0.0, 1.0);
    destination.rgb = 1.0 - (1.0 - destination.rgb) * (1.0 - color * amount);
}

// Pending jobs sit beside the foreground state. Keeping this geometry
// stationary prevents queue status from reading as part of the HUD.
void draw_queue(thread float4 &result, float2 point, constant Uniforms &uniforms) {
    float post_processing = clamp(uniforms.post_processing, 0.0, 1.0);
    float3 violet_accent = float3(0.64, 0.2, 1.0);
    float queued_count = min(uniforms.queued_count, 4.0);
    for (int index = 0; index < 4; index++) {
        if (float(index) >= queued_count) {
            break;
        }
        float2 center = float2(
            uniforms.width * 0.5 + 10.0 + float(index) * 3.4,
            0.0);
        float dot_distance = length(point - center) - 0.68;
        float dot = coverage(dot_distance, 0.22);
        float leading_processing = index == 0
            ? post_processing * clamp(uniforms.capturing, 0.0, 1.0)
            : 0.0;
        float3 queued_blue = float3(0.12, 0.58, 1.0);
        float3 dot_color = mix(queued_blue, violet_accent, leading_processing);
        composite(result, dot_color, dot * (index == 0 ? 0.92 : 0.52));
    }
}

// Loudness is perceptual, so lift quiet and distant speech with a square-root
// curve. A small gate keeps room noise from twitching the content.
float voice_level(constant Uniforms &uniforms) {
    float raw_level = clamp(max(uniforms.average * 1.15, uniforms.peak), 0.0, 1.0);
    return sqrt(max(raw_level - 0.02, 0.0) / 0.98);
}

// White while dictating, lavender while post-processing.
float3 content_color(constant Uniforms &uniforms) {
    return mix(float3(1.0), float3(0.86, 0.78, 1.0), clamp(uniforms.post_processing, 0.0, 1.0) * 0.7);
}

// A highlight that sweeps left to right across the pill while transcribing.
float shimmer(float x, float half_width, float time) {
    float sweep = mix(-half_width - 8.0, half_width + 8.0, fract(time * 0.85));
    float offset = (x - sweep) / 7.0;
    return exp2(-offset * offset);
}

// Wave: bars that follow the voice, collapsing into dots with a shimmer
// sweep while transcribing.
void draw_wave(thread float4 &result, float2 point, float shape, float clarity,
               constant Uniforms &uniforms) {
    const float bar_count = 9.0;
    const float bar_width = 2.4;
    const float bar_pitch = 5.0;
    float processing = clamp(uniforms.processing, 0.0, 1.0);
    float half_slots = (bar_count - 1.0) * 0.5;
    float slot = clamp(round(point.x / bar_pitch), -half_slots, half_slots);
    float envelope = 1.0 - abs(slot) / half_slots * 0.55;
    float time = uniforms.time;
    float level = voice_level(uniforms);
    float jitter = 0.6 + 0.4 * sin(time * 14.0 + slot * 1.7);
    float idle = 0.1 + 0.05 * sin(time * 3.0 + slot);
    float speaking = max(idle, level * envelope * jitter);
    float glint = shimmer(slot * bar_pitch, uniforms.width * 0.5, time);
    float thinking = 0.02 + 0.16 * glint;
    float amount = mix(speaking, thinking, processing);
    float min_height = 2.4;
    float max_height = uniforms.height - 8.0;
    float bar_height = min_height + (max_height - min_height) * amount;
    float bar_distance = rounded_box(
        point - float2(slot * bar_pitch, 0.0),
        float2(bar_width * 0.5, bar_height * 0.5),
        bar_width * 0.5);
    float bar = coverage(bar_distance, max(fwidth(bar_distance), 0.3)) * shape;
    float brightness = mix(0.96, 0.3 + 0.7 * glint, processing);
    composite(result, content_color(uniforms), bar * brightness * clarity);
}

// Orb: a color-shifting glow that swells with the voice; a spinning arc
// circles it while transcribing.
void draw_orb(thread float4 &result, float2 point, float shape, float clarity,
              constant Uniforms &uniforms) {
    float processing = clamp(uniforms.processing, 0.0, 1.0);
    float time = uniforms.time;
    float level = voice_level(uniforms);
    float breathing = 0.6 * sin(time * 3.0);
    float radius = mix(5.0 + breathing + 5.5 * level, 3.6 + 0.5 * sin(time * 5.0), processing);
    float angle = atan2(point.y, point.x);
    float swirl = 0.5 + 0.5 * sin(angle * 2.0 + time * 2.4 + length(point) * 0.35);
    float3 blue = float3(0.32, 0.56, 1.0);
    float3 violet = float3(0.74, 0.42, 1.0);
    float3 pink = float3(1.0, 0.46, 0.72);
    float3 orb_color = mix(mix(blue, violet, swirl), pink, level * 0.55 * (1.0 - swirl));
    orb_color = mix(orb_color, float3(0.86, 0.78, 1.0), clamp(uniforms.post_processing, 0.0, 1.0) * 0.6);
    float orb_distance = length(point) - radius;
    float halo = glow(orb_distance, 3.0 + 3.0 * level) * (1.0 - processing * 0.5);
    composite(result, orb_color, halo * 0.55 * shape * clarity);
    float core = coverage(orb_distance, max(fwidth(orb_distance), 0.35));
    float3 core_color = mix(orb_color, float3(1.0), 0.35 * smoothstep(radius, 0.0, length(point + float2(1.2, 1.2))));
    composite(result, core_color, core * 0.95 * shape * clarity);

    float ring_distance = abs(length(point) - (uniforms.height * 0.5 - 4.0)) - 0.9;
    float sweep = fract((angle + time * 5.0) / 6.2831853);
    float tail = smoothstep(0.35, 1.0, sweep);
    float ring = coverage(ring_distance, max(fwidth(ring_distance), 0.3));
    composite(result, content_color(uniforms), ring * tail * processing * shape * clarity);
}

// Line: a smooth oscilloscope trace; while transcribing it settles flat and a
// bright dash travels along it.
void draw_line(thread float4 &result, float2 point, float shape, float clarity,
               constant Uniforms &uniforms) {
    float processing = clamp(uniforms.processing, 0.0, 1.0);
    float time = uniforms.time;
    float level = voice_level(uniforms);
    float half_span = uniforms.width * 0.5 - 9.0;
    float x = clamp(point.x, -half_span, half_span);
    float taper = 1.0 - pow(abs(x) / half_span, 2.0);
    float amplitude = (uniforms.height * 0.5 - 5.0) * max(level, 0.06) * (1.0 - processing);
    float wave = sin(x * 0.42 - time * 9.0) * 0.6
        + sin(x * 0.23 + time * 5.3) * 0.3
        + sin(x * 0.71 - time * 13.0) * 0.1;
    float y = amplitude * taper * wave;
    float slope = amplitude * taper * 0.42 * 0.6;
    float trace_distance = length(float2(point.x - x, (point.y - y) / sqrt(1.0 + slope * slope))) - 0.75;
    float trace = coverage(trace_distance, max(fwidth(trace_distance), 0.3));
    float glint = shimmer(x, half_span, time);
    float brightness = mix(0.95, 0.25 + 0.75 * glint, processing);
    composite(result, content_color(uniforms), trace * brightness * shape * clarity);
}

// Dots: five dots that hop with the voice, then pulse in sequence while
// transcribing like a typing indicator.
void draw_dots(thread float4 &result, float2 point, float shape, float clarity,
               constant Uniforms &uniforms) {
    const float dot_count = 5.0;
    const float pitch = 7.0;
    float processing = clamp(uniforms.processing, 0.0, 1.0);
    float time = uniforms.time;
    float level = voice_level(uniforms);
    float half_slots = (dot_count - 1.0) * 0.5;
    float slot = clamp(round(point.x / pitch), -half_slots, half_slots);
    float envelope = 1.0 - abs(slot) / half_slots * 0.4;
    float jitter = 0.55 + 0.45 * sin(time * 12.0 + slot * 2.1);
    float hop_speaking = (uniforms.height * 0.5 - 6.0) * level * envelope * jitter;
    float phase = fract(time * 1.4 - (slot + half_slots) * 0.14);
    float pulse = smoothstep(0.0, 0.18, phase) * (1.0 - smoothstep(0.18, 0.42, phase));
    float hop = mix(hop_speaking, 2.6 * pulse, processing);
    float radius = mix(1.9 + 0.7 * level * jitter, 1.8 + 0.4 * pulse, processing);
    float dot_distance = length(point - float2(slot * pitch, hop)) - radius;
    float dot = coverage(dot_distance, max(fwidth(dot_distance), 0.3));
    float brightness = mix(0.95, 0.35 + 0.65 * pulse, processing);
    composite(result, content_color(uniforms), dot * brightness * shape * clarity);
}

// Blue to violet to pink, the palette shared by the colorful styles.
float3 palette(float t) {
    float3 blue = float3(0.32, 0.56, 1.0);
    float3 violet = float3(0.74, 0.42, 1.0);
    float3 pink = float3(1.0, 0.46, 0.72);
    t = clamp(t, 0.0, 1.0);
    return t < 0.5 ? mix(blue, violet, t * 2.0) : mix(violet, pink, t * 2.0 - 1.0);
}

// Coverage of a horizontal trace y(x) of the given half thickness, using its
// slope so steep parts keep their width.
float trace_coverage(float2 point, float y, float slope, float half_thickness) {
    float distance = abs(point.y - y) / sqrt(1.0 + slope * slope) - half_thickness;
    return coverage(distance, max(fwidth(distance), 0.3));
}

float pulse_shape(float d) {
    return exp2(-pow(d / 1.5, 2.0))
        - 0.35 * exp2(-pow((d + 3.6) / 1.7, 2.0))
        - 0.45 * exp2(-pow((d - 3.2) / 1.7, 2.0));
}

// Pulse: a heartbeat trace whose spike travels across and grows with the
// voice; it shrinks to a small blip while transcribing.
void draw_pulse(thread float4 &result, float2 point, float shape, float clarity,
                constant Uniforms &uniforms) {
    float processing = clamp(uniforms.processing, 0.0, 1.0);
    float level = voice_level(uniforms);
    float half_span = uniforms.width * 0.5 - 8.0;
    float x = clamp(point.x, -half_span, half_span);
    float head = mix(-half_span, half_span, fract(uniforms.time * mix(0.9, 1.3, processing)));
    float amplitude = (uniforms.height * 0.5 - 4.0) * mix(max(level, 0.14), 0.2, processing);
    float y = -amplitude * pulse_shape(x - head);
    float slope = -amplitude * (pulse_shape(x - head + 0.25) - pulse_shape(x - head - 0.25)) * 2.0;
    float trace = trace_coverage(float2(x, point.y), y, slope, 0.7) * step(abs(point.x), half_span + 0.8);
    float trail = 0.35 + 0.65 * exp2(-abs(x - head) / 9.0);
    composite(result, content_color(uniforms), trace * trail * shape * clarity);
}

// Equalizer: bars standing on a baseline that rise with the voice and settle
// into a low, sweeping row while transcribing.
void draw_equalizer(thread float4 &result, float2 point, float shape, float clarity,
                    constant Uniforms &uniforms) {
    const float bar_count = 7.0;
    const float bar_width = 3.6;
    const float bar_pitch = 6.6;
    float processing = clamp(uniforms.processing, 0.0, 1.0);
    float time = uniforms.time;
    float level = voice_level(uniforms);
    float half_slots = (bar_count - 1.0) * 0.5;
    float slot = clamp(round(point.x / bar_pitch), -half_slots, half_slots);
    float jitter = 0.5 + 0.5 * sin(time * 11.0 + slot * 2.3);
    float speaking = max(0.12 + 0.06 * sin(time * 2.5 + slot), level * (0.45 + 0.55 * jitter));
    float glint = shimmer(slot * bar_pitch, uniforms.width * 0.5, time);
    float amount = mix(speaking, 0.1 + 0.3 * glint, processing);
    float base = uniforms.height * 0.5 - 6.0;
    float max_height = uniforms.height - 12.0;
    float bar_height = max(bar_width, max_height * amount);
    float bar_distance = rounded_box(
        point - float2(slot * bar_pitch, base - bar_height * 0.5),
        float2(bar_width * 0.5, bar_height * 0.5),
        bar_width * 0.45);
    float bar = coverage(bar_distance, max(fwidth(bar_distance), 0.3)) * shape;
    composite(result, content_color(uniforms), bar * mix(0.95, 0.4 + 0.6 * glint, processing) * clarity);
}

// Spectrum: thin bars colored blue to pink; while transcribing the colors
// slide along a low row.
void draw_spectrum(thread float4 &result, float2 point, float shape, float clarity,
                   constant Uniforms &uniforms) {
    const float bar_count = 11.0;
    const float bar_width = 2.2;
    const float bar_pitch = 4.4;
    float processing = clamp(uniforms.processing, 0.0, 1.0);
    float time = uniforms.time;
    float level = voice_level(uniforms);
    float half_slots = (bar_count - 1.0) * 0.5;
    float slot = clamp(round(point.x / bar_pitch), -half_slots, half_slots);
    float envelope = 1.0 - abs(slot) / half_slots * 0.6;
    float jitter = 0.6 + 0.4 * sin(time * 13.0 + slot * 1.3);
    float speaking = max(0.1 + 0.05 * sin(time * 3.0 + slot), level * envelope * jitter);
    float amount = mix(speaking, 0.12 + 0.06 * sin(time * 6.0 + slot * 0.8), processing);
    float bar_height = 2.2 + (uniforms.height - 10.2) * amount;
    float bar_distance = rounded_box(
        point - float2(slot * bar_pitch, 0.0),
        float2(bar_width * 0.5, bar_height * 0.5),
        bar_width * 0.5);
    float bar = coverage(bar_distance, max(fwidth(bar_distance), 0.3)) * shape;
    float hue = (slot + half_slots) / (2.0 * half_slots);
    hue = fract(hue + processing * time * 0.5);
    float3 color = mix(palette(hue), float3(0.86, 0.78, 1.0), clamp(uniforms.post_processing, 0.0, 1.0) * 0.4);
    composite(result, color, bar * clarity);
}

// Twin: a blue and a pink trace moving against each other; they flatten
// while transcribing.
void draw_twin(thread float4 &result, float2 point, float shape, float clarity,
               constant Uniforms &uniforms) {
    float processing = clamp(uniforms.processing, 0.0, 1.0);
    float time = uniforms.time;
    float level = voice_level(uniforms);
    float half_span = uniforms.width * 0.5 - 9.0;
    float x = clamp(point.x, -half_span, half_span);
    float taper = 1.0 - pow(abs(x) / half_span, 2.0);
    float amplitude = (uniforms.height * 0.5 - 5.0) * max(level, 0.1) * mix(1.0, 0.25, processing) * taper;
    for (int index = 0; index < 2; index++) {
        float direction = index == 0 ? 1.0 : -1.0;
        float phase = x * 0.36 - direction * time * 7.0 + float(index) * 1.6;
        float y = amplitude * sin(phase);
        float slope = amplitude * 0.36 * cos(phase);
        float trace = trace_coverage(float2(x, point.y), y, slope, 0.65) * step(abs(point.x), half_span + 0.7);
        float3 color = index == 0 ? float3(0.36, 0.62, 1.0) : float3(1.0, 0.48, 0.74);
        composite(result, color, trace * 0.92 * shape * clarity);
    }
}

// Meter: a track that fills with loudness; while transcribing a short
// segment glides back and forth.
void draw_meter(thread float4 &result, float2 point, float shape, float clarity,
                constant Uniforms &uniforms) {
    float processing = clamp(uniforms.processing, 0.0, 1.0);
    float level = voice_level(uniforms);
    float half_track = uniforms.width * 0.5 - 7.0;
    const float half_thickness = 2.2;
    float track_distance = rounded_box(point, float2(half_track, half_thickness), half_thickness);
    float track = coverage(track_distance, max(fwidth(track_distance), 0.3));
    composite(result, float3(1.0), track * 0.14 * shape * clarity);

    float fill_end = -half_track + 2.0 * half_track * max(level, 0.05);
    float fill_half = (fill_end + half_track) * 0.5;
    float fill_distance = rounded_box(point - float2(-half_track + fill_half, 0.0),
                                      float2(max(fill_half, half_thickness), half_thickness), half_thickness);
    float fill = coverage(fill_distance, max(fwidth(fill_distance), 0.3));
    float3 fill_color = palette((point.x + half_track) / (2.0 * half_track));
    composite(result, fill_color, fill * (1.0 - processing) * shape * clarity);

    float segment = half_track * 0.3;
    float center = sin(uniforms.time * 3.0) * (half_track - segment);
    float glide_distance = rounded_box(point - float2(center, 0.0), float2(segment, half_thickness), half_thickness);
    float glide = coverage(glide_distance, max(fwidth(glide_distance), 0.3));
    composite(result, content_color(uniforms), glide * processing * shape * clarity);
}

// Pixels: a grid of small lights lit from the bottom like a level meter; a
// diagonal wave runs through it while transcribing.
void draw_pixels(thread float4 &result, float2 point, float shape, float clarity,
                 constant Uniforms &uniforms) {
    const float columns = 9.0;
    const float rows = 4.0;
    const float pitch = 4.6;
    float processing = clamp(uniforms.processing, 0.0, 1.0);
    float time = uniforms.time;
    float level = voice_level(uniforms);
    float half_columns = (columns - 1.0) * 0.5;
    float half_rows = (rows - 1.0) * 0.5;
    float column = clamp(round(point.x / pitch), -half_columns, half_columns);
    float row = clamp(round(point.y / pitch), -half_rows, half_rows);
    float height_from_bottom = half_rows - row;
    float envelope = 1.0 - abs(column) / half_columns * 0.5;
    float jitter = 0.55 + 0.45 * sin(time * 12.0 + column * 1.9);
    float lit_rows = max(0.6, level * envelope * jitter * rows);
    float speaking = step(height_from_bottom + 0.5, lit_rows);
    float wave = 0.5 + 0.5 * sin(time * 7.0 - (column + height_from_bottom) * 0.9);
    float lit = mix(speaking, smoothstep(0.6, 1.0, wave), processing);
    float cell_distance = rounded_box(point - float2(column * pitch, row * pitch), float2(1.5, 1.5), 0.6);
    float cell = coverage(cell_distance, max(fwidth(cell_distance), 0.3));
    float3 color = palette(height_from_bottom / (rows - 1.0));
    composite(result, float3(1.0), cell * 0.12 * shape * clarity);
    composite(result, color, cell * lit * 0.95 * shape * clarity);
}

// Ripple: rings that spread from a center dot, faster and brighter with the
// voice; they slow down while transcribing.
void draw_ripple(thread float4 &result, float2 point, float shape, float clarity,
                 constant Uniforms &uniforms) {
    float processing = clamp(uniforms.processing, 0.0, 1.0);
    float level = voice_level(uniforms);
    float radius = length(point);
    float max_radius = uniforms.height * 0.5 - 3.0;
    float speed = mix(0.5 + 0.9 * level, 0.35, processing);
    for (int index = 0; index < 3; index++) {
        float progress = fract(uniforms.time * speed + float(index) / 3.0);
        float ring_distance = abs(radius - progress * max_radius) - 0.6;
        float ring = coverage(ring_distance, max(fwidth(ring_distance), 0.3));
        float strength = (1.0 - progress) * mix(0.35 + 0.65 * level, 0.5, processing);
        composite(result, mix(float3(0.55, 0.75, 1.0), content_color(uniforms), 0.5), ring * strength * shape * clarity);
    }
    float core_radius = mix(2.4 + 2.2 * level, 2.2 + 0.6 * sin(uniforms.time * 5.0), processing);
    float core_distance = radius - core_radius;
    float core = coverage(core_distance, max(fwidth(core_distance), 0.3));
    composite(result, content_color(uniforms), core * shape * clarity);
}

// Halo: a colored ring that thickens and glows with the voice; while
// transcribing only a bright arc keeps turning.
void draw_halo(thread float4 &result, float2 point, float shape, float clarity,
               constant Uniforms &uniforms) {
    float processing = clamp(uniforms.processing, 0.0, 1.0);
    float time = uniforms.time;
    float level = voice_level(uniforms);
    float angle = atan2(point.y, point.x);
    float ring_radius = uniforms.height * 0.5 - 6.5;
    float thickness = mix(0.7 + 2.4 * level, 0.8, processing);
    float ring_distance = abs(length(point) - ring_radius) - thickness;
    float ring = coverage(ring_distance, max(fwidth(ring_distance), 0.3));
    float hue = 0.5 + 0.5 * sin(angle + time * 2.0);
    float3 color = mix(palette(hue), float3(0.86, 0.78, 1.0), clamp(uniforms.post_processing, 0.0, 1.0) * 0.5);
    float sweep = fract((angle + time * 5.0) / 6.2831853);
    float arc = mix(1.0, smoothstep(0.4, 1.0, sweep), processing);
    composite(result, color, glow(ring_distance, 1.5 + 2.5 * level) * 0.35 * arc * shape * clarity);
    composite(result, color, ring * arc * shape * clarity);
}

// Blob: a soft, color-shifting shape whose edge wobbles with the voice.
void draw_blob(thread float4 &result, float2 point, float shape, float clarity,
               constant Uniforms &uniforms) {
    float processing = clamp(uniforms.processing, 0.0, 1.0);
    float time = uniforms.time;
    float level = voice_level(uniforms);
    float angle = atan2(point.y, point.x);
    float base = mix(6.0 + 4.5 * level, 5.0, processing);
    float wobble = sin(angle * 3.0 + time * 2.2) + 0.6 * sin(angle * 5.0 - time * 3.1);
    float edge = base * (1.0 + 0.12 * wobble * mix(0.4 + level, 0.3, processing));
    float blob_distance = length(point) - edge;
    float swirl = 0.5 + 0.5 * sin(angle * 2.0 + time * 1.8);
    float3 color = mix(palette(swirl), float3(0.86, 0.78, 1.0), clamp(uniforms.post_processing, 0.0, 1.0) * 0.5);
    composite(result, color, glow(blob_distance, 2.5 + 2.0 * level) * 0.4 * shape * clarity);
    float body = coverage(blob_distance, max(fwidth(blob_distance), 0.35));
    float3 lit = mix(color, float3(1.0), 0.3 * smoothstep(edge, 0.0, length(point + float2(1.5, 1.5))));
    composite(result, lit, body * shape * clarity);
}

// Comet: a light circling a faint track, faster and larger as you talk.
void draw_comet(thread float4 &result, float2 point, float shape, float clarity,
                constant Uniforms &uniforms) {
    float processing = clamp(uniforms.processing, 0.0, 1.0);
    float level = voice_level(uniforms);
    float track_radius = uniforms.height * 0.5 - 6.0;
    float radius = length(point);
    float angle = atan2(point.y, point.x);
    float track_distance = abs(radius - track_radius) - 0.5;
    float track = coverage(track_distance, max(fwidth(track_distance), 0.3));
    composite(result, float3(1.0), track * 0.12 * shape * clarity);
    float head_angle = uniforms.time * mix(2.5 + 6.0 * level, 4.0, processing);
    float behind = fract((head_angle - angle) / 6.2831853);
    float tail = exp2(-behind * 7.0);
    float tail_distance = abs(radius - track_radius) - (0.5 + 1.0 * tail);
    float trail = coverage(tail_distance, max(fwidth(tail_distance), 0.3));
    composite(result, palette(0.3 + 0.5 * level), trail * tail * shape * clarity);
    float2 head = track_radius * float2(cos(head_angle), sin(head_angle));
    float head_distance = length(point - head) - (1.6 + 1.2 * level * (1.0 - processing));
    float head_dot = coverage(head_distance, max(fwidth(head_distance), 0.3));
    composite(result, content_color(uniforms), head_dot * shape * clarity);
}

// Dot: one small dot that grows with the voice and breathes while
// transcribing.
void draw_dot(thread float4 &result, float2 point, float shape, float clarity,
              constant Uniforms &uniforms) {
    float processing = clamp(uniforms.processing, 0.0, 1.0);
    float level = voice_level(uniforms);
    float radius = mix(2.2 + 3.0 * level, 2.4 + 0.8 * sin(uniforms.time * 4.0), processing);
    float dot_distance = length(point) - radius;
    composite(result, float3(0.55, 0.75, 1.0), glow(dot_distance, 1.5 + 2.0 * level) * 0.45 * shape * clarity);
    float dot = coverage(dot_distance, max(fwidth(dot_distance), 0.3));
    composite(result, content_color(uniforms), dot * shape * clarity);
}

// Aurora: bands of color flowing through the pill, brighter and taller with
// the voice; they dim to a slow drift while transcribing.
void draw_aurora(thread float4 &result, float2 point, float shape, float clarity,
                 constant Uniforms &uniforms) {
    float processing = clamp(uniforms.processing, 0.0, 1.0);
    float time = uniforms.time;
    float level = voice_level(uniforms);
    float energy = mix(0.25 + 0.75 * level, 0.3, processing);
    float half_span = uniforms.width * 0.5 - 4.0;
    float edge_fade = 1.0 - smoothstep(half_span * 0.6, half_span, abs(point.x));
    for (int index = 0; index < 3; index++) {
        float offset = float(index) * 2.1;
        float center = sin(point.x * 0.13 + time * (1.6 + 0.4 * float(index)) + offset)
            * (uniforms.height * 0.22) * (0.4 + energy);
        float spread = 1.6 + 2.6 * energy;
        float band = exp2(-pow((point.y - center) / spread, 2.0));
        float hue = fract(point.x / (uniforms.width) + 0.5 + time * 0.1 + float(index) * 0.33);
        composite(result, palette(hue), band * energy * 0.7 * edge_fade * shape * clarity);
    }
}

// The modern styles share a dark pill (a circle for Orb) with a soft shadow,
// hairline border, and a brief bloom when a dictation finishes.
float4 modern_indicator(float2 point, constant Uniforms &uniforms) {
    float lifecycle_softness = max(uniforms.softness, 0.0);
    float2 half_size = float2(uniforms.width, uniforms.height) * 0.5;
    float distance = rounded_box(point, half_size, uniforms.height * 0.5);
    float edge = (max(fwidth(distance), 0.32) + lifecycle_softness * 0.72)
        / max(uniforms.scale, 0.001);
    float shape = coverage(distance, edge);
    float clarity = exp2(-lifecycle_softness * 0.34);
    float completion = clamp(uniforms.completion, 0.0, 1.0);
    float completion_flash = smoothstep(0.0, 0.16, completion)
        * (1.0 - smoothstep(0.32, 1.0, completion));
    float4 result = 0.0;

    float shadow = glow(rounded_box(point - float2(0.0, 1.5), half_size, uniforms.height * 0.5), 5.0);
    composite(result, float3(0.0), shadow * 0.35 * (1.0 - shape));
    composite(result, float3(0.035, 0.035, 0.04), shape * 0.94);

    // Indices match IndicatorStyle::encoded in app_settings.rs.
    switch (int(round(uniforms.indicator_style))) {
        case 2: draw_orb(result, point, shape, clarity, uniforms); break;
        case 3: draw_line(result, point, shape, clarity, uniforms); break;
        case 4: draw_dots(result, point, shape, clarity, uniforms); break;
        case 5: draw_pulse(result, point, shape, clarity, uniforms); break;
        case 6: draw_equalizer(result, point, shape, clarity, uniforms); break;
        case 7: draw_spectrum(result, point, shape, clarity, uniforms); break;
        case 8: draw_twin(result, point, shape, clarity, uniforms); break;
        case 9: draw_meter(result, point, shape, clarity, uniforms); break;
        case 10: draw_pixels(result, point, shape, clarity, uniforms); break;
        case 11: draw_ripple(result, point, shape, clarity, uniforms); break;
        case 12: draw_halo(result, point, shape, clarity, uniforms); break;
        case 13: draw_blob(result, point, shape, clarity, uniforms); break;
        case 14: draw_comet(result, point, shape, clarity, uniforms); break;
        case 15: draw_dot(result, point, shape, clarity, uniforms); break;
        case 16: draw_aurora(result, point, shape, clarity, uniforms); break;
        default: draw_wave(result, point, shape, clarity, uniforms); break;
    }

    float stroke = coverage(abs(distance) - 0.4, 0.38 + lifecycle_softness * 0.3);
    composite(result, float3(1.0), stroke * 0.16 * clarity);
    float rim = exp2(-pow(abs(distance) / 0.8, 2.0));
    screen(result, float3(0.9, 0.95, 1.0), rim * completion_flash * 0.6);

    draw_queue(result, point, uniforms);
    return result * uniforms.opacity;
}

fragment float4 indicator_fragment(
    VertexOutput input [[stage_in]],
    constant Uniforms &uniforms [[buffer(0)]])
{
    float backing_scale = uniforms.resolution.x / 112.0;
    float2 point = (input.uv * uniforms.resolution - uniforms.resolution * 0.5)
        / backing_scale / max(uniforms.scale, 0.001);
    if (uniforms.indicator_style > 0.5) {
        return modern_indicator(point, uniforms);
    }
    float2 half_size = float2(uniforms.width, uniforms.height) * 0.5;
    float radius = uniforms.height * 0.5;
    float distance = rounded_box(point, half_size, radius);
    float lifecycle_softness = max(uniforms.softness, 0.0);
    float edge = (max(fwidth(distance), 0.32) + lifecycle_softness * 0.72)
        / max(uniforms.scale, 0.001);
    float shape = coverage(distance, edge);
    float detail_clarity = exp2(-lifecycle_softness * 0.34);
    float4 result = 0.0;

    float processing = clamp(uniforms.processing, 0.0, 1.0);
    float post_processing = clamp(uniforms.post_processing, 0.0, 1.0);
    float completion = clamp(uniforms.completion, 0.0, 1.0);
    float recording_flash = clamp(uniforms.recording_flash, 0.0, 1.0)
        * (1.0 - processing);
    float completion_flash = smoothstep(0.0, 0.16, completion)
        * (1.0 - smoothstep(0.32, 1.0, completion));
    float recording = 1.0 - processing;
    float circularity = 1.0 - smoothstep(0.5, 2.25, abs(uniforms.width - uniforms.height));

    float3 red_accent = float3(1.0, 0.025, 0.035);
    float3 blue_accent = float3(0.1, 0.34, 1.0);
    float3 violet_accent = float3(0.64, 0.2, 1.0);
    float3 pipeline_accent = mix(blue_accent, violet_accent, post_processing);
    float3 accent = mix(red_accent, pipeline_accent, processing);

    float average_power = clamp(uniforms.average, 0.0, 1.0);
    float peak_power = clamp(uniforms.peak, 0.0, 1.0);
    float average_activation = smoothstep(0.0, 0.1, average_power);
    float outer = recording * (
        glow(distance, 4.0 + lifecycle_softness * 0.5) * average_power * 0.72
        + glow(distance, 8.0 + lifecycle_softness) * average_power * 0.36);
    outer += processing * glow(distance, 2.6 + lifecycle_softness) * 0.045;
    outer += glow(distance, 6.0 + lifecycle_softness * 0.5) * completion_flash * 0.22;
    outer += glow(distance, 7.0 + lifecycle_softness * 0.5) * recording_flash * 0.5;
    composite(result, accent, outer * (1.0 - shape));

    float3 recording_base = mix(
        float3(0.5, 0.0, 0.0),
        float3(1.0, 0.0, 0.0),
        average_power);
    recording_base = mix(recording_base, float3(1.0, 0.08, 0.06), recording_flash * 0.72);
    float sphere_light = clamp(0.48 - point.x * 0.035 - point.y * 0.045, 0.0, 1.0);
    float3 blue_base = mix(
        float3(0.0, 0.02, 0.32),
        float3(0.04, 0.18, 0.68),
        sphere_light);
    float3 violet_base = mix(
        float3(0.12, 0.0, 0.3),
        float3(0.38, 0.04, 0.68),
        sphere_light);
    float3 pipeline_base = mix(blue_base, violet_base, post_processing);
    float3 base = mix(recording_base, pipeline_base, processing);
    composite(result, base, shape);

    float inside_edge = clamp(-distance / 4.0, 0.0, 1.0);
    float inner_edge = shape * (1.0 - inside_edge);
    float inner_edge_strength = mix(0.48, 0.07, processing);
    composite(result, accent, inner_edge * inner_edge_strength);

    // The classic recording stack: a padded red fill, a nearly
    // full-width white beam, and a broad peak-driven red glow.
    float2 beam_point = point - float2(0.0, -0.5);
    float red_fill_distance = rounded_box(beam_point, float2(22.0, 2.0), 2.0);
    float red_fill = glow(red_fill_distance, 2.0) * shape;
    screen(result, red_accent, red_fill * average_activation * recording * detail_clarity);

    float white_beam_distance = rounded_box(beam_point, float2(21.0, 1.0), 1.0);
    float white_beam = glow(white_beam_distance, 1.0) * shape;
    screen(
        result,
        float3(1.0),
        white_beam * average_activation * 0.56 * recording * detail_clarity);

    float peak_half_width = 22.0 * min(peak_power + 0.6, 1.0);
    float peak_distance = rounded_box(beam_point, float2(peak_half_width, 2.0), 2.0);
    float peak_beam = glow(peak_distance, 4.0) * shape;
    screen(
        result,
        red_accent,
        peak_beam * smoothstep(0.0, 0.1, peak_power) * 0.5 * recording * detail_clarity);

    if (processing > 0.0) {
        float orb_radius = uniforms.height * 0.5 - 0.6;
        float orb_distance = length(point) - orb_radius;
        float orb_edge = max(fwidth(orb_distance), 0.26);
        float orb_mask = coverage(orb_distance, orb_edge) * circularity;
        float2 sphere_uv = point / max(orb_radius, 0.001);
        float surface_z = sqrt(max(1.0 - dot(sphere_uv, sphere_uv), 0.0));
        float depth = mix(0.55, 1.45, clamp(uniforms.sphere_depth, 0.0, 1.0));
        float normal_z = pow(surface_z, depth);
        float light_phase = uniforms.light_angle * 6.2831853
            + uniforms.time * uniforms.line_speed * 0.22;
        float2 light_direction = float2(cos(light_phase), sin(light_phase));
        float light = clamp(
            0.32 + dot(sphere_uv, light_direction) * 0.18 + normal_z * 0.28,
            0.0,
            1.0);
        float3 orb_shadow = mix(float3(0.0, 0.015, 0.24), float3(0.1, 0.0, 0.26), post_processing);
        float3 orb_light = mix(float3(0.05, 0.22, 0.72), float3(0.4, 0.05, 0.7), post_processing);
        composite(result, mix(orb_shadow, orb_light, light), orb_mask * processing * detail_clarity);

        float rim = pow(1.0 - normal_z, 2.4);
        composite(result, pipeline_accent, rim * orb_mask * processing * 0.3 * detail_clarity);

        float style = clamp(uniforms.line_style, 0.0, 2.0);
        float sharpness = clamp(uniforms.line_sharpness, 0.0, 1.0);
        float blur_amount = 1.0 - sharpness;
        float highlight_count = clamp(round(uniforms.line_count), 1.0, 6.0);
        float shine = 0.0;
        float soft_shine = 0.0;
        if (style < 0.5) {
            // A moving area light gives the orb motion without drawing stripes
            // across its face.
            float focus = clamp(
                dot(sphere_uv, light_direction) * 0.55 + normal_z * 0.72,
                0.0,
                1.0);
            shine = pow(focus, mix(3.0, 12.0, sharpness));
        } else if (style < 1.5) {
            // Meridian highlights wrap around the sphere instead of crossing
            // it as flat screen-space lines.
            float longitude = atan2(sphere_uv.x, normal_z) / 6.2831853;
            float travel = uniforms.time * uniforms.line_speed * 0.11;
            for (int index = 0; index < 6; index++) {
                if (float(index) >= highlight_count) {
                    break;
                }
                float offset = float(index) / highlight_count;
                float angular_delta = fract(longitude - travel - offset + 0.5) - 0.5;
                float angular_distance = abs(angular_delta);
                float core = exp2(-pow(angular_distance / 0.018, 2.0));
                float halo_radius = mix(0.025, 0.14, blur_amount);
                float halo = exp2(-pow(angular_distance / halo_radius, 2.0));
                shine = max(shine, core);
                soft_shine = max(soft_shine, halo);
            }
        } else {
            // Curved ribbons remain available in the lab for comparison, but
            // are no longer the production default.
            float curvature = mix(0.0, 3.0, clamp(uniforms.line_curvature, 0.0, 1.0));
            float sweep_coordinate = point.x + sphere_uv.y * sphere_uv.y * curvature;
            float sweep_travel = orb_radius * 2.0 + 18.0;
            float sweep_start = fmod(
                uniforms.time * 38.0 * uniforms.line_speed,
                sweep_travel) - orb_radius - 7.0;
            float spacing = mix(7.0, 2.8, (highlight_count - 1.0) / 5.0);
            for (int index = 0; index < 6; index++) {
                if (float(index) >= highlight_count) {
                    break;
                }
                float line_distance = abs(
                    sweep_coordinate - (sweep_start - float(index) * spacing));
                float aa = max(fwidth(line_distance), 0.24);
                float band = 1.0 - smoothstep(
                    mix(1.4, 0.25, sharpness) - aa,
                    mix(4.8, 1.5, sharpness) + aa,
                    line_distance);
                shine = max(shine, band);
            }
        }
        float3 shine_color = mix(float3(0.66, 0.84, 1.0), float3(0.92, 0.72, 1.0), post_processing);
        float shine_strength = mix(0.16, 0.68, clamp(uniforms.line_glow, 0.0, 1.0));
        float surface_wrap = orb_mask * mix(0.28, 1.0, normal_z);
        float rim_spill = (1.0 - orb_mask) * glow(orb_distance, 2.2);
        composite(
            result,
            shine_color,
            soft_shine * (surface_wrap + rim_spill * 0.72) * processing * blur_amount
                * shine_strength * 0.52
                * detail_clarity);
        composite(
            result,
            shine_color,
            shine * surface_wrap * processing * shine_strength
                * mix(1.0, 0.36, blur_amount) * detail_clarity);
    }

    draw_queue(result, point, uniforms);

    screen(result, float3(0.72, 0.88, 1.0), shape * completion_flash * 0.32);
    float completion_rim = exp2(-pow(abs(distance) / 0.72, 2.0));
    screen(result, float3(0.82, 0.93, 1.0), completion_rim * completion_flash * 0.58);

    float sphere_outline = clamp(uniforms.sphere_outline, 0.0, 1.0);
    float processing_stroke = mix(0.04, 0.4, sphere_outline);
    float stroke = coverage(
        abs(distance) - 0.4,
        0.38 + lifecycle_softness * 0.3) * mix(0.56, processing_stroke, processing) * detail_clarity;
    composite(result, mix(accent, float3(1.0), 0.1), stroke);
    screen(result, float3(1.0, 0.2, 0.16), shape * recording_flash * 0.36);
    float recording_flash_rim = exp2(-pow(abs(distance) / 0.9, 2.0));
    screen(result, float3(1.0, 0.58, 0.48), recording_flash_rim * recording_flash * 0.82);

    result *= uniforms.opacity;
    return result;
}
