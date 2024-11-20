//! A slider that integrates with NIH-plug's [`Param`] types.

use atomic_refcell::AtomicRefCell;
use nih_plug::prelude::Param;
use std::borrow::Borrow;
use std::fmt::{Debug, Formatter, Result};

use crate::backend::Renderer;
use crate::renderer::BorderRadius;
use crate::renderer::Renderer as GraphicsRenderer;
use crate::text::Renderer as TextRenderer;
use crate::text_input::{Appearance, StyleSheet};
use crate::{
    alignment, event, iced_native, keyboard, layout, mouse, renderer, text, theme, touch, tree,
    Background, Clipboard, Color, Element, Event, Font, Layout, Length, Point, Rectangle, Shell,
    Size, TextInput, Theme, Tree, Vector, Widget,
};

use super::util;
use super::ParamMessage;

use iced_native::widget::text_input::State as TextInputState;

/// When shift+dragging a parameter, one pixel dragged corresponds to this much change in the
/// noramlized parameter.
const GRANULAR_DRAG_MULTIPLIER: f32 = 0.1;

/// The thickness of this widget's borders.
const BORDER_WIDTH: f32 = 1.0;

/// A slider that integrates with NIH-plug's [`Param`] types.
///
/// TODO: There are currently no styling options at all
/// TODO: Handle scrolling for steps (and shift+scroll for smaller steps?)
pub struct ParamSlider<'a, P: Param> {
    state: &'a mut State,

    param: &'a P,

    height: Length,
    width: Length,
    text_size: Option<u16>,
    font: Font,
}

struct TextInputTree(AtomicRefCell<Tree>);

impl TextInputTree {
    fn map<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&TextInputState) -> R,
    {
        let tree = self.0.borrow();
        let state = tree.state.downcast_ref::<TextInputState>();
        f(state)
    }

    fn map_mut<F, R>(&self, f: F) -> R
    where
        F: FnOnce(&mut TextInputState) -> R,
    {
        let mut tree = self.0.borrow_mut();
        let state = tree.state.downcast_mut::<TextInputState>();
        f(state)
    }
}

impl Debug for TextInputTree {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result {
        self.0.borrow().fmt(f)
    }
}

impl Default for TextInputTree {
    fn default() -> Self {
        Self(AtomicRefCell::new(Tree {
            tag: tree::Tag::of::<TextInputState>(),
            state: tree::State::new(TextInputState::new()),
            children: Vec::new(),
        }))
    }
}

// SAFETY: The `Any` type in `tree::state` is what makes the struct non send / sync.
//         But since we know the real type (`TextInputState`), which is send / sync,
//         this should be fine.
unsafe impl Send for TextInputTree {}
unsafe impl Sync for TextInputTree {}

/// State for a [`ParamSlider`].
#[derive(Debug, Default)]
pub struct State {
    keyboard_modifiers: keyboard::Modifiers,
    /// Will be set to `true` if we're dragging the parameter. Resetting the parameter or entering a
    /// text value should not initiate a drag.
    drag_active: bool,
    /// We keep track of the start coordinate and normalized value holding down Shift while dragging
    /// for higher precision dragging. This is a `None` value when granular dragging is not active.
    granular_drag_start_x_value: Option<(f32, f32)>,
    /// Track clicks for double clicks.
    last_click: Option<mouse::Click>,

    /// State for the text input overlay that will be shown when this widget is alt+clicked.
    text_input_tree: TextInputTree,
    /// The text that's currently in the text input. If this is set to `None`, then the text input
    /// is not visible.
    text_input_value: Option<String>,
}

/// An internal message for intercep- I mean handling output from the embedded [`TextInpu`] widget.
#[derive(Debug, Clone)]
enum TextInputMessage {
    /// A new value was entered in the text input dialog.
    Value(String),
    /// Enter was pressed.
    Submit,
}

/// The default text input style with the border removed.
struct TextInputStyle;

impl StyleSheet for TextInputStyle {
    type Style = Theme;

    fn active(&self, _style: &Self::Style) -> Appearance {
        Appearance {
            background: Background::Color(Color::TRANSPARENT),
            border_radius: 0.0,
            border_width: 0.0,
            border_color: Color::TRANSPARENT,
            icon_color: Color::default(),
        }
    }

    fn focused(&self, style: &Self::Style) -> Appearance {
        self.active(style)
    }

    fn placeholder_color(&self, _style: &Self::Style) -> Color {
        Color::from_rgb(0.7, 0.7, 0.7)
    }

    fn value_color(&self, _style: &Self::Style) -> Color {
        Color::from_rgb(0.3, 0.3, 0.3)
    }

    fn disabled_color(&self, style: &Self::Style) -> Color {
        self.value_color(style)
    }

    fn selection_color(&self, _style: &Self::Style) -> Color {
        Color::from_rgb(0.8, 0.8, 1.0)
    }

    fn disabled(&self, style: &Self::Style) -> Appearance {
        self.active(style)
    }
}

impl<'a, P: Param> ParamSlider<'a, P> {
    /// Creates a new [`ParamSlider`] for the given parameter.
    pub fn new(state: &'a mut State, param: &'a P) -> Self {
        Self {
            state,

            param,

            width: Length::from(180),
            height: Length::from(30),
            text_size: None,
            font: <Renderer as TextRenderer>::Font::default(),
        }
    }

    /// Sets the width of the [`ParamSlider`].
    pub fn width(mut self, width: Length) -> Self {
        self.width = width;
        self
    }

    /// Sets the height of the [`ParamSlider`].
    pub fn height(mut self, height: Length) -> Self {
        self.height = height;
        self
    }

    /// Sets the text size of the [`ParamSlider`].
    pub fn text_size(mut self, size: u16) -> Self {
        self.text_size = Some(size);
        self
    }

    /// Sets the font of the [`ParamSlider`].
    pub fn font(mut self, font: Font) -> Self {
        self.font = font;
        self
    }

    /// Create a temporary [`TextInput`] hooked up to [`State::text_input_value`] and outputting
    /// [`TextInputMessage`] messages and do something with it. This can be used to
    fn with_text_input<T, R, F>(&self, layout: Layout, renderer: R, current_value: &str, f: F) -> T
    where
        F: FnOnce(TextInput<'_, TextInputMessage>, &mut Tree, Layout, R) -> T,
        R: Borrow<Renderer>,
    {
        let mut text_input_tree = self.state.text_input_tree.0.borrow_mut();
        let text_input_state = text_input_tree.state.downcast_mut::<TextInputState>();
        text_input_state.focus();

        let text_size =
            self.text_size
                .unwrap_or_else(|| renderer.borrow().default_size() as u16) as f32;
        let text_width = renderer
            .borrow()
            .measure_width(current_value, text_size, self.font);
        let text_input = TextInput::new("", current_value)
            .font(self.font)
            .size(text_size)
            .width(Length::from(text_width.ceil() as u16))
            .style(theme::TextInput::Custom(Box::new(TextInputStyle)))
            .on_input(TextInputMessage::Value)
            .on_submit(TextInputMessage::Submit);

        // Make sure to not draw over the borders, and center the text
        let offset_node = layout::Node::with_children(
            Size {
                width: text_width,
                height: layout.bounds().size().height - (BORDER_WIDTH * 2.0),
            },
            vec![layout::Node::new(layout.bounds().size())],
        );
        let offset_layout = Layout::with_offset(
            Vector {
                x: layout.bounds().center_x() - (text_width / 2.0),
                y: layout.position().y + BORDER_WIDTH,
            },
            &offset_node,
        );

        f(text_input, &mut text_input_tree, offset_layout, renderer)
    }

    /// Set the normalized value for a parameter if that would change the parameter's plain value
    /// (to avoid unnecessary duplicate parameter changes). The begin- and end set parameter
    /// messages need to be sent before calling this function.
    fn set_normalized_value(&self, shell: &mut Shell<'_, ParamMessage>, normalized_value: f32) {
        // This snaps to the nearest plain value if the parameter is stepped in some way.
        // TODO: As an optimization, we could add a `const CONTINUOUS: bool` to the parameter to
        //       avoid this normalized->plain->normalized conversion for parameters that don't need
        //       it
        let plain_value = self.param.preview_plain(normalized_value);
        let current_plain_value = self.param.modulated_plain_value();
        if plain_value != current_plain_value {
            // For the aforementioned snapping
            let normalized_plain_value = self.param.preview_normalized(plain_value);
            shell.publish(ParamMessage::SetParameterNormalized(
                self.param.as_ptr(),
                normalized_plain_value,
            ));
        }
    }
}

impl<'a, P: Param> Widget<ParamMessage, Renderer> for ParamSlider<'a, P> {
    fn width(&self) -> Length {
        self.width
    }

    fn height(&self) -> Length {
        self.height
    }

    fn layout(&self, _renderer: &Renderer, limits: &layout::Limits) -> layout::Node {
        let limits = limits.width(self.width).height(self.height);
        let size = limits.resolve(Size::ZERO);

        layout::Node::new(size)
    }

    fn draw(
        &self,
        _tree: &Tree,
        renderer: &mut Renderer,
        theme: &Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        cursor_position: Point,
        _viewport: &Rectangle,
    ) {
        let bounds = layout.bounds();
        // I'm sure there's some philosophical meaning behind this
        let bounds_without_borders = Rectangle {
            x: bounds.x + BORDER_WIDTH,
            y: bounds.y + BORDER_WIDTH,
            width: bounds.width - (BORDER_WIDTH * 2.0),
            height: bounds.height - (BORDER_WIDTH * 2.0),
        };
        let is_mouse_over = bounds.contains(cursor_position);

        // The bar itself, show a different background color when the value is being edited or when
        // the mouse is hovering over it to indicate that it's interactive
        let background_color =
            if is_mouse_over || self.state.drag_active || self.state.text_input_value.is_some() {
                Color::new(0.5, 0.5, 0.5, 0.1)
            } else {
                Color::TRANSPARENT
            };

        renderer.fill_quad(
            renderer::Quad {
                bounds,
                border_color: Color::BLACK,
                border_width: BORDER_WIDTH,
                border_radius: BorderRadius::from(0.0),
            },
            background_color,
        );

        // Only draw the text input widget when it gets focussed. Otherwise, overlay the label with
        // the slider.
        if let Some(current_value) = &self.state.text_input_value {
            self.with_text_input(
                layout,
                renderer,
                current_value,
                |text_input, tree, layout, renderer| {
                    text_input.draw(tree, renderer, theme, layout, cursor_position, None)
                },
            )
        } else {
            // We'll visualize the difference between the current value and the default value if the
            // default value lies somewhere in the middle and the parameter is continuous. Otherwise
            // this appraoch looks a bit jarring.
            let current_value = self.param.modulated_normalized_value();
            let default_value = self.param.default_normalized_value();
            let fill_start_x = util::remap_rect_x_t(
                &bounds_without_borders,
                if self.param.step_count().is_none() && (0.45..=0.55).contains(&default_value) {
                    default_value
                } else {
                    0.0
                },
            );
            let fill_end_x = util::remap_rect_x_t(&bounds_without_borders, current_value);

            let fill_color = Color::from_rgb8(196, 196, 196);
            let fill_rect = Rectangle {
                x: fill_start_x.min(fill_end_x),
                width: (fill_end_x - fill_start_x).abs(),
                ..bounds_without_borders
            };
            renderer.fill_quad(
                renderer::Quad {
                    bounds: fill_rect,
                    border_color: Color::TRANSPARENT,
                    border_width: 0.0,
                    border_radius: BorderRadius::from(0.0),
                },
                fill_color,
            );

            // To make it more readable (and because it looks cool), the parts that overlap with the
            // fill rect will be rendered in white while the rest will be rendered in black.
            let display_value = self.param.to_string();
            let text_size =
                self.text_size
                    .unwrap_or_else(|| renderer.default_size() as u16) as f32;
            let text_bounds = Rectangle {
                x: bounds.center_x(),
                y: bounds.center_y(),
                ..bounds
            };
            renderer.fill_text(text::Text {
                content: &display_value,
                font: self.font,
                size: text_size,
                bounds: text_bounds,
                color: style.text_color,
                horizontal_alignment: alignment::Horizontal::Center,
                vertical_alignment: alignment::Vertical::Center,
            });

            // This will clip to the filled area
            renderer.with_layer(fill_rect, |renderer| {
                let filled_text_color = Color::from_rgb8(80, 80, 80);
                renderer.fill_text(text::Text {
                    content: &display_value,
                    font: self.font,
                    size: text_size,
                    bounds: text_bounds,
                    color: filled_text_color,
                    horizontal_alignment: alignment::Horizontal::Center,
                    vertical_alignment: alignment::Vertical::Center,
                });
            });
        }
    }

    fn on_event(
        &mut self,
        _tree: &mut Tree,
        event: Event,
        layout: Layout<'_>,
        cursor_position: Point,
        renderer: &Renderer,
        clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, ParamMessage>,
    ) -> event::Status {
        // The pressence of a value in `self.state.text_input_value` indicates that the field should
        // be focussed. The field handles defocussing by itself
        // FIMXE: This is super hacky, I have no idea how you can reuse the text input widget
        //        otherwise. Widgets are not supposed to handle messages from other widgets, but
        //        we'll do so anyways by using a special `TextInputMessage` type and our own
        //        `Shell`.
        let text_input_status = if let Some(current_value) = &self.state.text_input_value {
            let event = event.clone();
            let mut messages = Vec::new();
            let mut text_input_shell = Shell::new(&mut messages);
            let status = self.with_text_input(
                layout,
                renderer,
                current_value,
                |mut text_input, tree, layout, renderer| {
                    text_input.on_event(
                        tree,
                        event,
                        layout,
                        cursor_position,
                        renderer,
                        clipboard,
                        &mut text_input_shell,
                    )
                },
            );

            // Pressing escape will unfocus the text field, so we should propagate that change in
            // our own model
            if self.state.text_input_tree.map(|state| state.is_focused()) {
                for message in messages {
                    match message {
                        TextInputMessage::Value(s) => self.state.text_input_value = Some(s),
                        TextInputMessage::Submit => {
                            if let Some(normalized_value) = self
                                .state
                                .text_input_value
                                .as_ref()
                                .and_then(|s| self.param.string_to_normalized_value(s))
                            {
                                shell.publish(ParamMessage::BeginSetParameter(self.param.as_ptr()));
                                self.set_normalized_value(shell, normalized_value);
                                shell.publish(ParamMessage::EndSetParameter(self.param.as_ptr()));
                            }

                            // And defocus the text input widget again
                            self.state.text_input_value = None;
                        }
                    }
                }
            } else {
                self.state.text_input_value = None;
            }

            status
        } else {
            event::Status::Ignored
        };
        if text_input_status == event::Status::Captured {
            return event::Status::Captured;
        }

        // Compensate for the border when handling these events
        let bounds = layout.bounds();
        let bounds = Rectangle {
            x: bounds.x + BORDER_WIDTH,
            y: bounds.y + BORDER_WIDTH,
            width: bounds.width - (BORDER_WIDTH * 2.0),
            height: bounds.height - (BORDER_WIDTH * 2.0),
        };

        match event {
            Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left))
            | Event::Touch(touch::Event::FingerPressed { .. }) => {
                if bounds.contains(cursor_position) {
                    let click = mouse::Click::new(cursor_position, self.state.last_click);
                    self.state.last_click = Some(click);
                    if self.state.keyboard_modifiers.alt() {
                        // Alt+click should not start a drag, instead it should show the text entry
                        // widget
                        self.state.drag_active = false;

                        // Changing the parameter happens in the TextInput event handler above
                        self.state.text_input_tree.map_mut(|state| {
                            state.move_cursor_to_end();
                            state.select_all();
                        });
                        self.state.text_input_value = Some(self.param.to_string());
                    } else if self.state.keyboard_modifiers.command()
                        || matches!(click.kind(), mouse::click::Kind::Double)
                    {
                        // Likewise resetting a parameter should not let you immediately drag it to a new value
                        self.state.drag_active = false;

                        shell.publish(ParamMessage::BeginSetParameter(self.param.as_ptr()));
                        self.set_normalized_value(shell, self.param.default_normalized_value());
                        shell.publish(ParamMessage::EndSetParameter(self.param.as_ptr()));
                    } else if self.state.keyboard_modifiers.shift() {
                        shell.publish(ParamMessage::BeginSetParameter(self.param.as_ptr()));
                        self.state.drag_active = true;

                        // When holding down shift while clicking on a parameter we want to
                        // granuarly edit the parameter without jumping to a new value
                        self.state.granular_drag_start_x_value =
                            Some((cursor_position.x, self.param.modulated_normalized_value()));
                    } else {
                        shell.publish(ParamMessage::BeginSetParameter(self.param.as_ptr()));
                        self.state.drag_active = true;

                        self.set_normalized_value(
                            shell,
                            util::remap_rect_x_coordinate(&bounds, cursor_position.x),
                        );
                        self.state.granular_drag_start_x_value = None;
                    }

                    return event::Status::Captured;
                }
            }
            Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
            | Event::Touch(touch::Event::FingerLifted { .. } | touch::Event::FingerLost { .. }) => {
                if self.state.drag_active {
                    shell.publish(ParamMessage::EndSetParameter(self.param.as_ptr()));

                    self.state.drag_active = false;

                    return event::Status::Captured;
                }
            }
            Event::Mouse(mouse::Event::CursorMoved { .. })
            | Event::Touch(touch::Event::FingerMoved { .. }) => {
                // Don't do anything when we just reset the parameter because that would be weird
                if self.state.drag_active {
                    // If shift is being held then the drag should be more granular instead of
                    // absolute
                    if self.state.keyboard_modifiers.shift() {
                        let (drag_start_x, drag_start_value) = *self
                            .state
                            .granular_drag_start_x_value
                            .get_or_insert_with(|| {
                                (cursor_position.x, self.param.modulated_normalized_value())
                            });

                        self.set_normalized_value(
                            shell,
                            util::remap_rect_x_coordinate(
                                &bounds,
                                util::remap_rect_x_t(&bounds, drag_start_value)
                                    + (cursor_position.x - drag_start_x) * GRANULAR_DRAG_MULTIPLIER,
                            ),
                        );
                    } else {
                        self.state.granular_drag_start_x_value = None;

                        self.set_normalized_value(
                            shell,
                            util::remap_rect_x_coordinate(&bounds, cursor_position.x),
                        );
                    }

                    return event::Status::Captured;
                }
            }
            Event::Keyboard(keyboard::Event::ModifiersChanged(modifiers)) => {
                self.state.keyboard_modifiers = modifiers;

                // If this happens while dragging, snap back to reality uh I mean the current screen
                // position
                if self.state.drag_active
                    && self.state.granular_drag_start_x_value.is_some()
                    && !modifiers.shift()
                {
                    self.state.granular_drag_start_x_value = None;

                    self.set_normalized_value(
                        shell,
                        util::remap_rect_x_coordinate(&bounds, cursor_position.x),
                    );
                }

                return event::Status::Captured;
            }
            _ => {}
        }

        event::Status::Ignored
    }

    fn mouse_interaction(
        &self,
        _tree: &Tree,
        layout: Layout<'_>,
        cursor_position: Point,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        let bounds = layout.bounds();
        let is_mouse_over = bounds.contains(cursor_position);

        if is_mouse_over {
            mouse::Interaction::Pointer
        } else {
            mouse::Interaction::default()
        }
    }
}

impl<'a, P: Param> ParamSlider<'a, P> {
    /// Convert this [`ParamSlider`] into an [`Element`] with the correct message. You should have a
    /// variant on your own message type that wraps around [`ParamMessage`] so you can forward those
    /// messages to
    /// [`IcedEditor::handle_param_message()`][crate::IcedEditor::handle_param_message()].
    pub fn map<Message, F>(self, f: F) -> Element<'a, Message>
    where
        Message: 'static,
        F: Fn(ParamMessage) -> Message + 'static,
    {
        Element::from(self).map(f)
    }
}

impl<'a, P: Param> From<ParamSlider<'a, P>> for Element<'a, ParamMessage> {
    fn from(widget: ParamSlider<'a, P>) -> Self {
        Element::new(widget)
    }
}
