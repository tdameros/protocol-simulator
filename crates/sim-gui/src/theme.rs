use egui::{Color32, Context, CornerRadius, FontDefinitions, Stroke, Visuals};
use egui_phosphor::Variant;

const CORNER_RADIUS: CornerRadius = CornerRadius::same(6);
const ACCENT: Color32 = Color32::from_rgb(45, 110, 200);
const INPUT_BORDER: Color32 = Color32::from_gray(200);

pub fn apply(ctx: &Context) {
    let mut fonts = FontDefinitions::default();
    egui_phosphor::add_to_fonts(&mut fonts, Variant::Regular);
    ctx.set_fonts(fonts);

    let mut visuals = Visuals::light();
    visuals.panel_fill = Color32::from_rgb(245, 246, 248);
    visuals.window_fill = Color32::from_rgb(245, 246, 248);
    visuals.extreme_bg_color = Color32::WHITE;
    visuals.selection.bg_fill = ACCENT;
    visuals.hyperlink_color = ACCENT;

    // Inactive widgets (e.g. an unfocused TextEdit) get no border by default in egui,
    // relying only on a subtle fill-color contrast that disappears once the surrounding
    // panel is also near-white. Give them a visible outline so inputs stay legible.
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, INPUT_BORDER);

    for widgets in [
        &mut visuals.widgets.noninteractive,
        &mut visuals.widgets.inactive,
        &mut visuals.widgets.hovered,
        &mut visuals.widgets.active,
        &mut visuals.widgets.open,
    ] {
        widgets.corner_radius = CORNER_RADIUS;
    }

    ctx.set_visuals(visuals);

    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = egui::vec2(10.0, 5.0);
        style.spacing.window_margin = egui::Margin::same(10);
    });
}
