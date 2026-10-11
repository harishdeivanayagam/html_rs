//! The winit runtime: window lifecycle, the render effect, event dispatch and
//! focus management. Runs natively *and* on the web (wasm32 via Trunk).

use crate::{Element, Node};
use html_rs_render::{DrawList, Renderer};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, MouseButton as WinitMouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::window::{CursorIcon, Window, WindowId};

/// Builder returned by [`launch`].
pub struct App {
    title: String,
    width: f64,
    height: f64,
    view: Box<dyn Fn() -> Element>,
}

/// Launch an html.rs app with a view function — a closure returning the
/// element tree, re-evaluated automatically whenever signals it reads change.
///
/// ```ignore
/// launch(move || ui! { <p class="text-white">"Hello"</p> }).title("Hi").run();
/// ```
pub fn launch(view: impl Fn() -> Element + 'static) -> App {
    App { title: "html.rs".into(), width: 800.0, height: 600.0, view: Box::new(view) }
}

impl App {
    /// Window title.
    pub fn title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    /// Initial window size (logical px).
    pub fn size(mut self, width: f64, height: f64) -> Self {
        self.width = width;
        self.height = height;
        self
    }

    /// Builds the event loop and runs until the window is closed.
    pub fn run(self) {
        let shared = Rc::new(Shared {
            title: self.title,
            width: self.width,
            height: self.height,
            view: self.view,
            tree: RefCell::new(Element::new("div")),
            dirty: Cell::new(true),
            window: RefCell::new(None),
            renderer: RefCell::new(None),
            draw: RefCell::new(DrawList::empty()),
            focus: RefCell::new(None),
            hovered: RefCell::new(None),
            modifiers: Cell::new(winit::keyboard::ModifiersState::default()),
        });

        // The render effect: re-runs whenever signals read by the view change,
        // rebuilding the element tree and scheduling a repaint.
        let s = shared.clone();
        html_rs_signals::create_effect(move || {
            let tree = (s.view)();
            *s.tree.borrow_mut() = tree;
            s.dirty.set(true);
            if let Some(w) = s.window.borrow().as_ref() {
                w.request_redraw();
            }
        });

        let event_loop = EventLoop::builder().build().expect("event loop");
        let mut handler = AppHandler { shared };
        event_loop.run_app(&mut handler).expect("event loop error");
    }
}

struct Shared {
    title: String,
    width: f64,
    height: f64,
    view: Box<dyn Fn() -> Element>,
    tree: RefCell<Element>,
    dirty: Cell<bool>,
    window: RefCell<Option<Arc<Window>>>,
    renderer: RefCell<Option<Renderer>>,
    draw: RefCell<DrawList>,
    /// Focused element path (for `<input>`).
    focus: RefCell<Option<Vec<u32>>>,
    hovered: RefCell<Option<Vec<u32>>>,
    /// Last reported keyboard modifiers (winit 0.30 delivers them separately).
    modifiers: Cell<winit::keyboard::ModifiersState>,
}

struct AppHandler {
    shared: Rc<Shared>,
}

impl AppHandler {
    fn repaint(&self) {
        if let Some(w) = self.shared.window.borrow().as_ref() {
            w.request_redraw();
        }
    }

    /// Resolves an element by its path (child indices from the root).
    fn element_at_path(&self, path: &[u32]) -> Option<Element> {
        let mut el = self.shared.tree.borrow().clone();
        for &i in path {
            el = match el.children.get(i as usize)? {
                Node::Element(e) => e.clone(),
                Node::Text(_) => return None,
            };
        }
        Some(el)
    }

    fn focused_element(&self) -> Option<(Element, Vec<u32>)> {
        let focus = self.shared.focus.borrow().clone()?;
        let el = self.element_at_path(&focus)?;
        Some((el, focus))
    }

    /// Rebuilds layout + draw list and renders. Only called from RedrawRequested.
    fn redraw(&self) {
        let Some(window) = self.shared.window.borrow().as_ref().cloned() else { return };
        self.shared.dirty.set(false);
        let mut renderer_guard = self.shared.renderer.borrow_mut();
        let Some(renderer) = renderer_guard.as_mut() else { return };

        let scale = window.scale_factor() as f32;
        let size = window.inner_size();
        let tree = self.shared.tree.borrow().clone();
        let focus = self.shared.focus.borrow().clone();
        let hovered = self.shared.hovered.borrow().clone();

        let list = renderer.paint(
            &tree,
            size.width as f32,
            size.height as f32,
            scale,
            focus.as_deref(),
            hovered.as_deref(),
        );

        // Clear color: root element's bg if it set one, else the default.
        if let Some(bg) = html_rs_core::parse_classes(&tree.classes, 1.0).bg {
            renderer.set_clear_color(bg);
        }

        renderer.render(&list);
        *self.shared.draw.borrow_mut() = list;
    }
}

impl ApplicationHandler<()> for AppHandler {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.shared.window.borrow().is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title(self.shared.title.clone())
            .with_inner_size(winit::dpi::LogicalSize::new(self.shared.width, self.shared.height));
        let window = Arc::new(event_loop.create_window(attrs).expect("create window"));

        #[cfg(target_arch = "wasm32")]
        {
            use winit::platform::web::WindowExtWebSys;
            let canvas = window.canvas().expect("winit window has no canvas");
            let doc = web_sys::window().unwrap().document().unwrap();
            doc.body().unwrap().append_child(&canvas).ok();
            let style = canvas.style();
            style.set_property("width", "100%").ok();
            style.set_property("height", "100%").ok();
            style.set_property("display", "block").ok();
            style.set_property("outline", "none").ok();

            let shared = self.shared.clone();
            let w = window.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let renderer = Renderer::new(w).await;
                *shared.renderer.borrow_mut() = Some(renderer);
                shared.dirty.set(true);
                if let Some(win) = shared.window.borrow().as_ref() {
                    win.request_redraw();
                }
            });
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            let renderer = pollster::block_on(Renderer::new(window.clone()));
            *self.shared.renderer.borrow_mut() = Some(renderer);
            self.shared.dirty.set(true);
        }

        *self.shared.window.borrow_mut() = Some(window);
        self.repaint();
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, window_id: WindowId, event: WindowEvent) {
        let Some(window) = self.shared.window.borrow().as_ref().cloned() else { return };
        if window.id() != window_id {
            return;
        }

        match event {
            WindowEvent::CloseRequested => event_loop.exit(),

            WindowEvent::Resized(size) => {
                if let Some(r) = self.shared.renderer.borrow_mut().as_mut() {
                    r.resize(size.width, size.height);
                }
                self.shared.dirty.set(true);
                window.request_redraw();
            }

            WindowEvent::ScaleFactorChanged { .. } => {
                // The follow-up Resized event carries the new physical size.
                self.shared.dirty.set(true);
            }

            WindowEvent::ModifiersChanged(modifiers) => {
                self.shared.modifiers.set(modifiers.state());
            }

            WindowEvent::RedrawRequested => {
                if self.shared.dirty.get() {
                    self.redraw();
                }
            }

            WindowEvent::CursorMoved { position, .. } => {
                // position is in physical px, same space as layout rects.
                let (x, y) = (position.x as f32, position.y as f32);
                LAST_CURSOR.with(|c| c.set((x, y)));

                let new_hover = self.shared.draw.borrow().hit_test(x, y).map(|h| h.path.clone());
                let changed = { *self.shared.hovered.borrow() != new_hover };
                if changed {
                    let pointer = new_hover
                        .as_ref()
                        .and_then(|path| {
                            self.shared
                                .draw
                                .borrow()
                                .hits
                                .iter()
                                .rev()
                                .find(|h| h.path == *path)
                                .map(|h| h.cursor_pointer)
                        })
                        .unwrap_or(false);
                    window.set_cursor(if pointer { CursorIcon::Pointer } else { CursorIcon::Default });
                    *self.shared.hovered.borrow_mut() = new_hover;
                    self.shared.dirty.set(true);
                    window.request_redraw();
                }
            }

            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: WinitMouseButton::Left,
                ..
            } => {
                let scale = window.scale_factor() as f32;
                let (px, py) = LAST_CURSOR.with(|c| c.get());
                // Hit-test space is physical; handler coordinates are logical.
                let (lx, ly) = (px / scale, py / scale);

                let mut repaint = false;
                let hit = self.shared.draw.borrow().hit_test(px, py).map(|h| (
                    h.path.clone(),
                    h.is_input,
                    h.on_click.clone(),
                ));
                match hit {
                    Some((path, is_input, on_click)) => {
                        {
                            let mut focus = self.shared.focus.borrow_mut();
                            if is_input {
                                if focus.as_ref() != Some(&path) {
                                    *focus = Some(path.clone());
                                    repaint = true;
                                }
                            } else if focus.take().is_some() {
                                repaint = true;
                            }
                        }
                        if let Some(handler) = on_click {
                            handler(&html_rs_core::PointerEvent {
                                x: lx,
                                y: ly,
                                button: html_rs_core::MouseButton::Left,
                            });
                            repaint = true;
                        }
                    }
                    None => {
                        if self.shared.focus.borrow_mut().take().is_some() {
                            repaint = true;
                        }
                    }
                }
                if repaint {
                    window.request_redraw();
                }
            }

            WindowEvent::KeyboardInput { event: key_event, .. } => {
                if key_event.state != ElementState::Pressed {
                    return;
                }
                // All keyboard handling goes to the focused `<input>`.
                let Some((el, _path)) = self.focused_element() else { return };
                let value = el.get_attr("value").unwrap_or_default().to_string();
                let on_input = el.on_input.clone();
                let on_enter = el.on_enter.clone();
                let on_key = el.on_key.clone();

                let key = key_event.logical_key.clone();
                let mods = self.shared.modifiers.get();
                let ctrl = mods.control_key();
                let meta = mods.super_key();
                let mut handled = false;

                if let winit::keyboard::Key::Named(winit::keyboard::NamedKey::Enter) = key {
                    if let Some(h) = &on_enter {
                        h(&value);
                        handled = true;
                    }
                } else if let winit::keyboard::Key::Named(winit::keyboard::NamedKey::Backspace) = key {
                    if let Some(h) = &on_input {
                        let mut s = value.clone();
                        s.pop();
                        h(&s);
                        handled = true;
                    }
                } else if !ctrl && !meta {
                    if let Some(text) = &key_event.text {
                        if let Some(h) = &on_input {
                            h(&format!("{value}{text}"));
                            handled = true;
                        }
                    }
                }

                if !handled {
                    if let Some(h) = &on_key {
                        let key_name = match &key {
                            winit::keyboard::Key::Character(c) => c.to_string(),
                            winit::keyboard::Key::Named(n) => format!("{n:?}"),
                            winit::keyboard::Key::Unidentified(_) => "Unidentified".to_string(),
                            _ => format!("{key:?}"),
                        };
                        h(&html_rs_core::KeyEventInfo {
                            key: key_name,
                            ctrl,
                            shift: mods.shift_key(),
                            alt: mods.alt_key(),
                            meta,
                        });
                    }
                }
                window.request_redraw();
            }

            _ => {}
        }
    }

    fn about_to_wait(&mut self, _event_loop: &ActiveEventLoop) {
        if self.shared.dirty.get() {
            self.repaint();
        }
    }
}

thread_local! {
    /// Last cursor position in **physical** px (CursorMoved and MouseInput are
    /// separate events, so we remember the position between them).
    static LAST_CURSOR: Cell<(f32, f32)> = const { Cell::new((0.0, 0.0)) };
}
