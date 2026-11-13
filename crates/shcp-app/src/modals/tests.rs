use super::*;
use gpui::{AppContext, TestAppContext, VisualTestContext, point};

struct TestWorkspace(Entity<ModalLayer>);

impl Render for TestWorkspace {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().relative().size_full().child(self.0.clone())
    }
}

fn setup(cx: &mut TestAppContext) -> (Entity<ModalLayer>, &mut VisualTestContext) {
    cx.update(|cx| {
        cx.set_global(crate::theme::builtin::build(
            crate::theme::builtin::NOCTURNE,
            shcp_core::Accent::default(),
            true,
        ));
    });
    let (root, cx) = cx.add_window_view(|_, cx| TestWorkspace(cx.new(|_| ModalLayer::default())));
    let layer = cx.update(|_, cx| root.read(cx).0.clone());
    (layer, cx)
}

fn prompt(
    layer: &Entity<ModalLayer>,
    cx: &mut VisualTestContext,
) -> Entity<text_prompt::TextPrompt> {
    cx.update(|window, cx| {
        let prompt = cx.new(|cx| {
            text_prompt::TextPrompt::new("Rename", "Save", "original", |_, _, _| {}, window, cx)
        });
        layer.update(cx, |layer, cx| {
            layer.skip_next_entrance();
            layer.show(prompt.clone(), window, cx);
        });
        prompt
    })
}

#[gpui::test]
fn backdrop_closes_but_card_click_does_not(cx: &mut TestAppContext) {
    let (layer, cx) = setup(cx);
    let _prompt = prompt(&layer, cx);
    let center = cx.update(|window, _| {
        let size = window.viewport_size();
        point(
            size.width / 2.,
            (size.height + px(crate::title_bar::HEIGHT)) / 2.,
        )
    });
    cx.simulate_click(center, Default::default());
    cx.update(|_, cx| assert!(layer.read(cx).is_open()));
    cx.simulate_click(point(px(4.), px(100.)), Default::default());
    cx.update(|_, cx| assert!(!layer.read(cx).is_open()));
}

#[gpui::test]
fn escape_protects_edits_and_cancel_preserves_them(cx: &mut TestAppContext) {
    let (layer, cx) = setup(cx);
    let prompt = prompt(&layer, cx);
    cx.simulate_input(" changed");
    let edited = cx.update(|_, cx| prompt.read(cx).draft(cx));
    cx.simulate_keystrokes("escape");
    cx.update(|_, cx| {
        assert_eq!(layer.read(cx).stack.len(), 2);
        assert_ne!(layer.read(cx).stack[0].id, prompt.entity_id());
    });
    // Escape cancels the discard confirmation, not the editor underneath it.
    cx.simulate_keystrokes("escape");
    cx.update(|_, cx| {
        assert_eq!(layer.read(cx).stack.len(), 1);
        assert_eq!(prompt.read(cx).draft(cx), edited);
    });
    cx.simulate_keystrokes("escape");
    cx.simulate_keystrokes("enter");
    cx.update(|_, cx| assert!(!layer.read(cx).is_open()));
}

#[gpui::test]
fn destructive_confirmation_backdrop_never_confirms(cx: &mut TestAppContext) {
    let (layer, cx) = setup(cx);
    let confirmed = Rc::new(std::cell::Cell::new(false));
    cx.update(|window, cx| {
        let called = confirmed.clone();
        let modal = cx.new(|cx| {
            confirm::ConfirmModal::new(
                "Delete",
                "Delete this item?".into(),
                "Delete",
                move |_, _| called.set(true),
                cx,
            )
        });
        layer.update(cx, |layer, cx| {
            layer.skip_next_entrance();
            layer.show(modal, window, cx);
        });
    });
    cx.simulate_click(point(px(4.), px(100.)), Default::default());
    assert!(!confirmed.get());
    cx.update(|_, cx| assert!(!layer.read(cx).is_open()));
}

#[gpui::test]
fn backdrop_protects_edits_but_reverted_text_closes_directly(cx: &mut TestAppContext) {
    let (layer, cx) = setup(cx);
    let prompt = prompt(&layer, cx);
    cx.simulate_input(" changed");
    cx.simulate_click(point(px(4.), px(100.)), Default::default());
    cx.update(|_, cx| assert_eq!(layer.read(cx).stack.len(), 2));
    cx.simulate_keystrokes("escape");
    cx.simulate_keystrokes("ctrl-a");
    cx.simulate_input("original");
    cx.update(|_, cx| assert_eq!(prompt.read(cx).draft(cx), vec!["original"]));
    cx.simulate_click(point(px(4.), px(100.)), Default::default());
    cx.update(|_, cx| assert!(!layer.read(cx).is_open()));
}
