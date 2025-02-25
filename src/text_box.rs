// SPDX-License-Identifier: GPL-3.0-only

use cosmic::{
    iced::{
        event::{Event, Status},
        keyboard::{Event as KeyEvent, KeyCode, Modifiers},
        mouse::{self, Button, Event as MouseEvent, ScrollDelta},
        Color, Element, Length, Padding, Point, Rectangle, Size, Vector,
    },
    iced_core::{
        clipboard::Clipboard,
        image,
        layout::{self, Layout},
        renderer::{self, Quad},
        widget::{self, tree, Widget},
        Shell,
    },
    theme::Theme,
};
use cosmic_text::{Action, Edit, Metrics, ViEditor};
use std::{cell::Cell, cmp, sync::Mutex, time::Instant};

use crate::{FONT_SYSTEM, SWASH_CACHE};

pub struct Appearance {
    pub background_color: Option<Color>,
    pub text_color: Color,
}

impl Appearance {
    pub fn dark() -> Self {
        Self {
            background_color: Some(Color::from_rgb8(0x34, 0x34, 0x34)),
            text_color: Color::from_rgb8(0xFF, 0xFF, 0xFF),
        }
    }

    pub fn light() -> Self {
        Self {
            background_color: Some(Color::from_rgb8(0xFC, 0xFC, 0xFC)),
            text_color: Color::from_rgb8(0x00, 0x00, 0x00),
        }
    }
}

pub trait StyleSheet {
    fn appearance(&self) -> Appearance;
}

impl StyleSheet for Theme {
    fn appearance(&self) -> Appearance {
        if self.theme_type.is_dark() {
            Appearance::dark()
        } else {
            Appearance::light()
        }
    }
}

pub struct TextBox<'a, Message> {
    editor: &'a Mutex<ViEditor<'static>>,
    metrics: Metrics,
    padding: Padding,
    on_changed: Option<Message>,
}

impl<'a, Message> TextBox<'a, Message>
where
    Message: Clone,
{
    pub fn new(editor: &'a Mutex<ViEditor<'static>>, metrics: Metrics) -> Self {
        Self {
            editor,
            metrics,
            padding: Padding::new(0.0),
            on_changed: None,
        }
    }

    pub fn padding<P: Into<Padding>>(mut self, padding: P) -> Self {
        self.padding = padding.into();
        self
    }

    pub fn on_changed(mut self, on_changed: Message) -> Self {
        self.on_changed = Some(on_changed);
        self
    }
}

pub fn text_box<'a, Message>(
    editor: &'a Mutex<ViEditor<'static>>,
    metrics: Metrics,
) -> TextBox<'a, Message>
where
    Message: Clone,
{
    TextBox::new(editor, metrics)
}

//TODO: improve performance
fn draw_rect(
    buffer: &mut [u32],
    image_w: i32,
    image_h: i32,
    start_x: i32,
    start_y: i32,
    w: i32,
    h: i32,
    color: u32,
) {
    let alpha = (color >> 24) & 0xFF;
    if alpha == 0 {
        // Do not draw if alpha is zero
        return;
    } else if alpha >= 255 {
        // Handle overwrite
        for y in start_y..start_y + h {
            if y < 0 || y >= image_h {
                // Skip if y out of bounds
                continue;
            }

            let line_offset = y as usize * image_w as usize;
            for x in start_x..start_x + w {
                if x < 0 || x >= image_w {
                    // Skip if x out of bounds
                    continue;
                }

                let offset = line_offset + x as usize;
                buffer[offset] = color;
            }
        }
    } else {
        let n_alpha = 255 - alpha;
        for y in start_y..start_y + h {
            if y < 0 || y >= image_h {
                // Skip if y out of bounds
                continue;
            }

            let line_offset = y as usize * image_w as usize;
            for x in start_x..start_x + w {
                if x < 0 || x >= image_w {
                    // Skip if x out of bounds
                    continue;
                }

                // Alpha blend with current value
                let offset = line_offset + x as usize;
                let current = buffer[offset];
                if current & 0xFF000000 == 0 {
                    // Overwrite if buffer empty
                    buffer[offset] = color;
                } else {
                    let rb =
                        ((n_alpha * (current & 0x00FF00FF)) + (alpha * (color & 0x00FF00FF))) >> 8;
                    let ag = (n_alpha * ((current & 0xFF00FF00) >> 8))
                        + (alpha * (0x01000000 | ((color & 0x0000FF00) >> 8)));
                    buffer[offset] = (rb & 0x00FF00FF) | (ag & 0xFF00FF00);
                }
            }
        }
    }
}

impl<'a, 'editor, Message, Renderer> Widget<Message, Renderer> for TextBox<'a, Message>
where
    Message: Clone,
    Renderer: renderer::Renderer + image::Renderer<Handle = image::Handle>,
    Renderer::Theme: StyleSheet,
{
    fn tag(&self) -> tree::Tag {
        tree::Tag::of::<State>()
    }

    fn state(&self) -> tree::State {
        tree::State::new(State::new())
    }

    fn width(&self) -> Length {
        Length::Fill
    }

    fn height(&self) -> Length {
        Length::Fill
    }

    fn layout(&self, _renderer: &Renderer, limits: &layout::Limits) -> layout::Node {
        let limits = limits.width(Length::Fill).height(Length::Fill);

        let mut editor = self.editor.lock().unwrap();
        //TODO: set size?
        editor
            .borrow_with(&mut FONT_SYSTEM.lock().unwrap())
            .shape_as_needed();

        let mut layout_lines = 0;
        for line in editor.buffer().lines.iter() {
            match line.layout_opt() {
                Some(layout) => layout_lines += layout.len(),
                None => (),
            }
        }

        let height = layout_lines as f32 * editor.buffer().metrics().line_height;
        let size = Size::new(limits.max().width, height);

        layout::Node::new(limits.resolve(size))
    }

    fn mouse_interaction(
        &self,
        tree: &widget::Tree,
        layout: Layout<'_>,
        cursor_position: mouse::Cursor,
        _viewport: &Rectangle,
        _renderer: &Renderer,
    ) -> mouse::Interaction {
        let state = tree.state.downcast_ref::<State>();

        match &state.dragging {
            Some(Dragging::Scrollbar { .. }) => return mouse::Interaction::Idle,
            _ => {}
        }

        if let Some(p) = cursor_position.position_in(layout.bounds()) {
            let scale_factor = state.scale_factor.get();
            let editor = self.editor.lock().unwrap();
            let buffer_size = editor.buffer().size();

            let x = (p.x - self.padding.left) * scale_factor;
            let y = (p.y - self.padding.top) * scale_factor;
            if x >= 0.0 && x < buffer_size.0 && y >= 0.0 && y < buffer_size.1 {
                return mouse::Interaction::Text;
            }
        }

        mouse::Interaction::Idle
    }

    fn draw(
        &self,
        tree: &widget::Tree,
        renderer: &mut Renderer,
        theme: &Renderer::Theme,
        style: &renderer::Style,
        layout: Layout<'_>,
        _cursor_position: mouse::Cursor,
        viewport: &Rectangle,
    ) {
        let instant = Instant::now();

        let state = tree.state.downcast_ref::<State>();

        let appearance = theme.appearance();

        if let Some(background_color) = appearance.background_color {
            renderer.fill_quad(
                renderer::Quad {
                    bounds: layout.bounds(),
                    border_radius: 0.0.into(),
                    border_width: 0.0,
                    border_color: Color::TRANSPARENT,
                },
                background_color,
            );
        }

        let text_color = cosmic_text::Color::rgba(
            cmp::max(0, cmp::min(255, (appearance.text_color.r * 255.0) as i32)) as u8,
            cmp::max(0, cmp::min(255, (appearance.text_color.g * 255.0) as i32)) as u8,
            cmp::max(0, cmp::min(255, (appearance.text_color.b * 255.0) as i32)) as u8,
            cmp::max(0, cmp::min(255, (appearance.text_color.a * 255.0) as i32)) as u8,
        );

        let mut editor = self.editor.lock().unwrap();

        let view_w = cmp::min(viewport.width as i32, layout.bounds().width as i32)
            - self.padding.horizontal() as i32;
        let view_h = cmp::min(viewport.height as i32, layout.bounds().height as i32)
            - self.padding.vertical() as i32;

        let scale_factor = style.scale_factor as f32;

        let image_w = (view_w as f32 * scale_factor) as i32;
        let image_h = (view_h as f32 * scale_factor) as i32;

        //TODO: make this configurable and do not repeat
        let scrollbar_w = (8.0 * scale_factor) as i32;

        if image_w <= scrollbar_w || image_h <= 0 {
            // Zero sized image
            return;
        }

        // Adjust image width by scrollbar width
        let image_w = image_w - scrollbar_w;

        let mut font_system = FONT_SYSTEM.lock().unwrap();
        let mut editor = editor.borrow_with(&mut font_system);

        // Set metrics and size
        editor.buffer_mut().set_metrics_and_size(
            self.metrics.scale(scale_factor),
            image_w as f32,
            image_h as f32,
        );

        // Shape and layout as needed
        editor.shape_as_needed();

        if editor.buffer().redraw() {
            // Draw to pixel buffer
            let mut pixels = vec![0; image_w as usize * image_h as usize * 4];
            {
                let buffer = unsafe {
                    std::slice::from_raw_parts_mut(
                        pixels.as_mut_ptr() as *mut u32,
                        pixels.len() / 4,
                    )
                };

                editor.draw(
                    &mut SWASH_CACHE.lock().unwrap(),
                    text_color,
                    |x, y, w, h, color| {
                        draw_rect(buffer, image_w, image_h, x, y, w as i32, h as i32, color.0);
                    },
                );

                // Calculate scrollbar
                {
                    let mut start_line_opt = None;
                    let mut end_line = 0;
                    for run in editor.buffer().layout_runs() {
                        end_line = run.line_i;
                        if start_line_opt.is_none() {
                            start_line_opt = Some(end_line);
                        }
                    }

                    let start_line = start_line_opt.unwrap_or(end_line);
                    let lines = editor.buffer().lines.len();
                    let start_y = (start_line * image_h as usize) / lines;
                    let end_y = (((end_line + 1) * image_h as usize) / lines);

                    let rect = Rectangle::new(
                        [image_w as f32 / scale_factor, start_y as f32 / scale_factor].into(),
                        Size::new(
                            scrollbar_w as f32 / scale_factor,
                            (end_y as f32 - start_y as f32) / scale_factor,
                        ),
                    );
                    state.scrollbar_rect.set(rect);
                }
            }

            // Clear redraw flag
            editor.buffer_mut().set_redraw(false);

            state.scale_factor.set(scale_factor);
            *state.handle.lock().unwrap() =
                image::Handle::from_pixels(image_w as u32, image_h as u32, pixels);
        }

        let handle = state.handle.lock().unwrap().clone();
        let image_position =
            layout.position() + [self.padding.left as f32, self.padding.top as f32].into();
        let image_size = image::Renderer::dimensions(renderer, &handle);
        image::Renderer::draw(
            renderer,
            handle,
            Rectangle::new(
                image_position,
                Size::new(
                    image_size.width as f32 / scale_factor,
                    image_size.height as f32 / scale_factor,
                ),
            ),
            [0.0; 4],
        );

        // Draw scrollbar
        let scrollbar_alpha = match &state.dragging {
            Some(Dragging::Scrollbar { .. }) => 0.5,
            _ => 0.25,
        };
        renderer.fill_quad(
            Quad {
                bounds: state.scrollbar_rect.get()
                    + Vector::new(image_position.x, image_position.y),
                border_radius: 0.0.into(),
                border_width: 0.0,
                border_color: Color::TRANSPARENT,
            },
            Color::new(1.0, 1.0, 1.0, scrollbar_alpha),
        );

        let duration = instant.elapsed();
        log::debug!("redraw {}, {}: {:?}", view_w, view_h, duration);
    }

    fn on_event(
        &mut self,
        tree: &mut widget::Tree,
        event: Event,
        layout: Layout<'_>,
        cursor_position: mouse::Cursor,
        _renderer: &Renderer,
        _clipboard: &mut dyn Clipboard,
        shell: &mut Shell<'_, Message>,
        _viewport: &Rectangle<f32>,
    ) -> Status {
        let state = tree.state.downcast_mut::<State>();
        let scale_factor = state.scale_factor.get();
        let scrollbar_rect = state.scrollbar_rect.get();
        let mut editor = self.editor.lock().unwrap();
        let buffer_size = editor.buffer().size();
        let last_changed = editor.changed();
        let mut font_system = FONT_SYSTEM.lock().unwrap();
        let mut editor = editor.borrow_with(&mut font_system);

        let mut status = Status::Ignored;
        match event {
            Event::Keyboard(KeyEvent::KeyPressed {
                key_code,
                modifiers,
            }) => match key_code {
                KeyCode::Left => {
                    editor.action(Action::Left);
                    status = Status::Captured;
                }
                KeyCode::Right => {
                    editor.action(Action::Right);
                    status = Status::Captured;
                }
                KeyCode::Up => {
                    editor.action(Action::Up);
                    status = Status::Captured;
                }
                KeyCode::Down => {
                    editor.action(Action::Down);
                    status = Status::Captured;
                }
                KeyCode::Home => {
                    editor.action(Action::Home);
                    status = Status::Captured;
                }
                KeyCode::End => {
                    editor.action(Action::End);
                    status = Status::Captured;
                }
                KeyCode::PageUp => {
                    editor.action(Action::PageUp);
                    status = Status::Captured;
                }
                KeyCode::PageDown => {
                    editor.action(Action::PageDown);
                    status = Status::Captured;
                }
                KeyCode::Escape => {
                    editor.action(Action::Escape);
                    status = Status::Captured;
                }
                KeyCode::Enter => {
                    editor.action(Action::Enter);
                    status = Status::Captured;
                }
                KeyCode::Backspace => {
                    editor.action(Action::Backspace);
                    status = Status::Captured;
                }
                KeyCode::Delete => {
                    editor.action(Action::Delete);
                    status = Status::Captured;
                }
                KeyCode::Tab => {
                    if modifiers.shift() {
                        editor.action(Action::Unindent);
                    } else {
                        editor.action(Action::Indent);
                    }
                    status = Status::Captured;
                }
                _ => (),
            },
            Event::Keyboard(KeyEvent::ModifiersChanged(modifiers)) => {
                state.modifiers = modifiers;
            }
            Event::Keyboard(KeyEvent::CharacterReceived(character)) => {
                // Only parse keys when Super, Ctrl, and Alt are not pressed
                if !state.modifiers.logo() && !state.modifiers.control() && !state.modifiers.alt() {
                    if !character.is_control() {
                        editor.action(Action::Insert(character));
                    }
                    status = Status::Captured;
                }
            }
            Event::Mouse(MouseEvent::ButtonPressed(Button::Left)) => {
                if let Some(p) = cursor_position.position_in(layout.bounds()) {
                    let x_logical = p.x - self.padding.left;
                    let y_logical = p.y - self.padding.top;
                    let x = x_logical * scale_factor;
                    let y = y_logical * scale_factor;
                    if x >= 0.0 && x < buffer_size.0 && y >= 0.0 && y < buffer_size.1 {
                        editor.action(Action::Click {
                            x: x as i32,
                            y: y as i32,
                        });
                        state.dragging = Some(Dragging::Buffer);
                    } else if scrollbar_rect.contains(Point::new(x_logical, y_logical)) {
                        state.dragging = Some(Dragging::Scrollbar {
                            start_y: y,
                            start_scroll: editor.buffer().scroll(),
                        });
                    } else if x_logical >= scrollbar_rect.x
                        && x_logical < (scrollbar_rect.x + scrollbar_rect.width)
                    {
                        let mut buffer = editor.buffer_mut();
                        let scroll_offset =
                            ((y / buffer.size().1) * buffer.lines.len() as f32) as i32;
                        buffer.set_scroll(scroll_offset);
                        state.dragging = Some(Dragging::Scrollbar {
                            start_y: y,
                            start_scroll: editor.buffer().scroll(),
                        });
                    }
                    status = Status::Captured;
                }
            }
            Event::Mouse(MouseEvent::ButtonReleased(Button::Left)) => {
                state.dragging = None;
                status = Status::Captured;
            }
            Event::Mouse(MouseEvent::CursorMoved { .. }) => {
                if let Some(dragging) = &state.dragging {
                    if let Some(p) = cursor_position.position() {
                        let x = ((p.x - layout.bounds().x) - self.padding.left) * scale_factor;
                        let y = ((p.y - layout.bounds().y) - self.padding.top) * scale_factor;
                        match dragging {
                            Dragging::Buffer => {
                                editor.action(Action::Drag {
                                    x: x as i32,
                                    y: y as i32,
                                });
                            }
                            Dragging::Scrollbar {
                                start_y,
                                start_scroll,
                            } => {
                                let mut buffer = editor.buffer_mut();
                                let scroll_offset = (((y - start_y) / buffer.size().1)
                                    * buffer.lines.len() as f32)
                                    as i32;
                                buffer.set_scroll(start_scroll + scroll_offset);
                            }
                        }
                    }
                    status = Status::Captured;
                }
            }
            Event::Mouse(MouseEvent::WheelScrolled { delta }) => {
                if let Some(_p) = cursor_position.position_in(layout.bounds()) {
                    match delta {
                        ScrollDelta::Lines { x, y } => {
                            //TODO: this adjustment is just a guess!
                            state.scroll_pixels = 0.0;
                            let lines = (-y * 6.0) as i32;
                            if lines != 0 {
                                editor.action(Action::Scroll { lines });
                            }
                            status = Status::Captured;
                        }
                        ScrollDelta::Pixels { x, y } => {
                            //TODO: this adjustment is just a guess!
                            state.scroll_pixels -= y * 6.0;
                            let mut lines = 0;
                            let metrics = editor.buffer().metrics();
                            while state.scroll_pixels <= -metrics.line_height {
                                lines -= 1;
                                state.scroll_pixels += metrics.line_height;
                            }
                            while state.scroll_pixels >= metrics.line_height {
                                lines += 1;
                                state.scroll_pixels -= metrics.line_height;
                            }
                            if lines != 0 {
                                editor.action(Action::Scroll { lines });
                            }
                            status = Status::Captured;
                        }
                    }
                }
            }
            _ => (),
        }

        if editor.changed() != last_changed {
            if let Some(on_changed) = &self.on_changed {
                shell.publish(on_changed.clone());
            }
        }

        status
    }
}

impl<'a, 'editor, Message, Renderer> From<TextBox<'a, Message>> for Element<'a, Message, Renderer>
where
    Message: Clone + 'a,
    Renderer: renderer::Renderer + image::Renderer<Handle = image::Handle>,
    Renderer::Theme: StyleSheet,
{
    fn from(text_box: TextBox<'a, Message>) -> Self {
        Self::new(text_box)
    }
}

enum Dragging {
    Buffer,
    Scrollbar { start_y: f32, start_scroll: i32 },
}

pub struct State {
    modifiers: Modifiers,
    dragging: Option<Dragging>,
    scale_factor: Cell<f32>,
    scroll_pixels: f32,
    scrollbar_rect: Cell<Rectangle<f32>>,
    handle: Mutex<image::Handle>,
}

impl State {
    /// Creates a new [`State`].
    pub fn new() -> State {
        State {
            modifiers: Modifiers::empty(),
            dragging: None,
            scale_factor: Cell::new(1.0),
            scroll_pixels: 0.0,
            scrollbar_rect: Cell::new(Rectangle::default()),
            //TODO: make option!
            handle: Mutex::new(image::Handle::from_pixels(1, 1, vec![0, 0, 0, 0])),
        }
    }
}
