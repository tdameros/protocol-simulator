use egui::{Color32, Context, CornerRadius, FontDefinitions, Stroke, Theme, Visuals};
use egui_phosphor::Variant;

const CORNER_RADIUS: CornerRadius = CornerRadius::same(6);
const ACCENT: Color32 = Color32::from_rgb(45, 110, 200);
const LIGHT_INPUT_BORDER: Color32 = Color32::from_gray(200);
const DARK_INPUT_BORDER: Color32 = Color32::from_gray(90);

/// Installs both palettes and picks one.
///
/// Both, so that switching is a change of preference rather than a repaint of
/// the theme in place. That also makes the current theme something the app can
/// read back, which is what lets a project remember the one it was saved in.
pub fn apply(ctx: &Context, theme: Theme) {
    let mut fonts = FontDefinitions::default();
    egui_phosphor::add_to_fonts(&mut fonts, Variant::Regular);
    ctx.set_fonts(fonts);

    ctx.set_visuals_of(Theme::Light, palette(Theme::Light));
    ctx.set_visuals_of(Theme::Dark, palette(Theme::Dark));
    ctx.set_theme(theme);

    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 5.0);
        style.spacing.window_margin = egui::Margin::same(10);
    });
}

fn palette(theme: Theme) -> Visuals {
    let mut visuals = match theme {
        Theme::Light => Visuals::light(),
        Theme::Dark => Visuals::dark(),
    };

    if theme == Theme::Light {
        visuals.panel_fill = Color32::from_rgb(245, 246, 248);
        visuals.window_fill = Color32::from_rgb(245, 246, 248);
        visuals.extreme_bg_color = Color32::WHITE;
    }
    visuals.selection.bg_fill = ACCENT;
    visuals.hyperlink_color = ACCENT;

    // Inactive widgets (e.g. an unfocused TextEdit) get no border by default in egui,
    // relying only on a subtle fill-color contrast that disappears once the surrounding
    // panel is also near-white. Give them a visible outline so inputs stay legible.
    visuals.widgets.inactive.bg_stroke = Stroke::new(
        1.0,
        match theme {
            Theme::Light => LIGHT_INPUT_BORDER,
            Theme::Dark => DARK_INPUT_BORDER,
        },
    );

    for widgets in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        widgets.corner_radius = CORNER_RADIUS;
    }

    visuals
}
