// Imports
use super::{CubicBezier, Shapeable};
use crate::Transformable;
use crate::ext::{DPose2Ext, KurboShapeExt, Vector2Ext};
use kurbo::Shape;
use p2d::bounding_volume::{Aabb, BoundingVolume};
use p2d::glamx::prelude::DPose2;
use p2d::math::Vector2;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, rename = "curve")]
/// A smooth curve passing through all of its points.
pub struct Curve {
    /// The curve start
    #[serde(rename = "start")]
    pub start: Vector2,
    /// The points the curve passes through after the start
    #[serde(rename = "path")]
    pub path: Vec<Vector2>,
}

impl Transformable for Curve {
    fn translate(&mut self, offset: Vector2) {
        self.start += offset;
        for p in &mut self.path {
            *p += offset;
        }
    }

    fn rotate(&mut self, angle: f64, center: Vector2) {
        let pose = DPose2::from_rotation_wrt_center(angle, center);
        self.start = pose.transform_point(self.start);
        for p in &mut self.path {
            *p = pose.transform_point(*p);
        }
    }

    fn scale(&mut self, scale: Vector2) {
        self.start *= scale;
        for p in &mut self.path {
            *p *= scale;
        }
    }
}

impl Shapeable for Curve {
    fn bounds(&self) -> Aabb {
        let mut bounds = Aabb::new(self.start, self.start);
        bounds.merge(&self.outline_path().bounding_box().bounds_to_p2d_aabb());
        bounds
    }

    fn hitboxes(&self) -> Vec<Aabb> {
        let segments = self.segments();
        if segments.is_empty() {
            return vec![Aabb::new(self.start, self.start)];
        }
        segments
            .iter()
            .flat_map(|cubbez| cubbez.hitboxes())
            .collect()
    }

    fn outline_path(&self) -> kurbo::BezPath {
        let mut bez_path = kurbo::BezPath::new();
        bez_path.move_to(self.start.to_kurbo_point());
        for cubbez in self.segments() {
            bez_path.curve_to(
                cubbez.cp1.to_kurbo_point(),
                cubbez.cp2.to_kurbo_point(),
                cubbez.end.to_kurbo_point(),
            );
        }
        bez_path
    }
}

impl Curve {
    /// A new curve
    pub fn new(start: Vector2) -> Self {
        Self {
            start,
            path: Vec::new(),
        }
    }

    /// The cubic bezier segments of the curve, created with the catmull-rom spline algorithm.
    ///
    /// The first and last points are duplicated so that the curve passes through all points.
    /// Consecutive duplicate points are skipped.
    pub fn segments(&self) -> Vec<CubicBezier> {
        let mut points = Vec::with_capacity(self.path.len() + 1);
        points.push(self.start);
        for p in &self.path {
            if points.last() != Some(p) {
                points.push(*p);
            }
        }
        if points.len() < 2 {
            return Vec::new();
        }

        let first = points[0];
        let last = points[points.len() - 1];
        let extended = std::iter::once(first)
            .chain(points.iter().copied())
            .chain(std::iter::once(last))
            .collect::<Vec<Vector2>>();

        extended
            .windows(4)
            .filter_map(|w| CubicBezier::new_w_catmull_rom(w[0], w[1], w[2], w[3]))
            .collect()
    }
}

impl Extend<Vector2> for Curve {
    fn extend<T: IntoIterator<Item = Vector2>>(&mut self, iter: T) {
        self.path.extend(iter);
    }
}
