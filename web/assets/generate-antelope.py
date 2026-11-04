#!/usr/bin/env python3
"""Project a small faceted antelope mesh to three static, dithered SVGs.

After generating, optimize with:
    nix shell nixpkgs#svgo -c svgo --multipass assets/antelope-*.svg


SLOP!!!!
"""

import itertools
import math
from pathlib import Path

HERE = Path(__file__).resolve().parent

# Coordinates: x right, y down, z toward the viewer. The forward-offset
# lower rings form a real protruding muzzle, not a painted-on nose.
rings = [
    (95, 18, 5, 0),
    (104, 26, 12, 0),
    (119, 29, 19, 1),
    (137, 25, 22, 4),
    (157, 19, 22, 11),
    (176, 15, 19, 21),
    (193, 14, 16, 29),
    (204, 15, 13, 34),
    (213, 12, 10, 32),
    (220, 7, 5, 27),
]
mesh = []


def face(points, color, overlay=False):
    mesh.append((points, color, overlay))


def surface_z(x, y):
    """Interpolate the front of the faceted head at a given x/y."""
    for lower, upper in itertools.pairwise(rings):
        if lower[0] <= y <= upper[0]:
            t = (y - lower[0]) / (upper[0] - lower[0])
            width, depth, center = [
                lower[i] + t * (upper[i] - lower[i]) for i in (1, 2, 3)
            ]
            # The front half of each ring consists of three facets per side.
            fraction = min(abs(x) / width, 1)
            xs = (0, 0.5, math.sqrt(3) / 2, 1)
            zs = (1, math.sqrt(3) / 2, 0.5, 0)
            for i in range(3):
                if fraction <= xs[i + 1]:
                    f = (fraction - xs[i]) / (xs[i + 1] - xs[i])
                    return center + depth * (zs[i] + f * (zs[i + 1] - zs[i]))
    raise ValueError("Eye point outside head")


def eye(points, color, offset):
    face([(x, y, surface_z(x, y) + offset) for x, y in points], color, overlay=True)


def ring(y, width, depth, zcenter, n=12):
    return [
        (
            width * math.sin(2 * math.pi * i / n),
            y,
            zcenter + depth * math.cos(2 * math.pi * i / n),
        )
        for i in range(n)
    ]


def tube(layers, color, n=8):
    for a, b in itertools.pairwise(layers):
        for i in range(n):
            face([a[i], a[(i + 1) % n], b[(i + 1) % n], b[i]], color)


head = [ring(*r) for r in rings]
tube(head, "#bd8d61", 12)
face(list(reversed(head[0])), "#9b704e")
face(head[-1], "#413432")

# Horns are tapered, bent polygonal tubes, with alternating narrow ridges.
for side in (-1, 1):
    centers = [
        (side * x, y, z, r)
        for x, y, z, r in [
            (14, 103, 2, 7),
            (18, 90, 1, 6),
            (25, 78, 0, 5.5),
            (34, 67, -1, 5),
            (41, 55, -2, 4.5),
            (45, 43, -3, 4),
            (44, 31, -4, 3.2),
            (40, 20, -5, 2.4),
            (34, 10, -6, 1.5),
            (27, 3, -7, 0.2),
        ]
    ]
    horn = []
    for x, y, z, r in centers:
        horn.append(
            [
                (
                    x + r * math.cos(2 * math.pi * i / 8),
                    y,
                    z + r * math.sin(2 * math.pi * i / 8),
                )
                for i in range(8)
            ]
        )
    tube(horn, "#434b59")
    # Ear surfaces, placed behind the face and angled toward the viewer.
    face(
        [
            (side * 21, 116, -1),
            (side * 43, 99, -5),
            (side * 69, 98, -9),
            (side * 61, 113, -1),
            (side * 35, 123, 5),
        ],
        "#8d6c56",
    )
    face(
        [
            (side * 29, 115, 0),
            (side * 45, 104, -3),
            (side * 62, 101, -6),
            (side * 55, 111, 0),
            (side * 36, 119, 5),
        ],
        "#e0bda0",
    )
    # Eye sockets and eyes sit on the forward sloping head planes.
    eye(
        [(side * 10, 125), (side * 22, 127), (side * 20, 138), (side * 11, 137)],
        "#e4c9a3",
        0.15,
    )
    eye(
        [(side * 12, 129), (side * 20, 130), (side * 18, 135), (side * 13, 135)],
        "#222630",
        0.3,
    )

# Nose and mouth: front-facing raised polygons follow the projecting muzzle.
eye([(-8, 202), (0, 201), (8, 202), (6, 207), (0, 211), (-6, 207)], "#302d31", 0.2)
eye([(-5, 203), (0, 202), (5, 203), (3, 205), (0, 206), (-3, 205)], "#555052", 0.3)


def render(angle, filename, rainbow=False):
    angle = math.radians(angle)
    c, s = math.cos(angle), math.sin(angle)

    def rotate(p):
        x, y, z = p
        # Shorten the lower face while preserving the mesh and muzzle depth.
        y = 140 + (y - 140) * 0.72 if y > 140 else y
        return (x * c + z * s, y, -x * s + z * c)

    def project(p):
        x, y, z = p
        scale = 360 / (360 - z)
        return (90 + x * scale, 113 + (y - 113) * scale, z)

    def shade(points, base):
        a, b, d = points[:3]
        u = [b[i] - a[i] for i in range(3)]
        v = [d[i] - a[i] for i in range(3)]
        n = [
            u[1] * v[2] - u[2] * v[1],
            u[2] * v[0] - u[0] * v[2],
            u[0] * v[1] - u[1] * v[0],
        ]
        length = math.sqrt(sum(x * x for x in n)) or 1
        n = [x / length for x in n]
        # Two-sided lighting: facets have consistent color even where winding differs.
        diffuse = abs(n[0] * -0.4 + n[1] * -0.45 + n[2] * 0.8)
        value = 0.46 + 0.54 * diffuse
        rgb = [int(base[i : i + 2], 16) for i in (1, 3, 5)]
        return "#" + "".join(f"{min(255, round(x * value)):02x}" for x in rgb)

    polygons = []
    for vertices, color, overlay in mesh:
        rotated = [rotate(v) for v in vertices]
        projected = [project(v) for v in rotated]
        depth = 1000 if overlay else sum(v[2] for v in projected) / len(projected)
        is_rainbow = rainbow and (not overlay or color == "#e4c9a3")
        polygons.append(
            (
                depth,
                projected,
                shade(rotated, "#ffffff" if is_rainbow else color),
                is_rainbow,
            )
        )
    polygons.sort(key=lambda item: item[0])
    out = [
        '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 180 210" shape-rendering="crispEdges">',
        '<defs><pattern id="d" width="4" height="4" patternUnits="userSpaceOnUse"><path d="M0 0h2v2H0zm2 2h2v2H2z" fill="#1a1c29" opacity=".13"/></pattern><linearGradient id="rainbow" gradientUnits="userSpaceOnUse" x1="0" y1="0" x2="0" y2="210"><stop stop-color="#f14862"/><stop offset=".2" stop-color="#ff9a31"/><stop offset=".4" stop-color="#f1d634"/><stop offset=".6" stop-color="#31ce80"/><stop offset=".8" stop-color="#388fe9"/><stop offset="1" stop-color="#aa55dc"/></linearGradient></defs>',
    ]
    dither_paths = []
    for _, points, color, is_rainbow in polygons:
        xy = [(round(x, 1), round(y, 1)) for x, y, _ in points]
        p = " ".join(f"{x:g},{y:g}" for x, y in xy)
        if is_rainbow:
            opacity = 1 - int(color[1:3], 16) / 255
            out.append(
                f'<polygon points="{p}" fill="url(#rainbow)"/><polygon points="{p}" fill="#000" opacity="{opacity:.2f}"/>'
            )
        else:
            out.append(f'<polygon points="{p}" fill="{color}"/>')
        # Consistent winding makes the combined dither path cover the union of
        # all facets, even where the projected polygons overlap.
        area = sum(
            xy[i][0] * xy[(i + 1) % len(xy)][1] - xy[(i + 1) % len(xy)][0] * xy[i][1]
            for i in range(len(xy))
        )
        if area < 0:
            xy.reverse()
        dither_paths.append("M" + " ".join(f"{x:g},{y:g}" for x, y in xy) + "z")
    out.append(f'<path d="{"".join(dither_paths)}" fill="url(#d)"/>')
    out.append("</svg>")
    (HERE / filename).write_text("\n".join(out) + "\n")


for angle, name in [
    (-25, "antelope-left.svg"),
    (0, "antelope-center.svg"),
    (25, "antelope-right.svg"),
]:
    render(angle, name)
    render(angle, name.replace("antelope-", "antelope-rainbow-"), rainbow=True)
