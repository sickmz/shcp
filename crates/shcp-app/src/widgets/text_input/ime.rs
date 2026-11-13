//! Platform text input uses UTF-16 offsets; the widget stores UTF-8 byte offsets.
use super::*;
use gpui::{EntityInputHandler, UTF16Selection};
use std::ops::Range;

impl TextInput {
    fn byte_for_utf16(&self, offset: usize) -> usize {
        let mut units = 0;
        for (byte, ch) in self.text.char_indices() {
            if units + ch.len_utf16() > offset {
                return byte;
            }
            units += ch.len_utf16();
        }
        self.text.len()
    }

    fn to_utf16(&self, offset: usize) -> usize {
        self.text[..offset].encode_utf16().count()
    }

    fn replacement(&self, range: Option<Range<usize>>) -> Range<usize> {
        range
            .map(|r| {
                let a = self.byte_for_utf16(r.start);
                let b = self.byte_for_utf16(r.end);
                a.min(b)..a.max(b)
            })
            .or_else(|| self.marked.clone())
            .or_else(|| self.selection())
            .unwrap_or(self.cursor..self.cursor)
    }

    /// Puts the printable part of `text` in place of `range`, returning where it now is.
    fn replace(&mut self, range: Option<Range<usize>>, text: &str) -> Range<usize> {
        let range = self.replacement(range);
        let text = printable(text);
        self.text.replace_range(range.clone(), &text);
        range.start..range.start + text.len()
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        // Do not expose passwords through platform text services.
        if self.masked {
            return None;
        }
        let range = self.replacement(Some(range));
        *adjusted = Some(self.to_utf16(range.start)..self.to_utf16(range.end));
        Some(self.text[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let selected = self.selection().unwrap_or(self.cursor..self.cursor);
        Some(UTF16Selection {
            range: self.to_utf16(selected.start)..self.to_utf16(selected.end),
            reversed: self.anchor.is_some_and(|anchor| anchor > self.cursor),
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked
            .as_ref()
            .map(|r| self.to_utf16(r.start)..self.to_utf16(r.end))
    }

    fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        self.marked = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.cursor = self.replace(range, text).end;
        self.marked = None;
        self.anchor = None;
        self.changed(cx);
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = self.replace(range, text);
        self.marked = (!range.is_empty()).then_some(range.clone());
        self.cursor = range.end;
        if let Some(selected) = selected {
            let start = self.to_utf16(range.start);
            self.cursor = self.byte_for_utf16(start + selected.end).min(range.end);
        }
        self.anchor = None;
        self.changed(cx);
    }

    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let bounds = self.last_bounds?;
        let line = self.last_line.as_ref()?;
        let range = self.replacement(Some(range));
        let x = line.x_for_index(self.byte_to_display(range.start));
        Some(Bounds::new(
            point(bounds.left() + x - self.scroll_x, bounds.top()),
            size(px(1.), bounds.size.height),
        ))
    }

    fn character_index_for_point(
        &mut self,
        position: gpui::Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        let byte = self.byte_at_x(position.x)?;
        Some(self.to_utf16(byte))
    }
}
