// Imports
use super::Engine;
use crate::WidgetFlags;
use crate::pens::PenStyle;
use crate::store::StrokeKey;
use crate::strokes::Stroke;
use p2d::bounding_volume::Aabb;
use p2d::math::Vector2;
use std::time::Instant;

/// Touch interactions with images: tapping to select, dragging to move and pinching to resize.
impl Engine {
    /// The minimum width and height of an image while resizing it, in surface coordinates.
    const IMAGE_TOUCH_MIN_EXTENT: f64 = 16.0;

    /// Handles a finger tap at the given document position.
    ///
    /// Tapping an image selects it with the selector. Tapping outside a selection that consists only of
    /// images deselects it and switches back to the brush.
    pub fn image_touch_tap(&mut self, pos: Vector2) -> WidgetFlags {
        let mut widget_flags = WidgetFlags::default();

        if let Some(key) = self.image_at(pos) {
            let selection = self.store.selection_keys_as_rendered();
            if selection == [key] && self.current_pen_style_w_override() == PenStyle::Selector {
                return widget_flags;
            }

            widget_flags |= self.change_pen_style(PenStyle::Selector);
            self.store.set_selected_keys(&selection, false);
            self.store.set_selected(key, true);
            widget_flags |= self.current_pen_update_state();
            widget_flags.redraw = true;
        } else if self.selected_images_bounds().is_some() {
            let selection = self.store.selection_keys_as_rendered();
            widget_flags |= self.change_pen_style(PenStyle::Brush);
            self.store.set_selected_keys(&selection, false);
            widget_flags |= self.current_pen_update_state();
            widget_flags.redraw = true;
        }

        widget_flags
    }

    /// The bounds of the selection if it is not empty and consists only of images.
    pub fn selected_images_bounds(&self) -> Option<Aabb> {
        let selection = self.store.selection_keys_as_rendered();
        if selection.is_empty()
            || !selection
                .iter()
                .all(|&key| self.store.get_stroke_ref(key).is_some_and(is_image))
        {
            return None;
        }
        self.store.bounds_for_strokes(&selection)
    }

    /// Moves the selection by the given offset.
    ///
    /// `image_touch_finish()` must be called when the gesture ends.
    pub fn image_touch_translate(&mut self, offset: Vector2) -> WidgetFlags {
        let selection = self.store.selection_keys_as_rendered();
        if selection.is_empty() {
            return WidgetFlags::default();
        }
        self.store.translate_strokes(&selection, offset);
        self.store.translate_strokes_images(&selection, offset);

        let mut widget_flags = self.current_pen_update_state();
        widget_flags.redraw = true;
        widget_flags.store_modified = true;
        widget_flags
    }

    /// Scales the selection uniformly around the pivot, keeping a minimum size.
    ///
    /// `image_touch_finish()` must be called when the gesture ends.
    pub fn image_touch_scale(&mut self, scale: f64, pivot: Vector2) -> WidgetFlags {
        let selection = self.store.selection_keys_as_rendered();
        let Some(bounds) = self.store.bounds_for_strokes(&selection) else {
            return WidgetFlags::default();
        };
        let min_extent = Self::IMAGE_TOUCH_MIN_EXTENT / self.camera.total_zoom();
        if scale < 1.0 && bounds.extents().min_element() * scale < min_extent {
            return WidgetFlags::default();
        }
        let scale = Vector2::splat(scale);
        self.store
            .scale_strokes_with_pivot(&selection, scale, pivot);
        self.store
            .scale_strokes_images_with_pivot(&selection, scale, pivot);

        let mut widget_flags = self.current_pen_update_state();
        widget_flags.redraw = true;
        widget_flags.store_modified = true;
        widget_flags
    }

    /// Finishes moving or resizing the selection, updating its geometry and rendering and recording the change.
    pub fn image_touch_finish(&mut self) -> WidgetFlags {
        let selection = self.store.selection_keys_as_rendered();
        if selection.is_empty() {
            return WidgetFlags::default();
        }
        self.store.update_geometry_for_strokes(&selection);
        self.store.regenerate_rendering_for_strokes_threaded(
            self.tasks_tx.clone(),
            &selection,
            self.camera.viewport(),
            self.camera.image_scale(),
        );

        let mut widget_flags = self.current_pen_update_state()
            | self.doc_resize_autoexpand()
            | self.record(Instant::now());
        widget_flags.store_modified = true;
        widget_flags
    }

    /// The topmost image at the given document position.
    fn image_at(&self, pos: Vector2) -> Option<StrokeKey> {
        self.store
            .stroke_hitboxes_contain_coord(self.camera.viewport(), pos)
            .into_iter()
            .rev()
            .find(|&key| self.store.get_stroke_ref(key).is_some_and(is_image))
    }
}

fn is_image(stroke: &Stroke) -> bool {
    matches!(stroke, Stroke::BitmapImage(_) | Stroke::VectorImage(_))
}
