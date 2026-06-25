use egui::{Color32, CornerRadius, CursorIcon, FontFamily, FontId, Stroke, StrokeKind, Style, Visuals};

pub const XP_BG: Color32 = Color32::from_rgb(228, 233, 244);
pub const XP_TITLE_BG: Color32 = Color32::from_rgb(10, 50, 130);
pub const XP_TITLE_BG_TOP: Color32 = Color32::from_rgb(72, 138, 218);
pub const XP_TITLE_TEXT: Color32 = Color32::WHITE;
pub const XP_BUTTON_FACE: Color32 = Color32::from_rgb(240, 242, 248);
pub const XP_BUTTON_SHADOW: Color32 = Color32::from_rgb(158, 166, 192);
pub const XP_BUTTON_HIGHLIGHT: Color32 = Color32::from_rgb(255, 255, 255);
pub const XP_BUTTON_DARK_SHADOW: Color32 = Color32::from_rgb(80, 90, 120);
pub const XP_BORDER: Color32 = Color32::from_rgb(158, 166, 192);
pub const XP_GROUP_BG: Color32 = Color32::from_rgb(255, 255, 255);
pub const XP_TEXT: Color32 = Color32::from_rgb(0, 0, 0);
pub const XP_ACCENT: Color32 = Color32::from_rgb(49, 106, 197);

pub fn apply_xp_style(ctx: &egui::Context) {
    let mut style = Style::default();

    style.spacing.item_spacing = egui::vec2(8.0, 6.0);
    style.spacing.button_padding = egui::vec2(10.0, 5.0);
    style.spacing.window_margin = egui::Margin::same(10);
    style.spacing.indent = 18.0;

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

    visuals.widgets.hovered.bg_fill = Color32::from_rgb(220, 230, 248);
    visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, XP_TEXT);
    visuals.widgets.hovered.corner_radius = CornerRadius::same(3);
    visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, XP_ACCENT);

    visuals.widgets.active.bg_fill = Color32::from_rgb(180, 195, 225);
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
            FontId::new(16.0, FontFamily::Proportional),
        ),
        (
            egui::TextStyle::Body,
            FontId::new(13.0, FontFamily::Proportional),
        ),
        (
            egui::TextStyle::Button,
            FontId::new(13.0, FontFamily::Proportional),
        ),
        (
            egui::TextStyle::Small,
            FontId::new(11.0, FontFamily::Proportional),
        ),
        (
            egui::TextStyle::Monospace,
            FontId::new(12.0, FontFamily::Monospace),
        ),
    ]
    .into();

    ctx.set_style(style);
}

/// Smooth vertical gradient fill using vertex-colored mesh.
pub fn paint_v_gradient(painter: &egui::Painter, rect: egui::Rect, top: Color32, bottom: Color32) {
    use egui::epaint::{Mesh, Vertex, WHITE_UV};
    let mut mesh = Mesh::default();
    mesh.vertices.push(Vertex { pos: rect.left_top(),     uv: WHITE_UV, color: top });
    mesh.vertices.push(Vertex { pos: rect.right_top(),    uv: WHITE_UV, color: top });
    mesh.vertices.push(Vertex { pos: rect.left_bottom(),  uv: WHITE_UV, color: bottom });
    mesh.vertices.push(Vertex { pos: rect.right_bottom(), uv: WHITE_UV, color: bottom });
    mesh.indices.extend_from_slice(&[0, 1, 2, 1, 3, 2]);
    painter.add(egui::Shape::mesh(mesh));
}

/// Draw a gradient button that looks 3D like Windows XP.
pub fn xp_button_ui(ui: &mut egui::Ui, label: &str, big: bool) -> egui::Response {
    let font_size = if big { 18.0 } else { 13.0 };
    let padding = if big {
        egui::vec2(28.0, 12.0)
    } else {
        egui::vec2(10.0, 5.0)
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
    let response = response.on_hover_cursor(CursorIcon::PointingHand);

    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact(&response);
        let painter = ui.painter();
        let cr = CornerRadius::same(3);

        let (top_color, bot_color) = if response.is_pointer_button_down_on() {
            (Color32::from_rgb(185, 196, 225), Color32::from_rgb(165, 178, 215))
        } else if response.hovered() {
            (Color32::from_rgb(235, 242, 255), Color32::from_rgb(200, 218, 248))
        } else {
            (Color32::from_rgb(255, 255, 255), Color32::from_rgb(215, 220, 238))
        };

        paint_v_gradient(painter, rect, top_color, bot_color);

        // Outer border
        painter.rect_stroke(rect, cr, Stroke::new(1.0, XP_BUTTON_SHADOW), StrokeKind::Outside);

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
            // Inner highlight top-left
            painter.line_segment(
                [rect.left_top() + egui::vec2(1.0, 1.0), rect.right_top() + egui::vec2(-1.0, 1.0)],
                Stroke::new(1.0, XP_BUTTON_HIGHLIGHT),
            );
            painter.line_segment(
                [rect.left_top() + egui::vec2(1.0, 1.0), rect.left_bottom() + egui::vec2(1.0, -1.0)],
                Stroke::new(1.0, XP_BUTTON_HIGHLIGHT),
            );
            // Inner shadow bottom-right
            painter.line_segment(
                [rect.right_bottom() + egui::vec2(-1.0, -1.0), rect.left_bottom() + egui::vec2(1.0, -1.0)],
                Stroke::new(1.0, XP_BUTTON_DARK_SHADOW),
            );
            painter.line_segment(
                [rect.right_bottom() + egui::vec2(-1.0, -1.0), rect.right_top() + egui::vec2(-1.0, 1.0)],
                Stroke::new(1.0, XP_BUTTON_DARK_SHADOW),
            );
        }

        let text_offset = if response.is_pointer_button_down_on() {
            egui::vec2(1.0, 1.0)
        } else {
            egui::vec2(0.0, 0.0)
        };
        painter.galley(
            rect.center() - galley.size() / 2.0 + text_offset,
            galley,
            visuals.text_color(),
        );

        if big && response.has_focus() {
            painter.rect_stroke(rect.shrink(3.0), cr, Stroke::new(1.0, XP_BUTTON_DARK_SHADOW), StrokeKind::Inside);
        }
    }

    response
}

/// Close button with a red gradient and a painted X (no Unicode needed).
pub fn xp_close_button_ui(ui: &mut egui::Ui) -> egui::Response {
    let size = egui::vec2(20.0, 20.0);
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let response = response.on_hover_cursor(CursorIcon::PointingHand);

    if ui.is_rect_visible(rect) {
        let painter = ui.painter();
        let cr = CornerRadius::same(3);

        let (top_color, bot_color) = if response.is_pointer_button_down_on() {
            (Color32::from_rgb(160, 20, 20), Color32::from_rgb(130, 10, 10))
        } else if response.hovered() {
            (Color32::from_rgb(240, 80, 80), Color32::from_rgb(200, 30, 30))
        } else {
            (Color32::from_rgb(220, 60, 60), Color32::from_rgb(175, 20, 20))
        };

        paint_v_gradient(painter, rect, top_color, bot_color);
        painter.rect_stroke(rect, cr, Stroke::new(1.0, Color32::from_rgb(255, 100, 100)), StrokeKind::Outside);

        // Paint X with two diagonal lines
        let m = 6.0;
        let x_rect = rect.shrink(m);
        let stroke = Stroke::new(2.0, Color32::WHITE);
        painter.line_segment([x_rect.left_top(), x_rect.right_bottom()], stroke);
        painter.line_segment([x_rect.right_top(), x_rect.left_bottom()], stroke);
    }

    response
}
