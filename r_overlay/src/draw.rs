use crate::network::Command;
use serde::Deserialize;
use std::collections::HashMap;
use std::time::{Duration, Instant};
use tiny_skia::{Color, FillRule, Paint, PathBuilder, Pixmap, Rect, Stroke, Transform};
#[derive(Deserialize, Clone)]
pub enum LineSide {
    Left,
    Right,
    Center,
}
#[derive(Deserialize, Clone)]
pub struct PixelParams {
    pub x: i32,
    pub y: i32,
    pub color: String,
}
#[derive(Deserialize, Clone)]
pub struct LineParams {
    pub x1: i32,
    pub y1: i32,
    pub x2: i32,
    pub y2: i32,
    pub width: f32,
    pub side: LineSide,
    pub color: String,
}
#[derive(Deserialize, Clone)]
pub struct CircleParams {
    pub x: i32,
    pub y: i32,
    pub radius: f32,
    pub fill_color: String,
    pub outline_width: f32,
    pub outline_color: String,
}
#[derive(Deserialize, Clone)]
pub struct RectangleParams {
    pub x1: i32,
    pub y1: i32,
    pub x2: i32,
    pub y2: i32,
    pub fill_color: String,
    pub outline_width: f32,
    pub outline_color: String,
}
#[derive(Deserialize, Clone)]
pub enum DrawOperation {
    Pixel(PixelParams),
    Line(LineParams),
    Circle(CircleParams),
    Rectangle(RectangleParams),
}
pub struct Layer {
    pub operations: Vec<DrawOperation>,
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
                operations: command.operations,
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
                self.draw_operation(renderer, op);
            }
        }
    }
    pub fn draw_operation(&self, renderer: &mut Renderer, op: &DrawOperation) {
        match op {
            DrawOperation::Pixel(p) => renderer.draw_pixel(p.clone()),
            DrawOperation::Line(p) => renderer.draw_line(p.clone()),
            DrawOperation::Circle(p) => renderer.draw_circle(p.clone()),
            DrawOperation::Rectangle(p) => renderer.draw_rectangle(p.clone()),
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
    fn parse_color(hex: &str) -> Color {
        let hex = hex
            .strip_prefix("0x")
            .or_else(|| hex.strip_prefix("#"))
            .unwrap_or(hex);
        let (r, g, b, a) = if hex.len() == 8 {
            let a = u8::from_str_radix(&hex[0..2], 16).unwrap_or(255);
            let r = u8::from_str_radix(&hex[2..4], 16).unwrap_or(0);
            let g = u8::from_str_radix(&hex[4..6], 16).unwrap_or(0);
            let b = u8::from_str_radix(&hex[6..8], 16).unwrap_or(0);
            (r, g, b, a)
        } else if hex.len() == 6 {
            let r = u8::from_str_radix(&hex[0..2], 16).unwrap_or(0);
            let g = u8::from_str_radix(&hex[2..4], 16).unwrap_or(0);
            let b = u8::from_str_radix(&hex[4..6], 16).unwrap_or(0);
            (r, g, b, 255)
        } else {
            (0, 0, 0, 255)
        };
        let a_f = a as f32 / 255.0;
        Color::from_rgba(
            (r as f32 / 255.0) * a_f,
            (g as f32 / 255.0) * a_f,
            (b as f32 / 255.0) * a_f,
            a_f,
        )
        .unwrap()
    }
    pub fn draw_pixel(&mut self, params: PixelParams) {
        let x = params.x as f32;
        let y = params.y as f32;
        let color = Self::parse_color(&params.color);
        let mut paint = Paint::default();
        paint.set_color(color);
        if let Some(rect) = Rect::from_xywh(x, y, 1.0, 1.0) {
            self.pixmap
                .fill_rect(rect, &paint, Transform::identity(), None);
        }
    }
    pub fn draw_line(&mut self, params: LineParams) {
        let x1 = params.x1 as f32;
        let y1 = params.y1 as f32;
        let x2 = params.x2 as f32;
        let y2 = params.y2 as f32;
        let width = params.width;
        let color = Self::parse_color(&params.color);
        let dx = x2 - x1;
        let dy = y2 - y1;
        let length = (dx * dx + dy * dy).sqrt();
        if length == 0.0 {
            return;
        }
        let nx = -dy / length;
        let ny = dx / length;
        let (offset_x, offset_y) = match params.side {
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
            self.pixmap
                .stroke_path(&path, &paint, &stroke, Transform::identity(), None);
        }
    }
    pub fn draw_circle(&mut self, params: CircleParams) {
        let cx = params.x as f32;
        let cy = params.y as f32;
        let radius = params.radius;
        let fill_color = Self::parse_color(&params.fill_color);
        let outline_color = Self::parse_color(&params.outline_color);
        let outline_width = params.outline_width;
        let mut path = PathBuilder::new();
        path.push_circle(cx, cy, radius);
        if let Some(path) = path.finish() {
            let mut paint = Paint::default();
            paint.set_color(outline_color);
            let mut stroke = Stroke::default();
            stroke.width = outline_width;
            self.pixmap
                .stroke_path(&path, &paint, &stroke, Transform::identity(), None);
        }
        let inner_radius = radius - outline_width;
        if inner_radius > 0.0 {
            let mut inner_path = PathBuilder::new();
            inner_path.push_circle(cx, cy, inner_radius);
            if let Some(inner_path) = inner_path.finish() {
                let mut fill_paint = Paint::default();
                fill_paint.set_color(fill_color);
                self.pixmap.fill_path(
                    &inner_path,
                    &fill_paint,
                    FillRule::Winding,
                    Transform::identity(),
                    None,
                );
            }
        }
    }
    pub fn draw_rectangle(&mut self, params: RectangleParams) {
        let x1 = params.x1 as f32;
        let y1 = params.y1 as f32;
        let x2 = params.x2 as f32;
        let y2 = params.y2 as f32;
        let width = x2 - x1;
        let height = y2 - y1;
        let fill_color = Self::parse_color(&params.fill_color);
        let outline_color = Self::parse_color(&params.outline_color);
        let outline_width = params.outline_width;
        // Draw fill on the inner area
        let inner_x = x1 + outline_width;
        let inner_y = y1 + outline_width;
        let inner_width = width - 2.0 * outline_width;
        let inner_height = height - 2.0 * outline_width;
        if inner_width > 0.0 && inner_height > 0.0 {
            if let Some(inner_rect) = Rect::from_xywh(inner_x, inner_y, inner_width, inner_height) {
                let mut fill_paint = Paint::default();
                fill_paint.set_color(fill_color);
                self.pixmap
                    .fill_rect(inner_rect, &fill_paint, Transform::identity(), None);
            }
        }
        // Draw outline as filled border strips inside the rectangle
        let mut outline_paint = Paint::default();
        outline_paint.set_color(outline_color);
        // Left border
        if outline_width > 0.0 {
            if let Some(left_rect) = Rect::from_xywh(x1, y1, outline_width, height) {
                self.pixmap
                    .fill_rect(left_rect, &outline_paint, Transform::identity(), None);
            }
        }
        // Right border
        if outline_width > 0.0 {
            if let Some(right_rect) = Rect::from_xywh(x2 - outline_width, y1, outline_width, height)
            {
                self.pixmap
                    .fill_rect(right_rect, &outline_paint, Transform::identity(), None);
            }
        }
        // Top border (excluding corners already drawn)
        if outline_width > 0.0 {
            if let Some(top_rect) = Rect::from_xywh(
                x1 + outline_width,
                y1,
                width - 2.0 * outline_width,
                outline_width,
            ) {
                self.pixmap
                    .fill_rect(top_rect, &outline_paint, Transform::identity(), None);
            }
        }
        // Bottom border (excluding corners already drawn)
        if outline_width > 0.0 {
            if let Some(bottom_rect) = Rect::from_xywh(
                x1 + outline_width,
                y2 - outline_width,
                width - 2.0 * outline_width,
                outline_width,
            ) {
                self.pixmap
                    .fill_rect(bottom_rect, &outline_paint, Transform::identity(), None);
            }
        }
    }
}
