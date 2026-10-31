#version 100

//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

// Pixel positions can be thousands: mediump isn't precise enough for them.
#ifdef GL_FRAGMENT_PRECISION_HIGH
precision highp float;
#else
precision mediump float;
#endif

#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

uniform float alpha;
varying vec2 v_coords;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

// From the texture's coordinates to pixels in the window's rectangle (its
// top left at 0): v_coords * to_window_scale + to_window_offset.
uniform vec2 to_window_scale;
uniform vec2 to_window_offset;
// The window's rectangle's size, in pixels, and its corners' radius.
uniform vec2 window_size;
uniform float radius;

// How far p is outside a rectangle of half size h around 0 whose corners
// are rounded r (negative: inside).
float outside(vec2 p, vec2 h, float r) {
    vec2 q = abs(p) - h + r;
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}

void main() {
    vec4 color = texture2D(tex, v_coords);

#if defined(NO_ALPHA)
    color = vec4(color.rgb, 1.0) * alpha;
#else
    color = color * alpha;
#endif

    // Only the corners of the window's rectangle are cut off (smoothed
    // over a pixel): what the client draws outside it (shadows) stays.
    vec2 p = v_coords * to_window_scale + to_window_offset;
    if (p.x >= 0.0 && p.y >= 0.0 && p.x <= window_size.x && p.y <= window_size.y) {
        vec2 c = window_size / 2.0;
        color = color * clamp(0.5 - outside(p - c, c, radius), 0.0, 1.0);
    }

#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif

    gl_FragColor = color;
}

