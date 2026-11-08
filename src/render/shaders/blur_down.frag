#version 100

//_DEFINES_

#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

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

// Half a pixel of what's drawn, in the texture's coordinates (0 to 1
// across it), and how many of those apart samples are.
uniform vec2 halfpixel;
uniform float offset;
// Half a pixel of the texture: samples are kept inside its edge pixels
// (past them, the texture would wrap around).
uniform vec2 edge;

vec4 at(vec2 p) {
    return texture2D(tex, clamp(p, edge, 1.0 - edge));
}

void main() {
    vec2 uv = v_coords;
    vec2 d = halfpixel * offset;
    // Halving: the pixel, and four around it diagonally.
    vec4 color = at(uv) * 4.0;
    color += at(uv - d);
    color += at(uv + d);
    color += at(uv + vec2(d.x, -d.y));
    color += at(uv - vec2(d.x, -d.y));
    color = color / 8.0;
#if defined(NO_ALPHA)
    color = vec4(color.rgb, 1.0);
#endif
    color = color * alpha;
#if defined(DEBUG_FLAGS)
    if (tint == 1.0)
        color = vec4(0.0, 0.2, 0.0, 0.2) + color * 0.8;
#endif
    gl_FragColor = color;
}
