precision highp float;
uniform vec2 size;
uniform float alpha;
varying vec2 v_coords;
#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

uniform float blur;
uniform float spread;
uniform vec2 offset;
uniform float radius;
uniform vec4 color;

float outside(vec2 p, vec2 h, float r) {
    r = min(r, min(h.x, h.y));
    vec2 q = abs(p) - h + r;
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}

float erf(float x) {
    float a = abs(x);
    float t = 1.0 + a * (0.278393 + a * (0.230389 + a * (0.000972 + a * 0.078108)));
    t *= t;
    return sign(x) * (1.0 - 1.0 / (t * t));
}

void main() {
    vec2 p = v_coords * size;
    vec2 c = size / 2.0;
    vec2 shape = c - blur;
    float d = outside(p - c, shape, radius > 0.0 ? max(radius + spread, 0.0) : 0.0);
    float sigma = blur / 3.0;
    float cover = sigma > 0.0
        ? 0.5 - 0.5 * erf(d / (sigma * 1.4142136))
        : clamp(0.5 - d, 0.0, 1.0);
    float under = outside(p - c + offset, shape - spread, radius);
    cover *= clamp(0.5 + under, 0.0, 1.0);
    vec4 result = color * cover * alpha;
#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        result = vec4(0.0, 0.2, 0.0, 0.2) + result * 0.8;
#endif
    gl_FragColor = result;
}
