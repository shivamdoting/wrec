use super::{kit_icon, text_tooltip, Tokens, RADIUS};
use crate::assets::{PhosphorIcon, GEIST_MONO_FONT_FAMILY};
use gpui::prelude::FluentBuilder as _;
use gpui::{
    div, px, App, ClickEvent, ElementId, FontWeight, Hsla, InteractiveElement, IntoElement,
    ParentElement, RenderOnce, SharedString, StatefulInteractiveElement, Styled, Window,
};

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum ButtonKind {
    /// Ink-filled: the strong neutral action (permission grants, pause).
    Solid,
    /// Control-filled with a hairline border: the default button.
    Soft,
    /// No fill until hovered: titlebar/utility buttons.
    Ghost,
    /// The wrec red. Record/stop only.
    Accent,
}

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

#[derive(IntoElement)]
pub(crate) struct KitButton {
    id: ElementId,
    kind: ButtonKind,
    label: Option<SharedString>,
    icon: Option<PhosphorIcon>,
    icon_color: Option<Hsla>,
    icon_size: f32,
    height: f32,
    text_size: f32,
    square: Option<f32>,
    full_width: bool,
    disabled: bool,
    tooltip: Option<SharedString>,
    on_click: Option<ClickHandler>,
}

impl KitButton {
    pub(crate) fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            kind: ButtonKind::Soft,
            label: None,
            icon: None,
            icon_color: None,
            icon_size: 14.,
            height: 32.,
            text_size: 11.,
            square: None,
            full_width: false,
            disabled: false,
            tooltip: None,
            on_click: None,
        }
    }

    pub(crate) fn kind(mut self, kind: ButtonKind) -> Self {
        self.kind = kind;
        self
    }

    pub(crate) fn solid(self) -> Self {
        self.kind(ButtonKind::Solid)
    }

    pub(crate) fn ghost(self) -> Self {
        self.kind(ButtonKind::Ghost)
    }

    pub(crate) fn accent(self) -> Self {
        self.kind(ButtonKind::Accent)
    }

    pub(crate) fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub(crate) fn icon(mut self, icon: PhosphorIcon) -> Self {
        self.icon = Some(icon);
        self
    }

    pub(crate) fn icon_color(mut self, color: Hsla) -> Self {
        self.icon_color = Some(color);
        self
    }

    pub(crate) fn icon_size(mut self, size: f32) -> Self {
        self.icon_size = size;
        self
    }

    pub(crate) fn height(mut self, height: f32) -> Self {
        self.height = height;
        self
    }

    pub(crate) fn text_size(mut self, size: f32) -> Self {
        self.text_size = size;
        self
    }

    /// Icon-only square button of the given side length.
    pub(crate) fn square(mut self, side: f32) -> Self {
        self.square = Some(side);
        self
    }

    pub(crate) fn w_full(mut self) -> Self {
        self.full_width = true;
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

    pub(crate) fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for KitButton {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = Tokens::get(cx);
        let (bg, bg_hover, bg_active, fg, bordered) = match self.kind {
            ButtonKind::Solid => (t.solid, t.solid_hover, t.solid_active, t.on_solid, false),
            ButtonKind::Soft => (t.control, t.control_hover, t.control_active, t.ink, true),
            ButtonKind::Ghost => (
                gpui::transparent_black(),
                t.control,
                t.control_hover,
                t.ink_muted,
                false,
            ),
            ButtonKind::Accent => (
                t.accent,
                t.accent_hover,
                t.accent_active,
                t.on_accent,
                false,
            ),
        };
        let icon_color = self.icon_color.unwrap_or(fg);

        let mut button = div()
            .id(self.id)
            .flex()
            .items_center()
            .justify_center()
            .gap_2()
            .rounded(px(RADIUS))
            .text_color(fg)
            .when_some(self.square, |this, side| this.size(px(side)).flex_none())
            .when(self.square.is_none(), |this| this.h(px(self.height)).px_3())
            .when(self.full_width, |this| this.w_full())
            .when(bordered, |this| this.border_1().border_color(t.line))
            .bg(bg);

        if self.disabled {
            button = button.opacity(0.45).cursor_default();
        } else {
            button = button
                .cursor_pointer()
                .hover(move |this| this.bg(bg_hover))
                .active(move |this| this.bg(bg_active));
            if let Some(handler) = self.on_click {
                button = button.on_click(move |event, window, cx| handler(event, window, cx));
            }
        }

        if let Some(tooltip) = self.tooltip {
            button = button.tooltip(text_tooltip(tooltip));
        }

        button
            .when_some(self.icon, |this, icon| {
                this.child(kit_icon(icon, self.icon_size, icon_color))
            })
            .when_some(self.label, |this, label| {
                this.child(
                    div()
                        .font_family(GEIST_MONO_FONT_FAMILY)
                        .text_size(px(self.text_size))
                        .font_weight(FontWeight::MEDIUM)
                        .whitespace_nowrap()
                        .child(label),
                )
            })
    }
}
