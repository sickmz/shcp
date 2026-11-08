//! gpui element that paints a terminal grid: cell backgrounds, selection, cursor and
//! styled text (one shaped line per row segment). Also derives cols/rows from its bounds.
use crate::terminal_layout::{self, CellStyle};
use crate::terminal_view::TerminalView;
use crate::theme::Theme;
use gpui::prelude::*;
use gpui::{
    App, BorderStyle, Bounds, DispatchPhase, Element, ElementId, ElementInputHandler, Entity,
    Focusable, Font, FontStyle, FontWeight, GlobalElementId, Hsla, InspectorElementId, LayoutId,
    MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point, ShapedLine, SharedString,
    StrikethroughStyle, Style, TextRun, UnderlineStyle, Window, fill, outline, point, px, relative,
    rgb, size,
};
use shcp_core::TerminalHandle;
use shcp_core::term::{CursorInfo, CursorShape, GridPoint, Rgb};
use std::{cell::Cell, rc::Rc};

#[derive(Clone, Copy)]
pub struct TerminalMetrics {
    pub bounds: Bounds<Pixels>,
    pub cell_width: Pixels,
    pub line_height: Pixels,
    pub cols: u16,
    pub rows: u16,
    pub caret: Bounds<Pixels>,
}

pub type SharedMetrics = Rc<Cell<Option<TerminalMetrics>>>;

pub struct TerminalElement {
    pub view: Entity<TerminalView>,
    pub terminal: TerminalHandle,
    pub font: Font,
    pub font_size: Pixels,
    pub focused: bool,
    pub cursor_visible: bool,
    /// The shape chosen in the settings, over the one the program asks for.
    pub cursor_shape: Option<CursorShape>,
    pub metrics: SharedMetrics,
    pub theme: Theme,
    /// The cells of the link under the pointer.
    pub hovered_link: Option<(GridPoint, GridPoint)>,
}

pub struct Prepaint {
    bounds: Bounds<Pixels>,
    backgrounds: Vec<PaintQuad>,
    search: Vec<PaintQuad>,
    selection: Vec<PaintQuad>,
    link: Vec<PaintQuad>,
    cursor_under: Option<PaintQuad>,
    cursor_over: Option<PaintQuad>,
    lines: Vec<(Point<Pixels>, ShapedLine)>,
}

fn color(rgb_value: Rgb) -> Hsla {
    rgb((rgb_value.0 as u32) << 16 | (rgb_value.1 as u32) << 8 | rgb_value.2 as u32).into()
}

impl IntoElement for TerminalElement {
    type Element = Self;
    fn into_element(self) -> Self::Element {
        self
    }
}

impl TerminalElement {
    fn text_run(&self, len: usize, style: &CellStyle) -> TextRun {
        let mut fg = color(style.fg);
        if style.dim {
            fg.a *= 0.6;
        }
        let mut font = self.font.clone();
        if style.bold {
            // Dark on light gpui thickens the strokes itself: any more weight turns them to a smudge
            // at this size. Bold stays what it is on a dark theme.
            if !self.theme.is_light {
                font.weight = FontWeight::BOLD;
            }
        }
        if style.italic {
            font.style = FontStyle::Italic;
        }
        TextRun {
            len,
            font,
            color: fg,
            background_color: None,
            underline: style.underline.then_some(UnderlineStyle {
                thickness: px(1.),
                color: Some(fg),
                wavy: false,
            }),
            strikethrough: style.strike.then_some(StrikethroughStyle {
                thickness: px(1.),
                color: Some(fg),
            }),
        }
    }

    /// One rectangle per row of a range of cells; `height` of them, at the bottom of the row.
    fn range_quads(
        &self,
        range: (GridPoint, GridPoint),
        cols: u16,
        origin: Point<Pixels>,
        cell: (Pixels, Pixels),
        height: Pixels,
        color: Hsla,
    ) -> Vec<PaintQuad> {
        let (start, end) = range;
        (start.row..=end.row)
            .map(|row| {
                let from = if row == start.row { start.col } else { 0 };
                let to = if row == end.row { end.col } else { cols - 1 };
                let bounds = Bounds::new(
                    point(
                        origin.x + cell.0 * from as f32,
                        origin.y + cell.1 * row as f32 + cell.1 - height,
                    ),
                    size(cell.0 * (to.saturating_sub(from) + 1) as f32, height),
                );
                fill(bounds, color)
            })
            .collect()
    }

    fn cursor_quads(
        &self,
        shape: CursorShape,
        at: Point<Pixels>,
        cell: (Pixels, Pixels),
    ) -> (Option<PaintQuad>, Option<PaintQuad>) {
        let cursor = self.theme.terminal_cursor;
        let full = Bounds::new(at, size(cell.0, cell.1));
        match (shape, self.focused) {
            (CursorShape::Block, true) => (Some(fill(full, cursor)), None),
            (CursorShape::Block, false) => (None, Some(outline(full, cursor, BorderStyle::Solid))),
            (CursorShape::Beam, _) => (
                None,
                Some(fill(Bounds::new(at, size(px(2.), cell.1)), cursor)),
            ),
            (CursorShape::Underline, _) => (
                None,
                Some(fill(
                    Bounds::new(point(at.x, at.y + cell.1 - px(2.)), size(cell.0, px(2.))),
                    cursor,
                )),
            ),
        }
    }
}

impl Element for TerminalElement {
    type RequestLayoutState = ();
    type PrepaintState = Prepaint;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _state: &mut (),
        window: &mut Window,
        _cx: &mut App,
    ) -> Prepaint {
        let text_system = window.text_system().clone();
        let font_id = text_system.resolve_font(&self.font);
        let cell_width = text_system
            .advance(font_id, self.font_size, 'm')
            .map_or(self.font_size * 0.6, |s| s.width)
            .max(px(1.));
        let line_height = (self.font_size * 1.56).round().max(px(1.));

        let previous = self.metrics.get();
        let layout_moving = crate::motion::layout_moving();
        if layout_moving {
            // Redraw this terminal too, so it sends the final size when the animation settles.
            window.request_animation_frame();
        }
        // While a panel glides past, the terminal keeps its size, cut off by the panel if need be.
        let (cols, rows) = match previous.filter(|_| layout_moving) {
            Some(metrics) => (metrics.cols, metrics.rows),
            None => (
                ((bounds.size.width / cell_width).floor() as u16).max(2),
                ((bounds.size.height / line_height).floor() as u16).max(1),
            ),
        };
        if previous.is_none_or(|m| m.cols != cols || m.rows != rows) {
            self.terminal.resize(cols, rows);
        }
        let snapshot = self.terminal.snapshot();
        let cursor_point = snapshot
            .cursor
            .map_or(GridPoint { row: 0, col: 0 }, |c| c.point);
        let caret = Bounds::new(
            point(
                bounds.left() + cell_width * cursor_point.col as f32,
                bounds.top() + line_height * cursor_point.row as f32,
            ),
            size(cell_width, line_height),
        );
        self.metrics.set(Some(TerminalMetrics {
            bounds,
            cell_width,
            line_height,
            cols,
            rows,
            caret,
        }));
        let cell = (cell_width, line_height);
        let origin = bounds.origin;
        let palette = &self.theme.palette;
        // Use the same visibility for the cursor and the text inverted beneath a block.
        // The unfiltered snapshot still supplies the IME caret bounds during the off phase.
        let cursor = snapshot
            .cursor
            .filter(|_| self.cursor_visible)
            .map(|cursor| CursorInfo {
                shape: self.cursor_shape.unwrap_or(cursor.shape),
                ..cursor
            });
        let block_cursor = cursor
            .filter(|c| self.focused && c.shape == CursorShape::Block)
            .map(|c| c.point);
        let layout = terminal_layout::build(&snapshot, palette.bg, block_cursor, palette.bg);

        let backgrounds = layout
            .backgrounds
            .iter()
            .map(|b| {
                fill(
                    Bounds::new(
                        point(
                            origin.x + cell_width * b.col as f32,
                            origin.y + line_height * b.row as f32,
                        ),
                        size(cell_width * b.cols as f32, line_height),
                    ),
                    color(b.color),
                )
            })
            .collect();

        let selection = snapshot
            .selection
            .map(|range| {
                let color = self.theme.terminal_selection;
                self.range_quads(range, snapshot.cols, origin, cell, line_height, color)
            })
            .unwrap_or_default();
        let search = snapshot
            .search_hits
            .iter()
            .flat_map(|hit| {
                let base = if hit.current {
                    self.theme.accent
                } else {
                    self.theme.warning
                };
                let color = Hsla {
                    a: if hit.current { 0.55 } else { 0.3 },
                    ..base
                };
                let range = (hit.start, hit.end);
                self.range_quads(range, snapshot.cols, origin, cell, line_height, color)
            })
            .collect();
        // Links always show as links; the one under the pointer has a thicker line.
        let link = snapshot
            .links
            .iter()
            .map(|range| (*range, px(1.)))
            .chain(self.hovered_link.map(|range| (range, px(2.))))
            .flat_map(|(range, height)| {
                let color = self.theme.accent;
                self.range_quads(range, snapshot.cols, origin, cell, height, color)
            })
            .collect();

        let (cursor_under, cursor_over) = match cursor {
            Some(c) => {
                let at = point(
                    origin.x + cell_width * c.point.col as f32,
                    origin.y + line_height * c.point.row as f32,
                );
                self.cursor_quads(c.shape, at, cell)
            }
            None => (None, None),
        };

        let lines = layout
            .segments
            .iter()
            .map(|segment| {
                let runs: Vec<TextRun> = segment
                    .runs
                    .iter()
                    .map(|(len, style)| self.text_run(*len, style))
                    .collect();
                let force_width = if segment.wide {
                    cell_width * 2.
                } else {
                    cell_width
                };
                let line = text_system.shape_line(
                    SharedString::from(segment.text.clone()),
                    self.font_size,
                    &runs,
                    Some(force_width),
                );
                let at = point(
                    origin.x + cell_width * segment.col as f32,
                    origin.y + line_height * segment.row as f32,
                );
                (at, line)
            })
            .collect();

        Prepaint {
            bounds,
            backgrounds,
            search,
            selection,
            link,
            cursor_under,
            cursor_over,
            lines,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _state: &mut (),
        prepaint: &mut Prepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let focus = self.view.focus_handle(cx);
        window.handle_input(
            &focus,
            ElementInputHandler::new(bounds, self.view.clone()),
            cx,
        );
        let line_height = self.metrics.get().map_or(px(18.), |m| m.line_height);
        window.with_content_mask(
            Some(gpui::ContentMask {
                bounds: prepaint.bounds,
            }),
            |window| {
                for quad in prepaint.backgrounds.drain(..) {
                    window.paint_quad(quad);
                }
                for quad in prepaint.search.drain(..) {
                    window.paint_quad(quad);
                }
                for quad in prepaint.selection.drain(..) {
                    window.paint_quad(quad);
                }
                if let Some(quad) = prepaint.cursor_under.take() {
                    window.paint_quad(quad);
                }
                for (at, line) in prepaint.lines.drain(..) {
                    let _ = line.paint(at, line_height, window, cx);
                }
                for quad in prepaint.link.drain(..) {
                    window.paint_quad(quad);
                }
                if let Some(quad) = prepaint.cursor_over.take() {
                    window.paint_quad(quad);
                }
            },
        );

        // Drag selection must keep working when the pointer leaves the element.
        let view = self.view.clone();
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble {
                view.update(cx, |view, cx| view.mouse_move(event, window, cx));
            }
        });
        let view = self.view.clone();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, _window, cx| {
            if phase == DispatchPhase::Bubble {
                view.update(cx, |view, cx| view.mouse_up(event, cx));
            }
        });
    }
}
