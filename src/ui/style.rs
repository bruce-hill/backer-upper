use egui::{Color32, CornerRadius, FontFamily, FontId, Stroke, StrokeKind, Style, Visuals};

pub const XP_BG: Color32 = Color32::from_rgb(236, 233, 216);
pub const XP_TITLE_BG: Color32 = Color32::from_rgb(10, 36, 106);
pub const XP_TITLE_TEXT: Color32 = Color32::WHITE;
pub const XP_BUTTON_FACE: Color32 = Color32::from_rgb(236, 233, 216);
pub const XP_BUTTON_SHADOW: Color32 = Color32::from_rgb(172, 168, 153);
pub const XP_BUTTON_HIGHLIGHT: Color32 = Color32::from_rgb(255, 255, 255);
pub const XP_BUTTON_DARK_SHADOW: Color32 = Color32::from_rgb(113, 111, 100);
pub const XP_BORDER: Color32 = Color32::from_rgb(172, 168, 153);
pub const XP_GROUP_BG: Color32 = Color32::from_rgb(255, 255, 255);
pub const XP_TEXT: Color32 = Color32::from_rgb(0, 0, 0);
pub const XP_ACCENT: Color32 = Color32::from_rgb(49, 106, 197);

pub fn apply_xp_style(ctx: &egui::Context) {
    let mut style = Style::default();

    style.spacing.item_spacing = egui::vec2(6.0, 4.0);
    style.spacing.button_padding = egui::vec2(8.0, 3.0);
    style.spacing.window_margin = egui::Margin::same(6);
    style.spacing.indent = 16.0;

    let mut visuals = Visuals::light();
    visuals.panel_fill = XP_BG;
    visuals.window_fill = XP_BG;
    visuals.faint_bg_color = XP_GROUP_BG;
    visuals.extreme_bg_color = XP_GROUP_BG;
    visuals.window_stroke = Stroke::new(1.0, XP_BORDER);
    visuals.window_corner_radius = CornerRadius::same(0);

    visuals.widgets.noninteractive.bg_fill = XP_BG;
    visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, XP_TEXT);
    visuals.widgets.noninteractive.corner_radius = CornerRadius::same(3);

    visuals.widgets.inactive.bg_fill = XP_BUTTON_FACE;
    visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, XP_TEXT);
    visuals.widgets.inactive.corner_radius = CornerRadius::same(3);
    visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, XP_BUTTON_SHADOW);

    visuals.widgets.hovered.bg_fill = Color32::from_rgb(246, 244, 236);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, XP_TEXT);
    visuals.widgets.hovered.corner_radius = CornerRadius::same(3);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, XP_ACCENT);

    visuals.widgets.active.bg_fill = Color32::from_rgb(210, 208, 200);
    visuals.widgets.active.fg_stroke = Stroke::new(1.0, XP_TEXT);
    visuals.widgets.active.corner_radius = CornerRadius::same(3);
    visuals.widgets.active.bg_stroke = Stroke::new(1.0, XP_BUTTON_DARK_SHADOW);

    visuals.selection.bg_fill = XP_ACCENT;
    visuals.selection.stroke = Stroke::new(1.0, Color32::WHITE);

    visuals.text_cursor.stroke = Stroke::new(1.5, XP_TEXT);

    style.visuals = visuals;

    style.text_styles = [
        (
            egui::TextStyle::Heading,
            FontId::new(14.0, FontFamily::Proportional),
        ),
        (
            egui::TextStyle::Body,
            FontId::new(12.0, FontFamily::Proportional),
        ),
        (
            egui::TextStyle::Button,
            FontId::new(12.0, FontFamily::Proportional),
        ),
        (
            egui::TextStyle::Small,
            FontId::new(10.0, FontFamily::Proportional),
        ),
        (
            egui::TextStyle::Monospace,
            FontId::new(11.0, FontFamily::Monospace),
        ),
    ]
    .into();

    ctx.set_style(style);
}

/// Draw a gradient button that looks 3D like Windows XP.
pub fn xp_button_ui(
    ui: &mut egui::Ui,
    label: &str,
    big: bool,
) -> egui::Response {
    let font_size = if big { 18.0 } else { 12.0 };
    let padding = if big {
        egui::vec2(24.0, 10.0)
    } else {
        egui::vec2(8.0, 3.0)
    };

    let galley = ui.fonts(|f| {
        f.layout_no_wrap(
            label.to_owned(),
            FontId::new(font_size, FontFamily::Proportional),
            XP_TEXT,
        )
    });

    let desired_size = galley.size() + padding * 2.0;
    let (rect, response) = ui.allocate_exact_size(desired_size, egui::Sense::click());

    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact(&response);
        let painter = ui.painter();

        let corner_radius = CornerRadius::same(3);

        // Base fill with gradient effect
        let (top_color, bot_color) = if response.is_pointer_button_down_on() {
            (Color32::from_rgb(210, 208, 200), Color32::from_rgb(196, 193, 184))
        } else if response.hovered() {
            (Color32::from_rgb(255, 253, 245), Color32::from_rgb(236, 233, 218))
        } else {
            (Color32::from_rgb(255, 255, 255), Color32::from_rgb(218, 214, 200))
        };

        let half_y = rect.center().y;

        let top_rect = egui::Rect::from_min_max(rect.min, egui::pos2(rect.max.x, half_y));
        let bot_rect = egui::Rect::from_min_max(egui::pos2(rect.min.x, half_y), rect.max);

        painter.rect_filled(
            top_rect,
            CornerRadius { nw: 3, ne: 3, sw: 0, se: 0 },
            top_color,
        );
        painter.rect_filled(
            bot_rect,
            CornerRadius { nw: 0, ne: 0, sw: 3, se: 3 },
            bot_color,
        );

        // Outer border
        painter.rect_stroke(
            rect,
            corner_radius,
            Stroke::new(1.0, XP_BUTTON_SHADOW),
            StrokeKind::Outside,
        );

        if response.is_pointer_button_down_on() {
            painter.line_segment(
                [rect.left_top(), rect.right_top()],
                Stroke::new(1.0, XP_BUTTON_DARK_SHADOW),
            );
            painter.line_segment(
                [rect.left_top(), rect.left_bottom()],
                Stroke::new(1.0, XP_BUTTON_DARK_SHADOW),
            );
        } else {
            // Highlight top-left
            painter.line_segment(
                [
                    rect.left_top() + egui::vec2(1.0, 1.0),
                    rect.right_top() + egui::vec2(-1.0, 1.0),
                ],
                Stroke::new(1.0, XP_BUTTON_HIGHLIGHT),
            );
            painter.line_segment(
                [
                    rect.left_top() + egui::vec2(1.0, 1.0),
                    rect.left_bottom() + egui::vec2(1.0, -1.0),
                ],
                Stroke::new(1.0, XP_BUTTON_HIGHLIGHT),
            );
            // Shadow bottom-right
            painter.line_segment(
                [
                    rect.right_bottom() + egui::vec2(-1.0, -1.0),
                    rect.left_bottom() + egui::vec2(1.0, -1.0),
                ],
                Stroke::new(1.0, XP_BUTTON_DARK_SHADOW),
            );
            painter.line_segment(
                [
                    rect.right_bottom() + egui::vec2(-1.0, -1.0),
                    rect.right_top() + egui::vec2(-1.0, 1.0),
                ],
                Stroke::new(1.0, XP_BUTTON_DARK_SHADOW),
            );
        }

        // Label
        let text_offset = if response.is_pointer_button_down_on() {
            egui::vec2(1.0, 1.0)
        } else {
            egui::vec2(0.0, 0.0)
        };
        let text_color = visuals.text_color();
        painter.galley(
            rect.center() - galley.size() / 2.0 + text_offset,
            galley,
            text_color,
        );

        if big && response.has_focus() {
            painter.rect_stroke(
                rect.shrink(3.0),
                corner_radius,
                Stroke::new(1.0, XP_BUTTON_DARK_SHADOW),
                StrokeKind::Inside,
            );
        }
    }

    response
}
