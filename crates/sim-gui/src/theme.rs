use egui::{
    Color32, Context, CornerRadius, FontDefinitions, Stroke, TextStyle, Theme, Ui, Visuals,
};
use egui_phosphor::Variant;

const CORNER_RADIUS: CornerRadius = CornerRadius::same(6);
const BUTTON_PADDING: egui::Vec2 = egui::vec2(10.0, 5.0);
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
        // The traffic list draws its rows into a fixed set of bands and scrolls
        // the frames through them, so the widget at a given rectangle is a
        // different frame after the list resizes. egui reads that as a widget
        // whose id is not stable and, in a debug build, outlines every such
        // rectangle in red and logs a warning. It cannot tell a virtualised
        // list from a real defect. The check that catches a real duplicate,
        // `Options::warn_on_id_clash`, is a different one and stays on.
        //
        // Gated, because the field it names does not exist in a release build.
        #[cfg(debug_assertions)]
        {
            style.debug.warn_if_rect_changes_id = false;
        }
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.button_padding = BUTTON_PADDING;
        style.spacing.window_margin = egui::Margin::same(10);
    });
}

/// Keeps the height a row assumes in step with the height its contents really
/// have.
///
/// `Ui::horizontal` decides how tall a row is going to be before anything has
/// been put in it, and guesses `interact_size.y`. Everything shorter than that
/// guess is then centred against the guess rather than against the row, so a
/// label next to a button sits a couple of pixels high. egui's default guess is
/// 18, while a button of ours is the text plus `BUTTON_PADDING` twice, which is
/// where the two fell out of step.
///
/// Measured rather than written down, so it still holds after a zoom.
pub fn sync_row_height(ui: &Ui) {
    let wanted = ui.text_style_height(&TextStyle::Body) + 2.0 * BUTTON_PADDING.y;
    if (ui.spacing().interact_size.y - wanted).abs() > f32::EPSILON {
        ui.ctx()
            .all_styles_mut(|style| style.spacing.interact_size.y = wanted);
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Resizing the traffic list slides its rows through the bands they are
    /// drawn in, which a debug build of egui outlines in red across the whole
    /// panel. The warning that catches two widgets sharing an id is not that
    /// one and has to survive.
    ///
    /// Both of them only exist where the drawing they govern does, which is a
    /// build with debug assertions on.
    #[cfg(debug_assertions)]
    #[test]
    fn a_scrolling_list_is_not_reported_as_an_unstable_id() {
        let ctx = Context::default();
        apply(&ctx, Theme::Dark);

        assert!(!ctx.global_style().debug.warn_if_rect_changes_id);
        assert!(
            ctx.options(|options| options.warn_on_id_clash),
            "a duplicate id is still worth being told about"
        );
    }
}
