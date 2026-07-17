use super::{kit_icon, Tokens, RADIUS, RADIUS_MENU};
use crate::assets::GEIST_MONO_FONT_FAMILY;
use gpui::prelude::FluentBuilder as _;
use gpui::{
    deferred, div, px, App, Context, ElementId, Entity, EventEmitter, InteractiveElement,
    IntoElement, ParentElement, RenderOnce, SharedString, StatefulInteractiveElement, Styled,
    Window,
};

/// An entry in a [`Picker`]. `value` is the stable key handlers receive;
/// `label` is what's rendered.
pub(crate) trait PickerItem: Clone + 'static {
    fn label(&self) -> SharedString;
    fn value(&self) -> SharedString;
    fn disabled(&self) -> bool {
        false
    }
}

impl PickerItem for &'static str {
    fn label(&self) -> SharedString {
        (*self).into()
    }

    fn value(&self) -> SharedString {
        (*self).into()
    }
}

#[derive(Debug)]
pub(crate) enum PickerEvent {
    /// The user picked an item; carries the item's value.
    Confirm(SharedString),
}

pub(crate) struct PickerState<T: PickerItem> {
    items: Vec<T>,
    selected: Option<usize>,
    open: bool,
}

impl<T: PickerItem> EventEmitter<PickerEvent> for PickerState<T> {}

impl<T: PickerItem> PickerState<T> {
    pub(crate) fn new(items: Vec<T>, selected: Option<usize>) -> Self {
        Self {
            items,
            selected,
            open: false,
        }
    }

    /// Replace the items, keeping the selection if its value still exists.
    pub(crate) fn set_items(&mut self, items: Vec<T>, cx: &mut Context<Self>) {
        let selected_value = self.selected_value();
        self.items = items;
        self.selected = selected_value
            .and_then(|value| self.items.iter().position(|item| item.value() == value));
        cx.notify();
    }

    /// Select the item with this value without emitting an event.
    pub(crate) fn set_selected_value(&mut self, value: &str, cx: &mut Context<Self>) {
        self.selected = self
            .items
            .iter()
            .position(|item| item.value().as_ref() == value);
        cx.notify();
    }

    pub(crate) fn set_selected_index(&mut self, index: Option<usize>, cx: &mut Context<Self>) {
        self.selected = index.filter(|&ix| ix < self.items.len());
        cx.notify();
    }

    pub(crate) fn selected_value(&self) -> Option<SharedString> {
        self.selected
            .and_then(|ix| self.items.get(ix))
            .map(PickerItem::value)
    }

    fn selected_label(&self) -> Option<SharedString> {
        self.selected
            .and_then(|ix| self.items.get(ix))
            .map(PickerItem::label)
    }

    fn confirm(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(item) = self.items.get(ix) else {
            return;
        };
        if item.disabled() {
            return;
        }
        self.selected = Some(ix);
        self.open = false;
        cx.emit(PickerEvent::Confirm(item.value()));
        cx.notify();
    }
}

#[derive(IntoElement)]
pub(crate) struct Picker<T: PickerItem> {
    id: ElementId,
    state: Entity<PickerState<T>>,
    placeholder: SharedString,
    disabled: bool,
    menu_max_h: f32,
    height: f32,
}

impl<T: PickerItem> Picker<T> {
    pub(crate) fn new(id: impl Into<ElementId>, state: &Entity<PickerState<T>>) -> Self {
        Self {
            id: id.into(),
            state: state.clone(),
            placeholder: SharedString::default(),
            disabled: false,
            menu_max_h: 224.,
            height: 32.,
        }
    }

    pub(crate) fn placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    pub(crate) fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub(crate) fn menu_max_h(mut self, max_h: f32) -> Self {
        self.menu_max_h = max_h;
        self
    }

    pub(crate) fn height(mut self, height: f32) -> Self {
        self.height = height;
        self
    }
}

impl<T: PickerItem> RenderOnce for Picker<T> {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = Tokens::get(cx);
        let state = self.state.read(cx);
        let open = state.open && !self.disabled;
        let label = state.selected_label();
        let items = state.items.clone();
        let selected = state.selected;
        let entity = self.state.clone();

        let mut trigger = div()
            .id(self.id)
            .relative()
            .w_full()
            .h(px(self.height))
            .flex()
            .items_center()
            .gap_2()
            .px_2p5()
            .rounded(px(RADIUS))
            .bg(t.control)
            .border_1()
            .border_color(if open { t.line_strong } else { t.line })
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .truncate()
                    .font_family(GEIST_MONO_FONT_FAMILY)
                    .text_size(px(12.5))
                    .text_color(if label.is_some() { t.ink } else { t.ink_faint })
                    .child(label.unwrap_or(self.placeholder)),
            )
            .child(kit_icon(
                crate::assets::PhosphorIcon::CaretDown,
                11.,
                t.ink_muted,
            ));

        if self.disabled {
            trigger = trigger.opacity(0.45).cursor_default();
        } else {
            trigger = trigger
                .cursor_pointer()
                .hover(move |this| this.bg(t.control_hover))
                .on_click({
                    let entity = entity.clone();
                    move |_, _, cx| {
                        entity.update(cx, |state, cx| {
                            state.open = !state.open;
                            cx.notify();
                        });
                    }
                });
        }

        trigger.when(open, |this| {
            this.child(
                deferred(
                    div()
                        .id("picker-menu")
                        .occlude()
                        .absolute()
                        .top(px(self.height + 4.))
                        .left(px(-1.))
                        .right(px(-1.))
                        .max_h(px(self.menu_max_h))
                        .overflow_y_scroll()
                        .p_1()
                        .rounded(px(RADIUS_MENU))
                        .bg(t.raised)
                        .border_1()
                        .border_color(t.line)
                        .shadow(t.menu_shadow())
                        .on_mouse_down_out({
                            let entity = entity.clone();
                            move |_, _, cx| {
                                entity.update(cx, |state, cx| {
                                    state.open = false;
                                    cx.notify();
                                });
                            }
                        })
                        .children(items.into_iter().enumerate().map(|(ix, item)| {
                            let is_selected = selected == Some(ix);
                            let item_disabled = item.disabled();
                            let row = div()
                                .id(ix)
                                .flex()
                                .items_center()
                                .gap_2()
                                .px_2()
                                .py_1p5()
                                .rounded(px(RADIUS))
                                .font_family(GEIST_MONO_FONT_FAMILY)
                                .text_size(px(12.5))
                                .text_color(if item_disabled { t.ink_faint } else { t.ink })
                                .child(
                                    // Selection marker: small red square, the one
                                    // accent use outside record/live states.
                                    div()
                                        .flex_none()
                                        .size(px(5.))
                                        .when(is_selected, |this| this.bg(t.accent)),
                                )
                                .child(div().flex_1().min_w(px(0.)).truncate().child(item.label()));

                            if item_disabled {
                                row.cursor_default()
                            } else {
                                row.cursor_pointer()
                                    .hover(move |this| this.bg(t.control_hover))
                                    .on_click({
                                        let entity = entity.clone();
                                        move |_, _, cx| {
                                            entity.update(cx, |state, cx| state.confirm(ix, cx));
                                        }
                                    })
                            }
                        })),
                )
                .with_priority(1),
            )
        })
    }
}
