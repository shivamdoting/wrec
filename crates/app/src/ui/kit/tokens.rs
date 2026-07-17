//! Semantic design tokens for the wrec UI kit.
//!
//! Each mode defines only a handful of base colors; every other role
//! (hover/active ramps, muted text, hairlines, tracks) is derived by mixing,
//! so adding or tweaking a color never requires hand-defining its states.
//!
//! Current treatment: gray-washed surfaces with controls sitting on top as
//! raised cards — lighter than the surface, hairline-bordered, softly shadowed.

use gpui::{hsla, point, px, rgb, App, BoxShadow, Global, Hsla};

/// Corner radius for controls (buttons, switches, select triggers).
pub(crate) const RADIUS: f32 = 5.;
/// Corner radius for floating surfaces (menus, tooltips).
pub(crate) const RADIUS_MENU: f32 = 7.;

/// The colors a mode must define; everything else in [`Tokens`] is derived.
struct BasePalette {
    /// Window background.
    surface: u32,
    /// Sidebar background.
    panel: u32,
    /// Resting fill of interactive controls; sits *above* the gray surface.
    control: u32,
    /// Primary text; also the fill of solid controls.
    ink: u32,
    /// The wrec red. Reserved for record/live states.
    accent: u32,
    /// Text on top of `accent`.
    on_accent: u32,
}

const LIGHT_BASE: BasePalette = BasePalette {
    surface: 0xf2f2f3,
    panel: 0xe9e9ea,
    control: 0xffffff,
    ink: 0x1b1b1d,
    accent: 0xc62828,
    on_accent: 0xffffff,
};

const DARK_BASE: BasePalette = BasePalette {
    surface: 0x1b1b1e,
    panel: 0x151517,
    control: 0x28282c,
    ink: 0xf0f0f1,
    accent: 0xd03a3f,
    on_accent: 0xffffff,
};

#[derive(Clone, Copy)]
pub(crate) struct Tokens {
    pub is_dark: bool,

    // Surfaces
    pub surface: Hsla,
    pub panel: Hsla,
    /// Floating surfaces: menus, tooltips, toasts.
    pub raised: Hsla,
    /// Resting fill of interactive controls.
    pub control: Hsla,
    pub control_hover: Hsla,
    pub control_active: Hsla,

    // Text
    pub ink: Hsla,
    pub ink_muted: Hsla,
    pub ink_faint: Hsla,

    // Lines
    pub line: Hsla,
    pub line_strong: Hsla,

    // Solid (ink-filled) controls
    pub solid: Hsla,
    pub solid_hover: Hsla,
    pub solid_active: Hsla,
    pub on_solid: Hsla,

    // Accent (record/live red)
    pub accent: Hsla,
    pub accent_hover: Hsla,
    pub accent_active: Hsla,
    pub on_accent: Hsla,

    // Misc
    /// Off-state switch track.
    pub track_off: Hsla,
    pub selection: Hsla,
}

impl Global for Tokens {}

impl Tokens {
    pub(crate) fn light() -> Self {
        Self::from_base(&LIGHT_BASE, false)
    }

    pub(crate) fn dark() -> Self {
        Self::from_base(&DARK_BASE, true)
    }

    fn from_base(base: &BasePalette, is_dark: bool) -> Self {
        let surface = base.surface;
        let control = base.control;
        let ink = base.ink;
        let accent = base.accent;
        // Dark controls need slightly stronger hover steps to read at all.
        let step = if is_dark { 0.05 } else { 0.035 };

        Self {
            is_dark,
            surface: color(surface),
            panel: color(base.panel),
            raised: color(if is_dark {
                mix(control, ink, 0.04)
            } else {
                control
            }),
            control: color(control),
            control_hover: color(mix(control, ink, step)),
            control_active: color(mix(control, ink, step * 2.)),
            ink: color(ink),
            ink_muted: color(mix(ink, surface, 0.42)),
            ink_faint: color(mix(ink, surface, 0.62)),
            line: color(mix(ink, surface, if is_dark { 0.82 } else { 0.86 })),
            line_strong: color(mix(ink, surface, 0.68)),
            solid: color(ink),
            solid_hover: color(mix(ink, surface, 0.14)),
            solid_active: color(mix(ink, surface, 0.24)),
            on_solid: color(surface),
            accent: color(accent),
            accent_hover: color(mix(accent, ink, 0.12)),
            accent_active: color(mix(accent, ink, 0.2)),
            on_accent: color(base.on_accent),
            track_off: color(mix(ink, surface, 0.72)),
            selection: color(ink).opacity(0.14),
        }
    }

    /// Soft resting shadow for raised controls (buttons, select triggers,
    /// the active nav card). Dark mode needs more alpha to register at all.
    pub(crate) fn control_shadow(&self) -> Vec<BoxShadow> {
        let alpha = if self.is_dark { 0.32 } else { 0.07 };
        vec![
            BoxShadow {
                color: hsla(0., 0., 0., alpha),
                offset: point(px(0.), px(1.)),
                blur_radius: px(2.),
                spread_radius: px(0.),
            },
            BoxShadow {
                color: hsla(0., 0., 0., alpha * 0.5),
                offset: point(px(0.), px(2.)),
                blur_radius: px(6.),
                spread_radius: px(-2.),
            },
        ]
    }

    /// Floating shadow for menus and tooltips.
    pub(crate) fn menu_shadow(&self) -> Vec<BoxShadow> {
        let alpha = if self.is_dark { 0.55 } else { 0.14 };
        vec![
            BoxShadow {
                color: hsla(0., 0., 0., alpha),
                offset: point(px(0.), px(10.)),
                blur_radius: px(28.),
                spread_radius: px(-6.),
            },
            BoxShadow {
                color: hsla(0., 0., 0., alpha * 0.6),
                offset: point(px(0.), px(2.)),
                blur_radius: px(8.),
                spread_radius: px(-2.),
            },
        ]
    }

    pub(crate) fn install(dark: bool, cx: &mut App) {
        cx.set_global(if dark { Self::dark() } else { Self::light() });
    }

    pub(crate) fn get(cx: &App) -> Self {
        *cx.global::<Tokens>()
    }
}

fn color(hex: u32) -> Hsla {
    Hsla::from(rgb(hex))
}

/// Linear blend of two hex colors: `t = 0` gives `a`, `t = 1` gives `b`.
fn mix(a: u32, b: u32, t: f32) -> u32 {
    let a = rgb(a);
    let b = rgb(b);
    let channel = |x: f32, y: f32| ((x + (y - x) * t).clamp(0., 1.) * 255.).round() as u32;
    (channel(a.r, b.r) << 16) | (channel(a.g, b.g) << 8) | channel(a.b, b.b)
}
