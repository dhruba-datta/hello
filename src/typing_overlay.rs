//! The small panel that shows a Hello Type suggestion next to the caret. Plain AppKit, like
//! the recording indicator: it never takes focus and ignores the mouse.

use objc2::MainThreadMarker;
use objc2::rc::Retained;
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSEvent, NSFont, NSPanel, NSScreen, NSStatusWindowLevel,
    NSTextField, NSView, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

use crate::accessibility::ScreenRect;

const PADDING_X: f64 = 10.0;
const PADDING_Y: f64 = 5.0;
const FONT_SIZE: f64 = 13.0;
/// Space between the caret and the panel.
const CARET_GAP: f64 = 6.0;
const MAX_WIDTH: f64 = 420.0;

pub struct TypingOverlay {
    panel: Retained<NSPanel>,
    label: Retained<NSTextField>,
    shown: bool,
}

impl TypingOverlay {
    pub fn new(mtm: MainThreadMarker) -> Self {
        let frame = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(120.0, 28.0));
        let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
            mtm.alloc(),
            frame,
            NSWindowStyleMask::Borderless
                | NSWindowStyleMask::UtilityWindow
                | NSWindowStyleMask::NonactivatingPanel
                | NSWindowStyleMask::FullSizeContentView,
            NSBackingStoreType::Buffered,
            false,
        );
        let view = NSView::initWithFrame(mtm.alloc(), frame);
        view.setWantsLayer(true);
        let label = NSTextField::labelWithString(&NSString::from_str(""), mtm);
        label.setFont(Some(&NSFont::systemFontOfSize(FONT_SIZE)));
        label.setMaximumNumberOfLines(1);
        view.addSubview(&label);
        panel.setContentView(Some(&view));
        panel.setBackgroundColor(Some(&NSColor::clearColor()));
        panel.setOpaque(false);
        panel.setIgnoresMouseEvents(true);
        panel.setHasShadow(true);
        panel.setHidesOnDeactivate(false);
        panel.setCanHide(false);
        panel.setLevel(NSStatusWindowLevel);
        panel.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::CanJoinAllApplications
                | NSWindowCollectionBehavior::FullScreenAuxiliary
                | NSWindowCollectionBehavior::Stationary
                | NSWindowCollectionBehavior::IgnoresCycle,
        );
        unsafe { panel.setReleasedWhenClosed(false) };
        Self {
            panel,
            label,
            shown: false,
        }
    }

    pub fn show(&mut self, text: &str, caret: Option<ScreenRect>, mtm: MainThreadMarker) {
        self.apply_theme();
        self.label
            .setStringValue(&NSString::from_str(&format!("{text}  \u{21e5}")));
        self.label.sizeToFit();
        let label_frame = self.label.frame();
        let width = (label_frame.size.width + 2.0 * PADDING_X).min(MAX_WIDTH);
        let height = label_frame.size.height + 2.0 * PADDING_Y;
        self.label
            .setFrameOrigin(NSPoint::new(PADDING_X, PADDING_Y));
        if let Some(view) = self.panel.contentView() {
            view.setFrameSize(NSSize::new(width, height));
            if let Some(layer) = view.layer() {
                layer.setCornerRadius(8.0);
                layer.setMasksToBounds(true);
            }
        }
        let top_left = self.top_left(caret, width, height, mtm);
        self.panel
            .setFrame_display(NSRect::new(top_left, NSSize::new(width, height)), false);
        // setFrame places the bottom-left corner; move so top_left is the top edge.
        self.panel.setFrameTopLeftPoint(top_left);
        if !self.shown {
            self.panel.orderFrontRegardless();
            self.shown = true;
        }
    }

    pub fn hide(&mut self) {
        if self.shown {
            self.panel.orderOut(None);
            self.shown = false;
        }
    }

    /// Below the caret (above it near the bottom of the screen), kept on the screen the
    /// caret is on. Without caret bounds, next to the mouse pointer.
    fn top_left(
        &self,
        caret: Option<ScreenRect>,
        width: f64,
        height: f64,
        mtm: MainThreadMarker,
    ) -> NSPoint {
        let screens = NSScreen::screens(mtm);
        let primary_height = screens
            .iter()
            .next()
            .map_or(0.0, |screen| screen.frame().size.height);
        let (anchor_x, anchor_top, anchor_bottom) = match caret {
            // Accessibility measures from the top-left of the main display; AppKit from
            // its bottom-left.
            Some(rect) => (
                rect.x,
                primary_height - rect.y,
                primary_height - (rect.y + rect.height),
            ),
            None => {
                let pointer = NSEvent::mouseLocation();
                (pointer.x + 12.0, pointer.y + 4.0, pointer.y - 16.0)
            }
        };
        let screen = screens
            .iter()
            .find(|screen| {
                let frame = screen.frame();
                anchor_x >= frame.origin.x
                    && anchor_x < frame.origin.x + frame.size.width
                    && anchor_bottom >= frame.origin.y
                    && anchor_bottom < frame.origin.y + frame.size.height
            })
            .or_else(|| screens.iter().next());
        let Some(screen) = screen else {
            return NSPoint::new(anchor_x, anchor_bottom - CARET_GAP);
        };
        let visible = screen.visibleFrame();
        let mut top = anchor_bottom - CARET_GAP;
        if top - height < visible.origin.y {
            top = anchor_top + CARET_GAP + height;
        }
        let max_x = visible.origin.x + visible.size.width - width - 4.0;
        let x = anchor_x.min(max_x).max(visible.origin.x + 4.0);
        NSPoint::new(x, top)
    }

    fn apply_theme(&self) {
        let light = crate::desktop_ui::is_light();
        let (background, border, text) = if light {
            (
                (0.98, 0.98, 0.98, 0.97),
                (0.78, 0.78, 0.80, 1.0),
                (0.30, 0.30, 0.33, 1.0),
            )
        } else {
            (
                (0.12, 0.12, 0.13, 0.96),
                (0.26, 0.26, 0.29, 1.0),
                (0.80, 0.80, 0.83, 1.0),
            )
        };
        self.label.setTextColor(Some(&color(text)));
        if let Some(view) = self.panel.contentView()
            && let Some(layer) = view.layer()
        {
            layer.setBackgroundColor(Some(&color(background).CGColor()));
            layer.setBorderColor(Some(&color(border).CGColor()));
            layer.setBorderWidth(1.0);
        }
    }
}

fn color((red, green, blue, alpha): (f64, f64, f64, f64)) -> Retained<NSColor> {
    NSColor::colorWithSRGBRed_green_blue_alpha(red, green, blue, alpha)
}
