use super::{text_tooltip, Tokens};
use gpui::{
    div, px, App, ElementId, InteractiveElement, IntoElement, ParentElement, RenderOnce,
    SharedString, StatefulInteractiveElement, Styled, Window,
};

const TRACK_W: f32 = 34.;
const TRACK_H: f32 = 18.;
const THUMB: f32 = 12.;
const INSET: f32 = 3.;

type ToggleHandler = Box<dyn Fn(&bool, &mut Window, &mut App) + 'static>;

/// Flat instrument-style toggle: squared track, square thumb, no glow.
#[derive(IntoElement)]
pub(crate) struct KitSwitch {
    id: ElementId,
    checked: bool,
    disabled: bool,
    tooltip: Option<SharedString>,
    on_click: Option<ToggleHandler>,
}

impl KitSwitch {
    pub(crate) fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            checked: false,
            disabled: false,
            tooltip: None,
            on_click: None,
        }
    }

    pub(crate) fn checked(mut self, checked: bool) -> Self {
        self.checked = checked;
        self
    }

    pub(crate) fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub(crate) fn tooltip(mut self, tooltip: impl Into<SharedString>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }

    /// Handler receives the *new* checked value.
    pub(crate) fn on_click(
        mut self,
        handler: impl Fn(&bool, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for KitSwitch {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = Tokens::get(cx);
        let checked = self.checked;
        let track = if checked { t.solid } else { t.track_off };
        // Off-state thumb must read against the gray track in both modes;
        // `surface` disappears into it in dark mode.
        let thumb = if checked { t.on_solid } else { t.ink_muted };
        let thumb_x = if checked {
            TRACK_W - THUMB - INSET
        } else {
            INSET
        };

        let mut switch = div()
            .id(self.id)
            .relative()
            .flex_none()
            .w(px(TRACK_W))
            .h(px(TRACK_H))
            .rounded(px(4.))
            .bg(track)
            .child(
                div()
                    .absolute()
                    .top(px(INSET))
                    .left(px(thumb_x))
                    .size(px(THUMB))
                    .rounded(px(2.))
                    .bg(thumb),
            );

        if self.disabled {
            switch = switch.opacity(0.45).cursor_default();
        } else {
            switch = switch.cursor_pointer();
            if let Some(handler) = self.on_click {
                switch = switch.on_click(move |_, window, cx| handler(&!checked, window, cx));
            }
        }

        if let Some(tooltip) = self.tooltip {
            switch = switch.tooltip(text_tooltip(tooltip));
        }

        switch
    }
}
