use crate::network::Command;
use serde::Deserialize;
use smallvec::SmallVec;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform};
fn parse_color(s: &str) -> Color {
    let hex = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("#"))
        .unwrap_or(s);
    let css = if hex.len() == 8 {
        // Convert AARRGGBB to #RRGGBBAA
        format!("#{}{}{}{}", &hex[2..4], &hex[4..6], &hex[6..8], &hex[0..2])
    } else {
        format!("#{}", hex)
    };
    csscolorparser::parse(&css)
        .map(|c| Color::from_rgba(c.r as f32, c.g as f32, c.b as f32, c.a as f32).unwrap())
        .unwrap_or(Color::TRANSPARENT)
}
#[derive(Deserialize)]
pub enum LineSide {
    Left,
    Right,
    Center,
}
#[derive(Deserialize)]
#[serde(tag = "type")]
pub enum DrawOperation {
    Pixel {
        x: i32,
        y: i32,
        color: String,
    },
    Line {
        x1: i32,
        y1: i32,
        x2: i32,
        y2: i32,
        width: f32,
        side: LineSide,
        color: String,
    },
    Circle {
        x: i32,
        y: i32,
        radius: f32,
        fill_color: String,
        outline_width: f32,
        outline_color: String,
    },
    Rectangle {
        x1: i32,
        y1: i32,
        x2: i32,
        y2: i32,
        fill_color: String,
        outline_width: f32,
        outline_color: String,
    },
}
impl DrawOperation {
    fn render(&self, renderer: &mut Renderer) {
        match self {
            DrawOperation::Pixel { x, y, color } => {
                let color = parse_color(color);
                let mut paint = Paint::default();
                paint.set_color(color);
                if let Some(rect) = Rect::from_xywh(*x as f32, *y as f32, 1.0, 1.0) {
                    renderer
                        .pixmap
                        .fill_rect(rect, &paint, Transform::identity(), None);
                }
            }
            DrawOperation::Line {
                x1,
                y1,
                x2,
                y2,
                width,
                side,
                color,
            } => {
                let x1 = *x1 as f32;
                let y1 = *y1 as f32;
                let x2 = *x2 as f32;
                let y2 = *y2 as f32;
                let width = *width;
                let color = parse_color(color);
                let dx = x2 - x1;
                let dy = y2 - y1;
                let length = (dx * dx + dy * dy).sqrt();
                if length == 0.0 {
                    return;
                }
                let nx = -dy / length;
                let ny = dx / length;
                let (offset_x, offset_y) = match side {
                    LineSide::Center => (0.0, 0.0),
                    LineSide::Left => (nx * (width / 2.0), ny * (width / 2.0)),
                    LineSide::Right => (-nx * (width / 2.0), -ny * (width / 2.0)),
                };
                let start_x = x1 + offset_x;
                let start_y = y1 + offset_y;
                let end_x = x2 + offset_x;
                let end_y = y2 + offset_y;
                let mut path = PathBuilder::new();
                path.move_to(start_x, start_y);
                path.line_to(end_x, end_y);
                if let Some(path) = path.finish() {
                    let mut paint = Paint::default();
                    paint.set_color(color);
                    let mut stroke = Stroke::default();
                    stroke.width = width;
                    renderer.pixmap.stroke_path(
                        &path,
                        &paint,
                        &stroke,
                        Transform::identity(),
                        None,
                    );
                }
            }
            DrawOperation::Circle {
                x,
                y,
                radius,
                fill_color,
                outline_width,
                outline_color,
            } => {
                let cx = *x as f32;
                let cy = *y as f32;
                let radius = *radius;
                let fill_color = parse_color(fill_color);
                let outline_color = parse_color(outline_color);
                let outline_width = *outline_width;
                let mut path = PathBuilder::new();
                path.push_circle(cx, cy, radius);
                if let Some(path) = path.finish() {
                    let mut fill_paint = Paint::default();
                    fill_paint.set_color(fill_color);
                    renderer.pixmap.fill_path(
                        &path,
                        &fill_paint,
                        FillRule::Winding,
                        Transform::identity(),
                        None,
                    );
                    if outline_width > 0.0 {
                        let mut stroke_paint = Paint::default();
                        stroke_paint.set_color(outline_color);
                        let mut stroke = Stroke::default();
                        stroke.width = outline_width;
                        renderer.pixmap.stroke_path(
                            &path,
                            &stroke_paint,
                            &stroke,
                            Transform::identity(),
                            None,
                        );
                    }
                }
            }
            DrawOperation::Rectangle {
                x1,
                y1,
                x2,
                y2,
                fill_color,
                outline_width,
                outline_color,
            } => {
                let x1 = *x1 as f32;
                let y1 = *y1 as f32;
                let x2 = *x2 as f32;
                let y2 = *y2 as f32;
                let width = x2 - x1;
                let height = y2 - y1;
                let fill_color = parse_color(fill_color);
                let outline_color = parse_color(outline_color);
                let outline_width = *outline_width;
                if let Some(rect) = Rect::from_xywh(x1, y1, width, height) {
                    let mut pb = PathBuilder::new();
                    pb.push_rect(rect);
                    let path = pb.finish().unwrap();
                    let mut fill_paint = Paint::default();
                    fill_paint.set_color(fill_color);
                    renderer.pixmap.fill_path(
                        &path,
                        &fill_paint,
                        FillRule::Winding,
                        Transform::identity(),
                        None,
                    );
                    if outline_width > 0.0 {
                        let mut stroke_paint = Paint::default();
                        stroke_paint.set_color(outline_color);
                        let mut stroke = Stroke::default();
                        stroke.width = outline_width;
                        renderer.pixmap.stroke_path(
                            &path,
                            &stroke_paint,
                            &stroke,
                            Transform::identity(),
                            None,
                        );
                    }
                }
            }
        }
    }
}
pub struct Layer {
    pub operations: SmallVec<[DrawOperation; 8]>,
    pub expiry: Option<Instant>,
}
pub struct CanvasState {
    layers: HashMap<i32, Layer>,
}
impl CanvasState {
    pub fn new() -> Self {
        Self {
            layers: HashMap::new(),
        }
    }
    pub fn update(&mut self, command: Command) {
        let layer_id = command.layer.unwrap_or(0);
        let expiry = command
            .timeout_ms
            .map(|ms| Instant::now() + Duration::from_millis(ms));
        self.layers.insert(
            layer_id,
            Layer {
                operations: command.operations.into(),
                expiry,
            },
        );
    }
    pub fn prune(&mut self) -> bool {
        let now = Instant::now();
        let before = self.layers.len();
        self.layers
            .retain(|_, layer| layer.expiry.map_or(true, |e| e > now));
        let after = self.layers.len();
        before != after
    }
    pub fn render(&self, renderer: &mut Renderer) {
        renderer.pixmap.fill(Color::TRANSPARENT);
        let mut sorted_layers: Vec<_> = self.layers.iter().collect();
        sorted_layers.sort_by_key(|(z, _)| *z);
        for (_, layer) in sorted_layers {
            for op in &layer.operations {
                op.render(renderer);
            }
        }
    }
}
pub struct Renderer {
    pub pixmap: Pixmap,
}
impl Renderer {
    pub fn new(width: u32, height: u32) -> Self {
        let pixmap = Pixmap::new(width, height).expect("Failed to create pixmap");
        Self { pixmap }
    }
}
