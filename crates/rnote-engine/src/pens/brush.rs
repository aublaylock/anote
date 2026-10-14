// Imports
use super::PenBehaviour;
use super::PenStyle;
use super::pensconfig::brushconfig::BrushStyle;
use crate::engine::{EngineTask, EngineView, EngineViewMut};
use crate::store::StrokeKey;
use crate::strokes::BrushStroke;
use crate::strokes::Stroke;
use crate::tasks::{OneOffTaskError, OneOffTaskHandle};
use crate::{DrawableOnDoc, WidgetFlags};
use p2d::bounding_volume::{Aabb, BoundingVolume};
use p2d::math::Vector2;
use piet::RenderContext;
use rnote_compose::Constraints;
use rnote_compose::PenPath;
use rnote_compose::Style;
use rnote_compose::builders::buildable::{Buildable, BuilderCreator, BuilderProgress};
use rnote_compose::builders::{
    PenPathBuilderType, PenPathCurvedBuilder, PenPathModeledBuilder, PenPathSimpleBuilder,
};
use rnote_compose::eventresult::{EventPropagation, EventResult};
use rnote_compose::penevent::{PenEvent, PenProgress};
use rnote_compose::penpath::{Element, Segment};
use std::time::{Duration, Instant};
use tracing::error;

#[derive(Debug)]
enum BrushState {
    Idle,
    Drawing {
        path_builder: Box<dyn Buildable<Emit = Segment>>,
        current_stroke_key: StrokeKey,
        preview_style: Box<Style>,
        /// The latest element received from the pen.
        last_element: Element,
        /// The position where the pen was last considered to be moving.
        hold_anchor: Vector2,
        /// The time the pen was last considered to be moving.
        hold_anchor_time: Instant,
        /// Fires when the pen is held still. Only present when auto line is enabled.
        hold_task: Option<OneOffTaskHandle>,
    },
    /// The stroke was converted to a straight line after the pen was held still.
    /// The line end follows the pen until it is lifted.
    StraightLine {
        current_stroke_key: StrokeKey,
        start: Element,
        pressure: f64,
    },
}

#[derive(Debug)]
pub struct Brush {
    state: BrushState,
}

impl Default for Brush {
    fn default() -> Self {
        Self {
            state: BrushState::Idle,
        }
    }
}

impl PenBehaviour for Brush {
    fn init(&mut self, _engine_view: &EngineView) -> WidgetFlags {
        WidgetFlags::default()
    }

    fn deinit(&mut self) -> WidgetFlags {
        WidgetFlags::default()
    }

    fn style(&self) -> PenStyle {
        PenStyle::Brush
    }

    fn update_state(&mut self, _engine_view: &mut EngineViewMut) -> WidgetFlags {
        WidgetFlags::default()
    }

    fn handle_event(
        &mut self,
        event: PenEvent,
        now: Instant,
        engine_view: &mut EngineViewMut,
    ) -> (EventResult<PenProgress>, WidgetFlags) {
        let mut widget_flags = WidgetFlags::default();

        let event_result = match (&mut self.state, event) {
            (BrushState::Idle, PenEvent::Down { element, .. }) => {
                if !element.filter_by_bounds(
                    engine_view
                        .document
                        .bounds()
                        .loosened(Self::INPUT_OVERSHOOT),
                ) {
                    #[cfg(feature = "ui")]
                    {
                        if engine_view.config.pens_config.brush_config.style == BrushStyle::Marker {
                            play_marker_sound(engine_view);
                        } else {
                            trigger_brush_sound(engine_view);
                        }
                    }

                    engine_view
                        .config
                        .pens_config
                        .brush_config
                        .new_style_seeds();

                    let preview_style = Self::get_preview_style(&engine_view.as_im());
                    let brushstroke =
                        Stroke::BrushStroke(BrushStroke::new(element, preview_style.clone()));

                    let current_stroke_key = engine_view.store.insert_stroke(
                        brushstroke,
                        Some(
                            engine_view
                                .config
                                .pens_config
                                .brush_config
                                .layer_for_current_options(),
                        ),
                    );

                    engine_view.store.regenerate_rendering_for_stroke(
                        current_stroke_key,
                        engine_view.camera.viewport(),
                        engine_view.camera.image_scale(),
                    );

                    let hold_task = engine_view
                        .config
                        .pens_config
                        .brush_config
                        .auto_line
                        .then(|| new_hold_task(engine_view, current_stroke_key));

                    self.state = BrushState::Drawing {
                        path_builder: new_builder(
                            engine_view.config.pens_config.brush_config.builder_type,
                            element,
                            now,
                        ),
                        current_stroke_key,
                        preview_style: Box::new(preview_style),
                        last_element: element,
                        hold_anchor: element.pos,
                        hold_anchor_time: now,
                        hold_task,
                    };

                    EventResult {
                        handled: true,
                        propagate: EventPropagation::Stop,
                        progress: PenProgress::InProgress,
                    }
                } else {
                    EventResult {
                        handled: false,
                        propagate: EventPropagation::Proceed,
                        progress: PenProgress::Idle,
                    }
                }
            }
            (BrushState::Idle, _) => EventResult {
                handled: false,
                propagate: EventPropagation::Proceed,
                progress: PenProgress::Idle,
            },
            (
                BrushState::Drawing {
                    current_stroke_key, ..
                }
                | BrushState::StraightLine {
                    current_stroke_key, ..
                },
                PenEvent::Cancel,
            ) => {
                widget_flags |= Self::finish_stroke(*current_stroke_key, engine_view);
                self.state = BrushState::Idle;

                EventResult {
                    handled: true,
                    propagate: EventPropagation::Stop,
                    progress: PenProgress::Finished,
                }
            }
            (
                BrushState::StraightLine {
                    current_stroke_key,
                    start,
                    pressure,
                },
                pen_event,
            ) => match pen_event {
                PenEvent::Down { element, .. } | PenEvent::Up { element, .. } => {
                    let finished = matches!(pen_event, PenEvent::Up { .. });
                    let end = Element::new(element.pos, *pressure);
                    widget_flags |=
                        Self::update_straight_line(*current_stroke_key, *start, end, engine_view);

                    if finished {
                        widget_flags |= Self::finish_stroke(*current_stroke_key, engine_view);
                        self.state = BrushState::Idle;
                    }

                    EventResult {
                        handled: true,
                        propagate: EventPropagation::Stop,
                        progress: if finished {
                            PenProgress::Finished
                        } else {
                            PenProgress::InProgress
                        },
                    }
                }
                _ => EventResult {
                    handled: true,
                    propagate: EventPropagation::Stop,
                    progress: PenProgress::InProgress,
                },
            },
            (
                BrushState::Drawing {
                    path_builder,
                    current_stroke_key,
                    last_element,
                    hold_anchor,
                    hold_anchor_time,
                    hold_task,
                    ..
                },
                pen_event,
            ) => {
                if let PenEvent::Down { element, .. } = pen_event {
                    *last_element = element;

                    // Restart the hold timer whenever the pen moves noticeably
                    if (element.pos - *hold_anchor).length()
                        > Self::HOLD_MOVE_THRESHOLD / engine_view.camera.total_zoom()
                    {
                        *hold_anchor = element.pos;
                        *hold_anchor_time = now;
                        if let Some(task) = hold_task {
                            match task.reset_timeout() {
                                Ok(()) => {}
                                Err(OneOffTaskError::TimeoutReached) => {
                                    *task = new_hold_task(engine_view, *current_stroke_key);
                                }
                                Err(e) => error!("Could not reset brush hold task, Err: {e:?}"),
                            }
                        }
                    }
                }

                let builder_result =
                    path_builder.handle_event(pen_event, now, Constraints::default());
                let handled = builder_result.handled;
                let propagate = builder_result.propagate;

                let progress = match builder_result.progress {
                    BuilderProgress::InProgress => {
                        #[cfg(feature = "ui")]
                        {
                            if engine_view.config.pens_config.brush_config.style
                                != BrushStyle::Marker
                            {
                                trigger_brush_sound(engine_view);
                            }
                        }

                        PenProgress::InProgress
                    }
                    BuilderProgress::EmitContinue(segments) => {
                        #[cfg(feature = "ui")]
                        {
                            if engine_view.config.pens_config.brush_config.style
                                != BrushStyle::Marker
                            {
                                trigger_brush_sound(engine_view);
                            }
                        }

                        let n_segments = segments.len();

                        if n_segments != 0 {
                            if let Some(Stroke::BrushStroke(brushstroke)) =
                                engine_view.store.get_stroke_mut(*current_stroke_key)
                            {
                                brushstroke.extend_w_segments(segments);
                                widget_flags.store_modified = true;
                            }

                            engine_view.store.append_rendering_last_segments(
                                engine_view.tasks_tx.clone(),
                                *current_stroke_key,
                                n_segments,
                                engine_view.camera.viewport(),
                                engine_view.camera.image_scale(),
                            );
                        }

                        PenProgress::InProgress
                    }
                    BuilderProgress::Finished(segments) => {
                        let n_segments = segments.len();

                        if n_segments != 0 {
                            if let Some(Stroke::BrushStroke(brushstroke)) =
                                engine_view.store.get_stroke_mut(*current_stroke_key)
                            {
                                brushstroke.extend_w_segments(segments);
                                widget_flags.store_modified = true;
                            }

                            engine_view.store.append_rendering_last_segments(
                                engine_view.tasks_tx.clone(),
                                *current_stroke_key,
                                n_segments,
                                engine_view.camera.viewport(),
                                engine_view.camera.image_scale(),
                            );
                        }

                        widget_flags |= Self::finish_stroke(*current_stroke_key, engine_view);
                        self.state = BrushState::Idle;

                        PenProgress::Finished
                    }
                };

                EventResult {
                    handled,
                    propagate,
                    progress,
                }
            }
        };

        (event_result, widget_flags)
    }
}

impl DrawableOnDoc for Brush {
    fn bounds_on_doc(&self, engine_view: &EngineView) -> Option<Aabb> {
        let style = engine_view
            .config
            .pens_config
            .brush_config
            .style_for_current_options();

        match &self.state {
            // The straight line is rendered by the stroke itself
            BrushState::Idle | BrushState::StraightLine { .. } => None,
            BrushState::Drawing { path_builder, .. } => {
                path_builder.bounds(&style, engine_view.camera.zoom())
            }
        }
    }

    fn draw_on_doc(
        &self,
        cx: &mut piet_cairo::CairoRenderContext,
        engine_view: &EngineView,
    ) -> anyhow::Result<()> {
        cx.save().map_err(|e| anyhow::anyhow!("{e:?}"))?;

        match &self.state {
            BrushState::Idle | BrushState::StraightLine { .. } => {}
            BrushState::Drawing {
                path_builder,
                preview_style,
                ..
            } => {
                match engine_view.config.pens_config.brush_config.style {
                    BrushStyle::Marker => {
                        // Don't draw the marker, as the pen would render on top of other strokes, while the stroke itself would render underneath them.
                    }
                    BrushStyle::Solid | BrushStyle::Textured => {
                        path_builder.draw_styled(
                            cx,
                            preview_style,
                            engine_view.camera.total_zoom(),
                        );
                    }
                }
            }
        }

        cx.restore().map_err(|e| anyhow::anyhow!("{e:?}"))?;
        Ok(())
    }
}

impl Brush {
    const INPUT_OVERSHOOT: f64 = 30.0;
    /// How long the pen must be held still before the stroke is converted to a straight line.
    const HOLD_DURATION: Duration = Duration::from_millis(500);
    /// Movement (in surface pixels) below which the pen is considered to be held still.
    const HOLD_MOVE_THRESHOLD: f64 = 4.0;
    /// The minimum length (in surface pixels) of a stroke for it to be converted to a straight line.
    const STRAIGHT_LINE_MIN_LENGTH: f64 = 30.0;
    /// The maximum allowed deviation from the straight line, relative to the line length.
    const STRAIGHT_LINE_MAX_DEVIATION_RATIO: f64 = 0.1;
    /// The maximum allowed ratio between the stroke's path length and the straight line length.
    /// Rejects strokes that go back and forth.
    const STRAIGHT_LINE_MAX_PATH_LENGTH_RATIO: f64 = 1.25;

    /// Handles the timeout of the hold task, converting the current stroke to a straight line
    /// if the pen was held still and the stroke is roughly straight.
    pub(crate) fn handle_hold_timeout(
        &mut self,
        stroke_key: StrokeKey,
        engine_view: &mut EngineViewMut,
    ) -> WidgetFlags {
        let mut widget_flags = WidgetFlags::default();
        let BrushState::Drawing {
            current_stroke_key,
            last_element,
            hold_anchor_time,
            ..
        } = &self.state
        else {
            return widget_flags;
        };
        // The timeout might belong to an earlier stroke or the pen might have moved in the meantime
        if *current_stroke_key != stroke_key
            || hold_anchor_time.elapsed() < Self::HOLD_DURATION
            || !engine_view.config.pens_config.brush_config.auto_line
        {
            return widget_flags;
        }
        let Some(Stroke::BrushStroke(brushstroke)) = engine_view.store.get_stroke_ref(stroke_key)
        else {
            return widget_flags;
        };

        let mut elements = brushstroke.path.clone().into_elements();
        elements.push(*last_element);
        let zoom = engine_view.camera.total_zoom();
        if !Self::is_straight(&elements, zoom) {
            return widget_flags;
        }

        let pressure = elements.iter().map(|e| e.pressure).sum::<f64>() / elements.len() as f64;
        let start = Element::new(elements[0].pos, pressure);
        let end = Element::new(last_element.pos, pressure);
        let current_stroke_key = *current_stroke_key;

        widget_flags |= Self::update_straight_line(current_stroke_key, start, end, engine_view);
        self.state = BrushState::StraightLine {
            current_stroke_key,
            start,
            pressure,
        };

        widget_flags
    }

    /// Whether the given elements roughly form a straight line.
    fn is_straight(elements: &[Element], zoom: f64) -> bool {
        let (Some(first), Some(last)) = (elements.first(), elements.last()) else {
            return false;
        };
        let chord = last.pos - first.pos;
        let chord_len = chord.length();
        if chord_len < Self::STRAIGHT_LINE_MIN_LENGTH / zoom {
            return false;
        }

        let max_deviation = elements
            .iter()
            .map(|e| distance_to_segment(e.pos, first.pos, last.pos))
            .fold(0.0, f64::max);
        let path_len = elements
            .windows(2)
            .map(|w| (w[1].pos - w[0].pos).length())
            .sum::<f64>();

        max_deviation <= chord_len * Self::STRAIGHT_LINE_MAX_DEVIATION_RATIO
            && path_len <= chord_len * Self::STRAIGHT_LINE_MAX_PATH_LENGTH_RATIO
    }

    /// Replaces the path of the stroke with a straight line and regenerates its rendering.
    fn update_straight_line(
        stroke_key: StrokeKey,
        start: Element,
        end: Element,
        engine_view: &mut EngineViewMut,
    ) -> WidgetFlags {
        let mut widget_flags = WidgetFlags::default();
        if let Some(Stroke::BrushStroke(brushstroke)) = engine_view.store.get_stroke_mut(stroke_key)
        {
            brushstroke.replace_path(PenPath::new_w_segments(start, [Segment::LineTo { end }]));
            engine_view.store.regenerate_rendering_for_stroke(
                stroke_key,
                engine_view.camera.viewport(),
                engine_view.camera.image_scale(),
            );
            widget_flags.redraw = true;
            widget_flags.store_modified = true;
        }
        widget_flags
    }

    /// Applies the final style to the stroke, regenerates its rendering and records the change.
    fn finish_stroke(stroke_key: StrokeKey, engine_view: &mut EngineViewMut) -> WidgetFlags {
        let mut widget_flags = WidgetFlags::default();

        if let Some(Stroke::BrushStroke(brushstroke)) = engine_view.store.get_stroke_mut(stroke_key)
        {
            brushstroke.style = engine_view
                .config
                .pens_config
                .brush_config
                .style_for_current_options();
        }

        // Finish up the last stroke
        engine_view.store.update_geometry_for_stroke(stroke_key);
        engine_view.store.regenerate_rendering_for_stroke_threaded(
            engine_view.tasks_tx.clone(),
            stroke_key,
            engine_view.camera.viewport(),
            engine_view.camera.image_scale(),
        );
        widget_flags |= engine_view
            .document
            .resize_autoexpand(engine_view.store, engine_view.camera);

        widget_flags |= engine_view.store.record(Instant::now());
        widget_flags.store_modified = true;

        widget_flags
    }

    fn get_preview_style(engine_view: &EngineView) -> Style {
        let mut style = engine_view
            .config
            .pens_config
            .brush_config
            .style_for_current_options();

        if let Some(mut stroke_color) = style.stroke_color() {
            stroke_color.a = 1.0;
            style.set_stroke_color(stroke_color);
        }

        style
    }
}

#[cfg(feature = "ui")]
fn play_marker_sound(engine_view: &mut EngineViewMut) {
    if let Some(audioplayer) = engine_view.audioplayer {
        audioplayer.play_random_marker_sound();
    }
}

#[cfg(feature = "ui")]
fn trigger_brush_sound(engine_view: &mut EngineViewMut) {
    if let Some(audioplayer) = engine_view.audioplayer.as_mut() {
        audioplayer.trigger_random_brush_sound();
    }
}

fn new_hold_task(engine_view: &EngineViewMut, stroke_key: StrokeKey) -> OneOffTaskHandle {
    let tasks_tx = engine_view.tasks_tx.clone();
    OneOffTaskHandle::new(
        move || tasks_tx.send(EngineTask::BrushHoldTimeout { stroke_key }),
        Brush::HOLD_DURATION,
    )
}

/// The distance from `pos` to the line segment between `start` and `end`.
fn distance_to_segment(pos: Vector2, start: Vector2, end: Vector2) -> f64 {
    let seg = end - start;
    let len_sq = seg.length_squared();
    if len_sq == 0.0 {
        return (pos - start).length();
    }
    let t = ((pos - start).dot(seg) / len_sq).clamp(0.0, 1.0);
    (pos - (start + seg * t)).length()
}

fn new_builder(
    builder_type: PenPathBuilderType,
    element: Element,
    now: Instant,
) -> Box<dyn Buildable<Emit = Segment>> {
    match builder_type {
        PenPathBuilderType::Simple => Box::new(PenPathSimpleBuilder::start(element, now)),
        PenPathBuilderType::Curved => Box::new(PenPathCurvedBuilder::start(element, now)),
        PenPathBuilderType::Modeled => Box::new(PenPathModeledBuilder::start(element, now)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn elements(points: &[(f64, f64)]) -> Vec<Element> {
        points
            .iter()
            .map(|&(x, y)| Element::new(Vector2::new(x, y), 0.5))
            .collect()
    }

    #[test]
    fn straight_stroke_is_detected() {
        let stroke = elements(&[
            (0.0, 0.0),
            (25.0, 1.0),
            (50.0, -2.0),
            (75.0, 1.5),
            (100.0, 0.0),
        ]);
        assert!(Brush::is_straight(&stroke, 1.0));
    }

    #[test]
    fn curved_stroke_is_rejected() {
        let stroke = elements(&[
            (0.0, 0.0),
            (25.0, 20.0),
            (50.0, 30.0),
            (75.0, 20.0),
            (100.0, 0.0),
        ]);
        assert!(!Brush::is_straight(&stroke, 1.0));
    }

    #[test]
    fn back_and_forth_stroke_is_rejected() {
        let stroke = elements(&[(0.0, 0.0), (100.0, 0.0), (20.0, 0.0), (100.0, 0.0)]);
        assert!(!Brush::is_straight(&stroke, 1.0));
    }

    #[test]
    fn short_stroke_is_rejected() {
        let stroke = elements(&[(0.0, 0.0), (10.0, 0.0)]);
        assert!(!Brush::is_straight(&stroke, 1.0));
        // At higher zoom levels, the same document length is longer on screen
        assert!(Brush::is_straight(&stroke, 4.0));
    }

    #[test]
    fn distance_to_segment_clamps_to_ends() {
        let start = Vector2::new(0.0, 0.0);
        let end = Vector2::new(10.0, 0.0);
        assert_eq!(distance_to_segment(Vector2::new(5.0, 3.0), start, end), 3.0);
        assert_eq!(
            distance_to_segment(Vector2::new(13.0, 4.0), start, end),
            5.0
        );
    }
}
