//! Host pages share the Settings shell and keep the original list mounted in memory.
use super::*;

impl SettingsModal {
    /// Open Settings directly on a host, with the Hosts list as its parent page.
    pub fn for_host(
        state: Entity<AppState>,
        modals: Entity<ModalLayer>,
        host: HostId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut settings = Self::new(state, modals, window, cx);
        settings.page = Page::Hosts;
        settings.open_host_detail(host, window, cx);
        settings
    }

    pub(super) fn host_has_edits(&self, cx: &App) -> bool {
        self.host_detail
            .as_ref()
            .is_some_and(|detail| (detail.dirty)(cx))
    }

    pub(super) fn open_host_detail(
        &mut self,
        host: HostId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let parent = cx.weak_entity();
        let target = host.clone();
        let open = Rc::new(move |page, window: &mut Window, cx: &mut App| {
            parent
                .update(cx, |this, cx| {
                    this.open_host_editor(target.clone(), page, window, cx)
                })
                .ok();
        });
        let state = self.state.clone();
        let view = cx.new(|cx| HostSettingsModal::new(state, host.clone(), open, cx).embedded());
        self.embed_host(host, HostSection::Overview, view, window, cx);
    }

    fn open_host_editor(
        &mut self,
        host: HostId,
        page: HostPage,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let state = self.state.clone();
        match page {
            HostPage::Connection => {
                let Some(config) = state.read(cx).host(&host).cloned() else {
                    return;
                };
                let modals = self.modals.clone();
                let view = cx
                    .new(|cx| HostEditor::new(state, modals, Some(config), window, cx).embedded());
                self.embed_host(host, HostSection::Editor(page), view, window, cx);
            }
            HostPage::Commands => {
                let view =
                    cx.new(|cx| HostCommandsModal::new(state, host.clone(), window, cx).embedded());
                self.embed_host(host, HostSection::Editor(page), view, window, cx);
            }
            HostPage::KeyLogin => {
                let Some(config) = state.read(cx).host(&host).cloned() else {
                    return;
                };
                let session = state.update(cx, |state, cx| state.connect_host(&host, cx));
                let view =
                    cx.new(|cx| KeySetupModal::new(state, config, session, window, cx).embedded());
                self.embed_host(host, HostSection::Editor(page), view.clone(), window, cx);
                if let Some(detail) = &mut self.host_detail {
                    detail.key_setup = Some(view);
                }
            }
        }
    }

    fn embed_host<M: ModalView>(
        &mut self,
        host: HostId,
        section: HostSection,
        view: Entity<M>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let initial = view.read(cx).draft(cx);
        let weak = view.downgrade();
        let dirty = Box::new(move |cx: &App| {
            weak.upgrade()
                .is_some_and(|view| view.read(cx).has_unsaved_changes(&initial, cx))
        });
        let subscriptions = vec![
            cx.subscribe_in(&view, window, |this, _, _: &DismissEvent, window, cx| {
                // A successful save (or delete) has already committed the form.
                this.go_back(window, cx);
            }),
            cx.subscribe_in(
                &view,
                window,
                |this, _, _: &super::super::CancelEvent, window, cx| {
                    this.navigate(Destination::Back, window, cx);
                },
            ),
        ];
        let focus = view.focus_handle(cx);
        window.focus(&focus);
        self.host_detail = Some(HostDetail {
            host,
            section,
            view: view.into(),
            focus,
            key_setup: None,
            dirty,
            _subscriptions: subscriptions,
        });
        self.detail_scroll = ScrollHandle::new();
        self.open_dropdown = None;
        self.capturing = None;
        cx.notify();
    }

    pub(super) fn navigate(
        &mut self,
        destination: Destination,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.host_has_edits(cx) {
            let parent = cx.weak_entity();
            let confirm = cx.new(|cx| {
                ConfirmModal::new(
                    "Discard changes?",
                    "Your unsaved changes will be lost.".into(),
                    "Discard",
                    move |window, cx| {
                        parent
                            .update(cx, |this, cx| this.go_to(destination, window, cx))
                            .ok();
                    },
                    cx,
                )
            });
            self.modals.update(cx, |modals, cx| {
                modals.show_front(confirm, None, window, cx)
            });
        } else {
            self.go_to(destination, window, cx);
        }
    }

    fn go_to(&mut self, destination: Destination, window: &mut Window, cx: &mut Context<Self>) {
        match destination {
            Destination::Back => self.go_back(window, cx),
            Destination::Category(page) => {
                self.cancel_key_login(window, cx);
                self.host_detail = None;
                self.page = page;
                self.search.update(cx, |input, cx| input.set_text("", cx));
                self.search.read(cx).focus(window);
                cx.notify();
            }
        }
    }

    fn go_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel_key_login(window, cx);
        if let Some(detail) = self.host_detail.take()
            && matches!(detail.section, HostSection::Editor(_))
            && self.state.read(cx).host(&detail.host).is_some()
        {
            self.open_host_detail(detail.host, window, cx);
        } else {
            self.search.read(cx).focus(window);
            cx.notify();
        }
    }

    pub(super) fn cancel_key_login(&self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(view) = self
            .host_detail
            .as_ref()
            .and_then(|detail| detail.key_setup.clone())
        {
            view.update(cx, |view, cx| view.cancel_login(window, cx));
        }
    }
}
