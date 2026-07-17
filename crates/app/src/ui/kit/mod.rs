//! wrec's own component kit, built directly on gpui.
//!
//! Mono/instrument styling: monospace uppercase labels, flat surfaces,
//! hairline borders, tight radii, red reserved for record/live states.

mod button;
mod select;
mod switch;
mod tokens;

pub(crate) use button::KitButton;
pub(crate) use select::{Picker, PickerEvent, PickerItem, PickerState};
pub(crate) use switch::KitSwitch;
pub(crate) use tokens::{Tokens, RADIUS, RADIUS_MENU};

use crate::assets::{PhosphorIcon, GEIST_MONO_FONT_FAMILY};
use gpui::{
    div, px, svg, AnyView, App, AppContext as _, Hsla, IntoElement, ParentElement, Render,
    SharedString, Styled, Svg, Window,
};

/// Render one of the bundled Phosphor icons at a given size and color.
pub(crate) fn kit_icon(icon: PhosphorIcon, size: f32, color: Hsla) -> Svg {
    svg()
        .path(icon.asset_path())
        .size(px(size))
        .text_color(color)
        .flex_shrink_0()
}

struct TextTooltip {
    text: SharedString,
}

impl Render for TextTooltip {
    fn render(&mut self, _: &mut Window, cx: &mut gpui::Context<Self>) -> impl IntoElement {
        let t = Tokens::get(cx);
        div().m_2().child(
            div()
                .px_2()
                .py_1()
                .bg(t.raised)
                .border_1()
                .border_color(t.line)
                .rounded(px(RADIUS))
                .shadow(t.menu_shadow())
                .font_family(GEIST_MONO_FONT_FAMILY)
                .text_size(px(11.))
                .text_color(t.ink_muted)
                .child(self.text.clone()),
        )
    }
}

/// Build a tooltip callback for gpui's `.tooltip(...)` from a plain string.
pub(crate) fn text_tooltip(
    text: impl Into<SharedString>,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text: SharedString = text.into();
    move |_, cx| {
        let text = text.clone();
        cx.new(|_| TextTooltip { text }).into()
    }
}
