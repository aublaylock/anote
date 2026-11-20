// Imports
use crate::{RnAppWindow, RnCanvas, RnContextMenu, canvas::reject_pointer_input};
use gtk4::{
    CompositeTemplate, CornerType, EventControllerMotion, EventControllerScroll,
    EventControllerScrollFlags, EventSequenceState, GestureClick, GestureDrag, GestureLongPress,
    GestureZoom, PropagationPhase, ScrolledWindow, Widget, gdk, glib, glib::clone, graphene,
    prelude::*, subclass::prelude::*,
};
use once_cell::sync::Lazy;
use p2d::bounding_volume::Aabb;
use p2d::math::Vector2;
use rnote_compose::penevent::ShortcutKey;
use rnote_engine::Camera;
use rnote_engine::ext::GraphenePointExt;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

/// Touch pans and pinches that move further than this between two consecutive events, in surface coordinates,
/// are discarded as jumps. These are usually caused by a resting palm.
const TOUCH_MAX_STEP_DISTANCE: f64 = 100.0;
/// Pinches that change the scale by more than this factor between two consecutive events are discarded as jumps.
const PINCH_MAX_STEP_FACTOR: f64 = 1.3;

/// The maximum duration of a two finger tap.
const TWO_FINGER_TAP_MAX_DURATION: Duration = Duration::from_millis(300);
/// The maximum time between the two two-finger taps of a double tap.
const TWO_FINGER_DOUBLE_TAP_MAX_INTERVAL: Duration = Duration::from_millis(500);
/// The maximum movement of the center between the fingers during a two finger tap, in surface coordinates.
const TWO_FINGER_TAP_MAX_DISTANCE: f64 = 15.0;
/// The maximum distance between the two taps of a two finger double tap, in surface coordinates.
const TWO_FINGER_DOUBLE_TAP_MAX_DISTANCE: f64 = 60.0;
/// The maximum scale change during a two finger tap.
const TWO_FINGER_TAP_MAX_SCALE_CHANGE: f64 = 0.1;

/// State for detecting a two finger double tap, which pastes an image from the clipboard.
#[derive(Debug, Default)]
struct TwoFingerTapState {
    /// When and where the current two finger touch started, in wrapper coordinates.
    start: Cell<Option<(Instant, Vector2)>>,
    /// Whether the current two finger touch moved or scaled too much to be a tap.
    moved: Cell<bool>,
    /// When and where the last two finger tap happened, in wrapper coordinates.
    last_tap: Cell<Option<(Instant, Vector2)>>,
}

/// State of moving or resizing selected images with touch gestures.
#[derive(Debug, Default)]
struct ImageTouchState {
    /// When the current finger tap started.
    tap_start: Cell<Option<Instant>>,
    /// Whether a finger is moving the selected images.
    moving: Cell<bool>,
    /// The offset already applied while moving, in document coordinates.
    applied_offset: Cell<Vector2>,
    /// Whether a pinch is resizing the selected images.
    resizing: Cell<bool>,
}

#[derive(Debug, Default)]
struct Connections {
    appwindow_block_pinch_zoom_bind: Option<glib::Binding>,
    appwindow_show_scrollbars_bind: Option<glib::Binding>,
    appwindow_inertial_scrolling_bind: Option<glib::Binding>,
    appwindow_righthanded_bind: Option<glib::Binding>,
}

mod imp {
    use super::*;

    #[derive(Debug, CompositeTemplate)]
    #[template(resource = "/com/github/flxzt/rnote/ui/canvaswrapper.ui")]
    pub(crate) struct RnCanvasWrapper {
        pub(super) connections: RefCell<Connections>,
        pub(crate) canvas_touch_drawing_handler: RefCell<Option<glib::SignalHandlerId>>,
        pub(crate) show_scrollbars: Cell<bool>,
        pub(crate) block_pinch_zoom: Cell<bool>,
        pub(crate) inertial_scrolling: Cell<bool>,
        pub(crate) pointer_pos: Cell<Option<Vector2>>,
        pub(crate) last_contextmenu_pos: Cell<Option<Vector2>>,
        image_touch: ImageTouchState,
        two_finger_tap: TwoFingerTapState,

        pub(crate) pointer_motion_controller: EventControllerMotion,
        pub(crate) canvas_drag_gesture: GestureDrag,
        pub(crate) canvas_zoom_gesture: GestureZoom,
        pub(crate) canvas_multi_press_gesture: GestureClick,
        pub(crate) canvas_zoom_scroll_controller: EventControllerScroll,
        pub(crate) canvas_mouse_drag_middle_gesture: GestureDrag,
        pub(crate) canvas_alt_drag_gesture: GestureDrag,
        pub(crate) canvas_alt_shift_drag_gesture: GestureDrag,
        pub(crate) touch_two_finger_long_press_gesture: GestureLongPress,
        pub(crate) touch_long_press_gesture: GestureLongPress,
        pub(crate) touch_tap_gesture: GestureClick,
        pub(crate) touch_image_drag_gesture: GestureDrag,

        #[template_child]
        pub(crate) scroller: TemplateChild<ScrolledWindow>,
        #[template_child]
        pub(crate) canvas: TemplateChild<RnCanvas>,
        #[template_child]
        pub(crate) contextmenu: TemplateChild<RnContextMenu>,
    }

    impl Default for RnCanvasWrapper {
        fn default() -> Self {
            let pointer_motion_controller = EventControllerMotion::builder()
                .name("pointer_motion_controller")
                .propagation_phase(PropagationPhase::Capture)
                .build();

            // This allows touch dragging and dragging with pointer in the empty space around the canvas.
            // All relevant pointer events for drawing are captured and denied for propagation before they arrive at this gesture.
            let canvas_drag_gesture = GestureDrag::builder()
                .name("canvas_drag_gesture")
                .button(gdk::BUTTON_PRIMARY)
                .exclusive(true)
                .propagation_phase(PropagationPhase::Bubble)
                .build();

            let canvas_zoom_gesture = GestureZoom::builder()
                .name("canvas_zoom_gesture")
                .propagation_phase(PropagationPhase::Capture)
                .build();

            let canvas_multi_press_gesture = GestureClick::builder()
                .name("canvas_multi_press_gesture")
                .button(gdk::BUTTON_PRIMARY)
                .exclusive(true)
                .propagation_phase(PropagationPhase::Capture)
                .build();

            let canvas_zoom_scroll_controller = EventControllerScroll::builder()
                .name("canvas_zoom_scroll_controller")
                .propagation_phase(PropagationPhase::Bubble)
                .flags(EventControllerScrollFlags::VERTICAL)
                .build();

            let canvas_mouse_drag_middle_gesture = GestureDrag::builder()
                .name("canvas_mouse_drag_middle_gesture")
                .button(gdk::BUTTON_MIDDLE)
                .exclusive(true)
                .propagation_phase(PropagationPhase::Bubble)
                .build();

            // alt + drag for panning with pointer
            let canvas_alt_drag_gesture = GestureDrag::builder()
                .name("canvas_alt_drag_gesture")
                .button(gdk::BUTTON_PRIMARY)
                .exclusive(true)
                .propagation_phase(PropagationPhase::Capture)
                .build();

            // alt + shift + drag for zooming with pointer
            let canvas_alt_shift_drag_gesture = GestureDrag::builder()
                .name("canvas_alt_shift_drag_gesture")
                .button(gdk::BUTTON_PRIMARY)
                .exclusive(true)
                .propagation_phase(PropagationPhase::Capture)
                .build();

            let touch_two_finger_long_press_gesture = GestureLongPress::builder()
                .name("touch_two_finger_long_press_gesture")
                .touch_only(true)
                .n_points(2)
                // activate a bit quicker
                .delay_factor(0.8)
                .propagation_phase(PropagationPhase::Capture)
                .build();

            let touch_long_press_gesture = GestureLongPress::builder()
                .name("touch_long_press_gesture")
                .touch_only(true)
                .build();

            // Finger taps, to select images and deselect them
            let touch_tap_gesture = GestureClick::builder()
                .name("touch_tap_gesture")
                .touch_only(true)
                .propagation_phase(PropagationPhase::Capture)
                .build();

            // Moving selected images with one finger
            let touch_image_drag_gesture = GestureDrag::builder()
                .name("touch_image_drag_gesture")
                .touch_only(true)
                .propagation_phase(PropagationPhase::Capture)
                .build();

            Self {
                connections: RefCell::new(Connections::default()),
                canvas_touch_drawing_handler: RefCell::new(None),
                show_scrollbars: Cell::new(false),
                block_pinch_zoom: Cell::new(false),
                inertial_scrolling: Cell::new(true),
                pointer_pos: Cell::new(None),
                last_contextmenu_pos: Cell::new(None),
                image_touch: ImageTouchState::default(),
                two_finger_tap: TwoFingerTapState::default(),

                pointer_motion_controller,
                canvas_drag_gesture,
                canvas_zoom_gesture,
                canvas_multi_press_gesture,
                canvas_zoom_scroll_controller,
                canvas_mouse_drag_middle_gesture,
                canvas_alt_drag_gesture,
                canvas_alt_shift_drag_gesture,
                touch_two_finger_long_press_gesture,
                touch_long_press_gesture,
                touch_tap_gesture,
                touch_image_drag_gesture,

                scroller: TemplateChild::<ScrolledWindow>::default(),
                canvas: TemplateChild::<RnCanvas>::default(),
                contextmenu: TemplateChild::<RnContextMenu>::default(),
            }
        }
    }

    #[glib::object_subclass]
    impl ObjectSubclass for RnCanvasWrapper {
        const NAME: &'static str = "RnCanvasWrapper";
        type Type = super::RnCanvasWrapper;
        type ParentType = Widget;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for RnCanvasWrapper {
        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();

            // Add input controllers
            self.scroller
                .add_controller(self.pointer_motion_controller.clone());
            self.scroller
                .add_controller(self.canvas_drag_gesture.clone());
            self.scroller
                .add_controller(self.canvas_multi_press_gesture.clone());
            self.scroller
                .add_controller(self.canvas_zoom_scroll_controller.clone());
            self.scroller
                .add_controller(self.canvas_mouse_drag_middle_gesture.clone());
            self.scroller
                .add_controller(self.canvas_alt_drag_gesture.clone());
            self.scroller
                .add_controller(self.canvas_alt_shift_drag_gesture.clone());
            self.canvas
                .add_controller(self.touch_long_press_gesture.clone());
            // Touch gestures that need to take precedence over the scroller's own touch gestures are added to
            // this wrapper. As an ancestor of the scroller, it receives touch events first, and claiming a touch
            // here cancels it for the scroller.
            obj.add_controller(self.canvas_zoom_gesture.clone());
            obj.add_controller(self.touch_two_finger_long_press_gesture.clone());
            obj.add_controller(self.touch_tap_gesture.clone());
            obj.add_controller(self.touch_image_drag_gesture.clone());

            // group
            self.touch_two_finger_long_press_gesture
                .group_with(&self.canvas_zoom_gesture);
            // Grouped so that a finger moving an image can still be part of a pinch resizing it
            self.touch_image_drag_gesture
                .group_with(&self.canvas_zoom_gesture);

            self.setup_input();

            let canvas_touch_drawing_handler = self.canvas.connect_notify_local(
                Some("touch-drawing"),
                clone!(
                    #[weak(rename_to=canvaswrapper)]
                    obj,
                    move |_canvas, _pspec| {
                        // Disable the zoom gesture and kinetic scrolling when touch drawing is enabled.
                        canvaswrapper.imp().canvas_kinetic_scrolling_update();
                        canvaswrapper.imp().canvas_zoom_gesture_update();
                    }
                ),
            );

            self.canvas_touch_drawing_handler
                .replace(Some(canvas_touch_drawing_handler));
        }

        fn dispose(&self) {
            self.obj().disconnect_connections();

            if let Some(handler) = self.canvas_touch_drawing_handler.take() {
                self.canvas.disconnect(handler);
            }

            // the engine task handler needs to be be aborted here,
            // else a reference of the canvas is held forever in the handler causing a memory leak.
            self.canvas.abort_engine_task_handler();

            self.dispose_template();
            while let Some(child) = self.obj().first_child() {
                child.unparent();
            }
        }

        fn properties() -> &'static [glib::ParamSpec] {
            static PROPERTIES: Lazy<Vec<glib::ParamSpec>> = Lazy::new(|| {
                vec![
                    glib::ParamSpecBoolean::builder("show-scrollbars")
                        .default_value(false)
                        .build(),
                    glib::ParamSpecBoolean::builder("block-pinch-zoom")
                        .default_value(false)
                        .build(),
                    glib::ParamSpecBoolean::builder("inertial-scrolling")
                        .default_value(true)
                        .build(),
                ]
            });
            PROPERTIES.as_ref()
        }

        fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                "show-scrollbars" => self.show_scrollbars.get().to_value(),
                "block-pinch-zoom" => self.block_pinch_zoom.get().to_value(),
                "inertial-scrolling" => self.inertial_scrolling.get().to_value(),
                _ => unimplemented!(),
            }
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            match pspec.name() {
                "show-scrollbars" => {
                    let show_scrollbars = value
                        .get::<bool>()
                        .expect("The value needs to be of type `bool`");
                    self.show_scrollbars.replace(show_scrollbars);

                    self.scroller.hscrollbar().set_visible(show_scrollbars);
                    self.scroller.vscrollbar().set_visible(show_scrollbars);
                }
                "block-pinch-zoom" => {
                    let block_pinch_zoom = value
                        .get::<bool>()
                        .expect("The value needs to be of type `bool`");
                    self.block_pinch_zoom.replace(block_pinch_zoom);
                    self.canvas_zoom_gesture_update();
                }
                "inertial-scrolling" => {
                    let inertial_scrolling = value
                        .get::<bool>()
                        .expect("The value needs to be of type `bool`");

                    self.inertial_scrolling.replace(inertial_scrolling);
                    self.canvas_kinetic_scrolling_update();
                }
                _ => unimplemented!(),
            }
        }
    }

    impl WidgetImpl for RnCanvasWrapper {}

    impl RnCanvasWrapper {
        fn canvas_zoom_gesture_update(&self) {
            if !self.block_pinch_zoom.get() && !self.canvas.touch_drawing() {
                self.canvas_zoom_gesture
                    .set_propagation_phase(PropagationPhase::Capture);
            } else {
                self.canvas_zoom_gesture
                    .set_propagation_phase(PropagationPhase::None);
            }
        }

        fn canvas_kinetic_scrolling_update(&self) {
            self.scroller.set_kinetic_scrolling(
                !self.canvas.touch_drawing() && self.inertial_scrolling.get(),
            );
        }

        fn workaround_disable_kinetic_scrolling(&self) {
            let scroller = self.scroller.get();
            self.canvas
                .workaround_disable_kinetic_scrolling(Some(&scroller));
        }

        fn workaround_restore_kinetic_scrolling(&self) {
            let scroller = self.scroller.get();
            self.canvas
                .workaround_restore_kinetic_scrolling(Some(&scroller));
        }

        pub(super) fn workaround_cancel_kinetic_scrolling_for_zoom(&self) {
            self.workaround_disable_kinetic_scrolling();
            self.workaround_restore_kinetic_scrolling();
        }

        /// Converts a position on this wrapper to a document position.
        fn wrapper_pos_to_doc(&self, x: f64, y: f64) -> Option<Vector2> {
            let canvas = self.canvas.get();
            let pos = self
                .obj()
                .compute_point(&canvas, &graphene::Point::from_p2d_vec(Vector2::new(x, y)))?
                .to_p2d_vec();
            Some(
                canvas
                    .engine_ref()
                    .camera
                    .transform()
                    .inverse()
                    .transform_point2(pos),
            )
        }

        /// Whether the selection consists only of images and contains the given position on this wrapper.
        fn selected_images_contain(&self, x: f64, y: f64) -> Option<Aabb> {
            let pos = self.wrapper_pos_to_doc(x, y)?;
            let bounds = self.canvas.engine_ref().selected_images_bounds()?;
            bounds.contains_local_point(pos).then_some(bounds)
        }

        /// Handles the end of a two finger touch, pasting an image from the clipboard on a two finger double tap.
        fn two_finger_touch_end(&self) {
            let state = &self.two_finger_tap;
            let Some((start_time, center)) = state.start.take() else {
                return;
            };
            if state.moved.get() || start_time.elapsed() > TWO_FINGER_TAP_MAX_DURATION {
                state.last_tap.set(None);
                return;
            }
            let now = Instant::now();
            let is_double_tap = state
                .last_tap
                .take()
                .is_some_and(|(last_time, last_center)| {
                    now.saturating_duration_since(last_time) <= TWO_FINGER_DOUBLE_TAP_MAX_INTERVAL
                        && (center - last_center).length() <= TWO_FINGER_DOUBLE_TAP_MAX_DISTANCE
                });
            if !is_double_tap {
                state.last_tap.set(Some((now, center)));
                return;
            }
            if let Some(pos) = self.wrapper_pos_to_doc(center.x, center.y) {
                self.paste_clipboard_image_at(pos);
            }
        }

        /// Pastes the image on the clipboard centered at the given document position.
        ///
        /// Does nothing if the clipboard does not contain an image.
        fn paste_clipboard_image_at(&self, pos: Vector2) {
            let obj = self.obj();
            let clipboard = obj.clipboard();
            let formats = clipboard.formats();
            if ![
                "image/png",
                "image/jpeg",
                "image/jpg",
                "image/tiff",
                "image/bmp",
            ]
            .into_iter()
            .any(|mime| formats.contain_mime_type(mime))
            {
                return;
            }
            let respect_borders = obj
                .root()
                .and_downcast::<RnAppWindow>()
                .is_some_and(|appwindow| appwindow.respect_borders());
            let canvas = self.canvas.get();

            glib::spawn_future_local(clone!(
                #[weak]
                canvas,
                async move {
                    match clipboard.read_texture_future().await {
                        Ok(Some(texture)) => {
                            if let Err(e) = canvas
                                .load_in_bitmapimage_bytes_centered(
                                    texture.save_to_png_bytes().to_vec(),
                                    pos,
                                    respect_borders,
                                )
                                .await
                            {
                                tracing::error!(
                                    "Pasting clipboard image after two finger double tap failed, Err: {e:?}"
                                );
                            }
                        }
                        Ok(None) => {}
                        Err(e) => tracing::error!(
                            "Reading clipboard image after two finger double tap failed, Err: {e:?}"
                        ),
                    }
                }
            ));
        }

        /// Finishes moving or resizing images with touch, if active.
        fn image_touch_finish(&self) {
            let moved = self.image_touch.moving.replace(false);
            let resized = self.image_touch.resizing.replace(false);
            if moved {
                self.workaround_restore_kinetic_scrolling();
            }
            if moved || resized {
                let widget_flags = self.canvas.engine_mut().image_touch_finish();
                self.canvas.emit_handle_widget_flags(widget_flags);
            }
        }

        fn setup_input(&self) {
            let obj = self.obj();

            {
                self.pointer_motion_controller.connect_motion(clone!(
                    #[weak(rename_to=canvaswrapper)]
                    obj,
                    move |_, x, y| {
                        canvaswrapper
                            .imp()
                            .pointer_pos
                            .set(Some(Vector2::new(x, y)));
                    }
                ));

                self.pointer_motion_controller.connect_leave(clone!(
                    #[weak(rename_to=canvaswrapper)]
                    obj,
                    move |_| {
                        canvaswrapper.imp().pointer_pos.set(None);
                    }
                ));
            }

            // zoom scrolling with <ctrl> + scroll
            {
                self.canvas_zoom_scroll_controller.connect_scroll(clone!(
                    #[weak(rename_to=canvaswrapper)]
                    obj,
                    #[upgrade_or]
                    glib::Propagation::Proceed,
                    move |controller, _, dy| {
                        let modifiers = controller.current_event_state();

                        if !modifiers.contains(gdk::ModifierType::CONTROL_MASK) {
                            return glib::Propagation::Proceed;
                        }

                        // workaround for https://gitlab.gnome.org/GNOME/gtk/-/issues/187
                        canvaswrapper
                            .imp()
                            .workaround_cancel_kinetic_scrolling_for_zoom();

                        let canvas = canvaswrapper.canvas();
                        let old_zoom = canvas.engine_ref().camera.total_zoom();
                        let new_zoom = if dy < 0.0 {
                            old_zoom * (1.0 - dy * RnCanvas::ZOOM_SCROLL_STEP)
                        } else {
                            old_zoom * (1.0 / (1.0 + dy * RnCanvas::ZOOM_SCROLL_STEP))
                        };

                        if (Camera::ZOOM_MIN..=Camera::ZOOM_MAX).contains(&new_zoom) {
                            let camera_offset = canvas.engine_ref().camera.offset();
                            let camera_size = canvas.engine_ref().camera.size();
                            let screen_offset = canvaswrapper
                                .imp()
                                .pointer_pos
                                .get()
                                .map(|p| {
                                    let p = canvaswrapper
                                        .compute_point(&canvas, &graphene::Point::from_p2d_vec(p))
                                        .unwrap();
                                    p.to_p2d_vec()
                                })
                                .unwrap_or_else(|| camera_size * 0.5);
                            let new_camera_offset = (((camera_offset + screen_offset) / old_zoom)
                                * new_zoom)
                                - screen_offset;

                            let mut widget_flags = canvas.engine_mut().zoom_w_timeout(new_zoom);
                            widget_flags |= canvas
                                .engine_mut()
                                .camera_set_offset_expand(new_camera_offset);
                            canvas.emit_handle_widget_flags(widget_flags);
                        }

                        glib::Propagation::Stop
                    }
                ));
            }

            // Finger taps select images, or deselect them and switch back to the brush
            {
                const TOUCH_TAP_MAX_DURATION: Duration = Duration::from_millis(400);

                self.touch_tap_gesture.connect_pressed(clone!(
                    #[weak(rename_to=canvaswrapper)]
                    obj,
                    move |_, _, _, _| {
                        canvaswrapper
                            .imp()
                            .image_touch
                            .tap_start
                            .set(Some(Instant::now()));
                    }
                ));
                self.touch_tap_gesture.connect_released(clone!(
                    #[weak(rename_to=canvaswrapper)]
                    obj,
                    move |_, _, x, y| {
                        let imp = canvaswrapper.imp();
                        let Some(tap_start) = imp.image_touch.tap_start.take() else {
                            return;
                        };
                        let canvas = canvaswrapper.canvas();
                        if tap_start.elapsed() > TOUCH_TAP_MAX_DURATION || canvas.touch_drawing() {
                            return;
                        }
                        let Some(pos) = imp.wrapper_pos_to_doc(x, y) else {
                            return;
                        };
                        let widget_flags = canvas.engine_mut().image_touch_tap(pos);
                        canvas.emit_handle_widget_flags(widget_flags);
                    }
                ));
            }

            // Move selected images with one finger
            {
                self.touch_image_drag_gesture.connect_drag_begin(clone!(
                    #[weak(rename_to=canvaswrapper)]
                    obj,
                    move |gesture, x, y| {
                        let imp = canvaswrapper.imp();
                        imp.image_touch.moving.set(false);
                        if canvaswrapper.canvas().touch_drawing()
                            || imp.selected_images_contain(x, y).is_none()
                        {
                            // Not claiming, so panning and zooming keep working
                            return;
                        }
                        // Claiming and disabling kinetic scrolling prevent the view from panning while the
                        // image is moved
                        gesture.set_state(EventSequenceState::Claimed);
                        imp.workaround_disable_kinetic_scrolling();
                        imp.image_touch.moving.set(true);
                        imp.image_touch.applied_offset.set(Vector2::ZERO);
                    }
                ));
                self.touch_image_drag_gesture.connect_drag_update(clone!(
                    #[weak(rename_to=canvaswrapper)]
                    obj,
                    move |_, x, y| {
                        let imp = canvaswrapper.imp();
                        if !imp.image_touch.moving.get() || imp.image_touch.resizing.get() {
                            return;
                        }
                        let canvas = canvaswrapper.canvas();
                        let offset = Vector2::new(x, y) / canvas.engine_ref().camera.total_zoom();
                        let delta = offset - imp.image_touch.applied_offset.replace(offset);
                        let widget_flags = canvas.engine_mut().image_touch_translate(delta);
                        canvas.emit_handle_widget_flags(widget_flags);
                    }
                ));
                self.touch_image_drag_gesture.connect_drag_end(clone!(
                    #[weak(rename_to=canvaswrapper)]
                    obj,
                    move |_, _, _| {
                        let imp = canvaswrapper.imp();
                        // A pinch that took over finishes when it ends
                        if !imp.image_touch.resizing.get() {
                            imp.image_touch_finish();
                        }
                    }
                ));
                self.touch_image_drag_gesture.connect_cancel(clone!(
                    #[weak(rename_to=canvaswrapper)]
                    obj,
                    move |_, _| {
                        let imp = canvaswrapper.imp();
                        if !imp.image_touch.resizing.get() {
                            imp.image_touch_finish();
                        }
                    }
                ));
            }

            // Drag canvas gesture
            {
                let touch_drag_start = Rc::new(Cell::new(Vector2::ZERO));
                // The drag offset of the previous event and the sum of discarded jumps
                let touch_drag_prev = Rc::new(Cell::new(Vector2::ZERO));
                let touch_drag_discarded = Rc::new(Cell::new(Vector2::ZERO));

                self.canvas_drag_gesture.connect_drag_begin(clone!(
                    #[strong]
                    touch_drag_start,
                    #[strong]
                    touch_drag_prev,
                    #[strong]
                    touch_drag_discarded,
                    #[weak(rename_to=canvaswrapper)]
                    obj,
                    move |gesture, _, _| {
                        // We don't claim the sequence, because we we want to allow touch zooming.
                        // When the zoom gesture is recognized, it claims it and denies this touch drag gesture.

                        if let Some(event) = gesture.current_event()
                            && event.device_tool().is_some()
                        {
                            let pressure = event.axis(gdk::AxisUse::Pressure);
                            if pressure.is_some_and(|p| p <= 0.0) {
                                gesture.set_state(EventSequenceState::Denied);
                                return;
                            }
                        }

                        touch_drag_start.set(canvaswrapper.canvas().engine_ref().camera.offset());
                        touch_drag_prev.set(Vector2::ZERO);
                        touch_drag_discarded.set(Vector2::ZERO);
                    }
                ));
                self.canvas_drag_gesture.connect_drag_update(clone!(
                    #[strong]
                    touch_drag_start,
                    #[strong]
                    touch_drag_prev,
                    #[strong]
                    touch_drag_discarded,
                    #[weak(rename_to=canvaswrapper)]
                    obj,
                    move |_, x, y| {
                        // Don't pan while a finger moves selected images
                        if canvaswrapper.imp().image_touch.moving.get() {
                            return;
                        }
                        // Discard sudden jumps, which are usually caused by a resting palm
                        let drag = Vector2::new(x, y);
                        let step = drag - touch_drag_prev.replace(drag);
                        if step.length() > TOUCH_MAX_STEP_DISTANCE {
                            touch_drag_discarded.set(touch_drag_discarded.get() + step);
                            return;
                        }
                        let canvas = canvaswrapper.canvas();
                        let new_offset =
                            touch_drag_start.get() - (drag - touch_drag_discarded.get());
                        let widget_flags = canvas.engine_mut().camera_set_offset_expand(new_offset);
                        canvas.emit_handle_widget_flags(widget_flags);
                    }
                ));
                self.canvas_drag_gesture.connect_drag_end(clone!(
                    #[weak(rename_to=canvaswrapper)]
                    obj,
                    move |_, _, _| {
                        let widget_flags = canvaswrapper
                            .canvas()
                            .engine_mut()
                            .update_rendering_current_viewport();
                        canvaswrapper
                            .canvas()
                            .emit_handle_widget_flags(widget_flags);
                    }
                ));
            }

            // Move Canvas with middle mouse button
            {
                let mouse_drag_start = Rc::new(Cell::new(Vector2::ZERO));

                self.canvas_mouse_drag_middle_gesture
                    .connect_drag_begin(clone!(
                        #[strong]
                        mouse_drag_start,
                        #[weak(rename_to=canvaswrapper)]
                        obj,
                        move |gesture, _, _| {
                            // Stylus hover can trigger button events; ignore pan without pressure.
                            if let Some(event) = gesture.current_event()
                                && event.device_tool().is_some()
                            {
                                let pressure = event.axis(gdk::AxisUse::Pressure);
                                if pressure.is_some_and(|p| p <= 0.0) {
                                    gesture.set_state(EventSequenceState::Denied);
                                    return;
                                }
                            }

                            mouse_drag_start
                                .set(canvaswrapper.canvas().engine_ref().camera.offset());
                        }
                    ));
                self.canvas_mouse_drag_middle_gesture
                    .connect_drag_update(clone!(
                        #[strong]
                        mouse_drag_start,
                        #[weak(rename_to=canvaswrapper)]
                        obj,
                        move |_, x, y| {
                            let canvas = canvaswrapper.canvas();
                            let new_offset = mouse_drag_start.get() - Vector2::new(x, y);
                            let widget_flags =
                                canvas.engine_mut().camera_set_offset_expand(new_offset);
                            canvas.emit_handle_widget_flags(widget_flags);
                        }
                    ));
                self.canvas_mouse_drag_middle_gesture
                    .connect_drag_end(clone!(
                        #[weak(rename_to=canvaswrapper)]
                        obj,
                        move |_, _, _| {
                            let widget_flags = canvaswrapper
                                .canvas()
                                .engine_mut()
                                .update_rendering_current_viewport();
                            canvaswrapper
                                .canvas()
                                .emit_handle_widget_flags(widget_flags);
                        }
                    ));
            }

            // Canvas gesture zooming with dragging
            {
                // The pinch is handled in steps between consecutive events, so that sudden jumps can be
                // discarded.
                let prev_scale = Rc::new(Cell::new(1_f64));
                let prev_center: Rc<Cell<Option<Vector2>>> = Rc::new(Cell::new(None));
                let new_zoom = Rc::new(Cell::new(1.0));

                self.canvas_zoom_gesture.connect_begin(clone!(
                    #[strong]
                    prev_scale,
                    #[strong]
                    prev_center,
                    #[strong]
                    new_zoom,
                    #[weak(rename_to=canvaswrapper)]
                    obj,
                    move |gesture, _| {
                        gesture.set_state(EventSequenceState::Claimed);

                        // workaround for https://gitlab.gnome.org/GNOME/gtk/-/issues/187
                        canvaswrapper.imp().workaround_disable_kinetic_scrolling();

                        let center = gesture
                            .bounding_box_center()
                            .map(|(x, y)| Vector2::new(x, y));
                        let imp = canvaswrapper.imp();
                        imp.two_finger_tap
                            .start
                            .set(center.map(|center| (Instant::now(), center)));
                        imp.two_finger_tap.moved.set(false);
                        prev_scale.set(1.0);
                        prev_center.set(center);
                        new_zoom.set(canvaswrapper.canvas().engine_ref().camera.total_zoom());

                        // Pinching on selected images resizes them instead of zooming
                        if let Some(center) = center
                            && imp.selected_images_contain(center.x, center.y).is_some()
                        {
                            imp.image_touch.resizing.set(true);
                        }
                    }
                ));

                self.canvas_zoom_gesture.connect_scale_changed(clone!(
                    #[strong]
                    prev_scale,
                    #[strong]
                    prev_center,
                    #[strong]
                    new_zoom,
                    #[weak(rename_to=canvaswrapper)]
                    obj,
                    move |gesture, scale| {
                        let canvas = canvaswrapper.canvas();
                        let imp = canvaswrapper.imp();

                        let Some(center) = gesture
                            .bounding_box_center()
                            .map(|(x, y)| Vector2::new(x, y))
                        else {
                            return;
                        };
                        // Too much movement or scaling for a two finger tap
                        if let Some((_, start_center)) = imp.two_finger_tap.start.get()
                            && ((scale - 1.0).abs() > TWO_FINGER_TAP_MAX_SCALE_CHANGE
                                || (center - start_center).length() > TWO_FINGER_TAP_MAX_DISTANCE)
                        {
                            imp.two_finger_tap.moved.set(true);
                        }

                        let factor = scale / prev_scale.replace(scale);
                        let Some(prev_center) = prev_center.replace(Some(center)) else {
                            return;
                        };
                        let center_delta = center - prev_center;

                        // Discard sudden jumps, which are usually caused by a resting palm touching or
                        // leaving the screen. The pinch continues from the new finger positions.
                        if !(1.0 / PINCH_MAX_STEP_FACTOR..=PINCH_MAX_STEP_FACTOR).contains(&factor)
                            || center_delta.length() > TOUCH_MAX_STEP_DISTANCE
                        {
                            return;
                        }

                        if imp.image_touch.resizing.get() {
                            // Move along with the center between the fingers
                            let delta = center_delta / canvas.engine_ref().camera.total_zoom();
                            let widget_flags = canvas.engine_mut().image_touch_translate(delta);
                            canvas.emit_handle_widget_flags(widget_flags);

                            // Resize around the center between the fingers
                            if let Some(pivot) = imp.wrapper_pos_to_doc(center.x, center.y) {
                                let widget_flags =
                                    canvas.engine_mut().image_touch_scale(factor, pivot);
                                canvas.emit_handle_widget_flags(widget_flags);
                            }
                            return;
                        }

                        let old_zoom = new_zoom.get();
                        let zoom = (old_zoom * factor).clamp(Camera::ZOOM_MIN, Camera::ZOOM_MAX);
                        new_zoom.set(zoom);
                        let offset = canvas.engine_ref().camera.offset();

                        let mut widget_flags = canvas.engine_mut().zoom_w_timeout(zoom);
                        // Keep the document point under the previous center under the current center
                        let new_offset = (offset + prev_center) * (zoom / old_zoom) - center;
                        widget_flags |= canvas.engine_mut().camera_set_offset_expand(new_offset);

                        canvas.emit_handle_widget_flags(widget_flags);
                    }
                ));

                self.canvas_zoom_gesture.connect_end(clone!(
                    #[weak(rename_to=canvaswrapper)]
                    obj,
                    move |_gesture, _event_sequence| {
                        // workaround for https://gitlab.gnome.org/GNOME/gtk/-/issues/187
                        canvaswrapper.imp().workaround_restore_kinetic_scrolling();
                        canvaswrapper.imp().image_touch_finish();
                        canvaswrapper.imp().two_finger_touch_end();

                        let widget_flags = canvaswrapper
                            .canvas()
                            .engine_mut()
                            .update_rendering_current_viewport();
                        canvaswrapper
                            .canvas()
                            .emit_handle_widget_flags(widget_flags);
                    }
                ));

                self.canvas_zoom_gesture.connect_cancel(clone!(
                    #[weak(rename_to=canvaswrapper)]
                    obj,
                    move |gesture, _event_sequence| {
                        gesture.set_state(EventSequenceState::Denied);

                        // workaround for https://gitlab.gnome.org/GNOME/gtk/-/issues/187
                        canvaswrapper.imp().workaround_restore_kinetic_scrolling();
                        canvaswrapper.imp().image_touch_finish();
                        canvaswrapper.imp().two_finger_tap.start.set(None);

                        let widget_flags = canvaswrapper
                            .canvas()
                            .engine_mut()
                            .update_rendering_current_viewport();
                        canvaswrapper
                            .canvas()
                            .emit_handle_widget_flags(widget_flags);
                    }
                ));
            }

            // Pan with alt + drag
            {
                let offset_start = Rc::new(Cell::new(Vector2::ZERO));

                self.canvas_alt_drag_gesture.connect_drag_begin(clone!(
                    #[strong]
                    offset_start,
                    #[weak(rename_to=canvaswrapper)]
                    obj,
                    move |gesture, _, _| {
                        let modifiers = gesture.current_event_state();

                        // At the start BUTTON1_MASK is not included
                        if modifiers.contains(gdk::ModifierType::ALT_MASK)
                            && !modifiers.contains(gdk::ModifierType::SHIFT_MASK)
                        {
                            gesture.set_state(EventSequenceState::Claimed);
                            offset_start.set(canvaswrapper.canvas().engine_ref().camera.offset());
                        } else {
                            gesture.set_state(EventSequenceState::Denied);
                        }
                    }
                ));

                self.canvas_alt_drag_gesture.connect_drag_update(clone!(
                    #[strong]
                    offset_start,
                    #[weak(rename_to=canvaswrapper)]
                    obj,
                    move |_, offset_x, offset_y| {
                        let canvas = canvaswrapper.canvas();
                        let new_offset = offset_start.get() - Vector2::new(offset_x, offset_y);
                        let widget_flags = canvas.engine_mut().camera_set_offset_expand(new_offset);
                        canvas.emit_handle_widget_flags(widget_flags);
                    }
                ));

                self.canvas_alt_drag_gesture.connect_drag_end(clone!(
                    #[weak(rename_to=canvaswrapper)]
                    obj,
                    move |_, _, _| {
                        let widget_flags = canvaswrapper
                            .canvas()
                            .engine_mut()
                            .update_rendering_current_viewport();
                        canvaswrapper
                            .canvas()
                            .emit_handle_widget_flags(widget_flags);
                    }
                ));
            }

            // Double press to select word, triple press to select line
            {
                self.canvas_multi_press_gesture.connect_pressed(clone!(
                    #[weak(rename_to=canvaswrapper)]
                    obj,
                    move |signal, n_press, _, _| {
                        // cycle through 0, 1, 2 - single, double, triple press
                        let action = (n_press - 1) % 3;

                        if action <= 0 {
                            // Single press or invalid press count
                            return;
                        }

                        let canvas = canvaswrapper.canvas();

                        if signal.current_event().is_none_or(|event| {
                            reject_pointer_input(&event, canvas.touch_drawing())
                        }) {
                            // Reject certain kinds of input (same behavior as canvas)
                            return;
                        }

                        match action {
                            // Double press
                            1 => canvas.engine_mut().text_select_closest_word(),
                            // Triple press
                            2 => canvas.engine_mut().text_select_closest_line(),
                            _ => unreachable!(),
                        }
                    }
                ));
            }

            // Zoom with alt + shift + drag
            {
                let zoom_begin = Rc::new(Cell::new(1_f64));
                let prev_offset = Rc::new(Cell::new(Vector2::ZERO));

                self.canvas_alt_shift_drag_gesture
                    .connect_drag_begin(clone!(
                        #[strong]
                        zoom_begin,
                        #[strong]
                        prev_offset,
                        #[weak(rename_to=canvaswrapper)]
                        obj,
                        move |gesture, _, _| {
                            let modifiers = gesture.current_event_state();

                            // At the start BUTTON1_MASK is not included
                            if modifiers.contains(gdk::ModifierType::ALT_MASK)
                                && modifiers.contains(gdk::ModifierType::SHIFT_MASK)
                            {
                                gesture.set_state(EventSequenceState::Claimed);

                                // workaround for https://gitlab.gnome.org/GNOME/gtk/-/issues/187
                                canvaswrapper.imp().workaround_disable_kinetic_scrolling();

                                let current_zoom =
                                    canvaswrapper.canvas().engine_ref().camera.total_zoom();
                                zoom_begin.set(current_zoom);
                                prev_offset.set(Vector2::ZERO);
                            } else {
                                gesture.set_state(EventSequenceState::Denied);
                            }
                        }
                    ));

                self.canvas_alt_shift_drag_gesture
                    .connect_drag_update(clone!(
                        #[strong]
                        prev_offset,
                        #[weak(rename_to=canvaswrapper)]
                        obj,
                        move |_, offset_x, offset_y| {
                            let canvas = canvaswrapper.canvas();
                            let new_offset = Vector2::new(offset_x, offset_y);
                            let current_total_zoom =
                                canvaswrapper.canvas().engine_ref().camera.total_zoom();
                            // drag down zooms out, drag up zooms in
                            let new_zoom = current_total_zoom
                                * (1.0
                                    - (new_offset[1] - prev_offset.get()[1])
                                        * Camera::DRAG_ZOOM_MAGN_ZOOM_FACTOR);

                            if (Camera::ZOOM_MIN..=Camera::ZOOM_MAX).contains(&new_zoom) {
                                let viewport_center = canvas.engine_ref().camera.viewport_center();

                                let mut widget_flags = canvas.engine_mut().zoom_w_timeout(new_zoom);
                                widget_flags |= canvas
                                    .engine_mut()
                                    .camera
                                    .set_viewport_center(viewport_center);
                                widget_flags |= canvas.engine_mut().doc_expand_autoexpand();
                                canvas.emit_handle_widget_flags(widget_flags);
                            }

                            prev_offset.set(new_offset);
                        }
                    ));

                self.canvas_alt_shift_drag_gesture.connect_drag_end(clone!(
                    #[weak(rename_to=canvaswrapper)]
                    obj,
                    move |_, _, _| {
                        // workaround for https://gitlab.gnome.org/GNOME/gtk/-/issues/187
                        canvaswrapper.imp().workaround_restore_kinetic_scrolling();

                        let widget_flags = canvaswrapper
                            .canvas()
                            .engine_mut()
                            .update_rendering_current_viewport();
                        canvaswrapper
                            .canvas()
                            .emit_handle_widget_flags(widget_flags);
                    }
                ));
            }

            {
                // Shortcut with touch two-finger long-press.
                self.touch_two_finger_long_press_gesture
                    .connect_pressed(clone!(
                        #[weak(rename_to=canvaswrapper)]
                        obj,
                        move |_gesture, _, _| {
                            let (_, widget_flags) = canvaswrapper
                                .canvas()
                                .engine_mut()
                                .handle_pressed_shortcut_key(
                                    ShortcutKey::TouchTwoFingerLongPress,
                                    Instant::now(),
                                );
                            canvaswrapper
                                .canvas()
                                .emit_handle_widget_flags(widget_flags);
                        }
                    ));
            }

            {
                // Context menu
                self.touch_long_press_gesture.connect_pressed(clone!(
                    #[weak(rename_to=canvaswrapper)]
                    obj,
                    move |_gesture, x, y| {
                        let enabled = canvaswrapper
                            .root()
                            .and_downcast::<RnAppWindow>()
                            .and_then(|appwindow| appwindow.app().app_settings())
                            .is_none_or(|settings| {
                                settings.boolean("touch-long-press-contextmenu")
                            });
                        if !enabled {
                            return;
                        }
                        let popover = canvaswrapper.contextmenu().popover();
                        canvaswrapper
                            .imp()
                            .last_contextmenu_pos
                            .set(Some(Vector2::new(x, y)));
                        popover
                            .set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 4, 4)));
                        popover.popup();
                    }
                ));
            }
        }
    }
}

glib::wrapper! {
    pub(crate) struct RnCanvasWrapper(ObjectSubclass<imp::RnCanvasWrapper>)
        @extends Widget,
        @implements gtk4::Accessible, gtk4::Buildable, gtk4::ConstraintTarget;
}

impl Default for RnCanvasWrapper {
    fn default() -> Self {
        Self::new()
    }
}

impl RnCanvasWrapper {
    pub(crate) fn new() -> Self {
        glib::Object::new()
    }

    #[allow(unused)]
    pub(crate) fn show_scrollbars(&self) -> bool {
        self.property::<bool>("show-scrollbars")
    }

    #[allow(unused)]
    pub(crate) fn set_show_scrollbars(&self, show_scrollbars: bool) {
        self.set_property("show-scrollbars", show_scrollbars.to_value());
    }
    #[allow(unused)]
    pub(crate) fn block_pinch_zoom(&self) -> bool {
        self.property::<bool>("block-pinch-zoom")
    }

    #[allow(unused)]
    pub(crate) fn set_block_pinch_zoom(&self, block_pinch_zoom: bool) {
        self.set_property("block-pinch-zoom", block_pinch_zoom);
    }

    #[allow(unused)]
    pub(crate) fn inertial_scrolling(&self) -> bool {
        self.property::<bool>("inertial-scrolling")
    }

    #[allow(unused)]
    pub(crate) fn set_inertial_scrolling(&self, inertial_scrolling: bool) {
        self.set_property("inertial-scrolling", inertial_scrolling);
    }

    pub(crate) fn pointer_pos(&self) -> Option<Vector2> {
        self.imp().pointer_pos.get()
    }

    pub(crate) fn last_contextmenu_pos(&self) -> Option<Vector2> {
        self.imp().last_contextmenu_pos.get()
    }

    pub(crate) fn scroller(&self) -> ScrolledWindow {
        self.imp().scroller.get()
    }

    pub(crate) fn workaround_cancel_kinetic_scrolling_for_zoom(&self) {
        self.imp().workaround_cancel_kinetic_scrolling_for_zoom();
    }

    pub(crate) fn canvas(&self) -> RnCanvas {
        self.imp().canvas.get()
    }

    pub(crate) fn contextmenu(&self) -> RnContextMenu {
        self.imp().contextmenu.get()
    }

    /// Initializes for the given appwindow. Usually `init()` is only called once,
    /// but because this widget can be moved across appwindows through tabs,
    /// this function also disconnects and replaces all existing old connections
    ///
    /// The same method of the canvas child is chained up in here.
    pub(crate) fn init_reconnect(&self, appwindow: &RnAppWindow) {
        self.imp().canvas.init_reconnect(appwindow);

        let appwindow_block_pinch_zoom_bind = appwindow
            .bind_property("block-pinch-zoom", self, "block_pinch_zoom")
            .sync_create()
            .build();

        let appwindow_show_scrollbars_bind = appwindow
            .sidebar()
            .settings_panel()
            .general_show_scrollbars_row()
            .bind_property("active", self, "show-scrollbars")
            .sync_create()
            .build();

        let appwindow_inertial_scrolling_bind = appwindow
            .sidebar()
            .settings_panel()
            .general_inertial_scrolling_row()
            .bind_property("active", self, "inertial-scrolling")
            .sync_create()
            .build();

        let appwindow_righthanded_bind = appwindow
            .bind_property("righthanded", &self.scroller(), "window-placement")
            .transform_to(|_, righthanded: bool| {
                if righthanded {
                    Some(CornerType::BottomRight)
                } else {
                    Some(CornerType::BottomLeft)
                }
            })
            .sync_create()
            .build();

        let mut connections = self.imp().connections.borrow_mut();
        if let Some(old) = connections
            .appwindow_block_pinch_zoom_bind
            .replace(appwindow_block_pinch_zoom_bind)
        {
            old.unbind()
        }
        if let Some(old) = connections
            .appwindow_show_scrollbars_bind
            .replace(appwindow_show_scrollbars_bind)
        {
            old.unbind();
        }
        if let Some(old) = connections
            .appwindow_inertial_scrolling_bind
            .replace(appwindow_inertial_scrolling_bind)
        {
            old.unbind();
        }
        if let Some(old) = connections
            .appwindow_righthanded_bind
            .replace(appwindow_righthanded_bind)
        {
            old.unbind();
        }
    }

    /// This disconnects all connections with references to external objects,
    /// to prepare moving the widget to another appwindow.
    ///
    /// The same method of the canvas child is chained up in here.
    pub(crate) fn disconnect_connections(&self) {
        self.canvas().disconnect_connections();

        let mut connections = self.imp().connections.borrow_mut();
        if let Some(old) = connections.appwindow_block_pinch_zoom_bind.take() {
            old.unbind();
        }
        if let Some(old) = connections.appwindow_show_scrollbars_bind.take() {
            old.unbind();
        }
        if let Some(old) = connections.appwindow_inertial_scrolling_bind.take() {
            old.unbind();
        }
        if let Some(old) = connections.appwindow_righthanded_bind.take() {
            old.unbind();
        }
    }

    /// When the widget is the child of a tab page, we want to connect the title, icons, ..
    ///
    /// disconnects existing connections to old tab pages.
    ///
    /// The same method of the canvas child is chained up in here.
    pub(crate) fn connect_to_tab_page(&self, page: &adw::TabPage) {
        self.canvas().connect_to_tab_page(page);
    }
}
