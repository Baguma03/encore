//! Filled, rounded Material Symbols drawn as meshes, so they stay crisp at any size.
//!
//! The outlines are Material Symbols Rounded (Apache License 2.0) at FILL 1, weight 600 and
//! optical size 24, on the font's 960-unit em square with y pointing down.

use std::sync::OnceLock;

use egui::epaint::{Mesh, PathShape, PathStroke};
use egui::{Color32, Painter, Pos2, Shape, Vec2};

const EM: f32 = 960.0;
/// Line segments per quadratic curve.
const CURVE_STEPS: usize = 6;

const PAUSE: &str = "M657 794.5Q613.2 794.5 582.1 763.3Q551 732.2 551 688.5L551 271.5Q551 227.8 582.1 196.7Q613.2 165.5 657 165.5Q700.7 165.5 731.8 196.7Q763 227.8 763 271.5L763 688.5Q763 732.2 731.8 763.3Q700.7 794.5 657 794.5ZM303 794.5Q259.3 794.5 228.2 763.3Q197 732.2 197 688.5L197 271.5Q197 227.8 228.2 196.7Q259.3 165.5 303 165.5Q346.8 165.5 377.9 196.7Q409 227.8 409 271.5L409 688.5Q409 732.2 377.9 763.3Q346.8 794.5 303 794.5Z";
const PLAY_ARROW: &str = "M300.8 697.7L300.8 262.3Q300.8 239 316.7 224.2Q332.7 209.3 353.8 209.3Q360.5 209.3 368 211Q375.4 212.8 382.1 217L725.1 435.3Q736.9 443 743.1 455.1Q749.3 467.2 749.3 480Q749.3 492.8 743.1 504.9Q736.9 517 725.1 524.7L382.1 743Q375.4 747.2 368 749Q360.5 750.7 353.8 750.7Q332.7 750.7 316.7 735.8Q300.8 721 300.8 697.7Z";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Icon {
    Play,
    Pause,
}

impl Icon {
    const ALL: [Icon; 2] = [Icon::Play, Icon::Pause];

    fn path(self) -> &'static str {
        match self {
            Icon::Play => PLAY_ARROW,
            Icon::Pause => PAUSE,
        }
    }

    fn glyph(self) -> &'static Glyph {
        static GLYPHS: OnceLock<Vec<Glyph>> = OnceLock::new();
        let glyphs =
            GLYPHS.get_or_init(|| Icon::ALL.iter().map(|i| Glyph::new(i.path())).collect());
        &glyphs[Icon::ALL.iter().position(|&i| i == self).unwrap_or(0)]
    }
}

/// Flattened outline and its triangulation, in em units.
struct Glyph {
    contours: Vec<Vec<Pos2>>,
    /// Triangles per contour, indexing into it.
    triangles: Vec<Vec<[u32; 3]>>,
}

impl Glyph {
    fn new(path: &str) -> Self {
        let contours = parse_path(path);
        let triangles = contours.iter().map(|c| triangulate(c)).collect();
        Self {
            contours,
            triangles,
        }
    }
}

/// Paints `icon` centred on `center` at font size `em`, like a glyph of that pixel size.
pub fn paint_icon(painter: &Painter, icon: Icon, center: Pos2, em: f32, color: Color32) {
    let glyph = icon.glyph();
    let scale = em / EM;
    let at = |p: Pos2| center + (p.to_vec2() - Vec2::splat(EM / 2.0)) * scale;
    let mut mesh = Mesh::default();
    for (contour, triangles) in glyph.contours.iter().zip(&glyph.triangles) {
        let base = mesh.vertices.len() as u32;
        for &p in contour {
            mesh.colored_vertex(at(p), color);
        }
        for [a, b, c] in triangles {
            mesh.add_triangle(base + a, base + b, base + c);
        }
    }
    painter.add(Shape::mesh(mesh));
    // A hairline along the outline gives the mesh egui's anti-aliased edge.
    for contour in &glyph.contours {
        let points = contour.iter().map(|&p| at(p)).collect();
        painter.add(PathShape::closed_line(points, PathStroke::new(0.6, color)));
    }
}

/// Contours of an SVG path made of absolute `M`, `L`, `H`, `V`, `Q`, `C` and `Z` commands,
/// with curves flattened.
fn parse_path(path: &str) -> Vec<Vec<Pos2>> {
    let mut contours = Vec::new();
    let mut current: Vec<Pos2> = Vec::new();
    let mut tokens = Tokens::new(path);
    let mut command = 'M';
    let mut pen = Pos2::ZERO;
    while let Some(token) = tokens.peek_command() {
        if let Some(c) = token {
            command = c;
        }
        match command {
            'M' => {
                if current.len() > 2 {
                    contours.push(std::mem::take(&mut current));
                }
                current.clear();
                let Some(p) = tokens.point() else { break };
                pen = p;
                current.push(p);
                // Coordinates after a move are line-tos.
                command = 'L';
            }
            'L' => {
                let Some(p) = tokens.point() else { break };
                pen = p;
                current.push(p);
            }
            'H' => {
                let Some(x) = tokens.number() else { break };
                pen.x = x;
                current.push(pen);
            }
            'V' => {
                let Some(y) = tokens.number() else { break };
                pen.y = y;
                current.push(pen);
            }
            'Q' => {
                let (Some(c), Some(p)) = (tokens.point(), tokens.point()) else {
                    break;
                };
                for i in 1..=CURVE_STEPS {
                    let t = i as f32 / CURVE_STEPS as f32;
                    let u = 1.0 - t;
                    current.push(Pos2::new(
                        u * u * pen.x + 2.0 * u * t * c.x + t * t * p.x,
                        u * u * pen.y + 2.0 * u * t * c.y + t * t * p.y,
                    ));
                }
                pen = p;
            }
            'C' => {
                let (Some(c1), Some(c2), Some(p)) =
                    (tokens.point(), tokens.point(), tokens.point())
                else {
                    break;
                };
                for i in 1..=CURVE_STEPS * 2 {
                    let t = i as f32 / (CURVE_STEPS * 2) as f32;
                    let u = 1.0 - t;
                    let f = |a: f32, b: f32, c: f32, d: f32| {
                        u * u * u * a + 3.0 * u * u * t * b + 3.0 * u * t * t * c + t * t * t * d
                    };
                    current.push(Pos2::new(
                        f(pen.x, c1.x, c2.x, p.x),
                        f(pen.y, c1.y, c2.y, p.y),
                    ));
                }
                pen = p;
            }
            'Z' => {
                if current.len() > 2 {
                    contours.push(std::mem::take(&mut current));
                }
                current.clear();
            }
            _ => break,
        }
    }
    if current.len() > 2 {
        contours.push(current);
    }
    contours.into_iter().map(clean).collect()
}

/// Drops repeated points, including a last point equal to the first.
fn clean(mut points: Vec<Pos2>) -> Vec<Pos2> {
    points.dedup_by(|a, b| (*a - *b).length_sq() < 1e-4);
    while points.len() > 1 && (points[0] - points[points.len() - 1]).length_sq() < 1e-4 {
        points.pop();
    }
    points
}

struct Tokens<'a> {
    rest: &'a str,
}

impl<'a> Tokens<'a> {
    fn new(s: &'a str) -> Self {
        Self { rest: s }
    }

    fn skip_separators(&mut self) {
        self.rest = self
            .rest
            .trim_start_matches(|c: char| c.is_whitespace() || c == ',');
    }

    /// `None` at the end, `Some(Some(c))` before a command letter (consumed) and
    /// `Some(None)` before a number.
    fn peek_command(&mut self) -> Option<Option<char>> {
        self.skip_separators();
        let c = self.rest.chars().next()?;
        if c.is_ascii_alphabetic() {
            self.rest = &self.rest[1..];
            Some(Some(c))
        } else {
            Some(None)
        }
    }

    fn number(&mut self) -> Option<f32> {
        self.skip_separators();
        let end = self
            .rest
            .char_indices()
            .find(|&(i, c)| !(c.is_ascii_digit() || c == '.' || (i == 0 && c == '-')))
            .map_or(self.rest.len(), |(i, _)| i);
        let (num, rest) = self.rest.split_at(end);
        self.rest = rest;
        num.parse().ok()
    }

    fn point(&mut self) -> Option<Pos2> {
        Some(Pos2::new(self.number()?, self.number()?))
    }
}

/// Twice the signed area of a polygon; positive when counter-clockwise with y up.
fn signed_area(points: &[Pos2]) -> f32 {
    let n = points.len();
    (0..n)
        .map(|i| {
            let (a, b) = (points[i], points[(i + 1) % n]);
            a.x * b.y - b.x * a.y
        })
        .sum()
}

/// Ear-clipping triangulation of a simple polygon.
fn triangulate(points: &[Pos2]) -> Vec<[u32; 3]> {
    let n = points.len();
    if n < 3 {
        return Vec::new();
    }
    let orientation = signed_area(points).signum();
    let cross = |a: Pos2, b: Pos2, c: Pos2| (b - a).x * (c - a).y - (b - a).y * (c - a).x;
    let inside = |p: Pos2, a: Pos2, b: Pos2, c: Pos2| {
        let (d1, d2, d3) = (cross(a, b, p), cross(b, c, p), cross(c, a, p));
        let neg = d1 < 0.0 || d2 < 0.0 || d3 < 0.0;
        let pos = d1 > 0.0 || d2 > 0.0 || d3 > 0.0;
        !(neg && pos)
    };
    let mut remaining: Vec<usize> = (0..n).collect();
    let mut triangles = Vec::with_capacity(n - 2);
    let mut guard = 0;
    while remaining.len() > 3 && guard < n * n {
        guard += 1;
        let m = remaining.len();
        let ear = (0..m).find(|&i| {
            let (ia, ib, ic) = (
                remaining[(i + m - 1) % m],
                remaining[i],
                remaining[(i + 1) % m],
            );
            let (a, b, c) = (points[ia], points[ib], points[ic]);
            if cross(a, b, c) * orientation <= 0.0 {
                return false;
            }
            remaining
                .iter()
                .filter(|&&j| j != ia && j != ib && j != ic)
                .all(|&j| !inside(points[j], a, b, c))
        });
        // Degenerate leftovers: clip the flattest corner instead of looping forever.
        let i = ear.unwrap_or(0);
        let (ia, ib, ic) = (
            remaining[(i + m - 1) % m],
            remaining[i],
            remaining[(i + 1) % m],
        );
        triangles.push([ia as u32, ib as u32, ic as u32]);
        remaining.remove(i);
    }
    if remaining.len() == 3 {
        triangles.push([
            remaining[0] as u32,
            remaining[1] as u32,
            remaining[2] as u32,
        ]);
    }
    triangles
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area(points: &[Pos2], triangles: &[[u32; 3]]) -> f32 {
        triangles
            .iter()
            .map(|t| signed_area(&t.map(|i| points[i as usize])).abs() / 2.0)
            .sum()
    }

    #[test]
    fn parses_commands_and_implicit_line_tos() {
        let c = parse_path("M0 0 10 0V10H0Z M20 20L30 20L30 30Z");
        assert_eq!(c.len(), 2);
        assert_eq!(
            c[0],
            [(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)].map(|(x, y)| Pos2::new(x, y))
        );
        assert_eq!(c[1].len(), 3);
        let curve = parse_path("M0 0Q5 10 10 0Z");
        assert_eq!(curve[0].len(), CURVE_STEPS + 1);
        assert!(curve[0].iter().all(|p| p.y >= 0.0 && p.y <= 5.0));
    }

    #[test]
    fn triangulation_covers_non_convex_polygons() {
        // An L shape of area 3.
        let l: Vec<Pos2> = [
            (0.0, 0.0),
            (2.0, 0.0),
            (2.0, 1.0),
            (1.0, 1.0),
            (1.0, 2.0),
            (0.0, 2.0),
        ]
        .map(|(x, y)| Pos2::new(x, y))
        .to_vec();
        let t = triangulate(&l);
        assert_eq!(t.len(), 4);
        assert!((area(&l, &t) - 3.0).abs() < 1e-4);
        let mut reversed = l.clone();
        reversed.reverse();
        assert!((area(&reversed, &triangulate(&reversed)) - 3.0).abs() < 1e-4);
    }

    #[test]
    fn every_icon_is_fully_triangulated() {
        for icon in Icon::ALL {
            let glyph = icon.glyph();
            assert!(!glyph.contours.is_empty(), "{icon:?}");
            for (contour, triangles) in glyph.contours.iter().zip(&glyph.triangles) {
                let expected = signed_area(contour).abs() / 2.0;
                let got = area(contour, triangles);
                assert!(
                    (got - expected).abs() < expected * 1e-3,
                    "{icon:?}: {got} vs {expected}"
                );
                assert!(contour
                    .iter()
                    .all(|p| (0.0..=EM).contains(&p.x) && (0.0..=EM).contains(&p.y)));
            }
        }
    }
}
