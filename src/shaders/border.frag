precision highp float;
uniform vec2 size;
uniform float alpha;
varying vec2 v_coords;
#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

uniform float radius;
uniform float width;
uniform vec4 top;
uniform vec4 right;
uniform vec4 bottom;
uniform vec4 left;

// How far p is outside a rectangle of half size h around 0 whose corners
// are rounded r (negative: inside).
float outside(vec2 p, vec2 h, float r) {
    vec2 q = abs(p) - h + r;
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}

void main() {
    vec2 p = v_coords * size;
    vec2 c = size / 2.0;
    // Inside the outer edge, smoothed over a pixel; outside the inner one,
    // reaching a pixel under the window, whose corners are smoothed too
    float outer = outside(p - c, c, radius > 0.0 ? radius + width : 0.0);
    float inner = outside(p - c, c - width, radius);
    float cover = clamp(0.5 - outer, 0.0, 1.0) * clamp(1.5 + inner, 0.0, 1.0);
    // The nearest side's colour.
    vec4 color = top;
    float d = p.y;
    if (size.x - p.x < d) { d = size.x - p.x; color = right; }
    if (size.y - p.y < d) { d = size.y - p.y; color = bottom; }
    if (p.x < d) { color = left; }
    color = color * cover * alpha;
#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif
    gl_FragColor = color;
}
